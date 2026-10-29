# capacity_recovery::Identity

Exact physical pane and root/CLI process identity used to scope recovery state. Stores only pane ID, root PID/start and native Codex PID/start; changed identities reset observation state. Both processes must remain alive with the same start time before delivery, preventing pane replacement or PID reuse from receiving continuation.
