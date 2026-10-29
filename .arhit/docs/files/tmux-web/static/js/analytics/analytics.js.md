# tmux-web/static/js/analytics/analytics.js

Полноэкранный вью «Аналитика Claude Code» (#analytics в #main), Фаза 5.

НАЗНАЧЕНИЕ. Показывает, как человек работает с ассистентом: KPI-плитки за период со сравнением с предыдущим таким же периодом и десять чартов по сериям из GET /api/analytics/summary. Источник — индекс транскриптов ~/.claude/projects/**/*.jsonl (tmux-web/src/analytics/*).

ПУБЛИЧНЫЙ API.
- showAnalytics(range?) — показывает вью; range из 7|30|90|all, по умолчанию сохранённый в localStorage. Механика ТОЧНО по daily-summary.js::showDailySummary (строки 436-450): скрывает #home и placeholder терминала, гасит «Сводку дня» через window.ForgeApp.hideDailySummary, вешает Esc и наблюдателя темы, грузит список проектов и summary.
- hideAnalytics() — прячет вью, гасит поллинг, наблюдателя темы и обработчик Esc, сдвигает счётчик запросов (ответы в полёте не дорисуются).
- closeAnalytics() — hideAnalytics + fetchSessions(): по образцу closeDailySummary, fetchSessions сам решает, показать #home или активную сессию.
Оба show/hide экспортированы в window.ForgeApp через js/public-api.js — на них завязаны диплинк ?view=analytics (pwa/bootstrap.js) и вкладка настроек (Фаза 6).

ЗАВИСИМОСТИ. core/dom.js ($analytics*), analytics/api.js (вся сеть), analytics/chart.js (SVG-кит и форматтеры), analytics/view-core.js (чистая логика), terminal/xterm.js::showPlaceholder, sessions/sessions.js::fetchSessions.

СОСТАВ ОТРИСОВКИ. renderKpi — 22 плитки из KPI_TILES: у каждой key в KpiDto, форматтер и better ('up'|'down'|null) для окраски дельты по СМЫСЛУ метрики; медианная пауза и доля ответов < 10 с помечены hero и идут первыми. renderCharts — карточки чартов 3, 1, 4, 5, 6, 7, 8, 9, 10, 12 плана в этом порядке; карточку строит card(title, {note, wide, plots, pair}), чарт — draw(), который запоминает хендл в _charts для redraw при смене темы. renderEmpty — осмысленное объяснение вместо сетки заглушек, когда период пуст (isEmptySummary). renderProgress / renderRefreshing / renderError — прогресс-бар первого скана, тонкая пометка фонового добора и сообщение с кнопкой «Повторить».

СЕТЬ И ГОНКИ. Только через analytics/api.js: свой fetch дал бы 401 в remote-режиме (заголовки ставит подменённый window.fetch из core/auth.js). У каждого запроса номер _seq — рисуется только ответ с текущим номером, поэтому быстрое переключение периода не приводит к отрисовке устаревших данных.

ПОЛЛИНГ. viewMode() из view-core.js разводит два случая: индекса ещё нет (idle/scanning) — только прогресс-бар; ready при progress.running — данные плюс пометка о фоновом доборе. Поллинг GET /api/analytics/status раз в 1.5 с, гаснет при готовности и при закрытии вью; разовая сетевая ошибка тика не роняет вью.

ТЕМА. MutationObserver за атрибутами style/class на <html> (глобального события смены темы в devforge нет — applyTheme просто расставляет CSS-переменные) вызывает redraw() у всех хендлов из _charts. _charts обнуляется на каждой отрисовке, иначе перекрашивались бы призраки прошлых.

ОГРАНИЧЕНИЯ. Ни одного innerHTML: подписи проектов, инструментов и моделей приходят с диска пользователя. Выбор периода и проекта хранится в localStorage (forge.analytics.range / forge.analytics.project); недоступный localStorage не ошибка. Чарты 2, 11 и 13 плана появятся в Фазе 7.

## Фаза 7 — четыре новые карточки и семь плиток
- «Когда вы работаете» — тепловая карта час × день недели за период (матрица разворачивается `view-core::weekdayHourMatrix`), ночные часы подчёркнуты, в подписи — число и доля ночных сообщений. Часы локальные: бэкенд раскладывает UTC один раз, фронт повторно не сдвигает.
- «Разнообразие — признаки автопилота» — три плота (доли TTR/повторы/слэш/односложные, энтропия инструментов в битах, уникальные проекты в день) плюс текстовое пояснение `.analytics-card-hint`: числа сами по себе ничего не говорят, а падение TTR вместе с ростом односложных подтверждений — это и есть автопилот, ради которого дашборд затевался.
- «Версии Claude Code» — чарт `band`.
- «Глобальные счётчики Claude Code» (`renderGlobals`) — топ-10 навыков, плагинов и инструментов из `~/.claude.json`, с явной подписью «за всё время — период и проект на них не влияют». Нет блока `globals` в ответе — карточки просто нет.
- Плитки: `ackShare` (hero), `ttr`, `repeatShare`, `slashShare`, `nightShare`, `toolEntropyBits`. Поле `note` плитки теперь может быть функцией от KPI.
- Геометрия горизонтальных полос вынесена в `ROW_H = 26` (высота строки) и `GLOBALS_TOP = 10`: на живой приёмке 20 строк в фиксированной высоте налезали подписями на полосы.


Текущий контейнер поддерживает два режима: showAnalytics(range?,mode?) по умолчанию открывает «Агенты», запоминая mode отдельно в forge.analytics.mode; setAnalyticsMode переключает panel/ARIA и останавливает прежние запросы/таймеры. Новый createAgentView использует /api/agents/* и собственный storage forge.agents.filters. «Работа с Claude» сохраняет прежний /api/analytics API, метрики и ключи range/project, legacy load/status имеют mode+sequence guards. hideAnalytics останавливает обе ветви. Deep link /?view=analytics открывает agents; analyticsMode=claude явно выбирает старый экран.
