// ⚠ СТРАЖ КНОПКИ ВСТРЕЧИ (фронтовая Ф3 forge-meet, задачи ФФ3.2, ФФ3.4, ФФ3.5).
//
// Грузит РЕАЛЬНЫЙ модуль static/js/messenger/meet-row.js на крошечном фейковом
// DOM: панелей здесь нет вовсе — проверяется ровно то, что общее для трёх лент.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Кнопка держит четыре обещания, и каждое ломается
// ТИХО:
//
//   1. ПЛАГИН ВЫКЛЮЧЕН — НУЛЕВАЯ ЦЕНА. `attachMeetButton` обязана выйти ПЕРВОЙ
//      СТРОКОЙ: ни детекта, ни узла, ни очереди батча. Регрессия здесь не
//      видна глазами вовсе — она видна только в профайлере на ленте из 500
//      строк.
//   2. КЛИК НЕ ВСПЛЫВАЕТ. У лент делегированные обработчики (двойной клик по
//      строке начинает ответ). Без stopPropagation клик по кнопке заодно
//      выбирал бы цель ответа — и выглядело бы это как «панель живёт своей
//      жизнью».
//   3. АНТИ-МИГАНИЕ. Состояние читается СИНХРОННО из кэша прямо в билдере
//      строки. Начни оно читаться асинхронно — каждая перерисовка ленты давала
//      бы кадр с пустой кнопкой.
//   4. ЗАКРЕПЛЕНИЕ — АТРИБУТ НА СТРОКЕ. Не opacity на контейнере: `opacity`
//      создаёт группу композиции, и потомок физически не может быть
//      непрозрачнее предка. Поставь кто-нибудь закрепление на саму кнопку —
//      «идёт запись» перестанет быть видно без наведения, и никто не заметит,
//      пока не начнёт записывать встречу.
//
// Плюс таймер записи: он обязан считаться ВЫЧИТАНИЕМ от startedAt (а не
// инкрементом) и жить на ОДНОМ интервале — том, что уже есть в сторе.
//
// Запуск: node tmux-web/tests/frontend/messenger_meet_row.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM (ровно тот срез, которым пользуется meet-row.js)
// ---------------------------------------------------------------------------

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
        attributes: {},
        listeners: new Map(),
        className: '',
        hidden: false,
        title: '',
        type: '',
    };
    node.classList = {
        contains: (c) => node.className.split(/\s+/).filter(Boolean).includes(c),
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
    node.remove = () => detach(node);
    node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
    node.getAttribute = (k) => (k in node.attributes ? node.attributes[k] : null);
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.click = () => dispatch(node, 'click');
    node.querySelectorAll = (sel) => {
        const cls = /^\.([-\w]+)$/.exec(String(sel));
        const tag = /^([a-zA-Z]+)$/.exec(String(sel));
        const out = [];
        const walk = (n) => n.childNodes.forEach((c) => {
            if (c.nodeType !== 1) return;
            if (cls && c.classList.contains(cls[1])) out.push(c);
            if (tag && c.tagName === tag[1].toLowerCase()) out.push(c);
            walk(c);
        });
        walk(node);
        return out;
    };
    node.querySelector = (sel) => node.querySelectorAll(sel)[0] || null;
    return node;
}

function dispatch(node, type, ev) {
    const e = Object.assign({
        type,
        target: node,
        currentTarget: node,
        defaultPrevented: false,
        propagationStopped: false,
        preventDefault() { this.defaultPrevented = true; },
        stopPropagation() { this.propagationStopped = true; },
    }, ev || {});
    (node.listeners.get(type) || []).slice().forEach((fn) => fn(e));
    return e;
}

globalThis.document = { createElement: (t) => makeEl(t) };

// ---------------------------------------------------------------------------
// Модули и мини-фреймворк
// ---------------------------------------------------------------------------

