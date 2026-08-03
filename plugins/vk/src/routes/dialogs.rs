//! Роуты данных: беседы, история, отдельное сообщение, фильтры.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET  | `/api/vk/filters`                    | 4 серверных фильтра (замена телеграмных папок) |
//! | GET  | `/api/vk/dialogs`                    | страница списка бесед |
//! | GET  | `/api/vk/dialogs/:id/messages`       | страница истории |
//! | POST | `/api/vk/dialogs/:id/messages`       | отправка (Ф4.5) |
//! | GET  | `/api/vk/dialogs/:id/messages/:mid`  | одно сообщение |
//! | POST | `/api/vk/dialogs/:id/messages/:mid/button` | нажать кнопку сообщества по `{row, col}` (Ф9) |
//! | POST | `/api/vk/dialogs/:id/read`           | отметить прочитанным (Ф4.5) |
//! | POST | `/api/vk/dialogs/:id/typing`         | «печатает…» (Ф4.5) |
//!
//! # ПОРЯДОК ПРОВЕРОК — ДОСЛОВНО КАК У TELEGRAM
//!
//! ```text
//!   авторизация (409 not_authorized)
//!        ▼
//!   соединение (503 not_connected, ConnHealth + грейс 3 с)
//!        ▼
//!   резолв peer (404 unknown_dialog)
//!        ▼
//!   разбор параметров (400 invalid_input / 404 unknown_message)
//! ```
//!
//! Порядок — не стилистика. Разбор `:id` ДО авторизации позволил бы
//! неавторизованному клиенту прощупывать валидность идентификаторов по
//! различию ответов (404 против 409), то есть превратил бы роут в оракул. На
//! это есть тест `hostile_dialog_ids_hit_auth_before_any_parsing` в
//! [`super`], и ради него query-строка разбирается ВРУЧНУЮ внутри хендлера:
//! экстрактор `Query<T>` axum'а отработал бы ДО тела хендлера и отдал бы 400
//! на мусорный `?limit=abc` раньше, чем проверилась авторизация.
//!
//! # Почему `/filters` статичен
//!
//! У VK нет папок; их место занимает параметр `filter` метода
//! `messages.getConversations` с четырьмя допустимыми значениями. Состав
//! константен, поэтому роут не делает НИ ОДНОГО вызова VK и не проверяет
//! соединение — 503 на статичный ответ был бы враньём. Авторизацию он при этом
//! требует: список фильтров бессмысленен без аккаунта, а лишний открытый роут
//! в поверхности не нужен.
//!
//! Счётчиков в ответе нет намеренно: каждый счётчик — отдельный вызов VK, а
//! бюджет 3 rps; общий бейдж закрывает кадр `unread_total` (событие LongPoll
//! 80, Ф5).
//!
//! # Откуда берутся имена
//!
//! Каждый ответ `extended=1` приносит блоки `profiles`/`groups` — они
//! поглощаются [`crate::peers::PeerCache`] БЕСПЛАТНО. Отдельный `users.get`
//! делается только для тех, кого в ответе не оказалось, и пачкой. Неудача
//! резолва НЕ проваливает запрос: имя — украшение строки, и терять из-за него
//! страницу истории нельзя.
//!
//! # Про `clippy::result_large_err`
//!
//! [`ApiError`] крупнее телеграмного на три необязательных поля
//! (`captcha_sid`, `captcha_img`, `redirect_uri`) — они и есть контракт
//! captcha и валидации VK, ради которого фронту не нужен второй запрос. Из-за
//! них тип переваливает за порог линта. Заворачивать его в `Box` мы не
//! собираемся: это добавило бы аллокацию на КАЖДЫЙ отказ (а отказ — штатный
//! ответ роута: 409 без логина приходит на каждый запрос закрытой вкладки)
//! ради экономии стека на пути, который выполняется один раз за запрос.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::error::to_api_error;
use crate::api::methods::{clamp_limit, normalize_filter};
use crate::api::{VkClient, VkError};
use crate::dto::{self, DialogDto, MessageDto};
use crate::routes::error::ApiError;
use crate::state::VkState;

/// Сколько бесед отдаём, если клиент не попросил иначе.
pub const DEFAULT_DIALOGS: i64 = 40;

/// Сколько сообщений отдаём, если клиент не попросил иначе.
pub const DEFAULT_MESSAGES: i64 = 50;

/// Названия четырёх серверных фильтров.
///
/// Порядок и ключи обязаны совпадать с [`DIALOG_FILTERS`] — на это есть тест:
/// ключ, которого не знает `normalize_filter`, молча превратился бы в `all`, и
/// пункт колонки перестал бы что-либо фильтровать, оставаясь на экране.
pub const FILTER_TITLES: [(&str, &str); 4] = [
    ("all", "Все"),
    ("unread", "Непрочитанные"),
    ("important", "Важные"),
    ("unanswered", "Без ответа"),
];

/// Один пункт секции «директорий» в колонке бесед.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterDto {
    /// Значение параметра `filter` метода `messages.getConversations`.
    pub key: String,
    /// Подпись для UI.
    pub title: String,
}

/// Ответ `GET /api/vk/filters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiltersResp {
    /// Четыре серверных фильтра.
    pub filters: Vec<FilterDto>,
}

/// Ответ `GET /api/vk/dialogs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogsResp {
    /// Страница бесед.
    pub dialogs: Vec<DialogDto>,
    /// Есть ли что грузить дальше (по `count` из ответа VK).
    pub has_more: bool,
}

/// Ответ `GET /api/vk/dialogs/:id/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagesResp {
    /// Страница истории ПО ВОЗРАСТАНИЮ `id`.
    pub messages: Vec<MessageDto>,
    /// Заполнилась ли страница целиком (⇒ старее есть).
    pub has_more: bool,
}

/// Ответ роутов, отдающих одно сообщение.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageResp {
    /// Само сообщение.
    pub message: MessageDto,
}

