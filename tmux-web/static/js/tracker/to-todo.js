// tmux-web — плагин forge-tracker: перенос задачи в TODO вкладки Tasks.
//
// Экран «PUSH ▸ tasks/todo» из ~/Documents/ForgeTracker.pen (экран «Состояния
// и перенос»): заголовок переносимой задачи, объяснение, зачем спрашиваем,
// список корней, приписка про копию и кнопки CANCEL · PUSH ⏎.
//
// ЭТО КОПИЯ, А НЕ ПЕРЕЕЗД. В трекере после переноса не меняется НИЧЕГО: ни
// статус, ни `done_at`, ни отрезки, ни накопленное время, ни чек-лист. Модуль
// вообще не умеет писать в трекер — единственный запрос, который он делает,
// это `POST /api/todos` хоста. Обратной ссылки «этот todo сделан из той
// задачи» тоже не заводится: связывать две сущности — решение пользователя,
// а не побочный эффект кнопки.
//
// ПОЧЕМУ ВООБЩЕ СПРАШИВАЕМ КОРЕНЬ. Задачи трекера ГЛОБАЛЬНЫЕ: в модели
// (plugins/tracker/src/models.rs) нет никакого `root_path`, стор один на всю
// машину. TODO вкладки Tasks, наоборот, живёт в корне проекта —
// `POST /api/todos` обязателен к полю `path`, из которого сервер делает
// `paths::resolve_root`. Взять этот корень автоматически неоткуда, поэтому он
// выбирается в момент переноса — и это единственная причина существования
// диалога.
//
// НОВЫХ СЕРВЕРНЫХ РОУТОВ ФАЗА НЕ ДОБАВЛЯЕТ. Перенос целиком делает фронт
// через уже существующий `createTodoForPath` (js/tasks/crud.js). Крейт
// forge-tracker про `TodoStore` не знает и знать не должен: иначе личный слой
// задач начал бы зависеть от хранилища хоста, и плагин перестал бы быть
// отключаемым по флагу.
//
// ПОЧЕМУ DOM ПРИХОДИТ СНАРУЖИ, А НЕ ИЗ core/dom.js. Разбор задачи в текст
// описания, сборка списка корней и выбор корня по умолчанию — чистые функции,
// и они покрыты node-тестом (tmux-web/tests/frontend/tracker_push.test.mjs).
// Импорт `../core/dom.js` дёргает `document.getElementById` на уровне модуля и
// сделал бы файл неимпортируемым в голом node — ровно как у render-list.js и
// timer.js, которые тоже держатся без него. Поэтому ссылки на узлы и все
// внешние сервисы модуль получает один раз в `initTrackerPush(deps)`.

import { fmtDuration } from './render-list.js';

/** Последний выбранный корень — подставляется в следующий перенос. */
const LS_ROOT = 'forge.tracker.push.root';

/** Подпись виртуального варианта «корень активной сессии tmux» (из макета). */
export const SESSION_ROOT_LABEL = 'корень активной сессии tmux';

const local = {
    /**
     * Зависимости от main.js: узлы диалога и внешние сервисы.
     * @type {null | {
     *   dom: object,
     *   snapshot: function(): object,
     *   sessions: function(): Array<object>,
     *   activePath: function(): (string|null),
     *   create: function(string, string, string): Promise<object>,
     *   note: function(string, object=): void,
     *   openTasks: function(): void,
     * }}
     */
    deps: null,
    /** Слушатели навешены (идемпотентность init). */
    wired: false,
    /** Переносимая задача или null. */
    task: null,
    /** Варианты корней, показанные сейчас. */
    roots: [],
    /** Выбранный вариант (элемент `roots`) или null. */
    chosen: null,
    /** Запрос уже ушёл — второй клик по PUSH не должен завести вторую копию. */
    busy: false,
};

// ------------------------------------------------------------------ чистое

/**
 * Домашняя папка в пути → `~`, как в макете (`~/claudeWorkspace/F.O.R.G.E.`).
 *
 * Настоящего `$HOME` у браузера нет, и спрашивать его у сервера ради подписи
 * не стоит: сокращается только заведомо домашний префикс `/Users/<кто-то>` и
 * `/home/<кто-то>`. Всё остальное показывается как есть — лучше длинный
 * честный путь, чем красивый неверный.
 *
 * @param {string} path абсолютный путь
 * @returns {string}
 */
