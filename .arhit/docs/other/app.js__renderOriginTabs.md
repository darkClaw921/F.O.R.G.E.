# app.js::renderOriginTabs

Phase 5 — Рендерит горизонтальные origin-табы над session-list: [All] [Local] [server-1] [server-2] ... [+]. Контейнер #origin-tabs (см. index.html). Hidden при remote_mode=false. Клик по табу → state.activeOrigin меняется + saveActiveOriginToStorage + перерисовка sidebar. При выборе конкретного remote — lazy-load его данных (loadRemoteSessions / loadRemoteProjects). [+] таб → openSettingsModal('remotes').
