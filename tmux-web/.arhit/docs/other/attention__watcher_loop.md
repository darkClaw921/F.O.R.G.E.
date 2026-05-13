# attention::watcher_loop

Фоновый async-loop в src/attention.rs, опрашивает tmux-сессии раз в 1500мс и обновляет AttentionState флагами Claude prompt detection.

Сигнатура (Phase 1 cross-project sessions visibility): pub async fn watcher_loop(attention: Arc<AttentionState>). РАНЕЕ принимал projects: Arc<RwLock<ProjectStore>> и фильтровал сессии по tmux_prefix активного проекта — теперь обходит ВСЕ сессии, потому что фронтенду нужны флаги needs_attention для всех проектов одновременно (включая orphan-сессии в режиме 'All projects').

Поведение на одной итерации:
1. tokio::time::sleep(Duration::from_millis(1500)).
2. tmux::list_sessions() — все сессии (или пустой Vec при ошибке, unwrap_or_default).
3. Для КАЖДОЙ сессии без фильтрации: tmux::capture_pane(name) → detect_claude_prompt(pane) → attention.set(name, flag).

Ошибки tmux не валят loop: list_sessions/capture_pane → unwrap_or_default. tmux-сервер может отсутствовать (вернёт пустой Vec) и сессия может исчезнуть между list и capture (capture_pane сама вернёт Ok('')).

Loop вечный, живёт до завершения процесса. Spawn в main(): tokio::spawn(attention::watcher_loop(app_state.attention.clone())) — теперь принимает только attention, без projects (упрощает зависимости).

Файл: src/attention.rs.
