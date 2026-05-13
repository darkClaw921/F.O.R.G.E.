# LogQuery

Query-параметры для GET /api/git/log. Структура: LogQuery { limit: Option<u32> }. derive(Debug, Deserialize). Используется axum::extract::Query<LogQuery>. Default = 100 (через unwrap_or), затем .min(500) для защиты от больших запросов.
