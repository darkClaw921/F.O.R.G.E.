# plugins/echo/src/ws/mod.rs

WebSocket /ws/echo handler. Phase 5b изменения: после assistant_done парсит forge-actions из текста через actions::parser::extract, регистрирует в state.action_registry, бродкастит ServerMsg::ActionButtons. Новая функция handle_action_invoke обрабатывает ClientMsg::ActionInvoke: find_action в registry → actions::executor::invoke с autonomous_context=false → результат Prompt вызывает run_user_message с текстом, Ok/Error/Reject шлёт ServerMsg::Notification.
