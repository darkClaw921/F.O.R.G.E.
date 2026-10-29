//! `forge-tracker` — личный таск-трекер и тайм-трекер F.O.R.G.E.
//!
//! # Зачем отдельный слой
//!
//! В F.O.R.G.E. уже есть «рабочий» контур задач: канбан вкладки Tasks поверх
//! `br` и колонка TODO (`tmux-web/src/todos.rs`). Оба принадлежат ПРОЦЕССУ:
//! beads-задача — это обязательство перед агентом, а TODO-карточка при
//! промоуте исчезает. Личного слоя, где человек ведёт свои задачи и меряет по
//! ним время, не было. Трекер — именно он.
//!
//! # Три решения, определяющие устройство крейта
//!
//! 1. **Задачи глобальные.** Ни в одной структуре нет `root_path`, стор один —
//!    `~/.config/forge/tracker.json`. Задача принадлежит человеку, а не
//!    каталогу. Кнопка «→ tasks/todo» (фаза 8) выбирает корень в момент
//!    переноса и КОПИРУЕТ задачу, ничего не меняя в трекере.
//! 2. **Крейт листовой.** Нет зависимостей ни на `devforge`, ни на другие
//!    плагины: перенос в TODO делает фронт через существующий
//!    `POST /api/todos`, поэтому про `TodoStore` крейт не знает. Ребро
//!    направлено только в его сторону.
//! 3. **Таймеры параллельные.** Одновременно могут идти несколько отрезков по
//!    РАЗНЫМ задачам; внутри одной задачи активен максимум один (повторный
//!    старт возвращает текущий). Инвариант держит [`store::TrackerStore`], а
//!    не UI.
//!
//! # Устройство
//!
//! * [`models`] — модель данных и envelope файла;
//! * [`store`] — плоское JSON-хранилище с атомарной записью, CRUD, чек-лист,
//!   вехи и таймеры;
//! * [`report`] — чистые расчёты: суммы, отчёт по дням/задачам/спискам,
//!   данные Гантта;
//! * [`config`] — путь к стору (в тестах подменяется на временный каталог);
//! * [`error`] — типизированные ошибки, по классу которых HTTP-слой выберет
//!   код ответа.
//!
//! * [`state`] — состояние плагина: стор, broadcast-канал, воркеры;
//! * [`routes`] — HTTP-поверхность `/api/tracker/*`;
//! * [`ws`] — realtime-поток `/ws/tracker`;
//! * [`dto`] — снимок состояния, общий у `GET /state` и кадра `snapshot`.
//!
//! # Контракт плагина
//!
//! Наружу крейт отдаёт РОВНО ЧЕТЫРЕ функции — те же, что у `forge-vk`
//! (`plugins/vk/src/lib.rs`):
//!
//! 1. [`init`] — читает конфиг, поднимает стор и broadcast-канал;
//! 2. [`register_routes`] — навешивает `/api/tracker/*` и `/ws/tracker`;
//! 3. [`spawn_workers`] — запускает фоновые задачи и немедленно возвращается;
//! 4. [`shutdown`] — гасит воркеров и будит WS-циклы.
//!
//! Порядок вызова из `main.rs` (фаза 3) — ровно этот. Ошибка [`init`] НЕ
//! фатальна для хоста: трекер это опциональная вкладка, и сломанный
//! `tracker.json` не должен мешать человеку работать с терминалом, поэтому
//! хост логирует `warn` и стартует без плагина.
//!
//! Модули публичны для тестов и читаемости доков, но хосту из них не нужно
//! НИЧЕГО: он оперирует конфигом, состоянием как непрозрачным `Arc` и
//! роутером.
//!
//! Включение флагом `--tracker` появится в фазе 3, фронт — с фазы 4.

pub mod config;
pub mod dto;
pub mod error;
pub mod models;
pub mod report;
pub mod routes;
pub mod state;
pub mod store;
pub mod ws;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

pub use config::{default_store_path, TrackerConfig};
pub use dto::{ProgressDto, StateDto};
pub use error::{Result, TrackerError};
pub use models::{
    ChecklistItem, EntryPatch, ItemPatch, ListPatch, Milestone, MilestonePatch, TaskList,
    TaskPatch, TaskStatus, TimeEntry, TrackerFile, TrackerTask,
};
pub use report::{GanttBar, GanttData, Report, ReportGroupBy, ReportRow, Totals};
pub use state::TrackerState;
pub use store::{ChecklistProgress, NewTask, TrackerStore};
pub use ws::TrackerEvent;

/// Как часто сторож проверяет забытые таймеры.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// С какой длительности идущий отрезок считается забытым.
///
/// Двенадцать часов — намеренно много: рабочий день, перерыв и вечерняя правка
/// одной задачи в сумме дают вполне законные девять-десять часов, и порог
/// вроде восьми превратил бы предупреждение в фоновый шум, который перестают
/// читать.
const STALE_TIMER_HOURS: i64 = 12;

