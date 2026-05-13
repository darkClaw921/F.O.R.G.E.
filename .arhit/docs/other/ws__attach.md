# ws::attach

HTTP+WS handler GET /ws/attach в tmux-web/src/ws.rs.

## Сигнатура
pub async fn attach(ws: WebSocketUpgrade, Query(q): Query<AttachQuery>) -> Response

## Query-параметры (AttachQuery)
- session: String — обязателен. Имя tmux-сессии для tmux attach -t <session>.
- cols: u16 — стартовые столбцы PTY, default 80.
- rows: u16 — стартовые строки PTY, default 24.

Если session отсутствует, axum Query-extractor возвращает 400 Bad Request.

## Поведение
Логирует ws upgrade + session/cols/rows и вызывает ws.on_upgrade(|socket| handle_socket(socket, q)). Реальный bridge — в private async fn handle_socket.

## Регистрация
В main.rs: .route('/ws/attach', get(ws::attach)).
