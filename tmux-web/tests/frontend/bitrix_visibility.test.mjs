// Регресс-тест белого списка диалогов «Битрикс24» (Ф4, финализация в Ф11).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/bitrix/visibility.js через
// `await import()`. Фейковый DOM не нужен: модуль чистый и зависит только от
// `format.js`.
//
// Что покрыто:
//   • разбор настроек в рабочий фильтр и гейт `=== true` (дефолт — ВЫКЛЮЧЕНО);
//   • правило «включён, но ничего не выбрано ⇒ показываем всё»;
//   • видимость по виду и поимённо, приоритет поимённого выбора;
//   • КОНСЕРВАТИВНОЕ решение по диалогу вне загруженного списка — главное
//     отличие от VK, где тип выводится из самого идентификатора;
//   • применение фильтра к СЧЁТЧИКАМ (директории и бейдж), а не только к
//     колонке: пропуск этой точки и есть «протечка скрытого чата»;
//   • сводка со склонениями и чистка исчезнувших диалогов;
//   • ЧЕТЫРЕ ТОЧКИ ПРИМЕНЕНИЯ ЦЕЛИКОМ (Ф11) — на живых модулях панели, а не на
//     чистых предикатах: перечень читается ИЗ ШАПКИ visibility.js, и точка,
//     добавленная туда без покрытия здесь, роняет тест.
//
// Запуск: node tmux-web/tests/frontend/bitrix_visibility.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// Стаб DOM обязан стоять ПЕРВЫМ: `core/dom.js` резолвит все ref'ы один раз при
// импорте, а разделу «четыре точки» нужны живые dialogs.js и main.js.
import { readFileSync } from 'node:fs';
import {
    $dialogList, $dirCounts, $panel, $toasts, $unread,
} from './bitrix_dom_stub.mjs';

const userSettings = {};
globalThis.window.ForgeApp = { state: { userSettings } };

const modUrl = new URL('../../static/js/bitrix/visibility.js', import.meta.url).href;
const V = await import(modUrl);
const formatUrl = new URL('../../static/js/bitrix/format.js', import.meta.url).href;
const F = await import(formatUrl);
const STATE = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);
const DIALOGS = await import(new URL('../../static/js/bitrix/dialogs.js', import.meta.url).href);
const MAIN = await import(new URL('../../static/js/bitrix/main.js', import.meta.url).href);

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

/** Настройки в том виде, в каком их отдаёт /api/user-settings. */
function settings(enabled, kinds, peers) {
    return {
        bitrix_filter_enabled: enabled,
        bitrix_visible_kinds: kinds || [],
        bitrix_visible_peers: peers || [],
    };
}

/** Загруженный список: личный диалог, чат, чат задачи, группа. */
const ALL = [
    { id: '19', kind: 'user', unread_count: 3 },
    { id: 'chat3', kind: 'chat', unread_count: 1 },
    { id: 'chat7', kind: 'task', unread_count: 5, task_id: '1' },
    { id: 'sg12', kind: 'sg', unread_count: 2 },
];

const ids = (list) => list.map((d) => d.id);

// ---------------------------------------------------------------------------
// Разбор настроек
// ---------------------------------------------------------------------------

{
    const off = V.normalizeFilter(null);
    ok(!off.enabled && !off.active, 'без настроек фильтр выключен');
    eq(off.kinds, [], 'без настроек виды пусты');

    // Гейт именно `=== true`: дефолт этой настройки — ВЫКЛЮЧЕНО, в отличие от
    // bitrix_hotkeys_enabled, где дефолт включён.
    ok(!V.normalizeFilter(settings(undefined, ['user'])).enabled, 'undefined ⇒ выключено');
    ok(!V.normalizeFilter(settings('yes', ['user'])).enabled, 'строка «yes» ⇒ выключено');
    ok(!V.normalizeFilter(settings(1, ['user'])).enabled, 'единица ⇒ выключено');
    ok(V.normalizeFilter(settings(true, ['user'])).enabled, 'true ⇒ включено');

    const dirty = V.normalizeFilter(settings(true, ['user', 'user', '', 'task', 42], ['19', ' 19 ', 'chat3']));
    eq(dirty.kinds, ['user', 'task', '42'], 'дубли и пустые убраны, порядок сохранён');
    eq(dirty.peers, ['19', 'chat3'], 'идентификаторы триммятся и дедуплицируются');
    ok(dirty.kindSet.has('task') && dirty.peerSet.has('chat3'), 'наборы собраны');

    // ВКЛЮЧЁН — ещё не ЗНАЧИТ ДЕЙСТВУЕТ.
    const empty = V.normalizeFilter(settings(true, [], []));
    ok(empty.enabled && !empty.active, 'включён с пустым выбором — не действует');
    ok(!V.filterActive(empty), 'filterActive требует непустого выбора');
    ok(!V.filterActive(null), 'null — не фильтр');
    eq(ids(V.visibleDialogs(ALL, empty)), ids(ALL), 'пустой выбор показывает ВСЁ, а не ничего');
}

