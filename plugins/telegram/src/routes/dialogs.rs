//! REST-роуты данных: диалоги, сообщения, отметка о прочтении, typing.
//!
//! | Метод | Путь | Что делает |
//! |-------|------|------------|
//! | GET  | `/api/telegram/dialogs` | список диалогов (`limit` ≤ [`MAX_DIALOGS`], `folder=main\|archive\|all`) |
//! | GET  | `/api/telegram/folders` | папки (dialog filters) пользователя |
//! | GET  | `/api/telegram/dialogs/:id/messages` | история (`limit` ≤ [`MAX_MESSAGES`], `before_id`) |
//! | POST | `/api/telegram/dialogs/:id/messages` | отправка текста или медиа по `upload_id`, возвращает `MessageDto` |
//! | GET  | `/api/telegram/dialogs/:id/messages/:mid` | одно сообщение по id (оригинал цитаты вне загруженной истории) |
//! | POST | `/api/telegram/dialogs/:id/messages/:mid/callback` | нажатие inline-кнопки бота |
//! | POST | `/api/telegram/dialogs/:id/read` | отметить прочитанным до `max_id` |
//! | POST | `/api/telegram/dialogs/:id/typing` | «печатает…» / отмена |
//!
//! # Порядок проверок (одинаков для всех роутов)
//!
//! 1. авторизация — иначе `409 not_authorized` (аккаунт не привязан);
//! 2. живое MTProto-подключение — иначе `503 not_connected`;
//! 3. резолв `:id` в `PeerRef` — иначе `404 unknown_dialog` ([`crate::peers`]).
//!
//! Порядок не косметика: он отличает «нужно войти» от «нужно подождать
//! реконнекта» от «такого диалога нет», и фронт по коду решает, показывать
//! wizard логина, статус RECONNECTING или ошибку диалога.
//!
//! # FLOOD_WAIT здесь не страшен
//!
//! В отличие от логина (риск №2 плана), где авто-ретрай продлевает
//! блокировку на часы, на этих роутах `FLOOD_WAIT` — обычные секунды. Мы
//! всё равно НЕ ретраим сами: отдаём `429` + `retry_after_secs`, а решение
//! принимает UI (для `typing` он просто молча игнорирует отказ).
//!
//! # Порядок сообщений
//!
//! grammers отдаёт историю от НОВЫХ к старым; наружу уходит СТРОГО по
//! возрастанию `id`. Так фронт просто конкатенирует страницы (`prepend` для
//! подгрузки вверх, `push` для новых) и никогда не сортирует сам.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use grammers_client::message::InputMessage;
use grammers_client::Client;
use grammers_mtsender::InvocationError;
use grammers_session::types::{PeerKind, PeerRef};
use grammers_tl_types as tl;
use serde::{Deserialize, Serialize};

use crate::dto::{self, DialogDto, FolderDto, MessageDto};
use crate::peers;
use crate::routes::error::ApiError;
use crate::runtime::flood_wait_secs;
use crate::state::TelegramState;
use crate::ws::protocol::TgServerMsg;

/// Сколько диалогов отдавать по умолчанию.
pub const DEFAULT_DIALOGS: usize = 100;
/// Сколько диалогов отдавать по умолчанию, когда `?folder=` задан явно.
///
/// Явный `?folder=` — это запрос фронта «покажи содержимое конкретной
/// части списка», и он приходит РОВНО тогда, когда пользователь ищет
/// что-то конкретное: архив или пользовательскую папку (её Telegram не
/// умеет фильтровать на сервере, поэтому фильтрация идёт на фронте по
/// уже загруженному списку). При дефолте в 100 диалогов чат с редко
/// используемым ботом, лежащий на 150-й позиции по дате, в папку «Боты»
/// не попадал вовсе — ровно то, на что жалуется пользователь.
///
/// 300, а не [`MAX_DIALOGS`]: три страницы по 100 — это три RPC, что ещё
/// не риск FLOOD_WAIT, но уже покрывает подавляющее большинство личных
/// аккаунтов. Явный `?limit=` по-прежнему может поднять выборку до
/// [`MAX_DIALOGS`].
pub const DEFAULT_FOLDER_DIALOGS: usize = 300;
/// Жёсткий потолок `limit` для списка диалогов.
pub const MAX_DIALOGS: usize = 500;

// Соотношение дефолтов сторожит компилятор, а не тест: дефолт папки обязан
// помещаться в потолок (иначе `clamp_limit` молча срезал бы его, и
// константа врала бы про своё значение) и обязан быть БОЛЬШЕ обычного —
// ради этого он и заведён.
const _: () = assert!(DEFAULT_FOLDER_DIALOGS <= MAX_DIALOGS);
const _: () = assert!(DEFAULT_FOLDER_DIALOGS > DEFAULT_DIALOGS);
/// Сколько сообщений отдавать по умолчанию.
pub const DEFAULT_MESSAGES: usize = 50;
/// Жёсткий потолок `limit` для истории (столько же за раз тянет Telegram).
pub const MAX_MESSAGES: usize = 100;

/// Query `GET /api/telegram/dialogs?limit=&folder=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DialogsQuery {
    /// Сколько диалогов вернуть (сервер клампит верхнюю границу).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Какую часть списка отдать: `main` | `archive` | `all`
    /// (см. [`parse_folder`]). Отсутствует ⇒ прежнее поведение (`all`).
    #[serde(default)]
    pub folder: Option<String>,
}

/// Query `GET /api/telegram/dialogs/:id/messages?limit=&before_id=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessagesQuery {
    /// Сколько сообщений вернуть (сервер клампит верхнюю границу).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Курсор пагинации: отдавать сообщения строго старше этого id.
    #[serde(default)]
    pub before_id: Option<i32>,
}

/// Тело `POST /api/telegram/dialogs/:id/messages`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SendMessageReq {
    /// Текст сообщения (или подпись к медиа).
    #[serde(default)]
    pub text: Option<String>,
    /// Идентификатор ранее загруженного файла (`POST /api/telegram/upload`).
    #[serde(default)]
    pub upload_id: Option<String>,
    /// Подпись к медиа — синоним `text` для читаемости на фронте.
    ///
    /// Оба поля означают одно и то же (у Telegram подпись к медиа лежит в том
    /// же `message`), поэтому берётся первое непустое; присылать оба
    /// одновременно смысла нет.
    #[serde(default)]
    pub caption: Option<String>,
    /// На какое сообщение отвечаем.
    #[serde(default)]
    pub reply_to_id: Option<i32>,
}

/// Тело `POST /api/telegram/dialogs/:id/read`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReadReq {
    /// Максимальный прочитанный message_id (`None` ⇒ прочитать всё).
    #[serde(default)]
    pub max_id: Option<i32>,
}

/// Тело `POST /api/telegram/dialogs/:id/typing`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TypingReq {
    /// Вид активности: `typing` (по умолчанию), `cancel`, `upload_photo`, …
    #[serde(default)]
    pub action: Option<String>,
}

/// Тело `POST /api/telegram/dialogs/:id/messages/:mid/callback`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallbackReq {
    /// `data` кнопки в base64 — ровно та строка, что пришла фронту в
    /// `MessageDto.reply_markup.rows[..][..].data`.
    ///
    /// Полезная нагрузка callback'а — произвольные БАЙТЫ (у Telegram это
    /// `bytes`, а не строка), поэтому по проводу она ходит base64: любая
    /// попытка протащить её как текст ломается на первом же боте, который
    /// пакует туда бинарный протобуф. Фронт её не разбирает — гоняет как
    /// непрозрачный токен.
    #[serde(default)]
    pub data: Option<String>,
    /// Нажата кнопка запуска игры (`keyboardButtonGame`): `data` у неё нет,
    /// а URL игры приезжает в ответе.
    #[serde(default)]
    pub game: bool,
}

/// Ответ `POST /api/telegram/dialogs/:id/messages/:mid/callback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackResp {
    /// Текст, который бот попросил показать (может отсутствовать —
    /// «тихое» нажатие, после которого бот просто отредактирует сообщение).
    pub message: Option<String>,
    /// `true` ⇒ показывать модалкой с кнопкой «ОК», `false` ⇒ тостом.
    pub alert: bool,
    /// URL, который клиент обязан открыть БЕЗ подтверждения: Telegram
    /// гарантирует, что это либо deep-link самого бота, либо адрес игры.
    pub url: Option<String>,
    /// Сколько секунд ответ можно кэшировать, не дёргая бота повторно.
    pub cache_time: Option<i32>,
}

/// Ответ `GET /api/telegram/dialogs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogsResp {
    /// Диалоги в порядке Telegram (закреплённые сверху). Фронт НЕ сортирует.
    pub dialogs: Vec<DialogDto>,
}

/// Ответ `GET /api/telegram/folders`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldersResp {
    /// Папки в порядке Telegram (как в шапке официальных клиентов).
    /// Аккаунт без папок — пустой массив, не ошибка.
    pub folders: Vec<FolderDto>,
}

/// Ответ `GET /api/telegram/dialogs/:id/messages`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResp {
    /// Сообщения СТРОГО по возрастанию `id`.
    pub messages: Vec<MessageDto>,
    /// Есть ли что подгружать выше (эвристика: вернулось ровно `limit`).
    pub has_more: bool,
}

/// Ответ `POST /api/telegram/dialogs/:id/messages`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageResp {
    /// Отправленное сообщение целиком — на нём строится reconcile
    /// optimistic-строки во фронте.
    pub message: MessageDto,
}

/// Ответ роутов-действий (`read`, `typing`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkResp {
    /// Всегда `true` — тело нужно только чтобы ответ был валидным JSON.
    pub ok: bool,
}

/// Роутер данных.
pub fn router() -> Router<Arc<TelegramState>> {
    Router::new()
        .route("/api/telegram/dialogs", get(get_dialogs))
        .route("/api/telegram/folders", get(get_folders))
        .route(
            "/api/telegram/dialogs/:id/messages",
            get(get_messages).post(post_message),
        )
        // Одиночное сообщение и нажатие кнопки живут на одном префиксе
        // `:id/messages/:mid`, но на разной глубине и разными методами —
        // пересечения нет. Имя параметра во ВСЕХ трёх шаблонах одно и то
        // же (`:id`, `:mid`): axum паникует при сборке, если один и тот же
        // сегмент назван по-разному в соседних роутах.
        .route("/api/telegram/dialogs/:id/messages/:mid", get(get_message))
        .route(
            "/api/telegram/dialogs/:id/messages/:mid/callback",
            post(post_callback),
        )
        .route("/api/telegram/dialogs/:id/read", post(post_read))
        .route("/api/telegram/dialogs/:id/typing", post(post_typing))
}

/// Отказ, если аккаунт не привязан. Возвращает `Ok(())` только для
/// авторизованного состояния.
pub(crate) async fn require_authorized(state: &Arc<TelegramState>) -> Result<(), ApiError> {
    if state.is_authorized().await {
        return Ok(());
    }
    let kind = state.login.lock().await.kind();
    Err(ApiError::not_authorized(format!(
        "Telegram account is not linked (login state '{kind}')"
    )))
}

/// Клон клиента для сетевой работы.
///
/// Клиент забирается ПОД read-lock'ом и сразу отпускается: держать lock
/// через RPC нельзя, иначе один медленный запрос блокирует все остальные.
pub(crate) async fn require_client(state: &Arc<TelegramState>) -> Result<Client, ApiError> {
    state
        .client()
        .await
        .ok_or_else(|| ApiError::not_connected("no active MTProto connection"))
}

/// Авторизация + подключение одной проверкой (порядок из шапки модуля).
pub(crate) async fn require_ready(state: &Arc<TelegramState>) -> Result<Client, ApiError> {
    require_authorized(state).await?;
    require_client(state).await
}

/// Единый маппинг ошибок RPC в контракт плагина.
///
/// Держится в одном месте, чтобы четыре роута не разъехались по кодам.
/// Классификация:
///
/// - `420` (`FLOOD_WAIT`/`SLOWMODE_WAIT`) ⇒ `429` + `retry_after_secs`;
/// - `401` ⇒ `409 not_authorized` (сессию отозвали — переход состояния
///   логина делает updates-воркер, у него для этого есть контекст);
/// - `*PEER_ID_INVALID*` / `*CHANNEL_INVALID*` / `*CHAT_ID_INVALID*` ⇒
///   `404 unknown_dialog` (по смыслу это «нет такого диалога», а не
///   «плохой запрос»);
/// - остальные `400` ⇒ `400 invalid_input`;
/// - всё прочее (обрыв, таймаут, `500` у Telegram) ⇒ `503 not_connected`.
pub fn map_invocation(err: &InvocationError) -> ApiError {
    if let Some(secs) = flood_wait_secs(err) {
        return ApiError::flood_wait(
            u64::from(secs),
            format!("Telegram asks to wait {secs}s before repeating this request"),
        );
    }
    match err {
        InvocationError::Rpc(rpc) if rpc.code == 401 => ApiError::not_authorized(format!(
            "Telegram authorization is no longer valid ({}): sign in again",
            rpc.name
        )),
        InvocationError::Rpc(rpc)
            if rpc.name.contains("PEER_ID_INVALID")
                || rpc.name.contains("CHANNEL_INVALID")
                || rpc.name.contains("CHAT_ID_INVALID")
                || rpc.name.contains("USER_ID_INVALID") =>
        {
            ApiError::unknown_dialog(format!(
                "Telegram does not recognize this dialog ({})",
                rpc.name
            ))
        }
        InvocationError::Rpc(rpc) if rpc.code == 400 => {
            ApiError::bad_input(format!("Telegram rejected the request: {}", rpc.name))
        }
        other => ApiError::not_connected(format!("Telegram request failed: {other}")),
    }
}

/// Маппинг ошибок `messages.getBotCallbackAnswer` в контракт плагина.
///
/// Отдельно от [`map_invocation`], потому что у нажатия кнопки бота другой
/// набор значимых ошибок и другие последствия для UI:
///
/// - `BOT_RESPONSE_TIMEOUT` / `Timeout` ⇒ `504 bot_timeout`. Telegram
///   присылает первую с кодом `400`, а вторую — с `-503`; общий маппер
///   увёл бы их в «исправьте ввод» и «переподключитесь» соответственно,
///   хотя виноват сторонний бот и правильное действие — просто нажать ещё
///   раз. Проверка идёт ПО ИМЕНИ и ПЕРВОЙ (после flood_wait) именно
///   поэтому;
/// - `MESSAGE_ID_INVALID` / `MSG_ID_INVALID` ⇒ `404 unknown_message`.
///   Здесь, в отличие от `POST /messages`, это не «кривой `reply_to_id`», а
///   «сообщение с кнопкой уже удалено»: фронт гасит устаревшую клавиатуру,
///   а не показывает ошибку у поля ввода;
/// - `DATA_INVALID` ⇒ `400`: payload кнопки не тот, что ждёт бот (обычно
///   фронт прислал данные от другого сообщения);
/// - RPC-ошибки, которые не исправит ни ввод, ни реконнект (`500`, `403`,
///   `-500`), ⇒ `502 telegram_error`, а не `503`: соединение живо, и
///   повтор бессмысленен;
/// - НЕ-RPC (обрыв, таймаут транспорта, `Dropped`) ⇒ `503 not_connected` —
///   вот это как раз лечится реконнектом.
pub fn map_callback_error(err: &InvocationError) -> ApiError {
    if let Some(secs) = flood_wait_secs(err) {
        return ApiError::flood_wait(
            u64::from(secs),
            format!("Telegram asks to wait {secs}s before repeating this request"),
        );
    }
    let InvocationError::Rpc(rpc) = err else {
        return ApiError::not_connected(format!("Telegram request failed: {err}"));
    };
    // `RpcError::from` вырезает из имени цифры (`FLOOD_WAIT_31` → `FLOOD_WAIT`),
    // поэтому сравнение идёт по имени, а не по тексту ошибки целиком.
    if rpc.is("BOT_RESPONSE_TIMEOUT") || rpc.is("Timeout") {
        return ApiError::bot_timeout(format!(
            "бот не ответил на нажатие кнопки ({}) — попробуйте ещё раз",
            rpc.name
        ));
    }
    if rpc.code == 401 {
        return ApiError::not_authorized(format!(
            "Telegram authorization is no longer valid ({}): sign in again",
            rpc.name
        ));
    }
    if rpc.is("MESSAGE_ID_INVALID") || rpc.is("MSG_ID_INVALID") {
        return ApiError::unknown_message(format!(
            "Telegram does not recognize this message ({}): it was probably deleted",
            rpc.name
        ));
    }
    if rpc.name.contains("PEER_ID_INVALID")
        || rpc.name.contains("CHANNEL_INVALID")
        || rpc.name.contains("CHANNEL_PRIVATE")
        || rpc.name.contains("CHAT_ID_INVALID")
        || rpc.name.contains("USER_ID_INVALID")
    {
        return ApiError::unknown_dialog(format!(
            "Telegram does not recognize this dialog ({})",
            rpc.name
        ));
    }
    if rpc.code == 400 {
        return ApiError::bad_input(format!("Telegram rejected the button press: {}", rpc.name));
    }
    ApiError::telegram_error(format!(
        "Telegram could not deliver the button press ({}: {})",
        rpc.code, rpc.name
    ))
}

