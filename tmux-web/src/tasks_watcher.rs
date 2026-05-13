//! Phase 6.D — фоновый file-watcher для `.beads/issues.jsonl`.
//!
//! ### Назначение
//!
//! Следит за изменениями файла `<active.path>/.beads/issues.jsonl` (его
//! пишет `br sync --flush-only` и автоматически после каждой write-операции
//! `br create/update/close`), пересчитывает diff между предыдущим и новым
//! snapshot задач и бродкастит [`TaskEvent`] подписчикам через
//! `tokio::sync::broadcast::Sender`. WS-handler `/ws/tasks` (см. `ws_tasks.rs`)
//! подписывается и проксирует события в браузер.
//!
//! ### Как работает
//!
//! [`run_watcher`] — единственный публичный entry-point, его spawn'ит
//! `main.rs` один раз при старте сервера. Внутри — outer-loop, который
//! пересоздаёт notify-watcher при каждой смене активного проекта (сигнал
//! приходит через `tokio::sync::watch::Receiver<PathBuf>`).
//!
//! Для каждого активного пути:
//! 1. Читаем initial snapshot задач через [`crate::tasks::snapshot`] —
//!    это baseline для последующих диффов. НЕ бродкастим (клиент при
//!    подключении WS получит свой полный snapshot отдельно).
//! 2. Создаём notify::recommended_watcher с tokio::mpsc::UnboundedSender
//!    в роли EventHandler — паттерн рекомендуемый upstream'ом для
//!    интеграции с tokio runtime (см. notify docs Tokio Integration).
//!    Watch'им именно директорию `.beads/`, а не файл — некоторые редакторы/
//!    инструменты делают atomic-rename (write tmp → rename), и в этом случае
//!    inode файла меняется, а watcher на конкретный файл перестаёт получать
//!    события. Recursive=NonRecursive — нам не нужны вложенные.
//! 3. Inner-loop: select! между notify mpsc, watch (active_path change) и
//!    debounce-таймером. При получении события — стартуем (или продлеваем)
//!    200ms таймер. По истечении таймера — `snapshot()` + `diff_issues()` +
//!    broadcast каждого события.
//! 4. При смене active path: сбрасываем watcher (drop), сбрасываем prev
//!    snapshot, переходим к шагу 1 для нового пути.
//!
//! ### Граничные случаи
//!
//! - `.beads/issues.jsonl` отсутствует (новый проект, ещё не было `br sync`):
//!   watch на саму директорию `.beads/` всё равно поднимется (если она есть
//!   как минимум) — мы зацепим момент создания файла. Если и `.beads/` нет —
//!   логируем warn и не watch'им; cycle ждёт смены active_path.
//! - notify сам не дедуплицирует и не дебаунсит — мы делаем 200ms tail-debounce.
//! - broadcast::send возвращает количество получателей, отказ означает «никто
//!   не подписан» — это норма (никто не открыл WS); мы игнорируем.

use std::path::PathBuf;
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

/// Главный цикл watcher'а. Вызывается один раз из `main()` через
/// `tokio::spawn`, держится в памяти всю жизнь процесса. Завершается только
/// если `active_path_rx.changed().await` возвращает Err (sender дропнут —
/// процесс шатдаунится).
///
/// # Параметры
///
/// - `active_path_rx` — receiver, который получает новый путь активного
///   проекта при каждом `set_active_project` / `init_project` /
///   `create_project` (последние два не меняют active, но безопаснее
///   слать на всякий случай).
/// - `tasks_tx` — broadcast-sender, в который пушатся [`TaskEvent`]'ы.
///   Подписчики — WS-handler'ы `/ws/tasks` (по одному `subscribe()` на
///   соединение).
pub async fn run_watcher(
    mut active_path_rx: watch::Receiver<PathBuf>,
    tasks_tx: broadcast::Sender<TaskEvent>,
) {
    loop {
        // Текущий активный путь.
        let path = active_path_rx.borrow().clone();
        tracing::info!(path = %path.display(), "tasks watcher: starting for project");

        // Запускаем inner-loop, который ведёт watcher до следующей смены пути.
        watch_one(path, &mut active_path_rx, &tasks_tx).await;

        // Если active_path_rx закрылся — выходим.
        if active_path_rx.has_changed().is_err() {
            tracing::info!("tasks watcher: active_path channel closed, exiting");
            return;
        }
    }
}

