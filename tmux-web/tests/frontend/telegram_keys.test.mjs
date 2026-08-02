// Регресс-тест чистой логики горячих клавиш вкладки Telegram.
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/keys.js через
// `await import()`. Стаб DOM не нужен: модуль намеренно не импортирует ни DOM,
// ни соседние модули панели.
//
// Что покрыто: разбор события в каноническую комбинацию (в том числе на
// НЕлатинской раскладке), нормализация записанной строки, подписи для UI,
// разрешение переопределений и конфликтов, вычисление того, что слать на
// backend, и правило «в поле ввода одиночные клавиши не перехватываем».
//
// Запуск: node tmux-web/tests/frontend/telegram_keys.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/telegram/keys.js', import.meta.url).href;
const K = await import(modUrl);

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

/** Событие клавиатуры: code — физическая клавиша, key — символ раскладки. */
function ev(code, key, mods) {
    return {
        code,
        key: key === undefined ? '' : key,
        ctrlKey: !!(mods && mods.ctrl),
        altKey: !!(mods && mods.alt),
        shiftKey: !!(mods && mods.shift),
        metaKey: !!(mods && mods.meta),
    };
}

// ---------- реестр действий ----------

ok(K.TG_ACTIONS.length > 0, 'реестр: действия есть');
{
    const ids = K.TG_ACTIONS.map((a) => a.id);
    eq(new Set(ids).size, ids.length, 'реестр: id действий уникальны');
    const combos = K.TG_ACTIONS.map((a) => K.normalizeCombo(a.combo));
    ok(combos.every((c) => c !== null), 'реестр: все дефолтные комбинации разбираются');
    eq(new Set(combos).size, combos.length, 'реестр: дефолтные комбинации не пересекаются');
    ok(K.TG_ACTIONS.every((a) => a.title && a.group), 'реестр: у всех есть название и группа');
    // Escape обязан остаться неперeназначаемым: это единственный выход из
    // режима, и отдав его, пользователь мог бы запереть себя в панели.
    const esc = K.findAction('ui.escape');
    ok(esc && esc.fixed === true, 'реестр: Escape помечен fixed');
}

// ---------- comboFromEvent: раскладка ----------

eq(K.comboFromEvent(ev('KeyJ', 'j')), 'j', 'событие: латинская j');
// Главный сценарий фичи: в Telegram печатают по-русски, и key приезжает 'о'.
eq(K.comboFromEvent(ev('KeyJ', 'о')), 'j', 'событие: русская раскладка даёт ту же клавишу');
eq(K.comboFromEvent(ev('KeyR', 'к')), 'r', 'событие: русская раскладка, r');
eq(K.comboFromEvent(ev('BracketLeft', 'х')), '[', 'событие: скобка на русской раскладке');
eq(K.comboFromEvent(ev('Digit1', '1')), '1', 'событие: цифра');
eq(K.comboFromEvent(ev('Enter', 'Enter')), 'Enter', 'событие: Enter');
eq(K.comboFromEvent(ev('NumpadEnter', 'Enter')), 'Enter', 'событие: Enter на цифровом блоке = Enter');
eq(K.comboFromEvent(ev('Escape', 'Escape')), 'Escape', 'событие: Escape');
eq(K.comboFromEvent(ev('Space', ' ')), 'Space', 'событие: пробел');
eq(K.comboFromEvent(ev('ArrowDown', 'ArrowDown')), 'ArrowDown', 'событие: стрелка');
eq(K.comboFromEvent(ev('F5', 'F5')), 'F5', 'событие: функциональная клавиша');

// Модификаторы: порядок канонический, а не тот, в котором их проверяли.
eq(K.comboFromEvent(ev('KeyG', 'G', { shift: true })), 'Shift+g', 'событие: Shift+g');
eq(K.comboFromEvent(ev('KeyX', 'x', { ctrl: true })), 'Ctrl+x', 'событие: Ctrl+x');
eq(K.comboFromEvent(ev('KeyX', 'x', { meta: true, shift: true })), 'Shift+Meta+x',
    'событие: порядок модификаторов канонический (Shift раньше Meta)');
