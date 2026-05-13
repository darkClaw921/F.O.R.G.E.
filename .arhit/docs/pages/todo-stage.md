Фича TODO-stage (kanban storage column + промоут в bd-задачу с tmux-нотификацией). Реализована в Phases 1-5 (epics forge-phase1-backend-foundation-g06 .. forge-phase5-frontend-settings-1w9, проверена в forge-phase6-verification-hp9).

## Цель
Дать пользователю отдельную storage-колонку TODO слева от bd-канбана: туда пишутся идеи, не превращаясь в bd-задачи. Когда пользователь готов взять идею в работу, он перетаскивает её в open — и tmux-web 1) создаёт bd-задачу, 2) отправляет в активную tmux-сессию текст-промпт по шаблону (через send_keys), 3) при необходимости откладывает отправку (delay) или ставит её в очередь до закрытия предыдущей задачи (wait_previous).

## Backend компоненты
- tmux-web/src/todos.rs — TodoStore (Arc<RwLock<Inner>>) + Todo (id UUID v4, project_id, title, description, priority, issue_type, labels, created_at/updated_at). Persist в <project>/.forge/todos.json, atomic save через tempfile+rename.
- tmux-web/src/notifier.rs — фоновая корутина notifier_loop. NotifyJob (project_id, todo_title/desc, bd_task_id, deliver_at, wait_previous, target_session). NotifyMode {Immediate, Delayed, WaitPrevious}. Очередь per-(project, session). Persist в <project>/.forge/notify_state.json. NotifyHandle инкапсулирует mpsc::Sender<NotifierCmd>.
- tmux-web/src/tasks_watcher.rs — diff issues.jsonl, broadcast TaskEvent::Closed, который notifier_loop потребляет для wait_previous flow.
- tmux-web/src/tmux.rs::send_keys(session, text) — обёртка над tmux send-keys -t <session> <text> Enter.
- tmux-web/src/projects.rs::Project — расширен полями notify_template / notify_delay_minutes / notify_wait_previous / notify_session (#[serde(default)]).
- tmux-web/src/ws_todos.rs — TodoEvent (Upsert/Removed/Reload) + WS-handler /ws/todos с snapshot+heartbeat+lag-recovery.
- tmux-web/src/main.rs роуты: GET/POST /api/todos, PATCH/DELETE /api/todos/:id, POST /api/todos/:id/promote, PATCH /api/projects/:id/settings, GET /ws/todos. format_notify_template + resolve_notify_session — helpers для подстановки и резолва target session.

## Frontend компоненты (tmux-web/static/app.js)
- TASK_COLUMNS дополнено 'todo' слева.
- state.todosData + fetchTodos() (GET /api/todos?project_id) + connectTodosWs() (WS /ws/todos с reconnect backoff и fallback polling).
- renderTasks: колонка todo рендерится из state.todosData, остальные — из state.tasksData. renderTodoCard для карточек.
- Drag-drop: TODO→open вызывает promoteTodo(id, session). Прочие переходы (TODO↔другие) запрещены.
- openCreateModal({status:'todo'}) — POST /api/todos (без поля status).
- openTodoEditModal — модалка без status, с кнопкой Promote.
- Settings modal расширен раскрывающейся секцией Notifications (buildNotificationsForm + saveProjectSettings → PATCH /api/projects/:id/settings).

## Frontend компоненты (tmux-web/static/style.css)
- Колонка TODO: фиолетовый border-left.
- .notify-fieldset / .notify-field / .notify-actions / .btn-settings — стили формы Notifications в Settings.

## Поток promote
1. UI drop TODO→open → POST /api/todos/:id/promote {session}.
2. Backend handler промоут_todo: создаёт bd-задачу через br create (внутри active project path), удаляет TODO из TodoStore, broadcast TodoEvent::Removed + TaskEvent::Created.
3. Если notify_template задан — handler ставит NotifyJob в notifier очередь (mode = Immediate/Delayed/WaitPrevious в зависимости от настроек проекта).
4. notifier_loop в tick:
   - Immediate → tmux::send_keys сразу.
   - Delayed → ждёт deliver_at, потом send_keys.
   - WaitPrevious → блокирует очередь session до прихода TaskEvent::Closed для предыдущего bd_task_id.
5. notify_state.json флашится после каждой mutation. На рестарте сервера notifier_loop восстанавливает очередь и ждёт оставшийся delay или TaskEvent::Closed.

## Тесты / верификация (Phase 6)
- cargo check / cargo build --release: clean (1 pre-existing warning в pty.rs).
- Backend sanity через curl: POST /api/todos, PATCH /api/projects/:id/settings, DELETE /api/todos/:id — round-trip OK.
- Manual smoke-test в Chrome: см. tmux-web/SMOKE_TEST.md (8 сценариев).
- Аliases: 'todo column', 'notifier loop', 'tmux send keys', 'promote todo', 'promote queue', 'todo websocket'.