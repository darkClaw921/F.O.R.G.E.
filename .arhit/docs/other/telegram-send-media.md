# telegram-send-media

Отправка медиа в Telegram: расширение POST /api/telegram/dialogs/:id/messages (plugins/telegram/src/routes/dialogs.rs, Фаза 5).

## Контракт тела
{ text?, caption?, upload_id?, reply_to_id? }
- text и caption — синонимы (у Telegram подпись к медиа лежит в том же поле сообщения); берётся первое непустое (pick_text);
- только текст -> обычное сообщение (поведение Фазы 3 не изменилось);
- upload_id (+ необязательный текст как подпись) -> медиа-сообщение;
- ни того, ни другого -> 400 invalid_input.
Валидация вынесена в чистую validate_send(&SendMessageReq) -> Result<SendPlan, ApiError>, потому что сам хендлер до валидации требует require_ready (авторизация + живое подключение), которого в unit-тестах нет.

## Как выбирается способ отправки
is_photo_mime(mime): image/* уходит как photo (Telegram сожмёт и покажет превью), КРОМЕ image/gif (Telegram делает из gif видео-документ и анимация потерялась бы), image/webp (формат стикеров, часто PHOTO_EXT_INVALID), image/svg+xml и ico. Всё остальное — .file(uploaded).
.file(), а не .document(): видео без атрибутов duration/width/height Telegram всё равно показывает документом, а достать их можно только распарсив контейнер (ffmpeg/mp4-парсер) — такой зависимости в v1 нет. Осознанное ограничение, зафиксировано в doc-комментарии.
ВАЖНО: InputMessage::mime_type(&mime) обязан вызываться ДО .photo()/.file() — билдер запоминает mime заранее и подставляет его в uploaded_document.

## Порядок операций в хендлере
1. require_ready -> validate_send -> resolve_peer;
2. state.take_upload(&uid) ПОТРЕБЛЯЕТ запись (нет -> 409 invalid_state «unknown or already used upload_id»): именно это делает повторную отправку одного файла невозможной;
3. best-effort client.action(peer).oneshot(upload_photo|upload_document) — собеседник видит «отправляет файл»; ошибки игнорируются, активность гаснет у Telegram сама через ~6с и в v1 не продлевается;
4. upload_to_telegram (см. telegram-upload-routes) — во время него в WS сыплется upload_progress;
5. InputMessage -> send_message -> from_grammers_message (media в DTO заполняется) -> ответ для reconcile во фронте.

## Компенсация ошибок
- заливка не удалась -> запись ВОЗВРАЩАЕТСЯ в реестр (staging на диске, повтор не гонит файл из браузера заново);
- send_message не удался -> staging удаляется (Uploaded-хендл уже потерян, повтор всё равно начнётся с новой заливки);
- успех -> staging удаляется.

## Подпись
MAX_CAPTION_CHARS = 1024 (лимит Telegram). Превышение -> 400, а НЕ «обрезать и дослать остаток вторым сообщением»: молча превращать одно сообщение в два — сюрприз для пользователя, а обрезать текст — потеря данных. Фронт знает лимит и предупреждает до отправки.

## Тесты
is_photo_mime (матрица MIME), pick_text, validate_send (пусто/только текст/только upload_id/пробелы в upload_id/подпись ровно в лимите и на символ длиннее/для текста лимит подписи не применяется), upload_action.