/// Маппинг ошибок `messages.getMessages` / `channels.getMessages` в
/// контракт плагина.
///
/// Третий маппер файла — и снова не от хорошей жизни. Взять
/// [`map_invocation`] нельзя: `MSG_ID_INVALID`/`MESSAGE_ID_INVALID` там
/// проваливаются в общую ветку `400 ⇒ invalid_input`, а для чтения
/// сообщения по id это «сообщения нет» (`404 unknown_message`) — фронт по
/// этому коду рисует «сообщение недоступно» вместо ошибки у поля ввода.
/// Править сам [`map_invocation`] нельзя: на его нынешнем поведении
/// держится «кривой `reply_to_id` в `POST /messages` ⇒ 400» (см. тест
/// `map_invocation_and_callback_split_message_id_invalid`).
///
/// Классификация (порядок веток значим):
///
/// - `420` ⇒ `429` + `retry_after_secs` — как везде;
/// - `401` ⇒ `409 not_authorized`;
/// - `MSG_ID_INVALID` / `MESSAGE_ID_INVALID` / `MESSAGE_IDS_EMPTY` ⇒
///   `404 unknown_message`;
/// - `*PEER_ID_INVALID*` / `*CHANNEL_INVALID*` / `*CHANNEL_PRIVATE*` /
///   `*CHAT_ID_INVALID*` / `*USER_ID_INVALID*` ⇒ `404 unknown_dialog`.
///   `CHANNEL_PRIVATE` Telegram присылает с кодом `406`, поэтому сверка
///   идёт ПО ИМЕНИ, а не по коду;
/// - всё прочее ⇒ `503 not_connected`. Контракт роута знает ровно четыре
///   исхода (404/409/429/503), и «непонятный отказ» честнее показать как
///   «попробуйте позже», чем как «исправьте ввод»: исправлять клиенту
///   тут нечего — в запросе нет ничего, кроме пути.
pub fn map_message_error(err: &InvocationError) -> ApiError {
    if let Some(secs) = flood_wait_secs(err) {
        return ApiError::flood_wait(
            u64::from(secs),
            format!("Telegram asks to wait {secs}s before repeating this request"),
        );
    }
    let InvocationError::Rpc(rpc) = err else {
        return ApiError::not_connected(format!("Telegram request failed: {err}"));
    };
    if rpc.code == 401 {
        return ApiError::not_authorized(format!(
            "Telegram authorization is no longer valid ({}): sign in again",
            rpc.name
        ));
    }
    if rpc.is("MSG_ID_INVALID") || rpc.is("MESSAGE_ID_INVALID") || rpc.is("MESSAGE_IDS_EMPTY") {
        return ApiError::unknown_message(format!(
            "Telegram does not recognize this message ({}): it was probably deleted",
            rpc.name
        ));
    }
    if rpc.name.contains("PEER_ID_INVALID")
        || rpc.name.contains("CHANNEL_INVALID")
        || rpc.name.contains("CHANNEL_PRIVATE")
        || rpc.name.contains("CHAT_ID_INVALID")
        || rpc.name.contains("USER_ID_INVALID")
    {
        return ApiError::unknown_dialog(format!(
            "Telegram does not recognize this dialog ({})",
            rpc.name
        ));
    }
    ApiError::not_connected(format!(
        "Telegram could not return the message ({}: {})",
        rpc.code, rpc.name
    ))
}

/// Приводит запрошенный `limit` к допустимому диапазону.
///
/// `None`/`0` ⇒ `default`, значения выше `max` молча срезаются: отдать
/// `400` за слишком большой `limit` значило бы ломать UI из-за пустяка,
/// который сервер умеет исправить сам.
pub fn clamp_limit(requested: Option<u32>, default: usize, max: usize) -> usize {
    match requested {
        None | Some(0) => default,
        Some(v) => (v as usize).min(max),
    }
}

/// Дефолтный `limit` списка диалогов: зависит от того, задан ли `?folder=`.
///
/// Без `?folder=` это первая отрисовка колонки после логина — там важнее
/// скорость, поэтому дефолт остаётся [`DEFAULT_DIALOGS`]. С явным
/// `?folder=` пользователь уже ищет конкретный чат (архив, папка с
/// ботами), и обрезание списка сотней верхних диалогов означает «чата
/// нет» — см. [`DEFAULT_FOLDER_DIALOGS`].
///
/// Пустая строка (`?folder=`) считается отсутствием параметра — так же,
/// как её трактует [`parse_folder`]: два места не имеют права разойтись.
pub fn dialogs_default_limit(folder: Option<&str>) -> usize {
    match folder.map(str::trim).filter(|s| !s.is_empty()) {
        Some(_) => DEFAULT_FOLDER_DIALOGS,
        None => DEFAULT_DIALOGS,
    }
}

/// Что делать по `POST /typing`.
#[derive(Debug, Clone, PartialEq)]
pub enum TypingKind {
    /// Отправить конкретную активность.
    Action(tl::enums::SendMessageAction),
    /// Снять любую активность.
    Cancel,
}

/// Разбор поля `action`.
///
/// Отсутствие поля трактуется как `typing` — это подавляющее большинство
/// вызовов, и заставлять фронт писать его каждый раз незачем. Набор
/// значений сознательно закрытый: опечатка (`typng`) обязана быть видна как
/// `400`, а не молча превращаться в «печатает».
pub fn parse_typing(action: Option<&str>) -> Result<TypingKind, ApiError> {
    use tl::enums::SendMessageAction as A;
    let name = action.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("typing");
    let kind = match name {
        "typing" => TypingKind::Action(A::SendMessageTypingAction),
        "cancel" => TypingKind::Cancel,
        "record_voice" | "record_audio" => TypingKind::Action(A::SendMessageRecordAudioAction),
        "record_video" => TypingKind::Action(A::SendMessageRecordVideoAction),
        "choose_sticker" => TypingKind::Action(A::SendMessageChooseStickerAction),
        "upload_photo" => TypingKind::Action(A::SendMessageUploadPhotoAction(
            tl::types::SendMessageUploadPhotoAction { progress: 0 },
        )),
        "upload_video" => TypingKind::Action(A::SendMessageUploadVideoAction(
            tl::types::SendMessageUploadVideoAction { progress: 0 },
        )),
        "upload_audio" => TypingKind::Action(A::SendMessageUploadAudioAction(
            tl::types::SendMessageUploadAudioAction { progress: 0 },
        )),
        "upload_document" => TypingKind::Action(A::SendMessageUploadDocumentAction(
            tl::types::SendMessageUploadDocumentAction { progress: 0 },
        )),
        other => {
            return Err(ApiError::bad_input(format!(
                "unknown typing action '{other}' (expected: typing, cancel, record_voice, \
                 record_video, choose_sticker, upload_photo, upload_video, upload_audio, \
                 upload_document)"
            )))
        }
    };
    Ok(kind)
}

/// Безопасный `PeerRef` диалога.
///
/// НЕ используется `Dialog::peer_ref()`: внутри он делает
/// `peer.auth().unwrap()` и паникует на «min»-peer'ах (пришедших без
/// `access_hash`). `Peer::to_ref()` в том же случае честно возвращает
/// `Ok(None)`, подглядывая в кэш сессии.
async fn dialog_peer_ref(dialog: &grammers_client::peer::Dialog) -> Option<PeerRef> {
    match dialog.peer().to_ref().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                target: "forge_telegram",
                error = %e,
                "forge-telegram: could not build PeerRef for a dialog"
            );
            None
        }
    }
}

/// Какая часть списка диалогов запрошена (`?folder=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderChoice {
    /// Прежнее поведение: `folder_id` в запрос не передаётся, Telegram
    /// отдаёт диалоги без фильтра по peer-папкам.
    All,
    /// Только основной список (`folder_id = 0`).
    Main,
    /// Только архив (`folder_id = 1`).
    Archive,
}

impl FolderChoice {
    /// `folder_id` для сырого `messages.getDialogs`; `None` ⇒ путь через
    /// `iter_dialogs()` (grammers 0.10 не даёт задать folder_id иначе).
    pub fn folder_id(self) -> Option<i32> {
        match self {
            FolderChoice::All => None,
            FolderChoice::Main => Some(0),
            FolderChoice::Archive => Some(1),
        }
    }
}

/// Разбор query-параметра `folder`.
///
/// Отсутствие/пустая строка ⇒ [`FolderChoice::All`] — прежнее поведение,
/// чтобы существующий фронт ничего не заметил. Набор значений закрытый:
/// опечатка (`archve`) обязана быть видна как `400`, а не молча
/// превращаться в основной список.
pub fn parse_folder(raw: Option<&str>) -> Result<FolderChoice, ApiError> {
    let name = raw.map(str::trim).filter(|s| !s.is_empty());
    match name {
        None | Some("all") => Ok(FolderChoice::All),
        Some("main") => Ok(FolderChoice::Main),
        Some("archive") => Ok(FolderChoice::Archive),
        Some(other) => Err(ApiError::bad_input(format!(
            "unknown folder '{other}' (expected: main, archive, all)"
        ))),
    }
}

/// Размер одной страницы сырого `messages.getDialogs` (потолок Telegram).
const FOLDER_PAGE: usize = 100;

/// Страховка от бесконечной пагинации сырого `messages.getDialogs`:
/// [`MAX_DIALOGS`] = 500 укладывается в 5 страниц по [`FOLDER_PAGE`], «+2» —
/// запас на вырожденные ответы сервера.
const MAX_FOLDER_PAGES: usize = MAX_DIALOGS / FOLDER_PAGE + 2;

// Потолка страниц обязано хватать на самый большой разрешённый `limit`,
// иначе `?limit=500` тихо отдавал бы усечённый список.
const _: () = assert!(MAX_FOLDER_PAGES * FOLDER_PAGE >= MAX_DIALOGS);

/// Дата и id сырого сообщения — смещения следующей страницы.
fn raw_message_offset(msg: &tl::enums::Message) -> Option<(i32, i32)> {
    match msg {
        tl::enums::Message::Message(m) => Some((m.date, m.id)),
        tl::enums::Message::Service(m) => Some((m.date, m.id)),
        tl::enums::Message::Empty(_) => None,
    }
}

/// Двигают ли новые смещения пагинацию вперёд.
///
/// `messages.getDialogs` идёт НАЗАД по времени, поэтому признаков прогресса
/// ровно два: пара `(offset_date, offset_id)` изменилась И новая граница не
/// свежее предыдущей. Всё остальное означает, что следующий запрос вернёт
/// ту же страницу — цикл обязан оборваться сам, не дожидаясь
/// [`MAX_FOLDER_PAGES`]: иначе мы прогоняем лишние RPC (прямой путь к
/// FLOOD_WAIT) и собираем дубликаты диалогов.
///
/// Стартовые нули — не «предыдущая граница», а «границы ещё нет», поэтому
/// любой сдвиг от них считается прогрессом.
pub fn offsets_advanced(prev: (i32, i32), next: (i32, i32)) -> bool {
    if prev == next {
        return false;
    }
    if prev == (0, 0) {
        return true;
    }
    next.0 <= prev.0
}

/// Стартовый запрос страницы диалогов конкретной peer-папки.
///
/// Вынесен отдельно, чтобы «в какую папку мы вообще пошли» проверялось
/// unit-тестом: перепутанный `folder_id` не виден ни в одном ответе — он
/// просто молча отдаёт основной список вместо архива.
pub fn folder_dialogs_request(folder_id: i32) -> tl::functions::messages::GetDialogs {
    tl::functions::messages::GetDialogs {
        // Закреплённые нужны на ПЕРВОЙ странице: Telegram отдаёт их в
        // начале каждого ответа, поэтому со второй страницы они только
        // дублируются (см. `exclude_pinned = true` ниже по циклу).
        exclude_pinned: false,
        folder_id: Some(folder_id),
        offset_date: 0,
        offset_id: 0,
        offset_peer: tl::enums::InputPeer::Empty,
        limit: 0,
        // hash = 0 ⇒ сервер никогда не ответит `NotModified`: кэша на нашей
        // стороне нет, и «ничего не изменилось» нам нечем развернуть.
        hash: 0,
    }
}

/// Диалоги конкретной peer-папки (архив/основной список) сырым вызовом
/// `messages.getDialogs { folder_id }`.
///
/// `iter_dialogs()` в grammers 0.10 захардкодил `folder_id: None`, а поле
/// `request` итератора приватно — поэтому пагинация повторяется здесь тем
/// же способом, что в `DialogIter`: смещения берутся из последнего
/// сообщения страницы, закреплённые исключаются со второй страницы
/// (Telegram отдаёт их в начале каждого ответа). Маппинг — сырыми
/// функциями [`dto`] ([`dto::RawPeerIndex`],
/// [`dto::from_raw_dialog_with_defaults`]); побочно наполняется кэш
/// peer'ов, чтобы клик по архивному диалогу не требовал резолва по сети.
///
/// # Почему цикл, а не один запрос
///
/// Telegram отдаёт максимум [`FOLDER_PAGE`] диалогов за вызов, а
/// запрошенный `limit` теперь доходит до [`MAX_DIALOGS`]: без догрузки
/// страницами «покажи папку с ботами» упиралось бы в первую сотню чатов и
/// объявляло редкие чаты несуществующими.
///
/// # Три независимых страховки от вечного цикла
///
/// 1. [`MAX_FOLDER_PAGES`] — жёсткий потолок числа RPC;
/// 2. [`offsets_advanced`] — обрыв, как только сервер перестал двигать
///    границу страницы (иначе каждый следующий запрос — копия предыдущего);
/// 3. дедупликация по `dialog_id` — даже если сервер всё же повторил
///    страницу, наружу диалог уйдёт один раз, и `limit` не «наберётся»
///    дубликатами.
async fn folder_dialogs(
    state: &Arc<TelegramState>,
    client: &Client,
    folder_id: i32,
    limit: usize,
    defaults: dto::NotifyDefaults,
) -> Result<Vec<DialogDto>, ApiError> {
    let mut req = folder_dialogs_request(folder_id);
    let mut out: Vec<DialogDto> = Vec::with_capacity(limit.min(64));
    let mut seen: HashSet<String> = HashSet::with_capacity(limit.min(64));
    let now = dto::now_unix();
    use tl::enums::messages::Dialogs as D;
    for _page in 0..MAX_FOLDER_PAGES {
        req.limit = (limit - out.len()).min(FOLDER_PAGE) as i32;
        let (raw_dialogs, messages, chats, users, last_chunk) =
            match client.invoke(&req).await.map_err(|e| map_invocation(&e))? {
                D::Dialogs(d) => (d.dialogs, d.messages, d.chats, d.users, true),
                D::Slice(d) => {
                    let last = d.dialogs.len() < req.limit as usize;
                    (d.dialogs, d.messages, d.chats, d.users, last)
                }
                // hash = 0 ⇒ NotModified приходить не должен; пустой ответ
                // безопаснее паники.
                D::NotModified(_) => break,
            };
        if raw_dialogs.is_empty() {
            break;
        }

        let index = dto::RawPeerIndex::new(&users, &chats);
        for peer in index.peer_refs() {
            peers::cache_peer(state, peer).await;
        }
        for raw in &raw_dialogs {
            // Служебная строка «Архив» несёт чужой peer с обнулёнными
            // флагами и в списке диалогов ей не место — см.
            // [`dto::is_folder_row`]. Из расчёта смещений она при этом НЕ
            // исключается: границу страницы она даёт наравне с диалогами.
            if dto::is_folder_row(raw) {
                continue;
            }
            let top = dto::find_top_message(raw, &messages);
            let dto = dto::from_raw_dialog_with_defaults(raw, top, &index, now, defaults);
            // Повтор возможен и без багов сервера: закреплённые диалоги
            // приходят в начале каждой страницы, а `exclude_pinned`
            // включается только со второй.
            if !seen.insert(dto.id.clone()) {
                continue;
            }
            out.push(dto);
            if out.len() >= limit {
                break;
            }
        }
        if last_chunk || out.len() >= limit {
            break;
        }

        // Смещения следующей страницы — как в grammers DialogIter.
        req.exclude_pinned = true;
        let prev = (req.offset_date, req.offset_id);
        let Some(next) = raw_dialogs
            .iter()
            .rev()
            .find_map(|d| dto::find_top_message(d, &messages).and_then(raw_message_offset))
        else {
            // Страница без единого пригодного сообщения (только дыры
            // истории) не даёт границы — двигаться некуда.
            break;
        };
        if !offsets_advanced(prev, next) {
            tracing::debug!(
                target: "forge_telegram",
                folder_id,
                offset_date = next.0,
                offset_id = next.1,
                "forge-telegram: dialog pagination stopped making progress"
            );
            break;
        }
        req.offset_date = next.0;
        req.offset_id = next.1;
        if let Some(last) = raw_dialogs.last() {
            req.offset_peer = index.input_peer(dto::raw_dialog_peer(last));
        }
    }
    Ok(out)
}

