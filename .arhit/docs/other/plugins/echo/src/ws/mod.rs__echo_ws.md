# plugins/echo/src/ws/mod.rs::echo_ws

Axum handler GET /ws/echo?conversation_id=&token=. Upgrade'ит HTTP в WebSocket и передаёт управление в handle_socket. Auth уже выполнен bearer-auth middleware'ом host'а; token-query — только для удобства browser-WS клиента.
