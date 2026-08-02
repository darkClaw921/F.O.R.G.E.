# tmux-web/static/js/telegram/media.js

# js/telegram/media.js — приём медиа на фронте (Фаза 4)

Карточки семи видов (photo, video, voice, audio, document, sticker, gif) шириной
360px по дизайну, превью до 184px.

## Ключевые механизмы

- Ленивые превью через IntersectionObserver: карточка запрашивает thumb только
  когда доскроллили. Экономит и трафик, и FLOOD_WAIT-бюджет.
- `loadMedia` — fetch -> blob -> objectURL с ДВУХУРОВНЕВЫМ дедупом (по ключу
  медиа и по уже идущему запросу).
- Двухногий прогресс: `TG %` (сервер тянет из Telegram, событие
  download_progress по WS) и `NET %` (браузер тянет с сервера). Размер
  неизвестен -> индетерминированный режим полосы вместо вранья про проценты.
- Лайтбокс с ловушкой фокуса и Esc; `onFail` не даёт подписи залипнуть на
  'loading…'.
- `saveMedia` -> `sanitizeDownloadName` (живёт в format.js, покрыт Node-тестом):
  имя приходит от чужого аккаунта, а атрибут `download` браузеры понимают как
  подсказку пути.
- `playInCard` — ровно один активный плеер на вкладку.
- `revokeDialogMedia` / `dropMessageMedia` / `teardownMedia` — обязательная
  ревокация objectURL'ов.

## CSS-ловушки, найденные рантайм-прогоном (закреплены комментариями в CSS)

1. У `.tg-media-thumb` (display:block) и `.tg-media-play` (display:flex) есть
   собственный `display`, и он перебивает UA-правило `[hidden]{display:none}` —
   без явного `.tg-media-card [hidden]{display:none !important}` оверлей ▶
   оставался поверх фотографий.
2. Класс карточки формируется как `tg-media-${kind}`, поэтому `.tg-media-video`
   совпал с классом плеера и растягивал карточку. Плеер стилизуется по
   ВЛОЖЕННОСТИ (`.tg-media-player > video`), а не по классу элемента.
