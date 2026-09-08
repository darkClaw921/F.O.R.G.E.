// Регресс-тест МУЛЬТИПОРТАЛЬНОСТИ панели «Битрикс24» (Ф8) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в bitrix_dom_stub.mjs.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ:
//
//   1. АДРЕСАЦИЯ. Забытый `X-Forge-Portal` не даёт НИ ошибки, НИ 404: сервер
//      молча берёт первый слот реестра, и запрос уходит не в тот портал.
//      Симптом — «в чужой вкладке чужая переписка», диагноз по нему не
//      ставится.
//   2. МАРШРУТИЗАЦИЯ КАДРОВ. Сокет один на все порталы. Кадр соседа, попавший
//      в рендер ленты, показал бы чужое сообщение в открытом чате — и это
//      выглядело бы не как ошибка, а как настоящее сообщение.
//   3. БЕЙДЖ СОСЕДА. Он единственное, что видит пользователь про фоновый
//      портал; не вырасти он — событие пропадает молча.
//   4. ПУСТОЙ TITLE. Вкладка обязана подписываться `id`: хоста портала бэкенд
//      не отдаёт вовсе, и безымянная вкладка была бы неотличима от соседней.
//   5. ПАМЯТЬ ДИАЛОГА. Полного кэша состояния на портал НЕТ намеренно, и
//      единственное, что переживает переключение, — последний открытый диалог.
//   6. ПОРЯДОК. Перестановка шлёт ПОЛНЫЙ список id, а не позицию.
//
// Запуск: node tmux-web/tests/frontend/bitrix_portals.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $portalTabs, flush, jsonRes, netCalls, setNet,
} from './bitrix_dom_stub.mjs';

const P = await import(new URL('../../static/js/bitrix/portals.js', import.meta.url).href);
const A = await import(new URL('../../static/js/bitrix/api.js', import.meta.url).href);
const W = await import(new URL('../../static/js/bitrix/ws.js', import.meta.url).href);
const S = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) {
        console.log(`[ok]   ${label}`);
        return;
    }
    failures += 1;
    console.log(`[FAIL] ${label}\n       actual:   ${a}\n       expected: ${e}`);
}

function ok(cond, label) {
    eq(!!cond, true, label);
}

/** Строка реестра в форме `PortalDto`. */
function portal(id, extra = {}) {
    return {
        id,
        title: '',
        connected: true,
        login: { state: 'authorized' },
        unread: 0,
        ...extra,
    };
}

// ---------------------------------------------------------------------------
// 1. Чистая логика вкладок — без DOM и без сети
// ---------------------------------------------------------------------------

eq(P.portalLabel(portal('acme')), 'acme', 'пустой title показывается как id');
eq(P.portalLabel(portal('acme', { title: '  ' })), 'acme', 'title из пробелов — тоже id');
eq(P.portalLabel(portal('acme', { title: 'Работа' })), 'Работа', 'непустой title побеждает id');

eq(
    P.truncateLabel('коротко', 18),
    'коротко',
    'короткая подпись не трогается',
);
eq(
    P.truncateLabel('очень-длинное-имя-портала', 10),
    'очень-дли…',
    'длинная подпись усекается под ширину вкладки',
);

eq(
    P.nextActiveId([portal('a'), portal('b')], 'b'),
    'b',
    'активный портал сохраняется, пока он в реестре',
);
eq(
    P.nextActiveId([portal('a'), portal('b')], 'ушёл'),
    'a',
    'исчезнувший портал заменяется первым в реестре',
);
eq(P.nextActiveId([], 'a'), '', 'пустой реестр — активного портала нет');

eq(P.orderOf([portal('a'), portal('b')]), ['a', 'b'], 'порядок вкладок = порядок реестра');
eq(
    P.movedOrder(['a', 'b', 'c'], 'c', 'a'),
    ['c', 'a', 'b'],
    'перетаскивание влево ставит вкладку на место цели',
);
eq(
    P.movedOrder(['a', 'b', 'c'], 'a', 'c'),
    ['b', 'c', 'a'],
    'перетаскивание вправо ставит вкладку на место цели',
);
eq(
    P.movedOrder(['a', 'b'], 'нет-такого', 'a'),
    ['a', 'b'],
    'перетаскивание неизвестной вкладки ничего не меняет',
);

