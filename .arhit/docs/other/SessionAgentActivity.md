# SessionAgentActivity

Coherent tab activity fields: is_generating, started_at_ms of earliest working root, working_agent_count deduplicated by root task ID and providers stable enum slugs. age_secs(now_ms) derives nonnegative task age; false activity has no age. /api/sessions reads one map snapshot for bool/age/count/provider fields so transitions cannot split them across separate locks. Linked sessions expose activity only on the existing primary; the global AgentSnapshot count remains one per root task.
