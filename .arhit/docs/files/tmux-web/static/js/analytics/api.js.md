# tmux-web/static/js/analytics/api.js

Клиент REST-API аналитики использования Claude Code (Фаза 3). ТОЛЬКО сеть — ни одного узла DOM, ни одной строки разметки; данные отсюда берут полноэкранный дашборд (Фаза 5) и вкладка настроек (Фаза 6).

Устройство: локальный call(path, init) поверх apiFetch из core/api.js (образец js/echo/api.js:22-41) — разбирает JSON, при не-2xx бросает Error со status и body. Прямой fetch() ЗАПРЕЩЁН и проверяется тестом: он прошёл бы мимо подмены window.fetch из core/auth.js и в remote-режиме получил бы 401 без bearer-токена и CSRF-заголовка.

Экспортируемая таблица ANALYTICS_ROUTES (образец js/meet/api.js) — контракт с роутером Rust, сверяется в обе стороны тестом tmux-web/tests/analytics_routes_contract.rs. path — ДОСЛОВНАЯ строка из .route(...) в main.rs, сравнение текстовое.

Функции:
- getAnalyticsStatus() → GET /api/analytics/status. ЭТОТ ЗАПРОС ЗАПУСКАЕТ индексацию, если она не запускалась: на старте devforge транскрипты намеренно не читаются. Первый ответ почти всегда state:'scanning' — это норма, а не ошибка.
- getAnalyticsProjects() → GET /api/analytics/projects. Список проектов для селектора, по убыванию последней активности. Пустой массив во время первого скана валиден.
- getAnalyticsSummary({from,to,project,bucket}) → GET /api/analytics/summary. Пустой/отсутствующий параметр не отправляется вовсе — сервер подставит умолчание (весь диапазон индекса, bucket=day, все проекты). Ответ во время индексации ТОЖЕ 200 со state:'scanning' и пустыми сериями: проверять надо state, а не наличие данных.
- rescanAnalytics() → POST /api/analytics/rescan. Возвращает сразу текущий статус; пересбор идёт в фоне, дальше поллинг getAnalyticsStatus до 'ready'. Повторный вызов во время скана второго прохода не запускает.

Зависимости: core/api.js (apiFetch). Файл лежит в static/, поэтому попадает в бинарь через rust-embed (static_embed.rs).
