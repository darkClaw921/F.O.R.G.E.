//! Интеграционные тесты хранилища `forge-tracker`.
//!
//! Все тесты работают в `tempfile::tempdir()` и передают путь через
//! [`TrackerConfig::with_store_path`]. Живой `~/.config/forge/tracker.json`
//! пользователя не открывается ни одним тестом — это условие гейта фазы, а не
//! пожелание: трекер хранит личные задачи, и затереть их прогоном `cargo test`
//! недопустимо.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use forge_tracker::models::{ItemPatch, TaskPatch, TaskStatus};
use forge_tracker::report::{self, ReportGroupBy};
use forge_tracker::store::{NewTask, TrackerStore};
use forge_tracker::TrackerConfig;
use tempfile::TempDir;

fn store() -> (TempDir, TrackerStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = TrackerConfig::with_store_path(dir.path().join("tracker.json"));
    let store = TrackerStore::open(&cfg).expect("open store");
    (dir, store)
}

fn ts(raw: &str) -> DateTime<Utc> {
    report::parse_ts(raw).expect("parse ts")
}

// ------------------------------------------------------------------ базовое

#[test]
fn missing_file_gives_empty_store() {
    let (_dir, store) = store();
    assert!(store.tasks().is_empty());
    assert!(store.lists().is_empty());
    assert!(store.all_entries().is_empty());
}

#[test]
fn data_survives_reopen_from_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracker.json");

    let task_id = {
        let store = TrackerStore::load(&path).unwrap();
        let list = store.create_list("работа", Some("#C98BFF".into())).unwrap();
        let task = store
            .create_task(NewTask {
                title: "написать стор".into(),
                list_id: Some(list.id.clone()),
                estimate_min: Some(60),
                ..NewTask::default()
            })
            .unwrap();
        store.add_item(&task.id, "модель").unwrap();
        store.create_milestone("релиз", "2026-10-01", None, None).unwrap();
        task.id
    };

    let reopened = TrackerStore::load(&path).unwrap();
    assert_eq!(reopened.tasks().len(), 1);
    assert_eq!(reopened.lists().len(), 1);
    assert_eq!(reopened.items(&task_id).len(), 1);
    assert_eq!(reopened.milestones().len(), 1);
    assert_eq!(reopened.task(&task_id).unwrap().estimate_min, Some(60));
}

#[test]
fn store_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, store) = store();
    store.create_task(NewTask::new("приватная задача")).unwrap();

    let meta = std::fs::metadata(store.file_path()).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[test]
fn broken_json_does_not_panic_and_yields_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracker.json");
    std::fs::write(&path, b"{ not json at all ").unwrap();

    let store = TrackerStore::load(&path).expect("broken file must not be an error");
    assert!(store.tasks().is_empty());

    // Стор остаётся рабочим: первая же запись кладёт валидный файл.
    store.create_task(NewTask::new("после битого файла")).unwrap();
    let again = TrackerStore::load(&path).unwrap();
    assert_eq!(again.tasks().len(), 1);
}

// -------------------------------------------------------------------- CRUD

#[test]
fn task_crud_and_status_transitions() {
    let (_dir, store) = store();
    let task = store
        .create_task(NewTask {
            title: "первая".into(),
            priority: Some(9), // выше шкалы — обрезается до 4
            ..NewTask::default()
        })
        .unwrap();
    assert_eq!(task.status, TaskStatus::Inbox);
    assert_eq!(task.priority, 4);
    assert!(task.done_at.is_none());

    let updated = store
        .update_task(
            &task.id,
            TaskPatch {
                title: Some("первая, но переименована".into()),
                status: Some(TaskStatus::Done),
                ..TaskPatch::default()
            },
        )
        .unwrap();
    assert_eq!(updated.title, "первая, но переименована");
    assert!(updated.done_at.is_some(), "перевод в done ставит done_at");

    let back = store.set_status(&task.id, TaskStatus::Doing).unwrap();
    assert!(back.done_at.is_none(), "возврат в работу снимает done_at");

    store.delete_task(&task.id).unwrap();
    assert!(store.task(&task.id).is_none());
    assert!(store.delete_task(&task.id).is_err(), "повторное удаление — NotFound");
}

