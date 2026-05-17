# plugins/echo/src/routes/memory.rs

POST /api/echo/memories/regenerate endpoint. Принимает { scope: 'global_day'|'project_day'|'project', project_id?, day? }, валидирует обязательные поля, диспатчит на memory::summarize_day или memory::summarize_project. Синхронный с таймаутом 90с (REGENERATE_TIMEOUT). Ошибки: 400 на invalid scope/missing field/invalid date, 504 на таймаут, 500 на ClaudeRunner/DB error, 200 + {memory_id, scope, day?, project_id?} на успех. Зарегистрирован через routes::build_router → .merge(memory::router()).
