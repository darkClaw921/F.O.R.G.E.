# main::get_sessions

HTTP-handler GET /api/sessions (src/main.rs).

Сигнатура: async fn get_sessions(State(state): State<AppState>) -> Result<Json<Vec<SessionDto>>, (StatusCode, String)>.

Что делает:
1. Читает state.projects под read-lock'ом и берёт active().tmux_prefix.
2. Вызывает tmux::list_sessions().await — список всех tmux-сессий машины.
3. Снимает snapshot из state.attention.snapshot().await — HashMap<session_name, bool> с флагами «нужно внимание».
4. Фильтрует сессии через crate::projects::session_belongs(&prefix, &s.name) — оставляет только сессии активного проекта.
5. Маппит каждую отфильтрованную SessionInfo в SessionDto { info: s, needs_attention: attention.get(&s.name).copied().unwrap_or(false) }.
6. Возвращает Json(Vec<SessionDto>). Если tmux-сервер не запущен — список пустой (Ok([]), а не 500). Любая другая ошибка list_sessions — 500.

SessionDto (выше get_sessions):
#[derive(Debug, Serialize)] struct SessionDto { #[serde(flatten)] info: SessionInfo, needs_attention: bool }
- #[serde(flatten)] раскладывает все поля SessionInfo на верхний уровень JSON — фронтенд видит прежнюю структуру + добавленное поле needs_attention. SessionInfo не модифицируется (общее с tmux.rs).
- needs_attention: true означает «в панели сессии обнаружен Claude permission prompt» — фронтенд должен подсветить вкладку оранжевым (Phase 3); false (включая отсутствие записи) — нормальное состояние.

Зависимости:
- tmux::list_sessions, tmux::SessionInfo (read).
- attention::AttentionState::snapshot (read).
- projects::session_belongs (filter).
