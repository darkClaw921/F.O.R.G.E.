# PromoteOutcome

Результат promote_todo_core (tmux-web/src/main.rs): pub(crate) struct PromoteOutcome { pub task_id: String, pub notify_scheduled: bool }. task_id — id созданной bd-задачи (может быть пустым, если br не вернул поле id). notify_scheduled — был ли поставлен notify-job в очередь (false при skip из-за отсутствия сессии или при ошибке enqueue). Используется HTTP-handler'ом promote_todo для JSON-ответа { promoted, task_id, notify_scheduled } и воркером auto_promote::run (Фаза 4b) для получения task_id головы цепочки. См. promote_todo_core.
