# attention::prompt_at_bottom

Гейт «настоящий prompt рисуется в самом низу панели» — общий для оранжевого свечения (detect_claude_prompt) и авто-апрува (auto_approve::classify_prompt).

Как работает:
1. из панели берутся только непустые строки;
2. с конца срезается TODO-виджет Claude Code через todo_widget_start;
3. из оставшегося берутся последние 4 строки (запас на перенос footer'а по словам в узком терминале);
4. true, если в них есть любая фраза из BOTTOM_FOOTER_MARKERS ЛИБО одна из двух последних строк — пункт меню вида 'N. ' (Claude in Chrome footer'а не рисует, панель заканчивается самим меню).

BOTTOM_FOOTER_MARKERS: 'Esc to cancel' (permission/plan/question со старым footer'ом), 'to edit in Vim' и 'shift+tab to approve' — plan-prompt редакции 2.1.x, у которого внизу пункт '3. Tell Claude what to change', подсказка 'shift+tab to approve with this feedback' и строка 'ctrl+g to edit in Vim · ~/.claude/plans/<файл>.md'.

Зачем гейт: без него воркер авто-апрува однажды принял diff собственных тест-фикстур за AllowBroad-prompt и напечатал «2» в поле ввода чужой сессии. Процитированный prompt (diff, cat, лог) всегда стоит ВЫШЕ поля ввода и статус-бара, настоящий — ниже них (на время prompt'а CLI их скрывает).

Две правки 12.08.2026, обе от живых кейсов:
- срез TODO-виджета: список задач под prompt'ом вытеснял footer за окно из четырёх хвостовых строк (сессия 12312312) — см. [[attention::todo_widget_start]];
- footer plan-prompt'а 2.1.x: у него footer'а 'Enter to select · Tab/Arrow keys to navigate · Esc to cancel' больше нет (сессия 88568, план «лёгкая CRM») — см. [[attention::plan_menu_prompt]].
Оба среза/маркера безопасны для защиты от цитат: под цитатой всегда остаются рамка поля ввода, строка '❯ <текст>' и статус-бар — они ни под маркеры виджета, ни под footer-фразы не подходят.

Известная НЕ покрытая граница: виджет фоновых агентов ('⏺ main', '◯ create-tasks … 7m 16s · ↓ 86.4k tokens') под prompt'ом — зафиксирована тестом bottom_gate_stops_at_widgets_below_prompt. Маркер '⏺' намеренно не срезается: им же помечается обычное сообщение ассистента в потоке, и срез принял бы уже отвеченный prompt выше по экрану за активный.

Вызывающие: detect_claude_prompt (attention.rs), auto_approve::classify_prompt. Тесты: bottom_gate_ignores_todo_widget_below_prompt, bottom_gate_ignores_todo_widget_below_chrome_prompt, detects_numbered_plan_prompt_without_footer, todo_widget_strip_keeps_quoted_prompt_rejected, todo_header_without_items_is_not_a_widget, bottom_gate_handles_degenerate_panes, ignores_quoted_prompt_above_input_box.
