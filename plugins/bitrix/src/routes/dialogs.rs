//! Данные списка диалогов и лента сообщений.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET | `/api/bitrix/filters` | четыре директории одного списка + счётчики (Р2) |
//! | GET | `/api/bitrix/dialogs` | страница списка: `?limit&offset&filter` |
//! | GET | `/api/bitrix/dialogs/:id/messages` | страница истории: `?limit&before_id&after_id` |
//! | GET | `/api/bitrix/messages/:mid` | одно сообщение: `?dialog_id` |
//! | POST | `/api/bitrix/dialogs/:id/messages` | отправка текста (Р9, Р10, Р12) |
//! | PATCH | `/api/bitrix/messages/:mid` | правка текста |
//! | DELETE | `/api/bitrix/messages/:mid` | удаление |
//! | POST | `/api/bitrix/messages/:mid/like` | «лайк» — единственная реакция портала |
//! | POST | `/api/bitrix/messages/:mid/command` | команда бота (кнопка `COMMAND`, forge-zw3d) |
//! | POST | `/api/bitrix/dialogs/:id/read` | отметка о прочтении (окно 800 мс) |
//! | POST | `/api/bitrix/dialogs/:id/unread` | ручное «непрочитано» (`message_id` ОБЯЗАТЕЛЕН) |
//! | POST | `/api/bitrix/dialogs/:id/typing` | «печатает…» (окно 8 с) |
//! | POST | `/api/bitrix/dialogs/:id/pin` | закрепить/открепить (Р14) |
//! | POST | `/api/bitrix/dialogs/:id/hide` | скрыть из списка (Р14) |
//!
//! # ЦИКЛ ПЕРЕПИСКИ: четыре решения, которые нельзя потерять
//!
//! 1. **Отправка НЕ ПОВТОРЯЕТСЯ (Р9).** Запрет живёт в транспорте
//!    ([`crate::api::NO_RETRY_METHODS`]), а роут лишь честно отдаёт `503`:
//!    решение «повторить» принимает пользователь кнопкой на строке. Причина —
//!    отсутствие дедуп-идентификатора у `im.message.add`: повтор после обрыва
//!    это второе сообщение у собеседника, а не то же самое.
//! 2. **Длина проверяется ЗДЕСЬ (Р12).** Портал принимает текст любой длины,
//!    отвечает успехом и МОЛЧА режет его на 20 006 символах. «Успешно
//!    отправленный» обрубок хуже честного отказа: отправитель уверен, что
//!    собеседник прочитал всё. Поэтому >[`crate::dto::MAX_MESSAGE_CHARS`] —
//!    это `400 invalid_input` ДО единого сетевого вызова.
//! 3. **Ответ дублируется текстовой цитатой (Р10).** `REPLY_ID` уходит, но при
//!    ЧТЕНИИ портал его не отражает (`params` пустые, `reply_to` в DTO всегда
//!    `null`). Без цитаты ответ визуально теряется — поэтому её добавляет
//!    сервер ([`compose_reply`]), а не фронт: формат обязан быть один на все
//!    вкладки и версии кэшированных модулей.
//! 4. **После действия строка ПЕРЕЧИТЫВАЕТСЯ у портала** ([`reread_message`]).
//!    Локально собранная строка врала бы трижды: `URL_PREVIEW` переписывает
//!    текст, `IS_EDITED` ставит портал, а состав `LIKE` вообще известен только
//!    ему. Цена — второй запрос на действие; отказ перечитывания при этом НЕ
//!    делает действие неуспешным (см. [`MessageResp::echoed`]).
//!
//! # ПОРЯДОК ИСТОРИИ: портал отдаёт её ДВУМЯ РАЗНЫМИ способами
//!
//! ```text
//!   базовый вызов          → по УБЫВАНИЮ id   (новые первыми)
//!   LAST_ID   (вверх)      → по УБЫВАНИЮ id
//!   FIRST_ID  (вниз)       → по ВОЗРАСТАНИЮ id      ← вот здесь и ломается
//! ```
//!
//! Это не причуда одной ревизии, а поведение метода, снятое живьём. Наружу
//! страница уезжает ВСЕГДА по возрастанию
//! ([`crate::dto::sort_messages_ascending`]): лента рисуется сверху вниз от
//! старого к новому, и знание про два порядка означало бы во фронте две ветки
//! вставки, вторая из которых проверяется только при догрузке снизу — то есть
//! почти никогда.
//!
//! # ОДИН список, четыре директории (Р2, Вариант A)
//!
//! Директории `~/all`, `~/chats`, `~/tasks`, `~/unread` — это НЕ серверные
//! фильтры (как у VK, где каждая вкладка колонки означает отдельный запрос) и
//! НЕ папки (у портала их нет вовсе). Портал отдаёт ОДИН список, а разделение
//! выводится из полей строки: `kind == "task"` для задач, `unread_count > 0`
//! для непрочитанных. Поэтому переключение директории на фронте — локальная
//! операция, и лишнего запроса она не стоит; серверная фильтрация здесь нужна
//! только для корректной пагинации по выбранной директории.
//!
//! # Два источника списка (Р3)
//!
//! ```text
//!   im.recent.get   — ОСНОВА: отдаёт всё, включая чаты задач; страниц не умеет
//!         +
//!   im.recent.list  — ДОЗАГРУЗКА: умеет страницы, но чатов задач НЕ отдаёт
//! ```
//!
//! Ни один из них в одиночку не даёт полного списка. Основа — первый; второй
//! подключается, только когда первый пришёл «под завязку»
//! ([`RECENT_GET_SATURATION`]) и запрошенное окно за его пределами. Склейка —
//! [`crate::dto::merge_dialogs`], приоритет у `im.recent.get`.
//!
//! # ЛЕСТНИЦА ПРОВЕРОК: 409 → 503 → 404 → 400
//!
//! Порядок жёсткий и одинаковый во всех data-роутах:
//!
//! 1. **409** — вебхук не подключён ([`require_authorized`]);
//! 2. **503** — портал недоступен прямо сейчас ([`require_connected`]);
//! 3. **404** — запрошенного объекта нет ([`require_dialog_id`]: `:id`, не
//!    похожий ни на одну из трёх форм платформы, — это «диалога нет», а не
//!    «параметр плохой»);
//! 4. **400** — и только теперь разбор параметров.
//!
//! Смысл порядка — не в красоте, а в том, что **враждебный ввод не должен
//! доходить до парсера, пока не пройдена авторизация**. Иначе роут превращается
//! в оракул: по коду ответа (400 против 409) неподключённый клиент узнаёт,
//! какие идентификаторы портал считает валидными.
//!
//! Отсюда следствие, которое легко потерять при рефакторинге: **query
//! разбирается ВРУЧНУЮ** ([`query_i64`], [`query_str`]), а не типизированным
//! `Query<T>`-экстрактором. Экстрактор отрабатывает ДО тела хендлера и вернул
//! бы `400` раньше `409`, сломав лестницу молча — сборка при этом не упала бы.
//! На это есть тест-страж `hostile_dialog_ids_hit_auth_before_any_parsing`.

use std::sync::Arc;

use axum::extract::{Path, RawQuery};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::actions::{now_ms, READ_DEBOUNCE_MS, TYPING_INTERVAL_MS};
use crate::api::methods::LikeAction;
use crate::api::{to_api_error, B24Client, B24Error};
use crate::dto::{self, CountersDto, DialogDto, MessageDto};
use crate::registry::B24Registry;
use crate::routes::error::ApiError;
use crate::routes::Portal;
use crate::state::B24State;

/// Директории одного списка (Р2). Порядок — порядок показа в колонке.
///
/// Ключи заморожены: они уезжают в `?filter=`, в `data-dir` разметки и в
/// настройки фронта. Переименование обязано валить тест, а не тихо ломать
/// колонку.
pub const DIRS: [(&str, &str); 4] = [
    ("all", "~/all"),
    ("chats", "~/chats"),
    ("tasks", "~/tasks"),
    ("unread", "~/unread"),
];

/// Сколько строк отдаётся по умолчанию.
pub const DEFAULT_LIMIT: i64 = 40;

/// Потолок страницы списка.
///
/// Совпадает с платформенным потолком `im.recent.list` (200): просить больше
/// бессмысленно, а отдавать больше — значит держать в памяти страницу, которую
/// колонка всё равно рисует по мере прокрутки.
pub const MAX_LIMIT: i64 = 200;

/// Сколько строк `im.recent.get` считается «пришёл под завязку».
///
/// У метода нет ни `LIMIT`, ни страниц, но и бесконечным его ответ не бывает:
/// на длинных списках портал отдаёт верхушку. Ровно двести — граница, после
/// которой нельзя считать, что список кончился, и подключается дозагрузка
/// через `im.recent.list` (Р3).
pub const RECENT_GET_SATURATION: usize = 200;

/// Сколько страниц `im.recent.list` разрешено дозагрузить за один запрос.
///
/// Потолок нужен, чтобы `?offset=100000` не превратился в полсотни обращений к
/// порталу: бюджет 2 rps такой запрос выел бы целиком, а показать всё равно
/// нечего.
pub const MAX_EXTRA_PAGES: usize = 3;

