# attention::AttentionState

Разделяемое состояние watcher'а: какие tmux-сессии сейчас имеют открытый Claude permission prompt.

Структура:
- map: Arc<tokio::sync::RwLock<HashMap<String, bool>>>

Методы:
- new() -> Self — пустое состояние
- async snapshot() -> HashMap<String, bool> — клон текущего состояния (для axum-хендлера и WebSocket broadcast)
- async set(name: &str, flag: bool) — insert/перезапись флага для сессии

Cheaply cloneable: Clone делит общий map через Arc, поэтому передача в tokio::spawn и в axum-хендлеры не копирует данные.

Не удаляет ключи при set(name, false) — фронтенд различает 'никогда не видели' (None) vs 'видели, prompt закрыт' (Some(false)). Файл: src/attention.rs.