export function shortenPath(path) {
    const p = typeof path === 'string' ? path.trim() : '';
    if (!p) return '';
    const m = /^\/(?:Users|home)\/[^/]+(\/.*)?$/.exec(p);
    if (!m) return p === '/root' ? '~' : p;
    return `~${m[1] || ''}`;
}

/**
 * Список корней для диалога.
 *
 * Проекты берутся оттуда же, откуда их берёт остальной интерфейс: концепции
 * «проект» в devforge нет, источник истины — cwd открытых сессий tmux
 * (см. js/ws/todos-ws.js::sessionPathOrNull и paths::resolve_root на сервере).
 * Поэтому список проектов — это РАЗНЫЕ `session.path`, без повторов.
 *
 * Последним всегда идёт виртуальный вариант «корень активной сессии tmux»: он
 * есть в макете и он же — значение по умолчанию для первого переноса. Если
 * активной сессии нет, вариант остаётся в списке, но выбрать его нельзя
 * (`path: null`) — молча пропасть он не может, иначе непонятно, почему
 * умолчание вдруг другое.
 *
 * @param {Array<object>} sessions список сессий (`state.sessions`)
 * @param {string|null} activePath cwd активной сессии (`currentTodosPath()`)
 * @returns {Array<{path: (string|null), label: string, hint: string, kind: string}>}
 */
export function collectRoots(sessions, activePath) {
    const seen = new Set();
    const out = [];
    for (const s of Array.isArray(sessions) ? sessions : []) {
        const p = s && typeof s.path === 'string' ? s.path.trim() : '';
        if (!p || seen.has(p)) continue;
        seen.add(p);
        out.push({ path: p, label: shortenPath(p), hint: '', kind: 'project' });
    }
    out.sort((a, b) => a.label.localeCompare(b.label, 'ru'));

    const active = typeof activePath === 'string' ? activePath.trim() : '';
    out.push({
        path: active || null,
        label: SESSION_ROOT_LABEL,
        hint: active ? shortenPath(active) : 'сессия не выбрана',
        kind: 'session',
    });
    return out;
}

/**
 * Корень, выбранный при открытии диалога.
 *
 * Правило двухступенчатое и намеренно такое:
 *   1. если запомненный прошлый выбор ЕСТЬ среди проектов — берём его
 *      (критерий «выбранный корень подставляется в следующий перенос»);
 *   2. иначе — корень активной сессии (умолчание из плана: `currentTodosPath`).
 * Когда ни того, ни другого нет, берём первый доступный проект, а если и
 * проектов нет — `null`, и диалог не даст нажать PUSH.
 *
 * @param {Array<object>} roots результат `collectRoots`
 * @param {string|null} savedPath запомненный путь
 * @returns {object|null}
 */
export function pickRoot(roots, savedPath) {
    const usable = (Array.isArray(roots) ? roots : []).filter((r) => r && r.path);
    if (!usable.length) return null;
    const saved = typeof savedPath === 'string' ? savedPath.trim() : '';
    if (saved) {
        const hit = usable.find((r) => r.kind === 'project' && r.path === saved);
        if (hit) return hit;
    }
    return usable.find((r) => r.kind === 'session') || usable[0];
}

/** `P1`, `~4ч30м`, `2/5` и прочие мелочи описания. */
function estText(minutes) {
    const m = Number(minutes);
    if (!Number.isFinite(m) || m <= 0) return '';
    const h = Math.floor(m / 60);
    const rest = m % 60;
    if (h && rest) return `~${h}ч${rest}м`;
    if (h) return `~${h}ч`;
    return `~${rest}м`;
}

/**
 * Текст описания будущей TODO-карточки. ФОРМАТ ЗАФИКСИРОВАН здесь и в доке.
 *
 *     <заметки задачи, если есть>
 *
 *     // TRACKER NODE 3f1c9a20
 *     приоритет: P1
 *     статус: doing
 *     список: work
 *     теги: #forge #design
 *     оценка: ~4ч
 *     начало: 2026-09-08
 *     срок: 2026-09-09 18:00
 *     потрачено: 0:42:19
 *
 *     // CHECKLIST 2/5
 *     [x] свести палитру
 *     [ ] отдать в реализацию
 *
 * Пустые поля строк не занимают: описание должно читаться как записка, а не
 * как форма с прочерками. Заметки идут ПЕРВЫМИ и без заголовка — в карточке
 * Tasks видно начало текста, и там должно быть содержание, а не метаданные.
 *
 * @param {object} task задача трекера
 * @param {{listName?: string, seconds?: number, items?: Array<object>}} ctx
 * @returns {string}
 */