// ---------------------------------------------------------------------------
// Виды: три «пользовательских» вместо семи внутренних
// ---------------------------------------------------------------------------

{
    eq(V.VISIBLE_KINDS.map((k) => k.key), ['user', 'chat', 'task'], 'три вида в форме настроек');
    eq(V.filterKind({ kind: 'user' }), 'user', 'личный диалог');
    eq(V.filterKind({ kind: 'task' }), 'task', 'чат задачи');
    eq(V.filterKind({ kind: 'chat' }), 'chat', 'групповой чат');
    // Редкие виды портала сводятся к «чатам»: отдельные галочки под них
    // требовали бы от пользователя знания внутренней терминологии.
    for (const kind of ['sg', 'openline', 'copilot', 'collab', 'что-то новое']) {
        eq(V.filterKind({ kind }), 'chat', `${kind} считается чатом`);
    }
    // Словарь видов — общий с format.js: разъезд означал бы, что фильтр
    // молча перестал совпадать с колонкой.
    ok(F.DIALOG_KINDS.includes('task') && F.DIALOG_KINDS.includes('user'),
        'словарь видов format.js содержит виды белого списка');
    eq(V.kindLabel('task'), 'Чаты задач', 'подпись вида для формы настроек');
    eq(V.kindLabel('нет такого'), 'нет такого', 'незнакомый вид отдаёт сам себя');
}

// ---------------------------------------------------------------------------
// Видимость по виду и поимённо
// ---------------------------------------------------------------------------

{
    const byKind = V.normalizeFilter(settings(true, ['task']));
    eq(ids(V.visibleDialogs(ALL, byKind)), ['chat7'], 'по виду видны только чаты задач');
    ok(!V.dialogVisible({ id: '19', kind: 'user' }, byKind), 'личный диалог скрыт');

    const byPeer = V.normalizeFilter(settings(true, [], ['19']));
    eq(ids(V.visibleDialogs(ALL, byPeer)), ['19'], 'поимённый выбор без видов');

    // Поимённый выбор СИЛЬНЕЕ вида: отмеченный вручную диалог виден, даже если
    // его вид не отмечен.
    const both = V.normalizeFilter(settings(true, ['task'], ['19']));
    eq(ids(V.visibleDialogs(ALL, both)), ['19', 'chat7'], 'поимённый выбор добавляется к виду');

    // Порядок сервера сохраняется, массив — новый.
    const out = V.visibleDialogs(ALL, both);
    ok(out !== ALL, 'фильтрация возвращает НОВЫЙ массив');
    ok(!V.dialogVisible(null, both), 'пустая строка не видна');
    ok(!V.dialogVisible({ id: '' }, both), 'строка без id не видна');
    ok(V.dialogVisible({ id: '19' }, V.normalizeFilter(null)), 'без фильтра видно всё');
}

// ---------------------------------------------------------------------------
// Диалог вне загруженного списка — КОНСЕРВАТИВНО
// ---------------------------------------------------------------------------

