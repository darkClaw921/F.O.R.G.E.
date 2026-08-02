//! Отдача принятых медиа: `GET /api/telegram/media/:dialog_id/:message_id`.
//!
//! | Параметр | Значения | Смысл |
//! |----------|----------|-------|
//! | `:dialog_id` | `i64` в Bot API-формате | диалог |
//! | `:message_id` | `i32 > 0` | сообщение |
//! | `?kind=` | `thumb` \| `full` (по умолчанию `full`) | превью или оригинал |
//!
//! # Два пути
//!
//! ```text
//!            ┌─ кэш-попадание ─▶ ReaderStream файла + immutable Cache-Control
//! GET /media ┤
//!            └─ промах ─▶ inflight-дедуп ─▶ get_messages_by_id ─▶ iter_download
//!                          │                                        │
//!                          └─ ждём чужую загрузку ─▶ читаем кэш      └─ progress → кэш
//! ```
//!
//! Оба пути отдают файл ОДНОЙ функцией [`serve_cached`]: заголовки,
//! `Content-Length` и стриминг не должны различаться в зависимости от того,
//! кто и когда положил файл в кэш.
//!
//! # Почему `immutable`
//!
//! Содержимое по ключу `(dialog_id, message_id, kind)` неизменно: если
//! автор отредактировал сообщение, updates-воркер вызывает
//! [`crate::media_cache::invalidate`], и следующий запрос получает уже новый
//! файл. Без инвалидации `immutable` был бы багом — браузер держал бы старую
//! картинку год.
//!
//! # Почему нет `Accept-Ranges`
//!
//! Range-запросы в v1 не поддерживаются, поэтому мы их и НЕ анонсируем:
//! `<video>` увидев `Accept-Ranges: bytes`, шлёт `Range` и ждёт `206`;
//! получив вместо этого `200` с полным телом, часть браузеров ломает
//! воспроизведение. Фронт грузит медиа через `fetch` + `blob:` (у `<img>`/
//! `<video>` нет Bearer-токена), поэтому Range ему и не нужен.
//!
//! # Стриминг, а не буфер
//!
//! Файл уходит `tokio_util::io::ReaderStream` + `Body::from_stream`: RSS
//! процесса не зависит от размера медиа. То же и на приёме — чанки
//! `iter_download` пишутся в staging-файл, а не копятся в `Vec`.

use std::future::Future;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use grammers_client::media::{Downloadable, Media, PhotoSize};
use grammers_client::Client;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio_util::io::ReaderStream;

use crate::dto;
use crate::media_cache::{self, CacheKey, CachedMedia, MediaKind, MediaMeta};
use crate::peers;
use crate::progress::DownloadReporter;
use crate::routes::dialogs::{map_invocation, require_authorized, require_client};
use crate::routes::error::ApiError;
use crate::state::TelegramState;

/// Значение `Cache-Control` для успешной отдачи (год + `immutable`).
pub const CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// Сколько ждать чужую загрузку того же медиа, прежде чем взяться самому.
pub const INFLIGHT_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Сколько раз пытаться «дождаться и прочитать кэш» до того, как качать
/// самому. Два — потому что после одного ожидания файл либо есть, либо
/// загрузчик упал, и третий круг ожидания бессмысленен.
pub const MAX_INFLIGHT_ATTEMPTS: usize = 2;

/// Размер буфера чтения при стриминге файла клиенту (64 КиБ).
pub const STREAM_CHUNK: usize = 64 * 1024;

/// Query `?kind=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaQuery {
    /// `thumb` | `full`; отсутствие — это `full`.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Роутер медиа.
pub fn router() -> Router<Arc<TelegramState>> {
    Router::new().route(
        "/api/telegram/media/:dialog_id/:message_id",
        get(get_media),
    )
}

/// `GET /api/telegram/media/:dialog_id/:message_id?kind=thumb|full`.
///
/// Порядок проверок: форма параметров (`400`) → авторизация (`409`) → кэш →
/// подключение (`503`) → Telegram. Именно в таком: мусорный `kind` — ошибка
/// клиента в любом состоянии, а закэшированное медиа обязано отдаваться и
/// без живого MTProto-соединения (реконнект не должен гасить уже принятые
/// картинки).
///
/// # Errors
///
/// - `400 invalid_input` — `dialog_id`/`message_id`/`kind` не разбираются;
/// - `409 not_authorized` — аккаунт не привязан;
/// - `404 no_media` — сообщения нет, у него нет вложения или нет превью;
/// - `429 flood_wait` — Telegram просит подождать (`retry_after_secs`);
/// - `503 not_connected` — нет соединения либо загрузка сорвалась.
pub async fn get_media(
    State(state): State<Arc<TelegramState>>,
    Path((dialog_id, message_id)): Path<(String, String)>,
    Query(q): Query<MediaQuery>,
) -> Result<Response, ApiError> {
    let key = CacheKey::parse(&dialog_id, &message_id, q.kind.as_deref()).ok_or_else(|| {
        ApiError::bad_input(
            "expected /api/telegram/media/<dialog_id>/<message_id>?kind=thumb|full with numeric ids",
        )
    })?;
    require_authorized(&state).await?;

    // Быстрый путь: файл уже на диске — ни соединения, ни Telegram не нужно.
    if let Some(hit) = media_cache::get(&state.config, &key).await {
        return serve_cached(hit, &key, true).await;
    }

    let client = require_client(&state).await?;
    let hit = fetch_cached(&state, &key, || {
        download_media(state.clone(), client.clone(), key)
    })
    .await?;
    serve_cached(hit, &key, false).await
}

