# tmux-web/static/js/echo/memory.js

Echo memory viewer/editor. initMemoryPane — табы global_day/project/project_day + regenerate button. setMemoryFilters(projectId, day) — обновить активный project_id и day. refreshMemory — listMemories по текущему scope+фильтрам. beginEdit — inline textarea для patch. runRegenerate — валидирует обязательные params (project_id для project/project_day, day для global_day/project_day), POST /api/echo/memories/regenerate.
