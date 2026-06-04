# Фича «Следующий шаг» (Next Step)

Сквозная фича: когда Claude в tmux-сессии заканчивает ход и затихает, Echo автоматически предлагает РОВНО ОДИН готовый к отправке следующий шаг. Пользователь видит голубое свечение сессии в сайдбаре, наводит курсор -> интерактивный попап -> отправляет шаг в терминал или корректирует его (коррекция запоминается как правило и улучшает будущие предложения).

## Конвейер end-to-end

idle-детект (Host) -> опрос воркером -> генерация через Claude -> попап (Frontend) -> действие пользователя -> память (feedback -> правило -> подмешивание в prompt).

### 1. Host: idle-детект (Phase 1, tmux-web/src/attention.rs)
AttentionState.idle_started_at поддерживается по фронтам флага is_generating в set_generating: фронт true->false (сессия реально генерировала и затихла) фиксирует Instant::now(); false->true сбрасывает. При первом наблюдении (флаг сразу false) отметка НЕ ставится.
AttentionState::idle_snapshot() -> HashMap<session, idle_secs>, ИСКЛЮЧАЯ сессии с needs_attention=true (открыт permission/plan/question prompt) — это подавление генерации, когда нужен ответ пользователя, а не автоген.
HostApi расширен (echo-host-api): DTO IdleSession{name, idle_secs} + методы idle_sessions() и send_keys(session, text). Реализация — EchoHostAdapter (tmux-web/src/echo_host.rs): idle_sessions делегирует в idle_snapshot, send_keys — в tmux send-keys.

### 2. Backend: воркер + хранение + маршруты + WS (Phase 2, plugins/echo)
- state.rs: NextStepSuggestion{session, content, pane_excerpt, project_id, created_at_unix} + эфемерная карта EchoState.next_steps (RwLock<HashMap>).
- БД: таблица next_step_rules (миграция V005) + репозиторий db/repo/next_step.rs: insert_rule (UUIDv4, project_id=None -> глобальное) и list_rules (глобальные + по project_id, DESC, limit DEFAULT_RULES_LIMIT=20).
- Воркер next_step/mod.rs (spawn -> run_loop -> tick_once каждые 2с): опрашивает idle_sessions; для сессий idle >= IDLE_THRESHOLD_SECS=10с, не обработанных в текущем эпизоде и без активного предложения — generate_for_session. ProcessedSet + проверка next_steps защищают от двойного запуска (один эпизод затихания = не более одного предложения). reset_stale_episodes сбрасывает эпизод, когда сессия пропала из idle-списка (снова активна/prompt/закрыта): чистит processed + next_steps + broadcast has_suggestion=false.
- Генерация: capture_pane_full(100 строк) -> prompt = NEXT_STEP_META_PROMPT (строгий: один короткий шаг, готовый к терминалу, без markdown/преамбул, по-русски) + [learned_rules] (list_rules) + [terminal_tail] (snippet pane) -> runner.one_shot. Пустой ответ не сохраняется. Непустой -> next_steps + broadcast.
- WS: ServerMsg::NextStepEvent{session, has_suggestion} (ws/protocol.rs).
- REST routes/next_step.rs: GET /api/echo/next-steps (список), POST .../:session/send (send_keys + снять + broadcast), POST .../feedback (insert_rule + снять + broadcast), POST .../dismiss (снять + broadcast).
- Регистрация воркера в spawn_workers (lib.rs).

### 3. Frontend: свечение + интерактивный попап (Phase 3, tmux-web/static)
- core/state.js: state.nextSteps; догрузка через GET /api/echo/next-steps при инициализации.
- sessions/sessions.js: класс .has-next-step на .session-item при наличии записи в state.nextSteps.
- css: голубое свечение (sidebar.css) + стили попапа (next-step-popup.css).
- sessions/next-step-popup.js: интерактивный hover-попап (singleton на body), УДЕРЖИВАЕТСЯ пока курсор над ним (в отличие от пассивного ui/tooltip.js). textarea с текстом предложения + «Отправить в терминал» (sendNextStep); поле коррекции «Что нужно было сделать» + «Сохранить» (feedbackNextStep). После действия — оптимистичное снятие свечения, затем WS-подтверждение.
- echo/ws.js: обработка next_step_event (перефетч + перерендер сайдбара).

### 4. Память обратной связи (петля обучения)
Пользователь жмёт «Сохранить» с коррекцией -> POST feedback -> insert_rule пишет правило (context_summary = pane-выдержка + отвергнутое предложение, suggested_next = коррекция) в next_step_rules. При СЛЕДУЮЩЕМ затухании generate_for_session подмешивает правила через list_rules в блок [learned_rules] prompt'а — предложения становятся точнее. project_id сейчас всегда None (концепция проектов удалена) -> правила глобальные; схема готова к проектным правилам (git-корень) при появлении резолва.

## Инварианты
- Один эпизод затихания -> максимум одно предложение (ProcessedSet + наличие в next_steps).
- needs_attention (открытый prompt) подавляет генерацию (idle_snapshot исключает такие сессии).
- Пустой ответ модели не зажигает свечение, но эпизод считается обработанным.
- Возврат сессии в активность / закрытие / появление prompt -> сброс эпизода + гашение свечения (broadcast has_suggestion=false).

## Тесты
forge-echo: unit воркера (idle_session_generates_suggestion, below_threshold, does_not_regenerate_within_same_episode, episode_reset_clears_suggestion, empty_model_output_stores_nothing), unit репозитория (insert/list global+project+limit), интеграционные тесты маршрутов (send/feedback/dismiss/list, 404/400). devforge: idle_snapshot_* в attention.rs (tracks_generating_fronts, not_set_on_first_false, excludes_needs_attention, returns_elapsed_secs).