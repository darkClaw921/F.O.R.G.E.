# app.js::groupSessionsByFolder

Хелпер группировки сессий по folder_id (формат __folder:<path>) или ORPHAN_KEY для null. Сигнатура: groupSessionsByFolder(sessions, orphanKey) → Map<key, Session[]>. Внутри каждой группы сессии сортируются по name.localeCompare(). Используется в legacy renderSidebar и origin-aware renderOriginSection для согласованной папочной группировки. Экспортируется в window.__forge.groupSessionsByFolder для регресс-тестов. Файл: tmux-web/static/app.js (~947–959). Заменил groupSessionsByProject (Phase 3) — ключ изменён с project_id на folder_id.