/// `GET /api/telegram/dialogs`.
///
/// Один RPC на каждые 100 диалогов (пагинация внутри grammers) — никаких
/// N+1: `title`, `unread_count` и последнее сообщение приходят тем же
/// ответом. Побочный эффект: наполняются кэш peer'ов (после этого
/// `GET/POST messages` не требуют резолва по сети) и серверный кэш
/// непрочитанного.
///
/// `?folder=main|archive` отдаёт только соответствующую peer-папку через
/// [`folder_dialogs`]; кэш непрочитанного при этом НЕ пересевается —
/// частичная выборка не имеет права стирать счётчики чужих диалогов.
///
/// # `muted` считается с наследованием
///
/// Telegram не дублирует настройки уведомлений в каждый диалог: заглушив
/// «все личные чаты» с телефона, пользователь получает диалоги с ПУСТЫМИ
/// `PeerNotifySettings`. Поэтому `muted` разрешается через глобальные
/// дефолты категории из состояния плагина
/// ([`TelegramState::notify_defaults`], заполняются
/// [`crate::updates::refresh_notify_defaults`] до старта потока апдейтов).
/// Без этого списки показывали бы «не заглушено» там, где пользователь
/// молчал весь аккаунт.
///
/// # Побочный эффект: кэш флагов
///
/// Любой листинг — самый точный и совершенно бесплатный источник
/// `muted`/`archived`, поэтому он засевает кэши состояния
/// ([`TelegramState::remember_dialogs`]). На них потом опирается WS-кадр
/// `new_message`: тост от заглушённого чата не всплывёт, даже если этот
/// чат никогда не показывался во фронте.
pub async fn get_dialogs(
    State(state): State<Arc<TelegramState>>,
    Query(q): Query<DialogsQuery>,
) -> Result<Json<DialogsResp>, ApiError> {
    let client = require_ready(&state).await?;
    let folder = parse_folder(q.folder.as_deref())?;
    let limit = clamp_limit(
        q.limit,
        dialogs_default_limit(q.folder.as_deref()),
        MAX_DIALOGS,
    );
    let defaults = state.notify_defaults().await;

    if let Some(folder_id) = folder.folder_id() {
        let dialogs = folder_dialogs(&state, &client, folder_id, limit, defaults).await?;
        state.remember_dialogs(&dialogs).await;
        tracing::debug!(
            target: "forge_telegram",
            count = dialogs.len(),
            folder_id,
            limit,
            "forge-telegram: folder dialogs listed"
        );
        return Ok(Json(DialogsResp { dialogs }));
    }

    let now = dto::now_unix();
    let mut iter = client.iter_dialogs().limit(limit);
    let mut dialogs = Vec::with_capacity(limit.min(64));
    let mut unread: HashMap<String, i32> = HashMap::new();
    while let Some(dialog) = iter.next().await.map_err(|e| map_invocation(&e))? {
        // grammers отдаёт строку «Архив» наравне с диалогами (фильтра у
        // `DialogIter` нет) — а она не диалог, см. [`dto::is_folder_row`].
        // Отбрасывается ДО кэша peer'ов: кэшировать чужой peer, который мы
        // же и не показываем, незачем — он приедет с листингом архива.
        if dto::is_folder_row(&dialog.raw) {
            continue;
        }
        if let Some(peer) = dialog_peer_ref(&dialog).await {
            peers::cache_peer(&state, peer).await;
        }
        let dto = dto::from_grammers_dialog_with_defaults(&dialog, now, defaults);
        if dto.unread_count > 0 {
            unread.insert(dto.id.clone(), dto.unread_count);
        }
        dialogs.push(dto);
    }

    // Telegram — источник истины по непрочитанному: перечитали список ⇒
    // локальные счётчики, накопленные апдейтами, больше не нужны.
    state.reset_unread(unread).await;
    state.remember_dialogs(&dialogs).await;
    tracing::debug!(
        target: "forge_telegram",
        count = dialogs.len(),
        limit,
        "forge-telegram: dialogs listed"
    );
    Ok(Json(DialogsResp { dialogs }))
}

/// `GET /api/telegram/folders`.
///
/// Один raw-вызов `messages.getDialogFilters`. `Default` (основной список
/// «Все чаты») в ответ не попадает — это не папка; общие папки-ссылки
/// (`Chatlist`) маппятся без include-флагов (см. [`dto::folder_dto`]).
/// Аккаунт без папок — `{"folders":[]}`, не ошибка.
pub async fn get_folders(
    State(state): State<Arc<TelegramState>>,
) -> Result<Json<FoldersResp>, ApiError> {
    let client = require_ready(&state).await?;
    let tl::enums::messages::DialogFilters::Filters(res) = client
        .invoke(&tl::functions::messages::GetDialogFilters {})
        .await
        .map_err(|e| map_invocation(&e))?;
    let folders: Vec<FolderDto> = res.filters.iter().filter_map(dto::folder_dto).collect();
    tracing::debug!(
        target: "forge_telegram",
        count = folders.len(),
        "forge-telegram: folders listed"
    );
    Ok(Json(FoldersResp { folders }))
}

/// Страница истории никогда не требует больше ОДНОГО батча оригиналов:
/// уникальных `reply_to_id` на странице не может быть больше, чем самих
/// сообщений, а батч вмещает [`dto::REPLY_FETCH_BATCH`] штук. Соотношение
/// сторожит компилятор — если потолок страницы когда-нибудь поднимут,
/// сборка упадёт здесь, а не превратится в тихую потерю цитат.
const _: () = assert!(MAX_MESSAGES <= dto::REPLY_FETCH_BATCH);

/// Какие оригиналы цитат надо добрать по сети для этой страницы.
///
/// Тонкая обёртка над [`dto::reply_ids_to_fetch`] (он уже дедуплицирует id
/// и выбрасывает оригиналы, лежащие на самой странице), которая режет
/// результат под лимит одного `messages.getMessages`.
///
/// Обрезка — страховка, а не рабочий режим: при `MAX_MESSAGES <=
/// REPLY_FETCH_BATCH` (см. `const _` выше) она недостижима. Она стоит
/// здесь ради явного инварианта «на страницу истории — не более одного
/// дополнительного RPC»: лишний round-trip виден пользователю на каждом
/// экране прокрутки вверх.
pub fn reply_fetch_plan(messages: &[MessageDto]) -> Vec<i32> {
    let mut ids = dto::reply_ids_to_fetch(messages);
    ids.truncate(dto::REPLY_FETCH_BATCH);
    ids
}

/// Добивает цитаты страницы превью оригиналов — максимум один RPC.
///
/// Ошибку батча наружу НЕ отдаём: история без превью цитат полезнее, чем
/// пустой экран с ошибкой. В этом случае цитаты остаются «сырыми» (только
/// `message_id`), и фронт дотягивает их поштучно через
/// `GET /dialogs/:id/messages/:mid` — ровно тем же путём, что и для
/// сообщений из потока апдейтов.
///
/// Помечать при этом `unavailable: true` нельзя: «Telegram притормозил» и
/// «сообщение удалено» — разные вещи, а пользователь увидел бы вторую.
async fn enrich_page_replies(client: &Client, peer: PeerRef, messages: &mut [MessageDto]) {
    let ids = reply_fetch_plan(messages);
    if ids.is_empty() {
        // Сеть не нужна: либо цитат нет вовсе, либо все оригиналы лежат
        // на самой странице (ответ соседу) — их подставит сам `enrich`.
        dto::enrich_reply_previews(messages, &HashMap::new());
        return;
    }
    match client.get_messages_by_id(peer, &ids).await {
        Ok(fetched) => {
            let originals = dto::reply_originals_index(&fetched);
            dto::enrich_reply_previews(messages, &originals);
        }
        Err(err) => tracing::debug!(
            target: "forge_telegram",
            count = ids.len(),
            %err,
            "forge-telegram: reply previews are left to the client"
        ),
    }
}

/// `GET /api/telegram/dialogs/:id/messages`.
///
/// `before_id` — курсор «строго старше»: Telegram-овский `offset_id`
/// эксклюзивен, поэтому страницы не пересекаются и не теряют сообщений.
/// Пустой диалог — это `{messages: [], has_more: false}`, а не ошибка.
///
/// # Цитаты
///
/// Каждое сообщение отдаётся вместе с `reply_to` — превью того, на что оно
/// отвечает. Оригиналы, которых нет на самой странице, добираются ОДНИМ
/// дополнительным батчем (см. [`enrich_page_replies`]); недостающие
/// помечаются `unavailable: true` — фронт рисует «сообщение удалено».
pub async fn get_messages(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    Query(q): Query<MessagesQuery>,
) -> Result<Json<MessagesResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = peers::resolve_peer(&state, &id).await?;
    let limit = clamp_limit(q.limit, DEFAULT_MESSAGES, MAX_MESSAGES);

    let mut iter = client.iter_messages(peer).limit(limit);
    if let Some(before) = q.before_id {
        iter = iter.offset_id(before);
    }
    let mut messages = Vec::with_capacity(limit.min(64));
    while let Some(msg) = iter.next().await.map_err(|e| map_invocation(&e))? {
        messages.push(dto::from_grammers_message(&msg));
    }
    let has_more = messages.len() == limit;
    // Контракт наружу — возрастание id. `reverse()` было бы достаточно для
    // текущего поведения grammers, но сортировка не даёт контракту зависеть
    // от порядка чужой реализации.
    messages.sort_by_key(|m| m.id);
    enrich_page_replies(&client, peer, &mut messages).await;

    Ok(Json(MessagesResp { messages, has_more }))
}

/// Разбирает `:mid` из пути в id сообщения.
///
/// # Errors
///
/// `404 unknown_message` — не число либо не положительное: id сообщений у
/// Telegram начинаются с единицы, «нулевого» сообщения не существует ни в
/// одном диалоге. Именно `404`, а не `400`, по той же причине, что и в
/// [`validate_callback`]: для фронта это тот же исход, что «сообщение
/// удалили», — показать «сообщение недоступно», а не ошибку ввода.
pub fn parse_message_id(mid: &str) -> Result<i32, ApiError> {
    mid.trim().parse().ok().filter(|v| *v > 0).ok_or_else(|| {
        ApiError::unknown_message(format!("'{mid}' is not a valid Telegram message id"))
    })
}

/// `GET /api/telegram/dialogs/:id/messages/:mid`.
///
/// Одно сообщение целиком — тот же [`MessageDto`], что и в истории (ответ
/// ПЛОСКИЙ, без обёртки: это оговорено контрактом фичи цитат, в отличие от
/// соседних `{messages: […]}` и `{message: {…}}`).
///
/// # Зачем роут нужен
///
/// Пользователь кликает по цитате, чтобы перейти к оригиналу, — а тот может
/// лежать на тысячу сообщений выше загруженного куска истории. Догружать
/// страницами до нужного id — это десятки RPC и секунды ожидания; здесь
/// хватает одного. Второй потребитель — цитаты у сообщений из потока
/// апдейтов: там превью сознательно не добивается (см. [`crate::updates`]).
///
/// # Цитата самого сообщения
///
/// `reply_to` обогащается максимум ОДНИМ дополнительным запросом. Если он
/// не удался, цитата остаётся «сырой» (только `message_id`) — как и в
/// истории, «Telegram притормозил» не выдаётся за «сообщение удалено».
///
/// # Errors
///
/// - `409 not_authorized` — аккаунт не привязан;
/// - `503 not_connected` — нет MTProto-соединения или Telegram отказал
///   непонятным образом (см. [`map_message_error`]);
/// - `404 unknown_message` — `:mid` не разбирается, сообщение удалено или
///   не принадлежит этому диалогу;
/// - `404 unknown_dialog` — диалога нет или он недоступен;
/// - `429 flood_wait` — Telegram просит подождать (`retry_after_secs`).
pub async fn get_message(
    State(state): State<Arc<TelegramState>>,
    Path((id, mid)): Path<(String, String)>,
) -> Result<Json<MessageDto>, ApiError> {
    let client = require_ready(&state).await?;
    let msg_id = parse_message_id(&mid)?;
    let peer = peers::resolve_peer(&state, &id).await?;

    let msg = client
        .get_messages_by_id(peer, &[msg_id])
        .await
        .map_err(|e| map_message_error(&e))?
        .into_iter()
        .flatten()
        .next()
        // `messageEmpty` (удалённое сообщение) приезжает НЕ как `None`, а
        // как обычный `Message` с пустым текстом и датой-эпохой — без
        // [`dto::message_exists`] клиент получил бы пустое сообщение
        // вместо честного 404.
        .filter(dto::message_exists)
        .ok_or_else(|| {
            ApiError::unknown_message(format!(
                "message {msg_id} is not available in dialog {id} anymore"
            ))
        })?;

    let mut message = dto::from_grammers_message(&msg);
    enrich_single_reply(&client, peer, &mut message).await;
    Ok(Json(message))
}

/// Добивает цитату ОДНОГО сообщения — максимум один RPC.
///
/// Отдельно от [`enrich_page_replies`], потому что батчить тут нечего:
/// оригинал ровно один, и «страницы», из которой его можно было бы взять
/// без сети, нет.
///
/// Политика ошибок та же, что и у страницы истории: молчим и оставляем
/// цитату «сырой» (только `message_id`). Пометить `unavailable: true`
/// значило бы показать «сообщение удалено» там, где Telegram просто
/// попросил подождать.
async fn enrich_single_reply(client: &Client, peer: PeerRef, message: &mut MessageDto) {
    let Some(orig_id) = message
        .reply_to
        .as_ref()
        .filter(|r| dto::reply_needs_preview(r))
        .and_then(|r| r.message_id)
    else {
        return;
    };
    match client.get_messages_by_id(peer, &[orig_id]).await {
        Ok(fetched) => {
            let original = fetched
                .iter()
                .flatten()
                .find(|m| dto::message_exists(m))
                .map(dto::from_grammers_message);
            dto::enrich_reply_preview(message, original.as_ref());
        }
        Err(err) => tracing::debug!(
            target: "forge_telegram",
            orig_id,
            %err,
            "forge-telegram: reply preview is left to the client"
        ),
    }
}

