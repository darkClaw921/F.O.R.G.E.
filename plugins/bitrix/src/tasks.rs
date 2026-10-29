//! Задачи портала: модель карточки, таблицы статусов и приоритетов, матрица
//! действий и индекс `task_id ↔ chat_id`.
//!
//! # ГЛАВНЫЙ АРХИТЕКТУРНЫЙ ФАКТ
//!
//! **Чат задачи — обычный IM-чат** с `entity_type = "TASKS_TASK"` и
//! `entity_id = <task_id>`. Комментарии задачи и сообщения её чата — ОДИН И ТОТ
//! ЖЕ поток: `task.commentitem.add` возвращает идентификатор сообщения ЧАТА, а
//! `task.commentitem.getlist` отдаёт `[]` даже когда в чате четыре сообщения
//! (legacy-форум на «новых задачах» мёртв, `forumTopicId` у задачи `null`).
//!
//! Отсюда всё устройство фазы: ленту рисует тот же движок, что и обычный чат
//! ([`crate::routes::dialogs`]), а «задачи» — это директория одного списка (Р2)
//! плюс обогащённая ШАПКА над лентой, которую и собирает этот модуль.
//!
//! ```text
//!   tasks.task.get → chatId ──┐
//!                             ├─→ DIALOG_ID = "chat" + chatId ─→ im.dialog.messages.get
//!   im.chat.get{TASKS_TASK} ──┘        (лента = комментарии задачи)
//! ```
//!
//! # ТРИ ЛОВУШКИ ПЛАТФОРМЫ, ЖИВУЩИЕ В ЭТОМ МОДУЛЕ
//!
//! Все три дают не ошибку, а МОЛЧАЛИВУЮ ПУСТОТУ — то есть выглядят как
//! «данных нет», а не как «мы спросили неправильно». Каждая закрыта
//! тестом-стражем (`plugins/bitrix/tests/tasks_traps.rs`).
//!
//! | № | что | как выглядит | чем закрыта |
//! |---|---|---|---|
//! | 1 | `ENTITY_TYPE = "TASKS"` | `{"result":null}` при HTTP 200 | [`ENTITY_TASK`] — единственная константа, страж запрещает литерал `"TASKS"` |
//! | 2 | camelCase в `select` | поле приезжает `null`, неотличимо от настоящего `null` | [`LIST_SELECT`] — только UPPERCASE, карточка зовётся БЕЗ `select` |
//! | 3 | `{"STATUS": 2}` в фильтре | ноль записей при двух подходящих | [`FILTER_STATUS_KEY`] — только `REAL_STATUS` и операторные формы |
//!
//! # Матрица `action` — её нельзя захардкодить
//!
//! Портал присылает в карточке готовый набор булевых флагов:
//! `{"complete":true,"start":true,"pause":false,"delegate":true,…}`. Он
//! ДИНАМИЧЕН и зависит и от роли, и от текущего состояния: после
//! `tasks.task.start` становится `start:false, pause:true, defer:false`, после
//! `pause` возвращается обратно. Поэтому:
//!
//! 1. кнопки рисуются СТРОГО по флагам со значением `true`
//!    ([`TaskDto::actions`]);
//! 2. действие с флагом `false` отвергается ДО обращения к порталу
//!    (409 `invalid_state`) — иначе вызов уходит впустую и молча ничего не
//!    делает (именно так `tasks.task.defer` на выполняющейся задаче оказался
//!    no-op при разведке);
//! 3. набор действий не перечисляется во фронте — он приезжает с карточкой.
//!
//! [`ACTION_METHODS`] — это НЕ список доступных действий, а таблица
//! «действие → метод портала»: она отвечает на вопрос «умеет ли панель это
//! выполнить», тогда как на вопрос «можно ли это сейчас» отвечает матрица.
//!
//! # Индекс `task_id ↔ chat_id`
//!
//! [`TaskIndex`] — маленький кэш в памяти, который держит по задаче её чат и
//! её матрицу действий. Он существует ради одного требования: **действие с
//! флагом `false` обязано отвергаться БЕЗ обращения к порталу**. Карточка уже
//! на экране, значит матрица уже известна — спрашивать её второй раз означало
//! бы тратить один из двух запросов в секунду на то, чтобы ответить «нельзя».
//!
//! Обратное направление (`chat_id → task_id`) нужно ленте: по открытому
//! `chatN` панель понимает, какую карточку показывать над ним.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::api::schema::TaskRaw;

/// Сущность портала, означающая чат задачи.
///
/// Реэкспорт [`crate::dto::ENTITY_TASK`], а не вторая константа: значение
/// критично (см. ловушку 1 в шапке модуля), и два литерала однажды разъехались
/// бы.
pub use crate::dto::ENTITY_TASK;

/// Размер страницы `tasks.task.list` — ФИКСИРОВАННЫЙ у платформы.
///
/// Метод не принимает `LIMIT` вовсе: страница всегда пятьдесят записей, а
/// листается смещением `start = (N-1) * 50`. Просить меньше нечем, просить
/// больше — тоже.
pub const PAGE_SIZE: i64 = 50;

/// Ключ фильтра, использование которого ЗАПРЕЩЕНО (ловушка 3).
///
/// `{"STATUS": 2}` — псевдополе: оно пересекается с виртуальными состояниями
/// (`subStatus`), и живьём комбинация `{"RESPONSIBLE_ID":1,"STATUS":2,
/// "GROUP_ID":0}` вернула `total: 0` при двух полностью подходящих задачах.
/// Отказа при этом нет — ноль записей и всё.
///
/// Разрешены: [`FILTER_REAL_STATUS_KEY`] и операторные формы (`<STATUS`,
/// `>STATUS`), потому что оператор адресует РЕАЛЬНОЕ поле.
pub const FILTER_STATUS_KEY: &str = "STATUS";