#[test]
fn deleting_task_cascades_to_items_and_entries() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("с потрохами")).unwrap();
    let other = store.create_task(NewTask::new("соседняя")).unwrap();
    let item = store.add_item(&task.id, "пункт").unwrap();
    store.start_timer(&task.id, Some(&item.id), None).unwrap();
    store.start_timer(&other.id, None, None).unwrap();
    let milestone = store.create_milestone("релиз", "2026-10-01", None, None).unwrap();

    store.delete_task(&task.id).unwrap();

    assert!(store.items(&task.id).is_empty(), "пункты удалены каскадом");
    assert!(store.entries(&task.id).is_empty(), "отрезки удалены каскадом");
    assert_eq!(store.all_entries().len(), 1, "чужой отрезок не тронут");
    assert_eq!(
        store.milestones().first().map(|m| m.id.clone()),
        Some(milestone.id),
        "веха переживает удаление задачи"
    );
}

#[test]
fn deleting_list_keeps_its_tasks() {
    let (_dir, store) = store();
    let list = store.create_list("список", None).unwrap();
    let task = store
        .create_task(NewTask {
            title: "в списке".into(),
            list_id: Some(list.id.clone()),
            ..NewTask::default()
        })
        .unwrap();
    let ms = store
        .create_milestone("веха списка", "2026-10-01", Some(list.id.clone()), None)
        .unwrap();

    store.delete_list(&list.id).unwrap();

    assert!(store.task(&task.id).is_some(), "задача не удаляется вместе со списком");
    assert_eq!(store.task(&task.id).unwrap().list_id, None);
    assert_eq!(store.milestones().iter().find(|m| m.id == ms.id).unwrap().list_id, None);
}

#[test]
fn reorder_sets_order_by_position() {
    let (_dir, store) = store();
    let a = store.create_task(NewTask::new("A")).unwrap();
    let b = store.create_task(NewTask::new("B")).unwrap();
    let c = store.create_task(NewTask::new("C")).unwrap();

    store
        .reorder_tasks(&[c.id.clone(), a.id.clone(), b.id.clone()])
        .unwrap();

    let titles: Vec<String> = store.tasks().into_iter().map(|t| t.title).collect();
    assert_eq!(titles, vec!["C", "A", "B"]);
}

// --------------------------------------------------------------- чек-лист

#[test]
fn checklist_items_are_ordered_and_toggle_moves_done_at() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("с чек-листом")).unwrap();
    let first = store.add_item(&task.id, "раз").unwrap();
    let second = store.add_item(&task.id, "два").unwrap();
    store.add_item(&task.id, "три").unwrap();

    let titles: Vec<String> = store.items(&task.id).into_iter().map(|i| i.title).collect();
    assert_eq!(titles, vec!["раз", "два", "три"]);

    let toggled = store.toggle_item(&first.id).unwrap();
    assert!(toggled.done);
    assert!(toggled.done_at.is_some());

    let untoggled = store.toggle_item(&first.id).unwrap();
    assert!(!untoggled.done);
    assert!(untoggled.done_at.is_none(), "снятие галочки стирает done_at");

    store
        .update_item(&second.id, ItemPatch { done: Some(true), ..ItemPatch::default() })
        .unwrap();
    let progress = store.checklist_progress(&task.id);
    assert_eq!((progress.done, progress.total), (1, 3));
    assert_eq!(progress.percent(), 33);

    store
        .reorder_items(&task.id, &[second.id.clone(), first.id.clone()])
        .unwrap();
    let titles: Vec<String> = store.items(&task.id).into_iter().map(|i| i.title).collect();
    assert_eq!(titles[0], "два");
}

#[test]
fn deleting_item_keeps_its_time_on_the_task() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("задача")).unwrap();
    let item = store.add_item(&task.id, "пункт").unwrap();
    let entry = store
        .create_entry(
            &task.id,
            Some(&item.id),
            "2026-09-08T10:00:00Z",
            Some("2026-09-08T10:30:00Z"),
            None,
        )
        .unwrap();

    store.delete_item(&item.id).unwrap();

    let kept = store
        .entries(&task.id)
        .into_iter()
        .find(|e| e.id == entry.id)
        .expect("отрезок остался у задачи");
    assert_eq!(kept.item_id, None, "ссылка на удалённый пункт снята");
    assert_eq!(store.task_seconds_at(&task.id, ts("2026-09-08T12:00:00Z")), 1800);
}

