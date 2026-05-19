# attention::watcher_loop

Фоновый async-loop в src/attention.rs::watcher_loop. Сигнатура: pub async fn watcher_loop(attention: Arc<AttentionState>).

Каждые 1500мс обходит ВСЕ tmux::list_sessions и для каждой сессии:
1. tmux::capture_pane(name) — видимая часть pane (без scrollback). Запускает detect_claude_prompt → отдаёт detected в дедупликатор. Считает pane_hash через hash_pane(&pane).
2. tmux::capture_pane_full(name, 30) — последние 30 строк pane (со scrollback). Считает gen_hash через hash_pane(&pane30) и вызывает attention.update_generation(name, gen_hash). Это независимый сигнал 'идёт генерация': любое изменение 30-строчного окна → true. Никакой дедупликации для generating-флага не применяется.
3. Складывает SessionAttention (name, id, attached, session_group, pane_hash, detected) в collected.

После сбора всех сессий — deduplicate_attention(collected) выбирает primary в группах (по pane_hash + session_group, union-find), и финальные флаги пишутся через attention.set(name, flag).

Errors от tmux::list_sessions / tmux::capture_pane / tmux::capture_pane_full не валят loop — везде unwrap_or_default. tmux-сервер может отсутствовать; сессия может исчезнуть между list и capture (capture_pane сама вернёт Ok('')).

Loop вечный. Spawn в main(): tokio::spawn(attention::watcher_loop(app_state.attention.clone())).

Файл: src/attention.rs.
