//! Файловый кэш принятых медиа (`~/.forge/telegram-cache/media/…`).
//!
//! # Раскладка на диске
//!
//! ```text
//! <cache_dir>/media/<dialog_id>/<message_id>.<kind>.bin        ← байты
//! <cache_dir>/media/<dialog_id>/<message_id>.<kind>.meta.json  ← метаданные
//! <cache_dir>/media/<dialog_id>/<message_id>.<kind>.tmp.<uuid> ← staging
//! ```
//!
//! `kind` — `thumb` (превью) или `full` (оригинал). Разные `kind` одного
//! сообщения — независимые записи: превью может быть в кэше, а оригинал нет.
//!
//! # Почему запись двухступенчатая
//!
//! Порядок «сначала `bin`, потом `meta`» — это контракт целостности:
//! [`get`] считает попаданием только пару «непустой bin + читаемая meta»,
//! поэтому оборванная на середине запись (остался `.tmp` или появился
//! `bin` без `meta`) НИКОГДА не выдаётся за валидный кэш. Оба файла
//! пишутся во временный файл В ТОМ ЖЕ каталоге и переносятся `rename`,
//! который в пределах одной ФС атомарен: наблюдатель видит либо старое
//! содержимое, либо новое целиком, но никогда — половину.
//!
//! # Безопасность путей
//!
//! `dialog_id` и `message_id` приходят из URL, поэтому в пути они попадают
//! ТОЛЬКО через [`CacheKey::parse`], который разбирает их в `i64`/`i32`.
//! Строки в путь не конкатенируются вообще: `"../../etc"` не парсится в
//! `i64` и отбрасывается до любого обращения к ФС (см. тест
//! `parse_rejects_path_traversal`).
//!
//! # LRU
//!
//! Дисковый бюджет — `cfg.max_cache_bytes`. [`sweep_once`] удаляет самые
//! старые по `mtime` записи, пока размер не опустится ниже
//! [`SWEEP_TARGET_NUM`]/[`SWEEP_TARGET_DEN`] от лимита (гистерезис: без него
//! подчистка запускалась бы каждый тик). `mtime` — приближение «последнего
//! использования»: при попадании в кэш роут дёргает [`touch`], поэтому
//! часто открываемые файлы всплывают наверх.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::TelegramConfig;
use crate::state::TelegramState;

/// Подкаталог кэша, в котором живут медиа.
pub const MEDIA_SUBDIR: &str = "media";

/// Период работы LRU-подчистки.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Числитель целевой доли лимита, до которой подметает sweeper.
pub const SWEEP_TARGET_NUM: u64 = 9;
/// Знаменатель целевой доли лимита (9/10 ⇒ 90 %).
pub const SWEEP_TARGET_DEN: u64 = 10;

/// Сколько живёт брошенный staging-файл, прежде чем sweeper его удалит.
///
/// Час с запасом перекрывает любую живую загрузку (сам HTTP-запрос
/// ограничен таймаутом клиента), поэтому более старый `.tmp` — гарантированно
/// мусор от упавшего процесса.
pub const STAGING_TTL: Duration = Duration::from_secs(60 * 60);

/// Что именно кэшируем: превью или оригинал.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Миниатюра (быстрая, маленькая) — рисуется в ленте.
    Thumb,
    /// Оригинальный файл — грузится по явному действию пользователя.
    Full,
}

impl MediaKind {
    /// Строковое представление — оно же часть имени файла и поля WS-события.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Thumb => "thumb",
            MediaKind::Full => "full",
        }
    }

    /// Разбор значения query-параметра `kind`.
    ///
    /// Отсутствие параметра (и пустая строка) — это `full`: запрос без
    /// уточнения означает «дай файл». Всё остальное — `None`, то есть
    /// `400` у вызывающего: молча подставлять `full` вместо опечатки
    /// `thmub` значило бы скачивать мегабайты там, где просили превью.
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Some(MediaKind::Full),
            Some("full") => Some(MediaKind::Full),
            Some("thumb") => Some(MediaKind::Thumb),
            Some(_) => None,
        }
    }

    /// Все варианты — для инвалидации, которая обязана снести оба файла.
    pub fn all() -> [MediaKind; 2] {
        [MediaKind::Thumb, MediaKind::Full]
    }
}

impl std::fmt::Display for MediaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Проверенный ключ записи кэша.
///
/// Существует ровно для того, чтобы «сырые» строки из URL нельзя было
/// донести до файловой системы: собрать [`CacheKey`] можно только через
/// [`CacheKey::parse`], а он разбирает идентификаторы в числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Диалог в Bot API-формате (`777000`, `-100…`).
    pub dialog_id: i64,
    /// Идентификатор сообщения (в Telegram он 32-битный и положительный).
    pub message_id: i32,
    /// Превью или оригинал.
    pub kind: MediaKind,
}

impl CacheKey {
    /// Разбирает параметры запроса. `None` ⇒ клиент прислал мусор.
    pub fn parse(dialog_id: &str, message_id: &str, kind: Option<&str>) -> Option<Self> {
        let dialog_id: i64 = dialog_id.trim().parse().ok()?;
        let message_id: i32 = message_id.trim().parse().ok()?;
        if message_id <= 0 {
            return None;
        }
        Some(Self {
            dialog_id,
            message_id,
            kind: MediaKind::parse(kind)?,
        })
    }

    /// Ключ дедупликации одновременных скачиваний (`state.media_inflight`).
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.dialog_id, self.message_id, self.kind)
    }

    /// `dialog_id` строкой — в таком виде он уходит в DTO и WS-события.
    pub fn dialog_id_string(&self) -> String {
        self.dialog_id.to_string()
    }

    /// Базовое имя файлов записи: `<message_id>.<kind>`.
    fn stem(&self) -> String {
        format!("{}.{}", self.message_id, self.kind)
    }
}

