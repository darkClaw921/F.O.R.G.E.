//! Задачи портала: список, карточка, действия, счётчики.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET  | `/api/bitrix/tasks` | страница задач: `?filter&start&responsible&group` |
//! | GET  | `/api/bitrix/tasks/counters` | счётчики задач и чатов задач |
//! | GET  | `/api/bitrix/tasks/:id` | карточка задачи (БЕЗ `select`) |
//! | POST | `/api/bitrix/tasks/:id/:action` | действие по матрице `action` |
//!
//! # ТРИ ЛОВУШКИ, КОТОРЫЕ ЗАКРЫВАЕТ ЭТОТ МОДУЛЬ
//!
//! Каждая даёт не ошибку, а **молчаливую пустоту** — то есть выглядит как
//! «данных нет». Все три закрыты стражами `tests/tasks_traps.rs`.
//!
//! 1. **`ENTITY_TYPE = "TASKS"`** у `im.chat.get` возвращает `{"result":null}`
//!    при HTTP 200. Верное значение только `"TASKS_TASK"`; передать другое
//!    невозможно по типам — [`crate::api::B24Client::task_chat_id`] не
//!    принимает сущность параметром вовсе.
//! 2. **camelCase в `select`** портал молча игнорирует, и поле приезжает
//!    `null`, неотличимо от настоящего `null`. Поэтому карточка зовётся БЕЗ
//!    `select`, а список — со списком UPPERCASE-имён
//!    ([`crate::tasks::LIST_SELECT`]).
//! 3. **`{"STATUS": 2}` в фильтре** даёт ноль записей при подходящих задачах.
//!    Разрешены только `REAL_STATUS` и операторные формы; проверку делает
//!    [`crate::api::methods::task_list_params`] ДО сети.
//!
//! # Почему действие проверяется по КЭШУ, а не запросом
//!
//! Матрица `action` динамична и приезжает вместе с карточкой. Карточка уже на
//! экране в тот момент, когда пользователь жмёт кнопку, — значит матрица уже
//! прочитана, и спрашивать её у портала второй раз, чтобы ответить «нельзя»,
//! означало бы тратить один из двух запросов в секунду впустую. Поэтому
//! матрица живёт в [`crate::tasks::TaskIndex`], и запрещённое действие
//! отвергается `409 invalid_state` БЕЗ единого обращения к порталу.
//!
//! Если матрицы в индексе нет (действие пришло раньше карточки — например,
//! после перезапуска devforge), карточка читается один раз и решение
//! принимается по ней. Это единственная ветка, где на отказ тратится запрос.
//!
//! # После действия карточка и лента ПЕРЕЗАПРАШИВАЮТСЯ
//!
//! Смена статуса пишет в чат задачи системное сообщение (`author_id: 0`,
//! «начал выполнять задачу»), и его обязана увидеть лента. Поэтому ответ
//! действия несёт СВЕЖУЮ карточку (перечитанную у портала — матрица уже
//! другая) и `dialog_id`, по которому фронт перезапрашивает ленту. Собрать
//! новую матрицу локально невозможно: её правила знает только портал.
//!
//! # Р20: задача без чата
//!
//! `chatId` приходит нулём либо `null` — это ОБЯЗАТЕЛЬНАЯ ветка, а не редкий
//! случай. Карточка при этом отдаётся целиком, `chat_id` и `dialog_id`
//! остаются `null`, а `im.chat.get` НЕ вызывается: у задачи без обсуждения
//! спрашивать нечего.
//!
//! Единственное исключение — ПОСЛЕ успешного действия: портал пишет системное
//! сообщение, то есть создаёт чат, и если карточка о нём ещё не знает (портал
//! обновляет поле не мгновенно), чат добирается через
//! `im.chat.get {ENTITY_TYPE: "TASKS_TASK"}`. Без этого первое же действие над
//! свежей задачей оставляло бы пользователя без ленты до перезагрузки.
//!
//! # Лестница проверок — та же, что у диалогов
//!
//! `409` (не подключён) → `503` (нет связи) → `404` (нет такой задачи) →
//! `400` (разбор параметров). Query разбирается ВРУЧНУЮ: типизированный
//! экстрактор отработал бы ДО тела хендлера и вернул бы `400` раньше `409`,
//! превратив роут в оракул валидности идентификаторов портала.

