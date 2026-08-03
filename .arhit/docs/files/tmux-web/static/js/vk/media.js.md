# tmux-web/static/js/vk/media.js

tmux-web/static/js/vk/media.js (Ф7) — карточки вложений, загрузка, лайтбокс-галерея, плеер.

ЧЕМ ОТЛИЧАЕТСЯ ОТ js/telegram/media.js:
  1. У сообщения VK вложений до десяти: buildMediaCard(msg, att) принимает ВЛОЖЕНИЕ, renderAttachmentGroup(msg) рисует N карточек (потолок MAX_INLINE_CARDS = 6, хвост «…ещё N»), ключ кэша несёт ИНДЕКС — mediaKey(dialogId, messageId, index, kind).
  2. Лайтбокс — ГАЛЕРЕЯ: стрелки влево/вправо ходят по картиночным вложениям ОДНОГО сообщения (galleryOf/stepLightbox), границы не зациклены, кнопки на краях disabled.
  3. Видео не проксируется: вложение video у VK не содержит воспроизводимого файла. Карточка показывает превью и открывает player_url во внешней вкладке через safeUrl + openExternal (noopener,noreferrer).
  4. Появилась карточка other (ссылка, опрос, товар, запись на стене). Оригинала у неё не бывает НИКОГДА, а ПРЕВЬЮ и АДРЕС бывают — у вида link.
     КАРТОЧКА ССЫЛКИ (починка живого дефекта «не работают превью ссылок»): заголовок, превью через тот же прокси, что и все вложения, ДОМЕН отдельным <a class="vk-media-link"> (href — полный адрес, target=_blank, rel="noopener noreferrer", атрибуты ставятся setAttribute) и рабочий OPEN (openMedia -> openExternal(ctl.linkUrl)). SAVE остаётся отключённым: файла у ссылки не существует. Адрес берётся из MediaDto.link_url и проходит format.js::webUrl (строго http/https — схему выбирал собеседник), подпись — format.js::linkDomain (домен, а не полный адрес: длинный путь ломает раскладку). У опроса/товара link_url нет, и карточка выглядит как раньше: заголовок, OPEN и SAVE отключены.
     Превью для ссылки поехало потому, что canShowThumb перестал держать собственный белый список видов и теперь просто верит серверному has_thumb (см. док tmux-web/static/js/vk/format.js).

ЭКСПОРТ: THUMB_HEIGHT, MAX_ATTACH_INDEX, loadMedia, revokeDialogMedia, dropMessageMedia, resetMediaCards, buildMediaCard, renderAttachmentGroup, onDownloadProgress, openExternal, openMedia, saveMedia, stopPlayer, isPlayerClick, resolveCardClick, playInCard, initMedia, galleryOf, openLightbox, stepLightbox, closeLightbox, teardownMedia, _debugMediaState.

ИНВАРИАНТЫ: загрузка только fetch -> blob -> objectURL (у <img src> нет Bearer, в remote-mode прямой src молча ломается); ленивые превью через IntersectionObserver; полный файл только по явному действию; каждый objectURL обязан быть отозван (revokeDialogMedia при смене беседы, dropMessageMedia на правке/удалении, teardownMedia на logout); всё через textContent; НИ ОДИН адрес ФАЙЛА VK не попадает в <img src>/<a href>. Внешних адресов в DOM ровно два, и оба не файлы: страница-плеер видео (через safeUrl) и адрес вложения link (через webUrl). Второй — содержимое переписки, а не инфраструктура VK.

СТЫКОВКА: chat.js кладёт группу в .vk-line-media и зовёт resetMediaCards/revokeDialogMedia/dropMessageMedia/teardownMedia; main.js направляет WS-кадр download_progress в onDownloadProgress и зовёт initMedia/teardownMedia. Покрыт tmux-web/tests/frontend/vk_media.test.mjs (113 проверок, включая регресс карточки ссылки: домен, href/target/rel, запрос превью через прокси, отбрасывание javascript:/data:/vk://)..
