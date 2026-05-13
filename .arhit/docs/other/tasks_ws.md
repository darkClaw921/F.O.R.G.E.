# tasks_ws

Axum-handler /ws/tasks: upgrade WS и delegate в handle_socket. Регистрируется в main.rs как .route('/ws/tasks', get(ws_tasks::tasks_ws)).
