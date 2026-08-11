// Регресс-тест стриминга анализа чата (js/vk/analysis.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/vk_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы в null).
//
// ЧТО ИМЕННО ЗДЕСЬ ЗАЩИЩЕНО.
//
//   1. ФИЛЬТРАЦИЯ ЧУЖИХ КАДРОВ. Сервер вещает `analysis_*` без фильтрации по
//      беседе (plugins/vk/src/ws/mod.rs) — соседняя вкладка, разбирающая другой
//      чат, шлёт свои кадры в тот же сокет. Кадр принимается только если
//      совпали И `dialog_id`, И `run_id`; иначе чужой ответ затёк бы в попап.
//   2. ПОРЯДОК И ПОЛНОТА ПОТОКА. Дельты копятся в буфере и выливаются кадром
//      анимации; `analysis_done` обязан слить остаток СИНХРОННО, иначе
//      последний кусок ответа пропадал бы молча.
//   3. ЧАСТИЧНЫЙ ОТВЕТ ПЕРЕЖИВАЕТ ОШИБКУ: `analysis_error` пишет причину
//      отдельной строкой и НЕ трогает уже полученный текст.
//   4. ТРЕД Q&A едет в следующий запрос полем `history` — сервер не помнит, о
//      чём его спрашивали, он лишь ХРАНИТ тред рядом с разбором.
//   5. Кнопки консистентны после любого исхода (done / error / cancel), а
//      отмена разблокирует START даже когда сервер ответил «нечего гасить».
//   6. Вывод строится БЕЗ innerHTML (в ответе модели встречается что угодно).
//   7. ВОПРОС НЕ СТИРАЕТ РАЗБОР — главный регресс доработки «сохраняемый и
//      инкрементальный анализ». До неё контейнер вывода был один, и `ask`
//      затирал ровно тот текст, ради которого попап и открывали. Разбор живёт
//      в `#vk-analysis-out`, ответ на вопрос — в своей строке треда.
//   8. СОХРАНЁННЫЙ РАЗБОР приходит с сервера при открытии попапа (один GET на
//      беседу), REFRESH его заменяет, `no_new_messages` не трогает ничего.
//   9. ТРЕД ТОЖЕ ПРИХОДИТ С СЕРВЕРА тем же GET'ом (АЧ-И2) и переживает
//      перезагрузку страницы. Кнопки очистки разведены ПО АДРЕСАМ: разбор
//      стирает `DELETE …/analysis`, тред — `DELETE …/analysis/thread`, и ни
//      одна не задевает чужое.
//  10. СВОРАЧИВАЕТСЯ ТОЛЬКО `#vk-analysis-out`; состояние живёт в
//      localStorage, а старт разбора разворачивает блок принудительно.
//  11. MARKDOWN РИСУЕТСЯ ОДИН РАЗ, по `analysis_done`: во время стрима в блоке
//      только текстовые узлы, оборванный ответ так и остаётся сырым.
//
// Запуск: node tmux-web/tests/frontend/vk_analysis.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

import {
    $analysis, $analysisAsk, $analysisCancel, $analysisClear,
    $analysisClearThread, $analysisClose, $analysisDepth, $analysisError,
    $analysisOut, $analysisOutToggle, $analysisQuestion, $analysisRefresh, $analysisSend,
    $analysisStart, $analysisStatus, $analysisThread, $analysisUpdated,
    dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

/**
 * Даёт доехать И микрозадачам, И таймеру планировщика, И кадру анимации.
 *
 * Одного `flush` мало: очередь js/vk/ratelimit.js после исчерпания burst'а
 * взводит `setTimeout(…, 1)`, который срабатывает ПОЗЖЕ `setTimeout(…, 0)`.
 */
async function settle() {
    for (let i = 0; i < 4; i += 1) {
        await flush();
        await new Promise((resolve) => setTimeout(resolve, 3));
    }
}

// Хранилище с памятью: глубина запоминается между открытиями попапа, и инертный
// стаб (`getItem: () => null`) сделал бы эту проверку бессмысленной.
const store = new Map();
globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => { store.set(k, String(v)); },
    removeItem: (k) => { store.delete(k); },
};