/// Сколько секунд снимок счётчиков считается свежим.
///
/// Тридцать секунд закрывают типичную последовательность фронта «загрузить
/// список → загрузить директории»: счётчики приезжают вместе со списком одним
/// `batch` (Р3), и `GET /filters` сразу после этого не стоит ни одного
/// запроса.
pub const COUNTERS_TTL_SECS: i64 = 30;

/// Ответ `GET /api/bitrix/filters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiltersResp {
    /// Директории в порядке показа.
    pub filters: Vec<FilterDto>,
    /// Полная разбивка счётчиков — источник бейджа вкладки.
    pub counters: CountersDto,
}

/// Одна директория колонки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterDto {
    /// Машинный ключ (`all` / `chats` / `tasks` / `unread`) — он же значение
    /// `?filter=`.
    pub key: String,
    /// Подпись в колонке.
    pub title: String,
    /// Сколько непрочитанного попадает в эту директорию.
    pub unread: i64,
}

/// Ответ `GET /api/bitrix/dialogs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogsResp {
    /// Строки страницы.
    pub dialogs: Vec<DialogDto>,
    /// Есть ли ещё строки за этой страницей.
    pub has_more: bool,
}

/// Сколько сообщений отдаётся по умолчанию.
///
/// Совпадает с потолком платформы: страница истории — это один запрос из
/// бюджета в 2 rps, и просить меньше максимума значит платить тем же запросом
/// за меньшее число строк.
pub const MESSAGES_DEFAULT_LIMIT: i64 = crate::api::methods::MESSAGES_MAX_LIMIT;

/// Ответ `GET /api/bitrix/dialogs/:id/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagesResp {
    /// Страница истории — ВСЕГДА по возрастанию `id` (см. шапку модуля).
    pub messages: Vec<MessageDto>,
    /// Могут ли быть ещё сообщения в запрошенном направлении.
    ///
    /// Считается как «страница пришла полной». Точного признака у портала нет
    /// вовсе: `im.dialog.messages.get` не отдаёт ни `total`, ни `hasMore`, а
    /// фильтрация у него идёт после выборки. Неполная страница означает конец
    /// истории надёжно, полная — «возможно, есть ещё», и цена ошибки здесь
    /// один лишний запрос при прокрутке, а не потерянные сообщения.
    pub has_more: bool,
}

/// Тело `POST /api/bitrix/dialogs/:id/messages`.
///
/// Цитата приходит от фронта ГОТОВЫМИ полями (`reply_author`, `reply_text`), а
/// не добирается сервером по `reply_to`: строка, на которую отвечают, УЖЕ
/// нарисована в ленте, и повторный запрос к порталу ради её текста стоил бы
/// одного из двух запросов в секунду ровно в тот момент, когда пользователь
/// ждёт отправки. Собирает цитату всё равно сервер ([`compose_reply`]) — формат
/// обязан быть один.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SendReq {
    /// Текст сообщения.
    #[serde(default)]
    pub text: String,
    /// Идентификатор сообщения, на которое отвечаем (Р10).
    #[serde(default)]
    pub reply_to: Option<i64>,
    /// Автор цитируемого сообщения — для текстовой цитаты.
    #[serde(default)]
    pub reply_author: Option<String>,
    /// Текст цитируемого сообщения — для текстовой цитаты.
    #[serde(default)]
    pub reply_text: Option<String>,
    /// Идентификатор принятой загрузки (Ф9): сообщение отправляется ФАЙЛОМ.
    ///
    /// Необязательное поле, а не отдельный роут: `im.v2.File.upload` — это
    /// «залей файл И отправь сообщение» одним вызовом, поэтому у файла и у
    /// текста один и тот же путь отправки и один и тот же ответ. `text` при
    /// этом становится подписью и МОЖЕТ быть пустым ([`require_caption`]).
    ///
    /// Идентификатор ОДНОРАЗОВЫЙ: он потребляется реестром загрузок
    /// ([`crate::routes::upload::take_upload`]), и второй запрос с тем же
    /// значением получит `409 invalid_state` вместо второго файла в чате.
    #[serde(default)]
    pub upload_id: Option<String>,
}

/// Тело `PATCH /api/bitrix/messages/:mid`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EditReq {
    /// Новый текст. Пустой ОТВЕРГАЕТСЯ: у портала пустой `MESSAGE` удаляет
    /// сообщение, а удаление обязано быть явным действием.
    #[serde(default)]
    pub text: String,
    /// Диалог сообщения — нужен, чтобы перечитать строку после правки.
    #[serde(default)]
    pub dialog_id: Option<String>,
}

/// Тело `POST /api/bitrix/messages/:mid/like`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LikeReq {
    /// `auto` (по умолчанию), `plus` или `minus`.
    #[serde(default)]
    pub action: Option<String>,
    /// Диалог сообщения — нужен, чтобы перечитать строку после лайка.
    #[serde(default)]
    pub dialog_id: Option<String>,
}

/// Тело `POST /api/bitrix/messages/:mid/command` (forge-zw3d).
///
/// Поля дословно повторяют то, что уже приехало фронту в `ButtonDto`
/// (`bot_id`/`command`/`command_params` — см. `attach::button_action::COMMAND`):
/// сервер их не запоминает по сообщению, поэтому клавиатура-кнопка отдаёт их
/// обратно, а не заставляет сервер лезть в портал заново, чтобы узнать
/// собственную клавиатуру.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandReq {
    /// `BOT_ID` кнопки — обязателен, `im.message.command` без него не
    /// выполнить.
    pub bot_id: i64,
    /// `COMMAND` кнопки.
    pub command: String,
    /// `COMMAND_PARAMS` кнопки, если был.
    #[serde(default)]
    pub command_params: Option<String>,
}

/// Тело `POST /api/bitrix/dialogs/:id/read` и `…/unread`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MarkReq {
    /// Граница отметки. Для `read` НЕобязательна (без неё читается весь
    /// диалог), для `unread` — ОБЯЗАТЕЛЬНА (ловушка платформы).
    #[serde(default)]
    pub message_id: Option<i64>,
}

/// Тело `POST /api/bitrix/dialogs/:id/pin`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PinReq {
    /// `true` — закрепить (значение по умолчанию), `false` — открепить.
    #[serde(default)]
    pub pinned: Option<bool>,
}

/// Ответ роутов, меняющих ОДНО сообщение (отправка, правка, лайк).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageResp {
    /// Идентификатор сообщения у портала — им фронт заменяет оптимистичную
    /// строку.
    pub id: i64,
    /// Строка после действия. `None` означает «действие удалось, а перечитать
    /// строку не вышло» — фронт в этом случае оставляет то, что нарисовал сам.
    pub message: Option<MessageDto>,
    /// Строка ПЕРЕЧИТАНА у портала (а не собрана локально).
    ///
    /// Различие видно только здесь и важно ровно одним: у перечитанной строки
    /// текст канонический (портал переписывает ссылки при `URL_PREVIEW`), метка
    /// правки и состав лайков — настоящие.
    pub echoed: bool,
}

/// Ответ роутов-команд: удаление, отметки, «печатает…», pin/hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckResp {
    /// Действие выполнено (или осознанно подавлено — см. `sent`).
    pub ok: bool,
    /// Вызов действительно ушёл в портал.
    ///
    /// `false` у `read`/`typing` означает «подавлено окном троттлинга»
    /// ([`crate::actions`]) — это НЕ ошибка: жест пользователя порождает
    /// десятки таких вызовов в секунду, а бюджет портала — два запроса.
    pub sent: bool,
}

/// Роуты данных: список (Ф4), лента (Ф5) и цикл переписки (Ф6).
///
/// # Почему правка, удаление и лайк адресуются `/messages/:mid`, а не
/// `/dialogs/:id/messages/:mid`
///
/// Портал адресует эти три операции ОДНИМ `MESSAGE_ID`: ни `im.message.update`,
/// ни `im.message.delete`, ни `im.message.like` не принимают диалога вовсе.
/// Обязательный сегмент `:id` в пути был бы фикцией — сервер не смог бы его
/// проверить (сообщение в чужом диалоге портал отвергнет сам) и лишь создал бы
/// иллюзию, что операция ограничена этим диалогом. Диалог всё же нужен, но
/// только ради ПЕРЕЧИТЫВАНИЯ строки, поэтому он и приезжает необязательным
/// полем тела — ровно как `?dialog_id` у `GET /api/bitrix/messages/:mid`.
pub fn router() -> Router<Arc<B24Registry>> {
    Router::new()
        .route("/api/bitrix/filters", get(get_filters))
        .route("/api/bitrix/dialogs", get(get_dialogs))
        .route(
            "/api/bitrix/dialogs/:id/messages",
            get(get_messages).post(post_message),
        )
        .route("/api/bitrix/dialogs/:id/read", post(post_read))
        .route("/api/bitrix/dialogs/:id/unread", post(post_unread))
        .route("/api/bitrix/dialogs/:id/typing", post(post_typing))
        .route("/api/bitrix/dialogs/:id/pin", post(post_pin))
        .route("/api/bitrix/dialogs/:id/hide", post(post_hide))
        .route(
            "/api/bitrix/messages/:mid",
            get(get_message).patch(patch_message).delete(delete_message),
        )
        .route("/api/bitrix/messages/:mid/like", post(post_like))
        .route("/api/bitrix/messages/:mid/command", post(post_command))
}

