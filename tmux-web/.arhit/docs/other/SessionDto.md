# SessionDto

DTO для GET /api/sessions. Расширяет tmux::SessionInfo через #[serde(flatten)]: добавляет needs_attention (bool, флаг Claude permission prompt из AttentionState), project_id (Option<String>, id проекта-владельца сессии или None для orphan), project_name (Option<String>, отображаемое имя проекта или None). Phase 1 cross-project visibility: ранее DTO содержал только needs_attention, теперь backend возвращает все сессии без фильтрации, frontend сам группирует по project_id.
