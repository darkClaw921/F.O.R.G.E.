# tmux-web/static/js/telegram/api.js

REST-клиент вкладки Telegram поверх apiFetch. Разбирает единый контракт ошибок плагина {error:{code,message,retry_after_secs}} в Error с полями .status/.code/.retryAfterSecs — решения принимаются по КОДУ, не по тексту.

Фаза 1-2: getStatus, loginStart (НИКОГДА не ретраить автоматически при flood_wait), loginCode, loginPassword, logout.
Фаза 3: getDialogs(limit), getMessages(dialogId,{limit,beforeId}) (ответ по ВОЗРАСТАНИЮ id, страницы просто конкатенируются; has_more — эвристика 'вернулось ровно limit'), sendMessage(dialogId,{text,replyToId}) (ответ несёт полный MessageDto для reconcile; id<=0 = Telegram не подтвердил), markRead(dialogId,maxId) (без maxId — весь диалог), sendTyping(dialogId,action) (НЕ критично: вызывающий обязан глотать ошибки, включая 429).

## Ответы на сообщения (R1/R2)

- getMessage(dialogId, messageId) — GET /dialogs/:id/messages/:mid, ПЛОСКИЙ MessageDto (не
  {message:…}). Нужен, когда пользователь кликнул по цитате, а оригинал вне загруженной истории:
  только сервер отличает «оригинал далеко» от «оригинал удалён». Автоповтора при flood_wait нет.
- sendMessage — кода не менял: `replyToId` кладётся в тело НЕЗАВИСИМО от того, текст это или медиа
  (отдельной строкой, вне веток text/caption/upload_id), поэтому медиа-путь передаёт его без
  изменений. Это зафиксировано в jsdoc специально, чтобы следующий не «починил» проверку внутрь
  `if (p.text)` и не сломал цитату у файлов. Нечисловой replyToId просто не попадает в тело: сервер
  отвечает на битый reply_to_id 400, и лучше отправить без цитаты, чем не отправить вовсе.
