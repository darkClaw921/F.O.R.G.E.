# task_cwd

Резолвер cwd для br-команд task-хендлеров (tmux-web/src/main.rs). Сигнатура: fn task_cwd(state: &AppState, q: &HashMap<String, String>) -> PathBuf. Возвращает явный ?path=<abs> из query (trim, непустой) либо fallback на state.active_path_tx (каталог запуска сервера).

Фикс бага «clean колонки не переживает перезагрузку страницы»: active_path_tx после старта процесса НИКОГДА не обновляется (никто не вызывает .send), поэтому мутирующие хендлеры (close_task, patch_task, purge_task, reopen_task, create_task), бравшие cwd только из него, выполняли br в каталоге запуска сервера, а не в корне проекта текущей сессии. br close в чужом корне отвечал 'Issue not found', что маппилось в идемпотентный 204 -> фронт считал задачу закрытой, а реальная база проекта не менялась; после reload задачи возвращались. GET /api/tasks при этом УЖЕ принимал ?path= (tasks следуют за cwd сессии) — отсюда расхождение чтения и записи.

Используется во всех task-хендлерах: get_tasks, create_task, patch_task, close_task, reopen_task, purge_task. Фронтовая пара — withTaskPath (static/js/tasks/crud.js), добавляющая ?path=<cwd сессии> к URL мутаций. Связанные: AppState.active_path_tx, tasks::run_br, sessionCwdOrNull (ws/tasks-ws.js).
