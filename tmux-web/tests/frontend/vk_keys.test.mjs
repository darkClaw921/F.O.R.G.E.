// Регресс-тест реестра и чистой логики горячих клавиш вкладки VK.
//
// Грузит РЕАЛЬНЫЕ модули `static/js/vk/keys.js` (реестр + обёртки) и через
// него `static/js/messenger/hotkeys-core.js` (движок). Стаб DOM не нужен: оба
// модуля намеренно не импортируют ни DOM, ни соседние модули панели.
//
// Соседние тесты и разделение ответственности:
//   • `messenger_shared_parity.test.mjs` — движок VK совпадает с телеграмным;
//   • `telegram_keys.test.mjs`           — то же для реестра Telegram;
//   • здесь                              — реестр VK и его отличия.
//
// Что покрыто: уникальность id, отсутствие дублей дефолтных комбинаций,
// неприкосновенность `fixed`-действий, разбор события на РУССКОЙ раскладке
// (сопоставление по `event.code`), два прохода `resolveBindings`,
// «только отличия» в `overridesFromBindings`, правило «в поле ввода одиночные
// клавиши не перехватываем» и отличия реестра VK от телеграмного.
//
// Запуск: node tmux-web/tests/frontend/vk_keys.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const keysUrl = new URL('../../static/js/vk/keys.js', import.meta.url).href;
const tgUrl = new URL('../../static/js/telegram/keys.js', import.meta.url).href;
const K = await import(keysUrl);
const TG = await import(tgUrl);

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

// ---------------------------------------------------------------------------
// 1. Реестр
// ---------------------------------------------------------------------------

ok(K.VK_ACTIONS.length > 0, 'реестр: действия есть');
{
    const ids = K.VK_ACTIONS.map((a) => a.id);
    eq(new Set(ids).size, ids.length, 'реестр: id действий уникальны');

    // Дубль дефолтной комбинации означал бы, что одно из действий стартует
    // БЕЗ клавиши — и пользователь не поймёт, почему оно «не работает».
    const combos = K.VK_ACTIONS.map((a) => K.normalizeCombo(a.combo));
    ok(combos.every((c) => c !== null), 'реестр: все дефолтные комбинации разбираются');
    eq(new Set(combos).size, combos.length, 'реестр: дефолтные комбинации не пересекаются');

    ok(K.VK_ACTIONS.every((a) => a.title && a.group), 'реестр: у всех есть название и группа');
    ok(K.VK_ACTIONS.every((a) => K.VK_ACTION_GROUPS.includes(a.group)),
        'реестр: все группы объявлены в VK_ACTION_GROUPS');

    // id уезжает в user_settings.json и проходит через backend-санитайзер
    // (`sanitize_hotkeys` пропускает только [a-z0-9_.-]).
    ok(ids.every((id) => /^[a-z0-9_.-]+$/.test(id)),
        'реестр: id проходят backend-санитайзер (a-z0-9_.-)');

    const fixed = K.VK_ACTIONS.filter((a) => a.fixed).map((a) => a.id);
    eq(fixed, ['ui.escape'], 'реестр: не переназначается только Escape');
}

// ---------------------------------------------------------------------------
// 2. Отличия реестра VK от телеграмного (§ шапка keys.js)
// ---------------------------------------------------------------------------

{
    const vkIds = new Set(K.VK_ACTIONS.map((a) => a.id));
    const tgIds = new Set(TG.TG_ACTIONS.map((a) => a.id));

    // Отличие 3: у VK на сообщении до десяти вложений — нужно действие «перейти
    // к вложениям». У Telegram media — один объект, действие было бы лишним.
    ok(vkIds.has('msg.attachments'), 'отличия: у VK есть msg.attachments');
    ok(!tgIds.has('msg.attachments'), 'отличия: у Telegram такого действия нет');

    // Всё остальное — тот же состав: две панели одного приложения обязаны
    // управляться одинаково.
    const missing = Array.from(tgIds).filter((id) => !vkIds.has(id));
    eq(missing, [], 'отличия: ни одно телеграмное действие не потеряно');
    const extra = Array.from(vkIds).filter((id) => !tgIds.has(id));
    eq(extra, ['msg.attachments'], 'отличия: добавлено ровно одно действие');

    // Дефолты совпадают у общих действий — мышечная память переносится.
    for (const a of K.VK_ACTIONS) {
        const twin = TG.TG_ACTIONS.find((t) => t.id === a.id);
        if (!twin) continue;
        eq(K.normalizeCombo(a.combo), TG.normalizeCombo(twin.combo),
            `отличия: дефолт ${a.id} совпадает с телеграмным`);
    }

    // Отличие 1: фильтры серверные — у dir.next обязана быть подсказка про запрос.
    const dirNext = K.findAction('dir.next');
    ok(/запрос/i.test(dirNext.hint || ''), 'отличия: dir.next предупреждает про сетевой запрос');
    // Отличие 2: кнопки сообществ нажимаются не все.
    const buttons = K.findAction('msg.buttons');
    ok(/callback/i.test(buttons.hint || ''), 'отличия: msg.buttons предупреждает про callback');
}

