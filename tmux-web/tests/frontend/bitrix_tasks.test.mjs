// Регресс-тест КАРТОЧКИ ЗАДАЧИ панели «Битрикс24» (Ф10) на фейковом DOM.
//
// Грузит РЕАЛЬНЫЙ модуль `js/bitrix/tasks.js` поверх bitrix_dom_stub.mjs. Стаб
// обязан импортироваться ПЕРВЫМ: `core/dom.js` резолвит все `$b24*` один раз
// на верхнем уровне модуля.
//
// Что покрыто (в порядке значимости):
//
//   • КНОПКИ СТРОГО ПО МАТРИЦЕ: рисуется ровно то, что пришло в `task.actions`,
//     и ничего сверх. Матрица динамична (после «Начать» появляется «Пауза»),
//     поэтому захардкоженный набор кнопок означал бы вызов, который портал
//     молча проигнорирует;
//   • ПОСЛЕ ДЕЙСТВИЯ карточка перерисовывается ответом сервера, а лента
//     перезапрашивается: портал пишет в чат задачи системное сообщение о смене
//     статуса, и ждать его realtime'ом нельзя — событий задач вебхуку не
//     приходит вовсе;
//   • Р20 «ЗАДАЧА БЕЗ ЧАТА»: карточка есть, ленты нет, поле ввода заблокировано
//     с объяснением. Ветка обязательная, а не редкая;
//   • ОБЫЧНЫЙ ЧАТ гасит карточку: оставленная шапка чужой задачи над чужой
//     лентой опасна — кнопки в ней рабочие;
//   • `409 invalid_state` — не сбой, а признак устаревшей кнопки: карточка
//     тут же перечитывается;
//   • WS-кадр `task_update` применяется (действие выполнили в другой вкладке);
//   • ЧИСТЫЕ ФУНКЦИИ format.js: подписи действий, срок, просрочка.
//
// Запуск: node tmux-web/tests/frontend/bitrix_tasks.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $input, $send, $taskCard, dispatch, flush, jsonRes, netCalls, setNet,
} from './bitrix_dom_stub.mjs';

const tasks = await import(new URL('../../static/js/bitrix/tasks.js', import.meta.url).href);
const format = await import(new URL('../../static/js/bitrix/format.js', import.meta.url).href);
const state = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);

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

/** Все узлы поддерева в порядке обхода. */
function walk(node, out = []) {
    out.push(node);
    for (const c of (node.childNodes || [])) walk(c, out);
    return out;
}

/** Узлы карточки с заданным классом. */
function byClass(cls) {
    return walk($taskCard).filter((n) => String(n.className || '').split(/\s+/).includes(cls));
}

/** Весь текст карточки одной строкой. */
function cardText() {
    return $taskCard.textContent;
}

/**
 * Клик по кнопке карточки.
 *
 * Событие доставляется САМОЙ КАРТОЧКЕ с `target` на кнопке: слушатель
 * делегированный (карточка пересобирается после каждого действия, и вешать
 * слушатели на кнопки пришлось бы заново каждый раз), а фейковый DOM всплытия
 * не эмулирует.
 */
function clickAction(key) {
    const btn = byClass('b24-task-btn').find((b) => b.dataset.b24Task === key);
    if (!btn) throw new Error(`кнопки «${key}» нет в карточке`);
    dispatch($taskCard, 'click', { target: btn });
    return btn;
}

/** Кнопки действий: их ключи в порядке отрисовки. */
function actionKeys() {
    return byClass('b24-task-btn').map((b) => b.dataset.b24Task);
}

/** Карточка задачи — снята с живого ответа тестового портала. */
function card(over = {}) {
    return {
        id: '1',
        title: 'Прислать финальные тексты по фичам',
        description: 'Из встречи «Совещание по запуску продукта»',
        status: 2,
        status_label: 'Ждёт выполнения',
        status_key: 'pending',
        sub_status: -1,
        priority: 1,
        priority_label: 'средний',
        priority_key: 'normal',
        created_by: '1',
        created_by_name: 'Игорь Герасимов',
        responsible_id: '1',
        responsible_name: 'Игорь Герасимов',
        accomplices: [],
        auditors: [],
        deadline: '2026-07-04T19:00:00+03:00',
        overdue: true,
        is_new: false,
        group_id: null,
        group_name: null,
        chat_id: 7,
        dialog_id: 'chat7',
        new_comments_count: 0,
        actions: ['complete', 'defer', 'start'],
        url: 'https://test-work-2.bitrix24.ru/company/personal/user/1/tasks/task/view/1/',
        ...over,
    };
}

const taskDialog = { id: 'chat7', kind: 'task', task_id: '1', title: 'Задача' };

let reloaded = [];
tasks.initTasks({ onReloadFeed: (id) => reloaded.push(id) });

// ---------------------------------------------------------------------------
// 1. Карточка рисуется по ответу сервера
// ---------------------------------------------------------------------------

