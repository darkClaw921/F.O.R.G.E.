# forge-cdp::CdpTransport

`trait CdpTransport` (plugins/forge-cdp/src/transport.rs) — единственная граница, за которой прячется конкретная реализация протокола.

## Зачем трейт, если реализация одна

Решение «пишем свой CDP-клиент поверх tokio-tungstenite вместо chromiumoxide» — ставка (обоснование: второй ws-стек роняет `cargo tree -d`). У ставки должен быть путь отхода: если flat-сессии, автоаттач в OOPIF или биндинги окажутся неподъёмными, замена на chromiumoxide обязана быть работой ВНУТРИ `forge-cdp`, а не переписыванием `forge-meet`. Поэтому весь остальной крейт (`session`, `page`, `inject`) работает не с сокетом, а с `Arc<dyn CdpTransport>` (псевдоним `SharedTransport`).

Что означает подмена на практике: реализовать ровно три вещи — отправку команды с ожиданием ответа, широковещательную подписку на события, закрытие. Список фактически используемых методов протокола — в документации крейта `forge-cdp`.

Второе следствие: тесты крейта и его потребителей идут против `testing::RecordingTransport` и не требуют ни живого Chrome, ни сети. Проверка «в каком порядке ушли команды» — проверка списка записанных вызовов.

## Поверхность

- `async fn send(&self, method, params: Value, session_id: Option<&str>) -> Result<Value, CdpError>` — возвращает `result` целиком; ошибка Chrome → `CdpError::Protocol` с кодом; истёкший таймаут → `CdpError::Timeout`, а не вечное ожидание; закрытие сокета в полёте → `CdpError::TransportClosed`; вызовы конкурентны (роутинг по `id`).
- `fn subscribe(&self) -> broadcast::Receiver<CdpEvent>` — поток событий ВСЕХ целей; фильтрация по `EventFilter` на стороне подписчика, потому что в flat-режиме канал общий. ПОДПИСЫВАТЬСЯ НУЖНО ДО отправки команды, порождающей событие: иначе окно между send и subscribe теряет событие, а выглядит это как «иногда зависает».
- `fn is_closed(&self) -> bool`, `async fn close(&self)` — закрытие будит все ждущие команды `TransportClosed`.

## TransportConfig

`command_timeout` (дефолт 30 с) — ПОЛЕ, а не константа: у `Page.navigate` живой встречи и у `Runtime.evaluate` на мок-странице разумные пределы отличаются на порядок. `event_buffer` (дефолт 1024) — ёмкость broadcast: медленный подписчик обязан потерять СТАРЫЕ события, а не затормозить роутер (`RecvError::Lagged` виден в логе).

## wait_for_event

Свободная функция, а не метод трейта, — именно потому что приёмник открывается заранее. Отставание подписчика (`Lagged`) не считается ошибкой ожидания: канал специально теряет старые события под нагрузкой (`Runtime.bindingCalled` на каждый кадр PCM), потеря логируется и ожидание продолжается.

## Типы протокола (protocol.rs)

`CdpCommand{id, method, params, session_id}` с `to_wire()` (опускает пустые `params`/`sessionId`); `CdpResponse{id, session_id, result: Result<Value, ProtocolError>}`; `CdpEvent{method, params, session_id}`; `ProtocolError{code, message, data}`. `CdpMessage::parse` различает ответ и событие ПО НАЛИЧИЮ ПОЛЯ `id` — другого признака протокол не даёт; кадр без `id` и без `method` — ошибка `Malformed`, а не тишина (иначе несовместимость версии Chrome проявилась бы как «ничего не происходит»).

`CdpError` разделяет Timeout / TransportClosed / Protocol{code} / Malformed / Ws / UnexpectedShape / Launch / ChromeNotFound / JsException / ElementNotFound / Io. Разделение прикладное: `-32000 Cannot find context` значит «цель уехала, переподцепиться», обрыв сокета значит «браузера нет». `is_retryable()` истинно только для Timeout; `protocol_code()` даёт код без разбора текста.