/// Ключ фильтра по настоящему статусу — единственный, который работает.
pub const FILTER_REAL_STATUS_KEY: &str = "REAL_STATUS";

/// Ключ фильтра «пользователь фигурирует в задаче» в ЛЮБОЙ роли: постановщик,
/// ответственный, соисполнитель, наблюдатель.
///
/// Им ограничен КАЖДЫЙ пресет списка: вебхук обычно выпущен от
/// администратора, а администратору `tasks.task.list` без фильтра отдаёт
/// задачи ВСЕГО портала (живьём 76 840 против 28 своих).
pub const FILTER_MEMBER_KEY: &str = "MEMBER";

/// Поля `select` для СПИСКА задач — все UPPERCASE (ловушка 2).
///
/// Имена полей в `select` — это имена v1 (`CHAT_ID`, `RESPONSIBLE_ID`), а в
/// ОТВЕТЕ те же поля приезжают camelCase (`chatId`, `responsibleId`).
/// camelCase в `select` портал МОЛЧА игнорирует: `select:["ID","chatId"]`
/// вернул `chatId: null`, а `select:["ID","CHAT_ID"]` — `chatId: 7`. Пустое
/// значение при этом неотличимо от настоящего `null`, поэтому ошибка живёт
/// месяцами.
///
/// У КАРТОЧКИ `select` нет вовсе: `tasks.task.get` без него отдаёт всё, и
/// угадывать имена не нужно (см. [`crate::api::B24Client::task_get`]).
pub const LIST_SELECT: [&str; 14] = [
    "ID",
    "TITLE",
    "STATUS",
    "SUB_STATUS",
    "PRIORITY",
    "RESPONSIBLE_ID",
    "CREATED_BY",
    "DEADLINE",
    "CREATED_DATE",
    "CLOSED_DATE",
    "ACTIVITY_DATE",
    "GROUP_ID",
    "CHAT_ID",
    "NEW_COMMENTS_COUNT",
];

// ---------------------------------------------------------------------------
// Статусы и приоритеты.
// ---------------------------------------------------------------------------

/// Статус «новая» — служебный, в списках почти не встречается.
pub const STATUS_NEW: i64 = 1;
/// Статус «ждёт выполнения» (дефолт свежей задачи).
pub const STATUS_PENDING: i64 = 2;
/// Статус «выполняется».
pub const STATUS_IN_PROGRESS: i64 = 3;
/// Статус «ожидает контроля».
pub const STATUS_SUPPOSEDLY_COMPLETED: i64 = 4;
/// Статус «завершена». Всё, что МЕНЬШЕ, считается открытым.
pub const STATUS_COMPLETED: i64 = 5;
/// Статус «отложена».
pub const STATUS_DEFERRED: i64 = 6;
/// Статус «отклонена».
pub const STATUS_DECLINED: i64 = 7;

/// Подпись статуса по числовому коду v1.
///
/// Коды — то, что реально приходит в поле `status` (строкой: `"2"`). Проверено
/// живьём: `2 → tasks.task.start → 3 → tasks.task.pause → 2`, а подпись «Ждёт
/// выполнения» для `status:"2"` подтверждена гридом системного сообщения в
/// чате задачи.
pub fn status_label(status: i64) -> &'static str {
    match status {
        STATUS_NEW => "Новая",
        STATUS_PENDING => "Ждёт выполнения",
        STATUS_IN_PROGRESS => "Выполняется",
        STATUS_SUPPOSEDLY_COMPLETED => "Ожидает контроля",
        STATUS_COMPLETED => "Завершена",
        STATUS_DEFERRED => "Отложена",
        STATUS_DECLINED => "Отклонена",
        _ => "—",
    }
}

/// Машинный ключ статуса — им фронт красит статус-точку.
///
/// Отдельно от подписи: подпись человеческая и может быть переведена, ключ —
/// имя CSS-класса, и менять его нельзя.
pub fn status_key(status: i64) -> &'static str {
    match status {
        STATUS_NEW => "new",
        STATUS_PENDING => "pending",
        STATUS_IN_PROGRESS => "in_progress",
        STATUS_SUPPOSEDLY_COMPLETED => "review",
        STATUS_COMPLETED => "completed",
        STATUS_DEFERRED => "deferred",
        STATUS_DECLINED => "declined",
        _ => "unknown",
    }
}

/// Закрыта ли задача (статус ≥ «завершена»).
///
/// Ровно по этому признаку считается просрочка: у отклонённой или завершённой
/// задачи прошедший срок уже ничего не значит.
pub fn is_closed(status: i64) -> bool {
    status >= STATUS_COMPLETED
}

/// Приоритет «низкий».
pub const PRIORITY_LOW: i64 = 0;
/// Приоритет «средний» (значение по умолчанию у портала).
pub const PRIORITY_NORMAL: i64 = 1;
/// Приоритет «высокий».
pub const PRIORITY_HIGH: i64 = 2;

/// Подпись приоритета.
pub fn priority_label(priority: i64) -> &'static str {
    match priority {
        PRIORITY_LOW => "низкий",
        PRIORITY_HIGH => "высокий",
        // Средний — и значение по умолчанию, и всё незнакомое: приоритет
        // отсутствующего значения не «неизвестен», а обычен.
        _ => "средний",
    }
}

/// Машинный ключ приоритета — им фронт красит метку.
pub fn priority_key(priority: i64) -> &'static str {
    match priority {
        PRIORITY_LOW => "low",
        PRIORITY_HIGH => "high",
        _ => "normal",
    }
}

/// Значение `subStatus`, означающее «виртуального состояния нет».
pub const SUB_STATUS_NONE: i64 = -1;

// ---------------------------------------------------------------------------
// Матрица действий.
// ---------------------------------------------------------------------------

