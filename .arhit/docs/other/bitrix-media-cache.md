# bitrix-media-cache

plugins/bitrix/src/media_cache.rs — файловый кэш принятых байтов: вложения и аватары.

РАСКЛАДКА
  <cache_dir>/media/<dialog>/<message_id>.<index>.file.{bin,meta.json,tmp.<uuid>}
  <cache_dir>/media/avatars/<user_id>.avatar.{bin,meta.json}
<dialog> — каноническая форма идентификатора Битрикс24 (chat27 / sg12 / 19). Каталог avatars с каталогом диалога столкнуться не может: имя диалога либо одни цифры, либо начинается с chat/sg.

ДВА ВИДА, А НЕ ДВА РАЗМЕРА. MediaKind::{File, Avatar}. Отдельного thumb НЕТ: im.v2.File.download отдаёт ОРИГИНАЛ, а mediaUrl.preview["250"] без сессионной куки не работает. has_thumb в MediaDto значит «портал считает файл картинкой», а не «есть маленький файл».

КЛЮЧ ПАРСИТСЯ В ЧИСЛА — path traversal невозможен структурно.
  DialogPrefix — перечисление из ТРЁХ вариантов (User/Chat/Group);
  DialogKey { prefix, number: i64 > 0 }, DialogKey::parse / dir_name;
  CacheKey::File { dialog, message_id: i64 > 0, index: u32 <= MAX_ATTACH_INDEX(19) };
  CacheKey::Avatar { user_id: i64 > 0 };
  parse_entity_id / parse_attach_index — только ASCII-цифры, положительные, без ведущего '+', без null-байтов и не-ASCII-цифр.
Имя каталога собирается format!-ом из варианта перечисления и числа: строка из URL в путь не конкатенируется вообще. Ведущие нули нормализуются (0027 и 27 — один путь).

CacheKey::id() даёт ключ дедупа и адрес карточки во фронте: <dialog>/<mid>/<index> у вложения, avatar/<user_id> у аватара. Формат дословно совпадает с полем key кадра download_progress и с format.js::mediaKey.

ЦЕЛОСТНОСТЬ. Запись двухступенчатая: сначала bin (rename из staging в том же каталоге), потом meta. get() считает попаданием ТОЛЬКО пару «непустой bin + разбираемая meta» — оборванная запись не выдаётся за валидный кэш, иначе клиент получил бы обрезанный файл с Cache-Control: immutable, то есть навсегда. begin_staging / commit / put_atomic / write_atomic.

МЕТА (MediaMeta): mime, size, file_name, width, height, cached_at и source_tag — ОТПЕЧАТОК источника (source_tag(), FNV-1a, 16 hex). Сам адрес на диск не ложится никогда: downloadUrl содержит код вебхука, а отпечаток в URL не восстанавливается. Своя хеш-функция, а не DefaultHasher: тот не стабилен между версиями компилятора, и после обновления toolchain весь кэш аватаров молча протух бы.

ИНВАЛИДАЦИЯ. invalidate(cfg, dialog, message_id) — все вложения сообщения плюс брошенный staging; префикс сравнивается ВМЕСТЕ С ТОЧКОЙ, иначе удаление сообщения 4 снесло бы 42 и 420. Неразбираемый dialog — скан всех каталогов ДИАЛОГОВ (avatars не трогается). invalidate_avatar(cfg, user_id) — отдельно. Зовут: events::invalidate_media на ONIMV2MESSAGEDELETE и recent_sync::sync_once на обнаруженной правке. Без них Cache-Control: immutable у роута вложений был бы багом навсегда.

LRU. sweep_once(cfg, protected): удаление по mtime от старых к новым до 90 % лимита (SWEEP_TARGET_NUM/DEN — гистерезис). max_cache_bytes == 0 означает «не подрезать». touch() обновляет mtime на попадании, поэтому горячие файлы всплывают. protected — записи, которые сейчас качаются (перевод ключа inflight в имя записи делает file_id_of_inflight, покрыт тестом: разъедься он, sweeper удалял бы файлы во время скачивания). Брошенный staging старше STAGING_TTL(1ч) удаляется; возраст «из будущего» (NTP-коррекция) считается нулевым. sweeper_loop / spawn_sweeper — воркер стартует ВСЕГДА, даже без вебхука: медиа переживают logout.

22 юнит-теста, среди них parse_rejects_path_traversal, dialog_rule_matches_the_dto_parser, message_id_rule_matches_the_route, interrupted_write_is_not_a_hit, invalidate_matches_whole_message_id_not_decimal_prefix, sweep_drops_oldest_and_keeps_inflight.