/// Метаданные записи (сайдкар `.meta.json`).
///
/// Хранятся отдельно от байтов, потому что именно наличие меты — признак
/// целостности `bin` (см. шапку модуля), и потому что отдавать
/// `Content-Type` нужно, не заглядывая внутрь файла.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMeta {
    /// MIME из Telegram (`image/jpeg`, `video/mp4`, …).
    pub mime: Option<String>,
    /// Размер в байтах на момент записи.
    pub size: u64,
    /// Оригинальное имя файла, если Telegram его прислал.
    pub file_name: Option<String>,
    /// Ширина, если это изображение/видео.
    #[serde(default)]
    pub width: Option<i32>,
    /// Высота, если это изображение/видео.
    #[serde(default)]
    pub height: Option<i32>,
    /// Длительность в секундах (видео/аудио/голосовые).
    #[serde(default)]
    pub duration_secs: Option<i32>,
    /// Unix-время записи в кэш (диагностика; LRU считает по `mtime`).
    #[serde(default)]
    pub cached_at: i64,
}

impl MediaMeta {
    /// Мета с проставленным `cached_at = now`.
    pub fn now(mime: Option<String>, size: u64, file_name: Option<String>) -> Self {
        Self {
            mime,
            size,
            file_name,
            cached_at: unix_now(),
            ..Self::default()
        }
    }
}

/// Найденная в кэше запись.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMedia {
    /// Путь к байтам.
    pub path: PathBuf,
    /// Метаданные; `size` уже приведён к фактическому размеру файла.
    pub meta: MediaMeta,
}

/// Корень медиа-кэша.
pub fn media_root(cfg: &TelegramConfig) -> PathBuf {
    cfg.cache_dir.join(MEDIA_SUBDIR)
}

/// Каталог одного диалога.
pub fn dialog_dir(cfg: &TelegramConfig, dialog_id: i64) -> PathBuf {
    media_root(cfg).join(dialog_id.to_string())
}

/// Пути записи: `(bin, meta)`.
pub fn path_for(cfg: &TelegramConfig, key: &CacheKey) -> (PathBuf, PathBuf) {
    let dir = dialog_dir(cfg, key.dialog_id);
    let stem = key.stem();
    (
        dir.join(format!("{stem}.bin")),
        dir.join(format!("{stem}.meta.json")),
    )
}

/// Текущее unix-время в секундах.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Читает запись из кэша.
///
/// Попаданием считается ТОЛЬКО пара «непустой `bin` + разбираемая `meta`».
/// Осиротевший `bin` (запись оборвалась до меты) и брошенный `.tmp`
/// попаданием не являются — иначе пользователь получил бы обрезанный файл
/// с `Cache-Control: immutable`, то есть навсегда.
pub async fn get(cfg: &TelegramConfig, key: &CacheKey) -> Option<CachedMedia> {
    let (bin, meta_path) = path_for(cfg, key);
    let len = tokio::fs::metadata(&bin).await.ok()?.len();
    if len == 0 {
        return None;
    }
    let raw = tokio::fs::read(&meta_path).await.ok()?;
    let mut meta: MediaMeta = serde_json::from_slice(&raw).ok()?;
    // Фактический размер важнее записанного: по нему считается Content-Length.
    meta.size = len;
    Some(CachedMedia { path: bin, meta })
}

/// Есть ли запись в кэше (без чтения меты целиком).
pub async fn contains(cfg: &TelegramConfig, key: &CacheKey) -> bool {
    get(cfg, key).await.is_some()
}

/// Создаёт каталог диалога и возвращает уникальный путь staging-файла.
///
/// Staging лежит В ТОМ ЖЕ каталоге, что и цель: `rename` между разными
/// файловыми системами не атомарен (и на macOS/Linux вообще падает с
/// `EXDEV`), а `/tmp` вполне может оказаться другой ФС.
///
/// # Errors
///
/// Ошибка создания каталога кэша.
pub async fn begin_staging(cfg: &TelegramConfig, key: &CacheKey) -> std::io::Result<PathBuf> {
    let dir = dialog_dir(cfg, key.dialog_id);
    tokio::fs::create_dir_all(&dir).await?;
    let name = format!("{}.tmp.{}", key.stem(), uuid::Uuid::new_v4());
    Ok(dir.join(name))
}

/// Переносит готовый staging-файл в кэш и дописывает мету.
///
/// Вызывающий обязан ДО этого сбросить свои буферы (`flush`/`sync_all`):
/// `rename` переносит имя, а не содержимое страничного кэша.
///
/// # Errors
///
/// Ошибки `rename`/записи меты. При ошибке staging НЕ удаляется — это
/// делает вызывающий (у него есть guard, снимающий файл и при панике).
pub async fn commit(
    cfg: &TelegramConfig,
    key: &CacheKey,
    staging: &Path,
    meta: &MediaMeta,
) -> std::io::Result<PathBuf> {
    let (bin, meta_path) = path_for(cfg, key);
    if let Some(dir) = bin.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    // Мета пишется ПОСЛЕ байтов: пока её нет, запись не считается валидной.
    tokio::fs::rename(staging, &bin).await?;
    let json = serde_json::to_vec_pretty(meta).unwrap_or_else(|_| b"{}".to_vec());
    if let Err(e) = write_atomic(&meta_path, &json).await {
        // Байты уже на месте, но без меты кэш их не отдаст. Убираем bin,
        // чтобы не копить «невидимый» мусор, который посчитает только sweeper.
        let _ = tokio::fs::remove_file(&bin).await;
        return Err(e);
    }
    Ok(bin)
}

/// Кладёт в кэш готовые байты (превью, тесты).
///
/// Для больших файлов не годится — они пишутся потоково через
/// [`begin_staging`] + [`commit`], чтобы не держать гигабайт в памяти.
///
/// # Errors
///
/// Любая ошибка ФС на пути «создать каталог → записать staging → rename».
pub async fn put_atomic(
    cfg: &TelegramConfig,
    key: &CacheKey,
    bytes: &[u8],
    meta: &MediaMeta,
) -> std::io::Result<PathBuf> {
    let staging = begin_staging(cfg, key).await?;
    if let Err(e) = write_file_synced(&staging, bytes).await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(e);
    }
    match commit(cfg, key, &staging, meta).await {
        Ok(path) => Ok(path),
        Err(e) => {
            let _ = tokio::fs::remove_file(&staging).await;
            Err(e)
        }
    }
}

