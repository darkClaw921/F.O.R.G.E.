// Регресс-тест применения белого списка к колонке чатов (forge-c2dt).
//
// Чистая логика фильтра покрыта соседним telegram_visibility.test.mjs. Здесь
// проверяется, что `dialogs.js` реально ей пользуется: какие строки попадают в
// колонку, что считают счётчики папок и какие пункты навигации помечаются
// пустыми. Отдельно зафиксировано, что счётчики считают по ВИДИМОМУ срезу:
// цифра, посчитанная по всему списку, вела бы в чат, которого в колонке нет.
//
// Стаб DOM обязан стоять раньше первого импорта: `core/dom.js` резолвит ref'ы
// один раз на верхнем уровне модуля.
//
// Запуск: node tmux-web/tests/frontend/telegram_dialogs_filter.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM (срез, которым пользуется dialogs.js)
// ---------------------------------------------------------------------------

function matchesSel(node, sel) {
    if (!node || node.nodeType !== 1) return false;
    const m = String(sel).trim().match(/^([a-zA-Z]*)((?:\.[-\w]+)*)$/);
    if (!m) return false;
    const [, tag, classes] = m;
    if (tag && node.tagName !== tag.toLowerCase()) return false;
    for (const c of (classes || '').split('.').filter(Boolean)) {
        if (!node.classList.contains(c)) return false;
    }
    return true;
}

function makeText(data) {
    return { nodeType: 3, tagName: null, parentNode: null, textContent: String(data) };
}

function detach(node) {
    const p = node.parentNode;
    if (!p) return;
    const i = p.childNodes.indexOf(node);
    if (i >= 0) p.childNodes.splice(i, 1);
    node.parentNode = null;
}

function makeEl(tag) {
    const node = {
        nodeType: 1,
        tagName: String(tag || 'div').toLowerCase(),
        childNodes: [],
        parentNode: null,
        dataset: {},
        style: {},
        attributes: {},
        listeners: new Map(),
        className: '',
        hidden: false,
        title: '',
        tabIndex: -1,
    };
    node.classList = {
        add(...cs) {
            const set = new Set(node.className.split(/\s+/).filter(Boolean));
            cs.forEach((c) => set.add(c));
            node.className = Array.from(set).join(' ');
        },
        remove(...cs) {
            const set = new Set(node.className.split(/\s+/).filter(Boolean));
            cs.forEach((c) => set.delete(c));
            node.className = Array.from(set).join(' ');
        },
        contains(c) {
            return node.className.split(/\s+/).filter(Boolean).includes(c);
        },
        toggle(c, on) {
            const has = node.classList.contains(c);
            const want = on === undefined ? !has : !!on;
            if (want) node.classList.add(c);
            else node.classList.remove(c);
            return want;
        },
    };
    Object.defineProperty(node, 'textContent', {
        get: () => node.childNodes.map((c) => c.textContent).join(''),
        set(v) {
            node.childNodes.slice().forEach(detach);
            const t = String(v == null ? '' : v);
            if (t !== '') node.appendChild(makeText(t));
        },
    });
    Object.defineProperty(node, 'children', {
        get: () => node.childNodes.filter((c) => c.nodeType === 1),
    });
    node.appendChild = (child) => {
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
    node.closest = (sel) => {
        let cur = node;
        while (cur) {
            if (matchesSel(cur, sel)) return cur;
            cur = cur.parentNode;
        }
        return null;
    };
    node.querySelectorAll = (sel) => {
        const out = [];
        const walk = (n) => n.childNodes.forEach((c) => {
            if (c.nodeType !== 1) return;
            if (matchesSel(c, sel)) out.push(c);
            walk(c);
        });
        walk(node);
        return out;
    };
    node.querySelector = (sel) => node.querySelectorAll(sel)[0] || null;
    node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
    node.getAttribute = (k) => (k in node.attributes ? node.attributes[k] : null);
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.removeEventListener = () => {};
    node.focus = () => {};
    node.scrollIntoView = () => {};
    return node;
}

const els = new Map();
function el(id, tag) {
    const node = makeEl(tag || 'div');
    node.attributes.id = id;
    els.set(id, node);
    return node;
}

const $body = makeEl('body');
const $panel = el('telegram');
const $dirList = el('tg-dir-list', 'ul');
const $dialogList = el('tg-dialog-list', 'ul');
const $channelList = el('tg-channel-list', 'ul');
const $nodesTitle = el('tg-nodes-title');
el('tg-status-right');
$panel.appendChild($dirList);
$panel.appendChild($dialogList);
$panel.appendChild($channelList);
$panel.appendChild($nodesTitle);
$body.appendChild($panel);

const $document = makeEl('#document');
$document.getElementById = (id) => els.get(id) || null;
$document.createElement = (tag) => makeEl(tag);
$document.createTextNode = (t) => makeText(t);
$document.body = $body;
$document.documentElement = makeEl('html');
$document.querySelector = (sel) => $body.querySelector(sel);
$document.querySelectorAll = (sel) => $body.querySelectorAll(sel);
Object.defineProperty($document, 'activeElement', { get: () => null });
globalThis.document = $document;

globalThis.location = {
    protocol: 'http:', host: 'localhost', hash: '', href: 'http://localhost/',
    origin: 'http://localhost', pathname: '/', search: '',
};
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };

