# plugins/echo/src/scheduler/mod.rs

Echo autonomous scheduler — фоновый loop, опрашивающий autonomous_tasks каждые TICK_INTERVAL (5 секунд) и запускающий due-задачи через runner::run_task.

# Публичный API

- spawn(state: Arc<EchoState>, host: Arc<dyn HostApi>) -> JoinHandle<()> — запускает scheduler-task; возвращает handle для graceful shutdown (вызывается из forge_echo::spawn_workers и сохраняется в state.workers).
- TICK_INTERVAL: Duration — константа 5 секунд.
- RunningSet — type alias Arc<Mutex<HashSet<String>>> для in-memory защиты от двойного запуска задачи (если interval_seconds меньше длительности run'а).

# Внутреннее устройство

run_loop читает now → autonomous::find_due(db, now) → для каждой due-задачи проверяет, нет ли её id в RunningSet, если нет — добавляет, спавнит tokio::spawn(runner::run_task), по завершении удаляет id из set.

tick_once вынесен как pub(crate) для unit-тестов без 5-секундной задержки.

# Tolerance к ошибкам

- find_due Err → warn + продолжаем (следующий tick попробует снова).
- panic в run_task ловится tokio::spawn'ом, scheduler-loop не останавливается.

# Unit-тесты

- tick_picks_up_due_task_and_runs_it — due-задача исполняется, status=success, next_run_at сдвинут.
- tick_does_not_double_spawn_running_task — два tick'а подряд для медленной задачи дают один task_run.
- empty_due_tick_is_noop — нет due-задач → tick молча проходит.
