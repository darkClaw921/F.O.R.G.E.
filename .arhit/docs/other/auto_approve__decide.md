# auto_approve::decide

Решение о нажатии (tmux-web/src/auto_approve.rs). decide(kind, pane, &UserSettings) -> Option<&'static str>: '1' / '2' — номер пункта, None — не трогать.

Логика:
1. Plan → None всегда (утверждение плана человеку, настройкой не управляется).
2. Question → '1' только если settings.auto_approve_questions (default true, действует внутри включённого авто-апрува).
3. Блоклист (is_dangerous: DEFAULT_DANGEROUS + settings.auto_approve_blocklist, регистронезависимо, по нормализованной панели) → None. Проверяется ДО выбора пункта, поэтому блокирует и broad.
4. broad = settings.auto_approve_broad && kind ∈ {Permission3, AllowBroad} → '2', иначе '1'.

Почему broad ограничен двумя вариантами: в Permission2 (Yes/No) и AllowDeny (Allow/Deny) второй пункт — ОТКАЗ, и расширенный режим на них не распространяется. Регресс-тест broad_mode_picks_second_only_where_it_widens закрепляет это.
