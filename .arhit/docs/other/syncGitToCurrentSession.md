# syncGitToCurrentSession

Функция в tmux-web/static/js/tabs/tui-tabs.js. Синхронизирует cwd lazygit-вкладки с cwd текущей tmux-сессии (state.currentSession → s.path).

Вызывается из sessions/sessions.js в трёх местах:
1. openSession после connectWs — при первом открытии сессии.
2. switchSession после установки state.currentSession — при переключении между сессиями одного проекта (через WS-сообщение 'switch').
3. openSession после switchActiveProject — при переключении на сессию из другого проекта.

Поведение:
- Если state.gitTerm отсутствует или WS не открыт → ничего не делает. resolveCwd (см. ниже) подхватит свежий path при следующем openForActiveProject (т.е. при клике на git-вкладку).
- Если cwd сессии не определён (sessionCwdOrNull вернул null) → ничего не делает.
- Если t.currentCwd === cwd сессии → no-op (избегает лишних switch_cwd сообщений).
- Иначе → t.switchCwd(cwd), который шлёт {type:'switch_cwd',cwd} в WS, и бэкенд (ws.rs::lazygit_attach) рестартит lazygit под новым cwd.

Связка с createTuiTab: вкладка git инициализируется с resolveCwd: () => sessionCwdOrNull(). Это используется в openForActiveProject как первичный источник cwd; fallback — getActiveProject().path, если сессия не выбрана.

Цель: lazygit показывает git-репо именно той сессии, в которой работает юзер (включая orphan-сессии без project_id и сессии в подпапках проекта), а не статически корень активного проекта.
