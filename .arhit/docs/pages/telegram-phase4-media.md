# Фаза 4 плагина Telegram — приём медиа (эпик forge-tg-phase4-cb04)

Входящие фото, видео, файлы, голосовые и стикеры отображаются карточками 360px и открываются: превью в ленте, полный файл по явному действию, лайтбокс, сохранение, плееры.

## Backend (крейт forge-telegram)

| Модуль | Что делает |
|--------|-----------|
| src/media_cache.rs | файловый кэш ~/.forge/telegram-cache/media/<did>/<mid>.<kind>.{bin,meta.json}; атомарная запись (staging в том же каталоге + rename, мета ПОСЛЕ байтов = признак целостности); CacheKey::parse — единственный путь строк из URL в ФС; invalidate; LRU sweep_once + sweeper_loop |
| src/progress.rs | Throttle (100мс/256КиБ, финал и первое событие всегда), ProgressReader<R,F> (для отправки Ф5), DownloadReporter/UploadReporter |
| src/routes/media.rs | GET /api/telegram/media/:did/:mid?kind=thumb\|full: serve_cached (ReaderStream + immutable), inflight-дедуп, download_media (get_messages_by_id + iter_download чанками), pick_thumb_index, content_disposition |
| src/updates.rs | invalidations(events) + media_cache::invalidate на message_edited/message_deleted ДО broadcast |
| src/lib.rs | spawn_workers += media_cache::spawn_sweeper (воркеров стало 3) |

## Frontend

| Файл | Что делает |
|------|-----------|
| static/js/telegram/media.js | buildMediaCard (7 видов), ленивое превью (IntersectionObserver), loadMedia (fetch->blob->objectURL), onDownloadProgress, лайтбокс, saveMedia, playInCard, revokeDialogMedia/dropMessageMedia/teardownMedia |
| static/js/telegram/format.js | ЧИСТАЯ логика: formatBytes, durationLabel, mediaKey, mediaFileName, sanitizeDownloadName, mediaKindLabel, mediaGlyph, mediaMetaLine, progressPercent, progressLabel, canShowThumb, isPlayableKind, isImageKind |
| static/js/telegram/api.js | mediaUrl, fetchMedia (сырой Response + разбор контракта ошибок) |
| static/js/telegram/chat.js | карточка под строкой-подписью, resetMediaCards в renderHistory, revokeDialogMedia при смене диалога, пересборка строки на edited, dropMessageMedia на deleted |
| static/js/telegram/main.js | download_progress -> onDownloadProgress, teardownMedia в teardownTelegram |
| static/css/telegram.css | карточка, волна, оверлей ▶, прогрессбар, плеер, обе темы, prefers-reduced-motion |
| static/js/core/dom.js | + $tgLightboxCaption |

## Инварианты, которые нельзя потерять

1. **Cache-Control: immutable законен только вместе с инвалидацией.** Ответ медиа-роута кэшируется на год; правка/удаление сообщения ОБЯЗАНЫ снести файл (updates.rs) и objectURL во фронте (dropMessageMedia).
2. **Мета пишется ПОСЛЕ байтов.** Наличие .meta.json — признак целостности .bin; иначе клиент получил бы обрезанный файл навсегда.
3. **Строки из URL не попадают в путь.** Только CacheKey::parse (i64/i32 + whitelist kind).
4. **Один Notify на ключ = одна загрузка.** notified().enable() ДО повторной проверки кэша, MAX_INFLIGHT_ATTEMPTS=2, guard'ы на Drop для inflight и staging.
5. **Финальное событие прогресса всегда, с полным total.** final_total подставляет фактический размер, иначе прогрессбар замирает на 97% или остаётся индетерминированным.
6. **Sweeper не трогает то, что качается.** protected_keys: '<did>/<mid>/<kind>' -> '<did>/<mid>.<kind>'.
7. **Фронт грузит ТОЛЬКО через fetch.** У <img>/<video> нет Bearer; прямой src ломается в remote-mode.
8. **objectURL revoke'ается при смене диалога.** Иначе память вкладки растёт до перезагрузки страницы.
9. **CSS-ловушки:** '.tg-media-card [hidden]{display:none!important}' (свой display перебивает UA-правило) и стилизация плеера по вложенности, а не по классу '.tg-media-video' (он же класс карточки).

## Проверки

cargo test --workspace: 537 devforge + 187 forge-telegram + 195 forge-echo + 8 telegram_smoke + прочие — зелёные; clippy -p forge-telegram чист; cargo tree -i openssl-sys/openssl/native-tls/libsql/ece/web-push пусто; node tests/frontend/telegram_media.test.mjs 61/61 и telegram_format.test.mjs 67/67; e2e echo-ui 16/16; рантайм-прогон в браузере на изолированном инстансе (7 карточек, ленивые превью, blob-загрузка, прогресс, лайтбокс+Esc, SAVE с санитизацией, плееры, revoke, XSS-вектор в имени файла обезврежен), скриншоты обеих тем.

## Требует ручной проверки пользователем (нужен реальный аккаунт)

Реальные фото/видео/документы/голосовые из живого диалога; открытие лайтбокса на настоящем JPEG; воспроизведение настоящего mp4/ogg; прогресс на файле 100+ МБ; поведение при FLOOD_WAIT от Telegram; правка сообщения с медиа -> новое содержимое; переполнение кэша выше 512 МиБ и работа sweeper'а на реальных данных.