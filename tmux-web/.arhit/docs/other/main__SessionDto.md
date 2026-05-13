# main::SessionDto

DTO в src/main.rs (~line 190), сериализуемый в JSON для GET /api/sessions. Структура: name, attached, windows (всё из tmux::SessionInfo через #[serde(flatten)]) + needs_attention: bool (флаг Claude permission prompt из AttentionState) + project_id: Option<String> (id проекта-владельца сессии или None для orphan) + project_name: Option<String> (отображаемое имя проекта или None).

Cross-project sessions visibility (Phase 1): backend больше не фильтрует сессии по активному проекту — возвращает ВСЕ tmux-сессии, frontend сам группирует по project_id и применяет UI-фильтр. project_id/project_name заполняются в get_sessions через projects::session_belongs(prefix, name): ищется первый проект чей tmux_prefix матчит имя сессии; если ни один не подошёл — оба поля None (orphan).

Поле needs_attention заполняется из snapshot'а AppState.attention в хендлере: attention.get(&s.name).copied().unwrap_or(false). Snapshot снимается ОДИН РАЗ за вызов хендлера, чтобы все сессии в ответе видели согласованное состояние watcher'а.

Используется фронтендом app.js при polling /api/sessions каждые 3с. Файл: src/main.rs.
