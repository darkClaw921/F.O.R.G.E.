# Фаза 5 плагина Telegram — отправка медиа + typing/read/online (эпик forge-tg-phase5-lzhj)

Закрывает объём v1: полный медиа-обмен с двухэтапным прогрессом, индикатор набора в обе стороны, аккуратная отметка о прочтении, присутствие собеседников.

## Что появилось
Backend (plugins/telegram):
- routes/upload.rs — POST /api/telegram/upload на RAW streaming body + реестр загрузок + GC. Подробности: arhit doc show telegram-upload-routes.
- routes/dialogs.rs::post_message — отправка photo/file по upload_id с подписью. Подробности: arhit doc show telegram-send-media.
- routes/error.rs — новые коды 411 length_required и 413 too_large.
- state.rs — UploadEntry.mime + insert_upload/upload_entry/take_upload/evict_stale_uploads.
- lib.rs — четвёртый фоновый воркер (GC загрузок).
Host (tmux-web):
- src/auth.rs::csrf_guard — точечное исключение для /api/telegram/upload (arhit doc show csrf_guard).
Фронт (tmux-web/static):
- js/telegram/upload.js — новый модуль (arhit doc show telegram-upload-frontend).
- js/telegram/{chat,main,dialogs,api,format}.js — typing/read/online (arhit doc show telegram-presence-and-read).
- index.html: #tg-file-input; core/dom.js: , ; css/telegram.css: оверлей dropzone и состояния карточки отправки.

## Главное архитектурное решение фазы
Заливка в Telegram делается НЕ внутри POST /upload, а на шаге отправки сообщения; реестр хранит путь staging-файла, а не Uploaded-хендл.
Причина: фронт узнаёт upload_id только из ответа POST /upload, поэтому события upload_progress, отправленные раньше ответа, невозможно связать с карточкой — двухэтапный прогресс LOCAL -> TG просто не работал бы. Побочная выгода: выбранный и не отправленный файл вообще не уходит в сеть.

## Проверки
cargo test --workspace: 542 devforge + 214 forge-telegram + 195 forge-echo + 8 telegram_smoke + прочие — зелёные; clippy -p forge-telegram чист; cargo tree -i openssl-sys/openssl/native-tls/libsql/ece/web-push ПУСТО; node-тесты telegram_format 91/91, telegram_upload 41/41, telegram_media 61/61; e2e echo-ui 16/16.
Рантайм на ИЗОЛИРОВАННОМ инстансе (порт 17451, HOME в scratchpad — инстанс пользователя не тронут): 36/36 шагов, включая LOCAL->TG->SENT с реальными заголовками XHR, WS upload_progress -> TG 50%, dropzone без мигания и с preventDefault на dragover, троттлинг typing (три нажатия — один запрос), cancel при очистке поля, коалесценция read (три триггера — один запрос с максимальным id), гейт скрытой вкладки, [ONLINE] в строке/шапке/статусбаре без пересоздания узла. Плюс curl'ом на живом сервере: 409 без авторизации, 403 с чужим Origin, 415 на text/plain и на соседнем роуте.

## Найдено и исправлено рантайм-прогоном
1. typingLabel подставлял имя диалога любому user_id -> в группе выходило «Sofia, Sofia are typing…»; теперь title берётся только при dialog_id === user_id.
2. sendFiles не забирал подпись из #tg-input при программном вызове -> подпись теперь берётся по умолчанию, а opts.caption её перекрывает.
3. Счётчик ONLINE считал группы и каналы.

## Требует ручной проверки пользователем (нужен реальный аккаунт)
- отправка фото (уходит фото с превью, не документ) и PDF/zip (документ с исходным именем);
- подпись доходит вместе с медиа; каптион > 1024 символов отвергается с внятной ошибкой;
- файл 100+ МБ: сначала LOCAL-прогресс, затем TG-прогресс, затем ✓ SENT, сообщение реально приходит собеседнику;
- отправка в remote-mode с Bearer-токеном (заголовок Authorization проставлен вручную);
- обрыв сети на первом этапе -> ошибка и рабочий RETRY;
- вставка скриншота Ctrl+V и drag&drop из Finder;
- повторная отправка с тем же upload_id -> ошибка, а не дубль;
- собеседник видит typing при наборе и «отправляет файл» во время заливки;
- прочтение с телефона гасит бейдж в devforge; скрытая вкладка НЕ отмечает прочитанным;
- заход собеседника в Telegram меняет строку на [ONLINE] в реальном времени;
- поведение при FLOOD_WAIT на отправке файла.