export function buildDescription(task, ctx) {
    const t = task || {};
    const c = ctx || {};
    const lines = [];

    const notes = typeof t.notes === 'string' ? t.notes.trim() : '';
    if (notes) lines.push(notes, '');

    lines.push(`// TRACKER NODE ${String(t.id || '').slice(0, 8)}`);
    const prio = Number.isFinite(Number(t.priority)) ? Number(t.priority) : 2;
    lines.push(`приоритет: P${prio}`);
    lines.push(`статус: ${t.status || 'inbox'}`);
    if (c.listName) lines.push(`список: ${c.listName}`);
    const tags = Array.isArray(t.tags) ? t.tags.filter(Boolean) : [];
    if (tags.length) lines.push(`теги: ${tags.map((x) => `#${x}`).join(' ')}`);
    const est = estText(t.estimate_min);
    if (est) lines.push(`оценка: ${est}`);
    if (t.start) lines.push(`начало: ${t.start}`);
    if (t.due) lines.push(`срок: ${t.due}`);
    const secs = Number(c.seconds);
    if (Number.isFinite(secs) && secs > 0) lines.push(`потрачено: ${fmtDuration(secs)}`);

    const items = Array.isArray(c.items) ? c.items : [];
    if (items.length) {
        const done = items.filter((i) => i && i.done).length;
        lines.push('', `// CHECKLIST ${done}/${items.length}`);
        for (const it of items) {
            lines.push(`${it && it.done ? '[x]' : '[ ]'} ${(it && it.title) || ''}`);
        }
    }
    return lines.join('\n');
}

/**
 * Контекст задачи из снимка панели: имя списка, накопленное время и пункты
 * чек-листа по порядку. Отдельного запроса ради описания не делаем — всё это
 * уже лежит в снимке `/api/tracker/state`.
 *
 * @param {object} snap снимок панели
 * @param {object} task задача
 */
export function contextOf(snap, task) {
    const s = snap || {};
    const id = task && task.id;
    const list = (Array.isArray(s.lists) ? s.lists : [])
        .find((l) => l && task && l.id === task.list_id);
    const items = (Array.isArray(s.items) ? s.items : [])
        .filter((i) => i && i.task_id === id)
        .slice()
        .sort((a, b) => (a.order || 0) - (b.order || 0));
    const seconds = (s.task_seconds && s.task_seconds[id]) || 0;
    return { listName: (list && list.name) || '', seconds, items };
}

// --------------------------------------------------------------------- DOM

function readLs(key) {
    try { return localStorage.getItem(key) || ''; } catch (_) { return ''; }
}
function writeLs(key, value) {
    try { localStorage.setItem(key, value); } catch (_) { /* приватный режим */ }
}

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined && text !== null) n.textContent = text;
    return n;
}

function dom() {
    return (local.deps && local.deps.dom) || {};
}

function showError(message) {
    const { $trkPushError } = dom();
    if (!$trkPushError) return;
    $trkPushError.textContent = message || '';
    $trkPushError.hidden = !message;
}

/**
 * Перерисовать список корней и состояние кнопки PUSH.
 *
 * Кнопка гаснет, когда выбирать не из чего: подписать «нет ни одного корня»
 * и оставить кнопку живой значило бы отправить заведомый 400 и показать
 * человеку ошибку сервера вместо объяснения.
 */
function paintRoots() {
    const { $trkPushRoots, $trkPushOk } = dom();
    if ($trkPushRoots) {
        $trkPushRoots.textContent = '';
        for (const root of local.roots) {
            const li = el('li', 'trk-push-root');
            const on = !!local.chosen && local.chosen === root;
            if (on) li.classList.add('active');
            if (!root.path) li.classList.add('disabled');
            li.appendChild(el('span', 'trk-push-mark', on ? '(•)' : '( )'));
            li.appendChild(el('span', 'trk-push-path', root.label));
            if (root.hint) li.appendChild(el('span', 'trk-push-hint-path', root.hint));
            if (root.path) {
                li.addEventListener('click', () => {
                    local.chosen = root;
                    showError('');
                    paintRoots();
                });
            }
            $trkPushRoots.appendChild(li);
        }
        if (!local.roots.some((r) => r.path)) {
            // Внятная подсказка вместо молчащей кнопки: корней нет только
            // тогда, когда не открыто ни одной сессии tmux.
            $trkPushRoots.appendChild(el('li', 'trk-push-empty',
                'корней не найдено: открой сессию tmux в нужном проекте — '
                + 'TODO живёт в корне, и без него переносить некуда'));
        }
    }
    if ($trkPushOk) $trkPushOk.disabled = local.busy || !local.chosen;
}

