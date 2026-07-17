//! Мутирующие git-worktree операции для фичи «Новое окно в git worktree».
//!
//! ### Назначение
//!
//! Модуль инкапсулирует операции, которые *изменяют* состояние git-репозитория:
//! создание изолированной рабочей копии (`git worktree add` на новой ветке) и
//! её удаление (`git worktree remove`). Read-only модуль [`crate::git`]
//! (история коммитов для гант-диаграммы) сознательно оставлен неизменным —
//! мутирующая логика вынесена сюда, чтобы явно разделить «чтение» и «запись».
//!
//! ### Модель размещения worktree
//!
//! Рабочие копии складываются *внутри* корня репозитория, в скрытый каталог
//! `<repo>/.forge-worktrees/<имя>/`. Имя генерируется по времени
//! ([`alloc_worktree_name`]), а сам каталог `.forge-worktrees/` добавляется в
//! `.gitignore` ([`ensure_gitignore_entry`]), чтобы вложенные рабочие копии не
//! засоряли `git status` основного дерева. Каждая worktree живёт на собственной
//! новой ветке; при удалении рабочей копии ветка НЕ трогается — это осознанное
//! решение, чтобы не потерять незакоммиченную/незамерженную историю.
//!
//! ### Почему через CLI, а не git-крейт
//!
//! Как и [`crate::git`], модуль идёт самым простым путём: spawn'ит `git` как
//! subprocess через [`tokio::process::Command`] (не блокируя async-runtime).
//! Полноценный worktree-API у нативных крейтов (`git2`/`gix`) либо отсутствует,
//! либо тянет тяжёлую зависимость ради пары команд. CLI-путь совпадает с тем,
//! как git запускается в остальном проекте.
//!
//! ### Определение корня
//!
//! В отличие от [`crate::git`], который берёт «корень» через
//! [`crate::paths::resolve_root`] (маркеры `.beads/`/`.git/`), здесь всегда
//! резолвится корень ГЛАВНОГО рабочего дерева (main worktree) —
//! [`repo_toplevel`] через `git rev-parse --git-common-dir`. Это принципиально:
//! рабочие копии и каталог `.forge-worktrees/` живут под главным корнем, а
//! удаление ([`crate::delete_worktree_window`]) выполняется из окна, cwd
//! которого — сам linked-worktree; наивный `git rev-parse --show-toplevel`
//! вернул бы там корень linked-worktree, а не главного дерева, и удаление
//! сломалось бы.
//!
//! ### Отказы
//!
//! [`repo_toplevel`] деградирует мягко (не-git каталог → `Ok(None)`), тогда как
//! мутирующие [`worktree_add`]/[`worktree_remove`] возвращают `Err` со stderr
//! git при ненулевом exit — вызывающий код (эндпоинты Фазы 2) обязан сообщить
//! об ошибке пользователю. [`ensure_gitignore_entry`] некритична: любые ошибки
//! ввода-вывода лишь логируются через `tracing::warn!`, не прерывая основной
//! флоу создания окна.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use tokio::process::Command;

/// Имя каталога-контейнера для всех forge-worktree внутри репозитория.
///
/// Строка добавляется в `.gitignore` ([`ensure_gitignore_entry`]) и служит
/// базой для генерации путей рабочих копий (`<repo>/.forge-worktrees/<имя>/`).
const WORKTREES_DIR: &str = ".forge-worktrees";

/// Запись в `.gitignore`, скрывающая каталог с рабочими копиями от git.
///
/// Хвостовой `/` явно помечает запись как каталог. Именно с этой строкой
/// сравниваются (по `trim`, построчно) существующие строки `.gitignore` в
/// [`ensure_gitignore_entry`].
const GITIGNORE_ENTRY: &str = ".forge-worktrees/";

