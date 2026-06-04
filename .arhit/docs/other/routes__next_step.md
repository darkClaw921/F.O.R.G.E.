# routes::next_step

REST API фичи «Следующий шаг» (router в plugins/echo/src/routes/next_step.rs, монтируется в build_router). Ошибки сериализуются как {"error":"..."} (конвенция ApiError).

Маршруты:
- GET /api/echo/next-steps -> {items:[{session, content, created_at}]} — текущие эфемерные предложения из EchoState.next_steps. Фронт использует для догрузки при инициализации (api.js getNextSteps).
- POST /api/echo/next-steps/:session/send body {text?} — доставляет текст в tmux-сессию через HostApi::send_keys, затем снимает предложение из next_steps + broadcast NextStepEvent{has_suggestion:false}. text опционален: при отсутствии/пустоте берётся content сохранённого предложения. 404 если предложения нет и text не задан. Ответ {ok:true, sent}.
- POST /api/echo/next-steps/:session/feedback body {correction} — пишет правило в next_step_rules через rules_repo::insert_rule (context_summary = 'Контекст терминала:\n<pane_excerpt>\n\nОтвергнутое предложение: <content>', suggested_next = correction, project_id берётся из снятого предложения), снимает предложение + broadcast. Пустой correction -> 400. Ответ {ok:true, rule_id}.
- POST /api/echo/next-steps/:session/dismiss — просто снимает предложение из next_steps + broadcast (если было). Ответ {ok:true, dismissed}.

Общий хвост send/feedback/dismiss — broadcast_cleared (NextStepEvent has_suggestion:false). host-adapter достаётся из state.host (500 если отсутствует).

Зависимости: EchoState (next_steps, db, host, broadcast), HostApi::send_keys, rules_repo (insert_rule), ServerMsg::NextStepEvent. Бизнес-логика: все мутирующие маршруты снимают эфемерное предложение и уведомляют фронт через WS, чтобы погасить голубое свечение.
