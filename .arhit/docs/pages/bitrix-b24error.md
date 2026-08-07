# bitrix-b24error — конверт ответа, таблица ошибок, политика ретраев (`plugins/bitrix/src/api/error.rs`)

Модуль отвечает на четыре вопроса и больше ни на что: что пришло от портала
(`parse_envelope`), как показать это пользователю (`classify` / `to_api_error`),
повторять ли запрос (`retry_for`) и не утёк ли секрет (`without_url`). Все —
ЧИСТЫЕ функции, поэтому вся таблица проверяется юнит-тестами, а не «когда-нибудь
на живом портале».

## Конверт

Успех: `{"result": …, "time": {…, operating, operating_reset_at}}`.
Ошибка: `{"error":"CODE","error_description":"…"}` — **с любым HTTP-статусом**
(живьём наблюдались 200, 403, 429, 503).

`parse_envelope(status, body) -> Result<B24Response, B24Error>`, порядок
проверок:

1. **пустое тело при HTTP 200 → `Transport`** (retryable);
2. пустое тело при другом статусе → `Http`;
3. тело не JSON: статус 200 → `Decode`, иначе → `Http` (отвечал посредник);
4. есть `error` → отказ портала, независимо от статуса;
5. есть `result` → успех;
6. иначе → `Decode`.

Порядок 4 перед 5 важен: `batch` возвращает `result` даже когда часть команд
провалилась, а общий `error` означает отказ всего вызова.

`B24Response` несёт `result`, `time`, а также корневые `next` / `total`
(`im.recent.list` кладёт их рядом с `result`, причём `total` живьём равен `-1` —
отсюда знаковый тип).

## Почему пустое тело при 200 — это троттлинг

Портал при превышении интенсивности не отвечает JSON-ошибкой, а обрывает ответ,
не отдав ни строчки. Классифицировать это как `Decode` значило бы сказать
«чините код» там, где надо снизить частоту, и — что важнее — запретить повтор.
Поэтому пустое тело даёт retryable `Transport` → `503 not_connected`.
Тест: `empty_body_on_200_is_treated_as_transport_error`.

## Тип ошибки

| вариант | когда |
|---|---|
| `Transport{reason}` | DNS, connect, таймаут, обрыв TCP, **пустое тело при 200** |
| `Http{status}` | статус не 200 и тело не JSON-конверт (прокси, балансировщик) |
| `Decode{reason}` | не JSON при 200 либо JSON без `result` и без `error` |
| `Api{code, description}` | отказ портала по существу |
| `Throttled{retry_after_secs}` | `QUERY_LIMIT_EXCEEDED` — первый лимит |
| `OperatingLimit{reset_at}` | `OPERATION_TIME_LIMIT` — второй лимит |

Два лимита выделены в собственные варианты, потому что от них зависит
ПОВЕДЕНИЕ транспорта, а не только текст: интенсивность лечится повтором,
`operating` — паузой конкретного метода. `code()` возвращает для них исходный
код портала, чтобы в логах они выглядели так же, как их видит администратор.

## Таблица §4 целиком

| `error` | HTTP | `code` | ретрай |
|---|---|---|---|
| `INVALID_CREDENTIALS`, `NO_AUTH_FOUND` | 409 | `not_authorized` | нет |
| `insufficient_scope` | 409 | `scope_missing` | нет |
| `WRONG_AUTH_TYPE` | 409 | `unsupported_auth` | нет |
| `ERROR_METHOD_NOT_FOUND` | 501 | `method_unavailable` | нет |
| `ERROR_NO_ACCESS`, `ACCESS_DENIED` | 409 | `send_forbidden` | нет |
| `DIALOG_ID_EMPTY`, `MESSAGE_EMPTY`, `PARAMS_ERROR`, `*_ERROR` | 400 | `invalid_input` | нет |
| `CANT_EDIT_MESSAGE` | 409 | `invalid_state` | нет |
| `QUERY_LIMIT_EXCEEDED` (503) | 429 | `flood_wait` (1 c) | **ДА** |
| `OPERATION_TIME_LIMIT` (429) | 429 | `flood_wait` (до `operating_reset_at`) | нет |
| `INTERNAL_SERVER_ERROR`, `ERROR_UNEXPECTED_ANSWER` | 502 | `bitrix_error` | **ДА** |
| пустое тело при 200 / обрыв TCP | 503 | `not_connected` | **ДА** |
| прочее | 502 | `bitrix_error` | нет |

**Порядок ветвей значим.** Правило «`*_ERROR` → `invalid_input`» стоит ПОСЛЕ
явных кодов: `INTERNAL_SERVER_ERROR` тоже оканчивается на `_ERROR`, но означает
противоположное — не «поправьте ввод», а «портал сломался, повторите».
`insufficient_scope` приходит строчными буквами; это не опечатка портала.

`to_api_error(err, now)` принимает unix-время параметром: `retry_after_secs` —
величина ОТНОСИТЕЛЬНАЯ, а `operating_reset_at` — абсолютная, и без внешних
часов арифметику нельзя было бы проверить тестом. Текст портала подмешивается
в сообщение, но проходит очистку `scrub_urls`.

## Инвариант секрета

У VK самая вероятная утечка — `error.request_params` в теле ошибки. У Bitrix24
она другая и более коварная: секрет лежит в ПУТИ запроса, а `reqwest` цитирует
URL в каждом `Display`:

```
error sending request for url (https://p.bitrix24.ru/rest/1/wqdzbf…/profile.json)
```

Защита двухступенчатая:

- `transport_error(e)` — ЕДИНСТВЕННЫЙ путь превращения `reqwest::Error` в
  `B24Error`; вызывает `reqwest::Error::without_url()`, затем
  `B24Error::without_url()`;
- `scrub_urls(text)` вычищает две формы: `http(s)://…` до разделителя и
  `/rest/<uid>/<код>` без схемы (URL мог быть разрезан чужим кодом);
- `without_url()` потребляет и возвращает ошибку — вызов нельзя «забыть на
  половине».

Тест `transport_error_strips_the_url_from_a_real_reqwest_error` сначала
УБЕЖДАЕТСЯ, что живая ошибка `reqwest` действительно содержит код вебхука
(иначе проверка потеряла бы смысл), и только потом проверяет очистку.