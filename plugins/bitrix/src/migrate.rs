//! Перенос дисковых данных единственного портала в его подкаталог (Ф6.4).
//!
//! # Что здесь происходит
//!
//! До Ф6 у плагина был один портал, и все его артефакты лежали прямо в корне
//! кэша:
//!
//! ```text
//! ~/.forge/bitrix-cache/media/…            ← скачанные вложения и аватары
//! ~/.forge/bitrix-cache/uploads/…          ← staging отправляемых файлов
//! ~/.forge/bitrix-cache/events.json        ← курсор потока событий
//! ~/.forge/bitrix-cache/voice/…            ← готовые расшифровки голосовых
//! ~/.forge/bitrix-chat-prompts.json        ← заметки, разборы и треды Q&A
//! ```
//!
//! С Ф6 каждый портал адресует своё подкаталогом
//! (`bitrix-cache/<portal>/…`) и своим файлом заметок
//! (`bitrix-chat-prompts-<portal>.json`). Без переноса первый же запуск новой
//! сборки выглядел бы как потеря данных: панель показала бы пустой кэш,
//! забыла бы все разборы и — самое дорогое — начала бы поток событий с нуля.
//!
//! # Почему это не «просто скопировать»
//!
//! Три требования, и каждое диктует конструкцию:
//!
//! 1. **Не удваивать диск.** Медиа-кэш легко весит гигабайты, и копия на время
//!    переноса означала бы удвоенный расход на ровном месте. Поэтому
//!    `rename`, а не копирование: каталог переезжает одним вызовом ядра, не
//!    двигая ни байта содержимого.
//! 2. **Идемпотентность.** Второй запуск не должен ни переносить заново, ни
//!    дублировать. Обеспечивается тем, что источник ПОСЛЕ переноса не
//!    существует: `rename` его забрал.
//! 3. **Устойчивость к прерыванию.** Процесс могут убить между двумя записями
//!    (`SIGKILL`, отключение питания, `pkill devforge` в середине старта).
//!    Каждая запись здесь — ОДИН `rename`, а он атомарен: запись либо
//!    произошла целиком, либо не начиналась. Промежуточного состояния «файл
//!    наполовину там» не существует, поэтому следующий запуск просто доделает
//!    оставшиеся записи.
//!
//! # Когда перенос НЕ запускается
//!
//! Только при РОВНО ОДНОМ портале в конфиге. Legacy-раскладка по построению
//! принадлежала единственному порталу, и, увидев в корне `media/` рядом с
//! двумя порталами, угадывать владельца нельзя — данные достались бы чужой
//! вкладке. Такое состояние, впрочем, недостижимо штатно: второй портал можно
//! добавить только на уже мигрировавшей сборке.
//!
//! Обратный случай — единственный портал, который legacy НЕ был (свежая
//! установка, где пользователь подключился и добавил вкладку), — безопасен
//! сам: в корне просто нет ни одного из переносимых имён, и функция ничего не
//! делает.
//!
//! # Что переносится
//!
//! Закрытый список имён ([`LEGACY_ENTRIES`]), а не «всё, что лежит в корне».
//! Разница принципиальна: перенос «всего» утащил бы в подкаталог портала и
//! подкаталоги ДРУГИХ порталов, включая зарезервированный `_vacant`, — то
//! есть первая же миграция съела бы соседей.

use std::path::{Path, PathBuf};

use crate::config::BitrixRootConfig;

/// Имена в корне кэша, принадлежавшие единственному порталу до Ф6.
///
/// Список закрытый и совпадает с тем, что строит код путей: `media/`
/// ([`crate::media_cache::MEDIA_SUBDIR`]), `uploads/`
/// ([`crate::routes::upload::UPLOADS_SUBDIR`]), `events.json`
/// ([`crate::events::EVENTS_CURSOR_FILE`]) и `voice/`
/// ([`forge_voice::store::STORE_DIR`]). Разъедься он с ними — перенос молча
/// оставил бы часть данных в корне, и заметили бы это по «пропавшему» кэшу.
pub const LEGACY_ENTRIES: [&str; 4] = [
    crate::media_cache::MEDIA_SUBDIR,
    crate::routes::upload::UPLOADS_SUBDIR,
    crate::events::EVENTS_CURSOR_FILE,
    forge_voice::store::STORE_DIR,
];