/// Поднимает состояние плагина.
///
/// Синхронная и не трогающая сеть: у трекера нет ни токенов, ни внешних
/// сервисов, всё его состояние — один локальный JSON.
///
/// # Errors
///
/// Только фатальные беды файловой системы. Отсутствующий и даже битый
/// `tracker.json` ошибкой не считается: пользователь получает работающий
/// сервер и пустую вкладку, а исходные байты остаются на диске (см.
/// [`TrackerStore::load`]).
pub fn init(cfg: TrackerConfig) -> Result<Arc<TrackerState>> {
    let state = TrackerState::new(&cfg)?;
    let file = state.store.snapshot();
    tracing::info!(
        target: "forge_tracker",
        store_path = %cfg.store_path().display(),
        lists = file.lists.len(),
        tasks = file.tasks.len(),
        entries = file.entries.len(),
        active_timers = file.entries.iter().filter(|e| e.stopped_at.is_none()).count(),
        "forge-tracker: state initialized"
    );
    Ok(state)
}

/// Мерджит роуты плагина в приложение хоста.
///
/// Host-параметра нет намеренно: крейт листовой и про `AppState` не знает.
pub fn register_routes(app: Router, state: Arc<TrackerState>) -> Router {
    let plugin = routes::build_router(state);
    tracing::info!(
        target: "forge_tracker",
        "forge-tracker: registered routes (/api/tracker/*, /ws/tracker)"
    );
    app.merge(plugin)
}

/// Запускает фоновых воркеров плагина.
///
/// Воркер сейчас один — сторож забытых таймеров. Периодического сохранения
/// среди воркеров НЕТ и не будет: [`TrackerStore`] пишет файл в той же
/// операции, что и мутацию, поэтому «сохранить по таймеру» было бы записью уже
/// записанного, а единственный сценарий, который такой воркер якобы спасает
/// (падение процесса с несохранёнными данными), в этой модели невозможен.
///
/// Сторож нужен из-за параллельных таймеров: несколько идущих отрезков —
/// норма, и забытый со вчера таймер не бросается в глаза среди законных
/// соседей, зато молча дописывает часы в отчёт. Он ничего не останавливает сам
/// (сколько времени засчитать — решение человека, а не фоновой задачи), но
/// оставляет в журнале запись, по которой лишние сутки потом объясняются.
///
/// Возвращается немедленно: старт плагина не имеет права задерживать биндинг
/// порта хостом.
pub async fn spawn_workers(state: &Arc<TrackerState>) {
    let st = state.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(WATCHDOG_INTERVAL);
        // Первый тик срабатывает мгновенно, и это полезно: сразу после старта
        // сервера видно таймеры, пережившие перезапуск.
        loop {
            tokio::select! {
                _ = st.shutdown.cancelled() => break,
                _ = ticker.tick() => report_stale_timers(&st),
            }
        }
    });
    state.register_worker(handle).await;
}

/// Пишет в журнал отрезки, идущие дольше [`STALE_TIMER_HOURS`].
fn report_stale_timers(state: &TrackerState) {
    let now = chrono::Utc::now();
    for entry in state.store.active_entries() {
        let Some(started) = report::parse_ts(&entry.started_at) else {
            continue;
        };
        let hours = (now - started).num_hours();
        if hours >= STALE_TIMER_HOURS {
            tracing::warn!(
                target: "forge_tracker",
                entry_id = %entry.id,
                task_id = %entry.task_id,
                hours,
                "forge-tracker: a timer has been running for a suspiciously long time"
            );
        }
    }
}

/// Graceful shutdown плагина.
///
/// 1. `shutdown.cancel()` — будит WS-циклы и сторожа;
/// 2. `shutdown_workers()` — забирает ручки и гасит их.
///
/// Сохранять стор здесь НЕ НУЖНО и намеренно не делается: каждая мутация уже
/// записана на диск атомарно к моменту ответа роута. «Финальное сохранение» на
/// выходе было бы лишней записью файла в момент, когда процесс уже
/// останавливают, — то есть новым способом испортить данные, а не защитой от
/// потери.
///
/// Идемпотентна: `CancellationToken` можно отменять сколько угодно раз, а
/// список воркеров после первого прохода пуст.
pub async fn shutdown(state: &Arc<TrackerState>) {
    tracing::info!(target: "forge_tracker", "forge-tracker: shutdown initiated");
    state.shutdown.cancel();
    state.shutdown_workers().await;
    tracing::info!(target: "forge_tracker", "forge-tracker: shutdown complete");
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[tokio::test]
    async fn init_on_an_empty_dir_succeeds_and_shutdown_keeps_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tracker.json");
        let state = init(TrackerConfig::with_store_path(&path)).expect("init on an empty dir");
        assert!(state.store.tasks().is_empty());
        // Пустой стор не создаёт файл на ровном месте.
        assert!(!path.exists());

        let task = state
            .store
            .create_task(NewTask::new("починить кран"))
            .unwrap();
        spawn_workers(&state).await;
        shutdown(&state).await;

        assert!(path.exists(), "store file written");
        let reopened = TrackerStore::load(&path).unwrap();
        assert_eq!(reopened.tasks().len(), 1);
        assert_eq!(reopened.tasks()[0].id, task.id);
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state = init(TrackerConfig::with_store_path(
            dir.path().join("tracker.json"),
        ))
        .unwrap();
        spawn_workers(&state).await;
        shutdown(&state).await;
        shutdown(&state).await;
        assert!(state.shutdown.is_cancelled());
    }

    #[test]
    fn init_reads_an_existing_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker.json");
        {
            let store = TrackerStore::load(&path).unwrap();
            store.create_task(NewTask::new("старая задача")).unwrap();
        }
        let state = init(TrackerConfig::with_store_path(&path)).unwrap();
        assert_eq!(state.store.tasks().len(), 1);
        assert_eq!(state.store.tasks()[0].title, "старая задача");
    }
}