/// Возвращает абсолютный корень ГЛАВНОГО рабочего дерева (main worktree) того
/// git-репозитория, которому принадлежит `cwd` — даже если `cwd` находится
/// внутри linked-worktree (`.forge-worktrees/<имя>`).
///
/// # Почему НЕ `git rev-parse --show-toplevel`
///
/// `--show-toplevel`, запущенный внутри linked-worktree, возвращает корень
/// самого linked-worktree, а НЕ главного дерева. Этой фиче всегда нужен главный
/// корень: там лежит каталог `.forge-worktrees/`, туда пишется `.gitignore` и
/// оттуда безопасно выполнять `git worktree add`/`remove`. Поэтому корень
/// вычисляется через общий git-каталог: `git rev-parse --path-format=absolute
/// --git-common-dir` возвращает `<main>/.git` (одинаково для главного дерева и
/// всех worktree), а главный корень — его родитель. Так функция корректна и при
/// создании окна (cwd активной панели, обычно в главном дереве), и при удалении
/// (cwd — сам linked-worktree).
///
/// В отличие от [`crate::paths::resolve_root`] (маркеры `.beads/`/`.git/`),
/// здесь корень определяется строго git'ом.
///
/// # Параметры
/// - `cwd` — рабочая директория (cwd панели/окна tmux-сессии). Репозиторий и
///   его главный корень git ищет вверх от неё.
///
/// # Возврат
/// - `Ok(Some(path))` — `cwd` внутри git-репозитория; `path` — корень главного
///   рабочего дерева.
/// - `Ok(None)` — `cwd` не в git-репозитории (ненулевой exit git), git не
///   удалось заспавнить, вывод пуст или у него нет родителя. Никогда не
///   всплывает как `Err` — вызывающий сам решает, что показать при `None`.
///
/// # Пример
/// ```ignore
/// // и из главного дерева, и из .forge-worktrees/wt-… вернётся "/proj":
/// if let Some(top) = worktree::repo_toplevel(Path::new("/proj/src")).await? {
///     // top == "/proj"
/// }
/// ```
pub async fn repo_toplevel(cwd: &Path) -> anyhow::Result<Option<PathBuf>> {
    let output = match Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            // git не найден в PATH / иная ошибка spawn — трактуем как «не репо».
            tracing::debug!(error = ?e, cwd = ?cwd, "git rev-parse --git-common-dir spawn failed — treating as non-repo");
            return Ok(None);
        }
    };

    if !output.status.success() {
        // Не-git каталог ("fatal: not a git repository") — graceful None.
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::debug!(
            exit = ?output.status.code(),
            cwd = ?cwd,
            stderr = %stderr.trim(),
            "git rev-parse --git-common-dir non-zero exit — treating as non-repo"
        );
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    // `--git-common-dir` → `<main>/.git`; корень главного дерева — его родитель.
    // Общий git-каталог един для главного дерева и всех linked-worktree,
    // поэтому результат всегда указывает на ГЛАВНЫЙ корень.
    match Path::new(trimmed).parent() {
        Some(root) if !root.as_os_str().is_empty() => Ok(Some(root.to_path_buf())),
        _ => Ok(None),
    }
}