{
    const byKind = V.normalizeFilter(settings(true, ['chat']));

    // У Битрикс24 вид НЕ выводится из идентификатора: "chat7" — это и
    // обычный чат, и чат задачи. Значит про незагруженный диалог ничего
    // не известно, и при действующем фильтре он СКРЫТ.
    ok(!V.dialogIdVisible('chat99', byKind, ALL), 'незнакомый диалог скрыт (консервативно)');
    ok(V.dialogIdVisible('chat3', byKind, ALL), 'знакомый чат виден по виду');
    ok(!V.dialogIdVisible('chat7', byKind, ALL), 'знакомый чат задачи скрыт по виду');
    ok(!V.dialogIdVisible('', byKind, ALL), 'кадр без адреса скрыт');
    ok(!V.dialogIdVisible(null, byKind, ALL), 'null скрыт');

    // Поимённый выбор работает и без загруженного списка: id известен.
    const byPeer = V.normalizeFilter(settings(true, [], ['chat99']));
    ok(V.dialogIdVisible('chat99', byPeer, []), 'отмеченный вручную виден без списка');
    ok(V.dialogIdVisible('chat99', byPeer, null), 'null вместо списка не роняет');

    // Без действующего фильтра — видно всё, даже незнакомое.
    ok(V.dialogIdVisible('chat99', V.normalizeFilter(null), []), 'без фильтра виден любой');
}

// ---------------------------------------------------------------------------
// ФИЛЬТР ПРИМЕНЯЕТСЯ К СЧЁТЧИКАМ, А НЕ ТОЛЬКО К КОЛОНКЕ
// ---------------------------------------------------------------------------

{
    const byKind = V.normalizeFilter(settings(true, ['task']));

    // Точка 3 из перечня в шапке модуля: бейдж считается по ВИДИМЫМ строкам,
    // серверный total при действующем фильтре непригоден.
    eq(V.visibleUnread(ALL, byKind), 5, 'бейдж считает только видимые чаты');
    eq(V.visibleUnread(ALL, V.normalizeFilter(null)), 11, 'без фильтра — все строки');
    eq(V.visibleUnread([], byKind), 0, 'пустой список даёт ноль');
    eq(V.visibleUnread([{ id: 'chat7', kind: 'task', unread_count: 'мусор' }], byKind), 0,
        'нечисло не ломает сумму');

    // Точка 2: счётчики директорий считаются по тому же видимому списку.
    const visible = V.visibleDialogs(ALL, byKind);
    eq(F.dirCounts(visible), { all: 1, chats: 0, tasks: 1, unread: 1 },
        'директории считаются по видимым строкам');
    // Без фильтра те же директории видят весь список — значит фильтр реально
    // участвует в счётчиках, а не просто «не мешает».
    eq(F.dirCounts(ALL), { all: 4, chats: 3, tasks: 1, unread: 4 }, 'без фильтра — весь список');
}

// ---------------------------------------------------------------------------
// Сводка и чистка
// ---------------------------------------------------------------------------

{
    eq(V.filterSummary(null), 'Показаны все диалоги.', 'без фильтра');
    eq(V.filterSummary(V.normalizeFilter(settings(false, ['user']))), 'Показаны все диалоги.',
        'выключенный фильтр');
    eq(V.filterSummary(V.normalizeFilter(settings(true, [], []))),
        'Ничего не выбрано — пока показаны все диалоги.', 'включён, но пусто');
    eq(V.filterSummary(V.normalizeFilter(settings(true, ['user']))),
        'Показаны только: 1 вид.', 'склонение: один вид');
    eq(V.filterSummary(V.normalizeFilter(settings(true, ['user', 'task']))),
        'Показаны только: 2 вида.', 'склонение: два вида');
    eq(V.filterSummary(V.normalizeFilter(settings(true, ['user', 'task', 'chat'], ['19', 'chat3', 'chat7', 'sg12', '20']))),
        'Показаны только: 3 вида и 5 диалогов.', 'склонение: пять диалогов');
    eq(V.filterSummary(V.normalizeFilter(settings(true, [], ['19']))),
        'Показаны только: 1 диалог.', 'склонение: один диалог');

    eq(V.pruneMissingPeers(['19', 'chat404', 'chat7'], ALL), ['19', 'chat7'],
        'чистка убирает исчезнувшие, порядок сохраняется');
    eq(V.pruneMissingPeers(['19'], []), [], 'пустой список диалогов убирает всё');
    eq(V.pruneMissingPeers(null, ALL), [], 'null не роняет чистку');
    eq(V.pruneMissingPeers(['19', '19'], ALL), ['19'], 'дубль схлопывается');
}

