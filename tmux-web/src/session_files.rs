//! Файлы, вставленные в терминальную сессию из буфера обмена (Ctrl+V).
//!
//! Браузер не может «положить» файл в PTY: через WebSocket в tmux уходят
//! только байты ввода. Поэтому вставка файла работает в два шага:
//!
//! 1. фронт (`static/js/terminal/paste-files.js`) ловит `paste` с
//!    `clipboardData.files` и POST'ит содержимое на
//!    `POST /api/sessions/:name/files?name=<имя>`;
//! 2. бэкенд кладёт байты в `<root>/.forge/pasted/` и возвращает абсолютный
//!    путь, который фронт вставляет в терминал как обычный текст.
//!
//! `<root>` — корень проекта сессии: [`crate::paths::resolve_root`] от cwd
//! активной панели (`.beads/` → `.git/` → сам cwd), тот же корень, к которому
//! привязаны todos/notifier. Каталог `.forge/` уже перечислен в `.gitignore`
//! проекта, поэтому вставленные скриншоты не засоряют `git status`.
//!
//! Имя файла из буфера — недоверенный ввод: браузер (или пользователь через
//! свой скрипт) может прислать `../../.ssh/authorized_keys`. Поэтому имя
//! проходит через [`sanitize_file_name`], который оставляет только базовое
//! имя из безопасного алфавита; собранный путь физически не может выйти за
//! пределы `<root>/.forge/pasted/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Потолок размера одной вставки — 25 MiB.
///
/// Ставится `DefaultBodyLimit` на роут `POST /api/sessions/:name/files`
/// (дефолт axum — 2 MiB, что режет обычный скриншот ретины). Больше 25 MiB
/// через буфер обмена не вставляют — такое проще положить в проект руками.
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// Максимальная длина stem (имени без расширения) после санитизации.
const MAX_STEM_LEN: usize = 80;

/// Максимальная длина расширения (с учётом точки — `.` не считается).
const MAX_EXT_LEN: usize = 16;

/// Каталог, куда складываются вставленные файлы: `<root>/.forge/pasted`.
pub fn pasted_dir(root: &Path) -> PathBuf {
    root.join(".forge").join("pasted")
}

/// Расширение по MIME-типу для файлов, приехавших из буфера без имени.
///
/// Скриншот в macOS/Linux попадает в clipboard как `image/png` вообще без
/// имени файла — расширение нужно восстановить, иначе CLI (в первую очередь
/// Claude Code) не поймёт, что ему подсунули картинку.
///
/// Неизвестный тип → `bin`: лучше нейтральное расширение, чем угаданное
/// неверно.
pub fn ext_for_mime(mime: &str) -> &'static str {
    // `image/png; charset=binary` — берём часть до `;` и нормализуем регистр.
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/heic" => "heic",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "application/pdf" => "pdf",
        "application/json" => "json",
        "application/zip" => "zip",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "text/html" => "html",
        _ => "bin",
    }
}

/// Приводит присланное имя файла к безопасному имени внутри `pasted/`.
///
/// Правила (каждое закрыто тестом):
/// - берётся ТОЛЬКО базовое имя: всё до последнего `/` или `\` отбрасывается,
///   поэтому `../../etc/passwd` превращается в `passwd`;
/// - алфавит — ASCII-буквы/цифры и `.`, `_`, `-`, `+`, пробел; всё остальное
///   (в том числе кириллица, управляющие символы, `:`, `*`) заменяется на `_`.
///   Транслитерации нет намеренно: имя вставляется в командную строку, и
///   предсказуемый ASCII важнее красоты;
/// - ведущие точки срезаются — вставка не должна создавать скрытые файлы и не
///   должна давать `.` / `..`;
/// - stem обрезается до [`MAX_STEM_LEN`], расширение — до [`MAX_EXT_LEN`]
///   (некоторые FS ломаются на длинных именах, а длинный путь неудобно
///   вставлять в терминал);
/// - если после чистки не осталось имени — берётся `pasted_<stamp>` с
///   расширением из `content_type`;
/// - если имя есть, но без расширения, а MIME известен — расширение
///   добавляется.
///
/// # Параметры
/// - `raw` — имя из запроса (`?name=`), как прислал браузер;
/// - `content_type` — заголовок запроса, источник расширения для безымянных
///   вставок;
/// - `stamp` — временна́я метка для синтетического имени (вида
///   `20260805_142530`); передаётся снаружи, чтобы функция осталась чистой и
///   тестируемой.
pub fn sanitize_file_name(raw: &str, content_type: Option<&str>, stamp: &str) -> String {
    // 1. Только базовое имя — режем и по '/', и по '\' (Windows-клиент).
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    // 2. Фильтр алфавита.
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();

    // 3. Ведущие точки и пробелы: ни скрытых файлов, ни `.`/`..`.
    cleaned = cleaned.trim_start_matches(['.', ' ']).trim().to_string();

    let mime_ext = content_type.map(ext_for_mime);

    if cleaned.is_empty() {
        let ext = mime_ext.unwrap_or("bin");
        return format!("pasted_{stamp}.{ext}");
    }

    // 4. Разбор на stem + ext (по последней точке, если она не первый символ).
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if i > 0 => (cleaned[..i].to_string(), cleaned[i + 1..].to_string()),
        _ => (cleaned.clone(), String::new()),
    };

    let mut stem: String = stem.chars().take(MAX_STEM_LEN).collect();
    stem = stem.trim().to_string();
    if stem.is_empty() {
        stem = format!("pasted_{stamp}");
    }

    let mut ext: String = ext.chars().take(MAX_EXT_LEN).collect();
    if ext.is_empty() {
        // Имя без расширения: подставим по MIME, если он что-то говорит.
        if let Some(m) = mime_ext {
            if m != "bin" {
                ext = m.to_string();
            }
        }
    }

    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

