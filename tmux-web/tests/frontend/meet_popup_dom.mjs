// ВСПОМОГАТЕЛЬНЫЙ МОДУЛЬ (не тест): фейковый документ для тестов карточки
// встречи (`js/messenger/meet-popup.js`).
//
// Зачем отдельный файл: карточку проверяют ТРИ теста (каркас, вкладки, плеер), и
// три копии одного стаба разъехались бы на первой же правке — ровно поэтому в
// проекте уже живут `telegram_dom_stub.mjs` и `vk_dom_stub.mjs`.
//
// Отличие от панельных стабов: здесь НЕТ заранее объявленных узлов с id из
// `index.html`. Карточка строит свою разметку сама (в разметке приложения её
// нет вовсе), поэтому стабу достаточно уметь создавать узлы, доставлять события
// и изображать кадр отрисовки.
//
// Реализован ровно тот срез DOM, которым пользуется модуль:
//   • createElement / createTextNode / createDocumentFragment (фрагмент нужен
//     рендереру markdown — он отдаёт именно фрагмент, и стаб его РАСТВОРЯЕТ при
//     вставке, как это делает браузер);
//   • className / classList / dataset / hidden / value / disabled / src;
//   • addEventListener + removeEventListener (снятие НАСТОЯЩЕЕ: карточка
//     снимает слушатель Esc в teardown, и заглушка превратила бы проверку в
//     проверку пустоты);
//   • querySelector/querySelectorAll по `тег` и `.класс`;
//   • геометрия прокрутки (scrollTop/scrollHeight/clientHeight) — по ней
//     принимается решение о следовании за расшифровкой.

// ---------------------------------------------------------------------------
// Узлы
// ---------------------------------------------------------------------------

export function makeText(data) {
    return { nodeType: 3, tagName: null, parentNode: null, textContent: String(data) };
}

function detach(node) {
    const p = node.parentNode;
    if (!p) return;
    const i = p.childNodes.indexOf(node);
    if (i >= 0) p.childNodes.splice(i, 1);
    node.parentNode = null;
}

function matchesSel(node, sel) {
    if (!node || node.nodeType !== 1) return false;
    // Цифра в имени тега обязательна: рендерер markdown создаёт `h1`…`h6`, и
    // селектор без цифр молча не находил бы ни одного заголовка.
    const m = String(sel).trim().match(/^([a-zA-Z][a-zA-Z0-9]*)?((?:\.[-\w]+)*)$/);
    if (!m) return false;
    const [, tag, classes] = m;
    if (tag && node.tagName !== tag.toLowerCase()) return false;
    for (const c of (classes || '').split('.').filter(Boolean)) {
        if (!node.classList.contains(c)) return false;
    }
    return true;
}