#[test]
fn milestone_crud() {
    let (_dir, store) = store();
    let ms = store.create_milestone("демо", "2026-09-20", None, None).unwrap();
    let updated = store
        .update_milestone(
            &ms.id,
            forge_tracker::models::MilestonePatch {
                title: Some("демо для заказчика".into()),
                date: Some("2026-09-21".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.title, "демо для заказчика");
    assert_eq!(updated.date, "2026-09-21");

    store.delete_milestone(&ms.id).unwrap();
    assert!(store.milestones().is_empty());
    assert!(store.delete_milestone(&ms.id).is_err());
}

// ---------------------------------------------------------------- таймеры

#[test]
fn two_timers_on_different_tasks_run_in_parallel() {
    let (_dir, store) = store();
    let a = store.create_task(NewTask::new("A")).unwrap();
    let b = store.create_task(NewTask::new("B")).unwrap();

    let ea = store.start_timer(&a.id, None, None).unwrap();
    let eb = store.start_timer(&b.id, None, None).unwrap();

    assert_ne!(ea.id, eb.id);
    let active = store.active_entries();
    assert_eq!(active.len(), 2, "оба таймера идут одновременно");
    assert!(active.iter().all(|e| e.stopped_at.is_none()));
}

#[test]
fn second_start_on_the_same_task_returns_the_running_entry() {
    let (_dir, store) = store();
    let a = store.create_task(NewTask::new("A")).unwrap();

    let first = store.start_timer(&a.id, None, None).unwrap();
    std::thread::sleep(StdDuration::from_millis(20));
    let again = store.start_timer(&a.id, None, None).unwrap();

    assert_eq!(first.id, again.id, "повторный start возвращает текущий отрезок");
    assert_eq!(first.started_at, again.started_at, "начало не переставляется");
    assert_eq!(store.active_entries().len(), 1, "второго активного не появилось");
}

#[test]
fn stopping_one_task_does_not_touch_the_other() {
    let (_dir, store) = store();
    let a = store.create_task(NewTask::new("A")).unwrap();
    let b = store.create_task(NewTask::new("B")).unwrap();
    store.start_timer(&a.id, None, None).unwrap();
    let eb = store.start_timer(&b.id, None, None).unwrap();

    let stopped = store.stop_timer(&a.id).unwrap().expect("A был активен");
    assert!(stopped.stopped_at.is_some());

    let active = store.active_entries();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, eb.id, "таймер B продолжает идти");

    // Повторный стоп идемпотентен.
    assert!(store.stop_timer(&a.id).unwrap().is_none());
    assert!(store.stop_timer("нет такой задачи").is_err());
}

#[test]
fn checklist_timer_counts_into_task_total_once() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("задача")).unwrap();
    let item = store.add_item(&task.id, "пункт").unwrap();

    let entry = store.start_timer(&task.id, Some(&item.id), None).unwrap();
    assert_eq!(entry.item_id.as_deref(), Some(item.id.as_str()));
    store.stop_timer(&task.id).unwrap();

    // Правим отрезок на ровные 20 минут, чтобы сумма была детерминированной.
    store
        .update_entry(
            &entry.id,
            forge_tracker::models::EntryPatch {
                started_at: Some("2026-09-08T10:00:00Z".into()),
                stopped_at: Some(Some("2026-09-08T10:20:00Z".into())),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .create_entry(
            &task.id,
            None,
            "2026-09-08T11:00:00Z",
            Some("2026-09-08T11:30:00Z"),
            None,
        )
        .unwrap();

    let now = ts("2026-09-08T12:00:00Z");
    assert_eq!(store.task_seconds_at(&task.id, now), 50 * 60);
    assert_eq!(store.item_seconds_at(&item.id, now), 20 * 60);
}

#[test]
fn running_entry_survives_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracker.json");

    let (task_id, entry_id) = {
        let store = TrackerStore::load(&path).unwrap();
        let task = store.create_task(NewTask::new("незакрытый таймер")).unwrap();
        let entry = store.start_timer(&task.id, None, None).unwrap();
        (task.id, entry.id)
    };

    let reopened = TrackerStore::load(&path).unwrap();
    let active = reopened.active_entries();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, entry_id);
    assert!(reopened.active_entry_for_task(&task_id).is_some());
    assert!(reopened.task_seconds(&task_id) >= 0);
}