// ---------------------------------------------------------------------------
// 3. Разбор события: сопоставление по event.code (РУССКАЯ раскладка)
// ---------------------------------------------------------------------------

eq(K.comboFromEvent(ev('KeyJ', 'о')), 'j', 'русская раскладка: физическая J даёт j');
eq(K.comboFromEvent(ev('KeyK', 'л')), 'k', 'русская раскладка: физическая K даёт k');
eq(K.comboFromEvent(ev('KeyR', 'к')), 'r', 'русская раскладка: физическая R даёт r');
eq(K.comboFromEvent(ev('KeyV', 'м')), 'v', 'русская раскладка: физическая V даёт v');
eq(K.comboFromEvent(ev('KeyG', 'П', { shift: true })), 'Shift+g',
    'русская раскладка: Shift+G даёт Shift+g');
eq(K.comboFromEvent(ev('KeyD', 'в')), 'd', 'русская раскладка: физическая D даёт d');
eq(K.comboFromEvent(ev('KeyJ', 'j')), 'j', 'латиница: та же клавиша даёт то же');
eq(K.comboFromEvent(ev('Slash', '?', { shift: true })), 'Shift+/',
    'шпаргалка: Shift+Slash канонизируется в Shift+/');
eq(K.comboFromEvent(ev('BracketRight', ']')), ']', 'фильтры: BracketRight даёт ]');
eq(K.comboFromEvent(ev('Enter', 'Enter')), 'Enter', 'Enter опознан');
eq(K.comboFromEvent(ev('Escape', 'Escape')), 'Escape', 'Escape опознан');
eq(K.comboFromEvent(ev('ShiftLeft', 'Shift', { shift: true })), null,
    'один модификатор комбинацией не является');
eq(K.comboFromEvent(null), null, 'null не роняет разбор');

// Порядок модификаторов канонический независимо от порядка нажатия.
eq(K.comboFromEvent(ev('KeyX', 'ч', { shift: true, ctrl: true })), 'Ctrl+Shift+x',
    'модификаторы: канонический порядок Ctrl→Shift');
eq(K.normalizeCombo('Shift+Ctrl+x'), 'Ctrl+Shift+x', 'нормализация: порядок модификаторов чинится');
eq(K.normalizeCombo('cmd+K'), 'Meta+k', 'нормализация: cmd → Meta, регистр вниз');
eq(K.normalizeCombo('+++'), null, 'нормализация: мусор из плюсов отвергнут');
eq(K.normalizeCombo('a+b'), null, 'нормализация: две клавиши отвергнуты');
eq(K.formatCombo('Shift+/'), '⇧?', 'подпись: Shift+/ показывается как ⇧?');
eq(K.formatCombo('Enter'), '⏎', 'подпись: Enter показывается глифом');

// ---------------------------------------------------------------------------
// 4. resolveBindings — ДВА ПРОХОДА
// ---------------------------------------------------------------------------

