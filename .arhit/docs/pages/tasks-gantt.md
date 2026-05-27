ОБЗОР ФИЧИ: Гант-таймлайн на вкладке Tasks (epic forge-typf, базовая реализация Phase 1-4; доработки — диапазоны Сегодня/Вчера + hover-попап коммита с файлами).

== Что это ==
Гант-диаграмма (таймлайн) в нижней части вкладки Tasks под канбан-доской. Визуализирует длительность задач горизонтальными полосами на временной оси и накладывает поверх них вертикальные черты git-коммитов корня текущей tmux-сессии. Цель — наглядно сопоставить активность по задачам с реальными коммитами. Рисуется чистым DOM + CSS, без чарт-библиотек.

== Источник данных задач ==
state.tasksData.issues (тот же массив, что питает канбан — из GET /api/tasks). Фильтр в renderGantt: только задачи status in {in_progress, closed} (lowercase) с валидным created_at (Date.parse != NaN). open/blocked на таймлайне НЕ показываются.

== Полосы задач ==
Каждая отобранная задача — строка .gantt-row с подписью .gantt-row-label (id + укороченный ~40 симв title) и полосой .gantt-bar. Полоса от created_at до: closed_at (closed, .status-closed, зелёная var(--success)) либо до t1 домена (in_progress, .status-in_progress, синяя var(--info)). Полосы целиком вне окна отбрасываются; границы клампятся к [t0,t1]. left/width — inline-проценты от домена.

== Временной домен и переключатель диапазонов ==
Единый источник домена — хелпер ganttWindow(rows) (gantt.js, exported), возвращает {t0,t1,since,until}: t0/t1 — границы в МИЛЛИСЕКУНДАХ для рендера; since/until — те же в СЕКУНДАХ Unix для запроса коммитов (null = не ограничивать). state.ganttRange: number(7|30) | 'all' | 'today' | 'yesterday' (по умолчанию 7).
- 'today' (НОВОЕ): t0 = начало сегодняшних локальных суток (setHours(0,0,0,0)), t1 = now, since = floor(t0/1000), until = null.
- 'yesterday' (НОВОЕ): t0 = начало вчерашних суток, t1 = начало сегодняшних — окно ровно «вчера»; заданы И since, И until.
- number N (7/30): t0 = now - N*86400e3, t1 = now, since задан, until = null.
- 'all': t0 = min(rows.start) (пустой rows → t0=null → renderEmpty), t1 = now, since = null, until = null.
ВАЖНО: t1 больше НЕ всегда Date.now() — для 'yesterday' это начало текущих суток. Переключатель — toggle-кнопки #gantt-range (data-range=today|yesterday|7|30|all) в #gantt-toolbar; initGanttControls идемпотентно (dataset.ganttBound) вешает click → меняет state.ganttRange, переключает .active, вызывает fetchGitCommits (подгрузка под новые since/until + перерисовка).

== Эндпоинты и единицы ts ==
СПИСОК коммитов: GET /api/git/commits?path=<abs>&since=<unix>&until=<unix> → {"commits":[{hash,ts,subject,author}]}. since/until опциональны, оба в СЕКУНДАХ Unix (committer date %ct), задают диапазон [since, until]; fetchGitCommits добавляет каждый параметр в URL ТОЛЬКО когда он не null (since=null при 'all'; until=null при 'today'/N дней; 'yesterday' задаёт оба). Бэкенд git::list_commits(cwd, since_unix, until_unix) добавляет --since/--until к git log независимо.
ДЕТАЛИ коммита (НОВОЕ): GET /api/git/commit?path=<abs>&hash=<sha>[&server=<id>] → {"commit": {hash,ts,subject,body,author,files:[{status,path}]}} либо {"commit": null}. ts — committer date в СЕКУНДАХ. files — изменённые файлы из git show --name-status (status = одна буква A/M/D/R/C, path = НОВЫЙ путь при переименовании). Бэкенд git::commit_detail двумя вызовами git show: 1) мета+body (--no-patch --format=...%b), 2) --name-status --format= (только файлы). hash приходит из недоверенного query → валидируется git::is_valid_hash (hex, длина 4..=64) ПЕРЕД spawn; невалидный → {commit:null} (защита от инъекции аргументов). server задан → remote не проксируется → {commit:null} (коммиты локальны). Всё graceful: не-git каталог/нет коммита/ошибка → null, вкладка не падает.

== Коммиты как вертикальные черты ==
Overlay-слой .gantt-commits-overlay (absolute, во всю высоту) поверх дорожек. Каждый коммит из state.gitCommits — .gantt-commit (тонкая вертикальная черта, left% = (ts*1000 - t0)/span*100, цвет var(--accent), вне окна skip). renderCommits проставляет каждой черте dataset.hash (полный sha), dataset.subject, нативный title (hash7+subject — мгновенный fallback) и вызывает attachCommitHover.

