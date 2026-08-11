# plugins/claude-runner/src/events.rs::parse_line

Парсер одной NDJSON-строки стрима claude -p --output-format stream-json. Переехал из plugins/echo/src/claude/events.rs в общий крейт forge-claude-runner (Фаза 0), содержимое не менялось; Echo видит модуль по старому пути crate::claude::events через re-export.

Разворачивает обёртку {"type":"stream_event","event":{...}}, которую добавляет --include-partial-messages. Маппинг: content_block_delta/text_delta -> ClaudeEvent::TextDelta; thinking_delta -> Thinking; content_block_start с полностью сформированным tool_use -> ToolUse; result -> Result (is_error = явный is_error:true ЛИБО subtype с префиксом error, например error_max_turns); error -> Error. Всё остальное (system, message_start, content_block_stop, message_delta, message_stop, любой неизвестный type, невалидный JSON, пустая строка) -> None: forward-compat, не падаем на новых типах.

Толерантность к схеме: cache_creation_input_tokens и cache_read_input_tokens в Usage помечены serde(default) — Anthropic меняет схему ответа без bump'а версии. 15 тестов в том же файле — единственная страховка на формат stream-json.
