# attention

Модуль src/attention.rs — attention-watcher для подсветки tmux-вкладок при Claude permission prompt.

Содержит:
- AttentionState (Clone, Default) — разделяемое состояние HashMap<session_name, bool> под Arc<tokio::sync::RwLock>. Методы: new(), snapshot() -> HashMap (клон), set(name, flag) (insert).
- detect_claude_prompt(pane: &str) -> bool — строгий AND-детектор: pane должен содержать одновременно три маркера: '❯ 1. Yes', '2. Yes, and don', 'No, tell Claude'.
- watcher_loop (Phase 1.3) — фоновый tokio-loop, опрашивает сессии активного проекта раз в 1500мс через tmux::list_sessions + tmux::capture_pane.

Логика детектора: AND-семантика выбрана сознательно, чтобы минимизировать ложные срабатывания. Каждый из трёх маркеров может встретиться в обычном выводе, но все три вместе — практически однозначно Claude prompt.

Состояние: AttentionState не удаляет ключи при set(name, false) — это позволяет фронтенду различать 'никогда не видели' vs 'видели, prompt закрыт'.

Использование (Phase 2): подключается в AppState как поле attention: Arc<AttentionState>, watcher_loop стартует через tokio::spawn в main().

Юнит-тесты (5 шт.): detects_full_prompt, ignores_plain_shell_output, requires_all_three_markers (4 граничных кейса), attention_state_snapshot_and_set, attention_state_is_cheaply_cloneable.
