# attention::AttentionState

Shared state attention-watcher'а (tmux-web/src/attention.rs). Cheap-clone (Arc внутри). Используется AppState.attention.

## Поля (все под tokio::sync::RwLock)

- map: RwLock<HashMap<String, bool>> — needs_attention для оранжевой подсветки вкладки. Ключ — session.name. Значение true означает, что в pane сессии обнаружен Claude prompt (permission/plan/question).
- generating: RwLock<HashMap<String, bool>> — финальный is_generating (после per-tick дедупликации). Ключ — session.name. true = pane менялся в этом тике И сессия — primary в группе session_group/gen_hash50.
- last_gen_hash: RwLock<HashMap<String, u64>> — ПОСЛЕДНИЙ замеченный gen_hash50 на сессию. Используется update_generation для сравнения prev≠current. Заменил прежние поля hash_history (VecDeque) + константу GENERATION_WINDOW=4 (sliding window).

## Конструктор

- new() -> AttentionState — все три HashMap пустые. Создаётся в main.rs::main() и оборачивается в Arc::new() для совместного использования handler'ами и watcher'ом.

## Методы

### Чтение
- snapshot() -> HashMap<String, bool> — атомарный клон self.map. Используется get_sessions для needs_attention.
- generating_snapshot() -> HashMap<String, bool> — атомарный клон self.generating. Используется get_sessions для is_generating.

### Запись (используются ТОЛЬКО watcher_loop'ом)

- set(snapshot: HashMap<String, bool>) — полная замена self.map. Вызывается ПОСЛЕ deduplicate_attention с результатом дедупа needs_attention.
- set_generating(name: &str, flag: bool) — точечная установка self.generating[name]=flag. Вызывается ПОСЛЕ deduplicate_generating для каждой сессии. НОВЫЙ метод (Phase 1.4 рефакторинга).
- update_generation(name: &str, current_hash: u64) -> bool — RAW сигнал: возвращает true если last_gen_hash[name] != current_hash. Атомарно обновляет last_gen_hash[name]=current_hash. Не пишет в self.generating! Финальную запись делает set_generating после дедупа. Новая семантика заменила старый sliding-window K=4 (см. memory project_is_generating_debounce.md).
- cleanup для исчезнувших сессий — выполняется в watcher_loop через .retain на всех трёх HashMap (по списку текущих имён tmux::list_sessions).

## Связи

- main.rs::AppState.attention — единственный владелец Arc<AttentionState>.
- main.rs::get_sessions — читает snapshot() и generating_snapshot() для заполнения SessionDto.{needs_attention,is_generating}.
- attention::watcher_loop — единственный писатель.