eq(K.comboFromEvent(ev('KeyX', 'x', { ctrl: true, alt: true, shift: true, meta: true })),
    'Ctrl+Alt+Shift+Meta+x', 'событие: все четыре модификатора в порядке реестра');
eq(K.comboFromEvent(ev('Slash', '?', { shift: true })), 'Shift+/',
    'событие: «?» — это Shift и базовая клавиша, а не отдельный символ');

// Одиночный модификатор — не комбинация: иначе удержание ⌘ срабатывало бы
// как хоткей.
eq(K.comboFromEvent(ev('ShiftLeft', 'Shift', { shift: true })), null, 'событие: один Shift — не комбинация');
eq(K.comboFromEvent(ev('MetaLeft', 'Meta', { meta: true })), null, 'событие: один Meta — не комбинация');
eq(K.comboFromEvent(ev('ControlLeft', 'Control', { ctrl: true })), null, 'событие: один Ctrl — не комбинация');
eq(K.comboFromEvent(null), null, 'событие: null не роняет');
eq(K.comboFromEvent({}), null, 'событие: пустой объект → null');

// Фолбэк на key, когда code нет (экранные клавиатуры).
eq(K.comboFromEvent({ code: '', key: 'j' }), 'j', 'событие: без code берём key');
eq(K.comboFromEvent({ code: '', key: '?', shiftKey: true }), 'Shift+/',
    'событие: без code «?» тоже приводится к базовой клавише');
eq(K.comboFromEvent({ code: '', key: 'Enter' }), 'Enter', 'событие: без code спец-клавиша');

// ---------- normalizeCombo ----------

eq(K.normalizeCombo('j'), 'j', 'нормализация: простая клавиша');
eq(K.normalizeCombo('J'), 'j', 'нормализация: регистр не важен');
eq(K.normalizeCombo(' j '), 'j', 'нормализация: пробелы по краям');
eq(K.normalizeCombo('shift+g'), 'Shift+g', 'нормализация: модификатор в нижнем регистре');
eq(K.normalizeCombo('Shift+Ctrl+x'), 'Ctrl+Shift+x', 'нормализация: порядок модификаторов приводится');
eq(K.normalizeCombo('cmd+j'), 'Meta+j', 'нормализация: cmd → Meta');
eq(K.normalizeCombo('command+j'), 'Meta+j', 'нормализация: command → Meta');
eq(K.normalizeCombo('control+j'), 'Ctrl+j', 'нормализация: control → Ctrl');
eq(K.normalizeCombo('option+j'), 'Alt+j', 'нормализация: option → Alt');
eq(K.normalizeCombo('?'), '/', 'нормализация: «?» — это клавиша «/»');
eq(K.normalizeCombo('shift+?'), 'Shift+/', 'нормализация: Shift+? = Shift+/');
eq(K.normalizeCombo('escape'), 'Escape', 'нормализация: имя спец-клавиши');
eq(K.normalizeCombo('arrowup'), 'ArrowUp', 'нормализация: стрелка');
eq(K.normalizeCombo('f12'), 'F12', 'нормализация: функциональная клавиша');
// Дубликат модификатора не должен порождать вторую запись в каноне.
eq(K.normalizeCombo('Shift+Shift+g'), 'Shift+g', 'нормализация: повтор модификатора схлопывается');

eq(K.normalizeCombo(''), null, 'нормализация: пустая строка → null');
eq(K.normalizeCombo('   '), null, 'нормализация: пробелы → null');
eq(K.normalizeCombo('Shift'), null, 'нормализация: один модификатор — не комбинация');
eq(K.normalizeCombo('Ctrl+Shift'), null, 'нормализация: только модификаторы → null');
eq(K.normalizeCombo('a+b'), null, 'нормализация: две клавиши → null');
eq(K.normalizeCombo('Hyper+a'), null, 'нормализация: неизвестный модификатор → null');
eq(K.normalizeCombo('Unknown'), null, 'нормализация: неизвестное имя клавиши → null');
eq(K.normalizeCombo(null), null, 'нормализация: null не роняет');
eq(K.normalizeCombo(42), null, 'нормализация: число не роняет');