/// Тело `POST /api/vk/dialogs/:id/messages`.
///
/// Поля `upload_id`/`upload_ids` объявлены УЖЕ СЕЙЧАС, хотя конвейер загрузки
/// приезжает в Ф8: контракт тела не должен меняться под фронтом в чужой фазе.
/// До Ф8 они отвергаются `400 invalid_input` — честнее, чем молча
/// проигнорировать прикреплённый файл и отправить одну подпись.
#[derive(Debug, Default, Deserialize)]
pub struct SendMessageReq {
    /// Текст сообщения.
    #[serde(default)]
    pub text: Option<String>,
    /// Синоним `text` (подпись к вложению) — фронт вправе называть поле так,
    /// как ему удобнее.
    #[serde(default)]
    pub caption: Option<String>,
    /// Один загруженный файл (Ф8).
    #[serde(default)]
    pub upload_id: Option<String>,
    /// Альбом из нескольких файлов (Ф8).
    #[serde(default)]
    pub upload_ids: Option<Vec<String>>,
    /// `conversation_message_id` сообщения, на которое отвечаем.
    ///
    /// ОСНОВНОЙ способ адресовать ответ: именно `cmid` приезжает во фронт
    /// внутри `reply_to` каждой цитаты, и именно он одинаков у всех
    /// участников беседы. Превращается в параметр `forward` метода
    /// `messages.send` (см. [`crate::api::methods::reply_forward_json`]).
    #[serde(default)]
    pub reply_to_cmid: Option<i64>,
    /// Идентификатор сообщения (глобальный `message.id`), на которое отвечаем.
    ///
    /// Оставлен ради полноты контракта: у сообщения из личного диалога, до
    /// которого фронт добрался без `cmid`, других адресов нет. При наличии
    /// [`SendMessageReq::reply_to_cmid`] игнорируется — точный адрес сильнее.
    #[serde(default)]
    pub reply_to_id: Option<i64>,
    /// `payload` нажатой текстовой кнопки бота (Ф9).
    #[serde(default)]
    pub payload: Option<String>,
    /// `captcha_sid` из предыдущего отказа VK.
    #[serde(default)]
    pub captcha_sid: Option<String>,
    /// Символы, которые ввёл пользователь.
    #[serde(default)]
    pub captcha_key: Option<String>,
}

/// Тело `POST /api/vk/dialogs/:id/messages/:mid/button`.
///
/// КООРДИНАТЫ, А НЕ PAYLOAD — и это решение о безопасности, а не о вкусе.
/// `payload` кнопки бота уходит сообществу как есть; принимай его роут из тела
/// запроса, любая вкладка (или XSS в соседней панели) могла бы отправить боту
/// произвольную команду от имени пользователя. Здесь клиент называет лишь
/// место кнопки в клавиатуре, а `label` и `payload` сервер достаёт из
/// перечитанного сообщения сам.
///
/// Поля `Option<i64>`, а не `u8`: отсутствующая координата обязана дать
/// внятный `400 invalid_input`, а не молча превратиться в `0` (то есть в
/// «нажать первую кнопку»).
#[derive(Debug, Default, Deserialize)]
pub struct PressButtonReq {
    /// Номер ряда клавиатуры (0-based).
    #[serde(default)]
    pub row: Option<i64>,
    /// Номер кнопки в ряду (0-based).
    #[serde(default)]
    pub col: Option<i64>,
    /// `captcha_sid` из предыдущего отказа VK.
    #[serde(default)]
    pub captcha_sid: Option<String>,
    /// Символы, которые ввёл пользователь.
    #[serde(default)]
    pub captcha_key: Option<String>,
}

/// Тело `POST /api/vk/dialogs/:id/read`.
#[derive(Debug, Default, Deserialize)]
pub struct ReadReq {
    /// До какого сообщения включительно считать прочитанным.
    #[serde(default)]
    pub max_id: Option<i64>,
    /// `captcha_sid` из предыдущего отказа.
    #[serde(default)]
    pub captcha_sid: Option<String>,
    /// Символы с картинки.
    #[serde(default)]
    pub captcha_key: Option<String>,
}

/// Тело `POST /api/vk/dialogs/:id/typing`.
#[derive(Debug, Default, Deserialize)]
pub struct TypingReq {
    /// `typing` (по умолчанию) либо `audiomessage`.
    #[serde(default)]
    pub action: Option<String>,
}

/// Ответ роутов, которым нечего вернуть, кроме факта успеха.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OkResp {
    /// Всегда `true` — отказ приезжает конвертом ошибки, а не `ok:false`.
    pub ok: bool,
}

/// Потолок длины сообщения, символов.
///
/// VK отвергает более длинное кодом 914. Проверяем сами, чтобы не тратить
/// запрос из бюджета 3 rps на заведомо отвергаемое тело — и чтобы
/// пользователь увидел причину мгновенно, а не через круг по сети.
pub const MAX_MESSAGE_CHARS: usize = 4096;

/// Роуты данных.
pub fn router() -> Router<Arc<VkState>> {
    Router::new()
        .route("/api/vk/filters", get(get_filters))
        .route("/api/vk/dialogs", get(get_dialogs))
        .route(
            "/api/vk/dialogs/:id/messages",
            get(get_messages).post(post_message),
        )
        // Имя параметра во ВСЕХ шаблонах одно и то же (`:id`, `:mid`): axum
        // паникует при сборке, если соседние роуты называют один сегмент
        // по-разному.
        .route("/api/vk/dialogs/:id/messages/:mid", get(get_message))
        // `/button`, а НЕ `/callback`: нажимаются только кнопки, которые VK
        // разрешает нажать пользовательским токеном, а callback-механики у
        // клиента нет вовсе (см. [`post_button`]).
        .route(
            "/api/vk/dialogs/:id/messages/:mid/button",
            post(post_button),
        )
        .route("/api/vk/dialogs/:id/read", post(post_read))
        .route("/api/vk/dialogs/:id/typing", post(post_typing))
}

// ---------------------------------------------------------------------------
// Лестница проверок.
// ---------------------------------------------------------------------------

/// Ступень 1: аккаунт привязан?
///
/// В сообщение попадает ИМЯ состояния автомата: фронту этого достаточно,
/// чтобы показать нужный экран логина, а серверу — чтобы не гадать по логам,
/// почему пользователь видит 409.
pub(crate) async fn require_authorized(state: &Arc<VkState>) -> Result<(), ApiError> {
    if state.is_authorized().await {
        return Ok(());
    }
    let kind = state.login.lock().await.kind();
    Err(ApiError::not_authorized(format!(
        "VK account is not linked (login state '{kind}')"
    )))
}

/// Ступень 2: связь есть?
///
/// У VK нет постоянного соединения, поэтому смысл 503 наполняет
/// [`crate::state::ConnHealth`]: отказ считается «связи нет» ровно
/// [`crate::state::HEALTH_GRACE_SECS`] секунд, после чего запросы снова
/// пропускаются — иначе чинить здоровье было бы нечем, все вызовы отвергались
/// бы на входе.
pub(crate) async fn require_connected(state: &Arc<VkState>) -> Result<(), ApiError> {
    let now = dto::now_unix();
    if state.connected(now).await {
        return Ok(());
    }
    let reason = state
        .down_reason(now)
        .await
        .unwrap_or_else(|| "the last call to api.vk.com failed".to_string());
    Err(ApiError::not_connected(format!(
        "no connection to VK right now: {reason}"
    )))
}

/// Клиент API текущего токена (один на токен ⇒ один троттлер).
pub(crate) async fn require_client(state: &Arc<VkState>) -> Result<Arc<VkClient>, ApiError> {
    state.api_client().await.map_err(|e| {
        tracing::warn!(target: "forge_vk", error = %e, "forge-vk: could not build the API client");
        ApiError::not_connected(format!("could not build the VK client: {e}"))
    })
}