use std::sync::Arc;

use axum::extract::{Path, RawQuery};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::B24Client;
use crate::dto;
use crate::registry::B24Registry;
use crate::routes::dialogs::{map_portal_error, query_i64, query_str, require_ready};
use crate::routes::error::ApiError;
use crate::routes::Portal;
use crate::state::B24State;
use crate::tasks::{self, ActionMatrix, TaskDto};
use crate::ws::protocol::B24ServerMsg;

/// Пресеты фильтра списка задач.
///
/// Ключи заморожены: они уезжают в `?filter=` и в разметку панели.
///
/// **Ни один пресет не использует ключ `STATUS`** — он даёт ноль записей
/// молча (ловушка 3). Открытость выражается операторной формой
/// `<REAL_STATUS: 5` (проверено живьём: вернула обе открытые задачи), а
/// закрытость — прямым `REAL_STATUS: 5`.
pub const FILTERS: [(&str, &str); 5] = [
    ("all", "все мои"),
    ("mine", "я ответственный"),
    ("active", "в работе"),
    ("expired", "просрочены"),
    ("closed", "завершены"),
];

/// Пресет по умолчанию.
pub const DEFAULT_FILTER: &str = "all";

/// Ответ `GET /api/bitrix/tasks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksResp {
    /// Страница карточек.
    pub tasks: Vec<TaskDto>,
    /// Сколько всего задач подходит под фильтр.
    ///
    /// `None` означает «портал числа не прислал»: выдавать за него ноль
    /// нельзя — список схлопнулся бы в пустой при непустой странице.
    pub total: Option<i64>,
    /// Смещение этой страницы (кратно [`crate::tasks::PAGE_SIZE`]).
    pub start: i64,
    /// Есть ли ещё страницы.
    pub has_more: bool,
    /// Действующий пресет фильтра.
    pub filter: String,
}

/// Ответ `GET /api/bitrix/tasks/counters`.
///
/// **Два независимых набора, которые нельзя складывать.** `tasks.task.
/// counters.get` считает СОСТОЯНИЕ задач (просрочена, есть новые комментарии,
/// вас упомянули), `im.counters.get` — НЕПРОЧИТАННЫЕ СООБЩЕНИЯ в чатах задач
/// (`TYPE.TASKS_TASK`). Просроченная задача может не иметь ни одного
/// непрочитанного комментария, а чат задачи — светиться непрочитанным без
/// всякой просрочки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCountersResp {
    /// Просроченных задач.
    pub expired: i64,
    /// Задач с новыми комментариями.
    pub new_comments: i64,
    /// Упоминаний.
    pub mentioned: i64,
    /// Непрочитанных сообщений в чатах задач (`im.counters.get`,
    /// `TYPE.TASKS_TASK`) — им светится директория `~/tasks`.
    pub unread: i64,
}

impl TaskCountersResp {
    /// Собирает ответ из двух независимых семейств.
    pub fn new(tasks: tasks::TaskCountersDto, unread: i64) -> Self {
        Self {
            expired: tasks.expired,
            new_comments: tasks.new_comments,
            mentioned: tasks.mentioned,
            unread: unread.max(0),
        }
    }
}

/// Ответ `POST /api/bitrix/tasks/:id/:action`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskActionResp {
    /// Выполненное действие.
    pub action: String,
    /// Портал принял вызов.
    pub ok: bool,
    /// **СВЕЖАЯ карточка** — с новым статусом и НОВОЙ матрицей действий.
    ///
    /// `None` означает «действие удалось, а перечитать карточку не вышло»:
    /// фронт в этом случае перезапрашивает её сам, а не считает действие
    /// провалившимся.
    pub task: Option<TaskDto>,
    /// Диалог, чью ленту надо перезапросить: портал только что написал в него
    /// системное сообщение о смене статуса.
    pub dialog_id: Option<String>,
}