// Событие и запись должны сходиться в одну строку — иначе переназначенная
// клавиша не сработала бы никогда.
eq(K.normalizeCombo(K.comboFromEvent(ev('KeyG', 'G', { shift: true }))), 'Shift+g',
    'нормализация: канон события уже канонический (идемпотентность)');

// ---------- formatCombo ----------

eq(K.formatCombo('j'), 'J', 'подпись: буква в верхнем регистре');
eq(K.formatCombo('Shift+g'), '⇧G', 'подпись: Shift глифом');
eq(K.formatCombo('Shift+/'), '⇧?', 'подпись: Shift+/ показывается как ?');
eq(K.formatCombo('Enter'), '⏎', 'подпись: Enter глифом');
eq(K.formatCombo('Escape'), 'Esc', 'подпись: Escape коротко');
eq(K.formatCombo('Space'), 'Пробел', 'подпись: пробел словом');
eq(K.formatCombo('ArrowUp'), '↑', 'подпись: стрелка глифом');
eq(K.formatCombo('Ctrl+Enter'), 'Ctrl⏎', 'подпись: модификатор + спец-клавиша');
eq(K.formatCombo('Meta+j'), '⌘J', 'подпись: Meta глифом');
eq(K.formatCombo('['), '[', 'подпись: скобка как есть');
eq(K.formatCombo('мусор+'), '', 'подпись: мусор даёт пустую строку');
eq(K.formatCombo(null), '', 'подпись: null не роняет');

// ---------- resolveBindings: дефолты ----------

{
    const r = K.resolveBindings(null);
    eq(r.bindings.size, K.TG_ACTIONS.length, 'привязки: без переопределений все действия с клавишами');
    eq(r.conflicts.length, 0, 'привязки: дефолты не конфликтуют');
    eq(r.unbound.length, 0, 'привязки: без переопределений никто не остался без клавиши');
    eq(r.byCombo.get('j'), 'nav.next', 'привязки: j → следующий элемент');
    eq(r.byCombo.get('Escape'), 'ui.escape', 'привязки: Escape → отмена');
    eq(K.resolveBindings(undefined).bindings.size, K.TG_ACTIONS.length,
        'привязки: undefined = как без переопределений');
    eq(K.resolveBindings('мусор').bindings.size, K.TG_ACTIONS.length,
        'привязки: строка вместо карты не роняет');
}

// ---------- resolveBindings: переопределения ----------

{
    const r = K.resolveBindings({ 'msg.reply': 'Ctrl+r' });
    eq(r.bindings.get('msg.reply'), 'Ctrl+r', 'переопределение: применено');
    eq(r.byCombo.get('Ctrl+r'), 'msg.reply', 'переопределение: обратный индекс обновлён');
    // Освободившаяся клавиша НЕ должна остаться привязанной к прошлому действию.
    eq(r.byCombo.get('r'), undefined, 'переопределение: старая клавиша освобождена');
    eq(r.conflicts.length, 0, 'переопределение: конфликтов нет');
}

{
    // Переопределение забирает клавишу у дефолта другого действия. Победа за
    // переопределением, проигравший остаётся БЕЗ клавиши — молчаливый возврат
    // дефолта соврал бы пользователю.
    const r = K.resolveBindings({ 'msg.reply': 'j' });
    eq(r.bindings.get('msg.reply'), 'j', 'конфликт: переопределение сильнее дефолта');
    eq(r.bindings.get('nav.next'), undefined, 'конфликт: проигравший остался без клавиши');
    eq(r.byCombo.get('j'), 'msg.reply', 'конфликт: клавиша принадлежит победителю');
    eq(r.conflicts.length, 1, 'конфликт: зафиксирован ровно один');
    eq(r.conflicts[0].winner, 'msg.reply', 'конфликт: победитель назван');
    eq(r.conflicts[0].loser, 'nav.next', 'конфликт: проигравший назван');
    ok(r.unbound.includes('nav.next'), 'конфликт: проигравший попал в unbound');
}

{
    // Два переопределения на одну клавишу: побеждает то, что раньше в реестре,
    // иначе результат зависел бы от порядка ключей в JSON.
    const a = K.resolveBindings({ 'msg.reply': 'z', 'msg.copy': 'z' });
    const b = K.resolveBindings({ 'msg.copy': 'z', 'msg.reply': 'z' });
    eq(a.byCombo.get('z'), b.byCombo.get('z'),
        'конфликт: результат не зависит от порядка ключей в объекте');
    eq(a.conflicts.length, 1, 'конфликт: два претендента дают одну запись');
}

