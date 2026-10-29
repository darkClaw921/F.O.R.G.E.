//! Индексатор транскриптов: обход `~/.claude/projects/*` и инкрементальный
//! разбор файлов в [`SessionRollup`].
//!
//! ### Зачем отдельный слой
//!
//! Ядро разбора ([`super::parse`]) умеет читать один поток байт и ничего не
//! знает ни про файловую систему, ни про то, что этот же файл уже читали
//! вчера. На диске у пользователя ~1341 файл и ~2.7 ГБ транскриптов —
//! перечитывать это при каждом открытии вкладки нельзя ни по времени, ни по
//! износу диска. Поэтому здесь живут ровно две вещи: **обход** и
//! **инкрементальность**.
//!
//! ### Инкрементальность: ключ `(len, mtime, parse_offset)`
//!
//! Транскрипт Claude Code — append-only JSONL, поэтому решение о судьбе файла
//! принимается по трём числам, без чтения содержимого (см.
//! [`decide_action`]):
//!
//! | Наблюдение | Действие |
//! |---|---|
//! | `len` и `mtime` те же, что в индексе | [`FileAction::Unchanged`] — файл вообще не открывается |
//! | `len` вырос, `mtime` не уехал назад | [`FileAction::Append`] — `seek(parse_offset)` и дочитывание хвоста |
//! | `len` уменьшился, либо `mtime` уехал назад, либо `len` тот же при новом `mtime` | [`FileAction::Full`] — роллап выбрасывается, файл разбирается целиком |
//! | файла не было в индексе | [`FileAction::Full`] |
//! | файл исчез с диска | роллап просто не попадает в новый индекс |
//!
//! Последний случай «тот же `len`, но новый `mtime`» — это перезапись файла
//! ровно такой же длины. Формально это не «усох», но и не «дорос»: содержимое
//! могло измениться где угодно, поэтому единственный безопасный ответ —
//! полный пересбор.
//!
//! Ход, разрезанный границей дочитывания, считается ровно один раз: состояние
//! автомата ходов живёт в самом роллапе (`SessionRollup::turn_state`), а
//! `parse_offset` ядро оставляет на конце последней ПОЛНОЙ строки — половина
//! записи, дописываемой прямо в момент скана, будет дочитана следующим
//! проходом с нуля.
//!
//! Открытый ход здесь намеренно НЕ закрывается
//! (`SessionRollup::close_open_turn`): файл живой сессии может дорасти в любой
//! момент, а закрытие хода необратимо испортило бы равенство
//! «инкремент == полный пересбор».
//!
//! ### Параллелизм
//!
//! Разбор — блокирующий I/O плюс заметный CPU на `serde_json`, в асинхронном
//! раннере ему не место. Каждый файл уезжает в
//! [`tokio::task::spawn_blocking`], одновременность ограничена семафором с
//! числом пермитов по числу ядер. Паника внутри задачи не роняет процесс:
//! `JoinError` учитывается как ошибка файла, скан продолжается.
//!
//! Прогресс публикуется в [`ScanProgress`] атомарными счётчиками — их читает
//! `/api/analytics/status` (Фаза 3), пока скан идёт. Повторный запуск во
//! время идущего скана невозможен: [`scan_incremental`] возвращает `None`,
//! флаг `running` снимается на `Drop` даже при панике.

use std::collections::BTreeMap;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::parse::{append_session, parse_session};
use super::rollup::{AnalyticsIndex, SessionRollup};

/// Фолбэк параллелизма, когда `available_parallelism` недоступна.
const FALLBACK_PARALLELISM: usize = 4;

/// `~/.claude/projects` — корень транскриптов Claude Code.
///
/// Возвращает `None`, если `$HOME` не задан или пуст. Отсутствие самого
/// каталога ошибкой не считается: это просто пустой индекс.
///
/// Каталог `~/.codex` не читается намеренно — Codex в аналитике не
/// поддерживается (в коде проекта нулевая поддержка, на диске единицы
/// файлов).
pub fn claude_projects_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".claude").join("projects"))
}

// ---------------------------------------------------------------------------
// Обход
// ---------------------------------------------------------------------------

/// Метаданные одного файла-сессии, собранные обходом.
///
/// Содержимое файла на этом этапе НЕ читается — только `stat`: обход должен
/// быть дешёвым даже на 1341 файле, а чтение делает разбор.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    /// Абсолютный путь к `.jsonl`.
    pub path: PathBuf,
    /// Ключ файла в индексе и в кэше — строковое представление `path`.
    pub key: String,
    /// Имя каталога `~/.claude/projects/<dir>` (закодированный путь проекта).
    pub project_dir: String,
    /// Имя файла без расширения = `sessionId`.
    pub session_id: String,
    pub len: u64,
    /// Время модификации в unix-мс (может быть отрицательным до эпохи).
    pub mtime_ms: i64,
}

