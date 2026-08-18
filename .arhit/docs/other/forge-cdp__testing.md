# forge-cdp::testing

`plugins/forge-cdp/src/testing.rs` — помощники для тестов крейта и его потребителей. Виден снаружи только при feature `testing` (тестовый код не должен попадать в бинарь devforge), внутри крейта доступен всегда под `cfg(test)`.

Два инструмента отвечают на два РАЗНЫХ вопроса:

**`FakeCdpServer`** — «правильно ли устроен ТРАНСПОРТ»: настоящий WebSocket на свободном локальном порту, настоящие кадры, настоящий обрыв соединения, но без Chrome. `start(handler)` принимает функцию `Fn(&Value) -> Option<Value>`: вернуть кадр ответа либо `None` (так проверяется таймаут). Умеет `push(frame)` (пуш события клиенту), `received()`/`received_methods()` (что пришло) и `shutdown()` (обрыв — проверка «закрытие будит всех ждущих»). Дропается вместе с abort задачи.

**`RecordingTransport`** — «в каком порядке и с какими параметрами ушли КОМАНДЫ»: сокета нет вовсе, зато есть точный список вызовов (`calls`, `methods`, `calls_of`, `index_of`). Именно им проверяется порядок `setBypassCSP` → `grantPermissions` → `addBinding` → `addScriptToEvaluateOnNewDocument` → `setAutoAttach` → `navigate`, нарушение которого ломает инжект МОЛЧА. Умеет `set_response` (заготовленный результат), `set_failure` (ошибка протокола), `emit` (испустить событие подписчикам). Ответы по умолчанию (`Target.createTarget` → targetId, `Target.attachToTarget` → sessionId, `Runtime.evaluate`, `Browser.getWindowForTarget`) подобраны так, чтобы тест порядка команд не был обязан описывать ответ на каждую.
