# restore_session_history

POST /api/sessions/history/restore, body {name,path}. 409 если сессия уже в tmux::list_sessions(); иначе restore_one_session → 201 + {name}. 400 при невалидном теле/сбое создания.
