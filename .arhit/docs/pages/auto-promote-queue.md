# Авто-промоут TODO по очереди (auto-promote queue)

Сводная страница фичи «авто-промоут флагнутых TODO-карточек в очереди» (эпик forge-83eb, план serene-wondering-barto.md). Описывает фичу целиком: назначение, end-to-end поток, очередь/порядок, барьер, старт/возобновление цепочки, состояние и ограничения. Per-элементные доки: Todo, TodoStore::update, patch_todo, promote_todo_core, PromoteOutcome, auto_promote::run, auto_promote::pick_top, setTodoAutoPromote, renderTodoCard, openTodoEditModal.

## Назначение

Пользователь помечает TODO-карточки флагом «авто» (чекбокс в модалке редактирования или тогл прямо на карточке), и помеченные карточки автоматически промоутятся в bd-задачи (статус open) ПО ОЧЕРЕДИ — каждая следующая запускается по мере закрытия предыдущей. Это снимает рутину ручного drag/promote каждой карточки: достаточно один раз промоутнуть верхнюю задачу вручную (этот ручной promote задаёт «голову» цепочки), а дальше цепочка сама продвигается при закрытии задач.

## Поток end-to-end

1. Пометка карточки. Тогл `.auto-promote-toggle` на карточке (static/js/tasks/render.js, renderTodoCard) ИЛИ чекбокс `#td-auto-promote` в модалке редактирования (static/js/tasks/modals.js, openTodoEditModal). Оба ведут к одной точке записи.
2. PATCH флага. Frontend вызывает setTodoAutoPromote (static/js/tasks/crud.js) → `PATCH /api/todos/:id` с телом `{auto_promote: <bool>}` → main.rs patch_todo (через PatchTodoReq.auto_promote) → TodoStore::update проставляет поле Todo.auto_promote и пишет todos.json → broadcast WS upsert карточки (клиенты обновляют UI).
3. Старт цепочки (ручной promote). Пользователь вручную промоутит любую карточку → main.rs promote_todo_core создаёт bd-задачу (br create) и пишет голову цепочки: `auto_chain[root] = AutoChainEntry { active_task_id, session }`. Только ручной promote задаёт active_task_id и тем самым запускает/перезапускает цепочку для данного корня.
4. Закрытие задачи. Когда задача-голова закрывается, tasks_watcher (читает .beads/) эмитит `TaskEvent::Upsert { issue, status: "closed" }` в broadcast tasks_tx.
5. Продвижение цепочки. Воркер auto_promote::run слушает тот же broadcast. На каждое closed-событие вызывает handle_closed, который ищет root, где `entry.active_task_id == closed_id`. Если нашёл — берёт ВЕРХНЮЮ карточку TODO этого корня через pick_top (priority asc, при равенстве updated_at desc). Если у верхней карточки `auto_promote == true` → вызывает promote_todo_core(state, &top, entry.session, Some(NotifyMode::Immediate)); core создаёт новую задачу, перезаписывает `auto_chain[root]` новой головой — цепочка продолжается. Если флаг не стоит — барьер (см. ниже).

## Очередь и порядок

Порядок промоута повторяет порядок канбан-колонки один-в-один (auto_promote::pick_top дублирует фронтовую сортировку compareIssues из render.js):
1. priority ASC (u8, меньшее число = выше приоритет, идёт первым);
2. при равном priority — updated_at DESC (RFC3339-строки сравниваются лексикографически; новее = больше, идёт первым).

То есть «верхняя» карточка = первая в колонке UI. Цепочка всегда берёт именно текущую верхнюю карточку, а не «перепрыгивает» к следующей флагнутой.

## Барьер

pick_top НЕ фильтрует по auto_promote — он возвращает текущую верхнюю карточку как есть. Барьер проверяется в handle_closed уже после выбора top: если у верхней карточки `auto_promote == false` (или карточек вообще нет) — цепочка тихо останавливается. Непомеченная карточка наверху колонки служит барьером: всё, что за ней (даже помеченное «авто»), не запускается, пока пользователь вручную не разберётся с барьером (закроет/удалит/промоутит эту карточку вручную, заново задав голову цепочки). Это сознательное решение: цепочка реагирует на текущую верхнюю карточку, а не проскакивает её.

