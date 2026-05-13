# main.rs::patch_todo

Phase 3 — PATCH /api/todos/:id: обновляет title/description. 404 если id не найден. После апдейта → broadcast Upsert. description: отсутствует=не трогать, null=стирает (через custom deserialize_optional_optional_string).
