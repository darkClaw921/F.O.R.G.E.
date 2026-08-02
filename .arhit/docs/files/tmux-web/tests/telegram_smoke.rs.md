# tmux-web/tests/telegram_smoke.rs

# tmux-web/tests/telegram_smoke.rs — интеграционный smoke плагина Telegram

Поднимает роутер плагина в текущем процессе (`forge_telegram::init` +
`register_routes`) на реальном listener'е `127.0.0.1:0`. Сети и учётных данных
Telegram не требует; конфиг изолирован во временном каталоге, домашний каталог
пользователя не затрагивается. 14 тестов, ~1.5с.

## Состояния логина

Без сети достижимы ровно два: `not_configured` (кредов нет) и `disconnected`
(креды есть, сессии нет) — `test_status_is_not_configured` и
`test_status_is_disconnected_with_credentials` (второй заодно проверяет, что
api_hash не уезжает на фронт).

## REST

`test_routes_report_expected_error_codes` держит таблицу
«метод × путь × тело -> статус + code» для логина, диалогов, сообщений,
read/typing и медиа.

Загрузка файла проверяется отдельно: `test_upload_rejects_before_reading_the_body`
шлёт СЫРОЙ HTTP по TcpStream с `Content-Length: 64 МиБ` и десятком реальных
байт — 409 обязан прийти ДО тела (если бы хендлер буферизовал, тест упал бы по
таймауту). `test_upload_without_auth_returns_contract_body` повторяет то же
обычным клиентом.

ОГРАНИЧЕНИЕ: статусы 411/413/400 у /upload и 404 unknown_dialog на этом уровне
НЕДОСТИЖИМЫ — авторизация проверяется первой, поэтому без сессии всё упирается
в 409. Эти ветки покрыты unit-тестами routes/upload.rs и routes/dialogs.rs.

## WS

`test_ws_ping_pong` (первый кадр login_state, ping/pong),
`test_ws_receives_broadcast_without_filtering`,
`test_ws_delivers_message_event_shapes` (форма кадров = контракт фронта),
`test_ws_closes_idle_connection_without_pong` и
`test_ws_keeps_alive_while_client_ponging` — последние два поднимают стенд с
укороченными таймингами через `TelegramState::with_ws_timings`.

## Opt-in

`test_routes_absent_without_the_plugin` собирает роутер БЕЗ `register_routes` и
убеждается, что весь префикс `/api/telegram/*` и `/ws/telegram` дают 404 —
главный инвариант фичи.
