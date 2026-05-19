# buildSessionItem

Создаёт DOM <li class='session-item'> для одной сессии (tmux-web/static/js/sessions/sessions.js):
- .active при s.name === state.currentSession.
- .needs-attention при s.needs_attention (оранжевая подсветка, Claude permission/plan/question prompt).
- dataset.session=s.name.
- .session-meta: имя + подстрока 'N windows · attached(K)'.
- .session-actions: кнопка rename (stopPropagation + renameSession) + кнопка kill (stopPropagation + killSession).
- При s.is_generating добавляет отдельный <span class='claude-spark' title='Claude генерирует'>✶</span> ПОСЛЕ .session-actions. Span позиционируется CSS-классом absolute в правом нижнем углу li (см. sidebar.css). Кнопки rename/kill при этом остаются на своих местах — span поверх них в углу, не сдвигает flex-row.
- click-обработчик на li → openSession(s.name, sessOrigin).

Вынесена из renderSidebar чтобы переиспользовать рендер строки в обоих режимах фильтра (all-projects / single-project).

dtoOrigin(s) → 'local' | <remote-server-id>: пробрасывается во все session-actions для корректной маршрутизации API-запросов через apiFetch.
