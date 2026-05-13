# ws_todos.rs::todos_ws

Phase 3 — GET /ws/todos?project_id=... handler. Принимает Query<TodoWsQuery>; если project_id не задан — берёт активный проект. Upgrade в WebSocket → handle_socket с резолвленным project_id.
