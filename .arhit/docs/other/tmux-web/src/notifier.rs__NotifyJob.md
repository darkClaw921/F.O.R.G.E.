# tmux-web/src/notifier.rs::NotifyJob

Struct описывающий одну нотификацию в очереди notifier'а. Поля:
- id: String — UUID v4, уникален в notify_state.json.
- project_id: String — id проекта (для группировки wait_previous-очереди).
- task_id: String — id bd-задачи, созданной при promote.
- target_session: String — имя tmux-сессии куда отправить текст.
- text: String — что отправить (уже отрендеренный шаблон).
- mode: NotifyMode — режим доставки.
- created_at_unix_ms: u64 — timestamp создания (для логов).

Сериализуется в notify_state.json через serde. Конструируется через notifier::new_job().
