# attention::plan_menu_prompt

Распознаёт НУМЕРОВАННЫЙ plan-prompt (ExitPlanMode) Claude Code — редакции, у которых нет footer'а 'Enter to select · Tab/Arrow keys to navigate · Esc to cancel'. Принимает уже нормализованную панель (normalize_ws).

Критерий: панель содержит меню '❯ 1.' И хотя бы один маркер из PLAN_MENU_MARKERS:
- 'written up a plan' — заголовок редакции 2.1.x ('Claude has written up a plan and is ready to execute. Would you like to proceed?');
- 'Ready to code?' — заголовок редакции с 'Here is Claude's plan:';
- 'use auto mode', 'auto-accept edits', 'manually approve edits', 'keep planning' — пункты меню, которых нет ни у одной permission-формы.

Статус-бар обычной сессии ('⏵⏵ auto mode on (shift+tab to cycle)') под маркеры не подходит: там 'auto mode on', а не 'use auto mode' — тест auto_mode_status_bar_is_not_a_plan.

Два потребителя:
1. attention::detect_plan_prompt — оранжевое свечение; без этой ветки сессия с планом не горела (живой кейс: сессия 88568 с планом 'встроенная лёгкая CRM', needs_attention=false при висящем 'Would you like to proceed?').
2. auto_approve::classify_prompt — вызывается ДО permission-веток. Это инвариант безопасности: в редакции 'Ready to code?' меню '❯ 1. Yes, and auto-accept edits' / '2. Yes, and manually approve edits' / '3. No, keep planning' подходит под шаблон Permission3 ('2. Yes,' + '3. No'), и пока план опознавался только по исчезнувшему footer'у, авто-апрув в broad-режиме нажал бы '2' и принял план сам — прямое нарушение правила 'план не подтверждается автоматически никогда'.

Тесты: detects_numbered_plan_prompt_without_footer, detects_ready_to_code_plan_prompt, auto_mode_status_bar_is_not_a_plan (attention.rs); numbered_plan_prompt_is_plan_not_permission, ready_to_code_plan_is_not_classified_as_permission3 (auto_approve.rs). Связано: [[attention::prompt_at_bottom]].
