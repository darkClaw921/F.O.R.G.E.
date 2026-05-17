# plugins/echo/src/claude/prompt_builder.rs::CtxOpts

Опции построения контекста: include_pane_capture (bool), project_id (Option<String>) — фильтр для memories и (потенциально) сессий, include_memories (bool), capture_lines (i32, default 200), session_filter (Option<Vec<String>>) — whitelist имён сессий. Default: pane+memory включены, capture_lines=200.