// ---------------------------------------------------------------------------
// Лестница проверок.
// ---------------------------------------------------------------------------

/// Ступень 1 (409): портал подключён?
///
/// В сообщение уезжает ИМЯ состояния автомата — фронту этого достаточно, чтобы
/// показать нужный экран, а серверу — чтобы не гадать по логам, почему
/// пользователь видит 409.
pub(crate) async fn require_authorized(state: &Arc<B24State>) -> Result<(), ApiError> {
    if state.is_authorized().await {
        return Ok(());
    }
    let kind = state.login.lock().await.kind();
    Err(ApiError::not_authorized(format!(
        "Bitrix24 is not connected (login state '{kind}')"
    )))
}

/// Ступень 2 (503): связь есть?
///
/// Постоянного соединения у REST нет, поэтому смысл 503 наполняет
/// [`crate::state::ConnHealth`]: отказ считается «связи нет» ровно
/// [`crate::state::HEALTH_GRACE_SECS`] секунд, после чего запросы снова
/// пропускаются — иначе чинить здоровье было бы нечем.
pub(crate) async fn require_connected(state: &Arc<B24State>) -> Result<(), ApiError> {
    let now = dto::now_unix();
    if state.connected(now).await {
        return Ok(());
    }
    let reason = state
        .health()
        .await
        .down_reason(now)
        .unwrap_or_else(|| "the last call to the portal failed".to_string());
    Err(ApiError::not_connected(format!(
        "no connection to Bitrix24 right now: {reason}"
    )))
}

/// Клиент текущего вебхука (один на секрет ⇒ один троттлер).
pub(crate) async fn require_client(state: &Arc<B24State>) -> Result<Arc<B24Client>, ApiError> {
    state.api_client().await.map_err(|e| {
        tracing::warn!(target: "forge_bitrix", error = %e, "forge-bitrix: could not build the API client");
        ApiError::not_connected(format!("could not build the Bitrix24 client: {e}"))
    })
}

/// Ступени 1–2 разом: именно в этом порядке и ни в каком другом.
pub(crate) async fn require_ready(state: &Arc<B24State>) -> Result<Arc<B24Client>, ApiError> {
    require_authorized(state).await?;
    require_connected(state).await?;
    require_client(state).await
}

/// Ступень 3 (404): `:id` из URL — это вообще диалог?
///
/// Отвечает `404 unknown_dialog`, а НЕ `400`: для фронта «такого диалога нет»
/// и «идентификатор не разбирается» — одна и та же ветка, а разные коды
/// превратили бы роут в оракул валидности идентификаторов портала.
///
/// В сообщение враждебная строка НЕ попадает: ответ не должен работать
/// рефлектором чужого ввода.
pub(crate) fn require_dialog_id(raw: &str) -> Result<String, ApiError> {
    dto::parse_dialog_id(raw)
        .ok_or_else(|| ApiError::unknown_dialog("no such dialog in this portal".to_string()))
}

/// Ступень 3 (404) для идентификатора сообщения.
///
/// Идентификаторы у портала растут шагом 2 и всегда положительны; всё
/// остальное — «такого сообщения нет».
pub(crate) fn require_message_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::unknown_message("no such message in this portal".to_string()))
}

/// Ошибка портала → ошибка REST.
pub(crate) fn map_portal_error(err: &B24Error) -> ApiError {
    to_api_error(err, dto::now_unix())
}

// ---------------------------------------------------------------------------
// Разбор query — ВРУЧНУЮ и ПОСЛЕ проверок.
// ---------------------------------------------------------------------------

/// Сырое значение параметра query-строки.
///
/// Без percent-декодирования намеренно: все наши параметры — целые числа и
/// слова из закрытого списка. `filter` всё равно проходит через
/// [`normalize_dir`], который незнакомое значение приводит к `all`, а декодер
/// означал бы новую зависимость ради случая, которого нет.
///
/// При дублирующихся ключах (`?limit=1&limit=2`) берётся ПЕРВОЕ значение —
/// решение произвольное, но зафиксированное тестом: важно, что оно
/// детерминировано и не зависит от порядка обхода.
pub fn query_str<'a>(raw: Option<&'a str>, key: &str) -> Option<&'a str> {
    raw?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Числовой параметр query.
///
/// Пустое значение (`?limit=`) трактуется как отсутствие: `URLSearchParams`
/// умеет оставлять пустой хвост, и отвечать на него 400 значило бы ломать
/// вкладку на ровном месте. Всё остальное, что не разбирается в целое, —
/// ошибка клиента.
///
/// # Errors
///
/// `400 invalid_input` — значение есть, но это не целое число.
pub fn query_i64(raw: Option<&str>, key: &str) -> Result<Option<i64>, ApiError> {
    let Some(v) = query_str(raw, key) else {
        return Ok(None);
    };
    if v.is_empty() {
        return Ok(None);
    }
    v.parse::<i64>()
        .map(Some)
        .map_err(|_| ApiError::bad_input(format!("`{key}` must be an integer, got '{v}'")))
}

/// Приводит директорию к известной; незнакомая становится `all`.
///
/// Отказ (400) здесь был бы вреднее: значение приходит из настроек фронта,
/// которые браузер держит в кэше PWA неделями, и переименование директории в
/// будущем превратило бы старую вкладку в пустой экран с ошибкой вместо
/// полного списка.
pub fn normalize_dir(raw: Option<&str>) -> &'static str {
    let raw = raw.unwrap_or("all");
    DIRS.iter()
        .find(|(key, _)| *key == raw)
        .map_or("all", |(key, _)| *key)
}

/// Попадает ли строка в директорию.
///
/// Чистая табличная функция — ровно она же (построчно) живёт во фронте
/// (`js/bitrix/dialogs.js`), поэтому серверная страница и локальное
/// переключение директории обязаны давать одинаковый набор строк.
pub fn dir_matches(dir: &str, d: &DialogDto) -> bool {
    match dir {
        "chats" => d.kind != dto::kind::TASK,
        "tasks" => d.kind == dto::kind::TASK,
        "unread" => d.unread_count > 0,
        // `all` и всё незнакомое — весь список.
        _ => true,
    }
}

/// Счётчик непрочитанного по директории.
fn dir_unread(dir: &str, c: &CountersDto) -> i64 {
    match dir {
        "chats" => c.chat + c.dialog,
        "tasks" => c.tasks,
        "unread" => c.dialogs_unread(),
        _ => c.total,
    }
}

// ---------------------------------------------------------------------------
// Хендлеры.
// ---------------------------------------------------------------------------

/// `GET /api/bitrix/filters` — директории колонки и счётчики.
///
/// Счётчики берутся из снимка, оставленного холодным стартом (Р3), и стоят
/// сетевого вызова только если снимок протух ([`COUNTERS_TTL_SECS`]).
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, плюс всё, чем может ответить
/// портал на `im.counters.get`.
pub async fn get_filters(
    Portal(state): Portal,
) -> Result<Json<FiltersResp>, ApiError> {
    let client = require_ready(&state).await?;
    let now = dto::now_unix();

    let counters = match state.counters_fresh(now, COUNTERS_TTL_SECS).await {
        Some(c) => c,
        None => {
            let raw = client.counters().await.map_err(|e| map_portal_error(&e))?;
            let c = CountersDto::from_raw(&raw);
            state.set_counters(c, now).await;
            c
        }
    };

    Ok(Json(FiltersResp {
        filters: DIRS
            .iter()
            .map(|(key, title)| FilterDto {
                key: (*key).to_string(),
                title: (*title).to_string(),
                unread: dir_unread(key, &counters),
            })
            .collect(),
        counters,
    }))
}

