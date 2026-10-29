# codex_memory::codex_home

Resolves nonempty CODEX_HOME first, otherwise HOME/.codex. Returns None when both are unavailable. Does not use project cwd or analytics settings: native Codex memory and sub-agent memory belong to the CLI home, independent of whether agent statistics is enabled.
