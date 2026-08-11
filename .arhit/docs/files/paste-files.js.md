# paste-files.js

Модуль tmux-web/static/js/terminal/paste-files.js — передача файлов в терминальную сессию двумя жестами: вставка из буфера (Ctrl/Cmd+V) и перетаскивание из ОС (drag&drop).

ЗАЧЕМ. Через WebSocket в PTY уходят только байты ввода, файл туда не положить. Модуль перехватывает жест, заливает файл на бэкенд и вставляет в терминал полученный абсолютный путь — дальше CLI (в первую очередь Claude Code) работает с ним как с обычным введённым путём. Шаги «залить + вставить путь» (uploadFiles) у обоих жестов общие, различается только способ добыть список файлов.

ГДЕ ВИСЯТ СЛУШАТЕЛИ.
- paste — на document в фазе capture: xterm обрабатывает вставку в своём скрытом .xterm-helper-textarea и вызывает preventDefault, перехватить надо раньше него. Чтобы не ломать вставку в Telegram/VK/Echo/модалки, обрабатываются только события с target внутри #terminal (isTerminalTarget через contains) и только при state.activeTab === 'terminal'. Если в буфере нет файлов — событие не трогается, текст вставляет сам xterm.
- dragenter/dragover/dragleave/drop — на самом #terminal, тоже в capture (внутренние элементы xterm иначе получили бы всплывающее событие первыми).

API:
- initTerminalPasteFiles() — слушатель вставки, идемпотентна (флаг document.__forgePasteFilesBound);
- initTerminalDropFiles() — слушатели перетаскивания, идемпотентна (флаг .__forgeDropFilesBound); обе вызываются в core/bootstrap.js сразу после initTerminal;
- MAX_PASTE_BYTES = 25 MiB — совпадает с MAX_UPLOAD_BYTES бэкенда, проверяется на клиенте, чтобы не гонять заведомо отвергнутые байты;
- clipboardFiles(dt) — файлы из DataTransfer: сначала dt.files, затем dt.items (в части браузеров скриншот приходит только через items); items с kind === 'string' игнорируются;
- hasDraggedFiles(e) — есть ли в переносе файлы; читается dataTransfer.types, потому что сам список файлов в dragenter/dragover браузер не отдаёт (приватность), только в drop;
- pasteFileName(file, now) — имя для безымянной/обезличенной вставки (image.png, blob, unknown) → pasted_YYYYMMDD_HHMMSS; расширение доберёт бэкенд из ?mime=;
- isTerminalTarget(target) — target внутри #terminal.

DRAG&DROP, НЕОЧЕВИДНОЕ:
- preventDefault на dragover ОБЯЗАТЕЛЕН: без него браузер считает зону не принимающей и drop не приходит вовсе;
- preventDefault на drop обязателен тоже: иначе браузер откроет брошенный файл вместо страницы и сессия исчезнет с экрана;
- dragDepth — счётчик баланса dragenter/dragleave: dragleave стреляет и при переходе между дочерними элементами #terminal (у xterm их много), по одному событию подсветку гасить нельзя, иначе она мигает;
- .terminal-dropzone имеет pointer-events: none — иначе оверлей сам стал бы целью переноса, перехватил бы dragleave/drop у #terminal, подсветка залипла бы, а бросок потерялся;
- брошенная папка приезжает как File нулевого размера (содержимое браузер читать не даёт) — отсекается на клиенте с понятным текстом, а не 400-й «пустое тело» от бэкенда.

ЗАГРУЗКА. Файлы шлются ПОСЛЕДОВАТЕЛЬНО (порядок путей в командной строке должен совпадать с порядком файлов). Запрос: POST /api/sessions/<name>/files?name=<имя>&mime=<MIME> через apiFetch (origin из state.attachWsOrigin — работает и для remote-сессий), Content-Type всегда application/octet-stream (требование CSRF-guard), body — сам File. Из ответа берётся поле insert.

ВСТАВКА. state.term.paste(path + ' ') — эмитит onData, байты уходят в PTY с учётом bracketed paste. Пробел в конце: за путём обычно сразу печатают вопрос. Если WS не открыт — путь не вставляется, показывается ошибка.

СТАТУС. Тост .paste-toast (css/paste-files.css) в правом нижнем углу #terminal: прогресс висит до следующего сообщения, успех 2.5 с, ошибка 6 с. В сам терминал ничего не печатается — служебный текст ушёл бы в вывод tmux и остался в истории сессии.

ТЕСТЫ: e2e/tests/ui/session-files-ui.spec.ts — 4 UI-теста на оба жеста (синтетические DragEvent/ClipboardEvent с настоящими DataTransfer/File).

СВЯЗИ: core/state.js (term/ws/currentSession/attachWsOrigin/activeTab), core/dom.js ($terminalEl), core/api.js (apiFetch), бэкенд main.rs::upload_session_file + session_files.rs.
