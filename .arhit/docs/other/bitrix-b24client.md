# bitrix-b24client

# bitrix-b24client — транспорт плагина Bitrix24 (`plugins/bitrix/src/api/mod.rs`)

`B24Client` — ЕДИНСТВЕННАЯ точка выхода плагина в сеть. Всё, что уходит в
портал (методы Ф3–Ф10, `batch`, будущие медиа и загрузки), проходит здесь.

```
methods.rs ─┐
batch.rs   ─┴→ B24Client::call_value → throttle(2 лимита) → POST JSON → parse_envelope
                                             ↑                              ↓
                                    пауза метода / ведро            ретрай + ConnHealth
```

## Ответственность (пять пунктов, и ни одного больше)

1. **Сборка URL** `<portal>/rest/<uid>/<code>/<method>.json`. База берётся из
   `BitrixConfig::base_url_override`, если он задан (стенд, правило Р17), иначе
   из самого вебхука (`WebhookUrl::api_base()`).
2. **Формат вызова**: `POST`, `Content-Type: application/json`, параметры —
   JSON-телом. Вложенные структуры (`ATTACH`, `KEYBOARD`, `fields`, `filter`,
   `select`) в query-string ломаются, поэтому другой формы нет. Тип параметров —
   `serde_json::Value`, а не список пар как у VK: половина полезных параметров
   портала вложенная.
3. **Оба лимита портала** — один `Throttle` на клиент (см. `bitrix-throttle`).
4. **Ретраи**: не более ОДНОГО повтора и только по таблице `retry_for`.
5. **Здоровье соединения**: успех → `ConnHealth::Ok`, транспортный отказ →
   `ConnHealth::Down`. Без этой отметки ответ `503 not_connected` у data-роутов
   был бы ни на чём не основан. Ячейка `Arc<RwLock<ConnHealth>>` приходит
   снаружи — клиент и `B24State` обязаны смотреть в ОДНУ.

## Ключевые методы

| метод | что делает |
|---|---|
| `new(&cfg, &webhook, health)` | собирает клиент; ошибка, если нет ни вебхука, ни override |
| `call_value(method, params) -> B24Response` | вызов с сырым конвертом |
| `call::<T>(method, params) -> T` | то же + типизация `result` |
| `batch(calls) -> BatchOutcome` | пакет с авто-нарезкой (см. `bitrix-batch`) |
| `throttle()` / `throttle_stats()` | общий бюджет и счётчики ожиданий |
| `with_retry_backoff(d)` / `with_throttle(t)` | подмена таймингов для тестов |

Типизированные обёртки (`profile`, `scope`, `counters`, `server_time`) живут в
`api/methods.rs` в едином стиле: константа `M_*` с именем метода + типизированные
параметры + типизированный ответ из `api/schema.rs`.

## Почему таймаут обязателен, а не желателен

Превышение лимита интенсивности у Bitrix24 выглядит НЕ как JSON-ошибка, а как
зависание или обрыв TCP: портал просто перестаёт отвечать. Без клиентского
таймаута такой вызов держал бы задачу неопределённо долго и не породил бы
ошибки, которую можно было бы повторить. Оба таймаута — данные конфига:
`connect_timeout_secs` (10 с) отличает «сети нет» от «портал думает»,
`request_timeout_secs` (30 с) закрывает зависание.

## Р9: отправка не повторяется НИКОГДА

`NO_RETRY_METHODS = ["im.message.add", "im.v2.file.upload"]`. У этих методов нет
дедуп-идентификатора (аналога `random_id` у VK): портал принимает каждый вызов
как новое сообщение, поэтому повтор после обрыва — это дубль у собеседника.

Запрет живёт В ТРАНСПОРТЕ, а не в вызывающем коде, поэтому любой будущий роут
получает его автоматически. Сравнение регистронезависимое, и запрет НЕ
обходится вложением отправки в `batch`: `is_retryable` заглядывает внутрь
`cmd`. Идемпотентные соседи (`im.message.update`, `im.message.delete`)
повторяются штатно.

Тест: `transport_error_is_retried_once_and_message_add_never` — счётчик стенда
показывает 2 запроса для обычного метода и ровно 1 для отправки.

## Инвариант секрета

У Bitrix24 секрет — ВЕСЬ URL целиком, включая хост портала. Поэтому:

- `Debug` клиента печатает `base_url = "<redacted>"` (у VK там честный
  `api.vk.com` — у нас так нельзя);
- в логи уходят только `method`, `elapsed_ms`, `attempt` и КОД ошибки; ни URL,
  ни параметры не логируются никогда;
- каждая транспортная ошибка проходит `error::transport_error`, то есть
  `reqwest::Error::without_url()` + собственный `scrub_urls`;
- причина в `ConnHealth::Down` тоже чистится: она уезжает в `GET /status`.

Стражи: `webhook_code_never_appears_in_logs_or_errors` и
`no_source_line_logs_the_exposed_url` (`tests/b24_stub.rs`).

## Один клиент на секрет

`B24State::api_client()` кэширует клиент, ключуя его САМИМ секретом
(double-check под write-lock). Два клиента — это два ведра по 2 rps, то есть
4 rps в портал, который считает бюджет на пару портал+IP и отвечает на
превышение обрывами соединений. Смена вебхука сбрасывает кэш в
`B24State::set_webhook`, а проверка ключа в `api_client()` — вторая линия.

МЕТОДЫ, ДОБАВЛЕННЫЕ Ф6 (api/methods.rs).
message_add(dialog_id, text, reply_id) -> i64 — im.message.add. НЕ РЕТРАИТСЯ (Р9, NO_RETRY_METHODS). Результат приходит числом, но разбирается и строкой (message_id_from_result): промах означал бы, что успешно отправленное сообщение считается неотправленным, и пользователь жмёт «повторить», получая дубль.
message_update(message_id, text) -> bool — im.message.update. Пустой текст до сети не доходит: у портала он УДАЛЯЕТ сообщение.
message_delete(message_id) -> bool — im.message.delete.
message_like(message_id, LikeAction) -> bool — im.message.like; ACTION auto|plus|minus, метод ПЕРЕКЛЮЧАЮЩИЙ.
dialog_read(dialog_id, message_id?) -> i64 — im.dialog.read; наружу отдаётся только counter (ответ портала — объект, где dialogId приезжает ЧИСЛОМ при документированной строке).
dialog_unread(dialog_id, message_id) -> bool — im.dialog.unread; message_id ЗНАЧЕНИЕМ, не Option: без него портал отвечает MESSAGE_ID_ERROR, и вызов невозможен по типам.
dialog_writing(dialog_id) -> bool — im.dialog.writing (только исходящее).
recent_pin(dialog_id, pin) -> bool, recent_hide(dialog_id) -> bool — Р14; булев флаг у портала это "Y"/"N".
call_flag() — общий приватный помощник командных методов: успехом считается сам факт успешного конверта, а не строгий bool в result (портал отвечает то true, то объектом, то числом, и ронять исправную операцию из-за формы неиспользуемого ответа нельзя).
