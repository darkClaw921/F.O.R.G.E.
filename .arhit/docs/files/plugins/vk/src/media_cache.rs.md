# plugins/vk/src/media_cache.rs

plugins/vk/src/media_cache.rs (крейт forge-vk, Ф7) — файловый кэш принятых вложений VK.

РАСКЛАДКА НА ДИСКЕ:
  <cache_dir>/media/<peer_id>/<message_id>.<index>.<kind>.bin        — байты
  <cache_dir>/media/<peer_id>/<message_id>.<index>.<kind>.meta.json  — метаданные
  <cache_dir>/media/<peer_id>/<message_id>.<index>.<kind>.tmp.<uuid> — staging

ЕДИНСТВЕННОЕ СОДЕРЖАТЕЛЬНОЕ ОТЛИЧИЕ ОТ plugins/telegram/src/media_cache.rs — ИНДЕКС ВЛОЖЕНИЯ в ключе. У сообщения Telegram вложение одно, у сообщения VK их до десяти (attach::MAX_ATTACHMENTS), и ключ <peer>/<mid>.<kind> два фото одного сообщения не различил бы: второе затёрло бы первое и на диске, и в objectURL-учёте вкладки.

ЧЕГО ЗДЕСЬ НЕТ: URL вложений VK не кэшируются НИКОГДА — только байты. Ссылки VK подписанные и протухающие, адрес добывается непосредственно перед скачиванием (routes/media.rs::resolve_message), поэтому просроченной ссылки в системе не бывает: её негде хранить.

СОСТАВ:
  MediaKind {Thumb, Full} + parse(Option<&str>) — белый список query-параметра kind; опечатка это 400, а не «наверное, full».
  CacheKey {peer_id: i64, message_id: i32, index: u8, kind} — ЕДИНСТВЕННЫЙ путь строк из URL в файловую систему.
  MAX_ATTACH_INDEX = 9, MEDIA_SUBDIR, SWEEP_INTERVAL (10 мин), SWEEP_TARGET_NUM/DEN (гистерезис 90 %), STAGING_TTL (1 ч).
  MediaMeta {mime, size, file_name, width, height, duration_secs, cached_at} — сайдкар; ЕГО НАЛИЧИЕ и есть признак целостности bin.
  get / contains — попаданием считается только пара «непустой bin + читаемая meta».
  begin_staging -> commit — запись в том же каталоге + rename; мета пишется ПОСЛЕ байтов.
  put_atomic — готовые байты одним вызовом (превью, тесты).
  invalidate(cfg, peer_id, message_id) — сносит ВСЕ индексы и оба вида сообщения; зовётся из longpoll.rs на правку/удаление (ответ роута помечен Cache-Control: immutable, без инвалидации картинка осталась бы старой навсегда).
  total_bytes, touch, sweep_target, sweep_once, sweeper_loop, spawn_sweeper — LRU по mtime с гистерезисом; активные загрузки защищены ключами из VkState::media_inflight.

БЕЗОПАСНОСТЬ: CacheKey::parse разбирает идентификаторы в числа по ТЕМ ЖЕ правилам, что dto::parse_dialog_id и routes::dialogs::parse_message_id (ведущий + отвергается — одно вложение, одно написание адреса). Строки в путь не конкатенируются вообще, тест parse_rejects_path_traversal перенесён с телеграмного и расширен индексом.
