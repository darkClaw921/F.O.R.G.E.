# codex_memory::get_memory

GET /api/codex-memory handler accepts query map for local/remote semantics. Remote server queries return an empty DTO, matching Claude memory behavior. Local queries resolve CODEX_HOME and execute load_from inside Tokio spawn_blocking. Returns serialized CodexMemory on success; blocking-task or filesystem failures become 500. Project path query fields have no effect because memory is global.
