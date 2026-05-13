# main.rs::create_todo

Phase 3 — POST /api/todos: создаёт TODO. Body: project_id?, title (required), description?. project_id опционален (default=active). title после trim не должен быть пустым → 400. После создания → broadcast TodoEvent::Upsert. Ответ 201 + Todo JSON.