/// Создаёт новую рабочую копию на новой ветке: `git worktree add <path> -b <branch>`.
///
/// Команда запускается с `current_dir(toplevel)` (настоящий git-toplevel из
/// [`repo_toplevel`]). git создаёт каталог `path`, чекаутит в него новую ветку
/// `branch` (флаг `-b`) и регистрирует worktree в репозитории.
///
/// # Параметры
/// - `toplevel` — корень репозитория (рабочая директория для git).
/// - `path` — путь будущей рабочей копии; обычно
///   `<toplevel>/.forge-worktrees/<имя>` из [`alloc_worktree_name`]. Каталог
///   не должен существовать заранее — его создаёт git.
/// - `branch` — имя новой ветки. Ветка не должна существовать (иначе `-b`
///   вернёт ошибку).
///
/// # Возврат
/// `Ok(())` при успехе. При ненулевом exit git — `Err` с кодом выхода и
/// обрезанным stderr git (например, «branch already exists» или
/// «directory already exists»).
pub async fn worktree_add(toplevel: &Path, path: &Path, branch: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("worktree")
        .arg("add")
        .arg(path)
        .arg("-b")
        .arg(branch)
        .current_dir(toplevel)
        .output()
        .await
        .context("failed to spawn `git worktree add`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "git worktree add failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Удаляет рабочую копию: `git worktree remove [--force] <path>`.
///
/// Команда запускается с `current_dir(toplevel)`. Удаляется только сам worktree
/// (каталог + его регистрация в репозитории); ветка, на которую он был
/// зачекаучен, НЕ удаляется — это осознанное решение, чтобы не потерять
/// историю коммитов рабочей копии.
///
/// # Параметры
/// - `toplevel` — корень репозитория (рабочая директория для git).
/// - `path` — путь удаляемой рабочей копии.
/// - `force` — если `true`, добавляется флаг `--force`. Нужен, когда в worktree
///   есть незакоммиченные изменения или отслеживаемые/неотслеживаемые файлы,
///   иначе git откажется удалять «грязную» рабочую копию.
///
/// # Возврат
/// `Ok(())` при успехе. При ненулевом exit git — `Err` с кодом выхода и
/// обрезанным stderr git.
pub async fn worktree_remove(toplevel: &Path, path: &Path, force: bool) -> anyhow::Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("worktree").arg("remove");
    if force {
        cmd.arg("--force");
    }
    cmd.arg(path).current_dir(toplevel);

    let output = cmd
        .output()
        .await
        .context("failed to spawn `git worktree remove`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "git worktree remove failed (exit {:?}): {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Идемпотентно добавляет `.forge-worktrees/` в `<toplevel>/.gitignore`.
///
/// Скрывает каталог с вложенными рабочими копиями от git, чтобы они не
/// засоряли `git status` основного дерева. Поведение:
/// 1. Читает `<toplevel>/.gitignore`. Если файла нет — считает содержимое
///    пустым.
/// 2. Если хотя бы одна строка (после `trim`) равна [`GITIGNORE_ENTRY`] —
///    ничего не делает (запись уже есть → идемпотентность).
/// 3. Иначе дописывает `.forge-worktrees/\n`, добавляя ведущий `\n`, если файл
///    непустой и не заканчивается переводом строки (чтобы не склеить с
///    предыдущей записью).
///
/// # Отказоустойчивость
/// Функция *некритична* для основного флоу: любые ошибки чтения/записи (нет
/// прав, гонка и т.п.) не паникуют и не всплывают, а лишь логируются через
/// `tracing::warn!`. Отсутствующая запись в `.gitignore` не ломает создание
/// worktree — лишь делает его видимым в `git status`.
///
/// # Параметры
/// - `toplevel` — корень репозитория; путь к файлу — `<toplevel>/.gitignore`.
pub fn ensure_gitignore_entry(toplevel: &Path) {
    let gitignore = toplevel.join(".gitignore");

    let existing = match std::fs::read_to_string(&gitignore) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            tracing::warn!(
                error = ?e,
                path = ?gitignore,
                "failed to read .gitignore — skipping .forge-worktrees/ entry"
            );
            return;
        }
    };

    // Уже есть точная запись (сравнение по trim, построчно) — идемпотентно.
    if existing.lines().any(|line| line.trim() == GITIGNORE_ENTRY) {
        return;
    }

    let mut updated = existing;
    // Ведущий \n, только если файл непустой и не заканчивается на newline —
    // иначе запись склеится с предыдущей строкой.
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(GITIGNORE_ENTRY);
    updated.push('\n');

    if let Err(e) = std::fs::write(&gitignore, updated) {
        tracing::warn!(
            error = ?e,
            path = ?gitignore,
            "failed to write .forge-worktrees/ entry to .gitignore"
        );
    }
}

/// Подбирает свободное имя для новой рабочей копии и возвращает `(имя, путь)`.
///
/// Базовое имя — `wt-<secs>`, где `secs` — текущее время в секундах Unix
/// (`SystemTime::now().duration_since(UNIX_EPOCH)`; при ошибке (часы до эпохи)
/// используется `0`). Если каталог `base.join(name)` уже существует, к базовому
/// имени добавляется числовой суффикс: `wt-<secs>-2`, `wt-<secs>-3`, … — до
/// первого свободного. Практически коллизия возможна лишь при нескольких
/// worktree, созданных в одну и ту же секунду.
///
/// # Параметры
/// - `base` — каталог-контейнер рабочих копий, обычно
///   `<toplevel>/.forge-worktrees` (см. [`worktrees_base`]).
///
/// # Возврат
/// Кортеж `(dir_name, full_path)`, где `dir_name` — только имя каталога
/// (`wt-...`), а `full_path == base.join(dir_name)` — абсолютный путь будущей
/// рабочей копии, готовый для передачи в [`worktree_add`].
pub fn alloc_worktree_name(base: &Path) -> (String, PathBuf) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let stem = format!("wt-{secs}");
    let mut name = stem.clone();
    let mut suffix = 2u32;
    while base.join(&name).exists() {
        name = format!("{stem}-{suffix}");
        suffix += 1;
    }

    let path = base.join(&name);
    (name, path)
}

/// Возвращает каталог-контейнер рабочих копий для данного `toplevel`.
///
/// Просто `<toplevel>/.forge-worktrees` — база, которую ожидает
/// [`alloc_worktree_name`] и которая скрывается [`ensure_gitignore_entry`].
/// Вынесено в хелпер, чтобы имя каталога ([`WORKTREES_DIR`]) не дублировалось
/// в вызывающем коде эндпоинтов.
pub fn worktrees_base(toplevel: &Path) -> PathBuf {
    toplevel.join(WORKTREES_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktrees_base_appends_hidden_dir() {
        let base = worktrees_base(Path::new("/proj"));
        assert_eq!(base, PathBuf::from("/proj/.forge-worktrees"));
    }

    #[test]
    fn alloc_worktree_name_uses_wt_prefix_and_matches_path() {
        let tmp = std::env::temp_dir().join(format!("forge-wt-test-{}", std::process::id()));
        let (name, path) = alloc_worktree_name(&tmp);
        assert!(name.starts_with("wt-"), "name was {name}");
        assert_eq!(path, tmp.join(&name));
    }

    #[test]
    fn alloc_worktree_name_adds_suffix_on_collision() {
        // Готовим уникальный base и занимаем базовое имя wt-<secs> реальным
        // каталогом, чтобы проверить переход к суффиксу -2.
        let base = std::env::temp_dir().join(format!(
            "forge-wt-collide-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let occupied = base.join(format!("wt-{secs}"));
        std::fs::create_dir_all(&occupied).unwrap();

        let (name, path) = alloc_worktree_name(&base);
        // Базовое имя занято → должен появиться суффикс -2 (или далее).
        assert_ne!(name, format!("wt-{secs}"));
        assert!(name.starts_with(&format!("wt-{secs}-")), "name was {name}");
        assert_eq!(path, base.join(&name));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_gitignore_entry_creates_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "forge-gitignore-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let gitignore = dir.join(".gitignore");

        // (1) Файла нет — создаётся с единственной записью.
        ensure_gitignore_entry(&dir);
        let after_first = std::fs::read_to_string(&gitignore).unwrap();
        assert_eq!(after_first, ".forge-worktrees/\n");

        // (2) Повторный вызов не дублирует строку.
        ensure_gitignore_entry(&dir);
        let after_second = std::fs::read_to_string(&gitignore).unwrap();
        assert_eq!(after_second, ".forge-worktrees/\n");
        assert_eq!(
            after_second
                .lines()
                .filter(|l| l.trim() == ".forge-worktrees/")
                .count(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_gitignore_entry_appends_without_trailing_newline() {
        let dir = std::env::temp_dir().join(format!(
            "forge-gitignore-append-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let gitignore = dir.join(".gitignore");
        // Существующий файл без хвостового newline.
        std::fs::write(&gitignore, "target").unwrap();

        ensure_gitignore_entry(&dir);
        let content = std::fs::read_to_string(&gitignore).unwrap();
        // Ведущий \n добавлен, чтобы не склеить с "target".
        assert_eq!(content, "target\n.forge-worktrees/\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn repo_toplevel_returns_none_for_non_repo() {
        // Каталог заведомо вне git-репозитория.
        let dir = std::env::temp_dir().join(format!(
            "forge-nonrepo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let res = repo_toplevel(&dir).await.unwrap();
        assert!(res.is_none(), "expected None for non-repo, got {res:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn repo_toplevel_from_linked_worktree_returns_main_root() {
        // Регресс: `git rev-parse --show-toplevel` из linked-worktree вернул бы
        // корень самого worktree — тогда удаление ломается. repo_toplevel обязан
        // вернуть корень ГЛАВНОГО дерева. Требует `git` в PATH (как и остальные
        // repo_toplevel-тесты).
        let base = std::env::temp_dir().join(format!(
            "forge-mainroot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let main_repo = base.join("repo");
        std::fs::create_dir_all(&main_repo).unwrap();

        // ВАЖНО: тест может выполняться ИЗ pre-commit хука (`.githooks/pre-commit`
        // гоняет `cargo test`). Тогда `git commit` экспортирует в окружение
        // GIT_INDEX_FILE=.git/index (ОТНОСИТЕЛЬНЫЙ путь), GIT_PREFIX и пр., и
        // они наследуются вплоть до git-субпроцессов теста. Для `git worktree add`
        // это фатально: у linked-worktree свой индекс, а `<wt>/.git` — файл-гитлинк,
        // поэтому git падает с `.git/index: index file open failed: Not a directory`,
        // успев создать ветку. Чистим унаследованное git-окружение — репозиторий
        // определяется строго по cwd.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&main_repo)
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_NAMESPACE")
                .env_remove("GIT_PREFIX")
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "git {args:?} exit {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.local"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "--allow-empty", "-q", "-m", "init"]);

        // Linked-worktree в main_repo/.forge-worktrees/wt на новой ветке.
        let wt = main_repo.join(".forge-worktrees").join("wt");
        git(&["worktree", "add", wt.to_str().unwrap(), "-b", "forge/wt-test"]);

        // repo_toplevel из linked-worktree обязан вернуть ГЛАВНЫЙ корень.
        // (`git rev-parse --git-common-dir` индекс не читает, поэтому на него
        // унаследованный GIT_INDEX_FILE не влияет.)
        let got = repo_toplevel(&wt)
            .await
            .unwrap()
            .expect("expected Some(main root) from inside linked worktree");
        let got_canon = std::fs::canonicalize(&got).unwrap();
        let main_canon = std::fs::canonicalize(&main_repo).unwrap();
        assert_eq!(
            got_canon, main_canon,
            "expected main worktree root, got {got:?}"
        );

        // Уборка.
        git(&["worktree", "remove", "--force", wt.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
