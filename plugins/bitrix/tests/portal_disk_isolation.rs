//! ТЕСТ-СТРАЖ ИЗОЛЯЦИИ ПОРТАЛОВ НА ДИСКЕ (Ф6).
//!
//! # Что охраняется
//!
//! Два портала с ОДИНАКОВЫМИ `dialog_id` и `message_id` не пересекаются НИ
//! ОДНИМ файлом. Соседний тест-страж `portal_isolation.rs` охраняет то же
//! свойство на уровне HTTP (запрос порталу A не видит данных портала B); здесь
//! проверяется его дисковая половина — та, что переживает перезапуск процесса.
//!
//! # Почему совпадающие идентификаторы — не искусственный случай
//!
//! Идентификаторы у разных порталов НЕЗАВИСИМЫ: `chat7` есть и там, и там, а
//! `message_id` растёт от единицы у каждого портала свой. До Ф6 всё это
//! адресовалось БЕЗ портала, и два портала честно перетирали файлы друг друга.
//!
//! # Что именно ломается, если тест упадёт
//!
//! 1. **Медиа.** Вложение портала A отдаётся под именем портала B — то есть
//!    панель показывает чужой файл, и никакой ошибки при этом нет.
//! 2. **Курсор событий.** Портал B прочитал курсор портала A и решил, что уже
//!    всё видел. `im.v2.Event.*` ретроспективы не имеет: пропущенное не
//!    вернётся никогда.
//! 3. **Staging загрузок.** GC одного портала метёт `.part`-файлы соседа прямо
//!    посреди его отправки.
//! 4. **Заметки и разборы анализа.** Приватные заметки о собеседниках одного
//!    портала видны на другом.
//!
//! # Герметичность
//!
//! Сети нет вовсе: тест работает с путями и файлами. Ни `~/.forge`, ни
//! `~/.config/forge` не трогаются — общий корень кэша и база файла заметок
//! уводятся в tempdir.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use forge_bitrix::config::{
    portal_dir_component, BitrixConfig, BitrixRootConfig, PortalConfig, WebhookUrl,
};
use forge_bitrix::media_cache::{self, CacheKey, MediaMeta};

/// Диалог, который есть у ОБОИХ порталов.
const DIALOG: &str = "chat7";

/// Сообщение, номер которого есть у ОБОИХ порталов.
const MESSAGE_ID: &str = "113";

/// Общие настройки с корнем кэша и базой заметок в tempdir.
fn shared_cfg(dir: &Path) -> BitrixConfig {
    BitrixConfig {
        cache_dir: dir.join("bitrix-cache"),
        prompts_path: dir.join("bitrix-chat-prompts.json"),
        claude_cli_path: dir.join("no-such-claude"),
        config_path: dir.join("bitrix.toml"),
        ..BitrixConfig::default()
    }
}

/// Корневой конфиг с двумя порталами — ровно так его собирает загрузка файла.
fn two_portals(dir: &Path) -> BitrixRootConfig {
    let portals = vec![
        PortalConfig::from_webhook(&WebhookUrl::new("https://alpha.bitrix24.ru/rest/1/aaa111"), None),
        PortalConfig::from_webhook(&WebhookUrl::new("https://beta.bitrix24.ru/rest/1/bbb222"), None),
    ];
    BitrixRootConfig {
        shared: shared_cfg(dir),
        portals,
        migrated_from_legacy: false,
    }
}

/// Ключ вложения — ОДИН И ТОТ ЖЕ для обоих порталов.
fn key() -> CacheKey {
    CacheKey::parse_file(DIALOG, MESSAGE_ID, "0").expect("ключ вложения разбирается")
}

/// Все файлы под каталогом, рекурсивно.
fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.insert(path);
        }
    }
    out
}

