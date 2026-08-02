# telegram-upload-routes

plugins/telegram/src/routes/upload.rs — приём файла от браузера и заливка его в Telegram (Фаза 5 плагина forge-telegram).

## Эндпоинт
POST /api/telegram/upload — RAW streaming body (НЕ multipart).
Заголовки: Content-Type: application/octet-stream, X-Forge-File-Name: <encodeURIComponent(имя)>, Content-Length (обязателен).
Ответ 200: { upload_id, file_name, size, mime }.

## Порядок проверок (важен)
1. require_authorized -> 409 not_authorized ДО чтения тела (иначе клиент зря заливает гигабайт);
2. нет Content-Length -> 411 length_required (без размера нет ни прогресса, ни предварительной проверки лимита);
3. Content-Length > cfg.max_upload_bytes -> 413 too_large (тело не читается);
4. нет/пустой X-Forge-File-Name -> 400 invalid_input;
5. тело стримится чанками на диск.
Проверки живого MTProto здесь НЕТ намеренно: staging — локальная операция, отправка (и 503) случится в POST /dialogs/:id/messages, поэтому короткий обрыв связи не заставляет заново выбирать файл.

## Ключевые функции
- router(max_upload_bytes) — DefaultBodyLimit::max точечно на роуте. В доке отмечено, что для экстрактора Body этот слой НЕ работает (лимит применяют только буферизующие экстракторы), поэтому настоящая защита — Content-Length + счётчик записанных байт.
- post_upload(State, HeaderMap, Body) — сам хендлер.
- stream_to_file(body, path, limit) -> u64 — body.into_data_stream() + BufWriter + flush + sync_all; превышение limit обрывает запрос на первом лишнем байте. Возвращает записанные байты; расхождение с Content-Length -> 400.
- percent_decode(&str) — своя реализация вместо крейта (одна функция на 20 строк против новой зависимости в дереве, которое проходит gate-проверку openssl).
- sanitize_upload_name(&str) -> Option<String> — последний сегмент по '/' и '\\', срез control-символов (в т.ч. CRLF — заголовочная инъекция), кап MAX_FILE_NAME_LEN=255 байт по границе UTF-8, '.'/'..'/пусто -> "file"; None только когда клиент не передал имя вовсе.
- guess_mime(name) — mime_guess по расширению (не sniffing: файл ещё не пришёл, а Telegram сам смотрит на расширение).
- staging_path(cfg, upload_id) — <cache_dir>/uploads/<uuid>.part. Имя определяется ТОЛЬКО upload_id: ни байта клиентского ввода в ФС, path traversal невозможен по построению.
- StagingGuard (Drop) — снимает .part при обрыве соединения (axum дропает future хендлера), ошибке или панике.

## Заливка в Telegram
- upload_to_telegram(state, client, upload_id, entry) -> Uploaded: open_staging (нет файла -> 409 invalid_state «залей заново») -> ProgressReader над файлом с колбэком progress_hook -> client.upload_stream(&mut reader, size, name). На успехе финальный upload_progress с sent == total (иначе прогрессбар навсегда замирает на 97%), на ошибке финал НЕ шлётся (карточка обязана остаться в ошибке).
- SharedReporter = Arc<std::sync::Mutex<UploadReporter>> — std-мьютекс, а не tokio: колбэк дёргается из poll_read, где await невозможен.
- map_upload_io(&io::Error) — разворачивает InvocationError из io::Error::other (grammers так заворачивает RPC) и отдаёт его через общий map_invocation: FLOOD_WAIT -> 429 + retry_after_secs, прочее -> 503.

## Жизненный цикл staging и GC
Файл живёт до одного из трёх событий: успешная отправка сообщения (discard_staging), истечение UPLOAD_TTL_SECS=30 минут (evict_stale, вызывается лениво на каждой новой загрузке И воркером), рестарт процесса (cleanup_uploads_dir на старте — реестр в памяти, поэтому после рестарта каждый .part сирота).
- uploads_gc_loop / spawn_uploads_gc — воркер с тиком UPLOAD_GC_INTERVAL=5 минут, выходит по shutdown-токену; зарегистрирован в forge_telegram::spawn_workers (воркеров стало 4).

## Тесты (17+3)
Чистые функции (percent_decode на выводе encodeURIComponent, санитизация путей/CRLF/длины, mime, Content-Length), матрица кодов роута (409/411/413/400), стриминг 4 МиБ ленивым потоком, короткое и длинное тело -> 400 + staging удалён, TTL/GC/cleanup, прогресс на 700 КиБ (первое + финальное событие, монотонность), разворот FLOOD_WAIT_33 из io::Error.
