//! Прокси байтов: вложения сообщений и аватары сотрудников (Ф8).
//!
//! | метод | путь | что отдаёт |
//! |---|---|---|
//! | GET | `/api/bitrix/media/file/:dialog/:mid/:index` | байты вложения |
//! | GET | `/api/bitrix/media/:dialog/:mid/:index` | то же, короткая форма |
//! | GET | `/api/bitrix/media/avatar/:user_id` | байты аватара (Р6) |
//!
//! # ЕДИНСТВЕННЫЙ ПУТЬ СКАЧИВАНИЯ ВЛОЖЕНИЯ
//!
//! ```text
//! страница истории ──► params.FILE_ID[index] ──► im.v2.File.download {fileId}
//!                                                          │
//!                                                    downloadUrl (СЕКРЕТ)
//!                                                          │
//!                                            стрим в staging ──► rename в кэш
//!                                                          │
//!                                                   БАЙТЫ браузеру
//! ```
//!
//! Поля `urlDownload`, `urlShow` и `viewerAttrs.unifiedLink`, которые портал
//! кладёт прямо в дескриптор файла, **не работают без сессионной куки**: живая
//! проверка дала `HTTP 200` и 52 КБ HTML страницы логина у первых двух и `403`
//! у третьего. Их нет ни в [`crate::api::schema::FileRaw`], ни в
//! [`crate::attach::MediaDto`] — не объявить проще, чем не отдать.
//!
//! # `downloadUrl` НИКОГДА не покидает процесс
//!
//! Адрес имеет вид `https://<портал>/rest/<uid>/<КОД ВЕБХУКА>/download/?token=…`
//! — то есть цитирует секрет плагина ЦЕЛИКОМ. Поэтому:
//!
//! * он не уезжает в тело ответа (тест-страж `download_urls_never_leak_to_the_client`);
//! * **редирект запрещён**: заголовка `Location` в ответах этого модуля не
//!   бывает вовсе. Ответить `302` было бы вдвое дешевле по трафику и означало
//!   бы отдать браузеру, расширениям браузера и любому HAR-файлу поддержки
//!   полный доступ к порталу;
//! * он не логируется ни на каком уровне, включая `trace`;
//! * на диск вместо него ложится отпечаток
//!   ([`crate::media_cache::source_tag`]).
//!
//! # Лестница проверок
//!
//! `авторизация (409) → объект (404) → параметр (400) → кэш → связь (503) →
//! портал`.
//!
//! Авторизация ПЕРВОЙ — общее правило data-роутов плагина: иначе неподключённый
//! клиент прощупывал бы валидность идентификаторов портала по разнице кодов
//! ответа. А вот `503` стоит ПОСЛЕ кэша сознательно: уже скачанная картинка
//! обязана открываться и при оборванной сети — сетевой сбой не повод гасить
//! ленту, которая целиком лежит на диске.
//!
//! # `Cache-Control: immutable` и инвалидация
//!
//! У вложения ответ помечен `immutable` на год ([`CACHE_CONTROL_FILE`]).
//! Законно это ровно потому, что содержимое ключа `(dialog, message_id, index)`
//! неизменно, а когда оно всё-таки меняется — запись стирается сервером:
//!
//! | что случилось | кто зовёт [`crate::media_cache::invalidate`] |
//! |---|---|
//! | `ONIMV2MESSAGEDELETE` | [`crate::events`] |
//! | правка последнего сообщения замечена диффом | [`crate::recent_sync`] |
//!
//! У АВАТАРА `immutable` был бы багом: сотрудник меняет фото когда угодно, а
//! события об этом платформа не присылает вовсе. Поэтому там короткий
//! [`CACHE_CONTROL_AVATAR`] плюс серверная проверка отпечатка источника — новый
//! CDN-адрес выбрасывает старые байты из кэша.
//!
//! # Дедуп inflight
//!
//! Открытие ленты с десятком вложений даёт десяток параллельных запросов, а
//! бюджет портала — два вызова в секунду. Первый запросивший становится
//! владельцем ключа, остальные ждут его `Notify` и перечитывают кэш
//! ([`fetch_cached`]). Десять запросов одного файла стоят ОДНОГО похода в
//! портал.
//!
//! # Почему нет `Accept-Ranges`
//!
//! Range-запросы не поддерживаются, поэтому и не анонсируются: `<video>`,
//! увидев `Accept-Ranges: bytes`, шлёт `Range` и ждёт `206`; получив `200` с
//! полным телом, часть браузеров ломает воспроизведение. Фронт грузит медиа
//! через `fetch` + `blob:` (у `<img>`/`<audio>` нет Bearer-токена), поэтому
//! Range ему и не нужен.