/// Время модификации файла в unix-мс.
fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    let Ok(modified) = meta.modified() else {
        return 0;
    };
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        // Файл «из прошлого» до 1970 — считаем отрицательное значение, чтобы
        // сравнение «mtime уехал назад» осталось монотонным.
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Собирает список всех `.jsonl`-сессий под `projects_dir`.
///
/// Устройство каталога: `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`.
/// Всё, что не `.jsonl` (включая подкаталог `memory/`, которым занимается
/// [`crate::claude_memory`]), игнорируется. Нечитаемые записи каталога
/// пропускаются молча — один битый симлинк не должен ронять весь индекс.
///
/// Отсутствие `projects_dir` — не ошибка: пустой список.
pub fn list_session_files(projects_dir: &Path) -> Vec<SessionFile> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return out;
    };
    for project in projects.flatten() {
        if !project.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let project_dir = project.file_name().to_string_lossy().into_owned();
        let Ok(entries) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Some(session_id) = path.file_stem().map(|s| s.to_string_lossy().into_owned())
            else {
                continue;
            };
            out.push(SessionFile {
                key: path.to_string_lossy().into_owned(),
                project_dir: project_dir.clone(),
                session_id,
                len: meta.len(),
                mtime_ms: mtime_ms(&meta),
                path,
            });
        }
    }
    // Детерминированный порядок: и прогресс, и тесты не должны зависеть от
    // порядка выдачи readdir.
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

// ---------------------------------------------------------------------------
// Инкрементальность
// ---------------------------------------------------------------------------

/// Что делать с файлом по результатам сравнения `(len, mtime)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Файл не изменился — берём роллап из индекса, файл не открываем.
    Unchanged,
    /// Файл дорос — дочитываем хвост с `parse_offset`.
    Append,
    /// Разбираем целиком (новый, усохший, переписанный или с уехавшим назад
    /// `mtime`).
    Full,
}

/// Решение о судьбе файла по трём числам, без чтения содержимого.
///
/// Полная таблица правил — в документации модуля. Дополнительная защита:
/// если сохранённый `parse_offset` больше текущей длины файла (индекс
/// рассинхронизировался с диском), делаем полный пересбор — `seek` за конец
/// файла молча дал бы пустой хвост и «замороженные» числа.
pub fn decide_action(prev: Option<&SessionRollup>, file: &SessionFile) -> FileAction {
    let Some(prev) = prev else {
        return FileAction::Full;
    };
    if file.mtime_ms < prev.file_mtime_ms {
        return FileAction::Full;
    }
    if file.len < prev.file_len || prev.parse_offset > file.len {
        return FileAction::Full;
    }
    if file.len == prev.file_len {
        // Та же длина и то же время — файла никто не касался. Та же длина при
        // новом времени — перезапись «в размер», содержимое доверия не
        // заслуживает.
        return if file.mtime_ms == prev.file_mtime_ms {
            FileAction::Unchanged
        } else {
            FileAction::Full
        };
    }
    FileAction::Append
}

/// Результат разбора одного файла.
#[derive(Debug, Clone)]
pub struct FileScan {
    pub rollup: SessionRollup,
    /// Сколько байт содержимого реально прочитано с диска (0 для
    /// [`FileAction::Unchanged`]).
    pub bytes_read: u64,
}

/// Разбирает один файл согласно `action`.
///
/// Для [`FileAction::Append`] открывает файл, делает `seek` на сохранённый
/// `parse_offset` и доливает хвост в существующий роллап — счётчики
/// складываются, итоги `cost-state` перезаписываются, состояние автомата
/// ходов продолжается с того места, где остановился прошлый проход.
///
/// # Errors
///
/// Ошибка открытия/чтения файла. Битые строки внутри файла ошибкой не
/// являются — их считает само ядро разбора (`skipped_lines`).
pub fn scan_file(
    file: &SessionFile,
    prev: Option<SessionRollup>,
    action: FileAction,
) -> std::io::Result<FileScan> {
    let (mut rollup, bytes_read) = match action {
        FileAction::Unchanged => {
            let rollup = prev.expect("Unchanged без предыдущего роллапа невозможен");
            return Ok(FileScan {
                rollup,
                bytes_read: 0,
            });
        }
        FileAction::Full => {
            let handle = std::fs::File::open(&file.path)?;
            let rollup = parse_session(BufReader::new(handle), &file.session_id, 0)?;
            let read = rollup.parse_offset;
            (rollup, read)
        }
        FileAction::Append => {
            let mut rollup = prev.expect("Append без предыдущего роллапа невозможен");
            let start = rollup.parse_offset;
            let mut handle = std::fs::File::open(&file.path)?;
            handle.seek(SeekFrom::Start(start))?;
            append_session(&mut rollup, BufReader::new(handle))?;
            let read = rollup.parse_offset.saturating_sub(start);
            (rollup, read)
        }
    };

    rollup.file_len = file.len;
    rollup.file_mtime_ms = file.mtime_ms;
    // Сессия без единой записи с `cwd` (например, файл из одних `attachment`)
    // всё равно должна попасть в свой проект — берём имя каталога с диска.
    if rollup.project_dir.is_none() {
        rollup.project_dir = Some(file.project_dir.clone());
    }
    Ok(FileScan { rollup, bytes_read })
}