/// `GET /api/bitrix/dialogs?limit&offset&filter` — страница списка.
///
/// # Сколько это стоит
///
/// В обычном случае — ОДИН запрос: `batch{im.recent.get + im.counters.get}`
/// (Р3), после которого `GET /filters` отвечает из снимка бесплатно. Второй
/// запрос появляется только у длинных списков (дозагрузка `im.recent.list`) и
/// третий — когда в строках нашлись авторы, чьих имён нет в кэше (один `batch`
/// на всех сразу).
///
/// # Почему сортировка серверная
///
/// Закреплённые диалоги обязаны идти первыми, а страница — оставаться
/// устойчивой между запросами: если бы пины поднимал фронт, вторая страница
/// содержала бы строки, уже показанные на первой.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `400 invalid_input` (мусор в
/// `limit`/`offset`), плюс ошибки портала.
pub async fn get_dialogs(
    Portal(state): Portal,
    RawQuery(raw): RawQuery,
) -> Result<Json<DialogsResp>, ApiError> {
    // Ступени 1–2. Разбор параметров — ПОСЛЕ них и только после них.
    let client = require_ready(&state).await?;

    let q = raw.as_deref();
    let limit = query_i64(q, "limit")?
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    let offset = query_i64(q, "offset")?.max(Some(0)).unwrap_or(0).max(0);
    let dir = normalize_dir(query_str(q, "filter"));

    // --- источник 1: основа списка + счётчики одним пакетом (Р3) ---
    let now = dto::now_unix();
    let (mut items, counters) = client.cold_start().await.map_err(|e| map_portal_error(&e))?;
    state
        .set_counters(CountersDto::from_raw(&counters), now)
        .await;
    let base_len = items.len();

    // --- источник 2: дозагрузка, только если основа пришла под завязку ---
    let mut extra = Vec::new();
    if base_len >= RECENT_GET_SATURATION {
        let needed = (offset + limit) as usize;
        let mut page_offset = base_len as i64;
        for _ in 0..MAX_EXTRA_PAGES {
            if base_len + extra.len() >= needed {
                break;
            }
            let page = client
                .recent_list(MAX_LIMIT, page_offset)
                .await
                .map_err(|e| map_portal_error(&e))?;
            let got = page.items.len();
            extra.extend(page.items);
            // `LIMIT` НЕ гарантирует ровно N записей (портал фильтрует после
            // выборки), поэтому признак конца — только `hasMore`, а смещение
            // двигается на реально пришедшее число.
            if !page.has_more || got == 0 {
                break;
            }
            page_offset += got as i64;
        }
    }

    // --- имена: сначала бесплатно, потом одним доборным пакетом ---
    let users: Vec<crate::api::schema::UserRaw> = items
        .iter()
        .chain(extra.iter())
        .map(|i| i.user.clone())
        .collect();
    state.peers.absorb_users(&users);

    let wanted: Vec<i64> = items
        .iter()
        .chain(extra.iter())
        .flat_map(|i| {
            [
                i.message.author_id,
                i.user.id.as_i64().unwrap_or_default(),
            ]
        })
        .collect();
    if let Err(e) = state.peers.resolve_uncached(&client, &wanted).await {
        // Имена — украшение: страница списка из-за них не падает, подписью
        // станет идентификатор.
        tracing::debug!(target: "forge_bitrix", error = %e, "forge-bitrix: peer names left unresolved");
    }

    // --- проекция, директория, сортировка, страница ---
    let self_id = state.self_id().await;
    let peers = &state.peers;
    let project = |list: &mut Vec<crate::api::schema::RecentItemRaw>| -> Vec<DialogDto> {
        std::mem::take(list)
            .iter()
            .filter_map(|i| dto::dialog_from_raw(i, self_id, peers))
            .collect()
    };
    let mut dialogs = dto::merge_dialogs(project(&mut items), project(&mut extra));

    dialogs.retain(|d| dir_matches(dir, d));
    // Закреплённые сверху, дальше — по дате изменения (портал уже отдаёт их в
    // этом порядке, но полагаться на это нельзя: строки приехали из двух
    // источников).
    dialogs.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.date_update.cmp(&a.date_update))
    });

    let total = dialogs.len() as i64;
    let page: Vec<DialogDto> = dialogs
        .into_iter()
        .skip(offset.max(0) as usize)
        .take(limit as usize)
        .collect();
    let has_more = offset + (page.len() as i64) < total;

    tracing::debug!(
        target: "forge_bitrix",
        count = page.len(),
        dir,
        offset,
        limit,
        base_len,
        extra_pages = usize::from(!extra.is_empty()),
        "forge-bitrix: dialogs listed"
    );
    Ok(Json(DialogsResp {
        dialogs: page,
        has_more,
    }))
}

// ---------------------------------------------------------------------------
// Лента сообщений.
// ---------------------------------------------------------------------------

/// Загружает страницу истории и превращает её в DTO.
///
/// Общая часть обоих ленточных роутов: страница и одиночное сообщение
/// отличаются только границами запроса, а всё остальное — добор имён,
/// проекция, нормализация порядка — обязано делаться одинаково. Разъехавшись,
/// эти две ветки дали бы realtime-строку (Ф7) без имени автора при живой
/// строке из истории с именем.
pub(crate) async fn load_page(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    dialog_id: &str,
    limit: i64,
    before_id: Option<i64>,
    after_id: Option<i64>,
) -> Result<Vec<MessageDto>, ApiError> {
    let page = client
        .dialog_messages(dialog_id, limit, before_id, after_id)
        .await
        .map_err(|e| map_portal_error(&e))?;

    // `users[]` живьём ЧАЩЕ ВСЕГО ПУСТ — это не запасной источник имён, а
    // бесплатный бонус: то, что в нём приехало, кладём в кэш и не спрашиваем
    // повторно.
    let users = page.users.clone().into_values();
    state.peers.absorb_users(&users);

    let authors: Vec<i64> = page
        .messages
        .iter()
        .map(|m| m.author_id)
        .filter(|id| *id > 0)
        .collect();
    if let Err(e) = state.peers.resolve_uncached(client, &authors).await {
        // Имена — украшение: страница истории из-за них не падает, подписью
        // станет идентификатор.
        tracing::debug!(target: "forge_bitrix", error = %e, "forge-bitrix: message authors left unresolved");
    }

    let files = page.files.clone().into_values();
    let index = crate::attach::index_files(&files);
    let self_id = state.self_id().await;
    let peers = &state.peers;

    let mut messages = dto::sort_messages_ascending(
        page.messages
            .iter()
            .map(|m| dto::message_from_raw(m, dialog_id, self_id, &index, peers))
            .collect(),
    );
    // Огибающая голосовых: портал её не отдаёт вовсе, её посчитал бэкенд при
    // скачивании файла. Читается ТОЛЬКО для аудио-вложений — у остальных видов
    // сайдкара нет по построению, и диск не трогается.
    crate::voice::attach_waveforms(&state.config, dialog_id, &mut messages);
    Ok(messages)
}

