# main::SessionDto

DTO ответа GET /api/sessions (src/main.rs, добавлен в Phase 7 — Phase 2 wiring).

#[derive(Debug, Serialize)]
struct SessionDto {
    #[serde(flatten)]
    info: SessionInfo,
    needs_attention: bool,
}

Назначение: расширить ответ /api/sessions полем needs_attention без модификации tmux::SessionInfo (которая используется в других местах) и без поломки существующего фронтенд-контракта. #[serde(flatten)] раскладывает все поля SessionInfo на верхний уровень JSON, поэтому фронтенд видит прежний JSON + добавленное поле.

Семантика поля needs_attention:
- true → в панели сессии обнаружен Claude permission prompt (см. attention::detect_claude_prompt). Фронтенд должен подсветить вкладку оранжевым (Phase 3).
- false → нормальное состояние; либо запись отсутствует в snapshot'е (что также трактуется как false через .copied().unwrap_or(false)).

Заполняется в get_sessions: snapshot из state.attention.snapshot().await, лукап по s.name. AttentionState обновляется фоновым attention::watcher_loop каждые 1.5с.