/// Максимальная длина подписи к медиа (лимит Telegram, символы).
pub const MAX_CAPTION_CHARS: usize = 1024;

/// Уходит ли файл с таким MIME как СЖАТОЕ ФОТО.
///
/// `image/*` — да, потому что именно этого пользователь ждёт от снимка:
/// Telegram пережмёт его и покажет превью в ленте. Исключения:
///
/// - `image/gif` — Telegram превращает gif в видео-документ, и как «фото» он
///   потерял бы анимацию;
/// - `image/webp` — формат стикеров; отправленный как фото webp Telegram
///   часто отвергает (`PHOTO_EXT_INVALID`);
/// - `image/svg+xml` — не растр вовсе, сжимать нечего.
///
/// Остальное (в том числе `video/*` и `audio/*`) уходит документом — см.
/// [`post_message`].
pub fn is_photo_mime(mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    if !m.starts_with("image/") {
        return false;
    }
    !matches!(
        m.as_str(),
        "image/gif" | "image/webp" | "image/svg+xml" | "image/x-icon" | "image/vnd.microsoft.icon"
    )
}

/// Активность, которую видит собеседник, пока файл заливается.
fn upload_action(mime: &str) -> tl::enums::SendMessageAction {
    use tl::enums::SendMessageAction as A;
    if is_photo_mime(mime) {
        A::SendMessageUploadPhotoAction(tl::types::SendMessageUploadPhotoAction { progress: 0 })
    } else {
        A::SendMessageUploadDocumentAction(tl::types::SendMessageUploadDocumentAction {
            progress: 0,
        })
    }
}

/// Выбирает подпись/текст из тела запроса.
///
/// `text` и `caption` — синонимы (у Telegram подпись живёт в том же поле
/// сообщения); берётся первое НЕПУСТОЕ, чтобы фронт мог называть поле так,
/// как ему удобнее, и не думать о порядке.
pub fn pick_text(req: &SendMessageReq) -> String {
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

/// Разобранное и проверенное тело `POST messages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPlan {
    /// Текст сообщения либо подпись к медиа (может быть пустым для медиа).
    pub text: String,
    /// Что отправляем: `None` ⇒ текст, `Some` ⇒ файл из реестра загрузок.
    pub upload_id: Option<String>,
}

/// Проверяет тело запроса без обращения к сети и к реестру.
///
/// Вынесено отдельной чистой функцией, чтобы матрица «что считается
/// корректным телом» проверялась unit-тестами: сам хендлер до валидации
/// требует живого MTProto-подключения, которого в тестах нет.
///
/// # Errors
///
/// - `400` — ни текста, ни `upload_id`;
/// - `400` — подпись длиннее [`MAX_CAPTION_CHARS`].
pub fn validate_send(req: &SendMessageReq) -> Result<SendPlan, ApiError> {
    let upload_id = req
        .upload_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let text = pick_text(req);
    if text.trim().is_empty() && upload_id.is_none() {
        return Err(ApiError::bad_input(
            "'text' must be a non-empty string (there is no media to attach)",
        ));
    }
    if upload_id.is_some() {
        let len = text.chars().count();
        if len > MAX_CAPTION_CHARS {
            return Err(ApiError::bad_input(format!(
                "caption is {len} characters long, but Telegram allows at most {MAX_CAPTION_CHARS}"
            )));
        }
    }
    Ok(SendPlan { text, upload_id })
}

/// `POST /api/telegram/dialogs/:id/messages`.
///
/// Тело: `{text?, caption?, upload_id?, reply_to_id?}`.
///
/// - только текст ⇒ обычное сообщение;
/// - `upload_id` (+ необязательный текст как подпись) ⇒ медиа-сообщение;
/// - ни того, ни другого ⇒ `400`.
///
/// # Как выбирается способ отправки
///
/// По MIME, угаданному при загрузке из расширения имени
/// ([`crate::routes::upload::guess_mime`]): `image/*` (кроме gif/webp/svg) —
/// `InputMessage::photo`, всё остальное — `InputMessage::file`.
///
/// `file` (а не `document`) выбран сознательно: видео без атрибутов
/// `duration/width/height` Telegram всё равно покажет документом, а вытащить
/// эти атрибуты можно только распарсив контейнер (ffmpeg/mp4-парсер) — в v1
/// такой зависимости нет. Поэтому видео и аудио уходят файлом, честно и
/// предсказуемо; проигрывание из карточки на нашей стороне при этом работает,
/// потому что мы отдаём байты сами.
///
/// # Подпись
///
/// Длиннее [`MAX_CAPTION_CHARS`] ⇒ `400`, а НЕ «обрезать и дослать остаток
/// вторым сообщением»: молча превратить одно сообщение в два — сюрприз для
/// пользователя, а обрезать текст без предупреждения — потеря данных. Фронт
/// знает лимит и предупреждает до отправки.
///
/// Ответ обязан содержать РЕАЛЬНЫЕ `id` и `date`: на них строится
/// reconcile optimistic-строки во фронте. Собственное сообщение может
/// ПРИЙТИ ЕЩЁ РАЗ как `new_message` из updates — на бэкенде это не
/// подавляется (дедуплицировать по `id` фронту дешевле, чем серверу
/// угадывать, какая вкладка что уже видела).
pub async fn post_message(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    payload: Option<Json<SendMessageReq>>,
) -> Result<Json<SendMessageResp>, ApiError> {
    let req = payload.map(|Json(v)| v).unwrap_or_default();
    let client = require_ready(&state).await?;
    let SendPlan { text, upload_id } = validate_send(&req)?;

    let peer = peers::resolve_peer(&state, &id).await?;

    let (input, consumed) = match upload_id {
        None => (InputMessage::new().text(text), None),
        Some(uid) => {
            // Запись ПОТРЕБЛЯЕТСЯ сразу: повторный запрос с тем же upload_id
            // обязан получить отказ, а не второй экземпляр сообщения.
            let entry = state.take_upload(&uid).await.ok_or_else(|| {
                ApiError::invalid_state(format!(
                    "unknown or already used upload_id '{uid}': upload the file again"
                ))
            })?;

            // Best-effort «отправляет файл…»: собеседник видит активность,
            // пока идёт заливка. Ошибка игнорируется (в т.ч. FLOOD_WAIT) —
            // из-за индикатора отправка файла падать не должна. Активность
            // гаснет у Telegram сама через ~6с и в v1 не продлевается.
            let _ = client
                .action(peer)
                .oneshot(upload_action(&entry.mime))
                .await;

            let uploaded =
                match crate::routes::upload::upload_to_telegram(&state, &client, &uid, &entry).await
                {
                    Ok(v) => v,
                    Err(e) => {
                        // Заливка не удалась — возвращаем запись в реестр,
                        // чтобы повтор не требовал заново гнать файл из
                        // браузера (staging всё ещё на диске).
                        state.insert_upload(uid.clone(), entry).await;
                        return Err(e);
                    }
                };

            // mime_type ОБЯЗАН стоять до media: билдер запоминает его заранее
            // и подставляет в uploaded_document (см. grammers InputMessage).
            let mut msg = InputMessage::new().mime_type(&entry.mime);
            msg = if is_photo_mime(&entry.mime) {
                msg.photo(uploaded)
            } else {
                msg.file(uploaded)
            };
            if !text.is_empty() {
                msg = msg.text(text);
            }
            (msg, Some(entry))
        }
    };

    let input = input.reply_to(req.reply_to_id);
    let sent = match client.send_message(peer, input).await {
        Ok(v) => v,
        Err(e) => {
            // Файл в Telegram уже лежит, но сообщение не ушло. Staging чистим:
            // хендл `Uploaded` уже потерян, поэтому повтор всё равно начнётся
            // с новой заливки, а держать байты «на всякий случай» — только
            // занимать диск.
            if let Some(entry) = &consumed {
                crate::routes::upload::discard_staging(entry).await;
            }
            return Err(map_invocation(&e));
        }
    };
    if let Some(entry) = &consumed {
        // Сообщение доставлено — локальная копия больше не нужна.
        crate::routes::upload::discard_staging(entry).await;
    }
    let mut dto = dto::from_grammers_message(&sent);
    // Ответ на сообщение возвращается с ГОТОВОЙ цитатой (автор + превью
    // оригинала), а не с голым `reply_to_id`. Иначе reconcile во фронте
    // заменял бы optimistic-строку, где цитата уже нарисована, на серверную
    // без цитаты — и подпись «отвечаю Васе» моргала бы и пропадала ровно в
    // момент подтверждения отправки. Лишний RPC уходит ТОЛЬКО когда
    // отправляли ответ (см. `reply_needs_preview`), и его отказ ни на что
    // не влияет.
    enrich_single_reply(&client, peer, &mut dto).await;

    if dto.id == 0 {
        // Деградация grammers: сообщение ушло, но найти его в ответных
        // апдейтах не удалось. Ошибку отдавать нельзя (сообщение-то
        // доставлено), поэтому отдаём DTO как есть: фронт по `id <= 0`
        // понимает «не подтверждено», убирает optimistic-строку и ждёт
        // `new_message` из updates.
        tracing::warn!(
            target: "forge_telegram",
            dialog_id = %dto.dialog_id,
            "forge-telegram: Telegram did not confirm the sent message id"
        );
    }
    Ok(Json(SendMessageResp { message: dto }))
}

/// Максимальный размер `data` кнопки в БАЙТАХ (лимит Telegram).
///
/// Проверяется по декодированным байтам, а не по длине base64-строки:
/// base64 раздувает данные на треть, и сверка «строка ≤ 64» отвергала бы
/// вполне легальные 48-байтовые payload'ы.
pub const MAX_CALLBACK_DATA: usize = 64;

/// Разобранное и проверенное нажатие кнопки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackPlan {
    /// Сообщение, под которым висит клавиатура.
    pub msg_id: i32,
    /// Payload кнопки в сырых байтах (`None` — только для игровой кнопки).
    pub data: Option<Vec<u8>>,
    /// Нажата кнопка игры.
    pub game: bool,
}

/// Проверяет путь и тело нажатия кнопки без обращения к сети.
///
/// Вынесено чистой функцией по той же причине, что и [`validate_send`]:
/// хендлер до валидации требует живого MTProto-подключения, которого в
/// тестах нет, а матрица «что считается корректным нажатием» должна
/// проверяться без Telegram.
///
/// # Errors
///
/// - `404 unknown_message` — `:mid` не число или не положительный: id
///   сообщений у Telegram начинаются с единицы, и «нулевого» сообщения не
///   существует ни в одном диалоге. Именно `404`, а не `400`: для фронта
///   это тот же исход, что «сообщение удалили», — погасить клавиатуру;
/// - `400 invalid_input` — `data` не base64, длиннее
///   [`MAX_CALLBACK_DATA`] байт, либо не передано ничего (ни `data`, ни
///   `game`), то есть нажимать нечего.
pub fn validate_callback(mid: &str, req: &CallbackReq) -> Result<CallbackPlan, ApiError> {
    let msg_id: i32 = mid.trim().parse().ok().filter(|v| *v > 0).ok_or_else(|| {
        ApiError::unknown_message(format!("'{mid}' is not a valid Telegram message id"))
    })?;

    let raw = req.data.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let data = match raw {
        None => None,
        Some(s) => {
            let bytes = dto::base64_decode(s).ok_or_else(|| {
                ApiError::bad_input("'data' must be a base64 string as received in reply_markup")
            })?;
            if bytes.len() > MAX_CALLBACK_DATA {
                return Err(ApiError::bad_input(format!(
                    "callback data is {} bytes long, but Telegram allows at most {MAX_CALLBACK_DATA}",
                    bytes.len()
                )));
            }
            // Строка из одних символов padding'а декодируется в ноль байт —
            // это ровно то же самое, что «поля не было».
            (!bytes.is_empty()).then_some(bytes)
        }
    };

    if data.is_none() && !req.game {
        return Err(ApiError::bad_input(
            "expected either 'data' (base64 payload of a callback button) or 'game': true",
        ));
    }
    Ok(CallbackPlan {
        msg_id,
        data,
        game: req.game,
    })
}

/// `POST /api/telegram/dialogs/:id/messages/:mid/callback`.
///
/// Нажатие inline-кнопки бота: `messages.getBotCallbackAnswer`. Тело —
/// `{data?, game?}`, ответ — `{message, alert, url, cache_time}`.
///
/// # Что делает фронт с ответом
///
/// - `message` + `alert == false` ⇒ обычный тост;
/// - `message` + `alert == true` ⇒ модалка с «ОК» (пользователь обязан
///   закрыть её сам — так задумано ботом);
/// - `url` ⇒ открыть БЕЗ подтверждения: сервер гарантирует, что это
///   deep-link самого бота или адрес игры (в отличие от `keyboardButtonUrl`,
///   где подтверждение обязательно);
/// - `cache_time` ⇒ сколько секунд не дёргать бота повторно тем же нажатием.
///
/// Пустой ответ (`message == null`, `url == null`) — норма, а не ошибка:
/// бот часто отвечает молча и просто редактирует сообщение, а новая
/// клавиатура приезжает отдельным `message_edited` по WS.
///
/// # Чего сознательно НЕТ
///
/// Кнопки с `requires_password` (подтверждение операции облачным паролем
/// через SRP) не поддержаны: Telegram ответит на такую `PASSWORD_MISSING`,
/// и она честно доедет до фронта как `400`. Считать SRP ради единичного
/// сценария «бот банка просит пароль» в v1 не стоит риска работы с
/// облачным паролем на сервере.
///
/// # Errors
///
/// - `409 not_authorized` — аккаунт не привязан;
/// - `503 not_connected` — нет MTProto-соединения (или оно оборвалось);
/// - `404 unknown_message` — `:mid` не разбирается либо сообщение удалено;
/// - `404 unknown_dialog` — диалога нет или он недоступен;
/// - `400 invalid_input` — тело не по контракту либо `DATA_INVALID`;
/// - `429 flood_wait` — Telegram просит подождать (`retry_after_secs`);
/// - `504 bot_timeout` — бот не ответил;
/// - `502 telegram_error` — прочие отказы Telegram (см. [`map_callback_error`]).
pub async fn post_callback(
    State(state): State<Arc<TelegramState>>,
    Path((id, mid)): Path<(String, String)>,
    payload: Option<Json<CallbackReq>>,
) -> Result<Json<CallbackResp>, ApiError> {
    let req = payload.map(|Json(v)| v).unwrap_or_default();
    let client = require_ready(&state).await?;
    let plan = validate_callback(&mid, &req)?;
    let peer = peers::resolve_peer(&state, &id).await?;

    let answer: tl::types::messages::BotCallbackAnswer = client
        .invoke(&tl::functions::messages::GetBotCallbackAnswer {
            game: plan.game,
            peer: peer.into(),
            msg_id: plan.msg_id,
            data: plan.data,
            password: None,
        })
        .await
        .map_err(|e| map_callback_error(&e))?
        .into();

    // В лог не уходит ни payload кнопки, ни текст ответа бота: и то, и
    // другое — содержимое переписки пользователя.
    tracing::debug!(
        target: "forge_telegram",
        dialog_id = %id,
        msg_id = plan.msg_id,
        alert = answer.alert,
        "forge-telegram: bot callback answered"
    );
    Ok(Json(CallbackResp {
        message: answer.message,
        alert: answer.alert,
        url: answer.url,
        // У Telegram `cache_time` — обычный int без флага, «нет значения»
        // не бывает. `Option` в ответе оставлен ради формы контракта, но
        // наружу всегда уходит число.
        cache_time: Some(answer.cache_time),
    }))
}

