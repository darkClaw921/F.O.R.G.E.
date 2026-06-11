# close_task

HTTP-handler DELETE /api/tasks/:id?reason=...&path=... (tmux-web/src/main.rs). Закрывает issue через br close --json [-r reason] в cwd из task_cwd (?path= из query, фолбэк active_path_tx). 204 No Content при успехе; ошибка br -> 400.

Идемпотентность (оба кейса -> 204, не ошибка):
1. ISSUE_NOT_FOUND / 'Issue not found' — задача уже отсутствует (двойной клик, гонка с purge).
2. NOTHING_TO_DO / 'already closed' — br close уже закрытой задачи падает с exit=3 и error-JSON {code: NOTHING_TO_DO, 'all issues skipped: already closed'}. Кейс реален при stale-канбане: задачу закрыли через CLI (br close в сессии), UI ещё показывает её открытой, юзер жмёт clean -> до фикса каждая такая задача давала 400 и алерт 'Очистка завершена с ошибками: ok=0, fail=N'.

Используется: кнопка clean колонок open/in_progress (cleanColumn -> closeTask), кнопка закрытия одиночной карточки. Связанные: task_cwd, tasks::run_br, purge_task (аналогичная идемпотентность по ISSUE_NOT_FOUND), cleanColumn (после bulk-операции делает fetchTasks() для пересинхронизации борда).
