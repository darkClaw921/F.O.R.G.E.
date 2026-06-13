//! Источник данных о git-коммитах для гант-диаграммы вкладки Tasks.
//!
//! ### Назначение
//!
//! Read-only обёртка над `git log`, отдающая список коммитов git-корня
//! текущей сессии. Используется хендлером `GET /api/git/commits`
//! (`main.rs::get_git_commits`), который сериализует результат в JSON
//! `{"commits": [...]}`. Гант рисует коммиты как вертикальные черты на
//! временной оси рядом с полосами задач.
//!
//! ### Почему через CLI, а не git-крейт
//!
//! Парсинг репозитория через `git2`/`gix` потребовал бы тяжёлую нативную
//! зависимость и заметно больше кода. Нам же нужен лишь плоский список
//! коммитов в порядке `git log` (новые сверху). Поэтому модуль идёт самым
//! простым путём: spawn'ит `git log` как subprocess через
//! `tokio::process::Command` (не блокируя runtime — паттерн как
//! [`crate::tasks::list_tasks`]).
//!
//! ### Формат вывода git
//!
//! Используется `--pretty=format:%H%x1f%ct%x1f%an%x1f%s`, где `%x1f` —
//! ASCII Unit Separator (0x1F). Этот байт не встречается в нормальном
//! тексте subject/author, поэтому безопасен как разделитель полей даже
//! если в сообщении коммита есть табы, пробелы или Unicode.
//!
//! ### Отказы (graceful)
//!
//! В отличие от `tasks::list_tasks`, ошибки здесь НЕ всплывают как `Err`:
//! не-git каталог, ненулевой код возврата `git` или невозможность
//! заспавнить процесс → `Ok(vec![])`. Это сознательное решение: гант — это
//! опциональное украшение, и при отсутствии git он должен просто показать
//! задачи без коммитов, а не ронять весь эндпоинт.

use std::path::Path;

use serde::Serialize;
use tokio::process::Command;

/// Один git-коммит в формате, понятном фронтенду гант-диаграммы.
///
/// Сериализуется в JSON-объект `{"hash","ts","subject","author"}`.
///
/// # Поля
/// - `hash` — полный SHA-1 коммита (`%H`).
/// - `ts` — committer date как Unix timestamp в секундах (`%ct`).
///   Именно committer date, а не author date, чтобы порядок коммитов на
///   оси совпадал с порядком в истории после rebase/cherry-pick.
/// - `subject` — первая строка сообщения коммита (`%s`).
/// - `author` — имя автора (`%an`).
#[derive(Debug, Clone, Serialize)]
pub struct Commit {
    pub hash: String,
    pub ts: i64,
    pub subject: String,
    pub author: String,
}

/// Одно изменение файла в коммите для попапа деталей гант-диаграммы.
///
/// Сериализуется в JSON-объект `{"status","path"}`.
///
/// # Поля
/// - `status` — статус изменения из `git show --name-status`: `A` (added),
///   `M` (modified), `D` (deleted), `R<nnn>` (renamed, например `R100`),
///   `C<nnn>` (copied), `T` (type change) и т.п. Хранится как есть, без
///   нормализации — фронтенд решает, как отрисовать.
/// - `path` — путь файла. Для `R`/`C` (переименование/копирование) git
///   выводит две колонки `old<TAB>new`; в `path` кладётся НОВЫЙ путь
///   (последняя колонка), т.к. именно он отражает текущее состояние дерева.
#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub status: String,
    pub path: String,
}

