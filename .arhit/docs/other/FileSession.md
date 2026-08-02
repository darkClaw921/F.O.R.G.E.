# FileSession

plugins/telegram/src/session.rs — собственное хранилище MTProto-сессии плагина Telegram (реализация trait grammers_session::Session).

ЗАЧЕМ СВОЁ (план Б, риск №1 плана СРАБОТАЛ). Штатный SqliteSession из grammers-session (feature sqlite-storage) тянет libsql, который статически линкует свой SQLite. В бинаре devforge уже есть SQLite от rusqlite (features=["bundled"], плагин Echo). Символы sqlite3_* совпадают, линковщик оставляет одну реализацию, и libsql падает по assert_eq! на sqlite3_config(SQLITE_CONFIG_SERIALIZED)==SQLITE_MISUSE(21), потому что Echo уже вызвал sqlite3_initialize() на старте (libsql-0.9.30/src/local/database.rs:323). Воспроизводится в реальном devforge start --telegram на первом же POST /login/start: хендлер логина умирает, соединение обрывается. Обойти порядком инициализации нельзя — Echo стартует раньше. Поэтому grammers-session подключён как default-features=false, features=["serde"], libsql убран из дерева полностью (cargo tree -i libsql пусто), двойного SQLite больше нет.

ФОРМАТ. Состояние сериализуется в JSON через приватную структуру PersistedSession{version,home_dc,dc_options,peers,updates_state} (SessionData из grammers не выводит serde, поэтому нужна своя плоская проекция; peers вектором, а не map, потому что PeerId — плохой ключ JSON-объекта, а PeerInfo и так несёт свой id). Поле version=FORMAT_VERSION=1 оставлено под будущие миграции.

БЕЗОПАСНОСТЬ. В файле лежит auth_key аккаунта — фактический ключ от переписки. Запись атомарная (tmp в том же каталоге + fsync + rename) с правами 0600, через общие с config.rs примитивы write_secret_file/set_owner_only/tmp_path_for.

ТРОТТЛИНГ ЗАПИСИ. set_home_dc_id и set_dc_option пишут файл НЕМЕДЛЕННО (flush_critical) — там появляется auth-key, потеря которого потребовала бы повторного логина, а это FLOOD_WAIT. cache_peer и set_update_state вызываются на каждом апдейте, поэтому помечают состояние dirty и пишут не чаще FLUSH_THROTTLE=2с (flush_throttled). flush_now() дожимает отложенное и вызывается из TgRuntime::close() на shutdown.

ОБРАБОТКА СБОЕВ. Отсутствие файла — штатная новая сессия. БИТЫЙ файл тоже не фатален: warn + новая сессия, потому что восстановить auth-key из повреждённого JSON всё равно нельзя, а падать на старте нельзя тем более — иначе пользователь не сможет войти заново. Ошибка FileSessionError{Poisoned,Io,Serde} реализует Error+Send+Sync (требование SenderPool::new к Session::Error).

МЕТОДЫ TRAIT. home_dc_id/dc_option — синхронные и дешёвые (вызываются на каждом RPC), читают из Mutex<Inner>. Остальные возвращают BoxFuture; MutexGuard никогда не живёт через await, поэтому фьючи остаются Send. into_session_data стартует с SessionData::default() (там статически известные DC, которые trait обязан отдавать без персиста) и накладывает персист сверху.

ТЕСТЫ (6): дефолты новой сессии, немедленный персист auth-key и его перечитывание, права 0600 + отсутствие tmp-мусора, round-trip peers/updates_state через flush_now, битый файл не роняет старт, склейка частых мутаций троттлингом.
