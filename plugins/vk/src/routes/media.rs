//! Отдача принятых вложений: `GET /api/vk/media/:peer_id/:message_id/:index`.
//!
//! | Параметр | Значения | Смысл |
//! |----------|----------|-------|
//! | `:peer_id` | `i64 != 0` | беседа |
//! | `:message_id` | `i32 > 0` | сообщение (ГЛОБАЛЬНЫЙ id, не `cmid`) |
//! | `:index` | `0..=9` | порядковый номер вложения В СООБЩЕНИИ |
//! | `?kind=` | `thumb` \| `full` (по умолчанию `full`) | превью или оригинал |
//!
//! # Два пути
//!
//! ```text
//!            ┌─ кэш-попадание ─▶ ReaderStream файла + immutable Cache-Control
//! GET /media ┤
//!            └─ промах ─▶ дедуп по КЛЮЧУ ─▶ дедуп по СООБЩЕНИЮ ─▶ getById
//!                          │                                        │
//!                          └─ ждём чужую загрузку ─▶ читаем кэш      └─ CDN → кэш
//! ```
//!
//! Оба пути отдают файл ОДНОЙ функцией [`serve_cached`]: заголовки,
//! `Content-Length` и стриминг не должны различаться в зависимости от того,
//! кто и когда положил файл в кэш.
//!
//! # ИНВАРИАНТ: URL ВЛОЖЕНИЙ VK НЕ КЭШИРУЮТСЯ НИКОГДА
//!
//! Кэшируются только байты. Ссылки VK подписанные и протухающие, поэтому
//! адрес добывается непосредственно перед скачиванием
//! ([`crate::attach::url_for`] по свежему `messages.getById`) — просроченной
//! ссылки в системе просто не бывает, её негде хранить. Если CDN всё же
//! ответил `403`/`404` между резолвом и скачиванием, делается РОВНО ОДИН
//! повтор с заново добытым адресом, после чего — честный `404 no_media`.
//!
//! # ДЕДУП ДВУХ УРОВНЕЙ
//!
//! Добавление против Telegram, продиктованное лимитом 3 rps:
//!
//! * уровень **ключа** ([`crate::state::VkState::media_inflight`]) — копия
//!   телеграмного: два запроса одного и того же файла стоят одного скачивания;
//! * уровень **сообщения** ([`crate::state::VkState::message_inflight`] +
//!   короткоживущий [`crate::state::VkState::message_cache`]) — карточек у
//!   галереи из десяти фото десять, и каждая просит свой индекс. Без этого
//!   уровня галерея выстрелила бы десятью ОДИНАКОВЫМИ `getById`, то есть 3+
//!   секундами троттлинга на одну картинку. С ним — один запрос на всю
//!   галерею (тест `a_gallery_costs_exactly_one_get_by_id`).
//!
//! # Почему `immutable`
//!
//! Содержимое по ключу `(peer_id, message_id, index, kind)` неизменно: если
//! автор отредактировал сообщение, LongPoll-воркер вызывает
//! [`crate::media_cache::invalidate`], и следующий запрос получает уже новый
//! файл. Без инвалидации `immutable` был бы багом — браузер держал бы старую
//! картинку год.
//!
//! # Почему нет `Accept-Ranges`
//!
//! Range-запросы не поддерживаются, поэтому мы их и НЕ анонсируем: `<video>`,
//! увидев `Accept-Ranges: bytes`, шлёт `Range` и ждёт `206`; получив вместо
//! этого `200` с полным телом, часть браузеров ломает воспроизведение. Фронт
//! грузит медиа через `fetch` + `blob:` (у `<img>`/`<audio>` нет Bearer-токена),
//! поэтому Range ему и не нужен.

use std::future::Future;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio_util::io::ReaderStream;

use crate::api::schema::AttachmentRaw;
use crate::api::{VkClient, VkError};
use crate::attach;
use crate::dto;
use crate::media_cache::{self, CacheKey, CachedMedia, MediaKind, MediaMeta};
use crate::progress::DownloadReporter;
use crate::routes::dialogs::{map_vk_error, require_authorized, require_client, require_connected};
use crate::routes::error::ApiError;
use crate::state::VkState;

