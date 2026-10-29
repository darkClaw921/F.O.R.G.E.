# AgentStatsStore::flush

Persist the current metadata-only source/cursor snapshot on graceful shutdown, including valid partially indexed sources. Waits for the ingestion gate, then saves via forge_atomic in spawn_blocking without holding the published read lock. Startup can resume the incomplete cursor rather than replay private payloads. Server shutdown should abort recurring ingestion/runtime jobs before flush; sources remain authoritative if the cache is missing or invalid.
