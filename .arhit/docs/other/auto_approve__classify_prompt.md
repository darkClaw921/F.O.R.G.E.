# auto_approve::classify_prompt

Классификатор prompt'ов Claude Code (tmux-web/src/auto_approve.rs). Принимает содержимое панели (tmux capture-pane), возвращает Option<PromptKind>.

Панель прогоняется через attention::normalize_ws — все whitespace схлопываются в один пробел; без этого word-wrap длинного footer'а в узком терминале рвал бы маркеры (тот же приём, что в detect_claude_prompt).

Порядок проверок значим: Plan и Question определяются по footer'у ('Enter to select' + 'Tab/Arrow keys to navigate' / '↑/↓ to navigate') и проверяются ПЕРВЫМИ — prompt плана тоже содержит нумерованные пункты '1. Yes, and auto-accept edits' и без раннего выхода совпал бы с permission-веткой.

Дальше Chrome-ветка ('❯ 1. Allow' → AllowBroad если есть '2. Allow all', иначе AllowDeny), затем Yes-ветка ('❯ 1. Yes' → Permission3 при '2. Yes,' + '3. No', иначе Permission2 при '2. No').

None = prompt'а нет (обычный shell-вывод, работающий Claude).
