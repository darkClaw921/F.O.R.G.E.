//! Файловый кэш принятых байтов: вложения и аватары.
//!
//! # Раскладка на диске
//!
//! ```text
//! <cache_dir>/media/<dialog>/<message_id>.<index>.file.bin        ← байты вложения
//! <cache_dir>/media/<dialog>/<message_id>.<index>.file.meta.json  ← метаданные
//! <cache_dir>/media/<dialog>/<message_id>.<index>.file.tmp.<uuid> ← staging
//! <cache_dir>/media/avatars/<user_id>.avatar.bin                  ← байты аватара
//! <cache_dir>/media/avatars/<user_id>.avatar.meta.json            ← метаданные
//! ```
//!
//! С Ф6 `<cache_dir>` — это каталог ОДНОГО портала
//! (`~/.forge/bitrix-cache/<portal>/`), а не общий корень: `dialog_id` и
//! `message_id` у разных порталов совпадают сплошь и рядом, и до разведения по
//! подкаталогам два портала честно перетирали вложения друг друга. Модуль об
//! этом не знает — он по-прежнему строит путь от `cfg.cache_dir`, а
//! пер-портальным его делает [`crate::config::BitrixConfig::for_portal`].
//!
//! # Дисковый бюджет считается НА ПОРТАЛ
//!
//! `max_cache_bytes` — общая настройка (она в корне `bitrix.toml`), но
//! применяется к каждому порталу ОТДЕЛЬНО: [`sweep_once`] берёт
//! [`total_bytes`] своего `cache_dir`, то есть свой подкаталог. Решение
//! осознанное. Общий бюджет на сумму означал бы, что активный портал вытесняет
//! вложения тихого — вплоть до того, что вкладка, которой неделю не
//! пользовались, скачивает каждый файл заново, хотя её собственный кэш
//! крошечный. Подчистка обязана оставаться свойством ВКЛАДКИ, а не гонкой
//! между ними. Цена решения названа прямо: фактический потолок на диске —
//! `max_cache_bytes × число порталов`, и при трёх порталах дефолт стоит
//! уменьшить.
//!
//! Подчистка при этом НЕ привязана к подключению: скачанные медиа переживают
//! logout, поэтому свой воркер-подметальщик есть и у отключённой вкладки.
//!
//! `<dialog>` — КАНОНИЧЕСКАЯ форма идентификатора диалога Bitrix24: `chat27`,
//! `sg12` или голое число личного диалога (`19`). Каталог `avatars`
//! столкнуться с ней не может по построению: имя диалога либо состоит из одних
//! цифр, либо начинается с `chat`/`sg` (см. [`DialogKey`]).
//!
//! # ЧЕМ ЭТО ОТЛИЧАЕТСЯ ОТ БЛИЗНЕЦОВ VK И TELEGRAM
//!
//! 1. **Видов кэша два, а не два «размера»**: [`MediaKind::File`] и
//!    [`MediaKind::Avatar`]. Отдельного `thumb` НЕТ и быть не может: у портала
//!    единственный рабочий путь скачивания — `im.v2.File.download {fileId}`,
//!    который отдаёт ОРИГИНАЛ. Поля `mediaUrl.preview["250"]`, `urlShow` и
//!    `urlDownload` портал присылает, но без сессионной куки они отдают HTML
//!    страницы логина (живая проверка), поэтому превью скачать нечем. Признак
//!    `has_thumb` в [`crate::attach::MediaDto`] остаётся — он значит «портал
//!    считает файл картинкой», а не «есть отдельный маленький файл».
//! 2. **Ключ диалога — не число, а пара «префикс + число»**. `dialog_id` у
//!    Bitrix24 полиморфен (`"chat27"` для групп, `19` для личных), и свести его
//!    к одному `i64` нельзя: чат 27 и пользователь 27 — разные адресаты.
//!
//! # Безопасность путей
//!
//! Идентификаторы приходят из URL, поэтому в путь они попадают ТОЛЬКО через
//! [`CacheKey::parse_file`] / [`CacheKey::parse_avatar`], которые разбирают их
//! в `i64`/`u32` и в [`DialogPrefix`] — перечисление из ТРЁХ вариантов.
//! Строка из URL в путь не конкатенируется вообще: имя каталога собирается
//! `format!`-ом из разобранного числа и варианта перечисления, поэтому
//! `../../etc` физически не имеет способа туда попасть — он не парсится в
//! число и отбрасывается до единого обращения к ФС (тест
//! `parse_rejects_path_traversal`).
//!
//! Это структурная гарантия, а не проверка строки: убрать её можно только
//! переписав тип ключа, что видно в диффе.
//!
//! # Почему запись двухступенчатая
//!
//! Порядок «сначала `bin`, потом `meta`» — контракт целостности: [`get`]
//! считает попаданием ТОЛЬКО пару «непустой bin + разбираемая meta». Оборванная
//! запись (остался `.tmp`, или появился `bin` без меты) попаданием не является
//! — иначе клиент получил бы обрезанный файл, да ещё и с
//! `Cache-Control: immutable`, то есть навсегда.
//!
//! Оба файла пишутся во временный файл В ТОМ ЖЕ каталоге и переносятся
//! `rename`, атомарным в пределах одной ФС.
//!
//! # Чего в кэше НЕТ никогда
//!
//! **Адресов портала.** `downloadUrl` из `im.v2.File.download` содержит КОД
//! ВЕБХУКА целиком и не сохраняется ни в память, ни на диск: он добывается
//! непосредственно перед скачиванием и умирает вместе с запросом. Публичный
//! CDN-адрес аватара тоже на диск не ложится — вместо него в мете лежит
//! [`MediaMeta::source_tag`], короткий отпечаток ([`source_tag`]). Его хватает,
//! чтобы заметить «пользователь сменил фото», и он ничего не раскрывает тому,
//! кто прочитал каталог кэша.
//!
//! # LRU
//!
//! Дисковый бюджет — [`crate::config::BitrixConfig::max_cache_bytes`].
//! [`sweep_once`] удаляет самые старые по `mtime` записи, пока размер не
//! опустится ниже [`SWEEP_TARGET_NUM`]/[`SWEEP_TARGET_DEN`] от лимита
//! (гистерезис: без него подчистка запускалась бы каждый тик). `mtime` —
//! приближение «последнего использования»: на попадании роут дёргает
//! [`touch`], поэтому часто открываемые файлы всплывают наверх.
//!
//! Воркер подчистки запускается ВСЕГДА, даже без вебхука: накопленные медиа
//! переживают logout, и подметать их некому, если привязать воркер к
//! подключению.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::BitrixConfig;
use crate::state::B24State;

