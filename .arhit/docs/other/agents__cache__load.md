# agents::cache::load

Loads regular private cache files up to256MiB, validates version3 and rebuilds transient source identity sets before returning. Old v3 snapshots need no reset because event_session_ids is derived from retained metadata on load. Actual event ownership remains distinct from parent-only linkage. Invalid JSON, unsupported versions, nonfiles and oversized inputs return None so the existing background source discovery path recovers. Cache paths and source data remain private; no source text is added or logged.
