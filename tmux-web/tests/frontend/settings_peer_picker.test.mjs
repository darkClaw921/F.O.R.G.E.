// tmux-web / тест общей формы выбора бесед (`js/settings/peer-picker.js`).
//
// ⚠ СТРАЖ ГЛАВНОГО ИНВАРИАНТА ФОРМЫ: черновик хранит ВСЕ выбранные id, а не
// только нарисованные. Список бесед приходит верхушкой, и выбор человека
// вполне может содержать беседу, до которой руки не доходили полгода. Если бы
// черновик собирался из галочек на экране, первое же открытие настроек молча
// вычёркивало бы такие беседы из белого списка — именно это и проверяется
// здесь, вместе с тем, что чистка происходит ТОЛЬКО по явному нажатию.
//
// Запуск: `node tmux-web/tests/frontend/settings_peer_picker.test.mjs`.
// Код возврата 0/1. Фейковый DOM — общий стаб `meet_popup_dom.mjs`.

import { makeEl, makeText, dispatch } from './meet_popup_dom.mjs';

const $document = makeEl('#document');
$document.createElement = (tag) => {
    const node = makeEl(tag);
    node.style = {};
    node.checked = false;
    return node;
};
$document.createTextNode = (t) => makeText(t);
globalThis.document = $document;

const { buildPeerPicker } = await import(
    new URL('../../static/js/settings/peer-picker.js', import.meta.url).href
);

let failures = 0;
let checks = 0;
function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    } else {
        console.log(`[ok]   ${label}`);
    }
}
function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    } else {
        console.log(`[ok]   ${label}`);
    }
}
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const GROUPS = [
    { key: 'user', label: 'Личные сообщения' },
    { key: 'chat', label: 'Беседы' },
];
const PEERS = [
    { id: '1', title: 'Первая' },
    { id: '2', title: 'Вторая' },
];

function make(extra) {
    return buildPeerPicker(Object.assign({
        doc: $document,
        classPrefix: 'voice-scope',
        groupsTitle: 'Типы бесед',
        peersTitle: 'Отдельные беседы',
        loadGroups: async () => GROUPS,
        loadPeers: async () => PEERS,
    }, extra || {}));
}

const rowsOf = (node, cls) => node.querySelectorAll(`.${cls}`);

// ── 1. выбранная, но не загруженная беседа не теряется ──────────────────────

{
    const p = make({ initialPeers: ['1', '404'], initialGroups: ['chat'] });
    await flush();
    await flush();

    eq(p.peers(), ['1', '404'], 'черновик держит и загруженный id, и незагруженный');
    eq(p.groups(), ['chat'], 'выбор группы прочитан из настроек');
    ok(
        String(p.node.textContent).includes('Выбрано вне загруженного списка: 1'),
        'незагруженный выбор ВИДЕН человеку, а не спрятан',
    );
    ok(String(p.node.textContent).includes('id 404'), 'у сироты нарисована своя строка');
}

// ── 2. чистка — только по нажатию ───────────────────────────────────────────

{
    const p = make({ initialPeers: ['1', '404'] });
    await flush();
    await flush();
    eq(p.peers(), ['1', '404'], 'до нажатия сирота на месте');

    const prune = p.node.querySelector('.voice-scope-prune');
    ok(!!prune, 'кнопка «Убрать несуществующие» есть');
    eq(prune.textContent, 'Убрать несуществующие', 'подпись кнопки прежняя');
    dispatch(prune, 'click');
    eq(p.peers(), ['1'], 'нажатие вычистило только то, чего нет в списке');
}

// ── 3. отказ загрузки не трогает выбор ──────────────────────────────────────

{
    const p = make({
        initialPeers: ['404'],
        loadPeers: async () => { throw new Error('плагин выключен'); },
    });
    await flush();
    await flush();

    eq(p.peers(), ['404'], 'при недоступном списке выбор остаётся прежним');
    eq(p.isLoaded(), false, 'форма честно считает себя незагруженной');
    const prune = p.node.querySelector('.voice-scope-prune');
    dispatch(prune, 'click');
    eq(p.peers(), ['404'], 'чистка без загруженного списка НИЧЕГО не удаляет');
}

// ── 4. галочки правят черновик ──────────────────────────────────────────────

{
    let changes = 0;
    const p = make({ onChange: () => { changes += 1; } });
    await flush();
    await flush();

    const boxes = rowsOf(p.node, 'voice-scope-item').map((r) => r.querySelector('input'));
    ok(boxes.length >= 4, 'нарисованы и группы, и беседы');
    boxes[0].checked = true;
    dispatch(boxes[0], 'change');
    eq(p.groups(), ['user'], 'отметка группы попала в черновик');
    eq(changes, 1, 'об изменении сообщено наружу');

    boxes[0].checked = false;
    dispatch(boxes[0], 'change');
    eq(p.groups(), [], 'снятие галочки тоже видно');

    const clear = p.node.querySelector('.voice-scope-clear');
    boxes[2].checked = true;
    dispatch(boxes[2], 'change');
    ok(p.peers().length === 1, 'беседа выбрана');
    dispatch(clear, 'click');
    eq(p.peers(), [], '«Снять всё» очищает беседы');
    eq(p.groups(), [], '«Снять всё» очищает и группы');
}

// ── 5. форма выключается целиком, а не прячется ─────────────────────────────

{
    const p = make({ initialPeers: ['1'] });
    await flush();
    await flush();
    p.setDisabled(true);

    const boxes = rowsOf(p.node, 'voice-scope-item').map((r) => r.querySelector('input'));
    ok(boxes.length > 0, 'строки остались на экране');
    ok(boxes.every((b) => b.disabled === true), 'все галочки недоступны');
    eq(p.node.querySelector('.voice-scope-search').disabled, true, 'поиск недоступен');
    eq(p.node.querySelector('.voice-scope-prune').disabled, true, 'чистка недоступна');
    eq(p.node.querySelector('.voice-scope-clear').disabled, true, '«Снять всё» недоступно');
    eq(p.peers(), ['1'], 'выключение НЕ трогает сам выбор');

    p.setDisabled(false);
    const back = rowsOf(p.node, 'voice-scope-item').map((r) => r.querySelector('input'));
    ok(back.every((b) => b.disabled === false), 'включение возвращает доступность');
    eq(p.node.querySelector('.voice-scope-search').disabled, false, 'поиск снова доступен');
}

// ── 6. поиск фильтрует список, не трогая выбор ──────────────────────────────

{
    const p = make({ initialPeers: ['2'] });
    await flush();
    await flush();
    const search = p.node.querySelector('.voice-scope-search');
    search.value = 'перв';
    dispatch(search, 'input');
    ok(String(p.node.textContent).includes('Первая'), 'найденное показано');
    ok(!String(p.node.textContent).includes('Вторая'), 'непопавшее скрыто');
    eq(p.peers(), ['2'], 'выбор при поиске не меняется');
}

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED (frontend/settings_peer_picker)`);
    process.exit(1);
}
console.log(`\n${checks}/${checks} checks passed (frontend/settings_peer_picker)`);