/// Отдаёт файл из кэша потоком.
///
/// `touch_mtime` обновляет время последнего использования (LRU): делается
/// только на попадании и только «в фоне» — промах LRU не стоит задержки
/// ответа, поэтому ошибки игнорируются.
///
/// # Errors
///
/// `503 not_connected`, если файл исчез между проверкой и открытием (его мог
/// снести sweeper): клиенту это «повтори запрос», а не «медиа нет».
pub async fn serve_cached(
    hit: CachedMedia,
    key: &CacheKey,
    touch_mtime: bool,
) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(&hit.path).await.map_err(|e| {
        ApiError::not_connected(format!("cached media disappeared before it was served: {e}"))
    })?;
    if touch_mtime {
        let path = hit.path.clone();
        tokio::spawn(async move { media_cache::touch(path).await });
    }

    let stream = ReaderStream::with_capacity(file, STREAM_CHUNK);
    let mime = hit
        .meta
        .mime
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let disposition = content_disposition(hit.meta.file_name.as_deref(), key, hit.meta.mime.as_deref());

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, hit.meta.size)
        .header(header::CACHE_CONTROL, CACHE_CONTROL)
        .body(Body::from_stream(stream))
        .map_err(|e| ApiError::not_connected(format!("could not build media response: {e}")))?;

    // Заголовки со «внешними» данными ставим через HeaderValue::from_str: имя
    // файла приходит из Telegram, и невалидные байты не должны ронять ответ.
    let headers = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&mime) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// Собирает `Content-Disposition: inline`.
