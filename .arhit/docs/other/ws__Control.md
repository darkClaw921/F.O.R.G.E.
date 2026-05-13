# ws::Control

JSON control-сообщения от WS-клиента в tmux-web/src/ws.rs.

#[derive(Debug, Deserialize)]
#[serde(tag = 'type', rename_all = 'lowercase')]
enum Control {
    Resize { cols: u16, rows: u16 },
    Switch { session: String },
}

## Семантика
- Resize: PtyHandle::resize(cols, rows) -> SIGWINCH в tmux. cur_cols/cur_rows обновляются для будущих switch.
- Switch: kill старого PtyHandle (Drop), spawn_tmux_attach(new_session, cur_cols, cur_rows), reader-task пересоздаётся.

## Wire-формат
JSON отправляется как WebSocket Text frame:
- {"type":"resize","cols":120,"rows":40}
- {"type":"switch","session":"foo"}

Невалидный JSON / неизвестный type -> tracing::warn, WS остаётся живым.
