# initTrackerGate

Гейт вкладки трекера (tmux-web/static/js/tracker/gate.js).

Единственный GET /api/tracker/status за загрузку страницы. При 200 снимает hidden с кнопки вкладки #tracker-toggle, при 404 не делает ничего и НЕ пишет в консоль ни строчки: 404 — штатный путь для всех, кто запустил devforge без --tracker.

Модуль вынесен из main.js ради ленивости: bootstrap.js статически импортирует только gate.js, а тяжёлую панель (рендер, сокет, разбор строки, карточка) tabs.js подгружает динамическим import('../tracker/main.js') при первом открытии вкладки. Никогда не бросает. Ответ гейта доступен через trackerStatus() / isTrackerEnabled().
