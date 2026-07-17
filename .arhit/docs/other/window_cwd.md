# window_cwd

Функция tmux-web/src/tmux.rs. Возвращает cwd панели КОНКРЕТНОГО окна сессии по индексу.

Назначение: аналог session_cwd, но таргет указывает конкретное окно: '<session>:<index>'. Используется при УДАЛЕНИИ worktree-окна, чтобы узнать, какой каталог .forge-worktrees/<имя> за окном закреплён (и затем удалить именно его).

Сигнатура: pub async fn window_cwd(session: &str, index: u32) -> anyhow::Result<String>.
Команда: 'tmux display-message -p -t <session>:<index> -F #{pane_current_path}'.

Параметры:
- session: имя сессии; валидируется is_valid_session_name (иначе bail! без spawn).
- index: индекс окна в сессии.

Возврат: Ok(String) — обрезанный абсолютный путь. Err при невалидном имени сессии (без spawn), ненулевом exit tmux, или если путь пуст.

Реализация: делегирует приватному хелперу display_pane_current_path('<session>:<index>'). Связи: первый шаг delete_worktree_window; результат канонизируется и проверяется на принадлежность <toplevel>/.forge-worktrees (защита от удаления произвольного каталога).
