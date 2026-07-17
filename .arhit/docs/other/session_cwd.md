# session_cwd

Функция tmux-web/src/tmux.rs. Возвращает cwd активной панели сессии.

Назначение: узнать рабочую директорию, из которой пользователь нажал 'радужную кнопку', чтобы затем определить git-toplevel и создать worktree рядом. Команда: 'tmux display-message -p -t <session>: -F #{pane_current_path}'.

Сигнатура: pub async fn session_cwd(session: &str) -> anyhow::Result<String>.

Параметры:
- session: имя сессии; валидируется is_valid_session_name (иначе bail! без spawn).

Таргет: '<session>:' (двоеточие на конце) — session-target, резолвится tmux в активное окно/панель сессии. ДВОЕТОЧИЕ ОБЯЗАТЕЛЬНО: для сессий с числовыми именами (0, 1, …) таргет без ':' tmux истолковал бы как индекс окна и захватил бы чужое окно (известный регресс с числовыми именами сессий).

Возврат: Ok(String) — обрезанный (без \n) абсолютный путь. Err при невалидном имени сессии (без spawn), ненулевом exit tmux, или если путь пуст.

Реализация: делегирует приватному хелперу display_pane_current_path(target). Связи: первый шаг create_worktree_window; результат передаётся в worktree::repo_toplevel.