// ---------------------------------------------------------------------------
// ЧЕТЫРЕ ТОЧКИ ПРИМЕНЕНИЯ (Ф11)
//
// Перечень точек продублирован в ШАПКЕ js/bitrix/visibility.js — намеренно:
// модуль чистый и сам ни одной из них не выполняет, а пропуск любой означает
// протечку скрытого диалога (пользователь просил не показывать чат, а тот всё
// равно звенит тостом или считается в бейдже). Тест читает перечень ОТТУДА и
// требует, чтобы каждая точка была покрыта здесь: новая точка, дописанная в
// шапку без покрытия, роняет тест — ровно этого от дублирования и ждут.
// ---------------------------------------------------------------------------

/** Точки, перечисленные в шапке visibility.js: `{ n, title, ref }`. */
function declaredPoints() {
    const src = readFileSync(new URL('../../static/js/bitrix/visibility.js', import.meta.url), 'utf8');
    const from = src.indexOf('ВСЕ ТОЧКИ ПРИМЕНЕНИЯ');
    ok(from > 0, 'шапка visibility.js содержит перечень точек применения');
    const head = src.slice(from, src.indexOf('ЧЕМ БИТРИКС24 ОТЛИЧАЕТСЯ'));
    const out = [];
    for (const m of head.matchAll(/^\/\/\s{2,}(\d)\.\s+([^—]+)—\s*(\S+)/gm)) {
        out.push({ n: Number(m[1]), title: m[2].trim(), ref: m[3].trim() });
    }
    return out;
}

{
    const points = declaredPoints();
    eq(points.map((p) => p.n), [1, 2, 3, 4],
        'в шапке visibility.js перечислены ровно четыре точки, по порядку');

    // Каждая ссылка ведёт в реальную функцию реального модуля: переименование
    // без правки шапки — это разъехавшаяся документация, то есть та же
    // непокрытая точка, только тихая.
    for (const p of points) {
        const m = /^js\/bitrix\/([a-z]+)\.js::([A-Za-z]+)/.exec(p.ref);
        ok(m !== null, `точка ${p.n}: ссылка разобрана (${p.ref})`);
        if (!m) continue;
        const src = readFileSync(
            new URL(`../../static/js/bitrix/${m[1]}.js`, import.meta.url), 'utf8',
        );
        ok(src.includes(`function ${m[2]}(`),
            `точка ${p.n}: функция ${m[2]}() существует в ${m[1]}.js`);
    }

    // И каждая точка проверена НИЖЕ по факту поведения.
    const covered = ['КОЛОНКА', 'СЧЁТЧИКИ', 'БЕЙДЖ', 'ТОСТЫ'];
    for (let i = 0; i < points.length; i += 1) {
        ok(points[i].title.toUpperCase().startsWith(covered[i]),
            `точка ${points[i].n} («${points[i].title}») покрыта проверкой ниже`);
    }
}

/** Ставит настройки белого списка и сбрасывает кэш разбора. */
function applyFilter(enabled, kinds, peers) {
    userSettings.bitrix_filter_enabled = enabled;
    userSettings.bitrix_visible_kinds = kinds || [];
    userSettings.bitrix_visible_peers = peers || [];
    STATE.invalidateVisibilityFilter();
}