/// Таблица «ключ действия → метод портала».
///
/// Это ответ на вопрос **«умеет ли панель выполнить это действие»**, а НЕ
/// «доступно ли оно сейчас»: второе решает матрица `action` конкретной задачи.
/// Разделение принципиально — матрица динамична и приезжает с портала, а
/// таблица статична и описывает наши возможности.
///
/// Перечислены только действия, у которых есть семантический метод БЕЗ
/// обязательных дополнительных полей. Их портал проводит через свою
/// бизнес-логику (контроль, оценка) и пишет в чат задачи правильное системное
/// сообщение — чего `tasks.task.update {STATUS: …}` не делает.
///
/// Чего здесь намеренно нет:
///
/// | действие | почему |
/// |---|---|
/// | `edit`, `changeDeadline`, `edit.plan` | требуют формы редактирования, которой в панели нет |
/// | `delegate` | требует ВЫБОРА сотрудника; панель не умеет искать по оргструктуре (scope `user` вебхуку не выдан) |
/// | `remove` | безвозвратно удаляет задачу; «случайно нажал» здесь стоит слишком дорого |
/// | `rate`, `checklist.*`, `elapsedtime.add`, `favorite.*` | сущности, которых в панели нет вовсе |
///
/// Всё это остаётся видимым в карточке ссылкой «открыть в Битриксе»: панель —
/// мессенджер с шапкой задачи, а не второй интерфейс задач.
pub const ACTION_METHODS: [(&str, &str); 7] = [
    ("start", "tasks.task.start"),
    ("pause", "tasks.task.pause"),
    ("complete", "tasks.task.complete"),
    ("defer", "tasks.task.defer"),
    ("renew", "tasks.task.renew"),
    ("approve", "tasks.task.approve"),
    ("disapprove", "tasks.task.disapprove"),
];

/// Метод портала для действия, если панель умеет его выполнять.
pub fn portal_method(action: &str) -> Option<&'static str> {
    ACTION_METHODS
        .iter()
        .find(|(key, _)| *key == action)
        .map(|(_, method)| *method)
}

/// Похожа ли строка на ключ действия портала.
///
/// Ключи бывают составными (`edit.originator`, `checklist.reorder`,
/// `dayplan.timer.toggle`) — точка в них законна. Проверка нужна ДО обращения
/// к порталу: `:action` приходит из URL, то есть это чужой ввод.
pub fn is_action_key(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 48
        && raw.starts_with(|c: char| c.is_ascii_alphabetic())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
}

/// Матрица прав задачи: ключ действия → разрешено ли оно ПРЯМО СЕЙЧАС.
///
/// Хранится целиком (а не только разрешённое), потому что отличать «действие
/// сейчас нельзя» от «портал про такое действие не знает» приходится в разных
/// ветках, и склеивать их в одно «нет» значило бы отвечать пользователю
/// «нельзя» там, где правильный ответ — «панель этого не умеет».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionMatrix(HashMap<String, bool>);

impl ActionMatrix {
    /// Собирает матрицу из сырого `action` карточки.
    ///
    /// Значения приезжают [`serde_json::Value`], а не `bool`: одно неожиданное
    /// значение не должно ронять разбор всей карточки (см. `TaskRaw::action`).
    /// Всё, что не `true`, считается запретом — «не разрешено явно» и есть
    /// запрет.
    pub fn from_raw(raw: &crate::api::schema::PhpMap<serde_json::Value>) -> Self {
        let map = match raw {
            crate::api::schema::PhpMap::Map(m) => m
                .iter()
                .map(|(k, v)| (k.clone(), v.as_bool().unwrap_or(false)))
                .collect(),
            crate::api::schema::PhpMap::List(_) => HashMap::new(),
        };
        Self(map)
    }

    /// Разрешено ли действие прямо сейчас.
    ///
    /// Отсутствие ключа — это `false`: портал перечисляет всё, что знает, и
    /// молчание означает «недоступно».
    pub fn allows(&self, action: &str) -> bool {
        self.0.get(action).copied().unwrap_or(false)
    }

    /// Знает ли портал про такое действие вообще (независимо от значения).
    pub fn knows(&self, action: &str) -> bool {
        self.0.contains_key(action)
    }

    /// Разрешённые действия, ОТСОРТИРОВАННЫЕ, из числа выполнимых панелью.
    ///
    /// Ровно этот список уезжает в DTO и превращается в ряд кнопок. Сортировка
    /// не косметика: порядок обхода `HashMap` случаен, и без неё кнопки
    /// прыгали бы местами при каждом обновлении карточки.
    ///
    /// Фильтр по [`ACTION_METHODS`] здесь обязателен: кнопка, которую панель
    /// не умеет нажать, — это обещание, которое некому выполнить.
    pub fn allowed(&self) -> Vec<String> {
        let mut out: Vec<String> = ACTION_METHODS
            .iter()
            .map(|(key, _)| *key)
            .filter(|key| self.allows(key))
            .map(str::to_string)
            .collect();
        out.sort();
        out
    }

