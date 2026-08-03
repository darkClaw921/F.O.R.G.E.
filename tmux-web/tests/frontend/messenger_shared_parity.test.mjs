// СТРАЖ ДУБЛИРОВАНИЯ: движок комбинаций существует в ДВУХ копиях, и этот тест
// следит, чтобы они не разъехались.
//
// Копии:
//   • `static/js/telegram/keys.js`          — оригинал (панель Telegram);
//   • `static/js/messenger/hotkeys-core.js` — общий слой (панель VK и все
//                                             последующие).
//
// Telegram на общий слой НЕ мигрирован сознательно: панель работает, её правка
// ради красоты графа импортов — чистый риск без выгоды для пользователя.
// Платой за это решение и является настоящий тест: правка одной копии без
// второй роняет его, а не создаёт молчаливое расхождение вида «у меня в
// Telegram Ctrl+Shift+X работает, а в VK нет» — жалобу, которую невозможно
// диагностировать по симптому.
//
// Что сравнивается: ВЕСЬ публичный пересечённый API на таблице входов —
// keyFromEvent, comboFromEvent, normalizeCombo, formatCombo, isEditableTarget,
// comboAllowedIn (чистые), а также defaultBindings, sanitizeOverrides,
// resolveBindings, bindingRows, overridesFromBindings, groupedRows,
// actionForEvent (реестро-зависимые — ядру реестр TG_ACTIONS передаётся
// аргументом).
//
// В таблице обязательно есть РУССКАЯ РАСКЛАДКА (`code: 'KeyJ', key: 'о'`):
// сопоставление идёт по `event.code`, и подмена его на `event.key` обязана
// покраснеть здесь же, а не у пользователя.
//
// Когда Telegram будет мигрирован на общий слой, `telegram/keys.js` станет
// re-export-шимом — тест продолжит работать и станет тривиально зелёным.
// Удалять его тогда же и незачем, и не вредно.
//
// Запуск: node tmux-web/tests/frontend/messenger_shared_parity.test.mjs
// Код возврата 0 — расхождений нет, 1 — есть [FAIL].

const tgUrl = new URL('../../static/js/telegram/keys.js', import.meta.url).href;
const coreUrl = new URL('../../static/js/messenger/hotkeys-core.js', import.meta.url).href;

const tg = await import(tgUrl);
const core = await import(coreUrl);

const ACTIONS = tg.TG_ACTIONS;
const GROUPS = tg.TG_ACTION_GROUPS;

// ---------------------------------------------------------------------------
// Мини-фреймворк
// ---------------------------------------------------------------------------

let failures = 0;
let checks = 0;

/** Приводит Map/Set к сравнимому виду (JSON не умеет их сериализовать). */
function plain(v) {
    if (v instanceof Map) return { __map: Array.from(v.entries()) };
    if (v instanceof Set) return { __set: Array.from(v.values()) };
    if (Array.isArray(v)) return v.map(plain);
    if (v && typeof v === 'object') {
        const out = {};
        for (const k of Object.keys(v).sort()) out[k] = plain(v[k]);
        return out;
    }
    return v;
}

