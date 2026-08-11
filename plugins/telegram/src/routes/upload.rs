//! Приём файла от браузера: `POST /api/telegram/upload`.
//!
//! | Метод | Путь | Что делает |
//! |-------|------|------------|
//! | POST | `/api/telegram/upload` | принимает сырые байты файла в staging и выдаёт `upload_id` |
//!
//! # Почему RAW-тело, а не multipart
//!
//! `multipart/form-data` заставил бы тянуть фичу `axum/multipart` и парсить
//! границы частей, а главное — большинство реализаций складывает поле в
//! память целиком. Нам нужен ровно один файл, поэтому тело запроса — это и
//! есть файл: `xhr.send(file)` на фронте, `body.into_data_stream()` здесь.
//! Имя файла приезжает отдельным заголовком
//! [`FILE_NAME_HEADER`] (percent-encoded, потому что заголовки HTTP —
//! ASCII, а имена файлов бывают любые).
//!
//! # Порядок проверок (важен!)
//!
//! 1. **авторизация** — `409 not_authorized` ДО чтения тела. Иначе клиент
//!    зря заливает гигабайт и только потом узнаёт, что аккаунт не привязан;
//! 2. **`Content-Length`** — без него `411 Length Required`: неизвестен
//!    размер ⇒ нельзя ни показать процент, ни проверить лимит заранее;
//! 3. **лимит** `Content-Length > cfg.max_upload_bytes` ⇒ `413`;
//! 4. **имя файла** — заголовка нет либо после percent-decode пусто ⇒ `400`;
//! 5. и только теперь тело стримится на диск.
//!
//! Проверка живого MTProto-соединения здесь СОЗНАТЕЛЬНО не делается:
//! staging — локальная операция, а отправка в Telegram случится в
//! `POST /api/telegram/dialogs/:id/messages`, где `503 not_connected`
//! отдаётся штатно. Так короткий обрыв связи не заставляет заново выбирать
//! файл.
//!
//! # Двухэтапный прогресс
//!
//! Этап LOCAL (браузер → devforge) виден фронту напрямую через
//! `XMLHttpRequest.upload.onprogress`, поэтому сервер по нему НИЧЕГО не
//! вещает. Этап TG (devforge → Telegram) идёт позже, при отправке
//! сообщения, и вещается событиями `upload_progress`
//! ([`crate::progress::UploadReporter`]). Такое разделение — причина, по
//! которой заливка в Telegram НЕ делается внутри этого роута: фронт узнаёт
//! `upload_id` только из ответа, а значит не смог бы сопоставить с карточкой
//! события, пришедшие раньше ответа.
//!
//! # Жизненный цикл staging-файла
//!
//! `<cache_dir>/uploads/<upload_id>.part` живёт до одного из трёх событий:
//! успешная отправка сообщения (файл больше не нужен — байты уже в
//! Telegram), истечение [`crate::state::UPLOAD_TTL_SECS`] (пользователь
//! выбрал файл и передумал), рестарт процесса
//! ([`cleanup_uploads_dir`] на старте — реестр в памяти, а значит после
//! рестарта ни один `.part` уже никому не принадлежит).

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

use crate::progress::UploadReporter;
use crate::routes::dialogs::require_authorized;
use crate::routes::error::ApiError;
use crate::state::{TelegramState, UploadEntry, UPLOAD_TTL_SECS};

/// Подкаталог staging-файлов внутри `cache_dir`.
pub const UPLOADS_SUBDIR: &str = "uploads";

/// Расширение staging-файла (совпадает с привычным «недокачано»).
pub const STAGING_EXT: &str = "part";

/// Заголовок с именем файла (percent-encoded значение `encodeURIComponent`).
pub const FILE_NAME_HEADER: &str = "x-forge-file-name";

/// Максимальная длина имени файла в байтах.
///
/// 255 — предел компонента пути почти во всех ФС; Telegram тоже не любит
/// более длинные имена.
pub const MAX_FILE_NAME_LEN: usize = 255;

