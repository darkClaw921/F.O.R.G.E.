# next_step::spawn

Спавнит фоновый воркер фичи «Следующий шаг» и возвращает JoinHandle<()> для graceful shutdown (хост abort'ит его при завершении). Расположение: plugins/echo/src/next_step/mod.rs:84.

Сигнатура: spawn(state: Arc<EchoState>, host: Arc<dyn HostApi>) -> JoinHandle<(). Создаёт in-memory ProcessedSet (Arc<Mutex<HashSet<String>>>) и запускает run_loop, который каждые TICK_INTERVAL (2с) вызывает tick_once.

tick_once — одна итерация конвейера:
1. host.idle_sessions() — получить затихшие сессии (под капотом AttentionState::idle_snapshot, уже без needs_attention).
2. reset_stale_episodes — для сессий, ПРОПАВШИХ из idle-списка (снова активны / показан prompt / закрыты), чистит ProcessedSet + EchoState.next_steps и шлёт broadcast NextStepEvent{has_suggestion:false} (гасит свечение во фронте, разрешает новый эпизод).
3. Для каждой idle-сессии с idle_secs >= IDLE_THRESHOLD_SECS (10с), ещё не обработанной в этом эпизоде и без активного предложения — generate_for_session.

Защита от двойного запуска (один эпизод затихания -> не более одного предложения): ProcessedSet хранит уже обработанные сессии текущего эпизода; дополнительно проверяется наличие в next_steps (анти-гонка до попадания в processed). При ошибке генерации сессия удаляется из processed, чтобы повторить на следующем tick.

generate_for_session: capture_pane_full(session, 100 строк) -> строит prompt из NEXT_STEP_META_PROMPT (строгий русский: РОВНО ОДИН короткий шаг, готовый к отправке в терминал, без markdown/преамбул) + блок [learned_rules] из rules_repo::list_rules (правила памяти, project_id=None -> только глобальные, т.к. концепция проектов удалена) + [terminal_tail] (snippet pane, cap 6000 симв). Прогон через state.runner.one_shot. Пустой ответ модели НЕ сохраняется (свечение не зажигается), но эпизод считается обработанным. Непустой -> NextStepSuggestion{session, content, pane_excerpt(cap 4000), project_id, created_at_unix} в state.next_steps + broadcast NextStepEvent{has_suggestion:true}.

Зависимости: echo_host_api::HostApi (idle_sessions, capture_pane_full), EchoState (next_steps, runner, broadcast, db), rules_repo (list_rules), ServerMsg::NextStepEvent. Регистрируется в spawn_workers (lib.rs). Не паникует на ошибках host/БД — логирует и продолжает.

Константы: IDLE_THRESHOLD_SECS=10, TICK_INTERVAL=2с, CAPTURE_LINES=100, PANE_SNIPPET_CAP=6000, PANE_EXCERPT_CAP=4000.