{
    const clean = K.resolveBindings(null);
    eq(clean.conflicts.length, 0, 'дефолты: конфликтов нет');
    eq(clean.unbound.length, 0, 'дефолты: все действия с клавишами');
    eq(clean.bindings.get('nav.next'), 'j', 'дефолты: nav.next на j');
    eq(clean.bindings.get('msg.attachments'), 'v', 'дефолты: msg.attachments на v');
    eq(clean.byCombo.get('v'), 'msg.attachments', 'дефолты: обратная карта построена');

    // ГЛАВНОЕ: переопределение позднего действия обязано победить дефолт
    // раннего. Один проход по реестру отдал бы 'j' раннему nav.next, и
    // пользователь получил бы «моя настройка не применилась».
    const stolen = K.resolveBindings({ 'msg.copy': 'j' });
    eq(stolen.bindings.get('msg.copy'), 'j', 'два прохода: переопределение забрало j');
    eq(stolen.bindings.get('nav.next'), undefined, 'два прохода: дефолт nav.next проиграл');
    ok(stolen.unbound.includes('nav.next'), 'два прохода: проигравший в unbound');
    eq(stolen.conflicts, [{ combo: 'j', winner: 'msg.copy', loser: 'nav.next' }],
        'два прохода: конфликт назван поимённо');
    // Проигравший остаётся БЕЗ клавиши, а не молча возвращается к дефолту:
    // молчаливый возврат соврал бы пользователю, занявшему эту клавишу.
    eq(stolen.byCombo.get('j'), 'msg.copy', 'два прохода: j принадлежит победителю');

    // Обмен клавишами местами — оба переопределения обязаны примениться.
    const swap = K.resolveBindings({ 'nav.next': 'k', 'nav.prev': 'j' });
    eq(swap.bindings.get('nav.next'), 'k', 'обмен: nav.next получил k');
    eq(swap.bindings.get('nav.prev'), 'j', 'обмен: nav.prev получил j');
    eq(swap.conflicts.length, 0, 'обмен: конфликтов нет');

    // Два переопределения на одну клавишу: побеждает то, что раньше в реестре.
    const both = K.resolveBindings({ 'msg.quote': 'Ctrl+q', 'msg.copy': 'Ctrl+q' });
    eq(both.byCombo.get('Ctrl+q'), 'msg.quote', 'дубль: победило раннее в реестре');
    ok(both.unbound.includes('msg.copy'), 'дубль: позднее осталось без клавиши');

    // fixed не переназначается даже записью прямо в файле настроек.
    const esc = K.resolveBindings({ 'ui.escape': 'Ctrl+e' });
    eq(esc.bindings.get('ui.escape'), 'Escape', 'fixed: Escape не переехал');
    eq(esc.byCombo.get('Ctrl+e'), undefined, 'fixed: чужая комбинация не занята');

    // Чужие id и мусор игнорируются, но НЕ ломают остальное.
    const junk = K.resolveBindings({ 'nope.nope': 'q', 'msg.copy': '+++', 'msg.reply': 'Shift+R' });
    eq(junk.bindings.get('msg.copy'), 'y', 'мусор: значение отброшено, остался дефолт');
    eq(junk.bindings.get('msg.reply'), 'Shift+r', 'мусор: годное переопределение применилось');
    eq(junk.byCombo.get('q'), undefined, 'мусор: чужой id клавишу не занял');
}

// ---------------------------------------------------------------------------
// 5. sanitizeOverrides
// ---------------------------------------------------------------------------

{
    const clean = K.sanitizeOverrides({
        'msg.reply': 'Shift+R',
        'ui.escape': 'x',
        'nope.nope': 'q',
        'msg.copy': '',
    });
    eq(Array.from(clean.entries()), [['msg.reply', 'Shift+r']],
        'санитайзер: остаётся только применимое');
    eq(Array.from(K.sanitizeOverrides(null).entries()), [], 'санитайзер: null → пустая карта');
    eq(Array.from(K.sanitizeOverrides('j').entries()), [], 'санитайзер: не объект → пустая карта');
    eq(Array.from(K.sanitizeOverrides(new Map([['msg.copy', 'Ctrl+c']])).entries()),
        [['msg.copy', 'Ctrl+c']], 'санитайзер: Map принимается наравне с объектом');
}

// ---------------------------------------------------------------------------
// 6. overridesFromBindings — ТОЛЬКО отличия от дефолта
// ---------------------------------------------------------------------------

{
    // Если писать полную карту, смена дефолта в новой версии не доедет до
    // пользователей «со всем как из коробки», а снять переопределение станет
    // нечем: пустая карта перестанет означать «всё по умолчанию».
    const draft = new Map([
        ['msg.reply', 'r'], // ровно дефолт
        ['msg.copy', 'Ctrl+c'], // отличие
        ['ui.escape', 'Ctrl+e'], // fixed — не наше дело
    ]);
    eq(K.overridesFromBindings(draft), { 'msg.copy': 'Ctrl+c' },
        'отправка: пишутся только отличия от дефолта');
    eq(K.overridesFromBindings(new Map()), {}, 'отправка: пустой черновик → пустая карта');
    eq(K.overridesFromBindings({ 'msg.copy': 'Ctrl+c' }), { 'msg.copy': 'Ctrl+c' },
        'отправка: plain-объект принимается наравне с Map');
    eq(K.overridesFromBindings(new Map([['msg.reply', 'R']])), {},
        'отправка: дефолт в другом написании тоже не пишется');
    eq(K.overridesFromBindings(new Map([['msg.copy', '+++']])), {},
        'отправка: мусор не уезжает на backend');
    eq(K.overridesFromBindings(null), {}, 'отправка: null не роняет');
}

// ---------------------------------------------------------------------------
// 7. Строки и группы для UI
// ---------------------------------------------------------------------------