{
    // Мусор среди переопределений не должен ронять разбор и не должен
    // оставлять действие без клавиши.
    const r = K.resolveBindings({
        'msg.reply': 'Ctrl+r',
        'нет.такого': 'q',
        'msg.copy': 'Hyper+q',
        'ui.escape': 'q',
    });
    eq(r.bindings.get('msg.reply'), 'Ctrl+r', 'мусор: годное переопределение применено');
    eq(r.bindings.get('msg.copy'), 'y', 'мусор: непарсимая комбинация → дефолт действия');
    eq(r.bindings.get('ui.escape'), 'Escape', 'мусор: fixed-действие переназначить нельзя');
    eq(r.unbound.length, 0, 'мусор: никто не остался без клавиши');
}

// ---------- sanitizeOverrides ----------

{
    const m = K.sanitizeOverrides({ 'msg.reply': 'CTRL+R', 'unknown.id': 'x', 'ui.escape': 'q' });
    eq(m.get('msg.reply'), 'Ctrl+R'.replace('R', 'r'), 'санитайз: комбинация канонизирована');
    eq(m.has('unknown.id'), false, 'санитайз: чужой id отброшен');
    eq(m.has('ui.escape'), false, 'санитайз: fixed-действие отброшено');
    eq(K.sanitizeOverrides(null).size, 0, 'санитайз: null → пустая карта');
    eq(K.sanitizeOverrides([]).size, 0, 'санитайз: массив не роняет');
    // Map на входе — тот же результат, что и объект.
    eq(K.sanitizeOverrides(new Map([['msg.reply', 'x']])).get('msg.reply'), 'x',
        'санитайз: Map на входе поддержан');
}

// ---------- overridesFromBindings ----------

{
    const out = K.overridesFromBindings(new Map([
        ['msg.reply', 'x'],
        ['nav.next', 'j'],
        ['ui.escape', 'q'],
        ['чужой', 'z'],
    ]));
    eq(out, { 'msg.reply': 'x' }, 'отправка: только отличия от дефолта, без fixed и чужих');
    eq(K.overridesFromBindings(new Map()), {}, 'отправка: пустая карта → пустой объект');
    eq(K.overridesFromBindings(null), {}, 'отправка: null не роняет');
    eq(K.overridesFromBindings({ 'msg.reply': 'x' }), { 'msg.reply': 'x' },
        'отправка: plain-объект на входе поддержан');
    // Значение, равное дефолту в другой записи, не должно «прилипнуть».
    eq(K.overridesFromBindings(new Map([['msg.reply', 'R']])), {},
        'отправка: дефолт, записанный в другом регистре, отличием не считается');
}

// ---------- comboAllowedIn: поле ввода ----------

eq(K.comboAllowedIn('j', { editing: false }), true, 'ввод: вне поля одиночная клавиша разрешена');
// Ключевое правило: иначе букву «j» было бы не набрать в сообщении.
eq(K.comboAllowedIn('j', { editing: true }), false, 'ввод: в поле одиночная клавиша НЕ перехватывается');
eq(K.comboAllowedIn('Shift+g', { editing: true }), false,
    'ввод: Shift+буква — это заглавная буква, не хоткей');
eq(K.comboAllowedIn('Ctrl+j', { editing: true }), true, 'ввод: Ctrl-комбинация проходит');
eq(K.comboAllowedIn('Alt+j', { editing: true }), true, 'ввод: Alt-комбинация проходит');
eq(K.comboAllowedIn('Meta+j', { editing: true }), true, 'ввод: Meta-комбинация проходит');
eq(K.comboAllowedIn('Escape', { editing: true }), true, 'ввод: Escape работает и в поле');
eq(K.comboAllowedIn('Ctrl+Shift+j', { editing: true }), true,
    'ввод: Shift рядом с Ctrl не мешает');
eq(K.comboAllowedIn('мусор+', { editing: false }), false, 'ввод: мусор запрещён');

// ---------- actionForEvent ----------

