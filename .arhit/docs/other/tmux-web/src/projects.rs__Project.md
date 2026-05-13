# tmux-web/src/projects.rs::Project

Описание одного проекта в registry tmux-web.

## Поля
- id (String) — slug от name: [a-z0-9_-]+, уникален в registry.
- name (String) — человекочитаемое имя (произвольные символы).
- path (PathBuf) — абсолютный путь к корню. Передаётся в tmux new-session -c и br list.
- tmux_prefix (String) — префикс для фильтрации tmux-сессий и автопрефиксования.

## Поля нотификации (Phase 1)
Добавлены для конфигурации TODO→tmux уведомлений. Все #[serde(default)] для совместимости со старыми projects.json.
- notify_template (String) — шаблон текста, отправляемого через send_keys при promote TODO. Поддержка плейсхолдеров {title}/{description} (см. notifier). Пустая строка = не отправлять.
- notify_delay_minutes (u32) — задержка перед отправкой. 0 = немедленно.
- notify_wait_previous (bool) — ждать закрытия предыдущей tmux-задачи (через tasks_watcher) перед следующей отправкой. Default false.
- notify_session (Option<String>) — override имени tmux-сессии. None = использовать сессию активного проекта (по tmux_prefix).

## Derive
Debug, Clone, Default (необходим для ..Default::default() при инициализации старых литералов в add/default_project/set_transient_active), Serialize, Deserialize, PartialEq, Eq.
