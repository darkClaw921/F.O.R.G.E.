# auto_approve::ApprovalHistory

Журнал последних авто-апрувов (tmux-web/src/auto_approve.rs, фича tw-vyk). Arc<RwLock<VecDeque<ApprovalRecord>>>, cheap-clone, живёт в AppState.approvals.

API: new(), push(rec) — вытесняет самую старую запись при переполнении HISTORY_CAP=200, snapshot() — Vec<ApprovalRecord> НОВЫЕ ПЕРВЫМИ (разворот делается на бэке, чтобы фронтенд не сортировал).

ApprovalRecord { ts (unix secs), session, pane ('win.pane', пустая строка = активная панель), kind (Permission3/Permission2/AllowDeny/AllowBroad/Question строкой), choice ('1'/'2'), attempt (1 — цифра, 2 — цифра+Enter), subject }.

Только в памяти: при рестарте процесса журнал пуст. Персист не делается осознанно — это оперативный след последних действий, а не аудит; файл пришлось бы ротировать и чистить.

Пишет воркер auto_approve::run при каждом нажатии, читает handler main.rs::get_auto_approve_history (GET /api/auto-approve/history), рисует static/js/settings/interface-tab.js — кнопка «История» слева от чекбоксов авто-апрува.
