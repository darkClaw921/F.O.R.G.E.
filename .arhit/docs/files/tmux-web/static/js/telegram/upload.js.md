# tmux-web/static/js/telegram/upload.js

# js/telegram/upload.js — отправка файлов с двухэтапным прогрессом (Фаза 5)

Источники файлов: кнопка ⇪ (скрытый `#tg-file-input`), drag&drop по панели,
Ctrl+V скриншота из буфера. Очередь последовательная.

## Двухэтапный прогресс LOCAL -> TG

1. `LOCAL %` — браузер заливает файл на devforge через XMLHttpRequest
   (`upload.onprogress`; fetch прогресса ОТПРАВКИ не даёт).
2. `TG %` — devforge заливает файл в Telegram, события upload_progress по WS.
3. `✓ SENT` -> reconcile: настоящее сообщение из ленты заменяет карточку.

RETRY повторяет с первого этапа, CANCEL обрывает XHR.

## Ответ на сообщение (R1)

`sendFiles(files, opts)` принимает `opts = {caption?, replyToId?}`. Без явного `opts.replyToId`
цель берётся из reply.js::replyTargetId(dialogId) — ОДИН раз, при постановке в очередь, и
фиксируется в item'е (enqueue). Читать её перед самой отправкой нельзя: очередь последовательная, и
пока до файла дойдёт ход, полоса уже может показывать другое сообщение; побочно фиксация чинит
RETRY (уходит туда же, куда собирался пользователь).

`reply_to_id` получают ВСЕ файлы пачки, в отличие от подписи (подпись трижды — спам, а ответ тремя
файлами и должен выглядеть как три ответа). Optimistic-строка получает `data-reply-to-id` — хук для
отрисовки цитаты владельцем полосы.

Полоса гасится через `chat.cancelReply()`, а не `reply.clearReplyTarget()`: у полосы две половины —
цель в reply.js и DOM-узел в chat.js, — и сброс половины оставил бы полосу висеть над пустым
состоянием. Гасим только если файл реально принят в очередь (все отвергнуты по размеру — полосу
терять не за что) и только когда цель взята из полосы, а не передана явным `opts.replyToId`.

`_debugUploadState()` дополнен полем `replies: Array<?number>` — цели ответа по очереди.
Новая зависимость модуля: `import { replyTargetId } from './reply.js'` (лист, цикла нет).

## Гочи, которые стоили отладки

- **XHR идёт МИМО подменённого `window.fetch`** (js/core/auth.js), поэтому
  заголовок `Authorization: Bearer` ставится вручную из `getAuthToken()`.
  `Content-Length` вручную поставить НЕЛЬЗЯ (forbidden header) — браузер
  выставит его сам по объекту File.
- Оверлей dropzone: `pointer-events:none` + счётчик вложенности
  dragenter/dragleave; `preventDefault` на dragover обязателен, иначе браузер
  просто откроет файл.
- Скриншот из буфера приходит через `clipboardData.items` с общим именем
  'image.png' -> файл пересоздаётся как `pasted_<ts>.<ext>`.
- `sendFiles()` берёт подпись из `#tg-input` значением по умолчанию
  (`opts.caption` перекрывает) — иначе при программном вызове подпись терялась.

## Связь с бэкендом

`POST /api/telegram/upload` (application/octet-stream, X-File-Name) кладёт тело
в staging и отдаёт `upload_id`; сама заливка в Telegram происходит на шаге
`POST /api/telegram/dialogs/:id/messages`. Причина — см. страницу
telegram-phase5-media-send: фронт узнаёт upload_id ТОЛЬКО из ответа, поэтому
события прогресса, отправленные раньше ответа, не с чем сопоставить.