{
    const resolved = K.resolveBindings(null);
    eq(K.actionForEvent(ev('KeyJ', 'о'), resolved, {}), 'nav.next',
        'диспетчер: русская раскладка попадает в действие');
    eq(K.actionForEvent(ev('KeyJ', 'j'), resolved, { editing: true }), null,
        'диспетчер: в поле ввода одиночная клавиша игнорируется');
    eq(K.actionForEvent(ev('Escape', 'Escape'), resolved, { editing: true }), 'ui.escape',
        'диспетчер: Escape работает и в поле ввода');
    eq(K.actionForEvent(ev('KeyJ', 'j', { ctrl: true }), resolved, {}), null,
        'диспетчер: Ctrl+j ничему не назначен — не перехватываем');
    eq(K.actionForEvent(ev('ShiftLeft', 'Shift', { shift: true }), resolved, {}), null,
        'диспетчер: одиночный модификатор не действие');
    eq(K.actionForEvent(null, resolved, {}), null, 'диспетчер: null не роняет');
    eq(K.actionForEvent(ev('KeyJ', 'j'), null, {}), null, 'диспетчер: без привязок null');

    const custom = K.resolveBindings({ 'nav.next': 'Ctrl+n' });
    eq(K.actionForEvent(ev('KeyN', 'n', { ctrl: true }), custom, {}), 'nav.next',
        'диспетчер: переназначенная комбинация срабатывает');
    eq(K.actionForEvent(ev('KeyJ', 'j'), custom, {}), null,
        'диспетчер: освободившаяся клавиша больше ничего не делает');
    eq(K.actionForEvent(ev('KeyN', 'n', { ctrl: true }), custom, { editing: true }), 'nav.next',
        'диспетчер: Ctrl-переназначение работает и в поле ввода');
}

// ---------- bindingRows / groupedRows ----------

{
    const rows = K.bindingRows(null);
    eq(rows.length, K.TG_ACTIONS.length, 'строки: по одной на действие');
    ok(rows.every((r) => r.isDefault), 'строки: без переопределений все дефолтные');
    ok(rows.every((r) => r.label !== ''), 'строки: у всех есть подпись');
    const esc = rows.find((r) => r.id === 'ui.escape');
    ok(esc.fixed, 'строки: fixed отмечен');

    const custom = K.bindingRows({ 'msg.reply': 'j' });
    const loser = custom.find((r) => r.id === 'nav.next');
    eq(loser.combo, null, 'строки: проигравший конфликт показан без клавиши');
    eq(loser.label, '', 'строки: у него пустая подпись');
    eq(loser.conflictWith, 'msg.reply', 'строки: назван победитель конфликта');
    const winner = custom.find((r) => r.id === 'msg.reply');
    eq(winner.isDefault, false, 'строки: переопределение помечено как не-дефолт');

    const groups = K.groupedRows(null);
    ok(groups.length > 0, 'группы: не пусты');
    const total = groups.reduce((acc, g) => acc + g.items.length, 0);
    eq(total, K.TG_ACTIONS.length, 'группы: ни одно действие не потерялось');
    ok(groups.every((g) => g.items.length > 0), 'группы: пустых групп нет');
}

// ---------- isEditableTarget ----------

eq(K.isEditableTarget({ tagName: 'INPUT' }), true, 'фокус: input — поле ввода');
eq(K.isEditableTarget({ tagName: 'TEXTAREA' }), true, 'фокус: textarea — поле ввода');
eq(K.isEditableTarget({ tagName: 'SELECT' }), true, 'фокус: select — поле ввода');
eq(K.isEditableTarget({ tagName: 'DIV', isContentEditable: true }), true,
    'фокус: contentEditable — поле ввода');
eq(K.isEditableTarget({
    tagName: 'TEXTAREA',
    classList: { contains: (c) => c === 'xterm-helper-textarea' },
}), true, 'фокус: helper-textarea терминала — поле ввода');
eq(K.isEditableTarget({ tagName: 'LI' }), false, 'фокус: строка списка — не поле ввода');
eq(K.isEditableTarget(null), false, 'фокус: null не роняет');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/telegram_keys)`);
if (failures > 0) process.exit(1);
