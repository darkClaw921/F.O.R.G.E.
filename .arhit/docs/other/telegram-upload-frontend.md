# telegram-upload-frontend

tmux-web/static/js/telegram/upload.js — отправка файлов из браузера (Фаза 5 плагина forge-telegram).

## Три способа выбрать файл
1. Кнопка ⇪ #tg-attach кликает по скрытому #tg-file-input (input[type=file] multiple). .value сбрасывается ДО отправки: иначе повторный выбор того же файла не даёт события change и кнопка кажется сломанной.
2. Drag&drop на всей панели #telegram -> оверлей #tg-dropzone. preventDefault на dragover ОБЯЗАТЕЛЕН (без него браузер откроет файл вместо страницы); счётчик вложенности local.dragDepth гасит мигание оверлея, потому что dragenter/dragleave стреляют на каждом дочернем узле; hasFiles(e) отсекает перенос текста и ссылок.
3. paste на #tg-input — перехват ТОЛЬКО при наличии файлов (обычный текст обязан вставляться как текст). clipboardFiles(dt) читает и dt.files, и dt.items (скриншот в части браузеров приходит только через items); безымянные/'image.png'/'blob' переименовываются в pasted_<YYYYMMDD_HHMMSS>.<ext> через новый File().

## Очередь
sendFiles(files, opts) -> число принятых. Очередь СТРОГО последовательная (pump() с флагом реентерабельности): параллельные заливки делят канал и перемешивают события прогресса. Подпись по умолчанию берётся из #tg-input и поле очищается (пользователь ждёт одно сообщение с подписью, а не текст отдельно и файл отдельно) и достаётся ПЕРВОМУ файлу. Предварительная валидация uploadRejectReason: пустой файл и размер > MAX_UPLOAD_BYTES (2 ГиБ) отсекаются без сетевого запроса; источник истины остаётся серверный (413 too_large обрабатывается как обычная ошибка отправки).

## Два этапа прогресса
LOCAL % — браузер -> devforge, измеряется XMLHttpRequest.upload.onprogress (у fetch прогресса отправки нет). Запрос: POST /api/telegram/upload, xhr.send(file) — СЫРОЙ File, не FormData; Content-Type application/octet-stream; X-Forge-File-Name: encodeURIComponent(name); Authorization: Bearer ВРУЧНУЮ через getAuthToken() — XHR идёт МИМО подменённого window.fetch из core/auth.js, без этого отправка не работала бы в remote-mode. Content-Length ставит браузер сам (вручную запрещено).
TG % — devforge -> Telegram, приходит WS-событиями upload_progress {upload_id, sent, total}; карточка сразу после ответа первого этапа переводится в «TG 0%», иначе выглядит зависшей на «LOCAL 100%».
Затем POST /dialogs/:id/messages {upload_id, caption} (держится всё время заливки) -> '✓ SENT' -> optimistic-строка снимается, DTO уходит в chat.onNewMessage (insertMessage дедуплицирует, если WS-апдейт опередил ответ).

## Ошибки, retry и отмена
Провал любого этапа -> '!! FAILED' + системная строка с причиной (для flood_wait — сколько ждать) + кнопка RETRY, которая начинает С ПЕРВОГО этапа (upload_id мог протухнуть по TTL, staging — быть подчищен). CANCEL: xhr.abort() на первом этапе; на втором сообщение просто не отправляется — staging соберёт GC по TTL. teardownUpload() обрывает активный XHR при logout и уходе со страницы (вызывается из main.clearAccountData и main.teardownTelegram).

## Карточка
Классы принятых медиа (.tg-media-card / .tg-media-progress / .tg-media-status / .tg-media-btn) + .tg-upload-card (амберная рамка «в пути») и .tg-upload-failed (красная). Всё через textContent. Единственная точка смены визуального состояния — setStage(item, stage, sent, total).

## Чистая логика в format.js (тестируется в Node)
uploadMime, uploadKind, pastedFileName, uploadStageLabel, uploadQueueLabel, uploadRejectReason, parseApiError, MAX_UPLOAD_BYTES. Регресс-тест tmux-web/tests/frontend/telegram_upload.test.mjs (41 ассерт).