/** Настройки, которые видит панель через публичный контракт. */
const userSettings = {};
globalThis.window = {
    addEventListener() {}, removeEventListener() {},
    location: globalThis.location,
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    fetch: () => Promise.reject(new Error('offline')),
    setTimeout, clearTimeout,
    ForgeApp: { state: { userSettings } },
};
globalThis.fetch = globalThis.window.fetch;

// ---------------------------------------------------------------------------
// Модули
// ---------------------------------------------------------------------------

const base = new URL('../../static/js/telegram/', import.meta.url).href;
const dialogs = await import(`${base}dialogs.js`);
const { invalidateVisibilityFilter, tgState } = await import(`${base}state.js`);

// ---------------------------------------------------------------------------
// Мини-фреймворк
// ---------------------------------------------------------------------------

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

// ---------------------------------------------------------------------------
// Данные
// ---------------------------------------------------------------------------

const WORK = { id: 7, title: 'Работа', include_ids: ['10', '11'] };
const BOTS = { id: 8, title: 'Боты', bots: true };

function setup(enabled, folders, chats, activeDir) {
    userSettings.telegram_filter_enabled = enabled;
    userSettings.telegram_visible_folders = folders || [];
    userSettings.telegram_visible_dialogs = chats || [];
    invalidateVisibilityFilter();
    tgState.folders = [WORK, BOTS];
    // Поля bot/contact заданы ЯВНО у каждого личного чата: без них
    // `format.js::dialogCategory` возвращает 'user' («сервер не сказал»), и
    // категорийная папка вроде «Боты» вынужденно пропускает такой чат. Фикстура
    // без флагов проверяла бы деградацию для старого сервера, а не фильтр.
    tgState.dialogs = [
        { id: '10', kind: 'user', title: 'Коллега', unread_count: 2, muted: false, archived: false, bot: false, contact: true },
        { id: '11', kind: 'group', title: 'Отдел', unread_count: 1, muted: false, archived: false },
        { id: '12', kind: 'user', title: 'Робот', unread_count: 4, muted: false, archived: false, bot: true, contact: false },
        { id: '13', kind: 'user', title: 'Сосед', unread_count: 3, muted: false, archived: false, bot: false, contact: false },
        { id: '14', kind: 'channel', title: 'Канал', unread_count: 5, muted: false, archived: false },
        { id: '15', kind: 'user', title: 'Старый', unread_count: 7, muted: false, archived: true, bot: false, contact: true },
    ];
    tgState.activeDir = activeDir || 'inbox';
    dialogs.renderAll();
}

/** id чатов, нарисованных в колонке узлов. */
function nodeIds() {
    return $dialogList.children.map((li) => li.dataset.dialogId);
}

/** id каналов, нарисованных во второй колонке. */
function channelIds() {
    return $channelList.children.map((li) => li.dataset.dialogId);
}

/** Текст счётчика у пункта навигации. */
function dirCount(key) {
    const li = $dirList.children.find((n) => n.dataset.dir === key);
    if (!li) return null;
    const count = li.querySelector('.tg-dir-count');
    return count ? count.textContent : null;
}

/** Помечен ли пункт как «нет видимых чатов». */
function dirDimmed(key) {
    const li = $dirList.children.find((n) => n.dataset.dir === key);
    return !!(li && li.classList.contains('tg-dir-filtered-out'));
}

// ---------------------------------------------------------------------------
// 1. Фильтр выключен — поведение до фичи
// ---------------------------------------------------------------------------

setup(false, [], []);
eq(nodeIds(), ['10', '11', '12', '13'], 'выключен: в колонке все неархивные узлы');
eq(channelIds(), ['14'], 'выключен: канал в своей колонке');
eq(dirCount('inbox'), '(15)', 'выключен: счётчик inbox по всем неархивным');
eq(dirCount('archive'), '(7)', 'выключен: счётчик архива');
ok(!dirDimmed('inbox'), 'выключен: пункты не приглушаются');
ok(!dirDimmed('folder:8'), 'выключен: пустых пунктов не бывает');