/// Полные детали одного git-коммита для hover-попапа гант-диаграммы.
///
/// В отличие от лёгкого [`Commit`] (только мета для оси времени),
/// `CommitDetail` дополнительно несёт тело сообщения (`body`) и список
/// изменённых файлов (`files`). Запрашивается лениво по hash через
/// `GET /api/git/commit` (`main.rs::get_git_commit`).
///
/// Сериализуется в JSON `{"hash","ts","subject","body","author","files"}`.
///
/// # Поля
/// - `hash` — полный SHA коммита (`%H`).
/// - `ts` — committer date как Unix timestamp в секундах (`%ct`).
/// - `subject` — первая строка сообщения коммита (`%s`).
/// - `body` — тело сообщения коммита (`%b`), может быть многострочным или
///   пустым.
/// - `author` — имя автора (`%an`).
/// - `files` — список изменённых файлов (см. [`FileChange`]).
#[derive(Debug, Clone, Serialize)]
pub struct CommitDetail {
    pub hash: String,
    pub ts: i64,
    pub subject: String,
    pub body: String,
    pub author: String,
    pub files: Vec<FileChange>,
}

/// ASCII Unit Separator (0x1F) — разделитель полей в выводе `git log`.
const FIELD_SEP: char = '\u{1f}';

/// Возвращает список коммитов git-корня, в котором лежит `cwd`.
///
/// Корень определяется через [`crate::paths::resolve_root`] (поднимается по
/// `ancestors()` до первой папки с маркером `.beads/`/`.git/`), и `git log`
/// запускается с `current_dir(root)`. Это согласовано с тем, как
/// `tasks::list_tasks` находит `.beads/`.
///
/// # Параметры
/// - `cwd` — рабочая директория сессии (или значение `?path=`). Корень
///   репозитория ищется вверх от неё.
/// - `since_unix` — нижняя граница по committer date в секундах Unix.
///   `Some(v)` → добавляется флаг `--since=<v>` (только коммиты не старше
///   `v`); `None` → без `--since`, отдаются все коммиты (но не более
///   `--max-count=2000`).
/// - `until_unix` — верхняя граница по committer date в секундах Unix.
///   `Some(v)` → добавляется флаг `--until=<v>` (только коммиты не позже
///   `v`); `None` → без `--until`, верхняя граница не ограничивается. Флаги
///   `--since`/`--until` у `git log` независимы: `--since=X --until=Y`
///   отдаёт коммиты в диапазоне `[X, Y]`. Используется для диапазонов
///   «Сегодня»/«Вчера» гант-диаграммы.
///
/// # Возврат
/// `Ok(Vec<Commit>)` в порядке вывода `git log` (новые сверху). При любой
/// проблеме (не-git каталог, ненулевой exit `git`, ошибка spawn) —
/// `Ok(vec![])`, НЕ `Err` (graceful degradation для ганта).
///
/// # Пример
/// ```ignore
/// let commits = git::list_commits(Path::new("/proj/src"), Some(1_700_000_000), None).await?;
/// // commits[0] — самый свежий коммит за период
/// ```
pub async fn list_commits(
    cwd: &Path,
    since_unix: Option<i64>,
    until_unix: Option<i64>,
) -> anyhow::Result<Vec<Commit>> {
    let root = crate::paths::resolve_root(cwd);

    let mut args: Vec<String> = vec![
        "log".to_string(),
        "--max-count=2000".to_string(),
        "--pretty=format:%H%x1f%ct%x1f%an%x1f%s".to_string(),
    ];
    // `@<unix>` явно говорит git, что это unix-timestamp; голое число попадает
    // в approxidate-эвристику и может быть истолковано иначе. Унифицировано с
    // echo_host.rs (там тоже `@{since}`).
    if let Some(v) = since_unix {
        args.push(format!("--since=@{v}"));
    }
    if let Some(v) = until_unix {
        args.push(format!("--until=@{v}"));
    }

    let output = match Command::new("git")
        .args(&args)
        .current_dir(&root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            // git не найден в PATH / иная ошибка spawn — гант просто без коммитов.
            tracing::debug!(error = ?e, root = ?root, "git log spawn failed — returning empty commits");
            return Ok(Vec::new());
        }
    };

    if !output.status.success() {
        // Не-git каталог или другая ошибка git — graceful empty.
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::debug!(
            exit = ?output.status.code(),
            root = ?root,
            stderr = %stderr.trim(),
            "git log non-zero exit — returning empty commits"
        );
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_log(&stdout))
}

