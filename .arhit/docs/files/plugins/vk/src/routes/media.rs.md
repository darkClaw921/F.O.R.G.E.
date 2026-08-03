# plugins/vk/src/routes/media.rs

plugins/vk/src/routes/media.rs (Ф7) — отдача байтов вложений: GET /api/vk/media/:peer_id/:message_id/:index?kind=thumb|full.

ПОРЯДОК ПРОВЕРОК (обязателен и покрыт тестами):
  1. CacheKey::parse                 -> 400 invalid_input
  2. require_authorized              -> 409 (соединение НЕ требуется — см. шаг 3)
  3. media_cache::get -> HIT         -> отдать БЕЗ единого вызова VK
  4. require_connected               -> 503
  5. fetch_cached (дедуп по КЛЮЧУ) -> download_attachment:
       resolve_message (getById, дедуп по СООБЩЕНИЮ) -> attach::url_for -> get_stream в staging -> commit
  6. serve_cached(hit, key, touch_mtime)

ДЕДУП ДВУХ УРОВНЕЙ — добавление против Telegram, продиктованное лимитом 3 rps:
  * уровень ключа  — VkState::media_inflight (копия телеграмного acquire/Slot/InflightGuard);
  * уровень СООБЩЕНИЯ — VkState::message_inflight + короткоживущий message_cache (TTL 60 с, ёмкость 256). Без него галерея из десяти фото выстрелила бы десятью ОДИНАКОВЫМИ getById, то есть 3+ секундами троттлинга на одну картинку. Гейт фазы — vk_stub::a_gallery_costs_exactly_one_get_by_id.

ПРОТУХШАЯ ССЫЛКА: 403/404/410 от CDN даёт РОВНО ОДИН повтор с ЗАНОВО добытым адресом (forget_message + fetch_attachments), после чего 404 no_media. Повторять со старым адресом бессмысленно — он уже отвергнут.

СОСТАВ: router, get_media, kind_param (ручной разбор query — как во всех роутах плагина), serve_cached (ReaderStream + immutable Cache-Control + Content-Disposition), content_disposition/sanitize_ascii/percent_encode (имя файла приходит от чужого аккаунта), InflightGuard/Slot/acquire/fetch_cached, MessageGuard/acquire_message/resolve_message/fetch_attachments, StagingGuard, Attempt, download_attachment/try_download, stream_to_file, is_expired_link, final_total.

ЧЕГО НЕТ: Accept-Ranges. Range не поддержан, и анонсировать его нельзя — часть браузеров, увидев Accept-Ranges, шлёт Range и ломается на ответе 200.