/// Значение `Cache-Control` для успешной отдачи (год + `immutable`).
pub const CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// Сколько ждать чужую загрузку того же медиа, прежде чем взяться самому.
pub const INFLIGHT_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Сколько ждать чужой `getById` того же сообщения.
///
/// Короче [`INFLIGHT_WAIT`] на порядок: резолв — это ОДИН вызов API с
/// собственным таймаутом, а не скачивание стомегабайтного файла.
pub const MESSAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Сколько раз пытаться «дождаться и прочитать кэш» до того, как качать
/// самому. Два — потому что после одного ожидания файл либо есть, либо
/// загрузчик упал, и третий круг ожидания бессмысленен.
pub const MAX_INFLIGHT_ATTEMPTS: usize = 2;

/// Размер буфера чтения при стриминге файла клиенту (64 КиБ).
pub const STREAM_CHUNK: usize = 64 * 1024;

/// Роутер медиа.
pub fn router() -> Router<Arc<VkState>> {
    Router::new().route("/api/vk/media/:id/:mid/:index", get(get_media))
}

/// `GET /api/vk/media/:peer_id/:message_id/:index?kind=thumb|full`.
///
/// Порядок проверок: форма параметров (`400`) → авторизация (`409`) → кэш →
/// соединение (`503`) → VK. Именно в таком: мусорный `index` — ошибка клиента
/// в любом состоянии, а закэшированное медиа обязано отдаваться и без связи с
/// VK (сетевой сбой не должен гасить уже принятые картинки).
///
/// # Errors
///
/// - `400 invalid_input` — `peer_id`/`message_id`/`index`/`kind` не разбираются;
/// - `409 not_authorized` — аккаунт не привязан;
/// - `404 no_media` — сообщения нет, у него нет такого вложения, у вложения нет
///   файла нужного вида (например, `full` у видео) либо ссылка протухла дважды;
/// - `429 flood_wait` — VK просит подождать;
/// - `503 not_connected` — нет связи либо загрузка сорвалась.
pub async fn get_media(
    State(state): State<Arc<VkState>>,
    Path((peer, mid, index)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    // ↓ ЕДИНСТВЕННЫЙ путь строк из URL в файловую систему.
    let key = CacheKey::parse(&peer, &mid, &index, kind_param(query.as_deref()).as_deref())
        .ok_or_else(|| {
            ApiError::bad_input(
                "expected /api/vk/media/<peer_id>/<message_id>/<index>?kind=thumb|full \
                 with numeric ids and index 0..9",
            )
        })?;
    require_authorized(&state).await?;

    // Быстрый путь: файл уже на диске — ни соединения, ни VK не нужно.
    if let Some(hit) = media_cache::get(&state.config, &key).await {
        return serve_cached(hit, &key, true).await;
    }

    require_connected(&state).await?;
    let client = require_client(&state).await?;
    let hit = fetch_cached(&state, &key, || {
        download_attachment(state.clone(), client.clone(), key)
    })
    .await?;
    serve_cached(hit, &key, false).await
}

/// Значение `kind` из сырой query-строки.
///
/// Разбор ручной и без percent-декодирования — как во всех роутах плагина:
/// допустимых значений два, оба ASCII, и декодер здесь означал бы новую
/// зависимость ради случая, которого нет. Ошибочное значение отбраковывает
/// [`MediaKind::parse`], а не этот помощник.
fn kind_param(raw: Option<&str>) -> Option<String> {
    raw?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "kind").then(|| v.to_string())
    })
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
    let disposition =
        content_disposition(hit.meta.file_name.as_deref(), key, hit.meta.mime.as_deref());

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, hit.meta.size)
        .header(header::CACHE_CONTROL, CACHE_CONTROL)
        .body(Body::from_stream(stream))
        .map_err(|e| ApiError::not_connected(format!("could not build media response: {e}")))?;

    // Заголовки со «внешними» данными ставим через HeaderValue::from_str: имя
    // файла приходит из VK, и невалидные байты не должны ронять ответ.
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
/// Имя файла приходит из VK, то есть от чужого аккаунта. Поэтому:
///
/// 1. берётся только последний сегмент пути (`../../evil.sh` ⇒ `evil.sh`);
/// 2. ASCII-версия чистится до `[A-Za-z0-9._-]` — так в кавычках заведомо
///    не окажется ни кавычки, ни `;`, ни перевода строки (заголовочная
///    инъекция), ни `..`;
/// 3. оригинал добавляется как `filename*=UTF-8''…` в percent-encoding —
///    браузеры предпочтут его, поэтому кириллические имена не теряются.
///
/// Пустое имя заменяется на `vk_<message_id>_<index>_<kind>.<ext>` (расширение
/// — по MIME): `filename=""` некоторые клиенты понимают как «файл без имени» и
/// сохраняют его как `download`. ИНДЕКС в запасном имени обязателен — иначе два
/// фото одного сообщения сохранились бы под одним именем.
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

