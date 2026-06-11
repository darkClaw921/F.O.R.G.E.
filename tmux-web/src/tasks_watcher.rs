//! Фоновый file-watcher для `.beads/issues.jsonl` — мульти-корневой.
//!
//! ### Назначение
//!
//! Следит за изменениями `issues.jsonl` (его пишет `br sync --flush-only` и
//! автоматически каждая write-операция `br create/update/close`) СРАЗУ ВО ВСЕХ
//! интересных корнях, пересчитывает diff между предыдущим и новым snapshot
//! задач каждого корня и бродкастит [`TaskEvent`] подписчикам через общий
//! `tokio::sync::broadcast::Sender`.
//!
//! Подписчики глобального канала:
//! - `notifier.rs` — режим `wait_previous` (ждёт `closed` предыдущей задачи);
//! - `auto_promote::run` — цепочка авто-промоута TODO (ждёт закрытия головы).
//!
//! UI (`/ws/tasks`) глобальный канал НЕ использует — у него per-connection
//! watcher'ы на `?path=` клиента (см. `ws_tasks.rs`).
//!
//! ### Почему мульти-корневой
//!
//! Раньше watcher следил за единственным «активным путём»
//! (`active_path_tx`), который после старта процесса фактически никогда не
//! менялся — т.е. события приходили только для проекта, в котором запущен
//! сервер. Из-за этого авто-цепочка промоута НЕ двигалась для задач любых
//! других проектов: их closed-события просто никто не наблюдал (баг
//! «авто-запуск следующей todo не сработал, хотя задача закрыта»).
//!
//! Теперь [`run_multi_watcher`] держит по одному дочернему watcher-task'у на
//! каждый уникальный `.beads/`-корень из набора кандидатов, который ему
//! присылает collector-task из `main.rs` (cwd всех tmux-сессий + initial cwd
//! процесса + корни активных цепочек авто-промоута, пересборка каждые 5с).
//!
//! ### Как работает один корень ([`watch_root`])
//!
//! 1. Initial snapshot задач через [`crate::tasks::snapshot`] — baseline для
//!    диффов, НЕ бродкастится.
//! 2. notify::recommended_watcher на директорию `.beads/` (не на файл —
//!    atomic-rename меняет inode). Recursive=NonRecursive.
//! 3. select-loop: notify-событие → 200ms tail-debounce → `snapshot()` +
//!    `diff_issues()` → broadcast каждого события.
//!
//! Task живёт до исключения корня из набора (JoinHandle::abort) или до
//! shutdown процесса.
//!
//! ### Граничные случаи
//!
//! - У кандидата нет `.beads/` ни в нём, ни выше ([`find_beads_dir`]) —
//!   корень тихо пропускается (появится `.beads/` — подхватим на следующем
//!   тике collector'а).
//! - Несколько сессий в одном репо (включая подкаталоги) — дедуп по
//!   фактическому `.beads/`-пути: один watcher на репозиторий.
//! - broadcast::send Err = «никто не подписан» — норма, игнорируем.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{sleep_until, Instant};

use crate::tasks::{diff_issues, snapshot, TaskEvent};

/// Tail-debounce окно для группировки burst'ов file-system событий.
/// 200ms подобрано так, чтобы покрыть `br sync --flush-only` + write JSONL +
/// fsync (как правило умещается в ~50ms), но не вносить заметную задержку
/// в UI. Можно уменьшить до 100ms, но 200ms даёт запас при больших операциях.
pub const DEBOUNCE_MS: u64 = 200;

/// Главный цикл мульти-корневого watcher'а. Вызывается один раз из `main()`
/// через `tokio::spawn`, держится в памяти всю жизнь процесса.
///
/// # Параметры
///
/// - `roots_rx` — watch-receiver с набором путей-кандидатов (cwd сессий,
///   initial cwd, корни авто-цепочек). Каждый кандидат резолвится в фактический
///   `.beads/`-корень через [`find_beads_dir`]; кандидаты без `.beads/`
///   пропускаются. Sender держит collector-task в `main.rs`.
/// - `tasks_tx` — broadcast-sender, в который пушатся [`TaskEvent`]'ы всех
///   наблюдаемых корней. Подписчики — `notifier` и `auto_promote::run`.
///
/// На каждое изменение набора: спавним [`watch_root`] для новых корней,
/// abort'им task'и исчезнувших. Завершается, когда `roots_rx` закрылся
/// (collector дропнут — процесс шатдаунится).
pub async fn run_multi_watcher(
    mut roots_rx: watch::Receiver<BTreeSet<PathBuf>>,
    tasks_tx: broadcast::Sender<TaskEvent>,
) {
    // Ключ — фактическая директория `.beads/` (дедуп сессий одного репо).
    let mut active: HashMap<PathBuf, tokio::task::JoinHandle<()>> = HashMap::new();

    loop {
        // Желаемый набор: beads_dir -> snapshot_root (родитель .beads/).
        let desired: HashMap<PathBuf, PathBuf> = roots_rx
            .borrow()
            .iter()
            .filter_map(|candidate| {
                find_beads_dir(candidate).map(|beads_dir| {
                    let snapshot_root = beads_dir
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| candidate.clone());
                    (beads_dir, snapshot_root)
                })
            })
            .collect();

        // Стопаем watcher'ы корней, выпавших из набора.
        active.retain(|beads_dir, handle| {
            if desired.contains_key(beads_dir) {
                true
            } else {
                handle.abort();
                tracing::info!(beads = %beads_dir.display(), "tasks watcher: root removed");
                false
            }
        });

        // Спавним watcher'ы новых корней.
        for (beads_dir, snapshot_root) in desired {
            if !active.contains_key(&beads_dir) {
                tracing::info!(
                    beads = %beads_dir.display(),
                    root = %snapshot_root.display(),
                    "tasks watcher: root added"
                );
                let tx = tasks_tx.clone();
                let bd = beads_dir.clone();
                active.insert(
                    beads_dir,
                    tokio::spawn(watch_root(snapshot_root, bd, tx)),
                );
            }
        }

        if roots_rx.changed().await.is_err() {
            tracing::info!("tasks watcher: roots channel closed, exiting");
            for (_, handle) in active.drain() {
                handle.abort();
            }
            return;
        }
    }
}

