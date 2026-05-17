# plugins/echo/src/ws/mod.rs::handle_socket

Главный обработчик одного WS-соединения. Запускает tokio::select! с: idle timeout (60s); heartbeat ping каждые 15s; broadcast subscribe (форвардит ServerEvent в socket если conversation_id совпадает или broadcast); inbound ClientMsg parsing. При закрытии — Close+close socket. last_activity обновляется на любом входящем фрейме.