/// Подбирает свободное имя в каталоге: `name.ext`, `name-2.ext`, `name-3.ext`…
///
/// Вставка двух скриншотов подряд даёт одинаковые имена (`image.png`), и
/// молча перезаписать первый — потеря данных. Счётчик ограничен 1000
/// попытками: дальше почти наверняка проблема не в коллизиях, а в правах на
/// каталог, и лучше вернуть ошибку, чем крутить бесконечный цикл.
pub fn unique_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let direct = dir.join(name);
    if !direct.exists() {
        return Ok(direct);
    }

    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    };

    for n in 2..=1000 {
        let candidate = match ext {
            Some(e) => dir.join(format!("{stem}-{n}.{e}")),
            None => dir.join(format!("{stem}-{n}")),
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    anyhow::bail!("не удалось подобрать свободное имя для `{name}` в {dir:?}")
}

/// Текущая метка времени для синтетических имён: `YYYYMMDD_HHMMSS` (локальная
/// зона — файл смотрит человек, и UTC его только путает).
pub fn now_stamp() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// Сохраняет вставленные байты в `<root>/.forge/pasted/` и возвращает
/// абсолютный путь записанного файла.
///
/// Каталог создаётся при первой вставке (`create_dir_all`). Имя приводится
/// [`sanitize_file_name`] и разводится [`unique_path`], так что вызов никогда
/// не перезаписывает существующий файл и не пишет за пределы `pasted/`.
///
/// Ошибки: нет прав на создание каталога/записи, исчерпан счётчик коллизий.
pub async fn save_pasted(
    root: &Path,
    raw_name: &str,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<PathBuf> {
    let dir = pasted_dir(root);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("не удалось создать каталог {dir:?}"))?;

    let name = sanitize_file_name(raw_name, content_type, &now_stamp());
    let path = unique_path(&dir, &name)?;

    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("не удалось записать файл {path:?}"))?;

    Ok(path)
}

