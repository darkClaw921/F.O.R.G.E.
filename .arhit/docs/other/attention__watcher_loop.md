# attention::watcher_loop

Бесконечный фоновый tokio-task в tmux-web/src/attention.rs. Раз в 1500мс обновляет AttentionState (needs_attention + is_generating) для всех tmux-сессий.

## Сигнатура
async fn watcher_loop(projects: Arc<RwLock<ProjectStore>>, attention: Arc<AttentionState>)
Спавнится в main.rs::main() через tokio::spawn сразу после создания AppState.

## Pipeline на каждом тике (1500мс)

1. tmux::list_sessions().await — список ВСЕХ tmux-сессий (без фильтра по project).
2. Для каждой сессии параллельно:
   a. capture_pane(name).await — видимое содержимое pane (короткое).
   b. detect_claude_prompt(pane) → bool 'нужно внимание' (RAW сигнал).
   c. capture_pane_full(name, 50).await — последние 50 строк pane (изменено с 30 в Phase 3.1).
   d. gen_hash50 = hash(pane_full).
   e. attention.update_generation(name, gen_hash50).await → bool raw_generating (prev≠current, новая семантика; раньше — sliding window K=4).
3. Собирает Vec<GenSnapshot> { name, session_group, gen_hash50, attached, session_id, raw_generating } для каждой живой сессии.
4. Дедуп needs_attention: deduplicate_attention(needs_attention_snapshots) → HashMap<name, bool>. Группирует по (session_group, pane_hash); union-find; primary = pick_primary (attached>0 → max session_id → max name). state.set(...) под write-lock'ом.
5. Дедуп is_generating: deduplicate_generating(gen_snapshots) → HashMap<name, bool>. Группирует по (session_group, gen_hash50); среди raw_generating=true оставляет true только у primary (pick_primary_gen, идентичный pick_primary). Для каждой сессии state.set_generating(name, flag).await.
6. Cleanup: state.map.retain / state.generating.retain / state.last_gen_hash.retain по списку текущих имён сессий — убирает записи исчезнувших сессий, чтобы HashMap не рос неограниченно.

## Зачем дедуп is_generating

При attach клиента к pane (включая switch-client/resize) tmux перерисовывает pane на ВСЕХ сессиях одной session_group одинаковым контентом → одинаковый gen_hash50. Без дедупа все сессии загорались бы вместе с primary. Дедуп оставляет true только у primary, на остальных is_generating=false.

## Изменения относительно прошлой версии (Phase 1-3 рефакторинга)

- capture_pane_full(name, 30) → capture_pane_full(name, 50) [Phase 3.1].
- AttentionState.hash_history (VecDeque) + GENERATION_WINDOW=4 → AttentionState.last_gen_hash (HashMap<name, u64>) [Phase 1.1-1.2].
- update_generation: sliding window N=4 unique → prev≠current; теперь возвращает RAW сигнал, не пишет в generating [Phase 1.3].
- Добавлены set_generating, GenSnapshot, pick_primary_gen, deduplicate_generating [Phase 1.4, 2.1-2.3, 3.2-3.3].

## Гарантии

- Loop никогда не завершается штатно. Сбои tmux команд не валят loop (unwrap_or_default).
- Snapshot needs_attention и is_generating всегда согласован per-tick (оба обновляются под одним и тем же набором сессий).
- Memory bounded: cleanup через .retain не даёт HashMap расти неограниченно при создании-удалении большого числа сессий.

## Связи

- Читатель: main.rs::get_sessions (через AttentionState::snapshot и generating_snapshot).
- Писатель: единственный — этот watcher_loop.