///
/// Имя файла приходит из Telegram, то есть от чужого аккаунта. Поэтому:
///
/// 1. берётся только последний сегмент пути (`../../evil.sh` ⇒ `evil.sh`);
/// 2. ASCII-версия чистится до `[A-Za-z0-9._-]` — так в кавычках заведомо
///    не окажется ни кавычки, ни `;`, ни перевода строки (заголовочная
///    инъекция), ни `..`;
/// 3. оригинал добавляется как `filename*=UTF-8''…` в percent-encoding —
///    браузеры предпочтут его, поэтому кириллические имена не теряются.
///
/// Пустое имя заменяется на `telegram_<message_id>.<ext>` (расширение — по
/// MIME), потому что `filename=""` некоторые клиенты понимают как «файл без
/// имени» и сохраняют его как `download`.
pub fn content_disposition(file_name: Option<&str>, key: &CacheKey, mime: Option<&str>) -> String {
    let raw = file_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(last_path_segment)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .map(str::to_string)
        .unwrap_or_else(|| fallback_name(key, mime));

    let ascii = sanitize_ascii(&raw);
    let encoded = percent_encode(&raw);
    format!("inline; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Последний сегмент пути по обоим разделителям (Telegram отдаёт имена как
/// есть, а Windows-клиенты присылают `C:\dir\file.jpg`).
fn last_path_segment(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// Имя, когда Telegram его не прислал (фото и голосовые — всегда).
fn fallback_name(key: &CacheKey, mime: Option<&str>) -> String {
    let ext = mime
        .and_then(mime_guess::get_mime_extensions_str)
        .and_then(|exts| exts.first().copied())
        .unwrap_or("bin");
    format!("telegram_{}_{}.{ext}", key.message_id, key.kind)
}

/// Оставляет только безопасные для заголовка ASCII-символы.
fn sanitize_ascii(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Percent-encoding по RFC 5987 (attr-char): всё, кроме `A-Za-z0-9.!#$&+-^_`~`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '!' | '#' | '$' | '&' | '+' | '-' | '^' | '_' | '`' | '~') {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Снятие записи `media_inflight` — в том числе при панике и раннем `return`.
///
/// Без guard'а любой `?` в середине загрузки оставлял бы ключ в реестре
/// навсегда: все последующие запросы этого медиа ждали бы [`INFLIGHT_WAIT`]
/// и получали `503`. `Drop` не может `await`, поэтому снятие уходит в
/// отдельную задачу; штатный путь зовёт [`InflightGuard::release`]
/// напрямую, и тогда `Drop` уже ничего не делает.
struct InflightGuard {
    /// Состояние плагина (реестр живёт в нём).
    state: Arc<TelegramState>,
    /// Ключ записи.
    key: String,
    /// Запись уже снята штатно.
    released: bool,
}

impl InflightGuard {
    /// Снимает запись и будит ожидающих.
    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let taken = self.state.media_inflight.lock().await.remove(&self.key);
        if let Some(n) = taken {
            n.notify_waiters();
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let state = self.state.clone();
        let key = std::mem::take(&mut self.key);
        // Аварийный путь (паника/отмена запроса): доделываем асинхронно.
        tokio::spawn(async move {
            let taken = state.media_inflight.lock().await.remove(&key);
            if let Some(n) = taken {
                n.notify_waiters();
            }
        });
    }
}

/// Кто мы для этого ключа: загрузчик или ожидающий.
enum Slot {
    /// Мы взяли ключ и обязаны его отпустить.
    Owner(InflightGuard),
    /// Ключ занят — ждём `Notify` владельца.
    Waiter(Arc<Notify>),
}

/// Пытается занять ключ дедупликации.
async fn acquire(state: &Arc<TelegramState>, key: &CacheKey) -> Slot {
    let id = key.id();
    let mut guard = state.media_inflight.lock().await;
    match guard.get(&id) {
        Some(n) => Slot::Waiter(n.clone()),
        None => {
            guard.insert(id.clone(), Arc::new(Notify::new()));
            Slot::Owner(InflightGuard {
                state: state.clone(),
                key: id,
                released: false,
            })
        }
    }
}

/// Возвращает медиа из кэша, скачивая его РОВНО ОДИН раз на несколько
/// параллельных запросов.
///
/// `download` вызывается только владельцем ключа; остальные ждут `Notify` и
/// затем перечитывают кэш. Ожидание ограничено [`INFLIGHT_WAIT`], а число
/// круговых попыток — [`MAX_INFLIGHT_ATTEMPTS`]: если владелец упал, второй
/// запрос обязан взяться сам, а не висеть до таймаута клиента.
///
/// Гонка «владелец закончил, пока мы шли к await» закрыта тем же приёмом,
/// что в `updates::wait_authorized`: `notified()` включается ДО повторной
/// проверки кэша (`Notify::notify_waiters` не оставляет permit'ов).
///
/// # Errors
///
/// Ошибка загрузки владельца (её видит только он) либо `503`, если после
/// всех ожиданий файла в кэше так и нет.
pub async fn fetch_cached<F, Fut>(
    state: &Arc<TelegramState>,
    key: &CacheKey,
    download: F,
) -> Result<CachedMedia, ApiError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<CachedMedia, ApiError>>,
{
    for _ in 0..MAX_INFLIGHT_ATTEMPTS {
        if let Some(hit) = media_cache::get(&state.config, key).await {
            return Ok(hit);
        }
        match acquire(state, key).await {
            Slot::Owner(mut guard) => {
                let result = download().await;
                guard.release().await;
                return result;
            }
            Slot::Waiter(notify) => {
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                // Повторная проверка ПОСЛЕ enable: владелец мог закончить в
                // окне между `acquire` и подпиской.
                if let Some(hit) = media_cache::get(&state.config, key).await {
                    return Ok(hit);
                }
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(INFLIGHT_WAIT) => {
                        tracing::warn!(
                            target: "forge_telegram",
                            key = %key.id(),
                            "forge-telegram: gave up waiting for a concurrent media download"
                        );
                    }
                }
            }
        }
    }
    // Ждали, но файла нет: качаем сами, не претендуя на ключ (владелец мог
    // как раз в этот момент его отпускать).
    download().await
}

/// Удаляет staging-файл, если загрузка не довела дело до `commit`.
///
/// Обрыв соединения, `FLOOD_WAIT` посередине, отменённый клиентом запрос —
/// после любого из них в каталоге кэша не должно остаться `.tmp`-мусора.
struct StagingGuard {
    /// Путь; `None` ⇒ файл уже переехал в кэш.
    path: Option<PathBuf>,
}

impl StagingGuard {
    /// Загрузка удалась — файл больше не наш.
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Превью-кандидат в терминах, не зависящих от типов grammers.
///
/// Нужен, чтобы правило выбора миниатюры было чистой функцией и
/// тестировалось без сети: конструкторы `PhotoSize` приватны, собрать их в
/// тесте нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbCandidate {
    /// Размер в байтах (для встроенных — длина данных).
    pub bytes: usize,
    /// Можно скачать отдельным запросом (`Size`/`Progressive`).
    pub downloadable: bool,
    /// Данные уже пришли внутри сообщения (`Cached`/`Stripped`).
    pub embedded: bool,
}

/// Выбирает превью для `kind=thumb`.
///
/// Правило: САМОЕ МАЛЕНЬКОЕ из скачиваемых — превью в ленте показывается
/// высотой 184 px, и тянуть ради него полноразмерный `y`-вариант значит
/// платить мегабайтами за картинку, которую всё равно сожмут. Если
/// скачиваемых нет (документ без превью-локации), берётся самое КРУПНОЕ из
/// встроенных: они приходят бесплатно вместе с сообщением, и там «крупнее»
/// значит «менее размыто».
///
/// `None` ⇒ превью нет вовсе, роут отвечает `404 no_media`, а фронт рисует
/// generic-иконку.
pub fn pick_thumb_index(candidates: &[ThumbCandidate]) -> Option<usize> {
    let smallest_downloadable = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.downloadable && c.bytes > 0)
        .min_by_key(|(_, c)| c.bytes)
        .map(|(i, _)| i);
    if smallest_downloadable.is_some() {
        return smallest_downloadable;
    }
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.embedded && c.bytes > 0)
        .max_by_key(|(_, c)| c.bytes)
        .map(|(i, _)| i)
}

/// Превью вложения (у фото, документов и стикеров они устроены одинаково).
fn media_thumbs(media: &Media) -> Vec<PhotoSize> {
    match media {
        Media::Photo(p) => p.thumbs(),
        Media::Document(d) => d.thumbs(),
        Media::Sticker(s) => s.document.thumbs(),
        _ => Vec::new(),
    }
}

/// Проекция `PhotoSize` в [`ThumbCandidate`].
fn thumb_candidate(size: &PhotoSize) -> ThumbCandidate {
    ThumbCandidate {
        bytes: size.size(),
        downloadable: size.to_raw_input_location().is_some(),
        embedded: size.to_data().is_some_and(|d| !d.is_empty()),
    }
}

/// Разрешение превью, если оно известно.
fn thumb_dimensions(size: &PhotoSize) -> (Option<i32>, Option<i32>) {
    match size {
        PhotoSize::Size(s) => (Some(s.width), Some(s.height)),
        PhotoSize::Cached(s) => (Some(s.width), Some(s.height)),
        PhotoSize::Progressive(s) => (Some(s.width), Some(s.height)),
        PhotoSize::Empty(_) | PhotoSize::Stripped(_) | PhotoSize::Path(_) => (None, None),
    }
}

/// Скачивает медиа из Telegram и кладёт его в кэш.
///
/// Единственный сетевой путь модуля. Порядок: резолв peer'а → одно
/// `get_messages_by_id` → выбор источника байтов по `kind` → потоковая
/// запись в staging с событиями `download_progress` → `commit` (rename) →
/// перечитывание записи из кэша (чтобы отдавать её тем же кодом, что и
/// попадание).
///
/// # Errors
///
/// - `404 no_media` — сообщение удалено, у него нет вложения, вложение
///   недоступно (истёк `ttl_seconds`) или у документа нет превью;
/// - `429`/`503` — по классификации [`map_invocation`];
/// - `503 not_connected` — ошибка записи на диск.
pub async fn download_media(
    state: Arc<TelegramState>,
    client: Client,
    key: CacheKey,
) -> Result<CachedMedia, ApiError> {
    let dialog_id = key.dialog_id_string();
    let peer = peers::resolve_peer(&state, &dialog_id).await?;
    let msg = client
        .get_messages_by_id(peer, &[key.message_id])
        .await
        .map_err(|e| map_invocation(&e))?
        .into_iter()
        .flatten()
        .next()
        .ok_or_else(|| {
            ApiError::no_media(format!(
                "message {} is not available in dialog {dialog_id} anymore",
                key.message_id
            ))
        })?;
    let media = msg.media().ok_or_else(|| {
        ApiError::no_media(format!("message {} has no attachment", key.message_id))
    })?;
    let info = dto::media_dto(&media).ok_or_else(|| {
        ApiError::no_media(format!(
            "message {} has no downloadable attachment",
            key.message_id
        ))
    })?;

    let staging = media_cache::begin_staging(&state.config, &key)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not prepare the media cache: {e}")))?;
    let mut guard = StagingGuard {
        path: Some(staging.clone()),
    };

    let (received, meta) = match key.kind {
        MediaKind::Thumb => {
            let thumbs = media_thumbs(&media);
            let candidates: Vec<ThumbCandidate> = thumbs.iter().map(thumb_candidate).collect();
            let idx = pick_thumb_index(&candidates).ok_or_else(|| {
                ApiError::no_media(format!(
                    "message {} has no thumbnail (draw a generic icon)",
                    key.message_id
                ))
            })?;
            let chosen = &thumbs[idx];
            let (w, h) = thumb_dimensions(chosen);
            let received = stream_to_file(&state, &key, &client, chosen, &staging, None).await?;
            let mut meta = MediaMeta::now(Some("image/jpeg".to_string()), received, None);
            meta.width = w;
            meta.height = h;
            (received, meta)
        }
        MediaKind::Full => {
            let total = Downloadable::size(&media).map(|v| v as u64);
            let received = stream_to_file(&state, &key, &client, &media, &staging, total).await?;
            let mut meta = MediaMeta::now(info.mime.clone(), received, info.file_name.clone());
            meta.width = info.width;
            meta.height = info.height;
            meta.duration_secs = info.duration_secs;
            (received, meta)
        }
    };

    if received == 0 {
        return Err(ApiError::no_media(format!(
            "Telegram returned an empty file for message {}",
            key.message_id
        )));
    }

    media_cache::commit(&state.config, &key, &staging, &meta)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not store media in the cache: {e}")))?;
    guard.disarm();

    tracing::debug!(
        target: "forge_telegram",
        dialog_id = %dialog_id,
        message_id = key.message_id,
        kind = %key.kind,
        bytes = received,
        "forge-telegram: media downloaded into the cache"
    );

    media_cache::get(&state.config, &key).await.ok_or_else(|| {
        ApiError::not_connected("media was cached but vanished before it could be served")
    })
}

/// `total` для ФИНАЛЬНОГО события прогресса.
///
/// Когда Telegram не сообщил размер заранее (`total: None`), финальное
/// событие обязано подставить фактически полученное: фронт считает процент
/// как `received / total`, и с `total: null` на последнем кадре индикатор
/// навсегда остался бы индетерминированным, хотя файл уже целиком на диске.
pub fn final_total(total: Option<u64>, received: u64) -> Option<u64> {
    total.or(Some(received))
}

/// Пишет `downloadable` в файл чанками, отправляя `download_progress`.
///
/// Возвращает число записанных байт. Финальное событие отправляется всегда
/// (см. [`crate::progress`]), поэтому прогрессбар во фронте гарантированно
/// доходит до конца — даже когда `total` неизвестен.
async fn stream_to_file<D: Downloadable>(
    state: &Arc<TelegramState>,
    key: &CacheKey,
    client: &Client,
    downloadable: &D,
    staging: &FsPath,
    total: Option<u64>,
) -> Result<u64, ApiError> {
    let mut reporter = DownloadReporter::new(
        state.clone(),
        key.dialog_id_string(),
        key.message_id,
        key.kind.as_str(),
    );
    let mut file = tokio::fs::File::create(staging)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not open the staging file: {e}")))?;

    let mut iter = client.iter_download(downloadable);
    let mut received = 0u64;
    reporter.report(0, total);
    loop {
        let chunk = match iter.next().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(map_invocation(&e)),
        };
        file.write_all(&chunk)
            .await
            .map_err(|e| ApiError::not_connected(format!("could not write media chunk: {e}")))?;
        received += chunk.len() as u64;
        reporter.report(received, total);
    }
    file.flush()
        .await
        .and(Ok(()))
        .map_err(|e| ApiError::not_connected(format!("could not flush media file: {e}")))?;
    file.sync_all()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not fsync media file: {e}")))?;
    // Финал — после fsync: событие «готово» не должно опережать данные.
    reporter.finish(received, final_total(total, received));
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;
    use crate::login::{LoginFlow, TgUserDto};
    use crate::ws::protocol::TgServerMsg;
    use axum::http::Request;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn cfg(dir: &FsPath) -> TelegramConfig {
        TelegramConfig {
            session_path: dir.join("telegram.session"),
            cache_dir: dir.join("cache"),
            ..TelegramConfig::default()
        }
    }

    fn state_in(dir: &FsPath) -> Arc<TelegramState> {
        TelegramState::new(cfg(dir))
    }

    async fn authorized(dir: &FsPath) -> Arc<TelegramState> {
        let st = state_in(dir);
        st.set_login(LoginFlow::Authorized {
            user: TgUserDto {
                id: "1".into(),
                username: None,
                first_name: Some("me".into()),
                phone: None,
            },
        })
        .await;
        st
    }

    fn key() -> CacheKey {
        CacheKey {
            dialog_id: 777000,
            message_id: 7,
            kind: MediaKind::Full,
        }
    }

    async fn seed(state: &Arc<TelegramState>, key: &CacheKey, bytes: &[u8], mime: &str) {
        let mut meta = MediaMeta::now(
            Some(mime.to_string()),
            bytes.len() as u64,
            Some("holiday photo.jpg".into()),
        );
        meta.width = Some(1280);
        meta.height = Some(720);
        media_cache::put_atomic(&state.config, key, bytes, &meta)
            .await
            .unwrap();
    }

    fn app(state: Arc<TelegramState>) -> Router {
        router().with_state(state)
    }

    async fn body_bytes(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }

    // ---------------- выбор превью ----------------

    #[test]
    fn thumb_selection_prefers_the_smallest_downloadable() {
        let cands = [
            ThumbCandidate { bytes: 500, downloadable: false, embedded: true },
            ThumbCandidate { bytes: 90_000, downloadable: true, embedded: false },
            ThumbCandidate { bytes: 12_000, downloadable: true, embedded: false },
        ];
        assert_eq!(pick_thumb_index(&cands), Some(2));
    }

    #[test]
    fn thumb_selection_falls_back_to_the_largest_embedded() {
        let cands = [
            ThumbCandidate { bytes: 200, downloadable: false, embedded: true },
            ThumbCandidate { bytes: 900, downloadable: false, embedded: true },
            ThumbCandidate { bytes: 0, downloadable: true, embedded: false },
        ];
        assert_eq!(pick_thumb_index(&cands), Some(1));
    }

    #[test]
    fn thumb_selection_reports_absence() {
        assert_eq!(pick_thumb_index(&[]), None);
        let useless = [ThumbCandidate { bytes: 0, downloadable: false, embedded: false }];
        assert_eq!(pick_thumb_index(&useless), None);
    }

    // ---------------- Content-Disposition ----------------

    #[test]
    fn disposition_sanitizes_hostile_names() {
        let k = key();
        let d = content_disposition(Some("../../etc/pa\"ss;wd\r\n"), &k, Some("image/jpeg"));
        assert!(d.starts_with("inline; filename=\""), "{d}");
        // Ни кавычек, ни ; ни CRLF внутри ASCII-имени.
        let ascii = d
            .split_once("filename=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(v, _)| v.to_string())
            .unwrap();
        assert!(!ascii.contains('"') && !ascii.contains(';'), "{ascii}");
        assert!(!ascii.contains('\r') && !ascii.contains('\n'), "{ascii}");
        assert!(!ascii.contains('/') && !ascii.contains(".."), "{ascii}");
        assert!(ascii.contains("pa_ss_wd"), "{ascii}");
    }

    #[test]
    fn disposition_keeps_unicode_in_filename_star() {
        let k = key();
        let d = content_disposition(Some("отчёт.pdf"), &k, Some("application/pdf"));
        assert!(d.contains("filename*=UTF-8''"), "{d}");
        assert!(d.contains("%D0%BE%D1%82%D1%87"), "{d}");
        // ASCII-версия остаётся валидной и непустой.
        assert!(d.contains("filename=\""), "{d}");
    }

    #[test]
    fn disposition_generates_a_name_when_telegram_gave_none() {
        let k = CacheKey { kind: MediaKind::Thumb, ..key() };
        let d = content_disposition(None, &k, Some("image/jpeg"));
        assert!(d.contains("telegram_7_thumb."), "{d}");
        // Неизвестный MIME ⇒ .bin, а не пустое расширение.
        let d = content_disposition(None, &k, None);
        assert!(d.contains("telegram_7_thumb.bin"), "{d}");
        // Имя из одних точек не должно давать пустое filename="".
        let d = content_disposition(Some("..."), &k, None);
        assert!(!d.contains("filename=\"\""), "{d}");
    }

    // ---------------- путь кэш-попадания ----------------

    #[tokio::test]
    async fn cache_hit_is_served_with_headers_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let payload: Vec<u8> = (0..5000u32).map(|i| (i % 253) as u8).collect();
        seed(&st, &key(), &payload, "image/jpeg").await;

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7?kind=full")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let headers = resp.headers().clone();
        assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(
            headers[header::CONTENT_LENGTH],
            payload.len().to_string().as_str()
        );
        assert_eq!(headers[header::CACHE_CONTROL], CACHE_CONTROL);
        assert!(
            !headers.contains_key(header::ACCEPT_RANGES),
            "Accept-Ranges не анонсируем, пока нет 206"
        );
        let cd = headers[header::CONTENT_DISPOSITION].to_str().unwrap();
        assert!(cd.starts_with("inline; "), "{cd}");
        assert!(cd.contains("holiday_photo.jpg"), "{cd}");
        assert_eq!(body_bytes(resp).await, payload);
    }

    /// Большой файл уходит ЧАНКАМИ, а не одним буфером: первый кадр тела
    /// не превышает [`STREAM_CHUNK`]. Это машинная проверка того, что RSS
    /// процесса не растёт на размер медиа (буферизуй мы файл целиком —
    /// первый же кадр был бы на все 4 МиБ).
    #[tokio::test]
    async fn large_file_is_streamed_in_chunks() {
        use futures_util::StreamExt as _;

        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let payload = vec![0xA5u8; 4 * 1024 * 1024];
        seed(&st, &key(), &payload, "video/mp4").await;

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()[header::CONTENT_LENGTH],
            payload.len().to_string().as_str()
        );

        let mut stream = resp.into_body().into_data_stream();
        let first = stream.next().await.unwrap().unwrap();
        assert!(
            first.len() <= STREAM_CHUNK,
            "первый кадр {} байт — файл буферизуется целиком",
            first.len()
        );
        let mut total = first.len();
        while let Some(chunk) = stream.next().await {
            total += chunk.unwrap().len();
        }
        assert_eq!(total, payload.len(), "тело доехало не полностью");
    }

    /// Попадание отдаётся БЕЗ активного MTProto-подключения: реконнект не
    /// должен гасить уже принятые картинки.
    #[tokio::test]
    async fn cache_hit_needs_no_connection() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        assert!(!st.has_runtime().await, "рантайма в тесте нет");
        seed(&st, &key(), b"cached", "image/jpeg").await;

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_bytes(resp).await, b"cached".to_vec());
    }

    /// `kind` по умолчанию — `full`, а `thumb` — отдельная запись кэша.
    #[tokio::test]
    async fn kinds_are_independent_cache_entries() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        seed(
            &st,
            &CacheKey { kind: MediaKind::Thumb, ..key() },
            b"tiny",
            "image/jpeg",
        )
        .await;

        // thumb есть — отдаётся.
        let resp = app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7?kind=thumb")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_bytes(resp).await, b"tiny".to_vec());

        // full нет и соединения нет ⇒ 503, а не выдача превью под видом файла.
        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Инвалидация (её делает updates-воркер на правке/удалении) снимает
    /// именно то, что раньше отдавалось как `immutable`: старые байты больше
    /// не выдаются НИ ПРИ КАКИХ условиях — роут уходит на промах.
    #[tokio::test]
    async fn invalidated_media_is_no_longer_served() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        seed(&st, &k, b"old-content", "image/jpeg").await;

        let resp = app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_bytes(resp).await, b"old-content".to_vec());

        let removed = media_cache::invalidate(&st.config, "777000", 7).await;
        assert_eq!(removed, 2, "bin + meta");

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Соединения в тесте нет, поэтому промах ⇒ 503, а НЕ старое тело.
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Пустая мета ⇒ `application/octet-stream`, а не отсутствующий тип.
    #[tokio::test]
    async fn missing_mime_falls_back_to_octet_stream() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        media_cache::put_atomic(&st.config, &k, b"blob", &MediaMeta::now(None, 4, None))
            .await
            .unwrap();

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/octet-stream");
    }

    /// Кэш-попадание обновляет mtime (LRU видит «использовали только что»).
    #[tokio::test]
    async fn cache_hit_touches_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        seed(&st, &k, b"payload", "image/jpeg").await;
        let (bin, _) = media_cache::path_for(&st.config, &k);
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = body_bytes(resp).await;
        // touch уходит в отдельную задачу — даём ей шанс выполниться.
        for _ in 0..50 {
            if std::fs::metadata(&bin).unwrap().modified().unwrap() > old {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("mtime не обновился после кэш-попадания");
    }

    // ---------------- валидация и авторизация ----------------

    #[tokio::test]
    async fn broken_parameters_are_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        for uri in [
            "/api/telegram/media/abc/7",
            "/api/telegram/media/..%2F..%2Fetc/7",
            "/api/telegram/media/777000/0",
            "/api/telegram/media/777000/-3",
            "/api/telegram/media/777000/x",
            "/api/telegram/media/777000/7?kind=thmub",
            "/api/telegram/media/777000/7?kind=../../etc/passwd",
        ] {
            let resp = app(st.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body = body_bytes(resp).await;
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "invalid_input", "{uri}");
        }
    }

    /// Неавторизованный запрос — `409`, причём ДО любого обращения к диску.
    #[tokio::test]
    async fn unauthorized_request_is_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        seed(&st, &key(), b"secret", "image/jpeg").await;

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
        assert_eq!(json["error"]["code"], "not_authorized");
    }

    /// Промах кэша без подключения — `503 not_connected` (и никаких 500).
    #[tokio::test]
    async fn cache_miss_without_connection_is_service_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/99")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
        assert_eq!(json["error"]["code"], "not_connected");
    }

    // ---------------- inflight-дедуп ----------------

    /// Пять одновременных запросов одного медиа дают РОВНО одну загрузку.
    #[tokio::test]
    async fn concurrent_requests_download_once() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        let calls = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..5 {
            let st = st.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                fetch_cached(&st, &k, || {
                    let st = st.clone();
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Медленная «сеть», чтобы остальные успели встать в ожидание.
                        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                        media_cache::put_atomic(
                            &st.config,
                            &k,
                            b"downloaded-once",
                            &MediaMeta::now(Some("image/jpeg".into()), 15, None),
                        )
                        .await
                        .unwrap();
                        media_cache::get(&st.config, &k).await.ok_or_else(|| {
                            ApiError::not_connected("fake download lost the file")
                        })
                    }
                })
                .await
            }));
        }

        for t in tasks {
            let hit = t.await.unwrap().expect("все запросы обязаны получить ответ");
            assert_eq!(hit.meta.size, 15);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "загрузка должна была случиться ровно один раз"
        );
        assert!(
            st.media_inflight.lock().await.is_empty(),
            "реестр inflight обязан опустеть"
        );
    }

    /// Ошибка загрузчика не блокирует последующие попытки: запись inflight
    /// снимается, и следующий запрос качает сам.
    #[tokio::test]
    async fn failed_download_releases_the_inflight_slot() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();

        let err = fetch_cached(&st, &k, || async {
            Err::<CachedMedia, ApiError>(ApiError::not_connected("boom"))
        })
        .await
        .unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(st.media_inflight.lock().await.is_empty());

        // Второй заход работает штатно.
        let hit = fetch_cached(&st, &k, || {
            let st = st.clone();
            async move {
                media_cache::put_atomic(
                    &st.config,
                    &k,
                    b"second",
                    &MediaMeta::now(Some("image/jpeg".into()), 6, None),
                )
                .await
                .unwrap();
                media_cache::get(&st.config, &k)
                    .await
                    .ok_or_else(|| ApiError::not_connected("lost"))
            }
        })
        .await
        .unwrap();
        assert_eq!(hit.meta.size, 6);
    }

    /// Паника загрузчика тоже не оставляет ключ в реестре (Drop-guard).
    #[tokio::test]
    async fn panicking_download_releases_the_inflight_slot() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();

        let st2 = st.clone();
        let joined = tokio::spawn(async move {
            fetch_cached(&st2, &k, || async { panic!("network exploded") }).await
        })
        .await;
        assert!(joined.is_err(), "паника должна была дойти до JoinError");

        for _ in 0..50 {
            if st.media_inflight.lock().await.is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("запись inflight осталась после паники");
    }

    // ---------------- прогресс загрузки ----------------

    /// Прогресс идёт «разумной частотой», а ПОСЛЕДНЕЕ событие обязано иметь
    /// `received == total`. Цикл повторяет [`stream_to_file`] один в один
    /// (`report` на каждый чанк + `finish` в конце), только вместо сети —
    /// заранее известная нарезка: сеть в unit-тесте недоступна, а контракт
    /// событий проверяем именно тот, что уходит в WS.
    #[tokio::test]
    async fn download_progress_is_throttled_and_ends_complete() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let mut rx = st.subscribe();

        let total_bytes: u64 = 8 * 1024 * 1024;
        let chunk = 4 * 1024u64; // мелкие чанки ⇒ 2048 вызовов report
        let mut reporter =
            DownloadReporter::new(st.clone(), "777000", 7, MediaKind::Full.as_str());
        let mut received = 0;
        reporter.report(0, Some(total_bytes));
        while received < total_bytes {
            received += chunk;
            reporter.report(received, Some(total_bytes));
        }
        reporter.finish(received, final_total(Some(total_bytes), received));

        let mut frames = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            frames.push(ev);
        }
        assert!(
            frames.len() < 64,
            "2048 чанков не должны давать {} кадров",
            frames.len()
        );
        assert!(frames.len() >= 2, "нужны как минимум старт и финал");
        match frames.last().unwrap() {
            TgServerMsg::DownloadProgress {
                received, total, kind, ..
            } => {
                assert_eq!(*received, total_bytes);
                assert_eq!(*total, Some(total_bytes), "финал обязан быть полным");
                assert_eq!(kind, "full");
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    /// Неизвестный заранее размер: финал подставляет фактический, иначе
    /// прогрессбар навсегда остался бы индетерминированным.
    #[tokio::test]
    async fn unknown_total_is_resolved_on_the_final_event() {
        assert_eq!(final_total(None, 4096), Some(4096));
        assert_eq!(final_total(Some(10), 4096), Some(10));

        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let mut rx = st.subscribe();
        let mut reporter =
            DownloadReporter::new(st.clone(), "777000", 7, MediaKind::Thumb.as_str());
        reporter.report(0, None);
        reporter.finish(1234, final_total(None, 1234));

        let mut last = None;
        while let Ok(ev) = rx.try_recv() {
            last = Some(ev);
        }
        match last.unwrap() {
            TgServerMsg::DownloadProgress { received, total, .. } => {
                assert_eq!(received, 1234);
                assert_eq!(total, Some(1234));
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    // ---------------- обрыв и мусор ----------------

    /// Guard сносит staging-файл при любом выходе из загрузки.
    #[tokio::test]
    async fn staging_guard_removes_the_file_unless_disarmed() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();

        let staging = media_cache::begin_staging(&st.config, &k).await.unwrap();
        tokio::fs::write(&staging, b"partial").await.unwrap();
        {
            let _guard = StagingGuard {
                path: Some(staging.clone()),
            };
        }
        assert!(!staging.exists(), "брошенный staging должен исчезнуть");

        // disarm ⇒ файл остаётся (он уже переехал в кэш).
        let staging2 = media_cache::begin_staging(&st.config, &k).await.unwrap();
        tokio::fs::write(&staging2, b"committed").await.unwrap();
        {
            let mut guard = StagingGuard {
                path: Some(staging2.clone()),
            };
            guard.disarm();
        }
        assert!(staging2.exists());
    }

    /// Запрос, отменённый клиентом на середине загрузки: ни `.tmp`-мусора,
    /// ни залипшей записи `media_inflight`.
    #[tokio::test]
    async fn aborted_request_leaves_no_staging_and_no_inflight() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        let seen = Arc::new(tokio::sync::Mutex::new(None::<PathBuf>));

        let st2 = st.clone();
        let seen2 = seen.clone();
        let task = tokio::spawn(async move {
            fetch_cached(&st2, &k, || {
                let st = st2.clone();
                let seen = seen2.clone();
                async move {
                    let staging = media_cache::begin_staging(&st.config, &k).await.unwrap();
                    tokio::fs::write(&staging, b"half a file").await.unwrap();
                    let _guard = StagingGuard {
                        path: Some(staging.clone()),
                    };
                    *seen.lock().await = Some(staging);
                    // «Долгая сеть», которую оборвёт abort.
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Err(ApiError::not_connected("never reached"))
                }
            })
            .await
        });

        // Ждём, пока загрузчик создаст staging, и рвём запрос.
        let staging = loop {
            if let Some(p) = seen.lock().await.clone() {
                break p;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert!(staging.exists());
        task.abort();
        let _ = task.await;

        for _ in 0..100 {
            let inflight_empty = st.media_inflight.lock().await.is_empty();
            if !staging.exists() && inflight_empty {
                // Кэш при этом пуст: недокачанное медиа не должно выглядеть готовым.
                assert!(media_cache::get(&st.config, &k).await.is_none());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "после обрыва остались: staging={} inflight={}",
            staging.exists(),
            st.media_inflight.lock().await.len()
        );
    }

    /// Уже закэшированное медиа не вызывает загрузку вовсе.
    #[tokio::test]
    async fn cached_media_never_calls_the_downloader() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        seed(&st, &k, b"on-disk", "image/jpeg").await;
        let calls = Arc::new(AtomicUsize::new(0));

        let hit = fetch_cached(&st, &k, || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(ApiError::not_connected("should never be called"))
            }
        })
        .await
        .unwrap();
        assert_eq!(hit.meta.size, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    // ---------------- краевые параметры маршрута ----------------

    /// Каналы и супергруппы в Bot API-формате имеют ОТРИЦАТЕЛЬНЫЙ id
    /// (`-100…`): знак минус обязан разбираться, иначе медиа каналов
    /// недоступно вовсе.
    #[tokio::test]
    async fn negative_supergroup_dialog_ids_are_served() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = CacheKey {
            dialog_id: -1001234567890,
            message_id: 42,
            kind: MediaKind::Full,
        };
        seed(&st, &k, b"channel-media", "image/jpeg").await;

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/-1001234567890/42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_bytes(resp).await, b"channel-media".to_vec());
    }

    /// `message_id` — 32-битный: i32::MAX ещё валиден, i32::MAX + 1 — уже
    /// мусор (400), а не паника и не молчаливое усечение.
    #[tokio::test]
    async fn message_id_at_i32_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = CacheKey { message_id: i32::MAX, ..key() };
        seed(&st, &k, b"edge", "image/jpeg").await;

        let resp = app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/2147483647")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_bytes(resp).await, b"edge".to_vec());

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/2147483648")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
        assert_eq!(json["error"]["code"], "invalid_input");
    }

    /// Контракт из шапки модуля: мусорный `kind` — ошибка клиента В ЛЮБОМ
    /// состоянии, то есть 400 идёт РАНЬШЕ проверки авторизации (не 409).
    #[tokio::test]
    async fn parameter_validation_precedes_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path()); // НЕ авторизован
        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7?kind=thmub")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
        assert_eq!(json["error"]["code"], "invalid_input");
    }

    /// `?kind=` (пустое значение) и `?kind=%20full%20` (пробелы вокруг) —
    /// это `full`: неряшливый клиент не должен получать 400 за пустоту.
    #[tokio::test]
    async fn empty_and_padded_kind_mean_full() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        seed(&st, &key(), b"full-bytes", "image/jpeg").await;

        for uri in [
            "/api/telegram/media/777000/7?kind=",
            "/api/telegram/media/777000/7?kind=%20full%20",
        ] {
            let resp = app(st.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
            assert_eq!(body_bytes(resp).await, b"full-bytes".to_vec(), "{uri}");
        }
    }

    /// Whitelist `kind` регистрозависим (`FULL` — опечатка, а не оригинал),
    /// а дублированный query-параметр — ошибка клиента, но не 500.
    #[tokio::test]
    async fn kind_whitelist_is_case_sensitive_and_duplicates_are_client_errors() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        seed(&st, &key(), b"x", "image/jpeg").await;

        let resp = app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7?kind=FULL")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
        assert_eq!(json["error"]["code"], "invalid_input");

        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7?kind=full&kind=thumb")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "дубль параметра: {}",
            resp.status()
        );
    }

    // ---------------- враждебные метаданные из Telegram ----------------

    /// MIME приходит от чужого аккаунта: CRLF в нём не должен инжектировать
    /// заголовки, а пробельный MIME — падать в octet-stream.
    #[tokio::test]
    async fn hostile_mime_from_telegram_cannot_inject_headers() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        media_cache::put_atomic(
            &st.config,
            &k,
            b"payload",
            &MediaMeta::now(Some("image/jpeg\r\nx-evil: pwned".into()), 7, None),
        )
        .await
        .unwrap();

        let resp = app(st.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            !resp.headers().contains_key("x-evil"),
            "CRLF в MIME инжектировал заголовок"
        );
        if let Some(ct) = resp.headers().get(header::CONTENT_TYPE) {
            assert!(!ct.as_bytes().windows(4).any(|w| w == b"evil"), "{ct:?}");
        }
        assert_eq!(body_bytes(resp).await, b"payload".to_vec());

        // Пробельный MIME — как отсутствующий.
        let k8 = CacheKey { message_id: 8, ..key() };
        media_cache::put_atomic(&st.config, &k8, b"blob", &MediaMeta::now(Some("   ".into()), 4, None))
            .await
            .unwrap();
        let resp = app(st)
            .oneshot(
                Request::builder()
                    .uri("/api/telegram/media/777000/8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
    }

    /// RFC 5987: `%`, `*` и `'` НЕ входят в attr-char и обязаны кодироваться
    /// в `filename*` — иначе заголовок неоднозначен для браузера.
    #[test]
    fn rfc5987_reserved_chars_are_encoded_in_filename_star() {
        let k = key();
        let d = content_disposition(Some("100%*'.pdf"), &k, Some("application/pdf"));
        let star = d.split("filename*=UTF-8''").nth(1).unwrap();
        assert!(star.contains("%25"), "{d}"); // %
        assert!(star.contains("%2A"), "{d}"); // *
        assert!(star.contains("%27"), "{d}"); // '
        // Пробел кодируется, а не остаётся разрывать заголовок.
        let d = content_disposition(Some("my file.pdf"), &k, None);
        assert!(d.contains("my%20file.pdf"), "{d}");
    }

    // ---------------- конкурентное восстановление и гонка с sweeper ----------------

    /// Владелец ключа упал — ожидающий обязан скачать САМ (второй круг
    /// [`MAX_INFLIGHT_ATTEMPTS`]), а не висеть и не получать чужую ошибку.
    #[tokio::test]
    async fn waiter_downloads_itself_after_owner_failure() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();

        // Владелец: держит ключ 150 мс и падает.
        let st_owner = st.clone();
        let owner = tokio::spawn(async move {
            fetch_cached(&st_owner, &k, || async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                Err::<CachedMedia, ApiError>(ApiError::not_connected("owner network died"))
            })
            .await
        });

        // Дожидаемся, пока владелец реально займёт ключ.
        for _ in 0..200 {
            if !st.media_inflight.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            !st.media_inflight.lock().await.is_empty(),
            "владелец не занял ключ"
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let hit = fetch_cached(&st, &k, || {
            let st = st.clone();
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                media_cache::put_atomic(
                    &st.config,
                    &k,
                    b"waiter-won",
                    &MediaMeta::now(Some("image/jpeg".into()), 10, None),
                )
                .await
                .unwrap();
                media_cache::get(&st.config, &k)
                    .await
                    .ok_or_else(|| ApiError::not_connected("lost"))
            }
        })
        .await
        .expect("ожидающий обязан получить файл после падения владельца");

        assert_eq!(hit.meta.size, 10);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "ожидающий качает ровно один раз"
        );
        assert!(owner.await.unwrap().is_err(), "владелец обязан был упасть");
        assert!(st.media_inflight.lock().await.is_empty());
    }

    /// Sweeper удалил файл между проверкой кэша и открытием: клиент получает
    /// `503 not_connected` («повтори»), а не 500 и не пустое тело.
    #[tokio::test]
    async fn vanished_cache_file_is_retryable_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let k = key();
        seed(&st, &k, b"bytes", "image/jpeg").await;
        let hit = media_cache::get(&st.config, &k).await.unwrap();
        tokio::fs::remove_file(&hit.path).await.unwrap();

        let err = serve_cached(hit, &k, false).await.unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");
        assert!(
            err.body.message.contains("disappeared"),
            "{}",
            err.body.message
        );
    }
}
