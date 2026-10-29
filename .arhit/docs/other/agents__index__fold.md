# agents::index::fold

Owned-input compatibility wrapper around fold_refs. Collects incoming events into stable backing storage, then invokes the shared borrowed replay engine. Existing callers/tests retain their signature and the same deterministic result. Production source ingestion uses fold_refs directly on cached contributions, so it does not allocate a second owned event history.
