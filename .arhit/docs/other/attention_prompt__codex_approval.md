# attention_prompt::codex_approval

Recognizes Codex command execution, edits, additional permissions, network approval, terminal input approval, MCP server elicitation and unanswered-question confirmation. Requires a known heading, at least one selected numbered item, affirmative and refusal/back choices, and a final confirm/cancel/back footer. A normal input row inside the menu or content after the final footer prevents detection. These detectors are used only for orange signaling, never autoapproval.
