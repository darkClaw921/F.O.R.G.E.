# telegram_ws

WebSocket-хендлер плагина Telegram, GET /ws/telegram?token= (plugins/telegram/src/ws/mod.rs).

ЖИЗНЕННЫЙ ЦИКЛ: telegram_ws апгрейдит HTTP (Bearer-auth в remote-mode уже сделан middleware хоста — query-параметр token существует только потому, что браузерный WebSocket не умеет слать заголовок Authorization). handle_socket: (1) подписка на broadcast ДО отправки снапшота — иначе событие между снапшотом и подпиской терялось бы навсегда; (2) первым кадром уходит login_state, клиенту не нужно ждать события; (3) tokio::select! biased: shutdown-токен -> закрытие; idle-таймаут IDLE_TIMEOUT (60с без единого кадра) -> закрытие; heartbeat HEARTBEAT_INTERVAL (15с) -> TgServerMsg::Ping; событие broadcast -> кадр клиенту БЕЗ фильтрации; входящий кадр -> парсинг TgClientMsg (Pong сбрасывает idle-таймер).

Lagged(n) НЕ рвёт соединение: клиенту уходит TgServerMsg::Resync, он перечитывает состояние по REST. Битый входящий кадр — warn и игнор (forward-compat: он может прийти от более новой версии фронта). Ошибка сериализации ServerMsg логируется, но считается успехом — это баг сервера, а не повод рвать сокет.

ОТЛИЧИЕ ОТ ECHO ws/mod.rs: там события фильтруются по conversation_id, здесь фильтра нет вообще — сервер вещает всё.
