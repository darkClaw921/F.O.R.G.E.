# reconcile_wait_previous_on_startup

notifier.rs: на старте notifier_loop сверяет каждую last_promoted_open_id через task_is_open (br show <id> --json); если задача закрыта пока notifier был offline — вызывает handle_task_closed, продвигая wait_previous-очередь. None (br недоступен) НЕ продвигает (no premature fire).
