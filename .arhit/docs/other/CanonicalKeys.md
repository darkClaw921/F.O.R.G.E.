# CanonicalKeys

Per-fold memo of canonical session and provider/profile identity strings. session uses a typed AgentSessionId map and invokes the stable JSON tuple encoding only once per distinct identity; profile nests maps by provider and borrowed profile lookups to avoid repeatedly cloning profile paths before lookup. Returned owned key strings preserve the original BTreeMap key representation, ordering, escaping and deduplication rules. The memo lives only for one fold and does not persist identities beyond the existing index lifecycle.
