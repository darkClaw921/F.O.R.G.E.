# notifier_config

Phase 3 — глобальный конфиг notifier'а в tmux-web/src/notifier_config.rs. Снимает привязку notify-настроек к концепции Project (см. план remove-projects-concept.md).

## Зачем

Раньше notify_template, notify_delay_minutes, notify_wait_previous, notify_session жили в Project. После отказа от Project — переносятся в один глобальный файл ~/.config/forge/notifier.json (один на пользователя). Применяется ко всем promote-операциям TODO → bd-задача.

## Структуры

### NotifierConfig
- template: String — шаблон notify (плейсхолдеры {id}, {title}, {description}, {priority}, {type}). Пустая строка ⇒ notify не отправляется.
- delay_minutes: u32 — задержка перед отправкой. 0 ⇒ Immediate.
- wait_previous: bool — true ⇒ NotifyMode::WaitPrevious (ждём закрытия предыдущего promoted-issue).
- session: Option<String> — дефолтная tmux-сессия. None ⇒ обязателен body.session в promote.

### NotifierConfigStore
- Cheap-clonable (Arc<RwLock<Inner>>). Одна на процесс, в AppState.notifier_config.
- new(path) — НЕ создаёт файл при отсутствии (zero-config = defaults).
- get() — read-lock клон.
- put(cfg) — полная замена + atomic save.
- patch(req) — partial update; session="" ⇒ сброс в None.

### PatchNotifierConfigReq
DTO для PATCH /api/notifier-config. Все поля Option<T> — применяются только Some-варианты.

## Persistence

Atomic save: tempfile + rename. Битый JSON ⇒ warning + дефолты (devforge не падает).

## Default path

default_config_path() → ~/.config/forge/notifier.json (fallback в temp_dir если HOME не задан).
