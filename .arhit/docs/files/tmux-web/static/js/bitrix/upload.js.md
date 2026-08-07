# tmux-web/static/js/bitrix/upload.js

Панель «Битрикс24» — отправка файлов (Ф9).

ТРИ ВХОДА
  кнопка ⇪ #b24-attach -> скрытый #b24-file-input (value сбрасывается ДО
    отправки: иначе повторный выбор того же файла не даст change);
  drag&drop по #bitrix с оверлеем #b24-dropzone (счётчик вложенности —
    dragenter/dragleave стреляют на каждом дочернем узле; перетаскивание ТЕКСТА
    оверлей не поднимает, зона не должна мешать выделению; preventDefault на
    dragover обязателен, иначе браузер откроет файл вместо страницы);
  Ctrl+V в #b24-input — перехват ТОЛЬКО когда в буфере файл, иначе Ctrl+V
    перестанет работать по назначению. Скриншот получает имя
    pasted_YYYYMMDD_HHMMSS.png: браузер называет их все image.png.

ОДИН ФАЙЛ = ОДНО СООБЩЕНИЕ
im.v2.File.upload принимает ровно один файл и сам же отправляет сообщение;
альбома, как у VK, у платформы нет. Очередь состоит из файлов, а не из пачек, и
она СТРОГО ПОСЛЕДОВАТЕЛЬНАЯ: два параллельных base64-тела по 33 МБ дали бы
66 МБ в куче devforge разом. Подпись из поля ввода достаётся один раз и
достаётся ПЕРВОМУ файлу.

ГДЕ ЖИВЁТ КАРТОЧКА
В #b24-uploads НАД полем ввода, а не в ленте: chat.renderHistory() пересобирает
#b24-log целиком (правка, сообщение вставшее не в конец, смена диалога), и
карточка отправки исчезала бы вместе с прогрессом. По существу очередь отправки
— состояние строки ввода: в переписке файл появляется только после того, как
портал его принял.

ДВА ЭТАПА
  client — xhr.upload.onprogress, точные проценты (XHR вместо fetch именно
    ради этого: у fetch нет прогресса ОТПРАВКИ);
  portal — кадр WS upload_progress с indeterminate, бегущая полоса. Кадры фазы
    client приходят тоже, но ЭТОЙ вкладкой не применяются: свой прогресс приёма
    она знает точнее, а upload_id узнаёт только из ответа — когда фаза уже
    кончилась. Кадры нужны ВТОРОЙ вкладке devforge.

XHR идёт МИМО подменённого window.fetch (core/auth.js), поэтому Authorization:
Bearer ставится ВРУЧНУЮ через getAuthToken() — без этого отправка работала бы
только на localhost.

ОТМЕНА И ПОВТОР
Отмена доступна только до фазы portal: как только upload_id ушёл в
POST /messages, сервер его ПОТРЕБИЛ и начал заливку, прерывать которую нечем —
кнопка к этому моменту скрыта (неработающая кнопка хуже отсутствующей).
Автоповтора НЕТ (Р9): повтор — новая загрузка с первого этапа по кнопке ⟳.
Отказ по размеру повтора не предлагает вовсе — он ничего не починит.

КЛИЕНТСКАЯ ПРОВЕРКА РАЗМЕРА
uploadRejectReason(size, b24State.maxUploadBytes); лимит приходит из
GET /api/bitrix/status, а не зашит во фронт (скрипты живут в кэше PWA
неделями). Файл больше лимита не порождает ни одного запроса.

ЭКСПОРТ: UPLOAD_PATH, FILE_NAME_HEADER, MAX_FILES_PER_DROP, initUpload,
sendFiles, onUploadProgress, teardownUpload, cancelUpload, clipboardFiles,
_debugUploadState.

СВЯЗИ: core/dom.js ($b24Attach, $b24FileInput, $b24Dropzone, $b24Uploads,
$b24Input, $b24), core/auth.js (getAuthToken), bitrix/api.js (PATHS.upload,
sendMessage), bitrix/chat.js (currentDialogId, appendMessage, replyMode,
cancelMode, updateCharCount), bitrix/format.js (uploadMime, uploadKind,
pastedFileName, uploadStageLabel, uploadQueueLabel, uploadRejectReason,
progressPercent, formatBytes, mediaKindLabel, errorLabel), bitrix/state.js
(maxUploadBytes). Обратных импортов нет: chat.js о существовании upload.js не
знает.

ТЕСТ: tmux-web/tests/frontend/bitrix_upload.test.mjs (96 ассертов).
