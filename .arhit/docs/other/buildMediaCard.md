# buildMediaCard

tmux-web/static/js/telegram/media.js::buildMediaCard(msg) -> ?HTMLElement (алиас renderMediaCard) — рендер медиа-карточки в ленту Telegram, Фаза 4.

СТРУКТУРА (360px, дизайн hablo-taste): .tg-media-card[data-dialog-id][data-msg-id][data-media-kind] + класс tg-media-<kind>, внутри три строки — head (.tg-media-glyph + .tg-media-name с ellipsis + .tg-media-kind), body (.tg-media-thumb 184px | .tg-media-icon глиф | .tg-media-wave для voice/audio, оверлей .tg-media-play только у video, .tg-media-duration, .tg-media-player под плеер), foot (.tg-media-meta, .tg-media-progress + .tg-media-progress-bar, .tg-media-status, кнопки OPEN/SAVE).

ДИСПЕТЧЕР по media.kind: photo | video | document | sticker | voice | audio | other; незнакомый вид попадает в other (глиф + размер) — проглатывать сообщение нельзя, даже если рисовать нечего. Псевдо-волна голосового детерминирована по message_id (xorshift), иначе карточка 'дышала' бы при каждой перерисовке ленты.

ЛЕНИВОЕ ПРЕВЬЮ: IntersectionObserver (rootMargin 200px) — в истории на сотню медиа немедленный рендер дал бы сотню запросов. canShowThumb(media) === false (has_thumb=false, voice/other) -> сразу глиф, без запроса, который вернёт 404. Нет IntersectionObserver -> грузим сразу (лучше лишние запросы, чем пустые карточки).

ВСЁ содержимое через textContent/createElement: имена файлов приходят из Telegram, то есть от чужого аккаунта (рантайм-проверка: file_name с <img onerror> даёт 0 элементов img и не выполняет обработчик).

КОНТРОЛЛЕР карточки регистрируется в local.cards по ключу '<dialog_id>/<message_id>' — по нему onDownloadProgress находит нужную карточку. resetMediaCards() снимает контроллеры и наблюдателя перед перерисовкой ленты (иначе прогресс уходил бы в осиротевшие узлы); вызывается из chat.renderHistory.

ЛОВУШКА CSS (исправлена и закреплена правилом): у .tg-media-thumb (display:block) и .tg-media-play (display:flex) собственный display перебивает UA-правило [hidden]{display:none} -> без '.tg-media-card [hidden]{display:none!important}' оверлей ▶ появлялся поверх фотографий. Вторая ловушка: класс карточки tg-media-<kind> совпадает с именами, которыми хотелось стилизовать плеер (.tg-media-video/.tg-media-audio) — правило width:100% растягивало карточку видео на всю ширину лога; плеер стилизуется по вложенности '.tg-media-player > video|audio'.

СВЯЗИ: format.js (чистые mediaFileName/mediaGlyph/mediaKindLabel/mediaMetaLine/canShowThumb/durationLabel/formatBytes), chat.js::buildMessageRow (карточка добавляется под строку-подпись '[photo] caption'), media.js (loadMedia/openMedia/saveMedia/playInCard), css/telegram.css.
