# main.rs::format_notify_template

Phase 3 — Подставляет значения TODO/issue в template-строку. Поддерживает плейсхолдеры {id}, {title}, {description}, {priority}, {type}. Дефолтный шаблон в DEFAULT_NOTIFY_TEMPLATE='Новая задача [{id}]: {title} — нужно сделать'. Используется в promote_todo для генерации текста уведомления.