/// Ступени 1-2 разом: именно в этом порядке и ни в каком другом.
pub(crate) async fn require_ready(state: &Arc<VkState>) -> Result<Arc<VkClient>, ApiError> {
    require_authorized(state).await?;
    require_connected(state).await?;
    require_client(state).await
}

/// Ступень 3: `:id` из URL → `peer_id`.
///
/// Мусорный идентификатор — это `404 unknown_dialog`, а не `400`: для фронта
/// исход тот же, что «беседа удалена», и обрабатывается он одной веткой.
pub(crate) fn resolve_peer(dialog_id: &str) -> Result<i64, ApiError> {
    dto::parse_dialog_id(dialog_id).ok_or_else(|| {
        ApiError::unknown_dialog(format!("'{dialog_id}' is not a valid VK dialog id"))
    })
}

/// `:mid` из URL → идентификатор сообщения.
///
/// `404 unknown_message`, а не `400`, по той же причине: удалённое сообщение и
/// сообщение с непарсящимся id для UI — одно и то же «недоступно».
/// Потолок — `i32::MAX`: идентификаторы сообщений у VK 32-битные, и значение
/// сверх него не может принадлежать сообщению вовсе. Ведущий `+` отвергается
/// по той же причине, что и в [`crate::dto::parse_dialog_id`]: два написания
/// одного id развели бы ключи кэша на фронте.
pub(crate) fn parse_message_id(mid: &str) -> Result<i64, ApiError> {
    let t = mid.trim();
    t.strip_prefix('+')
        .is_none()
        .then(|| t.parse::<i64>().ok())
        .flatten()
        .filter(|v| *v > 0 && *v <= i64::from(i32::MAX))
        .ok_or_else(|| ApiError::unknown_message(format!("'{mid}' is not a valid VK message id")))
}

/// Ошибка VK → ошибка REST, с запоминанием картинки captcha.
///
/// Запоминание обязательно и живёт именно здесь: `captcha_img` — адрес на
/// серверах VK, и браузер туда не ходит. Прокси `GET /api/vk/captcha/:sid`
/// умеет отдать картинку только по `sid`, который ищет в
/// [`crate::state::VkState::captcha_imgs`], — а наполнить эту карту может лишь
/// тот, кто первым увидел ответ VK.
pub(crate) async fn map_vk_error(state: &Arc<VkState>, err: &VkError) -> ApiError {
    if let Some((sid, img)) = err.captcha() {
        state.remember_captcha(sid, img).await;
    }
    to_api_error(err)
}

// ---------------------------------------------------------------------------
// Разбор query — ВРУЧНУЮ и ПОСЛЕ проверок.
// ---------------------------------------------------------------------------

/// Сырое значение параметра query-строки.
///
/// Без percent-декодирования намеренно: все наши параметры — числа и слова из
/// закрытого списка, а `filter` всё равно проходит через
/// [`normalize_filter`], который незнакомое значение приводит к `all`.
/// Декодер здесь означал бы новую зависимость ради случая, которого нет.
fn query_value<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    raw.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Числовой параметр query.
///
/// Пустое значение (`?limit=`) трактуется как отсутствие: браузеры и наш
/// собственный `URLSearchParams` умеют оставлять пустой хвост, и отвечать на
/// него 400 значило бы ломать вкладку на ровном месте. Всё остальное, что не
/// разбирается в число, — ошибка клиента.
///
/// # Errors
///
/// `400 invalid_input` — значение есть, но это не целое число.
fn query_i64(raw: Option<&str>, key: &str) -> Result<Option<i64>, ApiError> {
    let Some(v) = raw.and_then(|q| query_value(q, key)) else {
        return Ok(None);
    };
    if v.is_empty() {
        return Ok(None);
    }
    v.parse::<i64>()
        .map(Some)
        .map_err(|_| ApiError::bad_input(format!("`{key}` must be an integer, got '{v}'")))
}

// ---------------------------------------------------------------------------
// Хендлеры.
// ---------------------------------------------------------------------------

/// `GET /api/vk/filters` — ЗАМЕНА телеграмного `GET /folders`.
///
/// Ответ КОНСТАНТЕН и не стоит ни одного вызова VK (см. шапку модуля).
///
/// # Errors
///
/// `409 not_authorized` — аккаунт не привязан.
pub async fn get_filters(
    State(state): State<Arc<VkState>>,
) -> Result<Json<FiltersResp>, ApiError> {
    require_authorized(&state).await?;
    Ok(Json(FiltersResp {
        filters: FILTER_TITLES
            .iter()
            .map(|(key, title)| FilterDto {
                key: (*key).to_string(),
                title: (*title).to_string(),
            })
            .collect(),
    }))
}

