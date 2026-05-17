Phase 4 — Autonomous tasks scheduler для плагина forge-echo.

# Архитектура

Два модуля внутри plugins/echo/src/scheduler/:

- mod.rs — фоновый loop с tick=5s. Поднимается из forge_echo::spawn_workers, возвращает JoinHandle сохраняемый в EchoState.workers.
- runner.rs — исполнитель одного task'а (insert_run → prompt → one_shot → finish_run → broadcast).

# Поток данных

1. main.rs::main → forge_echo::register_routes → forge_echo::spawn_workers(state, host).
2. spawn_workers вызывает scheduler::spawn(state, host) и регистрирует handle через state.register_worker.
3. scheduler::spawn запускает tokio::task → run_loop → каждые 5s: tick_once.
4. tick_once → autonomous::find_due(db, now) → для каждой due-задачи (с in-memory dedup) tokio::spawn(runner::run_task).
5. runner::run_task → insert_run → ensure conversation '__autonomous__/<task_id>' → prompt_builder::build → ClaudeRunner::one_shot → messages::insert(assistant) → finish_run(success) → stats::add_tokens → broadcast AutonomousTaskEvent.

# Защита от двойного запуска

- Уровень scheduler: RunningSet = Arc<Mutex<HashSet<task_id>>> — отбрасывает повторный spawn пока id в множестве.
- Уровень БД: runner.set_next_run сдвигает next_run_at СРАЗУ при insert_run (до выполнения). Следующий tick не увидит задачу как due пока run не финиширует.

# REST API

routes/autonomous.rs регистрирует /api/echo/autonomous-tasks* (GET list, POST create, PATCH update, DELETE, POST run-now, GET runs).

# WS broadcast

ServerMsg::AutonomousTaskEvent { task_id, run_id, status, message_preview } шлётся:
- running — перед началом one_shot.
- success — после успешного finish_run.
- error — после finish_with_error.

Это broadcast-event (ServerEvent::broadcast(...)) — рассылается всем WS-клиентам /ws/echo (а не на конкретную conversation).

# State changes

- state.rs: EchoState получил workers: Arc<Mutex<Vec<JoinHandle<()>>>>, register_worker, shutdown_workers (для Phase 6 graceful shutdown).
- chats.rs: добавлен create_with_id (нужен для детерминированных id служебных conversation).

# Verify status

- cargo build -p devforge: OK (без warning'ов от Phase 4 кода).
- cargo test -p forge-echo: 89 lib tests + 1 e2e integration test (autonomous_scheduler_e2e) — все зелёные.
- E2e test (autonomous_scheduler_e2e): scheduler с реальным spawn + мок CLI + task interval=2s → success TaskRun + broadcast event + token_stats bucket за 8 секунд.