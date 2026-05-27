# restore_all_session_history

POST /api/sessions/history/restore-all. Для каждой записи store.list(), пропуская активные имена в tmux, вызывает restore_one_session; возвращает Json {restored:[имена]}. Ошибки отдельных сессий логируются, не прерывают цикл.