/// `GET /api/bitrix/dialogs/:id/messages?limit&before_id&after_id` — страница
/// истории.
///
/// # Направления
///
/// - `before_id` (`LAST_ID`) — листаем ВВЕРХ: строго старее указанного;
/// - `after_id` (`FIRST_ID`) — догружаем НИЗ: строго новее указанного;
/// - ничего — хвост диалога.
///
/// Обе границы разом трактовать нечем, поэтому побеждает `before_id` (см.
/// [`crate::api::methods::dialog_messages_params`]).
///
/// # Сколько это стоит
///
/// Один запрос к порталу, плюс ещё один — если среди авторов страницы нашлись
/// неизвестные кэшу (одним `batch` на всех сразу).
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog` (`:id` не
/// похож ни на одну форму платформы), `400 invalid_input` (мусор в
/// `limit`/`before_id`/`after_id`), плюс ошибки портала.
pub async fn get_messages(
    Portal(state): Portal,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<MessagesResp>, ApiError> {
    // Лестница: 409 → 503 → 404 → 400. Порядок ЖЁСТКИЙ.
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;

    let q = raw.as_deref();
    // Клампится МОЛЧА: `limit` приходит от фронта, и ошибка на ровном месте
    // означала бы пустую ленту вместо пятидесяти сообщений.
    let limit = query_i64(q, "limit")?
        .unwrap_or(MESSAGES_DEFAULT_LIMIT)
        .clamp(1, crate::api::methods::MESSAGES_MAX_LIMIT);
    let before_id = query_i64(q, "before_id")?.filter(|v| *v > 0);
    let after_id = query_i64(q, "after_id")?.filter(|v| *v > 0);

    let messages = load_page(&state, &client, &dialog_id, limit, before_id, after_id).await?;
    let has_more = messages.len() as i64 >= limit;

    tracing::debug!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        count = messages.len(),
        limit,
        before_id,
        after_id,
        "forge-bitrix: history page served"
    );
    Ok(Json(MessagesResp { messages, has_more }))
}

/// `GET /api/bitrix/messages/:mid?dialog_id=…` — одно сообщение.
///
/// # Почему диалог обязателен
///
/// Метода «дай сообщение по идентификатору» у портала НЕТ вовсе: вся история
/// адресуется `DIALOG_ID`. Сообщение достаётся страницей на одну строку
/// (`LAST_ID = mid + 1`, `LIMIT = 1`): граница строгая (`id < LAST_ID`),
/// поэтому в ответ приходит либо сам `mid`, либо ближайшее к нему старое — и
/// второе означает, что запрошенного сообщения нет (удалено или чужое).
///
/// Прибавка единицы безопасна: идентификаторы у портала растут ШАГОМ 2, между
/// `mid` и `mid + 1` не может оказаться чужого сообщения.
///
/// Нужен роут догрузке realtime (Ф7): событие приносит идентификатор, а
/// перезагружать из-за него всю ленту — расточительство.
///
/// # Errors
///
/// `409`, `503`, `404 unknown_dialog` (нет или неразбираем `?dialog_id`),
/// `404 unknown_message` (`:mid` не число либо сообщения нет), `400` (мусор в
/// query), плюс ошибки портала.
pub async fn get_message(
    Portal(state): Portal,
    Path(mid): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<MessageDto>, ApiError> {
    let client = require_ready(&state).await?;
    let message_id = require_message_id(&mid)?;
    let dialog_id = require_dialog_id(query_str(raw.as_deref(), "dialog_id").unwrap_or_default())?;

    let page = load_page(
        &state,
        &client,
        &dialog_id,
        1,
        Some(message_id.saturating_add(1)),
        None,
    )
    .await?;

    page.into_iter()
        .find(|m| m.id == message_id)
        .map(Json)
        .ok_or_else(|| ApiError::unknown_message("no such message in this dialog".to_string()))
}

// ---------------------------------------------------------------------------
// Цикл переписки (Ф6).
// ---------------------------------------------------------------------------

/// Потолок ТЕКСТОВОЙ ЦИТАТЫ ответа, символов (Р10).
///
/// Цитата — это указатель на строку, которая и так есть в ленте, а не её копия.
/// Двести символов покрывают обычную реплику целиком; длинное сообщение
/// урезается многоточием, и это честнее, чем съедать половину лимита самого
/// ответа чужим текстом.
pub const QUOTE_MAX_CHARS: usize = 200;

/// Проверяет текст сообщения ДО единого сетевого вызова (Р12).
///
/// Две причины отказа, и обе — `400 invalid_input`:
///
/// 1. **пусто** — портал ответил бы `MESSAGE_EMPTY`, то есть тем же самым, но
///    ценой запроса из бюджета 2 rps;
/// 2. **длиннее [`crate::dto::MAX_MESSAGE_CHARS`]** — портал ответил бы
///    УСПЕХОМ, молча обрезав текст на 20 006 символах. Это и есть причина, по
///    которой проверка живёт здесь: «успешно отправленный» обрубок хуже отказа,
///    потому что отправитель о нём не узнает.
///
/// Счёт идёт в code point'ах, а не в байтах: у портала лимит символьный, и
/// байтовый счёт зарезал бы кириллицу вдвое раньше срока.
///
/// # Errors
///
/// `400 invalid_input` с числом символов в тексте ошибки — фронту есть что
/// показать пользователю, не пересчитывая заново.
pub fn require_text(raw: &str) -> Result<&str, ApiError> {
    let text = raw.trim_end_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return Err(ApiError::bad_input(
            "the message text is empty".to_string(),
        ));
    }
    let chars = text.chars().count();
    if chars > dto::MAX_MESSAGE_CHARS {
        return Err(ApiError::bad_input(format!(
            "the message is {chars} characters long, the Bitrix24 limit is {} — \
             the portal would silently truncate it",
            dto::MAX_MESSAGE_CHARS
        )));
    }
    Ok(text)
}

/// Подпись к файлу: пустая РАЗРЕШЕНА, длинная — нет (Ф9).
///
/// Отдельная функция, а не флаг у [`require_text`]: у файла и у текстового
/// сообщения разные контракты пустоты. Пустое текстовое сообщение отправить
/// нельзя (портал ответил бы `MESSAGE_EMPTY`), а файл без подписи — совершенно
/// нормальный и самый частый случай; требовать здесь текст значило бы заставить
/// пользователя писать «файл» перед каждой картинкой.
///
/// Потолок длины при этом ОБЩИЙ: подпись уезжает тем же полем `message`
/// метода `im.v2.File.upload` и режется порталом так же молча.
///
/// # Errors
///
/// `400 invalid_input` — подпись длиннее [`crate::dto::MAX_MESSAGE_CHARS`].
pub fn require_caption(raw: &str) -> Result<&str, ApiError> {
    let text = raw.trim_end_matches(['\n', '\r']);
    let chars = text.chars().count();
    if chars > dto::MAX_MESSAGE_CHARS {
        return Err(ApiError::bad_input(format!(
            "the caption is {chars} characters long, the Bitrix24 limit is {} — \
             the portal would silently truncate it",
            dto::MAX_MESSAGE_CHARS
        )));
    }
    Ok(text)
}

/// Собирает текст ответа: цитата строкой плюс сам ответ (Р10).
///
/// ```text
///   > Максим Крылов: исходная реплика
///   мой ответ
/// ```
///
/// # Почему цитата вообще нужна
///
/// `REPLY_ID` портал принимает, но при ЧТЕНИИ не отражает: `params`
/// возвращаются пустыми, и `reply_to` в [`MessageDto`] всегда `null`. Без
/// текстовой цитаты ответ в ленте неотличим от новой реплики — ни у нас, ни в
/// веб-мессенджере портала.
///
/// # Что делает с длиной
///
/// Цитата НИКОГДА не выталкивает ответ за лимит: она урезается до того, что
/// осталось от [`crate::dto::MAX_MESSAGE_CHARS`], а если не осталось ничего —
/// исчезает целиком. Обратный порядок (урезать ответ ради цитаты) означал бы,
/// что чужой текст вытесняет собственный.
///
/// Чистая функция: ни сети, ни состояния — вся она проверяется юнит-тестом.
pub fn compose_reply(author: Option<&str>, quoted: Option<&str>, text: &str) -> String {
    let author = author.unwrap_or_default().trim();
    let quoted = quoted.unwrap_or_default();
    // Цитата ВСЕГДА однострочна: перевод строки внутри неё сделал бы вторую
    // строку неотличимой от текста ответа.
    let flat: String = quoted.split_whitespace().collect::<Vec<_>>().join(" ");
    if author.is_empty() && flat.is_empty() {
        return text.to_string();
    }

    let head = if author.is_empty() {
        String::new()
    } else {
        format!("{}: ", dto::truncate_chars(author, 64))
    };
    let body = dto::truncate_chars(&flat, QUOTE_MAX_CHARS);
    let line = format!("> {head}{body}");

    // Сколько символов осталось на цитату после текста и перевода строки.
    let left = dto::MAX_MESSAGE_CHARS
        .saturating_sub(text.chars().count())
        .saturating_sub(1);
    if left < 4 {
        // Места нет вовсе — уходит только `REPLY_ID`.
        return text.to_string();
    }
    let line = dto::truncate_chars(&line, left);
    format!("{line}\n{text}")
}

/// Перечитывает одну строку у портала после действия над ней.
///
/// Отказ НЕ является ошибкой действия: сообщение уже отправлено (изменено,
/// лайкнуто), и вернуть `503` значило бы предложить пользователю повторить
/// успешную операцию — то есть создать дубль. Поэтому результат
/// `Option`, а вызывающий отмечает его флагом `echoed`.
async fn reread_message(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    dialog_id: &str,
    message_id: i64,
) -> Option<MessageDto> {
    match load_page(
        state,
        client,
        dialog_id,
        1,
        Some(message_id.saturating_add(1)),
        None,
    )
    .await
    {
        Ok(page) => page.into_iter().find(|m| m.id == message_id),
        Err(e) => {
            tracing::debug!(
                target: "forge_bitrix",
                message_id,
                code = %e.body.code,
                "forge-bitrix: the message could not be re-read after the action"
            );
            None
        }
    }
}

/// Строка, собранная ЛОКАЛЬНО, — запасной путь, когда перечитать не вышло.
///
/// Врёт ровно в том, чего не знает: текст здесь наш, а не переписанный порталом
/// (`URL_PREVIEW` оборачивает голые ссылки в `[URL]`), лайков нет, вложений нет.
/// Всё это приедет со следующей страницей истории или из фолбэк-синхронизации
/// (Ф7) — а строка в ленте появляется сразу, и это важнее.
fn synthesized_message(
    id: i64,
    dialog_id: &str,
    text: &str,
    self_id: i64,
    edited: bool,
    peers: &dyn dto::PeerNames,
) -> MessageDto {
    let raw = crate::api::schema::MessageRaw {
        id,
        author_id: self_id,
        date: Some(chrono::Utc::now().to_rfc3339()),
        text: Some(text.to_string()),
        ..Default::default()
    };
    let mut msg = dto::message_from_raw(&raw, dialog_id, self_id, &Default::default(), peers);
    msg.edited = edited;
    msg
}