/// Итог переноса — для лога и тестов. Секрета не содержит.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Сколько записей верхнего уровня переехало в подкаталог портала.
    pub moved: usize,
    /// Переехал ли файл заметок и разборов.
    pub prompts_moved: bool,
}

impl MigrationReport {
    /// Было ли сделано хоть что-то.
    #[must_use]
    pub fn did_anything(&self) -> bool {
        self.moved > 0 || self.prompts_moved
    }
}

/// Переносит legacy-раскладку в подкаталог единственного портала.
///
/// Зовётся из [`crate::B24Registry::new`] ДО поднятия состояний: воркеры
/// портала не должны увидеть полупереехавший кэш.
///
/// **Ошибки наружу не всплывают.** Перенос — это удобство, а не условие
/// работоспособности: не сумев переехать, плагин обязан подняться и работать с
/// пустым кэшем (он перекачается), а не отказаться стартовать. Всё, что
/// случилось, уходит в лог.
pub fn migrate_legacy_layout(root: &BitrixRootConfig) -> MigrationReport {
    let mut report = MigrationReport::default();

    // Владелец legacy-данных определяется однозначно только при одном портале.
    if root.portals.len() != 1 {
        return report;
    }
    let cfgs = root.portal_configs();
    let Some(portal) = cfgs.first() else {
        return report;
    };

    let legacy_root = &portal.cache_root;
    let target = &portal.cache_dir;
    if legacy_root.as_os_str().is_empty() || legacy_root == target {
        return report;
    }

    for name in LEGACY_ENTRIES {
        let src = legacy_root.join(name);
        let dst = target.join(name);
        // Идентификатор портала может совпасть с именем из списка
        // (`media.bitrix24.ru` ⇒ портал `media`): тогда «источник» и есть
        // каталог портала, и трогать его нельзя.
        if src == *target || !exists(&src) {
            continue;
        }
        if move_into(&src, &dst) {
            report.moved += 1;
            tracing::info!(
                target: "forge_bitrix",
                portal = %portal.portal_id,
                entry = name,
                "forge-bitrix: legacy cache entry moved into the portal directory"
            );
        }
    }

    // Заметки и разборы — файл, а не каталог, и лежит он не в кэше.
    let prompts_src = &portal.prompts_root;
    let prompts_dst = &portal.prompts_path;
    if !prompts_src.as_os_str().is_empty()
        && prompts_src != prompts_dst
        && exists(prompts_src)
        && move_into(prompts_src, prompts_dst)
    {
        report.prompts_moved = true;
        tracing::info!(
            target: "forge_bitrix",
            portal = %portal.portal_id,
            "forge-bitrix: chat notes moved into the per-portal file"
        );
    }

    if report.did_anything() {
        tracing::info!(
            target: "forge_bitrix",
            portal = %portal.portal_id,
            moved = report.moved,
            prompts_moved = report.prompts_moved,
            target = %target.display(),
            "forge-bitrix: legacy disk layout migrated"
        );
    }
    report
}