/// Подкаталог кэша, в котором живут медиа.
pub const MEDIA_SUBDIR: &str = "media";

/// Каталог аватаров внутри [`MEDIA_SUBDIR`].
///
/// С каталогом диалога не столкнётся: имя диалога — либо одни цифры, либо
/// `chat…`/`sg…` (см. [`DialogKey::dir_name`]).
pub const AVATARS_DIR: &str = "avatars";

/// Максимальный индекс вложения в ключе кэша.
///
/// Ровно на единицу меньше [`crate::attach::MAX_MEDIA`]: столько вложений
/// разбирает проекция сообщения, и индекс за этой границей означает, что
/// клиент ошибся или пробует систему на прочность. Ответ ему — `404`, а не
/// поход в портал.
pub const MAX_ATTACH_INDEX: u32 = (crate::attach::MAX_MEDIA as u32) - 1;

/// Период работы LRU-подчистки.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Числитель целевой доли лимита, до которой подметает sweeper.
pub const SWEEP_TARGET_NUM: u64 = 9;

/// Знаменатель целевой доли лимита (9/10 ⇒ 90 %).
pub const SWEEP_TARGET_DEN: u64 = 10;

/// Сколько живёт брошенный staging-файл, прежде чем sweeper его удалит.
///
/// Час с запасом перекрывает любую живую загрузку (сам вызов ограничен
/// `request_timeout_secs`), поэтому более старый `.tmp` — гарантированно мусор
/// от упавшего процесса.
pub const STAGING_TTL: Duration = Duration::from_secs(60 * 60);

// ---------------------------------------------------------------------------
// Ключ.
// ---------------------------------------------------------------------------

/// Вид записи кэша — он же часть имени файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Вложение сообщения (оригинал; превью у портала не скачивается — см.
    /// шапку модуля).
    File,
    /// Аватар сотрудника портала (Р6).
    Avatar,
}

impl MediaKind {
    /// Строковое представление — оно же суффикс имени файла.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::File => "file",
            MediaKind::Avatar => "avatar",
        }
    }
}

impl std::fmt::Display for MediaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Форма идентификатора диалога Bitrix24 — ЗАКРЫТОЕ перечисление.
///
/// Ровно то же множество, что принимает [`crate::dto::parse_dialog_id`].
/// Перечислением, а не строкой, именно ради пути: в имя каталога попадает
/// вариант этого типа, а не то, что прислал клиент.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogPrefix {
    /// Личный диалог: `DIALOG_ID` — это `user_id` собеседника (`"19"`).
    User,
    /// Групповой чат, чат задачи, открытая линия (`"chat27"`).
    Chat,
    /// Чат рабочей группы/проекта (`"sg12"`).
    Group,
}

impl DialogPrefix {
    /// Строка префикса (у личного диалога — пустая).
    pub fn as_str(self) -> &'static str {
        match self {
            DialogPrefix::User => "",
            DialogPrefix::Chat => "chat",
            DialogPrefix::Group => "sg",
        }
    }
}

/// Разобранный идентификатор диалога: закрытый префикс плюс число.
///
/// Свести к одному `i64` его нельзя: `chat27` и `27` — РАЗНЫЕ адресаты
/// (групповой чат и личная переписка с сотрудником 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DialogKey {
    /// Форма идентификатора.
    pub prefix: DialogPrefix,
    /// Числовая часть, всегда `> 0`.
    pub number: i64,
}

impl DialogKey {
    /// Разбирает `:dialog` из URL. `None` ⇒ клиент прислал мусор.
    ///
    /// Правило дословно повторяет [`crate::dto::parse_dialog_id`] (тест
    /// `dialog_rule_matches_the_dto_parser`): иначе один и тот же диалог был бы
    /// «доступен» на одном маршруте и «не существовал» на другом.
    ///
    /// Ведущий `+`, пробелы внутри, не-ASCII-цифры, ноль и отрицательные —
    /// отбрасываются: одному диалогу обязано соответствовать одно написание
    /// адреса, иначе во фронте появились бы две записи `objectURL` одного файла.
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() || s.len() > 32 {
            return None;
        }
        if let Some(rest) = s.strip_prefix("chat") {
            return digits(rest).map(|number| Self {
                prefix: DialogPrefix::Chat,
                number,
            });
        }
        if let Some(rest) = s.strip_prefix("sg") {
            return digits(rest).map(|number| Self {
                prefix: DialogPrefix::Group,
                number,
            });
        }
        digits(s).map(|number| Self {
            prefix: DialogPrefix::User,
            number,
        })
    }

    /// Каноническая строка (`"chat27"`, `"sg12"`, `"19"`).
    ///
    /// Собирается из РАЗОБРАННЫХ частей, поэтому ведущие нули нормализуются:
    /// `"0027"` и `"27"` — один путь на диске, а не два файла.
    pub fn dir_name(&self) -> String {
        format!("{}{}", self.prefix.as_str(), self.number)
    }
}

impl std::fmt::Display for DialogKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.prefix.as_str())?;
        write!(f, "{}", self.number)
    }
}

/// Строка из одних ASCII-цифр → положительное число.
///
/// `str::parse` сам отверг бы пробелы и знак, но не отверг бы `"+7"`, а `char::
/// is_numeric` пропустил бы арабо-индийские цифры — отсюда явная проверка
/// байтов.
fn digits(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<i64>().ok().filter(|v| *v > 0)
}

/// Проверенный ключ записи кэша.
///
/// Существует ровно затем, чтобы «сырые» строки из URL нельзя было донести до
/// файловой системы: собрать ключ можно только разбором.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheKey {
    /// Вложение сообщения.
    File {
        /// Диалог.
        dialog: DialogKey,
        /// Сообщение. У портала идентификаторы растут ШАГОМ 2 и положительны.
        message_id: i64,
        /// Порядковый номер вложения в `params.FILE_ID`,
        /// `0..=`[`MAX_ATTACH_INDEX`].
        index: u32,
    },
    /// Аватар сотрудника портала.
    Avatar {
        /// Пользователь портала.
        user_id: i64,
    },
}

