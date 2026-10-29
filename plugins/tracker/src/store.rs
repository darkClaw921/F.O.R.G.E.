//! Хранилище личного трекера: плоский JSON-файл под `RwLock`.
//!
//! # Чем отличается от `tmux-web/src/todos.rs`
//!
//! Внешне это тот же приём — `Arc<RwLock<Inner>>` поверх JSON-файла с
//! атомарной записью, — но с одним принципиальным отличием: **группировки по
//! корню проекта здесь нет**. У TODO-карточек хоста ключ хранилища —
//! `root_path`, потому что карточка живёт внутри проекта. Задача трекера
//! принадлежит человеку, а не каталогу: один плоский набор данных на машину,
//! файл `~/.config/forge/tracker.json`. Отсюда и `Inner` без `HashMap` по
//! путям, и отсутствие поля `root_path` в моделях.
//!
//! # Формат файла
//!
//! Envelope [`TrackerFile`]: `{ version, lists, tasks, items, milestones,
//! entries }`. Пункты чек-листа и вехи лежат СВОИМИ коллекциями, а не внутри
//! задачи (обоснование — в шапке [`crate::models`]).
//!
//! # Живучесть
//!
//! * файла нет → пустой стор, первый же мутирующий вызов его создаст;
//! * файл битый → `warn` в журнал и пустой стор; сервер обязан подняться,
//!   потому что трекер — не критичная подсистема, а личные заметки;
//! * запись — только через `forge_atomic::write_bytes_atomic`
//!   (`tmp` → `fsync` → `rename` → `0600`): частичного состояния на диске не
//!   бывает, а права на файле с личными задачами — часть контракта.
//!
//! Сохранение происходит ВНУТРИ write-lock'а, поэтому на диске всегда лежит
//! согласованный снимок (тот же приём, что в `todos.rs`).
//!
//! # Таймеры
//!
//! Правила зафиксированы с пользователем и держатся здесь, а не в UI:
//!
//! * несколько таймеров по РАЗНЫМ задачам могут идти одновременно — активных
//!   [`TimeEntry`] (`stopped_at == None`) в файле может быть много;
//! * по ОДНОЙ задаче активен максимум один отрезок: повторный
//!   [`TrackerStore::start_timer`] не создаёт второй, а возвращает текущий;
//! * [`TrackerStore::stop_timer`] идемпотентен: остановка уже остановленного
//!   — не ошибка, а `Ok(None)`;
//! * время пункта чек-листа — это обычный отрезок с `item_id`, и в сумму
//!   задачи оно входит ровно один раз.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;

use crate::config::TrackerConfig;
use crate::error::{Result, TrackerError};
use crate::models::{
    ChecklistItem, EntryPatch, ItemPatch, ListPatch, Milestone, MilestonePatch, TaskList,
    TaskPatch, TaskStatus, TimeEntry, TrackerFile, TrackerTask, MAX_PRIORITY, SOURCE_MANUAL,
    SOURCE_TIMER, STORE_VERSION,
};
use crate::report::{
    self, GanttData, Report, ReportGroupBy, Totals,
};

/// Данные для создания задачи.
///
/// Отдельная структура, а не десяток аргументов: набор полей будет расти
/// (фаза 4 добавит разбор prompt-строки `!1 #тег ~2ч @список due:`), и
/// позиционные аргументы там кончились бы перепутанными строками.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    /// Заголовок (обязателен).
    pub title: String,
    /// Список или `None`.
    pub list_id: Option<String>,
    /// Заметки.
    pub notes: Option<String>,
    /// Статус; `None` → [`TaskStatus::Inbox`].
    pub status: Option<TaskStatus>,
    /// Приоритет; `None` → `2`.
    pub priority: Option<u8>,
    /// Теги.
    pub tags: Vec<String>,
    /// Оценка в минутах.
    pub estimate_min: Option<u32>,
    /// Плановое начало.
    pub start: Option<String>,
    /// Срок.
    pub due: Option<String>,
}

impl NewTask {
    /// Задача с одним лишь заголовком — самый частый путь создания.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }
}

