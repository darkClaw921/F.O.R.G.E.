# plugins/echo/src/routes/autonomous.rs

REST API для управления автономными задачами Echo.

# Endpoints (все под /api/echo/)

- GET /autonomous-tasks — возвращает { items: [AutonomousTask] }. list_tasks(enabled_only=false).
- POST /autonomous-tasks { name, prompt_template, interval_seconds, model, project_id? } → 201 Created с заполненной AutonomousTask. Валидация: interval >= 1, name не пустой. next_run_at = now + interval_seconds устанавливается в db::repo::autonomous::create_task.
- PATCH /autonomous-tasks/:id { name?, prompt_template?, interval_seconds?, model?, enabled? } → 200 с обновлённой задачей. 404 если id не найден. Валидация interval >= 1. Спецсемантика: при enabled=true и next_run_at=NULL ставит next_run_at=now (чтобы scheduler сразу подобрал).
- DELETE /autonomous-tasks/:id → 204 No Content (идемпотентно даже для отсутствующих задач). Cascade FK сносит связанные task_runs.
- POST /autonomous-tasks/:id/run-now → 200 { ok, task_id, spawned }. Спавнит tokio::task(runner::run_task) — немедленный запуск без ожидания tick'а scheduler'а. 404 если задача не найдена, 503 если HostApi adapter не зарегистрирован.
- GET /autonomous-tasks/:id/runs?limit=50 → { items: [TaskRun] } в порядке started_at DESC. 404 для unknown task.

# Error type

ApiError(StatusCode, String) → JSON { error: msg }. internal(anyhow::Error) логирует и возвращает 500.

# Тесты

10 unit-тестов покрывают: create (success/bad_request), list, patch (success, enable с NULL next_run, 404), delete (idempotent), list_runs (404, items), run_now (200 + spawn видно в БД, 404 для unknown).
