# tmux-web/tests/frontend/settings_analytics_tab.test.mjs

Страж вкладки настроек «Аналитика» (Фаза 6). 55 проверок, запуск: node tmux-web/tests/frontend/settings_analytics_tab.test.mjs, код 0/1.

Грузит РЕАЛЬНЫЙ модуль static/js/settings/analytics-tab.js на фейковом документе из meet_popup_dom.mjs; сеть приходит инъекцией, поэтому ни браузера, ни сервера не нужно — и этим же доказано, что вкладка не ходит никуда, кроме переданных функций.

ЧТО ПРОВЕРЯЕТСЯ.
1. Готовый индекс: шесть плиток, границы периода from/to ровно за 30 дней включая сегодня, hero-плитка медианной паузы первой, цвет дельты ПО СМЫСЛУ метрики (падение медианной паузы — is-bad, падение доли «не читая» — is-good), данные спарклайна и legend:false, подпись «индекс обновлён».
2. Пустой период объяснён словами, кит графиков на пустом ряде не зовётся.
3. state 'scanning' → прогресс-бар, плитки скрыты, ширина полосы 25% на 3 из 12, запущен опрос status; следующий тик по готовности перечитывает summary и рисует плитки.
4. state 'ready' + progress.running → данные, а не прогресс, плюс пометка об обновлении.
5. Кнопки: порядок close → show; пересбор блокирует кнопку и показывает прогресс; ошибка пересбора её разблокирует и называет причину; упавший summary не роняет вкладку; dispose() снимает таймер.
6. СЕМЬ ТОЧЕК РЕГИСТРАЦИИ в static/js/settings/modal.js — текстом по исходнику: импорт renderAnalyticsTab, data-tab=analytics, id=ps-panel-analytics + data-panel, querySelector('#ps-analytics-content'), analyticsTabState, name === 'analytics' + renderAnalyticsPanel(), initialTab === 'analytics'. Плюс: переданы все три функции api, взяты из ../analytics/api.js, рендер ленивый, onClose: close. Смысл — поймать частичную регистрацию при будущих правках modal.js: реестра там нет намеренно, и пропуск любой точки даёт молча неработающую вкладку.
7. В файле вкладки нет прямого fetch и нет innerHTML (комментарии из проверки вырезаются).

МУТАЦИОННО ПРОВЕРЕНО: снятие whitelist initialTab и порча селектора контента роняют тест.

Подхватывается tests/analytics_frontend_suite.rs по маске (settings_analytics_tab), то есть идёт в обычном cargo test -p devforge.
