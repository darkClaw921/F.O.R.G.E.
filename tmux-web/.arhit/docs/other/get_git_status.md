# get_git_status

REST handler GET /api/git/status (axum). Берёт path активного проекта из state.projects.read().await.active().path.clone() и делегирует в git::status(&cwd). На успех возвращает Json<GitStatus>. На ошибку git → 500 INTERNAL_SERVER_ERROR с текстом через format!('{e:#}') (Display + chain контекст из anyhow). Не-репо обрабатывается на уровне git::status (возвращает Ok с GitStatus { repo: false }, не ошибка) — фронтенд показывает плейсхолдер 'Not a git repository'. Используется Git tab во фронтенде для отрисовки списка modified/staged файлов и branch info.
