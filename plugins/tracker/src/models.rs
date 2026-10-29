//! Модель данных личного трекера.
//!
//! # Что здесь лежит
//!
//! Пять сущностей и один envelope файла:
//!
//! * [`TaskList`] — пользовательский список («// LISTS» в левой колонке UI);
//! * [`TrackerTask`] — сама задача; её [`TaskStatus`] одновременно служит
//!   колонкой канбана;
//! * [`ChecklistItem`] — пункт чек-листа внутри задачи;
//! * [`Milestone`] — веха диаграммы Гантта, к задаче НЕ привязана;
//! * [`TimeEntry`] — отрезок времени; `item_id` позволяет мерить время по
//!   конкретному пункту чек-листа, и это время входит в общее время задачи;
//! * [`TrackerFile`] — то, что лежит в `~/.config/forge/tracker.json`.
//!
//! # Три решения, зафиксированные в типах
//!
//! **1. Поля `root_path` нет ни в одной структуре.** Задачи трекера
//! ГЛОБАЛЬНЫЕ для всего F.O.R.G.E. Это главное отличие от TODO-карточек
//! (`tmux-web/src/todos.rs`), которые группируются по корню проекта. Кнопка
//! «→ tasks/todo» (фаза 8) корень выбирает в момент переноса, а сама задача
//! после переноса не меняется — значит хранить корень в задаче незачем.
//!
//! **2. Коллекции плоские, вложенности нет.** Пункты чек-листа и отрезки
//! времени лежат в файле СВОИМИ списками и ссылаются на задачу через
//! `task_id`, а не хранятся массивами внутри [`TrackerTask`]. Причина
//! практическая: отрезок времени принадлежит одновременно задаче и
//! (опционально) пункту, а UI и отчёты читают отрезки поперёк всех задач —
//! за день, за неделю, по спискам. Вложенное хранение заставило бы обходить
//! все задачи ради одной суммы и породило бы вторую точку правды для
//! `item_id`.
//!
//! **3. Даты — строки RFC 3339 в UTC.** Как во всём остальном проекте:
//! `serde` их не трогает, а расчёты парсят по месту (см.
//! [`crate::store`]). Поля `start`/`due` задачи допускают и голую дату
//! (`YYYY-MM-DD`) — Гантт рисует по дням, время суток там смысла не имеет.
//!
//! # Совместимость
//!
//! `deny_unknown_fields` НЕ ставится нигде: файл, записанный более новой
//! версией трекера, обязан читаться старой без падения — незнакомые ключи
//! просто игнорируются. Все необязательные поля помечены `#[serde(default)]`,
//! поэтому и обратное направление (старый файл, новый код) работает.
//! Именование полей — `snake_case`, как в `todos.json`.

use serde::{Deserialize, Deserializer, Serialize};

/// Версия формата файла хранилища.
///
/// Пишется в [`TrackerFile::version`]. Пока формат один; поле существует,
/// чтобы будущая миграция могла отличить старый файл, не гадая по набору
/// ключей.
pub const STORE_VERSION: u32 = 1;

/// Приоритет по умолчанию (шкала 0..=4, как у `br` и TODO-карточек хоста:
/// 0 — critical, 4 — backlog).
pub const DEFAULT_PRIORITY: u8 = 2;

/// Максимально допустимый приоритет.
pub const MAX_PRIORITY: u8 = 4;

/// Список задач («проект» в терминах UI, но без всякой связи с каталогом на
/// диске).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    /// UUID v4.
    pub id: String,
    /// Отображаемое имя.
    pub name: String,
    /// Цвет метки списка (произвольная CSS-строка) или `None`.
    #[serde(default)]
    pub color: Option<String>,
    /// Порядок в левой колонке; меньше — выше.
    #[serde(default)]
    pub order: i32,
}

/// Статус задачи. Он же — колонка канбана (фаза 6).
///
/// Сериализуется СТАБИЛЬНЫМИ строками `inbox`/`today`/`doing`/`waiting`/
/// `done`: имена уходят в JSON стора и в REST, поэтому переименование любого
/// варианта — ломающее изменение формата, а не рефакторинг.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Не разобранное; сюда падает всё созданное без указания статуса.
    #[default]
    Inbox,
    /// Запланировано на сегодня.
    Today,
    /// В работе.
    Doing,
    /// Ждёт внешнего события.
    Waiting,
    /// Сделано; вместе со статусом проставляется `done_at`.
    Done,
}

impl TaskStatus {
    /// Все статусы в порядке колонок канбана — слева направо.
    pub const ALL: [TaskStatus; 5] = [
        TaskStatus::Inbox,
        TaskStatus::Today,
        TaskStatus::Doing,
        TaskStatus::Waiting,
        TaskStatus::Done,
    ];