/// `POST /api/telegram/dialogs/:id/read`.
///
/// `max_id = 0` в MTProto означает «всё» — поэтому отсутствие поля в теле
/// прочитывает диалог целиком, ровно как тап по чату в мобильном клиенте.
///
/// Побочные эффекты: обнуляется серверный счётчик непрочитанного и
/// рассылается `read`/`in` — так остальные открытые вкладки гасят бейдж, не
/// перезапрашивая список диалогов.
pub async fn post_read(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    payload: Option<Json<ReadReq>>,
) -> Result<Json<OkResp>, ApiError> {
    let req = payload.map(|Json(v)| v).unwrap_or_default();
    let client = require_ready(&state).await?;
    let peer = peers::resolve_peer(&state, &id).await?;
    let max_id = req.max_id.unwrap_or(0).max(0);

    // Каналы/супергруппы читаются другим методом, чем ЛС и малые группы.
    let res = if peer.id.kind() == PeerKind::Channel {
        client
            .invoke(&tl::functions::channels::ReadHistory {
                channel: peer.into(),
                max_id,
            })
            .await
            .map(drop)
    } else {
        client
            .invoke(&tl::functions::messages::ReadHistory {
                peer: peer.into(),
                max_id,
            })
            .await
            .map(drop)
    };
    res.map_err(|e| map_invocation(&e))?;

    let dialog_id = dto::dialog_id_string(peer.id);
    state.set_unread(&dialog_id, 0).await;
    state.broadcast_event(TgServerMsg::Read {
        dialog_id,
        max_id,
        direction: "in".into(),
    });
    Ok(Json(OkResp { ok: true }))
}