#[test]
fn manual_entries_are_validated() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("задача")).unwrap();

    let ok = store
        .create_entry(
            &task.id,
            None,
            "2026-09-08T10:00:00Z",
            Some("2026-09-08T11:00:00Z"),
            Some("руками".into()),
        )
        .unwrap();
    assert_eq!(ok.source, "manual");

    // Конец раньше начала — конфликт.
    assert!(store
        .create_entry(
            &task.id,
            None,
            "2026-09-08T11:00:00Z",
            Some("2026-09-08T10:00:00Z"),
            None,
        )
        .is_err());
    // Неразбираемая отметка — конфликт.
    assert!(store
        .create_entry(&task.id, None, "вчера", None, None)
        .is_err());
    // Правка, выворачивающая отрезок наизнанку, тоже отбивается.
    assert!(store
        .update_entry(
            &ok.id,
            forge_tracker::models::EntryPatch {
                started_at: Some("2026-09-08T23:00:00Z".into()),
                ..Default::default()
            },
        )
        .is_err());

    store.delete_entry(&ok.id).unwrap();
    assert!(store.entries(&task.id).is_empty());
}

#[test]
fn second_running_manual_entry_is_rejected() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("задача")).unwrap();
    store.start_timer(&task.id, None, None).unwrap();

    // Инвариант «один активный отрезок на задачу» держится и на ручном пути.
    assert!(store
        .create_entry(&task.id, None, "2026-09-08T10:00:00Z", None, None)
        .is_err());
}

// -------------------------------------------------------------- агрегации

#[test]
fn report_by_day_task_and_list() {
    let (_dir, store) = store();
    let list = store.create_list("работа", None).unwrap();
    let a = store
        .create_task(NewTask {
            title: "A".into(),
            list_id: Some(list.id.clone()),
            ..NewTask::default()
        })
        .unwrap();
    let b = store.create_task(NewTask::new("B")).unwrap();

    store
        .create_entry(&a.id, None, "2026-09-08T10:00:00Z", Some("2026-09-08T11:00:00Z"), None)
        .unwrap();
    store
        .create_entry(&b.id, None, "2026-09-08T11:00:00Z", Some("2026-09-08T11:30:00Z"), None)
        .unwrap();

    let now = ts("2026-09-08T12:00:00Z");
    let by_task = store.report_at(ReportGroupBy::Task, None, None, now);
    assert_eq!(by_task.total_seconds, 5400);
    assert_eq!(by_task.rows.len(), 2);
    assert_eq!(by_task.rows[0].label, "A");

    let by_list = store.report_at(ReportGroupBy::List, None, None, now);
    assert_eq!(by_list.rows.len(), 2);
    assert_eq!(by_list.rows[0].label, "работа");

    let by_day = store.report_at(ReportGroupBy::Day, None, None, now);
    assert_eq!(by_day.total_seconds, 5400);
    assert!(!by_day.rows.is_empty());
}

#[test]
fn gantt_flags_over_estimate_and_tolerates_missing_dates() {
    let (_dir, store) = store();
    let over = store
        .create_task(NewTask {
            title: "оценка час".into(),
            estimate_min: Some(60),
            start: Some("2026-09-08".into()),
            due: Some("2026-09-09".into()),
            ..NewTask::default()
        })
        .unwrap();
    let plain = store.create_task(NewTask::new("без дат и оценки")).unwrap();

    // 4:38 факта против часа плана.
    store
        .create_entry(&over.id, None, "2026-09-08T00:00:00Z", Some("2026-09-08T04:38:00Z"), None)
        .unwrap();

    let data = store.gantt_at(None, None, ts("2026-09-08T12:00:00Z"));
    let bar = data.bars.iter().find(|b| b.task_id == over.id).unwrap();
    assert_eq!(bar.actual_seconds, 278 * 60);
    assert!(bar.over_estimate);

    let plain_bar = data.bars.iter().find(|b| b.task_id == plain.id).unwrap();
    assert!(!plain_bar.over_estimate, "без оценки превышения не бывает");
    assert_eq!(plain_bar.start, None);
}

#[test]
fn totals_cover_running_timer() {
    let (_dir, store) = store();
    let task = store.create_task(NewTask::new("идёт прямо сейчас")).unwrap();
    let entry = store.start_timer(&task.id, None, None).unwrap();

    let started = ts(&entry.started_at);
    let totals = store.totals_at(started + Duration::minutes(5));
    assert!(
        totals.today_seconds >= 300,
        "идущий таймер учитывается до «сейчас»: {totals:?}"
    );
    assert!(totals.week_seconds >= totals.today_seconds);
}