/// Парсит stdout `git log --pretty=format:%H%x1f%ct%x1f%an%x1f%s` в
/// `Vec<Commit>`.
///
/// Каждая непустая строка разбивается по [`FIELD_SEP`] на 4 поля
/// `hash, ct, author, subject`. Строки с числом полей < 4 или с
/// непарсимым `ct` пропускаются (битый/неожиданный вывод не должен ронять
/// эндпоинт). Subject склеивается обратно из остатка на случай, если в
/// сообщении вдруг окажется байт 0x1F (крайне маловероятно, но безопасно).
fn parse_log(stdout: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        // splitn(4): hash, ct, author, остаток = subject (subject может
        // теоретически содержать 0x1F, поэтому не дробим его дальше).
        let mut parts = line.splitn(4, FIELD_SEP);
        let (Some(hash), Some(ct), Some(author), Some(subject)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(ts) = ct.parse::<i64>() else {
            continue;
        };
        commits.push(Commit {
            hash: hash.to_string(),
            ts,
            subject: subject.to_string(),
            author: author.to_string(),
        });
    }
    commits
}

/// Проверяет, что `h` — безопасный git-hash (или его сокращённый префикс).
///
/// Возвращает `true` только если длина `h` в диапазоне `4..=64` И все
/// символы — шестнадцатеричные (`0-9`, `a-f`, `A-F`). Иначе `false`.
///
/// Это критичная защита от инъекции аргументов: `hash` приходит из
/// query-параметра `?hash=` и подставляется в командную строку `git show`.
/// Без валидации строка вида `"--output=/etc/passwd"` или `"; rm -rf /"`
/// могла бы быть истолкована git как флаг/аргумент. Hex-only + ограничение
/// длины гарантируют, что значение не может быть ничем, кроме SHA (или его
/// префикса): диапазон покрывает короткие хеши (от 4 символов) и полные
/// SHA-1 (40) / SHA-256 (64).
pub fn is_valid_hash(h: &str) -> bool {
    let len = h.len();
    (4..=64).contains(&len) && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Возвращает полные детали коммита `hash` (мета + тело + изменённые файлы)
/// или `None`, если коммит недоступен.
///
/// # Безопасность
/// `hash` приходит из недоверенного query-параметра, поэтому ПЕРЕД любым
/// вызовом git проходит через [`is_valid_hash`]. Если он невалиден
/// (не hex / не та длина) — сразу `Ok(None)`, git НЕ запускается. Это
/// исключает инъекцию аргументов в `git show`.
///
/// # Реализация (двухвызовный вариант)
/// Для надёжного парсинга используются два независимых вызова `git show` в
/// `current_dir(resolve_root(cwd))`:
/// 1. `git show --no-patch --format=%H%x1f%ct%x1f%an%x1f%s%x1f%b <hash>` —
///    отдаёт ровно одну строку-запись с метаданными (hash/ts/author/subject)
///    и телом (`%b`, может быть многострочным). Парсится [`parse_meta`].
/// 2. `git show --name-status --format= <hash>` — пустой `--format=` гасит
///    шапку, остаётся только список изменённых файлов в виде строк
///    `status<TAB>path` (для `R`/`C` — `status<TAB>old<TAB>new`). Парсится
///    [`parse_name_status`].
///
/// Разделение на два вызова делает парсинг тривиально надёжным: блок файлов
/// и многострочный body никогда не смешиваются в одном потоке, поэтому не
/// нужна эвристика «где кончается body и начинаются файлы».
///
/// # Параметры
/// - `cwd` — рабочая директория сессии; git-корень ищется вверх через
///   [`crate::paths::resolve_root`] (как в [`list_commits`]).
/// - `hash` — SHA коммита (полный или сокращённый префикс).
///
/// # Возврат
/// - `Ok(Some(CommitDetail))` — коммит найден и распарсен.
/// - `Ok(None)` — невалидный hash (без spawn), не-git каталог, ненулевой
///   exit `git`, коммит не найден, ошибка spawn или пустой вывод меты.
///   Никогда не паникует и не всплывает как `Err` (graceful, как у ганта).
pub async fn commit_detail(cwd: &Path, hash: &str) -> anyhow::Result<Option<CommitDetail>> {
    // Защита от инъекции аргументов git — до любого spawn.
    if !is_valid_hash(hash) {
        tracing::debug!(hash = %hash, "commit_detail: invalid hash — returning None");
        return Ok(None);
    }

    let root = crate::paths::resolve_root(cwd);

    // (1) Мета + body одной записью.
    let meta_out = match Command::new("git")
        .args([
            "show",
            "--no-patch",
            "--format=%H%x1f%ct%x1f%an%x1f%s%x1f%b",
            hash,
        ])
        .current_dir(&root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!(error = ?e, root = ?root, "git show (meta) spawn failed — returning None");
            return Ok(None);
        }
    };
    if !meta_out.status.success() {
        let stderr = String::from_utf8_lossy(&meta_out.stderr);
        tracing::debug!(
            exit = ?meta_out.status.code(),
            root = ?root,
            stderr = %stderr.trim(),
            "git show (meta) non-zero exit — returning None"
        );
        return Ok(None);
    }
    let meta_stdout = String::from_utf8_lossy(&meta_out.stdout);
    let Some((hash_s, ts, author, subject, body)) = parse_meta(&meta_stdout) else {
        tracing::debug!(root = ?root, "git show (meta) empty/unparsable — returning None");
        return Ok(None);
    };

    // (2) Изменённые файлы (пустой --format= → только name-status строки).
    let files_out = match Command::new("git")
        .args(["show", "--name-status", "--format=", hash])
        .current_dir(&root)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!(error = ?e, root = ?root, "git show (files) spawn failed — returning None");
            return Ok(None);
        }
    };
    // Ненулевой exit на втором вызове трактуем как «нет файлов», а не отказ:
    // мета уже получена, попап без списка файлов всё ещё полезен.
    let files = if files_out.status.success() {
        let files_stdout = String::from_utf8_lossy(&files_out.stdout);
        parse_name_status(&files_stdout)
    } else {
        Vec::new()
    };

    Ok(Some(CommitDetail {
        hash: hash_s,
        ts,
        subject,
        body,
        author,
        files,
    }))
}