/// Прогресс чек-листа задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecklistProgress {
    /// Сколько пунктов отмечено.
    pub done: usize,
    /// Сколько всего пунктов.
    pub total: usize,
}

impl ChecklistProgress {
    /// Доля выполненного в процентах (0 при пустом чек-листе).
    pub fn percent(self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        ((self.done * 100) / self.total) as u8
    }
}

/// Состояние под замком.
#[derive(Debug, Default)]
struct Inner {
    file_path: PathBuf,
    lists: Vec<TaskList>,
    tasks: Vec<TrackerTask>,
    items: Vec<ChecklistItem>,
    milestones: Vec<Milestone>,
    entries: Vec<TimeEntry>,
}

impl Inner {
    fn snapshot(&self) -> TrackerFile {
        TrackerFile {
            version: STORE_VERSION,
            lists: self.lists.clone(),
            tasks: self.tasks.clone(),
            items: self.items.clone(),
            milestones: self.milestones.clone(),
            entries: self.entries.clone(),
        }
    }

    /// Сериализует и атомарно пишет снимок. Вызывается ВНУТРИ write-lock'а.
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.snapshot())?;
        forge_atomic::write_bytes_atomic(&self.file_path, &bytes)
            .map_err(|e| TrackerError::Io(e.to_string()))
    }

    fn task_mut(&mut self, id: &str) -> Result<&mut TrackerTask> {
        self.tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| TrackerError::NotFound(format!("task {id}")))
    }
}

/// Хранилище трекера: потокобезопасное и дешёвое в клонировании.
#[derive(Debug, Clone)]
pub struct TrackerStore {
    inner: Arc<RwLock<Inner>>,
}

impl TrackerStore {
    /// Открывает стор по конфигу.
    pub fn open(config: &TrackerConfig) -> Result<Self> {
        Self::load(config.store_path())
    }

