//! Резолвинг «корня» по произвольному cwd.
//!
//! ### Назначение
//!
//! После удаления концепции `Project` (см. план
//! `remove-projects-concept.md`) источник истины для группировки TODO,
//! notifier-конфига и других per-folder состояний — это сам `cwd` сессии.
//! Но один и тот же проект может иметь много вложенных cwd (например,
//! `repo/`, `repo/src/`, `repo/src/foo/`). Чтобы TODO «приклеивались»
//! к одному корню, [`resolve_root`] поднимается вверх по ancestors и
//! ищет каноничный маркер.
//!
//! ### Алгоритм
//!
//! 1. Идём по `cwd.ancestors()` (т.е. сам cwd, потом каждый родительский
//!    путь до корня FS) и ищем первую папку с подкаталогом `.beads/`.
//!    Если нашли — это и есть корень (приоритет over `.git/`, потому что
//!    `.beads/` — точечная мета F.O.R.G.E.; если есть и `.beads/`, и
//!    `.git/`, нас интересует именно та папка, где живут локальные
//!    задачи).
//! 2. Если `.beads/` нигде не нашли — повторяем проход и ищем `.git/`.
//! 3. Иначе возвращаем сам `cwd` как fallback.
//!
//! ### Семантика для несуществующих путей
//!
//! Функция не паникует на путях, которых нет на диске. `is_dir()` для
//! отсутствующего пути возвращает `false`, поэтому ни один маркер не
//! найдётся и мы вернёмся к fallback. Это важно: вызовы приходят из
//! пользовательских хендлеров, где path — произвольный input.
//!
//! ### Не выполняем canonicalize()
//!
//! Намеренно: `canonicalize` (a) ломается на несуществующих путях, (b)
//! на macOS разворачивает `/tmp` → `/private/tmp`, что сделает тесты
//! хрупкими. Если бы canonicalize был нужен — это была бы обязанность
//! вызывающей стороны (например, на этапе сохранения todos.json).

use std::path::{Path, PathBuf};

/// Резолвит «корень» для произвольного cwd по маркерам `.beads/` и `.git/`.
///
/// Алгоритм:
/// 1. Поднимается по `cwd.ancestors()`, ищет первую папку с `.beads/`.
/// 2. Если не нашёл — ищет первую папку с `.git/`.
/// 3. Иначе возвращает сам `cwd` как `PathBuf`.
///
/// Никогда не паникует — на несуществующем cwd просто вернёт его как
/// есть (без canonicalize).
///
/// ### Примеры
///
/// ```ignore
/// // cwd = /tmp/proj/src, /tmp/proj/.beads/ существует
/// // -> /tmp/proj
///
/// // cwd = /tmp/proj/src, /tmp/proj/.git/ существует, .beads/ нет
/// // -> /tmp/proj
///
/// // cwd = /tmp/empty, маркеров нет
/// // -> /tmp/empty
/// ```
pub fn resolve_root(cwd: &Path) -> PathBuf {
    if let Some(p) = find_marker(cwd, ".beads") {
        return p;
    }
    if let Some(p) = find_marker(cwd, ".git") {
        return p;
    }
    cwd.to_path_buf()
}

/// Идёт вверх от `start` (включая сам `start`) и ищет первую папку, в
/// которой существует подкаталог `marker`.
///
/// Возвращает `Some(parent)` — папку, **содержащую** marker (т.е. сам
/// `<parent>/<marker>/` есть на диске и является каталогом). Если ни в
/// одной из ancestors маркер не нашёлся — `None`.
fn find_marker(start: &Path, marker: &str) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(marker);
        if candidate.is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Создаёт подкаталог (с любой вложенностью) внутри `root`.
    fn mkdir_in(root: &Path, sub: &str) -> PathBuf {
        let p = root.join(sub);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resolves_to_beads_when_present() {
        let tmp = TempDir::new().unwrap();
        mkdir_in(tmp.path(), ".beads");
        // cwd сам корень — должен вернуть его же.
        let got = resolve_root(tmp.path());
        assert_eq!(got, tmp.path());
    }

    #[test]
    fn resolves_to_git_when_no_beads() {
        let tmp = TempDir::new().unwrap();
        mkdir_in(tmp.path(), ".git");
        let got = resolve_root(tmp.path());
        assert_eq!(got, tmp.path());
    }

    #[test]
    fn resolves_to_cwd_when_no_markers() {
        let tmp = TempDir::new().unwrap();
        // ни `.beads/`, ни `.git/` — fallback на сам cwd.
        let got = resolve_root(tmp.path());
        assert_eq!(got, tmp.path());
    }

    #[test]
    fn prefers_beads_over_git() {
        let tmp = TempDir::new().unwrap();
        // Внутри одной и той же папки оба маркера → выбираем .beads/.
        mkdir_in(tmp.path(), ".beads");
        mkdir_in(tmp.path(), ".git");
        let got = resolve_root(tmp.path());
        assert_eq!(got, tmp.path());

        // Дополнительный кейс: .git/ глубже, .beads/ — выше по дереву.
        // Алгоритм должен выбрать .beads/, потому что мы сперва идём по
        // всему ancestors-цепочке за `.beads/` и только потом — за `.git/`.
        let outer = TempDir::new().unwrap();
        mkdir_in(outer.path(), ".beads");
        let inner = mkdir_in(outer.path(), "deep/sub");
        mkdir_in(&inner, ".git");
        let got = resolve_root(&inner);
        assert_eq!(got, outer.path());
    }

    #[test]
    fn walks_up_to_find_marker() {
        let tmp = TempDir::new().unwrap();
        // tmp/a/.beads/, cwd = tmp/a/b/c
        let a = mkdir_in(tmp.path(), "a");
        mkdir_in(&a, ".beads");
        let c = mkdir_in(&a, "b/c");
        let got = resolve_root(&c);
        assert_eq!(got, a);
    }

    #[test]
    fn nonexistent_path_falls_back_to_cwd() {
        // Несуществующий путь — функция не должна паниковать и просто
        // возвращает сам путь как fallback (ни одного маркера не найдётся,
        // потому что is_dir() для отсутствующих каталогов = false).
        let p = PathBuf::from("/definitely/does/not/exist/forge-test");
        let got = resolve_root(&p);
        assert_eq!(got, p);
    }
}
