# plugins/vk/src/routes/upload.rs

Приём файла от браузера и трёхшаговый конвейер заливки в VK.

## Что делает

Единственный роут модуля — POST /api/vk/upload: принимает СЫРОЕ тело запроса (xhr.send(file)) плюс заголовок X-Forge-File-Name (percent-encoded) и кладёт байты в <cache_dir>/uploads/<uuid>.part, возвращая {upload_id, file_name, size, mime}. НИ ОДНОГО обращения к VK этот роут не делает.

## Почему конвейер живёт НЕ здесь

Upload-сервер VK выдаётся под КОНКРЕТНЫЙ peer_id (photos.getMessagesUploadServer(peer_id)), а на шаге /upload адресат ещё неизвестен: пользователь вправе прикрепить файл и передумать, куда слать. Поэтому три шага (1) getMessagesUploadServer -> (2) POST upload_url multipart -> (3) save + (4) messages.send выполняются внутри POST /api/vk/dialogs/:id/messages. Следствия: upload_id НЕ равен attachment-строке VK и никогда ей не станет; кадр upload_progress приходит во время POST /messages, а не во время POST /upload (форма кадра не меняется, фронт обязан слушать его после нажатия «отправить»).

## Порядок проверок в post_upload (дословный)

авторизация (409 not_authorized) -> Content-Length (411 length_required) -> лимит max_upload_bytes (413 too_large) -> X-Forge-File-Name (400 invalid_input) -> стриминг тела на диск. Отказ происходит ДО чтения тела: тест upload_refuses_before_reading_the_body (сырой TCP с завышенным Content-Length) краснеет, если это нарушить. Связь с VK здесь НЕ проверяется: staging — локальная операция, а 503 отдаётся там, где реально идёт сеть.

## Ключевые элементы

- post_upload / stream_to_file / StagingGuard — приём байтов; чанк пришёл, чанк записан (RSS не растёт), StagingGuard удаляет .part при любом отказе.
- parse_content_length, percent_decode, sanitize_upload_name, truncate_utf8, guess_mime — чистые функции разбора; имя staging-файла определяется ТОЛЬКО upload_id, поэтому path traversal невозможен по построению.
- classify_upload(mime, file_name) -> UploadKind{Photo,Doc,AudioMessage} — чистая, табличный тест. image/gif и image/svg+xml -> Doc (иначе VK теряет анимацию, и фронтовый uploadKind разошёлся бы с сервером), audio/ogg + расширение .ogg -> AudioMessage, прочие image/* -> Photo, остальное -> Doc.
- attachment_string(kind, saved) -> Option<String> — чистая; собирает <тип><owner>_<id>[_<access_key>]; префикс doc и у голосового (audio_message — имя типа вложения, а не адреса).
- upload_photos — АЛЬБОМ до MAX_ALBUM_PHOTOS=5 полями file1..file5: три троттленных запроса на пятёрку вместо пятнадцати.
- upload_doc — ветка документа/голосового (тип задаётся при выдаче адреса).
- build_attachment — собирает строку attachment, сохраняя ПОРЯДОК ПОЛЬЗОВАТЕЛЯ.
- take_uploads — потребляет записи реестра (всё или ничего); повторная отправка с тем же upload_id даёт 409 invalid_state.
- staged_part — стык с reqwest: ReaderStream над ProgressReader + Part::stream_with_length(size). stream_with_length ОБЯЗАТЕЛЕН: без него reqwest уходит в chunked и буферизует файл целиком.
- evict_stale / cleanup_uploads_dir / uploads_gc_loop / spawn_uploads_gc — TTL 30 минут, подчистка сирот прошлого процесса на старте и раз в 5 минут.

## Связи

state.rs (UploadEntry, uploads, take_upload), progress.rs (ProgressReader/UploadReporter), api/mod.rs (upload_multipart_parts, UploadPart), routes/dialogs.rs (post_message вызывает take_uploads + build_attachment), lib.rs (spawn_uploads_gc).
