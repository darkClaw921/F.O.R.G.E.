# forge-tracker — личный таск-трекер и тайм-трекер (итоговая архитектура)

Плагин F.O.R.G.E., дающий пользователю ЛИЧНЫЙ слой задач: свои списки, приоритеты, статусы, сроки, чек-листы, вехи и замеры времени, с режимами list / kanban / gantt / report и кнопкой копирования задачи во вкладку Tasks. Реализация закончена: фазы 1-10 закрыты, живая приёмка пройдена.

## Зачем отдельный слой

Рабочий контур задач уже был — канбан вкладки Tasks поверх br и колонка TODO (`tmux-web/src/todos.rs`, `~/.config/forge/todos.json`, привязана к корню проекта). Оба принадлежат ПРОЦЕССУ: beads-задача это обязательство перед агентом, а TODO-карточка при промоуте исчезает. Места, где человек ведёт свои дела и меряет по ним время, не было.

## Решения, зафиксированные с пользователем

1. Отдельная вкладка «Мои задачи» (кнопка `✇`, id `#tracker-toggle`), панель `#tracker`.
2. ЗАДАЧИ ГЛОБАЛЬНЫЕ для всего FORGE. В модели НЕТ поля `root_path` (сторож — тест `models.rs::no_model_carries_root_path` и проверка `state leaks project scope` в `tests/tracker_routes.rs`), стор один: `~/.config/forge/tracker.json`, права `0600`.
3. Таймеры можно запускать одновременно по РАЗНЫМ задачам; по одной задаче активный отрезок ровно один.
4. Время пункта чек-листа (`TimeEntry.item_id`) — часть времени задачи, а не отдельная сумма.
5. Кнопка `[→ tasks/todo]` КОПИРУЕТ задачу в todo и ничего не меняет в трекере; корень выбирается в диалоге в момент переноса.
6. Хранилище — JSON-файл, как `todos.json`, запись атомарная (`forge-atomic`).
7. Плагин opt-in; палитра и терминальная вёрстка панели собственные, тему приложения панель НЕ наследует (`data-tracker-theme`, зелёный фосфор `--t-bg: #050805` / `--t-acc: #33FF88`; в PLAN.md записан устаревший фиолетовый — источник истины макет `~/Documents/ForgeTracker.pen` и `css/tracker.css`).

## Крейт plugins/tracker (forge-tracker)

Член workspace и ЛИСТОВОЙ: в зависимостях нет ни `devforge`, ни другого плагина, про `AppState` и `TodoStore` крейт не знает. Перенос задачи в Tasks/todo делает ФРОНТ существующим `POST /api/todos` — ребро направлено только в сторону трекера.

- `src/models.rs` — `TaskList`, `TrackerTask` (`TaskStatus` = колонки канбана: inbox · today · doing · waiting · done), `ChecklistItem`, `Milestone`, `TimeEntry`, конверт `TrackerFile`; патчи различают «ключа нет» и «ключ = null» через `Option<Option<T>>` + `deserialize_with` — это единственный способ выразить «сбросить поле» в PATCH.
- `src/store.rs` — `TrackerStore`: `Arc<RwLock<Inner>>`, загрузка с `warn` на битом файле (плагин не должен ронять хост), атомарная запись, CRUD задач/списков/пунктов/вех/отрезков, каскады, таймеры.
- `src/report.rs` — чистые расчёты: суммы задачи и пункта, отчёт по осям day/task/list, `ReportKpi`/`build_kpi`, данные Гантта с флагом превышения оценки, суммы за сегодня и неделю. Дни ЛОКАЛЬНЫЕ, отрезок через полночь режется по границе.
- `src/config.rs` — `TrackerConfig` (путь к стору; `from_home()` у хоста, временный каталог в тестах).
- `src/error.rs`, `src/routes/error.rs` — `TrackerError { Io, Json, NotFound, Conflict }` и единый конверт ошибок HTTP.
- `src/state.rs`, `src/dto.rs` — общее состояние плагина и DTO снимка (`StateDto`).
- `src/ws.rs` — `/ws/tracker`.
- `src/routes/{mod,tasks,entries,items,lists,milestones,report}.rs` — таблица маршрутов.

Контракт плагина ровно четыре функции: `init` → `register_routes` → `spawn_workers` → `shutdown` (образец — `plugins/vk/src/lib.rs`).

## REST и WS

24 маршрута под `/api/tracker/*` плюс `/ws/tracker`:

