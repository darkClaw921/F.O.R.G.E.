# tmux-web/src/attention.rs::AttentionState

Разделяемое состояние индикаторов tmux-сессий (Arc<RwLock<...>> внутри, Clone — дешёвый Arc-клон).

Поля:
- map: HashMap<session_name, bool> — флаг 'нужно внимание' (оранжевая подсветка вкладки). Пишется в watcher_loop после deduplicate_attention.
- generating: HashMap<session_name, bool> — финальный флаг 'is_generating' (что-то рисуется в pane). Пишется ТОЛЬКО через set_generating из watcher_loop после дедупликации сырых changed-сигналов.
- last_gen_hash: HashMap<session_name, u64> — последний наблюдённый хэш pane (последние 30 строк) на сессию. Используется в update_generation для получения 'сырого' сигнала changed=(prev!=current). При первом наблюдении prev отсутствует — changed=false (нет точки сравнения).

Методы:
- new() / Default — пустое состояние.
- snapshot() -> HashMap<String,bool> — копия 'needs_attention' карты.
- set(name, flag) — пишет 'needs_attention'. Insert даже при false (фронтенду нужно различать 'не видели' vs 'видели, false').
- generating_snapshot() -> HashMap<String,bool> — копия 'generating' карты.
- set_generating(name, flag) — единственный писатель 'generating'. Вызывается дедуп-фазой watcher_loop. Insert даже при false.
- update_generation(name, current_hash) -> bool — сырой сигнал changed=(prev!=current). Сохраняет current как новый prev в last_gen_hash. НЕ пишет в self.generating — это сделано сознательно, чтобы дедуп-фаза могла видеть raw-changed по всем сессиям и сворачивать linked-группы перед записью финального флага. tracing: info на изменение хэша, debug на тот же / первое наблюдение.

Архитектура дедупликации (Phase 2-3): watcher_loop собирает Vec<GenSnapshot> со всеми changed-сигналами + meta (pane_hash/session_group), применяет deduplicate_generating (Phase 2), затем пишет финальные флаги через set_generating. Дедуп-ось: linked-сессии tmux получают одинаковые changed одновременно — без дедупа индикатор горел бы во всех вкладках группы.