{
    // Список: личный диалог (3 непрочитанных), чат (1), чат задачи (5).
    STATE.b24State.dialogs = [
        { id: '19', kind: 'user', unread_count: 3, title: 'Иванов' },
        { id: 'chat3', kind: 'chat', unread_count: 1, title: 'Общий' },
        { id: 'chat7', kind: 'task', unread_count: 5, title: 'Задача', task_id: '1' },
    ];
    STATE.b24State.activeDir = 'all';
    // Серверный счётчик заведомо БОЛЬШЕ суммы видимого: при действующем
    // фильтре он обязан игнорироваться целиком.
    STATE.b24State.counters = {
        total: 99, chat: 0, dialog: 0, tasks: 0, notify: 0,
        expired: 0, new_comments: 0, mentioned: 0,
    };
    // Четыре пункта директорий приходят из стаба — ровно как из index.html.
    // `initDialogs` обязателен: ячейки счётчиков он берёт из core/dom.js и
    // раскладывает по карте ОДИН раз, а без неё renderDirs пишет в никуда.
    DIALOGS.initDialogs({ onSelect: () => {}, onDir: () => {} });

    // ── БЕЗ ФИЛЬТРА: видно всё ──
    applyFilter(false);
    DIALOGS.renderAll();
    eq($dialogList.querySelectorAll('.b24-dialog').length, 3, 'без фильтра: в колонке все три');
    MAIN.recomputeUnread();
    eq($unread.textContent, '99',
        'без фильтра: бейдж берёт СЕРВЕРНЫЙ счётчик, а не сумму страницы');

    // ── ТОЧКА 1: КОЛОНКА ──
    applyFilter(true, ['user']);
    DIALOGS.renderAll();
    const shown = $dialogList.querySelectorAll('.b24-dialog').map((li) => li.dataset.dialogId);
    eq(shown, ['19'], 'точка 1 (колонка): показан только разрешённый вид');

    // ── ТОЧКА 2: СЧЁТЧИКИ ДИРЕКТОРИЙ ──
    DIALOGS.renderDirs();
    eq($dirCounts.all.textContent, '1',
        'точка 2 (счётчики): ~/all считает ВИДИМЫЕ строки, а не загруженные');

    // Чат задачи скрыт — директория задач обязана обнулиться, иначе счётчик
    // сообщал бы о чатах, которых в колонке нет.
    eq($dirCounts.tasks.textContent, '',
        'точка 2 (счётчики): скрытый чат задачи не попадает в ~/tasks');

    // ── ТОЧКА 3: БЕЙДЖ ВКЛАДКИ ──
    MAIN.recomputeUnread();
    eq($unread.textContent, '3',
        'точка 3 (бейдж): при действующем фильтре серверный total игнорируется,'
        + ' считаются непрочитанные ВИДИМЫХ диалогов');
    eq($unread.hidden, false, 'точка 3 (бейдж): показан');

    // Всё скрыто — бейдж гаснет целиком, хотя портал по-прежнему присылает 99.
    applyFilter(true, [], ['нет-такого']);
    MAIN.recomputeUnread();
    eq($unread.hidden, true, 'точка 3 (бейдж): не видно ни одного диалога — бейдж скрыт');

    // ── ТОЧКА 4: ТОСТЫ ──
    applyFilter(true, ['user']);
    const frameFor = (dialogId) => ({
        dialog_id: dialogId,
        muted: false,
        message: {
            id: Date.now(), dialog_id: dialogId, out: false, system: false,
            text: 'входящее', sender: { id: '2', name: 'Петров' },
        },
    });
    $toasts.childNodes.slice().forEach((n) => n.remove());
    $panel.hidden = true; // вкладка не открыта — тост в принципе возможен

    // `ok(... === null)`, а не `eq(..., null)`: при провале eq сериализовал бы
    // УЗЕЛ тоста, а у него циклическая ссылка на родителя — тест падал бы
    // исключением JSON.stringify вместо внятного [FAIL].
    ok(MAIN.maybeToast(frameFor('chat3')) === null,
        'точка 4 (тосты): скрытый диалог не всплывает уведомлением');
    ok(MAIN.maybeToast(frameFor('19')) !== null,
        'точка 4 (тосты): видимый диалог уведомление показывает');

    // Диалога нет в загруженном списке — решение КОНСЕРВАТИВНОЕ: скрыт.
    ok(MAIN.maybeToast(frameFor('chat404')) === null,
        'точка 4 (тосты): неизвестный диалог при действующем фильтре скрыт');

    // Выключенный фильтр возвращает всё — в том числе тосты.
    applyFilter(false);
    ok(MAIN.maybeToast(frameFor('chat404')) !== null,
        'без фильтра: неизвестный диалог уведомление показывает');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_visibility)`);
if (failures > 0) process.exit(1);