// Включённый тумблер с пустым выбором фильтром не считается.
setup(true, [], []);
eq(nodeIds(), ['10', '11', '12', '13'], 'включён без выбора: видно всё');
ok(!dirDimmed('folder:8'), 'включён без выбора: пункты не приглушены');

// ---------------------------------------------------------------------------
// 2. Фильтр по папке
// ---------------------------------------------------------------------------

setup(true, ['folder:7'], []);
eq(nodeIds(), ['10', '11'], 'папка: в колонке только её чаты');
eq(channelIds(), [], 'папка: канал вне выбора скрыт');
// Счётчик обязан считать по видимому срезу: иначе цифра ведёт в никуда.
eq(dirCount('inbox'), '(3)', 'папка: счётчик inbox считает только видимые (2+1)');
eq(dirCount('folder:7'), '(3)', 'папка: её собственный счётчик тот же');
eq(dirCount('archive'), '', 'папка: архив пуст — счётчика нет');
ok(dirDimmed('folder:8'), 'папка: пункт «Боты» помечен пустым');
ok(dirDimmed('archive'), 'папка: архив помечен пустым');
ok(!dirDimmed('inbox'), 'папка: inbox не пуст');
// Пункт остаётся в навигации — структуру аккаунта не прячем.
eq($dirList.children.length, 4, 'папка: все пункты навигации на месте');

// ---------------------------------------------------------------------------
// 3. Поимённый выбор
// ---------------------------------------------------------------------------

setup(true, [], ['13']);
eq(nodeIds(), ['13'], 'поимённо: виден только выбранный чат');
eq(dirCount('inbox'), '(3)', 'поимённо: счётчик по нему одному');

// Поимённый выбор складывается с папкой, а не заменяет её.
setup(true, ['folder:8'], ['13']);
eq(nodeIds(), ['12', '13'], 'смешанный выбор: бот из папки + поимённый чат');

// Выбранный архивный чат виден в архиве, но не в inbox.
setup(true, [], ['15']);
eq(nodeIds(), [], 'архивный выбор: в inbox его нет');
ok(dirDimmed('inbox'), 'архивный выбор: inbox помечен пустым');
ok(!dirDimmed('archive'), 'архивный выбор: архив не пуст');
setup(true, [], ['15'], 'archive');
eq(nodeIds(), ['15'], 'архивный выбор: виден при открытом архиве');

// ---------------------------------------------------------------------------
// 4. Служебные пункты в белом списке
// ---------------------------------------------------------------------------

setup(true, ['inbox'], []);
eq(nodeIds(), ['10', '11', '12', '13'], 'inbox в списке: все неархивные видны');
ok(dirDimmed('archive'), 'inbox в списке: архив по-прежнему скрыт');

setup(true, ['archive'], []);
eq(nodeIds(), [], 'archive в списке: в inbox пусто');
setup(true, ['archive'], [], 'archive');
eq(nodeIds(), ['15'], 'archive в списке: архивный чат виден');

// ---------------------------------------------------------------------------
// 5. Порядок фильтров и краевые случаи
// ---------------------------------------------------------------------------

// Скрытый чат не «всплывает» ни в какой папке: белый список действует во всех
// пунктах навигации, а не только в активном. Открываем папку «Боты», выбрав
// только папку «Работа», — её чаты в «Ботах» и так не состоят, но и скрытый
// бот (12) появиться не должен.
setup(true, ['folder:7'], [], 'folder:8');
eq(nodeIds(), [], 'скрытые чаты не появляются в чужой папке');

// Исчезнувшая папка в белом списке: совпадений не даёт, но и не роняет рендер.
setup(true, ['folder:99'], []);
eq(nodeIds(), [], 'исчезнувшая папка: колонка пуста');
eq($dirList.children.length, 4, 'исчезнувшая папка: навигация цела');

// Активная папка удалена — dialogs.js откатывается на inbox (прежнее поведение
// не сломано фильтром).
setup(true, ['inbox'], [], 'folder:404');
eq(tgState.activeDir, 'inbox', 'удалённая активная папка: откат на inbox');
eq(nodeIds(), ['10', '11', '12', '13'], 'удалённая активная папка: список нарисован');

// Смена настроек действует немедленно, без перезагрузки страницы.
setup(true, ['folder:7'], []);
eq(nodeIds(), ['10', '11'], 'применение: исходный выбор');
userSettings.telegram_visible_dialogs = ['13'];
invalidateVisibilityFilter();
dialogs.renderAll();
eq(nodeIds(), ['10', '11', '13'], 'применение: новый чат появился сразу после перерисовки');
userSettings.telegram_filter_enabled = false;
invalidateVisibilityFilter();
dialogs.renderAll();
eq(nodeIds(), ['10', '11', '12', '13'], 'применение: выключение вернуло всех');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/telegram_dialogs_filter)`);
if (failures > 0) process.exit(1);
