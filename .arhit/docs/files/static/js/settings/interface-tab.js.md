# static/js/settings/interface-tab.js

Вкладка «Интерфейс» модалки настроек (tmux-web/static/js/settings/interface-tab.js). Экспортирует buildInterfaceForm(settings, onSaved) — fieldset с тумблерами и кнопкой истории авто-апрува.

Поля (1:1 с UserSettings в src/user_settings.rs):
- cmd_hints_enabled (default false), next_step_enabled (default false);
- auto_approve_enabled (false), auto_approve_broad (false), auto_approve_questions (TRUE), auto_approve_blocklist (массив строк).

Дефолты: чекбоксы выключенных по умолчанию фич инициализируются строго '=== true'; auto_approve_questions — единственный с '!== false', потому что его дефолт включённый (он живёт внутри уже opt-in авто-апрува и нужен для ВЫключения).

Строка авто-апрува (.auto-approve-row) — flex: слева кнопка «История», справа главный чекбокс. Зависимые контролы (broad/questions/textarea стоп-слов) disabled и приглушены, пока главный тумблер выключен (syncAutoApprove).

Кнопка «История» тоглит панель .auto-approve-history и при каждом открытии заново тянет GET /api/auto-approve/history (журнал пополняется воркером в фоне, кэшировать нечего), затем renderHistory рисует таблицу: Время (toLocaleString), Сессия, Окно (pane 'win.pane' либо «активное»), Выбор ('1' / '1 + Enter' при attempt>1), Команда (моноширинная, обрезается многоточием, полный текст в title). Пустой ответ — не ошибка, показывается пояснение, что журнал очищается при перезапуске сервера.

Save шлёт свои поля одним PATCH через updateUserSettings (user-settings-api.js); стоп-слова разбиваются по строкам с trim и отбрасыванием пустых (семантика полной замены списка на бэке). Стили — static/css/notifications.css (.auto-approve-row, .auto-approve-history-btn, .auto-approve-history, .auto-approve-history-table).
