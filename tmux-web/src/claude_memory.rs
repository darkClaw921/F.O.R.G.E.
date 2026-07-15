//! Чтение и редактирование памяти Claude Code
//! (`~/.claude/projects/<encoded-cwd>/memory/`) для произвольного каталога
//! проекта.
//!
//! ### Назначение
//!
//! Claude Code (агент, которым управляется этот же репозиторий) хранит
//! межсессионную память в `~/.claude/projects/<encoded>/memory/`, где
//! `<encoded>` — абсолютный путь проекта с заменой каждого не-алфанумерик
//! символа на `-` (см. [`encode_project_dir`]). Индекс — `MEMORY.md`
//! (список одностроковых ссылок на остальные `*.md` файлы той же папки).
//!
//! Кнопка «🧠» в шапке сессии tmux читает эту память для cwd открытой
//! сессии и показывает её в модалке — с возможностью правки прямо там же
//! (без похода в отдельный редактор/терминал).
//!
//! ### Резолвинг каталога
//!
//! Один и тот же git-проект может быть открыт с разных вложенных cwd
//! (`repo/`, `repo/src/`), а сама папка памяти Claude Code создаётся под
//! тем cwd, с которого была запущена самая первая сессия (обычно корень
//! репозитория). Поэтому [`resolve_memory_dir`] пробует по порядку: сам
//! `cwd`, `paths::resolve_root(cwd)`, и далее — все ancestors `cwd` до
//! `$HOME` включительно, возвращая первую найденную папку памяти. Один и
//! тот же резолвинг используют и чтение ([`load_project_memory`]), и
//! запись ([`save_project_memory_file`]) — правка всегда попадает туда же,
//! откуда была прочитана.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Заменяет каждый не-алфанумерик символ пути на `-` — та же схема
/// кодирования, что использует Claude Code для имени `~/.claude/projects/*`.
fn encode_project_dir(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Убирает завершающий(е) `/` у пути (без обращения к диску, в отличие от
/// `canonicalize`).
///
/// Важно: cwd tmux-сессии в этом проекте приходит с trailing slash
/// (`SessionInfo::path`, см. `tmux::list_sessions`), а `Path::ancestors()`
/// отдаёт САМ путь как первый элемент без нормализации — т.е. с этим же
/// trailing slash. Без нормализации это ломает сразу две вещи:
/// 1) [`encode_project_dir`] кодирует финальный `/` в лишний `-`, из-за
///    чего закодированное имя не совпадает с реальной папкой
///    `~/.claude/projects/<encoded>` (там просто нет trailing slash в
///    исходном cwd, с которым запускался Claude Code);
/// 2) `paths::resolve_root` находит `.beads`/`.git` прямо в этом первом
///    (нетронутом) элементе ancestors и возвращает путь С той же trailing
///    slash — то есть возвращает по сути тот же `cwd` (только с
///    сохранённым слэшем), а не «очищенный» родительский путь, и наша
///    проверка `root != cwd` не срабатывает.
///
/// `Path::components()` normalизует путь (убирает финальный `/`, схлопывает
/// `//`), поэтому пересборка через `collect::<PathBuf>()` даёт canonical-по
/// форме (не по файловой системе) путь без обращения к диску.
fn normalize_path(path: &Path) -> PathBuf {
    path.components().collect()
}

fn memory_dir_for(claude_projects_dir: &Path, cwd: &Path) -> PathBuf {
    claude_projects_dir
        .join(encode_project_dir(cwd))
        .join("memory")
}

/// Ищет папку памяти Claude Code для `cwd` (порядок резолвинга — см.
/// модульную документацию). Возвращает `None`, если `$HOME` не задан или
/// ни один кандидат не существует на диске.
fn resolve_memory_dir(cwd: &Path) -> Option<PathBuf> {
    let home = match std::env::var("HOME") {
        Ok(h) if !h.trim().is_empty() => PathBuf::from(h),
        _ => return None,
    };
    let claude_projects_dir = home.join(".claude").join("projects");
    let cwd = normalize_path(cwd);

    let mut candidates: Vec<PathBuf> = vec![cwd.clone()];
    let root = normalize_path(&crate::paths::resolve_root(&cwd));
    if root != cwd {
        candidates.push(root);
    }
    for ancestor in cwd.ancestors().skip(1) {
        if ancestor == home {
            break;
        }
        candidates.push(ancestor.to_path_buf());
    }

    let mut seen = std::collections::HashSet::new();
    candidates.into_iter().find_map(|candidate| {
        let dir = memory_dir_for(&claude_projects_dir, &candidate);
        if !seen.insert(dir.clone()) {
            return None;
        }
        dir.is_dir().then_some(dir)
    })
}

/// Куда указывала бы память для `cwd`, даже если папки ещё нет на диске
/// (для показа в UI / диагностики).
fn expected_memory_dir(cwd: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    memory_dir_for(
        &PathBuf::from(home).join(".claude").join("projects"),
        &normalize_path(cwd),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryFile {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectMemory {
    /// Абсолютный путь папки памяти (для отладки / показа в UI).
    pub dir: String,
    /// `true`, если папка памяти нашлась на диске.
    pub exists: bool,
    /// Сырое содержимое `MEMORY.md` (индекс). Пусто, если файла нет —
    /// это валидное состояние (папка памяти существует, индекса ещё нет).
    pub index: String,
    /// Остальные `*.md`-файлы папки памяти, по алфавиту, с оригинальным
    /// (не склеенным) содержимым — пригодны для редактирования и обратной
    /// записи через [`save_project_memory_file`].
    pub files: Vec<MemoryFile>,
}

/// Ищет папку памяти Claude Code для `cwd` и читает её содержимое целиком.
///
/// Не паникует и не возвращает `Err`: отсутствие `$HOME`, папки памяти или
/// прав на чтение — валидный результат `exists=false`.
pub async fn load_project_memory(cwd: &Path) -> ProjectMemory {
    let Some(dir) = resolve_memory_dir(cwd) else {
        return ProjectMemory {
            dir: expected_memory_dir(cwd).display().to_string(),
            exists: false,
            index: String::new(),
            files: Vec::new(),
        };
    };

    let index = tokio::fs::read_to_string(dir.join("MEMORY.md"))
        .await
        .unwrap_or_default();

    let mut names: Vec<String> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "MEMORY.md" || !name.to_ascii_lowercase().ends_with(".md") {
                continue;
            }
            names.push(name);
        }
    }
    names.sort();

    let mut files = Vec::with_capacity(names.len());
    for name in names {
        if let Ok(content) = tokio::fs::read_to_string(dir.join(&name)).await {
            files.push(MemoryFile { name, content });
        }
    }

    ProjectMemory {
        dir: dir.display().to_string(),
        exists: true,
        index,
        files,
    }
}

#[derive(Debug)]
pub enum SaveError {
    /// `file` — не простое имя `*.md` (содержит `/`, `\`, `..` или не
    /// оканчивается на `.md`). Защита от path traversal — см.
    /// [`is_valid_memory_filename`].
    InvalidFileName,
    /// Папка памяти для этого `cwd` не найдена на диске — редактировать
    /// нечего (создание новой папки памяти из UI не поддерживается).
    MemoryDirNotFound,
    Io(std::io::Error),
}

/// Разрешены только простые имена файлов `*.md` без разделителей пути и
/// без `..` — защита от записи вне папки памяти (path traversal через
/// `file=../../../etc/passwd` и т.п.).
fn is_valid_memory_filename(name: &str) -> bool {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return false;
    }
    name.to_ascii_lowercase().ends_with(".md")
}

/// Перезаписывает один файл (`MEMORY.md` или любой связанный `*.md`) в
/// папке памяти, резолвнутой для `cwd` тем же алгоритмом, что и
/// [`load_project_memory`] — правка гарантированно попадает в тот же
/// файл, который был показан пользователю.
///
/// Не создаёт новую папку памяти: если для `cwd` папка ещё не существует,
/// возвращает [`SaveError::MemoryDirNotFound`] (создание с нуля из UI —
/// вне охвата этой фичи).
pub async fn save_project_memory_file(cwd: &Path, file: &str, content: &str) -> Result<(), SaveError> {
    if !is_valid_memory_filename(file) {
        return Err(SaveError::InvalidFileName);
    }
    let dir = resolve_memory_dir(cwd).ok_or(SaveError::MemoryDirNotFound)?;
    tokio::fs::write(dir.join(file), content)
        .await
        .map_err(SaveError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn encodes_non_alnum_as_dash() {
        let p = Path::new("/Users/igor/claudeWorkspace/F.O.R.G.E./tmux-web");
        assert_eq!(
            encode_project_dir(p),
            "-Users-igor-claudeWorkspace-F-O-R-G-E--tmux-web"
        );
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        let p = Path::new("/Users/igor/proj/");
        assert_eq!(normalize_path(p), PathBuf::from("/Users/igor/proj"));
        // Кодирование trailing-slash пути (как приходит от tmux::list_sessions)
        // без нормализации даёт лишний '-' в конце и не совпадает с реальной
        // папкой памяти (баг: см. комментарий над normalize_path).
        assert_ne!(encode_project_dir(p), encode_project_dir(&normalize_path(p)));
    }

    #[test]
    fn filename_validation_blocks_traversal() {
        assert!(is_valid_memory_filename("MEMORY.md"));
        assert!(is_valid_memory_filename("feedback_foo.md"));
        assert!(!is_valid_memory_filename("../MEMORY.md"));
        assert!(!is_valid_memory_filename("sub/MEMORY.md"));
        assert!(!is_valid_memory_filename("notes.txt"));
        assert!(!is_valid_memory_filename(""));
    }

    /// Все HOME-зависимые под-кейсы в одном тесте, чтобы `set_var`/`remove_var`
    /// (процесс-глобальны) не гонялись параллельно с другими тестами этого же
    /// модуля (тот же паттерн, что в vapid.rs::default_vapid_path тестах).
    /// Старое значение HOME сохраняется и восстанавливается в конце.
    #[tokio::test]
    async fn load_and_save_project_memory_cases() {
        let saved = std::env::var("HOME").ok();

        // (а) без HOME — валидный exists=false, без паники.
        std::env::remove_var("HOME");
        let got = load_project_memory(Path::new("/tmp/whatever")).await;
        assert!(!got.exists);
        assert_eq!(got.index, "");
        assert!(got.files.is_empty());
        let save_err = save_project_memory_file(Path::new("/tmp/whatever"), "MEMORY.md", "x").await;
        assert!(matches!(save_err, Err(SaveError::MemoryDirNotFound)));

        // (б) MEMORY.md + связанный файл читаются как отдельные поля.
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());

        let cwd = PathBuf::from("/tmp/some/project");
        let encoded = encode_project_dir(&cwd);
        let mem_dir = home
            .path()
            .join(".claude")
            .join("projects")
            .join(&encoded)
            .join("memory");
        tokio::fs::create_dir_all(&mem_dir).await.unwrap();
        tokio::fs::write(mem_dir.join("MEMORY.md"), "- [Foo](foo.md) — hook\n")
            .await
            .unwrap();
        tokio::fs::write(mem_dir.join("foo.md"), "---\nname: foo\n---\n\nBody text")
            .await
            .unwrap();

        let got = load_project_memory(&cwd).await;
        assert!(got.exists);
        assert!(got.index.contains("Foo"));
        assert_eq!(got.files.len(), 1);
        assert_eq!(got.files[0].name, "foo.md");
        assert!(got.files[0].content.contains("Body text"));

        // (в) регрессия: `cwd` с trailing slash (как реально отдаёт
        // `tmux::list_sessions` — см. `SessionInfo::path`) должен резолвиться
        // в ту же папку памяти, что и `cwd` без слэша.
        let repo_root = home.path().join("workspace").join("proj");
        tokio::fs::create_dir_all(repo_root.join(".beads"))
            .await
            .unwrap();
        let repo_mem_dir = home
            .path()
            .join(".claude")
            .join("projects")
            .join(encode_project_dir(&repo_root))
            .join("memory");
        tokio::fs::create_dir_all(&repo_mem_dir).await.unwrap();
        tokio::fs::write(repo_mem_dir.join("MEMORY.md"), "hello memory")
            .await
            .unwrap();

        let bare = load_project_memory(&repo_root).await;
        let mut with_slash = repo_root.to_string_lossy().to_string();
        with_slash.push('/');
        let slashed = load_project_memory(Path::new(&with_slash)).await;

        assert!(bare.exists);
        assert!(slashed.exists, "trailing-slash cwd must find the same memory dir");
        assert_eq!(bare.dir, slashed.dir);
        assert_eq!(slashed.index, "hello memory");

        // (г) сохранение перезаписывает существующий файл и не даёт выйти
        // за пределы папки памяти через traversal в имени файла.
        save_project_memory_file(&cwd, "foo.md", "updated body")
            .await
            .expect("save into existing memory dir must succeed");
        let reloaded = load_project_memory(&cwd).await;
        assert_eq!(reloaded.files[0].content, "updated body");

        let traversal = save_project_memory_file(&cwd, "../evil.md", "pwned").await;
        assert!(matches!(traversal, Err(SaveError::InvalidFileName)));
        assert!(!home.path().join("evil.md").exists());

        if let Some(h) = saved {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }
}
