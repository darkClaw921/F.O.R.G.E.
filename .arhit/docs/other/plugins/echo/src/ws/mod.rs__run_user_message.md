# plugins/echo/src/ws/mod.rs::run_user_message

Полный цикл UserMessage: проверка существования conversation (no_conversation error если нет) → insert user-msg → prompt_builder::build → ClaudeRunner::stream → broadcast AssistantChunk для каждого ClaudeEvent → insert assistant-msg с usage из Result-event → stats::add_tokens для текущей минуты → broadcast StatsUpdate + AssistantDone. Все события идут через state.broadcast как ServerEvent::to_conversation. Запускается через tokio::spawn чтобы не блокировать WS select-loop.
