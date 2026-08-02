# renderHome

Асинхронный рендер экрана «Недавние сессии» (#home, tmux-web/static/js/home/home.js). Показывается в #main, когда нет активных tmux-сессий (showHome). Загружает историю GET /api/sessions/history (массив HistorySession {name, path, folder_label, windows, first_seen, last_seen}, first_seen/last_seen — Unix-СЕКУНДЫ).

ГРУППИРОВКА ПО ДНЯМ (новое): вместо плоского грида карточки сгруппированы по ДНЮ СОЗДАНИЯ сессии (first_seen) через groupByDay(records). Каждая группа — сворачиваемая секция .home-day-group, построенная buildDayGroup(dayKey, recs): заголовок .home-day-header (стрелка ▸/▾ .home-day-toggle, дата .home-day-title через humanDayLabel, счётчик .home-day-count, кнопка .home-day-restore «Открыть все (N)» → restoreDay(recs)) + грид карточек .home-day-cards (каждая — buildHomeCard). Клик по заголовку → toggleDay: инвертирует состояние, тогглит класс .collapsed и стрелку БЕЗ повторного fetch/renderHome.

СОСТОЯНИЕ СВОРАЧИВАНИЯ: модульный Map dayCollapsed (dayKey→bool «свёрнут»). Для нового дня инициализируется collapsed=(dayKey!==todayKey()) — по умолчанию раскрыт только сегодняшний день. Ручные переключения переживают периодический poll renderHome (каждые ~3с через fetchSessions), т.к. dayCollapsed не пересоздаётся; дни, исчезнувшие из истории, вычищаются (как и Map selected для мультивыбора). Пустая история → #home-empty + dayCollapsed.clear().

ХЕЛПЕРЫ ДАТ (по образцу daily-summary.js): dayKeyFromUnix(sec) → YYYY-MM-DD в локальной зоне (sec*1000); todayKey/yesterdayKey; humanDayLabel(dayKey) → «Сегодня»/«Вчера»/«21 июля 2026, суббота» (toLocaleDateString ru-RU, день недели переставлен в конец). Регистр даты НЕ трогается CSS (text-transform убран) — в русском месяц/день недели строчные.

Сортировка: группы по dayKey убыв. (свежие дни сверху), внутри группы записи по first_seen убыв. Мультивыбор (selected) и глобальные кнопки «Открыть все»/«Открыть выбранные» работают поверх групп без изменений. Регресс-тест pure-логики: tmux-web/tests/frontend/home_day_grouping.test.js (node, 20 ассертов).
