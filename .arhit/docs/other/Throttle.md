# Throttle

plugins/vk/src/api/throttle.rs. Асинхронная обёртка над Bucket: один Arc<Throttle> на VkClient, то есть на ТОКЕН (лимит — свойство токена, а не вызова, дробить его между REST-хендлерами и LongPoll-воркером нельзя). acquire() ждёт, пока лимит позволит запрос, и УДЕРЖИВАЕТ lock через sleep намеренно: tokio::sync::Mutex справедлив (FIFO), очередь обслуживается по приходу, а не устраивает thundering herd. stats() отдаёт ThrottleStats{total_waits, total_wait_ms} в GET /api/vk/status и в лог. НЕ троттлятся: a_check LongPoll, POST upload_url, GET CDN вложений.
