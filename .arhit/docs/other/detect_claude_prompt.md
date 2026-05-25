# detect_claude_prompt

Детектор интерактивного Claude Code prompt в tmux pane (attention.rs). Возвращает true если pane содержит один из трёх типов: permission (❯ 1. Yes + 2. Yes, + 3. No), plan/ExitPlanMode (footer 'Enter to select' + 'Tab/Arrow keys to navigate'), question/AskUserQuestion (footer 'Enter to select' + '↑/↓ to navigate').

НОРМАЛИЗАЦИЯ WHITESPACE (фикс 2026-05-25): перед поиском маркеров pane прогоняется через normalize_ws() — все последовательности whitespace (включая \n) схлопываются в один пробел. Причина: Claude Code — full-screen TUI, в узком терминале он переносит длинный footer (~60 символов) по словам на несколько строк, и contains('Tab/Arrow keys to navigate') не находил разорванный маркер → свечение не загоралось на plan/question prompt detached-сессий (permission работал, т.к. маркеры короткие). normalize_ws покрывает word-wrap. Дополнительно capture_pane использует флаг -J (join wrapped lines) для tmux-wrap случаев.

Связано: дедуп needs_attention (deduplicate_attention). Это был ВТОРОЙ независимый баг свечения — первый был дедуп по pane_hash.
