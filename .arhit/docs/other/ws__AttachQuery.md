# ws::AttachQuery

Query-параметры WebSocket endpoint /ws/attach в tmux-web/src/ws.rs.

#[derive(Debug, Deserialize)]
pub struct AttachQuery {
    pub session: String,
    #[serde(default = 'default_cols')] pub cols: u16,
    #[serde(default = 'default_rows')] pub rows: u16,
}

Дефолты: default_cols=80, default_rows=24. Поле session обязательно — если отсутствует, axum::extract::Query возвращает 400.
