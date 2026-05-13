# ws_todos.rs::TodoEvent

Phase 3 — Событие TODO для broadcast-канала. Сериализуется через serde tag='kind' rename_all='snake_case'. Варианты: Upsert{todo: Todo} (создана/обновлена), Removed{project_id, id} (удалена), Reload{project_id} (сигнал клиенту переподтянуть через fetchTodos). Метод project_id() для фильтрации стрима.