netCalls.length = 0;
setNet(async (url) => {
    if (url.startsWith('/api/bitrix/tasks/1')) return jsonRes(200, card());
    return jsonRes(200, {});
});
await tasks.showTaskFor(taskDialog);
await flush(8);

eq(netCalls.map((c) => `${c.method} ${c.url}`), ['GET /api/bitrix/tasks/1'],
    'карточка читается ОДНИМ запросом по замороженному пути');
ok(!$taskCard.hidden, 'карточка показана');
ok(cardText().includes('#1'), 'номер задачи в шапке');
ok(cardText().includes('Прислать финальные тексты'), 'заголовок в шапке');
ok(cardText().includes('Ждёт выполнения'), 'статус в шапке');
ok(cardText().includes('средний'), 'приоритет в шапке');
ok(cardText().includes('Игорь Герасимов'), 'ответственный в шапке');
ok(cardText().includes('ПРОСРОЧЕНО'), 'метка просрочки');
eq(byClass('b24-task-dot').map((n) => n.className), ['b24-task-dot b24-task-dot-pending'],
    'статус-точка красится ключом статуса, а не подписью');

const link = byClass('b24-task-link')[0];
ok(!!link, 'ссылка «открыть в Битриксе» есть');
eq(link.href, 'https://test-work-2.bitrix24.ru/company/personal/user/1/tasks/task/view/1/',
    'ссылка ведёт на задачу в портале');
eq(link.rel, 'noopener noreferrer', 'внешняя ссылка открывается безопасно');
eq(link.target, '_blank', 'портал открывается в новой вкладке, не подменяя devforge');

// ГЛАВНОЕ: кнопки — ровно те, что прислал сервер, и ничего сверх.
eq(actionKeys(), ['complete', 'defer', 'start'], 'кнопки строятся СТРОГО по task.actions');
ok(!cardText().includes('Удалить'), 'кнопки, которой нет в матрице, нет и на экране');
eq(byClass('b24-task-btn').map((b) => b.textContent),
    ['Завершить', 'Отложить', 'Начать'],
    'подписи кнопок берутся из словаря, а сам набор — с сервера');

// Поле ввода живо: чат у задачи есть.
eq($input.disabled, false, 'поле ввода открыто, когда у задачи есть чат');
eq($send.disabled, false, 'кнопка отправки открыта');

// ---------------------------------------------------------------------------
// 2. Действие: путь, перерисовка карточки, перезапрос ленты
// ---------------------------------------------------------------------------

netCalls.length = 0;
reloaded = [];
const started = card({
    status: 3,
    status_label: 'Выполняется',
    status_key: 'in_progress',
    actions: ['complete', 'pause'],
});
setNet(async (url, init) => {
    if (url === '/api/bitrix/tasks/1/start' && init.method === 'POST') {
        return jsonRes(200, { action: 'start', ok: true, task: started, dialog_id: 'chat7' });
    }
    return jsonRes(200, card());
});

clickAction('start');
await flush(10);

eq(netCalls.map((c) => `${c.method} ${c.url}`), ['POST /api/bitrix/tasks/1/start'],
    'действие уходит ОДНИМ POST по пути /api/bitrix/tasks/:id/:action');
ok(cardText().includes('Выполняется'), 'карточка перерисована ответом сервера');
eq(actionKeys(), ['complete', 'pause'],
    'МАТРИЦА ДИНАМИЧНА: после «Начать» появилась «Пауза», а сама «Начать» исчезла');
eq(reloaded, ['chat7'],
    'лента перезапрошена: портал написал в чат задачи системное сообщение о смене статуса');
eq(byClass('b24-task-dot').map((n) => n.className), ['b24-task-dot b24-task-dot-in_progress'],
    'статус-точка перекрасилась вместе со статусом');

// ---------------------------------------------------------------------------
// 3. Отказ 409 invalid_state — не сбой, а устаревшая кнопка
// ---------------------------------------------------------------------------

netCalls.length = 0;
let refetched = 0;
setNet(async (url, init) => {
    if (init.method === 'POST') {
        return jsonRes(409, { error: { code: 'invalid_state', message: 'not allowed right now' } });
    }
    refetched += 1;
    return jsonRes(200, started);
});
clickAction('pause');
await flush(10);

ok(netCalls.some((c) => c.method === 'POST' && c.url === '/api/bitrix/tasks/1/pause'),
    'запрещённое действие всё же уходит на сервер: решает МАТРИЦА СЕРВЕРА, а не догадка фронта');
eq(refetched, 1, '409 invalid_state перечитывает карточку — кнопка на экране устарела');

// ---------------------------------------------------------------------------
// 4. Р20: задача без чата
// ---------------------------------------------------------------------------

netCalls.length = 0;
const chatless = card({ id: '5', chat_id: null, dialog_id: null, overdue: false });
setNet(async () => jsonRes(200, chatless));
await tasks.showTaskFor({ id: 'task5', kind: 'task', task_id: '5' });
await flush(8);

ok(!$taskCard.hidden, 'Р20: карточка задачи без чата ВСЁ РАВНО показывается');
ok(cardText().includes('Обсуждение задачи ещё не начато'),
    'Р20: пустая лента объяснена словами, а не молчанием');
