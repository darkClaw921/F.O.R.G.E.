# deduplicate_generating

Дедуплицирует флаги is_generating среди сессий одной итерации watcher_loop (tmux-web/src/attention.rs, private fn).

Структурно-парная функция к deduplicate_attention, но работает с другим сырым сигналом (changed вместо detected) и другой осью группировки (gen_hash вместо pane_hash).

## Сигнатура

fn deduplicate_generating(items: &[GenSnapshot]) -> Vec<(String, bool)>

Возвращает вектор (session_name, final_flag) — по одной записи на каждую входную сессию (в т.ч. явный false для тех, кто не получил primary).

## Алгоритм

Сессии группируются по двум осям с помощью union-find (path compression):

1. **gen_hash** — точное совпадение содержимого последних 50 строк pane (включая scrollback). Linked-сессии, рендерящие одно и то же, объединяются.
2. **session_group** — Some(g) означает linked-сессии tmux: они делят окна и должны давать общий сигнал 'генерации', даже если рендеринг немного разошёлся (cursor blink и т.п.).

Внутри каждой объединённой группы:
- если ни одна сессия не имеет changed=true — все остаются false;
- если хотя бы одна имеет changed=true — выбирается primary через pick_primary_gen, ему true, остальным false.

## Внутренние функции

find(parent, x) — union-find с path compression.
union(parent, a, b) — объединение двух деревьев.

Эти функции локально объявлены в теле deduplicate_generating, точная копия из deduplicate_attention.

## Зачем подавление

Linked-сессии в tmux session-group меняются одновременно. Без дедупа все они получат changed=true одновременно → индикатор 'генерирует' загорается во ВСЕХ вкладках одной группы, что выглядит как ложное срабатывание. Primary получает true, остальные — явный false (для надёжного перезаписывания в AttentionState).

## Pipeline

watcher_loop собирает Vec<GenSnapshot> → deduplicate_generating → for (name, flag) in result: set_generating(name, flag).

## Phase

Добавлено в Phase 2 (forge-c4u) плана is_generating debounce rework. Подключение caller'ов — Phase 3.
