# SessionDto::origin

Phase 3 — поле origin: String в SessionDto (main.rs). Источник записи: всегда 'local' для локальных сессий. Сериализуется ВСЕГДА (даже при remote_mode=false), чтобы фронтенд получал унифицированный формат. Прокси-ответы через ?server=<id> НЕ создают SessionDto на этой стороне — там прокидывается готовый JSON remote'а, обогащённый remote_proxy::enrich_with_origin (Phase 3.4). Аналогичные поля добавлены в ProjectDto::origin и Todo::origin (см. их документацию).