use std::future::Future;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio_util::io::ReaderStream;

use crate::api::B24Client;
use crate::attach::MediaDto;
use crate::media_cache::{self, CacheKey, CachedMedia, DialogKey, MediaMeta};
use crate::progress::DownloadReporter;
use crate::routes::dialogs::{
    load_page, map_portal_error, require_authorized, require_client, require_connected,
};
use crate::routes::error::ApiError;
use crate::state::B24State;

/// `Cache-Control` вложения: год плюс `immutable`.
///
/// Законен ТОЛЬКО в связке с работающей инвалидацией (см. шапку модуля). Убрав
/// одну, обязательно убрать и второе.
pub const CACHE_CONTROL_FILE: &str = "private, max-age=31536000, immutable";

/// `Cache-Control` аватара: час, БЕЗ `immutable`.
///
/// Сотрудник меняет фото когда захочет, а события об этом платформа не
/// присылает: пометить ответ `immutable` значило бы оставить старое лицо в
/// браузере на год. Час — компромисс между «лишние запросы» и «сменил фото, а
/// панель не заметила».
pub const CACHE_CONTROL_AVATAR: &str = "private, max-age=3600";

/// Сколько ждать чужую загрузку того же медиа, прежде чем взяться самому.
pub const INFLIGHT_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Сколько раз «подождать и перечитать кэш» до того, как качать самому.
///
/// Два: после одного ожидания файл либо есть, либо владелец упал, и третий круг
/// ожидания бессмысленен.
pub const MAX_INFLIGHT_ATTEMPTS: usize = 2;

/// Размер буфера чтения при стриминге файла клиенту (64 КиБ).
pub const STREAM_CHUNK: usize = 64 * 1024;

/// Роутер медиа.
///
/// Путей у вложения ДВА и они полные синонимы: длинная форма `…/media/file/…`
/// — та, что записана в замороженной таблице [`crate::routes`], короткая
/// `…/media/…` — та, которой адресуются соседние панели. Коллизии нет:
/// сегментов разное число, а `avatar` в позиции `:dialog` не разбирается в
/// идентификатор диалога.
pub fn router() -> Router<Arc<B24State>> {
    Router::new()
        .route(
            "/api/bitrix/media/file/:dialog/:mid/:index",
            get(get_attachment),
        )
        .route("/api/bitrix/media/:dialog/:mid/:index", get(get_attachment))
        .route("/api/bitrix/media/avatar/:user_id", get(get_avatar))
}

// ---------------------------------------------------------------------------
// Вложения.
// ---------------------------------------------------------------------------

