//! Приём файла от браузера: `POST /api/vk/upload` — и трёхшаговый конвейер
//! заливки в VK, который живёт НЕ здесь, а внутри отправки сообщения.
//!
//! | Метод | Путь | Что делает |
//! |-------|------|------------|
//! | POST | `/api/vk/upload` | принимает сырые байты файла в staging и выдаёт `upload_id` |
//!
//! # ГЛАВНОЕ СТРУКТУРНОЕ РЕШЕНИЕ ФАЗЫ
//!
//! `POST /api/vk/upload` кладёт байты на НАШ диск и больше ничего не делает —
//! **ни одного обращения к VK**. Причина не в удобстве, а в протоколе:
//! upload-сервер VK выдаётся под КОНКРЕТНЫЙ `peer_id`
//! (`photos.getMessagesUploadServer(peer_id)`), а на шаге `/upload` адресат
//! ещё неизвестен — пользователь вправе прикрепить файл и передумать, куда
//! слать. Поэтому весь конвейер
//!
//! ```text
//! (1) photos.getMessagesUploadServer(peer_id)                 [ТРОТТЛЕННЫЙ]
//!     docs.getMessagesUploadServer(peer_id, type=doc|audio_message)
//! (2) POST <upload_url> multipart                             [НЕ троттлится]
//! (3) photos.saveMessagesPhoto | docs.save                    [ТРОТТЛЕННЫЙ]
//! (4) messages.send(attachment="photo{owner}_{id}")           [ТРОТТЛЕННЫЙ]
//! ```
//!
//! выполняется внутри `POST /api/vk/dialogs/:id/messages`
//! ([`crate::routes::dialogs::post_message`]). Три следствия, которые обязаны
//! быть на виду:
//!
//! 1. `upload_id` НЕ соответствует attachment-строке VK и никогда ей не
//!    станет: attachment-строка рождается на шаге (3) и живёт ровно до
//!    `messages.send` в том же запросе;
//! 2. кадр `upload_progress` приходит ВО ВРЕМЯ `POST /messages`, а не во время
//!    `POST /upload`. Это единственное поведенческое отличие, видимое фронту:
//!    он обязан продолжать слушать кадр после нажатия «отправить». Форма кадра
//!    не меняется;
//! 3. staging-файл — не кэш, а полуфабрикат: он расходуется первой же
//!    отправкой ([`crate::state::VkState::take_upload`]).
//!
//! # Почему RAW-тело, а не multipart
//!
//! `multipart/form-data` заставил бы тянуть фичу `axum/multipart` и парсить
//! границы частей, а главное — большинство реализаций складывает поле в память
//! целиком. Нам нужен ровно один файл, поэтому тело запроса — это и есть файл:
//! `xhr.send(file)` на фронте, `body.into_data_stream()` здесь. Имя файла
//! приезжает отдельным заголовком [`FILE_NAME_HEADER`] (percent-encoded, потому
//! что заголовки HTTP — ASCII, а имена файлов бывают любые).
//!
//! # Порядок проверок (важен!)
//!
//! 1. **авторизация** — `409 not_authorized` ДО чтения тела. Иначе клиент зря
//!    заливает гигабайт и только потом узнаёт, что аккаунт не привязан;
//! 2. **`Content-Length`** — без него `411 Length Required`: неизвестен размер
//!    ⇒ нельзя ни показать процент, ни проверить лимит заранее;
//! 3. **лимит** `Content-Length > cfg.max_upload_bytes` ⇒ `413`;
//! 4. **имя файла** — заголовка нет либо после percent-decode пусто ⇒ `400`;
//! 5. и только теперь тело стримится на диск.
//!
//! Проверка связи с VK здесь СОЗНАТЕЛЬНО не делается (лестница урезана до
//! одной ступени): staging — локальная операция, а `503 not_connected`
//! отдаётся штатно там, где реально идёт сеть, — в `POST /messages`. Так
//! короткий сетевой чих не заставляет заново выбирать файл.
//!
//! # Жизненный цикл staging-файла
//!
//! `<cache_dir>/uploads/<upload_id>.part` живёт до одного из трёх событий:
//! успешная отправка сообщения (байты уже в VK), истечение
//! [`crate::state::UPLOAD_TTL_SECS`] (файл выбрали и передумали), рестарт
//! процесса ([`cleanup_uploads_dir`] на старте — реестр живёт в памяти, а
//! значит после рестарта ни один `.part` уже никому не принадлежит).
//!
//! # Про `clippy::result_large_err`
//!
//! Ровно та же причина, что в [`crate::routes::dialogs`]: [`ApiError`] несёт
//! контракт captcha и валидации VK и потому крупнее телеграмного. `Box` ради
//! линта добавил бы аллокацию на КАЖДЫЙ отказ — а отказ здесь штатный ответ
//! (409 без логина приходит на каждый запрос закрытой вкладки).
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use serde_json::Value;

use crate::api::{UploadPart, VkClient};
use crate::progress::{ProgressReader, UploadReporter};
use crate::routes::dialogs::{map_vk_error, require_authorized};
use crate::routes::error::ApiError;
use crate::state::{UploadEntry, VkState, UPLOAD_TTL_SECS};

/// Подкаталог staging-файлов внутри `cache_dir`.
pub const UPLOADS_SUBDIR: &str = "uploads";

/// Расширение staging-файла (совпадает с привычным «недокачано»).
pub const STAGING_EXT: &str = "part";

/// Заголовок с именем файла (percent-encoded значение `encodeURIComponent`).
pub const FILE_NAME_HEADER: &str = "x-forge-file-name";

/// Максимальная длина имени файла в байтах.
///
/// 255 — предел компонента пути почти во всех ФС; VK более длинные имена
/// документов тоже обрезает.
pub const MAX_FILE_NAME_LEN: usize = 255;

/// Имя, которым заменяется пустое/непригодное.
pub const FALLBACK_FILE_NAME: &str = "file";

/// MIME по умолчанию, когда расширение ничего не подсказало.
pub const DEFAULT_MIME: &str = "application/octet-stream";

/// Как часто GC-воркер проверяет реестр загрузок.
pub const UPLOAD_GC_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Сколько фотографий VK принимает ОДНИМ multipart-запросом (поля
/// `file1..file5`).
///
/// Это и есть смысл альбома: пятёрка по одному стоила бы 15 троттленных
/// запросов и пять секунд при лимите 3 rps, одним запросом — те же три.
pub const MAX_ALBUM_PHOTOS: usize = 5;

/// Сколько вложений VK разрешает в одном сообщении.
pub const MAX_ATTACHMENTS: usize = 10;

/// Ответ `POST /api/vk/upload`.
///
/// Форма ДОСЛОВНО телеграмная: фронт обеих панелей читает один и тот же набор
/// полей, и расхождение здесь означало бы разошедшиеся `upload.js`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResp {
    /// Идентификатор загрузки — им сообщение ссылается на файл.
    ///
    /// Это НАШ идентификатор, а не адрес чего-либо в VK (см. шапку модуля).
    pub upload_id: String,
    /// Санитизированное имя файла (именно оно уйдёт в VK).
    pub file_name: String,
    /// Размер в байтах (совпадает с `Content-Length`).
    pub size: u64,
    /// MIME, угаданный по расширению: по нему фронт подписывает карточку, а
    /// сервер выбирает ветку конвейера ([`classify_upload`]).
    pub mime: String,
}

/// Роутер загрузки.
///
/// `max_upload_bytes` приходит из конфига, поэтому лимит тела навешивается
/// ТОЧЕЧНО на этот роут: глобальный `DefaultBodyLimit` приложения (2 МиБ по
/// умолчанию в axum) остаётся как есть, иначе любой другой JSON-роут хоста
/// внезапно начал бы принимать гигабайты.
///
/// Слой — вторая линия обороны, а не единственная: экстрактор [`Body`] тело
/// не «лимитирует» (лимит применяют только буферизующие экстракторы — `Bytes`,
/// `String`, `Json`), поэтому фактический предел проверяется по
/// `Content-Length` до чтения и по счётчику записанных байт во время чтения.
pub fn router(max_upload_bytes: u64) -> Router<Arc<VkState>> {
    let limit = usize::try_from(max_upload_bytes).unwrap_or(usize::MAX);
    Router::new().route(
        "/api/vk/upload",
        post(post_upload).layer(DefaultBodyLimit::max(limit)),
    )
}