impl CacheKey {
    /// Разбирает тройку `:dialog/:mid/:index`. `None` ⇒ мусор.
    ///
    /// Роут разбирает те же три части ПООТДЕЛЬНО (ему нужны разные коды
    /// ошибок), но теми же функциями — [`DialogKey::parse`],
    /// [`parse_entity_id`], [`parse_attach_index`]: второго набора правил в
    /// плагине нет.
    pub fn parse_file(dialog: &str, mid: &str, index: &str) -> Option<Self> {
        Some(Self::File {
            dialog: DialogKey::parse(dialog)?,
            message_id: parse_entity_id(mid)?,
            index: parse_attach_index(index)?,
        })
    }

    /// Разбирает `:user_id` аватара. `None` ⇒ мусор.
    pub fn parse_avatar(user_id: &str) -> Option<Self> {
        Some(Self::Avatar {
            user_id: parse_entity_id(user_id)?,
        })
    }

    /// Вид записи.
    pub fn kind(&self) -> MediaKind {
        match self {
            CacheKey::File { .. } => MediaKind::File,
            CacheKey::Avatar { .. } => MediaKind::Avatar,
        }
    }

    /// Ключ дедупликации одновременных скачиваний
    /// ([`crate::state::B24State::media_inflight`]).
    ///
    /// У вложения формат совпадает с полем `key` кадра
    /// [`crate::ws::protocol::B24ServerMsg::DownloadProgress`] и с ключом
    /// карточки во фронте (`js/bitrix/media.js`): ключи в логах обеих половин
    /// одинаковы, и прогресс второго файла сообщения не прилетает в карточку
    /// первого.
    pub fn id(&self) -> String {
        match self {
            CacheKey::File {
                dialog,
                message_id,
                index,
            } => format!("{dialog}/{message_id}/{index}"),
            CacheKey::Avatar { user_id } => format!("avatar/{user_id}"),
        }
    }

    /// Имя каталога записи внутри `media/`.
    fn dir_name(&self) -> String {
        match self {
            CacheKey::File { dialog, .. } => dialog.dir_name(),
            CacheKey::Avatar { .. } => AVATARS_DIR.to_string(),
        }
    }

    /// Базовое имя файлов записи (без расширения).
    fn stem(&self) -> String {
        match self {
            CacheKey::File {
                message_id, index, ..
            } => format!("{message_id}.{index}.{}", MediaKind::File),
            CacheKey::Avatar { user_id } => format!("{user_id}.{}", MediaKind::Avatar),
        }
    }
}

/// Положительный числовой идентификатор из URL (`:mid`, `:user_id`).
///
/// Правило совпадает с `routes::dialogs::require_message_id` во всём, кроме
/// ведущего `+` (тест `message_id_rule_matches_the_route`): роут его принимает,
/// ключ кэша — нет. Расхождение осознанное — ключ живёт ещё и во фронте
/// (`js/bitrix/media.js`), и два написания одного адреса развели бы там две
/// записи `objectURL` одного файла.
pub fn parse_entity_id(raw: &str) -> Option<i64> {
    digits(raw.trim())
}

/// `:index` из URL → `u32 <= `[`MAX_ATTACH_INDEX`].
pub fn parse_attach_index(raw: &str) -> Option<u32> {
    let t = raw.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<u32>().ok().filter(|v| *v <= MAX_ATTACH_INDEX)
}

// ---------------------------------------------------------------------------
// Метаданные.
// ---------------------------------------------------------------------------

/// Метаданные записи (сайдкар `.meta.json`).
///
/// Хранятся отдельно от байтов, потому что именно наличие меты — признак
/// целостности `bin` (см. шапку модуля), и потому что `Content-Type` нужно
/// отдавать, не заглядывая внутрь файла.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMeta {
    /// MIME (`image/png`, `text/plain`, …).
    #[serde(default)]
    pub mime: Option<String>,
    /// Размер в байтах на момент записи.
    #[serde(default)]
    pub size: u64,
    /// Имя файла, как его прислал портал.
    #[serde(default)]
    pub file_name: Option<String>,
    /// Ширина картинки, пиксели.
    #[serde(default)]
    pub width: Option<i64>,
    /// Высота картинки, пиксели.
    #[serde(default)]
    pub height: Option<i64>,
    /// Отпечаток ИСТОЧНИКА, из которого взяты байты (см. [`source_tag`]).
    ///
    /// Нужен аватарам: сотрудник меняет фото, портал отдаёт новый CDN-адрес, а
    /// события об этом не существует. Несовпадение отпечатка — единственный
    /// доступный признак «в кэше лежит старое фото».
    ///
    /// САМ АДРЕС здесь не хранится никогда: `downloadUrl` содержит код вебхука,
    /// и правило «адресов портала на диске нет» не должно зависеть от того,
    /// какой именно адрес попал в эту функцию.
    #[serde(default)]
    pub source_tag: Option<String>,
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

    /// Тот же конструктор с отпечатком источника.
    #[must_use]
    pub fn with_source(mut self, source: &str) -> Self {
        self.source_tag = Some(source_tag(source));
        self
    }
}

