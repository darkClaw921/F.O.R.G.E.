# tmux-web/src/attention.rs

Attention-watcher для tmux-сессий. Раз в 1500мс watcher_loop обходит все сессии (без фильтра по project, фильтр снят ради cross-project sessions visibility), снимает capture_pane и обновляет общие AttentionState через два независимых сигнала.

## Структура AttentionState

Поля под tokio::RwLock:
- map: HashMap<String, bool> — needs_attention для оранжевой подсветки вкладки (Claude permission/plan/question prompt).
- generating: HashMap<String, bool> — is_generating: индикатор работы (что-то рисуется в pane).
- last_hash: HashMap<String, u64> — хэш pane на предыдущем тике (для is_generating).
- prev_prev_hash: HashMap<String, u64> — хэш на тике перед предыдущим (debounce).

Clone дешёвый — лишь клонирование Arc.

## detect_claude_prompt(pane)
OR трёх детекторов:
1. detect_permission_prompt — три маркера AND: '❯ 1. Yes', '2. Yes,', '3. No'.
2. detect_plan_prompt — footer 'Enter to select' + 'Tab/Arrow keys to navigate'.
3. detect_question_prompt — footer 'Enter to select' + '↑/↓ to navigate'.

## update_generation(name, current_hash) — debounce двух последовательных изменений

Семантика: is_generating = (prev_prev != prev) && (prev != current_hash). Если истории недостаточно (< 2 предыдущих наблюдений) — false. На каждом тике сдвиг: prev_prev <- prev, prev <- current.

Цель — устранить ложные 'вспышки' индикатора при разовой перерисовке pane (switch-client, resize-window, attach/detach клиента, любые одиночные события tmux-сервера). Реальная генерация Claude меняет pane на каждом тике watcher'а (1.5с), поэтому индикатор появляется через ~3 секунды после старта генерации.

## watcher_loop(attention)

Бесконечный цикл (1500мс). На каждом тике:
1. tmux::list_sessions без фильтра.
2. Для каждой сессии: capture_pane (видимая часть) + detect_claude_prompt → flag detected; capture_pane_full(name, 30) → gen_hash → update_generation. Оба capture делаются независимо: один для prompt-детектора (видимое), другой для is_generating (последние 30 строк включая scrollback).
3. deduplicate_attention(collected) — группировка по pane_hash и session_group (union-find); primary = attached>0 → max session_id → max name. Только primary получает needs_attention=true.
4. attention.set(name, final_flag) для каждой сессии.

Loop никогда не завершается штатно. Сбои tmux::list_sessions/capture_pane не валят loop (unwrap_or_default → пустой результат, тик пропускается).

## Известные ограничения

См. memory project_attention_dedup_bug.md: дедуп needs_attention может прятать вкладку (linked-сессия attached → оригинал гасится; одинаковый pane_hash → схлопывание). Решение по правке не принято.