{
    const rows = K.bindingRows(null);
    eq(rows.length, K.VK_ACTIONS.length, 'строки: по одной на действие');
    ok(rows.every((r) => r.isDefault), 'строки: без переопределений все дефолтные');
    ok(rows.every((r) => r.label !== ''), 'строки: у всех есть подпись');
    eq(rows.find((r) => r.id === 'ui.escape').fixed, true, 'строки: fixed отмечен');

    const custom = K.bindingRows({ 'msg.attachments': 'j' });
    const loser = custom.find((r) => r.id === 'nav.next');
    eq(loser.combo, null, 'строки: проигравший конфликт показан без клавиши');
    eq(loser.label, '', 'строки: у него пустая подпись');
    eq(loser.conflictWith, 'msg.attachments', 'строки: назван победитель конфликта');
    eq(custom.find((r) => r.id === 'msg.attachments').isDefault, false,
        'строки: переопределение помечено как не-дефолт');

    const groups = K.groupedRows(null);
    ok(groups.length > 0, 'группы: не пусты');
    eq(groups.map((g) => g.name), K.VK_ACTION_GROUPS, 'группы: порядок из VK_ACTION_GROUPS');
    const total = groups.reduce((acc, g) => acc + g.items.length, 0);
    eq(total, K.VK_ACTIONS.length, 'группы: ни одно действие не потерялось');
    ok(groups.every((g) => g.items.length > 0), 'группы: пустых групп нет');
}

// ---------------------------------------------------------------------------
// 8. actionForEvent и правило поля ввода
// ---------------------------------------------------------------------------

{
    const resolved = K.resolveBindings(null);
    eq(K.actionForEvent(ev('KeyJ', 'о'), resolved, { editing: false }), 'nav.next',
        'диспетчер: русская j двигает курсор');
    eq(K.actionForEvent(ev('KeyV', 'м'), resolved, { editing: false }), 'msg.attachments',
        'диспетчер: русская v ведёт к вложениям');
    eq(K.actionForEvent(ev('KeyQ', 'й'), resolved, { editing: false }), null,
        'диспетчер: незанятая клавиша не даёт действия');

    // В поле ввода одиночные клавиши обязаны доходить до поля — иначе букву
    // «j» в сообщении не набрать.
    eq(K.actionForEvent(ev('KeyJ', 'о'), resolved, { editing: true }), null,
        'поле ввода: одиночная j не перехватывается');
    eq(K.actionForEvent(ev('KeyG', 'П', { shift: true }), resolved, { editing: true }), null,
        'поле ввода: Shift+g — это заглавная буква, не хоткей');
    eq(K.actionForEvent(ev('Escape', 'Escape'), resolved, { editing: true }), 'ui.escape',
        'поле ввода: Escape проходит всегда');

    eq(K.comboAllowedIn('j', { editing: true }), false, 'контекст: одиночная в поле запрещена');
    eq(K.comboAllowedIn('Ctrl+j', { editing: true }), true, 'контекст: с Ctrl разрешена');
    eq(K.comboAllowedIn('Alt+j', { editing: true }), true, 'контекст: с Alt разрешена');
    eq(K.comboAllowedIn('Meta+j', { editing: true }), true, 'контекст: с Meta разрешена');
    eq(K.comboAllowedIn('Shift+j', { editing: true }), false,
        'контекст: Shift — обычный набор, не хоткей');
    eq(K.comboAllowedIn('j', { editing: false }), true, 'контекст: вне поля разрешена');
}

// ---------------------------------------------------------------------------
// 9. isEditableTarget и defaultBindings
// ---------------------------------------------------------------------------

eq(K.isEditableTarget({ tagName: 'INPUT' }), true, 'фокус: input — поле ввода');
eq(K.isEditableTarget({ tagName: 'TEXTAREA' }), true, 'фокус: textarea — поле ввода');
eq(K.isEditableTarget({ tagName: 'DIV', isContentEditable: true }), true,
    'фокус: contentEditable — поле ввода');
eq(K.isEditableTarget({
    tagName: 'TEXTAREA',
    classList: { contains: (c) => c === 'xterm-helper-textarea' },
}), true, 'фокус: helper-textarea терминала — поле ввода');
eq(K.isEditableTarget({ tagName: 'LI' }), false, 'фокус: строка списка — не поле ввода');
eq(K.isEditableTarget(null), false, 'фокус: null не роняет');

eq(K.defaultBindings().size, K.VK_ACTIONS.length, 'дефолты: карта покрывает весь реестр');
eq(K.defaultBindings().get('ui.help'), 'Shift+/', 'дефолты: шпаргалка на Shift+/');
eq(K.findAction('msg.attachments').combo, 'v', 'findAction: находит по id');
eq(K.findAction('nope'), null, 'findAction: чужой id → null');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/vk_keys)`);
if (failures > 0) process.exit(1);
