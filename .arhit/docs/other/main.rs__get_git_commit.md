# main.rs::get_git_commit

Axum-хендлер GET /api/git/commit — детали одного git-коммита (мета + тело + изменённые файлы) для hover-попапа гант-диаграммы вкладки Tasks. Сигнатура: async fn get_git_commit(State<AppState>, Query<HashMap<String,String>>) -> Result<Response,(StatusCode,String)>. Ответ — Json {"commit": {hash,ts,subject,body,author,files:[{status,path},...]}} либо {"commit": null}.

Query-параметры:
- hash=<sha> (обязателен): передаётся в git::commit_detail как есть; валидация hex+длина живёт внутри commit_detail (Ok(None) при мусоре). Отсутствует/пустой → {commit:null}.
- path=<abs>: cwd для поиска git-корня; если не задан — state.active_path_tx.borrow().clone() (как get_git_commits).
- server=<id> (extract_server_id): remote НЕ проксируется → сразу {commit:null} (коммиты — локальная фича).

Граница ошибок полностью graceful: git::commit_detail возвращает Ok(None) при невалидном hash / не-git каталоге / отсутствующем коммите, а хендлер дополнительно делает .unwrap_or(None) на случай неожиданного Err — попап никогда не роняет вкладку Tasks. Зарегистрирован роутом .route("/api/git/commit", get(get_git_commit)) рядом с /api/git/commits в main.rs. Построен по образцу get_git_commits (тот же стиль чтения cwd/server и graceful-ответа).