/// `GET /api/vk/dialogs?limit&offset&filter` — страница списка бесед.
///
/// Один вызов `messages.getConversations(extended=1)`: вместе с беседами
/// приезжают последние сообщения И имена участников, поэтому страница стоит
/// ровно один запрос (плюс, редко, один пакетный `users.get` на тех, кого в
/// ответе не оказалось).
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `400 invalid_input` (мусор в
/// `limit`/`offset`), плюс всё, чем может ответить VK
/// ([`crate::api::error::to_api_error`]).
pub async fn get_dialogs(
    State(state): State<Arc<VkState>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<DialogsResp>, ApiError> {
    let client = require_ready(&state).await?;
    // ↓ разбор параметров — ПОСЛЕДНИЙ шаг лестницы.
    let q = raw.as_deref();
    let limit = clamp_limit(query_i64(q, "limit")?.unwrap_or(DEFAULT_DIALOGS));
    let offset = query_i64(q, "offset")?.unwrap_or(0).max(0);
    let filter = normalize_filter(q.and_then(|s| query_value(s, "filter")).unwrap_or("all"));

    let page = match client.get_conversations(limit, offset, filter).await {
        Ok(p) => p,
        Err(e) => return Err(map_vk_error(&state, &e).await),
    };

    // Имена — бесплатно, из того же ответа.
    state.peers.absorb_extended(&page.profiles, &page.groups);
    let mut wanted: Vec<i64> = Vec::with_capacity(page.items.len() * 2);
    for item in &page.items {
        wanted.push(item.conversation.peer.id);
        if let Some(last) = item.last_message.as_ref() {
            wanted.push(last.from_id);
        }
    }
    resolve_names(&state, &client, &wanted).await;

    let now = dto::now_unix();
    let dialogs: Vec<DialogDto> = page
        .items
        .iter()
        .filter_map(|item| {
            dto::dialog_from_raw(
                &item.conversation,
                item.last_message.as_ref(),
                &state.peers,
                now,
            )
        })
        .collect();

    // `has_more` считается по ЧИСЛУ ПРИШЕДШИХ бесед, а не по числу отданных:
    // `email`-беседы отфильтрованы проекцией, и если судить по результату,
    // страница с одной такой беседой навсегда объявила бы себя последней.
    let has_more = offset + (page.items.len() as i64) < page.count;

    tracing::debug!(
        target: "forge_vk",
        count = dialogs.len(),
        filter,
        offset,
        "forge-vk: dialogs listed"
    );
    Ok(Json(DialogsResp { dialogs, has_more }))
}

/// `GET /api/vk/dialogs/:id/messages?limit&before_id` — страница истории.
///
/// `before_id` — это `start_message_id` VK; сам он в ответ не попадает
/// (`offset=1`), иначе последнее сообщение предыдущей страницы дублировалось бы
/// в ленте.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`,
/// `400 invalid_input`, плюс ошибки VK.
pub async fn get_messages(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<MessagesResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let q = raw.as_deref();
    let limit = clamp_limit(query_i64(q, "limit")?.unwrap_or(DEFAULT_MESSAGES));
    // Ноль и отрицательное значат «с конца»: `?before_id=0` приезжает от
    // фронта, у которого лента ещё пуста, и отвечать на это 400 было бы
    // придиркой к нулевому значению счётчика.
    let before = query_i64(q, "before_id")?.filter(|v| *v > 0);

    let page = match client.get_history(peer, limit, before).await {
        Ok(p) => p,
        Err(e) => return Err(map_vk_error(&state, &e).await),
    };

    state.peers.absorb_extended(&page.profiles, &page.groups);
    let mut wanted: Vec<i64> = vec![peer];
    for m in &page.items {
        collect_author_ids(m, &mut wanted);
    }
    resolve_names(&state, &client, &wanted).await;

    let mut messages: Vec<MessageDto> = page
        .items
        .iter()
        .map(|m| dto::message_from_raw(m, &state.peers))
        .collect();
    // Контракт наружу — ВОЗРАСТАНИЕ id. VK отдаёт историю от новых к старым,
    // но сортировка (а не `reverse`) не даёт контракту зависеть от того, что
    // VK решит завтра.
    messages.sort_by_key(|m| m.id);
    let has_more = page.items.len() as i64 >= limit;

    Ok(Json(MessagesResp { messages, has_more }))
}

/// `GET /api/vk/dialogs/:id/messages/:mid` — одно сообщение.
///
/// # Почему 404 порождаем МЫ
///
/// `messages.getById` на чужой или удалённый идентификатор возвращает не
/// ошибку, а пустой `items: []`. Без явной проверки клиент получил бы `200` с
/// пустым телом — то есть «сообщение есть, но пустое» вместо «сообщения нет».
///
/// Принадлежность беседе проверяется отдельно: `getById` адресует сообщения
/// по глобальному id и с радостью отдаст сообщение из ЛЮБОГО диалога
/// аккаунта. Без сверки `peer_id` роут стал бы способом читать чужую беседу,
/// подставив её id в путь к открытой.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`,
/// `404 unknown_message`, плюс ошибки VK.
pub async fn get_message(
    State(state): State<Arc<VkState>>,
    Path((id, mid)): Path<(String, String)>,
) -> Result<Json<MessageResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let message_id = parse_message_id(&mid)?;

    let message = fetch_message(&state, &client, peer, message_id).await?;
    Ok(Json(MessageResp { message }))
}

/// `POST /api/vk/dialogs/:id/messages` — отправка текста и/или файлов.
///
/// # Здесь же живёт ВЕСЬ конвейер загрузки
///
/// `POST /api/vk/upload` кладёт байты только на наш диск: upload-сервер VK
/// выдаётся под КОНКРЕТНЫЙ `peer_id`, а на шаге `/upload` адресат ещё
/// неизвестен. Поэтому три шага (`getMessagesUploadServer` → `POST upload_url`
/// → `save`) выполняются ЗДЕСЬ, непосредственно перед `messages.send`, и
/// кадры `upload_progress` фронт получает во время ЭТОГО запроса, а не
/// предыдущего (см. [`crate::routes::upload`]).
///
/// # Идемпотентность
///
/// `random_id` генерируется РОВНО ОДИН РАЗ на логическую отправку и уезжает в
/// параметрах; транспорт при повторе шлёт те же параметры
/// ([`crate::api::VkClient::call_value`]), а VK дедуплицирует отправку по
/// `random_id` и возвращает ТОТ ЖЕ `message_id`. Благодаря этому обрыв
/// соединения в середине ответа не превращается в дубль сообщения у
/// собеседника — свойство, которого у Telegram нет вовсе.
///
/// # Почему после отправки идёт второй запрос
///
/// `messages.send` возвращает голое число. Фронту нужен ПОЛНЫЙ DTO — тот же,
/// что в ленте: optimistic-строка заменяется реальным сообщением, и объект
/// другой формы заставил бы её мигнуть. Поэтому `messages.getById` — не
/// перестраховка, а часть контракта отправки.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`,
/// `400 invalid_input` (пустое сообщение без вложений, слишком длинный текст,
/// больше [`crate::routes::upload::MAX_ATTACHMENTS`] файлов),
/// `409 invalid_state` (`upload_id` уже потрачен или протух),
/// `409 send_forbidden` (VK 900/901/902), `409 captcha_required` (VK 14).
pub async fn post_message(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    body: Option<Json<SendMessageReq>>,
) -> Result<Json<MessageResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    // Тело разбирается ПОСЛЕДНИМ: `Option<Json<T>>` не отвергает запрос сам, а
    // отдаёт `None` — иначе битый JSON получил бы 400 раньше авторизации.
    let req = body.map(|Json(v)| v).unwrap_or_default();

    let upload_ids = collect_upload_ids(&req);
    if upload_ids.len() > crate::routes::upload::MAX_ATTACHMENTS {
        return Err(ApiError::bad_input(format!(
            "{} attachments requested, VK allows at most {}",
            upload_ids.len(),
            crate::routes::upload::MAX_ATTACHMENTS
        )));
    }

    let text = pick_text(&req);
    // Пустой текст законен ТОЛЬКО с вложением: у VK это сообщение-фотография,
    // и требовать к ней подпись было бы выдумкой.
    if text.trim().is_empty() && upload_ids.is_empty() {
        return Err(ApiError::bad_input("the message text is empty"));
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(ApiError::bad_input(format!(
            "the message is too long: {} characters, the limit is {MAX_MESSAGE_CHARS}",
            text.chars().count()
        )));
    }

    // ↓ Загрузки ПОТРЕБЛЯЮТСЯ здесь: повторный запрос с тем же `upload_id`
    //   получит `409 invalid_state` вместо второго такого же сообщения.
    let staged = crate::routes::upload::take_uploads(&state, &upload_ids).await?;
    let attachment = if staged.is_empty() {
        None
    } else {
        // Три шага конвейера — ВНУТРИ этого запроса (см. шапку хендлера).
        let built = crate::routes::upload::build_attachment(&state, &client, peer, &staged).await;
        // Staging больше не нужен ни при каком исходе: при успехе байты уже в
        // VK, при отказе фронт начинает повтор с первого этапа (файл мог
        // протухнуть, а `upload_id` уже потрачен).
        for (_, entry) in &staged {
            crate::routes::upload::discard_staging(entry).await;
        }
        Some(built?)
    };

    // Цель ответа: `cmid` сильнее `message_id`. Обе формы одновременно VK не
    // принимает, а выбирать «что важнее» в момент отправки поздно — поэтому
    // выбор делается здесь и ровно один раз.
    let reply_cmid = req.reply_to_cmid.filter(|v| *v > 0);
    let body = crate::api::methods::SendBody {
        // Пустой текст в параметры не кладётся: `message=` у VK означает
        // «отправить пустую строку», а не «текста нет».
        message: Some(text).filter(|t| !t.is_empty()),
        attachment,
        reply_to: if reply_cmid.is_some() {
            None
        } else {
            req.reply_to_id.filter(|v| *v > 0)
        },
        forward: reply_cmid.map(|cmid| crate::api::methods::reply_forward_json(peer, cmid)),
        payload: req.payload.clone().filter(|p| !p.is_empty()),
        captcha: captcha_from(req.captcha_sid.clone(), req.captcha_key.clone()),
    };
    // ↓ ОДИН раз на логическую отправку. Пересоздание при повторе означало бы
    //   второе сообщение у собеседника.
    let random_id = crate::api::methods::new_random_id();

    let message_id = match client.send_message(peer, random_id, body).await {
        Ok(v) => v,
        Err(e) => return Err(map_vk_error(&state, &e).await),
    };

    // Сообщение отправлено; если добор полного DTO не удался, отдать 500 было
    // бы враньём — оно уже у собеседника. Возвращаем минимальный DTO с тем же
    // id, и фронт заменит optimistic-строку им.
    match fetch_message(&state, &client, peer, i64::from(message_id)).await {
        Ok(message) => Ok(Json(MessageResp { message })),
        Err(e) => {
            tracing::warn!(
                target: "forge_vk",
                message_id,
                error = %e.body.message,
                "forge-vk: message sent, but fetching it back failed"
            );
            Ok(Json(MessageResp {
                message: MessageDto {
                    id: message_id,
                    dialog_id: dto::dialog_id_string(peer),
                    out: true,
                    date: dto::now_unix(),
                    text: pick_text(&req),
                    ..MessageDto::default()
                },
            }))
        }
    }
}