/// `POST /api/bitrix/dialogs/:id/messages` — отправить текст ИЛИ файл.
///
/// # Что происходит по шагам
///
/// ```text
///   409/503/404 ─→ разбор тела ─→ Р12 (длина) ─→ Р10 (цитата) ─→ im.message.add
///                                                                     │
///                                          перечитать строку ←────────┘
/// ```
///
/// С непустым `upload_id` (Ф9) меняется ровно одна стрелка: вместо
/// `im.message.add` зовётся `im.v2.File.upload`, который заливает файл И
/// отправляет сообщение одним вызовом (Р7). Всё остальное — перечитывание
/// строки, дедуп, рассылка кадра `message_new` — общее, и именно ради этого
/// файл не получил собственного роута.
///
/// # Отличия ветки файла
///
/// - `text` становится ПОДПИСЬЮ и может быть пустым ([`require_caption`]);
/// - `reply_to` уезжает только ТЕКСТОВОЙ цитатой: `im.v2.File.upload` поля
///   `REPLY_ID` не принимает вовсе. Для чтения это ничего не меняет — портал
///   `REPLY_ID` при чтении всё равно не отражает (Р10);
/// - `upload_id` ОДНОРАЗОВЫЙ: он потребляется до заливки, поэтому повторный
///   запрос с тем же значением получит `409 invalid_state`, а не второй файл.
///
/// # Сколько это стоит
///
/// Два запроса к порталу: отправка и перечитывание строки. Второй нужен,
/// потому что портал переписывает текст (`URL_PREVIEW`) и нормализует
/// вложения, а фронт обязан заменить оптимистичную строку НАСТОЯЩЕЙ. Отказ
/// второго запроса ответ не роняет. Для файла второй запрос ещё и обязателен
/// по существу: дескриптор вложения приезжает только с перечитанной строкой.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected` (в том числе транспортный отказ
/// САМОЙ отправки — повтор при этом не делается, Р9), `404 unknown_dialog`,
/// `400 invalid_input` (пустой или слишком длинный текст, битое тело),
/// `409 invalid_state` (`upload_id` неизвестен, потрачен или протух), плюс
/// ошибки портала (`409 send_forbidden` — нет прав писать в этот чат).
pub async fn post_message(
    Portal(state): Portal,
    Path(id): Path<String>,
    body: Option<Json<SendReq>>,
) -> Result<Json<MessageResp>, ApiError> {
    // Лестница: 409 → 503 → 404 → 400. Тело разбирается ПОСЛЕДНИМ.
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;
    let req = body
        .map(|Json(v)| v)
        .ok_or_else(|| ApiError::bad_input("the request body must be a JSON object".to_string()))?;

    let upload_id = req
        .upload_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (message_id, outgoing) = match upload_id {
        // --- файл (Ф9): заливка и отправка ОДНИМ вызовом портала ---
        Some(uid) => {
            let caption = require_caption(&req.text)?;
            let outgoing = match req.reply_to.filter(|v| *v > 0) {
                Some(_) => {
                    compose_reply(req.reply_author.as_deref(), req.reply_text.as_deref(), caption)
                }
                None => caption.to_string(),
            };
            let id = crate::routes::upload::send_staged_file(
                &state,
                &client,
                &dialog_id,
                uid,
                &outgoing,
            )
            .await?;
            (id, outgoing)
        }
        // --- текст (Ф6) ---
        None => {
            let text = require_text(&req.text)?;
            let outgoing = match req.reply_to.filter(|v| *v > 0) {
                Some(_) => {
                    compose_reply(req.reply_author.as_deref(), req.reply_text.as_deref(), text)
                }
                None => text.to_string(),
            };
            let id = client
                .message_add(&dialog_id, &outgoing, req.reply_to)
                .await
                .map_err(|e| map_portal_error(&e))?;
            (id, outgoing)
        }
    };

    let echoed = reread_message(&state, &client, &dialog_id, message_id).await;
    let echoed_ok = echoed.is_some();
    let message = match echoed {
        Some(m) => m,
        None => synthesized_message(
            message_id,
            &dialog_id,
            &outgoing,
            state.self_id().await,
            false,
            &state.peers,
        ),
    };

    // Кадр уходит СРАЗУ, не дожидаясь потока событий (Ф7).
    //
    // Своё сообщение приезжает событием так же, как чужое, — но через один
    // цикл опроса, то есть до четырёх секунд. Без этой рассылки строка была бы
    // видна ТОЛЬКО той вкладке, которая нажала «отправить»: во второй вкладке
    // devforge (и на телефоне) она появлялась бы с задержкой, и это читалось бы
    // как потерянное сообщение.
    //
    // Порядок обязателен: сначала регистрируем идентификатор в кольцевом
    // буфере дедупа (Р19), потом рассылаем. Событие с этим же `message.id`
    // придёт через секунды и обязано отсеяться — иначе строка задвоится у всех,
    // включая отправителя.
    state.note_message(message_id);
    state.broadcast_event(crate::ws::protocol::B24ServerMsg::MessageNew {
        dialog_id: dialog_id.clone(),
        message: message.clone(),
        // Своё сообщение тост не порождает в любом случае (`out: true`), но
        // поле обязано быть честным: фронт применяет кадр одной функцией.
        muted: state.is_muted(&dialog_id).await,
    });

    tracing::info!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        message_id,
        chars = outgoing.chars().count(),
        reply = req.reply_to.is_some(),
        // Имя файла в лог не уезжает: оно пользовательский ввод, а факта
        // «сообщение было файлом» для диагностики достаточно.
        file = upload_id.is_some(),
        echoed = echoed_ok,
        "forge-bitrix: message sent"
    );
    Ok(Json(MessageResp {
        id: message_id,
        message: Some(message),
        echoed: echoed_ok,
    }))
}

/// `PATCH /api/bitrix/messages/:mid` — изменить текст сообщения.
///
/// # Почему пустой текст — отказ, а не удаление
///
/// У портала пустой `MESSAGE` в `im.message.update` УДАЛЯЕТ сообщение. Принять
/// его значило бы, что случайное «выделил всё и нажал Enter» стирает строку без
/// единого вопроса, причём необратимо. Удаление живёт в своём методе и на своей
/// кнопке, у которой есть подтверждение.
///
/// # Что видно после успеха
///
/// В перечитанной строке появляется `edited: true` (`params.IS_EDITED == "Y"`)
/// — метку ставит сам портал. Чужие правки при этом приезжают с задержкой до
/// минуты: событие `ONIMV2MESSAGEUPDATE` документировано, но живьём не
/// приходит, и ловит их только фолбэк-воркер (Ф7). Это ожидаемое поведение, а
/// не баг.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_message` (`:mid` не
/// положительное целое), `404 unknown_dialog` (в теле нет разбираемого
/// `dialog_id`), `400 invalid_input` (пустой или слишком длинный текст),
/// `409 invalid_state` (`CANT_EDIT_MESSAGE`: истекло окно правки либо
/// сообщение чужое).
pub async fn patch_message(
    Portal(state): Portal,
    Path(mid): Path<String>,
    body: Option<Json<EditReq>>,
) -> Result<Json<MessageResp>, ApiError> {
    let client = require_ready(&state).await?;
    let message_id = require_message_id(&mid)?;
    let req = body
        .map(|Json(v)| v)
        .ok_or_else(|| ApiError::bad_input("the request body must be a JSON object".to_string()))?;
    let dialog_id = require_dialog_id(req.dialog_id.as_deref().unwrap_or_default())?;
    let text = require_text(&req.text)?;

    client
        .message_update(message_id, text)
        .await
        .map_err(|e| map_portal_error(&e))?;

    let echoed = reread_message(&state, &client, &dialog_id, message_id).await;
    let echoed_ok = echoed.is_some();
    let message = match echoed {
        Some(m) => m,
        None => synthesized_message(
            message_id,
            &dialog_id,
            text,
            state.self_id().await,
            true,
            &state.peers,
        ),
    };

    // Правка тоже уходит кадром сразу: событие `ONIMV2MESSAGEUPDATE` живьём НЕ
    // ПРИХОДИТ вовсе, и без этой рассылки соседняя вкладка увидела бы новый
    // текст только через цикл фолбэк-синхронизации, то есть до минуты (Ф7).
    state.broadcast_event(crate::ws::protocol::B24ServerMsg::MessageUpdate {
        dialog_id: dialog_id.clone(),
        message: message.clone(),
    });

    tracing::info!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        message_id,
        chars = text.chars().count(),
        "forge-bitrix: message edited"
    );
    Ok(Json(MessageResp {
        id: message_id,
        message: Some(message),
        echoed: echoed_ok,
    }))
}

/// `DELETE /api/bitrix/messages/:mid` — удалить сообщение.
///
/// Диалог не нужен вовсе: перечитывать после удаления нечего, а портал
/// адресует метод одним `MESSAGE_ID`.
///
/// # Почему здесь нет рассылки кадра, а у отправки и правки — есть
///
/// Удаление приезжает потоком событий (`ONIMV2MESSAGEDELETE`) в ближайший цикл
/// опроса, то есть соседние вкладки узнают о нём сами. Отправка тоже приезжает
/// событием, но её кадр рассылается немедленно намеренно — своё сообщение
/// обязано появиться у всех мгновенно; а вот правка не приезжает НИКОГДА
/// (`ONIMV2MESSAGEUPDATE` живьём не приходит), и там рассылка — единственный
/// быстрый путь. Ровно поэтому три соседних роута ведут себя по-разному.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_message`, `409 invalid_state`
/// (`CANT_EDIT_MESSAGE`: чужое сообщение либо истекло окно).
pub async fn delete_message(
    Portal(state): Portal,
    Path(mid): Path<String>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let message_id = require_message_id(&mid)?;

    client
        .message_delete(message_id)
        .await
        .map_err(|e| map_portal_error(&e))?;

    tracing::info!(
        target: "forge_bitrix",
        message_id,
        "forge-bitrix: message deleted"
    );
    Ok(Json(AckResp {
        ok: true,
        sent: true,
    }))
}