/// Парсит stdout `git show --no-patch --format=%H%x1f%ct%x1f%an%x1f%s%x1f%b`
/// в кортеж `(hash, ts, author, subject, body)`.
///
/// Запись — это одна логическая строка из 5 полей, разделённых
/// [`FIELD_SEP`]. Тело (`%b`) идёт последним полем и может содержать
/// переводы строк, поэтому stdout НЕ разбивается по `\n`: первые четыре
/// поля отделяются по `FIELD_SEP`, а остаток (всё после 4-го разделителя,
/// включая возможные переводы строк) считается телом. Хвостовой `\n`,
/// который git добавляет после записи, обрезается.
///
/// Возвращает `None` для пустого/битого вывода (меньше 5 полей или
/// непарсимый `ct`), чтобы [`commit_detail`] мог отдать graceful `Ok(None)`.
fn parse_meta(stdout: &str) -> Option<(String, i64, String, String, String)> {
    let trimmed = stdout.strip_suffix('\n').unwrap_or(stdout);
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.splitn(5, FIELD_SEP);
    let hash = parts.next()?;
    let ct = parts.next()?;
    let author = parts.next()?;
    let subject = parts.next()?;
    // body — остаток (может быть многострочным или пустым).
    let body = parts.next().unwrap_or("");
    let ts = ct.parse::<i64>().ok()?;
    Some((
        hash.to_string(),
        ts,
        author.to_string(),
        subject.to_string(),
        body.to_string(),
    ))
}