    /// Стабильное строковое имя статуса (то же, что в JSON).
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Inbox => "inbox",
            TaskStatus::Today => "today",
            TaskStatus::Doing => "doing",
            TaskStatus::Waiting => "waiting",
            TaskStatus::Done => "done",
        }
    }

    /// Разбор статуса из строки REST-запроса. Регистр не важен.
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.trim().to_ascii_lowercase();
        TaskStatus::ALL.into_iter().find(|st| st.as_str() == lower)
    }
}

/// Задача личного трекера.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerTask {
    /// UUID v4.
    pub id: String,
    /// Список, которому принадлежит задача, или `None` («// inbox»).
    #[serde(default)]
    pub list_id: Option<String>,
    /// Заголовок; единственное обязательное поле при создании.
    pub title: String,
    /// Свободные заметки.
    #[serde(default)]
    pub notes: Option<String>,
    /// Статус = колонка канбана.
    #[serde(default)]
    pub status: TaskStatus,
    /// Приоритет 0..=4 (0 — самый высокий).
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Теги из prompt-строки (`#тег`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Оценка в минутах; сравнивается с фактом на Гантте.
    #[serde(default)]
    pub estimate_min: Option<u32>,
    /// Плановое начало для Гантта: `YYYY-MM-DD` или RFC 3339.
    #[serde(default)]
    pub start: Option<String>,
    /// Срок: `YYYY-MM-DD` или RFC 3339.
    #[serde(default)]
    pub due: Option<String>,
    /// Порядок внутри своей колонки/списка; меньше — выше.
    #[serde(default)]
    pub order: i32,
    /// RFC 3339 UTC.
    pub created_at: String,
    /// RFC 3339 UTC; обновляется при любом изменении задачи.
    pub updated_at: String,
    /// Момент перевода в [`TaskStatus::Done`]; при возврате в работу
    /// снимается в `None`.
    #[serde(default)]
    pub done_at: Option<String>,
}

/// Пункт чек-листа внутри задачи.
///
/// У пункта может быть собственный таймер: отрезок с
/// [`TimeEntry::item_id`] `= Some(item.id)`. Такое время входит в общее время
/// задачи ровно один раз — отдельной «суммы по пунктам» в модели нет
/// специально, чтобы двойного учёта не могло возникнуть по построению.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    /// UUID v4.
    pub id: String,
    /// Задача-владелец; при её удалении пункт удаляется каскадом.
    pub task_id: String,
    /// Текст пункта.
    pub title: String,
    /// Отмечен ли пункт.
    #[serde(default)]
    pub done: bool,
    /// Момент отметки (RFC 3339 UTC); снимается при снятии галочки.
    #[serde(default)]
    pub done_at: Option<String>,
    /// Порядок внутри задачи; меньше — выше.
    #[serde(default)]
    pub order: i32,
}

/// Веха диаграммы Гантта.
///
/// К задачам НЕ привязана и переживает удаление любой из них: веха — это
/// дата в календаре проекта («релиз», «демо»), а не свойство задачи. Рисуется
/// отдельной дорожкой `// MILESTONES` символом `◆` (фаза 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    /// UUID v4.
    pub id: String,
    /// Подпись вехи.
    pub title: String,
    /// Дата вехи: `YYYY-MM-DD` или RFC 3339.
    pub date: String,
    /// Список, к дорожке которого относится веха, или `None` (общая).
    #[serde(default)]
    pub list_id: Option<String>,
    /// Цвет ромба или `None`.
    #[serde(default)]
    pub color: Option<String>,
}

/// Отрезок времени.
///
/// Активный отрезок — тот, у которого `stopped_at == None`. Одновременно
/// активных отрезков может быть НЕСКОЛЬКО (по разным задачам), но по одной
/// задаче — максимум один; инвариант держит [`crate::store`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeEntry {
    /// UUID v4.
    pub id: String,
    /// Задача, которой засчитывается время.
    pub task_id: String,
    /// Пункт чек-листа, если время меряли по пункту.
    #[serde(default)]
    pub item_id: Option<String>,
    /// Начало (RFC 3339 UTC).
    pub started_at: String,
    /// Конец (RFC 3339 UTC) или `None` для идущего таймера.
    #[serde(default)]
    pub stopped_at: Option<String>,
    /// Комментарий к отрезку.
    #[serde(default)]
    pub note: Option<String>,
    /// Откуда отрезок взялся: `timer` (кнопка старт/стоп) или `manual`
    /// (заведён/поправлен руками задним числом).
    #[serde(default = "default_source")]
    pub source: String,
}