/// Последний сегмент пути по обоим разделителям (VK отдаёт имена как есть, а
/// Windows-клиенты присылают `C:\dir\file.jpg`).
fn last_path_segment(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// Имя, когда VK его не прислал (фото, голосовые и стикеры — всегда).
fn fallback_name(key: &CacheKey, mime: Option<&str>) -> String {
    let ext = mime
        .and_then(mime_guess::get_mime_extensions_str)
        .and_then(|exts| exts.first().copied())
        .unwrap_or("bin");
    format!("vk_{}_{}_{}.{ext}", key.message_id, key.index, key.kind)
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
        if c.is_ascii_alphanumeric()
            || matches!(
                c,
                '.' | '!' | '#' | '$' | '&' | '+' | '-' | '^' | '_' | '`' | '~'
            )
        {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Уровень 1 дедупа: ключ кэша.
// ---------------------------------------------------------------------------

/// Снятие записи `media_inflight` — в том числе при панике и раннем `return`.
///
/// Без guard'а любой `?` в середине загрузки оставлял бы ключ в реестре
/// навсегда: все последующие запросы этого медиа ждали бы [`INFLIGHT_WAIT`]
/// и получали `503`. `Drop` не может `await`, поэтому снятие уходит в
/// отдельную задачу; штатный путь зовёт [`InflightGuard::release`]
/// напрямую, и тогда `Drop` уже ничего не делает.
struct InflightGuard {
    /// Состояние плагина (реестр живёт в нём).
    state: Arc<VkState>,
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
async fn acquire(state: &Arc<VkState>, key: &CacheKey) -> Slot {
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
/// Гонка «владелец закончил, пока мы шли к await» закрыта тем же приёмом, что
/// в `longpoll::wait_authorized`: `notified()` включается ДО повторной проверки
/// кэша (`Notify::notify_waiters` не оставляет permit'ов).
///
/// # Errors
///
/// Ошибка загрузки владельца (её видит только он) либо `503`, если после
/// всех ожиданий файла в кэше так и нет.
pub async fn fetch_cached<F, Fut>(
    state: &Arc<VkState>,
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
                            target: "forge_vk",
                            key = %key.id(),
                            "forge-vk: gave up waiting for a concurrent media download"
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

// ---------------------------------------------------------------------------
// Уровень 2 дедупа: сообщение.
// ---------------------------------------------------------------------------

/// Снятие записи `message_inflight` (близнец [`InflightGuard`]).
struct MessageGuard {
    /// Состояние плагина.
    state: Arc<VkState>,
    /// Ключ `(peer_id, message_id)`.
    key: (i64, i32),
    /// Запись уже снята штатно.
    released: bool,
}

impl MessageGuard {
    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let taken = self.state.message_inflight.lock().await.remove(&self.key);
        if let Some(n) = taken {
            n.notify_waiters();
        }
    }
}

impl Drop for MessageGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let state = self.state.clone();
        let key = self.key;
        tokio::spawn(async move {
            let taken = state.message_inflight.lock().await.remove(&key);
            if let Some(n) = taken {
                n.notify_waiters();
            }
        });
    }
}

/// Занять ключ сообщения либо стать ожидающим.
async fn acquire_message(state: &Arc<VkState>, key: (i64, i32)) -> Result<MessageGuard, Arc<Notify>> {
    let mut guard = state.message_inflight.lock().await;
    match guard.get(&key) {
        Some(n) => Err(n.clone()),
        None => {
            guard.insert(key, Arc::new(Notify::new()));
            Ok(MessageGuard {
                state: state.clone(),
                key,
                released: false,
            })
        }
    }
}

/// Вложения сообщения — РОВНО ОДИН `getById` на всю галерею.
///
/// Это и есть второй уровень дедупа: десять карточек одного сообщения приходят
/// почти одновременно, и без него каждая стоила бы своего запроса — при
/// бюджете 3 rps это 3+ секунды троттлинга на одну картинку.
///
/// # Errors
///
/// Ошибки VK по [`map_vk_error`]; `404 unknown_message`, если сообщения нет в
/// этой беседе (пустой `items` — штатный ответ VK на чужой или удалённый id).
async fn resolve_message(
    state: &Arc<VkState>,
    client: &VkClient,
    key: &CacheKey,
) -> Result<Arc<Vec<AttachmentRaw>>, ApiError> {
    let mkey = key.message_key();
    for _ in 0..MAX_INFLIGHT_ATTEMPTS {
        if let Some(hit) = state
            .cached_message(mkey.0, mkey.1, dto::now_unix())
            .await
        {
            return Ok(hit);
        }
        match acquire_message(state, mkey).await {
            Ok(mut guard) => {
                let result = fetch_attachments(state, client, key).await;
                guard.release().await;
                return result;
            }
            Err(notify) => {
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(hit) = state
                    .cached_message(mkey.0, mkey.1, dto::now_unix())
                    .await
                {
                    return Ok(hit);
                }
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(MESSAGE_WAIT) => {
                        tracing::warn!(
                            target: "forge_vk",
                            peer_id = mkey.0,
                            message_id = mkey.1,
                            "forge-vk: gave up waiting for a concurrent message resolve"
                        );
                    }
                }
            }
        }
    }
    fetch_attachments(state, client, key).await
}

/// `messages.getById` → вложения, с записью в короткоживущий кэш.
///
/// Имена авторов здесь НЕ добираются намеренно: карточке вложения они не
/// нужны, а лишний `users.get` — это треть секундного бюджета запросов.
async fn fetch_attachments(
    state: &Arc<VkState>,
    client: &VkClient,
    key: &CacheKey,
) -> Result<Arc<Vec<AttachmentRaw>>, ApiError> {
    let page = match client.get_by_id(&[i64::from(key.message_id)]).await {
        Ok(p) => p,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };
    let raw = page
        .items
        .into_iter()
        // Сверка беседы обязательна: `messages.getById` адресуется глобальным
        // id, и без неё маршрут отдавал бы вложение из ЧУЖОЙ беседы тому, кто
        // подобрал номер.
        .find(|m| m.peer_id == key.peer_id)
        .ok_or_else(|| {
            ApiError::unknown_message(format!(
                "message {} is not available in dialog {}",
                key.message_id, key.peer_id
            ))
        })?;

    let atts = Arc::new(raw.attachments);
    state
        .remember_message(key.peer_id, key.message_id, atts.clone(), dto::now_unix())
        .await;
    Ok(atts)
}

// ---------------------------------------------------------------------------
// Скачивание.
// ---------------------------------------------------------------------------

/// Удаляет staging-файл, если загрузка не довела дело до `commit`.
///
/// Обрыв соединения, отменённый клиентом запрос, ошибка записи — после любого
/// из них в каталоге кэша не должно остаться `.tmp`-мусора.
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

/// Исход одной попытки скачивания.
enum Attempt {
    /// CDN отверг подписанную ссылку — она протухла между резолвом и
    /// скачиванием. Единственный случай, когда повтор ИМЕЕТ смысл.
    Expired(u16),
    /// Всё остальное — уже готовый ответ клиенту.
    Failed(ApiError),
}

/// Скачивает вложение из VK и кладёт его в кэш.
///
/// Единственный сетевой путь модуля. Порядок: резолв сообщения (с дедупом) →
/// [`attach::url_for`] → потоковая запись в staging с событиями
/// `download_progress` → `commit` (rename) → перечитывание записи из кэша
/// (чтобы отдавать её тем же кодом, что и попадание).
///
/// # Errors
///
/// - `404 no_media` — вложения с таким индексом нет, у него нет файла нужного
///   вида (например, `full` у видео) или ссылка протухла дважды подряд;
/// - `404 unknown_message` — сообщения нет в этой беседе;
/// - `429`/`503`/`409` — по классификации [`map_vk_error`];
/// - `503 not_connected` — ошибка записи на диск.
pub async fn download_attachment(
    state: Arc<VkState>,
    client: Arc<VkClient>,
    key: CacheKey,
) -> Result<CachedMedia, ApiError> {
    let atts = resolve_message(&state, &client, &key).await?;
    match try_download(&state, &client, &key, &atts).await {
        Ok(hit) => Ok(hit),
        Err(Attempt::Failed(e)) => Err(e),
        Err(Attempt::Expired(status)) => {
            // ↓ РОВНО ОДИН повтор, и обязательно с ЗАНОВО добытым адресом:
            //   повторять с той же ссылкой бессмысленно — она уже отвергнута.
            tracing::debug!(
                target: "forge_vk",
                key = %key.id(),
                status,
                "forge-vk: attachment link expired, resolving it again"
            );
            state.forget_message(key.peer_id, key.message_id).await;
            let fresh = fetch_attachments(&state, &client, &key).await?;
            match try_download(&state, &client, &key, &fresh).await {
                Ok(hit) => Ok(hit),
                Err(Attempt::Failed(e)) => Err(e),
                Err(Attempt::Expired(status)) => Err(ApiError::no_media(format!(
                    "VK refused the attachment link twice (HTTP {status}) for message {} \
                     attachment {}",
                    key.message_id, key.index
                ))),
            }
        }
    }
}

/// Одна попытка: выбрать адрес, скачать, положить в кэш.
async fn try_download(
    state: &Arc<VkState>,
    client: &VkClient,
    key: &CacheKey,
    atts: &[AttachmentRaw],
) -> Result<CachedMedia, Attempt> {
    let att = atts.get(usize::from(key.index)).ok_or_else(|| {
        Attempt::Failed(ApiError::no_media(format!(
            "message {} has no attachment #{}",
            key.message_id, key.index
        )))
    })?;
    let url = attach::url_for(att, key.kind).ok_or_else(|| {
        Attempt::Failed(ApiError::no_media(format!(
            "attachment #{} of message {} has no {} to download",
            key.index, key.message_id, key.kind
        )))
    })?;

    let staging = media_cache::begin_staging(&state.config, key)
        .await
        .map_err(|e| {
            Attempt::Failed(ApiError::not_connected(format!(
                "could not prepare the media cache: {e}"
            )))
        })?;
    let mut guard = StagingGuard {
        path: Some(staging.clone()),
    };

    let (received, declared_mime) = stream_to_file(state, client, key, &url, &staging).await?;
    if received == 0 {
        return Err(Attempt::Failed(ApiError::no_media(format!(
            "VK returned an empty file for attachment #{} of message {}",
            key.index, key.message_id
        ))));
    }

    let info = attach::media_from_attachment(key.index, att);
    let mut meta = MediaMeta::now(
        // MIME из ответа CDN точнее вычисленного по вложению: у документа с
        // безымянным файлом иначе получился бы `application/octet-stream`.
        declared_mime.or_else(|| thumb_mime(key, &info)),
        received,
        info.file_name.clone(),
    );
    meta.width = info.width;
    meta.height = info.height;
    meta.duration_secs = info.duration_secs;

    media_cache::commit(&state.config, key, &staging, &meta)
        .await
        .map_err(|e| {
            Attempt::Failed(ApiError::not_connected(format!(
                "could not store media in the cache: {e}"
            )))
        })?;
    guard.disarm();

    tracing::debug!(
        target: "forge_vk",
        peer_id = key.peer_id,
        message_id = key.message_id,
        index = key.index,
        kind = %key.kind,
        bytes = received,
        "forge-vk: attachment downloaded into the cache"
    );

    media_cache::get(&state.config, key).await.ok_or_else(|| {
        Attempt::Failed(ApiError::not_connected(
            "media was cached but vanished before it could be served",
        ))
    })
}

/// MIME, когда CDN его не объявил.
///
/// У превью это всегда картинка, независимо от вида вложения: и кадр видео, и
/// превью документа приезжают JPEG'ом. У оригинала берётся то, что вычислил
/// разбор вложения.
fn thumb_mime(key: &CacheKey, info: &dto::MediaDto) -> Option<String> {
    match key.kind {
        MediaKind::Thumb => Some("image/jpeg".to_string()),
        MediaKind::Full => info.mime.clone(),
    }
}

/// Пишет тело ответа CDN в файл чанками, отправляя `download_progress`.
///
/// Возвращает `(сколько байт записано, MIME из заголовка ответа)`. Финальное
/// событие отправляется всегда (см. [`crate::progress`]), поэтому прогрессбар
/// во фронте гарантированно доходит до конца — даже когда размер неизвестен.
async fn stream_to_file(
    state: &Arc<VkState>,
    client: &VkClient,
    key: &CacheKey,
    url: &str,
    staging: &FsPath,
) -> Result<(u64, Option<String>), Attempt> {
    let resp = match client.get_stream(url).await {
        Ok(r) => r,
        // ↓ ЕДИНСТВЕННАЯ ветка, где повтор осмыслен: подписанная ссылка VK
        //   протухла между резолвом и скачиванием.
        Err(VkError::Http { status }) if is_expired_link(status) => {
            return Err(Attempt::Expired(status))
        }
        Err(e) => return Err(Attempt::Failed(crate::api::error::to_api_error(&e))),
    };
    let total = resp.content_length();
    let mime = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_string())
        .filter(|v| !v.is_empty() && v != "application/octet-stream");

    let mut reporter = DownloadReporter::for_key(state.clone(), key);
    let mut file = tokio::fs::File::create(staging).await.map_err(|e| {
        Attempt::Failed(ApiError::not_connected(format!(
            "could not open the staging file: {e}"
        )))
    })?;

    let mut stream = resp.bytes_stream();
    let mut received = 0u64;
    reporter.report(0, total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            Attempt::Failed(ApiError::not_connected(format!(
                "the attachment stream broke: {}",
                e.without_url()
            )))
        })?;
        file.write_all(&chunk).await.map_err(|e| {
            Attempt::Failed(ApiError::not_connected(format!(
                "could not write media chunk: {e}"
            )))
        })?;
        received += chunk.len() as u64;
        reporter.report(received, total);
    }
    file.flush().await.map_err(|e| {
        Attempt::Failed(ApiError::not_connected(format!(
            "could not flush media file: {e}"
        )))
    })?;
    file.sync_all().await.map_err(|e| {
        Attempt::Failed(ApiError::not_connected(format!(
            "could not fsync media file: {e}"
        )))
    })?;
    // Финал — после fsync: событие «готово» не должно опережать данные.
    reporter.finish(received, final_total(total, received));
    Ok((received, mime))
}