/// Роуты задач.
///
/// `/tasks/counters` объявлен ДО `/tasks/:id` намеренно: статический сегмент
/// у `matchit` приоритетнее параметра, но полагаться на порядок объявления
/// нельзя — тест `the_counters_route_is_not_shadowed_by_the_id_route`
/// фиксирует поведение.
pub fn router() -> Router<Arc<B24Registry>> {
    Router::new()
        .route("/api/bitrix/tasks", get(get_tasks))
        .route("/api/bitrix/tasks/counters", get(get_task_counters))
        .route("/api/bitrix/tasks/:id", get(get_task))
        .route("/api/bitrix/tasks/:id/:action", post(post_task_action))
}

// ---------------------------------------------------------------------------
// Ступень 3 (404) и разбор пресета.
// ---------------------------------------------------------------------------

/// `:id` из URL — это вообще задача?
///
/// Идентификаторы задач у портала положительны и растут. Всё остальное —
/// «такой задачи нет», а не «параметр плохой»: разные коды превратили бы роут
/// в оракул существования задач портала.
///
/// Враждебная строка в текст ответа НЕ попадает.
pub fn require_task_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::unknown_task("no such task in this portal".to_string()))
}

/// Приводит пресет фильтра к известному; незнакомый становится
/// [`DEFAULT_FILTER`].
///
/// Отказ (400) здесь был бы вреднее: значение приходит из настроек фронта,
/// которые браузер держит в кэше PWA неделями.
pub fn normalize_filter(raw: Option<&str>) -> &'static str {
    let raw = raw.unwrap_or(DEFAULT_FILTER);
    FILTERS
        .iter()
        .find(|(key, _)| *key == raw)
        .map_or(DEFAULT_FILTER, |(key, _)| *key)
}

/// Собирает фильтр портала по пресету.
///
/// **Ни одна ветка не использует ключ `STATUS`.** Это не стилистика: живьём
/// `{"STATUS":2}` вернул ноль записей при двух подходящих задачах — молча, с
/// успешным ответом. Работают `REAL_STATUS` и операторные формы.
///
/// `now` передаётся аргументом, а не берётся из часов: граница просрочки —
/// ровно то, что обязано быть детерминированным в тесте.
///
/// **Каждый пресет ограничен участием владельца вебхука** (`MEMBER`: любая
/// роль — постановщик, ответственный, соисполнитель, наблюдатель). Без этого
/// администратор портала видел бы в «все» задачи всей компании, а в «мои» —
/// только те, где он ответственный.
pub fn portal_filter(preset: &str, self_id: i64, responsible: Option<i64>, group: Option<i64>, now: i64) -> serde_json::Value {
    let mut filter = serde_json::Map::new();
    if self_id > 0 {
        filter.insert(tasks::FILTER_MEMBER_KEY.into(), json!(self_id));
    }
    match preset {
        "mine" => {
            filter.insert("RESPONSIBLE_ID".into(), json!(self_id.max(0)));
        }
        "active" => {
            // «Не закрыта» — операторная форма по РЕАЛЬНОМУ статусу.
            filter.insert(
                format!("<{}", tasks::FILTER_REAL_STATUS_KEY),
                json!(tasks::STATUS_COMPLETED),
            );
        }
        "expired" => {
            filter.insert(
                format!("<{}", tasks::FILTER_REAL_STATUS_KEY),
                json!(tasks::STATUS_COMPLETED),
            );
            filter.insert("<DEADLINE".into(), json!(format_portal_date(now)));
        }
        "closed" => {
            filter.insert(
                tasks::FILTER_REAL_STATUS_KEY.into(),
                json!(tasks::STATUS_COMPLETED),
            );
        }
        // `all` и всё незнакомое — без фильтра по состоянию.
        _ => {}
    }
    // Уточнения поверх пресета: они ортогональны состоянию.
    if let Some(id) = responsible.filter(|v| *v > 0) {
        filter.insert("RESPONSIBLE_ID".into(), json!(id));
    }
    if let Some(id) = group.filter(|v| *v >= 0) {
        filter.insert("GROUP_ID".into(), json!(id));
    }
    serde_json::Value::Object(filter)
}