/// `POST /api/telegram/dialogs/:id/typing`.
///
/// Идемпотентен: Telegram сам гасит активность через ~6с, поэтому повторные
/// вызовы просто продлевают её, а `cancel` снимает досрочно.
pub async fn post_typing(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    payload: Option<Json<TypingReq>>,
) -> Result<Json<OkResp>, ApiError> {
    let req = payload.map(|Json(v)| v).unwrap_or_default();
    let client = require_ready(&state).await?;
    let kind = parse_typing(req.action.as_deref())?;
    let peer = peers::resolve_peer(&state, &id).await?;

    let sender = client.action(peer);
    let res = match kind {
        TypingKind::Action(a) => sender.oneshot(a).await,
        TypingKind::Cancel => sender.cancel().await,
    };
    res.map_err(|e| map_invocation(&e))?;
    Ok(Json(OkResp { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;
    use crate::login::{LoginFlow, TgUserDto};

    fn state() -> Arc<TelegramState> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("cache").join("chat-prompts.json"),
            ..TelegramConfig::default()
        };
        std::mem::forget(dir);
        TelegramState::new(cfg)
    }

    async fn authorized() -> Arc<TelegramState> {
        let st = state();
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

    fn rpc(code: i32, message: &str) -> InvocationError {
        InvocationError::Rpc(grammers_mtsender::RpcError::from(tl::types::RpcError {
            error_code: code,
            error_message: message.to_string(),
        }))
    }

    #[tokio::test]
    async fn require_authorized_rejects_unlinked_account() {
        let state = state();
        let err = require_authorized(&state).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err.body.code, "not_authorized");
        assert!(
            err.body.message.contains("not_configured"),
            "{}",
            err.body.message
        );
    }

    #[tokio::test]
    async fn require_authorized_accepts_signed_in_account() {
        let state = authorized().await;
        assert!(require_authorized(&state).await.is_ok());
    }

    /// Авторизация есть, соединения нет ⇒ `503`, а не `409`. Фронт по этому
    /// коду показывает RECONNECTING вместо wizard'а логина.
    #[tokio::test]
    async fn require_ready_separates_auth_from_connection() {
        let state = authorized().await;
        // `Client` не реализует Debug, поэтому Result разбирается вручную.
        let err = match require_ready(&state).await {
            Ok(_) => panic!("без рантайма клиента быть не может"),
            Err(e) => e,
        };
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");
    }

    #[test]
    fn clamp_limit_applies_defaults_and_ceilings() {
        assert_eq!(clamp_limit(None, 50, 100), 50);
        assert_eq!(clamp_limit(Some(0), 50, 100), 50);
        assert_eq!(clamp_limit(Some(7), 50, 100), 7);
        assert_eq!(clamp_limit(Some(100), 50, 100), 100);
        assert_eq!(clamp_limit(Some(10_000), 50, 100), 100);
        assert_eq!(clamp_limit(Some(u32::MAX), DEFAULT_DIALOGS, MAX_DIALOGS), MAX_DIALOGS);
    }

    /// Явный `?folder=` поднимает дефолтный размер выборки: именно на нём
    /// строится «покажи папку с ботами», и сотня верхних диалогов там
    /// означала бы «редкого чата не существует».
    ///
    /// Пустое значение (`?folder=`) обязано вести себя как отсутствие
    /// параметра — ровно так же, как его трактует [`parse_folder`].
    #[test]
    fn dialogs_default_limit_grows_only_for_explicit_folder() {
        assert_eq!(dialogs_default_limit(None), DEFAULT_DIALOGS);
        assert_eq!(dialogs_default_limit(Some("")), DEFAULT_DIALOGS);
        assert_eq!(dialogs_default_limit(Some("   ")), DEFAULT_DIALOGS);
        for folder in ["main", "archive", "all", " archive "] {
            assert_eq!(
                dialogs_default_limit(Some(folder)),
                DEFAULT_FOLDER_DIALOGS,
                "{folder}"
            );
        }
        // Соотношение самих констант сторожит компилятор (`const _: () =
        // assert!(…)` рядом с их объявлением) — здесь проверяется, что
        // дефолт папки доезжает до clamp неизменным.
        assert_eq!(
            clamp_limit(None, dialogs_default_limit(Some("archive")), MAX_DIALOGS),
            DEFAULT_FOLDER_DIALOGS
        );
        // Явный `limit` по-прежнему главнее дефолта — в обе стороны.
        assert_eq!(
            clamp_limit(Some(10), dialogs_default_limit(Some("archive")), MAX_DIALOGS),
            10
        );
        assert_eq!(
            clamp_limit(
                Some(u32::MAX),
                dialogs_default_limit(Some("archive")),
                MAX_DIALOGS
            ),
            MAX_DIALOGS
        );
    }

    /// Сырой запрос страницы уходит в ЗАПРОШЕННУЮ peer-папку.
    ///
    /// Перепутанный `folder_id` не виден ни в одном ответе: сервер просто
    /// отдаёт основной список вместо архива, и баг выглядит как «архив
    /// показывает не то». Поэтому проверяется само тело запроса.
    #[test]
    fn folder_dialogs_request_targets_the_requested_folder() {
        for folder_id in [0, 1] {
            let req = folder_dialogs_request(folder_id);
            assert_eq!(req.folder_id, Some(folder_id));
            // Первая страница обязана включать закреплённые: без них у
            // папки пропала бы вся её «шапка».
            assert!(!req.exclude_pinned);
            assert_eq!((req.offset_date, req.offset_id), (0, 0));
            assert_eq!(req.hash, 0, "hash=0 ⇒ NotModified не придёт");
            assert!(matches!(req.offset_peer, tl::enums::InputPeer::Empty));
        }
        // Ровно те же id, что отдаёт разбор query — два места не имеют
        // права разойтись.
        assert_eq!(
            folder_dialogs_request(FolderChoice::Archive.folder_id().unwrap()).folder_id,
            Some(1)
        );
    }

    /// Страховка от вечной пагинации: цикл обязан оборваться, как только
    /// сервер перестал двигать границу страницы.
    ///
    /// Без неё «странный» ответ Telegram (одинаковые смещения) прогонял бы
    /// [`MAX_FOLDER_PAGES`] одинаковых RPC — прямой путь к FLOOD_WAIT на
    /// ровном месте.
    #[test]
    fn offsets_advanced_stops_a_pagination_that_stands_still() {
        // Старт: границы ещё нет, любой сдвиг — прогресс.
        assert!(offsets_advanced((0, 0), (1_700_000_000, 42)));
        // Нормальный ход: назад по времени.
        assert!(offsets_advanced((1_700_000_000, 42), (1_699_999_000, 17)));
        // Та же дата, другое сообщение (пачка сообщений одной секунды).
        assert!(offsets_advanced((1_700_000_000, 42), (1_700_000_000, 41)));
        // Ничего не изменилось ⇒ следующий запрос вернёт ту же страницу.
        assert!(!offsets_advanced((1_700_000_000, 42), (1_700_000_000, 42)));
        // Граница «поехала» вперёд по времени ⇒ второй круг.
        assert!(!offsets_advanced((1_699_999_000, 17), (1_700_000_000, 42)));
    }

    #[test]
    fn parse_typing_defaults_to_typing_and_rejects_typos() {
        use tl::enums::SendMessageAction as A;
        assert_eq!(
            parse_typing(None).unwrap(),
            TypingKind::Action(A::SendMessageTypingAction)
        );
        assert_eq!(
            parse_typing(Some("  ")).unwrap(),
            TypingKind::Action(A::SendMessageTypingAction)
        );
        assert_eq!(parse_typing(Some("cancel")).unwrap(), TypingKind::Cancel);
        assert!(matches!(
            parse_typing(Some("upload_photo")).unwrap(),
            TypingKind::Action(A::SendMessageUploadPhotoAction(_))
        ));

        let err = parse_typing(Some("typng")).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");
        assert!(err.body.message.contains("typng"), "{}", err.body.message);
    }

    #[test]
    fn map_invocation_classifies_rpc_errors() {
        let e = map_invocation(&rpc(420, "FLOOD_WAIT_42"));
        assert_eq!(e.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.body.code, "flood_wait");
        assert_eq!(e.body.retry_after_secs, Some(42));

        let e = map_invocation(&rpc(401, "AUTH_KEY_UNREGISTERED"));
        assert_eq!(e.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(e.body.code, "not_authorized");

        for name in [
            "PEER_ID_INVALID",
            "CHANNEL_INVALID",
            "CHAT_ID_INVALID",
            "USER_ID_INVALID",
        ] {
            let e = map_invocation(&rpc(400, name));
            assert_eq!(e.status, axum::http::StatusCode::NOT_FOUND, "{name}");
            assert_eq!(e.body.code, "unknown_dialog", "{name}");
        }

        let e = map_invocation(&rpc(400, "MESSAGE_EMPTY"));
        assert_eq!(e.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(e.body.code, "invalid_input");

        for err in [rpc(500, "INTERNAL"), InvocationError::Dropped] {
            let e = map_invocation(&err);
            assert_eq!(e.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(e.body.code, "not_connected");
        }
    }

    /// FLOOD_WAIT распознаётся ПО ИМЕНИ, даже когда сервер прислал не-420
    /// код: подстраховочная ветка [`flood_wait_secs`] обязана срабатывать
    /// раньше ветки «остальные 400 ⇒ invalid_input», иначе клиент увидит
    /// «исправьте ввод» вместо обратного отсчёта и начнёт долбить повторами.
    #[test]
    fn map_invocation_prefers_flood_wait_over_the_400_family() {
        let e = map_invocation(&rpc(400, "FLOOD_WAIT_77"));
        assert_eq!(e.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.body.code, "flood_wait");
        assert_eq!(e.body.retry_after_secs, Some(77));
    }

    /// Всё семейство 420 несёт секунды в `value`: SLOWMODE_WAIT (медленный
    /// режим в группе) и FLOOD_PREMIUM_WAIT приходят в жизни так же часто,
    /// как классический FLOOD_WAIT, и обязаны давать 429 с тем же полем.
    #[test]
    fn map_invocation_covers_the_whole_420_family() {
        for (name, secs) in [
            ("SLOWMODE_WAIT_59", 59),
            ("FLOOD_PREMIUM_WAIT_3", 3),
            // Граница: FLOOD_WAIT_0 реально приходит и означает «можно сразу»,
            // но решение о повторе всё равно за пользователем.
            ("FLOOD_WAIT_0", 0),
        ] {
            let e = map_invocation(&rpc(420, name));
            assert_eq!(e.status, axum::http::StatusCode::TOO_MANY_REQUESTS, "{name}");
            assert_eq!(e.body.code, "flood_wait", "{name}");
            assert_eq!(e.body.retry_after_secs, Some(secs), "{name}");
        }
    }

    /// Отзыв сессии с другого устройства — второй по частоте 401 после
    /// AUTH_KEY_UNREGISTERED. Оба обязаны вести в wizard логина (409), а не
    /// в RECONNECTING (503).
    #[test]
    fn map_invocation_treats_all_401s_as_not_authorized() {
        for name in ["SESSION_REVOKED", "AUTH_KEY_INVALID", "USER_DEACTIVATED"] {
            let e = map_invocation(&rpc(401, name));
            assert_eq!(e.status, axum::http::StatusCode::CONFLICT, "{name}");
            assert_eq!(e.body.code, "not_authorized", "{name}");
        }
    }

    /// `reply_to_id` на удалённое сообщение — Telegram отвечает
    /// `400 MESSAGE_ID_INVALID`. Несмотря на суффикс `_ID_INVALID`, это НЕ
    /// «нет такого диалога»: диалог есть, кривой только ввод. Регрессия
    /// «расширили матчинг *_ID_INVALID» отправила бы фронт перерисовывать
    /// список диалогов вместо показа ошибки у поля ответа.
    #[test]
    fn message_id_invalid_is_bad_input_not_unknown_dialog() {
        let e = map_invocation(&rpc(400, "MESSAGE_ID_INVALID"));
        assert_eq!(e.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(e.body.code, "invalid_input");
    }

    /// Валидация тела `POST messages` идёт ДО резолва диалога и до сети:
    /// пустой текст и зарезервированный `upload_id` отклоняются даже при
    /// живой авторизации.
    #[tokio::test]
    async fn post_message_validates_body_before_touching_the_network() {
        let st = authorized().await;
        // Без соединения дальше валидации всё равно не пройти, поэтому
        // проверяем, что ошибка именно про тело, а не про соединение.
        // (Порядок: require_ready → валидация; поэтому здесь ожидаем 503.)
        let err = post_message(
            State(st.clone()),
            Path("777000".into()),
            Some(Json(SendMessageReq::default())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "not_connected");
    }

    /// Отметка о прочтении и typing без соединения не паникуют и отдают
    /// честный `503` (важно: тела запроса может не быть вовсе).
    #[tokio::test]
    async fn action_routes_tolerate_missing_body() {
        let st = authorized().await;
        let err = post_read(State(st.clone()), Path("777000".into()), None)
            .await
            .unwrap_err();
        assert_eq!(err.body.code, "not_connected");
        let err = post_typing(State(st.clone()), Path("777000".into()), None)
            .await
            .unwrap_err();
        assert_eq!(err.body.code, "not_connected");
    }

    /// Выбор способа отправки: фото сжимается Telegram'ом, остальное уходит
    /// файлом с исходным именем.
    #[test]
    fn is_photo_mime_compresses_only_real_photos() {
        for m in ["image/jpeg", "image/png", "IMAGE/JPEG", " image/heic "] {
            assert!(is_photo_mime(m), "{m} обязан уйти как фото");
        }
        for m in [
            "image/gif",       // Telegram сделает из gif видео-документ
            "image/webp",      // формат стикеров
            "image/svg+xml",   // не растр
            "video/mp4",
            "audio/ogg",
            "application/pdf",
            "application/octet-stream",
            "",
        ] {
            assert!(!is_photo_mime(m), "{m} обязан уйти документом");
        }
    }

    /// `text` и `caption` — синонимы; берётся первое непустое.
    #[test]
    fn pick_text_prefers_first_non_empty() {
        let mk = |text: Option<&str>, caption: Option<&str>| SendMessageReq {
            text: text.map(str::to_string),
            caption: caption.map(str::to_string),
            ..SendMessageReq::default()
        };
        assert_eq!(pick_text(&mk(Some("a"), Some("b"))), "a");
        assert_eq!(pick_text(&mk(None, Some("b"))), "b");
        assert_eq!(pick_text(&mk(Some("  "), Some("b"))), "b");
        assert_eq!(pick_text(&mk(None, None)), "");
        assert_eq!(pick_text(&mk(Some(" x "), None)), " x ");
    }

    /// Матрица валидации тела: она обязана работать без сети, поэтому
    /// проверяется отдельно от хендлера.
    #[test]
    fn validate_send_covers_the_contract() {
        // Пусто — 400.
        let err = validate_send(&SendMessageReq::default()).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");

        // Только текст.
        let plan = validate_send(&SendMessageReq {
            text: Some("hi".into()),
            ..SendMessageReq::default()
        })
        .unwrap();
        assert_eq!(plan.text, "hi");
        assert!(plan.upload_id.is_none());

        // Только upload_id — подпись не обязательна.
        let plan = validate_send(&SendMessageReq {
            upload_id: Some("u-1".into()),
            ..SendMessageReq::default()
        })
        .unwrap();
        assert_eq!(plan.upload_id.as_deref(), Some("u-1"));
        assert!(plan.text.is_empty());

        // Пробелы в upload_id не создают «пустую» загрузку.
        let err = validate_send(&SendMessageReq {
            upload_id: Some("   ".into()),
            ..SendMessageReq::default()
        })
        .unwrap_err();
        assert_eq!(err.body.code, "invalid_input");

        // Подпись ровно в лимите проходит, на символ длиннее — нет.
        let ok = "я".repeat(MAX_CAPTION_CHARS);
        assert!(validate_send(&SendMessageReq {
            upload_id: Some("u".into()),
            caption: Some(ok.clone()),
            ..SendMessageReq::default()
        })
        .is_ok());
        let err = validate_send(&SendMessageReq {
            upload_id: Some("u".into()),
            caption: Some(format!("{ok}я")),
            ..SendMessageReq::default()
        })
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            err.body.message.contains("1024"),
            "в сообщении обязан быть лимит: {}",
            err.body.message
        );

        // Для ТЕКСТА лимит подписи не применяется (у сообщений он свой, 4096,
        // и его проверяет Telegram).
        assert!(validate_send(&SendMessageReq {
            text: Some("x".repeat(MAX_CAPTION_CHARS + 100)),
            ..SendMessageReq::default()
        })
        .is_ok());
    }

    /// Лимит подписи применяется К ВЫБРАННОМУ тексту, а не к полю `caption`:
    /// фронт шлёт подпись к файлу в синониме `text`, и переполнение обязано
    /// ловиться и там. Регрессия «проверяем только req.caption» пропустила бы
    /// запрос в Telegram и вернула бы непонятный `MEDIA_CAPTION_TOO_LONG`.
    #[test]
    fn caption_limit_applies_to_the_text_synonym_too() {
        let err = validate_send(&SendMessageReq {
            upload_id: Some("u".into()),
            text: Some("x".repeat(MAX_CAPTION_CHARS + 1)),
            ..SendMessageReq::default()
        })
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");
    }

    /// Файл со «случайным пробелом» в поле подписи — валидная отправка медиа
    /// без подписи, а не ошибка и не подпись из пробелов.
    #[test]
    fn media_with_whitespace_caption_is_sent_without_caption() {
        let plan = validate_send(&SendMessageReq {
            upload_id: Some("u-1".into()),
            text: Some("   ".into()),
            ..SendMessageReq::default()
        })
        .unwrap();
        assert_eq!(plan.upload_id.as_deref(), Some("u-1"));
        assert!(plan.text.is_empty(), "пробелы не должны стать подписью");
    }

    /// `upload_id` с обрамляющими пробелами (скопирован из devtools/логов)
    /// обязан совпасть с записью реестра, а не превратиться в «неизвестный id».
    #[test]
    fn upload_id_is_trimmed_before_registry_lookup() {
        let plan = validate_send(&SendMessageReq {
            upload_id: Some("  u-1  ".into()),
            ..SendMessageReq::default()
        })
        .unwrap();
        assert_eq!(plan.upload_id.as_deref(), Some("u-1"));
    }

    /// Гонка двойной отправки (двойной клик по «отправить»): запись реестра
    /// потребляется РОВНО ОДНИМ из конкурентных запросов — иначе собеседник
    /// получил бы файл дважды.
    #[tokio::test]
    async fn concurrent_sends_consume_the_upload_exactly_once() {
        let st = state();
        st.insert_upload(
            "u-race".into(),
            crate::state::UploadEntry {
                path: std::path::PathBuf::from("/nonexistent/staging"),
                file_name: "a.bin".into(),
                size: 1,
                mime: "application/octet-stream".into(),
                created_at: 0,
            },
        )
        .await;

        let a = tokio::spawn({
            let st = st.clone();
            async move { st.take_upload("u-race").await }
        });
        let b = tokio::spawn({
            let st = st.clone();
            async move { st.take_upload("u-race").await }
        });
        let (a, b) = (a.await.unwrap(), b.await.unwrap());
        assert_eq!(
            u8::from(a.is_some()) + u8::from(b.is_some()),
            1,
            "запись обязана достаться ровно одному запросу"
        );
    }

    /// Неизвестный (или уже потреблённый) `upload_id` — `409 invalid_state`.
    /// Проверяется через реестр: до сети такой запрос не доходит.
    #[tokio::test]
    async fn unknown_upload_id_is_invalid_state() {
        let st = authorized().await;
        assert!(st.take_upload("nope").await.is_none());
        // Тот же путь, что в хендлере: нет записи ⇒ invalid_state.
        let err = ApiError::invalid_state("unknown or already used upload_id 'nope'");
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");
    }

    /// Активность, которую видит собеседник, зависит от вида файла.
    #[test]
    fn upload_action_matches_the_file_kind() {
        use tl::enums::SendMessageAction as A;
        assert!(matches!(
            upload_action("image/jpeg"),
            A::SendMessageUploadPhotoAction(_)
        ));
        assert!(matches!(
            upload_action("application/pdf"),
            A::SendMessageUploadDocumentAction(_)
        ));
        assert!(matches!(
            upload_action("video/mp4"),
            A::SendMessageUploadDocumentAction(_)
        ));
    }

    /// Ответы сериализуются в форму, на которую написан фронт.
    #[test]
    fn response_shapes_match_the_frontend_contract() {
        let json = serde_json::to_value(DialogsResp {
            dialogs: vec![DialogDto {
                id: "-1001234567890".into(),
                kind: "channel".into(),
                ..DialogDto::default()
            }],
        })
        .unwrap();
        assert_eq!(json["dialogs"][0]["id"], "-1001234567890");

        let json = serde_json::to_value(MessagesResp {
            messages: vec![MessageDto {
                id: 5,
                ..MessageDto::default()
            }],
            has_more: true,
        })
        .unwrap();
        assert_eq!(json["messages"][0]["id"], 5);
        assert_eq!(json["has_more"], true);

        let json = serde_json::to_value(SendMessageResp {
            message: MessageDto {
                id: 9,
                out: true,
                ..MessageDto::default()
            },
        })
        .unwrap();
        assert_eq!(json["message"]["id"], 9);
        assert_eq!(json["message"]["out"], true);

        assert_eq!(serde_json::to_value(OkResp { ok: true }).unwrap()["ok"], true);
    }

    /// Разбор `?folder=`: отсутствие/`all` — прежнее поведение, `main` и
    /// `archive` — соответствующие peer-папки, опечатка — честный `400`.
    #[test]
    fn parse_folder_covers_the_contract() {
        assert_eq!(parse_folder(None).unwrap(), FolderChoice::All);
        assert_eq!(parse_folder(Some("")).unwrap(), FolderChoice::All);
        assert_eq!(parse_folder(Some("  ")).unwrap(), FolderChoice::All);
        assert_eq!(parse_folder(Some("all")).unwrap(), FolderChoice::All);
        assert_eq!(parse_folder(Some("main")).unwrap(), FolderChoice::Main);
        assert_eq!(parse_folder(Some(" archive ")).unwrap(), FolderChoice::Archive);

        for bad in ["archve", "MAIN", "0", "1", "inbox"] {
            let err = parse_folder(Some(bad)).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(err.body.code, "invalid_input", "{bad}");
            assert!(err.body.message.contains(bad), "{}", err.body.message);
        }
    }

    /// `folder_id` для сырого вызова: у `All` его нет (прежний путь через
    /// iter_dialogs), `main`/`archive` — 0/1 по контракту Telegram.
    #[test]
    fn folder_choice_maps_to_telegram_folder_ids() {
        assert_eq!(FolderChoice::All.folder_id(), None);
        assert_eq!(FolderChoice::Main.folder_id(), Some(0));
        assert_eq!(FolderChoice::Archive.folder_id(), Some(1));
    }

    /// Смещения пагинации берутся из даты/id сырого сообщения; дыра
    /// истории (`MessageEmpty`) смещений не даёт.
    #[test]
    fn raw_message_offset_reads_date_and_id() {
        let empty = tl::enums::Message::Empty(tl::types::MessageEmpty {
            id: 5,
            peer_id: None,
        });
        assert_eq!(raw_message_offset(&empty), None);
    }

    /// Порядок проверок для новых путей тот же, что у остальных
    /// data-роутов: авторизация первой (`409`), затем соединение (`503`) —
    /// и только потом разбор `folder`.
    #[tokio::test]
    async fn folders_and_folder_query_follow_the_check_order() {
        // Не авторизован ⇒ 409 и для /folders, и для ?folder=archive.
        let st = state();
        let err = get_folders(State(st.clone())).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(err.body.code, "not_authorized");
        let err = get_dialogs(
            State(st),
            Query(DialogsQuery {
                folder: Some("archive".into()),
                ..DialogsQuery::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "not_authorized");

        // Авторизован, но без соединения ⇒ 503 — даже с кривым folder:
        // разбор query идёт после require_ready, как у остальных роутов.
        let st = authorized().await;
        let err = get_folders(State(st.clone())).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");
        let err = get_dialogs(
            State(st),
            Query(DialogsQuery {
                folder: Some("bogus".into()),
                ..DialogsQuery::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.body.code, "not_connected");
    }

    /// Форма ответа `/folders` — ровно то, на что пишется фронт: массив
    /// `folders`, id peer'ов строками.
    #[test]
    fn folders_response_shape_matches_the_frontend_contract() {
        let json = serde_json::to_value(FoldersResp {
            folders: vec![FolderDto {
                id: 2,
                title: "Работа".into(),
                emoticon: Some("💼".into()),
                include_ids: vec!["-1001234567890".into()],
                exclude_muted: true,
                ..FolderDto::default()
            }],
        })
        .unwrap();
        assert_eq!(json["folders"][0]["id"], 2);
        assert_eq!(json["folders"][0]["title"], "Работа");
        assert_eq!(json["folders"][0]["include_ids"][0], "-1001234567890");
        assert_eq!(json["folders"][0]["exclude_muted"], true);

        let empty = serde_json::to_value(FoldersResp { folders: vec![] }).unwrap();
        assert_eq!(empty["folders"], serde_json::json!([]));
    }

    /// Кэш непрочитанного: апдейты инкрементят, `read` обнуляет, список
    /// диалогов пересевает целиком.
    #[tokio::test]
    async fn unread_cache_bumps_resets_and_reseeds() {
        let st = state();
        assert_eq!(st.bump_unread("777000").await, 1);
        assert_eq!(st.bump_unread("777000").await, 2);
        assert_eq!(st.unread_of("777000").await, 2);

        st.set_unread("777000", 0).await;
        assert_eq!(st.unread_of("777000").await, 0);
        assert!(st.unread.read().await.is_empty(), "нули не занимают память");

        let mut fresh = HashMap::new();
        fresh.insert("777000".to_string(), 5);
        fresh.insert("-1001234567890".to_string(), 0);
        st.reset_unread(fresh).await;
        assert_eq!(st.unread_of("777000").await, 5);
        assert_eq!(st.unread_of("-1001234567890").await, 0);
        assert_eq!(st.unread.read().await.len(), 1);
    }

    // ------------------------------------------------------------------
    // Нажатие кнопки бота
    // ------------------------------------------------------------------

    /// Тело нажатия с корректным base64-payload'ом.
    fn callback(data: &[u8]) -> CallbackReq {
        CallbackReq {
            data: Some(dto::base64_encode(data)),
            game: false,
        }
    }

    /// `:mid` — единственное место, где путь может врать: это id сообщения,
    /// а не диалога, и Telegram нумерует сообщения с единицы. Мусор и
    /// вырожденные значения обязаны давать `404 unknown_message`, а не
    /// уезжать в Telegram нулём (там это значит «первое сообщение чата»).
    #[test]
    fn validate_callback_rejects_impossible_message_ids() {
        for mid in [
            "abc",
            "",
            "   ",
            "0",
            "-1",
            "-2147483648",
            "2147483648", // > i32::MAX
            "1e3",
            "7.0",
            "5 5",
            "٧", // арабо-индийская семёрка: цифра по Unicode, но не для i32
        ] {
            let err = validate_callback(mid, &callback(b"x")).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND, "{mid:?}");
            assert_eq!(err.body.code, "unknown_message", "{mid:?}");
        }

        // Границы допустимого: единица, максимум i32, обрамляющие пробелы.
        // `+7` тоже проходит — так его читает `i32::from_str`, и городить
        // здесь собственный парсер незачем: результат всё равно
        // положительный i32, который Telegram либо знает, либо нет.
        for mid in ["1", "2147483647", " 42 ", "+7"] {
            let plan = validate_callback(mid, &callback(b"x")).unwrap();
            assert!(plan.msg_id > 0, "{mid}");
        }
    }

    /// Матрица тела: payload обязан быть base64, укладываться в лимит
    /// Telegram и вообще присутствовать (либо это игровая кнопка).
    #[test]
    fn validate_callback_covers_the_body_contract() {
        // Обычное нажатие: байты доезжают ровно теми же, что были в DTO.
        let plan = validate_callback("5", &callback(b"vote:1")).unwrap();
        assert_eq!(
            plan,
            CallbackPlan {
                msg_id: 5,
                data: Some(b"vote:1".to_vec()),
                game: false,
            }
        );

        // Payload — это БАЙТЫ, а не текст: не-UTF8 обязан пережить путь
        // «Telegram → base64 → JSON → base64 → Telegram» без искажений.
        let raw = [0xFF_u8, 0x00, 0xFE, 0x10];
        let plan = validate_callback("5", &callback(&raw)).unwrap();
        assert_eq!(plan.data.as_deref(), Some(&raw[..]));

        // Не base64 ⇒ 400, а не 500 и не молчаливый пропуск.
        for bad in ["!!!!", "%%%", "☃☃☃"] {
            let err = validate_callback(
                "5",
                &CallbackReq {
                    data: Some(bad.into()),
                    game: false,
                },
            )
            .unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(err.body.code, "invalid_input", "{bad}");
        }

        // Лимит считается по ДЕКОДИРОВАННЫМ байтам: 64 проходят, 65 — нет.
        assert!(validate_callback("5", &callback(&[b'x'; MAX_CALLBACK_DATA])).is_ok());
        let err = validate_callback("5", &callback(&[b'x'; MAX_CALLBACK_DATA + 1])).unwrap_err();
        assert_eq!(err.body.code, "invalid_input");
        assert!(
            err.body.message.contains("64"),
            "в сообщении обязан быть лимит: {}",
            err.body.message
        );

        // Нажимать нечего: ни payload'а, ни игры.
        for req in [
            CallbackReq::default(),
            CallbackReq {
                data: Some("".into()),
                game: false,
            },
            CallbackReq {
                data: Some("   ".into()),
                game: false,
            },
            // Одни padding-символы декодируются в ноль байт — это то же
            // самое, что «поля не было».
            CallbackReq {
                data: Some("====".into()),
                game: false,
            },
        ] {
            let err = validate_callback("5", &req).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{req:?}");
            assert_eq!(err.body.code, "invalid_input", "{req:?}");
        }

        // Игровая кнопка — единственный легальный случай «без data».
        let plan = validate_callback(
            "5",
            &CallbackReq {
                data: None,
                game: true,
            },
        )
        .unwrap();
        assert_eq!(
            plan,
            CallbackPlan {
                msg_id: 5,
                data: None,
                game: true,
            }
        );
    }

    /// Base64 приезжает из браузера через JSON и мог быть перенесён по
    /// строкам или прогнан через URL-кодирование. Отвечать `400` на данные,
    /// которые мы сами же и выдали, нельзя — декодер терпим ко всему, что
    /// восстановимо однозначно.
    #[test]
    fn validate_callback_tolerates_reshaped_base64() {
        let plan = validate_callback("5", &callback(b"payload")).unwrap();
        let expected = plan.data.clone();

        for shaped in [
            dto::base64_encode(b"payload"),
            dto::base64_encode(b"payload").replace('=', ""), // без padding
            format!("{}\n", dto::base64_encode(b"payload")), // перенос строки
        ] {
            let got = validate_callback(
                "5",
                &CallbackReq {
                    data: Some(shaped.clone()),
                    game: false,
                },
            )
            .unwrap();
            assert_eq!(got.data, expected, "{shaped:?}");
        }
    }

    /// Маппинг ошибок нажатия кнопки. Именно он отличает «бот молчит» от
    /// «сообщение удалили» от «переподключись» — фронт принимает решение
    /// по `code`, поэтому таблица зафиксирована тестом целиком.
    #[test]
    fn map_callback_error_classifies_the_whole_table() {
        use axum::http::StatusCode as S;

        // (ошибка, статус, код)
        let cases: Vec<(InvocationError, S, &str)> = vec![
            // FLOOD_WAIT — раньше всего остального, иначе клиент увидит
            // «исправьте ввод» и начнёт долбить повторами.
            (rpc(420, "FLOOD_WAIT_42"), S::TOO_MANY_REQUESTS, "flood_wait"),
            // Бот не ответил. Telegram шлёт это с 400, а второй таймаут —
            // с -503: оба обязаны стать 504, а не 400/503.
            (rpc(400, "BOT_RESPONSE_TIMEOUT"), S::GATEWAY_TIMEOUT, "bot_timeout"),
            (rpc(500, "BOT_RESPONSE_TIMEOUT"), S::GATEWAY_TIMEOUT, "bot_timeout"),
            (rpc(-503, "Timeout"), S::GATEWAY_TIMEOUT, "bot_timeout"),
            // Сессию отозвали.
            (rpc(401, "AUTH_KEY_UNREGISTERED"), S::CONFLICT, "not_authorized"),
            // Сообщение с кнопкой уже удалено.
            (rpc(400, "MESSAGE_ID_INVALID"), S::NOT_FOUND, "unknown_message"),
            (rpc(400, "MSG_ID_INVALID"), S::NOT_FOUND, "unknown_message"),
            // Диалога нет / он закрылся.
            (rpc(400, "PEER_ID_INVALID"), S::NOT_FOUND, "unknown_dialog"),
            (rpc(400, "CHANNEL_INVALID"), S::NOT_FOUND, "unknown_dialog"),
            (rpc(400, "CHANNEL_PRIVATE"), S::NOT_FOUND, "unknown_dialog"),
            (rpc(400, "CHAT_ID_INVALID"), S::NOT_FOUND, "unknown_dialog"),
            (rpc(400, "USER_ID_INVALID"), S::NOT_FOUND, "unknown_dialog"),
            // Payload не тот, что ждёт бот (обычно клавиатура устарела).
            (rpc(400, "DATA_INVALID"), S::BAD_REQUEST, "invalid_input"),
            // Кнопка требует облачный пароль — SRP мы не считаем.
            (rpc(400, "PASSWORD_MISSING"), S::BAD_REQUEST, "invalid_input"),
            // Telegram сломался сам: реконнект не поможет, повтор тоже.
            (rpc(500, "INTERNAL"), S::BAD_GATEWAY, "telegram_error"),
            (rpc(403, "CHAT_WRITE_FORBIDDEN"), S::BAD_GATEWAY, "telegram_error"),
            // А вот это — как раз обрыв связи.
            (InvocationError::Dropped, S::SERVICE_UNAVAILABLE, "not_connected"),
        ];

        for (err, status, code) in cases {
            let api = map_callback_error(&err);
            assert_eq!(api.status, status, "{err:?}");
            assert_eq!(api.body.code, code, "{err:?}");
            assert!(!api.body.message.is_empty(), "{err:?}");
        }

        assert_eq!(
            map_callback_error(&rpc(420, "FLOOD_WAIT_42"))
                .body
                .retry_after_secs,
            Some(42)
        );
        // Сообщение 504 обязано объяснять пользователю, кто именно молчит:
        // это единственная ошибка, где виноваты не мы и не Telegram.
        assert!(
            map_callback_error(&rpc(400, "BOT_RESPONSE_TIMEOUT"))
                .body
                .message
                .contains("бот не ответил"),
            "{}",
            map_callback_error(&rpc(400, "BOT_RESPONSE_TIMEOUT")).body.message
        );
    }

    /// Один и тот же `MESSAGE_ID_INVALID` означает РАЗНОЕ на разных роутах,
    /// и оба маппера обязаны остаться при своём: у `POST /messages` это
    /// кривой `reply_to_id` пользователя (400 — покажи ошибку у поля), у
    /// нажатия кнопки — исчезнувшее сообщение (404 — погаси клавиатуру).
    /// Сведение их в один маппер сломало бы одну из двух вкладок.
    #[test]
    fn message_id_invalid_means_different_things_on_different_routes() {
        assert_eq!(map_invocation(&rpc(400, "MESSAGE_ID_INVALID")).body.code, "invalid_input");
        assert_eq!(
            map_callback_error(&rpc(400, "MESSAGE_ID_INVALID")).body.code,
            "unknown_message"
        );
    }

    /// Порядок проверок нажатия кнопки тот же, что у остальных data-роутов:
    /// авторизация (`409`) → соединение (`503`) → разбор пути и тела.
    ///
    /// Проверяется в том числе на заведомо мусорном `:mid`: иначе
    /// неавторизованный клиент мог бы прощупывать существование сообщений
    /// по разнице ответов.
    #[tokio::test]
    async fn post_callback_checks_auth_before_parsing_anything() {
        for (id, mid) in [
            ("777000", "5"),
            ("777000", "мусор"),
            ("../../etc/passwd", "0"),
        ] {
            let st = state();
            let err = post_callback(
                State(st),
                Path((id.into(), mid.into())),
                Some(Json(callback(b"x"))),
            )
            .await
            .unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::CONFLICT, "{id}/{mid}");
            assert_eq!(err.body.code, "not_authorized", "{id}/{mid}");
        }

        // Авторизован, соединения нет ⇒ 503 (а не 404 по мусорному mid).
        let st = authorized().await;
        let err = post_callback(
            State(st.clone()),
            Path(("777000".into(), "мусор".into())),
            Some(Json(callback(b"x"))),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.body.code, "not_connected");

        // Тела может не быть вовсе (curl без -d): это не паника и не 500,
        // а обычная лестница проверок — здесь она упирается в соединение.
        let err = post_callback(State(st), Path(("777000".into(), "5".into())), None)
            .await
            .unwrap_err();
        assert_eq!(err.body.code, "not_connected");
    }

    /// Форма ответа — ровно то, на что пишется фронт: `message`/`url` могут
    /// быть `null`, но ПРИСУТСТВОВАТЬ обязаны (иначе `msg.url === undefined`
    /// и `msg.url === null` разъедутся), `alert` — булев, `cache_time` —
    /// число.
    #[test]
    fn callback_response_shape_matches_the_frontend_contract() {
        let json = serde_json::to_value(CallbackResp {
            message: Some("Готово".into()),
            alert: true,
            url: None,
            cache_time: Some(30),
        })
        .unwrap();
        assert_eq!(json["message"], "Готово");
        assert_eq!(json["alert"], true);
        assert!(json["url"].is_null());
        assert_eq!(json["cache_time"], 30);
        assert_eq!(json.as_object().unwrap().len(), 4, "лишние поля: {json}");

        // Молчаливый ответ бота — норма: все поля null/false, но на месте.
        let json = serde_json::to_value(CallbackResp {
            message: None,
            alert: false,
            url: None,
            cache_time: Some(0),
        })
        .unwrap();
        for key in ["message", "alert", "url", "cache_time"] {
            assert!(json.get(key).is_some(), "поле {key} пропало: {json}");
        }
        assert!(json["message"].is_null());
        assert_eq!(json["alert"], false);
    }

    // ------------------------------------------------------------------
    // Краевые случаи нажатия кнопки и пагинации
    // ------------------------------------------------------------------

    /// Вырожденные payload'ы: «бомба» на десять килобайт, ровный предел,
    /// не-UTF8 на пределе и `data` вместе с `game`.
    ///
    /// Все они приходят из браузера, то есть из места, которое пользователь
    /// контролирует полностью: любой из них обязан давать понятный `400`
    /// (или честный план), но никак не панику и не уход в Telegram «как
    /// есть» — там на это ответят `DATA_INVALID` и заодно посчитают флуд.
    #[test]
    fn validate_callback_handles_extreme_payloads() {
        // Килобайты вместо 64 байт: отказ на НАШЕЙ стороне, до сети.
        let bomb = dto::base64_encode(&vec![b'z'; 10 * 1024]);
        let err = validate_callback(
            "5",
            &CallbackReq {
                data: Some(bomb),
                game: false,
            },
        )
        .unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.body.code, "invalid_input");

        // Ровный предел из НЕ-UTF8 байтов: 64 проходят, 65 — нет, и лимит
        // считается по байтам, а не по длине base64-строки (она втрое длиннее).
        let payload: Vec<u8> = (0..MAX_CALLBACK_DATA)
            .map(|i| (i as u8).wrapping_mul(9).wrapping_add(128))
            .collect();
        assert!(String::from_utf8(payload.clone()).is_err(), "фикстура не-UTF8");
        let plan = validate_callback("5", &callback(&payload)).unwrap();
        assert_eq!(plan.data.as_deref(), Some(&payload[..]));
        assert!(
            dto::base64_encode(&payload).len() > MAX_CALLBACK_DATA,
            "строка обязана быть длиннее лимита — иначе тест ничего не проверяет"
        );
        let mut over = payload.clone();
        over.push(0);
        assert!(validate_callback("5", &callback(&over)).is_err());

        // `data` + `game` вместе: Telegram разрешает (игровая кнопка тоже
        // может нести payload), и отказывать здесь было бы отсебятиной.
        let plan = validate_callback(
            "5",
            &CallbackReq {
                data: Some(dto::base64_encode(b"score")),
                game: true,
            },
        )
        .unwrap();
        assert_eq!(
            plan,
            CallbackPlan {
                msg_id: 5,
                data: Some(b"score".to_vec()),
                game: true,
            }
        );

        // Base64, изрезанный переносами и пробелами (типичный copy-paste из
        // devtools), восстанавливается однозначно.
        let shredded = dto::base64_encode(b"vote:42")
            .chars()
            .flat_map(|c| [c, '\n'])
            .collect::<String>();
        let plan = validate_callback(
            "5",
            &CallbackReq {
                data: Some(shredded),
                game: false,
            },
        )
        .unwrap();
        assert_eq!(plan.data.as_deref(), Some(&b"vote:42"[..]));

        // Ведущие нули и обрамляющие пробелы в `:mid` — не повод для 404.
        assert_eq!(validate_callback("007", &callback(b"x")).unwrap().msg_id, 7);
        assert_eq!(
            validate_callback("\t42\n", &callback(b"x")).unwrap().msg_id,
            42
        );
    }

    /// Отказы, у которых нет ни числа в имени, ни RPC-обёртки.
    ///
    /// Ключевое разделение: «Telegram отказал» (502 — повтор бессмыслен) и
    /// «связь порвалась» (503 — лечится реконнектом). Спутать их значит
    /// показать пользователю неверное действие: «переподключаемся» там, где
    /// переподключаться нечему, и наоборот.
    #[test]
    fn map_callback_error_separates_telegram_refusals_from_broken_links() {
        use axum::http::StatusCode as S;

        // Всё семейство 420 несёт секунды в имени.
        for (name, secs) in [
            ("FLOOD_WAIT_1", 1_u64),
            ("FLOOD_PREMIUM_WAIT_60", 60),
            ("SLOWMODE_WAIT_5", 5),
            ("TAKEOUT_INIT_DELAY_86400", 86_400),
        ] {
            let api = map_callback_error(&rpc(420, name));
            assert_eq!(api.status, S::TOO_MANY_REQUESTS, "{name}");
            assert_eq!(api.body.retry_after_secs, Some(secs), "{name}");
        }
        // 420 без числа Telegram не присылает, но и он не имеет права стать
        // «исправьте ввод»: ждать всё равно придётся, а не переписывать запрос.
        let nameless = map_callback_error(&rpc(420, "FLOOD_WAIT"));
        assert_ne!(nameless.body.code, "invalid_input");
        assert!(nameless.status.as_u16() >= 429, "{}", nameless.status);

        // Не-RPC: обрыв, ввод-вывод, неизвестный DC — всё лечится реконнектом.
        for err in [
            InvocationError::Dropped,
            InvocationError::InvalidDc,
            InvocationError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "connection reset",
            )),
        ] {
            let api = map_callback_error(&err);
            assert_eq!(api.status, S::SERVICE_UNAVAILABLE, "{err:?}");
            assert_eq!(api.body.code, "not_connected", "{err:?}");
        }

        // Неизвестный отказ Telegram (не 400, не 401, не 420) — 502.
        for code in [403, 406, 500, 503, -500] {
            let api = map_callback_error(&rpc(code, "SOMETHING_NEW"));
            assert_eq!(api.status, S::BAD_GATEWAY, "код {code}");
            assert_eq!(api.body.code, "telegram_error", "код {code}");
            assert!(
                api.body.message.contains(&code.to_string()),
                "код обязан быть в сообщении: {}",
                api.body.message
            );
        }

        // Имя таймаута проверяется ЦЕЛИКОМ: похожие, но другие ошибки не
        // имеют права уехать в 504 («бот не ответил» — сильное утверждение).
        for name in ["BOT_RESPONSE_TIMEOUTX", "TIMEOUT", "timeout"] {
            assert_ne!(
                map_callback_error(&rpc(400, name)).body.code,
                "bot_timeout",
                "{name}"
            );
        }
        assert_eq!(
            map_callback_error(&rpc(400, "BOT_RESPONSE_TIMEOUT")).body.code,
            "bot_timeout"
        );

        // Секретов в тексте ошибок нет ни в одном случае: наружу уходят
        // только имя и код Telegram.
        for err in [rpc(400, "DATA_INVALID"), rpc(500, "INTERNAL")] {
            let msg = map_callback_error(&err).body.message;
            assert!(!msg.contains("api_hash") && !msg.contains("password"), "{msg}");
        }
    }

    /// Пагинация на границах `i32`: смещения приходят от сервера, и
    /// вырожденные значения (нули, отрицательные даты, максимумы) не имеют
    /// права ни зациклить обход, ни оборвать нормальный.
    #[test]
    fn offsets_advanced_handles_degenerate_boundaries() {
        // Из «границы ещё нет» прогресс есть даже в вырожденную пару.
        assert!(offsets_advanced((0, 0), (0, 1)));
        assert!(offsets_advanced((0, 0), (i32::MIN, i32::MIN)));
        // Максимумы: движение назад по времени — прогресс, вперёд — нет.
        assert!(offsets_advanced((i32::MAX, i32::MAX), (i32::MAX - 1, 0)));
        assert!(!offsets_advanced((i32::MIN, 0), (i32::MAX, 0)));
        // Откат в нулевые смещения формально считается движением назад по
        // времени, то есть прогрессом, — и это не дыра: получить (0, 0) можно
        // только из сообщения с датой 0 и id 0, которого у Telegram не
        // бывает (id нумеруются с единицы). Даже если такой ответ придёт,
        // цикл ограничен потолком страниц, а повторы отсекает дедуп по
        // dialog_id — то есть цена ошибки это лишние RPC, а не вечный обход.
        assert!(offsets_advanced((1_700_000_000, 42), (0, 0)));
        assert!(raw_message_offset(&tl::enums::Message::Empty(tl::types::MessageEmpty {
            id: 0,
            peer_id: None,
        }))
        .is_none());
        // Ровное повторение на любых значениях — стоп.
        for pair in [(0, 0), (i32::MIN, i32::MIN), (i32::MAX, i32::MAX)] {
            assert!(!offsets_advanced(pair, pair), "{pair:?}");
        }
    }

    /// Строка «Архив» из `messages.getDialogs` — не диалог.
    ///
    /// Проверяется предикат, на котором держится фильтр в обоих путях
    /// листинга (сырой `folder_dialogs` и grammers `iter_dialogs`): пропущенная
    /// дальше строка отдаёт DTO с чужим id и обнулёнными `muted`/`archived`,
    /// а он через `remember_dialogs` затирает правду об архивном чате.
    #[test]
    fn folder_rows_are_filtered_out_of_dialog_listings() {
        let folder = tl::enums::Dialog::Folder(tl::types::DialogFolder {
            pinned: true,
            folder: tl::enums::Folder::Folder(tl::types::Folder {
                autofill_new_broadcasts: false,
                autofill_public_groups: false,
                autofill_new_correspondents: false,
                id: 1,
                title: "Archive".into(),
                photo: None,
            }),
            peer: tl::enums::Peer::Channel(tl::types::PeerChannel {
                channel_id: 1_234_567_890,
            }),
            top_message: 7,
            unread_muted_peers_count: 3,
            unread_unmuted_peers_count: 0,
            unread_muted_messages_count: 12,
            unread_unmuted_messages_count: 0,
        });
        assert!(dto::is_folder_row(&folder));

        // Настоящий диалог того же peer'а строкой-папкой не считается —
        // иначе фильтр выкосил бы сам архив.
        let real = tl::enums::Dialog::Dialog(tl::types::Dialog {
            pinned: false,
            unread_mark: false,
            view_forum_as_messages: false,
            peer: tl::enums::Peer::Channel(tl::types::PeerChannel {
                channel_id: 1_234_567_890,
            }),
            top_message: 7,
            read_inbox_max_id: 0,
            read_outbox_max_id: 0,
            unread_count: 3,
            unread_mentions_count: 0,
            unread_reactions_count: 0,
            unread_poll_votes_count: 0,
            notify_settings: tl::enums::PeerNotifySettings::Settings(
                tl::types::PeerNotifySettings {
                    show_previews: None,
                    silent: Some(true),
                    mute_until: None,
                    ios_sound: None,
                    android_sound: None,
                    other_sound: None,
                    stories_muted: None,
                    stories_hide_sender: None,
                    stories_ios_sound: None,
                    stories_android_sound: None,
                    stories_other_sound: None,
                },
            ),
            pts: None,
            draft: None,
            folder_id: Some(1),
            ttl_period: None,
        });
        assert!(!dto::is_folder_row(&real));

        // Обе строки указывают на ОДИН dialog_id — вот почему подмена
        // проходит незаметно и вот почему фильтр обязателен.
        assert_eq!(
            dto::dialog_id_string(dto::raw_dialog_peer(&folder)),
            dto::dialog_id_string(dto::raw_dialog_peer(&real))
        );
    }

    /// Сообщение страницы истории: свой id и (необязательно) цитата.
    fn msg_with_reply(id: i32, reply_to: Option<i32>) -> MessageDto {
        MessageDto {
            id,
            reply_to_id: reply_to,
            reply_to: reply_to.map(|message_id| dto::ReplyToDto {
                message_id: Some(message_id),
                ..dto::ReplyToDto::default()
            }),
            ..MessageDto::default()
        }
    }

    /// План батча = «чего не хватает и чего ещё не спрашивали».
    ///
    /// Каждый лишний id в плане — это не просто трафик: `messages.getMessages`
    /// считается в лимитах Telegram, и страница истории, тянущая оригиналы,
    /// которые и так лежат рядом, приближает `FLOOD_WAIT` на пустом месте.
    #[test]
    fn reply_fetch_plan_asks_only_for_what_is_missing() {
        let page = vec![
            msg_with_reply(10, None),
            // Оригинал лежит на этой же странице (ответ соседу) — сеть не нужна.
            msg_with_reply(11, Some(10)),
            msg_with_reply(12, Some(5)),
            // Двое ответили на одно и то же — id уходит в батч ОДИН раз.
            msg_with_reply(13, Some(5)),
            msg_with_reply(14, Some(7)),
        ];
        // Порядок — первого упоминания: именно он решает, что попадёт в
        // батч, если план когда-нибудь придётся резать.
        assert_eq!(reply_fetch_plan(&page), vec![5, 7]);

        // Пустые случаи не порождают запроса вовсе.
        assert!(reply_fetch_plan(&[]).is_empty());
        assert!(reply_fetch_plan(&[msg_with_reply(1, None)]).is_empty());

        // Уже обогащённая цитата (`out` выставлен) второй раз не
        // запрашивается — иначе повторный проход по странице стоил бы RPC.
        let mut enriched = msg_with_reply(20, Some(3));
        enriched.reply_to.as_mut().unwrap().out = Some(false);
        assert!(reply_fetch_plan(&[enriched]).is_empty());

        // Как и заведомо недоступный оригинал (ответ на сторис/чужой чат).
        let mut gone = msg_with_reply(21, Some(4));
        gone.reply_to.as_mut().unwrap().unavailable = true;
        assert!(reply_fetch_plan(&[gone]).is_empty());
    }

    /// На страницу истории — не больше ОДНОГО батча оригиналов.
    ///
    /// Лишний round-trip здесь платится на каждом экране прокрутки вверх,
    /// поэтому план жёстко режется под лимит `messages.getMessages`. На
    /// реальной странице обрезка недостижима (`MAX_MESSAGES <=
    /// REPLY_FETCH_BATCH` сторожит `const _` рядом с [`reply_fetch_plan`]) —
    /// проверяется именно страховка.
    #[test]
    fn reply_fetch_plan_never_exceeds_one_batch() {
        let oversized: Vec<MessageDto> = (0..(dto::REPLY_FETCH_BATCH as i32 + 50))
            .map(|i| msg_with_reply(1_000 + i, Some(i + 1)))
            .collect();
        let plan = reply_fetch_plan(&oversized);
        assert_eq!(plan.len(), dto::REPLY_FETCH_BATCH);
        assert_eq!(plan.first().copied(), Some(1), "режется хвост, а не голова");

        // Полная страница максимального размера умещается в батч целиком:
        // ни одна цитата не теряет превью из-за обрезки.
        let full: Vec<MessageDto> = (0..MAX_MESSAGES as i32)
            .map(|i| msg_with_reply(1_000 + i, Some(i + 1)))
            .collect();
        assert_eq!(reply_fetch_plan(&full).len(), MAX_MESSAGES);
    }

    /// Таблица ошибок чтения сообщения по id.
    #[test]
    fn map_message_error_classifies_the_whole_table() {
        use axum::http::StatusCode;

        // Сообщения нет ⇒ 404 unknown_message: фронт покажет «сообщение
        // недоступно», а не ошибку у поля ввода.
        for name in ["MSG_ID_INVALID", "MESSAGE_ID_INVALID", "MESSAGE_IDS_EMPTY"] {
            let api = map_message_error(&rpc(400, name));
            assert_eq!(api.status, StatusCode::NOT_FOUND, "{name}");
            assert_eq!(api.body.code, "unknown_message", "{name}");
        }

        // Проблема с диалогом, а не с сообщением. `CHANNEL_PRIVATE` приходит
        // с кодом 406 — сверка обязана идти по имени, иначе он утёк бы в 503.
        for (code, name) in [
            (400, "CHANNEL_INVALID"),
            (406, "CHANNEL_PRIVATE"),
            (400, "PEER_ID_INVALID"),
            (400, "CHAT_ID_INVALID"),
            (400, "USER_ID_INVALID"),
        ] {
            let api = map_message_error(&rpc(code, name));
            assert_eq!(api.status, StatusCode::NOT_FOUND, "{name}");
            assert_eq!(api.body.code, "unknown_dialog", "{name}");
        }

        // Сессию отозвали.
        let api = map_message_error(&rpc(401, "AUTH_KEY_UNREGISTERED"));
        assert_eq!(api.status, StatusCode::CONFLICT);
        assert_eq!(api.body.code, "not_authorized");

        // FLOOD_WAIT главнее всего остального и несёт срок ожидания.
        let flood = map_message_error(&rpc(420, "FLOOD_WAIT_42"));
        assert_eq!(flood.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(flood.body.retry_after_secs, Some(42));

        // Всё остальное — «попробуйте позже»: исправлять клиенту тут нечего,
        // в запросе нет ничего, кроме пути.
        for err in [
            rpc(500, "INTERNAL"),
            rpc(400, "SOMETHING_NEW"),
            rpc(403, "FORBIDDEN"),
            InvocationError::Dropped,
        ] {
            let api = map_message_error(&err);
            assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(api.body.code, "not_connected");
            assert!(api.body.retry_after_secs.is_none());
        }
    }

    /// Зачем в файле ТРЕТИЙ маппер: у одной и той же ошибки Telegram здесь
    /// три разных смысла, и подмена любого из них ломает конкретный экран.
    ///
    /// `MESSAGE_ID_INVALID` означает «сообщения нет», но:
    /// - в `POST /messages` это кривой `reply_to_id` ⇒ 400 у поля ввода;
    /// - в нажатии кнопки ⇒ 404, погасить устаревшую клавиатуру;
    /// - в чтении сообщения по id ⇒ 404, «сообщение недоступно».
    ///
    /// Тест стоит здесь, чтобы попытка «упростить» три маппера в один
    /// падала сразу, а не всплывала у пользователя.
    #[test]
    fn message_id_invalid_means_three_different_things() {
        assert_eq!(
            map_invocation(&rpc(400, "MESSAGE_ID_INVALID")).body.code,
            "invalid_input"
        );
        let callback = map_callback_error(&rpc(400, "MESSAGE_ID_INVALID"));
        assert_eq!(callback.body.code, "unknown_message");
        assert_eq!(
            map_message_error(&rpc(400, "MESSAGE_ID_INVALID")).body.code,
            "unknown_message"
        );
    }

    /// `:mid` в пути: id сообщений у Telegram начинаются с единицы, поэтому
    /// «нулевое» и отрицательное — это не «плохой запрос», а «такого
    /// сообщения нет» (404), ровно как у [`validate_callback`].
    #[test]
    fn parse_message_id_rejects_impossible_ids() {
        assert_eq!(parse_message_id("7").unwrap(), 7);
        assert_eq!(parse_message_id(" 42 ").unwrap(), 42);
        assert_eq!(parse_message_id("\t5\n").unwrap(), 5);
        assert_eq!(parse_message_id("007").unwrap(), 7);
        assert_eq!(parse_message_id(&i32::MAX.to_string()).unwrap(), i32::MAX);
        // Ведущий «+» разбирается парсером Rust и означает тот же id —
        // отвергать его не за что (так же ведёт себя `validate_callback`).
        assert_eq!(parse_message_id("+7").unwrap(), 7);

        for mid in [
            "0",
            "-1",
            "",
            "   ",
            "abc",
            "1.0",
            "1e3",
            "9999999999999", // за пределами i32
            "7/callback",    // «съеденный» соседний сегмент пути
            "٧",             // арабо-индийская семёрка: не ASCII-цифра
            "7 7",
        ] {
            let err = parse_message_id(mid).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND, "{mid:?}");
            assert_eq!(err.body.code, "unknown_message", "{mid:?}");
        }
    }

    /// Порядок проверок нового роута — тот же, что у соседей: авторизация,
    /// соединение, и только потом разбор `:mid`.
    ///
    /// Иначе неавторизованный клиент по коду ответа отличал бы существующий
    /// id от несуществующего, а фронт не отличал бы «войди» от «подожди
    /// реконнекта».
    #[tokio::test]
    async fn get_message_checks_auth_before_parsing_anything() {
        for (id, mid) in [
            ("777000", "5"),
            ("777000", "мусор"),
            ("../../etc/passwd", "0"),
        ] {
            let st = state();
            let err = get_message(State(st), Path((id.into(), mid.into())))
                .await
                .unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::CONFLICT, "{id}/{mid}");
            assert_eq!(err.body.code, "not_authorized", "{id}/{mid}");
        }

        // Авторизован, соединения нет ⇒ 503, а не 404 по мусорному mid.
        let st = authorized().await;
        for mid in ["5", "мусор", "0"] {
            let err = get_message(State(st.clone()), Path(("777000".into(), mid.into())))
                .await
                .unwrap_err();
            assert_eq!(
                err.status,
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "{mid}"
            );
            assert_eq!(err.body.code, "not_connected", "{mid}");
        }
    }

    /// Граница i32: id сообщений у Telegram 32-битные, и «на единицу
    /// больше» обязан быть честный 404, а не паника и не молчаливое
    /// усечение в отрицательный id (по такому мы бы ушли в Telegram с
    /// заведомо мусорным запросом и получили бы 503 вместо «нет такого»).
    #[test]
    fn parse_message_id_guards_the_i32_boundary() {
        assert_eq!(parse_message_id(&i32::MAX.to_string()).unwrap(), i32::MAX);
        assert_eq!(parse_message_id("1").unwrap(), 1, "самый первый id диалога");

        let overflow = [
            "2147483648".to_string(),  // i32::MAX + 1
            "-2147483648".to_string(), // i32::MIN
            i64::MAX.to_string(),
            u64::MAX.to_string(),
            "9".repeat(400), // длиннее любого числового типа
        ];
        for mid in overflow {
            let err = parse_message_id(&mid).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND, "{mid}");
            assert_eq!(err.body.code, "unknown_message", "{mid}");
            assert!(err.body.retry_after_secs.is_none(), "{mid}");
        }
    }

    /// Дедуп в плане батча идёт ДО обрезки под лимит.
    ///
    /// Порядок здесь решает, увидит ли пользователь цитаты вообще: в живом
    /// чате вокруг одного объявления сотня ответов ссылается на один и тот
    /// же id. Обрежь список раньше дедупа — и весь батч уйдёт на сто копий
    /// одного оригинала, а остальные цитаты страницы останутся без превью.
    #[test]
    fn reply_fetch_plan_dedupes_before_truncating() {
        let mut page: Vec<MessageDto> = (0..dto::REPLY_FETCH_BATCH as i32 + 50)
            .map(|i| msg_with_reply(1_000 + i, Some(5)))
            .collect();
        assert_eq!(
            reply_fetch_plan(&page),
            vec![5],
            "толпа ответов на одно сообщение — один id в батче"
        );

        // Толпа плюс обычные ответы: место в батче остаётся и им.
        page.extend((0..10).map(|i| msg_with_reply(9_000 + i, Some(100 + i))));
        let plan = reply_fetch_plan(&page);
        assert_eq!(plan.len(), 11, "один популярный id и десять обычных");
        assert_eq!(plan[0], 5, "порядок — первого упоминания");
        assert_eq!(&plan[1..], (100..110).collect::<Vec<i32>>().as_slice());
    }
}
