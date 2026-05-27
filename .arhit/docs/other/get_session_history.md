# get_session_history

GET /api/sessions/history. Возвращает Json(state.history.list()) — журнал HistorySession, сортировка по last_seen убыв. Активные и закрытые сессии вперемешку, фронт сам помечает запущенные.