eq(
    P.computeTabs([portal('a', { unread: 5 })], 'a', new Map([['a', 2]])),
    [{
        id: 'a', label: 'a', title: '', unread: 2, active: true,
        connected: true, state: 'authorized',
    }],
    'живой счётчик перекрывает снимок unread из DTO',
);
eq(
    P.computeTabs([portal('a', { connected: false, login: { state: 'throttled' } })], '', new Map())[0],
    {
        id: 'a', label: 'a', title: '', unread: 0, active: false,
        connected: false, state: 'throttled',
    },
    'состояние подключения доезжает до вкладки',
);

// ---------------------------------------------------------------------------
// 2. Маршрутизация кадров по portal_id (ws.js::frameRoute)
// ---------------------------------------------------------------------------

eq(W.frameRoute({ type: 'ping', portal_id: '_socket' }, 'a'), 'socket', 'ping — служебный кадр сокета');
eq(W.frameRoute({ type: 'portals', portal_id: '_socket' }, 'a'), 'socket', 'portals — служебный кадр сокета');
eq(W.frameRoute({ type: 'message_new', portal_id: 'a' }, 'a'), 'active', 'кадр своего портала идёт в рендер');
eq(W.frameRoute({ type: 'message_new', portal_id: 'b' }, 'a'), 'foreign', 'кадр соседа в рендер НЕ идёт');
eq(W.frameRoute({ type: 'message_new' }, 'a'), 'invalid', 'кадр без portal_id — ошибка протокола');
eq(W.frameRoute({ type: 'message_new', portal_id: '' }, 'a'), 'invalid', 'пустой portal_id — тоже ошибка');
eq(
    W.frameRoute({ type: 'message_new', portal_id: 'b' }, ''),
    'active',
    'пока активный портал не выбран, все кадры считаются своими',
);
eq(
    W.frameRoute({ type: 'login_state', portal_id: '_vacant' }, 'a'),
    'active',
    'слот без портала рисует экран подключения, а не бейдж соседа',
);

// ---------------------------------------------------------------------------
// 3. Загрузка состава: заголовок, активный портал, рендер ряда
// ---------------------------------------------------------------------------

netCalls.length = 0;
setNet(async (url) => {
    if (url.startsWith('/api/bitrix/portals')) {
        return jsonRes(200, {
            portals: [
                portal('work', { title: 'Работа', unread: 3 }),
                portal('acme'),
            ],
        });
    }
    return jsonRes(200, {});
});

const switched = [];
P.initB24Portals({
    onSwitch: (id, back) => switched.push([id, back]),
    onNotice: () => {},
});
await P.loadPortals();
await flush();

eq(P.activePortalId(), 'work', 'первым активным становится первый слот реестра');
eq(A.activePortal(), 'work', 'api.js узнал активный портал');
eq(switched, [], 'ПЕРВЫЙ выбор портала не считается переключением — reset не нужен');
eq($portalTabs.children.length, 2, 'ряд нарисовал обе вкладки');
eq(
    $portalTabs.children.map((li) => li.dataset.portalId),
    ['work', 'acme'],
    'порядок вкладок = порядок реестра',
);
eq(
    $portalTabs.children[0].children.map((n) => n.textContent).filter(Boolean),
    ['Работа', '3'],
    'вкладка показывает имя и бейдж непрочитанного из снимка',
);
ok($portalTabs.children[0].className.includes('active'), 'первая вкладка активна');

// Заголовок адресации обязан уходить с КАЖДЫМ запросом.
netCalls.length = 0;
await A.getFilters();
await flush();
eq(netCalls.length, 1, 'запрос ушёл');
eq(netCalls[0].headers['X-Forge-Portal'], 'work', 'X-Forge-Portal подставлен активным порталом');

// Явный портал параметром — то, ради чего это умеет Ф7.5.
netCalls.length = 0;
await A.getDialogs({ portal: 'acme' });
await flush();
eq(
    netCalls[0].headers['X-Forge-Portal'],
    'acme',
    'явный портал параметром побеждает активный',
);

// ---------------------------------------------------------------------------
// 4. Бейдж соседней вкладки
// ---------------------------------------------------------------------------

P.bumpPortalUnread('acme', 1);
eq(
    P.computeTabs([portal('acme')], 'work', new Map([['acme', 1]]))[0].unread,
    1,
    'бейдж соседа считается локально до прихода counters',
);
eq(
    $portalTabs.children[1].children.map((n) => n.textContent).filter(Boolean),
    ['acme', '1'],
    'бейдж неактивного портала вырос в разметке',
);
P.setPortalUnread('acme', 12);
eq(
    $portalTabs.children[1].children.map((n) => n.textContent).filter(Boolean),
    ['acme', '12'],
    'кадр counters ставит ТОЧНОЕ серверное число',
);

