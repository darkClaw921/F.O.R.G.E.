# post_git_stage

REST handler POST /api/git/stage (axum). Body: PathsReq { paths: Vec<String> } через Json extractor. Пустой массив → 400 BAD_REQUEST 'paths is empty' (явно запрещаем git add без аргументов). Делегирует в git::stage(&cwd, &req.paths). На успех — 204 NO_CONTENT (без тела). Ошибки git (например, путь вне репо, ENOENT) → 400 со stderr-текстом через format!('{e:#}'). Cwd берётся из active project. Используется фронтендом при чек/унчек файлов в Git tab.