/// Watcher одного `.beads/`-корня: initial snapshot → notify на `.beads/` →
/// debounce → diff → broadcast. Живёт до `JoinHandle::abort` из
/// [`run_multi_watcher`] (или до фатальной ошибки notify).
async fn watch_root(
    snapshot_root: PathBuf,
    beads_dir: PathBuf,
    tasks_tx: broadcast::Sender<TaskEvent>,
) {
    // 1) Initial snapshot — baseline, не бродкастим.
    let mut prev = match snapshot(&snapshot_root).await {
        Ok(s) => {
            tracing::debug!(path = %snapshot_root.display(), n = s.len(), "initial snapshot taken");
            s
        }
        Err(e) => {
            // Не критично: `br` недоступен или БД пуста. Пустой baseline —
            // следующее событие даст «added» для всех задач.
            tracing::warn!(
                error = ?e,
                path = %snapshot_root.display(),
                "initial snapshot failed; using empty baseline"
            );
            std::collections::HashMap::new()
        }
    };

    // 2) notify watcher через tokio::mpsc::UnboundedSender как EventHandler —
    //    паттерн из notify docs (Tokio Integration).
    let (notify_tx, mut notify_rx) =
        mpsc::unbounded_channel::<notify::Result<notify::Event>>();

    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(
        move |res: notify::Result<notify::Event>| {
            let _ = notify_tx.send(res);
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = ?e, "failed to create notify watcher for root");
            return;
        }
    };

    if let Err(e) = watcher.watch(&beads_dir, RecursiveMode::NonRecursive) {
        tracing::error!(
            error = ?e,
            path = %beads_dir.display(),
            "watcher.watch failed for root"
        );
        return;
    }
    tracing::info!(path = %beads_dir.display(), "watching .beads/ for changes");

    // 3) select-loop: notify event / debounce timer.
    let mut debounce_deadline: Option<Instant> = None;

    loop {
        let timer = debounce_deadline.map(sleep_until);

        tokio::select! {
            biased;

            // Истечение debounce-таймера → snapshot, diff, broadcast.
            _ = async {
                if let Some(t) = timer { t.await }
                else { std::future::pending::<()>().await }
            } => {
                debounce_deadline = None;
                match snapshot(&snapshot_root).await {
                    Ok(new_snap) => {
                        let events = diff_issues(&prev, &new_snap);
                        if !events.is_empty() {
                            tracing::debug!(
                                n = events.len(),
                                root = %snapshot_root.display(),
                                "broadcasting task events"
                            );
                            for ev in events {
                                // send Err = нет подписчиков, игнор.
                                let _ = tasks_tx.send(ev);
                            }
                        }
                        prev = new_snap;
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, "snapshot failed during debounce");
                    }
                }
            }

            // Notify-событие — стартуем/продлеваем debounce.
            event = notify_rx.recv() => {
                match event {
                    None => {
                        tracing::warn!("notify channel closed unexpectedly");
                        return;
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = ?e, "notify error event");
                    }
                    Some(Ok(ev)) => {
                        if relevant_event(&ev) {
                            debounce_deadline =
                                Some(Instant::now() + Duration::from_millis(DEBOUNCE_MS));
                        }
                    }
                }
            }
        }
    }
}

/// Ищет `.beads/` начиная с `start` и вверх по родительским каталогам.
/// Возвращает `Some(<path>/.beads)` при первом найденном или `None` если
/// дошли до корня файловой системы.
///
/// Поведение зеркалит логику `br`: проект может быть подкаталогом репо
/// (например, `F.O.R.G.E./tmux-web` — `.beads` лежит в `F.O.R.G.E.`).
pub fn find_beads_dir(start: &std::path::Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        let candidate = cur.join(".beads");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Фильтр: интересны ли пути события. `br sync` пишет `issues.jsonl`
/// (иногда через write tmp → rename). Любой path в событии, кончающийся на
/// `issues.jsonl` или `.tmp`/`.swp` рядом — считаем релевантным.
pub fn relevant_event(ev: &notify::Event) -> bool {
    ev.paths.iter().any(|p| {
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        name == "issues.jsonl"
            || name.starts_with("issues.jsonl.")  // например issues.jsonl.tmp
            || name.ends_with(".jsonl")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_event(path: &str) -> notify::Event {
        notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    #[test]
    fn relevant_jsonl() {
        assert!(relevant_event(&fake_event("/x/.beads/issues.jsonl")));
    }

    #[test]
    fn relevant_tmp_sibling() {
        assert!(relevant_event(&fake_event("/x/.beads/issues.jsonl.tmp")));
    }

    #[test]
    fn not_relevant_db() {
        assert!(!relevant_event(&fake_event("/x/.beads/beads.db")));
        assert!(!relevant_event(&fake_event("/x/.beads/beads.db-wal")));
    }

    #[test]
    fn relevant_other_jsonl() {
        // Если кто-то когда-то добавит другие .jsonl — пускай тоже триггерит,
        // лишний snapshot не повредит, а пропустить полезное событие — хуже.
        assert!(relevant_event(&fake_event("/x/.beads/labels.jsonl")));
    }
}
