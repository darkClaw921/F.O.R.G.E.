# app.js::renderSidebarWithOrigin

Phase 5 — Origin-aware рендер sidebar. Запускается из renderSidebar при isRemoteMode()=true. Структура: origin-group-header (collapse при клике) → project-sub-header → session-item.in-origin. Фильтр state.activeOrigin: 'all' (все), 'local' (только локальный), <server_id> (один remote). При первом рендере remote-секции (не свёрнута и нет кэша) — вызывает loadRemoteProjects/loadRemoteSessions; после загрузки sessions делает rerender sidebar.
