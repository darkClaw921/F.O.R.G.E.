# tmux-web/src/claude_memory.rs

Модуль чтения И РЕДАКТИРОВАНИЯ памяти Claude Code (~/.claude/projects/<encoded-cwd>/memory/) для произвольного каталога проекта.

Назначение: питает кнопку 🧠 в правом верхнем углу шапки сессии tmux (#tab-bar → .claude-memory-btn), которая открывает модалку с текстом персистентной памяти Claude Code (MEMORY.md + все связанные *.md файлы) для проекта, к которому привязана cwd текущей tmux-сессии — с возможностью правки прямо в модалке.

Структуры:
- ProjectMemory { dir, exists, index (сырой MEMORY.md, может быть пустым), files: Vec<MemoryFile> } — files содержат ОРИГИНАЛЬНОЕ (не склеенное) содержимое каждого файла, пригодное для обратной записи.
- MemoryFile { name, content }.

Ключевые функции:
- encode_project_dir(path) — кодирует путь как Claude Code: не-алфанумерик → '-'.
- normalize_path(path) — убирает trailing slash (components().collect()). БАГФИКС 2026-07-15: SessionInfo::path реально отдаёт cwd с trailing slash, что ломало и кодирование, и paths::resolve_root. Нормализация применяется один раз в resolve_memory_dir.
- resolve_memory_dir(cwd) -> Option<PathBuf> — общий для чтения И записи резолвинг папки памяти (сам cwd → paths::resolve_root(cwd) → ancestors до $HOME), гарантирует что save попадает в ту же папку, откуда читали.
- expected_memory_dir(cwd) — путь, который ПОКАЗАЛСЯ бы в UI, даже если папки нет на диске (для exists=false ответа).
- load_project_memory(cwd) -> ProjectMemory — не паникует, не возвращает Err.
- is_valid_memory_filename(name) — защита от path traversal: запрещены '/', '\\', '..'; обязателен суффикс '.md'.
- save_project_memory_file(cwd, file, content) -> Result<(), SaveError> — перезаписывает ОДИН файл в уже существующей папке памяти (НЕ создаёт новую папку — SaveError::MemoryDirNotFound, если папки для cwd ещё нет).
- SaveError: InvalidFileName | MemoryDirNotFound | Io(io::Error).

Хендлеры в main.rs:
- GET /api/claude-memory?path=<cwd> — Json(ProjectMemory). Remote (?server=) не проксируется.
- PUT /api/claude-memory {path?, file, content} — save_project_memory_file; 204 success, 400 InvalidFileName, 404 MemoryDirNotFound, 500 Io.

Фронтенд: static/js/sessions/claude-memory.js — каждая секция (MEMORY.md + каждый файл) рендерится отдельно (buildMemorySection) с кнопкой ✎, которая переключает view↔textarea и PUT'ит изменения; после успешного save вся модалка перерисовывается заново (renderMemory) — проще и надёжнее точечного патча DOM.

Тесты: load_and_save_project_memory_cases (единственный HOME-мутирующий тест — все под-кейсы последовательно, включая regression на trailing slash и на успешный/заблокированный save), filename_validation_blocks_traversal, normalize_strips_trailing_slash, encodes_non_alnum_as_dash.