const ROW = await import(new URL('../../static/js/messenger/meet-row.js', import.meta.url).href);
const STORE = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);
const CORE = await import(new URL('../../static/js/messenger/meet-core.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const MEET_URL = 'https://telemost.yandex.ru/j/60497752704486';

/** Сообщение Telegram со ссылкой на встречу. */
function msgWith(id, text) {
    return { id, dialog_id: '777000', text: text === undefined ? `созвон ${MEET_URL}` : text };
}

/** Строка ленты и контейнер действий — как их строят панели. */
function makeRow() {
    const row = makeEl('div');
    row.className = 'tg-line';
    const box = makeEl('span');
    box.className = 'tg-line-actions';
    row.appendChild(box);
    return { row, box };
}

/** Транспорт батча с журналом. */
function transport(answer) {
    const calls = [];
    return {
        calls,
        fn: async (q) => {
            calls.push(q);
            return answer ? answer(q) : { badges: [] };
        },
    };
}

function badge(messageId, status, extra) {
    return Object.assign({
        chat_id: '777000',
        message_id: messageId,
        meeting_id: `m-${messageId}`,
        status,
        summary_status: 'none',
        platform: 'telemost',
        started_at: null,
        ended_at: null,
        duration_ms: null,
    }, extra || {});
}

// ===========================================================================
// 1. ГЕЙТ: при выключенном плагине кнопки нет и цены нет
// ===========================================================================

STORE.resetMeetStore();
ROW.resetMeetRow();

{
    const { row, box } = makeRow();
    const built = ROW.attachMeetButton(row, { panel: 'telegram', msg: msgWith(7), chatId: '777000', container: box });
    eq(built, null, 'гейт: при выключенном плагине attachMeetButton возвращает null');
    eq(box.children.length, 0, 'гейт: узел не создан');
    eq(STORE._debugMeetStore().pending, 0, 'гейт: в очередь батча ничего не поставлено');
    eq(ROW._debugMeetRow().stats.built, 0, 'гейт: кнопка не строилась вообще');
    ok(!('meetPinned' in row.dataset), 'гейт: строка не помечена');
}

// ===========================================================================
// 2. КНОПКА: контракт разметки
// ===========================================================================

const net = transport(() => ({ badges: [] }));
STORE.configureMeetStore({ fetchStates: net.fn, batchDelayMs: 5 });

let opened = null;
ROW.configureMeetRow({ open: (d) => { opened = d; } });

const first = makeRow();
const btn = ROW.attachMeetButton(first.row, {
    panel: 'telegram', msg: msgWith(7), chatId: '777000', container: first.box, root: first.row,
});

ok(!!btn, 'кнопка: на сообщении со ссылкой она есть');
eq(btn.tagName, 'button', 'кнопка: это button — доступна с клавиатуры и табом');
eq(btn.type, 'button', 'кнопка: type=button, иначе внутри формы она бы её отправляла');
eq(btn.dataset.action, 'meet',
    'кнопка: data-action="meet" — по нему её находят клавиатура и e2e (у Битрикса это даёт хоткей даром)');
eq(btn.dataset.meetKey, STORE.meetKey('telegram', '777000', 7), 'кнопка: ключ строки ленты');
eq(btn.dataset.meetStatus, 'none', 'кнопка: статус «можно записать» — кэш ещё пуст');
eq(btn.dataset.meetLocal, 'telemost:60497752704486', 'кнопка: локальная метка комнаты для диагностики');
ok(btn.getAttribute('aria-label') && btn.title, 'кнопка: подписана и для мыши, и для скринридера');
ok(btn.title.includes('Телемост'), 'кнопка: подсказка называет платформу');
eq(btn.children.length, 2, 'кнопка: глиф и подпись — отдельные узлы (пульсирует только глиф)');
eq(btn.children[0].className, 'meet-glyph', 'кнопка: первый узел — глиф');
eq(btn.children[1].className, 'meet-label', 'кнопка: второй узел — подпись');
ok(btn.className.includes('meet-btn') && btn.className.includes('tg-meet-btn'),
    'кнопка: общий класс (вид) плюс панельный (видимость в своей палитре)');
ok(first.box.children[0] === btn, 'кнопка: положена в контейнер действий, а не в строку');
ok(!('meetPinned' in first.row.dataset),
    'кнопка: «можно записать» НЕ закреплено — иначе оно мозолило бы глаз на каждой ссылке');

// Сообщение без ссылки — узла нет.
{
    const plain = makeRow();
    eq(ROW.attachMeetButton(plain.row, { panel: 'telegram', msg: msgWith(8, 'просто текст'), chatId: '777000', container: plain.box }),
        null, 'кнопка: без ссылки на встречу узла нет');
}
// Неизвестная панель — узла нет (класс собрать не из чего).
{
    const alien = makeRow();
    eq(ROW.attachMeetButton(alien.row, { panel: 'slack', msg: msgWith(9), chatId: '777000', container: alien.box }),
        null, 'кнопка: у незнакомой панели узла нет');
}
// Промах кэша поставлен в очередь батча (N+1 не возникает: строк со ссылкой единицы).
ok(STORE._debugMeetStore().pending > 0 || net.calls.length > 0,
    'кнопка: промах кэша ушёл в очередь батча');

// ===========================================================================
// 3. КЛИК: preventDefault И stopPropagation — оба обязательны
// ===========================================================================

{
    const ev = dispatch(btn, 'click');
    eq(ev.defaultPrevented, true, 'клик: preventDefault вызван');
    eq(ev.propagationStopped, true,
        'клик: stopPropagation вызван — иначе клик по кнопке заодно выбрал бы цель ответа');
    ok(opened && opened.key === btn.dataset.meetKey, 'клик: обработчику передан ключ строки');
    eq(opened.panel, 'telegram', 'клик: и панель');
    eq(opened.messageId, 7, 'клик: и идентификатор сообщения');
    eq(opened.meet.url, MEET_URL, 'клик: и КАНОНИЧЕСКИЙ адрес встречи (именно он уедет на сервер)');
    ok(opened.row === first.row, 'клик: и сама строка');
}

// Свой обработчик у ctx перекрывает общий.
{
    let local = null;
    const own = makeRow();
    const b = ROW.attachMeetButton(own.row, {
        panel: 'vk', msg: { id: 11, dialog_id: '2000000001', text: MEET_URL }, chatId: '2000000001',
        container: own.box, onOpen: (d) => { local = d; },
    });
    ok(b.className.includes('vk-meet-btn'), 'панель VK: свой класс кнопки');
    dispatch(b, 'click');
    ok(local && local.panel === 'vk', 'клик: обработчик из ctx имеет приоритет над общим');
}

// ===========================================================================
// 4. АНТИ-МИГАНИЕ: прогретый кэш даёт тот же вид при пересборке строки
// ===========================================================================

await sleep(30);
STORE.applyBadges('telegram', '777000', [badge(7, 'recording', { started_at: 1700000000 })], [7]);

{
    const a = makeRow();
    const ba = ROW.attachMeetButton(a.row, { panel: 'telegram', msg: msgWith(7), chatId: '777000', container: a.box });
    const b = makeRow();
    const bb = ROW.attachMeetButton(b.row, { panel: 'telegram', msg: msgWith(7), chatId: '777000', container: b.box });
    eq(ba.children[0].textContent, bb.children[0].textContent,
        'анти-мигание: две сборки строки подряд дают ОДИН И ТОТ ЖЕ глиф');
    eq(ba.dataset.meetStatus, 'recording', 'анти-мигание: статус взят из кэша СИНХРОННО, а не следующим кадром');
    eq(ba.dataset.meetStatus, bb.dataset.meetStatus, 'анти-мигание: и статус тот же');
    eq(a.row.dataset.meetPinned, 'recording',
        'закрепление: идущая запись помечена НА СТРОКЕ (opacity контейнера закрепить кнопку не может)');
}

// ===========================================================================
// 5. ТАБЛИЦА СОСТОЯНИЙ: глиф, тон, закрепление, пульс
// ===========================================================================

{
    const seen = new Map();
    for (const status of Object.keys(ROW.MEET_VIEWS)) {
        const view = ROW.meetViewFor({ status, startedAtMs: null, summaryStatus: null }, 0);
        eq(view.status, status, `состояние «${status}»: вид найден по статусу`);
        ok(view.glyph && view.glyph.length <= 2, `состояние «${status}»: глиф ровно один символ`);
        ok(typeof view.title === 'string' && view.title.length > 0,
            `состояние «${status}»: у него есть человеческая подпись`);
        const key = `${view.glyph}|${view.label}|${view.tone}`;
        seen.set(key, (seen.get(key) || 0) + 1);
    }
    ok(seen.size >= 8, 'состояния: виды различимы (глиф+подпись+тон повторяются только у близких этапов)');

    eq(ROW.meetViewFor(null, 0).pinned, false,
        'закрепление: «можно записать» НЕ закреплено');
    for (const status of ['queued', 'recording', 'transcribing', 'ready', 'failed', 'cancelled']) {
        eq(ROW.meetViewFor({ status }, 0).pinned, true, `закрепление: «${status}» видно без наведения`);
    }
    eq(ROW.meetViewFor({ status: 'recording' }, 0).pulse, true, 'пульс: идущая запись пульсирует');
    eq(ROW.meetViewFor({ status: 'ready' }, 0).pulse, false, 'пульс: готовая запись не пульсирует');
    eq(ROW.meetViewFor({ status: 'none' }, 0).pulse, false, 'пульс: «можно записать» не пульсирует');

    // Саммари не получилось, а транскрипт есть: статус остаётся ready (на
    // бэкенде переход summarizing → failed запрещён намеренно), и молчать об
    // этом нельзя — человек ждёт саммари.
    const warn = ROW.meetViewFor({ status: 'ready', summaryStatus: 'failed' }, 0);
    eq(warn.tone, 'warn', 'саммари: провал виден тоном, хотя статус остался ready');
    ok(warn.title.includes('саммари'), 'саммари: и словами в подсказке');

    // Незнакомый статус (бэкенд ушёл вперёд) не роняет кнопку.
    const alien = ROW.meetViewFor({ status: 'teleporting' }, 0);
    eq(alien.status, 'teleporting', 'незнакомый статус: сохранён как есть');
    ok(alien.glyph, 'незнакомый статус: кнопка всё равно нарисована, а не сломана');
}

// ===========================================================================
// 6. ТАЙМЕР ЗАПИСИ: вычитание от startedAt, а не инкремент
// ===========================================================================

eq(ROW.formatMeetClock(0), '0:00', 'таймер: ноль');
eq(ROW.formatMeetClock(7 * 1000), '0:07', 'таймер: секунды с ведущим нулём');
eq(ROW.formatMeetClock(72 * 1000), '1:12', 'таймер: минуты без ведущего нуля — ширина на строке дорога');
eq(ROW.formatMeetClock(3672 * 1000), '1:01:12', 'таймер: после часа появляется час');
eq(ROW.formatMeetClock(-5), '0:00', 'таймер: отрицательное время (часы разъехались) — ноль, а не минус');

{
    const started = 1700000000000;
    const view = ROW.meetViewFor({ status: 'recording', startedAtMs: started }, started + 95 * 1000);
    eq(view.label, '1:35', 'таймер: подпись считается ВЫЧИТАНИЕМ от startedAt');
    ok(view.title.includes('1:35'), 'таймер: подсказка мыши несёт живую цифру');
    ok(!view.aria.includes('1:35'),
        'таймер: подпись для скринридера СТАБИЛЬНА — иначе фокус на кнопке превратился бы '
        + 'в посекундное бормотание');

    // «Перезагрузка вкладки»: строка собирается заново через час после старта.
    // Инкрементный счётчик показал бы 0:00 — и это ровно та ложь, ради
    // которой таймер и считается вычитанием.
    let clock = started + 3600 * 1000;
    ROW.configureMeetRow({ now: () => clock });
    STORE.applyBadges('telegram', '777000', [badge(7, 'recording', { started_at: started / 1000 })], [7]);
    const fresh = makeRow();
    const fb = ROW.attachMeetButton(fresh.row, { panel: 'telegram', msg: msgWith(7), chatId: '777000', container: fresh.box });
    eq(fb.children[1].textContent, '1:00:00',
        'таймер: после пересборки ленты показано время ОТ СТАРТА, а не с нуля');
}

// ===========================================================================
// 7. ПЕРЕРИСОВКА: одна подписка на модуль, один интервал на документ
// ===========================================================================

{
    STORE.resetMeetStore();
    ROW.resetMeetRow();

    // Подменяем таймеры стора: живой интервал обязан быть ОДИН и обязан
    // сниматься, когда последняя запись ушла в терминальный статус.
    const intervals = [];
    let ticker = null;
    STORE.configureMeetStore({
        fetchStates: transport().fn,
        batchDelayMs: 5,
        timers: {
            setInterval: (fn) => { ticker = fn; intervals.push(fn); return intervals.length; },
            clearInterval: () => { ticker = null; },
            now: () => 1700000000000 + 130 * 1000,
        },
    });
    let clock = 1700000000000 + 130 * 1000;
    ROW.configureMeetRow({ now: () => clock });

    const log = makeEl('div');
    const rows = [];
    for (let i = 0; i < 5; i += 1) {
        const r = makeRow();
        log.appendChild(r.row);
        rows.push(r);
        ROW.attachMeetButton(r.row, {
            panel: 'telegram', msg: msgWith(100 + i), chatId: '777000', container: r.box, root: log,
        });
    }
    eq(ROW._debugMeetRow().roots, 1, 'перерисовка: зарегистрирована ОДНА лента, а не пять строк');
    ok(ROW._debugMeetRow().subscribed, 'перерисовка: подписка на стор заведена лениво — при первой ленте');

    // Многократная пересборка ленты: ни подписок, ни лент не прибавляется.
    for (let round = 0; round < 100; round += 1) {
        const r = makeRow();
        log.appendChild(r.row);
        ROW.attachMeetButton(r.row, {
            panel: 'telegram', msg: msgWith(100), chatId: '777000', container: r.box, root: log,
        });
        r.row.remove();
    }
    eq(ROW._debugMeetRow().roots, 1, 'утечки нет: сто пересборок ленты не добавили ни одной ленты');
    eq(ROW._debugMeetRow().subscribed, true, 'утечки нет: подписка на месте');
    // Считаем подписчиков В САМОМ СТОРЕ, а не флаг в модуле: перезаписанная
    // ссылка на отписку выглядела бы как одна подписка, а на деле их стало бы
    // сто одна — и каждая перерисовывала бы ленту целиком на каждый тик.
    eq(STORE._debugMeetStore().subscribers, 1,
        'утечки нет: у стора РОВНО ОДИН подписчик от кнопок, сколько бы раз лента ни пересобиралась');
    eq(intervals.length, 0, 'таймер: без активных встреч интервал не заводится вовсе');

    // Пошла запись — интервал появился РОВНО ОДИН на все пять строк.
    STORE.applyMeetSnapshot({
        meetings: [{
            id: 'm-100', status: 'recording', summary_status: 'none', platform: 'telemost',
            started_at: 1700000000, ended_at: null, duration_ms: null,
        }],
        deps: null,
        limits: null,
    });
    STORE.applyBadges('telegram', '777000', [
        badge(100, 'recording', { meeting_id: 'm-100', started_at: 1700000000 }),
        badge(101, 'recording', { meeting_id: 'm-100', started_at: 1700000000 }),
    ], [100, 101]);
    eq(intervals.length, 1, 'таймер: интервал ОДИН на документ, а не по одному на кнопку');
    eq(STORE._debugMeetStore().ticking, true, 'таймер: тикает, пока идёт запись');

    eq(rows[0].box.children[0].children[1].textContent, '2:10',
        'таймер: кадр states перерисовал подпись без пересборки строки');

    // Тик двигает цифру на всех кнопках сразу.
    clock += 5000;
    ticker();
    eq(rows[0].box.children[0].children[1].textContent, '2:15', 'таймер: тик обновил подпись');
    eq(rows[1].box.children[0].children[1].textContent, '2:15', 'таймер: и у соседней строки');

    // Запись закончилась — интервал снят.
    STORE.applyMeetingUpdate({
        id: 'm-100', status: 'ready', summary_status: 'ok', platform: 'telemost',
        started_at: 1700000000, ended_at: 1700000200, duration_ms: 200000,
    });
    eq(STORE._debugMeetStore().ticking, false,
        'таймер: последняя живая встреча ушла — интервал снят, страница снова не тикает');
    eq(rows[0].box.children[0].dataset.meetStatus, 'ready', 'кадр meeting_update перекрасил кнопку');
    eq(rows[0].row.dataset.meetPinned, 'ready', 'готовая запись закреплена — она видна без наведения');
}

// ===========================================================================
// 8. ХОТКЕЙ: поиск кнопки по data-action, а не по классу
// ===========================================================================

{
    const r = makeRow();
    const b = ROW.attachMeetButton(r.row, {
        panel: 'telegram', msg: msgWith(7), chatId: '777000', container: r.box,
    });
    ok(ROW.meetButtonOf(r.row) === b, 'хоткей: кнопка находится по data-action — тем же способом, что у Битрикса');
    let pressed = 0;
    ROW.configureMeetRow({ open: () => { pressed += 1; } });
    eq(ROW.pressMeetButton(r.row), true, 'хоткей: нажатие выполнено');
    eq(pressed, 1, 'хоткей: обработчик вызван ровно один раз');

    const empty = makeRow();
    eq(ROW.pressMeetButton(empty.row), false,
        'хоткей: на строке без встречи возвращается false — событие не поглощается');
    eq(ROW.meetButtonOf(null), null, 'хоткей: без строки — null, а не исключение');
}

// ===========================================================================
// 8.5. ПЛОЩАДКА БЕЗ ЗВУКА: КНОПКИ НЕТ ВОВСЕ
// ===========================================================================
//
// `audio: false` в реестре — это измеренный факт «звук отсюда мы забрать не
// умеем» (кандидат — Zoom с его WASM-пайплайном), а не «пока не проверяли».
// Нарисованная кнопка обещала бы запись, которая гарантированно вышла бы
// пустой, и человек узнал бы об этом через полчаса после встречи. Молчаливое
// отсутствие кнопки честнее невыполнимого обещания.
{
    const ZOOM_URL = 'https://us02web.zoom.us/j/83123456789?pwd=x';
    const zoomMsg = (id) => ({ id, dialog_id: '777000', text: `созвон ${ZOOM_URL}` });

    // Сначала — со звуком: кнопка обязана быть, иначе тест ниже зеленел бы от
    // любой другой причины (не тот хост, не та панель, закрытый гейт).
    const withAudio = makeRow();
    ok(ROW.attachMeetButton(withAudio.row, {
        panel: 'telegram', msg: zoomMsg(41), chatId: '777000', container: withAudio.box,
    }) !== null, 'со звуком кнопка для Zoom рисуется');

    CORE.configureMeetPlatforms([{
        platform: 'zoom',
        title: 'Zoom',
        hosts: ['zoom.us'],
        host_suffixes: ['.zoom.us'],
        paths: ['/j/(\\d{8,15})'],
        keep_params: ['pwd'],
        key_case: 'preserve',
        audio: false,
    }]);
    const noAudio = makeRow();
    eq(ROW.attachMeetButton(noAudio.row, {
        panel: 'telegram', msg: zoomMsg(42), chatId: '777000', container: noAudio.box,
    }), null, 'без звука кнопка не рисуется');
    eq(noAudio.box.children.length, 0, 'в контейнере действий не осталось узла');

    CORE.resetMeetPlatforms();
    const back = makeRow();
    ok(ROW.attachMeetButton(back.row, {
        panel: 'telegram', msg: zoomMsg(43), chatId: '777000', container: back.box,
    }) !== null, 'сброс реестра возвращает кнопку');
}

// ===========================================================================
// 9. ВЫКЛЮЧЕННЫЙ ПЛАГИН ПОСЛЕ СБРОСА: ни узла, ни подписки
// ===========================================================================

STORE.resetMeetStore();
ROW.resetMeetRow();
{
    const r = makeRow();
    eq(ROW.attachMeetButton(r.row, { panel: 'telegram', msg: msgWith(7), chatId: '777000', container: r.box }),
        null, 'после сброса гейт снова закрыт');
    eq(ROW._debugMeetRow().subscribed, false, 'после сброса подписка снята — она не переживает выгрузку страницы');
}

// ===========================================================================

if (failures === 0) console.log(`[OK] messenger_meet_row: ${checks} проверок`);
else console.error(`[FAIL] messenger_meet_row: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
