# main.rs::get_git_commits

Axum-хендлер GET /api/git/commits — список git-коммитов корня текущей сессии для гант-диаграммы вкладки Tasks. Сигнатура: async fn get_git_commits(State<AppState>, Query<HashMap<String,String>>) -> Result<Response,(StatusCode,String)>. Ответ — Json {"commits": [{hash,ts,subject,author},...]}.

Query-параметры:
- path=<abs>: cwd для поиска git-корня; если не задан — state.active_path_tx.borrow().clone() (cwd активной сессии, как get_tasks).
- since=<unix>: опциональная нижняя граница по committer date (секунды Unix); непарсимое значение тихо → None.
- until=<unix>: опциональная верхняя граница по committer date (секунды Unix); непарсимое → None. Вместе с since задаёт диапазон [since, until] — используется кнопками Сегодня/Вчера ганта.
- server=<id> (extract_server_id): remote НЕ проксируется → сразу {commits:[]} (коммиты — локальная фича).

Читает since и until через q.get(...).and_then(parse::<i64>().ok()), вызывает git::list_commits(&cwd, since, until).await.unwrap_or_default(). Граница ошибок полностью graceful: list_commits уже возвращает Ok(vec![]) при проблемах, а .unwrap_or_default() добавляет страховку на случай Err — вкладка Tasks никогда не падает из-за коммитов. Зарегистрирован роутом .route("/api/git/commits", get(get_git_commits)). Рядом зарегистрирован get_git_commit (GET /api/git/commit) — детали одного коммита.