/// Каталог staging-файлов.
pub fn uploads_dir(cfg: &crate::config::VkConfig) -> PathBuf {
    cfg.cache_dir.join(UPLOADS_SUBDIR)
}

/// Путь staging-файла конкретной загрузки.
///
/// Имя целиком определяется `upload_id` (UUID v4), а НЕ именем файла от
/// клиента: так в ФС не попадает ни один байт пользовательского ввода, и path
/// traversal через `X-Forge-File-Name` невозможен по построению.
pub fn staging_path(cfg: &crate::config::VkConfig, upload_id: &str) -> PathBuf {
    uploads_dir(cfg).join(format!("{upload_id}.{STAGING_EXT}"))
}

/// Текущее unix-время в секундах.
fn unix_now() -> i64 {
    crate::dto::now_unix()
}

/// Разбирает `Content-Length`.
///
/// Отсутствие заголовка и мусор в нём — РАЗНЫЕ случаи: первое даёт `411`
/// («пришли размер»), второе `400` («размер нечитаем»). Оба возвращаются как
/// `None` только потому, что вызывающий всё равно обязан отказать; различие
/// сделано текстом ошибки.
pub fn parse_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Декодирует percent-encoding (`encodeURIComponent`) в UTF-8-строку.
///
/// Своя реализация вместо крейта `percent-encoding`: нужна одна функция на 20
/// строк, а любая новая зависимость в этом плагине проходит через
/// gate-проверку дерева (`cargo tree -i openssl-sys`). Невалидные байты и
/// незакрытые `%` не считаются ошибкой — они остаются как есть, потому что имя
/// файла не должно ронять загрузку.
pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Приводит имя файла к безопасному виду.
///
/// Шаги (порядок важен):
///
/// 1. только последний сегмент пути — и по `/`, и по `\` (Windows-клиенты
///    присылают `C:\dir\file.jpg`, а `../../evil.sh` не должен пролезть даже в
///    имя, которое увидит получатель);
/// 2. управляющие символы (в т.ч. `\r\n`, которым можно было бы попытаться
///    инжектировать заголовок) выбрасываются;
/// 3. обрезка по краям и по [`MAX_FILE_NAME_LEN`] байт с сохранением границы
///    UTF-8 — полуобрезанный символ сделал бы имя нечитаемым;
/// 4. `.`, `..` и пустая строка ⇒ [`FALLBACK_FILE_NAME`].
///
/// Возвращает `None` ТОЛЬКО для входа, который после decode пуст — это сигнал
/// «клиент не передал имя» (`400`), а не «подставь заглушку».
pub fn sanitize_upload_name(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    let tail = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = tail.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    let capped = truncate_utf8(trimmed, MAX_FILE_NAME_LEN);
    let name = capped.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Some(FALLBACK_FILE_NAME.to_string());
    }
    Some(name.to_string())
}

/// Обрезает строку до `max` байт, не разрывая символ.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// MIME по расширению имени.
///
/// Только по имени и никогда по содержимому: sniffing потребовал бы читать
/// начало файла (а он ещё не пришёл) и всё равно был бы менее точен, чем
/// расширение, — VK тоже смотрит именно на расширение, когда решает, принять
/// ли файл как фотографию.
pub fn guess_mime(file_name: &str) -> String {
    mime_guess::from_path(file_name)
        .first()
        .map(|m| m.essence_str().to_string())
        .unwrap_or_else(|| DEFAULT_MIME.to_string())
}

/// Удаляет staging-файл, если загрузка не дошла до реестра.
///
/// Обрыв соединения (axum дропает future хендлера), несовпадение размера,
/// ошибка записи — после любого из них `.part`-файла оставаться не должно.
struct StagingGuard {
    /// Путь; `None` ⇒ файл уже принадлежит реестру.
    path: Option<PathBuf>,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Загрузка удалась — файл больше не наш.
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            // Синхронное удаление: `Drop` не умеет await, а `remove_file` —
            // один быстрый syscall (файл лежит на локальном диске).
            let _ = std::fs::remove_file(p);
        }
    }
}

/// `POST /api/vk/upload`.
///
/// # Errors
///
/// - `409 not_authorized` — аккаунт не привязан (тело НЕ читается);
/// - `411 length_required` — нет `Content-Length`;
/// - `413 too_large` — размер больше `cfg.max_upload_bytes`;
/// - `400 invalid_input` — нет имени файла, мусорный `Content-Length`, либо
///   фактический размер тела не совпал с заявленным;
/// - `503 not_connected` — ошибка файловой системы (для клиента это
///   «повтори»: диск и права — не его вина и не его дело).
pub async fn post_upload(
    State(state): State<Arc<VkState>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<UploadResp>, ApiError> {
    // (1) Авторизация — до чтения тела.
    require_authorized(&state).await?;

    // (2) Размер обязателен.
    if !headers.contains_key(axum::http::header::CONTENT_LENGTH) {
        return Err(ApiError::length_required(
            "Content-Length is required: without it the upload cannot be size-checked or shown as progress",
        ));
    }
    let declared = parse_content_length(&headers).ok_or_else(|| {
        ApiError::bad_input("Content-Length must be a non-negative integer number of bytes")
    })?;

    // (3) Лимит.
    let max = state.config.max_upload_bytes;
    if declared > max {
        return Err(ApiError::too_large(format!(
            "file is {declared} bytes, which exceeds the limit of {max} bytes \
             (raise max_upload_bytes in vk.toml to allow it)"
        )));
    }

    // (4) Имя файла.
    let raw_name = headers
        .get(FILE_NAME_HEADER)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .ok_or_else(|| {
            ApiError::bad_input(format!(
                "header {FILE_NAME_HEADER} is required (percent-encoded original file name)"
            ))
        })?;
    let file_name = sanitize_upload_name(&percent_decode(&raw_name)).ok_or_else(|| {
        ApiError::bad_input(format!("header {FILE_NAME_HEADER} decodes to an empty name"))
    })?;
    let mime = guess_mime(&file_name);

    // (5) Тело — на диск.
    let upload_id = uuid::Uuid::new_v4().to_string();
    let dir = uploads_dir(&state.config);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        ApiError::not_connected(format!("could not create upload staging directory: {e}"))
    })?;
    let path = staging_path(&state.config, &upload_id);
    let mut guard = StagingGuard::new(path.clone());

    let written = stream_to_file(body, &path, declared).await?;
    if written != declared {
        return Err(ApiError::bad_input(format!(
            "body ended after {written} bytes, but Content-Length promised {declared}"
        )));
    }

    let entry = UploadEntry {
        path: path.clone(),
        file_name: file_name.clone(),
        size: declared,
        mime: mime.clone(),
        created_at: unix_now(),
    };
    // Ленивая подчистка на каждой новой загрузке: даже если GC-воркер почему-то
    // не запущен (тесты, урезанная сборка), реестр не растёт без предела.
    evict_stale(&state).await;
    state.insert_upload(upload_id.clone(), entry).await;
    guard.disarm();

    tracing::info!(
        target: "forge_vk",
        upload_id = %upload_id,
        size = declared,
        mime = %mime,
        "forge-vk: file staged for sending"
    );
    Ok(Json(UploadResp {
        upload_id,
        file_name,
        size: declared,
        mime,
    }))
}

/// Стримит тело запроса в файл и возвращает число записанных байт.
///
/// Буферизации целиком в памяти нет ни на одном шаге: чанк пришёл — чанк
/// записан. Поэтому заливка 500 МБ не двигает RSS процесса.
///
/// `limit` — уже проверенный `Content-Length`. Он же второй предохранитель:
/// клиент, который прислал больше, чем обещал, обрывается на первом лишнем
/// байте, а не после того, как заполнит диск.
async fn stream_to_file(body: Body, path: &Path, limit: u64) -> Result<u64, ApiError> {
    let file = tokio::fs::File::create(path)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not create staging file: {e}")))?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            ApiError::bad_input(format!("upload stream broke after {written} bytes: {e}"))
        })?;
        written += chunk.len() as u64;
        if written > limit {
            return Err(ApiError::bad_input(format!(
                "body is larger than the declared Content-Length of {limit} bytes"
            )));
        }
        writer
            .write_all(&chunk)
            .await
            .map_err(|e| ApiError::not_connected(format!("could not write staging file: {e}")))?;
    }

    writer
        .flush()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not flush staging file: {e}")))?;
    // sync_all, а не только flush: дальше файл читает конвейер отправки, и он
    // обязан увидеть все байты, а не хвост в страничном кэше.
    writer
        .into_inner()
        .sync_all()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not sync staging file: {e}")))?;
    Ok(written)
}