/// ГЛАВНЫЙ КРИТЕРИЙ ФАЗЫ: при совпадающих `dialog_id` и `message_id` два
/// портала не делят НИ ОДНОГО файла.
///
/// Проверяется не арифметикой путей, а фактом на диске: оба портала реально
/// пишут вложение, курсор, staging и заметки, после чего множества их файлов
/// сверяются на пересечение.
#[tokio::test]
async fn two_portals_share_no_file_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let root = two_portals(dir.path());
    let cfgs = root.portal_configs();
    assert_eq!(cfgs.len(), 2, "два портала в конфиге — два конфига");
    let (a, b) = (&cfgs[0], &cfgs[1]);
    assert_ne!(a.portal_id, b.portal_id, "порталы различимы");

    let meta = MediaMeta::default();
    for (cfg, bytes) in [(a, b"alpha bytes".as_slice()), (b, b"beta bytes".as_slice())] {
        // Медиа-кэш: одно и то же вложение одного и того же сообщения.
        media_cache::put_atomic(cfg, &key(), bytes, &meta)
            .await
            .expect("вложение записано");
        // Курсор событий.
        forge_bitrix::events::save_cursor(
            &forge_bitrix::events::cursor_path(cfg),
            &forge_bitrix::state::EventCursor {
                offset: String::from_utf8_lossy(bytes).into_owned(),
                saved_at: 1_700_000_000,
            },
        );
        // Staging загрузки: один и тот же `upload_id` (UUID совпасть не может,
        // но GC соседа не должен зависеть от того, совпал он или нет).
        let staging = forge_bitrix::routes::upload::staging_path(cfg, "same-upload-id");
        std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
        std::fs::write(&staging, bytes).unwrap();
        // Расшифровка голосового и заметки анализа.
        std::fs::create_dir_all(cfg.cache_dir.join("voice")).unwrap();
        std::fs::write(cfg.cache_dir.join("voice").join("t.json"), bytes).unwrap();
        std::fs::write(&cfg.prompts_path, bytes).unwrap();
    }

    let files_a: BTreeSet<PathBuf> = files_under(&a.cache_dir);
    let files_b: BTreeSet<PathBuf> = files_under(&b.cache_dir);
    assert!(!files_a.is_empty() && !files_b.is_empty(), "оба портала писали");
    let shared: Vec<&PathBuf> = files_a.intersection(&files_b).collect();
    assert!(
        shared.is_empty(),
        "порталы делят файлы: {shared:?}"
    );
    assert_ne!(
        a.prompts_path, b.prompts_path,
        "заметки и разборы — отдельный файл на портал"
    );

    // Содержимое не перетёрто: каждый портал читает СВОЁ.
    let got_a = media_cache::get(a, &key()).await.expect("вложение портала A на месте");
    let got_b = media_cache::get(b, &key()).await.expect("вложение портала B на месте");
    assert_eq!(std::fs::read(&got_a.path).unwrap(), b"alpha bytes");
    assert_eq!(std::fs::read(&got_b.path).unwrap(), b"beta bytes");
    assert_eq!(std::fs::read(&a.prompts_path).unwrap(), b"alpha bytes");
    assert_eq!(std::fs::read(&b.prompts_path).unwrap(), b"beta bytes");
}

/// Курсор портала A не читается порталом B.
///
/// Отдельным тестом, а не строкой в предыдущем: последствие здесь особенное.
/// Прочитав чужой курсор, портал решает, что очередь уже подтверждена, и
/// пропущенные события не вернутся — у `im.v2.Event.*` нет ретроспективы.
#[tokio::test]
async fn the_events_cursor_of_one_portal_is_invisible_to_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let cfgs = two_portals(dir.path()).portal_configs();
    let (a, b) = (&cfgs[0], &cfgs[1]);

    let now = 1_700_000_000;
    let path_a = forge_bitrix::events::cursor_path(a);
    let path_b = forge_bitrix::events::cursor_path(b);
    assert_ne!(path_a, path_b, "у каждого портала свой файл курсора");

    forge_bitrix::events::save_cursor(
        &path_a,
        &forge_bitrix::state::EventCursor {
            offset: "alpha-offset".into(),
            saved_at: now,
        },
    );

    assert_eq!(
        forge_bitrix::events::load_cursor(&path_a, now).map(|c| c.offset),
        Some("alpha-offset".to_string()),
        "свой курсор портал A читает"
    );
    assert!(
        forge_bitrix::events::load_cursor(&path_b, now).is_none(),
        "портал B обязан уйти в холодный старт, а не подхватить чужое подтверждение"
    );
}

