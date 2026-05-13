# buildNotificationsForm

Phase 5 — рендер fieldset с notify-настройками одного проекта в Settings modal (tmux-web/static/app.js).

Параметры: project (DTO с полями notify_template, notify_delay_minutes, notify_wait_previous, notify_session) и onSaved (callback после успешного PATCH).

Поля формы:
- notify_template — textarea (multiline, rows=3), placeholder 'task: {title}\n{description}'. Поддерживает плейсхолдеры {id} {title} {description} {priority} {type}.
- notify_delay_minutes — input[type=number, min=0, step=1]. 0 = отправлять сразу.
- notify_wait_previous — checkbox. Если включён, переопределяет delay (сообщение уходит после закрытия предыдущей задачи в той же сессии).
- notify_session — input[text], опциональный override tmux-сессии (пусто = текущая сессия проекта).

Hint выводит подсказку про плейсхолдеры и семантику delay/wait_previous.

Save-кнопка собирает payload: notify_template (string, может быть пустой), notify_delay_minutes (parseInt fallback 0), notify_wait_previous (bool), notify_session ('' → null). Зовёт saveProjectSettings(project.id, payload). На успех — onSaved(); на ошибку — inline-сообщение в форме (не закрывая модалку).

Возвращает <fieldset class='notify-fieldset'>.
