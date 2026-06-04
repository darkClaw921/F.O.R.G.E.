# renderHome

tmux-web/static/js/home/home.js. Рендерит экран «Недавние сессии»: GET /api/sessions/history → карточки buildHomeCard в #home-cards. Кнопки заголовка навешиваются один раз (headerBound): «Открыть все» → restoreAll, «Открыть выбранные (N)» → restoreSelected. Каждый перерендер делает selected.clear()+updateSelectionUI(). Карточки кликабельны для мультивыбора (см. [[restoreSelected]]); кнопки ▶ Запустить → restoreSession и ✕ → deleteHistory используют stopPropagation, чтобы не триггерить выбор. Пустая история → #home-empty, кнопки скрыты.
