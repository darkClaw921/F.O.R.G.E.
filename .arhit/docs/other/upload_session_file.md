# upload_session_file

Handler tmux-web/src/main.rs — POST /api/sessions/:name/files?name=<имя>&mime=<MIME>.

Принимает файл, вставленный в терминал сочетанием Ctrl/Cmd+V, и кладёт его в проект сессии.

ФОРМАТ ЗАПРОСА. Тело — сырые байты файла (не multipart: файл всегда один, разбор multipart ничего не даёт). Content-Type ВСЕГДА application/octet-stream — этого требует CSRF-guard (auth::is_allowed_raw_body), поэтому настоящий MIME файла едет в query-параметре mime. Если бы MIME ехал в Content-Type, вставка .txt ушла бы с text/plain — «простым» для CORS типом, и исключение в CSRF-guard стало бы дырой.

ЛОГИКА. try_proxy_to_remote (эндпоинт origin-aware, ?server=<id> уводит запрос на remote-инстанс — body: Option<Bytes> пропускает бинарь без изменений) → проверка непустого тела → tmux::session_cwd(name) → paths::resolve_root → session_files::save_pasted.

ОТВЕТ 200: {path: <абсолютный путь>, name: <итоговое имя файла>, size: <байт>, insert: <строка для вставки в шелл — путь, при необходимости в кавычках>}. Фронт вставляет именно insert.

ОШИБКИ: 400 — пустое тело, невалидное имя сессии, сессии нет; 413 — тело больше session_files::MAX_UPLOAD_BYTES (25 MiB, DefaultBodyLimit навешен прямо на роут); 415 — Content-Type не octet-stream (режет CSRF-guard до handler'а); 500 — не удалось создать каталог или записать файл.

Роут зарегистрирован рядом с /api/sessions/:name/windows; статический сегмент files не конфликтует с :index-роутами (matchit отдаёт приоритет статике).

СВЯЗИ: session_files.rs (вся файловая логика + санитизация), auth.rs::is_session_file_upload_path, static/js/terminal/paste-files.js (единственный клиент).