    /// Пуста ли матрица (карточка пришла из списка, где `action` не отдают).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// DTO карточки.
// ---------------------------------------------------------------------------

/// Карточка задачи — то, что рисуется ШАПКОЙ над лентой её чата.
///
/// Приезжает `GET /api/bitrix/tasks/:id`, страницей `GET /api/bitrix/tasks` и
/// WS-кадром `task_update`. Одна форма на все три источника: расхождение
/// означало бы, что карточка после действия выглядит иначе, чем после
/// перезагрузки вкладки.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDto {
    /// Идентификатор задачи СТРОКОЙ (как все идентификаторы плагина).
    pub id: String,
    /// Заголовок.
    pub title: String,
    /// Описание ПОСЛЕ разбора BB-кодов: портал размечает его `[B]`, `[URL]` и
    /// прочим, а карточка показывает текст.
    pub description: String,
    /// Числовой код статуса (2…7).
    pub status: i64,
    /// Подпись статуса.
    pub status_label: String,
    /// Машинный ключ статуса — имя CSS-класса статус-точки.
    pub status_key: String,
    /// Виртуальное состояние портала; `-1` ⇒ его нет.
    pub sub_status: i64,
    /// Числовой код приоритета (0/1/2).
    pub priority: i64,
    /// Подпись приоритета.
    pub priority_label: String,
    /// Машинный ключ приоритета.
    pub priority_key: String,
    /// Постановщик (строкой).
    pub created_by: String,
    /// Имя постановщика, если портал его прислал.
    pub created_by_name: Option<String>,
    /// Ответственный (строкой).
    pub responsible_id: String,
    /// Имя ответственного.
    pub responsible_name: Option<String>,
    /// Соисполнители.
    pub accomplices: Vec<String>,
    /// Наблюдатели.
    pub auditors: Vec<String>,
    /// Крайний срок (ATOM), если он задан.
    pub deadline: Option<String>,
    /// Когда поставлена.
    pub created_date: Option<String>,
    /// Когда закрыта.
    pub closed_date: Option<String>,
    /// Последняя активность.
    pub activity_date: Option<String>,
    /// **Просрочена.** Отдельного поля у портала НЕТ: признак считается как
    /// `deadline < now && status < 5`.
    pub overdue: bool,
    /// Задачу ещё не открывали (`notViewed == "Y"`).
    pub is_new: bool,
    /// Рабочая группа, если задача в ней (`"0"` ⇒ вне групп).
    pub group_id: Option<String>,
    /// Название группы, если портал его прислал.
    pub group_name: Option<String>,
    /// Чат задачи. **`None` ⇒ обсуждения ещё нет** (Р20) — лента не грузится,
    /// поле ввода блокируется с объяснением.
    pub chat_id: Option<i64>,
    /// `DIALOG_ID` чата задачи (`"chat7"`) — им лента адресуется без сборки на
    /// фронте.
    pub dialog_id: Option<String>,
    /// Непрочитанных комментариев по этой задаче.
    pub new_comments_count: i64,
    /// **Разрешённые ПРЯМО СЕЙЧАС действия**, которые панель умеет выполнить.
    /// Ряд кнопок строится строго по этому списку и перерисовывается после
    /// каждого действия — матрица динамична.
    pub actions: Vec<String>,
    /// Адрес задачи в интерфейсе портала.
    ///
    /// АБСОЛЮТНЫЙ и рабочий: без него ссылка «открыть в Битриксе» не открывает
    /// ничего. Содержит ХОСТ портала (единственная часть вебхука, которую
    /// можно показывать: пользователь и так видит её в адресной строке) и
    /// НИКОГДА — код вебхука. Собирается в
    /// [`crate::config::WebhookUrl::portal_origin`], страж —
    /// `the_task_card_never_carries_the_webhook_code`.
    pub url: Option<String>,
}

/// Счётчики СОСТОЯНИЯ задач (`tasks.task.counters.get`).
///
/// # Почему это отдельный тип, а не поля [`crate::dto::CountersDto`]
///
/// Портал считает две РАЗНЫЕ вещи разными методами, и складывать их нельзя:
///
/// | что | метод | обнуляется |
/// |---|---|---|
/// | непрочитанные СООБЩЕНИЯ чатов задач | `im.counters.get` → `TYPE.TASKS_TASK` | прочтением ленты |
/// | СОСТОЯНИЕ задач (просрочена, новые комментарии, упоминания) | `tasks.task.counters.get` | действием над задачей |
///
/// Просроченная задача может не иметь ни одного непрочитанного комментария, а
/// чат задачи — светиться непрочитанным без всякой просрочки. Сложенный бейдж
/// нельзя обнулить ни чтением, ни закрытием задачи — то есть он навсегда
/// «горит», и пользователь перестаёт на него смотреть.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCountersDto {
    /// Просроченных задач.
    pub expired: i64,
    /// Задач с новыми комментариями.
    pub new_comments: i64,
    /// Упоминаний в задачах.
    pub mentioned: i64,
}

impl TaskCountersDto {
    /// Проекция сырого `tasks.task.counters.get`.
    pub fn from_raw(raw: &crate::api::schema::TaskCountersRaw) -> Self {
        Self {
            expired: raw.expired.counter.max(0),
            new_comments: raw.new_comments.counter.max(0),
            mentioned: raw.mentioned.counter.max(0),
        }
    }

    /// Есть ли на что смотреть вообще.
    pub fn is_zero(&self) -> bool {
        self.expired == 0 && self.new_comments == 0 && self.mentioned == 0
    }
}

/// Относительный путь задачи в интерфейсе портала.
///
/// Форма подтверждена живьём (`entity_link.url` чата задачи):
/// `/company/personal/user/<uid>/tasks/task/view/<task_id>/`. Портал сам
/// перекидывает на групповой вид, если задача в рабочей группе, поэтому вторая
/// форма пути не нужна.
pub fn task_path(user_id: &str, task_id: &str) -> String {
    let uid = if user_id.trim().is_empty() {
        "0"
    } else {
        user_id.trim()
    };
    format!("/company/personal/user/{uid}/tasks/task/view/{task_id}/")
}

