# auto_approve::ApprovalRecord

Запись журнала авто-апрува (tmux-web/src/auto_approve.rs), сериализуется в JSON для GET /api/auto-approve/history.

Поля: ts (unix secs), session (имя tmux-сессии), window (ИМЯ окна — #{window_name}, основная колонка «Окно» в UI), pane ('win.pane' — точный адрес нажатия, уходит в tooltip), kind (Permission3/Permission2/AllowDeny/AllowBroad/Question), choice ('1'/'2'), attempt (1 — цифра, 2 — цифра+Enter), subject (команда/адрес/заголовок вопроса, см. extract_subject).

Почему и window, и pane: имя окна — для человека, индекс — точный адрес, куда ушла клавиша (в окне может быть несколько панелей). UI показывает window, при пустом — pane, при пустом обоих — «активное».
