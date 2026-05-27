# delete_session_history

DELETE /api/sessions/history, body {name,path} → state.history.remove(name,path), 200. Идемпотентно, активную tmux-сессию не трогает.