/// Дата для фильтра портала: `2026-08-06T08:20:30+03:00`.
///
/// Портал сравнивает даты в СВОЕЙ таймзоне, но ISO-8601 со смещением он
/// понимает однозначно, поэтому отправляется UTC-форма — она не зависит от
/// того, где стоит devforge.
pub fn format_portal_date(now: i64) -> String {
    chrono::DateTime::from_timestamp(now, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Хендлеры.
// ---------------------------------------------------------------------------

/// `GET /api/bitrix/tasks?filter&start&responsible&group` — страница задач.
///
/// Страница платформенная и фиксированная: пятьдесят записей, смещение
/// `start` кратно ей (`LIMIT` у метода нет вовсе).
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `400 invalid_input` (мусор в
/// числовых параметрах), плюс ошибки портала.
pub async fn get_tasks(
    Portal(state): Portal,
    RawQuery(raw): RawQuery,
) -> Result<Json<TasksResp>, ApiError> {
    // Ступени 1–2. Разбор параметров — ПОСЛЕ них и только после них.
    let client = require_ready(&state).await?;

    let q = raw.as_deref();
    let start = query_i64(q, "start")?.unwrap_or(0).max(0);
    let responsible = query_i64(q, "responsible")?;
    let group = query_i64(q, "group")?;
    let preset = normalize_filter(query_str(q, "filter"));

    let now = dto::now_unix();
    let filter = portal_filter(preset, state.self_id().await, responsible, group, now);
    let (raws, total) = client
        .task_list(&filter, start)
        .await
        .map_err(|e| map_portal_error(&e))?;

    let origin = state.webhook().await.portal_origin();
    let page: Vec<TaskDto> = raws
        .iter()
        .map(|r| tasks::task_from_raw(r, origin.as_deref(), now))
        .collect();

    // Индекс наполняется бесплатно: строка списка несёт `chatId`, и после
    // просмотра списка лента задачи открывается без лишнего запроса. Матрицы в
    // списке НЕТ вовсе — пустая её не затирает (см. `TaskIndex::remember`).
    for (dto, raw) in page.iter().zip(raws.iter()) {
        if let Ok(id) = dto.id.parse::<i64>() {
            state.remember_task(id, dto.chat_id, ActionMatrix::from_raw(&raw.action), now);
        }
    }

    let start = start / tasks::PAGE_SIZE * tasks::PAGE_SIZE;
    let has_more = match total {
        Some(t) => start + (page.len() as i64) < t,
        // Без `total` признак один: страница пришла полной.
        None => page.len() as i64 >= tasks::PAGE_SIZE,
    };

    tracing::debug!(
        target: "forge_bitrix",
        count = page.len(),
        filter = preset,
        start,
        total = total.unwrap_or(-1),
        "forge-bitrix: tasks listed"
    );
    Ok(Json(TasksResp {
        tasks: page,
        total,
        start,
        has_more,
        filter: preset.to_string(),
    }))
}

/// `GET /api/bitrix/tasks/:id` — карточка задачи.
///
/// Зовёт `tasks.task.get` **БЕЗ `select`** (ловушка 2) и НЕ трогает
/// `im.chat.get`: чат берётся из `chatId` карточки, а его отсутствие — это
/// законная ветка «задача без чата» (Р20), а не повод для второго запроса.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_task`, плюс ошибки
/// портала.
pub async fn get_task(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<TaskDto>, ApiError> {
    let client = require_ready(&state).await?;
    let task_id = require_task_id(&id)?;
    let dto = load_card(&state, &client, task_id).await?;
    Ok(Json(dto))
}

/// `GET /api/bitrix/tasks/counters` — счётчики задач и чатов задач.
///
/// Два независимых набора одним ответом: состояние задач
/// (`tasks.task.counters.get`) и непрочитанные сообщения чатов задач
/// (`im.counters.get`, `TYPE.TASKS_TASK`). Второй берётся из снимка, оставленного
/// списком диалогов, и сетевого вызова обычно не стоит.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, плюс ошибки портала.
pub async fn get_task_counters(
    Portal(state): Portal,
) -> Result<Json<TaskCountersResp>, ApiError> {
    let client = require_ready(&state).await?;
    let now = dto::now_unix();

    // Снимок оставляет фолбэк-воркер (Ф10: третья команда его пакета), и пока
    // он свеж, роут не стоит ни одного запроса к порталу.
    let task_counters = match state
        .task_counters_fresh(now, crate::routes::dialogs::COUNTERS_TTL_SECS)
        .await
    {
        Some(c) => c,
        None => {
            let raw = client
                .task_counters(state.self_id().await)
                .await
                .map_err(|e| map_portal_error(&e))?;
            let c = tasks::TaskCountersDto::from_raw(&raw);
            state.set_task_counters(c, now).await;
            c
        }
    };

    let unread = match state
        .counters_fresh(now, crate::routes::dialogs::COUNTERS_TTL_SECS)
        .await
    {
        Some(c) => c.tasks,
        None => {
            let im = client.counters().await.map_err(|e| map_portal_error(&e))?;
            let c = dto::CountersDto::from_raw(&im);
            state.set_counters(c, now).await;
            c.tasks
        }
    };

    Ok(Json(TaskCountersResp::new(task_counters, unread)))
}

/// `POST /api/bitrix/tasks/:id/:action` — действие над задачей.
///
/// # Порядок решений
///
/// 1. лестница `409 → 503 → 404` (задача) → `400` (форма имени действия);
/// 2. **умеет ли панель** это действие ([`crate::tasks::ACTION_METHODS`]) —
///    иначе `501 method_unavailable`;
/// 3. **можно ли сейчас** (матрица `action`) — иначе `409 invalid_state`,
///    причём по КЭШУ, то есть без обращения к порталу;
/// 4. вызов семантического метода портала;
/// 5. перечитывание карточки — статус и МАТРИЦА уже другие;
/// 6. кадр `task_update` всем вкладкам.
///
/// # Errors
///
/// `409 not_authorized` / `invalid_state`, `503 not_connected`,
/// `404 unknown_task`, `400 invalid_input`, `501 method_unavailable`, плюс
/// ошибки портала (`ACCESS_DENIED` → `409 send_forbidden`).
pub async fn post_task_action(
    Portal(state): Portal,
    Path((id, action)): Path<(String, String)>,
) -> Result<Json<TaskActionResp>, ApiError> {
    let client = require_ready(&state).await?;
    let task_id = require_task_id(&id)?;
    if !tasks::is_action_key(&action) {
        // Враждебная строка в ответ не цитируется: он не должен работать
        // рефлектором чужого ввода.
        return Err(ApiError::bad_input(
            "`action` must be a portal action key".to_string(),
        ));
    }

    // Панель выполняет не всё, что портал разрешает: `edit` и `delegate`
    // требуют формы и выбора сотрудника, `remove` — безвозвратен. Это «мы так
    // не умеем» (501), а не «сейчас нельзя» (409): разные ответы ведут
    // пользователя в разные места.
    let Some(method) = tasks::portal_method(&action) else {
        return Err(ApiError::method_unavailable(format!(
            "the panel does not perform '{action}'; open the task in Bitrix24"
        )));
    };

    // Матрица: сначала из индекса (карточка уже на экране — значит она
    // прочитана), и только при промахе — одним запросом карточки.
    let known = state.task_entry(task_id);
    let matrix = match known.as_ref().filter(|e| !e.actions.is_empty()) {
        Some(entry) => entry.actions.clone(),
        None => {
            // Карточку читаем ради матрицы: `load_card` кладёт её в индекс, а
            // сам DTO здесь не нужен — ответ действия несёт СВЕЖУЮ карточку,
            // прочитанную уже после вызова портала.
            load_card(&state, &client, task_id).await?;
            state
                .task_entry(task_id)
                .map(|e| e.actions)
                .unwrap_or_default()
        }
    };
    if !matrix.allows(&action) {
        return Err(ApiError::invalid_state(format!(
            "action '{action}' is not allowed for this task right now"
        )));
    }

    client
        .task_action(method, task_id)
        .await
        .map_err(|e| map_portal_error(&e))?;

    // Карточка ПЕРЕЧИТЫВАЕТСЯ: и статус, и матрица уже другие (после `start`
    // становится `start:false, pause:true`), а собрать новую матрицу локально
    // невозможно — её правила знает только портал.
    let card = load_card(&state, &client, task_id).await.ok();

    // Портал только что написал в чат задачи системное сообщение о смене
    // статуса. Если чата у карточки всё ещё нет, добираем его вторым путём —
    // иначе первое действие над свежей задачей оставит пользователя без ленты
    // (Р20, ветка «чат появился позже»).
    let mut dialog_id = card.as_ref().and_then(|c| c.dialog_id.clone());
    if dialog_id.is_none() {
        if let Ok(Some(chat)) = client.task_chat_id(task_id).await {
            state.remember_task(task_id, Some(chat), ActionMatrix::default(), dto::now_unix());
            dialog_id = Some(tasks::dialog_id_of(chat));
        }
    }

    if let Some(task) = card.clone() {
        state.broadcast_event(B24ServerMsg::TaskUpdate {
            task_id: task.id.clone(),
            task,
        });
    }
    // Смена статуса двигает и счётчики задач, и строку списка: пусть
    // фолбэк-синхронизация не ждёт своей минуты.
    state.invalidate_counters().await;
    state.sync_nudge.notify_waiters();

    tracing::info!(
        target: "forge_bitrix",
        task_id,
        action = %action,
        method,
        status = card.as_ref().map(|c| c.status).unwrap_or(-1),
        "forge-bitrix: task action performed"
    );
    Ok(Json(TaskActionResp {
        action,
        ok: true,
        task: card,
        dialog_id,
    }))
}

// ---------------------------------------------------------------------------
// Общее.
// ---------------------------------------------------------------------------

/// Читает карточку у портала и обновляет индекс.
///
/// Одно место на все три вызывающих (карточка, промах индекса, перечитывание
/// после действия): вторая копия однажды забыла бы обновить индекс, и матрица
/// в нём осталась бы от прошлого статуса — то есть кнопка «Начать» жила бы на
/// уже выполняющейся задаче.
///
/// «Задачи нет» приезжает от портала УСПЕХОМ с пустым результатом
/// (`{"result": []}`, проверено живьём), и здесь это превращается в
/// `404 unknown_task` — а не в `502`, каким выглядела бы неудача разбора.
///
/// # Errors
///
/// `404 unknown_task` — задачи нет либо она недоступна владельцу вебхука;
/// плюс всё, чем может ответить портал на `tasks.task.get`.
async fn load_card(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    task_id: i64,
) -> Result<TaskDto, ApiError> {
    let raw = client
        .task_get(task_id)
        .await
        .map_err(|e| map_portal_error(&e))?
        .ok_or_else(|| ApiError::unknown_task("no such task in this portal".to_string()))?;
    let now = dto::now_unix();
    let origin = state.webhook().await.portal_origin();
    let dto = tasks::task_from_raw(&raw, origin.as_deref(), now);
    state.remember_task(task_id, dto.chat_id, ActionMatrix::from_raw(&raw.action), now);
    Ok(dto)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пресеты фильтра — контракт с фронтом: переименование обязано валить
    /// тест, а не молча превращать директорию в «все задачи».
    #[test]
    fn the_filter_presets_are_frozen() {
        let keys: Vec<&str> = FILTERS.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["all", "mine", "active", "expired", "closed"]);
        assert_eq!(normalize_filter(None), "all");
        assert_eq!(normalize_filter(Some("expired")), "expired");
        assert_eq!(normalize_filter(Some("нет-такого")), "all");
    }

    /// ЛОВУШКА 3 НА УРОВНЕ РОУТА: ни один пресет не рождает ключ `STATUS`.
    ///
    /// Проверяется именно результат сборки, а не намерение: живьём такой ключ
    /// даёт ноль записей молча, и заметить это в UI невозможно — директория
    /// просто пуста.
    #[test]
    fn no_preset_ever_produces_the_forbidden_status_key() {
        for (preset, _) in FILTERS {
            let filter = portal_filter(preset, 1, Some(1), Some(0), 1_785_993_630);
            let map = filter.as_object().expect("фильтр — объект");
            assert!(
                !map.contains_key(tasks::FILTER_STATUS_KEY),
                "пресет '{preset}' собрал запрещённый ключ STATUS: {filter}"
            );
            // И сам вызов обязан собраться без ошибки.
            crate::api::methods::task_list_params(&filter, 0)
                .unwrap_or_else(|e| panic!("пресет '{preset}' отвергнут транспортом: {e}"));
        }
    }

    /// Пресеты выражают состояние ТОЛЬКО через `REAL_STATUS` и операторные
    /// формы.
    #[test]
    fn the_presets_use_real_status_only() {
        let all = portal_filter("all", 1, None, None, 0);
        assert_eq!(
            all,
            json!({"MEMBER": 1}),
            "«все» — это все задачи С УЧАСТИЕМ, а не весь портал"
        );

        let mine = portal_filter("mine", 7, None, None, 0);
        assert_eq!(mine["RESPONSIBLE_ID"], 7);
        assert_eq!(mine["MEMBER"], 7);

        let active = portal_filter("active", 1, None, None, 0);
        assert_eq!(active["<REAL_STATUS"], 5);

        let closed = portal_filter("closed", 1, None, None, 0);
        assert_eq!(closed["REAL_STATUS"], 5);

        let expired = portal_filter("expired", 1, None, None, 1_785_993_630);
        assert_eq!(expired["<REAL_STATUS"], 5);
        assert!(
            expired["<DEADLINE"].as_str().unwrap().starts_with("2026-"),
            "срок обязан уехать датой: {expired}"
        );
    }

    /// Уточнения ложатся поверх пресета и не отменяют его.
    #[test]
    fn the_refinements_stack_on_top_of_the_preset() {
        let f = portal_filter("active", 1, Some(19), Some(0), 0);
        assert_eq!(f["<REAL_STATUS"], 5);
        assert_eq!(f["RESPONSIBLE_ID"], 19);
        assert_eq!(f["GROUP_ID"], 0);

        // Мусорные значения не попадают в фильтр вовсе.
        let f = portal_filter("all", 1, Some(-3), Some(-1), 0);
        assert_eq!(f, json!({"MEMBER": 1}));
    }

    /// Каждый пресет ограничен участием владельца вебхука в ЛЮБОЙ роли; без
    /// известного владельца ограничение не ставится (ноль дал бы пустой
    /// список).
    #[test]
    fn every_preset_is_scoped_to_membership() {
        for (preset, _) in FILTERS {
            let f = portal_filter(preset, 2816, None, None, 1_785_993_630);
            assert_eq!(f[tasks::FILTER_MEMBER_KEY], 2816, "пресет '{preset}': {f}");
            let f = portal_filter(preset, 0, None, None, 1_785_993_630);
            assert!(
                f.get(tasks::FILTER_MEMBER_KEY).is_none(),
                "пресет '{preset}' без владельца: {f}"
            );
        }
    }

    /// Ступень 3: `:id` — это чужой ввод, и «не разбирается» = «нет такой
    /// задачи», а не «плохой параметр».
    #[test]
    fn hostile_task_ids_are_a_404_not_a_400() {
        for good in ["1", " 42 ", "999999"] {
            assert!(require_task_id(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "0",
            "-7",
            "abc",
            "1;drop",
            "../../etc/passwd",
            "99999999999999999999999999",
            "1.5",
        ] {
            let err = require_task_id(bad).expect_err(bad);
            assert_eq!(err.status.as_u16(), 404, "{bad}");
            assert_eq!(err.body.code, "unknown_task", "{bad}");
            assert!(
                !err.body.message.contains(bad.trim()) || bad.trim().is_empty(),
                "враждебная строка отражена в ответе: {bad}"
            );
        }
    }

    /// Дата фильтра — ISO-8601 в UTC: она не зависит от того, где стоит
    /// devforge.
    #[test]
    fn the_filter_date_is_iso_utc() {
        let s = format_portal_date(1_785_993_630);
        assert!(s.ends_with('Z'), "{s}");
        assert!(s.starts_with("2026-08-06T"), "{s}");
    }
}
