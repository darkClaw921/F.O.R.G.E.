# tmux-web/static/css/layout.css

Основной grid-layout приложения: #layout (CSS grid: sidebar + main), #main (flex column контейнер для tab-bar/контента/window-bar), #placeholder (заглушка когда нет активной сессии), .layout-collapsed (сжатый сайдбар), .sidebar-toggle-btn (кнопка-гамбургер в tab-bar). 80 строк. Импортируется ВТОРЫМ после base.css — задаёт высокоуровневую геометрию страницы.