/// Пишет файл и сбрасывает его на диск.
async fn write_file_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::File::create(path).await?;
    f.write_all(bytes).await?;
    f.flush().await?;
    f.sync_all().await
}

/// Атомарная запись файла: `<name>.tmp.<uuid>` в том же каталоге → `rename`.
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "cache".to_string());
    let tmp = dir.join(format!("{name}.tmp.{}", uuid::Uuid::new_v4()));
    if let Err(e) = write_file_synced(&tmp, bytes).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}

/// Удаляет из кэша ВСЕ файлы сообщения (оба `kind`, мету и брошенные `.tmp`).
///
/// Зовётся из updates-воркера на `message_edited` / `message_deleted`:
/// ответ роута помечен `Cache-Control: immutable`, поэтому без инвалидации
/// отредактированное медиа осталось бы старым навсегда.
///
/// `dialog_id` приходит из события и бывает ПУСТЫМ (удаление в ЛС Telegram
/// не привязывает к диалогу — см. `updates::deleted_event`). В этом случае
/// подчищаются одноимённые записи во всех каталогах: промах невозможен, а
/// лишний обход дешёвый (каталогов столько же, сколько диалогов с медиа).
///
/// Возвращает число удалённых файлов. Ошибки ФС не считаются фатальными:
/// кэш — это ускоритель, а не источник истины.
pub async fn invalidate(cfg: &TelegramConfig, dialog_id: &str, message_id: i32) -> usize {
    if message_id <= 0 {
        return 0;
    }
    let prefix = format!("{message_id}.");
    match dialog_id.trim().parse::<i64>() {
        Ok(id) => remove_prefixed(&dialog_dir(cfg, id), &prefix).await,
        Err(_) => {
            let mut removed = 0;
            for dir in dialog_dirs(cfg).await {
                removed += remove_prefixed(&dir, &prefix).await;
            }
            removed
        }
    }
}

/// Удаляет из каталога всё, что начинается с `prefix`.
async fn remove_prefixed(dir: &Path, prefix: &str) -> usize {
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(prefix) {
            continue;
        }
        if tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Каталоги диалогов внутри кэша.
async fn dialog_dirs(cfg: &TelegramConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(media_root(cfg)).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            out.push(entry.path());
        }
    }
    out
}

/// Суммарный размер кэша в байтах.
pub async fn total_bytes(cfg: &TelegramConfig) -> u64 {
    let mut total = 0;
    for dir in dialog_dirs(cfg).await {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Ok(md) = entry.metadata().await {
                if md.is_file() {
                    total += md.len();
                }
            }
        }
    }
    total
}

/// Обновляет `mtime` файла — «этим воспользовались только что».
///
/// Best-effort: ошибки проглатываются, потому что промах LRU не стоит
/// сломанной отдачи файла. Работа синхронная и уходит в blocking-пул
/// (`std::fs::File::set_modified` у tokio-обёртки нет).
pub async fn touch(path: PathBuf) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
    })
    .await;
}

/// Одна запись кэша глазами sweeper'а.
#[derive(Debug, Clone)]
struct Entry {
    /// Файл с байтами.
    bin: PathBuf,
    /// Сайдкар меты (может отсутствовать — тогда запись битая и уйдёт первой).
    meta: PathBuf,
    /// Сайдкар посчитанной огибающей (есть только у аудио, см.
    /// [`forge_voice::waveform::SIDECAR_SUFFIX`]).
    wave: PathBuf,
    /// Суммарный вес записи со всеми сайдкарами.
    bytes: u64,
    /// Время последнего использования (приближённо).
    mtime: SystemTime,
    /// Ключ дедупа — по нему проверяется «сейчас качается».
    id: String,
}

/// Итог прогона подчистки.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepStats {
    /// Сколько записей просмотрено.
    pub scanned: usize,
    /// Сколько записей удалено.
    pub removed: usize,
    /// Сколько байт освобождено.
    pub freed: u64,
    /// Размер кэша до прогона.
    pub total_before: u64,
    /// Размер кэша после прогона.
    pub total_after: u64,
    /// Сколько брошенных staging-файлов подчищено.
    pub stale_staging: usize,
}

/// Целевой размер, до которого подметаем (90 % лимита — гистерезис).
pub fn sweep_target(max_bytes: u64) -> u64 {
    max_bytes / SWEEP_TARGET_DEN * SWEEP_TARGET_NUM
}

/// Один прогон LRU-подчистки.
///
/// Порядок удаления — по `mtime` от старых к новым, поэтому «горячие»
/// файлы (их `mtime` обновляет [`touch`] на каждом попадании) выживают.
/// Записи из `protected` (ключи [`CacheKey::id`] активных загрузок)
/// пропускаются: удалить файл, который прямо сейчас качается, значит
/// отдать клиенту пустоту.
pub async fn sweep_once(cfg: &TelegramConfig, protected: &HashSet<String>) -> SweepStats {
    let mut stats = SweepStats::default();
    let mut entries: Vec<Entry> = Vec::new();
    let now = SystemTime::now();

    for dir in dialog_dirs(cfg).await {
        let dialog_id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i64>().ok());
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(md) = entry.metadata().await else {
                continue;
            };
            if !md.is_file() {
                continue;
            }
            stats.total_before += md.len();

            // Брошенный staging старше TTL — мусор упавшего процесса.
            if name.contains(".tmp.") {
                let age = md
                    .modified()
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .unwrap_or_default();
                if age > STAGING_TTL && tokio::fs::remove_file(entry.path()).await.is_ok() {
                    stats.stale_staging += 1;
                    stats.freed += md.len();
                }
                continue;
            }
            let Some(stem) = name.strip_suffix(".bin") else {
                continue; // .meta.json и .wave.json учитываются со своим .bin
            };
            let meta = dir.join(format!("{stem}.meta.json"));
            let meta_len = tokio::fs::metadata(&meta)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            // Огибающая ходит за байтами: считаться она может только по ним, и
            // пережить их не должна — иначе после подчистки в каталоге
            // остались бы вечные сироты, которые никто уже не удалит.
            let wave = dir.join(format!(
                "{stem}{}",
                forge_voice::waveform::SIDECAR_SUFFIX
            ));
            let wave_len = tokio::fs::metadata(&wave)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let id = dialog_id
                .map(|d| format!("{d}/{stem}"))
                .unwrap_or_else(|| stem.to_string());
            entries.push(Entry {
                bin: entry.path(),
                meta,
                wave,
                bytes: md.len() + meta_len + wave_len,
                mtime: md.modified().unwrap_or(UNIX_EPOCH),
                id,
            });
        }
    }

    stats.scanned = entries.len();
    stats.total_after = stats.total_before - stats.freed;
    let target = sweep_target(cfg.max_cache_bytes);
    if stats.total_after <= cfg.max_cache_bytes {
        return stats;
    }

    entries.sort_by_key(|e| e.mtime);
    for e in entries {
        if stats.total_after <= target {
            break;
        }
        if protected.contains(&e.id) {
            continue;
        }
        if tokio::fs::remove_file(&e.bin).await.is_err() {
            continue;
        }
        let _ = tokio::fs::remove_file(&e.meta).await;
        let _ = tokio::fs::remove_file(&e.wave).await;
        stats.removed += 1;
        stats.freed += e.bytes;
        stats.total_after = stats.total_after.saturating_sub(e.bytes);
    }
    stats
}

