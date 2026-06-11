# withTaskPath

Хелпер фронтенда (tmux-web/static/js/tasks/crud.js). Сигнатура: withTaskPath(url, origin) -> string. Добавляет ?path=<cwd текущей сессии> (sessionCwdOrNull из ws/tasks-ws.js) к URL мутаций /api/tasks — только для origin='local'/falsy; для remote origin URL не трогается (remote-сервер резолвит свой путь сам, а apiFetch добавит ?server=). Если cwd нет (сессия не выбрана/без path) — URL без изменений, сервер фолбэкнется на active_path_tx (прежнее поведение).

Применяется во всех мутациях задач: createTask (POST /api/tasks), updateTask (PATCH), closeTask (DELETE, совместим с ?reason=), purgeTask (POST /purge), reopenTask (POST /reopen). Серверная пара — task_cwd (main.rs). Причина появления: баг clean-колонки — мутации шли в каталог запуска сервера, см. док task_cwd.