## Старт / возобновление цепочки

- Запуск цепочки: только ручной promote задаёт active_task_id (голову). Автопродвижение active_task_id не «само-стартует» — нужен явный первый ручной promote.
- Продвижение: только при закрытии именно задачи-головы (`closed_id == entry.active_task_id`). Посторонние закрытия (задачи не из цепочки) и re-touch старых уже закрытых задач игнорируются — handle_closed просто не находит совпадающего root и выходит.
- Анти-гонка: поиск головы и её удаление делаются под ОДНИМ write-lock'ом state.auto_chain — запись извлекается (find + remove) ДО любого await на `br create`. Дубль closed-события или повторный re-touch не запустят вторую промоут-операцию для той же головы. Poisoned lock обрабатывается мягко (warn + выход, без паники).
- Mode = Immediate: при продвижении цепочки promote_todo_core вызывается с `NotifyMode::Immediate`, а НЕ с cfg.wait_previous. Immediate обязателен: использовать wait_previous нельзя — воркер уже ждёт closed-событие, и двойная сериализация ожидания закрытия привела бы к дедлоку цепочки.
- session протягивается: entry.session наследуется от ручного promote, запустившего цепочку, и передаётся каждой следующей промоут-операции (для адресации уведомления в нужную tmux-сессию). None → фолбэк на cfg.session из NotifierConfig.

## Состояние

`auto_chain: AutoChainMap = Arc<RwLock<HashMap<String, AutoChainEntry>>>` (тип в auto_promote.rs, поле в AppState, main.rs). Ключ — root_path (paths::resolve_root от cwd сессии; TODO привязаны к корню, не к проекту — концепция Project удалена). Значение — AutoChainEntry { active_task_id: String, session: Option<String> } — одна «голова» цепочки на корень.

Состояние держится ТОЛЬКО в памяти — persist на диск намеренно НЕ делается (MVP-решение). При рестарте процесса цепочка обнуляется и перезапускается следующим ручным promote (self-heal). Терять при рестарте нечего критичного: незавершённые TODO остаются в todos.json, пользователь просто промоутит верхнюю вручную. RwLock допускает параллельные чтения и сериализованные записи; мутации редки (раз на промоут), узких мест нет.

## Ограничения

- Воркер реагирует на TaskEvent из broadcast tasks_tx, который питается из tasks_watcher (file-watcher по .beads/). Фича работает для проекта/корня, чьи задачи видит watcher.
- Subscribe воркера на broadcast ОБЯЗАН быть сделан в main.rs ДО передачи tasks_tx в tasks_watcher (там sender move'ится), иначе ранние события потеряются.
- broadcast Lagged → пропущенные closed-события «само-лечатся» при следующем ручном промоуте.
- Ошибка `br create` при продвижении логируется (error) и обрывает цепочку — перезапуск ручным промоутом.

## Ключевые файлы

- tmux-web/src/auto_promote.rs — типы (AutoChainEntry, AutoChainMap), воркер run, handle_closed, чистая функция pick_top (+ юнит-тесты порядка/барьера).
- tmux-web/src/main.rs — promote_todo_core (старт/перезапись головы, NotifyMode override), поле AppState.auto_chain, patch_todo (PATCH флага), spawn auto_promote::run рядом с tasks_watcher.
- tmux-web/src/todos.rs — поле Todo.auto_promote, параметр в TodoStore::update.
- static/js/tasks/render.js — renderTodoCard + тогл .auto-promote-toggle.
- static/js/tasks/crud.js — setTodoAutoPromote (PATCH флага).
- static/js/tasks/modals.js — openTodoEditModal + чекбокс #td-auto-promote.
- static/css/tasks.css — стили тогла/чекбокса авто-промоута.