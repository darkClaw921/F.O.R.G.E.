# stability_region

auto_approve::stability_region(pane) -> String — часть панели tmux, по которой авто-апрув считает гард стабильности («экран не менялся два тика подряд»).

Зачем: раньше hash_pane считался по ВСЕЙ панели. В окне, где Claude Code запустил суб-агента (типично для worktree-окон wt:<ts>, в которых гоняют run-phase), над prompt'ом рисуется живая строка статуса — спиннер '⏺ run-phase(...)', '✶ Gesticulating… (1h 16m 12s · ↓ 25.8k tokens)'. Она обновляется чаще тика воркера (2 с), поэтому хэш панели отличался КАЖДЫЙ тик, SessionState.seen никогда не доходил до 2, и prompt в таком окне не подтверждался вообще никогда. Подтверждено двумя подряд снятыми capture-pane живого окна wt:1785950679: панели отличались ровно одним символом спиннера в строке суб-агента.

Как устроено: ищется первая строка, содержащая '❯ 1.'. Регион — от (индекс меню − STABILITY_LEAD_LINES=1) до конца панели, нормализованный через attention::normalize_ws. LEAD=1 берёт строку-вопрос ('Do you want to make this edit to config.rs?', 'Do you want to proceed?'), чтобы два разных запроса с одинаковыми пунктами не считались одним экраном; LEAD=2 брать нельзя — там уже стоит анимированная строка статуса. Если меню не найдено (prompt'а нет), берутся последние STABILITY_TAIL_LINES=6 строк — состояние обновляется, но нажимать всё равно нечего.

Связи: вызывается в auto_approve::run перед hash_pane; результат кладётся в SessionState.pane_hash. На classify_prompt/decide/блоклист не влияет — те по-прежнему смотрят всю панель (prompt_at_bottom, extract_command_region).

Тесты: stability_region_ignores_agent_spinner_above_prompt (регрессия живого кейса), stability_region_keeps_prompt_header, stability_region_without_menu_uses_tail.
