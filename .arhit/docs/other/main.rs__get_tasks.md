# main.rs::get_tasks

Axum-хендлер для GET /api/tasks. Resolves std::env::current_dir(), вызывает tasks::list_tasks(&cwd).await и оборачивает результат в Json<serde_json::Value>. Ошибка current_dir или list_tasks → (StatusCode::INTERNAL_SERVER_ERROR, String). Логирует ошибку через tracing::error!. В Phase 6.A current_dir используется как единственный 'активный проект' — в Phase 6.B заменится на путь активного проекта из ProjectStore. Регистрируется в Router как .route('/api/tasks', get(get_tasks)).