eq($input.disabled, true, 'Р20: поле ввода заблокировано — писать физически некуда');
eq($send.disabled, true, 'Р20: кнопка отправки заблокирована');
eq($input.placeholder, 'обсуждение ещё не начато', 'Р20: подсказка в самом поле');
eq(actionKeys(), ['complete', 'defer', 'start'],
    'Р20: действия над задачей без чата доступны — они не требуют переписки');

// ---------------------------------------------------------------------------
// 5. Обычный чат гасит карточку
// ---------------------------------------------------------------------------

netCalls.length = 0;
await tasks.showTaskFor({ id: 'chat27', kind: 'chat', title: 'Полигон' });
await flush(4);

eq(netCalls.length, 0, 'обычный чат не стоит ни одного запроса задач');
eq($taskCard.hidden, true,
    'карточка ПРЯЧЕТСЯ: оставленная шапка чужой задачи опасна — кнопки в ней рабочие');
eq($input.disabled, false, 'поле ввода разблокировано обратно');

// ---------------------------------------------------------------------------
// 6. WS-кадр task_update (действие выполнили в другой вкладке)
// ---------------------------------------------------------------------------

netCalls.length = 0;
setNet(async () => jsonRes(200, card()));
await tasks.showTaskFor(taskDialog);
await flush(8);
netCalls.length = 0;

tasks.applyTaskUpdate(card({
    status: 5,
    status_label: 'Завершена',
    status_key: 'completed',
    actions: ['renew'],
    overdue: false,
}));

eq(netCalls.length, 0, 'кадр применяется БЕЗ запроса: карточка приезжает в нём целиком');
ok(cardText().includes('Завершена'), 'статус из кадра виден в шапке');
eq(actionKeys(), ['renew'], 'матрица из кадра заменила ряд кнопок целиком');
ok(state.b24State.tasks.get('1').status === 5, 'карточка легла в состояние панели');

// ---------------------------------------------------------------------------
// 7. Директория ~/tasks: карточки тянутся страницей платформы
// ---------------------------------------------------------------------------

netCalls.length = 0;
state.b24State.tasks.clear();
setNet(async (url) => {
    if (url.startsWith('/api/bitrix/tasks?') || url === '/api/bitrix/tasks') {
        return jsonRes(200, { tasks: [card(), card({ id: '3', status_key: 'completed' })], total: 2 });
    }
    return jsonRes(200, {});
});
await tasks.loadTasks({ filter: 'all' });
await flush(6);

eq(netCalls.map((c) => c.url), ['/api/bitrix/tasks?filter=all'],
    'список задач читается пресетом фильтра, а не сырым фильтром портала');
eq(state.b24State.tasks.size, 2, 'карточки легли в состояние — колонка красит по ним статус-точки');

// ---------------------------------------------------------------------------
// 8. Чистые функции format.js
// ---------------------------------------------------------------------------

eq(format.taskActionLabel('start'), 'Начать', 'подпись известного действия');
eq(format.taskActionLabel('pause'), 'Пауза', 'подпись известного действия');
eq(format.taskActionLabel('checklist.add'), 'checklist.add',
    'НЕЗНАКОМОЕ действие рисуется как есть: спрятать кнопку, которую портал разрешил, хуже');

ok(format.taskDeadlineLabel(card()).startsWith('просрочено'),
    'просрочка решается сервером и попадает в подпись срока');
ok(format.taskDeadlineLabel(card({ overdue: false })).startsWith('до '),
    'не просроченный срок читается «до …»');
eq(format.taskDeadlineLabel(card({ deadline: null })), null, 'без срока подписи нет');
eq(format.taskDeadlineLabel(card({ deadline: 'завтра' })), null,
    'неразобранная дата — это «срока нет», а не «01.01.1970»');

eq(format.taskNoChatNote(card()), null, 'у задачи с чатом подписи нет');
ok(String(format.taskNoChatNote(chatless)).includes('не начато'), 'Р20: подпись есть');

const map = new Map([['1', card()]]);
eq(format.taskOfDialog(map, taskDialog).id, '1', 'карточка находится по task_id строки');
eq(format.taskOfDialog(map, { id: 'chat27', kind: 'chat' }), null,
    'у обычного чата карточки нет по определению');
eq(format.taskOfDialog(map, { id: 'x', kind: 'task', task_id: '999' }), null,
    'незагруженная задача не выдумывается');

// Словарь подписей действий обязан покрывать ровно те ключи, которые
// выполняет сервер (ACTION_METHODS в plugins/bitrix/src/tasks.rs).
eq(Object.keys(format.TASK_ACTION_LABELS).sort(),
    ['approve', 'complete', 'defer', 'disapprove', 'pause', 'renew', 'start'],
    'словарь подписей совпадает с таблицей ACTION_METHODS сервера');

// ---------------------------------------------------------------------------

console.log(`\n${checks - failures}/${checks} checks passed`);
if (failures > 0) process.exit(1);