/** Обе копии обязаны дать один результат. */
function same(label, a, b) {
    checks += 1;
    const sa = JSON.stringify(plain(a));
    const sb = JSON.stringify(plain(b));
    if (sa !== sb) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       telegram/keys.js:          ${sa}\n       messenger/hotkeys-core.js: ${sb}`);
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
// 0. Обе копии на месте и экспортируют одни имена
// ---------------------------------------------------------------------------

const SHARED_NAMES = [
    'keyFromEvent', 'comboFromEvent', 'normalizeCombo', 'formatCombo',
    'defaultBindings', 'sanitizeOverrides', 'resolveBindings', 'bindingRows',
    'overridesFromBindings', 'groupedRows', 'isEditableTarget', 'comboAllowedIn',
    'actionForEvent', 'findAction',
];
for (const name of SHARED_NAMES) {
    ok(typeof tg[name] === 'function', `telegram/keys.js экспортирует ${name}`);
    ok(typeof core[name] === 'function', `messenger/hotkeys-core.js экспортирует ${name}`);
}

// ---------------------------------------------------------------------------
// 1. keyFromEvent / comboFromEvent — таблица событий
// ---------------------------------------------------------------------------

/** Событие клавиатуры в том виде, в каком его видят обе копии. */
function ev(code, key, mods) {
    return {
        code,
        key,
        ctrlKey: !!(mods && mods.ctrl),
        altKey: !!(mods && mods.alt),
        shiftKey: !!(mods && mods.shift),
        metaKey: !!(mods && mods.meta),
    };
}

const EVENTS = [
    // ── русская раскладка: код физической клавиши, key — кириллица ──
    ['RU j', ev('KeyJ', 'о')],
    ['RU k', ev('KeyK', 'л')],
    ['RU d', ev('KeyD', 'в')],
    ['RU r', ev('KeyR', 'к')],
    ['RU Shift+g', ev('KeyG', 'П', { shift: true })],
    ['RU Ctrl+Shift+x', ev('KeyX', 'ч', { ctrl: true, shift: true })],
    // ── латиница ──
    ['EN j', ev('KeyJ', 'j')],
    ['EN Shift+G', ev('KeyG', 'G', { shift: true })],
    ['EN Ctrl+Enter', ev('Enter', 'Enter', { ctrl: true })],
    ['EN Meta+k', ev('KeyK', 'k', { meta: true })],
    ['EN Alt+k', ev('KeyK', '˚', { alt: true })],
    ['EN все модификаторы', ev('KeyZ', 'z', { ctrl: true, alt: true, shift: true, meta: true })],
    // ── цифры и функциональные ──
    ['Digit1', ev('Digit1', '1')],
    ['Shift+Digit1', ev('Digit1', '!', { shift: true })],
    ['Numpad5', ev('Numpad5', '5')],
    ['NumpadEnter', ev('NumpadEnter', 'Enter')],
    ['F5', ev('F5', 'F5')],
    ['F13', ev('F13', 'F13')],
    ['F25 (вне диапазона)', ev('F25', 'F25')],
    // ── пунктуация ──
    ['BracketLeft', ev('BracketLeft', '[')],
    ['BracketRight', ev('BracketRight', ']')],
    ['Shift+Slash', ev('Slash', '?', { shift: true })],
    ['Backslash', ev('Backslash', '\\')],
    ['Semicolon', ev('Semicolon', ';')],
    ['Quote', ev('Quote', "'")],
    ['Comma', ev('Comma', ',')],
    ['Period', ev('Period', '.')],
    ['Minus', ev('Minus', '-')],
    ['Equal', ev('Equal', '=')],
    ['Backquote', ev('Backquote', '`')],
    // ── навигационные ──
    ['Escape', ev('Escape', 'Escape')],
    ['Tab', ev('Tab', 'Tab')],
    ['Space', ev('Space', ' ')],
    ['Backspace', ev('Backspace', 'Backspace')],
    ['Delete', ev('Delete', 'Delete')],
    ['Home', ev('Home', 'Home')],
    ['End', ev('End', 'End')],
    ['PageUp', ev('PageUp', 'PageUp')],
    ['PageDown', ev('PageDown', 'PageDown')],
    ['ArrowUp', ev('ArrowUp', 'ArrowUp')],
    ['ArrowDown', ev('ArrowDown', 'ArrowDown')],
    ['ArrowLeft', ev('ArrowLeft', 'ArrowLeft')],
    ['ArrowRight', ev('ArrowRight', 'ArrowRight')],
    // ── одни модификаторы: комбинацией не являются ──
    ['только Shift', ev('ShiftLeft', 'Shift', { shift: true })],
    ['только Control', ev('ControlLeft', 'Control', { ctrl: true })],
    ['только Meta', ev('MetaLeft', 'Meta', { meta: true })],
    ['CapsLock', ev('CapsLock', 'CapsLock')],
    ['AltGraph', ev('AltRight', 'AltGraph', { alt: true })],
    // ── фолбэк на key: экранные клавиатуры кода не дают ──
    ['без code: буква', ev('', 'q')],
    ['без code: заглавная', ev('', 'Q', { shift: true })],
    ['без code: символ с Shift', ev('', '?', { shift: true })],
    ['без code: пробел', ev('', ' ')],
    ['без code: Enter', ev('', 'Enter')],
    ['без code: ArrowUp', ev('', 'ArrowUp')],
    ['без code: неизвестное длинное', ev('', 'BrightnessUp')],
    ['без code и без key', ev('', '')],
    ['неизвестный code', ev('Lang1', '')],
    // ── мусор ──
    ['null', null],
    ['undefined', undefined],
    ['пустой объект', {}],
    ['code не строка', { code: 42, key: 7 }],
];

for (const [label, e] of EVENTS) {
    same(`keyFromEvent: ${label}`, tg.keyFromEvent(e), core.keyFromEvent(e));
    same(`comboFromEvent: ${label}`, tg.comboFromEvent(e), core.comboFromEvent(e));
}

// ---------------------------------------------------------------------------
// 2. normalizeCombo / formatCombo / comboAllowedIn — таблица строк
// ---------------------------------------------------------------------------

