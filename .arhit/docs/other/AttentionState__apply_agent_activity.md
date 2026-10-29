# AttentionState::apply_agent_activity

Publish one journal-derived activity map and independent busy set. Replaces compatibility generating/age maps without calling the legacy boolean-front idle helper. Clears pending Echo idle on any bound working/waiting/unknown session; only observer-approved explicit endings create idle on detached nonbusy sessions. Attached-session suppression and needs_attention filtering remain active. Busy flags are published before idle updates, and idle_snapshot filters them, avoiding a transient old-idle read when a new task starts.
