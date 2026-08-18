# Фикс: авто-апрув молчал под TODO-виджетом (12.08.2026)

## Симптом
В сессии 12312312 висел prompt 'Do you want to proceed? / ❯ 1. Yes / 2. No', авто-апрув его не подтверждал, оранжевое свечение не горело (GET /api/sessions отдавал needs_attention=false). Раньше в этой же сессии апрувы работали — в /api/auto-approve/history 7 записей, последняя за 21 минуту до диагностики.

## Причина
Гейт attention::prompt_at_bottom берёт последние 4 непустые строки панели и требует в них 'Esc to cancel' или пункт меню 'N. '. Claude Code рисует список задач ПОД prompt'ом:

    Esc to cancel · Tab to amend · ctrl+e to explain
    5 tasks (0 done, 1 in progress, 4 open)
    ◼ Изучить репозитории anti_chaos и hormonal_bot
    ◻ Подготовить сервер (Docker, базовые пакеты)
    ◻ Развернуть anti_chaos бот (@antichaos_check_bot)
    ◻ Развернуть hormonal_bot (@powercouplesyncbot)
    ◻ Найти исходники HR-нумеролог бота (@HRnumerolog_bot)

Последние 4 непустые строки — пункты списка → гейт false → classify_prompt None → воркер считает, что prompt'а нет. Тот же гейт стоит в detect_claude_prompt, поэтому пропадало и свечение. Апрувы в этой сессии работали ровно до момента, когда агент создал задачи.

Отвергнутые версии: настройки (все включены, блоклист пуст), числовое имя сессии (target с ':' корректен), Claude-панель не опозналась (pane_current_command = 2.1.228), блоклист/LLM-эскалация (записей в историю не было вовсе — до decide дело не доходило), анти-луп (attempts обнуляется как раз при classify_prompt == None).

## Изменение
tmux-web/src/attention.rs:
- prompt_at_bottom: перед взятием хвоста срезает TODO-виджет;
- todo_widget_start + is_todo_header + константы TODO_WIDGET_MAX_LINES (24), TODO_ITEM_MARKERS ('◼ ◻ ☑ ☐ ✓ ✔').

Виджетом считается заголовок 'N tasks (…)' с идущими под ним ТОЛЬКО пунктами-маркерами. '⏺'/'◯' (виджет фоновых агентов, сообщения ассистента) намеренно не срезаются: '⏺' начинает обычные сообщения в потоке, и срез принял бы уже отвеченный prompt выше по экрану за активный. Эта граница остаётся зафиксированной тестом bottom_gate_stops_at_widgets_below_prompt.

Защита от процитированного prompt'а сохранена: под цитатой в diff/логе всегда идут рамка поля ввода, строка '❯ <текст>' и статус-бар — под маркеры виджета они не подходят, срез не срабатывает.

## Тесты
attention.rs: bottom_gate_ignores_todo_widget_below_prompt, bottom_gate_ignores_todo_widget_below_chrome_prompt (Chrome-форма без footer'а), todo_widget_strip_keeps_quoted_prompt_rejected, todo_header_without_items_is_not_a_widget.
auto_approve.rs: classifies_permission_under_todo_widget (классификация + decide + extract_subject).
Полный прогон: 685 passed.

## Живая проверка
Изолированный tmux (TMUX_TMPDIR=/private/tmp/aatodo/tmux, env -u TMUX, HOME во временной папке, devforge run --port 7399):
1) реальный capture панели 12312312 как фикстура → в логе 'auto-approved Claude Code prompt session=fixt kind=Permission2 choice=1 attempt=1', цифра '1' напечаталась в панель;
2) контроль — процитированный prompt с полем ввода, статус-баром и TODO-виджетом под ними → 0 апрувов, панель не тронута.

Рабочий devforge при этом не перезапускался; чтобы фикс подхватился на рабочем сервере, нужен рестарт процесса.