const A = await import(new URL('../../static/js/vk/analysis.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// ИМЕННО 1, А НЕ 0: `createLimiter` берёт значение только если оно строго
// больше нуля, иначе молча возвращается к штатным 380 мс — и тест «зависал» бы
// на пустом месте, не показывая ни одного запроса.
R.vkLimits.minIntervalMs = 1;

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

// ВНИМАНИЕ: `/analysis` теперь обслуживает ТРИ метода (GET — сохранённый
// разбор, DELETE — стереть, POST — запустить), поэтому фильтровать только по
// пути нельзя: подгрузка при открытии попапа считалась бы запуском.

/** Запросы запуска разбора, в порядке отправки. */
function startCalls() {
    return netCalls.filter((c) => c.method === 'POST' && /\/analysis$/.test(c.url));
}

/** Запросы сохранённого разбора (GET). */
function savedCalls() {
    return netCalls.filter((c) => c.method === 'GET' && /\/analysis$/.test(c.url));
}

/** Запросы очистки сохранённого разбора (DELETE). */
function deleteCalls() {
    return netCalls.filter((c) => c.method === 'DELETE' && /\/analysis$/.test(c.url));
}

/** Запросы отмены. */
function cancelCalls() {
    return netCalls.filter((c) => /\/analysis\/cancel$/.test(c.url));
}

/** Запросы очистки ТРЕДА (АЧ-И2) — свой путь, не путать с очисткой разбора. */
function threadCalls() {
    return netCalls.filter((c) => c.method === 'DELETE' && /\/analysis\/thread$/.test(c.url));
}

/** Текст вывода модели. */
const out = () => String($analysisOut.textContent || '');

/** Кадр started/delta/done/error как его шлёт plugins/vk/src/ws/protocol.rs. */
const started = (extra) => Object.assign({
    type: 'analysis_started', run_id: 'r1', dialog_id: '1234',
    kind: 'analyze', messages_used: 120, truncated: false,
}, extra || {});
const delta = (text, extra) => Object.assign({
    type: 'analysis_delta', run_id: 'r1', dialog_id: '1234', text,
}, extra || {});
const done = (extra) => Object.assign({
    type: 'analysis_done', run_id: 'r1', dialog_id: '1234',
    ok: true, reason: null, input_tokens: 0, output_tokens: 0,
}, extra || {});

/** Ответ роута запуска. */
const startDto = (runId) => ({
    run_id: runId || 'r1', attached: false, messages_used: 120, truncated: false,
    no_new_messages: false,
});

/** Ответ GET …/analysis. Без аргументов — «разбора ещё не было». */
const savedDto = (extra) => Object.assign({
    text: '', last_message_id: null, messages_used: 0, updated_at: null,
    max_chars: 12000,
}, extra || {});

/**
 * Штатный обработчик сети: POST запускает run, GET отдаёт пустой разбор.
 *
 * Метод значим: три разных действия висят на ОДНОМ пути, и обработчик,
 * смотрящий только на url, зеленел бы на подменённом смысле запроса.
 */
function net(opts) {
    const o = opts || {};
    setNet(async (url, init) => {
        const method = (init && init.method) || 'GET';
        if (/\/analysis\/cancel$/.test(url)) return jsonRes(200, o.cancel || { cancelled: true });
        // Тред живёт по СВОЕМУ адресу (АЧ-И2) — проверка стоит ДО общего
        // `/analysis$`, иначе очистка треда считалась бы очисткой разбора.
        if (/\/analysis\/thread$/.test(url)) {
            if (o.threadFail) {
                return jsonRes(500, {
                    error: { code: 'storage_failed', message: 'диск только на чтение' },
                });
            }
            return jsonRes(200, o.clearThread || { cleared: true });
        }
        if (/\/analysis$/.test(url)) {
            if (method === 'POST') return jsonRes(200, o.start || startDto('r1'));
            if (method === 'DELETE') return jsonRes(200, o.clear || { cleared: true });
            return jsonRes(200, o.saved || savedDto());
        }
        return jsonRes(200, {});
    });
}

A.initAnalysis({ onNotice: () => {} });

S.vkState.dialogs = [{ id: '1234', title: 'Подрядчик', type: 'user', first_name: 'Пётр', last_name: 'Смирнов' }];
S.vkState.activeDialogId = '1234';

net();

// ---------------------------------------------------------------------------
// 1. Открытие попапа
// ---------------------------------------------------------------------------

A.openAnalysis();
await settle();
eq($analysis.hidden, false, 'открытие: попап показан');
ok(A.isAnalysisOpen(), 'открытие: isAnalysisOpen() === true');
eq($analysisStart.disabled, false, 'открытие: START доступен');
eq($analysisCancel.hidden, true, 'открытие: STOP спрятан');
eq(savedCalls().length, 1, 'открытие: сохранённый разбор запрошен ровно один раз');
eq($analysisRefresh.disabled, true, 'открытие: REFRESH заблокирован — сохранённого разбора нет');
eq($analysisClear.disabled, true, 'открытие: CLEAR анализа нечего нажимать');
eq(String($analysisUpdated.textContent), '', 'открытие: строки «разобрано до …» без разбора нет');

// Повторное открытие того же диалога GET'а НЕ повторяет (бюджет 3 rps общий).
A.openAnalysis();
await settle();
eq(savedCalls().length, 1, 'повторное открытие: второго GET нет');

// ANALYZE, нажатый ПОКА идёт подгрузка сохранённого разбора, обязан сработать:
// кнопка в это время доступна, и молчаливый отказ выглядел бы поломкой.
// (Ровно на этом спотыкалась живая приёмка.)
let releaseSaved = null;
setNet((url, init) => {
    const method = (init && init.method) || 'GET';
    if (/\/analysis$/.test(url) && method === 'POST') {
        return Promise.resolve(jsonRes(200, startDto('slow')));
    }
    if (/\/analysis$/.test(url)) {
        return new Promise((resolve) => { releaseSaved = () => resolve(jsonRes(200, savedDto())); });
    }
    return Promise.resolve(jsonRes(200, {}));
});
A.openAnalysis('4321');
await settle();
eq(A._debugAnalysisState().analysisBusy, true, 'подгрузка: GET ещё в полёте');
const startsBeforeBusy = startCalls().length;
dispatch($analysisStart, 'click', {});
await settle();
eq(startCalls().length, startsBeforeBusy + 1, 'ANALYZE во время подгрузки: запрос всё равно ушёл');
if (releaseSaved) releaseSaved();
await settle();
eq(A._debugAnalysisState().runId, 'slow', 'ANALYZE во время подгрузки: run взят под управление');
A.onAnalysisDone(done({ run_id: 'slow', dialog_id: '4321', ok: false, reason: 'cancelled' }));

// Возвращаемся к беседе сценария.
net();
A.openAnalysis('1234');
await settle();

// ---------------------------------------------------------------------------
// 2. START шлёт POST {kind:"analyze", depth}
// ---------------------------------------------------------------------------

const startsBefore2 = startCalls().length;
dispatch($analysisStart, 'click', {});
eq($analysisStart.disabled, true, 'START: кнопка гаснет СРАЗУ, до ответа сервера');
await settle();

eq(startCalls().length, startsBefore2 + 1, 'START: ушёл один POST');
const start1 = startCalls()[startCalls().length - 1];
eq(start1.method, 'POST', 'START: именно POST');
eq(start1.url, '/api/vk/dialogs/1234/analysis', 'START: путь совпадает с серверным контрактом');
eq(start1.body, { kind: 'analyze', depth: 200 }, 'START: тело — {kind, depth}');
eq(A._debugAnalysisState().runId, 'r1', 'START: run взят под управление попапа');
eq($analysisCancel.hidden, false, 'START: STOP показан');

// ---------------------------------------------------------------------------
// 3. started → delta×N → done рисует текст в правильном порядке
// ---------------------------------------------------------------------------

A.onAnalysisStarted(started({ truncated: true }));
ok(String($analysisStatus.textContent).includes('120'), 'started: статус называет число сообщений');
ok(String($analysisStatus.textContent).includes('усечено'), 'started: усечение показано');
eq($analysisStart.disabled, true, 'started: START заблокирован');
eq($analysisCancel.hidden, false, 'started: STOP доступен');

A.onAnalysisDelta(delta('Первое. '));
A.onAnalysisDelta(delta('Второе. '));
eq(out(), '', 'дельты: до кадра анимации DOM не трогается');
ok(A._debugAnalysisState().bufLen > 0, 'дельты: копятся в буфере');
await settle();
eq(out(), 'Первое. Второе. ', 'дельты: текст вылит в правильном порядке');

// ---------------------------------------------------------------------------
// 4. Чужие кадры игнорируются
// ---------------------------------------------------------------------------

A.onAnalysisDelta(delta('ЧУЖОЙ ЧАТ', { dialog_id: '999' }));
A.onAnalysisDelta(delta('ЧУЖОЙ RUN', { run_id: 'r-other' }));
A.onAnalysisStarted(started({ dialog_id: '999', run_id: 'r-other' }));
await settle();
eq(out(), 'Первое. Второе. ', 'фильтрация: чужие dialog_id и run_id не попали в вывод');
eq(A._debugAnalysisState().runId, 'r1', 'фильтрация: чужой started не перехватил run');

// Последняя дельта уходит БЕЗ ожидания кадра — done обязан слить её сам.
A.onAnalysisDelta(delta('Хвост.'));
A.onAnalysisDone(done({ input_tokens: 1200, output_tokens: 300 }));
eq(out(), 'Первое. Второе. Хвост.', 'done: остаток буфера долит синхронно');
eq($analysisStart.disabled, false, 'done: START разблокирован');
eq($analysisCancel.hidden, true, 'done: STOP спрятан');
eq(A._debugAnalysisState().runId, null, 'done: run отпущен');
ok(String($analysisStatus.textContent).includes('готово'), 'done: статус говорит «готово»');

// Кадры уже завершённого run'а больше ничего не рисуют.
A.onAnalysisDelta(delta('ПОСЛЕ ФИНАЛА'));
await settle();
eq(out(), 'Первое. Второе. Хвост.', 'после done: опоздавшая дельта отброшена');

// ---------------------------------------------------------------------------
// 5. analysis_error: причина показана, частичный вывод сохранён
// ---------------------------------------------------------------------------

net({ start: startDto('r2') });

dispatch($analysisStart, 'click', {});
await settle();
A.onAnalysisStarted(started({ run_id: 'r2' }));
A.onAnalysisDelta(delta('Половина ответа', { run_id: 'r2' }));
await settle();
A.onAnalysisError({
    type: 'analysis_error', run_id: 'r2', dialog_id: '1234', message: 'claude exited with code 1',
});
eq(out(), 'Половина ответа', 'ошибка: частичный вывод НЕ стёрт');
eq($analysisError.hidden, false, 'ошибка: строка ошибки показана');
ok(String($analysisError.textContent).includes('claude exited'), 'ошибка: причина видна пользователю');
eq($analysisStart.disabled, false, 'ошибка: START разблокирован');
eq($analysisCancel.hidden, true, 'ошибка: STOP спрятан');

// ---------------------------------------------------------------------------
// 6. Отмена: POST cancel и разблокировка START даже при «нечего гасить»
// ---------------------------------------------------------------------------

// Сервер отвечает «run не найден» — гонка «стоп ровно в момент финала».
net({ start: startDto('r3'), cancel: { cancelled: false } });

dispatch($analysisStart, 'click', {});
await settle();
A.onAnalysisStarted(started({ run_id: 'r3' }));
A.onAnalysisDelta(delta('Начал думать', { run_id: 'r3' }));
await settle();

dispatch($analysisCancel, 'click', {});
await settle();

eq(cancelCalls().length, 1, 'CANCEL: ушёл один POST');
eq(cancelCalls()[0].url, '/api/vk/dialogs/1234/analysis/cancel', 'CANCEL: путь совпадает с контрактом');
eq(cancelCalls()[0].body, { run_id: 'r3' }, 'CANCEL: тело — {run_id}');
eq($analysisStart.disabled, false, 'CANCEL: START разблокирован, хотя сервер ответил cancelled:false');
eq($analysisCancel.hidden, true, 'CANCEL: STOP спрятан');
eq(A._debugAnalysisState().runId, null, 'CANCEL: run отпущен');
eq(out(), 'Начал думать', 'CANCEL: уже полученный текст остался на экране');

// Опоздавший `analysis_done` отменённого run'а ничего не ломает.
A.onAnalysisDone(done({ run_id: 'r3', ok: false, reason: 'cancelled' }));
eq($analysisStart.disabled, false, 'CANCEL: поздний done не блокирует кнопки обратно');

// ---------------------------------------------------------------------------
// 7. Пустой вопрос не отправляется
// ---------------------------------------------------------------------------

const beforeEmpty = startCalls().length;
$analysisQuestion.value = '   \n  ';
dispatch($analysisAsk, 'submit', {});
await settle();
eq(startCalls().length, beforeEmpty, 'пустой вопрос: запроса нет');

// ---------------------------------------------------------------------------
// 8. Q&A-тред: пара уезжает в тред и в history следующего запроса
// ---------------------------------------------------------------------------

net({ start: startDto('q1') });

$analysisQuestion.value = 'Кто обещал смету?';
dispatch($analysisAsk, 'submit', {});
await settle();

const ask1 = startCalls()[startCalls().length - 1];
eq(ask1.body, { kind: 'ask', question: 'Кто обещал смету?', depth: 200 },
    'ASK: тело — {kind, question, depth}, истории ещё нет');
eq($analysisQuestion.value, '', 'ASK: поле вопроса очищено');

// ГЛАВНЫЙ РЕГРЕСС ЭТОЙ ДОРАБОТКИ: до неё вывод был один на всё, и вопрос
// стирал разбор ещё на запуске. Снимок делается ДО кадров ответа.
const outBeforeAsk = out();
ok(outBeforeAsk !== '', 'ASK: перед вопросом в выводе есть текст разбора (иначе проверка пуста)');
ok(A._debugAnalysisState().askRow, 'ASK: строка треда открыта сразу, до первой дельты');
ok(String($analysisThread.textContent).includes('Кто обещал смету?'),
    'ASK: вопрос виден в треде ещё до ответа');
eq(out(), outBeforeAsk, 'ASK: запуск вопроса НЕ тронул разбор');

A.onAnalysisStarted(started({ run_id: 'q1', kind: 'ask' }));
eq(out(), outBeforeAsk, 'ASK: analysis_started вопроса НЕ стёр разбор');
A.onAnalysisDelta(delta('Смету обещал Пётр.', { run_id: 'q1' }));
await settle();
eq(out(), outBeforeAsk, 'ASK: дельты ответа не попали в вывод разбора');
ok(String($analysisThread.textContent).includes('Смету обещал Пётр.'),
    'ASK: дельты ответа льются в строку треда');
A.onAnalysisDone(done({ run_id: 'q1' }));

ok(String($analysisThread.textContent).includes('Кто обещал смету?'),
    'ASK: вопрос остался в треде');
ok(String($analysisThread.textContent).includes('Смету обещал Пётр.'),
    'ASK: ответ остался в треде');
eq(out(), outBeforeAsk, 'ASK: разбор пережил весь цикл вопроса');
eq(A._debugAnalysisState().askRow, false, 'ASK: строка треда отпущена после финала');
eq(A._debugAnalysisState().thread, 1, 'ASK: пара учтена в истории');

net({ start: startDto('q2') });
$analysisQuestion.value = 'А когда?';
dispatch($analysisAsk, 'submit', {});
await settle();

const ask2 = startCalls()[startCalls().length - 1];
eq(ask2.body, {
    kind: 'ask',
    question: 'А когда?',
    depth: 200,
    history: [{ q: 'Кто обещал смету?', a: 'Смету обещал Пётр.' }],
}, 'ASK: второй вопрос уехал вместе с историей первой пары');

A.onAnalysisDone(done({ run_id: 'q2' }));

// Клиентская обрезка истории — то же правило, что и на сервере.
eq(A.clampThread([{ q: 'a', a: 'b' }]).length, 1, 'clampThread: короткая история проходит целиком');
eq(A.clampThread(Array.from({ length: 9 }, (_, i) => ({ q: `q${i}`, a: 'a' }))).length,
    A.MAX_HISTORY_TURNS, 'clampThread: не больше MAX_HISTORY_TURNS пар');
eq(A.clampThread(Array.from({ length: 9 }, (_, i) => ({ q: `q${i}`, a: 'a' })))[0].q, 'q3',
    'clampThread: отбрасываются САМЫЕ СТАРЫЕ пары');
ok(A.clampThread([{ q: 'x'.repeat(9000), a: '' }, { q: 'свежий', a: '' }])
    .every((t) => t.q !== 'x'.repeat(9000)), 'clampThread: лимит символов режет старое');

// ---------------------------------------------------------------------------
// 9. Глубина: значения из списка запоминаются, чужие игнорируются
// ---------------------------------------------------------------------------

eq(A.pickDepth('500'), 500, 'pickDepth: значение из списка принято');
eq(A.pickDepth(1000), null, 'pickDepth: значение вне списка отвергнуто');
eq(A.pickDepth('нет'), null, 'pickDepth: мусор отвергнут');

$analysisDepth.value = '500';
dispatch($analysisDepth, 'change', {});
eq(A._debugAnalysisState().depth, 500, 'глубина: выбор принят');
eq(store.get('vk.analysis.depth'), '500', 'глубина: запомнена в localStorage');

$analysisDepth.value = '9999';
dispatch($analysisDepth, 'change', {});
eq(A._debugAnalysisState().depth, 500, 'глубина: значение вне списка проигнорировано');
eq($analysisDepth.value, '500', 'глубина: селект возвращён к действующему значению');

net({ start: startDto('d1') });
dispatch($analysisStart, 'click', {});
await settle();
eq(startCalls()[startCalls().length - 1].body, { kind: 'analyze', depth: 500 },
    'глубина: уезжает в запрос');
A.onAnalysisDone(done({ run_id: 'd1' }));

// ---------------------------------------------------------------------------
// 10. `analysis_started` может ОБОГНАТЬ ответ POST'а
// ---------------------------------------------------------------------------
//
// hub.rs шлёт кадр до того, как маршрут вернёт run_id. Попап с запуском «в
// полёте» обязан усыновить run_id из кадра, иначе первые дельты потерялись бы.

let releaseStart = null;
setNet((url, init) => {
    if (/\/analysis$/.test(url) && (init && init.method) === 'POST') {
        return new Promise((resolve) => { releaseStart = () => resolve(jsonRes(200, startDto('race'))); });
    }
    return Promise.resolve(jsonRes(200, {}));
});

dispatch($analysisStart, 'click', {});
await settle();
eq(A._debugAnalysisState().pending, true, 'гонка: запуск считается «в полёте»');

A.onAnalysisStarted(started({ run_id: 'race' }));
eq(A._debugAnalysisState().runId, 'race', 'гонка: run_id усыновлён из кадра');
A.onAnalysisDelta(delta('Ранняя дельта', { run_id: 'race' }));
await settle();
eq(out(), 'Ранняя дельта', 'гонка: дельта до ответа POST не потеряна');

if (releaseStart) releaseStart();
await settle();
eq(A._debugAnalysisState().runId, 'race', 'гонка: ответ POST не подменил run');
A.onAnalysisDone(done({ run_id: 'race' }));

// ---------------------------------------------------------------------------
// 11. Мусорные кадры не роняют обработчики
// ---------------------------------------------------------------------------

const junk = [
    null, undefined, 0, '', 'analysis_delta', [],
    {}, { type: 'analysis_delta' }, { run_id: 'r1' },
    { type: 'analysis_delta', dialog_id: '1234' },
    { type: 'analysis_done', dialog_id: null, run_id: null },
    { type: 'analysis_error', dialog_id: '1234', run_id: 'r1' },
];
let threw = null;
try {
    for (const j of junk) {
        A.onAnalysisStarted(j);
        A.onAnalysisDelta(j);
        A.onAnalysisDone(j);
        A.onAnalysisError(j);
    }
} catch (e) {
    threw = e;
}
eq(threw, null, 'мусор: ни один обработчик не бросил исключение');

// ---------------------------------------------------------------------------
// 12. Автоскролл вывода: следование гаснет от прокрутки вверх
// ---------------------------------------------------------------------------
//
// Решение принимает общая фабрика js/messenger/follow-core.js (она покрыта
// messenger_follow_factory.test.mjs) — здесь проверяется только проводка:
// собственный доскролл идёт, пока пользователь не уехал вверх.

net({ start: startDto('s1') });
dispatch($analysisStart, 'click', {});
await settle();
A.onAnalysisStarted(started({ run_id: 's1' }));

// Геометрия ленты: браузерной раскладки в стабе нет, выставляем руками.
$analysisOut.clientHeight = 100;
$analysisOut.scrollHeight = 400;
$analysisOut.scrollTop = 300;
// Текст подрос — лента обязана догнать новый низ (400 − 100).
$analysisOut.scrollHeight = 500;
A.onAnalysisDelta(delta('строка', { run_id: 's1' }));
await settle();
eq($analysisOut.scrollTop, 400, 'автоскролл: у нижнего края лента догоняет выросший текст');

// Пользователь уехал вверх — следование обязано погаснуть.
$analysisOut.scrollTop = 40;
dispatch($analysisOut, 'scroll', {});
eq(A._debugAnalysisState().following, false, 'автоскролл: прокрутка вверх снимает следование');
A.onAnalysisDelta(delta('ещё строка', { run_id: 's1' }));
await settle();
eq($analysisOut.scrollTop, 40, 'автоскролл: читающего историю вниз не дёргает');

// Вернулся к нижнему краю — следование возобновляется.
$analysisOut.scrollTop = 400;
dispatch($analysisOut, 'scroll', {});
eq(A._debugAnalysisState().following, true, 'автоскролл: возврат к низу возобновляет следование');
A.onAnalysisDone(done({ run_id: 's1' }));

// ---------------------------------------------------------------------------
// 13. Сохранённый разбор: подгрузка, REFRESH, две кнопки очистки
// ---------------------------------------------------------------------------
//
// Всё это — доработка «сохраняемый и инкрементальный анализ». Разбор живёт на
// сервере рядом с заметкой, поэтому переживает перезагрузку; тред Q&A живёт
// только в памяти вкладки, поэтому его кнопка очистки в сеть не ходит вовсе.

const SAVED_AT = 1754000000;
net({
    saved: savedDto({
        text: 'РАЗБОР ОТ ВЧЕРА', last_message_id: 7042, messages_used: 120,
        updated_at: SAVED_AT,
    }),
});

const savedBefore = savedCalls().length;
A.openAnalysis('5678');
await settle();

eq(savedCalls().length, savedBefore + 1, 'смена беседы: сохранённый разбор запрошен заново');
eq(savedCalls()[savedCalls().length - 1].url, '/api/vk/dialogs/5678/analysis',
    'смена беседы: GET ушёл по НОВОЙ беседе');
eq(out(), 'РАЗБОР ОТ ВЧЕРА', 'подгрузка: сохранённый разбор отрисован в выводе');
ok(String($analysisUpdated.textContent).startsWith('разобрано до '),
    'подгрузка: строка «разобрано до …» заполнена');
ok(String($analysisUpdated.textContent).includes('120 сообщений'),
    'подгрузка: строка называет число разобранных сообщений');
eq($analysisRefresh.disabled, false, 'подгрузка: REFRESH включён — есть что обновлять');
eq($analysisClear.disabled, false, 'подгрузка: CLEAR анализа включён');
eq(String($analysisThread.textContent), '', 'смена беседы: тред прошлой беседы не протёк');

// Чистая функция метки — без DOM и без часового пояса раннера.
eq(A.updatedLabel(null, 0), '', 'updatedLabel: разбора нет — строки нет');
eq(A.updatedLabel(0, 5), '', 'updatedLabel: нулевой момент считается отсутствием');
ok(A.updatedLabel(SAVED_AT, 7).includes('7 сообщений'), 'updatedLabel: счётчик подставлен');
ok(!A.updatedLabel(SAVED_AT, 0).includes('сообщений'),
    'updatedLabel: нулевой счётчик не показывается вовсе');

// --- REFRESH без новых сообщений: модель НЕ запускалась --------------------

net({
    saved: savedDto({ text: 'РАЗБОР ОТ ВЧЕРА', messages_used: 120, updated_at: SAVED_AT }),
    start: {
        run_id: null, attached: false, messages_used: 0, truncated: false,
        no_new_messages: true,
    },
});

dispatch($analysisRefresh, 'click', {});
await settle();

const refresh1 = startCalls()[startCalls().length - 1];
eq(refresh1.body, { kind: 'refresh', depth: 500 }, 'REFRESH: тело — {kind:"refresh", depth}');
ok(String($analysisStatus.textContent).includes('новых сообщений нет'),
    'no_new_messages: статус объясняет, что обновлять было нечего');
eq(out(), 'РАЗБОР ОТ ВЧЕРА', 'no_new_messages: вывод не тронут');
eq(A._debugAnalysisState().runId, null, 'no_new_messages: run не заводится (run_id пришёл null)');
eq(A._debugAnalysisState().pending, false, 'no_new_messages: попап не остался в «идёт разбор»');
eq($analysisRefresh.disabled, false, 'no_new_messages: REFRESH снова доступен');

// --- REFRESH с новыми сообщениями: разбор заменяется целиком ---------------

net({ start: startDto('rf1') });
dispatch($analysisRefresh, 'click', {});
await settle();

A.onAnalysisStarted(started({
    run_id: 'rf1', dialog_id: '5678', kind: 'refresh', messages_used: 7,
}));
eq(out(), 'РАЗБОР ОТ ВЧЕРА',
    'REFRESH: прошлый разбор держится на экране, пока не пришла первая дельта');
A.onAnalysisDelta(delta('РАЗБОР СВЕЖИЙ', { run_id: 'rf1', dialog_id: '5678' }));
await settle();
eq(out(), 'РАЗБОР СВЕЖИЙ', 'REFRESH: первая дельта заменила прошлый разбор целиком');
eq(String($analysisThread.textContent), '', 'REFRESH: тред не при делах');

A.onAnalysisDone(done({ run_id: 'rf1', dialog_id: '5678' }));
ok(String($analysisUpdated.textContent).includes('7 сообщений'),
    'REFRESH: метка пересчитана по числу ДОЧИТАННЫХ сообщений');
eq($analysisRefresh.disabled, false, 'REFRESH: кнопка снова доступна');

// --- CLEAR Q&A: DELETE по своему пути, разбор не тронут --------------------
//
// До АЧ-И2 эта кнопка чистила только память вкладки, и тред возвращался первой
// же перезагрузкой. Теперь она обязана сходить в сеть — и ровно по СВОЕМУ
// адресу: общий `DELETE …/analysis` стёр бы разбор.

net({ start: startDto('q9') });
$analysisQuestion.value = 'Что изменилось?';
dispatch($analysisAsk, 'submit', {});
await settle();
A.onAnalysisStarted(started({ run_id: 'q9', dialog_id: '5678', kind: 'ask' }));
A.onAnalysisDelta(delta('Сроки сдвинули.', { run_id: 'q9', dialog_id: '5678' }));
A.onAnalysisDone(done({ run_id: 'q9', dialog_id: '5678' }));
eq(A._debugAnalysisState().thread, 1, 'CLEAR Q&A: перед очисткой в треде есть пара');
eq(out(), 'РАЗБОР СВЕЖИЙ', 'CLEAR Q&A: вопрос по-прежнему не трогает разбор');

// Упавший сервер: тред обязан остаться на экране. Показать пустоту значило бы
// соврать — на сервере пары целы, и следующее открытие попапа их вернёт.
net({ threadFail: true });
const threadsBeforeFail = threadCalls().length;
const deletesBeforeFail = deleteCalls().length;
dispatch($analysisClearThread, 'click', {});
await settle();
eq(threadCalls().length, threadsBeforeFail + 1, 'CLEAR Q&A (ошибка): запрос всё-таки ушёл');
eq(deleteCalls().length, deletesBeforeFail, 'CLEAR Q&A (ошибка): по разбору DELETE не уходил');
eq(A._debugAnalysisState().thread, 1, 'CLEAR Q&A (ошибка): пара НЕ стёрта');
ok(String($analysisThread.textContent).includes('Что изменилось?'),
    'CLEAR Q&A (ошибка): тред остался на экране');
eq($analysisError.hidden, false, 'CLEAR Q&A (ошибка): показана строка ошибки');
ok(String($analysisError.textContent).includes('права на каталог'),
    'CLEAR Q&A (ошибка): текст выбран по err.code (storage_failed), а не по сообщению');
eq($analysisClearThread.disabled, false, 'CLEAR Q&A (ошибка): кнопка снова доступна');

net({ start: startDto('q9') });
const threadsBefore = threadCalls().length;
const deletesBeforeThreadClear = deleteCalls().length;
dispatch($analysisClearThread, 'click', {});
await settle();
eq(threadCalls().length, threadsBefore + 1, 'CLEAR Q&A: ушёл ровно один DELETE …/analysis/thread');
eq(threadCalls()[threadCalls().length - 1].url, '/api/vk/dialogs/5678/analysis/thread',
    'CLEAR Q&A: путь совпадает с серверным контрактом');
eq(deleteCalls().length, deletesBeforeThreadClear,
    'CLEAR Q&A: по адресу разбора не ушло НИ ОДНОГО запроса');
eq(String($analysisThread.textContent), '', 'CLEAR Q&A: DOM треда пуст');
eq(A._debugAnalysisState().thread, 0, 'CLEAR Q&A: история для модели тоже стёрта');
eq(out(), 'РАЗБОР СВЕЖИЙ', 'CLEAR Q&A: разбор остался на месте');

// --- CLEAR анализа: DELETE, пустой вывод, REFRESH гаснет -------------------

net({ clear: { cleared: true } });
const deletesBefore = deleteCalls().length;
dispatch($analysisClear, 'click', {});
await settle();

eq(deleteCalls().length, deletesBefore + 1, 'CLEAR анализа: ушёл ровно один DELETE');
eq(deleteCalls()[deleteCalls().length - 1].url, '/api/vk/dialogs/5678/analysis',
    'CLEAR анализа: путь совпадает с серверным контрактом');
eq(out(), '', 'CLEAR анализа: вывод пуст');
eq(String($analysisUpdated.textContent), '', 'CLEAR анализа: строка даты убрана');
eq($analysisRefresh.disabled, true, 'CLEAR анализа: REFRESH заблокирован — обновлять нечего');
eq($analysisClear.disabled, true, 'CLEAR анализа: повторно стирать нечего');

// Клик по погашенной кнопке (клавиатура, e2e) запроса не порождает.
const startsBefore = startCalls().length;
dispatch($analysisRefresh, 'click', {});
await settle();
eq(startCalls().length, startsBefore, 'REFRESH без разбора: запрос не уходит');

// ---------------------------------------------------------------------------
// 14. Тред с сервера, Markdown и сворачивание разбора (АЧ-И2)
// ---------------------------------------------------------------------------
//
// Три доработки одной жалобы: «после обновления страницы история вопросов
// пропадает», «разбор занимает весь экран», «ответ показан сырым текстом».
//
// Здесь же — граница между сырым текстом и разметкой: во время стрима в блоке
// обязаны быть ТОЛЬКО текстовые узлы (парсить незакрытую разметку на каждой
// дельте — мерцание и работа восемь раз в секунду), а после `analysis_done` —
// дерево элементов.

/** Имена тегов всех ЭЛЕМЕНТОВ поддерева, сверху вниз. */
function elementTags(node) {
    const acc = [];
    const walk = (n) => {
        for (const c of (n.childNodes || [])) {
            if (c.nodeType !== 1) continue;
            acc.push(c.tagName);
            walk(c);
        }
    };
    walk(node);
    return acc;
}

/** Первый элемент поддерева с таким тегом. */
function findTag(node, tag) {
    for (const c of (node.childNodes || [])) {
        if (c.nodeType !== 1) continue;
        if (c.tagName === tag) return c;
        const deep = findTag(c, tag);
        if (deep) return deep;
    }
    return null;
}

net({
    saved: savedDto({
        text: '# Итог\n\nВсё **плохо**\n\n| срок | статус |\n|---|---|\n| май | сорван |',
        updated_at: SAVED_AT,
        messages_used: 33,
        thread: [
            { q: 'Кто виноват?', a: 'Подрядчик', at: 1754000100 },
            { q: 'Что делать?', a: '- ждать\n- звонить', at: 1754000200 },
        ],
    }),
});

A.openAnalysis('9012');
await settle();

eq(A._debugAnalysisState().thread, 2, 'тред из GET: обе пары попали в состояние');
ok(String($analysisThread.textContent).includes('Кто виноват?'),
    'тред из GET: первый вопрос отрисован');
ok(String($analysisThread.textContent).includes('Что делать?'),
    'тред из GET: второй вопрос отрисован');
ok(String($analysisThread.textContent).includes('Подрядчик'),
    'тред из GET: ответ отрисован');
eq($analysisThread.childNodes.length, 2, 'тред из GET: ровно две строки, без дублей');
ok(elementTags($analysisThread).includes('ul'),
    'тред из GET: ответ со списком отрисован Markdown\'ом СРАЗУ, без стадии сырого текста');
ok(!String($analysisThread.textContent).includes('- ждать'),
    'тред из GET: маркеры списка съедены разметкой, а не показаны как текст');

// Сохранённый разбор — тоже целый текст, поэтому Markdown сразу.
ok(elementTags($analysisOut).includes('h1'), 'сохранённый разбор: заголовок стал <h1>');
ok(elementTags($analysisOut).includes('table'), 'сохранённый разбор: таблица стала <table>');
ok(!out().includes('**'), 'сохранённый разбор: символы разметки не показаны как текст');

// --- Две очистки независимы в ОБЕ стороны ---------------------------------

net({ clear: { cleared: true } });
const deletesBeforeIndep = deleteCalls().length;
const threadsBeforeIndep = threadCalls().length;
dispatch($analysisClear, 'click', {});
await settle();
eq(deleteCalls().length, deletesBeforeIndep + 1, 'CLEAR анализа: ушёл DELETE по разбору');
eq(threadCalls().length, threadsBeforeIndep, 'CLEAR анализа: по треду запросов не было');
eq(out(), '', 'CLEAR анализа: разбор стёрт');
eq(A._debugAnalysisState().thread, 2, 'CLEAR анализа: тред НЕ тронут');
ok(String($analysisThread.textContent).includes('Кто виноват?'),
    'CLEAR анализа: пары остались на экране');

// --- Стрим: сырой текст, после done — Markdown ----------------------------

net({ start: startDto('m1') });
dispatch($analysisStart, 'click', {});
await settle();
A.onAnalysisStarted(started({ run_id: 'm1', dialog_id: '9012', kind: 'analyze' }));
A.onAnalysisDelta(delta('| срок | статус |\n', { run_id: 'm1', dialog_id: '9012' }));
await settle();
eq(elementTags($analysisOut), [], 'стрим: в блоке ТОЛЬКО текстовые узлы');
ok(out().includes('| срок | статус |'), 'стрим: показан сырой текст как есть');

A.onAnalysisDelta(delta('|---|---|\n| май | сорван |\n\nСм. [договор](https://example.com/d).',
    { run_id: 'm1', dialog_id: '9012' }));
await settle();
A.onAnalysisDone(done({ run_id: 'm1', dialog_id: '9012' }));

const outTags = elementTags($analysisOut);
ok(outTags.includes('table'), 'после done: таблица перерисована в <table>');
ok(outTags.includes('a'), 'после done: ссылка стала <a>');
const link = findTag($analysisOut, 'a');
eq(link ? link.getAttribute('href') : null, 'https://example.com/d',
    'после done: href ссылки сохранён');
eq(link ? link.getAttribute('target') : null, '_blank',
    'после done: ссылка открывается в новой вкладке');
eq(link ? link.getAttribute('rel') : null, 'noopener noreferrer',
    'после done: у ссылки стоит rel=noopener noreferrer');

// Оборванный ответ Markdown'ом НЕ перерисовывается: у него незакрыты таблицы и
// списки, и «дорисовать» их значило бы показать структуру, которой не было.
net({ start: startDto('m2'), cancel: { cancelled: true } });
dispatch($analysisStart, 'click', {});
await settle();
A.onAnalysisStarted(started({ run_id: 'm2', dialog_id: '9012', kind: 'analyze' }));
A.onAnalysisDelta(delta('# Незакон', { run_id: 'm2', dialog_id: '9012' }));
await settle();
A.onAnalysisDone(done({ run_id: 'm2', dialog_id: '9012', ok: false, reason: 'cancelled' }));
eq(elementTags($analysisOut), [], 'отменённый run: текст остался сырым');
eq(out(), '# Незакон', 'отменённый run: полученный кусок цел');

// --- Ответ на вопрос: тоже сырой в стриме и Markdown после done -----------

net({ start: startDto('q7') });
$analysisQuestion.value = 'Итоги?';
dispatch($analysisAsk, 'submit', {});
await settle();
A.onAnalysisStarted(started({ run_id: 'q7', dialog_id: '9012', kind: 'ask' }));
A.onAnalysisDelta(delta('**Срыв** сроков', { run_id: 'q7', dialog_id: '9012' }));
await settle();
const askRowNode = $analysisThread.childNodes[$analysisThread.childNodes.length - 1];
eq(elementTags(askRowNode.childNodes[1]), [], 'ответ в стриме: только текстовые узлы');
A.onAnalysisDone(done({ run_id: 'q7', dialog_id: '9012' }));
ok(elementTags(askRowNode.childNodes[1]).includes('strong'),
    'ответ после done: разметка отрисована');
eq(A._debugAnalysisState().thread, 3, 'ответ после done: пара добавлена к приехавшим из GET');

// --- Сворачивание ---------------------------------------------------------

eq($analysisOut.hidden, false, 'сворачивание: по умолчанию разбор развёрнут');
eq($analysisOutToggle.getAttribute('aria-expanded'), 'true',
    'сворачивание: aria-expanded=true у развёрнутого');

dispatch($analysisOutToggle, 'click', {});
eq($analysisOut.hidden, true, 'сворачивание: клик спрятал ТОЛЬКО блок разбора');
eq($analysisThread.hidden, false, 'сворачивание: тред остался на экране');
eq($analysisUpdated.hidden, false, 'сворачивание: строка «разобрано до …» осталась');
eq($analysisStatus.hidden, false, 'сворачивание: статус остался');
eq($analysisOutToggle.getAttribute('aria-expanded'), 'false',
    'сворачивание: aria-expanded=false у свёрнутого');
eq(store.get('vk.analysis.collapsed'), '1', 'сворачивание: состояние записано в localStorage');
eq(A._debugAnalysisState().collapsed, true, 'сворачивание: состояние видно в снимке');

dispatch($analysisOutToggle, 'click', {});
eq($analysisOut.hidden, false, 'сворачивание: второй клик разворачивает обратно');
eq(store.get('vk.analysis.collapsed'), '0', 'сворачивание: развёрнутое тоже запоминается');

// Новый разбор обязан развернуть блок: иначе дельты текут в невидимое.
dispatch($analysisOutToggle, 'click', {});
eq($analysisOut.hidden, true, 'авто-разворот: перед запуском блок свёрнут');
net({ start: startDto('m3') });
dispatch($analysisStart, 'click', {});
await settle();
eq($analysisOut.hidden, false, 'авто-разворот: старт разбора развернул блок');
eq(store.get('vk.analysis.collapsed'), '0',
    'авто-разворот: состояние сохранено — после перезагрузки блок останется развёрнутым');
A.onAnalysisDone(done({ run_id: 'm3', dialog_id: '9012', ok: false, reason: 'cancelled' }));

// Недоступное хранилище (Safari private mode) не роняет модуль: переключение
// обязано работать, просто не запоминаться.
const realStore = globalThis.localStorage;
globalThis.localStorage = {
    getItem() { throw new Error('storage disabled'); },
    setItem() { throw new Error('storage disabled'); },
    removeItem() { throw new Error('storage disabled'); },
};
let collapseThrew = false;
try {
    A.setOutCollapsed(true);
    A.toggleOutCollapsed();
} catch (_) {
    collapseThrew = true;
}
globalThis.localStorage = realStore;
ok(!collapseThrew, 'бросающее хранилище: переключение не роняет модуль');
eq($analysisOut.hidden, false, 'бросающее хранилище: состояние всё равно применилось к DOM');

// --- Смена беседы не смешивает треды --------------------------------------

net({
    saved: savedDto({
        text: 'ЧУЖОЙ РАЗБОР',
        updated_at: SAVED_AT,
        thread: [{ q: 'Один вопрос', a: 'Один ответ', at: 1754000300 }],
    }),
});
A.openAnalysis('3333');
await settle();
eq(A._debugAnalysisState().thread, 1, 'смена беседы: тред заменён тредом новой беседы');
eq($analysisThread.childNodes.length, 1, 'смена беседы: в DOM ровно одна строка');
ok(String($analysisThread.textContent).includes('Один вопрос'),
    'смена беседы: показан вопрос НОВОЙ беседы');
ok(!String($analysisThread.textContent).includes('Кто виноват?'),
    'смена беседы: пары прошлой беседы не протекли');

// ---------------------------------------------------------------------------
// 15. Закрытие попапа и инвариант XSS
// ---------------------------------------------------------------------------

dispatch($analysisClose, 'click', {});
eq($analysis.hidden, true, 'крестик закрывает попап');
ok(!A.isAnalysisOpen(), 'крестик: isAnalysisOpen() === false');

const source = readFileSync(new URL('../../static/js/vk/analysis.js', import.meta.url), 'utf8');
ok(!source.includes('innerHTML'),
    'XSS: в модуле нет innerHTML — ответ модели попадает в DOM только текстом');

// Кнопка ASK работает и кликом (мобильные клавиатуры submit порождают не всегда).
ok(typeof A.askQuestion === 'function' && typeof A.startAnalysis === 'function'
    && typeof A.cancelRun === 'function', 'экспорт: START/ASK/STOP доступны наружу');
ok(($analysisSend.listeners.get('click') || []).length > 0, 'ASK: кнопка слушает click');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/vk_analysis)`);
process.exit(failures ? 1 : 0);