/**
 * Открыть диалог переноса.
 *
 * @param {object} task задача трекера
 */
export function openPushDialog(task) {
    const { $trkPush, $trkPushNode } = dom();
    if (!$trkPush || !task) return;
    local.task = task;
    local.busy = false;
    showError('');

    const deps = local.deps;
    local.roots = collectRoots(deps.sessions(), deps.activePath());
    local.chosen = pickRoot(local.roots, readLs(LS_ROOT));

    if ($trkPushNode) {
        const prio = Number.isFinite(Number(task.priority)) ? Number(task.priority) : 2;
        $trkPushNode.textContent = `P${prio} ${task.title || ''}`;
    }
    paintRoots();
    $trkPush.hidden = false;
}

/** Закрыть диалог, ничего не создавая. */
export function closePushDialog() {
    const { $trkPush } = dom();
    if (!$trkPush) return;
    $trkPush.hidden = true;
    local.task = null;
    local.roots = [];
    local.chosen = null;
    local.busy = false;
    showError('');
}

/** Открыт ли диалог. Нужен карточке задачи, чтобы не перехватывать Esc. */
export function isPushOpen() {
    const { $trkPush } = dom();
    return !!$trkPush && !$trkPush.hidden;
}

/**
 * Собственно перенос: одна карточка в колонку TODO выбранного корня.
 *
 * ПОВТОРНЫЙ ПЕРЕНОС ТОЙ ЖЕ ЗАДАЧИ РАЗРЕШЁН и создаёт вторую карточку. Это
 * принятое правило, а не недосмотр: связи между задачей и todo не заводится,
 * значит проверять «уже переносили» не по чему, а угадывать по заголовку
 * значило бы запрещать человеку намеренную вторую копию (например, в другой
 * корень). Защищаемся только от ДВОЙНОГО КЛИКА — флагом `busy`.
 */
async function push() {
    if (local.busy || !local.task || !local.chosen || !local.chosen.path) return;
    const deps = local.deps;
    const task = local.task;
    const root = local.chosen;
    local.busy = true;
    paintRoots();
    showError('');

    const title = String(task.title || '').trim() || 'задача трекера';
    const description = buildDescription(task, contextOf(deps.snapshot(), task));

    try {
        await deps.create(root.path, title, description);
    } catch (e) {
        local.busy = false;
        paintRoots();
        // Ошибка показывается ЗДЕСЬ, а не в статус-баре за спиной у диалога:
        // диалог не закрывается, выбранный корень сохраняется, повтор — один
        // клик. Задача трекера при этом не тронута ничем.
        showError(`перенос не удался: ${(e && e.message) || e}`);
        return;
    }

    writeLs(LS_ROOT, root.path);
    closePushDialog();
    deps.note(`→ tasks/todo: «${title}» скопирована в ${root.label}`, {
        label: '[открыть Tasks]',
        run: () => deps.openTasks(),
        ttl: 10000,
    });
}

function onKeydown(ev) {
    if (ev.key !== 'Escape') return;
    if (!isPushOpen()) return;
    ev.stopPropagation();
    closePushDialog();
}

/**
 * Навесить слушатели диалога. Идемпотентна: зовётся из initTracker на каждом
 * входе на вкладку.
 *
 * @param {object} deps узлы диалога (`dom`) и сервисы: `snapshot`, `sessions`,
 *   `activePath`, `create`, `note`, `openTasks`.
 */
export function initTrackerPush(deps) {
    local.deps = deps || local.deps;
    if (local.wired) return;
    local.wired = true;
    const { $trkPush, $trkPushClose, $trkPushCancel, $trkPushOk } = dom();
    if ($trkPushClose) $trkPushClose.addEventListener('click', closePushDialog);
    if ($trkPushCancel) $trkPushCancel.addEventListener('click', closePushDialog);
    if ($trkPushOk) $trkPushOk.addEventListener('click', push);
    if ($trkPush) {
        $trkPush.addEventListener('click', (ev) => {
            if (ev.target === $trkPush) closePushDialog();
        });
    }
    document.addEventListener('keydown', onKeydown);
}

/** Снять слушатель Esc (beforeunload / teardown панели). */
export function teardownTrackerPush() {
    document.removeEventListener('keydown', onKeydown);
    local.wired = false;
}
