# bitrix-b24stub — локальный стенд портала (`plugins/bitrix/tests/b24_stub.rs`)

Стенд, полностью заменяющий живой портал Bitrix24 в тестах. Секретов не
требует, наружу не ходит и гоняет ВЕСЬ транспорт через настоящий `reqwest`:
сборку URL, разбор конверта, оба лимита, политику ретраев, нарезку `batch` и
очистку секрета из логов.

## Почему свой стенд, а не wiremock/httpmock

Новых dev-зависимостей крейт не заводит намеренно (инвариант дерева:
`cargo tree -d` без новых дублей). Стенд собран из уже имеющихся `axum` +
`tokio::net::TcpListener::bind("127.0.0.1:0")` и занимает меньше места, чем
конфигурация мок-сервера. Ровно ради него существует
`BitrixConfig::base_url_override`, а правило Р17 разрешает `http://` на
петлевой адрес.

Порт всегда `127.0.0.1:0` — ОС выдаёт свободный. Жёсткий порт однажды упёрся бы
в живой инстанс devforge на 7331.

## Что умеет изображать

| сценарий | `Reply` | зачем |
|---|---|---|
| успех с `time` | `Result` / `ResultWithTime{operating, reset_in}` | обычный ответ + оба поля второго лимита |
| отказ портала | `ApiError{status, code, description, reset_in}` | вся таблица §4, с ЛЮБЫМ HTTP-статусом |
| интенсивность | `Reply::query_limit()` — 503 `QUERY_LIMIT_EXCEEDED` | первый лимит |
| operating | `Reply::operating_limit(n)` — 429 + `operating_reset_at` | второй лимит |
| **пустое тело при 200** | `EmptyOk` | так выглядит троттлинг живьём |
| обрыв TCP | `Drop` — заголовки доехали, тело нет | ретрай транспорта |
| зависание | `Hang(d)` | портал перестал отвечать; ловится таймаутом |
| мусор вместо JSON | `Garbage(status)` | captive portal, балансировщик |

Скрипт — очередь на метод: `state.script("server.time", vec![…])`. Очередь
кончилась ⇒ дефолтный успех, слепленный с ЖИВЫХ ответов тестового портала
(`profile`, `scope`, `im.counters.get`, `server.time`).

## Журнал вызовов

`Call{method, code, body, at, batch_len}`. Записывается КОД ВЕБХУКА ИЗ ПУТИ —
по нему тест видит, что клиент собрал URL по контракту §4. `at` нужен тестам
троттлинга (интервалы), `batch_len` — тесту нарезки (50 + 5), счётчики —
инварианту «отправка не ретраится».

## batch

Стенд демультиплексирует `cmd` и отвечает теми же ключами. Команды после 50-й
не выполняются и приезжают в `result_error` с `ERROR_BATCH_LENGTH_EXCEEDED` —
БЕЗ общей ошибки, ровно как портал. `fail_in_batch(method)` заставляет
конкретный метод отказать внутри пачки.

## Захват логов

Своя копия `src/testlog.rs`: интеграционный тест — отдельный бинарь, и
`#[cfg(test)]`-модули крейта ему не видны. Подписчик ГЛОБАЛЬНЫЙ и ставится один
раз (`OnceLock`) из `start_stub`: `tracing` кэширует «интерес» к callsite один
раз на процесс, и тест, выполнившийся первым без подписчика, навсегда пометил бы
`warn!` в `api/mod.rs` неинтересным.

## Переменные окружения

Стенд их НЕ трогает. Тесты собирают `BitrixConfig` значением и зовут
`B24Client::new` напрямую, поэтому `FORGE_BITRIX_CONFIG_PATH` не нужен, а
`set_var` в многопоточном тестовом бинаре был бы гонкой. Если тест будущей фазы
позовёт `forge_bitrix::init`, переменную придётся выставить — Р17 иначе
отвергнет `http://127.0.0.1`.

## Тесты фазы 2 поверх стенда

`throttle_spaces_calls_evenly`, `operating_limit_blocks_only_one_method_until_reset_at`,
`empty_body_on_200_is_treated_as_transport_error`,
`batch_splits_at_fifty_and_demuxes_result_error`,
`transport_error_is_retried_once_and_message_add_never`,
`webhook_code_never_appears_in_logs_or_errors`, `no_source_line_logs_the_exposed_url`.