export function makeEl(tag) {
    const node = {
        nodeType: 1,
        tagName: String(tag || 'div').toLowerCase(),
        childNodes: [],
        parentNode: null,
        dataset: {},
        attributes: {},
        listeners: new Map(),
        // У настоящего элемента `style` есть ВСЕГДА; код прода поэтому пишет
        // `el.style.width = …` без проверок, и без этого поля такая строка
        // роняет тест на ровном месте.
        style: {},
        className: '',
        hidden: false,
        title: '',
        type: '',
        value: '',
        disabled: false,
        src: '',
        alt: '',
        preload: '',
        controls: false,
        placeholder: '',
        id: '',
        pauseCount: 0,
        scrollTop: 0,
        scrollHeight: 0,
        clientHeight: 0,
    };
    node.classList = {
        contains: (c) => node.className.split(/\s+/).filter(Boolean).includes(c),
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
    // `firstChild`/`lastChild` — не украшение: очистка узла в проде пишется как
    // `while (n.firstChild) n.removeChild(n.firstChild)` (единственный способ
    // опустошить контейнер без innerHTML), и без этих геттеров такой цикл в
    // тестах молча не делает НИЧЕГО — узлы копятся, а проверки зеленеют.
    Object.defineProperty(node, 'firstChild', {
        get: () => node.childNodes[0] || null,
    });
    Object.defineProperty(node, 'lastChild', {
        get: () => node.childNodes[node.childNodes.length - 1] || null,
    });
    node.appendChild = (child) => {
        // Фрагмент (nodeType 11) РАСТВОРЯЕТСЯ: в дерево уходят его дети, а не
        // он сам. Без этого правила отрисованный markdown оказался бы завёрнут
        // в лишний узел, которого в браузере нет.
        if (child && child.nodeType === 11) {
            child.childNodes.slice().forEach((grand) => {
                detach(grand);
                grand.parentNode = node;
                node.childNodes.push(grand);
            });
            return child;
        }
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
    node.matches = (sel) => matchesSel(node, sel);
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
    // Снятие атрибута `src` обязано обнулять и СВОЙСТВО: в браузере
    // `audio.src` — отражение атрибута, и после `removeAttribute('src')` оно
    // отдаёт пустую строку. Без этой связи тест плеера зеленел бы на коде,
    // который источник не снимает.
    node.removeAttribute = (k) => {
        delete node.attributes[k];
        if (k === 'src') node.src = '';
    };
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    // Снятие НАСТОЯЩЕЕ — см. шапку.
    node.removeEventListener = (type, fn) => {
        const arr = node.listeners.get(type) || [];
        const i = arr.indexOf(fn);
        if (i >= 0) arr.splice(i, 1);
    };
    node.click = () => dispatch(node, 'click', { target: node });
    node.focus = () => {};
    node.pause = () => { node.pauseCount += 1; };
    node.scrollIntoView = () => {};
    return node;
}

/**
 * Доставка события ОДНОМУ узлу.
 *
 * Всплытие не эмулируется: и карточка, и панели вешают слушателей на конкретные
 * узлы и на документ, а решают всё по `e.target`.
 *
 * ⚠ СЕМАНТИКА ОСТАНОВОК — КАК В БРАУЗЕРЕ, и это принципиально (forge-o0sdg):
 * вызываются ВСЕ слушатели узла, и `stopPropagation()` их НЕ отменяет — он
 * останавливает всплытие выше по дереву, которого здесь и нет. Отменяет только
 * `stopImmediatePropagation()`. Дефект «одно нажатие Esc закрывает два окна»
 * пережил тесты ровно потому, что в код верили на слово: два модуля вешали по
 * слушателю на один документ и оба звали `stopPropagation()`, ожидая от него
 * чужого поведения.
 */
export function dispatch(node, type, ev) {
    const e = Object.assign({
        type,
        target: node,
        currentTarget: node,
        defaultPrevented: false,
        propagationStopped: false,
        immediateStopped: false,
        preventDefault() { this.defaultPrevented = true; },
        stopPropagation() { this.propagationStopped = true; },
        stopImmediatePropagation() {
            this.propagationStopped = true;
            this.immediateStopped = true;
        },
    }, ev || {});
    for (const fn of (node.listeners.get(type) || []).slice()) {
        if (e.immediateStopped) break;
        fn(e);
    }
    return e;
}

// ---------------------------------------------------------------------------
// Документ
// ---------------------------------------------------------------------------

/**
 * Фейковый документ + панели.
 *
 * @param {Array<string>} [panels] — какие панели завести (`telegram` и т.п.)
 * @returns {{doc: object, host: object, hosts: Map<string, object>,
 *   frames: Array<Function>, runFrames: Function, keydown: Function}}
 */
export function makeDoc(panels) {
    const ids = new Map();
    const doc = makeEl('#document');
    doc.createElement = (tag) => makeEl(tag);
    doc.createTextNode = (t) => makeText(t);
    doc.createDocumentFragment = () => {
        const frag = makeEl('#fragment');
        frag.nodeType = 11;
        return frag;
    };
    doc.getElementById = (id) => ids.get(String(id)) || null;

    const hosts = new Map();
    for (const name of (panels && panels.length ? panels : ['telegram'])) {
        const host = makeEl('div');
        host.id = name;
        host.attributes.id = name;
        ids.set(name, host);
        hosts.set(name, host);
        doc.appendChild(host);
    }

    // Кадр отрисовки: карточка сливает им буфер дельт. Кадры НЕ выполняются
    // сами — тест зовёт `runFrames()`, и тем самым видит, сколько раз буфер
    // сливался на самом деле.
    const frames = [];
    const raf = (fn) => {
        frames.push(fn);
        return frames.length;
    };
    const caf = (id) => { frames[id - 1] = null; };
    const runFrames = () => {
        const batch = frames.splice(0, frames.length);
        for (const fn of batch) if (fn) fn(0);
        return batch.filter(Boolean).length;
    };

    return {
        doc,
        hosts,
        host: hosts.get((panels && panels[0]) || 'telegram'),
        frames,
        raf,
        caf,
        runFrames,
        /** Нажатие клавиши в документе (Esc и прочее). */
        keydown: (key, ev) => dispatch(doc, 'keydown', Object.assign({ key }, ev || {})),
        /** Сколько слушателей типа висит на документе — для проверки teardown. */
        docListeners: (type) => (doc.listeners.get(type) || []).length,
    };
}

/** Счётчик таймеров: карточка не имеет права заводить свои интервалы. */
export function makeTimers(now) {
    const stats = { setTimeout: 0, clearTimeout: 0, setInterval: 0, clearInterval: 0 };
    const pending = new Map();
    let seq = 0;
    let clock = Number.isFinite(now) ? now : 1700000000000;
    return {
        stats,
        pending,
        advance: (ms) => { clock += ms; },
        set: (ms) => { clock = ms; },
        api: {
            setTimeout: (fn, ms) => {
                stats.setTimeout += 1;
                seq += 1;
                pending.set(seq, { fn, ms });
                return seq;
            },
            clearTimeout: (id) => { stats.clearTimeout += 1; pending.delete(id); },
            setInterval: () => { stats.setInterval += 1; return 0; },
            clearInterval: () => { stats.clearInterval += 1; },
            now: () => clock,
        },
        /** Выполнить все отложенные таймеры (резервный путь без кадров). */
        run: () => {
            const batch = Array.from(pending.entries());
            pending.clear();
            for (const [, t] of batch) t.fn();
            return batch.length;
        },
    };
}
