// ⚠ СТРАЖ «ОДНО НАЖАТИЕ Esc — РОВНО ОДНО ОКНО» (forge-o0sdg).
//
// Почему этот тест существует отдельно от meet_popup.test.mjs и
// messenger_analysis_core.test.mjs: каждый из них поднимает СВОЙ документ и
// СВОЙ единственный модуль. Дефект же возникает ровно тогда, когда на ОДНОМ
// узле (document) висят слушатели ДВУХ модулей сразу — а такой связки не
// проверял никто. Оба модуля защищались вызовом `stopPropagation()`, который
// между слушателями одного и того же узла не действует вовсе (он останавливает
// всплытие ВЫШЕ по дереву, а не остальных слушателей этого узла), и живая
// приёмка в Chrome показала закрытие двух окон одним Esc во всех трёх панелях.
//
// Проверяется:
//   1. КАРТОЧКА ВСТРЕЧИ ПОВЕРХ ПОПАПА АНАЛИЗА — оба модуля на одном документе,
//      одно нажатие закрывает ТОЛЬКО карточку (она выше), второе — попап.
//   2. ОБРАТНЫЙ СЦЕНАРИЙ: когда открыто одно окно, Esc обязан его закрыть.
//      Чинить «поглощением» Esc нельзя.
//   3. РЕЕСТР СЛОЁВ (`messenger/esc-stack.js`): верхний выбирается по ЗАЯВЛЕННОЙ
//      ГЛУБИНЕ, а не по порядку регистрации модулей — иначе поведение зависело
//      бы от порядка импортов.
//   4. НА ДОКУМЕНТЕ ЖИВЁТ ОДИН слушатель Esc на все слои, и он снимается, когда
//      снят последний слой (bfcache: переживший выгрузку слушатель стреляет по
//      мёртвому DOM).
//
// МУТАЦИОННАЯ ПРОВЕРКА: вернуть в meet-popup.js собственный
// `document.addEventListener('keydown', …)` со `stopPropagation()` — краснеет
// раздел 1.
//
// Запуск: node tmux-web/tests/frontend/messenger_esc_layers.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { dispatch, makeDoc } from './meet_popup_dom.mjs';

const env = makeDoc(['telegram']);

// Ядро анализа работает с ГЛОБАЛЬНЫМ `document` (в отличие от карточки, которой
// документ передают конфигом). Чтобы оба модуля оказались на ОДНОМ узле — а
// именно это и воспроизводит дефект — глобаль подменяется тем же объектом.
globalThis.document = env.doc;
const memory = new Map();
globalThis.localStorage = {
    getItem: (k) => (memory.has(k) ? memory.get(k) : null),
    setItem: (k, v) => { memory.set(k, String(v)); },
    removeItem: (k) => { memory.delete(k); },
};
globalThis.requestAnimationFrame = env.raf;
globalThis.cancelAnimationFrame = env.caf;

const POPUP = await import(new URL('../../static/js/messenger/meet-popup.js', import.meta.url).href);
const CORE = await import(new URL('../../static/js/messenger/analysis-core.js', import.meta.url).href);
const STACK = await import(new URL('../../static/js/messenger/esc-stack.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function ok(cond, label) {
    checks += 1;
    if (cond) return;
    failures += 1;
    console.error(`[FAIL] ${label}`);
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) return;
    failures += 1;
    console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms || 0));

/** Заглушка транспорта: обоим модулям важно лишь то, что вызовы дают Promise. */
function stubApi() {
    const calls = [];
    const log = (name) => (...args) => {
        calls.push({ name, args });
        return Promise.resolve({});
    };
    return {
        calls,
        // analysis-core
        getPrompt: log('getPrompt'),
        putPrompt: log('putPrompt'),
        getSavedAnalysis: log('getSavedAnalysis'),
        clearAnalysis: log('clearAnalysis'),
        clearThread: log('clearThread'),
        start: log('start'),
        cancel: log('cancel'),
        // meet-popup
        getMeeting: log('getMeeting'),
        getSummary: log('getSummary'),
        getTranscript: (id, opts) => {
            calls.push({ name: 'getTranscript', args: [id, opts] });
            return Promise.resolve({ segments: [], total: 0 });
        },
        startMeeting: log('startMeeting'),
        stopMeeting: log('stopMeeting'),
        cancelMeeting: log('cancelMeeting'),
        confirmMeeting: log('confirmMeeting'),
        showMeetingWindow: log('showMeetingWindow'),
        retryMeeting: log('retryMeeting'),
    };
}

const MEET_DETAIL = {
    key: 'telegram|777000|7',
    panel: 'telegram',
    chatId: '777000',
    messageId: 7,
    meet: {
        url: 'https://telemost.yandex.ru/j/60497752704486',
        title: 'Созвон',
        platform: 'telemost',
        localKey: 'telemost:60497752704486',
    },
    state: null,
};

/** Попап анализа на глобальном документе, с минимумом узлов. */
function makeAnalysis(prefix) {
    const root = env.doc.createElement('div');
    root.hidden = true;
    const panel = CORE.createAnalysisPanel({
        prefix,
        refs: { root },
        api: stubApi(),
        activeDialogId: () => '4242',
    });
    panel.init();
    return panel;
}

// ===========================================================================
// 1. Карточка встречи поверх попапа анализа: одно нажатие — одно окно
// ===========================================================================