/// `GET /api/bitrix/media/file/:dialog/:mid/:index` — байты вложения.
///
/// # Errors
///
/// - `409 not_authorized` — портал не подключён;
/// - `404 unknown_dialog` / `404 unknown_message` — идентификатор не разбирается
///   либо такого сообщения в диалоге нет;
/// - `400 invalid_input` — индекс вложения не число или больше
///   [`crate::media_cache::MAX_ATTACH_INDEX`];
/// - `404 no_media` — у сообщения нет вложения с таким индексом, у вложения нет
///   файла, либо сообщение УДАЛЕНО (байты портал больше не отдаёт);
/// - `429 flood_wait` / `502` — портал ограничил или отказал;
/// - `503 not_connected` — связи нет, а в кэше пусто.
pub async fn get_attachment(
    State(state): State<Arc<B24State>>,
    Path((dialog, mid, index)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    // Ступень 1: подключение. Строго ДО разбора — иначе роут работал бы
    // оракулом валидности идентификаторов портала для неподключённого клиента.
    require_authorized(&state).await?;

    // ↓ ЕДИНСТВЕННЫЙ путь строк из URL к файловой системе: три разбора в числа
    //   и в перечисление из трёх вариантов. Строки в путь не конкатенируются.
    let dialog = DialogKey::parse(&dialog)
        .ok_or_else(|| ApiError::unknown_dialog("no such dialog in this portal".to_string()))?;
    let message_id = media_cache::parse_entity_id(&mid)
        .ok_or_else(|| ApiError::unknown_message("no such message in this portal".to_string()))?;
    let index = media_cache::parse_attach_index(&index).ok_or_else(|| {
        ApiError::bad_input(format!(
            "the attachment index must be a number within 0..={}",
            media_cache::MAX_ATTACH_INDEX
        ))
    })?;
    let key = CacheKey::File {
        dialog,
        message_id,
        index,
    };

    // Быстрый путь: файл на диске — ни связи, ни портала не нужно.
    if let Some(hit) = media_cache::get(&state.config, &key).await {
        return serve_cached(hit, &key, CACHE_CONTROL_FILE, true).await;
    }

    // Сообщение удалено — портал байты больше не отдаст. Отвечаем сразу и БЕЗ
    // ЕДИНОГО СЕТЕВОГО ВЫЗОВА: иначе каждый показ старой ленты стоил бы двух
    // обращений к порталу и всё равно заканчивался бы этим же 404.
    if state.is_deleted(message_id) {
        return Err(deleted_error(message_id));
    }

    require_connected(&state).await?;
    let client = require_client(&state).await?;
    let hit = fetch_cached(&state, &key, || {
        download_attachment(state.clone(), client.clone(), key)
    })
    .await?;
    serve_cached(hit, &key, CACHE_CONTROL_FILE, false).await
}

/// Отказ «сообщение удалено».
///
/// Отдельная функция, потому что причина одна, а мест две (память об удалении и
/// свежая страница истории), и формулировка обязана совпадать: фронт различает
/// ветки по коду, а человек в поддержке — по тексту.
fn deleted_error(message_id: i64) -> ApiError {
    ApiError::no_media(format!(
        "message {message_id} was deleted, the portal no longer serves its files"
    ))
}

/// Скачивает вложение из портала и кладёт его в кэш.
///
/// Единственный сетевой путь вложений. Порядок: страница истории на ОДНО
/// сообщение → `im.v2.File.download` → потоковая запись в staging с кадрами
/// `download_progress` → `commit` (rename) → перечитывание записи из кэша,
/// чтобы отдавать её тем же кодом, что и попадание.
///
/// # Errors
///
/// См. [`get_attachment`].
pub async fn download_attachment(
    state: Arc<B24State>,
    client: Arc<B24Client>,
    key: CacheKey,
) -> Result<CachedMedia, ApiError> {
    let CacheKey::File {
        dialog,
        message_id,
        index,
    } = key
    else {
        // Аватары качает `download_avatar`; сюда попасть невозможно, но
        // `unreachable!()` в сетевом коде — плохой способ узнать о своей
        // ошибке.
        return Err(ApiError::no_media("not an attachment key".to_string()));
    };
    let dialog_id = dialog.dir_name();

    // Страница ровно на одно сообщение: метода «дай сообщение по id» у портала
    // нет, поэтому лестница та же, что у `GET /api/bitrix/messages/:mid` (Ф5) —
    // `LAST_ID = mid + 1`, `LIMIT = 1`.
    let page = load_page(
        &state,
        &client,
        &dialog_id,
        1,
        Some(message_id.saturating_add(1)),
        None,
    )
    .await?;
    let message = page
        .into_iter()
        .find(|m| m.id == message_id)
        .ok_or_else(|| {
            ApiError::unknown_message(format!(
                "message {message_id} is not available in dialog {dialog_id}"
            ))
        })?;

    if message.deleted {
        // Запоминаем, чтобы следующий запрос той же карточки стоил нуля
        // вызовов портала.
        state.note_deleted(message_id);
        return Err(deleted_error(message_id));
    }

    let media = message
        .media
        .into_iter()
        .find(|m| m.index == index)
        .ok_or_else(|| {
            ApiError::no_media(format!(
                "message {message_id} has no attachment #{index}"
            ))
        })?;
    if !media.has_full || media.file_id <= 0 {
        // Дескриптор файла не приехал в ответе портала: карточка в ленте
        // нарисована заглушкой, и скачивать нечего.
        return Err(ApiError::no_media(format!(
            "attachment #{index} of message {message_id} has no file on the portal"
        )));
    }

    // ↓ ЗДЕСЬ РОЖДАЕТСЯ СЕКРЕТ. Дальше он живёт только в локальной переменной:
    //   ни в логе, ни в кэше, ни в ответе его не будет.
    let download_url = client
        .file_download(media.file_id)
        .await
        .map_err(|e| map_portal_error(&e))?;

    let staging = media_cache::begin_staging(&state.config, &key)
        .await
        .map_err(|e| {
            ApiError::not_connected(format!("could not prepare the media cache: {e}"))
        })?;
    let mut guard = StagingGuard {
        path: Some(staging.clone()),
    };

    let (received, declared_mime) =
        stream_to_file(&state, &client, &key, &download_url, &staging).await?;
    if received == 0 {
        return Err(ApiError::no_media(format!(
            "the portal returned an empty file for attachment #{index} of message {message_id}"
        )));
    }

    let mut meta = MediaMeta::now(
        // MIME из заголовка ответа точнее вычисленного по имени: у файла без
        // расширения иначе получился бы `application/octet-stream`.
        declared_mime.or_else(|| guess_mime(&media)),
        received,
        media.name.clone(),
    )
    // Отпечаток, а не адрес: `downloadUrl` содержит код вебхука и на диск не
    // ложится ни в каком виде.
    .with_source(&download_url);
    meta.width = media.width;
    meta.height = media.height;

    media_cache::commit(&state.config, &key, &staging, &meta)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not store media in the cache: {e}")))?;
    guard.disarm();

    tracing::debug!(
        target: "forge_bitrix",
        key = %key.id(),
        file_id = media.file_id,
        bytes = received,
        "forge-bitrix: attachment downloaded into the cache"
    );

    media_cache::get(&state.config, &key).await.ok_or_else(|| {
        ApiError::not_connected("media was cached but vanished before it could be served".to_string())
    })
}

/// MIME по имени файла, когда сервер его не объявил.
fn guess_mime(media: &MediaDto) -> Option<String> {
    let name = media.name.as_deref()?;
    mime_guess::from_path(name)
        .first_raw()
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Аватары (Р6).
// ---------------------------------------------------------------------------

/// `GET /api/bitrix/media/avatar/:user_id` — байты аватара сотрудника.
///
/// # Почему через прокси, если CDN портала публичен
///
/// Решение Р6, и оно про инвариант, а не про безопасность конкретного адреса.
/// Правило «внешних URL в DOM панели нет» проверяется одним грепом по разметке
/// ленты; единственное исключение превращает эту проверку в разбор каждого поля
/// каждого DTO. Плюс прямые CDN-ссылки утекали бы `Referer` девфоржа на хост
/// портала при каждом рендере списка.
///
/// # Откуда берётся адрес
///
/// Из [`crate::peers::PeerCache`], который наполняется БЕСПЛАТНО: блок `user`
/// приезжает в каждом событии `ONIMV2MESSAGEADD` и в каждой строке
/// `im.recent.get`. Отдельного запроса ради аватара обычно не нужно вовсе;
/// добор `im.user.list.get` делается, только если о сотруднике не известно
/// ничего.
///
/// # Errors
///
/// - `409 not_authorized`;
/// - `404 no_media` — идентификатор не число, сотрудника нет либо фото у него
///   нет (фронт рисует инициалы на цвете из `color`);
/// - `503 not_connected` — связи нет, а в кэше пусто.
pub async fn get_avatar(
    State(state): State<Arc<B24State>>,
    Path(raw_user): Path<String>,
) -> Result<Response, ApiError> {
    require_authorized(&state).await?;

    let user_id = media_cache::parse_entity_id(&raw_user)
        .ok_or_else(|| ApiError::no_media("no such user in this portal".to_string()))?;
    let key = CacheKey::Avatar { user_id };

    let known = state.peers.avatar(user_id);
    if let Some(hit) = media_cache::get(&state.config, &key).await {
        // Отпечаток источника — единственный доступный признак «фото сменили»:
        // событий об этом платформа не присылает. Совпал (или адрес сейчас
        // неизвестен) — отдаём кэш и в портал не идём.
        let fresh = match (&known, &hit.meta.source_tag) {
            (Some(url), Some(tag)) => &media_cache::source_tag(url) == tag,
            _ => true,
        };
        if fresh {
            return serve_cached(hit, &key, CACHE_CONTROL_AVATAR, true).await;
        }
        media_cache::invalidate_avatar(&state.config, user_id).await;
    }

    require_connected(&state).await?;
    let client = require_client(&state).await?;
    let hit = fetch_cached(&state, &key, || {
        download_avatar(state.clone(), client.clone(), user_id)
    })
    .await?;
    serve_cached(hit, &key, CACHE_CONTROL_AVATAR, false).await
}

/// Скачивает аватар с CDN портала и кладёт его в кэш.
///
/// # Почему адрес берётся напрямую, а не через `im.v2.File.download`
///
/// Аватар — не файл Диска: у него нет `fileId`, и метод скачивания его не
/// знает. CDN-адрес при этом публичен и работает без всякой авторизации
/// (в отличие от `urlDownload` вложения), поэтому запрос идёт по нему —
/// но наружу по-прежнему уходят БАЙТЫ, а не ссылка (Р6).
///
/// # Errors
///
/// `404 no_media`, если сотрудник неизвестен порталу или фото у него нет;
/// транспортные отказы CDN.
pub async fn download_avatar(
    state: Arc<B24State>,
    client: Arc<B24Client>,
    user_id: i64,
) -> Result<CachedMedia, ApiError> {
    let key = CacheKey::Avatar { user_id };

    let mut url = state.peers.avatar(user_id);
    if url.is_none() {
        // О сотруднике не известно НИЧЕГО — добираем одним запросом. Это
        // редкая ветка: обычно кэш имён уже наполнен событиями и строками
        // списка.
        if let Err(e) = state.peers.resolve_uncached(&client, &[user_id]).await {
            return Err(map_portal_error(&e));
        }
        url = state.peers.avatar(user_id);
    }
    let url = url.ok_or_else(|| {
        ApiError::no_media(format!("user {user_id} has no avatar on the portal"))
    })?;

    let staging = media_cache::begin_staging(&state.config, &key)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not prepare the media cache: {e}")))?;
    let mut guard = StagingGuard {
        path: Some(staging.clone()),
    };

    let (received, declared_mime) = stream_to_file(&state, &client, &key, &url, &staging).await?;
    if received == 0 {
        return Err(ApiError::no_media(format!(
            "the portal returned an empty avatar for user {user_id}"
        )));
    }

    let meta = MediaMeta::now(
        declared_mime.or_else(|| Some("image/jpeg".to_string())),
        received,
        None,
    )
    .with_source(&url);

    media_cache::commit(&state.config, &key, &staging, &meta)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not store the avatar: {e}")))?;
    guard.disarm();

    media_cache::get(&state.config, &key)
        .await
        .ok_or_else(|| ApiError::not_connected("the avatar vanished from the cache".to_string()))
}

// ---------------------------------------------------------------------------
// Отдача.
// ---------------------------------------------------------------------------

/// Отдаёт файл из кэша потоком.
///
/// `touch_mtime` обновляет время последнего использования (LRU): только на
/// попадании и только «в фоне» — промах LRU не стоит задержки ответа.
///
/// # Errors
///
/// `503 not_connected`, если файл исчез между проверкой и открытием (его мог
/// снести sweeper): клиенту это «повтори запрос», а не «медиа нет».
pub async fn serve_cached(
    hit: CachedMedia,
    key: &CacheKey,
    cache_control: &'static str,
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
        .header(header::CACHE_CONTROL, cache_control)
        // ↓ Заголовка `Location` здесь нет и быть не может: редирект на
        //   `downloadUrl` отдал бы браузеру код вебхука (см. шапку модуля).
        .body(Body::from_stream(stream))
        .map_err(|e| ApiError::not_connected(format!("could not build media response: {e}")))?;

    // Заголовки с «внешними» данными ставятся через `HeaderValue::from_str`:
    // имя файла пришло от собеседника, и невалидные байты не должны ронять
    // ответ.
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
/// Имя файла приходит из портала, то есть от чужого сотрудника. Поэтому:
///
/// 1. берётся только последний сегмент пути (`../../evil.sh` ⇒ `evil.sh`);
/// 2. ASCII-версия чистится до `[A-Za-z0-9._-]` — так в кавычках заведомо не
///    окажется ни кавычки, ни `;`, ни перевода строки (заголовочная инъекция);
/// 3. оригинал добавляется как `filename*=UTF-8''…` в percent-encoding —
///    браузеры предпочтут его, поэтому кириллические имена не теряются.
///
/// Пустое имя заменяется запасным: `filename=""` часть клиентов понимает как
/// «файл без имени» и сохраняет его как `download`.
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

/// Последний сегмент пути по обоим разделителям (Windows-клиенты присылают
/// `C:\dir\file.jpg`).
fn last_path_segment(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// Имя, когда портал его не прислал (аватары — всегда).
///
/// У вложения в запасное имя обязательно входит ИНДЕКС: иначе два файла одного
/// сообщения сохранились бы под одним именем.
fn fallback_name(key: &CacheKey, mime: Option<&str>) -> String {
    let ext = mime
        .and_then(mime_guess::get_mime_extensions_str)
        .and_then(|exts| exts.first().copied())
        .unwrap_or("bin");
    match key {
        CacheKey::File {
            message_id, index, ..
        } => format!("b24_{message_id}_{index}.{ext}"),
        CacheKey::Avatar { user_id } => format!("b24_avatar_{user_id}.{ext}"),
    }
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

/// Percent-encoding по RFC 5987 (attr-char).
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
// Дедуп inflight.
// ---------------------------------------------------------------------------

/// Снятие записи `media_inflight` — в том числе при панике и раннем `return`.
///
/// Без guard'а любой `?` в середине загрузки оставлял бы ключ в реестре
/// НАВСЕГДА: все последующие запросы этого медиа ждали бы [`INFLIGHT_WAIT`] и
/// получали `503`. `Drop` не умеет `await`, поэтому аварийное снятие уходит в
/// отдельную задачу; штатный путь зовёт [`InflightGuard::release`] сам, и тогда
/// `Drop` уже ничего не делает.
struct InflightGuard {
    /// Состояние плагина (реестр живёт в нём).
    state: Arc<B24State>,
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
async fn acquire(state: &Arc<B24State>, key: &CacheKey) -> Slot {
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
/// перечитывают кэш. Ожидание ограничено [`INFLIGHT_WAIT`], число круговых
/// попыток — [`MAX_INFLIGHT_ATTEMPTS`]: если владелец упал, второй запрос обязан
/// взяться сам, а не висеть до таймаута клиента.
///
/// Гонка «владелец закончил, пока мы шли к `await`» закрыта тем же приёмом, что
/// в [`crate::events`]: `notified()` включается ДО повторной проверки кэша —
/// `Notify::notify_waiters` не оставляет permit'ов.
///
/// # Errors
///
/// Ошибка загрузки владельца либо `503`, если после всех ожиданий файла в кэше
/// так и нет.
pub async fn fetch_cached<F, Fut>(
    state: &Arc<B24State>,
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
                // Повторная проверка ПОСЛЕ enable: владелец мог закончить в окне
                // между `acquire` и подпиской.
                if let Some(hit) = media_cache::get(&state.config, key).await {
                    return Ok(hit);
                }
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(INFLIGHT_WAIT) => {
                        tracing::warn!(
                            target: "forge_bitrix",
                            key = %key.id(),
                            "forge-bitrix: gave up waiting for a concurrent media download"
                        );
                    }
                }
            }
        }
    }
    // Ждали, но файла нет: качаем сами, не претендуя на ключ (владелец мог как
    // раз в этот момент его отпускать).
    download().await
}

// ---------------------------------------------------------------------------
// Стриминг.
// ---------------------------------------------------------------------------

/// Удаляет staging-файл, если загрузка не довела дело до `commit`.
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

/// Пишет тело ответа в файл кусками, рассылая кадры `download_progress`.
///
/// Возвращает `(сколько байт записано, MIME из заголовка ответа)`.
///
/// # Почему потоком, а не `bytes()`
///
/// Буферизация целиком означала бы пиковую память в размер файла НА КАЖДЫЙ
/// параллельный запрос. Здесь память ограничена размером куска, который отдаёт
/// `reqwest`, и не зависит от размера файла.
///
/// Финальный кадр прогресса отправляется ПОСЛЕ `fsync`: событие «готово» не
/// должно опережать данные.
async fn stream_to_file(
    state: &Arc<B24State>,
    client: &B24Client,
    key: &CacheKey,
    url: &str,
    staging: &FsPath,
) -> Result<(u64, Option<String>), ApiError> {
    let resp = client
        .get_stream(url)
        .await
        .map_err(|e| crate::api::error::to_api_error(&e, crate::dto::now_unix()))?;
    let total = resp.content_length();
    let mime = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_string())
        .filter(|v| !v.is_empty() && v != "application/octet-stream");

    let mut reporter = DownloadReporter::for_key(state.clone(), key);
    let mut file = tokio::fs::File::create(staging)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not open the staging file: {e}")))?;

    let mut stream = resp.bytes_stream();
    let mut received = 0u64;
    reporter.report(0, total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            // `without_url` обязателен: в тексте ошибки reqwest адрес есть
            // всегда, а у вложения это код вебхука.
            ApiError::not_connected(format!("the media stream broke: {}", e.without_url()))
        })?;
        file.write_all(&chunk)
            .await
            .map_err(|e| ApiError::not_connected(format!("could not write a media chunk: {e}")))?;
        received += chunk.len() as u64;
        reporter.report(received, total);
    }
    file.flush()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not flush the media file: {e}")))?;
    file.sync_all()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not fsync the media file: {e}")))?;
    reporter.finish(received, total.or(Some(received)));
    Ok((received, mime))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_key() -> CacheKey {
        CacheKey::parse_file("chat27", "113", "0").unwrap()
    }

    /// Имя файла приходит от собеседника — в заголовок оно попадает только
    /// вычищенным, а инъекция перевода строки и кавычек невозможна.
    #[test]
    fn content_disposition_sanitizes_hostile_names() {
        let d = content_disposition(Some("../../evil.sh"), &file_key(), None);
        assert!(d.contains("filename=\"evil.sh\""), "{d}");
        assert!(!d.contains(".."), "{d}");

        let d = content_disposition(Some("a\"; drop\r\nX-Evil: 1"), &file_key(), None);
        assert!(!d.contains('\r') && !d.contains('\n'), "{d}");
        assert_eq!(d.matches('"').count(), 2, "кавычек ровно две: {d}");

        // Кириллица теряться не должна — она уезжает в filename*.
        let d = content_disposition(Some("отчёт.txt"), &file_key(), None);
        assert!(d.contains("filename*=UTF-8''"), "{d}");
        assert!(d.contains("%D0%BE"), "{d}");
    }

    /// Запасное имя вложения обязано содержать ИНДЕКС: иначе два файла одного
    /// сообщения сохранились бы под одним именем.
    #[test]
    fn the_fallback_name_keeps_the_attachment_index() {
        let first = content_disposition(None, &CacheKey::parse_file("chat27", "113", "0").unwrap(), Some("image/png"));
        let second = content_disposition(None, &CacheKey::parse_file("chat27", "113", "1").unwrap(), Some("image/png"));
        assert_ne!(first, second, "{first} == {second}");
        assert!(first.contains("b24_113_0."), "{first}");

        let avatar = content_disposition(None, &CacheKey::parse_avatar("17").unwrap(), None);
        assert!(avatar.contains("b24_avatar_17."), "{avatar}");
    }

    /// Пустое и «пустое после чистки» имя не должно давать `filename=""`.
    #[test]
    fn empty_names_fall_back() {
        for name in [Some(""), Some("   "), Some("."), Some(".."), None] {
            let d = content_disposition(name, &file_key(), None);
            assert!(!d.contains("filename=\"\""), "{name:?} -> {d}");
        }
        // Имя из одних недопустимых символов превращается в «file».
        let d = content_disposition(Some("///"), &file_key(), None);
        assert!(d.contains("filename=\"b24_113_0."), "{d}");
    }

    /// MIME для карточки берётся по имени файла, а не выдумывается.
    #[test]
    fn mime_is_guessed_from_the_file_name() {
        let png = MediaDto {
            name: Some("shot.png".into()),
            ..MediaDto::default()
        };
        assert_eq!(guess_mime(&png).as_deref(), Some("image/png"));
        assert_eq!(guess_mime(&MediaDto::default()), None);
    }
}
