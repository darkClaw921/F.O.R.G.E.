# tmux-web/src/notifier.rs::NotifyHandle

Cheap-clonable handle для отправки команд в фоновый notifier_loop. Внутри Arc через mpsc::Sender<NotifyCommand>. Хранится в AppState.notify, проброшен во все API-handler'ы.

Публичный API:
- async fn enqueue(&self, job: NotifyJob) -> Result<()> — поставить новый job в очередь. Не блокирует caller'а сверх mpsc-slot (256 ёмкость): практически instant.

Возврат Err только если канал закрыт (loop умер) — критическая ошибка, требует ресторта сервера.

Используется Phase 3 endpoint POST /api/todos/:id/promote: после br create вызывает state.notify.enqueue(notifier::new_job(...)).await.
