# promote_todo_core

Ядро промоута TODO -> bd-задача (tmux-web/src/main.rs), выделенное из HTTP-handler'а promote_todo в Фазе 3 рефакторинга авто-промоута (эпик forge-83eb). Сигнатура: pub(crate) async fn promote_todo_core(state: &AppState, todo: &todos::Todo, target_session: Option<String>, mode_override: Option<notifier::NotifyMode>) -> Result<PromoteOutcome, anyhow::Error>.

Назначение: единая точка логики 'TODO -> bd-issue + notify-job', вызываемая из двух мест — (1) HTTP-handler promote_todo (ручной промоут через POST /api/todos/:id/promote) и (2) фоновый воркер auto_promote::run (Фаза 4b, авто-промоут следующей флагнутой TODO после закрытия предыдущей задачи).

Параметры:
- state: общий AppState (доступ к todos, notifier_config, user_settings, notify, todos_tx, auto_chain).
- todo: карточка для промоута; caller уже загрузил её (get не делается внутри).
- target_session: явная целевая tmux-сессия. None -> фолбэк на NotifierConfig.session. Пусто и там -> notify скипается.
- mode_override: явный NotifyMode. None -> режим вычисляется из cfg (wait_previous > delayed > immediate).

Алгоритм:
1. br create --json (через tasks::run_br в todo.root_path) с --title/-d/-t/-p из полей todo; извлечь .id (фолбэк на .created[0].id). Ошибка br -> Err(anyhow).
2. state.todos.delete(todo.id) + broadcast ws_todos::removed(root_path, id).
3. Резолв итоговой сессии: target_session (trim/empty) иначе cfg.session (trim/empty).
4. Если сессия есть: сборка text (cfg.template иначе DEFAULT_PROMOTE_TEMPLATE '[{id}] {title}'; plan_mode suffix из user_settings иначе PLAN_MODE_SUFFIX), резолв mode (mode_override иначе из cfg), notifier::new_job + notify.enqueue. notify_scheduled=true (false если enqueue Err).
5. Если сессии нет: notify скипается, notify_scheduled=false, лог info session_missing.
6. ВСЕГДА (даже при skip notify, после успешного br create): запись state.auto_chain[todo.root_path] = AutoChainEntry { active_task_id: task_id, session: итоговая_сессия }. Poisoned lock обрабатывается мягко (if let Ok, иначе warn без паники). Это голова цепочки авто-промоута, читаемая auto_promote::run.
7. Вернуть PromoteOutcome { task_id, notify_scheduled }.

Связанные элементы: PromoteOutcome (возвращаемый тип), promote_todo (HTTP-обёртка поверх core), auto_promote::AutoChainEntry/AutoChainMap (запись цепочки), format_notify_template, DEFAULT_PROMOTE_TEMPLATE, PLAN_MODE_SUFFIX, notifier::new_job/NotifyMode, tasks::run_br, todos::TodoStore::get/delete, ws_todos::removed.

Ограничения/инварианты: bd-задача создаётся ВСЕГДА (если br не упал), даже если notify скипнут — поведение совпадает с прежним inline-кодом promote_todo. HTTP-ответ ручного promote не изменился: { promoted: true, task_id, notify_scheduled }.