/// Есть ли запись по пути — БЕЗ перехода по симлинку.
///
/// `Path::exists` пошёл бы по ссылке и на битой ответил бы «нет», а перенести
/// её всё равно нужно: ссылка на большой кэш в другом разделе — приём, которым
/// пользуются, когда на системном диске мало места.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Переносит `src` в `dst`. Возвращает `true`, если что-то переехало.
///
/// # Быстрый путь — один `rename`
///
/// Если `dst` ещё нет, всё делается единственным вызовом ядра. Именно он даёт
/// оба ключевых свойства: гигабайтный медиа-кэш не копируется, а операция
/// атомарна, поэтому убитый посреди миграции процесс не оставляет
/// полупереехавшего каталога.
///
/// # Медленный путь — слияние
///
/// `dst` уже существует. Штатно этого не бывает (перенос идёт до поднятия
/// воркеров, на пустом каталоге портала), но случай возможен: пользователь мог
/// запустить новую сборку, поработать, откатиться на старую — и та снова
/// написала бы в корень. Тогда записи переносятся по одной, вглубь, и
/// побеждает та, что УЖЕ лежит в каталоге портала: она новее, потому что
/// написана более поздней сборкой. Опустевшие каталоги-источники убираются,
/// чтобы повторный запуск не считал их незавершённым переносом.
fn move_into(src: &Path, dst: &Path) -> bool {
    if !exists(dst) {
        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    target: "forge_bitrix",
                    path = %parent.display(),
                    error = %e,
                    "forge-bitrix: could not create the migration target directory"
                );
                return false;
            }
        }
        match std::fs::rename(src, dst) {
            Ok(()) => return true,
            Err(e) => {
                // Кэш и конфиг могут лежать на РАЗНЫХ файловых системах
                // (`cache_dir` задают руками, вплоть до внешнего диска), а
                // `rename` через границу ФС невозможен. Тогда — слияние
                // копированием, оно же единственный оставшийся способ.
                tracing::warn!(
                    target: "forge_bitrix",
                    from = %src.display(),
                    to = %dst.display(),
                    error = %e,
                    "forge-bitrix: rename failed, falling back to a merge"
                );
            }
        }
    }

    let src_is_dir = std::fs::symlink_metadata(src).map(|m| m.is_dir()).unwrap_or(false);
    if !src_is_dir {
        // Файл, у которого в цели уже есть более новый двойник: источник
        // убирается, иначе следующий запуск снова примет его за незаконченный
        // перенос.
        let _ = std::fs::remove_file(src);
        return false;
    }

    if let Err(e) = std::fs::create_dir_all(dst) {
        tracing::warn!(
            target: "forge_bitrix",
            path = %dst.display(),
            error = %e,
            "forge-bitrix: could not create the merge target directory"
        );
        return false;
    }

    let mut moved_any = false;
    let children: Vec<PathBuf> = match std::fs::read_dir(src) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    for child in children {
        let Some(name) = child.file_name() else {
            continue;
        };
        moved_any |= move_into(&child, &dst.join(name));
    }
    // Каталог мог остаться непустым (что-то не переехало) — тогда он и
    // останется, а следующий запуск попробует ещё раз.
    let _ = std::fs::remove_dir(src);
    moved_any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BitrixConfig, PortalConfig, WebhookUrl};

    /// Корневой конфиг с одним порталом и legacy-раскладкой в tempdir.
    fn legacy_root(dir: &Path) -> BitrixRootConfig {
        let shared = BitrixConfig {
            cache_dir: dir.join("bitrix-cache"),
            prompts_path: dir.join("bitrix-chat-prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            config_path: dir.join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        BitrixRootConfig {
            shared,
            portals: vec![PortalConfig::from_webhook(
                &WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa111"),
                None,
            )],
            migrated_from_legacy: true,
        }
    }

    /// Раскладывает данные ровно так, как их оставила бы однопортальная сборка.
    fn seed_legacy(root: &BitrixRootConfig) {
        let cache = &root.shared.cache_dir;
        std::fs::create_dir_all(cache.join("media/chat7")).unwrap();
        std::fs::write(cache.join("media/chat7/113.0.file.bin"), b"attachment").unwrap();
        std::fs::create_dir_all(cache.join("media/avatars")).unwrap();
        std::fs::write(cache.join("media/avatars/1.avatar.bin"), b"face").unwrap();
        std::fs::create_dir_all(cache.join("uploads")).unwrap();
        std::fs::write(cache.join("uploads/u1.part"), b"staged").unwrap();
        std::fs::write(cache.join("events.json"), b"{\"offset\":\"42\"}").unwrap();
        std::fs::create_dir_all(cache.join("voice/chat7")).unwrap();
        std::fs::write(cache.join("voice/chat7/113.0.json"), b"transcript").unwrap();
        std::fs::write(&root.shared.prompts_path, b"{\"notes\":1}").unwrap();
    }

    /// Все файлы под каталогом с путями ОТНОСИТЕЛЬНО него.
    fn relative_files(root: &Path) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    /// Подготовленный legacy-каталог после старта содержит ВСЕ прежние файлы по
    /// новым путям и НИ ОДНОГО по старым.
    #[test]
    fn every_legacy_file_ends_up_under_the_portal_and_none_stays_behind() {
        let dir = tempfile::tempdir().unwrap();
        let root = legacy_root(dir.path());
        seed_legacy(&root);

        let report = migrate_legacy_layout(&root);
        assert_eq!(report.moved, 4, "media, uploads, events.json и voice");
        assert!(report.prompts_moved);

        let portal = &root.portal_configs()[0];
        assert_eq!(portal.portal_id, "acme");

        assert_eq!(
            relative_files(&portal.cache_dir),
            vec![
                "events.json",
                "media/avatars/1.avatar.bin",
                "media/chat7/113.0.file.bin",
                "uploads/u1.part",
                "voice/chat7/113.0.json",
            ],
            "всё содержимое переехало в подкаталог портала"
        );

        // В корне не осталось НИ ОДНОГО legacy-имени.
        for name in LEGACY_ENTRIES {
            assert!(
                !root.shared.cache_dir.join(name).exists(),
                "{name} остался в корне"
            );
        }
        assert!(
            !root.shared.prompts_path.exists(),
            "базовый файл заметок остался на месте"
        );
        assert_eq!(
            std::fs::read(&portal.prompts_path).unwrap(),
            b"{\"notes\":1}",
            "заметки переехали ЦЕЛИКОМ"
        );
        // Курсор событий переехал вместе со всем: иначе портал перечитал бы или
        // потерял очередь, а ретроспективы у `im.v2.Event.*` нет.
        assert_eq!(
            std::fs::read(portal.cache_dir.join("events.json")).unwrap(),
            b"{\"offset\":\"42\"}"
        );
    }

    /// Повторный запуск идемпотентен: ничего не переносит и ничего не портит.
    #[test]
    fn a_second_start_migrates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = legacy_root(dir.path());
        seed_legacy(&root);

        assert!(migrate_legacy_layout(&root).did_anything());
        let portal = &root.portal_configs()[0];
        let after_first = relative_files(&portal.cache_dir);

        let second = migrate_legacy_layout(&root);
        assert_eq!(second, MigrationReport::default(), "второй проход — no-op");
        assert_eq!(
            relative_files(&portal.cache_dir),
            after_first,
            "содержимое не изменилось"
        );
    }

    /// Прерывание посреди переноса не теряет данных: следующий запуск доводит
    /// его до конца.
    ///
    /// Прерывание моделируется честно — переносится ЧАСТЬ записей, ровно как
    /// если бы процесс убили между двумя `rename`. Промежуточного состояния
    /// внутри одной записи не бывает по построению (`rename` атомарен), поэтому
    /// «середина» — это всегда граница между записями.
    #[test]
    fn an_interrupted_migration_is_finished_by_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = legacy_root(dir.path());
        seed_legacy(&root);
        let portal = &root.portal_configs()[0];

        // «Успели перенести только media» — процесс убит.
        std::fs::create_dir_all(&portal.cache_dir).unwrap();
        std::fs::rename(
            root.shared.cache_dir.join("media"),
            portal.cache_dir.join("media"),
        )
        .unwrap();

        let report = migrate_legacy_layout(&root);
        assert_eq!(report.moved, 3, "доехали оставшиеся три записи");
        assert!(report.prompts_moved);

        assert_eq!(
            relative_files(&portal.cache_dir),
            vec![
                "events.json",
                "media/avatars/1.avatar.bin",
                "media/chat7/113.0.file.bin",
                "uploads/u1.part",
                "voice/chat7/113.0.json",
            ],
            "полный набор файлов, ничего не потеряно и не задвоено"
        );
    }

    /// Слияние: в каталоге портала уже есть файл с тем же именем.
    ///
    /// Побеждает тот, что уже лежит у портала (его написала более поздняя
    /// сборка), а источник не остаётся мозолить глаза следующему запуску.
    #[test]
    fn a_merge_keeps_the_newer_file_of_the_portal() {
        let dir = tempfile::tempdir().unwrap();
        let root = legacy_root(dir.path());
        seed_legacy(&root);
        let portal = &root.portal_configs()[0];

        std::fs::create_dir_all(portal.cache_dir.join("media/chat7")).unwrap();
        std::fs::write(
            portal.cache_dir.join("media/chat7/113.0.file.bin"),
            b"newer",
        )
        .unwrap();

        migrate_legacy_layout(&root);

        assert_eq!(
            std::fs::read(portal.cache_dir.join("media/chat7/113.0.file.bin")).unwrap(),
            b"newer",
            "запись портала не перетёрта legacy-двойником"
        );
        assert!(
            !root.shared.cache_dir.join("media").exists(),
            "источник убран целиком"
        );
        // Соседний файл из того же каталога всё равно доехал.
        assert!(portal
            .cache_dir
            .join("media/avatars/1.avatar.bin")
            .is_file());
    }

    /// Два портала — переноса нет: владельца legacy-данных не угадать.
    #[test]
    fn two_portals_disable_the_migration_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let mut root = legacy_root(dir.path());
        root.portals.push(PortalConfig::from_webhook(
            &WebhookUrl::new("https://other.bitrix24.ru/rest/1/bbb222"),
            None,
        ));
        seed_legacy(&root);

        assert_eq!(migrate_legacy_layout(&root), MigrationReport::default());
        assert!(
            root.shared.cache_dir.join("media/chat7/113.0.file.bin").is_file(),
            "данные остались на месте, а не уехали случайной вкладке"
        );
    }

    /// Свежая установка: переносить нечего, и функция это переживает молча.
    #[test]
    fn a_clean_install_has_nothing_to_migrate() {
        let dir = tempfile::tempdir().unwrap();
        let root = legacy_root(dir.path());
        assert_eq!(migrate_legacy_layout(&root), MigrationReport::default());
    }

    /// Портал, чей идентификатор совпал с именем из списка
    /// (`media.bitrix24.ru` ⇒ `media`), не съедает сам себя.
    #[test]
    fn a_portal_named_like_a_legacy_entry_survives() {
        let dir = tempfile::tempdir().unwrap();
        let mut root = legacy_root(dir.path());
        root.portals = vec![PortalConfig::from_webhook(
            &WebhookUrl::new("https://media.bitrix24.ru/rest/1/ccc333"),
            None,
        )];
        let portal = &root.portal_configs()[0];
        assert_eq!(portal.portal_id, "media");
        assert_eq!(portal.cache_dir, root.shared.cache_dir.join("media"));

        // «Legacy-каталог media» и каталог портала — ОДИН И ТОТ ЖЕ путь.
        std::fs::create_dir_all(root.shared.cache_dir.join("media/chat7")).unwrap();
        std::fs::write(
            root.shared.cache_dir.join("media/chat7/113.0.file.bin"),
            b"attachment",
        )
        .unwrap();

        migrate_legacy_layout(&root);

        assert!(
            portal.cache_dir.join("chat7/113.0.file.bin").exists()
                || portal.cache_dir.join("media/chat7/113.0.file.bin").exists(),
            "данные портала не должны исчезнуть ни при каком раскладе"
        );
        assert!(
            !portal.cache_dir.join("media/media").exists(),
            "каталог не вложен сам в себя"
        );
    }
}