const COMBOS = [
    'j', 'J', 'k', 'Enter', 'Escape', 'escape', 'ESCAPE', 'Space', 'space', 'Tab',
    'Ctrl+j', 'ctrl+J', 'CTRL+j', 'control+j', 'Shift+g', 'shift+G',
    'Shift+Ctrl+x', 'Ctrl+Shift+x', 'x+Shift+Ctrl', 'Meta+k', 'cmd+k', 'Command+K',
    'super+a', 'win+a', 'option+b', 'opt+b', 'alt+b', 'Ctrl+Alt+Shift+Meta+z',
    'Shift+/', '?', '!', '@', '{', '}', ':', '"', '<', '>', '|', '~', '_', '+',
    '++', '+++', '', '   ', 'a+b', 'Ctrl+', 'Ctrl', 'Shift+Alt', 'Hyper+j',
    'foo', 'ArrowLeft', 'arrowleft', 'ARROWLEFT', 'pageup', 'PageDown',
    'f5', 'F5', 'f13', 'F24', 'F25', 'f0', 'Ctrl+ArrowUp', 'Alt+Home',
    'backspace', 'Delete', 'home', 'end', ' j ', 'Ctrl + j',
];

for (const raw of COMBOS) {
    same(`normalizeCombo: ${JSON.stringify(raw)}`, tg.normalizeCombo(raw), core.normalizeCombo(raw));
    same(`formatCombo: ${JSON.stringify(raw)}`, tg.formatCombo(raw), core.formatCombo(raw));
    for (const editing of [false, true]) {
        same(
            `comboAllowedIn(${JSON.stringify(raw)}, editing=${editing})`,
            tg.comboAllowedIn(raw, { editing }),
            core.comboAllowedIn(raw, { editing }),
        );
    }
}

// Не-строки: обе копии обязаны одинаково не падать.
for (const junk of [null, undefined, 42, {}, [], true]) {
    same(`normalizeCombo(мусор ${JSON.stringify(junk)})`, tg.normalizeCombo(junk), core.normalizeCombo(junk));
    same(`formatCombo(мусор ${JSON.stringify(junk)})`, tg.formatCombo(junk), core.formatCombo(junk));
}
// Контекст без поля editing и вовсе без контекста.
same('comboAllowedIn: без ctx', tg.comboAllowedIn('j'), core.comboAllowedIn('j'));
same('comboAllowedIn: ctx=null', tg.comboAllowedIn('j', null), core.comboAllowedIn('j', null));

// ---------------------------------------------------------------------------
// 3. isEditableTarget
// ---------------------------------------------------------------------------

function fakeEl(tag, opts) {
    const classes = (opts && opts.classes) || [];
    return {
        tagName: tag,
        isContentEditable: !!(opts && opts.editable),
        classList: { contains: (c) => classes.includes(c) },
    };
}

const TARGETS = [
    ['input', fakeEl('INPUT')],
    ['textarea', fakeEl('TEXTAREA')],
    ['select', fakeEl('SELECT')],
    ['div', fakeEl('DIV')],
    ['button', fakeEl('BUTTON')],
    ['contenteditable div', fakeEl('DIV', { editable: true })],
    ['xterm helper', fakeEl('TEXTAREA', { classes: ['xterm-helper-textarea'] })],
    ['div с чужим классом', fakeEl('DIV', { classes: ['vk-line'] })],
    ['без tagName', { classList: { contains: () => false } }],
    ['null', null],
];
for (const [label, el] of TARGETS) {
    same(`isEditableTarget: ${label}`, tg.isEditableTarget(el), core.isEditableTarget(el));
}

// ---------------------------------------------------------------------------
// 4. Реестро-зависимые функции: ядру передаётся TG_ACTIONS
// ---------------------------------------------------------------------------

/** Карты переопределений, включая конфликтные и заведомо мусорные. */
const OVERRIDE_SETS = [
    ['пустая', {}],
    ['null', null],
    ['undefined', undefined],
    ['не объект', 'j'],
    ['одно переопределение', { 'msg.reply': 'Shift+R' }],
    ['несколько', { 'msg.reply': 'Shift+R', 'focus.input': 'Ctrl+i', 'nav.next': 'ArrowDown' }],
    ['дефолт записан явно', { 'msg.reply': 'r' }],
    ['дефолт в другом написании', { 'msg.reply': 'R' }],
    // Переопределение забирает клавишу у чужого ДЕФОЛТА: проигрывает дефолт,
    // а не переопределение — это и есть «два прохода».
    ['конфликт с дефолтом соседа', { 'msg.copy': 'j' }],
    ['конфликт с дефолтом раннего действия', { 'ui.theme': 'd' }],
    // Два переопределения на одну клавишу: выигрывает то, что раньше в реестре.
    ['конфликт двух переопределений', { 'msg.copy': 'Ctrl+q', 'msg.quote': 'Ctrl+q' }],
    ['цепочка обменов', { 'nav.next': 'k', 'nav.prev': 'j' }],
    ['fixed пытаются переназначить', { 'ui.escape': 'Ctrl+e' }],
    ['неизвестный id', { 'nope.nope': 'q' }],
    ['мусорная комбинация', { 'msg.copy': '+++' }],
    ['пустая комбинация', { 'msg.copy': '' }],
    ['комбинация не строка', { 'msg.copy': 42 }],
    ['смесь всего', {
        'msg.reply': 'Shift+R',
        'ui.escape': 'x',
        'nope.nope': 'q',
        'msg.copy': 'j',
        'log.older': '+++',
    }],
    ['Map вместо объекта', new Map([['msg.reply', 'Shift+R']])],
];

