//! Снимок состояния трекера — ОДИН на два канала.
//!
//! Панель трекера получает начальное состояние двумя разными путями:
//! `GET /api/tracker/state` при открытии вкладки и первым кадром
//! `{"kind":"snapshot", …}` при подключении к `/ws/tracker`. Если бы каждый
//! путь собирал ответ сам, они бы неизбежно разъехались — и расхождение
//! проявилось бы не ошибкой сборки, а «после переподключения половина полей
//! исчезла». Поэтому оба зовут [`StateDto::from_store`], и форма у них
//! совпадает по построению.
//!
//! # Что кладётся в снимок и почему именно это
//!
//! Сырых коллекций (`lists`, `tasks`, `items`, `milestones`, `entries`) UI не
//! хватает: терминальная панель показывает рядом с каждой задачей её суммарное
//! время и прогресс чек-листа `2/5 · 40%`. Считать это на фронте означало бы
//! продублировать в JS правила, которые в `report.rs` уже написаны и покрыты
//! тестами (время пункта чек-листа входит в сумму задачи РОВНО ОДИН РАЗ,
//! идущий таймер учитывается по «сейчас»). Поэтому агрегаты считает сервер:
//!
//! * `active` — идущие отрезки для блока `// RUNNING`; их может быть
//!   несколько, параллельные таймеры по разным задачам штатны;
//! * `task_seconds` — секунды по каждой задаче на момент `now`;
//! * `item_seconds` — секунды по каждому пункту чек-листа на момент `now`;
//!   пункт показывает своё время рядом с собой (в макете — колонка справа от
//!   текста), но в сумму задачи оно УЖЕ входит: у отрезка пункта заполнены и
//!   `task_id`, и `item_id`, поэтому `task_seconds` его учитывает. Складывать
//!   `task_seconds` с `item_seconds` нельзя — это и был бы двойной счёт;
//! * `progress` — прогресс чек-листа по каждой задаче;
//! * `totals` — суммы за сегодня и за неделю для статус-бара;
//! * `now` — момент, НА КОТОРЫЙ посчитаны агрегаты. Без него фронт не может
//!   корректно «дотикивать» идущий таймер: часы браузера и сервера расходятся,
//!   и без опорной точки секундомер прыгал бы при каждом snapshot.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::{ChecklistItem, Milestone, TaskList, TimeEntry, TrackerTask};
use crate::report::Totals;
use crate::store::TrackerStore;

/// Прогресс чек-листа задачи в форме, готовой к отрисовке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressDto {
    /// Отмеченных пунктов.
    pub done: usize,
    /// Всего пунктов.
    pub total: usize,
    /// Доля выполненного, 0..100.
    pub percent: u8,
}

/// Полный снимок состояния трекера.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDto {
    /// Списки задач.
    pub lists: Vec<TaskList>,
    /// Задачи.
    pub tasks: Vec<TrackerTask>,
    /// Пункты чек-листов всех задач.
    pub items: Vec<ChecklistItem>,
    /// Вехи Гантта.
    pub milestones: Vec<Milestone>,
    /// Все отрезки времени.
    pub entries: Vec<TimeEntry>,
    /// Идущие сейчас отрезки.
    pub active: Vec<TimeEntry>,
    /// Секунды по каждой задаче на момент `now`.
    pub task_seconds: BTreeMap<String, i64>,
    /// Секунды по каждому пункту чек-листа на момент `now`.
    ///
    /// Подмножество [`Self::task_seconds`], а не добавка к ним: время пункта
    /// уже посчитано в сумме его задачи.
    pub item_seconds: BTreeMap<String, i64>,
    /// Прогресс чек-листа по каждой задаче.
    pub progress: BTreeMap<String, ProgressDto>,
    /// Суммы за сегодня и за неделю.
    pub totals: Totals,
    /// Момент, на который посчитаны агрегаты (RFC 3339, UTC).
    pub now: String,
}

impl StateDto {
    /// Собирает снимок на явно заданный момент.
    ///
    /// Явный `now` — не украшение: на нём держатся тесты агрегатов (иначе они
    /// зависели бы от системных часов) и он же уезжает в поле `now` ответа.
    pub fn from_store_at(store: &TrackerStore, now: DateTime<Utc>) -> Self {
        let file = store.snapshot();
        let active: Vec<TimeEntry> = file
            .entries
            .iter()
            .filter(|e| e.stopped_at.is_none())
            .cloned()
            .collect();

        let mut task_seconds = BTreeMap::new();
        let mut progress = BTreeMap::new();
        for task in &file.tasks {
            task_seconds.insert(
                task.id.clone(),
                crate::report::task_seconds(&file.entries, &task.id, now),
            );
            let p = store.checklist_progress(&task.id);
            progress.insert(
                task.id.clone(),
                ProgressDto {
                    done: p.done,
                    total: p.total,
                    percent: p.percent(),
                },
            );
        }

        let mut item_seconds = BTreeMap::new();
        for item in &file.items {
            item_seconds.insert(
                item.id.clone(),
                crate::report::item_seconds(&file.entries, &item.id, now),
            );
        }

        Self {
            lists: store.lists(),
            tasks: store.tasks(),
            items: file.items,
            milestones: store.milestones(),
            entries: file.entries.clone(),
            active,
            task_seconds,
            item_seconds,
            progress,
            totals: store.totals_at(now),
            now: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }

    /// Снимок «на сейчас».
    pub fn from_store(store: &TrackerStore) -> Self {
        Self::from_store_at(store, Utc::now())
    }
}