/// Короткий необратимый отпечаток строки (FNV-1a, 64 бита, hex).
///
/// Криптографического качества здесь не нужно и не место: задача — заметить
/// «адрес источника поменялся», а не защититься от подбора. Зато отпечаток
/// НЕ ВОССТАНАВЛИВАЕТСЯ в адрес, поэтому в `meta.json` не оседает ни CDN-ссылка
/// портала, ни тем более `downloadUrl` с кодом вебхука.
///
/// Своя функция, а не крейт: `DefaultHasher` из std не гарантирует стабильности
/// между версиями компилятора, и после обновления toolchain весь кэш аватаров
/// молча протух бы.
pub fn source_tag(raw: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Найденная в кэше запись.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMedia {
    /// Путь к байтам.
    pub path: PathBuf,
    /// Метаданные; `size` приведён к фактическому размеру файла.
    pub meta: MediaMeta,
}

// ---------------------------------------------------------------------------
// Пути.
// ---------------------------------------------------------------------------

/// Корень медиа-кэша.
pub fn media_root(cfg: &BitrixConfig) -> PathBuf {
    cfg.cache_dir.join(MEDIA_SUBDIR)
}

/// Каталог записи.
pub fn dir_for(cfg: &BitrixConfig, key: &CacheKey) -> PathBuf {
    media_root(cfg).join(key.dir_name())
}

/// Пути записи: `(bin, meta)`.
pub fn path_for(cfg: &BitrixConfig, key: &CacheKey) -> (PathBuf, PathBuf) {
    let dir = dir_for(cfg, key);
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

// ---------------------------------------------------------------------------
// Чтение и запись.
// ---------------------------------------------------------------------------

/// Читает запись из кэша.
///
/// Попаданием считается ТОЛЬКО пара «непустой `bin` + разбираемая `meta`».
pub async fn get(cfg: &BitrixConfig, key: &CacheKey) -> Option<CachedMedia> {
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

/// Есть ли запись в кэше.
pub async fn contains(cfg: &BitrixConfig, key: &CacheKey) -> bool {
    get(cfg, key).await.is_some()
}

/// Создаёт каталог записи и возвращает уникальный путь staging-файла.
///
/// Staging лежит В ТОМ ЖЕ каталоге, что и цель: `rename` между разными
/// файловыми системами не атомарен (и на macOS/Linux падает с `EXDEV`), а
/// `/tmp` вполне может оказаться другой ФС.
///
/// # Errors
///
/// Ошибка создания каталога кэша.
pub async fn begin_staging(cfg: &BitrixConfig, key: &CacheKey) -> std::io::Result<PathBuf> {
    let dir = dir_for(cfg, key);
    tokio::fs::create_dir_all(&dir).await?;
    Ok(dir.join(format!("{}.tmp.{}", key.stem(), uuid::Uuid::new_v4())))
}

/// Переносит готовый staging-файл в кэш и дописывает мету.
///
/// Вызывающий обязан ДО этого сбросить буферы (`flush`/`sync_all`): `rename`
/// переносит имя, а не содержимое страничного кэша.
///
/// # Errors
///
/// Ошибки `rename` и записи меты. При ошибке staging НЕ удаляется — это делает
/// вызывающий, у которого есть guard, снимающий файл и при панике.
pub async fn commit(
    cfg: &BitrixConfig,
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
        // Байты на месте, но без меты кэш их не отдаст. Убираем bin, чтобы не
        // копить «невидимый» мусор, который посчитает только sweeper.
        let _ = tokio::fs::remove_file(&bin).await;
        return Err(e);
    }
    Ok(bin)
}

/// Кладёт в кэш готовые байты (аватары, тесты).
///
/// Для больших файлов не годится — они пишутся потоково через
/// [`begin_staging`] + [`commit`], чтобы не держать мегабайты в памяти.
///
/// # Errors
///
/// Любая ошибка ФС на пути «создать каталог → записать staging → rename».
pub async fn put_atomic(
    cfg: &BitrixConfig,
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

// ---------------------------------------------------------------------------
// Инвалидация.
// ---------------------------------------------------------------------------

/// Удаляет из кэша ВСЕ файлы сообщения (все индексы, мету и брошенные `.tmp`).
///
/// # Почему без этого `Cache-Control: immutable` был бы багом
///
/// Ответ роута вложений помечен `immutable` на год. Правомерно это ровно
/// потому, что содержимое ключа `(dialog, message_id, index)` неизменно, а
/// когда оно всё-таки меняется — правка или удаление сообщения, — плагин
/// стирает запись сам:
///
/// | событие | кто зовёт |
/// |---|---|
/// | `ONIMV2MESSAGEDELETE` | [`crate::events`] |
/// | обнаруженная правка последнего сообщения | [`crate::recent_sync`] |
///
/// `dialog` приходит из события строкой и бывает неразбираемым — тогда
/// подчищаются одноимённые записи во ВСЕХ каталогах диалогов: промах
/// невозможен, а лишний обход дешёвый.
///
/// Возвращает число удалённых файлов. Ошибки ФС не фатальны: кэш — ускоритель,
/// а не источник истины.
pub async fn invalidate(cfg: &BitrixConfig, dialog: &str, message_id: i64) -> usize {
    if message_id <= 0 {
        return 0;
    }
    // Точка в префиксе обязательна: без неё инвалидация сообщения 4 снесла бы
    // ещё и 42, и 420.
    let prefix = format!("{message_id}.");
    match DialogKey::parse(dialog) {
        Some(key) => remove_prefixed(&media_root(cfg).join(key.dir_name()), &prefix).await,
        None => {
            let mut removed = 0;
            for dir in dialog_dirs(cfg).await {
                removed += remove_prefixed(&dir, &prefix).await;
            }
            removed
        }
    }
}

/// Удаляет из кэша аватар пользователя.
///
/// Зовётся, когда портал отдал ДРУГОЙ адрес аватара (см.
/// [`MediaMeta::source_tag`]): событий о смене фото платформа не присылает
/// вовсе, поэтому единственный момент, когда о ней можно узнать, — очередной
/// ответ со свежим `user`.
pub async fn invalidate_avatar(cfg: &BitrixConfig, user_id: i64) -> usize {
    if user_id <= 0 {
        return 0;
    }
    remove_prefixed(
        &media_root(cfg).join(AVATARS_DIR),
        &format!("{user_id}.{}", MediaKind::Avatar),
    )
    .await
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

/// Каталоги ДИАЛОГОВ внутри кэша (без `avatars`).
async fn dialog_dirs(cfg: &BitrixConfig) -> Vec<PathBuf> {
    all_dirs(cfg)
        .await
        .into_iter()
        .filter(|d| d.file_name().and_then(|s| s.to_str()) != Some(AVATARS_DIR))
        .collect()
}

/// Все каталоги внутри кэша, включая `avatars`.
async fn all_dirs(cfg: &BitrixConfig) -> Vec<PathBuf> {
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
pub async fn total_bytes(cfg: &BitrixConfig) -> u64 {
    let mut total = 0;
    for dir in all_dirs(cfg).await {
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
/// Best-effort: ошибки проглатываются, потому что промах LRU не стоит сломанной
/// отдачи файла. Работа синхронная и уходит в blocking-пул (у tokio-обёртки нет
/// `set_modified`).
pub async fn touch(path: PathBuf) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
    })
    .await;
}

// ---------------------------------------------------------------------------
// LRU.
// ---------------------------------------------------------------------------

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
    /// `<каталог>/<stem>` — по нему проверяется «сейчас качается».
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
/// Порядок удаления — по `mtime` от старых к новым, поэтому «горячие» файлы
/// (их `mtime` обновляет [`touch`] на каждом попадании) выживают. Записи из
/// `protected` (см. [`protected_keys`]) пропускаются: удалить файл, который
/// прямо сейчас качается, значит отдать клиенту пустоту.
///
/// `max_cache_bytes == 0` означает «не подрезать»: подчистка тогда занимается
/// только брошенным staging'ом.
pub async fn sweep_once(cfg: &BitrixConfig, protected: &HashSet<String>) -> SweepStats {
    let mut stats = SweepStats::default();
    let mut entries: Vec<Entry> = Vec::new();
    let now = SystemTime::now();

    for dir in all_dirs(cfg).await {
        let dir_name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
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
                    // `duration_since` у времени «из будущего» даёт Err (часы
                    // прыгнули назад после NTP-коррекции) — возраст тогда
                    // нулевой, и живая загрузка не будет убита.
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
            entries.push(Entry {
                bin: entry.path(),
                meta,
                wave,
                bytes: md.len() + meta_len + wave_len,
                mtime: md.modified().unwrap_or(UNIX_EPOCH),
                id: format!("{dir_name}/{stem}"),
            });
        }
    }

    stats.scanned = entries.len();
    stats.total_after = stats.total_before - stats.freed;
    if cfg.max_cache_bytes == 0 || stats.total_after <= cfg.max_cache_bytes {
        return stats;
    }
    let target = sweep_target(cfg.max_cache_bytes);

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

/// Имя записи (`<каталог>/<stem>`) по ключу дедупа.
///
/// Ключ inflight — `<dialog>/<mid>/<index>` либо `avatar/<user_id>`, имя записи
/// — `<dialog>/<mid>.<index>.file` либо `avatars/<user_id>.avatar`. Перевод
/// вынесен в отдельную функцию и покрыт тестом: разъедься он, и sweeper начал
/// бы удалять файлы прямо во время скачивания.
fn file_id_of_inflight(key: &str) -> Option<String> {
    let mut parts = key.split('/');
    let head = parts.next()?;
    if head == "avatar" {
        let user = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        return Some(format!("{AVATARS_DIR}/{user}.{}", MediaKind::Avatar));
    }
    let mid = parts.next()?;
    let index = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(format!("{head}/{mid}.{index}.{}", MediaKind::File))
}

/// Записи, которые сейчас качаются (их sweeper не трогает).
async fn protected_keys(state: &Arc<B24State>) -> HashSet<String> {
    state
        .media_inflight
        .lock()
        .await
        .keys()
        .filter_map(|k| file_id_of_inflight(k))
        .collect()
}

/// Фоновый воркер LRU-подчистки: прогон на старте и далее раз в
/// [`SWEEP_INTERVAL`]. Останавливается по `shutdown`-токену.
pub async fn sweeper_loop(state: Arc<B24State>) {
    loop {
        let protected = protected_keys(&state).await;
        let stats = sweep_once(&state.config, &protected).await;
        if stats.removed > 0 || stats.stale_staging > 0 {
            tracing::info!(
                target: "forge_bitrix",
                scanned = stats.scanned,
                removed = stats.removed,
                stale_staging = stats.stale_staging,
                freed = stats.freed,
                total_after = stats.total_after,
                limit = state.config.max_cache_bytes,
                "forge-bitrix: media cache swept"
            );
        } else {
            tracing::debug!(
                target: "forge_bitrix",
                scanned = stats.scanned,
                total = stats.total_after,
                limit = state.config.max_cache_bytes,
                "forge-bitrix: media cache within limits"
            );
        }

        tokio::select! {
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_bitrix", "forge-bitrix: media sweeper stopped");
                return;
            }
            _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
        }
    }
}

/// Запускает воркер подчистки и регистрирует его для graceful shutdown.
///
/// Запускается ВСЕГДА, даже без вебхука: накопленные медиа переживают logout, и
/// привязка воркера к подключению означала бы, что после выхода кэш растёт до
/// упора и подмести его некому.
pub async fn spawn_sweeper(state: &Arc<B24State>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { sweeper_loop(st).await });
    state.register_worker(handle).await;
    tracing::info!(
        target: "forge_bitrix",
        interval_secs = SWEEP_INTERVAL.as_secs(),
        limit_bytes = state.config.max_cache_bytes,
        "forge-bitrix: media cache sweeper started"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> BitrixConfig {
        BitrixConfig {
            cache_dir: dir.join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.join("prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            config_path: dir.join("bitrix.toml"),
            ..BitrixConfig::default()
        }
    }

    fn key() -> CacheKey {
        CacheKey::parse_file("chat27", "113", "0").unwrap()
    }

    fn meta_of(size: u64) -> MediaMeta {
        MediaMeta::now(
            Some("text/plain".into()),
            size,
            Some("forge_probe.txt".into()),
        )
    }

    /// ГЛАВНЫЙ ТЕСТ БЕЗОПАСНОСТИ: ничего похожего на путь в ключ не проходит.
    #[test]
    fn parse_rejects_path_traversal() {
        for bad in [
            "../../etc",
            "..",
            "../",
            "..%2F..",
            "%2e%2e",
            "/etc/passwd",
            "1/../../2",
            "chat27/../../etc",
            "avatars",
            "abc",
            "",
            "   ",
            "1e3",
            "0x10",
            "\u{664}\u{662}", // ٤٢ — арабо-индийские цифры
            "27\0",
            "+27",
            "-27",
            "27.0",
            "😀",
        ] {
            assert!(
                CacheKey::parse_file(bad, "113", "0").is_none(),
                "dialog {bad:?} не должен парситься"
            );
            assert!(
                CacheKey::parse_file("chat27", bad, "0").is_none(),
                "message_id {bad:?} не должен парситься"
            );
            assert!(
                CacheKey::parse_file("chat27", "113", bad).is_none(),
                "index {bad:?} не должен парситься"
            );
            assert!(
                CacheKey::parse_avatar(bad).is_none(),
                "user_id {bad:?} не должен парситься"
            );
        }
        // Ноль и отрицательные — тоже мусор.
        assert!(CacheKey::parse_file("chat0", "113", "0").is_none());
        assert!(CacheKey::parse_file("chat27", "0", "0").is_none());
        assert!(CacheKey::parse_avatar("0").is_none());
        // А нормальные значения проходят.
        let k = CacheKey::parse_file("sg12", "113", "19").unwrap();
        assert_eq!(k.id(), "sg12/113/19");
        assert_eq!(CacheKey::parse_avatar("17").unwrap().id(), "avatar/17");
    }

    /// Все три формы `dialog_id` портала различаются, а ведущие нули
    /// нормализуются: `chat27` и `27` — РАЗНЫЕ адресаты, `0027` и `27` — один.
    #[test]
    fn dialog_forms_are_distinct_and_canonical() {
        let chat = DialogKey::parse("chat27").unwrap();
        let group = DialogKey::parse("sg27").unwrap();
        let user = DialogKey::parse("27").unwrap();
        assert_ne!(chat, group);
        assert_ne!(chat, user);
        assert_eq!(chat.dir_name(), "chat27");
        assert_eq!(group.dir_name(), "sg27");
        assert_eq!(user.dir_name(), "27");
        assert_eq!(DialogKey::parse("0027").unwrap(), user, "ведущие нули — тот же диалог");
        assert_eq!(DialogKey::parse(" chat27 ").unwrap(), chat);
        assert!(DialogKey::parse("chat").is_none(), "префикс без числа");
        assert!(DialogKey::parse("sg").is_none());
        assert!(DialogKey::parse(&"9".repeat(33)).is_none(), "длина ограничена");
    }

    /// Правило разбора `:dialog` обязано совпадать с тем, что применяет
    /// проекция DTO: иначе один и тот же диалог был бы «доступен» на одном
    /// маршруте и «не существовал» на другом.
    #[test]
    fn dialog_rule_matches_the_dto_parser() {
        for raw in [
            "chat27", "sg12", "19", "0027", " 19 ", "+19", "-19", "0", "chat", "chat0", "abc", "",
            "chat27x", "😀",
        ] {
            let ours = DialogKey::parse(raw).is_some();
            // Единственное осознанное расхождение: `dto::parse_dialog_id`
            // пропускает НУЛЕВЫЕ идентификаторы (`"0"`, `"chat0"`), потому что
            // для него это просто строка нужной формы, а ключу кэша нужен
            // положительный номер каталога.
            let zero = crate::dto::parse_dialog_id(raw)
                .is_some_and(|id| id.trim_start_matches(['c', 'h', 'a', 't', 's', 'g']).parse::<i64>() == Ok(0));
            let dto = crate::dto::parse_dialog_id(raw).is_some() && !zero;
            assert_eq!(ours, dto, "расхождение на {raw:?}");
        }
    }

    /// Правило разбора `:mid` обязано совпадать с роутом.
    #[test]
    fn message_id_rule_matches_the_route() {
        for raw in ["113", " 113 ", "+113", "0", "-1", "abc", "", "9223372036854775808"] {
            let ours = parse_entity_id(raw);
            let route = crate::routes::dialogs::require_message_id(raw).ok();
            // `+113` роут принимает (`i64::from_str` его понимает), а ключ кэша
            // — нет: два написания одного адреса развели бы во фронте две
            // записи objectURL одного файла.
            let expected = if raw.trim().starts_with('+') { None } else { route };
            assert_eq!(ours, expected, "расхождение на {raw:?}");
        }
    }

    /// Индекс ограничен моделью: вложения за границей разбора не существует.
    #[test]
    fn parse_caps_the_attachment_index() {
        assert!(CacheKey::parse_file("chat27", "113", "0").is_some());
        assert_eq!(MAX_ATTACH_INDEX, 19);
        assert!(CacheKey::parse_file("chat27", "113", "19").is_some());
        assert!(
            CacheKey::parse_file("chat27", "113", "20").is_none(),
            "проекция разбирает максимум MAX_MEDIA вложений"
        );
        assert!(CacheKey::parse_file("chat27", "113", "4294967296").is_none());
        // Ведущие нули индекса нормализуются в один путь.
        assert_eq!(
            CacheKey::parse_file("chat27", "113", "00").unwrap().id(),
            "chat27/113/0"
        );
    }

    /// Пути записей всегда лежат ВНУТРИ cache_dir, и аватары не путаются с
    /// диалогами.
    #[test]
    fn paths_stay_inside_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());

        let (bin, meta) = path_for(&cfg, &key());
        assert!(bin.starts_with(&cfg.cache_dir), "{}", bin.display());
        assert!(meta.starts_with(&cfg.cache_dir));
        assert!(bin.to_string_lossy().ends_with("chat27/113.0.file.bin"));
        assert!(meta.to_string_lossy().ends_with("chat27/113.0.file.meta.json"));

        let avatar = CacheKey::parse_avatar("17").unwrap();
        let (abin, ameta) = path_for(&cfg, &avatar);
        assert!(abin.to_string_lossy().ends_with("avatars/17.avatar.bin"));
        assert!(ameta.to_string_lossy().ends_with("avatars/17.avatar.meta.json"));
        assert_ne!(dir_for(&cfg, &key()), dir_for(&cfg, &avatar));
    }

    /// Два вложения одного сообщения — РАЗНЫЕ файлы (иначе второе затирало бы
    /// первое и на диске, и в учёте objectURL во фронте).
    #[tokio::test]
    async fn attachments_of_one_message_are_separate_records() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let first = CacheKey::parse_file("chat27", "113", "0").unwrap();
        let second = CacheKey::parse_file("chat27", "113", "1").unwrap();
        put_atomic(&cfg, &first, b"AAAA", &meta_of(4)).await.unwrap();
        put_atomic(&cfg, &second, b"BBBBBB", &meta_of(6))
            .await
            .unwrap();

        let a = get(&cfg, &first).await.unwrap();
        let b = get(&cfg, &second).await.unwrap();
        assert_ne!(a.path, b.path);
        assert_eq!(tokio::fs::read(&a.path).await.unwrap(), b"AAAA");
        assert_eq!(tokio::fs::read(&b.path).await.unwrap(), b"BBBBBB");
        assert_ne!(first.id(), second.id());
    }

    #[tokio::test]
    async fn put_then_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key();
        assert!(get(&cfg, &k).await.is_none(), "пустой кэш — промах");

        let bytes = b"forge probe payload".to_vec();
        put_atomic(&cfg, &k, &bytes, &meta_of(bytes.len() as u64))
            .await
            .unwrap();

        let hit = get(&cfg, &k).await.expect("после записи ожидается попадание");
        assert_eq!(hit.meta.mime.as_deref(), Some("text/plain"));
        assert_eq!(hit.meta.file_name.as_deref(), Some("forge_probe.txt"));
        assert_eq!(hit.meta.size, bytes.len() as u64);
        assert_eq!(tokio::fs::read(&hit.path).await.unwrap(), bytes);
        assert!(contains(&cfg, &k).await);
        // Аватар с тем же номером — независимая запись.
        assert!(get(&cfg, &CacheKey::parse_avatar("113").unwrap())
            .await
            .is_none());
    }

    /// Оборванная запись не должна выглядеть попаданием.
    #[tokio::test]
    async fn interrupted_write_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key();

        let staging = begin_staging(&cfg, &k).await.unwrap();
        tokio::fs::write(&staging, b"half").await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "staging — не кэш");

        let (bin, meta_path) = path_for(&cfg, &k);
        tokio::fs::rename(&staging, &bin).await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "bin без меты — не кэш");

        tokio::fs::write(&meta_path, b"{not json").await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "битая мета — не кэш");

        tokio::fs::write(&bin, b"").await.unwrap();
        let json = serde_json::to_vec(&meta_of(0)).unwrap();
        tokio::fs::write(&meta_path, &json).await.unwrap();
        assert!(get(&cfg, &k).await.is_none(), "пустой bin — не кэш");
    }

    #[tokio::test]
    async fn invalidate_removes_every_attachment_of_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        for index in ["0", "1"] {
            let k = CacheKey::parse_file("chat27", "113", index).unwrap();
            put_atomic(&cfg, &k, b"data", &meta_of(4)).await.unwrap();
        }
        let staging = begin_staging(&cfg, &key()).await.unwrap();
        tokio::fs::write(&staging, b"junk").await.unwrap();
        let other = CacheKey::parse_file("chat27", "115", "0").unwrap();
        put_atomic(&cfg, &other, b"keep", &meta_of(4)).await.unwrap();

        let removed = invalidate(&cfg, "chat27", 113).await;
        assert_eq!(removed, 5, "2 bin + 2 meta + staging");
        for index in ["0", "1"] {
            assert!(get(&cfg, &CacheKey::parse_file("chat27", "113", index).unwrap())
                .await
                .is_none());
        }
        assert!(get(&cfg, &other).await.is_some(), "чужая запись уцелела");
        assert!(!staging.exists());
    }

    /// Инвалидация сообщения 4 не должна зацепить 42 и 420.
    #[tokio::test]
    async fn invalidate_matches_whole_message_id_not_decimal_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        for id in ["4", "42", "420"] {
            let k = CacheKey::parse_file("chat27", id, "0").unwrap();
            put_atomic(&cfg, &k, b"x", &meta_of(1)).await.unwrap();
        }
        assert_eq!(invalidate(&cfg, "chat27", 4).await, 2, "bin + meta");
        assert!(get(&cfg, &CacheKey::parse_file("chat27", "4", "0").unwrap())
            .await
            .is_none());
        for id in ["42", "420"] {
            assert!(
                get(&cfg, &CacheKey::parse_file("chat27", id, "0").unwrap())
                    .await
                    .is_some(),
                "{id} — не префикс четвёрки"
            );
        }
    }

    /// Событие без разбираемого диалога чистит все каталоги диалогов, но НЕ
    /// трогает аватары: у них своё пространство имён.
    #[tokio::test]
    async fn invalidate_without_a_dialog_scans_dialog_dirs_only() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let a = CacheKey::parse_file("chat27", "9", "0").unwrap();
        let b = CacheKey::parse_file("19", "9", "3").unwrap();
        let avatar = CacheKey::parse_avatar("9").unwrap();
        for (k, bytes) in [(a, &b"a"[..]), (b, &b"b"[..]), (avatar, &b"c"[..])] {
            put_atomic(&cfg, &k, bytes, &meta_of(1)).await.unwrap();
        }

        assert_eq!(invalidate(&cfg, "", 9).await, 4);
        assert!(get(&cfg, &a).await.is_none());
        assert!(get(&cfg, &b).await.is_none());
        assert!(
            get(&cfg, &avatar).await.is_some(),
            "аватар 9 — не сообщение 9"
        );
    }

    /// Мусорный `message_id` (0/отрицательный) не должен удалять ничего.
    #[tokio::test]
    async fn invalidate_ignores_nonpositive_ids() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key();
        put_atomic(&cfg, &k, b"data", &meta_of(4)).await.unwrap();
        assert_eq!(invalidate(&cfg, "chat27", 0).await, 0);
        assert_eq!(invalidate(&cfg, "chat27", -113).await, 0);
        assert_eq!(invalidate(&cfg, "", -1).await, 0);
        assert_eq!(invalidate_avatar(&cfg, 0).await, 0);
        assert!(get(&cfg, &k).await.is_some());
    }

    /// Аватар инвалидируется отдельно и по своему пространству имён.
    #[tokio::test]
    async fn invalidate_avatar_removes_only_that_user() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let a = CacheKey::parse_avatar("1").unwrap();
        let b = CacheKey::parse_avatar("17").unwrap();
        // 17 и 1 начинаются одинаково — суффикс `.avatar` в префиксе обязателен.
        put_atomic(&cfg, &a, b"a", &meta_of(1)).await.unwrap();
        put_atomic(&cfg, &b, b"b", &meta_of(1)).await.unwrap();

        assert_eq!(invalidate_avatar(&cfg, 1).await, 2);
        assert!(get(&cfg, &a).await.is_none());
        assert!(get(&cfg, &b).await.is_some(), "17 — не префикс единицы");
    }

    /// Отпечаток источника стабилен, различает адреса и НЕ содержит самого
    /// адреса: код вебхука через него не утекает даже при чтении каталога кэша.
    #[test]
    fn the_source_tag_is_stable_and_opaque() {
        let url = "https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/download/?token=xyz";
        let tag = source_tag(url);
        assert_eq!(tag, source_tag(url), "отпечаток стабилен");
        assert_ne!(tag, source_tag(&format!("{url}2")));
        assert_eq!(tag.len(), 16);
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!tag.contains("wqdzbfhmn83d23j4"));
        assert!(!tag.contains("bitrix24"));
    }

    /// Параллельные записи одного ключа не портят файл.
    #[tokio::test]
    async fn concurrent_put_atomic_keeps_the_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(cfg(dir.path()));
        let k = key();

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
        let mut rd = tokio::fs::read_dir(dir_for(&cfg, &k)).await.unwrap();
        let mut names = Vec::new();
        while let Ok(Some(e)) = rd.next_entry().await {
            names.push(e.file_name().to_string_lossy().to_string());
        }
        names.sort();
        assert_eq!(names, vec!["113.0.file.bin", "113.0.file.meta.json"]);
    }

    #[tokio::test]
    async fn total_bytes_counts_attachments_and_avatars() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        assert_eq!(total_bytes(&cfg).await, 0);
        put_atomic(&cfg, &key(), &vec![0u8; 1000], &meta_of(1000))
            .await
            .unwrap();
        let only_file = total_bytes(&cfg).await;
        assert!(only_file > 1000, "мета тоже весит: {only_file}");
        put_atomic(
            &cfg,
            &CacheKey::parse_avatar("17").unwrap(),
            &vec![0u8; 500],
            &meta_of(500),
        )
        .await
        .unwrap();
        assert!(total_bytes(&cfg).await > only_file + 500, "аватары считаются");
    }

    /// Перевод ключа дедупа в имя записи покрыт тестом: разъедься он, sweeper
    /// удалял бы файлы прямо во время скачивания.
    #[test]
    fn inflight_keys_map_to_record_names() {
        assert_eq!(
            file_id_of_inflight("chat27/113/0").as_deref(),
            Some("chat27/113.0.file")
        );
        assert_eq!(
            file_id_of_inflight("19/113/2").as_deref(),
            Some("19/113.2.file")
        );
        assert_eq!(
            file_id_of_inflight("avatar/17").as_deref(),
            Some("avatars/17.avatar")
        );
        assert_eq!(file_id_of_inflight("chat27/113").as_deref(), None);
        assert_eq!(file_id_of_inflight("chat27/113/0/full").as_deref(), None);
        assert_eq!(file_id_of_inflight("avatar/17/extra").as_deref(), None);

        // И обратная сверка на живых ключах: то, что кладёт роут, sweeper
        // обязан узнавать.
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        for k in [key(), CacheKey::parse_avatar("17").unwrap()] {
            let (bin, _) = path_for(&cfg, &k);
            let stem = bin.file_name().unwrap().to_string_lossy();
            let expected = format!("{}/{}", k.dir_name(), stem.trim_end_matches(".bin"));
            assert_eq!(file_id_of_inflight(&k.id()).as_deref(), Some(expected.as_str()));
        }
    }

    /// Sweeper удаляет самые старые записи, пока не уложится в 90 % лимита, и
    /// не трогает то, что сейчас качается.
    #[tokio::test]
    async fn sweep_drops_oldest_and_keeps_inflight() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        // Лимит 5 КиБ: четыре записи по 2 КиБ обязаны ужаться до ≤ 4.5 КиБ.
        cfg.max_cache_bytes = 5 * 1024;

        let mut keys = Vec::new();
        for i in 1..=4i64 {
            let k = CacheKey::parse_file("chat27", &i.to_string(), "0").unwrap();
            put_atomic(&cfg, &k, &vec![7u8; 2048], &meta_of(2048))
                .await
                .unwrap();
            keys.push(k);
        }
        // Явно расставляем «возраст»: #1 самый старый, #4 самый свежий.
        for (n, k) in keys.iter().enumerate() {
            let (bin, _) = path_for(&cfg, k);
            let when = SystemTime::now() - Duration::from_secs(1000 - n as u64 * 100);
            std::fs::OpenOptions::new()
                .write(true)
                .open(&bin)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }

        // #1 «качается» — его трогать нельзя.
        let protected: HashSet<String> = ["chat27/1.0.file".to_string()].into_iter().collect();
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
            "самая свежая запись обязана уцелеть"
        );
    }

    /// Пока кэш в пределах лимита, sweeper ничего не удаляет.
    #[tokio::test]
    async fn sweep_is_a_noop_below_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let k = key();
        put_atomic(&cfg, &k, &vec![1u8; 4096], &meta_of(4096))
            .await
            .unwrap();

        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.scanned, 1);
        assert!(get(&cfg, &k).await.is_some());
    }

    /// `max_cache_bytes == 0` означает «не подрезать».
    #[tokio::test]
    async fn a_zero_limit_disables_trimming() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg(dir.path());
        cfg.max_cache_bytes = 0;
        put_atomic(&cfg, &key(), &vec![1u8; 8192], &meta_of(8192))
            .await
            .unwrap();
        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.removed, 0);
        assert!(get(&cfg, &key()).await.is_some());
    }

    /// Брошенный staging старше TTL удаляется; свежий — нет, в том числе с
    /// `mtime` «из будущего» (NTP-коррекция часов).
    #[tokio::test]
    async fn sweep_removes_stale_staging_only() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());

        let stale = begin_staging(&cfg, &key()).await.unwrap();
        tokio::fs::write(&stale, b"orphan").await.unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - STAGING_TTL - Duration::from_secs(60))
            .unwrap();

        let fresh = begin_staging(&cfg, &key()).await.unwrap();
        tokio::fs::write(&fresh, b"in progress").await.unwrap();

        let future = begin_staging(&cfg, &key()).await.unwrap();
        tokio::fs::write(&future, b"clock skew").await.unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&future)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(24 * 3600))
            .unwrap();

        let stats = sweep_once(&cfg, &HashSet::new()).await;
        assert_eq!(stats.stale_staging, 1);
        assert!(!stale.exists());
        assert!(fresh.exists(), "живая загрузка не удаляется");
        assert!(future.exists(), "staging с будущим mtime не удаляется");
    }
}
