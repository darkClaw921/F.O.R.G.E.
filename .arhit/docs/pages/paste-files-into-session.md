Фича: передача файлов в терминальную сессию — вставка из буфера (forge-660j) и drag&drop (forge-mgad).

ПРОБЛЕМА. В веб-терминале devforge нельзя было передать сессии файл: через /ws/attach в PTY уходят только байты ввода. Чтобы показать Claude Code скриншот, приходилось вручную сохранять его в проект и печатать путь.

РЕШЕНИЕ. Ctrl/Cmd+V с файлом в буфере ИЛИ перетаскивание файла в область терминала → файл заливается на бэкенд → в терминал вставляется абсолютный путь к сохранённому файлу. Дальше это обычный текст в строке ввода CLI. Оба жеста делят один путь заливки и вставки, различается только способ добыть файлы из события.

ПОТОК:
1. static/js/terminal/paste-files.js ловит paste на document (capture, target внутри #terminal, вкладка терминала активна) либо drop на #terminal (capture). Текст без файлов не трогается — его вставляет xterm.
2. POST /api/sessions/<session>/files?name=<имя>&mime=<MIME>, тело — сырые байты, Content-Type: application/octet-stream.
3. main.rs::upload_session_file → tmux::session_cwd → paths::resolve_root → session_files::save_pasted → <root>/.forge/pasted/<имя>.
4. Ответ {path, name, size, insert}; фронт делает state.term.paste(insert + ' ').

РЕШЕНИЯ И ИХ ПРИЧИНЫ (неочевидные развилки):
- MIME в query, а не в Content-Type. CSRF-guard (auth.rs) пропускает мутирующий /api/* с телом только если это application/json либо путь из белого списка raw-заливок с application/octet-stream. Реальный Content-Type файла (например text/plain у .txt) — «простой» для CORS, и разрешить его значило бы открыть CSRF-вектор. Поэтому Content-Type фиксирован octet-stream, а MIME (нужный только для восстановления расширения) едет в ?mime=.
- Белый список путей стал предикатом. RAW_UPLOAD_PATHS — точные строки, но имя сессии в пути произвольное. Добавлен auth.rs::is_session_file_upload_path: ровно два сегмента после /api/sessions/, второй — files, имя непустое. Соседние /api/sessions/:name/windows под исключение не попадают (закрыто тестом).
- Файл кладётся в проект, а не в /tmp. Корень — paths::resolve_root от cwd активной панели, каталог .forge/pasted/. Путь внутри проекта переживает перезапуск, попадает в контекст CLI и уже игнорируется git (.forge в .gitignore).
- В терминал вставляется абсолютный путь, а не относительный: cwd панели может отличаться от корня проекта, а абсолютный путь верен в любом случае.
- Имя санитизируется до ASCII без транслитерации: путь идёт в командную строку, предсказуемость важнее красоты. Кириллическое имя превратится в подчёркивания — это осознанный размен.
- Коллизии разводятся суффиксом -2/-3, а не перезаписью: два скриншота подряд приезжают как image.png, и перезапись была бы потерей данных.
- Лимит 25 MiB через DefaultBodyLimit на конкретный роут (дефолт axum 2 MiB не вмещает скриншот ретины); клиент проверяет размер до отправки.
- Статус показывается DOM-тостом поверх #terminal, а не печатается в терминал: печать ушла бы в вывод tmux и осталась в истории сессии.
- Remote-сессии работают без отдельного кода: try_proxy_to_remote принимает body как Bytes, фронт передаёт origin из state.attachWsOrigin.
- Drag&drop: preventDefault обязателен и на dragover (иначе drop не придёт вовсе), и на drop (иначе браузер откроет файл вместо страницы). Подсветка гасится по счётчику dragDepth, а не по первому dragleave — событие стреляет при переходе между дочерними элементами xterm. Оверлей .terminal-dropzone имеет pointer-events: none, иначе он сам перехватил бы бросок. Брошенная папка (File нулевого размера) отсекается на клиенте с понятным текстом.

ПРОВЕРЕНО:
- эндпоинт вживую (devforge + tmux-сессия): вставка без имени → pasted_<stamp>.png; ../../evil file.png → evil file.png внутри pasted/ с закавыченным insert; повтор имени → evil file-2.png; Content-Type text/plain → 415; 30 МБ → 413; пустое тело → 400; несуществующая сессия → 400;
- 16 unit-тестов Rust (session_files 11 + auth 5), весь бинарь: 598 passed;
- 4 UI e2e-теста (e2e/tests/ui/session-files-ui.spec.ts): drop файла → файл на диске и путь в тосте, dragenter подсвечивает зону и drop её гасит, вставка обезличенного image.png → pasted_<дата>.png, пустой файл/папка → понятная ошибка. Тесты идемпотентны (уникальные имена + самоочистка в afterAll).

ФАЙЛЫ: tmux-web/src/session_files.rs (новый), tmux-web/src/main.rs (upload_session_file + роут), tmux-web/src/auth.rs (is_session_file_upload_path + тесты), tmux-web/static/js/terminal/paste-files.js (новый), tmux-web/static/css/paste-files.css (новый), tmux-web/static/style.css (@import), tmux-web/static/js/core/bootstrap.js (init обоих жестов), e2e/tests/ui/session-files-ui.spec.ts (новый).