/// Оборачивает путь в одинарные кавычки для вставки в командную строку,
/// если в нём есть пробелы или иные символы, которые shell разберёт сам.
///
/// Одинарная кавычка внутри пути не появится (санитизация её вырезает), но
/// на случай экзотического `<root>` она экранируется каноническим для sh
/// приёмом `'\''`.
pub fn shell_quote(path: &str) -> String {
    let needs_quote = path
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '~')));
    if !needs_quote {
        return path.to_string();
    }
    format!("'{}'", path.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_reduced_to_base_name() {
        let got = sanitize_file_name("../../etc/passwd", None, "20260805_120000");
        assert_eq!(got, "passwd");
        let got = sanitize_file_name(r"..\..\windows\system32\cmd.exe", None, "20260805_120000");
        assert_eq!(got, "cmd.exe");
        assert!(!got.contains('/') && !got.contains('\\'));
    }

    #[test]
    fn absolute_path_loses_directories() {
        let got = sanitize_file_name("/Users/me/.ssh/id_rsa", None, "20260805_120000");
        assert_eq!(got, "id_rsa");
    }

    #[test]
    fn dot_names_never_survive() {
        assert_eq!(
            sanitize_file_name("..", Some("image/png"), "20260805_120000"),
            "pasted_20260805_120000.png"
        );
        assert_eq!(
            sanitize_file_name(".", Some("image/png"), "20260805_120000"),
            "pasted_20260805_120000.png"
        );
        // Скрытый файл превращается в обычный.
        assert_eq!(
            sanitize_file_name(".bashrc", None, "20260805_120000"),
            "bashrc"
        );
    }

    #[test]
    fn empty_name_gets_mime_extension() {
        assert_eq!(
            sanitize_file_name("", Some("image/png"), "20260805_142530"),
            "pasted_20260805_142530.png"
        );
        assert_eq!(
            sanitize_file_name("", Some("application/pdf"), "20260805_142530"),
            "pasted_20260805_142530.pdf"
        );
        assert_eq!(
            sanitize_file_name("", None, "20260805_142530"),
            "pasted_20260805_142530.bin"
        );
    }

    #[test]
    fn extension_added_when_missing_but_mime_known() {
        assert_eq!(
            sanitize_file_name("screenshot", Some("image/png"), "20260805_120000"),
            "screenshot.png"
        );
        // Неизвестный MIME не выдумывает расширение.
        assert_eq!(
            sanitize_file_name("data", Some("application/octet-stream"), "20260805_120000"),
            "data"
        );
    }

    #[test]
    fn unsafe_chars_replaced_not_dropped() {
        let got = sanitize_file_name("отчёт;rm -rf *.png", None, "20260805_120000");
        assert!(!got.contains(';'));
        assert!(!got.contains('*'));
        assert!(got.ends_with(".png"));
        assert!(got.is_ascii());
    }

    #[test]
    fn long_name_truncated() {
        let raw = format!("{}.png", "a".repeat(500));
        let got = sanitize_file_name(&raw, None, "20260805_120000");
        assert_eq!(got, format!("{}.png", "a".repeat(MAX_STEM_LEN)));

        let raw = format!("file.{}", "b".repeat(100));
        let got = sanitize_file_name(&raw, None, "20260805_120000");
        assert_eq!(got, format!("file.{}", "b".repeat(MAX_EXT_LEN)));
    }

    #[test]
    fn mime_with_parameters_parsed() {
        assert_eq!(ext_for_mime("image/png; charset=binary"), "png");
        assert_eq!(ext_for_mime("IMAGE/JPEG"), "jpg");
        assert_eq!(ext_for_mime("weird/thing"), "bin");
    }

    #[test]
    fn unique_path_avoids_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let first = unique_path(dir, "image.png").unwrap();
        assert_eq!(first, dir.join("image.png"));
        std::fs::write(&first, b"1").unwrap();

        let second = unique_path(dir, "image.png").unwrap();
        assert_eq!(second, dir.join("image-2.png"));
        std::fs::write(&second, b"2").unwrap();

        let third = unique_path(dir, "image.png").unwrap();
        assert_eq!(third, dir.join("image-3.png"));

        // Файл без расширения тоже разводится.
        let noext = dir.join("dump");
        std::fs::write(&noext, b"x").unwrap();
        assert_eq!(unique_path(dir, "dump").unwrap(), dir.join("dump-2"));
    }

    #[tokio::test]
    async fn save_pasted_writes_into_forge_pasted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let path = save_pasted(root, "../../evil.png", Some("image/png"), b"PNG")
            .await
            .unwrap();

        assert_eq!(path, pasted_dir(root).join("evil.png"));
        assert!(path.starts_with(pasted_dir(root)));
        assert_eq!(std::fs::read(&path).unwrap(), b"PNG");

        // Вторая вставка с тем же именем не затирает первую.
        let path2 = save_pasted(root, "evil.png", Some("image/png"), b"PNG2")
            .await
            .unwrap();
        assert_ne!(path, path2);
        assert_eq!(std::fs::read(&path).unwrap(), b"PNG");
    }

    #[test]
    fn shell_quote_only_when_needed() {
        assert_eq!(shell_quote("/tmp/a/b.png"), "/tmp/a/b.png");
        assert_eq!(shell_quote("/tmp/my file.png"), "'/tmp/my file.png'");
        assert_eq!(shell_quote("/tmp/it's.png"), r"'/tmp/it'\''s.png'");
    }
}
