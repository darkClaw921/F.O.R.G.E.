// Регресс-тест колонки бесед панели VK (js/vk/dialogs.js) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ: core/dom.js резолвит ref'ы один раз на верхнем
// уровне, и модуль, загруженный раньше стаба, увидел бы одни null.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. «Директория» у VK — это СЕТЕВОЙ ЗАПРОС, а не
// фильтр загруженного массива, и почти все возможные ошибки колонки растут
// именно отсюда:
//
//   1. переключение фильтров не чаще одного запроса в DIR_DEBOUNCE_MS —
//      иначе прощёлкивание колонки съедает весь бюджет 3 rps;
//   2. БЕЙДЖ НЕ МЕНЯЕТСЯ при переходе на ~/important: он считается по списку
//      filter=all, а не по транзиентной выборке (иначе число прыгало бы от
//      того, куда пользователь ткнул);
//   3. ответ покинутого фильтра не дорисовывается поверх нового;
//   4. данные собеседника попадают в DOM только через textContent;
//   5. повторный initDialogs не удваивает слушателей.
//
// Запуск: node tmux-web/tests/frontend/vk_dialogs.test.mjs

import {
    $dirList, $dialogList, $communityList, $nodesTitle, $statusRight,
    dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

const D = await import(new URL('../../static/js/vk/dialogs.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);

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

/** Ждёт, пока пройдёт дебаунс и планировщик выпустит запрос. */
const settle = async (ms) => {
    await new Promise((r) => setTimeout(r, ms));
    await flush();
};

function dialog(id, extra) {
    return Object.assign({
        id: String(id),
        kind: 'user',
        title: `Собеседник ${id}`,
        username: `user${String(id).replace('-', '')}`,
        bot: false,
        contact: false,
        unread_count: 0,
        pinned: false,
        muted: false,
        archived: false,
        folder_id: null,
        last_message: null,
        members: null,
        important: false,
        can_write: true,
        can_write_reason: null,
    }, extra || {});
}

const ALL = [
    dialog(1234, { unread_count: 3 }),
    dialog(2000000007, { kind: 'chat', title: 'Проектная', username: null, members: 4, unread_count: 2 }),
    dialog(-55, { kind: 'group', title: 'Клуб', username: 'test_club', bot: true, muted: true, unread_count: 40 }),
];

/** Сетевой сценарий: страница бесед по фильтру + фильтры. */
function serveDialogs(byFilter) {
    setNet(async (url) => {
        if (url.startsWith('/api/vk/filters')) {
            return jsonRes(200, {
                filters: [
                    { key: 'all', title: 'Все' },
                    { key: 'unread', title: 'Непрочитанные' },
                    { key: 'important', title: 'Важные' },
                    { key: 'unanswered', title: 'Без ответа' },
                ],
            });
        }
        const filter = /filter=([a-z]+)/.exec(url);
        const key = filter ? filter[1] : 'all';
        return jsonRes(200, { dialogs: byFilter[key] || [], has_more: false });
    });
}

// ---------------------------------------------------------------------------
// Загрузка списка и отрисовка.
// ---------------------------------------------------------------------------

serveDialogs({ all: ALL, important: [ALL[0]] });
D.initDialogs({ onSelect: () => {} });
await D.loadDialogs();
await settle(10);

eq(S.vkState.dialogs.length, 3, 'список загружен');
eq(netCalls.filter((c) => c.url.includes('filter=all')).length, 1, 'один запрос списка');
ok(netCalls.some((c) => c.url.startsWith('/api/vk/filters')), 'фильтры запрошены');

eq($dialogList.children.length, 2, 'узлы: человек и беседа');
eq($communityList.children.length, 1, 'сообщества отдельным списком (peer_id < 0)');
eq($nodesTitle.textContent, '// ~/all — 2 NODES', 'заголовок секции');

const row = $dialogList.children[0];
eq(row.dataset.dialogId, '1234', 'id беседы в data-атрибуте');
ok(row.textContent.includes('Собеседник 1234'), `имя в строке: ${row.textContent}`);
ok(row.textContent.includes('@user1234'), `ник приписью: ${row.textContent}`);
ok(row.textContent.includes('(3)'), 'счётчик непрочитанного');
const chatRow = $dialogList.children[1];
ok(chatRow.textContent.includes('[CHAT] 4 members'), `статус беседы: ${chatRow.textContent}`);
const clubRow = $communityList.children[0];
ok(clubRow.textContent.includes('[BOT]'), 'сообщество помечено как бот');
ok(clubRow.textContent.includes('[M]'), 'заглушённое помечено');
ok(clubRow.className.includes('vk-muted'), 'и приглушено классом');

// ---------------------------------------------------------------------------
// Секция // FILTERS и бейдж.
// ---------------------------------------------------------------------------

eq($dirList.children.length, 4, 'четыре серверных фильтра');
eq($dirList.children.map((li) => li.dataset.dir), ['all', 'unread', 'important', 'unanswered'],
    'ключи фильтров');
eq($dirList.children.map((li) => li.textContent), ['~/all(5)', '~/unread(5)', '~/important', '~/unanswered'],
    'счётчик только у all/unread, заглушённые в сумму не входят');

const badgeBefore = $dirList.children[0].textContent;

// ---------------------------------------------------------------------------
// Переключение фильтра: дебаунс + один запрос.
// ---------------------------------------------------------------------------

const before = netCalls.length;
// Пользователь быстро прощёлкал три пункта.
// Слушатель делегирован на сам <ul> (строки пересоздаются при каждом
// рендере), поэтому событие доставляется списку с нужным target.
dispatch($dirList, 'click', { target: $dirList.children[1] });
dispatch($dirList, 'click', { target: $dirList.children[3] });
dispatch($dirList, 'click', { target: $dirList.children[2] });
eq(S.vkState.activeDir, 'important', 'подсветка переехала СРАЗУ, до запроса');
eq(netCalls.length, before, 'до истечения дебаунса запросов нет');

await settle(D.DIR_DEBOUNCE_MS + 50);
const dirCalls = netCalls.slice(before).filter((c) => c.url.includes('/api/vk/dialogs?'));
eq(dirCalls.length, 1, 'три клика подряд — ОДИН запрос (дебаунс 300 мс)');
ok(dirCalls[0].url.includes('filter=important'), `запрошен последний фильтр: ${dirCalls[0].url}`);

eq(S.vkState.dirDialogs.length, 1, 'транзиентная выборка легла отдельно');
eq(S.vkState.dialogs.length, 3, 'список filter=all НЕ затёрт');
eq($dialogList.children.length, 1, 'колонка показывает выборку фильтра');

// ГЛАВНАЯ ПРОВЕРКА: бейдж считается по filter=all и не зависит от выбранного
// фильтра. Иначе «непрочитано 47» превращалось бы в «2» от одного клика.
eq($dirList.children[0].textContent, badgeBefore, 'счётчик ~/all не изменился');
eq($dirList.children[1].textContent, '~/unread(5)', 'счётчик ~/unread не изменился');

// Возврат на ~/all запроса не стоит — список уже есть.
const beforeAll = netCalls.length;
dispatch($dirList, 'click', { target: $dirList.children[0] });
await settle(D.DIR_DEBOUNCE_MS + 50);
eq(netCalls.length, beforeAll, '~/all рисуется из уже загруженного списка, без запроса');
eq($dialogList.children.length, 2, 'вернулись все узлы');

// ---------------------------------------------------------------------------
// Ответ покинутого фильтра не должен дорисоваться.
// ---------------------------------------------------------------------------

{
    let release = null;
    setNet((url) => {
        if (url.startsWith('/api/vk/filters')) return Promise.resolve(jsonRes(200, { filters: [] }));
        if (url.includes('filter=unanswered')) {
            return new Promise((res) => { release = () => res(jsonRes(200, { dialogs: [dialog(777)] })); });
        }
        return Promise.resolve(jsonRes(200, { dialogs: [], has_more: false }));
    });

    D.selectDir('unanswered');
    await settle(D.DIR_DEBOUNCE_MS + 50);
    ok(typeof release === 'function', 'запрос ушедшего фильтра в полёте');

    // Пользователь передумал и вернулся на ~/all, ответ доезжает ПОСЛЕ.
    D.selectDir('all');
    release();
    await settle(20);
    eq(S.vkState.activeDir, 'all', 'активен ~/all');
    eq(S.vkState.dirDialogs.length, 0, 'ответ покинутого фильтра ОТБРОШЕН по поколению');
    eq($dialogList.children.length, 2, 'колонка показывает список all');
}

// ---------------------------------------------------------------------------
// Точечные обновления.
// ---------------------------------------------------------------------------

serveDialogs({ all: ALL });
D.setUnread('1234', 0);
eq($dialogList.children[0].textContent.includes('(3)'), false, 'счётчик обнулён точечно');
eq($dirList.children[0].textContent, '~/all(2)', 'счётчик фильтра пересчитан');

eq(D.bumpUnread('1234'), 1, 'инкремент непрочитанного');
D.onRead({ dialog_id: '1234', direction: 'in', max_id: 10 });
eq(S.vkState.dialogs[0].unread_count, 0, 'read с direction=in обнуляет');
D.onRead({ dialog_id: '1234', direction: 'out', max_id: 10 });
eq(S.vkState.dialogs[0].unread_count, 0, 'read с direction=out колонку не трогает');

eq(D.onNewMessage({ dialog_id: '1234', out: false, id: 5 }), true, 'входящее поднимает счётчик');
eq(S.vkState.dialogs[0].unread_count, 1, 'счётчик поднялся');
eq(D.onNewMessage({ dialog_id: '1234', out: true, id: 6 }), false, 'своё исходящее — нет');

// Присутствие.
eq(D.onOnline({ user_id: '1234', online: true }), true, 'статус применён к строке');
ok($dialogList.children[0].textContent.includes('[ONLINE]'), 'строка показывает ONLINE');
eq($statusRight.textContent, '1 ONLINE', 'счётчик в статусбаре');
eq(D.presenceOf('1234').online, true, 'присутствие доступно снаружи');
eq(D.presenceOf('нет такого'), null, 'о незнакомце ничего не утверждаем');
// Событие про беседу (не человека) не должно давать «+1 в сети».
D.onOnline({ user_id: '2000000007', online: true });
eq($statusRight.textContent, '1 ONLINE', 'беседа в счётчик присутствия не попадает');

D.onMuted({ dialog_id: '1234', muted: true });
ok($dialogList.children[0].textContent.includes('[M]'), 'метка mute появилась точечно');
D.onMuted({ dialog_id: '1234', muted: false });
ok(!$dialogList.children[0].textContent.includes('[M]'), 'и снялась');

// ---------------------------------------------------------------------------
// Выбор беседы и XSS.
// ---------------------------------------------------------------------------

const selected = [];
D.initDialogs({ onSelect: (id) => selected.push(id) });
dispatch($dialogList.children[0], 'click', { target: $dialogList.children[0] });
eq(selected, ['1234'], 'клик выбирает беседу');
dispatch($dialogList.children[0], 'click', { target: $dialogList.children[0] });
eq(selected, ['1234'], 'повторный клик по активной — no-op (не перезагружаем историю)');
ok($dialogList.children[0].className.includes('active'), 'активная строка подсвечена');
D.clearActive();
ok(!$dialogList.children[0].className.includes('active'), 'подсветка снята');

// Повторный initDialogs не удваивает слушателей: иначе один клик по фильтру
// давал бы два запроса.
{
    D.initDialogs({ onSelect: (id) => selected.push(id) });
    D.initDialogs({ onSelect: (id) => selected.push(id) });
    const n = netCalls.length;
    dispatch($dirList, 'click', { target: $dirList.children[2] });
    await settle(D.DIR_DEBOUNCE_MS + 50);
    const calls = netCalls.slice(n).filter((c) => c.url.includes('/api/vk/dialogs?'));
    eq(calls.length, 1, 'повторный initDialogs не удваивает обработчики');
    D.selectDir('all');
    await settle(10);
}

// Имя собеседника с разметкой обязано доехать ТЕКСТОМ.
{
    const evil = '<img src=x onerror=alert(1)>';
    S.vkState.dialogs = [dialog(4242, { title: evil, username: null })];
    S.vkState.activeDir = 'all';
    D.renderAll();
    const li = $dialogList.children[0];
    eq(li.title, evil, 'полное имя — в title (атрибут не парсится как HTML)');
    // Разметка не превратилась в узлы: у строки ровно два элемента-потомка
    // (имя и статус), сколько бы тегов ни было во «вредном» имени.
    eq(li.children.length, 2, 'из имени не выросло ни одного лишнего узла');
    // Имя теперь показывается КАК ЕСТЬ, поэтому «onerror=» в тексте строки
    // законно: важно, что это ТЕКСТ. Проверяем именно это — узел имени
    // содержит только текстовые потомки.
    const nameEl = li.children[0];
    ok(nameEl.textContent.includes(evil), `разметка доехала текстом: ${nameEl.textContent}`);
    ok(nameEl.children.every((c) => c.nodeType === 1 && c.children.length === 0),
        'внутри имени нет разобранных из строки элементов');
}

// ---------------------------------------------------------------------------
// Сброс.
// ---------------------------------------------------------------------------

D.resetDialogs();
eq($dialogList.children.length, 0, 'колонка очищена');
eq($communityList.children.length, 0, 'сообщества очищены');
eq($statusRight.textContent, '', 'счётчик присутствия сброшен');
eq(D._debugDialogsState().presence, 0, 'присутствие прошлого аккаунта забыто');

console.log(`vk_dialogs: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
