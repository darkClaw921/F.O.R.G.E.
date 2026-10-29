# tmux-web/static/js/tracker/gate.js

Гейт вкладки «Мои задачи» (плагин forge-tracker, opt-in --tracker).

Крошечный модуль, вынесенный из main.js РАДИ ЛЕНИВОСТИ: гейт обязан отработать на каждой загрузке страницы, а панель целиком — нет. main.js тянет за собой рендер ленты, WebSocket, разбор строки и карточку задачи; грузить это тем, кто в трекер сегодня не зайдёт, незачем. Поэтому здесь только один запрос и одна кнопка, а тяжёлая часть подгружается динамическим import() при первом открытии вкладки (js/tabs/tabs.js).

Функции:
- initTrackerGate() — ЕДИНСТВЕННЫЙ GET /api/tracker/status за загрузку страницы. При 200 и enabled=true снимает hidden с кнопки #tracker-toggle и запоминает ответ. При 404 (devforge запущен без --tracker) не делает НИЧЕГО и не пишет в консоль ни строчки — это штатный путь. Не бросает никогда.
- trackerStatus() — ответ сервера или null. Панель берёт отсюда store_path для статус-бара (строка GLOBAL JSON …/tracker.json).
- isTrackerEnabled() — включён ли плагин; main.js проверяет это в initTracker/suspend/resume.

Зависимости: js/core/dom.js ($trackerToggle), js/tracker/api.js (getStatus).
Зовётся из js/core/bootstrap.js рядом с гейтами Telegram, VK, Битрикса и встреч.
