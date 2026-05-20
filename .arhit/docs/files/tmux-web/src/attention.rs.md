# tmux-web/src/attention.rs

Attention-watcher для tmux-сессий. Раз в 1500мс watcher_loop обходит все сессии (без фильтра по project), снимает capture_pane и обновляет общие AttentionState через два независимых сигнала.

## Структура AttentionState

Поля под tokio::RwLock:
- map: HashMap<String, bool> — needs_attention для оранжевой подсветки вкладки (Claude permission/plan/question prompt).
- generating: HashMap<String, bool> — is_generating: индикатор работы (что-то рисуется в pane).
- hash_history: HashMap<String, VecDeque<u64>> — скользящее окно последних GENERATION_WINDOW (=4) хэшей pane на сессию.

Clone дешёвый — лишь клонирование Arc.

## detect_claude_prompt(pane)
OR трёх детекторов: detect_permission_prompt ('❯ 1. Yes' + '2. Yes,' + '3. No'), detect_plan_prompt ('Enter to select' + 'Tab/Arrow keys to navigate'), detect_question_prompt ('Enter to select' + '↑/↓ to navigate').

## update_generation(name, current_hash) — sliding window N=4 unique hashes

Семантика: is_generating = окно из 4 хэшей заполнено И все 4 хэша уникальны. Это эквивалентно 3 подряд идущим переходам + защита от осцилляций вида A→B→A→B (типичный паттерн tmux redraw после attach клиента).

Раньше был debounce N=2 (поля last_hash + prev_prev_hash): требовалось 2 подряд идущих изменения. Этого не хватало — при клике на сессию через WS /ws/attach создавался новый tmux-клиент, и серия из 2-3 тиков redraw на linked-сессиях ложно зажигала индикатор на ВСЕХ сессиях. Окно K=4 + uniqueness даёт порог ~4.5с и отсеивает осцилляции.

Реальная генерация Claude меняет pane уникальными значениями на каждом тике (1.5с), поэтому индикатор появляется через ~4.5с после старта генерации (раньше ~3с).

## watcher_loop(attention)

Бесконечный цикл (1500мс). На каждом тике: tmux::list_sessions без фильтра; для каждой сессии capture_pane (видимое) → detect_claude_prompt → flag detected + capture_pane_full(name, 30) → gen_hash → update_generation. Дедуп needs_attention через deduplicate_attention (группировка по pane_hash и session_group + union-find; primary = attached>0 → max session_id → max name).

Loop никогда не завершается штатно. Сбои tmux команд не валят loop (unwrap_or_default).

## Известные ограничения

См. memory project_attention_dedup_bug.md: дедуп needs_attention может прятать вкладку.
