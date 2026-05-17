# plugins/echo/src/db/repo/chats.rs

CRUD-репозиторий для таблицы chat_sessions. ChatSession{id (UUIDv4), title, project_id (Option, soft-FK на хост-projects), model, created_at, updated_at}, Serialize. API: create(db,title,project_id?,model)→ChatSession; list(db,project_id?,limit)→ORDER BY updated_at DESC; get(db,id)→Option; delete(db,id)→cascade удаляет messages благодаря FK; touch_updated(db,id) обновляет updated_at на текущий ts (зовётся при инсерте message). Все вызовы через tokio_rusqlite Connection::call (blocking thread). 3 unit-теста.
