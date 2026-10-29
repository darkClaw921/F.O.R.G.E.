# tmux-web/src/analytics/scan.rs

Индексатор транскриптов Claude Code: обход ~/.claude/projects/* и инкрементальный разбор файлов в SessionRollup. Слой между ядром разбора (parse/rollup, знает только поток байт) и хранилищем.

## Зачем
На диске пользователя сотни файлов и >1 ГБ транскриптов. Перечитывать это при каждом открытии вкладки нельзя ни по времени, ни по износу диска. Здесь живут ровно две вещи: обход и инкрементальность.

## Обход
* claude_projects_dir() -> Option<PathBuf> — $HOME/.claude/projects. None при пустом/незаданном HOME. Каталог ~/.codex НЕ читается: Codex в аналитике не поддерживается.
* SessionFile { path, key, project_dir, session_id, len, mtime_ms } — метаданные файла. key = строковый путь, он же ключ индекса и кэша; project_dir = имя каталога ~/.claude/projects/<dir>; session_id = имя файла без расширения.
* list_session_files(&Path) -> Vec<SessionFile> — только stat, содержимое НЕ читается. Всё, кроме *.jsonl (включая подкаталог memory/, которым занимается claude_memory.rs), игнорируется. Нечитаемые записи каталога пропускаются молча. Отсутствие projects_dir — не ошибка, пустой список. Результат отсортирован по key: ни прогресс, ни тесты не должны зависеть от порядка readdir.
* mtime_ms() — время модификации в unix-мс, для файлов до эпохи отрицательное (сравнение «mtime уехал назад» остаётся монотонным).

## Инкрементальность: ключ (len, mtime, parse_offset)
Транскрипт — append-only JSONL, поэтому судьба файла решается по трём числам без чтения содержимого. decide_action(prev: Option<&SessionRollup>, file: &SessionFile) -> FileAction:
* len и mtime те же → FileAction::Unchanged — файл вообще не открывается.
* len вырос, mtime не уехал назад → FileAction::Append — seek(parse_offset) и дочитывание хвоста.
* len уменьшился, ИЛИ mtime уехал назад, ИЛИ len тот же при новом mtime, ИЛИ сохранённый parse_offset > len → FileAction::Full, роллап выбрасывается.
* файла не было в индексе → Full. Файл исчез с диска → роллап просто не попадает в новый индекс.

Случай «тот же len, новый mtime» — перезапись «в размер»: формально не усадка и не рост, содержимое могло измениться где угодно, единственный безопасный ответ — полный пересбор. Случай parse_offset > len — рассинхрон индекса с диском: seek за конец файла молча дал бы пустой хвост и замороженные числа.

scan_file(file, prev, action) -> io::Result<FileScan> исполняет решение: Full — parse_session с нуля; Append — File::open + seek(parse_offset) + append_session в существующий роллап. FileScan.bytes_read — сколько байт содержимого реально прочитано (0 для Unchanged); по этому счётчику тесты и проверяют, что дочитывается хвост, а не файл целиком. После разбора проставляются file_len/file_mtime_ms и, если сессия не встретила ни одной записи с cwd, project_dir берётся с диска.

## Два инварианта, которые легко сломать
1. Ход, разрезанный границей дочитывания, считается ровно один раз: состояние автомата ходов живёт в самом роллапе (SessionRollup::turn_state), а parse_offset ядро оставляет на конце последней ПОЛНОЙ строки — половина записи, дописываемой прямо в момент скана, будет дочитана следующим проходом заново.
2. SessionRollup::close_open_turn() здесь НЕ вызывается: файл живой сессии может дорасти в любой момент, а закрытие хода необратимо испортило бы равенство «инкремент == полный пересбор».

## Параллелизм и прогресс
scan_incremental(projects_dir, prev: BTreeMap<key, SessionRollup>, progress: Arc<ScanProgress>) -> Option<ScanOutcome> — async-точка входа.
* Каждый файл разбирается в tokio::task::spawn_blocking (блокирующий I/O + CPU на serde_json в асинхронном раннере недопустим). Одновременность ограничена tokio::sync::Semaphore с числом пермитов parallelism() = available_parallelism (фолбэк 4). Пермит берётся ДО spawn_blocking, иначе в раннер разом улетели бы все задачи и семафор ничего бы не ограничивал.
* Паника внутри задачи не роняет процесс: JoinError учитывается как ошибка файла (errors += 1), скан продолжается. Ошибка отдельного файла (нет прав, исчез между stat и open) — то же самое; его роллап в индекс не попадает и на следующем проходе он будет разобран как новый.
* ScanProgress — атомарные счётчики total/done/errors/files_read/bytes_read/running, снимок через snapshot() -> ScanProgressSnapshot (Serialize, отдаётся из /api/analytics/status Фазы 3).
* Повторный скан во время идущего невозможен: try_begin() делает compare_exchange на running и возвращает ScanGuard; scan_incremental отдаёт None, если захват не удался. Флаг снимается на Drop гварда — то есть и при обычном возврате, и при панике, и при отмене задачи (иначе одна неудача навсегда заблокировала бы пересбор).
* ScanOutcome { files, index: AnalyticsIndex, errors, files_read, bytes_read, elapsed_ms } — новый набор роллапов и собранный запросопригодный индекс.

## Замер на живом архиве
498 файлов / 1406 МБ / 4.5 с в debug-сборке, errors=0 (ignore-тест measure_full_scan_of_live_archive в tmux-web/tests/analytics_cache.rs).

Зависимости: analytics::parse (parse_session/append_session), analytics::rollup (SessionRollup, AnalyticsIndex), tokio (spawn_blocking, Semaphore), tracing. Потребитель — analytics::cache и AnalyticsStore Фазы 3.

## Фаза 7 — чтение ~/.claude.json
`claude_config_path()` (`$HOME/.claude.json`), `read_global_usage(path) -> Option<GlobalUsage>` и `file_mtime_ms(path)`. `GlobalUsage` — узкий вид на конфиг: только `skillUsage`, `pluginUsage`, `toolUsage` (имя → `UsageEntry { usage_count, last_used_at }`); всё остальное (токены, идентификаторы, история проектов) игнорируется и в память не попадает. Отсутствие файла или битый JSON — `None` без ошибки: конфиг CLI не часть контракта devforge.

