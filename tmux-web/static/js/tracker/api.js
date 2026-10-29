// tmux-web — плагин forge-tracker: тонкий клиент REST `/api/tracker/*`.
//
// Модуль знает ровно две вещи: адреса роутов (таблица заморожена в
// plugins/tracker/src/routes/mod.rs) и единый контракт ошибок
// `{"error":{"code","message"}}` (plugins/tracker/src/routes/error.rs).
// Никакого состояния он не держит: снимок живёт в js/tracker/main.js, а
// живые обновления приходят через js/tracker/ws.js.
//
// ГЕЙТ ПЛАГИНА. `getStatus()` — ЕДИНСТВЕННОЕ место, где 404 не ошибка:
// плагин opt-in, и запуск devforge без `--tracker` штатен для всех, кто им
// не пользуется. Поэтому 404 возвращается как `null` и в консоль НЕ пишется
// ни строчки — иначе каждая загрузка страницы у всех остальных давала бы
// красную строку. Все прочие функции на 404 честно бросают: там 404 значит
// «задачи с таким id нет», и молчание спрятало бы расхождение состояния.
//
// Авторизация не делается здесь: `window.fetch` подменён в js/core/auth.js и
// сам добавляет `Authorization: Bearer …` ко всем same-origin запросам.

/** Ошибка REST-слоя трекера: несёт HTTP-код и машиночитаемый `code` сервера. */
export class TrackerApiError extends Error {
    constructor(status, code, message) {
        super(message || `tracker api ${status}`);
        this.name = 'TrackerApiError';
        this.status = status;
        /** `bad_request` | `not_found` | `conflict` | `internal` | `http`. */
        this.code = code || 'http';
    }
}

/**
 * Общий вызов: JSON туда, JSON обратно, ошибка — в TrackerApiError.
 *
 * Пустое тело ответа (204 или роут, который ничего не возвращает) даёт
 * `null`, а не падение на JSON.parse.
 */
async function call(method, path, body) {
    const init = { method, headers: { Accept: 'application/json' } };
    if (body !== undefined) {
        init.headers['Content-Type'] = 'application/json';
        init.body = JSON.stringify(body);
    }
    let res;
    try {
        res = await fetch(path, init);
    } catch (e) {
        // Сеть/сервер лёг: у ошибки нет ни статуса, ни кода сервера, но у
        // вызывающего должен быть один тип исключения на все случаи.
        throw new TrackerApiError(0, 'network', String((e && e.message) || e));
    }
    const raw = await res.text();
    let data = null;
    if (raw) {
        try { data = JSON.parse(raw); } catch (_) { data = null; }
    }
    if (!res.ok) {
        const err = data && data.error;
        throw new TrackerApiError(
            res.status,
            (err && err.code) || 'http',
            (err && err.message) || `${method} ${path} → ${res.status}`,
        );
    }
    return data;
}

const get = (p) => call('GET', p);
const post = (p, b) => call('POST', p, b === undefined ? {} : b);
const patch = (p, b) => call('PATCH', p, b);
const del = (p) => call('DELETE', p);

/** Хвост query-строки из пар, у которых значение задано. */
function qs(params) {
    const u = new URLSearchParams();
    Object.keys(params || {}).forEach((k) => {
        const v = params[k];
        if (v !== undefined && v !== null && v !== '') u.set(k, String(v));
    });
    const s = u.toString();
    return s ? `?${s}` : '';
}

const enc = encodeURIComponent;

// ---------------------------------------------------------------- гейт и снимок

/**
 * `GET /api/tracker/status` — гейт плагина.
 *
 * @returns {Promise<object|null>} тело ответа при 200, `null` при 404 или
 *          любой сетевой беде. Не бросает НИКОГДА: вызывающий (гейт кнопки в
 *          core/bootstrap.js) обязан уметь работать без плагина.
 */
export async function getStatus() {
    try {
        const res = await fetch('/api/tracker/status', { headers: { Accept: 'application/json' } });
        if (res.status === 404) return null;
        if (!res.ok) return null;
        return await res.json();
    } catch (_) {
        return null;
    }
}