/// Голосовое с ОДИНАКОВЫМ `message_id` на двух порталах даёт две независимые
/// расшифровки.
///
/// Стор расшифровок висит на `cache_dir` (`<cache_dir>/voice/`), поэтому
/// разведение каталогов развело и его — общий крейт `forge_voice` при этом не
/// менялся ни строкой, и расшифровки VK с Telegram остались там же, где были.
#[tokio::test]
async fn the_same_voice_message_id_yields_independent_transcripts() {
    use forge_voice::store::{Engine, Transcript, VoiceKey};

    let dir = tempfile::tempdir().unwrap();
    let cfgs = two_portals(dir.path()).portal_configs();
    let state_a = forge_bitrix::state::B24State::new(cfgs[0].clone());
    let state_b = forge_bitrix::state::B24State::new(cfgs[1].clone());

    let (store_a, store_b) = (state_a.voice_store(), state_b.voice_store());
    assert_ne!(store_a.root(), store_b.root(), "у каждого портала свой стор");

    // Один и тот же ключ: и диалог, и номер сообщения совпадают.
    let key = VoiceKey::new(DIALOG, MESSAGE_ID, 0);
    store_a
        .put(&key, &Transcript::new("отчёт по альфе", Engine::Local))
        .expect("расшифровка портала A сохранена");

    assert_eq!(
        store_a.get(&key).map(|t| t.text),
        Some("отчёт по альфе".to_string())
    );
    assert!(
        store_b.get(&key).is_none(),
        "портал B обязан расшифровывать своё голосовое сам, а не показывать чужой текст"
    );

    store_b
        .put(&key, &Transcript::new("отчёт по бете", Engine::Local))
        .expect("расшифровка портала B сохранена");
    assert_eq!(
        store_a.get(&key).map(|t| t.text),
        Some("отчёт по альфе".to_string()),
        "запись соседа не перетёрла нашу"
    );
    assert_eq!(
        store_b.get(&key).map(|t| t.text),
        Some("отчёт по бете".to_string())
    );
    assert_ne!(store_a.path(&key), store_b.path(&key), "это два разных файла");
}

/// Заметка и разбор, сделанные на портале A, не видны на портале B при
/// СОВПАДАЮЩЕМ `dialog_id`.
///
/// Проверяется через настоящее состояние плагина, а не через арифметику путей:
/// заметку кладёт `set_chat_prompt` (он же пишет файл), а читает её второй
/// портал, поднятый со своим конфигом. До Ф6 оба состояния грузили ОДИН файл, и
/// приватная заметка о собеседнике из рабочего портала показывалась в личном.
#[tokio::test]
async fn a_chat_note_made_on_one_portal_is_invisible_on_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let cfgs = two_portals(dir.path()).portal_configs();
    let (a, b) = (cfgs[0].clone(), cfgs[1].clone());
    assert_ne!(a.prompts_path, b.prompts_path, "файл заметок — на портал");

    let state_a = forge_bitrix::state::B24State::new(a.clone());
    let state_b = forge_bitrix::state::B24State::new(b.clone());

    state_a
        .set_chat_prompt(DIALOG, "торгуется жёстко, скидку не давать", 1_700_000_000)
        .await
        .expect("заметка сохранена");

    assert!(
        state_a.chat_prompt(DIALOG).await.is_some(),
        "свою заметку портал A видит"
    );
    assert!(
        state_b.chat_prompt(DIALOG).await.is_none(),
        "портал B не имеет права видеть чужую заметку о том же dialog_id"
    );

    // И на диске это ДВА файла, из которых написан ровно один.
    assert!(a.prompts_path.is_file(), "файл портала A создан");
    assert!(
        !b.prompts_path.exists(),
        "портал B ничего не писал — его файла ещё нет"
    );

    // Портал B, поднятый заново, по-прежнему пуст: дело в пути, а не в кэше в
    // памяти.
    let restarted_b = forge_bitrix::state::B24State::new(b.clone());
    assert!(restarted_b.chat_prompt(DIALOG).await.is_none());
}

