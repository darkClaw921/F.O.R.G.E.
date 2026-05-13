# tmux-web/src/attention.rs::SessionAttention

Внутренняя структура снимка одной сессии для дедупа в watcher_loop. Поля: name, id, attached (u32), session_group (Option<String>), pane_hash (u64), detected (bool). Используется только в attention.rs, в API не уходит.
