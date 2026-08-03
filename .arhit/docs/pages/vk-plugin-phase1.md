# Плагин VK — Фаза 1 (каркас)

Что появилось: crate forge-vk (plugins/vk), вкладка #vk во фронте, opt-in флаг --vk. Данных VK ещё нет — есть полностью встроенный пустой плагин.

## Backend
- plugins/vk/Cargo.toml — reqwest из workspace + feature multipart, tokio-util(io), toml, uuid, mime_guess; dev: tempfile, tower. Новых dev-зависимостей (wiremock/httpmock) нет и не будет: стенд Ф2 пишется на axum+tokio.
- src/lib.rs — четыре точки входа без host-параметра (init / register_routes / spawn_workers / shutdown).
- src/config.rs — SecretToken БЕЗ Serialize (утечка = ошибка компиляции), цепочка default -> ~/.config/forge/vk.toml -> FORGE_VK_* -> validate_and_fix, атомарная запись 0600, починка прав шире 0600 при загрузке.
- src/login.rs — 7 состояний автомата (чистая половина; сеть — Ф3).
- src/dto.rs — форма проводных структур (наполнение Ф4/Ф6/Ф7/Ф9).
- src/state.rs — VkState, ConnHealth с грейсом 3с, курсор LongPoll, реестр загрузок.
- src/routes/{mod,error}.rs — таблица путей, GET /api/vk/status, конверт ошибок из 16 кодов.
- src/ws/{mod,protocol}.rs — /ws/vk и набор кадров.

## Хост
Cargo.toml (members + reqwest в [workspace.dependencies]), tmux-web/Cargo.toml (reqwest = workspace + path-dep), main.rs (регистрация после телеграмного блока и ДО .fallback/.layer, warn вместо падения, shutdown), cli.rs + server_config.rs + daemon.rs (флаг --vk целиком, включая проброс в дочерний процесс).

## Frontend
index.html (кнопка #vk-toggle, пейн #vk), css/vk.css (терминальная стилистика, синий акцент, контрасты прогнаны чекером), @import в style.css, секция VK в js/core/dom.js (48 ref'ов), ветки в bootstrap.js и tabs.js, js/vk/{state,theme,mobile,main}.js, ПЕРВЫЙ файл общего слоя js/messenger/theme-core.js + его node-тест.

## Инфраструктура
.arhit/config.json (sourcePaths += plugins/vk/src), .githooks/pre-commit (cargo test --workspace вместо cd tmux-web && cargo test — прежняя команда не запускала юнит-тесты плагинов вовсе).

## Инварианты фазы, проверенные гейтом
- cargo tree -i openssl-sys пуст; единственная версия reqwest в дереве.
- cargo test --workspace и cargo clippy --workspace --all-targets зелёные.
- test_routes_absent_without_the_plugin: без register_routes весь /api/vk/* и /ws/vk отдают 404.
- GET /api/vk/status без токена -> 200 not_configured; токен не появляется в ответе ни целиком, ни отпечатком.
- js/telegram/* и css/telegram.css НЕ редактировались (git diff по ним пуст) — это инвариант всего проекта, а не только Ф1.

## Три замороженных решения по именам (Ф1.1)
1. /api/vk/dialogs, а не /api/vk/conversations.
2. no_messages_scope, а не insufficient_scope.
3. vk_visible_kinds / vk_visible_peers, а не filters / dialogs.
Таблицы-близнецы: шапка plugins/vk/src/routes/mod.rs и шапка tmux-web/static/js/vk/state.js.