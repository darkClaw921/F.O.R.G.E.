# tmux-web/src/attention.rs::update_generation

Метод AttentionState. Возвращает сырой сигнал changed=(prev!=current) для одного pane-хэша сессии и сохраняет current как новый prev.

Сигнатура: pub async fn update_generation(&self, name: &str, current_hash: u64) -> bool.

Логика:
1. Берёт write-lock на self.last_gen_hash.
2. map.insert(name.to_string(), current_hash) — возвращает прежнее значение Option<u64>.
3. changed = prev.map(|p| p != current_hash).unwrap_or(false). Первое наблюдение → false (нет prev → нет точки сравнения).

Что критично: НЕ пишет в self.generating. Финальный флаг is_generating устанавливается отдельным методом set_generating из watcher_loop после дедупликации сырых сигналов по pane_hash/session_group (linked-сессии меняются одновременно и не должны давать множественные индикаторы).

Tracing:
- info при changed=true ('pane hash changed', поля prev+current+changed=true)
- debug при changed=false с prev=Some (без изменений)
- debug при changed=false с prev=None (первое наблюдение)

История: до Phase 1 рефакторинга использовал sliding-window GENERATION_WINDOW=4 хэшей с проверкой 'все уникальны', что давало порог ~4.5с и подавляло осцилляции tmux redraw. Логика осцилляции теперь переносится в Phase 2 deduplicate_generating: вместо подавления внутри одной сессии — подавление между linked-сессиями одной группы.