/// Проекция сырой задачи в карточку.
///
/// Чистая функция: ни сети, ни состояния — поэтому проверяется на живых
/// фикстурах ответов портала.
///
/// `portal_origin` — `https://<хост>` либо `None`, когда вебхук не разобран;
/// во втором случае ссылка «открыть в Битриксе» просто не появляется, а
/// карточка остаётся рабочей.
///
/// `now` передаётся аргументом, а не берётся из часов: просрочка — граничное
/// условие, и тест на неё обязан быть детерминированным.
pub fn task_from_raw(raw: &TaskRaw, portal_origin: Option<&str>, now: i64) -> TaskDto {
    let id = raw.id.as_string();
    let status = raw.status.as_i64().unwrap_or(STATUS_PENDING);
    let priority = raw.priority.as_i64().unwrap_or(PRIORITY_NORMAL);
    let responsible_id = raw.responsible_id.as_string();
    let created_by = raw.created_by.as_string();

    let description = raw
        .description
        .as_deref()
        .map(crate::bbcode::to_plain)
        .unwrap_or_default();

    // Ноль и пустая строка означают «чата нет» так же, как и отсутствие поля:
    // портал отдаёт все три формы (Р20).
    let chat_id = raw.chat_id.as_i64().filter(|v| *v > 0);

    let deadline = raw.deadline.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let overdue = !is_closed(status)
        && deadline
            .and_then(parse_atom_unix)
            .is_some_and(|ts| ts < now);

    let group_id = raw
        .group_id
        .as_i64()
        .filter(|v| *v > 0)
        .map(|v| v.to_string());
    let group_name = raw
        .group
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    TaskDto {
        title: raw.title.clone().unwrap_or_default(),
        description,
        status,
        status_label: status_label(status).to_string(),
        status_key: status_key(status).to_string(),
        sub_status: raw.sub_status.as_i64().unwrap_or(SUB_STATUS_NONE),
        priority,
        priority_label: priority_label(priority).to_string(),
        priority_key: priority_key(priority).to_string(),
        created_by_name: raw
            .creator
            .as_ref()
            .and_then(|u| u.name.clone())
            .filter(|n| !n.trim().is_empty()),
        responsible_name: raw
            .responsible
            .as_ref()
            .and_then(|u| u.name.clone())
            .filter(|n| !n.trim().is_empty()),
        accomplices: raw.accomplices.iter().map(|v| v.as_string()).collect(),
        auditors: raw.auditors.iter().map(|v| v.as_string()).collect(),
        deadline: deadline.map(str::to_string),
        created_date: raw.created_date.clone(),
        closed_date: raw.closed_date.clone(),
        activity_date: raw.activity_date.clone(),
        overdue,
        is_new: raw.not_viewed.as_deref() == Some("Y"),
        group_id,
        group_name,
        chat_id,
        dialog_id: chat_id.map(dialog_id_of),
        new_comments_count: raw.new_comments_count.max(0),
        actions: ActionMatrix::from_raw(&raw.action).allowed(),
        url: portal_origin.map(|origin| {
            format!(
                "{}{}",
                origin.trim_end_matches('/'),
                task_path(&responsible_id, &id)
            )
        }),
        responsible_id,
        created_by,
        id,
    }
}

/// `DIALOG_ID` чата задачи из его числового идентификатора.
///
/// Конкатенация, а не запрос: `"chat" + chatId` — правило платформы, а не
/// эвристика (подтверждено `im.dialog.get {DIALOG_ID:"chat7"}` → `entity_id:
/// "1"`).
pub fn dialog_id_of(chat_id: i64) -> String {
    format!("chat{chat_id}")
}

/// Unix-время из даты портала (ATOM с таймзоной: `2026-07-04T19:00:00+03:00`).
///
/// `None` на всём, что не разбирается: неразобранный срок обязан означать
/// «просрочку посчитать нечем», а не «просрочено».
pub fn parse_atom_unix(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.timestamp())
}

// ---------------------------------------------------------------------------
// Индекс task_id ↔ chat_id.
// ---------------------------------------------------------------------------

/// Что индекс помнит о задаче.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskEntry {
    /// Чат задачи, если он есть.
    pub chat_id: Option<i64>,
    /// Матрица прав на момент последнего чтения карточки.
    pub actions: ActionMatrix,
    /// Когда запись обновлена (unix).
    pub updated_at: i64,
}

/// Потолок индекса.
///
/// Портал отдаёт задачи страницами по пятьдесят, и держать их все незачем:
/// индекс нужен открытой карточке и последним просмотренным. При переполнении
/// вычищается самая старая запись — обращение к порталу за карточкой стоит
/// одного запроса, а неограниченная карта на длинном портале стоит памяти
/// процесса.
pub const INDEX_CAPACITY: usize = 512;

/// Индекс `task_id ↔ chat_id` и матриц действий.
///
/// # Зачем он существует
///
/// Ради одного требования: **действие, запрещённое матрицей, обязано
/// отвергаться БЕЗ обращения к порталу**. Карточка уже на экране — значит
/// матрица уже прочитана, и спрашивать её второй раз только чтобы ответить
/// «нельзя» означало бы тратить один из двух запросов в секунду впустую.
///
/// Заодно он отвечает на обратный вопрос ленты: «открыт `chat9` — какую
/// карточку показывать над ним».
///
/// # Почему это не кэш ответов
///
/// Индекс НЕ подменяет `tasks.task.get`: карточка всегда читается у портала
/// заново (статус мог измениться в чужом браузере). Здесь живёт только то, что
/// нужно принять решение ДО запроса.
#[derive(Debug, Default)]
pub struct TaskIndex {
    /// `task_id` → запись.
    by_task: HashMap<i64, TaskEntry>,
    /// `chat_id` → `task_id`.
    by_chat: HashMap<i64, i64>,
}

