# Облачный провайдер polza.ai в forge-meet (Ф14)

Модуль `plugins/meet/src/cloud/` — HTTP-клиент polza.ai: расшифровка и саммари без локального GPU. Точка входа для Ф15, которая ставит переключатели стадий локально/облако.

## Состав

| файл | что делает |
|---|---|
| `cloud/mod.rs` | CloudConfig, SecretKey, CloudClient, CloudError, разбор чисел и тел отказов |
| `cloud/service.rs` | GET /key, GET /balance, GET /models + разбор цен |
| `cloud/stt.rs` | POST /audio/transcriptions окнами, нарезка, mp3, сдвиг на общую ось |
| `cloud/summary.rs` | POST /chat/completions потоком SSE, дельты в существующий приёмник |
| `tests/polza_stub.rs` | локальный стенд на axum: 17 тестов |
| `tests/polza_live.rs` | живые вызовы за FORGE_MEET_POLZA_KEY, по умолчанию пропускаются |

## Решения, которые определяют всё остальное

1. **base_url — поле конфига.** Единственная точка подмены в тестах. wiremock в plugins/meet НЕ заводится: он стал бы четвёртым способом делать то же самое и сделал бы ложным объяснение в Cargo.toml, по которому regex не подключается напрямую (он живёт в дереве транзитивом wiremock).
2. **Ключ уходит только заголовком Authorization.** SecretKey печатает маску в Debug и Display, ошибки reqwest чистятся without_url(). На это есть тест стенда, проверяющий query, тело и все заголовки.
3. **Общего таймаута у HTTP-клиента нет.** reqwest::ClientBuilder::timeout ограничивает запрос вместе с чтением тела и убил бы стрим саммари. Есть только connect_timeout, потолок ставится на каждый вызов: у служебных — на весь запрос, у стрима — на паузу между кадрами и на приход заголовков.
4. **HTTP только reqwest из workspace.** Добавлена ровно одна feature — multipart (её уже включает forge-vk). Cargo.lock не вырос, cargo tree -d = те же 49 дублей, cargo tree -i openssl-sys пуст.

## Факты API, снятые живыми запросами

- база https://api.polza.ai/api/v1, Bearer pza_...; API OpenAI-совместимый
- GET /key бесплатен: limit, limit_remaining, limit_reset, usage, а также label — маска ключа, которую сервис отдаёт сам. **limit_reset у живого ключа = monthly**, а не daily, как было записано в плане, поэтому поле осталось строкой
- GET /balance: amount, reservedAmount, spentAmount СТРОКАМИ, в рублях
- GET /models: 388 записей, конверт {object, data}. **Поля pricing на верхнем уровне нет вообще** (не null) ни у одной записи — цена лежит в top_provider.pricing. Формы две: {prompt_per_million, completion_per_million, currency} и {stt_per_minute, currency}, значения строками
- отказ: HTTP 401 и тело {"error":{"code","message","trace_id"}}
- STT: единственная пригодная модель openai/whisper-1 с response_format=verbose_json, таймкоды посегментные. Поле tokens в ответе — числовые идентификаторы токенов модели, а не метки времени, поэтому WhisperSegment.tokens у облачных сегментов пуст ВСЕГДА
- жёсткий предел около 60 секунд ПО СТЕНЕ на запрос, поэтому окно 90 секунд аудио и нарезка на нашей стороне
- на облако уходит mp3: WAV на 193 КБ роняет их шлюз с Maximum call stack size exceeded

## Расхождение с планом: сдвиг окна это не remap_segment_times

Задача Ф14.3 требовала переиспользовать pipeline::speakers::remap_segment_times. Это невозможно и было бы ошибкой: та функция УМНОЖАЕТ таймкоды (линейная коррекция дрейфа) и работает на AttributedSegment, то есть ПОСЛЕ атрибуции. Сдвиг окна — СЛОЖЕНИЕ на WhisperSegment ДО атрибуции. Введена cloud::stt::shift_segments; на разницу есть тест, проверяющий что длительность сегмента не изменилась.

## Деградация, которую обязана учесть Ф15

Пословных меток облако не даёт. Ветка атрибуции «уверенность ниже порога ⇒ режем сегмент по границе слова» в облачном режиме обязана деградировать до пометки overlapped: разрезать нечем.

## Проверка

- 29 юнит-тестов в модуле, 17 тестов стенда, 3 живых теста
- живой прогон через собственный клиент: 3 сегмента с таймкодами у расшифровки, 5 кадров SSE у саммари, provider = Google AI Studio; потрачено 0.54 рубля