/// Ключи медиа, которые сейчас качаются (их sweeper не трогает).
async fn protected_keys(state: &Arc<TelegramState>) -> HashSet<String> {
    state
        .media_inflight
        .lock()
        .await
        .keys()
        // Ключ inflight — "<did>/<mid>/<kind>", ключ файла — "<did>/<mid>.<kind>".
        .filter_map(|k| {
            let (head, kind) = k.rsplit_once('/')?;
            Some(format!("{head}.{kind}"))
        })
        .collect()
}

/// Фоновый воркер LRU-подчистки: прогон на старте и далее раз в
/// [`SWEEP_INTERVAL`]. Останавливается по `shutdown`-токену.
pub async fn sweeper_loop(state: Arc<TelegramState>) {
    loop {
        let protected = protected_keys(&state).await;
        let stats = sweep_once(&state.config, &protected).await;
        if stats.removed > 0 || stats.stale_staging > 0 {
            tracing::info!(
                target: "forge_telegram",
                scanned = stats.scanned,
                removed = stats.removed,
                stale_staging = stats.stale_staging,
                freed = stats.freed,
                total_after = stats.total_after,
                limit = state.config.max_cache_bytes,
                "forge-telegram: media cache swept"
            );
        } else {
            tracing::debug!(
                target: "forge_telegram",
                scanned = stats.scanned,
                total = stats.total_after,
                limit = state.config.max_cache_bytes,
                "forge-telegram: media cache within limits"
            );
        }

        tokio::select! {
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_telegram", "forge-telegram: media sweeper stopped");
                return;
            }
            _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
        }
    }
}

