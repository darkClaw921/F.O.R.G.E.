# tmux-web/src/notifier.rs::NotifyMode

Enum режима доставки нотификации в notifier.rs. Сериализуется в JSON с tag='kind' и snake_case rename. Три варианта:
- Immediate — выполнить сразу при enqueue.
- Delayed { fire_at_unix_ms: u64 } — выполнить не раньше абсолютного Unix-таймстампа в миллисекундах. При рестарте: если время уже прошло — fire сразу.
- WaitPrevious { previous_task_id: Option<String> } — ждать, пока bd-задача с указанным id перейдёт в status=closed. None означает 'нет предыдущего' (fire если очередь проекта пуста).

PartialEq+Eq для тестирования сериализации. Все варианты самодостаточны (содержат всю нужную инфу), что упрощает persist/restore.