// ---------------------------------------------------------------------------
// Реестр загрузок: TTL, сироты, потребление.
// ---------------------------------------------------------------------------

/// Открывает staging-файл загрузки.
///
/// # Errors
///
/// `409 invalid_state`, если файла нет: запись в реестре есть, а байтов нет —
/// значит, GC уже прибрал протухшую загрузку либо кто-то почистил кэш руками.
/// Клиенту это «залей файл заново», а не «сервер сломался».
pub async fn open_staging(entry: &UploadEntry) -> Result<tokio::fs::File, ApiError> {
    tokio::fs::File::open(&entry.path).await.map_err(|e| {
        ApiError::invalid_state(format!(
            "staged file for this upload_id is no longer available ({e}): upload the file again"
        ))
    })
}

/// Выбрасывает из реестра протухшие записи вместе с их staging-файлами.
///
/// Возвращает число удалённых записей.
pub async fn evict_stale(state: &Arc<VkState>) -> usize {
    let stale = state.evict_stale_uploads(UPLOAD_TTL_SECS, unix_now()).await;
    for entry in &stale {
        let _ = tokio::fs::remove_file(&entry.path).await;
    }
    if !stale.is_empty() {
        tracing::info!(
            target: "forge_vk",
            removed = stale.len(),
            ttl_secs = UPLOAD_TTL_SECS,
            "forge-vk: expired uploads discarded"
        );
    }
    stale.len()
}

/// Удаляет staging-файл потреблённой загрузки (сообщение отправлено).
///
/// Ошибка удаления не фатальна: файл всё равно подберёт [`evict_stale`] или
/// [`cleanup_uploads_dir`] после рестарта.
pub async fn discard_staging(entry: &UploadEntry) {
    if let Err(e) = tokio::fs::remove_file(&entry.path).await {
        tracing::debug!(
            target: "forge_vk",
            path = %entry.path.display(),
            error = %e,
            "forge-vk: could not remove staging file"
        );
    }
}

/// Чистит каталог staging'а на старте процесса.
///
/// Реестр `uploads` живёт только в памяти, поэтому после рестарта каждый
/// `.part` — сирота: сослаться на него уже некому, а место он занимает вполне
/// реальное. Возвращает число удалённых файлов.
pub async fn cleanup_uploads_dir(cfg: &crate::config::VkConfig) -> usize {
    let dir = uploads_dir(cfg);
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let is_file = entry.metadata().await.map(|m| m.is_file()).unwrap_or(false);
        if is_file && tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            target: "forge_vk",
            removed,
            dir = %dir.display(),
            "forge-vk: orphaned upload staging files removed on startup"
        );
    }
    removed
}

/// Фоновый воркер: подчистка протухших загрузок раз в [`UPLOAD_GC_INTERVAL`].
///
/// Первый прогон — сразу на старте (он же убирает сирот от прошлого процесса),
/// дальше по таймеру. Останавливается по `shutdown`-токену.
pub async fn uploads_gc_loop(state: Arc<VkState>) {
    cleanup_uploads_dir(&state.config).await;
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_vk", "forge-vk: uploads GC stopped");
                return;
            }
            _ = tokio::time::sleep(UPLOAD_GC_INTERVAL) => {}
        }
        evict_stale(&state).await;
    }
}

/// Запускает GC-воркер загрузок и регистрирует его для graceful shutdown.
pub async fn spawn_uploads_gc(state: &Arc<VkState>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { uploads_gc_loop(st).await });
    state.register_worker(handle).await;
    tracing::info!(
        target: "forge_vk",
        interval_secs = UPLOAD_GC_INTERVAL.as_secs(),
        ttl_secs = UPLOAD_TTL_SECS,
        "forge-vk: uploads GC started"
    );
}

// ---------------------------------------------------------------------------
// Вторая половина: staging-файл → VK. Три шага плюс отправка.
// ---------------------------------------------------------------------------

/// Ветка конвейера, по которой файл уезжает в VK.
///
/// У Telegram ветка одна (`upload_stream` + `InputMessage::file`), у VK их три,
/// и выбирается ветка ДО заливки: upload-сервер для фото, документа и
/// голосового — разные адреса, выданные разными методами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    /// `photos.getMessagesUploadServer` → `photos.saveMessagesPhoto`.
    Photo,
    /// `docs.getMessagesUploadServer(type=doc)` → `docs.save`.
    Doc,
    /// `docs.getMessagesUploadServer(type=audio_message)` → `docs.save`.
    ///
    /// Голосовое у VK — документ особого типа, и тип этот задаётся ПРИ ВЫДАЧЕ
    /// адреса, а не при сохранении: перепутав его, получаешь обычный файл
    /// `.ogg` вместо кружка с осциллограммой.
    AudioMessage,
}

impl UploadKind {
    /// Значение параметра `type` метода `docs.getMessagesUploadServer`.
    pub fn doc_type(self) -> &'static str {
        match self {
            Self::AudioMessage => "audio_message",
            _ => "doc",
        }
    }

    /// Фотографии едут своим конвейером и альбомом.
    pub fn is_photo(self) -> bool {
        matches!(self, Self::Photo)
    }
}

/// Выбор ветки конвейера по MIME и имени файла. ЧИСТАЯ.
///
/// Правила:
///
/// | вход | ветка | почему |
/// |---|---|---|
/// | `audio/ogg` + расширение `.ogg` | [`UploadKind::AudioMessage`] | голосовое сообщение |
/// | `image/gif`, `image/svg+xml` | [`UploadKind::Doc`] | **исключение**, см. ниже |
/// | остальные `image/*` | [`UploadKind::Photo`] | обычная фотография |
/// | всё прочее | [`UploadKind::Doc`] | документ |
///
/// # Почему gif и svg — документы
///
/// Сохранённая как фотография гифка теряет анимацию (VK берёт первый кадр), а
/// svg он не принимает как изображение вовсе. Это не самодеятельность: фронт
/// (`js/vk/format.js::uploadKind`) уже относит оба типа к документам и по этому
/// же правилу выбирает лимит размера. Разойдись мы здесь — пользователь получил
/// бы отказ по «документному» лимиту в 200 МБ на файл, который сервер попытался
/// бы отправить фотографией.
///
/// # Почему у голосового два условия
///
/// `audio/ogg` бывает и у обычной музыки, скачанной в ogg; кружком-голосовым в
/// VK становится только то, что записано браузером (`MediaRecorder` отдаёт
/// именно `.ogg`). Требование расширения — дешёвый способ не превратить
/// музыкальный файл в голосовое, которое нельзя перемотать.
pub fn classify_upload(mime: &str, file_name: &str) -> UploadKind {
    let m = mime.trim().to_ascii_lowercase();
    let name = file_name.trim().to_ascii_lowercase();
    if m == "audio/ogg" && name.ends_with(".ogg") {
        return UploadKind::AudioMessage;
    }
    if m == "image/gif" || m == "image/svg+xml" {
        return UploadKind::Doc;
    }
    if m.starts_with("image/") {
        return UploadKind::Photo;
    }
    UploadKind::Doc
}

