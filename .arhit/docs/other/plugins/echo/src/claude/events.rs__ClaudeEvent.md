# plugins/echo/src/claude/events.rs::ClaudeEvent

Высокоуровневое событие assistant-стрима. Варианты: TextDelta{text} — приращение текстового блока; Thinking{text} — приращение thinking-блока (reasoning models); ToolUse{name, input} — вызов tool с инпутом; Result{usage, raw_json} — финал run'а с usage и сырым JSON для аудита; Error{message} — ошибка от CLI. Поглощается WS-loop'ом (ws/mod.rs) и autonomous-runner'ом.