/// `POST /api/bitrix/messages/:mid/like` — «лайк».
///
/// Метод ПЕРЕКЛЮЧАЮЩИЙ: второй вызов с `auto` возвращает состояние к
/// исходному. Ответ портала всегда `true` и о состоянии не говорит ничего —
/// поэтому строка перечитывается, и новый состав `likes` фронт берёт из неё, а
/// не угадывает.
///
/// Эмодзи-реакций у платформы для вебхука нет вовсе (§6): UI не должен
/// предлагать выбор, которого не существует.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_message`, `404 unknown_dialog`
/// (нет разбираемого `dialog_id` в теле), `400 invalid_input` (незнакомый
/// `action`), плюс ошибки портала.
pub async fn post_like(
    Portal(state): Portal,
    Path(mid): Path<String>,
    body: Option<Json<LikeReq>>,
) -> Result<Json<MessageResp>, ApiError> {
    let client = require_ready(&state).await?;
    let message_id = require_message_id(&mid)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();
    let dialog_id = require_dialog_id(req.dialog_id.as_deref().unwrap_or_default())?;
    let action = LikeAction::parse(req.action.as_deref()).ok_or_else(|| {
        ApiError::bad_input("`action` must be one of: auto, plus, minus".to_string())
    })?;

    client
        .message_like(message_id, action)
        .await
        .map_err(|e| map_portal_error(&e))?;

    let message = reread_message(&state, &client, &dialog_id, message_id).await;
    let echoed = message.is_some();
    tracing::debug!(
        target: "forge_bitrix",
        message_id,
        action = action.as_str(),
        echoed,
        "forge-bitrix: like toggled"
    );
    Ok(Json(MessageResp {
        id: message_id,
        message,
        echoed,
    }))
}

/// `POST /api/bitrix/messages/:mid/command` — выполнить команду бота
/// (кнопка клавиатуры вида `COMMAND`, forge-zw3d).
///
/// # Почему НЕ перечитывает сообщение, в отличие от [`post_like`]
///
/// Команда не гарантированно меняет ТЕКУЩЕЕ сообщение — типичный бот отвечает
/// НОВЫМ сообщением (его подхватит realtime-воркер Ф7 тем же путём, что и
/// любую другую входящую строку), а не правкой старого. Перечитывание здесь
/// либо ничего не изменило бы, либо соврало бы, что действие «применилось» к
/// сообщению, которого правка не касалась. Постоянная блокировка кнопки
/// (`BLOCK: Y`, [`crate::attach::ButtonDto::lock_after_press`]) — решение
/// фронта по данным, которые у него уже есть, а не по свежему ответу сервера.
///
/// # Errors
///
/// `400 invalid_input` — пустая `command`; `409 not_authorized`, `503`,
/// `404 unknown_message`, плюс ошибки портала (в т.ч. `BOT_ID_ERROR`,
/// `COMMAND_ERROR` — их отдаёт `map_portal_error`).
pub async fn post_command(
    Portal(state): Portal,
    Path(mid): Path<String>,
    Json(req): Json<CommandReq>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let message_id = require_message_id(&mid)?;
    if req.command.trim().is_empty() {
        return Err(ApiError::bad_input("`command` must not be empty".to_string()));
    }

    client
        .message_command(
            message_id,
            req.bot_id,
            req.command.trim(),
            req.command_params.as_deref(),
        )
        .await
        .map_err(|e| map_portal_error(&e))?;

    tracing::debug!(
        target: "forge_bitrix",
        message_id,
        bot_id = req.bot_id,
        command = %req.command,
        "forge-bitrix: bot command executed"
    );
    Ok(Json(AckResp { ok: true, sent: true }))
}

/// `POST /api/bitrix/dialogs/:id/read` — отметить прочитанным.
///
/// # Окно 800 мс на диалог
///
/// Отметку порождает ПРОКРУТКА, то есть десятки событий в секунду, а бюджет
/// портала — два запроса в секунду на весь плагин. Первый вызов в окне уходит
/// немедленно, остальные подавляются и получают `sent: false` (см.
/// [`crate::actions`]). Подавление безопасно: без `message_id` метод читает
/// диалог целиком, а следующее событие ленты повторит отметку.
///
/// После успеха снимок счётчиков ВЫБРАСЫВАЕТСЯ: пользователь только что своими
/// руками обнулил непрочитанное, и показывать ему старое число ещё полминуты
/// (TTL снимка) — значит выглядеть сломанным.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_dialog`, плюс ошибки портала.
pub async fn post_read(
    Portal(state): Portal,
    Path(id): Path<String>,
    body: Option<Json<MarkReq>>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();

    if !state
        .read_gate
        .allow(&dialog_id, now_ms(), READ_DEBOUNCE_MS)
    {
        return Ok(Json(AckResp {
            ok: true,
            sent: false,
        }));
    }

    let left = client
        .dialog_read(&dialog_id, req.message_id)
        .await
        .map_err(|e| map_portal_error(&e))?;
    state.invalidate_counters().await;

    tracing::debug!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        counter_left = left,
        "forge-bitrix: dialog marked read"
    );
    Ok(Json(AckResp {
        ok: true,
        sent: true,
    }))
}

/// `POST /api/bitrix/dialogs/:id/unread` — пометить непрочитанным.
///
/// # ЛОВУШКА ПЛАТФОРМЫ: `message_id` обязателен
///
/// Без него портал отвечает `MESSAGE_ID_ERROR` («First unread message id can't
/// be empty»). Поэтому отсутствие идентификатора — это `400 invalid_input` НА
/// НАШЕЙ СТОРОНЕ, и ни одного сетевого вызова такой запрос не порождает: тратить
/// запрос из бюджета 2 rps на заведомый отказ незачем.
///
/// Троттлинга здесь нет намеренно: это нажатие кнопки, а не жест — оно бывает
/// раз в минуту, а не сорок раз в секунду.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_dialog`, `400 invalid_input`
/// (нет `message_id`), плюс ошибки портала.
pub async fn post_unread(
    Portal(state): Portal,
    Path(id): Path<String>,
    body: Option<Json<MarkReq>>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();

    let message_id = req.message_id.filter(|v| *v > 0).ok_or_else(|| {
        ApiError::bad_input(
            "`message_id` is required: Bitrix24 rejects im.dialog.unread without it".to_string(),
        )
    })?;

    client
        .dialog_unread(&dialog_id, message_id)
        .await
        .map_err(|e| map_portal_error(&e))?;
    state.invalidate_counters().await;
    // Окно прочтения сбрасывается: пользователь только что сказал обратное, и
    // следующая отметка о прочтении обязана уйти немедленно, а не дожидаться
    // остатка чужого окна.
    state.read_gate.forget(&dialog_id);

    tracing::debug!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        message_id,
        "forge-bitrix: dialog marked unread"
    );
    Ok(Json(AckResp {
        ok: true,
        sent: true,
    }))
}

/// `POST /api/bitrix/dialogs/:id/typing` — «печатает…».
///
/// Окно — 8 секунд на диалог: индикатор портала живёт около десяти, и обновлять
/// его чаще бессмысленно. Подавленный вызов отвечает `sent: false` и в портал
/// не идёт.
///
/// Обратной стороны у этого действия нет: ВХОДЯЩИХ событий набора текста
/// вебхуку не приходит вовсе (§6), поэтому чужое «печатает» в панели не
/// появляется никогда.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_dialog`, плюс ошибки портала.
pub async fn post_typing(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;

    if !state
        .typing_gate
        .allow(&dialog_id, now_ms(), TYPING_INTERVAL_MS)
    {
        return Ok(Json(AckResp {
            ok: true,
            sent: false,
        }));
    }

    client
        .dialog_writing(&dialog_id)
        .await
        .map_err(|e| map_portal_error(&e))?;
    Ok(Json(AckResp {
        ok: true,
        sent: true,
    }))
}

/// `POST /api/bitrix/dialogs/:id/pin` — закрепить или открепить диалог (Р14).
///
/// Тело необязательно: без него диалог ЗАКРЕПЛЯЕТСЯ (`pinned: true` по
/// умолчанию) — так кнопка «⌖» работает одним вызовом, а открепление приходит
/// явным `{"pinned": false}`.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_dialog`, плюс ошибки портала.
pub async fn post_pin(
    Portal(state): Portal,
    Path(id): Path<String>,
    body: Option<Json<PinReq>>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;
    let pinned = body.map(|Json(v)| v).unwrap_or_default().pinned.unwrap_or(true);

    client
        .recent_pin(&dialog_id, pinned)
        .await
        .map_err(|e| map_portal_error(&e))?;

    tracing::debug!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        pinned,
        "forge-bitrix: dialog pin toggled"
    );
    Ok(Json(AckResp {
        ok: true,
        sent: true,
    }))
}