/// Строка вложения VK из ответа save-метода. ЧИСТАЯ.
///
/// Форма — `<тип><owner_id>_<media_id>[_<access_key>]`. `access_key`
/// обязателен для приватных объектов и безвреден для публичных, поэтому
/// добавляется всегда, когда VK его прислал.
///
/// Для документа и голосового префикс — `doc` в обоих случаях: голосовое у VK
/// хранится документом, и адресуется тоже как документ. `audio_message` —
/// название ТИПА вложения в полученном сообщении, а не префикс адреса, и
/// перепутать их означает получить от VK «вложение не найдено».
///
/// `None` означает «ответ не похож на сохранённое вложение» (нулевые
/// идентификаторы, чужая форма): вызывающий обязан отказать, а не отправить
/// сообщение с мусорной строкой — VK принял бы его как текст без вложения.
pub fn attachment_string(kind: UploadKind, saved: &Value) -> Option<String> {
    let obj = match kind {
        UploadKind::Photo => saved,
        // docs.save отдаёт объект с полем по имени типа; берём первое
        // непустое, чтобы не зависеть от того, чем VK назовёт граффити.
        UploadKind::Doc | UploadKind::AudioMessage => ["doc", "audio_message", "graffiti"]
            .into_iter()
            .find_map(|k| saved.get(k))
            .unwrap_or(saved),
    };

    let id = scalar_i64(obj, "id")?;
    let owner = scalar_i64(obj, "owner_id")?;
    if id == 0 || owner == 0 {
        return None;
    }
    let prefix = if kind.is_photo() { "photo" } else { "doc" };
    let key = obj
        .get("access_key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    Some(match key {
        Some(k) => format!("{prefix}{owner}_{id}_{k}"),
        None => format!("{prefix}{owner}_{id}"),
    })
}

/// Числовое поле, пережившее присылку строкой.
///
/// VK отдаёт `server` у upload-сервера то числом, то строкой в зависимости от
/// метода и версии API; строгий разбор ронял бы отправку на ровном месте.
fn scalar_i64(value: &Value, key: &str) -> Option<i64> {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Строковое поле, пережившее присылку числом.
fn scalar_string(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Отчётчик прогресса, разделённый между колбэком ридера и вызывающим.
///
/// `std::sync::Mutex`, а не `tokio::sync::Mutex`: колбэк дёргается из
/// `poll_read`, то есть из синхронного контекста, где `await` невозможен.
/// Критическая секция — одна арифметическая проверка throttle плюс
/// `broadcast::send`, поэтому блокировки исполнителя не создаёт.
pub type SharedReporter = Arc<std::sync::Mutex<UploadReporter>>;

/// Создаёт отчётчик `upload_progress` для одной загрузки.
pub fn shared_reporter(state: Arc<VkState>, upload_id: &str, total: u64) -> SharedReporter {
    Arc::new(std::sync::Mutex::new(UploadReporter::new(
        state, upload_id, total,
    )))
}

/// Колбэк для [`ProgressReader`].
///
/// Отравленный mutex (паника в другом потоке) молча игнорируется: прогрессбар —
/// не причина ронять уже идущую отправку файла.
fn progress_hook(reporter: &SharedReporter) -> impl FnMut(u64) + Unpin {
    let rep = reporter.clone();
    move |sent: u64| {
        if let Ok(mut r) = rep.lock() {
            r.report(sent);
        }
    }
}

/// Собирает часть multipart-запроса из staging-файла.
///
/// Именно здесь сходятся три вещи: файл читается ПОТОКОМ
/// ([`tokio_util::io::ReaderStream`]), каждый прочитанный кусок двигает
/// прогресс ([`ProgressReader`]), а `size` попадает в
/// `Part::stream_with_length` — без него `reqwest` ушёл бы в chunked, который
/// upload-сервер VK не принимает, и заодно забуферизовал бы файл целиком.
///
/// # Errors
///
/// `409 invalid_state`, если staging-файла уже нет (GC успел раньше).
async fn staged_part(
    state: &Arc<VkState>,
    upload_id: &str,
    entry: &UploadEntry,
    field: &str,
) -> Result<(UploadPart, SharedReporter), ApiError> {
    let file = open_staging(entry).await?;
    let reporter = shared_reporter(state.clone(), upload_id, entry.size);
    let reader = ProgressReader::new(file, progress_hook(&reporter));
    let stream = tokio_util::io::ReaderStream::new(reader);
    Ok((
        UploadPart {
            field: field.to_string(),
            file_name: entry.file_name.clone(),
            mime: entry.mime.clone(),
            size: entry.size,
            body: reqwest::Body::wrap_stream(stream),
        },
        reporter,
    ))
}

/// Досылает финальный кадр `upload_progress` (`sent == total`).
///
/// Уходит ТОЛЬКО после успешной заливки: карточка, оставшаяся в ошибке, не
/// имеет права показать «✓ SENT». Без этого кадра прогрессбар навсегда замер бы
/// на 97 % — throttle мог съесть последний промежуточный отчёт.
fn finish_reporters(reporters: &[(String, SharedReporter, u64)]) {
    for (_, rep, total) in reporters {
        if let Ok(mut r) = rep.lock() {
            r.finish(*total);
        }
    }
}

/// Заливает до [`MAX_ALBUM_PHOTOS`] фотографий ОДНИМ конвейером.
///
/// Три шага: адрес (троттлится) → multipart (не троттлится) → сохранение
/// (троттлится). Возвращает attachment-строки В ПОРЯДКЕ переданных элементов.
///
/// # Errors
///
/// Ошибки VK как есть ([`map_vk_error`]), `409 invalid_state` при исчезнувшем
/// staging'е, `502 vk_error` — если ответ upload-сервера или save-метода не
/// похож на сохранённые фотографии.
pub async fn upload_photos(
    state: &Arc<VkState>,
    client: &VkClient,
    peer_id: i64,
    items: &[(String, UploadEntry)],
) -> Result<Vec<String>, ApiError> {
    if items.is_empty() {
        return Ok(Vec::new());
    }

    // (1) Адрес — под КОНКРЕТНЫЙ peer_id. Ради этого весь конвейер и живёт
    //     внутри отправки сообщения.
    let server = match client.photos_upload_server(peer_id).await {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };

    // (2) Файлы — одним запросом, полями file1..fileN.
    let mut parts = Vec::with_capacity(items.len());
    let mut reporters = Vec::with_capacity(items.len());
    for (i, (upload_id, entry)) in items.iter().enumerate() {
        let (part, rep) = staged_part(state, upload_id, entry, &format!("file{}", i + 1)).await?;
        parts.push(part);
        reporters.push((upload_id.clone(), rep, entry.size));
    }
    let uploaded = client
        .upload_multipart_parts(&server.upload_url, parts)
        .await;
    let uploaded = match uploaded {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };
    finish_reporters(&reporters);

    // (3) Сохранение — тоже одним вызовом на всю пятёрку.
    let photo = scalar_string(&uploaded, "photo").ok_or_else(|| {
        ApiError::vk_error("the VK upload server returned no `photo` field: the file was not accepted")
    })?;
    let server_id = scalar_i64(&uploaded, "server").ok_or_else(|| {
        ApiError::vk_error("the VK upload server returned no `server` field: the file was not accepted")
    })?;
    let hash = scalar_string(&uploaded, "hash").unwrap_or_default();

    let saved = match client.photos_save(&photo, server_id, &hash).await {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };
    let list = saved.as_array().cloned().unwrap_or_default();
    if list.len() < items.len() {
        return Err(ApiError::vk_error(format!(
            "VK saved {} of {} uploaded photos: nothing was sent, upload them again",
            list.len(),
            items.len()
        )));
    }

    let mut out = Vec::with_capacity(items.len());
    for value in list.iter().take(items.len()) {
        out.push(attachment_string(UploadKind::Photo, value).ok_or_else(|| {
            ApiError::vk_error("VK returned a saved photo without identifiers")
        })?);
    }
    Ok(out)
}

/// Заливает ОДИН документ (или голосовое) — те же три шага.
///
/// # Errors
///
/// Как у [`upload_photos`].
pub async fn upload_doc(
    state: &Arc<VkState>,
    client: &VkClient,
    peer_id: i64,
    upload_id: &str,
    entry: &UploadEntry,
    kind: UploadKind,
) -> Result<String, ApiError> {
    // (1) Адрес: тип документа задаётся ЗДЕСЬ — перепутав, получишь файл .ogg
    //     вместо голосового.
    let server = match client.docs_upload_server(peer_id, kind.doc_type()).await {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };

    // (2) Файл — поле `file`.
    let (part, reporter) = staged_part(state, upload_id, entry, "file").await?;
    let uploaded = match client
        .upload_multipart_parts(&server.upload_url, vec![part])
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };
    finish_reporters(&[(upload_id.to_string(), reporter, entry.size)]);

    // (3) Сохранение.
    let file = scalar_string(&uploaded, "file").ok_or_else(|| {
        ApiError::vk_error("the VK upload server returned no `file` field: the file was not accepted")
    })?;
    let saved = match client.docs_save(&file, Some(&entry.file_name)).await {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };
    attachment_string(kind, &saved)
        .ok_or_else(|| ApiError::vk_error("VK returned a saved document without identifiers"))
}

/// Собирает строку `attachment` метода `messages.send` из всех загрузок.
///
/// Фотографии группируются по [`MAX_ALBUM_PHOTOS`] и уезжают альбомами,
/// документы — по одному (у VK нет способа сохранить два документа одним
/// вызовом). Порядок вложений в строке — ПОРЯДОК ПОЛЬЗОВАТЕЛЯ: он выбирал файлы
/// в определённой последовательности, и перестановка выглядела бы как ошибка
/// отправки.
///
/// # Errors
///
/// Всё, чем могут ответить [`upload_photos`] и [`upload_doc`].
pub async fn build_attachment(
    state: &Arc<VkState>,
    client: &VkClient,
    peer_id: i64,
    items: &[(String, UploadEntry)],
) -> Result<String, ApiError> {
    let mut slots: Vec<Option<String>> = vec![None; items.len()];

    // Фотографии — пачками по пять, с запоминанием исходных позиций.
    let photos: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, (_, e))| classify_upload(&e.mime, &e.file_name).is_photo())
        .map(|(i, _)| i)
        .collect();
    for chunk in photos.chunks(MAX_ALBUM_PHOTOS) {
        let batch: Vec<(String, UploadEntry)> =
            chunk.iter().map(|i| items[*i].clone()).collect();
        let strings = upload_photos(state, client, peer_id, &batch).await?;
        for (slot, s) in chunk.iter().zip(strings) {
            slots[*slot] = Some(s);
        }
    }

    // Документы и голосовые — по одному.
    for (i, (upload_id, entry)) in items.iter().enumerate() {
        let kind = classify_upload(&entry.mime, &entry.file_name);
        if kind.is_photo() {
            continue;
        }
        slots[i] = Some(upload_doc(state, client, peer_id, upload_id, entry, kind).await?);
    }

    Ok(slots
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(","))
}