== HOVER-ПОПАП КОММИТА С ФАЙЛАМИ (НОВОЕ) ==
.gantt-commit-popover — один shared DOM-элемент на всю страницу (ленивая ensurePopover(), аппендится в body, переиспользуется всеми чертами; на нём mouseenter отменяет скрытие / mouseleave планирует). Заменяет нативный title богатым попапом.
- КЭШ: module-level detailCache = Map<hash, detail|null> — кэширует и успех, и null, чтобы не повторять fetch.
- АНТИ-ГОНКА: popoverToken (монотонный счётчик) — устаревший fetch-ответ не перерисует попап; таймеры showTimer (POPOVER_SHOW_DELAY=120мс) / hideTimer (POPOVER_HIDE_DELAY=150мс).
- ПОТОК: mouseenter → 120мс → openPopoverFor: hash из dataset; detailCache.has → мгновенный renderPopover из кэша; иначе renderPopover(null,...) (fallback hash7+subject) + fetchCommitDetail(hash) → по token-проверенному ответу перерисовка. fetchCommitDetail: GET /api/git/commit?path=<enc cwd>&hash=<enc hash> (path только если sessionCwdOrNull()!=null), возвращает json.commit (объект|null).
- РЕНДЕР: всё через textContent (никакого innerHTML с git-данными — XSS-safe). Содержимое: head (hash7 моноширинный + дата fmtDate(ts*1000)), author, subject (жирн), body в <pre> при непустом trim (обрезка до 800 симв), список .gantt-commit-files → .gantt-file (.gantt-file-status status-<перваяБуква статуса upper> + .gantt-file-path). detail===null → минимальный fallback hash7+subject.
- ПОЗИЦИЯ: positionPopover — position:fixed от getBoundingClientRect черты, справа от черты (при нехватке места слева), кламп по window.innerWidth/innerHeight (POPOVER_GAP=10px, POPOVER_MARGIN=8px).
CSS (tasks.css, только переменные темы): .gantt-commit-popover (bg --bg-elev, border --border-input, shadow --shadow-pill, max-width 360px, max-height 50vh overflow auto, z-index 1000, [hidden]→display:none). Статусы файлов: status-A=--success, status-M=--info, status-D=--danger, status-R/status-C=--fg-dim. .gantt-commit hover-стейт: cursor:pointer, утолщение 2px→4px, --accent-hover.

== ОГРАНИЧЕНИЕ: 'начата' = created_at ==
beads (br) НЕ хранит момент перехода задачи open→in_progress. Поэтому левая граница полосы ВСЕГДА привязана к created_at, а не к фактическому началу работы. Сознательное фиксированное допущение: для closed полоса = created_at→closed_at, для in_progress = created_at→t1 домена.

== Файлы ==
- tmux-web/src/git.rs — backend: struct Commit (Serialize) + list_commits(cwd, since_unix, until_unix) + parse_log; struct CommitDetail{hash,ts,subject,body,author,files:Vec<FileChange>} и FileChange{status,path} + commit_detail(cwd, hash) + parse_meta/parse_name_status; is_valid_hash (валидация hex 4..=64); юнит-тесты парсеров и валидатора.
- tmux-web/src/main.rs — хендлеры get_git_commits (роут /api/git/commits, ?since/?until/?server) и get_git_commit (роут /api/git/commit, ?path/?hash/?server); mod git.
- tmux-web/static/js/tasks/gantt.js — frontend: ganttWindow, renderGantt, fetchGitCommits, initGanttControls, renderCommits + попап (ensurePopover/attachCommitHover/openPopoverFor/fetchCommitDetail/renderPopover/positionPopover/scheduleHide, detailCache, popoverToken) + хелперы (buildAxis, clamp, parseTs, fmtDate, fmtAxisDate, shortTitle, renderEmpty).
- Разметка: tmux-web/static/index.html (#tasks-gantt → #gantt-toolbar(.gantt-title + #gantt-range кнопки today/yesterday/7/30/all) + #gantt-canvas).
- Стили: tmux-web/static/css/tasks.css, раздел 'Gantt timeline' (#tasks-gantt, #gantt-canvas, .gantt-axis/.gantt-tick, .gantt-row/.gantt-row-label, .gantt-bar.status-*, .gantt-commit + hover, .gantt-commit-popover, .gantt-file/.gantt-file-status status-*, .gantt-empty).
- Поля state: tmux-web/static/js/core/state.js (gitCommits, ganttRange).

== Точки интеграции ==
- render.js (renderTasks): после рендера канбана вызывает renderGantt() — перерисовка при каждом WS-апдейте задач.
- tabs/tabs.js (switchTab onTasks): при входе на вкладку Tasks вызывает initGanttControls() + fetchGitCommits().
- ws/tasks-ws.js (syncTasksToCurrentSession): при смене cwd активной сессии (если вкладка tasks активна) вызывает fetchGitCommits(); sessionCwdOrNull экспортируется отсюда и импортируется в gantt.js.