/// `POST /api/vk/dialogs/:id/read` — отметить беседу прочитанной.
///
/// # Errors
///
/// Та же лестница; ошибки VK — как есть.
pub async fn post_read(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    body: Option<Json<ReadReq>>,
) -> Result<Json<OkResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();
    let captcha = captcha_from(req.captcha_sid.clone(), req.captcha_key.clone());

    match client
        .mark_as_read(peer, req.max_id.filter(|v| *v > 0), captcha.as_ref())
        .await
    {
        Ok(()) => Ok(Json(OkResp { ok: true })),
        Err(e) => Err(map_vk_error(&state, &e).await),
    }
}

/// `POST /api/vk/dialogs/:id/typing` — «печатает…».
///
/// VK держит статус около десяти секунд, поэтому фронт обязан ПОВТОРЯТЬ вызов
/// по таймеру, а не звать его на каждую букву: при лимите 3 rps набор текста
/// иначе съел бы весь бюджет запросов (троттлинг живёт в
/// `js/vk/format.js::typingAllowed`).
///
/// # Errors
///
/// Та же лестница; ошибки VK — как есть.
pub async fn post_typing(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    body: Option<Json<TypingReq>>,
) -> Result<Json<OkResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();
    // Незнакомый вид активности VK отвергает кодом 100; нормализация живёт в
    // транспорте, здесь достаточно передать как есть.
    let action = req.action.as_deref().unwrap_or("typing");

    match client.set_activity(peer, action).await {
        Ok(()) => Ok(Json(OkResp { ok: true })),
        Err(e) => Err(map_vk_error(&state, &e).await),
    }
}

/// `POST /api/vk/dialogs/:id/messages/:mid/button` — нажать кнопку сообщества.
///
/// # Почему роут называется `/button`, а не `/callback`
///
/// Нажать под ПОЛЬЗОВАТЕЛЬСКИМ токеном можно ровно один вид кнопок VK —
/// `text`, и «нажатие» это обычная отправка сообщения с подписью кнопки и её
/// `payload` (именно так работает официальный клиент). Никакой callback-ветки
/// здесь нет и быть не может, см. ниже.
///
/// # Почему callback нереализуем — ЗАФИКСИРОВАНО НАВСЕГДА
///
/// `messages.sendMessageEventAnswer` — это метод СТОРОНЫ СООБЩЕСТВА: им бот
/// ОТВЕЧАЕТ на уже полученный `message_event`, и он требует ключа доступа
/// сообщества. Обратной, клиентской половины — «отправить `message_event` от
/// имени пользователя» — в публичном API VK нет вовсе: ни в документации, ни в
/// клиентских библиотеках; в экосистеме этот сценарий закрывается только
/// группами. Прямого аналога телеграмному `messages.getBotCallbackAnswer`
/// у VK НЕ СУЩЕСТВУЕТ, и обходных путей мы не изобретаем — кнопка честно
/// отвечает `409 unsupported_button`, а UI рисует её отключённой с
/// пояснением. По той же причине из контракта ошибок выпал код `bot_timeout`
/// (504): ждать ответа бота здесь просто негде.
///
/// # Координаты вместо payload
///
/// Тело несёт только `{row, col}` ([`PressButtonReq`]). `label` и `payload`
/// сервер достаёт из СВЕЖЕ ПЕРЕЧИТАННОГО сообщения — поэтому клиент не может
/// отправить сообществу произвольную команду от имени пользователя, а
/// устаревшая клавиатура (бот успел её переписать) даёт честный отказ вместо
/// нажатия «не той» кнопки.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`,
/// `404 unknown_message`, `400 invalid_input` (координат нет, они за границами
/// либо клавиатура их уже не содержит; кнопка без подписи),
/// `409 unsupported_button` (вид кнопки нажать нельзя), `409 captcha_required`,
/// плюс всё, чем может ответить `messages.send`.
pub async fn post_button(
    State(state): State<Arc<VkState>>,
    Path((id, mid)): Path<(String, String)>,
    body: Option<Json<PressButtonReq>>,
) -> Result<Json<MessageResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let message_id = parse_message_id(&mid)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();
    let (row, col) = press_coords(&req)?;

    // Сырое сообщение, а не DTO: наружу `payload` не ездит вовсе, и взять его
    // можно только здесь.
    let raw = fetch_message_raw(&state, &client, peer, message_id).await?;
    let button = raw
        .keyboard
        .as_ref()
        .and_then(|kb| pick_button(kb, row, col))
        .ok_or_else(|| {
            ApiError::bad_input(format!(
                "message {message_id} has no button at row {row}, column {col}"
            ))
        })?;

    let kind = button.action.kind.trim();
    match plan_for(kind) {
        ButtonPlan::Unsupported => Err(ApiError::unsupported_button(format!(
            "ВКонтакте не позволяет нажать кнопку этого типа из стороннего клиента \
             (тип «{kind}»)"
        ))),
        // Сюда фронт не приходит: `open_link` он открывает сам. Но если
        // пришёл — отвечаем внятно, а не «ошибкой сервера».
        ButtonPlan::ClientSide => Err(ApiError::unsupported_button(
            "кнопка-ссылка открывается браузером на стороне клиента — \
             серверу нажимать нечего",
        )),
        ButtonPlan::Send => {
            let label = button.action.label.trim().to_string();
            if label.is_empty() {
                return Err(ApiError::bad_input(format!(
                    "the button at row {row}, column {col} has no label to send"
                )));
            }
            // ↓ ПОЛНАЯ подпись из сырого сообщения, а не обрезанная проекция:
            //   бот сверяет текст, и «Подтвердить зака…» ему ни о чём не
            //   говорит.
            let send_body = crate::api::methods::SendBody {
                message: Some(label),
                payload: Some(button.action.payload.clone()).filter(|p| !p.trim().is_empty()),
                captcha: captcha_from(req.captcha_sid.clone(), req.captcha_key.clone()),
                attachment: None,
                reply_to: None,
                forward: None,
            };
            // ОДИН random_id на логическое нажатие: повтор транспорта не имеет
            // права превратиться во второе сообщение сообществу.
            let random_id = crate::api::methods::new_random_id();
            let sent = match client.send_message(peer, random_id, send_body).await {
                Ok(v) => v,
                Err(e) => return Err(map_vk_error(&state, &e).await),
            };
            tracing::debug!(
                target: "forge_vk",
                peer,
                message_id,
                row,
                col,
                sent,
                "forge-vk: bot button pressed"
            );
            let message = fetch_message(&state, &client, peer, i64::from(sent)).await?;
            Ok(Json(MessageResp { message }))
        }
    }
}