{
    const analysis = makeAnalysis('tg');
    const card = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: stubApi(),
        raf: env.raf, caf: env.caf,
    });

    analysis.open('4242');
    card.open(MEET_DETAIL);
    await sleep(0);
    eq([analysis.isOpen(), card.isOpen()], [true, true], 'оба окна открыты — исходная позиция');

    env.keydown('Escape');
    eq(card.isOpen(), false, 'Esc закрыл карточку встречи — она лежит ПОВЕРХ попапа');
    eq(analysis.isOpen(), true,
        'и НЕ тронул попап анализа: одно нажатие закрывает РОВНО ОДНО окно');

    env.keydown('Escape');
    eq(analysis.isOpen(), false, 'второе нажатие закрывает попап анализа');

    // 2. Обратный сценарий: одно открытое окно обязано закрываться.
    analysis.open('4242');
    await sleep(0);
    env.keydown('Escape');
    eq(analysis.isOpen(), false, 'ОДИНОЧНЫЙ попап анализа закрывается по Esc');

    card.open(MEET_DETAIL);
    await sleep(0);
    env.keydown('Escape');
    eq(card.isOpen(), false, 'ОДИНОЧНАЯ карточка встречи закрывается по Esc');

    // Обратный порядок открытия ничего не меняет: верх определяется глубиной
    // слоя, а не тем, что открыли последним.
    card.open(MEET_DETAIL);
    analysis.open('4242');
    await sleep(0);
    env.keydown('Escape');
    eq([card.isOpen(), analysis.isOpen()], [false, true],
        'даже если анализ открыт ПОЗЖЕ карточки, Esc забирает карточку (она выше)');
    analysis.close();

    // Esc не при открытых окнах не глотается — соседи (лайтбокс, боковая
    // колонка) обязаны получить своё нажатие.
    const seen = [];
    const spy = (e) => { seen.push(!!e.defaultPrevented); };
    env.doc.addEventListener('keydown', spy);
    env.keydown('Escape');
    eq(seen, [false],
        'при закрытых окнах Esc уходит соседям неотменённым');
    env.doc.removeEventListener('keydown', spy);

    card.teardown();
    analysis.teardown();
    eq(env.docListeners('keydown'), 0,
        'после teardown обоих на документе не осталось ни одного слушателя Esc');
}

// ===========================================================================
// 3. Реестр слоёв: верхний — по глубине, а не по порядку регистрации
// ===========================================================================

{
    const closed = [];
    const low = { open: true };
    const high = { open: true };

    // Верхний слой регистрируется ПЕРВЫМ — если бы решал порядок регистрации
    // (как решал бы `stopImmediatePropagation`), Esc ушёл бы не туда.
    const offHigh = STACK.registerEscLayer({
        doc: env.doc,
        name: 'high',
        depth: STACK.ESC_DEPTH.MEET_CARD,
        isOpen: () => high.open,
        close: () => { high.open = false; closed.push('high'); },
    });
    const offLow = STACK.registerEscLayer({
        doc: env.doc,
        name: 'low',
        depth: STACK.ESC_DEPTH.REPLY_BAR,
        isOpen: () => low.open,
        close: () => { low.open = false; closed.push('low'); },
    });

    eq(env.docListeners('keydown'), 1, 'на документе ОДИН слушатель Esc на все слои');
    eq(STACK.topEscLayer(env.doc).name, 'high', 'верхний слой — самый глубокий из открытых');

    env.keydown('Escape');
    eq(closed, ['high'], 'закрылся только верхний');
    eq(STACK.topEscLayer(env.doc).name, 'low', 'теперь верхний — оставшийся');

    env.keydown('Escape');
    eq(closed, ['high', 'low'], 'следующее нажатие закрывает нижний');
    eq(STACK.topEscLayer(env.doc), null, 'открытых слоёв не осталось');

    // Чужие клавиши слоям не принадлежат.
    low.open = true;
    env.keydown('Enter');
    eq(low.open, true, 'не-Esc реестр не трогает');

    offHigh();
    offLow();
    eq(env.docListeners('keydown'), 0, 'снятие последнего слоя снимает и слушатель');
    eq(STACK.topEscLayer(env.doc), null, 'реестр документа пуст');
}

// ===========================================================================
// 4. Фейковый документ обязан вести себя как браузер
// ===========================================================================

{
    const node = env.doc.createElement('div');
    const seen = [];
    node.addEventListener('keydown', (e) => { seen.push('a'); e.stopPropagation(); });
    node.addEventListener('keydown', () => { seen.push('b'); });
    dispatch(node, 'keydown', { key: 'Escape' });
    eq(seen, ['a', 'b'],
        'stopPropagation НЕ отменяет соседних слушателей ТОГО ЖЕ узла — как в браузере');

    const seen2 = [];
    const node2 = env.doc.createElement('div');
    node2.addEventListener('keydown', (e) => { seen2.push('a'); e.stopImmediatePropagation(); });
    node2.addEventListener('keydown', () => { seen2.push('b'); });
    dispatch(node2, 'keydown', { key: 'Escape' });
    eq(seen2, ['a'], 'а stopImmediatePropagation — отменяет');
}

console.log(`\n${failures === 0 ? '[ok]' : '[FAIL]'} messenger_esc_layers: ${checks - failures}/${checks} проверок`);
process.exit(failures === 0 ? 0 : 1);
