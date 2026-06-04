# fetchNextSteps

tmux-web/static/js/sessions/sessions.js — догружает текущие эфемерные предложения «следующего шага» через GET /api/echo/next-steps и складывает в state.nextSteps (map session→{content}). Graceful: ошибка запроса НЕ пробрасывается (try/catch + console.warn), чтобы не ломать рендер сессий. Вызывается параллельно с GET /api/sessions из fetchSessions() (poll каждые 3с, Promise.all) и напрямую из echo/ws.js по WS-событию next_step_event для мгновенной реакции. Ответ {items:[{session,content,created_at}]} нормализуется в {session:{content}}.