for (const [label, ov] of OVERRIDE_SETS) {
    same(`resolveBindings: ${label}`, tg.resolveBindings(ov), core.resolveBindings(ACTIONS, ov));
    same(`bindingRows: ${label}`, tg.bindingRows(ov), core.bindingRows(ACTIONS, ov));
    same(`groupedRows: ${label}`, tg.groupedRows(ov), core.groupedRows(ACTIONS, GROUPS, ov));
    same(
        `overridesFromBindings: ${label}`,
        tg.overridesFromBindings(ov && typeof ov === 'object' ? ov : {}),
        core.overridesFromBindings(ACTIONS, ov && typeof ov === 'object' ? ov : {}),
    );
}

same('defaultBindings', tg.defaultBindings(), core.defaultBindings(ACTIONS));

for (const id of ['msg.reply', 'ui.escape', 'nope.nope', '', 'NAV.NEXT']) {
    same(`findAction: ${JSON.stringify(id)}`, tg.findAction(id), core.findAction(ACTIONS, id));
}

// sanitizeOverrides — отдельно, потому что именно он отсекает чужие id и fixed.
for (const [label, ov] of OVERRIDE_SETS) {
    same(`sanitizeOverrides: ${label}`, tg.sanitizeOverrides(ov), core.sanitizeOverrides(ov, ACTIONS));
}

// ---------------------------------------------------------------------------
// 5. actionForEvent — на РАЗБОРЕ КАЖДОЙ КОПИИ, а не на общем
// ---------------------------------------------------------------------------
//
// Сравнивать надо сквозной путь «событие → id действия», иначе расхождение в
// resolveBindings, скомпенсированное расхождением в actionForEvent, прошло бы
// незамеченным. Поэтому каждая копия получает СВОЙ результат resolveBindings.

for (const [ovLabel, ov] of OVERRIDE_SETS) {
    const tgResolved = tg.resolveBindings(ov);
    const coreResolved = core.resolveBindings(ACTIONS, ov);
    for (const [label, e] of EVENTS) {
        for (const editing of [false, true]) {
            same(
                `actionForEvent[${ovLabel}]: ${label} (editing=${editing})`,
                tg.actionForEvent(e, tgResolved, { editing }),
                core.actionForEvent(e, coreResolved, { editing }),
            );
        }
    }
}

// Вырожденные аргументы.
same('actionForEvent: resolved=null', tg.actionForEvent(ev('KeyJ', 'о'), null, {}), core.actionForEvent(ev('KeyJ', 'о'), null, {}));
same('actionForEvent: event=null', tg.actionForEvent(null, tg.resolveBindings({}), {}), core.actionForEvent(null, core.resolveBindings(ACTIONS, {}), {}));
same(
    'actionForEvent: без ctx',
    tg.actionForEvent(ev('KeyJ', 'о'), tg.resolveBindings({})),
    core.actionForEvent(ev('KeyJ', 'о'), core.resolveBindings(ACTIONS, {})),
);

// ---------------------------------------------------------------------------
// 6. Контрольная проверка самого страж-теста
// ---------------------------------------------------------------------------
//
// Тест обязан не только сравнивать, но и уметь ОТЛИЧАТЬ: сравнение,
// возвращающее «одинаково» на заведомо разных значениях, зеленело бы всегда.

ok(JSON.stringify(plain(tg.resolveBindings({}))) !== JSON.stringify(plain(core.resolveBindings([], {}))),
    'самопроверка: разбор с пустым реестром ОТЛИЧАЕТСЯ от разбора с TG_ACTIONS');
ok(tg.comboFromEvent(ev('KeyJ', 'о')) === 'j',
    'самопроверка: русская раскладка даёт j (сопоставление по event.code)');
ok(core.comboFromEvent(ev('KeyJ', 'о')) === 'j',
    'самопроверка: ядро тоже даёт j на русской раскладке');

// ---------------------------------------------------------------------------

if (failures) {
    console.error(`\n${failures} расхождений из ${checks} проверок`);
    console.error('Движок комбинаций разъехался: правьте ОБЕ копии — telegram/keys.js и messenger/hotkeys-core.js.');
} else {
    console.log(`OK: ${checks} проверок, обе копии движка комбинаций совпадают`);
}
process.exit(failures ? 1 : 0);