impl TaskIndex {
    /// Пустой индекс.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запоминает связь и матрицу.
    ///
    /// Матрица кладётся ТОЛЬКО непустой: строка списка `tasks.task.list`
    /// приходит без `action` вовсе, и затирать ею матрицу, прочитанную
    /// карточкой, значило бы запрещать пользователю все действия сразу после
    /// обновления списка.
    pub fn remember(&mut self, task_id: i64, chat_id: Option<i64>, actions: ActionMatrix, now: i64) {
        if task_id <= 0 {
            return;
        }
        let entry = self.by_task.entry(task_id).or_default();
        if chat_id.is_some() {
            entry.chat_id = chat_id;
        }
        if !actions.is_empty() {
            entry.actions = actions;
        }
        entry.updated_at = now;
        if let Some(chat) = chat_id.filter(|v| *v > 0) {
            self.by_chat.insert(chat, task_id);
        }
        self.evict();
    }

    /// Запись о задаче.
    pub fn get(&self, task_id: i64) -> Option<&TaskEntry> {
        self.by_task.get(&task_id)
    }

    /// Чат задачи, если он известен.
    pub fn chat_of(&self, task_id: i64) -> Option<i64> {
        self.by_task.get(&task_id).and_then(|e| e.chat_id)
    }

    /// Задача чата — обратное направление для ленты.
    pub fn task_of_chat(&self, chat_id: i64) -> Option<i64> {
        self.by_chat.get(&chat_id).copied()
    }

    /// Сколько задач помнит индекс.
    pub fn len(&self) -> usize {
        self.by_task.len()
    }

    /// Пуст ли индекс.
    pub fn is_empty(&self) -> bool {
        self.by_task.is_empty()
    }

    /// Забывает всё — вызывается при logout: чужие задачи в памяти после
    /// смены вебхука не нужны никому.
    pub fn clear(&mut self) {
        self.by_task.clear();
        self.by_chat.clear();
    }