/// Имя, которым заменяется пустое/непригодное.
pub const FALLBACK_FILE_NAME: &str = "file";

/// MIME по умолчанию, когда расширение ничего не подсказало.
pub const DEFAULT_MIME: &str = "application/octet-stream";

/// Как часто GC-воркер проверяет реестр загрузок.
pub const UPLOAD_GC_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Ответ `POST /api/telegram/upload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResp {
    /// Идентификатор загрузки — им сообщение ссылается на файл.
    pub upload_id: String,
    /// Санитизированное имя файла (именно оно уйдёт в Telegram).
    pub file_name: String,
    /// Размер в байтах (совпадает с `Content-Length`).
    pub size: u64,
    /// MIME, угаданный по расширению — фронту он нужен для подписи карточки.
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
/// не «лимитирует» (лимит применяют только буферизующие экстракторы —
/// `Bytes`, `String`, `Json`), поэтому фактический предел проверяется по
/// `Content-Length` до чтения и по счётчику записанных байт во время чтения.
pub fn router(max_upload_bytes: u64) -> Router<Arc<TelegramState>> {
    let limit = usize::try_from(max_upload_bytes).unwrap_or(usize::MAX);
    Router::new().route(
        "/api/telegram/upload",
        post(post_upload).layer(DefaultBodyLimit::max(limit)),
    )
}

/// Каталог staging-файлов.
pub fn uploads_dir(cfg: &crate::config::TelegramConfig) -> PathBuf {
    cfg.cache_dir.join(UPLOADS_SUBDIR)
}

/// Путь staging-файла конкретной загрузки.
///
/// Имя целиком определяется `upload_id` (UUID v4), а НЕ именем файла от
/// клиента: так в ФС не попадает ни один байт пользовательского ввода, и
/// path traversal через `X-Forge-File-Name` невозможен по построению.
pub fn staging_path(cfg: &crate::config::TelegramConfig, upload_id: &str) -> PathBuf {
    uploads_dir(cfg).join(format!("{upload_id}.{STAGING_EXT}"))
}

/// Текущее unix-время в секундах.
fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
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
/// Своя реализация вместо крейта `percent-encoding`: нужна одна функция на
/// 20 строк, а любая новая зависимость в этом плагине проходит через
/// gate-проверку дерева (`cargo tree -i openssl-sys`). Невалидные байты и
/// незакрытые `%` не считаются ошибкой — они остаются как есть, потому что
/// имя файла не должно ронять загрузку.
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
///    присылают `C:\dir\file.jpg`, а `../../evil.sh` не должен пролезть даже
///    в имя, которое увидит получатель);
/// 2. управляющие символы (в т.ч. `\r\n`, которым можно было бы попытаться
///    инжектировать заголовок) выбрасываются;
/// 3. обрезка по краям и по [`MAX_FILE_NAME_LEN`] байт с сохранением границы
///    UTF-8 —半 обрезанный символ сделал бы имя нечитаемым;
/// 4. `.`, `..` и пустая строка ⇒ [`FALLBACK_FILE_NAME`].
///
/// Возвращает `None` ТОЛЬКО для входа, который после decode пуст — это
/// сигнал «клиент не передал имя» (`400`), а не «подставь заглушку».
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
/// расширение, — Telegram сам смотрит именно на расширение, когда решает,
/// можно ли принять файл как фото.
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

/// `POST /api/telegram/upload`.
///
/// # Errors
///
/// - `409 not_authorized` — аккаунт не привязан (тело НЕ читается);
/// - `411` — нет `Content-Length`;
/// - `413` — размер больше `cfg.max_upload_bytes`;
/// - `400 invalid_input` — нет имени файла, мусорный `Content-Length`, либо
///   фактический размер тела не совпал с заявленным;
/// - `503 not_connected` — ошибка файловой системы (для клиента это
///   «повтори»: диск/права — не его вина и не его дело).
pub async fn post_upload(
    State(state): State<Arc<TelegramState>>,
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
             (raise max_upload_bytes in telegram.toml to allow it)"
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
        target: "forge_telegram",
        upload_id = %upload_id,
        size = declared,
        mime = %mime,
        "forge-telegram: file staged for sending"
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
    // sync_all, а не только flush: дальше файл читает upload_stream, и он
    // обязан увидеть все байты, а не хвост в страничном кэше.
    writer
        .into_inner()
        .sync_all()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not sync staging file: {e}")))?;
    Ok(written)
}

// ---------------------------------------------------------------------------
// Вторая половина конвейера: staging-файл → Telegram
// ---------------------------------------------------------------------------

/// Отчётчик прогресса, разделённый между колбэком ридера и вызывающим.
///
/// `std::sync::Mutex`, а не `tokio::sync::Mutex`: колбэк дёргается из
/// `poll_read`, то есть из синхронного контекста, где `await` невозможен.
/// Критическая секция — одна арифметическая проверка throttle плюс
/// `broadcast::send`, поэтому блокировки исполнителя не создаёт.
pub type SharedReporter = Arc<std::sync::Mutex<UploadReporter>>;

/// Создаёт отчётчик `upload_progress` для одной загрузки.
pub fn shared_reporter(state: Arc<TelegramState>, upload_id: &str, total: u64) -> SharedReporter {
    Arc::new(std::sync::Mutex::new(UploadReporter::new(
        state, upload_id, total,
    )))
}

/// Колбэк для [`crate::progress::ProgressReader`].
///
/// Отравленный mutex (паника в другом потоке) молча игнорируется: прогрессбар
/// — не причина ронять уже идущую отправку файла.
pub fn progress_hook(reporter: &SharedReporter) -> impl FnMut(u64) + Unpin {
    let rep = reporter.clone();
    move |sent: u64| {
        if let Ok(mut r) = rep.lock() {
            r.report(sent);
        }
    }
}

/// Открывает staging-файл загрузки.
///
/// # Errors
///
/// `409 invalid_state`, если файла нет: запись в реестре есть, а байтов нет —
/// значит, GC уже прибрал протухшую загрузку либо кто-то почистил кэш
/// руками. Клиенту это «залей файл заново», а не «сервер сломался».
pub async fn open_staging(entry: &UploadEntry) -> Result<tokio::fs::File, ApiError> {
    tokio::fs::File::open(&entry.path).await.map_err(|e| {
        ApiError::invalid_state(format!(
            "staged file for this upload_id is no longer available ({e}): upload the file again"
        ))
    })
}

/// Переводит ошибку `upload_stream` в контракт плагина.
///
/// grammers оборачивает ошибки RPC в `io::Error::other`, поэтому FLOOD_WAIT
/// на заливке пришёл бы как безликое `503`. Разворачиваем обратно и отдаём
/// его через тот же [`crate::routes::dialogs::map_invocation`], что и все
/// остальные роуты: `429` + `retry_after_secs`.
pub fn map_upload_io(err: &std::io::Error) -> ApiError {
    if let Some(inv) = err
        .get_ref()
        .and_then(|e| e.downcast_ref::<grammers_mtsender::InvocationError>())
    {
        return crate::routes::dialogs::map_invocation(inv);
    }
    ApiError::not_connected(format!("uploading the file to Telegram failed: {err}"))
}

/// Заливает staging-файл в Telegram и отдаёт хендл для `InputMessage`.
///
/// Прогресс: [`crate::progress::ProgressReader`] считает байты на каждом
/// `poll_read`, [`UploadReporter`] прореживает их до одного события в 100 мс
/// / 256 КиБ и вещает `upload_progress`. Финальное событие отправляется
/// ВСЕГДА и всегда имеет `sent == total` — иначе прогрессбар во фронте
/// навсегда замирал бы на 97 %.
///
/// Возвращённый `Uploaded` живёт на серверах Telegram меньше суток, поэтому
/// использовать его надо сразу (тот же запрос), а не складывать в реестр.
///
/// # Errors
///
/// `409 invalid_state` (staging исчез), `429 flood_wait`, `503 not_connected`,
/// а также любая ошибка, которую [`map_upload_io`] распознает как RPC.
pub async fn upload_to_telegram(
    state: &Arc<TelegramState>,
    client: &grammers_client::Client,
    upload_id: &str,
    entry: &UploadEntry,
) -> Result<grammers_client::media::Uploaded, ApiError> {
    let file = open_staging(entry).await?;
    let reporter = shared_reporter(state.clone(), upload_id, entry.size);
    let mut reader = crate::progress::ProgressReader::new(file, progress_hook(&reporter));

    let size = usize::try_from(entry.size).map_err(|_| {
        ApiError::too_large("file is too large to be uploaded on this platform".to_string())
    })?;
    let res = client
        .upload_stream(&mut reader, size, entry.file_name.clone())
        .await;

    match res {
        Ok(uploaded) => {
            // Финал уходит даже если Telegram проглотил последний чанк
            // «мгновенно»: фронту нужен ровно один кадр с sent == total.
            if let Ok(mut r) = reporter.lock() {
                r.finish(entry.size);
            }
            tracing::info!(
                target: "forge_telegram",
                upload_id = %upload_id,
                size = entry.size,
                "forge-telegram: file uploaded to Telegram"
            );
            Ok(uploaded)
        }
        Err(e) => {
            // Финального события НЕ шлём: карточка обязана остаться в
            // состоянии ошибки, а не «✓ SENT».
            tracing::warn!(
                target: "forge_telegram",
                upload_id = %upload_id,
                error = %e,
                "forge-telegram: upload to Telegram failed"
            );
            Err(map_upload_io(&e))
        }
    }
}

/// Выбрасывает из реестра протухшие записи вместе с их staging-файлами.
///
/// Возвращает число удалённых записей.
pub async fn evict_stale(state: &Arc<TelegramState>) -> usize {
    let stale = state
        .evict_stale_uploads(UPLOAD_TTL_SECS, unix_now())
        .await;
    for entry in &stale {
        let _ = tokio::fs::remove_file(&entry.path).await;
    }
    if !stale.is_empty() {
        tracing::info!(
            target: "forge_telegram",
            removed = stale.len(),
            ttl_secs = UPLOAD_TTL_SECS,
            "forge-telegram: expired uploads discarded"
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
            target: "forge_telegram",
            path = %entry.path.display(),
            error = %e,
            "forge-telegram: could not remove staging file"
        );
    }
}

/// Чистит каталог staging'а на старте процесса.
///
/// Реестр `uploads` живёт только в памяти, поэтому после рестарта каждый
/// `.part` — сирота: сослаться на него уже некому, а размер он занимает
/// вполне реальный. Возвращает число удалённых файлов.
pub async fn cleanup_uploads_dir(cfg: &crate::config::TelegramConfig) -> usize {
    let dir = uploads_dir(cfg);
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let is_file = entry
            .metadata()
            .await
            .map(|m| m.is_file())
            .unwrap_or(false);
        if is_file && tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            target: "forge_telegram",
            removed,
            dir = %dir.display(),
            "forge-telegram: orphaned upload staging files removed on startup"
        );
    }
    removed
}

/// Фоновый воркер: подчистка протухших загрузок раз в [`UPLOAD_GC_INTERVAL`].
///
/// Первый прогон — сразу на старте (он же убирает сирот от прошлого
/// процесса), дальше по таймеру. Останавливается по `shutdown`-токену.
pub async fn uploads_gc_loop(state: Arc<TelegramState>) {
    cleanup_uploads_dir(&state.config).await;
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_telegram", "forge-telegram: uploads GC stopped");
                return;
            }
            _ = tokio::time::sleep(UPLOAD_GC_INTERVAL) => {}
        }
        evict_stale(&state).await;
    }
}

/// Запускает GC-воркер загрузок и регистрирует его для graceful shutdown.
pub async fn spawn_uploads_gc(state: &Arc<TelegramState>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { uploads_gc_loop(st).await });
    state.register_worker(handle).await;
    tracing::info!(
        target: "forge_telegram",
        interval_secs = UPLOAD_GC_INTERVAL.as_secs(),
        ttl_secs = UPLOAD_TTL_SECS,
        "forge-telegram: uploads GC started"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;
    use crate::login::{LoginFlow, TgUserDto};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn state_in(dir: &Path) -> Arc<TelegramState> {
        let cfg = TelegramConfig {
            session_path: dir.join("telegram.session"),
            cache_dir: dir.join("cache"),
            prompts_path: dir.join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        TelegramState::new(cfg)
    }

    async fn authorized(dir: &Path) -> Arc<TelegramState> {
        let st = state_in(dir);
        st.set_login(LoginFlow::Authorized {
            user: TgUserDto {
                id: "1".into(),
                username: None,
                first_name: None,
                phone: None,
            },
        })
        .await;
        st
    }

    fn app(state: Arc<TelegramState>) -> Router {
        let limit = state.config.max_upload_bytes;
        router(limit).with_state(state)
    }

    fn upload_req(name: Option<&str>, len: Option<usize>, body: &'static [u8]) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/telegram/upload")
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

    #[test]
    fn sanitize_upload_name_strips_paths_and_controls() {
        assert_eq!(sanitize_upload_name("report.pdf").unwrap(), "report.pdf");
        // Компоненты пути — оба разделителя.
        assert_eq!(
            sanitize_upload_name("../../etc/passwd").unwrap(),
            "passwd"
        );
        assert_eq!(
            sanitize_upload_name("C:\\Windows\\evil.exe").unwrap(),
            "evil.exe"
        );
        // Заголовочная инъекция и управляющие символы.
        assert_eq!(
            sanitize_upload_name("a\r\nX-Evil: 1.txt").unwrap(),
            "aX-Evil: 1.txt"
        );
        // Только точки — не имя.
        assert_eq!(sanitize_upload_name("..").unwrap(), FALLBACK_FILE_NAME);
        assert_eq!(sanitize_upload_name("./").unwrap(), FALLBACK_FILE_NAME);
        // Пустой вход — это «клиент не передал имя».
        assert!(sanitize_upload_name("").is_none());
        assert!(sanitize_upload_name("   ").is_none());
    }

    #[test]
    fn sanitize_upload_name_caps_length_on_char_boundary() {
        let long = "я".repeat(400); // 800 байт
        let got = sanitize_upload_name(&long).unwrap();
        assert!(got.len() <= MAX_FILE_NAME_LEN, "{}", got.len());
        // Строка осталась валидным UTF-8 (иначе .chars() дал бы иное).
        assert!(got.chars().all(|c| c == 'я'));
    }

    #[test]
    fn guess_mime_uses_extension() {
        assert_eq!(guess_mime("a.jpg"), "image/jpeg");
        assert_eq!(guess_mime("a.png"), "image/png");
        assert_eq!(guess_mime("a.pdf"), "application/pdf");
        assert_eq!(guess_mime("a.mp4"), "video/mp4");
        assert_eq!(guess_mime("noext"), DEFAULT_MIME);
        assert_eq!(guess_mime("a.unknown-ext-xyz"), DEFAULT_MIME);
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
    }

    #[test]
    fn staging_path_never_contains_client_input() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        let p = staging_path(&cfg, "0191b0de-1111-4222-8333-444455556666");
        assert!(p.starts_with(uploads_dir(&cfg)));
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "0191b0de-1111-4222-8333-444455556666.part"
        );
    }

    // ---------------- роут ----------------

    /// Без авторизации — 409 и тело даже не читается.
    #[tokio::test]
    async fn upload_without_auth_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "not_authorized");
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
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "length_required");
    }

    #[tokio::test]
    async fn upload_over_limit_is_413() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        // Урезанный лимит: настоящие 2 ГиБ в тесте не пришлёшь.
        let cfg = TelegramConfig {
            max_upload_bytes: 4,
            ..st.config.clone()
        };
        let st = {
            let s = TelegramState::new(cfg);
            s.set_login(LoginFlow::Authorized {
                user: TgUserDto {
                    id: "1".into(),
                    username: None,
                    first_name: None,
                    phone: None,
                },
            })
            .await;
            s
        };
        let resp = app(st)
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
        let resp = app(st)
            .oneshot(upload_req(Some("%20%20"), Some(5), b"hello"))
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
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/telegram/upload")
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
        // Содержимое не переупорядочено: первый и последний чанк на месте.
        let bytes = tokio::fs::read(&entry.path).await.unwrap();
        assert_eq!(bytes[0], 0);
        assert_eq!(bytes[bytes.len() - 1], ((CHUNKS - 1) % 251) as u8);
    }

    // ---------------- staging → Telegram ----------------

    /// Прогресс отправки: события идут по мере чтения файла, финальное — с
    /// `sent == total`. Проверяется без сети: `upload_stream` читает ридер
    /// точно так же, как здесь `read_to_end`.
    #[tokio::test]
    async fn progress_reporting_covers_the_whole_file() {
        use tokio::io::AsyncReadExt;

        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        tokio::fs::create_dir_all(uploads_dir(&st.config))
            .await
            .unwrap();
        let path = staging_path(&st.config, "u-progress");
        let data = vec![7u8; 700 * 1024]; // > 2 порогов по 256 КиБ
        tokio::fs::write(&path, &data).await.unwrap();
        let entry = UploadEntry {
            path: path.clone(),
            file_name: "a.bin".into(),
            size: data.len() as u64,
            mime: DEFAULT_MIME.into(),
            created_at: unix_now(),
        };

        let mut rx = st.subscribe();
        let file = open_staging(&entry).await.unwrap();
        let reporter = shared_reporter(st.clone(), "u-progress", entry.size);
        let mut reader = crate::progress::ProgressReader::new(file, progress_hook(&reporter));
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        reporter.lock().unwrap().finish(entry.size);

        assert_eq!(out.len(), data.len(), "ридер прозрачен");

        let mut frames = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            frames.push(ev);
        }
        assert!(frames.len() >= 2, "ожидались первое и финальное: {frames:?}");
        match frames.last().unwrap() {
            crate::ws::protocol::TgServerMsg::UploadProgress {
                upload_id,
                sent,
                total,
            } => {
                assert_eq!(upload_id, "u-progress");
                assert_eq!(*sent, entry.size, "финальное событие обязано быть полным");
                assert_eq!(*total, entry.size);
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
        // Ни одно событие не «перепрыгнуло» полный размер.
        for f in &frames {
            if let crate::ws::protocol::TgServerMsg::UploadProgress { sent, total, .. } = f {
                assert!(sent <= total, "{sent} > {total}");
            }
        }
    }

    /// Записи в реестре нет, а файла нет тем более ⇒ `409 invalid_state`
    /// («залей заново»), а не `500`.
    #[tokio::test]
    async fn open_staging_reports_missing_file_as_invalid_state() {
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

    /// FLOOD_WAIT на заливке приезжает завёрнутым в `io::Error` — и обязан
    /// разворачиваться обратно в `429` с `retry_after_secs`.
    #[test]
    fn map_upload_io_unwraps_rpc_errors() {
        use grammers_mtsender::{InvocationError, RpcError};
        use grammers_tl_types as tl;

        let inv = InvocationError::Rpc(RpcError::from(tl::types::RpcError {
            error_code: 420,
            error_message: "FLOOD_WAIT_33".to_string(),
        }));
        let e = map_upload_io(&std::io::Error::other(inv));
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.body.code, "flood_wait");
        assert_eq!(e.body.retry_after_secs, Some(33));

        // Обычная ошибка ввода-вывода остаётся 503.
        let e = map_upload_io(&std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "boom",
        ));
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(e.body.code, "not_connected");
    }

    // ---------------- TTL / GC ----------------

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

    #[tokio::test]
    async fn cleanup_uploads_dir_removes_orphans() {
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

    // ---------------- краевые входы: percent-encoding и имена ----------------

    /// Оборванный escape в самом конце строки не должен ни паниковать
    /// (индексация за границей), ни съедать символы.
    #[test]
    fn percent_decode_survives_truncated_and_invalid_sequences() {
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("file%2"), "file%2");
        // Двойной процент: первый — литерал, со второго начинается валидный escape.
        assert_eq!(percent_decode("%%41"), "%A");
        // Байты вне UTF-8 после декода не паникуют и дают валидную строку.
        let decoded = percent_decode("%FF%FE.bin");
        assert!(decoded.contains('\u{FFFD}'), "{decoded:?}");
        assert!(decoded.ends_with(".bin"), "{decoded:?}");
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

        // 64 emoji по 4 байта = 256 байт: 255 и 254 и 253 — середина символа,
        // корректная обрезка даёт 63 целых emoji (252 байта).
        let emoji = "🎉".repeat(64);
        let got = sanitize_upload_name(&emoji).unwrap();
        assert_eq!(got.chars().count(), 63, "len={}", got.len());
        assert_eq!(got.len(), 252);
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
        // Хвостовой слэш: последний сегмент пути пуст ⇒ тоже заглушка.
        assert_eq!(sanitize_upload_name("dir/").unwrap(), FALLBACK_FILE_NAME);
    }

    /// Камеры и Windows-клиенты присылают расширения в верхнем регистре.
    #[test]
    fn guess_mime_handles_uppercase_and_dotfiles() {
        assert_eq!(guess_mime("DSC0001.JPG"), "image/jpeg");
        // Точка в конце и дотфайл — расширения нет.
        assert_eq!(guess_mime("archive."), DEFAULT_MIME);
        assert_eq!(guess_mime(".gitignore"), DEFAULT_MIME);
    }

    // ---------------- краевые входы: Content-Length ----------------

    /// u64::MAX разбирается, а u64::MAX + 1 — мусор, но не паника/усечение.
    #[test]
    fn parse_content_length_at_u64_boundaries() {
        let mut h = HeaderMap::new();
        h.insert("content-length", u64::MAX.to_string().parse().unwrap());
        assert_eq!(parse_content_length(&h), Some(u64::MAX));
        h.insert("content-length", "18446744073709551616".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
    }

    /// Переполняющий u64 `Content-Length` от сломанного клиента — 400, а не
    /// 500 и не молчаливое усечение до меньшего числа.
    #[tokio::test]
    async fn content_length_overflow_is_bad_input_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/telegram/upload")
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
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            max_upload_bytes: 5,
            ..TelegramConfig::default()
        };
        let st = TelegramState::new(cfg);
        st.set_login(LoginFlow::Authorized {
            user: TgUserDto {
                id: "1".into(),
                username: None,
                first_name: None,
                phone: None,
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

    // ---------------- краевые входы: заголовок имени ----------------

    /// Заголовок есть, но значение пустое — это «имени нет» ⇒ 400.
    #[tokio::test]
    async fn empty_file_name_header_is_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st)
            .oneshot(upload_req(Some(""), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"]["code"], "invalid_input");
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

    // ---------------- конкурентность и ФС ----------------

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
        let (r1, r2) = (r1.unwrap(), r2.unwrap());
        assert_eq!(r1.status(), StatusCode::OK);
        assert_eq!(r2.status(), StatusCode::OK);
        let j1 = body_json(r1).await;
        let j2 = body_json(r2).await;
        let id1 = j1["upload_id"].as_str().unwrap();
        let id2 = j2["upload_id"].as_str().unwrap();
        assert_ne!(id1, id2, "каждая загрузка получает свой id");
        let e1 = st.upload_entry(id1).await.unwrap();
        let e2 = st.upload_entry(id2).await.unwrap();
        assert_eq!(tokio::fs::read(&e1.path).await.unwrap(), b"aaaa");
        assert_eq!(tokio::fs::read(&e2.path).await.unwrap(), b"bbbb");
    }

    /// Скачок часов назад (NTP) даёт `created_at` «из будущего» — такая
    /// запись НЕ протухла; а протухшая запись с уже удалённым вручную файлом
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
        // Протухшая запись, чей staging кто-то уже снёс руками.
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
}
