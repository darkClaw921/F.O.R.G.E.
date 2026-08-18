# Plan-prompt Claude Code 2.1.x: свечение и запрет авто-апрува (12.08.2026)

## Симптом
Сессия 88568 (partner_bitrix24_cabinet) висела на плане «встроенная лёгкая CRM» — оранжевое свечение не горело, GET /api/sessions отдавал needs_attention=false, is_generating=true.

## Что за форма
    ───────────────────────────────────────────────
     Claude has written up a plan and is ready to execute. Would you like to proceed?

     ❯ 1. Yes, and use auto mode
       2. Yes, manually approve edits
       3. Tell Claude what to change
          shift+tab to approve with this feedback

     ctrl+g to edit in Vim · ~/.claude/plans/stateful-frolicking-meadow.md

Footer'а 'Enter to select · Tab/Arrow keys to navigate · Esc to cancel' у этой редакции НЕТ.

## Две независимые причины
1. Гейт attention::prompt_at_bottom искал только 'Esc to cancel' (плюс пункт меню в двух последних строках). Последние четыре строки здесь — пункт 3, подсказка shift+tab и путь к файлу плана → гейт false → и свечение, и авто-апрув слепы.
2. detect_plan_prompt требовал 'Enter to select' + 'Tab/Arrow keys to navigate' — маркеров, которых в редакции нет.

is_generating=true — следствие, а не отдельный баг: watcher исключает из «генерирующих» только сессии с needs_attention=true (attention.rs, дедуп индикаторов).

## Скрытая опасность, найденная попутно
Редакция 'Ready to code?' имеет меню '❯ 1. Yes, and auto-accept edits' / '2. Yes, and manually approve edits' / '3. No, keep planning' — это подходит под шаблон Permission3 ('2. Yes,' + '3. No'). Если бы починили ТОЛЬКО гейт, auto_approve::classify_prompt вернул бы Permission3, и в broad-режиме воркер нажал бы '2', то есть принял план за пользователя. Правило 'план не подтверждается автоматически никогда' держалось лишь на footer'е, которого в новых редакциях больше нет.

## Изменение
tmux-web/src/attention.rs:
- BOTTOM_FOOTER_MARKERS = ['Esc to cancel', 'to edit in Vim', 'shift+tab to approve'] — используется в prompt_at_bottom;
- plan_menu_prompt(norm) + PLAN_MENU_MARKERS ('written up a plan', 'Ready to code?', 'use auto mode', 'auto-accept edits', 'manually approve edits', 'keep planning'), требует наличие меню '❯ 1.';
- detect_plan_prompt = старый footer ИЛИ plan_menu_prompt.

tmux-web/src/auto_approve.rs:
- classify_prompt вызывает attention::plan_menu_prompt ДО permission-веток.

## Тесты (690 passed)
attention.rs: detects_numbered_plan_prompt_without_footer, detects_ready_to_code_plan_prompt, auto_mode_status_bar_is_not_a_plan.
auto_approve.rs: numbered_plan_prompt_is_plan_not_permission, ready_to_code_plan_is_not_classified_as_permission3.

## Живая проверка
Изолированный tmux (TMUX_TMPDIR=/private/tmp/aaplan/tmux, env -u TMUX, HOME во временной папке, devforge run --port 7399):
1) фикстура plan 2.1.x → needs_attention=true, в логе 'auto-approve skipped (plan / questions off) kind=Plan', панель не тронута;
2) фикстура 'Ready to code?' → needs_attention=true, 0 нажатий;
3) регресс-контроль Permission3 → 'auto-approved … kind=Permission3 choice=2 attempt=1', затем attempt=2 с Enter, дальше молчание (анти-луп).

Рабочий devforge не перезапускался — чтобы фикс подхватился, нужен рестарт процесса. Связано: [[auto-approve-todo-widget-fix]].