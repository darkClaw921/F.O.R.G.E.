# get_git_log

REST handler GET /api/git/log?limit=N (axum). Принимает axum::extract::Query<LogQuery> где LogQuery { limit: Option<u32> }. Default = 100, clamp до 500. Делегирует в git::log(&cwd, limit) и возвращает Json<Vec<GitCommit>>. Cwd берётся из active project. На сбой git → 500 с текстом ошибки. Для не-репо/пустого репо git::log возвращает [] (не ошибка). Используется фронтендом для отрисовки commit graph (canvas).