// ---------------------------------------------------------------------------
// Кнопки сообществ — чистая логика.
// ---------------------------------------------------------------------------

/// Что делать с нажатой кнопкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonPlan {
    /// `text` — отправить подпись с `payload` обычным `messages.send`.
    Send,
    /// `open_link` — дело клиента: серверу нажимать нечего.
    ClientSide,
    /// Всё остальное: под пользовательским токеном нажать невозможно.
    Unsupported,
}

/// Вид кнопки → план действий.
///
/// Таблица поддержки одна на весь плагин — [`crate::dto::button_is_supported`]:
/// именно её флаг уезжает во фронт полем `supported`, и расхождение здесь дало
/// бы кнопку, которая рисуется активной, но всегда отвечает 409 (или наоборот).
pub(crate) fn plan_for(kind: &str) -> ButtonPlan {
    match kind {
        "text" => ButtonPlan::Send,
        "open_link" => ButtonPlan::ClientSide,
        _ => ButtonPlan::Unsupported,
    }
}

/// Координаты нажатия из тела запроса.
///
/// Отсутствующая координата — это `400`, а не «ноль»: тело без `row` означает
/// сломанного клиента, а «нажать первую кнопку вместо той, что просили» —
/// худший из возможных ответов на такую поломку.
///
/// Верхняя граница — те же потолки, по которым нарезана проекция
/// ([`crate::dto::MAX_KB_ROWS`]): координаты сверх них не могли приехать из
/// нашего же DTO.
fn press_coords(req: &PressButtonReq) -> Result<(usize, usize), ApiError> {
    let pick = |value: Option<i64>, name: &str, max: usize| -> Result<usize, ApiError> {
        let v = value.ok_or_else(|| ApiError::bad_input(format!("`{name}` is required")))?;
        usize::try_from(v)
            .ok()
            .filter(|v| *v < max)
            .ok_or_else(|| ApiError::bad_input(format!("`{name}` is out of range: {v}")))
    };
    let row = pick(req.row, "row", crate::dto::MAX_KB_ROWS)?;
    let col = pick(req.col, "col", crate::dto::MAX_KB_ROW_BUTTONS)?;
    Ok((row, col))
}

/// Кнопка сырой клавиатуры по координатам.
///
/// Координаты СЫРЫЕ: проекция ([`crate::dto::reply_markup_from_raw`]) их не
/// перенумеровывает, поэтому индекс из DTO адресует ту же кнопку и здесь.
fn pick_button(
    kb: &crate::api::schema::KeyboardRaw,
    row: usize,
    col: usize,
) -> Option<&crate::api::schema::KeyboardButtonRaw> {
    kb.buttons.get(row)?.get(col)
}

// ---------------------------------------------------------------------------
// Общее для хендлеров.
// ---------------------------------------------------------------------------

/// Текст сообщения: `text` либо `caption`, первое НЕПУСТОЕ.
///
/// Два имени одного поля — уступка фронту (у подписи к вложению своё название
/// в UI), а не признак двух разных смыслов: у VK текст и подпись живут в одном
/// параметре `message`.
pub(crate) fn pick_text(req: &SendMessageReq) -> String {
    for s in [req.text.as_deref(), req.caption.as_deref()]
        .into_iter()
        .flatten()
    {
        if !s.trim().is_empty() {
            return s.to_string();
        }
    }
    String::new()
}

/// Идентификаторы загрузок одного сообщения — из обоих полей тела, БЕЗ
/// дублей и с сохранением порядка.
///
/// Полей два намеренно: `upload_ids` — родная для VK форма (альбом из пяти фото
/// стоит трёх запросов вместо пятнадцати), а `upload_id` оставлен
/// односоставным синонимом, чтобы тело запроса совпадало с телеграмным и
/// `js/vk/upload.js` оставался диффабельным близнецом `js/telegram/upload.js`.
///
/// Дедупликация не косметическая: один и тот же `upload_id`, названный дважды,
/// после первого [`crate::state::VkState::take_upload`] превратился бы в
/// `409 invalid_state` на собственном же запросе.
pub(crate) fn collect_upload_ids(req: &SendMessageReq) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let single = req.upload_id.iter().cloned();
    let many = req.upload_ids.iter().flatten().cloned();
    for raw in single.chain(many) {
        let id = raw.trim().to_string();
        if id.is_empty() || out.contains(&id) {
            continue;
        }
        out.push(id);
    }
    out
}

/// Собирает ответ на captcha, если обе половины непустые.
///
/// Половина бесполезна: VK отвергнет запрос кодом 100, и пользователь увидит
/// «неверные параметры» вместо «введите символы ещё раз».
pub(crate) fn captcha_from(
    sid: Option<String>,
    key: Option<String>,
) -> Option<crate::api::methods::Captcha> {
    let sid = sid?.trim().to_string();
    let key = key?.trim().to_string();
    if sid.is_empty() || key.is_empty() {
        return None;
    }
    Some(crate::api::methods::Captcha { sid, key })
}

