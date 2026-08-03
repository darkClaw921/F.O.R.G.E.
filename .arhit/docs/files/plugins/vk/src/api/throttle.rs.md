# plugins/vk/src/api/throttle.rs

Превентивное соблюдение лимита 3 rps пользовательского токена VK — token bucket с инъекцией часов. Подсистемы такого класса у плагина Telegram нет вовсе.

ПОЧЕМУ ПРЕВЕНТИВНО: FLOOD_WAIT у Telegram узнаётся пост-фактум, а лимит VK известен заранее. Известный лимит дешевле соблюдать, чем ловить: код 6 стоит запроса и секунды, а регулярное его получение переходит в код 9 (flood control), из которого выходить долго.

Clock (trait) + SystemClock — источник времени. Существует ради тестов: ядро проверяется на ручных отметках, без единого sleep.

Bucket { tokens, capacity, refill_per_sec, last } — ЧИСТОЕ ЯДРО без асинхронности и без Instant::now() внутри.
- new(rps, now): ведро ПОЛНОЕ (бюджет предыдущей секунды не израсходован). capacity = max(rps, 1): при rps<1 ведро ёмкостью rps не вместило бы ЦЕЛЫЙ токен и не прошёл бы НИ ОДИН запрос — вместо «медленно» получилось бы «никогда».
- take(now) -> Result<(), Duration>: Ok — идти в сеть, Err(d) — ждать d. Время, идущее назад, не наливает токенов и не откатывает last.

Throttle { inner: Mutex<Inner{bucket, stats}>, clock } — асинхронная обёртка. new(rps) / with_clock(rps, clock) / acquire() / stats() / available(). Ведро и счётчики под ОДНИМ lock'ом: два мьютекса дали бы второй порядок захвата и расхождение статистики.

acquire() УДЕРЖИВАЕТ lock через sleep НАМЕРЕННО: tokio::sync::Mutex справедлив (FIFO), поэтому очередь обслуживается по приходу; отпусти мы lock на время сна, все ожидающие проснулись бы разом (thundering herd).

ThrottleStats { total_waits, total_wait_ms } — уходит в GET /api/vk/status и в лог: растущий total_wait_ms при спокойной работе означает завышенный rate_limit_rps.

ПОЧЕМУ BUCKET, А НЕ ФИКСИРОВАННОЕ ОКНО: залп из трёх запросов при открытии вкладки (getConversations + users.get + groups.getById) проходит мгновенно; непрерывное пополнение — ближайшая аппроксимация скользящей секунды VK; фиксированное окно даёт пограничный залп 6 запросов на стыке окон.

ЧЕСТНАЯ ОГОВОРКА. Token bucket формально допускает C + r*W запросов в окне W, то есть 6 за секунду. «Не больше 3 в любую скользящую секунду» и «залп из 3 проходит мгновенно» взаимоисключающи; выбран второй вариант. В УСТАНОВИВШЕМСЯ режиме интервал ровно 1/rps, что и проверяет стенд.

ЧТО НЕ ТРОТТЛИТСЯ: a_check LongPoll (чужой хост, 25-секундное удержание съело бы бюджет), POST upload_url, GET CDN вложений.

Тесты: bucket_take_on_injected_clock (3 подряд Ok, 4-й Err ~333 мс, +334 мс Ok, +10 c не больше capacity), bucket_rounds_in_favour_of_the_limit, bucket_survives_time_going_backwards, bucket_tolerates_a_nonsensical_rps, bucket_handles_fractional_rps, concurrent_callers_share_one_budget.
