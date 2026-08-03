# tmux-web/static/js/vk/upload.js

Отправка файлов из панели VK: три способа выбрать файл, одна очередь ПАЧЕК, два этапа прогресса.

## Потоки

⇪ #vk-attach -> скрытый #vk-file-input; drag&drop по всей панели #vk (оверлей #vk-dropzone, счётчик вложенности против мигания); Ctrl+V в #vk-input (скриншот получает имя pasted_<дата>_<время>.png). Дальше — очередь: [1] LOCAL % (браузер -> devforge, POST /api/vk/upload, прогресс из xhr.upload.onprogress), [2] VK % (devforge -> VK, кадры WS upload_progress), затем ✓ SENT и строку дорисовывает chat.onNewMessage.

## Три отличия от js/telegram/upload.js

1. ОДНО СООБЩЕНИЕ НЕСЁТ НЕСКОЛЬКО ФАЙЛОВ. Очередь состоит из пачек: пачка собирает uploadIds[] и уходит ОДНИМ sendMessage — у VK альбом из пяти фотографий стоит трёх запросов вместо пятнадцати. Пачка нарезается по MAX_PER_MESSAGE=10 (серверный потолок вложений), подпись уходит только с первой пачкой, цель ответа — со всеми.
2. ЭТАП «VK %» ИДЁТ ВО ВРЕМЯ POST /messages. Upload-сервер VK выдаётся под конкретный peer_id, поэтому конвейер живёт внутри отправки сообщения, и кадры upload_progress приходят ПОСЛЕ нажатия «отправить»: индикатор не имеет права гаснуть раньше подтверждения.
3. ЛИМИТ РАЗМЕРА ЗАВИСИТ ОТ ВИДА ВЛОЖЕНИЯ (UPLOAD_LIMITS в format.js): у фотографии он жёстче, чем у документа, и одно число дало бы либо ложный отказ, либо пропущенный.

## Инварианты, перенесённые дословно

XHR идёт МИМО подменённого window.fetch (у fetch нет прогресса отправки), поэтому Authorization: Bearer ставится ВРУЧНУЮ из core/auth.js::getAuthToken — без этого отправка файлов работала бы только на localhost. Имя файла едет заголовком X-Forge-File-Name (encodeURIComponent). Очередь строго последовательная. cancelUpload обрывает XHR. Никакого innerHTML.

## Публичные функции

initUpload (идемпотентна, зовётся из main.initVk), sendFiles(files, {caption, replyToCmid}) -> число принятых, onUploadProgress(evt) (кадр WS), cancelUpload(item), teardownUpload (beforeunload; поднимает epoch, чтобы летящий pump не взялся за пачки нового поколения), _debugUploadState, clipboardFiles, константы UPLOAD_PATH, FILE_NAME_HEADER, MAX_PER_MESSAGE.

## Связи

core/dom.js ($vk, $vkAttach, $vkFileInput, $vkDropzone, $vkInput, $vkLog), core/auth.js (getAuthToken), ./api.js (sendMessage), ./chat.js (systemLine, onNewMessage, cancelReply, cancelTyping), ./reply.js (replyTargetCmid — только чтение), ./format.js (uploadMime/uploadKind/uploadCardKind/uploadRejectReason/uploadStageLabel/uploadQueueLabel/progressPercent/parseApiError/pastedFileName/formatBytes/formatTime/mediaGlyph/mediaKindLabel), ./ratelimit.js (бюджет 3 rps), ./state.js. Стили — .vk-upload-card / .vk-upload-cancel / .vk-upload-failed в css/vk.css. Тест: tmux-web/tests/frontend/vk_upload.test.mjs.