/// Запускает воркер подчистки и регистрирует его для graceful shutdown.
pub async fn spawn_sweeper(state: &Arc<TelegramState>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { sweeper_loop(st).await });
    state.register_worker(handle).await;
    tracing::info!(
        target: "forge_telegram",
        interval_secs = SWEEP_INTERVAL.as_secs(),
        limit_bytes = state.config.max_cache_bytes,
        "forge-telegram: media cache sweeper started"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> TelegramConfig {
        TelegramConfig {
            session_path: dir.join("telegram.session"),
            cache_dir: dir.join("cache"),
            prompts_path: dir.join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        }
    }

    fn key(kind: MediaKind) -> CacheKey {
        CacheKey {
            dialog_id: -1001234567890,
            message_id: 42,
            kind,
        }
    }

    fn meta_of(size: u64) -> MediaMeta {
        MediaMeta::now(Some("image/jpeg".into()), size, Some("photo.jpg".into()))
    }

    #[test]
    fn kind_parse_whitelist() {
        assert_eq!(MediaKind::parse(None), Some(MediaKind::Full));
        assert_eq!(MediaKind::parse(Some("")), Some(MediaKind::Full));
        assert_eq!(MediaKind::parse(Some(" full ")), Some(MediaKind::Full));
        assert_eq!(MediaKind::parse(Some("thumb")), Some(MediaKind::Thumb));
        // Опечатка обязана быть видна как ошибка, а не «наверное, full».
        assert_eq!(MediaKind::parse(Some("thmub")), None);
        assert_eq!(MediaKind::parse(Some("FULL")), None);
        assert_eq!(MediaKind::parse(Some("../full")), None);
    }

    /// Главный тест безопасности: ничего похожего на путь в ключ не проходит.
    #[test]
    fn parse_rejects_path_traversal() {
        for bad in [
            "../../etc",
            "..",
            "/etc/passwd",
            "1/../../2",
            "abc",
            "",
            "1e3",
        ] {
            assert!(
                CacheKey::parse(bad, "7", Some("full")).is_none(),
                "dialog_id {bad:?} не должен парситься"
            );
            assert!(
                CacheKey::parse("777000", bad, Some("full")).is_none(),
                "message_id {bad:?} не должен парситься"
            );
        }
        // Отрицательный/нулевой message_id тоже мусор.
        assert!(CacheKey::parse("777000", "0", None).is_none());
        assert!(CacheKey::parse("777000", "-5", None).is_none());
        // А нормальные значения — проходят, в том числе отрицательный канал.
        let k = CacheKey::parse("-1001234567890", "42", Some("thumb")).unwrap();
        assert_eq!(k.dialog_id, -1001234567890);
        assert_eq!(k.message_id, 42);
        assert_eq!(k.kind, MediaKind::Thumb);
        assert_eq!(k.id(), "-1001234567890/42/thumb");
    }

    /// Пути записи всегда лежат ВНУТРИ cache_dir.
    #[test]
    fn paths_stay_inside_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let (bin, meta) = path_for(&cfg, &key(MediaKind::Full));
        assert!(bin.starts_with(&cfg.cache_dir), "{}", bin.display());
        assert!(meta.starts_with(&cfg.cache_dir), "{}", meta.display());
        assert!(bin.to_string_lossy().ends_with("42.full.bin"));
        assert!(meta.to_string_lossy().ends_with("42.full.meta.json"));
    }

    #[tokio::test]
    async fn put_then_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        assert!(get(&cfg, &k).await.is_none(), "пустой кэш — это промах");

        let bytes = b"binary-payload".to_vec();
        put_atomic(&cfg, &k, &bytes, &meta_of(bytes.len() as u64))
            .await
            .unwrap();

        let hit = get(&cfg, &k).await.expect("после записи ожидается попадание");
        assert_eq!(hit.meta.mime.as_deref(), Some("image/jpeg"));
        assert_eq!(hit.meta.file_name.as_deref(), Some("photo.jpg"));
        assert_eq!(hit.meta.size, bytes.len() as u64);
        assert_eq!(tokio::fs::read(&hit.path).await.unwrap(), bytes);

        // Другой kind — независимая запись.
        assert!(get(&cfg, &key(MediaKind::Thumb)).await.is_none());
    }

    /// Оборванная запись не должна выглядеть попаданием: ни брошенный
    /// `.tmp`, ни `bin` без меты, ни пустой `bin`.
    #[tokio::test]
    async fn interrupted_write_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);

        // 1. Остался только staging.
        let staging = begin_staging(&cfg, &k).await.unwrap();
        tokio::fs::write(&staging, b"half").await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "staging — не кэш");

        // 2. Есть bin, но нет меты.
        let (bin, meta_path) = path_for(&cfg, &k);
        tokio::fs::rename(&staging, &bin).await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "bin без меты — не кэш");

        // 3. Мета битая.
        tokio::fs::write(&meta_path, b"{not json").await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "битая мета — не кэш");

        // 4. Пустой bin с валидной метой.
        tokio::fs::write(&bin, b"").await.unwrap();
        let json = serde_json::to_vec(&meta_of(0)).unwrap();
        tokio::fs::write(&meta_path, &json).await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "пустой bin — не кэш");
    }

    #[tokio::test]
    async fn invalidate_removes_every_file_of_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        for kind in MediaKind::all() {
            put_atomic(&cfg, &key(kind), b"data", &meta_of(4))
                .await
                .unwrap();
        }
        // Плюс брошенный staging того же сообщения.
        let staging = begin_staging(&cfg, &key(MediaKind::Full)).await.unwrap();
        tokio::fs::write(&staging, b"junk").await.unwrap();
        // И чужое сообщение, которое трогать нельзя.
        let other = CacheKey {
            message_id: 43,
            ..key(MediaKind::Full)
        };
        put_atomic(&cfg, &other, b"keep", &meta_of(4)).await.unwrap();

        let removed = invalidate(&cfg, "-1001234567890", 42).await;
        assert_eq!(removed, 5, "2 bin + 2 meta + staging");
        assert!(get(&cfg, &key(MediaKind::Full)).await.is_none());
        assert!(get(&cfg, &key(MediaKind::Thumb)).await.is_none());
        assert!(get(&cfg, &other).await.is_some(), "чужая запись уцелела");
        assert!(!staging.exists());
    }

    /// Удаление в ЛС приходит без `dialog_id` — чистим по всем каталогам.
    #[tokio::test]
    async fn invalidate_without_dialog_scans_every_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let a = CacheKey {
            dialog_id: 777000,
            message_id: 9,
            kind: MediaKind::Full,
        };
        let b = CacheKey {
            dialog_id: -100999,
            message_id: 9,
            kind: MediaKind::Thumb,
        };
        put_atomic(&cfg, &a, b"a", &meta_of(1)).await.unwrap();
        put_atomic(&cfg, &b, b"b", &meta_of(1)).await.unwrap();

        assert_eq!(invalidate(&cfg, "", 9).await, 4);
        assert!(get(&cfg, &a).await.is_none());
        assert!(get(&cfg, &b).await.is_none());
    }

    /// Параллельные записи одного ключа не портят файл: каждая пишет в свой
    /// staging, побеждает последний `rename`, и результат — целое значение
    /// одного из писателей, а не их смесь.
    #[tokio::test]
    async fn concurrent_put_atomic_keeps_the_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(cfg(dir.path()));
        let k = key(MediaKind::Full);

        let mut tasks = Vec::new();
        for i in 0..8u8 {
            let cfg = cfg.clone();
            tasks.push(tokio::spawn(async move {
                let payload = vec![b'a' + i; 4096];
                put_atomic(&cfg, &k, &payload, &meta_of(payload.len() as u64))
                    .await
                    .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let hit = get(&cfg, &k).await.unwrap();
        let bytes = tokio::fs::read(&hit.path).await.unwrap();
        assert_eq!(bytes.len(), 4096);
        let first = bytes[0];
        assert!(
            bytes.iter().all(|b| *b == first),
            "файл смешал данные разных писателей"
        );
        // Мусора не осталось.
        let mut rd = tokio::fs::read_dir(dialog_dir(&cfg, k.dialog_id))
            .await
            .unwrap();
        let mut names = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            names.push(e.file_name().to_string_lossy().to_string());
        }
        names.sort();
        assert_eq!(names, vec!["42.full.bin", "42.full.meta.json"]);
    }

    #[tokio::test]
    async fn total_bytes_counts_everything() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        assert_eq!(total_bytes(&cfg).await, 0);
        put_atomic(&cfg, &key(MediaKind::Full), &vec![0u8; 1000], &meta_of(1000))
            .await
            .unwrap();
        let total = total_bytes(&cfg).await;
        assert!(total > 1000, "мета тоже весит: {total}");
    }

    /// Sweeper удаляет самые старые записи, пока не уложится в 90 % лимита,
    /// и не трогает то, что сейчас качается.
    #[tokio::test]
    async fn sweep_drops_oldest_and_keeps_inflight() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        // Лимит 5 КиБ: четыре записи по 2 КиБ обязаны ужаться до ≤ 4.5 КиБ.
        cfg.max_cache_bytes = 5 * 1024;

        let mut keys = Vec::new();
        for i in 1..=4i32 {
            let k = CacheKey {
                dialog_id: 777000,
                message_id: i,
                kind: MediaKind::Full,
            };
            put_atomic(&cfg, &k, &vec![7u8; 2048], &meta_of(2048))
                .await
                .unwrap();
            keys.push(k);
        }
        // Явно расставляем «возраст»: #1 самый старый, #4 самый свежий.
        for (n, k) in keys.iter().enumerate() {
            let (bin, _) = path_for(&cfg, k);
            let when = SystemTime::now() - Duration::from_secs(1000 - n as u64 * 100);
            let f = std::fs::OpenOptions::new().write(true).open(&bin).unwrap();
            f.set_modified(when).unwrap();
        }

        // #1 «качается» — его трогать нельзя, значит уйдут #2 и #3.
        let protected: HashSet<String> = ["777000/1.full".to_string()].into_iter().collect();
        let stats = sweep_once(&cfg, &protected).await;

        assert!(stats.removed >= 1, "sweeper обязан был что-то удалить");
        assert!(
            stats.total_after <= sweep_target(cfg.max_cache_bytes),
            "после подчистки {} > цели {}",
            stats.total_after,
            sweep_target(cfg.max_cache_bytes)
        );
        assert!(
            get(&cfg, &keys[0]).await.is_some(),
            "активная загрузка не должна быть удалена"
        );
        assert!(
            get(&cfg, &keys[3]).await.is_some(),
            "самая свежая запись должна была уцелеть"
        );
    }

    /// Пока кэш в пределах лимита, sweeper ничего не удаляет.
    #[tokio::test]
    async fn sweep_is_a_noop_below_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path()); // лимит по умолчанию — 512 МиБ
        let k = key(MediaKind::Full);
        put_atomic(&cfg, &k, &vec![1u8; 4096], &meta_of(4096))
            .await
            .unwrap();

        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.scanned, 1);
        assert!(get(&cfg, &k).await.is_some());
    }

    /// Брошенный staging старше TTL удаляется даже при пустом кэше.
    #[tokio::test]
    async fn sweep_removes_stale_staging() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let staging = begin_staging(&cfg, &key(MediaKind::Full)).await.unwrap();
        tokio::fs::write(&staging, b"orphan").await.unwrap();
        let old = SystemTime::now() - STAGING_TTL - Duration::from_secs(60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&staging)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.stale_staging, 1);
        assert!(!staging.exists());
    }

    /// Свежий staging (идёт загрузка) НЕ удаляется.
    #[tokio::test]
    async fn sweep_keeps_fresh_staging() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let staging = begin_staging(&cfg, &key(MediaKind::Full)).await.unwrap();
        tokio::fs::write(&staging, b"in progress").await.unwrap();

        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.stale_staging, 0);
        assert!(staging.exists());
    }

    #[tokio::test]
    async fn touch_updates_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        let bin = put_atomic(&cfg, &k, b"x", &meta_of(1)).await.unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(old)
            .unwrap();

        touch(bin.clone()).await;
        let after = std::fs::metadata(&bin).unwrap().modified().unwrap();
        assert!(after > old, "mtime не обновился");
    }

    /// Ключи в реестре загрузок (`<did>/<mid>/<kind>`) и имена файлов
    /// (`<did>/<mid>.<kind>`) устроены по-разному, поэтому перевод между
    /// ними — отдельная проверяемая деталь: разъедься он, и sweeper начал бы
    /// удалять файлы прямо во время скачивания.
    #[tokio::test]
    async fn protected_keys_match_file_naming() {
        let dir = tempfile::tempdir().unwrap();
        let state = TelegramState::new(cfg(dir.path()));
        let k = key(MediaKind::Full);
        state
            .media_inflight
            .lock()
            .await
            .insert(k.id(), Arc::new(tokio::sync::Notify::new()));

        let protected = protected_keys(&state).await;
        // Ровно тот вид, в котором sweep_once собирает Entry::id.
        assert!(
            protected.contains("-1001234567890/42.full"),
            "{protected:?}"
        );

        // И это работает end-to-end: запись под защитой не удаляется даже
        // при лимите, который требует снести всё.
        let mut cfg = cfg(dir.path());
        cfg.max_cache_bytes = 1;
        put_atomic(&cfg, &k, &vec![9u8; 4096], &meta_of(4096))
            .await
            .unwrap();
        let stats = sweep_once(&cfg, &protected).await;
        assert_eq!(stats.removed, 0, "защищённая запись была удалена");
        assert!(get(&cfg, &k).await.is_some());
    }

    #[test]
    fn sweep_target_leaves_hysteresis() {
        assert_eq!(sweep_target(1000), 900);
        assert_eq!(sweep_target(512 * 1024 * 1024), 483183819);
        assert_eq!(sweep_target(0), 0);
    }

    /// Числовые границы типов: сами границы валидны, переполнение на
    /// единицу — мусор (а не молчаливое усечение), null-байты и
    /// не-ASCII-цифры не проходят вовсе.
    #[test]
    fn parse_handles_numeric_boundaries() {
        let k = CacheKey::parse("9223372036854775807", "2147483647", None).unwrap();
        assert_eq!(k.dialog_id, i64::MAX);
        assert_eq!(k.message_id, i32::MAX);
        assert!(CacheKey::parse("-9223372036854775808", "1", None).is_some());

        // i64::MAX+1 / i64::MIN-1 / i32::MAX+1 — за границей типа.
        assert!(CacheKey::parse("9223372036854775808", "1", None).is_none());
        assert!(CacheKey::parse("-9223372036854775809", "1", None).is_none());
        assert!(CacheKey::parse("1", "2147483648", None).is_none());
        // i32::MIN проходит парсер числа, но режется проверкой message_id > 0.
        assert!(CacheKey::parse("1", "-2147483648", None).is_none());

        // Null-байты (классическая инъекция в путь) и юникод-цифры.
        assert!(CacheKey::parse("42\0", "7", None).is_none());
        assert!(CacheKey::parse("42", "7\0", None).is_none());
        assert!(CacheKey::parse("\u{664}\u{662}", "7", None).is_none()); // ٤٢
        assert!(CacheKey::parse("42", "\u{667}", None).is_none()); // ٧
        assert_eq!(MediaKind::parse(Some("full\0")), None);

        // «+42» и «042» Rust-парсер принимает, но нормализует в каноническое
        // число — на диске это ОДИН путь, а не три разных файла.
        assert_eq!(CacheKey::parse("+42", "042", None).unwrap().id(), "42/42/full");
    }

    /// Инвалидация сообщения 4 не должна зацепить 42 и 420: префикс
    /// сравнивается вместе с точкой, а не как «начинается с цифр».
    #[tokio::test]
    async fn invalidate_matches_whole_message_id_not_decimal_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mk = |id: i32| CacheKey {
            dialog_id: 777000,
            message_id: id,
            kind: MediaKind::Full,
        };
        for id in [4, 42, 420] {
            put_atomic(&cfg, &mk(id), b"x", &meta_of(1)).await.unwrap();
        }

        let removed = invalidate(&cfg, "777000", 4).await;
        assert_eq!(removed, 2, "bin + meta только сообщения 4");
        assert!(get(&cfg, &mk(4)).await.is_none());
        assert!(get(&cfg, &mk(42)).await.is_some(), "42 — не префикс-совпадение");
        assert!(get(&cfg, &mk(420)).await.is_some(), "420 — не префикс-совпадение");
    }

    /// Событие с мусорным message_id (0/отрицательный) не должно ничего
    /// удалять — в том числе в режиме «скан всех каталогов» при пустом
    /// dialog_id.
    #[tokio::test]
    async fn invalidate_ignores_nonpositive_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        put_atomic(&cfg, &k, b"data", &meta_of(4)).await.unwrap();

        assert_eq!(invalidate(&cfg, "-1001234567890", 0).await, 0);
        assert_eq!(invalidate(&cfg, "-1001234567890", -42).await, 0);
        assert_eq!(invalidate(&cfg, "", -1).await, 0);
        assert!(get(&cfg, &k).await.is_some(), "запись не должна пострадать");
    }

    /// `Content-Length` считается по фактическому размеру файла, а не по
    /// записанному в мете: файл могли усечь извне, а мета могла соврать.
    #[tokio::test]
    async fn get_reports_actual_size_not_recorded_meta_size() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        put_atomic(&cfg, &k, &[1u8; 100], &meta_of(999_999))
            .await
            .unwrap();

        let hit = get(&cfg, &k).await.unwrap();
        assert_eq!(hit.meta.size, 100, "размер — по факту, а не по мете");
    }

    /// Мета-сирота (bin удалили извне) — промах, а не паника и не попадание
    /// с несуществующим путём.
    #[tokio::test]
    async fn meta_without_bin_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        put_atomic(&cfg, &k, b"data", &meta_of(4)).await.unwrap();

        let (bin, meta_path) = path_for(&cfg, &k);
        tokio::fs::remove_file(&bin).await.unwrap();
        assert!(meta_path.exists(), "мета осталась сиротой");
        assert!(get(&cfg, &k).await.is_none());
        assert!(!contains(&cfg, &k).await);
    }

    /// Мета старого формата (записана до появления width/height/duration и
    /// cached_at) обязана читаться: иначе апгрейд версии молча обнулил бы
    /// весь накопленный кэш и всё перекачалось бы заново.
    #[tokio::test]
    async fn legacy_meta_without_new_fields_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        let (bin, meta_path) = path_for(&cfg, &k);
        tokio::fs::create_dir_all(bin.parent().unwrap()).await.unwrap();
        tokio::fs::write(&bin, b"png").await.unwrap();
        tokio::fs::write(&meta_path, br#"{"mime":"image/png","size":3}"#)
            .await
            .unwrap();

        let hit = get(&cfg, &k).await.expect("старый формат меты обязан читаться");
        assert_eq!(hit.meta.mime.as_deref(), Some("image/png"));
        assert_eq!(hit.meta.file_name, None);
        assert_eq!(hit.meta.width, None);
        assert_eq!(hit.meta.height, None);
        assert_eq!(hit.meta.duration_secs, None);
        assert_eq!(hit.meta.cached_at, 0);
    }

    /// Мета «из будущего» (новая версия добавила поля, потом откатились) —
    /// лишние ключи игнорируются, кэш выживает после даунгрейда.
    #[tokio::test]
    async fn meta_with_unknown_future_fields_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        let (bin, meta_path) = path_for(&cfg, &k);
        tokio::fs::create_dir_all(bin.parent().unwrap()).await.unwrap();
        tokio::fs::write(&bin, b"data").await.unwrap();
        tokio::fs::write(
            &meta_path,
            br#"{"mime":"image/jpeg","size":4,"file_name":"a.jpg","future_flag":true,"nested":{"x":[1,2]}}"#,
        )
        .await
        .unwrap();

        let hit = get(&cfg, &k).await.expect("лишние поля не должны ломать разбор");
        assert_eq!(hit.meta.mime.as_deref(), Some("image/jpeg"));
        assert_eq!(hit.meta.file_name.as_deref(), Some("a.jpg"));
    }

    /// Граница гистерезиса: ровно на лимите — ещё не перебор, один байт
    /// сверх — уже подчистка до цели.
    #[tokio::test]
    async fn sweep_is_noop_exactly_at_limit_and_wakes_one_byte_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        for i in 1..=2i32 {
            let k = CacheKey {
                dialog_id: 777000,
                message_id: i,
                kind: MediaKind::Full,
            };
            put_atomic(&cfg, &k, &vec![7u8; 2048], &meta_of(2048))
                .await
                .unwrap();
        }
        let total = total_bytes(&cfg).await;

        cfg.max_cache_bytes = total;
        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.removed, 0, "ровно на лимите удалять нечего");
        assert_eq!(stats.total_after, total);

        cfg.max_cache_bytes = total - 1;
        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert!(stats.removed >= 1, "байт сверх лимита обязан включить подчистку");
        assert!(stats.total_after <= sweep_target(cfg.max_cache_bytes));
    }

    /// Ничья по mtime не должна приводить к «снести всё»: цикл обязан
    /// остановиться, как только размер опустился до цели.
    #[tokio::test]
    async fn sweep_with_tied_mtimes_stops_at_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        let mut keys = Vec::new();
        for i in 1..=4i32 {
            let k = CacheKey {
                dialog_id: 777000,
                message_id: i,
                kind: MediaKind::Full,
            };
            put_atomic(&cfg, &k, &vec![7u8; 2048], &meta_of(2048))
                .await
                .unwrap();
            keys.push(k);
        }
        // Всем записям — ОДИН И ТОТ ЖЕ mtime.
        let when = SystemTime::now() - Duration::from_secs(500);
        for k in &keys {
            let (bin, _) = path_for(&cfg, k);
            std::fs::OpenOptions::new()
                .write(true)
                .open(&bin)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }

        // Лимит 0.7·total ⇒ цель 0.63·total ⇒ достаточно удалить ровно 2
        // записи из 4 (останется 0.5·total).
        let total = total_bytes(&cfg).await;
        cfg.max_cache_bytes = total * 7 / 10;
        let stats = sweep_once(&cfg, &HashSet::new()).await;

        assert_eq!(stats.removed, 2, "при ничьей удаляется ровно до цели, не всё");
        assert!(stats.total_after <= sweep_target(cfg.max_cache_bytes));
        let mut hits = 0;
        for k in &keys {
            if get(&cfg, k).await.is_some() {
                hits += 1;
            }
        }
        assert_eq!(hits, 2, "две записи обязаны уцелеть");
    }

    /// Стороннее содержимое в дереве кэша (файл в корне media/, каталог с
    /// нечисловым именем, подкаталог внутри каталога диалога) не роняет
    /// sweeper и не удаляется сверх необходимого.
    #[tokio::test]
    async fn sweep_survives_foreign_entries_in_cache_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        let k = CacheKey {
            dialog_id: 777000,
            message_id: 1,
            kind: MediaKind::Full,
        };
        put_atomic(&cfg, &k, &vec![7u8; 2048], &meta_of(2048))
            .await
            .unwrap();

        let root = media_root(&cfg);
        tokio::fs::write(root.join("README.txt"), b"stray").await.unwrap();
        let junk = root.join("exports"); // нечисловое имя ⇒ dialog_id = None
        tokio::fs::create_dir_all(&junk).await.unwrap();
        tokio::fs::write(junk.join("5.full.bin"), &vec![9u8; 2048])
            .await
            .unwrap();
        tokio::fs::write(junk.join("5.full.meta.json"), b"{\"size\":2048}")
            .await
            .unwrap();
        let nested = dialog_dir(&cfg, 777000).join("sub");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(nested.join("inner.dat"), b"keep").await.unwrap();

        // Под лимитом: ничего не удалено, обе bin-записи просканированы.
        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.scanned, 2, "bin диалога + bin в exports/");

        // Над лимитом: вычищаются обе записи, стороннее не тронуто.
        cfg.max_cache_bytes = 1;
        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.removed, 2);
        assert!(root.join("README.txt").exists(), "файл в корне вне обхода");
        assert!(nested.join("inner.dat").exists(), "подкаталог не трогается");
        assert!(!junk.join("5.full.bin").exists());
        assert!(get(&cfg, &k).await.is_none());
    }

    /// Часы прыгнули назад (NTP-коррекция): mtime staging'а оказался «в
    /// будущем». Возраст обязан считаться нулевым, а не переполняться в
    /// «очень старый» — иначе sweeper убил бы файл живой загрузки.
    #[tokio::test]
    async fn sweep_keeps_staging_with_future_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let staging = begin_staging(&cfg, &key(MediaKind::Full)).await.unwrap();
        tokio::fs::write(&staging, b"in progress").await.unwrap();
        let future = SystemTime::now() + Duration::from_secs(24 * 3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&staging)
            .unwrap()
            .set_modified(future)
            .unwrap();

        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.stale_staging, 0);
        assert!(staging.exists(), "staging с будущим mtime не должен удаляться");
    }

    /// Нулевой payload (в Telegram бывают пустые документы) записывается без
    /// ошибки, но НИКОГДА не отдаётся как попадание: пустой файл с
    /// `Cache-Control: immutable` — это навсегда сломанное медиа.
    #[tokio::test]
    async fn zero_byte_payload_is_never_served_as_hit() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        let bin = put_atomic(&cfg, &k, b"", &meta_of(0)).await.unwrap();
        assert!(bin.exists(), "запись прошла без ошибки");
        assert!(get(&cfg, &k).await.is_none(), "пустой bin — не попадание");
        assert!(!contains(&cfg, &k).await);
    }

    /// Staging исчез до commit (например, загрузка шла дольше TTL и sweeper
    /// его удалил): commit падает ошибкой и НЕ оставляет полузаписи —
    /// ни bin, ни меты.
    #[tokio::test]
    async fn commit_with_vanished_staging_fails_without_partial_state() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key(MediaKind::Full);
        let staging = begin_staging(&cfg, &k).await.unwrap();
        // Файл так и не был создан (или его уже удалили).
        assert!(commit(&cfg, &k, &staging, &meta_of(4)).await.is_err());

        let (bin, meta_path) = path_for(&cfg, &k);
        assert!(!bin.exists(), "после неудачного commit не должно быть bin");
        assert!(!meta_path.exists(), "и меты тоже");
        assert!(get(&cfg, &k).await.is_none());
    }

    /// На месте cache_dir лежит обычный файл: запись отдаёт ошибку (а не
    /// панику), чтение — промах, sweeper — тихий no-op.
    #[tokio::test]
    async fn cache_dir_being_a_file_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        tokio::fs::write(&cfg.cache_dir, b"not a dir").await.unwrap();

        let k = key(MediaKind::Full);
        assert!(begin_staging(&cfg, &k).await.is_err());
        assert!(put_atomic(&cfg, &k, b"x", &meta_of(1)).await.is_err());
        assert!(get(&cfg, &k).await.is_none());
        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats, SweepStats::default(), "sweep по битому корню — no-op");
    }
}
