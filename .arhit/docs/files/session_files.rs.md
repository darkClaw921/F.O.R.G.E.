# session_files.rs

Модуль tmux-web/src/session_files.rs — приём файлов, вставленных в терминальную сессию из буфера обмена (Ctrl/Cmd+V).

ЗАЧЕМ. Через /ws/attach в PTY уходят только байты ввода — «положить» файл в tmux браузер не может. Поэтому вставка файла реализована в три шага: (1) фронт ловит paste с clipboardData.files; (2) POST /api/sessions/:name/files кладёт байты на диск и возвращает абсолютный путь; (3) фронт вставляет путь в терминал через term.paste — CLI (Claude Code и любой другой) получает его так, будто путь напечатали руками.

КУДА ПИШЕТ. <root>/.forge/pasted/, где root = paths::resolve_root(tmux::session_cwd(session)) — тот же корень, к которому привязаны todos/notifier/git (.beads → .git → сам cwd). Каталог .forge уже в .gitignore проекта, поэтому вставленные скриншоты не засоряют git status.

API модуля:
- MAX_UPLOAD_BYTES = 25 MiB — потолок одной вставки; вешается как DefaultBodyLimit на роут (дефолт axum 2 MiB режет обычный скриншот ретины);
- pasted_dir(root) -> <root>/.forge/pasted;
- ext_for_mime(mime) -> расширение по MIME (png/jpg/gif/webp/svg/pdf/json/zip/txt/md/csv/html, иначе bin). MIME с параметрами (image/png; charset=binary) и в верхнем регистре разбирается корректно;
- sanitize_file_name(raw, content_type, stamp) -> безопасное имя. Берёт ТОЛЬКО базовое имя (режет и по /, и по \), фильтрует алфавит до ASCII [A-Za-z0-9._-+ ] (остальное → _, транслитерации намеренно нет: имя вставляется в командную строку), срезает ведущие точки (ни скрытых файлов, ни . / ..), обрезает stem до 80 и расширение до 16 символов, для пустого имени даёт pasted_<stamp>.<ext по MIME>, а имени без расширения добавляет расширение по MIME;
- unique_path(dir, name) -> свободный путь: name.ext, name-2.ext, … до 1000 попыток (две вставки скриншотов подряд дают одинаковые image.png — молча перезаписать = потеря данных);
- now_stamp() -> YYYYMMDD_HHMMSS в локальной зоне (файл смотрит человек);
- save_pasted(root, raw_name, content_type, bytes) -> абсолютный путь записанного файла; create_dir_all + sanitize + unique_path + write;
- shell_quote(path) -> путь в одинарных кавычках, если в нём есть что-то кроме [A-Za-z0-9/._-+~]; внутренняя кавычка экранируется каноническим '\'' .

БЕЗОПАСНОСТЬ. Имя из буфера — недоверенный ввод: sanitize_file_name делает выход за пределы pasted/ физически невозможным (../../etc/passwd → passwd, /Users/me/.ssh/id_rsa → id_rsa). 11 unit-тестов в модуле фиксируют каждое правило, включая save_pasted_writes_into_forge_pasted (проверяет и путь, и отсутствие перезаписи).

СВЯЗИ: main.rs::upload_session_file (HTTP-обвязка), auth.rs::is_session_file_upload_path (исключение CSRF-guard), paths::resolve_root, tmux::session_cwd, static/js/terminal/paste-files.js (клиент).
