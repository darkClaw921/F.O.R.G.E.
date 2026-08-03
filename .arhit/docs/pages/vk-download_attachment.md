plugins/vk/src/routes/media.rs::download_attachment(state, client, key) -> Result<CachedMedia, ApiError> (Ф7) — единственный сетевой путь модуля медиа.

ПОРЯДОК: resolve_message (дедуп по сообщению) -> try_download: attach::url_for -> begin_staging -> stream_to_file (get_stream + DownloadReporter) -> commit (мета ПОСЛЕ байтов) -> перечитать из кэша, чтобы отдавать тем же кодом, что и попадание.

ПРОТУХШАЯ ССЫЛКА: try_download возвращает Attempt::Expired на 403/404/410 от CDN. Тогда делается РОВНО ОДИН повтор — с forget_message + новым fetch_attachments, то есть с ЗАНОВО добытым адресом. Повторять со старым бессмысленно: он уже отвергнут. Второй Expired подряд -> 404 no_media.

Это и есть реализация инварианта «URL вложений VK не кэшируются никогда»: адрес существует ровно на время одного скачивания.

StagingGuard снимает .tmp при любом раннем выходе (обрыв, отмена запроса, ошибка записи). Пустой файл (0 байт) считается отказом: пустой bin с Cache-Control: immutable это навсегда сломанное медиа.