/// Стартовая уборка сирот метёт ТОЛЬКО свой подкаталог.
///
/// Первым прогоном на старте GC считает сиротой КАЖДЫЙ `.part` в своём
/// `uploads/`: реестр загрузок живёт в памяти, и после рестарта сослаться на
/// файл уже некому. Пока каталог был общим, это было безобидно — портал в
/// процессе был один. С двумя порталами тот же проход стёр бы staging соседа
/// прямо посреди его отправки, причём тихо: пользователь увидел бы только
/// «этот upload_id неизвестен».
#[tokio::test]
async fn the_startup_upload_sweep_never_reaches_the_neighbour_staging() {
    let dir = tempfile::tempdir().unwrap();
    let cfgs = two_portals(dir.path()).portal_configs();
    let (a, b) = (&cfgs[0], &cfgs[1]);

    let mut staged = Vec::new();
    for cfg in [a, b] {
        let path = forge_bitrix::routes::upload::staging_path(cfg, "u1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"payload").unwrap();
        staged.push(path);
    }

    // Стартует только портал A.
    let removed = forge_bitrix::routes::upload::cleanup_uploads_dir(a).await;
    assert_eq!(removed, 1, "портал A убрал СВОЮ сироту");
    assert!(!staged[0].exists(), "свой staging убран");
    assert!(
        staged[1].is_file(),
        "staging соседа не имеет права исчезнуть: он ещё в его памяти"
    );
}

/// `portal_id` попадает в путь на диске, поэтому он — источник path traversal.
///
/// [`portal_dir_component`] обязана возвращать ОДИН безопасный компонент:
/// никаких разделителей, никаких `.` и `..`. Тест держит это свойство
/// независимо от того, что сегодня делает `normalize_portal_id`: правка
/// нормализации («разрешим точку в имени портала») не должна превращать
/// подкаталог кэша в дыру.
#[test]
fn a_hostile_portal_id_can_never_escape_the_cache_root() {
    let root = Path::new("/tmp/forge-cache-root");
    let hostile = [
        "..",
        ".",
        "../..",
        "../../etc/passwd",
        "/etc/passwd",
        "a/b",
        "a\\b",
        "..\\..\\windows",
        "....//....//",
        "",
        "   ",
        "\0",
        "\u{202e}nwod-edispu",
    ];
    for raw in hostile {
        let component = portal_dir_component(raw);
        assert!(!component.is_empty(), "компонент пуст для {raw:?}");
        assert!(!component.contains('/'), "разделитель в {component:?}");
        assert!(!component.contains('\\'), "разделитель в {component:?}");
        assert!(!component.contains('\0'), "NUL в {component:?}");
        assert_ne!(component, ".", "точка как компонент для {raw:?}");
        assert_ne!(component, "..", "выход наверх для {raw:?}");
        assert_eq!(
            Path::new(&component).components().count(),
            1,
            "ровно один компонент пути для {raw:?}, получено {component:?}"
        );

        let dir = root.join(&component);
        assert!(
            dir.starts_with(root),
            "каталог {dir:?} ушёл из-под корня на {raw:?}"
        );
        assert_eq!(
            dir.parent(),
            Some(root),
            "каталог {dir:?} не лежит НЕПОСРЕДСТВЕННО в корне ({raw:?})"
        );
    }
}

/// Разведение по порталам ИДЕМПОТЕНТНО.
///
/// `for_portal` зовут и при сборке реестра из файла, и при подключении портала
/// на живом процессе, причём «общие настройки» во втором случае могут прийти из
/// уже разведённого конфига (реестр, собранный поверх готового состояния).
/// Вложись подкаталог в подкаталог — портал после перезапуска не нашёл бы ни
/// медиа, ни курсора.
#[test]
fn portalising_a_config_twice_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let shared = shared_cfg(dir.path());
    let webhook = WebhookUrl::new("https://alpha.bitrix24.ru/rest/1/aaa111");

    let once = shared.for_portal("alpha", "Альфа", &webhook);
    let twice = once.for_portal("alpha", "Альфа", &webhook);
    assert_eq!(once.cache_dir, twice.cache_dir);
    assert_eq!(once.prompts_path, twice.prompts_path);
    assert_eq!(
        once.cache_dir,
        shared.cache_dir.join("alpha"),
        "подкаталог портала лежит НЕПОСРЕДСТВЕННО в общем корне"
    );

    // Смена портала на уже разведённом конфиге отсчитывается от того же корня,
    // а не от каталога предыдущего портала.
    let moved = once.for_portal("beta", "Бета", &webhook);
    assert_eq!(moved.cache_dir, shared.cache_dir.join("beta"));
    assert_eq!(moved.prompts_path, twice.prompts_path.with_file_name("bitrix-chat-prompts-beta.json"));
}

/// Пустой слот (свежая установка) тоже получает СВОЙ каталог.
///
/// Писать ему почти нечего — без вебхука портал не ходит в сеть, — но каталог
/// обязан быть определён: иначе экран подключения писал бы в общий корень,
/// вперемешку с подкаталогами настоящих порталов, и первая же миграция или
/// подчистка приняла бы его файлы за legacy-данные.
#[test]
fn the_vacant_slot_gets_its_own_reserved_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = BitrixRootConfig {
        shared: shared_cfg(dir.path()),
        portals: Vec::new(),
        migrated_from_legacy: false,
    };
    let vacant = root.primary();
    assert!(vacant.portal_id.is_empty(), "у пустого слота нет идентификатора");
    assert_eq!(
        vacant.cache_dir,
        root.shared.cache_dir.join("_vacant"),
        "пустой слот не пишет в общий корень"
    );
    assert_ne!(
        vacant.cache_dir, root.shared.cache_dir,
        "каталог пустого слота ОТЛИЧЕН от общего — на этом держится purge_cache"
    );
    assert!(
        vacant.cache_dir.starts_with(&root.shared.cache_dir),
        "и лежит ВНУТРИ общего"
    );
}
