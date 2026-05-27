# auto_promote

Модуль состояния цепочки авто-промоута TODO. Полная документация: см. tmux-web/src/auto_promote.rs. Типы: AutoChainEntry (голова цепочки: active_task_id + session) и AutoChainMap = Arc<RwLock<HashMap<root_path, AutoChainEntry>>> (in-memory, без persist). Используется AppState.auto_chain; пишет promote_todo_core (Фаза 3), читает воркер auto_promote::run (Фаза 4b).
