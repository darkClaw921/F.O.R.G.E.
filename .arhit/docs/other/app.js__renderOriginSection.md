# app.js::renderOriginSection

Phase 5 — Рендерит одну origin-секцию: collapsible header с цветной точкой (online/offline/local/unknown), затем projects (project-sub-header) с группами сессий внутри. originKey: 'local' либо server_id. Использует isOriginCollapsed/toggleOriginCollapsed (localStorage 'forge.collapsedOrigins'). Внутри секции группирует sessions по project_id, с auto-группами по cwd, orphan секцией в конце (как в legacy renderSidebar).