// ---------------------------------------------------------------------------
// Прогресс
// ---------------------------------------------------------------------------

/// Атомарные счётчики идущего скана.
///
/// Живут в `Arc` рядом с хранилищем индекса: скан пишет их из
/// blocking-задач, а хендлер `/api/analytics/status` читает `snapshot()` в
/// любой момент, не блокируя ни того, ни другого.
#[derive(Debug, Default)]
pub struct ScanProgress {
    /// Всего файлов к обработке в текущем скане.
    pub total: AtomicUsize,
    /// Обработано файлов (включая пропущенные без изменений и ошибочные).
    pub done: AtomicUsize,
    /// Файлов, на которых разбор упал (нет прав, I/O-ошибка, паника задачи).
    pub errors: AtomicUsize,
    /// Файлов, реально открытых на чтение содержимого.
    pub files_read: AtomicUsize,
    /// Прочитано байт содержимого — по этому счётчику тесты и проверяют, что
    /// дочитывается хвост, а не файл целиком.
    pub bytes_read: AtomicU64,
    /// Флаг «скан идёт»: второй скан не стартует, пока не снят.
    pub running: AtomicBool,
}

/// Снимок счётчиков для отдачи наружу.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanProgressSnapshot {
    pub total: usize,
    pub done: usize,
    pub errors: usize,
    pub files_read: usize,
    pub bytes_read: u64,
    pub running: bool,
}

impl ScanProgress {
    pub fn snapshot(&self) -> ScanProgressSnapshot {
        ScanProgressSnapshot {
            total: self.total.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            files_read: self.files_read.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Acquire),
        }
    }

    /// Обнуляет счётчики перед новым сканом (флаг `running` не трогает — им
    /// владеет [`ScanGuard`]).
    fn reset(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.errors.store(0, Ordering::Relaxed);
        self.files_read.store(0, Ordering::Relaxed);
        self.bytes_read.store(0, Ordering::Relaxed);
    }
}

/// RAII-владелец флага `running`.
///
/// Флаг снимается на `Drop` — то есть и при обычном возврате, и при панике,
/// и при отмене задачи. Иначе одна неудача навсегда заблокировала бы
/// возможность пересобрать индекс.
pub struct ScanGuard(Arc<ScanProgress>);

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

/// Пытается захватить право на скан. `None` — скан уже идёт.
pub fn try_begin(progress: &Arc<ScanProgress>) -> Option<ScanGuard> {
    progress
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| ScanGuard(Arc::clone(progress)))
}

// ---------------------------------------------------------------------------
// Скан
// ---------------------------------------------------------------------------

/// Итог одного прохода индексатора.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// Роллапы по ключу-пути. Исчезнувшие с диска файлы сюда не попадают.
    pub files: BTreeMap<String, SessionRollup>,
    /// Сводный индекс, готовый для запросов.
    pub index: AnalyticsIndex,
    pub errors: usize,
    pub files_read: usize,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
}

/// Число одновременно разбираемых файлов.
fn parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(FALLBACK_PARALLELISM)
        .max(1)
}

