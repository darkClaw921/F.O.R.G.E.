//! Атомарная запись приватного файла.
//!
//! Одна точка на все приватные файлы плагинов-мессенджеров: конфиг с токеном,
//! хранилище заметок и разборов ([`crate::prompts::save`]), курсор LongPoll.
//! Вынесена ИМЕННО в общий крейт, а не оставлена копией в каждом плагине:
//! у Telegram есть только три кирпича (`write_secret_file`, `tmp_path_for`,
//! `set_owner_only`), а у Bitrix атомарная запись спрятана внутри приватной
//! `write_table` — то есть третья и четвёртая копии `tmp+rename+chmod`
//! появились бы сами собой и разъехались бы с оригиналом по правам.
//!
//! Инвариант, ради которого всё это существует: **частичного состояния на
//! диске не бывает**. Либо целевой файл прежний, либо он новый целиком; при
//! любой ошибке временный файл убирается, а права остаются `0600` — файлы,
//! которые пишутся отсюда, содержат либо секрет, либо заметки о конкретных
//! людях.

use std::path::{Path, PathBuf};

/// ЕДИНАЯ точка атомарной записи приватного файла.
///
/// Последовательность: `create_dir_all` родителя → запись во временный файл
/// рядом ([`tmp_path_for`], права `0600` выставляются ДО первого байта) →
/// `fsync` → `rename` на целевой путь → повторный [`set_owner_only`] (rename
/// сохраняет права tmp-файла, но целевой мог существовать раньше с другими).
///
/// При ЛЮБОЙ ошибке временный файл удаляется, а целевой остаётся ровно таким,
/// каким был.
///
/// # Errors
///
/// Ошибка создания каталога, записи, `fsync`, `rename` или смены прав.
pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }

    let tmp = tmp_path_for(path);
    let outcome = write_secret_file(&tmp, bytes)
        .and_then(|()| {
            std::fs::rename(&tmp, path).map_err(|e| {
                anyhow::anyhow!(
                    "failed to move {} onto {}: {e}",
                    tmp.display(),
                    path.display()
                )
            })
        })
        .and_then(|()| set_owner_only(path));

    if let Err(e) = outcome {
        // Никаких частичных состояний: убираем tmp, целевой файл остался
        // прежним.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Путь временного файла рядом с целевым: `.<имя>.<pid>.tmp`.
///
/// PID в имени защищает от коллизии двух процессов, пишущих файл одновременно:
/// последний `rename` выигрывает — это ожидаемое поведение, но обрезанного
/// файла не увидит никто.
#[must_use]
pub fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let tmp_name = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// Создаёт файл с правами `0600` ДО записи содержимого, пишет и `fsync`'ит.
///
/// # Errors
///
/// Ошибка открытия, смены прав, записи или `fsync`.
pub fn write_secret_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", path.display()))?;
    // Файл мог остаться с прошлого раза (create, а не create_new) — тогда
    // mode() не применяется, поэтому права выставляются явно, и всё это ДО
    // первого write_all.
    set_owner_only(path)?;
    f.write_all(bytes)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| anyhow::anyhow!("failed to fsync {}: {e}", path.display()))?;
    Ok(())
}

/// Выставляет права `0600` (на не-Unix — no-op).
///
/// # Errors
///
/// Ошибка `chmod`.
pub fn set_owner_only(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow::anyhow!("failed to chmod 600 {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Штатная запись: содержимое на месте, права owner-only, tmp не остался.
    #[cfg(unix)]
    #[test]
    fn a_successful_write_is_private_and_leaves_no_temporaries() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // Каталог создаётся сам: путь может указывать в ещё не существующий
        // `~/.forge`.
        let path = dir.path().join("nested").join("secret.json");
        write_bytes_atomic(&path, b"tainy").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"tainy");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "приватные файлы — только владельцу");

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n != "secret.json")
            .collect();
        assert!(leftovers.is_empty(), "tmp не остаётся: {leftovers:?}");
    }

    /// Перезапись поверх файла с широкими правами обязана их ужать: `rename`
    /// сохраняет права tmp, но целевой мог существовать раньше.
    #[cfg(unix)]
    #[test]
    fn overwriting_tightens_the_permissions_of_the_target() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_bytes_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "унаследованные 0644 обязаны ужаться");
    }

    /// Мусорный tmp от упавшего процесса перезаписывается, а его широкие права
    /// ужимаются ДО того, как в файл уедет первый байт секрета.
    #[cfg(unix)]
    #[test]
    fn a_stale_tmp_file_is_tightened_before_the_secret_is_written() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        let tmp = tmp_path_for(&path);
        std::fs::write(&tmp, b"stale garbage from a crashed process").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();

        write_bytes_atomic(&path, b"secret").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!tmp.exists(), "tmp переименован, мусора нет");
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ: неудача не оставляет ни tmp-мусора, ни испорченного
    /// целевого файла. Целевой путь — непустой КАТАЛОГ: запись tmp проходит,
    /// `rename` падает, и уборка обязана сработать именно на этой ветке.
    #[test]
    fn a_failed_write_leaves_neither_temporaries_nor_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("inhabitant"), b"x").unwrap();

        assert!(write_bytes_atomic(&path, "новое содержимое".as_bytes()).is_err());

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n != "busy")
            .collect();
        assert!(leftovers.is_empty(), "после неудачи tmp убран: {leftovers:?}");
        assert!(path.join("inhabitant").is_file(), "чужие данные не тронуты");
    }

    /// Имя tmp-файла: скрытое, рядом с целевым и с PID — два процесса не
    /// затирают друг другу временный файл.
    #[test]
    fn the_temporary_lives_next_to_the_target_and_carries_the_pid() {
        let tmp = tmp_path_for(Path::new("/tmp/forge/p.json"));
        assert_eq!(tmp.parent().unwrap(), Path::new("/tmp/forge"));
        assert_eq!(
            tmp.file_name().unwrap().to_string_lossy(),
            format!(".p.json.{}.tmp", std::process::id())
        );
    }
}