/// Источник отрезка, созданного таймером.
pub const SOURCE_TIMER: &str = "timer";

/// Источник отрезка, заведённого вручную.
pub const SOURCE_MANUAL: &str = "manual";

fn default_source() -> String {
    SOURCE_TIMER.to_string()
}

fn default_priority() -> u8 {
    DEFAULT_PRIORITY
}

/// Envelope файла `~/.config/forge/tracker.json`.
///
/// Все коллекции помечены `#[serde(default)]`: файл, где отсутствует любая из
/// них (в том числе пустой `{}`), читается как пустой стор, а не как ошибка.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackerFile {
    /// [`STORE_VERSION`] на момент записи.
    #[serde(default)]
    pub version: u32,
    /// Списки.
    #[serde(default)]
    pub lists: Vec<TaskList>,
    /// Задачи.
    #[serde(default)]
    pub tasks: Vec<TrackerTask>,
    /// Пункты чек-листов всех задач.
    #[serde(default)]
    pub items: Vec<ChecklistItem>,
    /// Вехи Гантта.
    #[serde(default)]
    pub milestones: Vec<Milestone>,
    /// Отрезки времени.
    #[serde(default)]
    pub entries: Vec<TimeEntry>,
}

/// Патч задачи для частичного обновления.
///
/// Три состояния каждого обнуляемого поля различаются намеренно, потому что
/// REST-слою фазы 2 нужны все три:
///
/// * ключа в JSON нет → `None` → поле не трогаем;
/// * ключ есть со значением → `Some(Some(v))` → ставим `v`;
/// * ключ есть и равен `null` → `Some(None)` → СБРАСЫВАЕМ поле.
///
/// Без этого «убрать срок у задачи» было бы невыразимо: обычный `Option`
/// склеивает «не передали» и «передали null».
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskPatch {
    /// Новый заголовок.
    #[serde(default)]
    pub title: Option<String>,
    /// Новый список (или `null` — вынуть из списка).
    #[serde(default, deserialize_with = "some_option")]
    pub list_id: Option<Option<String>>,
    /// Новые заметки (или `null` — стереть).
    #[serde(default, deserialize_with = "some_option")]
    pub notes: Option<Option<String>>,
    /// Новый статус.
    #[serde(default)]
    pub status: Option<TaskStatus>,
    /// Новый приоритет; значения больше [`MAX_PRIORITY`] обрезаются.
    #[serde(default)]
    pub priority: Option<u8>,
    /// Новый набор тегов целиком.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Новая оценка (или `null` — снять).
    #[serde(default, deserialize_with = "some_option")]
    pub estimate_min: Option<Option<u32>>,
    /// Новое плановое начало (или `null` — снять).
    #[serde(default, deserialize_with = "some_option")]
    pub start: Option<Option<String>>,
    /// Новый срок (или `null` — снять).
    #[serde(default, deserialize_with = "some_option")]
    pub due: Option<Option<String>>,
    /// Новый порядок.
    #[serde(default)]
    pub order: Option<i32>,
}

/// Патч списка.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListPatch {
    /// Новое имя.
    #[serde(default)]
    pub name: Option<String>,
    /// Новый цвет (или `null` — снять).
    #[serde(default, deserialize_with = "some_option")]
    pub color: Option<Option<String>>,
    /// Новый порядок.
    #[serde(default)]
    pub order: Option<i32>,
}

/// Патч пункта чек-листа.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemPatch {
    /// Новый текст.
    #[serde(default)]
    pub title: Option<String>,
    /// Новое состояние галочки; смена значения двигает `done_at`.
    #[serde(default)]
    pub done: Option<bool>,
    /// Новый порядок.
    #[serde(default)]
    pub order: Option<i32>,
}

/// Патч вехи.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MilestonePatch {
    /// Новая подпись.
    #[serde(default)]
    pub title: Option<String>,
    /// Новая дата.
    #[serde(default)]
    pub date: Option<String>,
    /// Новый список (или `null` — общая веха).
    #[serde(default, deserialize_with = "some_option")]
    pub list_id: Option<Option<String>>,
    /// Новый цвет (или `null` — снять).
    #[serde(default, deserialize_with = "some_option")]
    pub color: Option<Option<String>>,
}

/// Патч отрезка времени (ручная правка задним числом).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EntryPatch {
    /// Новое начало (RFC 3339).
    #[serde(default)]
    pub started_at: Option<String>,
    /// Новый конец (или `null` — снова сделать отрезок активным).
    #[serde(default, deserialize_with = "some_option")]
    pub stopped_at: Option<Option<String>>,
    /// Новый комментарий (или `null` — стереть).
    #[serde(default, deserialize_with = "some_option")]
    pub note: Option<Option<String>>,
    /// Перепривязка к пункту чек-листа (или `null` — отвязать).
    #[serde(default, deserialize_with = "some_option")]
    pub item_id: Option<Option<String>>,
}

