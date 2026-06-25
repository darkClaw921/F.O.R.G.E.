Фича «Очищать контекст перед выполнением задачи» для TODO-карточек (kanban). Добавлен per-TODO булев флаг clear_context, который при promote заставляет notifier перед доставкой текста задачи в tmux-сессию отправить команду /clear, подождать 2 секунды и только затем отправить сам текст. Цель: дать Claude Code сбросить контекст диалога перед взятием новой задачи.

## Backend
- tmux-web/src/todos.rs::Todo — новое поле clear_context: bool (#[serde(default)], backward-compat). create() и create_by_cwd() получили доп. параметр clear_context: bool (после plan_mode). update() получил параметр clear_context: Option<bool> (None=не трогать, Some(b)=записать), между plan_mode и auto_promote.
- tmux-web/src/notifier.rs::NotifyJob — новое поле clear_context: bool (#[serde(default)]). new_job() получил параметр clear_context: bool. fire_job(): если job.clear_context==true, перед retry-циклом доставки текста отправляет send_keys(session, '/clear'), ошибку логирует warn (не прерывает), затем tokio::time::sleep(2s).
- tmux-web/src/main.rs — CreateTodoReq.clear_context (#[serde(default)] bool) прокидывается в todos.create; PatchTodoReq.clear_context (#[serde(default)] Option<bool>) прокидывается в todos.update; promote_todo_core передаёт todo.clear_context в notifier::new_job.

## Frontend (tmux-web/static/js/tasks/modals.js)
- openCreateModal: для isTodo добавлен чекбокс #tm-clear-context; значение пишется в todoPayload.clear_context (POST /api/todos).
- openTodoEditModal: добавлен чекбокс #td-clear-context (инициализируется из todo.clear_context); diff пишется в patch.clear_context (PATCH /api/todos/:id).

## Поток
UI чекбокс → todos.json (clear_context) → promote_todo_core → NotifyJob.clear_context → notifier fire_job → tmux send_keys '/clear' + sleep 2s + send_keys text.

## Инварианты
- Дефолт false = поведение до фичи (нулевая конфигурация). serde(default) на всех новых полях обеспечивает загрузку старых todos.json / notify_state.json без падений.
- /clear best-effort: провал доставки /clear не блокирует доставку основного текста.