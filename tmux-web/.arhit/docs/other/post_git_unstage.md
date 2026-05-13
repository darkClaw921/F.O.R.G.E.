# post_git_unstage

REST handler POST /api/git/unstage (axum). Семантически зеркало post_git_stage: тот же body PathsReq { paths: Vec<String> }, такая же проверка на пустой массив (→ 400), такой же успех (204) и mapping ошибок (400 со stderr). Делегирует в git::unstage(&cwd, &req.paths). PathsReq переиспользуется между stage и unstage. Используется фронтендом для снятия файла со стейджа.