/** `GET /api/tracker/state` — полный снимок (та же форма, что кадр `snapshot`). */
export const getState = () => get('/api/tracker/state');

// ------------------------------------------------------------------------ задачи

export const listTasks = () => get('/api/tracker/tasks');
export const getTask = (id) => get(`/api/tracker/tasks/${enc(id)}`);
export const createTask = (payload) => post('/api/tracker/tasks', payload);
export const patchTask = (id, payload) => patch(`/api/tracker/tasks/${enc(id)}`, payload);
export const deleteTask = (id) => del(`/api/tracker/tasks/${enc(id)}`);
export const setTaskStatus = (id, status) => patch(`/api/tracker/tasks/${enc(id)}/status`, { status });
/** Полный порядок задач одним списком id (перестановка drag'ом). */
export const reorderTasks = (ids) => post('/api/tracker/tasks/reorder', { ids });

// ----------------------------------------------------------------------- таймеры

/**
 * Старт таймера задачи.
 *
 * Параллельные таймеры по РАЗНЫМ задачам штатны. Повторный старт по той же
 * задаче сервер не считает ошибкой — он возвращает уже идущий отрезок.
 */
export const startTimer = (id, body) => post(`/api/tracker/tasks/${enc(id)}/timer/start`, body || {});
export const stopTimer = (id) => post(`/api/tracker/tasks/${enc(id)}/timer/stop`, {});

// -------------------------------------------------------------- отрезки времени

export const listEntries = (params) => get(`/api/tracker/entries${qs(params)}`);
export const createEntry = (payload) => post('/api/tracker/entries', payload);
export const patchEntry = (id, payload) => patch(`/api/tracker/entries/${enc(id)}`, payload);
export const deleteEntry = (id) => del(`/api/tracker/entries/${enc(id)}`);

// ------------------------------------------------------------------------ списки

export const listLists = () => get('/api/tracker/lists');
export const createList = (payload) => post('/api/tracker/lists', payload);
export const patchList = (id, payload) => patch(`/api/tracker/lists/${enc(id)}`, payload);
export const deleteList = (id) => del(`/api/tracker/lists/${enc(id)}`);
export const reorderLists = (ids) => post('/api/tracker/lists/reorder', { ids });

// ---------------------------------------------------------------------- чек-лист

export const listItems = (taskId) => get(`/api/tracker/tasks/${enc(taskId)}/items`);
export const createItem = (taskId, payload) => post(`/api/tracker/tasks/${enc(taskId)}/items`, payload);
export const patchItem = (taskId, itemId, payload) => patch(`/api/tracker/tasks/${enc(taskId)}/items/${enc(itemId)}`, payload);
export const deleteItem = (taskId, itemId) => del(`/api/tracker/tasks/${enc(taskId)}/items/${enc(itemId)}`);
export const toggleItem = (taskId, itemId) => post(`/api/tracker/tasks/${enc(taskId)}/items/${enc(itemId)}/toggle`, {});
export const reorderItems = (taskId, ids) => post(`/api/tracker/tasks/${enc(taskId)}/items/reorder`, { ids });
export const startItemTimer = (taskId, itemId) => post(`/api/tracker/tasks/${enc(taskId)}/items/${enc(itemId)}/timer/start`, {});
export const stopItemTimer = (taskId, itemId) => post(`/api/tracker/tasks/${enc(taskId)}/items/${enc(itemId)}/timer/stop`, {});

// -------------------------------------------------------------------------- вехи

export const listMilestones = () => get('/api/tracker/milestones');
export const createMilestone = (payload) => post('/api/tracker/milestones', payload);
export const patchMilestone = (id, payload) => patch(`/api/tracker/milestones/${enc(id)}`, payload);
export const deleteMilestone = (id) => del(`/api/tracker/milestones/${enc(id)}`);

// ----------------------------------------------------------------------- отчёты

/** `GET /api/tracker/report?from&to&group_by=day|task|list` (экран — фаза 9). */
export const getReport = (params) => get(`/api/tracker/report${qs(params)}`);
/** `GET /api/tracker/gantt?from&to` (экран — фаза 7). */
export const getGantt = (params) => get(`/api/tracker/gantt${qs(params)}`);