/// Значит ли статус CDN «ссылка больше не действует».
///
/// `403` — просроченная или чужая подпись, `404` — файл убрали, `410` — то же
/// самое, но честнее. Всё остальное (5xx, 429) к ссылке отношения не имеет, и
/// повтор с новым адресом ничего бы не изменил.
fn is_expired_link(status: u16) -> bool {
    matches!(status, 403 | 404 | 410)
}

/// `total` для ФИНАЛЬНОГО события прогресса.
///
/// Когда CDN не сообщил размер заранее (`total: None`), финальное событие
/// обязано подставить фактически полученное: фронт считает процент как
/// `received / total`, и с `total: null` на последнем кадре индикатор навсегда
/// остался бы индетерминированным, хотя файл уже целиком на диске.
pub fn final_total(total: Option<u64>, received: u64) -> Option<u64> {
    total.or(Some(received))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VkConfig;
    use crate::login::{LoginFlow, VkUserDto};
    use axum::http::Request;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn cfg(dir: &FsPath) -> VkConfig {
        VkConfig {
            cache_dir: dir.join("cache"),
            config_path: dir.join("vk.toml"),
            ..VkConfig::default()
        }
    }

    fn state_in(dir: &FsPath) -> Arc<VkState> {
        VkState::new(cfg(dir))
    }

    async fn authorized(dir: &FsPath) -> Arc<VkState> {
        let st = state_in(dir);
        st.set_login(LoginFlow::Authorized {
            user: VkUserDto {
                id: "999".into(),
                first_name: Some("я".into()),
                ..Default::default()
            },
        })
        .await;
        // Здоровье «сейчас» — иначе первый же запрос упёрся бы в 503 и тесты
        // проверяли бы лестницу, а не отдачу.
        st.note_ok(dto::now_unix()).await;
        st
    }

    fn key() -> CacheKey {
        CacheKey {
            peer_id: 2_000_000_001,
            message_id: 7,
            index: 0,
            kind: MediaKind::Full,
        }
    }

    async fn seed(state: &Arc<VkState>, key: &CacheKey, bytes: &[u8], mime: &str) {
        let mut meta = MediaMeta::now(
            Some(mime.to_string()),
            bytes.len() as u64,
            Some("отпуск фото.jpg".into()),
        );
        meta.width = Some(1280);
        meta.height = Some(720);
        media_cache::put_atomic(&state.config, key, bytes, &meta)
            .await
            .unwrap();
    }

    fn app(state: Arc<VkState>) -> Router {
        router().with_state(state)
    }

    async fn body_bytes(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }

    // ---------------- kind из query ----------------

    #[test]
    fn kind_param_reads_only_its_own_key() {
        assert_eq!(kind_param(None), None);
        assert_eq!(kind_param(Some("")), None);
        assert_eq!(kind_param(Some("kind=thumb")).as_deref(), Some("thumb"));
        assert_eq!(
            kind_param(Some("x=1&kind=full&y=2")).as_deref(),
            Some("full")
        );
        assert_eq!(kind_param(Some("kinda=thumb")), None);
        assert_eq!(kind_param(Some("nokind")), None);
    }

    // ---------------- Content-Disposition ----------------

    #[test]
    fn disposition_sanitizes_hostile_names() {
        let k = key();
        let d = content_disposition(Some("../../etc/pa\"ss;wd\r\n"), &k, Some("image/jpeg"));
        assert!(d.starts_with("inline; filename=\""), "{d}");
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
        let d = content_disposition(Some("отчёт.pdf"), &key(), Some("application/pdf"));
        assert!(d.contains("filename*=UTF-8''"), "{d}");
        assert!(d.contains("%D0%BE%D1%82%D1%87"), "{d}");
        assert!(d.contains("filename=\""), "{d}");
    }

    /// Запасное имя несёт ИНДЕКС: без него два фото одного сообщения
    /// сохранились бы под одним именем и второе перезаписало бы первое.
    #[test]
    fn disposition_generates_a_name_with_the_attachment_index() {
        let a = content_disposition(None, &key(), Some("image/jpeg"));
        assert!(a.contains("vk_7_0_full."), "{a}");
        let k = CacheKey {
            index: 3,
            kind: MediaKind::Thumb,
            ..key()
        };
        let b = content_disposition(None, &k, Some("image/jpeg"));
        assert!(b.contains("vk_7_3_thumb."), "{b}");
        // Неизвестный MIME ⇒ .bin, а не пустое расширение.
        let c = content_disposition(None, &k, None);
        assert!(c.contains("vk_7_3_thumb.bin"), "{c}");
    }

    // ---------------- порядок проверок ----------------

    /// Мусорные параметры — это `400`, и до авторизации дело не доходит:
    /// форма запроса не зависит от состояния логина.
    #[tokio::test]
    async fn broken_parameters_are_400_before_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(dir.path()); // НЕ авторизован
        for uri in [
            "/api/vk/media/../7/0",
            "/api/vk/media/2000000001/0/0",
            "/api/vk/media/2000000001/7/10",
            "/api/vk/media/2000000001/7/0?kind=thmub",
        ] {
            let resp = app(state.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    /// Неавторизованному — `409`, даже если файл лежит в кэше: кэш принадлежит
    /// аккаунту, а не процессу.
    #[tokio::test]
    async fn unauthorized_never_sees_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_in(dir.path());
        seed(&state, &key(), b"secret bytes", "image/jpeg").await;

        let resp = app(state)
            .oneshot(
                Request::builder()
                    .uri("/api/vk/media/2000000001/7/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// ГЕЙТ ФАЗЫ: попадание в кэш отдаётся БЕЗ единого обращения к VK и без
    /// требования связи. Здоровье намеренно «сломано» — ответ обязан быть 200.
    #[tokio::test]
    async fn a_cache_hit_needs_neither_connection_nor_vk() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        state.note_down(dto::now_unix(), "network is down").await;
        assert!(!state.connected(dto::now_unix()).await, "связи нет");
        // Токена нет вовсе ⇒ построить клиента невозможно: любой поход в VK
        // на этом пути дал бы 503, а не 200.
        seed(&state, &key(), b"cached-bytes", "image/jpeg").await;

        let resp = app(state)
            .oneshot(
                Request::builder()
                    .uri("/api/vk/media/2000000001/7/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let headers = resp.headers().clone();
        assert_eq!(headers[header::CACHE_CONTROL], CACHE_CONTROL);
        assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(headers[header::CONTENT_LENGTH], "12");
        assert!(
            headers[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains("filename*=UTF-8''"),
            "{headers:?}"
        );
        assert_eq!(body_bytes(resp).await, b"cached-bytes");
    }

    /// Разные индексы — разные файлы и на выдаче тоже.
    #[tokio::test]
    async fn each_attachment_index_is_served_separately() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        seed(&state, &key(), b"FIRST", "image/jpeg").await;
        seed(
            &state,
            &CacheKey { index: 1, ..key() },
            b"SECOND",
            "image/png",
        )
        .await;

        for (uri, expected) in [
            ("/api/vk/media/2000000001/7/0", &b"FIRST"[..]),
            ("/api/vk/media/2000000001/7/1", &b"SECOND"[..]),
        ] {
            let resp = app(state.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
            assert_eq!(body_bytes(resp).await, expected, "{uri}");
        }
    }

    /// Промах при отсутствии связи — `503`, а не поход в VK.
    #[tokio::test]
    async fn a_miss_without_connection_is_503() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        state.note_down(dto::now_unix(), "network is down").await;

        let resp = app(state)
            .oneshot(
                Request::builder()
                    .uri("/api/vk/media/2000000001/7/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ---------------- дедуп по ключу ----------------

    /// Десять параллельных запросов ОДНОГО файла стоят одного скачивания.
    #[tokio::test]
    async fn concurrent_requests_download_once() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        let k = key();
        let calls = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..10 {
            let state = state.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                fetch_cached(&state.clone(), &k, || {
                    let state = state.clone();
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Изображаем медленную загрузку, чтобы остальные
                        // девять успели встать в ожидание.
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        seed(&state, &k, b"payload", "image/jpeg").await;
                        media_cache::get(&state.config, &k).await.ok_or_else(|| {
                            ApiError::not_connected("cache vanished")
                        })
                    }
                })
                .await
                .map(|h| h.meta.size)
            }));
        }
        for t in tasks {
            assert_eq!(t.await.unwrap().unwrap(), 7);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "скачали больше одного раза");
    }

    /// Упавшая загрузка не оставляет ключ занятым навсегда.
    #[tokio::test]
    async fn a_failed_download_releases_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        let k = key();

        let err = fetch_cached(&state, &k, || async {
            Err::<CachedMedia, ApiError>(ApiError::no_media("nope"))
        })
        .await
        .unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert!(
            state.media_inflight.lock().await.is_empty(),
            "ключ остался в реестре"
        );
    }

    // ---------------- дедуп по сообщению ----------------

    /// Ключ сообщения не зависит ни от индекса, ни от вида: на этом и держится
    /// «одна галерея — один getById».
    #[test]
    fn the_message_key_ignores_index_and_kind() {
        let a = CacheKey {
            index: 0,
            kind: MediaKind::Thumb,
            ..key()
        };
        let b = CacheKey {
            index: 9,
            kind: MediaKind::Full,
            ..key()
        };
        assert_eq!(a.message_key(), b.message_key());
        assert_ne!(a.id(), b.id());
    }

    /// Короткоживущий кэш сообщений отдаёт вложения без похода в VK и
    /// протухает по TTL.
    #[tokio::test]
    async fn the_message_cache_expires() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        let atts = Arc::new(vec![AttachmentRaw {
            kind: "photo".into(),
            ..AttachmentRaw::default()
        }]);
        let now = 1_800_000_000;
        state.remember_message(1, 7, atts.clone(), now).await;

        assert!(state.cached_message(1, 7, now).await.is_some());
        assert!(state.cached_message(1, 7, now + 59).await.is_some());
        assert!(
            state.cached_message(1, 7, now + 61).await.is_none(),
            "запись обязана протухнуть"
        );
        assert!(state.cached_message(2, 7, now).await.is_none(), "чужая беседа");
    }

    /// Ёмкость кэша сообщений ограничена: карта наполняется запросами
    /// браузера и без потолка росла бы неограниченно.
    #[tokio::test]
    async fn the_message_cache_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        let now = 1_800_000_000;
        for i in 1..=300i32 {
            state
                .remember_message(1, i, Arc::new(Vec::new()), now)
                .await;
        }
        let len = state.message_cache.lock().await.len();
        assert_eq!(len, crate::state::MESSAGE_CACHE_CAPACITY);
        // Вытесняется самая старая запись, свежая остаётся.
        assert!(state.cached_message(1, 1, now).await.is_none());
        assert!(state.cached_message(1, 300, now).await.is_some());
    }

    /// Правка сообщения выбрасывает его из кэша: иначе повтор скачивания
    /// пошёл бы по адресу вложения, которого в сообщении уже нет.
    #[tokio::test]
    async fn forget_message_drops_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let state = authorized(dir.path()).await;
        let now = dto::now_unix();
        state.remember_message(1, 7, Arc::new(Vec::new()), now).await;
        state.forget_message(1, 7).await;
        assert!(state.cached_message(1, 7, now).await.is_none());
    }

    // ---------------- прочее ----------------

    #[test]
    fn expired_link_statuses_are_a_closed_list() {
        for s in [403, 404, 410] {
            assert!(is_expired_link(s), "{s}");
        }
        for s in [200, 429, 500, 502, 503] {
            assert!(!is_expired_link(s), "{s}");
        }
    }

    #[test]
    fn final_total_falls_back_to_received() {
        assert_eq!(final_total(Some(10), 10), Some(10));
        assert_eq!(final_total(None, 42), Some(42));
    }
}