- `GET /status`, `GET /state` — гейт и полный снимок с посчитанными СЕРВЕРОМ агрегатами (`task_seconds`, `item_seconds`, `totals`, `progress`, `active`, `now`).
- задачи: `GET|POST /tasks`, `GET|PATCH|DELETE /tasks/:id`, `PATCH /tasks/:id/status`, `POST /tasks/reorder`, `POST /tasks/:id/timer/start|stop`.
- чек-лист: `GET|POST /tasks/:id/items`, `PATCH|DELETE /tasks/:id/items/:item`, `POST …/toggle`, `POST …/reorder`, `POST …/timer/start|stop`.
- отрезки: `GET|POST /entries`, `PATCH|DELETE /entries/:id`.
- списки: `GET|POST /lists`, `PATCH|DELETE /lists/:id`, `POST /lists/reorder`.
- вехи: `GET|POST /milestones`, `PATCH|DELETE /milestones/:id`.
- отчёты: `GET /report`, `GET /gantt`.

Кадры сокета: `snapshot` (первым при подключении), `task_upsert` / `task_removed`, `entry_upsert` / `entry_removed`, `list_*`, `item_*`, `milestone_*`, `reload`. Медленный подписчик не отбрасывается, а получает `reload` и перечитывает снимок.

## Подключение к хосту (opt-in)

Флаг `--tracker`, переменная `DEVFORGE_TRACKER=1` или `server_config.json {"tracker": true}` (`tmux-web/src/{cli,server_config,daemon}.rs`, блок в `main.rs` после meet). Без флага роуты НЕ регистрируются вовсе, поэтому `/api/tracker/*` отдаёт 404 — на этот 404 опирается гейт вкладки и не пишет в консоль ни строчки. Ошибка инициализации плагина не мешает старту сервера: хост стартует без панели с `warn`. Дымовой тест — `tmux-web/tests/tracker_smoke.rs`.

## Фронт

Ленивая загрузка: `js/core/bootstrap.js` статически импортирует ТОЛЬКО `js/tracker/gate.js` (один `GET /api/tracker/status` за загрузку), а панель целиком `js/tabs/tabs.js` подтягивает динамическим `import('../tracker/main.js')` при первом открытии вкладки. Сокет живёт, только пока вкладка открыта.

- `gate.js` — гейт кнопки вкладки.
- `api.js` — тонкая обёртка над REST (`TrackerApiError`).
- `ws.js` — соединение и разбор кадров (`frameToHandler` чистая и покрыта тестом).
- `parse.js` — грамматика prompt-строки: `!N` приоритет, `#тег`, `~2ч` оценка, `@список` (создаётся, если нет), `due:…`; неизвестный токен остаётся в заголовке.
- `render-list.js` — режим list, ручной порядок `order`, `planOrder` для DnD.
- `render-kanban.js` — режим kanban, колонки = `TaskStatus`, оптимистичный перенос с откатом.
- `render-gantt.js` — режим gantt: сетка СИМВОЛЬНАЯ (4 знака на день), дорожка вех `◆`/`▼`, флаг превышения оценки; единственный режим со своим запросом `GET /gantt`.
- `render-report.js` — режим report: KPI с сервера, столбики по дням, доли, `EXPORT CSV` (UTF-8 с BOM, разделитель `;`).
- `modals.js` — карточка задачи: `// FIELDS`, заметки, чек-лист с таймером пункта, лента отрезков с ручной правкой и `[+ manual]`; форма и живые блоки перерисовываются порознь.
- `timer.js` — ОДИН секундный тикер на панель, отсчёт от опоры `snap.now`.
- `to-todo.js` — диалог выбора корня и копия задачи через `POST /api/todos`.
- `main.js` — состояние панели, сайдбар, режимы, мутации, `refreshTracker`.
- `css/tracker.css` — собственная палитра и терминальная вёрстка; единственный `!important` — правило `#tracker[hidden] { display: none !important }`, без него `hidden` бессилен против `display: flex` селектора по id.

## Тесты

`cargo test -p forge-tracker` — 90 (unit 36, store 21, routes 28, ws 5). Фронт: `tmux-web/tests/frontend/tracker_{parse,timer,kanban,gantt,report,push}.test.mjs` — 92 проверки. Дымовой тест хоста — `tmux-web/tests/tracker_smoke.rs`. Живая приёмка фазы 10 — 61 проверка в headless Chrome на изолированном стенде.

Полный план: `~/.claude/tracker-plan/PLAN.md`.