/// Одно сообщение по id, уже спроецированное.
///
/// Общая для `GET .../messages/:mid` и для отправки (Ф4.5): `messages.send`
/// возвращает только число, а фронту нужен полный DTO — тот же, что в ленте,
/// иначе optimistic-строка заменялась бы объектом другой формы.
///
/// # Errors
///
/// `404 unknown_message` — пустой `items` либо сообщение из другой беседы;
/// ошибки VK — как есть.
pub(crate) async fn fetch_message(
    state: &Arc<VkState>,
    client: &VkClient,
    peer: i64,
    message_id: i64,
) -> Result<MessageDto, ApiError> {
    let raw = fetch_message_raw(state, client, peer, message_id).await?;
    Ok(dto::message_from_raw(&raw, &state.peers))
}

/// То же самое, но СЫРЫМ объектом VK.
///
/// Нужен нажатию кнопки ([`post_button`]): `payload` кнопок в DTO не ездит по
/// решению контракта, и достать его можно только из сырого сообщения.
///
/// # Errors
///
/// `404 unknown_message` — пустой `items` либо сообщение из другой беседы;
/// ошибки VK — как есть.
pub(crate) async fn fetch_message_raw(
    state: &Arc<VkState>,
    client: &VkClient,
    peer: i64,
    message_id: i64,
) -> Result<crate::api::schema::MessageRaw, ApiError> {
    let page = match client.get_by_id(&[message_id]).await {
        Ok(p) => p,
        Err(e) => return Err(map_vk_error(state, &e).await),
    };

    state.peers.absorb_extended(&page.profiles, &page.groups);
    let raw = page
        .items
        .into_iter()
        // ↓ ИМЕННО ЗДЕСЬ рождается `unknown_message`: пустой `items` — это
        //   штатный ответ VK на чужой или удалённый id.
        .find(|m| m.peer_id == peer)
        .ok_or_else(|| {
            ApiError::unknown_message(format!(
                "message {message_id} is not available in dialog {peer}"
            ))
        })?;

    let mut wanted = vec![peer];
    collect_author_ids(&raw, &mut wanted);
    resolve_names(state, client, &wanted).await;

    Ok(raw)
}

/// Досылает недостающие имена, НЕ проваливая запрос при неудаче.
///
/// Имя — украшение строки; страница истории без имён читается, страница с
/// ошибкой вместо истории — нет.
pub(crate) async fn resolve_names(state: &Arc<VkState>, client: &VkClient, ids: &[i64]) {
    if let Err(e) = state.peers.resolve_uncached(client, ids).await {
        tracing::debug!(
            target: "forge_vk",
            error = %e,
            "forge-vk: some display names are left unresolved"
        );
    }
}

/// Собирает id всех, кого надо подписать в этом сообщении.
///
/// Автор, авторы пересылок и автор цитаты — по тем же границам глубины, что и
/// проекция ([`crate::dto::MAX_FWD_DEPTH`]): резолвить имена глубже, чем UI
/// собирается рисовать, значит платить запросами за невидимое.
fn collect_author_ids(msg: &crate::api::schema::MessageRaw, out: &mut Vec<i64>) {
    out.push(msg.from_id);
    if let Some(r) = msg.reply_message.as_ref() {
        out.push(r.from_id);
    }
    collect_nested_authors(&msg.fwd_messages, 0, out);
}

