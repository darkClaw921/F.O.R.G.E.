# tmux-web/src/attention.rs::hash_pane

Хэширует содержимое панели (str) в u64 через std::collections::hash_map::DefaultHasher. Используется как pane_hash для дедупа и debug-логов в watcher_loop. Криптостойкость не требуется.