/// Десериализатор `Option<Option<T>>`, различающий «ключа нет» и «ключ = null».
///
/// Вызывается serde ТОЛЬКО когда ключ присутствует, поэтому результат всегда
/// `Some(...)`; отсутствие ключа даёт `None` через `#[serde(default)]`.
fn some_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings_are_stable() {
        // Имена уходят в JSON и в REST: изменение любого — ломающее.
        let json = serde_json::to_string(&TaskStatus::ALL).unwrap();
        assert_eq!(json, r#"["inbox","today","doing","waiting","done"]"#);
        for st in TaskStatus::ALL {
            assert_eq!(TaskStatus::parse(st.as_str()), Some(st));
        }
        assert_eq!(TaskStatus::parse("DOING"), Some(TaskStatus::Doing));
        assert_eq!(TaskStatus::parse("nope"), None);
    }

    #[test]
    fn task_round_trip_is_lossless() {
        let task = TrackerTask {
            id: "t1".into(),
            list_id: Some("l1".into()),
            title: "написать модель".into(),
            notes: Some("с заметкой".into()),
            status: TaskStatus::Doing,
            priority: 1,
            tags: vec!["forge".into(), "tracker".into()],
            estimate_min: Some(60),
            start: Some("2026-09-08".into()),
            due: Some("2026-09-10".into()),
            order: 3,
            created_at: "2026-09-08T10:00:00Z".into(),
            updated_at: "2026-09-08T11:00:00Z".into(),
            done_at: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        let back: TrackerTask = serde_json::from_str(&json).unwrap();
        assert_eq!(task, back);
    }

    #[test]
    fn entry_and_milestone_round_trip() {
        let entry = TimeEntry {
            id: "e1".into(),
            task_id: "t1".into(),
            item_id: Some("i1".into()),
            started_at: "2026-09-08T10:00:00Z".into(),
            stopped_at: None,
            note: None,
            source: SOURCE_TIMER.into(),
        };
        let back: TimeEntry = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(entry, back);

        let ms = Milestone {
            id: "m1".into(),
            title: "релиз".into(),
            date: "2026-10-01".into(),
            list_id: None,
            color: Some("#C98BFF".into()),
        };
        let back: Milestone = serde_json::from_str(&serde_json::to_string(&ms).unwrap()).unwrap();
        assert_eq!(ms, back);
    }

    #[test]
    fn unknown_fields_do_not_break_deserialization() {
        // Файл от более новой версии трекера обязан читаться старым кодом.
        let json = r#"{
            "id": "t1",
            "title": "задача",
            "created_at": "2026-09-08T10:00:00Z",
            "updated_at": "2026-09-08T10:00:00Z",
            "future_field": {"nested": [1, 2, 3]}
        }"#;
        let task: TrackerTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.status, TaskStatus::Inbox);
        assert_eq!(task.priority, DEFAULT_PRIORITY);
        assert!(task.tags.is_empty());

        let file: TrackerFile = serde_json::from_str(r#"{"unknown": 1}"#).unwrap();
        assert!(file.tasks.is_empty());
        assert!(file.entries.is_empty());
    }

    #[test]
    fn patch_distinguishes_absent_from_null() {
        let absent: TaskPatch = serde_json::from_str("{}").unwrap();
        assert!(absent.due.is_none());

        let cleared: TaskPatch = serde_json::from_str(r#"{"due": null}"#).unwrap();
        assert_eq!(cleared.due, Some(None));

        let set: TaskPatch = serde_json::from_str(r#"{"due": "2026-09-09"}"#).unwrap();
        assert_eq!(set.due, Some(Some("2026-09-09".to_string())));
    }

    #[test]
    fn no_model_carries_root_path() {
        // Задачи трекера глобальные: поля root_path не должно быть даже в
        // сериализованном виде, иначе фронт начнёт на него опираться.
        let task = TrackerTask {
            id: "t1".into(),
            list_id: None,
            title: "x".into(),
            notes: None,
            status: TaskStatus::Inbox,
            priority: DEFAULT_PRIORITY,
            tags: vec![],
            estimate_min: None,
            start: None,
            due: None,
            order: 0,
            created_at: "2026-09-08T10:00:00Z".into(),
            updated_at: "2026-09-08T10:00:00Z".into(),
            done_at: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("root_path"), "{json}");
        assert!(!json.contains("project_id"), "{json}");
    }
}