/// Инкрементальный скан каталога транскриптов.
///
/// `prev` — роллапы из кэша (или из памяти) по ключу-пути; на выходе — новый
/// набор роллапов и собранный [`AnalyticsIndex`].
///
/// Возвращает `None`, если скан уже идёт: повторный запрос `rescan` во время
/// скана не должен порождать второй проход по 2.7 ГБ.
///
/// Ошибка на отдельном файле (нет прав, исчез между `stat` и `open`, паника
/// в blocking-задаче) не отменяет скан: файл пропускается, `errors`
/// увеличивается. Роллап такого файла в индекс не попадает — на следующем
/// проходе он будет разобран заново как новый.
pub async fn scan_incremental(
    projects_dir: PathBuf,
    prev: BTreeMap<String, SessionRollup>,
    progress: Arc<ScanProgress>,
) -> Option<ScanOutcome> {
    let _guard = try_begin(&progress)?;
    let started = std::time::Instant::now();

    let listing = {
        let dir = projects_dir.clone();
        tokio::task::spawn_blocking(move || list_session_files(&dir))
            .await
            .unwrap_or_default()
    };
    progress.reset(listing.len());

    let mut prev = prev;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(parallelism()));
    let mut tasks = Vec::with_capacity(listing.len());

    for file in listing {
        let action = decide_action(prev.get(&file.key), &file);
        let carried = match action {
            FileAction::Full => None,
            _ => prev.remove(&file.key),
        };
        let progress = Arc::clone(&progress);
        let semaphore = Arc::clone(&semaphore);
        // Пермит берётся ДО spawn_blocking: иначе в раннер разом улетели бы
        // все 1341 задача и семафор ограничивал бы уже ничего.
        let Ok(permit) = semaphore.acquire_owned().await else {
            break;
        };
        tasks.push(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let result = scan_file(&file, carried, action);
            progress.done.fetch_add(1, Ordering::Relaxed);
            if let Ok(scan) = &result {
                if scan.bytes_read > 0 {
                    progress.files_read.fetch_add(1, Ordering::Relaxed);
                    progress
                        .bytes_read
                        .fetch_add(scan.bytes_read, Ordering::Relaxed);
                }
            }
            (file.key, result)
        }));
    }

    let mut files: BTreeMap<String, SessionRollup> = BTreeMap::new();
    for task in tasks {
        match task.await {
            Ok((key, Ok(scan))) => {
                files.insert(key, scan.rollup);
            }
            Ok((key, Err(e))) => {
                progress.errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(file = %key, error = %e, "analytics: файл пропущен");
            }
            // Паника внутри blocking-задачи: считаем ошибкой файла, процесс
            // живёт дальше.
            Err(e) => {
                progress.errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(error = %e, "analytics: задача разбора файла упала");
            }
        }
    }

    let mut index = AnalyticsIndex::default();
    for rollup in files.values() {
        index.add_session(rollup);
    }

    let snapshot = progress.snapshot();
    Some(ScanOutcome {
        files,
        index,
        errors: snapshot.errors,
        files_read: snapshot.files_read,
        bytes_read: snapshot.bytes_read,
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    })
}

// ---------------------------------------------------------------------------
// Глобальные счётчики CLI из ~/.claude.json
// ---------------------------------------------------------------------------

/// `~/.claude.json` — конфиг Claude Code с глобальными счётчиками
/// использования навыков, плагинов и инструментов.
///
/// `None`, если `$HOME` не задан.
pub fn claude_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".claude.json"))
}

/// Счётчик одного элемента из `~/.claude.json`.
///
/// Все поля опциональны: формат конфига меняется от версии к версии CLI, и
/// строгая схема ломалась бы на каждом обновлении.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageEntry {
    pub usage_count: u64,
    pub last_used_at: i64,
}

/// Узкий вид на `~/.claude.json`: только три карты счётчиков.
///
/// Файл большой (сотни килобайт) и полон приватных полей — токенов,
/// идентификаторов, истории проектов. Сюда тянутся ровно три карты, всё
/// остальное игнорируется и в память не попадает.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalUsage {
    /// Навыки: имя → счётчик.
    pub skill_usage: BTreeMap<String, UsageEntry>,
    /// Плагины: `имя@маркетплейс` → счётчик.
    pub plugin_usage: BTreeMap<String, UsageEntry>,
    /// Инструменты: имя → счётчик (глобальный, за всё время).
    pub tool_usage: BTreeMap<String, UsageEntry>,
}

/// Читает и разбирает `~/.claude.json`.
///
/// Возвращает `None`, если файла нет, он не читается или JSON битый —
/// плитки просто не показываются. Ошибкой это не считается: конфиг CLI не
/// является частью контракта devforge, и его отсутствие не должно ронять ни
/// API, ни дашборд.
pub fn read_global_usage(path: &Path) -> Option<GlobalUsage> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<GlobalUsage>(&bytes) {
        Ok(u) => Some(u),
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "analytics: ~/.claude.json не разобран, справочные плитки не показываются"
            );
            None
        }
    }
}

/// Время модификации файла в unix-мс; `0`, если файла нет.
pub fn file_mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path).map(|m| mtime_ms(&m)).unwrap_or(0)
}
