# tmux-web/src/attention.rs

Attention-watcher для tmux-сессий. Раз в 1500мс watcher_loop обходит ВСЕ сессии (без фильтра по project), снимает capture_pane и обновляет общие AttentionState через два независимых сигнала: needs_attention (Claude prompt) и is_generating (активность в pane).

## Структура AttentionState

Поля под tokio::RwLock:
- map: HashMap<String, bool> — needs_attention для оранжевой подсветки вкладки (Claude permission/plan/question prompt).
- generating: HashMap<String, bool> — финальный is_generating (после дедупликации).
- last_gen_hash: HashMap<String, u64> — ПОСЛЕДНИЙ замеченный gen_hash50 на сессию (заменил hash_history: VecDeque + GENERATION_WINDOW=4). Используется в update_generation для определения 'pane изменился между prev и current'.

Clone дешёвый — лишь клонирование Arc.

## detect_claude_prompt(pane)
OR трёх детекторов: detect_permission_prompt ('❯ 1. Yes' + '2. Yes,' + '3. No'), detect_plan_prompt ('Enter to select' + 'Tab/Arrow keys to navigate'), detect_question_prompt ('Enter to select' + '↑/↓ to navigate').

## update_generation(name, current_hash) → bool (raw signal: prev≠current)

Семантика (PHASE 1 рефакторинг): возвращает true если prev!=current (т.е. pane изменился между этим и предыдущим тиком). Под write-lock'ом обновляет last_gen_hash[name]=current_hash. НЕ пишет в self.generating — финальную запись делает set_generating ПОСЛЕ дедупликации.

Это сменило прежнюю sliding-window-семантику (K=4 уникальных хэшей). Старый K=4 защищал от осцилляций типа A→B→A→B (типичный tmux redraw при switch-client/resize), но давал ложные срабатывания на ВСЕХ сессиях одной session_group + создавал лаг ~4.5с до загорания индикатора. Новый prev≠current даёт мгновенный сигнал, а ложные подсветки на 'зрителях' прибиваются per-tick дедупом в deduplicate_generating.

## set_generating(name, flag)
Прямая запись в self.generating[name]=flag под write-lock'ом. Вызывается watcher_loop'ом после deduplicate_generating, чтобы записать ИТОГОВЫЙ флаг (а не raw сигнал из update_generation).

## GenSnapshot (struct)
Per-tick кортеж для дедупа: name, session_group, gen_hash50, attached, session_id, raw_generating. Собирается watcher_loop'ом в Vec<GenSnapshot> перед deduplicate_generating.

## pick_primary_gen
Копия pick_primary (для needs_attention): среди группы выбирает primary по правилу attached>0 → max session_id → max name. Используется deduplicate_generating.

## deduplicate_generating(snapshots: Vec<GenSnapshot>) → HashMap<String, bool>
Per-tick дедуп. Группирует сессии по ключу (session_group, gen_hash50). В каждой группе с raw_generating=true оставляет true только у primary (через pick_primary_gen), остальные → false. Это устраняет 'загорается на чужих' (например, на сидящих в группе или просто наблюдающих за тем же pane сессиях с одинаковым контентом).

## watcher_loop(attention)

Бесконечный цикл (1500мс). На каждом тике:
1. tmux::list_sessions без фильтра.
2. Для каждой сессии: capture_pane (видимое) → detect_claude_prompt → flag detected; capture_pane_full(name, 50) → gen_hash50 → update_generation(name, hash) → raw_generating bool.
3. Собирает Vec<GenSnapshot> для всех живых сессий.
4. Дедуп needs_attention через deduplicate_attention (группировка по pane_hash и session_group; primary = attached>0 → max session_id → max name) → state.set.
5. Дедуп is_generating через deduplicate_generating → state.set_generating (для каждой сессии).
6. Cleanup last_gen_hash и map/generating для исчезнувших сессий через .retain (по списку текущих имён).

Loop никогда не завершается штатно. Сбои tmux команд не валят loop (unwrap_or_default).

## Изменения относительно прошлой версии

- 30 строк → 50 строк pane (capture_pane_full).
- sliding window K=4 unique + hash_history → prev≠current + last_gen_hash.
- Добавлен per-tick дедуп is_generating через GenSnapshot/deduplicate_generating (параллельный с deduplicate_attention).

## Известные ограничения

См. memory project_attention_dedup_bug.md: дедуп needs_attention может прятать вкладку.
