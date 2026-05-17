# plugins/echo/src/actions/executor.rs

Executor для Action. invoke(action, host, autonomous_context) — основной API. Hard-reject: System action + autonomous_context=true → anyhow::Err (защита от автономного выполнения system-actions без подтверждения пользователя). InvokeResult: Prompt{text} (фронт отправит как user_message)|Ok|Error{msg}. execute_system dispatch: OpenSession валидирует через host.list_sessions; OpenProject через list_projects; RestartSession/CreateTask — stub-Ok с tracing-логом, ожидают расширения HostApi в Phase 6.
