# auto_promote::run

Фоновый воркер цепочки авто-промоута (tmux-web/src/auto_promote.rs). Сигнатура: pub async fn run(state: AppState, mut tasks_rx: broadcast::Receiver<TaskEvent>). Spawn'ится в main.rs рядом с tasks_watcher (auto_promote_rx = tasks_tx.subscribe() делается ДО move'а tasks_tx в run_watcher, иначе ранние closed-события теряются — broadcast хранит только 64).

Назначение: реализует 'очередь' авто-промоута. Пользователь флагает TODO-карточки полем auto_promote; после закрытия текущей задачи цепочки воркер автоматически промоутит следующую верхнюю флагнутую TODO без ручного нажатия 'promote'.

Логика цикла (loop на tasks_rx.recv().await):
- Ok(TaskEvent::Upsert{issue}) — извлекает id/status (как notifier.rs стр.193-200). Если status != 'closed' -> continue. Иначе -> handle_closed(&state, &id).await.
- Ok(TaskEvent::Removed{..}) -> continue (физическое удаление цепочку не двигает).
- Err(Lagged(n)) -> tracing::warn + continue.
- Err(Closed) -> tracing::info + return (sender дропнут, процесс завершается).

handle_closed(state, closed_id) — приватная async:
1. Анти-гонка: под ОДНИМ write-lock'ом state.auto_chain найти root где entry.active_task_id==closed_id и СРАЗУ remove запись (до любого await на br create). Posioned lock -> warn без паники + return. Нет совпадения -> return (посторонняя задача / re-touch).
2. top = pick_top(state.todos.list(&root)) — верхняя карточка канбана БЕЗ фильтра.
3. Барьер: если список пуст ИЛИ top.auto_promote==false -> tracing::debug + return (цепочка тихо останавливается, запись уже удалена в п.1).
4. Иначе (top.auto_promote==true): promote_todo_core(state, &top, entry.session.clone(), Some(NotifyMode::Immediate)).await. Ok -> info(root, closed_id, new_task_id, session); Err -> error (цепочка обрывается, перезапуск ручным промоутом). promote_todo_core сам перезапишет auto_chain[root] на новую голову.

Инварианты: mode=Immediate ОБЯЗАТЕЛЕН (НЕ cfg.wait_previous — двойная сериализация ожидания закрытия дала бы дедлок: воркер уже ждёт closed-событие). session протягивается по цепочке от ручного промоута (None -> фолбэк cfg.session в core). Состояние in-memory, self-heal через ручной promote при рестарте.

Связанные: handle_closed, pick_top, promote_todo_core, AutoChainEntry/AutoChainMap, tasks::TaskEvent, notifier::NotifyMode, todos::TodoStore::list.
