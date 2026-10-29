# main::capacity_recovery_startup

After starting agent ingestion and runtime workers, startup spawns capacity_recovery::run with the shared AgentRuntime clone. The module is enabled by default for the user-requested exact Codex capacity error and is independent of permission auto-approval. Runtime inventory remains its eligibility source; disabling agent collection leaves no candidates and therefore suspends recovery. Existing attention, auto-approval and plugin workers are unchanged.
