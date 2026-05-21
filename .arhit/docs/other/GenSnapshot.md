# GenSnapshot

Снимок состояния одной сессии для дедупликации сигнала is_generating в одной итерации watcher_loop (tmux-web/src/attention.rs).

Структурно-парный к SessionAttention тип, но описывает другой сигнал — 'pane менялся за последний тик'. Используется только внутри attention.rs (private struct).

## Поля

- name: String — ключ итогового флага в AttentionState.generating.
- id: String — tmux id вида $0. Используется для tie-break (наибольший id лексикографически) при выборе primary.
- attached: u32 — число прикреплённых tmux-клиентов. Приоритет в pick_primary_gen.
- session_group: Option<String> — имя tmux session-group. Some(_) означает linked-сессии, которые меняются одновременно и должны давать общий флаг.
- gen_hash: u64 — DefaultHasher по последним 50 строкам pane (включая scrollback). Ось группировки в deduplicate_generating.
- changed: bool — 'сырой' сигнал от AttentionState.update_generation: prev != current хэш pane. Исходное состояние ДО дедупа.

## Pipeline

watcher_loop собирает Vec<GenSnapshot>, передаёт в deduplicate_generating, который возвращает Vec<(name, final_flag)>. Финальные флаги пишутся через AttentionState.set_generating(name, flag).

## Почему отдельный тип, а не SessionAttention

SessionAttention хранит detected (permission prompt) + pane_hash (видимая часть). GenSnapshot хранит changed (raw signal от update_generation) + gen_hash (50 строк со scrollback). Это разные сигналы с разными осями дедупа, поэтому типы не объединены.

## Phase

Добавлено в Phase 2 (forge-wzv) плана is_generating debounce rework. Caller'ы (watcher_loop) подключатся в Phase 3.