/// `POST /api/bitrix/dialogs/:id/hide` — скрыть диалог из списка (Р14).
///
/// Ни чат, ни история не удаляются: диалог вернётся в список от первого же
/// нового сообщения. Обратной операции у портала нет — именно поэтому фронт
/// обязан спрашивать подтверждение.
///
/// Этим объём управления чатами исчерпан (Р14): создание, переименование,
/// приглашение участников и выход из чата в v1 не делаются вовсе.
///
/// # Errors
///
/// `409 not_authorized`, `503`, `404 unknown_dialog`, плюс ошибки портала.
pub async fn post_hide(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<AckResp>, ApiError> {
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;

    client
        .recent_hide(&dialog_id)
        .await
        .map_err(|e| map_portal_error(&e))?;

    tracing::info!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        "forge-bitrix: dialog hidden from the list"
    );
    Ok(Json(AckResp {
        ok: true,
        sent: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ключи директорий — контракт с фронтом и с настройками (Р2).
    #[test]
    fn the_directory_keys_are_frozen() {
        let keys: Vec<&str> = DIRS.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["all", "chats", "tasks", "unread"]);
        let titles: Vec<&str> = DIRS.iter().map(|(_, t)| *t).collect();
        assert_eq!(titles, vec!["~/all", "~/chats", "~/tasks", "~/unread"]);
    }

    fn dialog(id: &str, kind: &str, unread: i64) -> DialogDto {
        DialogDto {
            id: id.into(),
            kind: kind.into(),
            unread_count: unread,
            ..DialogDto::default()
        }
    }

    /// Директории делят ОДИН список по полям строки, а не запросами.
    #[test]
    fn directories_split_one_list_by_row_fields() {
        let task = dialog("chat7", dto::kind::TASK, 0);
        let chat = dialog("chat3", dto::kind::CHAT, 0);
        let unread_user = dialog("19", dto::kind::USER, 3);

        assert!(dir_matches("all", &task) && dir_matches("all", &chat));
        assert!(dir_matches("tasks", &task) && !dir_matches("tasks", &chat));
        assert!(dir_matches("chats", &chat) && !dir_matches("chats", &task));
        assert!(dir_matches("chats", &unread_user), "личные — тоже чаты");
        assert!(dir_matches("unread", &unread_user) && !dir_matches("unread", &chat));
        // Незнакомая директория показывает весь список, а не пустоту.
        assert!(dir_matches("выдумка", &task));
    }

    #[test]
    fn unknown_directories_fall_back_to_all() {
        assert_eq!(normalize_dir(Some("tasks")), "tasks");
        assert_eq!(normalize_dir(Some("unread")), "unread");
        assert_eq!(normalize_dir(None), "all");
        assert_eq!(normalize_dir(Some("")), "all");
        assert_eq!(normalize_dir(Some("../../etc/passwd")), "all");
        assert_eq!(normalize_dir(Some("TASKS")), "all", "регистр значим");
    }

    /// Счётчики директорий: уведомления портала попадают в бейдж, но не в
    /// `~/unread` — открыть их в панели всё равно нельзя (Р13).
    #[test]
    fn directory_counters_split_the_portal_numbers() {
        let c = CountersDto {
            total: 9,
            chat: 2,
            dialog: 1,
            tasks: 1,
            notify: 5,
        };
        assert_eq!(dir_unread("all", &c), 9);
        assert_eq!(dir_unread("chats", &c), 3);
        assert_eq!(dir_unread("tasks", &c), 1);
        assert_eq!(dir_unread("unread", &c), 4, "без уведомлений портала");
    }

    /// Разбор query: пустое значение — не ошибка, мусор — ошибка, дубли
    /// детерминированы.
    #[test]
    fn query_parsing_is_lenient_where_it_must_be_and_strict_where_it_can() {
        assert_eq!(query_i64(Some("limit=40&offset=0"), "limit").unwrap(), Some(40));
        assert_eq!(query_i64(Some("limit="), "limit").unwrap(), None);
        assert_eq!(query_i64(None, "limit").unwrap(), None);
        assert_eq!(query_i64(Some("offset=5"), "limit").unwrap(), None);
        assert_eq!(query_i64(Some("limit=-3"), "limit").unwrap(), Some(-3));

        let err = query_i64(Some("limit=abc"), "limit").unwrap_err();
        assert_eq!(err.status.as_u16(), 400);
        assert_eq!(err.body.code, "invalid_input");

        // Дубли: берётся первое значение — важно, что детерминировано.
        assert_eq!(query_i64(Some("limit=1&limit=2"), "limit").unwrap(), Some(1));
        assert_eq!(query_str(Some("filter=tasks&filter=all"), "filter"), Some("tasks"));
        // Ключ без значения не считается значением.
        assert_eq!(query_str(Some("filter"), "filter"), None);
    }

    /// СТУПЕНЬ 404: `:id` из URL — это «диалога нет», а не «параметр плохой».
    ///
    /// Разные коды на «не разбирается» и «не существует» превратили бы роут в
    /// оракул: по ответу можно было бы перебирать валидные идентификаторы
    /// портала.
    #[test]
    fn a_hostile_dialog_id_is_a_404_and_is_never_echoed_back() {
        assert_eq!(require_dialog_id(" chat27 ").unwrap(), "chat27");
        assert_eq!(require_dialog_id("19").unwrap(), "19");
        assert_eq!(require_dialog_id("sg12").unwrap(), "sg12");

        for hostile in [
            "",
            "../../etc/passwd",
            "chat7/../chat8",
            "<script>alert(1)</script>",
            "chat7\u{0}",
            "CHAT7",
            "+19",
            &"9".repeat(64),
        ] {
            let err = require_dialog_id(hostile).unwrap_err();
            assert_eq!(err.status.as_u16(), 404, "{hostile:?}");
            assert_eq!(err.body.code, "unknown_dialog");
            assert!(
                !err.body.message.contains(hostile) || hostile.is_empty(),
                "враждебная строка отразилась обратно: {}",
                err.body.message
            );
        }
    }

    /// Р12: длина проверяется ДО сети, потому что портал режет молча.
    ///
    /// Счёт в code point'ах, а не в байтах: кириллица иначе упиралась бы в
    /// лимит вдвое раньше срока.
    #[test]
    fn the_text_limit_is_counted_in_characters_and_is_checked_before_the_network() {
        assert_eq!(require_text("привет").unwrap(), "привет");
        // Хвостовой перевод строки — это Enter в поле ввода, а не текст.
        assert_eq!(require_text("привет\n").unwrap(), "привет");

        // Ровно потолок — можно.
        let exact = "я".repeat(dto::MAX_MESSAGE_CHARS);
        assert_eq!(require_text(&exact).unwrap().chars().count(), dto::MAX_MESSAGE_CHARS);
        // …а потолок в БАЙТАХ вдвое больше и отвергать его нельзя.
        assert!(exact.len() > dto::MAX_MESSAGE_CHARS);

        // На символ больше — отказ, и в тексте ошибки есть длина.
        let over = "я".repeat(dto::MAX_MESSAGE_CHARS + 1);
        let err = require_text(&over).unwrap_err();
        assert_eq!(err.status.as_u16(), 400);
        assert_eq!(err.body.code, "invalid_input");
        assert!(err.body.message.contains("20001"), "{}", err.body.message);

        // Пустое и пробельное — тоже 400, но без обращения к порталу.
        for empty in ["", "   ", "\n", "\t \n"] {
            let err = require_text(empty).unwrap_err();
            assert_eq!(err.status.as_u16(), 400, "{empty:?}");
            assert_eq!(err.body.code, "invalid_input");
        }
    }

    /// Р10: цитата уходит ТЕКСТОМ и никогда не выталкивает сам ответ за лимит.
    #[test]
    fn a_reply_quote_is_one_line_and_never_squeezes_out_the_answer() {
        let out = compose_reply(Some("Максим Крылов"), Some("исходная реплика"), "мой ответ");
        assert_eq!(out, "> Максим Крылов: исходная реплика\nмой ответ");

        // Перевод строки внутри цитаты схлопывается: вторая строка была бы
        // неотличима от текста ответа.
        let out = compose_reply(Some("Бот"), Some("первая\nвторая   третья"), "ок");
        assert_eq!(out, "> Бот: первая вторая третья\nок");

        // Длинная цитата урезается многоточием, а не тащит чужой текст целиком.
        let long = "ц".repeat(QUOTE_MAX_CHARS * 2);
        let out = compose_reply(Some("А"), Some(&long), "ответ");
        let quote_line = out.lines().next().unwrap();
        assert!(quote_line.chars().count() <= QUOTE_MAX_CHARS + 8, "{quote_line}");
        assert!(quote_line.ends_with('…'), "{quote_line}");

        // Ответ во всю длину лимита: цитата исчезает, а ответ остаётся целым.
        let full = "я".repeat(dto::MAX_MESSAGE_CHARS);
        let out = compose_reply(Some("А"), Some("цитата"), &full);
        assert_eq!(out, full, "цитата не имеет права вытеснять собственный текст");
        assert!(out.chars().count() <= dto::MAX_MESSAGE_CHARS);

        // Почти во всю длину: итог всё равно в пределах лимита.
        let almost = "я".repeat(dto::MAX_MESSAGE_CHARS - 40);
        let out = compose_reply(Some("Максим"), Some("исходная реплика"), &almost);
        assert!(
            out.chars().count() <= dto::MAX_MESSAGE_CHARS,
            "итог {} символов",
            out.chars().count()
        );

        // Нечего цитировать — текст уходит как есть (REPLY_ID при этом всё
        // равно отправляется: он в параметрах, а не в тексте).
        assert_eq!(compose_reply(None, None, "просто ответ"), "просто ответ");
        assert_eq!(compose_reply(Some("  "), Some(" \n "), "ответ"), "ответ");
    }

    /// Идентификатор сообщения: только положительное целое, иначе 404.
    #[test]
    fn a_message_id_is_a_positive_integer_or_a_404() {
        assert_eq!(require_message_id("113").unwrap(), 113);
        assert_eq!(require_message_id(" 7 ").unwrap(), 7);

        for hostile in ["", "0", "-1", "abc", "1.5", "1e3", "999999999999999999999", "7%00"] {
            let err = require_message_id(hostile).unwrap_err();
            assert_eq!(err.status.as_u16(), 404, "{hostile:?}");
            assert_eq!(err.body.code, "unknown_message");
        }
    }
}
