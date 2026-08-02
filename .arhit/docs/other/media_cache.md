# media_cache

plugins/telegram/src/media_cache.rs (крейт forge-telegram) — файловый кэш принятых медиа Telegram.

РАСКЛАДКА: <cache_dir>/media/<dialog_id>/<message_id>.<kind>.bin (байты) + <message_id>.<kind>.meta.json (MediaMeta: mime/size/file_name/width/height/duration_secs/cached_at) + <message_id>.<kind>.tmp.<uuid> (staging). kind = thumb|full — независимые записи одного сообщения.

ИНВАРИАНТ ЦЕЛОСТНОСТИ: bin пишется ПЕРВЫМ, meta — второй; get() считает попаданием только пару 'непустой bin + разбираемая meta'. Поэтому оборванная запись (остался .tmp, bin без меты, битая мета, пустой bin) НИКОГДА не выдаётся за кэш — иначе клиент получил бы обрезанный файл с Cache-Control: immutable, то есть навсегда. Все записи атомарны: staging в ТОМ ЖЕ каталоге + rename (между ФС rename не атомарен и падает EXDEV, поэтому /tmp не годится).

БЕЗОПАСНОСТЬ ПУТЕЙ: строки из URL в путь не конкатенируются вообще. Единственный конструктор — CacheKey::parse(dialog_id: &str, message_id: &str, kind: Option<&str>) -> Option<CacheKey>: dialog_id разбирается в i64, message_id в i32 (>0), kind через whitelist MediaKind::parse (None/'' => Full, опечатка => None, то есть 400 у роута). '../../etc' не парсится в i64 и отбрасывается до любого обращения к ФС.

API: media_root/dialog_dir/path_for (пути), get (попадание с фактическим size), contains, begin_staging (создаёт каталог, уникальный .tmp путь), commit (rename staging -> bin, затем атомарная мета; при ошибке меты bin удаляется), put_atomic (готовые байты: превью и тесты), invalidate(cfg, dialog_id: &str, message_id) -> usize (удаляет ВСЕ файлы сообщения: оба kind, мету и брошенные .tmp; пустой/непарсящийся dialog_id => обход всех каталогов, потому что удаление в ЛС Telegram не привязывает к диалогу), total_bytes, touch (обновляет mtime через std::fs::File::set_modified в blocking-пуле — 'этим воспользовались только что').

LRU: sweep_once(cfg, protected) сортирует записи по mtime (старые первыми) и удаляет пары bin+meta, пока размер не опустится ниже sweep_target = 90% max_cache_bytes (гистерезис против подчистки каждый тик). protected — ключи активных загрузок из state.media_inflight ('<did>/<mid>/<kind>' преобразуется в '<did>/<mid>.<kind>'): удалять качающийся файл значит отдать клиенту пустоту. Заодно удаляются staging-файлы старше STAGING_TTL (1ч) — мусор упавшего процесса. sweeper_loop: прогон на старте + раз в SWEEP_INTERVAL (10 мин), выход по shutdown-токену; spawn_sweeper регистрирует воркера в state.workers.

СВЯЗИ: config.rs (cache_dir, max_cache_bytes), state.rs (media_inflight, shutdown, register_worker), routes/media.rs (get/put/commit/touch на горячем пути), updates.rs (invalidate при message_edited/message_deleted), lib.rs::spawn_workers (spawn_sweeper). Тесты (15) покрывают round-trip, все четыре вида оборванной записи, инвалидацию с dialog_id и без, параллельные put_atomic одного ключа, path traversal, LRU с protected, staging TTL, touch.
