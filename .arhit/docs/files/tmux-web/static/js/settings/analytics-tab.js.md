# tmux-web/static/js/settings/analytics-tab.js

Вкладка «Аналитика» в модалке настроек devforge (Фаза 6 дашборда аналитики Claude Code).

НАЗНАЧЕНИЕ. Компактная витрина аналитики внутри настроек: шесть плиток и спарклайн сообщений за последние 30 дней плюс дверь в полноэкранный дашборд. Полный набор чартов, периодов и селектор проекта живут в static/js/analytics/analytics.js — вторая копия сетки карточек в панели шириной ~720px была бы нечитаемой и потребовала бы правки в двух местах при каждом изменении метрик.

ЭКСПОРТЫ.
- renderAnalyticsTab(host, deps) — рисует вкладку в переданный узел, СИНХРОННО возвращает refs (узлы + reload() + dispose() + ready-промис первого запроса). Кнопки появляются сразу, не дожидаясь сети.
- TAB_RANGE = '30' — период компактной сводки.
- TAB_TILES — состав плиток: medianPauseMs и shareUnder10s (обе hero — главный вопрос дашборда «читаю ли я код, прежде чем согласиться»), humanMessages, sessions, costUsd, tokensOut (показывает in/out парой).

DEPS (инъекция, образец meet-tab.js). getAnalyticsSummary, getAnalyticsStatus, rescanAnalytics — три роута /api/analytics/*; onClose — закрытие модалки; showAnalytics — переход во вью (по умолчанию window.ForgeApp.showAnalytics); sparkline — рисовальщик (по умолчанию lineChart из analytics/chart.js); document, setTimeout/clearTimeout, now — подменяемы ради тестов.

ЗАВИСИМОСТИ. analytics/chart.js (lineChart и форматтеры formatMs/formatPercent/formatCount/formatUsd/formatCompact) и analytics/view-core.js (rangeToQuery, viewMode, progressPercent, computeDelta, isEmptySummary). Развилки и формулы ОБЩИЕ с полноэкранным вью — собственная копия дала бы вкладку и вью, по-разному трактующие один ответ.

ОГРАНИЧЕНИЯ (проверяются tests/frontend/settings_analytics_tab.test.mjs).
1. Ни одного fetch и ни одного импорта analytics/api.js: свой fetch означал бы 401 в remote-режиме мимо apiFetch.
2. Ни одного innerHTML, включая очистку контейнера (while (n.firstChild) n.removeChild(...)): значения приходят с сервера, то есть с диска пользователя.
3. state 'scanning' → прогресс-бар и опрос status раз в 1.5 с, а НЕ нулевые плитки: индексация ленивая и первое открытие иначе выглядит как поломка.
4. state 'ready' при progress.running → ДАННЫЕ плюс пометка «цифры сейчас обновятся»: индекс поднят из кэша, в фоне дочитываются хвосты.
5. Пустой период объясняется словами, а не шестью нулями.
6. Кнопка «Открыть на весь экран» зовёт onClose() ПЕРЕД showAnalytics() — иначе дашборд окажется под оверлеем настроек.
7. dispose() снимает таймер поллинга.

CSS. Своих классов почти нет: плитки, дельты и прогресс берутся от полноэкранного вью (analytics-tile*, analytics-progress* из css/analytics.css). В css/settings-modal.css дописаны только .analytics-tab-kpi (мельче шаг сетки под ширину панели), .analytics-tab-spark и .analytics-tab-meta.

РЕГИСТРАЦИЯ. Вызывается из static/js/settings/modal.js (renderAnalyticsPanel, ленивый рендер по первому показу вкладки) — реестра табов там нет, регистрация вручную в семи местах.

## Фаза 7
Седьмая плитка — «Односложных подтверждений» (`ackShare`, hero, лучше меньше): прямой индикатор автопилота, ради которого фича и делалась, должен быть виден в компактной сводке, а не только в полноэкранном вью. Страж больше не проверяет магическое число плиток — он сверяется с `TAB_TILES.length`.

