# main::get_auto_approve_history

GET /api/auto-approve/history (tmux-web/src/main.rs) — журнал последних авто-апрувов для кнопки «История» в настройках интерфейса.

Отдаёт Json<Vec<auto_approve::ApprovalRecord>> — до 200 записей, новые первыми: ts, session, pane ('win.pane' = в каком окне), kind, choice, attempt, subject (команда/адрес/заголовок вопроса).

Ошибок нет: при выключенном авто-апруве или после рестарта сервера возвращается пустой массив (журнал только в памяти). Роут зарегистрирован рядом с /api/user-settings, то есть под auth_middleware — фронтенд ходит обычным fetch (window.fetch патчится в core/auth.js и сам подставляет Bearer).