    /// Открывает стор по явному пути к файлу.
    ///
    /// Отсутствующий файл — это пустой стор, а не ошибка. Битый файл — тоже:
    /// он логируется через `warn` и НЕ роняет сервер (перезапись случится при
    /// первом изменении, поэтому исходные байты остаются на диске до тех пор,
    /// пока пользователь их не потеряет осознанно).
    pub fn load(file_path: impl AsRef<Path>) -> Result<Self> {
        let file_path = file_path.as_ref().to_path_buf();
        let mut inner = Inner {
            file_path,
            ..Inner::default()
        };

        if inner.file_path.exists() {
            match std::fs::read(&inner.file_path) {
                Ok(raw) if !raw.is_empty() => match serde_json::from_slice::<TrackerFile>(&raw) {
                    Ok(parsed) => {
                        inner.lists = parsed.lists;
                        inner.tasks = parsed.tasks;
                        inner.items = parsed.items;
                        inner.milestones = parsed.milestones;
                        inner.entries = parsed.entries;
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %inner.file_path.display(),
                            error = %e,
                            "tracker.json is not valid JSON, starting with an empty store"
                        );
                    }
                },
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        path = %inner.file_path.display(),
                        error = %e,
                        "failed to read tracker.json, starting with an empty store"
                    );
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
        })
    }

    /// Путь к файлу хранилища.
    pub fn file_path(&self) -> PathBuf {
        self.read().file_path.clone()
    }

    /// Полный снимок для `GET /api/tracker/state` (фаза 2).
    pub fn snapshot(&self) -> TrackerFile {
        self.read().snapshot()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().expect("TrackerStore lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().expect("TrackerStore lock poisoned")
    }

    // ---------------------------------------------------------------- lists

    /// Все списки, отсортированные по `order`, затем по имени.
    pub fn lists(&self) -> Vec<TaskList> {
        let mut out = self.read().lists.clone();
        out.sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
        out
    }

    /// Создаёт список. `order` по умолчанию — в конец.
    pub fn create_list(&self, name: &str, color: Option<String>) -> Result<TaskList> {
        let mut inner = self.write();
        let order = next_order(inner.lists.iter().map(|l| l.order));
        let list = TaskList {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            color,
            order,
        };
        inner.lists.push(list.clone());
        inner.save()?;
        Ok(list)
    }

    /// Частичное обновление списка.
    pub fn update_list(&self, id: &str, patch: ListPatch) -> Result<TaskList> {
        let mut inner = self.write();
        let list = inner
            .lists
            .iter_mut()
            .find(|l| l.id == id)
            .ok_or_else(|| TrackerError::NotFound(format!("list {id}")))?;
        if let Some(name) = patch.name {
            list.name = name;
        }
        if let Some(color) = patch.color {
            list.color = color;
        }
        if let Some(order) = patch.order {
            list.order = order;
        }
        let out = list.clone();
        inner.save()?;
        Ok(out)
    }

    /// Удаляет список.
    ///
    /// Задачи списка НЕ удаляются — они переезжают в «без списка»
    /// (`list_id = None`). Удаление контейнера не должно уничтожать чужой
    /// труд; то же правило применено к вехам, привязанным к списку.
    pub fn delete_list(&self, id: &str) -> Result<()> {
        let mut inner = self.write();
        let before = inner.lists.len();
        inner.lists.retain(|l| l.id != id);
        if inner.lists.len() == before {
            return Err(TrackerError::NotFound(format!("list {id}")));
        }
        let now = now_rfc3339();
        for task in inner.tasks.iter_mut() {
            if task.list_id.as_deref() == Some(id) {
                task.list_id = None;
                task.updated_at = now.clone();
            }
        }
        for ms in inner.milestones.iter_mut() {
            if ms.list_id.as_deref() == Some(id) {
                ms.list_id = None;
            }
        }
        inner.save()
    }

    /// Переставляет списки: `order` становится равен позиции в `ids`.
    /// Не упомянутые списки уезжают в конец, сохраняя относительный порядок.
    pub fn reorder_lists(&self, ids: &[String]) -> Result<()> {
        let mut inner = self.write();
        let tail = ids.len() as i32;
        for list in inner.lists.iter_mut() {
            list.order = match ids.iter().position(|i| i == &list.id) {
                Some(pos) => pos as i32,
                None => tail + list.order,
            };
        }
        inner.save()
    }

    // ---------------------------------------------------------------- tasks

    /// Все задачи, отсортированные по `order`, затем по времени создания.
    pub fn tasks(&self) -> Vec<TrackerTask> {
        let mut out = self.read().tasks.clone();
        out.sort_by(|a, b| a.order.cmp(&b.order).then(a.created_at.cmp(&b.created_at)));
        out
    }

    /// Задача по `id`.
    pub fn task(&self, id: &str) -> Option<TrackerTask> {
        self.read().tasks.iter().find(|t| t.id == id).cloned()
    }

    /// Создаёт задачу.
    pub fn create_task(&self, new: NewTask) -> Result<TrackerTask> {
        let mut inner = self.write();
        let now = now_rfc3339();
        let status = new.status.unwrap_or_default();
        let task = TrackerTask {
            id: Uuid::new_v4().to_string(),
            list_id: new.list_id,
            title: new.title,
            notes: new.notes,
            status,
            priority: clamp_priority(new.priority.unwrap_or(crate::models::DEFAULT_PRIORITY)),
            tags: new.tags,
            estimate_min: new.estimate_min,
            start: new.start,
            due: new.due,
            order: next_order(inner.tasks.iter().map(|t| t.order)),
            created_at: now.clone(),
            updated_at: now.clone(),
            done_at: if status == TaskStatus::Done {
                Some(now)
            } else {
                None
            },
        };
        inner.tasks.push(task.clone());
        inner.save()?;
        Ok(task)
    }

    /// Частичное обновление задачи.
    ///
    /// Перевод в [`TaskStatus::Done`] проставляет `done_at`, возврат в работу
    /// — снимает: иначе отчёт «сделано за неделю» считал бы вернувшиеся в
    /// работу задачи.
    pub fn update_task(&self, id: &str, patch: TaskPatch) -> Result<TrackerTask> {
        let mut inner = self.write();
        let now = now_rfc3339();
        let task = inner.task_mut(id)?;

        if let Some(title) = patch.title {
            task.title = title;
        }
        if let Some(list_id) = patch.list_id {
            task.list_id = list_id;
        }
        if let Some(notes) = patch.notes {
            task.notes = notes;
        }
        if let Some(status) = patch.status {
            if status != task.status {
                task.done_at = if status == TaskStatus::Done {
                    Some(now.clone())
                } else {
                    None
                };
            }
            task.status = status;
        }
        if let Some(priority) = patch.priority {
            task.priority = clamp_priority(priority);
        }
        if let Some(tags) = patch.tags {
            task.tags = tags;
        }
        if let Some(estimate) = patch.estimate_min {
            task.estimate_min = estimate;
        }
        if let Some(start) = patch.start {
            task.start = start;
        }
        if let Some(due) = patch.due {
            task.due = due;
        }
        if let Some(order) = patch.order {
            task.order = order;
        }
        task.updated_at = now;
        let out = task.clone();
        inner.save()?;
        Ok(out)
    }

    /// Смена статуса (перенос между колонками канбана).
    pub fn set_status(&self, id: &str, status: TaskStatus) -> Result<TrackerTask> {
        self.update_task(
            id,
            TaskPatch {
                status: Some(status),
                ..TaskPatch::default()
            },
        )
    }

    /// Удаляет задачу КАСКАДНО: вместе с ней уходят её пункты чек-листа и все
    /// отрезки времени. Осиротевший отрезок был бы временем «ни на что» и
    /// портил бы любую сумму в отчёте.
    ///
    /// Вехи не трогаются: они к задачам не привязаны.
    pub fn delete_task(&self, id: &str) -> Result<()> {
        let mut inner = self.write();
        let before = inner.tasks.len();
        inner.tasks.retain(|t| t.id != id);
        if inner.tasks.len() == before {
            return Err(TrackerError::NotFound(format!("task {id}")));
        }
        inner.items.retain(|i| i.task_id != id);
        inner.entries.retain(|e| e.task_id != id);
        inner.save()
    }

    /// Переставляет задачи: `order` становится равен позиции в `ids`.
    pub fn reorder_tasks(&self, ids: &[String]) -> Result<()> {
        let mut inner = self.write();
        let now = now_rfc3339();
        let tail = ids.len() as i32;
        for task in inner.tasks.iter_mut() {
            let order = match ids.iter().position(|i| i == &task.id) {
                Some(pos) => pos as i32,
                None => tail + task.order,
            };
            if order != task.order {
                task.order = order;
                task.updated_at = now.clone();
            }
        }
        inner.save()
    }

    // ------------------------------------------------------------ checklist

    /// Пункты чек-листа задачи, упорядоченные по `order`.
    pub fn items(&self, task_id: &str) -> Vec<ChecklistItem> {
        let inner = self.read();
        let mut out: Vec<ChecklistItem> = inner
            .items
            .iter()
            .filter(|i| i.task_id == task_id)
            .cloned()
            .collect();
        out.sort_by_key(|i| i.order);
        out
    }

    /// Прогресс чек-листа: сколько отмечено из скольких.
    pub fn checklist_progress(&self, task_id: &str) -> ChecklistProgress {
        let inner = self.read();
        let mut total = 0;
        let mut done = 0;
        for item in inner.items.iter().filter(|i| i.task_id == task_id) {
            total += 1;
            if item.done {
                done += 1;
            }
        }
        ChecklistProgress { done, total }
    }

    /// Добавляет пункт в конец чек-листа задачи.
    pub fn add_item(&self, task_id: &str, title: &str) -> Result<ChecklistItem> {
        let mut inner = self.write();
        if !inner.tasks.iter().any(|t| t.id == task_id) {
            return Err(TrackerError::NotFound(format!("task {task_id}")));
        }
        let order = next_order(
            inner
                .items
                .iter()
                .filter(|i| i.task_id == task_id)
                .map(|i| i.order),
        );
        let item = ChecklistItem {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            title: title.to_string(),
            done: false,
            done_at: None,
            order,
        };
        inner.items.push(item.clone());
        inner.save()?;
        Ok(item)
    }

    /// Частичное обновление пункта: текст, галочка, порядок.
    ///
    /// Смена `done` двигает `done_at` в обе стороны: поставили — записали
    /// момент, сняли — стёрли.
    pub fn update_item(&self, id: &str, patch: ItemPatch) -> Result<ChecklistItem> {
        let mut inner = self.write();
        let now = now_rfc3339();
        let item = inner
            .items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| TrackerError::NotFound(format!("item {id}")))?;
        if let Some(title) = patch.title {
            item.title = title;
        }
        if let Some(done) = patch.done {
            if done != item.done {
                item.done_at = if done { Some(now) } else { None };
            }
            item.done = done;
        }
        if let Some(order) = patch.order {
            item.order = order;
        }
        let out = item.clone();
        inner.save()?;
        Ok(out)
    }

    /// Переключает галочку пункта.
    pub fn toggle_item(&self, id: &str) -> Result<ChecklistItem> {
        let current = self
            .read()
            .items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.done)
            .ok_or_else(|| TrackerError::NotFound(format!("item {id}")))?;
        self.update_item(
            id,
            ItemPatch {
                done: Some(!current),
                ..ItemPatch::default()
            },
        )
    }

    /// Удаляет пункт.
    ///
    /// Отрезки времени, снятые по пункту, остаются у задачи и лишь теряют
    /// `item_id`: время человек потратил, и оно обязано остаться в сумме
    /// задачи, даже если сам пункт удалён.
    pub fn delete_item(&self, id: &str) -> Result<()> {
        let mut inner = self.write();
        let before = inner.items.len();
        inner.items.retain(|i| i.id != id);
        if inner.items.len() == before {
            return Err(TrackerError::NotFound(format!("item {id}")));
        }
        for entry in inner.entries.iter_mut() {
            if entry.item_id.as_deref() == Some(id) {
                entry.item_id = None;
            }
        }
        inner.save()
    }

    /// Переставляет пункты внутри задачи.
    pub fn reorder_items(&self, task_id: &str, ids: &[String]) -> Result<()> {
        let mut inner = self.write();
        let tail = ids.len() as i32;
        for item in inner.items.iter_mut().filter(|i| i.task_id == task_id) {
            item.order = match ids.iter().position(|i| i == &item.id) {
                Some(pos) => pos as i32,
                None => tail + item.order,
            };
        }
        inner.save()
    }

    // ----------------------------------------------------------- milestones

    /// Все вехи, отсортированные по дате.
    pub fn milestones(&self) -> Vec<Milestone> {
        let mut out = self.read().milestones.clone();
        out.sort_by(|a, b| a.date.cmp(&b.date).then(a.title.cmp(&b.title)));
        out
    }

    /// Создаёт веху.
    pub fn create_milestone(
        &self,
        title: &str,
        date: &str,
        list_id: Option<String>,
        color: Option<String>,
    ) -> Result<Milestone> {
        let mut inner = self.write();
        let ms = Milestone {
            id: Uuid::new_v4().to_string(),
            title: title.to_string(),
            date: date.to_string(),
            list_id,
            color,
        };
        inner.milestones.push(ms.clone());
        inner.save()?;
        Ok(ms)
    }

    /// Частичное обновление вехи.
    pub fn update_milestone(&self, id: &str, patch: MilestonePatch) -> Result<Milestone> {
        let mut inner = self.write();
        let ms = inner
            .milestones
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| TrackerError::NotFound(format!("milestone {id}")))?;
        if let Some(title) = patch.title {
            ms.title = title;
        }
        if let Some(date) = patch.date {
            ms.date = date;
        }
        if let Some(list_id) = patch.list_id {
            ms.list_id = list_id;
        }
        if let Some(color) = patch.color {
            ms.color = color;
        }
        let out = ms.clone();
        inner.save()?;
        Ok(out)
    }

    /// Удаляет веху.
    pub fn delete_milestone(&self, id: &str) -> Result<()> {
        let mut inner = self.write();
        let before = inner.milestones.len();
        inner.milestones.retain(|m| m.id != id);
        if inner.milestones.len() == before {
            return Err(TrackerError::NotFound(format!("milestone {id}")));
        }
        inner.save()
    }

    // --------------------------------------------------------------- timers

    /// Все отрезки задачи, от новых к старым.
    pub fn entries(&self, task_id: &str) -> Vec<TimeEntry> {
        let inner = self.read();
        let mut out: Vec<TimeEntry> = inner
            .entries
            .iter()
            .filter(|e| e.task_id == task_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        out
    }

    /// Все отрезки стора (для отчётов и восстановления UI после перезагрузки).
    pub fn all_entries(&self) -> Vec<TimeEntry> {
        self.read().entries.clone()
    }

    /// Идущие сейчас отрезки — блок `// RUNNING` в UI.
    ///
    /// Их может быть несколько: параллельные таймеры по разным задачам —
    /// штатный сценарий, а не ошибка.
    pub fn active_entries(&self) -> Vec<TimeEntry> {
        self.read()
            .entries
            .iter()
            .filter(|e| e.stopped_at.is_none())
            .cloned()
            .collect()
    }

    /// Активный отрезок конкретной задачи, если он есть.
    pub fn active_entry_for_task(&self, task_id: &str) -> Option<TimeEntry> {
        self.read()
            .entries
            .iter()
            .find(|e| e.task_id == task_id && e.stopped_at.is_none())
            .cloned()
    }

    /// Запускает таймер по задаче (или по пункту её чек-листа).
    ///
    /// Если по этой задаче отрезок уже идёт — НОВЫЙ НЕ СОЗДАЁТСЯ, возвращается
    /// текущий. Это и есть инвариант «по одной задаче максимум один активный
    /// отрезок»: двойное нажатие кнопки в UI или повторный POST не должны
    /// раздваивать учёт. Параллельность при этом сохраняется — ограничение
    /// действует внутри задачи, а не глобально.
    pub fn start_timer(
        &self,
        task_id: &str,
        item_id: Option<&str>,
        note: Option<String>,
    ) -> Result<TimeEntry> {
        let mut inner = self.write();
        if !inner.tasks.iter().any(|t| t.id == task_id) {
            return Err(TrackerError::NotFound(format!("task {task_id}")));
        }
        if let Some(item) = item_id {
            let belongs = inner
                .items
                .iter()
                .any(|i| i.id == item && i.task_id == task_id);
            if !belongs {
                return Err(TrackerError::NotFound(format!(
                    "checklist item {item} of task {task_id}"
                )));
            }
        }

        if let Some(existing) = inner
            .entries
            .iter()
            .find(|e| e.task_id == task_id && e.stopped_at.is_none())
        {
            return Ok(existing.clone());
        }

        let entry = TimeEntry {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            item_id: item_id.map(|s| s.to_string()),
            started_at: now_rfc3339(),
            stopped_at: None,
            note,
            source: SOURCE_TIMER.to_string(),
        };
        inner.entries.push(entry.clone());
        inner.save()?;
        Ok(entry)
    }

    /// Останавливает таймер задачи.
    ///
    /// Идемпотентно: если активного отрезка нет, возвращается `Ok(None)` —
    /// повторный стоп это не ошибка приложения, а состояние «уже стоит».
    /// Активные отрезки ДРУГИХ задач не затрагиваются.
    pub fn stop_timer(&self, task_id: &str) -> Result<Option<TimeEntry>> {
        let mut inner = self.write();
        if !inner.tasks.iter().any(|t| t.id == task_id) {
            return Err(TrackerError::NotFound(format!("task {task_id}")));
        }
        let now = now_rfc3339();
        let Some(entry) = inner
            .entries
            .iter_mut()
            .find(|e| e.task_id == task_id && e.stopped_at.is_none())
        else {
            return Ok(None);
        };
        entry.stopped_at = Some(now);
        let out = entry.clone();
        inner.save()?;
        Ok(Some(out))
    }

    /// Останавливает конкретный отрезок по его `id`. Уже остановленный
    /// возвращается как есть.
    pub fn stop_entry(&self, entry_id: &str) -> Result<TimeEntry> {
        let mut inner = self.write();
        let now = now_rfc3339();
        let entry = inner
            .entries
            .iter_mut()
            .find(|e| e.id == entry_id)
            .ok_or_else(|| TrackerError::NotFound(format!("entry {entry_id}")))?;
        if entry.stopped_at.is_some() {
            return Ok(entry.clone());
        }
        entry.stopped_at = Some(now);
        let out = entry.clone();
        inner.save()?;
        Ok(out)
    }

    /// Заводит отрезок с явными временами (правка задним числом).
    ///
    /// `stopped_at = None` допустим — так восстанавливают потерянный активный
    /// таймер; при этом действует тот же инвариант одного активного отрезка на
    /// задачу.
    pub fn create_entry(
        &self,
        task_id: &str,
        item_id: Option<&str>,
        started_at: &str,
        stopped_at: Option<&str>,
        note: Option<String>,
    ) -> Result<TimeEntry> {
        let start = report::parse_ts(started_at)
            .ok_or_else(|| TrackerError::Conflict(format!("bad started_at: {started_at}")))?;
        let stop = match stopped_at {
            Some(s) => Some(
                report::parse_ts(s)
                    .ok_or_else(|| TrackerError::Conflict(format!("bad stopped_at: {s}")))?,
            ),
            None => None,
        };
        if let Some(stop) = stop {
            if stop <= start {
                return Err(TrackerError::Conflict(
                    "stopped_at must be later than started_at".to_string(),
                ));
            }
        }

        let mut inner = self.write();
        if !inner.tasks.iter().any(|t| t.id == task_id) {
            return Err(TrackerError::NotFound(format!("task {task_id}")));
        }
        if let Some(item) = item_id {
            if !inner
                .items
                .iter()
                .any(|i| i.id == item && i.task_id == task_id)
            {
                return Err(TrackerError::NotFound(format!(
                    "checklist item {item} of task {task_id}"
                )));
            }
        }
        if stop.is_none()
            && inner
                .entries
                .iter()
                .any(|e| e.task_id == task_id && e.stopped_at.is_none())
        {
            return Err(TrackerError::Conflict(format!(
                "task {task_id} already has a running time entry"
            )));
        }

        let entry = TimeEntry {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            item_id: item_id.map(|s| s.to_string()),
            started_at: to_rfc3339(start),
            stopped_at: stop.map(to_rfc3339),
            note,
            source: SOURCE_MANUAL.to_string(),
        };
        inner.entries.push(entry.clone());
        inner.save()?;
        Ok(entry)
    }

    /// Правка существующего отрезка.
    ///
    /// Проверка `stopped_at > started_at` выполняется по ИТОГОВОМУ состоянию,
    /// а не по патчу: иначе сдвиг одной границы мог бы вывернуть отрезок
    /// наизнанку и дать отрицательную длительность в отчёте.
    pub fn update_entry(&self, id: &str, patch: EntryPatch) -> Result<TimeEntry> {
        let mut inner = self.write();
        let (task_id, current) = {
            let e = inner
                .entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| TrackerError::NotFound(format!("entry {id}")))?;
            (e.task_id.clone(), e.clone())
        };

        let mut next = current.clone();
        if let Some(started) = patch.started_at {
            next.started_at = started;
        }
        if let Some(stopped) = patch.stopped_at {
            next.stopped_at = stopped;
        }
        if let Some(note) = patch.note {
            next.note = note;
        }
        if let Some(item_id) = patch.item_id {
            if let Some(item) = item_id.as_deref() {
                if !inner
                    .items
                    .iter()
                    .any(|i| i.id == item && i.task_id == task_id)
                {
                    return Err(TrackerError::NotFound(format!(
                        "checklist item {item} of task {task_id}"
                    )));
                }
            }
            next.item_id = item_id;
        }

        let start = report::parse_ts(&next.started_at).ok_or_else(|| {
            TrackerError::Conflict(format!("bad started_at: {}", next.started_at))
        })?;
        match next.stopped_at.as_deref() {
            Some(s) => {
                let stop = report::parse_ts(s)
                    .ok_or_else(|| TrackerError::Conflict(format!("bad stopped_at: {s}")))?;
                if stop <= start {
                    return Err(TrackerError::Conflict(
                        "stopped_at must be later than started_at".to_string(),
                    ));
                }
            }
            None => {
                if current.stopped_at.is_some()
                    && inner
                        .entries
                        .iter()
                        .any(|e| e.id != id && e.task_id == task_id && e.stopped_at.is_none())
                {
                    return Err(TrackerError::Conflict(format!(
                        "task {task_id} already has a running time entry"
                    )));
                }
            }
        }

        let slot = inner
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .expect("entry existence checked above");
        *slot = next.clone();
        inner.save()?;
        Ok(next)
    }

    /// Удаляет отрезок.
    pub fn delete_entry(&self, id: &str) -> Result<()> {
        let mut inner = self.write();
        let before = inner.entries.len();
        inner.entries.retain(|e| e.id != id);
        if inner.entries.len() == before {
            return Err(TrackerError::NotFound(format!("entry {id}")));
        }
        inner.save()
    }

    // ----------------------------------------------------------- aggregates

    /// Суммарное время задачи в секундах на момент `now`, включая время
    /// пунктов чек-листа и идущий таймер.
    pub fn task_seconds_at(&self, task_id: &str, now: DateTime<Utc>) -> i64 {
        report::task_seconds(&self.read().entries, task_id, now)
    }

    /// То же, но «сейчас» берётся из системных часов.
    pub fn task_seconds(&self, task_id: &str) -> i64 {
        self.task_seconds_at(task_id, Utc::now())
    }

    /// Суммарное время пункта чек-листа в секундах.
    pub fn item_seconds_at(&self, item_id: &str, now: DateTime<Utc>) -> i64 {
        report::item_seconds(&self.read().entries, item_id, now)
    }

    /// Отчёт за период по выбранной оси.
    pub fn report_at(
        &self,
        group_by: ReportGroupBy,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Report {
        let inner = self.read();
        report::build_report(
            &inner.tasks,
            &inner.lists,
            &inner.entries,
            group_by,
            from,
            to,
            now,
        )
    }

    /// Отчёт «на сейчас».
    pub fn report(
        &self,
        group_by: ReportGroupBy,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Report {
        self.report_at(group_by, from, to, Utc::now())
    }

    /// Данные диаграммы Гантта за окно.
    pub fn gantt_at(
        &self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> GanttData {
        let inner = self.read();
        report::build_gantt(
            &inner.tasks,
            &inner.entries,
            &inner.milestones,
            from,
            to,
            now,
        )
    }

    /// Данные Гантта «на сейчас».
    pub fn gantt(&self, from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> GanttData {
        self.gantt_at(from, to, Utc::now())
    }

    /// Суммы за сегодня и за неделю — для статус-бара UI.
    pub fn totals_at(&self, now: DateTime<Utc>) -> Totals {
        report::build_totals(&self.read().entries, now)
    }

    /// Суммы «на сейчас».
    pub fn totals(&self) -> Totals {
        self.totals_at(Utc::now())
    }
}

/// Текущий момент в RFC 3339 с точностью до секунд, всегда UTC (`Z`).
fn now_rfc3339() -> String {
    to_rfc3339(Utc::now())
}

fn to_rfc3339(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn clamp_priority(p: u8) -> u8 {
    p.min(MAX_PRIORITY)
}

/// Следующий `order` — на единицу больше максимального (или 0 для пустого).
fn next_order(existing: impl Iterator<Item = i32>) -> i32 {
    existing.max().map(|m| m.saturating_add(1)).unwrap_or(0)
}
