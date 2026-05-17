# tmux-web/src/tmux.rs::rename_session

pub async fn rename_session(old: &str, new: &str) -> anyhow::Result<()> — переименовывает существующую tmux-сессию через 'tmux rename-session -t <old> <new>'. Валидирует оба имени через is_valid_session_name. Если old == new → Ok(()) без вызова tmux. Любой ненулевой exit tmux маппится в Err с stderr-сообщением (включая случаи: сессии нет, имя занято). Используется backend-эндпоинтом PATCH /api/sessions/:name.