    /// Выселяет самую старую запись при переполнении.
    fn evict(&mut self) {
        while self.by_task.len() > INDEX_CAPACITY {
            let Some(oldest) = self
                .by_task
                .iter()
                .min_by_key(|(id, e)| (e.updated_at, **id))
                .map(|(id, _)| *id)
            else {
                return;
            };
            if let Some(entry) = self.by_task.remove(&oldest) {
                if let Some(chat) = entry.chat_id {
                    self.by_chat.remove(&chat);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(value: serde_json::Value) -> TaskRaw {
        serde_json::from_value(value).expect("сырая задача разбирается")
    }

    /// Живой ответ `tasks.task.get` тестового портала (сокращён до полей,
    /// которые читает проекция).
    fn live_card() -> TaskRaw {
        raw(json!({
            "id": "1",
            "title": "Прислать финальные тексты по фичам до конца недели",
            "description": "Из встречи [B]«Совещание по запуску продукта»[/B]",
            "descriptionInBbcode": "Y",
            "priority": "1",
            "status": "2",
            "createdBy": "1",
            "responsibleId": "1",
            "createdDate": "2026-06-29T14:40:42+03:00",
            "deadline": "2026-07-04T19:00:00+03:00",
            "activityDate": "2026-08-05T19:51:04+03:00",
            "groupId": "0",
            "group": [],
            "auditors": [],
            "accomplices": [],
            "chatId": 7,
            "newCommentsCount": 0,
            "notViewed": "N",
            "creator": {"id": "1", "name": "Игорь Герасимов", "link": "/company/personal/user/1/"},
            "responsible": {"id": "1", "name": "Игорь Герасимов", "link": "/company/personal/user/1/"},
            "action": {
                "complete": true, "start": true, "pause": false, "delegate": true,
                "defer": true, "renew": false, "edit": true, "remove": true,
                "changeDeadline": true, "rate": true, "take": false, "approve": false,
                "accept": false, "decline": false, "checklist.add": true
            }
        }))
    }

    /// ТАБЛИЦА СТАТУСОВ. Коды — контракт с порталом: переименование обязано
    /// валить тест, а не молча менять подпись в карточке.
    #[test]
    fn the_status_table_matches_the_portal() {
        let table = [
            (1, "Новая", "new"),
            (2, "Ждёт выполнения", "pending"),
            (3, "Выполняется", "in_progress"),
            (4, "Ожидает контроля", "review"),
            (5, "Завершена", "completed"),
            (6, "Отложена", "deferred"),
            (7, "Отклонена", "declined"),
        ];
        for (code, label, key) in table {
            assert_eq!(status_label(code), label, "код {code}");
            assert_eq!(status_key(code), key, "код {code}");
        }
        // Незнакомый код не паникует и не врёт подписью.
        assert_eq!(status_label(99), "—");
        assert_eq!(status_key(99), "unknown");

        // Открытость: всё, что меньше «завершена», открыто.
        for open in [1, 2, 3, 4] {
            assert!(!is_closed(open), "статус {open} открыт");
        }
        for closed in [5, 6, 7] {
            assert!(is_closed(closed), "статус {closed} закрыт");
        }
    }

    /// ТАБЛИЦА ПРИОРИТЕТОВ. Отсутствующее значение — «средний», а не
    /// «неизвестный»: у портала это дефолт.
    #[test]
    fn the_priority_table_matches_the_portal() {
        assert_eq!(priority_label(0), "низкий");
        assert_eq!(priority_label(1), "средний");
        assert_eq!(priority_label(2), "высокий");
        assert_eq!(priority_label(7), "средний");
        assert_eq!(priority_key(0), "low");
        assert_eq!(priority_key(1), "normal");
        assert_eq!(priority_key(2), "high");
    }

    /// Кнопки строятся СТРОГО по флагам `true` — и только по тем действиям,
    /// которые панель умеет выполнить.
    #[test]
    fn buttons_are_built_strictly_from_the_true_flags() {
        let matrix = ActionMatrix::from_raw(&live_card().action);

        assert!(matrix.allows("start"));
        assert!(!matrix.allows("pause"));
        // Портал знает про `pause`, просто сейчас нельзя — это РАЗНЫЕ вещи.
        assert!(matrix.knows("pause"));
        assert!(!matrix.knows("нет-такого-действия"));

        // `delegate` и `edit` разрешены порталом, но панель их не выполняет —
        // кнопки быть не должно.
        assert!(matrix.allows("delegate"));
        assert_eq!(matrix.allowed(), vec!["complete".to_string(), "defer".into(), "start".into()]);
    }

    /// Матрица ДИНАМИЧНА: после `start` она переворачивается. Захардкоженный
    /// набор кнопок дал бы «Начать» на уже выполняющейся задаче.
    #[test]
    fn the_matrix_flips_with_the_status() {
        let before = ActionMatrix::from_raw(&live_card().action);
        assert_eq!(before.allowed(), vec!["complete", "defer", "start"]);

        let after = ActionMatrix::from_raw(
            &raw(json!({"action": {"start": false, "pause": true, "defer": false, "complete": true}}))
                .action,
        );
        assert_eq!(after.allowed(), vec!["complete", "pause"]);
    }

    /// Пустой `action` (PHP-массив `[]` либо строка списка, где матрицы нет
    /// вовсе) не даёт ни одной кнопки — и не паникует.
    #[test]
    fn an_empty_matrix_yields_no_buttons() {
        let empty = ActionMatrix::from_raw(&raw(json!({"action": []})).action);
        assert!(empty.is_empty());
        assert!(empty.allowed().is_empty());
        assert!(!empty.allows("start"));

        let missing = ActionMatrix::from_raw(&raw(json!({})).action);
        assert!(missing.allowed().is_empty());
    }

    /// Нелогическое значение флага не роняет карточку: оно означает «нельзя».
    #[test]
    fn a_non_boolean_flag_is_a_denial_not_a_crash() {
        let m = ActionMatrix::from_raw(&raw(json!({"action": {"start": "Y", "pause": 1}})).action);
        assert!(!m.allows("start"));
        assert!(!m.allows("pause"));
        assert!(m.knows("start"), "ключ всё же известен");
    }

    /// Таблица «действие → метод» и разбор ключа действия.
    #[test]
    fn the_action_method_table_is_frozen() {
        assert_eq!(portal_method("start"), Some("tasks.task.start"));
        assert_eq!(portal_method("complete"), Some("tasks.task.complete"));
        assert_eq!(portal_method("disapprove"), Some("tasks.task.disapprove"));
        // Панель не выполняет — и не должна делать вид, что выполняет.
        assert_eq!(portal_method("delegate"), None);
        assert_eq!(portal_method("remove"), None);
        assert_eq!(portal_method("edit"), None);

        // Все методы таблицы — из пространства задач.
        for (_, method) in ACTION_METHODS {
            assert!(method.starts_with("tasks.task."), "{method}");
        }
    }

    /// `:action` — это чужой ввод из URL. Составные ключи законны, мусор — нет.
    #[test]
    fn action_keys_are_validated_before_anything_else() {
        for good in ["start", "checklist.add", "edit.originator", "dayplan.timer.toggle"] {
            assert!(is_action_key(good), "{good}");
        }
        for bad in [
            "",
            "1start",
            "../../etc/passwd",
            "start;drop",
            "старт",
            "a".repeat(49).as_str(),
        ] {
            assert!(!is_action_key(bad), "{bad}");
        }
    }

    /// Проекция живого ответа портала.
    #[test]
    fn the_live_card_projects_into_the_dto() {
        // «Сейчас» ПОЗЖЕ срока — задача просрочена и не закрыта.
        let now = parse_atom_unix("2026-08-05T12:00:00+03:00").unwrap();
        let dto = task_from_raw(&live_card(), Some("https://test-work-2.bitrix24.ru"), now);

        assert_eq!(dto.id, "1");
        assert_eq!(dto.status, 2);
        assert_eq!(dto.status_label, "Ждёт выполнения");
        assert_eq!(dto.priority_label, "средний");
        assert_eq!(dto.responsible_id, "1");
        assert_eq!(dto.responsible_name.as_deref(), Some("Игорь Герасимов"));
        assert_eq!(dto.chat_id, Some(7));
        assert_eq!(dto.dialog_id.as_deref(), Some("chat7"));
        assert!(dto.overdue, "срок 4 июля, «сейчас» 5 августа");
        assert_eq!(dto.actions, vec!["complete", "defer", "start"]);
        assert_eq!(
            dto.url.as_deref(),
            Some("https://test-work-2.bitrix24.ru/company/personal/user/1/tasks/task/view/1/")
        );
        // BB-коды описания разобраны в текст.
        assert!(dto.description.contains("«Совещание по запуску продукта»"));
        assert!(!dto.description.contains("[B]"));
        assert_eq!(dto.group_id, None, "groupId=0 — задача вне групп");
    }

    /// Просрочка — вычисляемый признак, а не поле портала.
    #[test]
    fn overdue_is_computed_from_the_deadline_and_the_status() {
        let after = parse_atom_unix("2026-08-05T12:00:00+03:00").unwrap();
        let before = parse_atom_unix("2026-07-01T12:00:00+03:00").unwrap();

        let open = live_card();
        assert!(task_from_raw(&open, None, after).overdue);
        assert!(!task_from_raw(&open, None, before).overdue, "срок ещё не вышел");

        // Закрытая задача не бывает просроченной, каким бы ни был срок.
        let mut closed = live_card();
        closed.status = crate::api::schema::StrOrNum::Str("5".into());
        assert!(!task_from_raw(&closed, None, after).overdue);

        // Без срока просрочку считать нечем.
        let mut no_deadline = live_card();
        no_deadline.deadline = None;
        assert!(!task_from_raw(&no_deadline, None, after).overdue);

        // Неразобранная дата — тоже «нечем считать», а не «просрочено».
        let mut broken = live_card();
        broken.deadline = Some("завтра".into());
        assert!(!task_from_raw(&broken, None, after).overdue);
    }

    /// Р20: ВЕТКА «ЗАДАЧА БЕЗ ЧАТА». Портал отдаёт три формы отсутствия —
    /// ноль, `null` и пустую строку, — и все три обязаны означать одно.
    #[test]
    fn a_task_without_a_chat_has_no_dialog_id() {
        for form in [json!(0), json!(null), json!(""), json!("0")] {
            let mut card = live_card();
            card.chat_id = serde_json::from_value(form.clone()).unwrap_or_default();
            let dto = task_from_raw(&card, None, 0);
            assert_eq!(dto.chat_id, None, "форма {form}");
            assert_eq!(dto.dialog_id, None, "форма {form}");
            // Карточка при этом полноценна: заголовок, статус и кнопки на месте.
            assert!(!dto.title.is_empty());
            assert_eq!(dto.actions, vec!["complete", "defer", "start"]);
        }
    }

    /// `chatId` ПОЛИМОРФЕН: `tasks.task.get` даёт число, `tasks.task.list` —
    /// строку. Строгий `i64` ронял бы разбор ровно на списке.
    #[test]
    fn the_polymorphic_chat_id_parses_in_both_forms() {
        let from_get = task_from_raw(&raw(json!({"id": "1", "chatId": 7})), None, 0);
        let from_list = task_from_raw(&raw(json!({"id": "1", "chatId": "9"})), None, 0);
        assert_eq!(from_get.chat_id, Some(7));
        assert_eq!(from_list.chat_id, Some(9));
        assert_eq!(from_list.dialog_id.as_deref(), Some("chat9"));
    }

    /// Карточка НИКОГДА не несёт кода вебхука: ссылка собирается из ХОСТА.
    #[test]
    fn the_task_card_never_carries_the_webhook_code() {
        const CODE: &str = "wqdzbfhmn83d23j4";
        let webhook = crate::config::WebhookUrl::new(format!(
            "https://test-work-2.bitrix24.ru/rest/1/{CODE}/"
        ));
        let dto = task_from_raw(&live_card(), webhook.portal_origin().as_deref(), 0);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains(CODE), "код вебхука в карточке: {json}");
        assert!(json.contains("test-work-2.bitrix24.ru"));
    }

    /// Индекс отвечает в обе стороны и не теряет матрицу при обновлении из
    /// списка (где `action` не приходит вовсе).
    #[test]
    fn the_index_answers_both_directions_and_keeps_the_matrix() {
        let mut idx = TaskIndex::new();
        let matrix = ActionMatrix::from_raw(&live_card().action);
        idx.remember(1, Some(7), matrix, 100);

        assert_eq!(idx.chat_of(1), Some(7));
        assert_eq!(idx.task_of_chat(7), Some(1));
        assert!(idx.get(1).unwrap().actions.allows("start"));

        // Обновление из списка (пустая матрица) НЕ затирает права.
        idx.remember(1, Some(7), ActionMatrix::default(), 200);
        assert!(
            idx.get(1).unwrap().actions.allows("start"),
            "пустая матрица списка затёрла права карточки"
        );
        assert_eq!(idx.get(1).unwrap().updated_at, 200);

        // Задача без чата запоминается, но обратной связи не создаёт.
        idx.remember(5, None, ActionMatrix::default(), 300);
        assert_eq!(idx.chat_of(5), None);
        assert_eq!(idx.len(), 2);

        idx.clear();
        assert!(idx.is_empty());
        assert_eq!(idx.task_of_chat(7), None);
    }

    /// Индекс ограничен: длинный портал не должен утаскивать память процесса.
    #[test]
    fn the_index_evicts_the_oldest_entry() {
        let mut idx = TaskIndex::new();
        for i in 1..=(INDEX_CAPACITY as i64 + 10) {
            idx.remember(i, Some(1000 + i), ActionMatrix::default(), i);
        }
        assert_eq!(idx.len(), INDEX_CAPACITY);
        assert!(idx.get(1).is_none(), "самая старая запись выселена");
        assert!(idx.get(INDEX_CAPACITY as i64 + 10).is_some());
        assert_eq!(idx.task_of_chat(1001), None, "обратная связь тоже вычищена");
    }

    /// Отрицательный и нулевой идентификатор в индекс не попадают.
    #[test]
    fn the_index_ignores_impossible_task_ids() {
        let mut idx = TaskIndex::new();
        idx.remember(0, Some(7), ActionMatrix::default(), 1);
        idx.remember(-3, Some(8), ActionMatrix::default(), 1);
        assert!(idx.is_empty());
    }

    /// Ключи фильтра: запрещённый и разрешённый.
    #[test]
    fn the_filter_keys_are_frozen() {
        assert_eq!(FILTER_STATUS_KEY, "STATUS");
        assert_eq!(FILTER_REAL_STATUS_KEY, "REAL_STATUS");
        assert_eq!(ENTITY_TASK, "TASKS_TASK");
        assert_eq!(PAGE_SIZE, 50);
    }

    /// ЛОВУШКА 2 в самой таблице: ни одного camelCase-имени в `select`.
    #[test]
    fn the_list_select_is_uppercase_only() {
        for name in LIST_SELECT {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()),
                "camelCase в LIST_SELECT: {name} — портал молча вернёт null"
            );
        }
        assert!(LIST_SELECT.contains(&"CHAT_ID"), "без CHAT_ID лента задачи не откроется");
    }

    /// Путь задачи собирается даже при пустом ответственном.
    #[test]
    fn the_task_path_survives_a_missing_user() {
        assert_eq!(
            task_path("1", "7"),
            "/company/personal/user/1/tasks/task/view/7/"
        );
        assert_eq!(
            task_path("", "7"),
            "/company/personal/user/0/tasks/task/view/7/"
        );
    }
}