// ---------------------------------------------------------------------------
// 5. Переключение: память последнего диалога и полный reset снаружи
// ---------------------------------------------------------------------------

S.b24State.activeDialogId = 'chat7';
P.switchPortal('acme');
await flush();

eq(P.activePortalId(), 'acme', 'активным стал выбранный портал');
eq(A.activePortal(), 'acme', 'заголовок адресации переехал вместе с вкладкой');
eq(switched, [['acme', '']], 'панель получила команду на reset+reload, диалога у нового портала нет');
eq(P.rememberedDialog('work'), 'chat7', 'открытый диалог прошлого портала запомнен');
eq(
    $portalTabs.children[1].children.map((n) => n.textContent).filter(Boolean),
    ['acme'],
    'бейдж вкладки, на которую перешли, обнуляется сразу',
);

// Возврат обязан вернуть пользователя в его последний диалог.
switched.length = 0;
S.b24State.activeDialogId = 'sg12';
P.switchPortal('work');
await flush();
eq(switched, [['work', 'chat7']], 'возврат отдаёт последний диалог этого портала');
eq(P.rememberedDialog('acme'), 'sg12', 'диалог второго портала тоже запомнен');

// ---------------------------------------------------------------------------
// 6. Перестановка: уезжает ПОЛНЫЙ список id
// ---------------------------------------------------------------------------

netCalls.length = 0;
setNet(async (url, init) => {
    if (url.startsWith('/api/bitrix/portals/')) return jsonRes(200, portal('acme'));
    if (url.startsWith('/api/bitrix/portals')) {
        return jsonRes(200, { portals: [portal('acme'), portal('work', { title: 'Работа' })] });
    }
    return jsonRes(200, {});
});

$portalTabs.listeners.get('dragstart')[0]({
    target: $portalTabs.children[1],
    dataTransfer: { setData() {}, effectAllowed: '' },
});
$portalTabs.listeners.get('drop')[0]({
    target: $portalTabs.children[0],
    preventDefault() {},
});
await flush(10);

const patch = netCalls.find((c) => c.method === 'PATCH');
ok(patch, 'перестановка ушла PATCH-ом');
eq(patch.url, '/api/bitrix/portals/acme', 'адресатом PATCH стал перетащенный портал');
eq(patch.body, { order: ['acme', 'work'] }, 'уехал ПОЛНЫЙ список id, а не позиция');
eq(patch.headers['X-Forge-Portal'], 'work', 'PATCH тоже уходит с заголовком адресации');

// ---------------------------------------------------------------------------
// 7. Исчезнувший портал: 404 unknown_portal и чистка памяти
// ---------------------------------------------------------------------------

const err = new Error('нет такого портала');
err.code = 'unknown_portal';
ok(P.handleUnknownPortal(err), 'unknown_portal распознан');

const other = new Error('иное');
other.code = 'not_connected';
eq(P.handleUnknownPortal(other), false, 'чужой код ошибки сюда не относится');

setNet(async (url) => {
    if (url.startsWith('/api/bitrix/portals')) return jsonRes(200, { portals: [portal('acme')] });
    return jsonRes(200, {});
});
switched.length = 0;
await P.loadPortals();
await flush();
eq(P.activePortalId(), 'acme', 'после исчезновения портала активным стал уцелевший');
ok(switched.length === 1 && switched[0][0] === 'acme', 'смена активного портала = переключение с reset');
eq(P.rememberedDialog('work'), '', 'память исчезнувшего портала освобождена');

// ---------------------------------------------------------------------------
// 8. Пустой список = «порталов ещё нет»
// ---------------------------------------------------------------------------

setNet(async () => jsonRes(200, { portals: [] }));
await P.loadPortals();
await flush();
eq(P.hasPortals(), false, 'пустой массив вкладок означает «порталов ещё нет»');
eq($portalTabs.children.length, 0, 'ряд пуст, но сам никуда не делся — кнопка + на месте');

// ---------------------------------------------------------------------------

console.log(`\n${checks - failures}/${checks} checks passed`);
if (failures > 0) process.exit(1);
