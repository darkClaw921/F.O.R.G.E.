# forge-cdp

Листовой крейт `plugins/forge-cdp` (пакет `forge-cdp`) — тонкий клиент Chrome DevTools Protocol, фундамент плагина `forge-meet` (бот видеовстреч). Добавлен в members корневого Cargo.toml в фазе 1 эпика forge-meet-f1-mriy.

## Зачем крейт существует и почему он именно такой

**1. Свой клиент, а не chromiumoxide.** `chromiumoxide` тянет `async-tungstenite` — ВТОРОЙ ws-стек в дерево, где уже живёт `tokio-tungstenite 0.24` (tmux-web/Cargo.toml, используется в `remote_proxy.rs`). Гейт `cargo tree -d` краснеет немедленно. Плагину нужно ~20 методов протокола, все из его стабильного ядра, — это дешевле второго стека и мегабайтов генерируемых типов. Node-сайдкар на Playwright исключён как регресс упаковки: дистрибуция devforge — один Rust-бинарь через brew.

**2. Крейт ЛИСТОВОЙ.** Ни одной зависимости на другой член workspace — ни на devforge, ни на плагины, ни на `forge-atomic` (хотя `chrome.pid` просился бы под атомарную запись). Это условие подмены реализации: если tokio-tungstenite-путь окажется тупиковым, замена локализована внутри крейта и не задевает `forge-meet`.

**3. Payload'ы в `serde_json::Value`.** CDP дрейфует между версиями Chrome; мы используем малую его часть и владеем дрейфом руками, в точках, где реально читаем поля.

## Зависимости — ни одного нового узла в дереве

`tokio` (workspace, features=full), `tokio-tungstenite 0.24` (features УЖЕ, чем у tmux-web: `connect`+`handshake`, без TLS — CDP всегда ws://127.0.0.1), `futures-util`, `serde`, `serde_json`, `async-trait`, `tracing`, `base64 0.22` (PNG скриншота приходит base64-строкой), `libc 0.2` (setsid/kill/killpg/getpgid), `uuid 1.23` (случайное имя биндинга на встречу). Dev: `tempfile`.

Отдельно: взят `libc`, а НЕ `nix`, хотя задача упоминала оба. Причина — `nix` в дереве присутствует в ДВУХ версиях сразу (0.25.1 через portable-pty и 0.31.3 через os_info → grammers-mtsender). Прямая зависимость закрепила бы чужой транзитивный выбор, а промах версией дал бы третий узел. `libc` в дереве ровно один и даёт все четыре нужных вызова; `tmux-web/src/daemon.rs` делает через него тот же `setsid`.

Feature `testing` (по умолчанию выключена) открывает наружу модуль `testing` с `RecordingTransport` и `FakeCdpServer` — чтобы тестовый код не попадал в бинарь devforge, но был доступен тестам `forge-meet`.

## Структура

- `protocol.rs` — типы провода (`CdpCommand`, `CdpResponse`, `CdpEvent`, `ProtocolError`), разбор кадра `CdpMessage::parse`, `CdpError`, фильтры событий.
- `transport.rs` — `trait CdpTransport` (граница подмены) + `TransportConfig` + `wait_for_event`.
- `ws.rs` — боевая реализация транспорта поверх tokio-tungstenite: роутер ответов по id, broadcast событий.
- `session.rs` — `CdpBrowser`/`CdpSession`, flat-сессии, насос автоаттача с гарантированным `Runtime.runIfWaitingForDebugger`.
- `page.rs` — порядок команд старта цели, ввод (`Input.dispatchMouseEvent`/`insertText`), окно (`Browser.setWindowBounds`), скриншот.
- `inject.rs` — генерация исходника инжекта с биндингом, ring buffer и удалением глобала.
- `launch.rs` — запуск Chrome, флаги, `DevToolsActivePort`, файл тишины, `chrome.pid`.
- `orphans.rs` — подметальщик осиротевших Chrome (`killpg` с двойной проверкой).

## Список методов CDP, который придётся поддержать при подмене реализации

Target: createTarget, attachToTarget, setAutoAttach, closeTarget, событие attachedToTarget. Page: enable, navigate, setBypassCSP, addScriptToEvaluateOnNewDocument, captureScreenshot, события loadEventFired/frameNavigated. Runtime: enable, evaluate, addBinding, runIfWaitingForDebugger, событие bindingCalled. Input: dispatchMouseEvent, insertText. Browser: grantPermissions, getWindowForTarget, setWindowBounds, getWindowBounds. Log: enable, событие entryAdded.