/// Авторы пересылок, вглубь до [`crate::dto::MAX_FWD_DEPTH`].
fn collect_nested_authors(
    list: &[crate::api::schema::NestedMessageRaw],
    depth: usize,
    out: &mut Vec<i64>,
) {
    if depth >= dto::MAX_FWD_DEPTH {
        return;
    }
    for m in list.iter().take(dto::MAX_FWD_PER_LEVEL) {
        out.push(m.from_id);
        collect_nested_authors(&m.fwd_messages, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::methods::DIALOG_FILTERS;

    /// Ключи фильтров обязаны совпадать с тем, что понимает транспорт: иначе
    /// пункт колонки остался бы на экране, молча перестав фильтровать.
    #[test]
    fn filter_titles_match_the_transport_table() {
        let keys: Vec<&str> = FILTER_TITLES.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, DIALOG_FILTERS.to_vec());
        for (key, title) in FILTER_TITLES {
            assert_eq!(normalize_filter(key), key);
            assert!(!title.is_empty(), "{key}");
        }
    }

    #[test]
    fn query_values_are_read_positionally() {
        let raw = "limit=25&offset=100&filter=unread";
        assert_eq!(query_value(raw, "limit"), Some("25"));
        assert_eq!(query_value(raw, "filter"), Some("unread"));
        assert_eq!(query_value(raw, "nope"), None);
        // Похожие имена не должны совпадать по префиксу.
        assert_eq!(query_value("limitless=1", "limit"), None);
    }

    #[test]
    fn numeric_query_params_reject_garbage_but_tolerate_emptiness() {
        assert_eq!(query_i64(Some("limit=25"), "limit").unwrap(), Some(25));
        assert_eq!(query_i64(Some("limit=-3"), "limit").unwrap(), Some(-3));
        assert_eq!(query_i64(None, "limit").unwrap(), None);
        assert_eq!(query_i64(Some("other=1"), "limit").unwrap(), None);
        assert_eq!(
            query_i64(Some("limit="), "limit").unwrap(),
            None,
            "пустой хвост URLSearchParams — не ошибка"
        );
        for garbage in ["limit=abc", "limit=1e3", "limit=2.5", "limit=99999999999999999999"] {
            let err = query_i64(Some(garbage), "limit").unwrap_err();
            assert_eq!(err.body.code, "invalid_input", "{garbage}");
        }
    }

    /// Границы страницы: ноль и отрицательное — опечатка клиента (⇒ 1), а не
    /// «по умолчанию»; десять тысяч зажимаются потолком.
    #[test]
    fn page_limits_are_clamped() {
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(-42), 1);
        assert_eq!(clamp_limit(1), 1);
        assert_eq!(clamp_limit(100), 100);
        assert_eq!(clamp_limit(10_000), crate::api::methods::MAX_DIALOGS);
        assert_eq!(clamp_limit(DEFAULT_DIALOGS), DEFAULT_DIALOGS);
        assert_eq!(clamp_limit(DEFAULT_MESSAGES), DEFAULT_MESSAGES);
    }

    /// `:mid` — только положительное 32-битное. Ноль у VK не существует, а
    /// значение за i32 не может принадлежать сообщению вовсе.
    #[test]
    fn message_id_parsing_is_strict() {
        assert_eq!(parse_message_id("7001").unwrap(), 7001);
        assert_eq!(parse_message_id(" 7001 ").unwrap(), 7001);
        for garbage in ["0", "-1", "abc", "", "2147483648", "7.5", "+7", "😀"] {
            let err = parse_message_id(garbage).unwrap_err();
            assert_eq!(err.body.code, "unknown_message", "{garbage}");
        }
    }

    /// Мусорный `:id` — 404 `unknown_dialog`, а не 400: для UI это тот же
    /// исход, что «беседа удалена».
    #[test]
    fn dialog_id_resolution_is_a_404() {
        assert_eq!(resolve_peer("2000000007").unwrap(), 2_000_000_007);
        assert_eq!(resolve_peer("-55").unwrap(), -55);
        for garbage in ["", "0", "../etc", "abc", "+1"] {
            let err = resolve_peer(garbage).unwrap_err();
            assert_eq!(err.body.code, "unknown_dialog", "{garbage}");
            assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
        }
    }

    /// `text` и `caption` — два имени одного поля; берётся первое НЕПУСТОЕ.
    #[test]
    fn text_is_picked_from_either_field() {
        let with = |text: Option<&str>, caption: Option<&str>| SendMessageReq {
            text: text.map(str::to_string),
            caption: caption.map(str::to_string),
            ..SendMessageReq::default()
        };
        assert_eq!(pick_text(&with(Some("а"), Some("б"))), "а");
        assert_eq!(pick_text(&with(None, Some("б"))), "б");
        assert_eq!(pick_text(&with(Some("   "), Some("б"))), "б", "пустой text не выигрывает");
        assert_eq!(pick_text(&with(None, None)), "");
        // Пробелы ВНУТРИ сохраняются: это текст пользователя.
        assert_eq!(pick_text(&with(Some("  привет  "), None)), "  привет  ");
    }

    /// Половина ответа на captcha бесполезна: VK отвергнет её кодом 100, а
    /// пользователь увидит «неверные параметры» вместо «введите ещё раз».
    #[test]
    fn captcha_requires_both_halves() {
        assert!(captcha_from(Some("239".into()), Some("abc".into())).is_some());
        assert!(captcha_from(Some("239".into()), None).is_none());
        assert!(captcha_from(None, Some("abc".into())).is_none());
        assert!(captcha_from(Some("  ".into()), Some("abc".into())).is_none());
        let c = captcha_from(Some(" 239 ".into()), Some(" abc ".into())).unwrap();
        assert_eq!((c.sid.as_str(), c.key.as_str()), ("239", "abc"));
    }

    /// План действий по виду кнопки обязан совпадать с таблицей поддержки,
    /// которая уезжает во фронт полем `supported`.
    ///
    /// Мутация «считать `callback` нажимаемым» краснеет здесь и в
    /// `dto::tests::button_kinds_map_to_the_support_table` одновременно.
    #[test]
    fn button_plan_matches_the_support_table() {
        assert_eq!(plan_for("text"), ButtonPlan::Send);
        assert_eq!(plan_for("open_link"), ButtonPlan::ClientSide);
        for kind in ["callback", "vkpay", "open_app", "location", "other", "", "TEXT"] {
            assert_eq!(plan_for(kind), ButtonPlan::Unsupported, "{kind}");
        }
        // Обе половины решения берутся из одного словаря: нажимаемое серверу
        // (Send) плюс клиентское (ClientSide) — ровно то, что помечено
        // `supported: true`.
        for kind in ["text", "open_link", "callback", "vkpay", "location", "zzz"] {
            let supported = crate::dto::button_is_supported(kind);
            assert_eq!(
                supported,
                plan_for(kind) != ButtonPlan::Unsupported,
                "{kind}"
            );
        }
    }

    /// Координаты обязательны и ограничены теми же потолками, по которым
    /// нарезана проекция: «нет координаты» не превращается в «нажать первую».
    #[test]
    fn press_coordinates_are_required_and_bounded() {
        let with = |row: Option<i64>, col: Option<i64>| PressButtonReq {
            row,
            col,
            ..PressButtonReq::default()
        };
        assert_eq!(press_coords(&with(Some(0), Some(0))).unwrap(), (0, 0));
        assert_eq!(press_coords(&with(Some(31), Some(11))).unwrap(), (31, 11));
        for (row, col) in [
            (None, Some(0)),
            (Some(0), None),
            (None, None),
            (Some(-1), Some(0)),
            (Some(0), Some(-1)),
            (Some(32), Some(0)),
            (Some(0), Some(12)),
            (Some(i64::MAX), Some(0)),
        ] {
            let err = press_coords(&with(row, col)).unwrap_err();
            assert_eq!(err.body.code, "invalid_input", "{row:?}/{col:?}");
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        }
    }

    /// Координаты адресуют СЫРУЮ матрицу; выход за неё — не паника и не
    /// «ближайшая кнопка», а `None` (то есть 400 в хендлере).
    #[test]
    fn button_lookup_is_bounded_by_the_raw_matrix() {
        use crate::api::schema::{KeyboardActionRaw, KeyboardButtonRaw, KeyboardRaw};

        let btn = |label: &str| KeyboardButtonRaw {
            color: String::new(),
            action: KeyboardActionRaw {
                kind: "text".into(),
                label: label.into(),
                ..KeyboardActionRaw::default()
            },
        };
        let kb = KeyboardRaw {
            buttons: vec![vec![btn("a"), btn("b")], vec![btn("c")]],
            ..KeyboardRaw::default()
        };
        assert_eq!(pick_button(&kb, 0, 1).unwrap().action.label, "b");
        assert_eq!(pick_button(&kb, 1, 0).unwrap().action.label, "c");
        assert!(pick_button(&kb, 1, 1).is_none(), "ряд короче");
        assert!(pick_button(&kb, 2, 0).is_none(), "рядов меньше");
        assert!(pick_button(&KeyboardRaw::default(), 0, 0).is_none());
    }

    /// Резолв имён не должен уходить глубже, чем рисует UI: платить запросами
    /// за невидимые уровни пересылок незачем.
    #[test]
    fn author_collection_respects_forward_limits() {
        use crate::api::schema::{MessageRaw, NestedMessageRaw};

        fn tree(depth: usize, width: usize, seed: i64) -> Vec<NestedMessageRaw> {
            if depth == 0 {
                return Vec::new();
            }
            (0..width)
                .map(|i| NestedMessageRaw {
                    from_id: seed + i as i64,
                    fwd_messages: tree(depth - 1, width, seed * 10),
                    ..NestedMessageRaw::default()
                })
                .collect()
        }

        let msg = MessageRaw {
            id: 1,
            peer_id: 1234,
            date: 1,
            from_id: 1234,
            reply_message: Some(Box::new(NestedMessageRaw {
                from_id: 999,
                ..NestedMessageRaw::default()
            })),
            fwd_messages: tree(6, 20, 100),
            ..MessageRaw::default()
        };

        let mut ids = Vec::new();
        collect_author_ids(&msg, &mut ids);
        assert!(ids.contains(&1234) && ids.contains(&999));
        // 1 автор + 1 цитата + 10 + 100 + 1000 уровней пересылок — и не больше.
        assert_eq!(ids.len(), 2 + 10 + 100 + 1000);
    }
}
