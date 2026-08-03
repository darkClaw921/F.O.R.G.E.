МОДЕЛЬ МЕДИА ПЛАГИНА VK (Ф7) — как байты вложения доезжают от VK до вкладки.

  строка ленты (chat.js)
    -> renderAttachmentGroup(msg)            N карточек, cap MAX_INLINE_CARDS = 6
      -> buildMediaCard(msg, att)            одна карточка на ВЛОЖЕНИЕ
        -> loadMedia(dialog, mid, index, kind)
          -> GET /api/vk/media/:peer/:mid/:index?kind=thumb|full   (прокси бэкенда, Bearer)
            -> media_cache::get                                    ПОПАДАНИЕ: ни одного вызова VK
            -> fetch_cached (дедуп по ключу)
              -> resolve_message (дедуп по СООБЩЕНИЮ) -> messages.getById
              -> attach::url_for(att, kind)  -> pick_size
              -> VkClient::get_stream -> staging -> media_cache::commit
          -> blob -> objectURL -> vkState.mediaCache[<peer>/<mid>/<idx>/<kind>]

ЧЕТЫРЕ РЕШЕНИЯ, КОТОРЫЕ НЕЛЬЗЯ ПОТЕРЯТЬ:

1. ИНДЕКС ВЛОЖЕНИЯ ВХОДИТ В КЛЮЧ ВЕЗДЕ: маршрут, CacheKey, имя файла на диске, mediaKey во фронте, кадр download_progress. У сообщения VK до десяти вложений; ключ без индекса означает, что второе фото затирает первое и на диске, и в objectURL-учёте.

2. URL ВЛОЖЕНИЙ VK НЕ КЭШИРУЮТСЯ НИКОГДА — кэшируются только байты. Ссылки подписанные и протухающие; адрес добывается непосредственно перед скачиванием, поэтому просроченной ссылки в системе не бывает. 403/404/410 от CDN -> ОДИН повтор с новым getById -> 404 no_media.

3. ДЕДУП ДВУХ УРОВНЕЙ (лимит 3 rps): по ключу кэша И по сообщению. Галерея из десяти вложений обязана стоить ОДНОГО getById.

4. ПРЯМЫХ АДРЕСОВ VK В DOM НЕ БЫВАЕТ: ни один att.url не попадает в <img src>/<a href>. Байты идут через прокси (у <img> нет Bearer, в remote-mode прямой src молча ломается), единственная внешняя ссылка — страница плеера видео — проходит через safeUrl и открывается с noopener,noreferrer. То же правило действует для картинки captcha.

ЧЕСТНЫЕ ОГРАНИЧЕНИЯ ПЛАТФОРМЫ: вложение video не содержит воспроизводимого файла (есть только player_url); у audio ссылки может не быть вовсе (правообладатели); у голосового нет превью; карточки other (ссылка, опрос, товар, запись на стене) не имеют ни файла, ни превью — только заголовок.

ГДЕ ЧТО ЛЕЖИТ: plugins/vk/src/media_cache.rs (кэш и LRU), plugins/vk/src/attach.rs (pick_size, url_for), plugins/vk/src/routes/media.rs (роут и оба дедупа), plugins/vk/src/progress.rs (кадры прогресса), tmux-web/static/js/vk/media.js (карточки, галерея, плеер), tmux-web/tests/frontend/vk_media.test.mjs, plugins/vk/tests/vk_stub.rs (гейты фазы).