/// ПОТРЕБЛЯЕТ записи реестра под все переданные `upload_id`.
///
/// # Почему потребление безвозвратно
///
/// `take_upload` убирает запись из реестра ЕЩЁ ДО заливки, и это осознанный
/// размен: повторный `POST /messages` с тем же `upload_id` обязан получить
/// `409`, а не отправить файл второй раз. Двойной клик по «Отправить» и
/// автоповтор фронта — вещи куда более частые, чем неудачная заливка, и цена
/// ошибки у них разная: лишнее сообщение у собеседника не отзывается, а файл
/// всегда можно залить заново.
///
/// Staging уже взятых записей подчищается сразу же, если взять удалось не всё:
/// иначе оборванная попытка оставляла бы файлы до истечения TTL.
///
/// # Errors
///
/// `409 invalid_state` — записи нет: либо `upload_id` уже потрачен, либо
/// протух по [`crate::state::UPLOAD_TTL_SECS`].
pub async fn take_uploads(
    state: &Arc<VkState>,
    upload_ids: &[String],
) -> Result<Vec<(String, UploadEntry)>, ApiError> {
    let mut taken: Vec<(String, UploadEntry)> = Vec::with_capacity(upload_ids.len());
    for id in upload_ids {
        match state.take_upload(id).await {
            Some(entry) => taken.push((id.clone(), entry)),
            None => {
                for (_, e) in &taken {
                    discard_staging(e).await;
                }
                return Err(ApiError::invalid_state(format!(
                    "upload_id '{id}' is unknown, already used or expired: upload the file again"
                )));
            }
        }
    }
    Ok(taken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VkConfig;
    use crate::login::{LoginFlow, VkUserDto};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn state_in(dir: &Path) -> Arc<VkState> {
        VkState::new(VkConfig {
            cache_dir: dir.join("cache"),
            config_path: dir.join("vk.toml"),
            ..VkConfig::default()
        })
    }

    async fn authorized(dir: &Path) -> Arc<VkState> {
        let st = state_in(dir);
        st.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1".into(),
                ..Default::default()
            },
        })
        .await;
        st
    }

    fn app(state: Arc<VkState>) -> Router {
        let limit = state.config.max_upload_bytes;
        router(limit).with_state(state)
    }

    fn upload_req(name: Option<&str>, len: Option<usize>, body: &'static [u8]) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/vk/upload")
            .header("content-type", "application/octet-stream");
        if let Some(n) = name {
            b = b.header(FILE_NAME_HEADER, n);
        }
        if let Some(l) = len {
            b = b.header("content-length", l);
        }
        b.body(Body::from(body)).unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    // ---------------- чистые функции ----------------

    #[test]
    fn percent_decode_handles_encode_uri_component_output() {
        assert_eq!(percent_decode("plain.txt"), "plain.txt");
        assert_eq!(percent_decode("my%20photo.jpg"), "my photo.jpg");
        // Кириллица: encodeURIComponent('отчёт.pdf').
        assert_eq!(
            percent_decode("%D0%BE%D1%82%D1%87%D1%91%D1%82.pdf"),
            "отчёт.pdf"
        );
        // Незакрытый и мусорный процент остаются как есть, а не рушат разбор.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zz.bin"), "a%zz.bin");
        assert_eq!(percent_decode("%2F%2E%2E%2Fetc"), "/../etc");
    }

    /// Оборванный escape в конце строки не должен ни паниковать (индексация за
    /// границей), ни съедать символы.
    #[test]
    fn percent_decode_survives_truncated_and_invalid_sequences() {
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("file%2"), "file%2");
        // Двойной процент: первый — литерал, со второго начинается escape.
        assert_eq!(percent_decode("%%41"), "%A");
        let decoded = percent_decode("%FF%FE.bin");
        assert!(decoded.contains('\u{FFFD}'), "{decoded:?}");
        assert!(decoded.ends_with(".bin"), "{decoded:?}");
    }

    #[test]
    fn sanitize_upload_name_strips_paths_and_controls() {
        assert_eq!(sanitize_upload_name("report.pdf").unwrap(), "report.pdf");
        assert_eq!(sanitize_upload_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(
            sanitize_upload_name("C:\\Windows\\evil.exe").unwrap(),
            "evil.exe"
        );
        assert_eq!(
            sanitize_upload_name("a\r\nX-Evil: 1.txt").unwrap(),
            "aX-Evil: 1.txt"
        );
        assert_eq!(sanitize_upload_name("..").unwrap(), FALLBACK_FILE_NAME);
        assert_eq!(sanitize_upload_name("./").unwrap(), FALLBACK_FILE_NAME);
        assert!(sanitize_upload_name("").is_none());
        assert!(sanitize_upload_name("   ").is_none());
    }

    /// Traversal, NUL и CRLF, протащенные через percent-encoding, не должны
    /// доживать до имени: decode и sanitize работают в связке.
    #[test]
    fn decoded_traversal_and_nul_bytes_do_not_reach_the_name() {
        assert_eq!(
            sanitize_upload_name(&percent_decode("..%2F..%2Fetc%2Fpasswd")).unwrap(),
            "passwd"
        );
        assert_eq!(
            sanitize_upload_name(&percent_decode("a%00b.txt")).unwrap(),
            "ab.txt"
        );
        assert_eq!(
            sanitize_upload_name(&percent_decode("evil%0D%0A.txt")).unwrap(),
            "evil.txt"
        );
    }

    /// Граница в 255 байт: ровно 255 — без изменений, 256 — обрезка, а для
    /// 4-байтовых emoji обрезка обязана попасть на границу символа.
    #[test]
    fn sanitize_upload_name_at_the_255_byte_boundary() {
        let exact = "a".repeat(MAX_FILE_NAME_LEN);
        assert_eq!(sanitize_upload_name(&exact).unwrap(), exact);

        let over = "a".repeat(MAX_FILE_NAME_LEN + 1);
        assert_eq!(sanitize_upload_name(&over).unwrap().len(), MAX_FILE_NAME_LEN);

        // 64 emoji по 4 байта = 256 байт ⇒ корректная обрезка даёт 63 целых.
        let emoji = "🎉".repeat(64);
        let got = sanitize_upload_name(&emoji).unwrap();
        assert_eq!(got.chars().count(), 63, "len={}", got.len());
        assert_eq!(got.len(), 252);

        // Кириллица (2 байта на символ) — тоже по границе символа.
        let long = "я".repeat(400);
        let got = sanitize_upload_name(&long).unwrap();
        assert!(got.len() <= MAX_FILE_NAME_LEN, "{}", got.len());
        assert!(got.chars().all(|c| c == 'я'));
    }

    /// Пробельный мусор и управляющий мусор — разные случаи: первый значит
    /// «имени нет» (роут ответит 400), второй — «имя непригодно» (заглушка).
    #[test]
    fn sanitize_upload_name_distinguishes_whitespace_from_control_garbage() {
        assert!(sanitize_upload_name("\r\n\t").is_none());
        assert_eq!(
            sanitize_upload_name("\u{1}\u{2}").unwrap(),
            FALLBACK_FILE_NAME
        );
        assert_eq!(sanitize_upload_name("dir/").unwrap(), FALLBACK_FILE_NAME);
    }

    #[test]
    fn guess_mime_uses_extension() {
        assert_eq!(guess_mime("a.jpg"), "image/jpeg");
        assert_eq!(guess_mime("a.png"), "image/png");
        assert_eq!(guess_mime("a.pdf"), "application/pdf");
        assert_eq!(guess_mime("a.mp4"), "video/mp4");
        assert_eq!(guess_mime("a.ogg"), "audio/ogg");
        assert_eq!(guess_mime("noext"), DEFAULT_MIME);
        assert_eq!(guess_mime("a.unknown-ext-xyz"), DEFAULT_MIME);
        // Камеры и Windows-клиенты присылают расширения в верхнем регистре.
        assert_eq!(guess_mime("DSC0001.JPG"), "image/jpeg");
        assert_eq!(guess_mime("archive."), DEFAULT_MIME);
        assert_eq!(guess_mime(".gitignore"), DEFAULT_MIME);
    }

    #[test]
    fn parse_content_length_rejects_garbage() {
        let mut h = HeaderMap::new();
        assert_eq!(parse_content_length(&h), None);
        h.insert("content-length", "1024".parse().unwrap());
        assert_eq!(parse_content_length(&h), Some(1024));
        h.insert("content-length", "-1".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
        h.insert("content-length", "abc".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
        // u64::MAX разбирается, а u64::MAX + 1 — мусор (без усечения).
        h.insert("content-length", u64::MAX.to_string().parse().unwrap());
        assert_eq!(parse_content_length(&h), Some(u64::MAX));
        h.insert("content-length", "18446744073709551616".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
    }

    #[test]
    fn staging_path_never_contains_client_input() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = VkConfig {
            cache_dir: dir.path().join("cache"),
            ..VkConfig::default()
        };
        let p = staging_path(&cfg, "0191b0de-1111-4222-8333-444455556666");
        assert!(p.starts_with(uploads_dir(&cfg)));
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "0191b0de-1111-4222-8333-444455556666.part"
        );
    }

    // ---------------- роут ----------------

    /// Без авторизации — 409, и тело даже не читается: каталог staging'а не
    /// создан, реестр пуст.
    #[tokio::test]
    async fn upload_without_auth_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
        assert!(st.uploads.lock().await.is_empty());
        assert!(!uploads_dir(&st.config).exists(), "каталог даже не создан");
    }

    #[tokio::test]
    async fn upload_without_content_length_is_411() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st)
            .oneshot(upload_req(Some("a.txt"), None, b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::LENGTH_REQUIRED);
        assert_eq!(body_json(resp).await["error"]["code"], "length_required");
    }

    #[tokio::test]
    async fn upload_over_limit_is_413() {
        let dir = tempfile::tempdir().unwrap();
        let st = VkState::new(VkConfig {
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            // Настоящие 2 ГиБ в тесте не пришлёшь.
            max_upload_bytes: 4,
            ..VkConfig::default()
        });
        st.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1".into(),
                ..Default::default()
            },
        })
        .await;

        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "too_large");
        assert!(
            json["error"]["message"].as_str().unwrap().contains('4'),
            "в сообщении обязан быть лимит: {json}"
        );
        assert!(st.uploads.lock().await.is_empty());
    }

    #[tokio::test]
    async fn upload_without_name_is_400() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(None, Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"]["code"], "invalid_input");

        // Заголовок есть, но декодируется в пустоту.
        let resp = app(st.clone())
            .oneshot(upload_req(Some("%20%20"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // И пустое значение заголовка — тоже «имени нет».
        let resp = app(st)
            .oneshot(upload_req(Some(""), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upload_stages_file_and_registers_it() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(
                Some("%D0%BE%D1%82%D1%87%D1%91%D1%82.pdf"),
                Some(5),
                b"hello",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let id = json["upload_id"].as_str().unwrap().to_string();
        assert!(!id.is_empty());
        assert_eq!(json["file_name"], "отчёт.pdf");
        assert_eq!(json["size"], 5);
        assert_eq!(json["mime"], "application/pdf");

        let entry = st.upload_entry(&id).await.expect("запись в реестре");
        assert_eq!(entry.size, 5);
        assert_eq!(entry.file_name, "отчёт.pdf");
        assert_eq!(entry.mime, "application/pdf");
        assert_eq!(tokio::fs::read(&entry.path).await.unwrap(), b"hello");
        // Имя staging'а — только upload_id, ни байта клиентского ввода.
        assert_eq!(entry.path, staging_path(&st.config, &id));
    }

    /// НИ ОДНОГО обращения к VK этот роут не делает — это и есть структурное
    /// решение фазы. Проверяется тем, что `base_url` указывает на закрытый
    /// порт: любой сетевой вызов дал бы отказ и здоровье `Down`.
    #[tokio::test]
    async fn upload_never_touches_vk() {
        let dir = tempfile::tempdir().unwrap();
        let st = VkState::new(VkConfig {
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            base_url: "http://127.0.0.1:1/method".into(),
            ..VkConfig::default()
        });
        st.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1".into(),
                ..Default::default()
            },
        })
        .await;

        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.jpg"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            matches!(st.health().await, crate::state::ConnHealth::Ok { at: 0 }),
            "здоровье не тронуто ⇒ сети не было: {:?}",
            st.health().await
        );
    }

    /// Тело короче обещанного — 400, и staging не остаётся мусором.
    #[tokio::test]
    async fn short_body_is_rejected_and_staging_removed() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.bin"), Some(100), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(st.uploads.lock().await.is_empty());
        let left = std::fs::read_dir(uploads_dir(&st.config))
            .map(|rd| rd.count())
            .unwrap_or(0);
        assert_eq!(left, 0, "staging-файл обязан быть удалён");
    }

    /// Тело длиннее обещанного обрывается, а не пишется на диск целиком.
    #[tokio::test]
    async fn oversized_body_is_rejected_and_staging_removed() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.bin"), Some(2), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(st.uploads.lock().await.is_empty());
        let left = std::fs::read_dir(uploads_dir(&st.config))
            .map(|rd| rd.count())
            .unwrap_or(0);
        assert_eq!(left, 0);
    }

    /// Переполняющий u64 `Content-Length` от сломанного клиента — 400, а не
    /// 500 и не молчаливое усечение до меньшего числа.
    #[tokio::test]
    async fn content_length_overflow_is_bad_input_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/vk/upload")
            .header(FILE_NAME_HEADER, "a.bin")
            .header("content-length", "18446744073709551616")
            .body(Body::from(&b"hello"[..]))
            .unwrap();
        let resp = app(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"]["code"], "invalid_input");
        assert!(st.uploads.lock().await.is_empty());
    }

    /// Ровно `max_upload_bytes` — это ещё НЕ «больше лимита» (off-by-one).
    #[tokio::test]
    async fn upload_exactly_at_the_limit_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let st = VkState::new(VkConfig {
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            max_upload_bytes: 5,
            ..VkConfig::default()
        });
        st.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "1".into(),
                ..Default::default()
            },
        })
        .await;
        let resp = app(st)
            .oneshot(upload_req(Some("edge.bin"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["size"], 5);
    }

    /// Пользователь выбрал пустой файл: размер 0 — валидная граница, staging
    /// создаётся пустым, без 500 и без деления на ноль в прогрессе.
    #[tokio::test]
    async fn zero_byte_upload_is_staged_without_errors() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("empty.txt"), Some(0), b""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["size"], 0);
        let id = json["upload_id"].as_str().unwrap();
        let entry = st.upload_entry(id).await.expect("запись в реестре");
        assert_eq!(tokio::fs::metadata(&entry.path).await.unwrap().len(), 0);
    }

    /// Тело приходит МНОГИМИ чанками и пишется по мере поступления.
    ///
    /// Это и есть проверяемая часть критерия «заливка не растит RSS»: тело
    /// собирается ленивым стримом, никогда не существует целиком ни в одном
    /// буфере, а на диск попадает байт-в-байт.
    #[tokio::test]
    async fn body_is_written_chunk_by_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;

        const CHUNK: usize = 64 * 1024;
        const CHUNKS: usize = 64; // 4 МиБ
        let stream = futures_util::stream::iter((0..CHUNKS).map(|i| {
            let byte = (i % 251) as u8;
            Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![byte; CHUNK]))
        }));
        let req = Request::builder()
            .method("POST")
            .uri("/api/vk/upload")
            .header("content-type", "application/octet-stream")
            .header(FILE_NAME_HEADER, "big.bin")
            .header("content-length", CHUNK * CHUNKS)
            .body(Body::from_stream(stream))
            .unwrap();

        let resp = app(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["size"], (CHUNK * CHUNKS) as u64);

        let id = json["upload_id"].as_str().unwrap();
        let entry = st.upload_entry(id).await.unwrap();
        let md = tokio::fs::metadata(&entry.path).await.unwrap();
        assert_eq!(md.len(), (CHUNK * CHUNKS) as u64);
        let bytes = tokio::fs::read(&entry.path).await.unwrap();
        assert_eq!(bytes[0], 0);
        assert_eq!(bytes[bytes.len() - 1], ((CHUNKS - 1) % 251) as u8);
    }

    /// curl-клиенты шлют UTF-8 байты в заголовке КАК ЕСТЬ, без
    /// percent-encoding — lossy-декод обязан дать читаемое имя, а не 500.
    #[tokio::test]
    async fn raw_utf8_file_name_header_is_decoded_lossily() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let mut req = upload_req(None, Some(5), b"hello");
        req.headers_mut().insert(
            FILE_NAME_HEADER,
            axum::http::HeaderValue::from_bytes("отчёт.txt".as_bytes()).unwrap(),
        );
        let resp = app(st).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["file_name"], "отчёт.txt");
    }

    /// Две одновременные загрузки одного и того же имени не перетирают друг
    /// друга: разные `upload_id`, разные staging-файлы, байты не смешаны.
    #[tokio::test]
    async fn parallel_uploads_of_the_same_file_stay_independent() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let (r1, r2) = tokio::join!(
            app(st.clone()).oneshot(upload_req(Some("same.bin"), Some(4), b"aaaa")),
            app(st.clone()).oneshot(upload_req(Some("same.bin"), Some(4), b"bbbb")),
        );
        let (j1, j2) = (body_json(r1.unwrap()).await, body_json(r2.unwrap()).await);
        let id1 = j1["upload_id"].as_str().unwrap();
        let id2 = j2["upload_id"].as_str().unwrap();
        assert_ne!(id1, id2, "каждая загрузка получает свой id");
        let e1 = st.upload_entry(id1).await.unwrap();
        let e2 = st.upload_entry(id2).await.unwrap();
        assert_eq!(tokio::fs::read(&e1.path).await.unwrap(), b"aaaa");
        assert_eq!(tokio::fs::read(&e2.path).await.unwrap(), b"bbbb");
    }

    /// Ошибка ФС (нет каталога) — это `503 not_connected` («повтори»), а не
    /// 500 и не 400: диск и права — не вина клиента.
    #[tokio::test]
    async fn stream_to_file_reports_fs_errors_as_not_connected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("x.part");
        let err = stream_to_file(Body::from(&b"hi"[..]), &path, 2)
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");
    }

    // ---------------- TTL / GC / потребление ----------------

    /// `upload_id` РАСХОДУЕТСЯ: повторная отправка того же файла невозможна по
    /// построению — второй попытке уже нечего брать.
    #[tokio::test]
    async fn an_upload_id_is_consumed_by_the_first_send() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.bin"), Some(5), b"hello"))
            .await
            .unwrap();
        let id = body_json(resp).await["upload_id"].as_str().unwrap().to_string();

        let taken = st.take_upload(&id).await.expect("первая отправка забирает");
        assert_eq!(taken.file_name, "a.bin");
        assert!(
            st.take_upload(&id).await.is_none(),
            "второй раз брать нечего — отсюда и 409 invalid_state"
        );
        // Файл потреблённой загрузки убирается явно.
        discard_staging(&taken).await;
        assert!(!taken.path.exists());
    }

    #[tokio::test]
    async fn evict_stale_removes_expired_entries_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        tokio::fs::create_dir_all(uploads_dir(&st.config))
            .await
            .unwrap();

        let fresh = staging_path(&st.config, "fresh");
        let old = staging_path(&st.config, "old");
        tokio::fs::write(&fresh, b"x").await.unwrap();
        tokio::fs::write(&old, b"x").await.unwrap();
        st.insert_upload(
            "fresh".into(),
            UploadEntry {
                path: fresh.clone(),
                file_name: "f".into(),
                size: 1,
                mime: DEFAULT_MIME.into(),
                created_at: unix_now(),
            },
        )
        .await;
        st.insert_upload(
            "old".into(),
            UploadEntry {
                path: old.clone(),
                file_name: "o".into(),
                size: 1,
                mime: DEFAULT_MIME.into(),
                created_at: unix_now() - UPLOAD_TTL_SECS - 1,
            },
        )
        .await;

        assert_eq!(evict_stale(&st).await, 1);
        assert!(st.upload_entry("fresh").await.is_some());
        assert!(st.upload_entry("old").await.is_none());
        assert!(fresh.exists(), "живая запись не тронута");
        assert!(!old.exists(), "staging протухшей записи удалён");
    }

    /// Сироты прошлого процесса: реестр живёт в памяти, поэтому после
    /// рестарта `.part`-файлы не принадлежат никому.
    #[tokio::test]
    async fn cleanup_uploads_dir_removes_orphans_of_a_previous_process() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        // Каталога нет — не паника и не ошибка.
        assert_eq!(cleanup_uploads_dir(&st.config).await, 0);

        tokio::fs::create_dir_all(uploads_dir(&st.config))
            .await
            .unwrap();
        for i in 0..3 {
            tokio::fs::write(staging_path(&st.config, &format!("orphan{i}")), b"x")
                .await
                .unwrap();
        }
        assert_eq!(cleanup_uploads_dir(&st.config).await, 3);
        assert_eq!(
            std::fs::read_dir(uploads_dir(&st.config)).unwrap().count(),
            0
        );
    }

    /// Пользователь (или другой процесс) положил каталог внутрь uploads/ —
    /// стартовая подчистка не трогает его и не падает.
    #[tokio::test]
    async fn cleanup_uploads_dir_skips_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        let root = uploads_dir(&st.config);
        tokio::fs::create_dir_all(root.join("nested")).await.unwrap();
        tokio::fs::write(root.join("a.part"), b"x").await.unwrap();
        tokio::fs::write(root.join("nested").join("keep.part"), b"x")
            .await
            .unwrap();

        assert_eq!(cleanup_uploads_dir(&st.config).await, 1);
        assert!(
            root.join("nested").join("keep.part").exists(),
            "вложенный каталог не наш — не трогаем"
        );
    }

    /// Скачок часов назад (NTP) даёт `created_at` «из будущего» — такая запись
    /// НЕ протухла; а протухшая запись с уже удалённым вручную файлом
    /// выселяется без паники.
    #[tokio::test]
    async fn evict_stale_tolerates_future_timestamps_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        tokio::fs::create_dir_all(uploads_dir(&st.config))
            .await
            .unwrap();

        let future = staging_path(&st.config, "future");
        tokio::fs::write(&future, b"x").await.unwrap();
        st.insert_upload(
            "future".into(),
            UploadEntry {
                path: future.clone(),
                file_name: "f".into(),
                size: 1,
                mime: DEFAULT_MIME.into(),
                created_at: unix_now() + 3600,
            },
        )
        .await;
        st.insert_upload(
            "gone".into(),
            UploadEntry {
                path: staging_path(&st.config, "gone"),
                file_name: "g".into(),
                size: 1,
                mime: DEFAULT_MIME.into(),
                created_at: unix_now() - UPLOAD_TTL_SECS - 10,
            },
        )
        .await;

        assert_eq!(evict_stale(&st).await, 1);
        assert!(
            st.upload_entry("future").await.is_some(),
            "метка из будущего — не повод удалять"
        );
        assert!(st.upload_entry("gone").await.is_none());
        assert!(future.exists());
    }

    /// Записи в реестре нет, а файла нет тем более ⇒ `409 invalid_state`
    /// («залей заново»), а не `500`.
    #[tokio::test]
    async fn open_staging_reports_a_missing_file_as_invalid_state() {
        let dir = tempfile::tempdir().unwrap();
        let entry = UploadEntry {
            path: dir.path().join("nope.part"),
            file_name: "a.bin".into(),
            size: 1,
            mime: DEFAULT_MIME.into(),
            created_at: unix_now(),
        };
        let err = open_staging(&entry).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");
        assert!(
            err.body.message.contains("upload the file again"),
            "{}",
            err.body.message
        );
    }

    // ---------------- выбор ветки конвейера ----------------

    /// ТАБЛИЧНЫЙ тест выбора ветки: именно он решает, каким из трёх
    /// конвейеров уедет файл, и ошибка здесь видна только на живой отправке
    /// («гифка перестала двигаться», «голосовое приехало файлом»).
    #[test]
    fn classify_upload_table() {
        use UploadKind::{AudioMessage, Doc, Photo};
        let cases: [(&str, &str, UploadKind); 16] = [
            // Фотографии.
            ("image/jpeg", "DSC0001.JPG", Photo),
            ("image/png", "screenshot.png", Photo),
            ("image/webp", "sticker.webp", Photo),
            ("image/heic", "IMG_0001.heic", Photo),
            // Исключения внутри image/*: анимация и вектор — документы.
            ("image/gif", "cat.gif", Doc),
            ("image/svg+xml", "diagram.svg", Doc),
            // Голосовое: ДВА условия сразу.
            ("audio/ogg", "voice.ogg", AudioMessage),
            ("audio/ogg", "VOICE.OGG", AudioMessage),
            // Музыка в ogg под другим именем голосовым не становится.
            ("audio/ogg", "track.oga", Doc),
            ("audio/mpeg", "track.mp3", Doc),
            ("audio/mp4", "voice.ogg", Doc),
            // Всё прочее — документ.
            ("video/mp4", "clip.mp4", Doc),
            ("application/pdf", "report.pdf", Doc),
            ("application/octet-stream", "noext", Doc),
            ("", "", Doc),
            ("IMAGE/JPEG", "a.jpg", Photo),
        ];
        for (mime, name, expected) in cases {
            assert_eq!(
                classify_upload(mime, name),
                expected,
                "mime={mime:?} name={name:?}"
            );
        }
    }

    /// Тип документа задаётся при ВЫДАЧЕ адреса: перепутав его, получишь файл
    /// `.ogg` вместо кружка с осциллограммой.
    #[test]
    fn doc_type_distinguishes_a_voice_message() {
        assert_eq!(UploadKind::Doc.doc_type(), "doc");
        assert_eq!(UploadKind::AudioMessage.doc_type(), "audio_message");
        assert_eq!(UploadKind::Photo.doc_type(), "doc");
        assert!(UploadKind::Photo.is_photo());
        assert!(!UploadKind::AudioMessage.is_photo());
    }

    // ---------------- attachment-строка ----------------

    /// Форма строки — контракт с VK: `<тип><owner>_<id>[_<access_key>]`.
    #[test]
    fn attachment_string_builds_the_vk_address() {
        let photo = serde_json::json!({"id": 457239018, "owner_id": 105431859});
        assert_eq!(
            attachment_string(UploadKind::Photo, &photo).unwrap(),
            "photo105431859_457239018"
        );

        // access_key приезжает у приватных объектов и обязан доехать.
        let private = serde_json::json!({
            "id": 1, "owner_id": -55, "access_key": "a1b2c3"
        });
        assert_eq!(
            attachment_string(UploadKind::Photo, &private).unwrap(),
            "photo-55_1_a1b2c3"
        );

        // docs.save отдаёт объект с полем по имени типа.
        let doc = serde_json::json!({
            "type": "doc",
            "doc": {"id": 700, "owner_id": 42, "title": "отчёт.pdf"}
        });
        assert_eq!(
            attachment_string(UploadKind::Doc, &doc).unwrap(),
            "doc42_700"
        );

        // У голосового ПРЕФИКС ТОТ ЖЕ (`doc`): `audio_message` — название типа
        // вложения в полученном сообщении, а не префикс адреса.
        let voice = serde_json::json!({
            "type": "audio_message",
            "audio_message": {"id": 9, "owner_id": 42, "access_key": "k"}
        });
        assert_eq!(
            attachment_string(UploadKind::AudioMessage, &voice).unwrap(),
            "doc42_9_k"
        );
    }

    /// Вырожденные ответы VK: строка НЕ строится, потому что сообщение с
    /// мусорным `attachment` VK принял бы как текст без вложения — то есть
    /// пользователь получил бы «отправлено» без файла.
    #[test]
    fn attachment_string_rejects_degenerate_answers() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"id": 0, "owner_id": 42}),
            serde_json::json!({"id": 42, "owner_id": 0}),
            serde_json::json!({"id": "не число", "owner_id": 42}),
            serde_json::json!([]),
            serde_json::json!("photo1_2"),
        ] {
            assert!(
                attachment_string(UploadKind::Photo, &value).is_none(),
                "{value}"
            );
        }
        assert!(attachment_string(UploadKind::Doc, &serde_json::json!({"type": "doc"})).is_none());
    }

    /// VK отдаёт числа то числом, то строкой (зависит от метода и версии) —
    /// строгий разбор ронял бы отправку на ровном месте.
    #[test]
    fn scalars_survive_both_json_shapes() {
        let v = serde_json::json!({"server": "123456", "photo": 42, "hash": "abc"});
        assert_eq!(scalar_i64(&v, "server"), Some(123_456));
        assert_eq!(scalar_string(&v, "photo").as_deref(), Some("42"));
        assert_eq!(scalar_string(&v, "hash").as_deref(), Some("abc"));
        assert_eq!(scalar_i64(&v, "nope"), None);
        assert_eq!(scalar_string(&v, "nope"), None);
        // Строка с идентификаторами тоже разбирается числом.
        let s = serde_json::json!({"id": "700", "owner_id": "-55"});
        assert_eq!(
            attachment_string(UploadKind::Photo, &s).unwrap(),
            "photo-55_700"
        );
    }

    // ---------------- потребление реестра ----------------

    /// `take_uploads` берёт всё или ничего: если один из `upload_id` уже
    /// потрачен, staging остальных подчищается сразу, а не ждёт TTL.
    #[tokio::test]
    async fn take_uploads_is_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        tokio::fs::create_dir_all(uploads_dir(&st.config))
            .await
            .unwrap();

        let mut paths = Vec::new();
        for id in ["a", "b"] {
            let p = staging_path(&st.config, id);
            tokio::fs::write(&p, b"x").await.unwrap();
            paths.push(p.clone());
            st.insert_upload(
                id.into(),
                UploadEntry {
                    path: p,
                    file_name: format!("{id}.bin"),
                    size: 1,
                    mime: DEFAULT_MIME.into(),
                    created_at: unix_now(),
                },
            )
            .await;
        }

        let err = take_uploads(&st, &["a".into(), "missing".into()])
            .await
            .unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");
        assert!(err.body.message.contains("missing"), "{}", err.body.message);
        assert!(!paths[0].exists(), "staging взятой записи подчищен сразу");

        // Уцелевшая запись берётся штатно.
        let taken = take_uploads(&st, &["b".into()]).await.unwrap();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].1.file_name, "b.bin");
    }

    #[tokio::test]
    async fn gc_worker_stops_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        spawn_uploads_gc(&st).await;
        assert_eq!(st.workers.lock().await.len(), 1);
        st.shutdown.cancel();
        st.shutdown_workers().await;
        assert!(st.workers.lock().await.is_empty());
    }
}