/// Внутренний цикл — обслуживает один активный путь до его смены.
///
/// Возвращается, когда:
/// - `active_path_rx.changed()` сигналит новый путь.
/// - Или `active_path_rx` закрылся (тогда возвращается; внешний loop тоже выйдет).
async fn watch_one(
    project_path: PathBuf,
    active_path_rx: &mut watch::Receiver<PathBuf>,
    tasks_tx: &broadcast::Sender<TaskEvent>,
) {
    // 1) Initial snapshot — baseline, не бродкастим.
    let mut prev = match snapshot(&project_path).await {
        Ok(s) => {
            tracing::debug!(path = %project_path.display(), n = s.len(), "initial snapshot taken");
            s
        }
        Err(e) => {
            // Не критично: либо `br` ещё не доступен, либо `.beads/` отсутствует.
            // Возвращаем пустой snapshot и продолжаем — следующее событие
            // создаст «added» для всех задач.
            tracing::warn!(
                error = ?e,
                path = %project_path.display(),
                "initial snapshot failed; using empty baseline"
            );
            std::collections::HashMap::new()
        }
    };

    // 2) Найдём фактический `.beads/` каталог. `br` walk'ает up до корня репо,
    //    поэтому active project может быть подкаталогом — мы должны идти тем же
    //    путём, иначе watch на пустую директорию ничего не даст.
    let beads_dir = match find_beads_dir(&project_path) {
        Some(d) => d,
        None => {
            tracing::warn!(
                path = %project_path.display(),
                "tasks watcher: no .beads/ found in project path or its ancestors — skipping watch"
            );
            let _ = active_path_rx.changed().await;
            return;
        }
    };

    // 3) Создаём notify watcher через tokio::mpsc::UnboundedSender как
    //    EventHandler. Это паттерн из notify docs: любой
    //    `Fn(notify::Result<Event>) + Send + 'static` подходит, а tokio
    //    UnboundedSender реализует `Fn` через `send()`.
    let (notify_tx, mut notify_rx) =
        mpsc::unbounded_channel::<notify::Result<notify::Event>>();

    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(
        move |res: notify::Result<notify::Event>| {
            // Если receiver дропнут — send_error, игнорируем.
            let _ = notify_tx.send(res);
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = ?e, "failed to create notify watcher; will wait for path change");
            let _ = active_path_rx.changed().await;
            return;
        }
    };

    if let Err(e) = watcher.watch(&beads_dir, RecursiveMode::NonRecursive) {
        tracing::error!(
            error = ?e,
            path = %beads_dir.display(),
            "watcher.watch failed; waiting for active path change"
        );
        let _ = active_path_rx.changed().await;
        return;
    }
    tracing::info!(path = %beads_dir.display(), "watching .beads/ for changes");

    // 4) Inner select-loop: notify event / active path change / debounce timer.
    //
    // Дебаунс: храним Option<Instant> — момент, к которому надо
    // среагировать. Каждое новое событие — обновляем deadline на now+200ms.
    // sleep_until(deadline) внутри select сравнивается с notify_rx.recv():
    // если новое событие приходит — мы пересоздаём ветку с новым deadline.
    let mut debounce_deadline: Option<Instant> = None;

    loop {
        // Если debounce активен — ждём notify ИЛИ истечения таймера ИЛИ change.
        // Если debounce не активен — таймера нет, ждём только notify/change.
        let timer = match debounce_deadline {
            Some(t) => Some(sleep_until(t)),
            None => None,
        };

        tokio::select! {
            biased;

            // Смена активного проекта — выходим, outer-loop пересоздаст watcher.
            change = active_path_rx.changed() => {
                if change.is_err() {
                    tracing::info!("active_path channel closed");
                    return;
                }
                let new_path = active_path_rx.borrow().clone();
                if new_path != project_path {
                    tracing::info!(
                        from = %project_path.display(),
                        to = %new_path.display(),
                        "tasks watcher: active project changed"
                    );
                    return;
                }
                // Тот же путь — игнор (на практике вряд ли).
            }

            // Истечение debounce-таймера → берём snapshot, считаем diff, broadcast.
            _ = async {
                if let Some(t) = timer { t.await }
                else { std::future::pending::<()>().await }
            } => {
                debounce_deadline = None;
                match snapshot(&project_path).await {
                    Ok(new_snap) => {
                        let events = diff_issues(&prev, &new_snap);
                        if !events.is_empty() {
                            tracing::debug!(n = events.len(), "broadcasting task events");
                            for ev in events {
                                // send Err = нет подписчиков, игнор.
                                let _ = tasks_tx.send(ev);
                            }
                            prev = new_snap;
                        } else {
                            tracing::trace!("debounce fired but diff empty");
                            // Всё равно обновляем prev на случай, если diff был
                            // пуст из-за глюка JSON порядка ключей etc.
                            prev = new_snap;
                        }
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
                        // Канал закрыт (watcher дропнут?) — ситуация
                        // теоретическая. Уходим в outer-loop.
                        tracing::warn!("notify channel closed unexpectedly");
                        return;
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = ?e, "notify error event");
                        // Не падаем — продолжаем слушать.
                    }
                    Some(Ok(ev)) => {
                        // Фильтр: нас интересуют только события на issues.jsonl
                        // (или его tmp-собратья при atomic-rename). Beads
                        // пишет именно `issues.jsonl` через `br sync`.
                        if relevant_event(&ev) {
                            tracing::trace!(?ev, "relevant fs event");
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
