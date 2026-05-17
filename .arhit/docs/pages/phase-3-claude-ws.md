# Phase 3: Claude CLI integration + chat WS streaming (Echo)

## Завершено

Добавлены следующие компоненты:

### Claude CLI integration
- plugins/echo/src/claude/events.rs — NDJSON-парсер с tolerantным Usage (12 unit-тестов)
- plugins/echo/src/claude/mod.rs — ClaudeRunner с stream/one_shot/cancel, Semaphore(N), kill_on_drop, AbortHandle map (5 unit-тестов с mock-CLI)
- plugins/echo/src/claude/prompt_builder.rs — build() собирает [system_context]+[tmux_sessions]+[memories]+[projects]+[user_message] с skip-on-error semantics (10 unit-тестов с MockHost)

### WebSocket
- plugins/echo/src/ws/mod.rs — echo_ws handler с biased select-loop (heartbeat 15s, idle timeout 60s, broadcast subscribe с conversation_id filter)
- plugins/echo/src/ws/protocol.rs — ClientMsg/ServerMsg tagged enums (13 round-trip тестов)
- ws::run_user_message — полный pipeline: insert user → prompt → ClaudeRunner.stream → broadcast AssistantChunk → insert assistant + stats::add_tokens → StatsUpdate + AssistantDone

### REST routes
- plugins/echo/src/routes/stats.rs — GET /api/echo/stats?range=hour|day (60 минут/24 часа bucket'ов, fill empties), POST /api/echo/run/:id/cancel (5 unit-тестов)

### Хост-интеграция
- tmux-web/src/tmux.rs — pub async fn capture_pane_full(session, lines): clamp 10000, reject negative, missing-session → Ok('') (3 unit-теста)
- tmux-web/src/echo_host.rs — реальные impl HostApi::list_sessions (через tmux::list_sessions+map) и capture_pane_full (delegate)

### State + Init
- plugins/echo/src/state.rs — EchoState получил поле runner: Arc<ClaudeRunner>; ServerEvent теперь содержит {conversation_id, msg: ServerMsg}
- plugins/echo/src/lib.rs — init() создаёт ClaudeRunner с cli_path (default ~/.local/bin/claude) и max_parallel_runs (default 4); register_routes регистрирует /ws/echo + stats router

## Verify результаты
- cargo build -p devforge — clean (только pre-existing warning про server_config::save)
- cargo test -p forge-echo — 69 lib + 2 db_init + 1 e2e = 72 проходят
- Integration test plugins/echo/tests/run_user_message_e2e.rs — full pipeline с mock-CLI: user msg в DB → CLI spawn → assistant msg с usage → token_stats bucket

## Заблокированные манульные шаги
Шаги 3-7 из P3.10 (wscat + sqlite3 проверки) требуют интерактивного запуска devforge и реального Claude CLI. Автоматизированы через integration test e2e + unit tests; manual smoke оставлен для пользователя.