/// Парсит stdout `git show --name-status --format=` в `Vec<FileChange>`.
///
/// Каждая непустая строка имеет вид `status<TAB>path` (например `M\tsrc/x.rs`)
/// или, для переименований/копий, `status<TAB>old<TAB>new` (`R100\ta\tb`).
/// Поля разбиваются по табу: первое — `status`, последнее — `path` (для `R`/`C`
/// это новый путь, который отражает текущее состояние дерева). Строки без
/// табуляции (пустой `--format=` может оставить ведущий пустой перевод
/// строки) пропускаются.
fn parse_name_status(stdout: &str) -> Vec<FileChange> {
    let mut files = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let Some(status) = cols.next() else {
            continue;
        };
        if status.is_empty() {
            continue;
        }
        // Последняя колонка — актуальный путь (новый для R/C).
        let Some(path) = cols.last() else {
            // Строка без таба (только статус) — нечего показать, пропускаем.
            continue;
        };
        if path.is_empty() {
            continue;
        }
        files.push(FileChange {
            status: status.to_string(),
            path: path.to_string(),
        });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_lines() {
        let stdout = "abc123\u{1f}1700000000\u{1f}Jane Doe\u{1f}Fix the bug\n\
                      def456\u{1f}1699999999\u{1f}John\u{1f}Add feature";
        let commits = parse_log(stdout);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abc123");
        assert_eq!(commits[0].ts, 1_700_000_000);
        assert_eq!(commits[0].author, "Jane Doe");
        assert_eq!(commits[0].subject, "Fix the bug");
        assert_eq!(commits[1].hash, "def456");
        assert_eq!(commits[1].ts, 1_699_999_999);
        assert_eq!(commits[1].subject, "Add feature");
    }

    #[test]
    fn skips_empty_and_malformed_lines() {
        let stdout = "\n\
                      onlyhash\n\
                      hash\u{1f}notanumber\u{1f}auth\u{1f}subj\n\
                      good\u{1f}123\u{1f}auth\u{1f}subj\n";
        let commits = parse_log(stdout);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].hash, "good");
        assert_eq!(commits[0].ts, 123);
    }

    #[test]
    fn subject_with_separators_or_tabs_preserved() {
        // tab внутри subject не ломает парсинг (разделитель — 0x1F, не whitespace).
        let stdout = "h\u{1f}10\u{1f}A\u{1f}msg\twith\ttabs and spaces";
        let commits = parse_log(stdout);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "msg\twith\ttabs and spaces");
    }

    #[test]
    fn empty_stdout_yields_empty_vec() {
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn commit_serializes_with_expected_fields() {
        let c = Commit {
            hash: "abc".into(),
            ts: 42,
            subject: "s".into(),
            author: "a".into(),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["hash"], "abc");
        assert_eq!(v["ts"], 42);
        assert_eq!(v["subject"], "s");
        assert_eq!(v["author"], "a");
    }

    #[test]
    fn is_valid_hash_accepts_real_shas_and_short_prefix() {
        // Сокращённый префикс (минимум 4 символа).
        assert!(is_valid_hash("abc1"));
        // Полный SHA-1 (40 hex).
        assert!(is_valid_hash("0123456789abcdef0123456789abcdef01234567"));
        // Полный SHA-256 (64 hex), верхняя граница диапазона.
        assert!(is_valid_hash(&"a".repeat(64)));
        // Смешанный регистр hex.
        assert!(is_valid_hash("ABCdef12"));
    }

    #[test]
    fn is_valid_hash_rejects_injection_and_bad_input() {
        // Пустая строка.
        assert!(!is_valid_hash(""));
        // Слишком короткая (< 4).
        assert!(!is_valid_hash("abc"));
        // Попытка инъекции аргументов.
        assert!(!is_valid_hash("; rm -rf /"));
        // Путь.
        assert!(!is_valid_hash("../etc"));
        // Не-hex буквы.
        assert!(!is_valid_hash("zzz"));
        assert!(!is_valid_hash("xyz123"));
        // Длиннее 64.
        assert!(!is_valid_hash(&"a".repeat(65)));
        // Пробелы / спецсимволы.
        assert!(!is_valid_hash("abc def"));
        assert!(!is_valid_hash("--output"));
    }

    #[test]
    fn parse_meta_parses_single_line_body() {
        let stdout = "deadbeef\u{1f}1700000000\u{1f}Jane Doe\u{1f}Fix bug\u{1f}Some body text\n";
        let (hash, ts, author, subject, body) = parse_meta(stdout).unwrap();
        assert_eq!(hash, "deadbeef");
        assert_eq!(ts, 1_700_000_000);
        assert_eq!(author, "Jane Doe");
        assert_eq!(subject, "Fix bug");
        assert_eq!(body, "Some body text");
    }

    #[test]
    fn parse_meta_preserves_multiline_body() {
        let stdout =
            "h1\u{1f}10\u{1f}A\u{1f}subj\u{1f}line one\nline two\n\nline four\n";
        let (_, _, _, subject, body) = parse_meta(stdout).unwrap();
        assert_eq!(subject, "subj");
        // Многострочное тело сохраняется целиком (хвостовой \n обрезан).
        assert_eq!(body, "line one\nline two\n\nline four");
    }

    #[test]
    fn parse_meta_handles_empty_body() {
        let stdout = "h1\u{1f}10\u{1f}A\u{1f}subj\u{1f}\n";
        let (_, _, _, subject, body) = parse_meta(stdout).unwrap();
        assert_eq!(subject, "subj");
        assert_eq!(body, "");
    }

    #[test]
    fn parse_meta_rejects_empty_and_malformed() {
        // Пустой stdout.
        assert!(parse_meta("").is_none());
        assert!(parse_meta("\n").is_none());
        // Меньше 5 полей.
        assert!(parse_meta("h\u{1f}10\u{1f}A").is_none());
        // Непарсимый ct.
        assert!(parse_meta("h\u{1f}notnum\u{1f}A\u{1f}s\u{1f}b").is_none());
    }

    #[test]
    fn parse_name_status_parses_add_modify_delete() {
        let stdout = "A\tsrc/new.rs\nM\tsrc/main.rs\nD\tsrc/old.rs\n";
        let files = parse_name_status(stdout);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].status, "A");
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[1].status, "M");
        assert_eq!(files[1].path, "src/main.rs");
        assert_eq!(files[2].status, "D");
        assert_eq!(files[2].path, "src/old.rs");
    }

    #[test]
    fn parse_name_status_takes_new_path_for_rename() {
        // Переименование: R100<TAB>old<TAB>new → path = new (последняя колонка).
        let stdout = "R100\tsrc/old_name.rs\tsrc/new_name.rs\n";
        let files = parse_name_status(stdout);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, "R100");
        assert_eq!(files[0].path, "src/new_name.rs");
    }

    #[test]
    fn parse_name_status_skips_blank_and_format_padding() {
        // Пустой --format= может оставить ведущий пустой перевод строки.
        let stdout = "\nM\tsrc/x.rs\n\n";
        let files = parse_name_status(stdout);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/x.rs");
        // Полностью пустой ввод → пустой список (запись без файлов).
        assert!(parse_name_status("").is_empty());
    }

    #[test]
    fn file_change_serializes_with_expected_fields() {
        let f = FileChange {
            status: "M".into(),
            path: "src/main.rs".into(),
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["status"], "M");
        assert_eq!(v["path"], "src/main.rs");
    }

    #[test]
    fn commit_detail_serializes_with_expected_fields() {
        let d = CommitDetail {
            hash: "abcd".into(),
            ts: 100,
            subject: "subj".into(),
            body: "body".into(),
            author: "auth".into(),
            files: vec![FileChange {
                status: "A".into(),
                path: "f.rs".into(),
            }],
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["hash"], "abcd");
        assert_eq!(v["ts"], 100);
        assert_eq!(v["subject"], "subj");
        assert_eq!(v["body"], "body");
        assert_eq!(v["author"], "auth");
        assert_eq!(v["files"][0]["status"], "A");
        assert_eq!(v["files"][0]["path"], "f.rs");
    }
}
