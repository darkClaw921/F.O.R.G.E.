# app.js::dtoOrigin

Phase 5 — Возвращает origin для DTO-объекта (Project/Session/Task/Todo). Бэкенд проставляет поле .origin начиная с Phase 3 (см. remote_proxy::enrich_with_origin для прокси-ответов; локальные DTO имеют origin='local'). Fallback: 'local' если поле отсутствует.
