// СТРАЖ ОБЩЕГО ЯДРА попапа «Анализ чата» (js/messenger/analysis-core.js).
//
// Поведение панели целиком проверяется на живой панели VK
// (`vk_analysis.test.mjs`, 181 ассерт, и `vk_analysis_prompt.test.mjs`, 52) —
// дублировать те сценарии здесь незачем. Настоящий тест закрывает ровно то,
// чего та пара проверить НЕ МОЖЕТ, потому что смотрит на одну панель и один
// префикс:
//
//   1. ИЗОЛЯЦИЯ ЭКЗЕМПЛЯРОВ. Ядро — фабрика, а не модуль с общим состоянием.
//      Когда рядом с VK появятся Telegram и Bitrix24, три попапа будут жить на
//      одной странице одновременно; общий `let` означал бы, что открытие
//      второго стирает беседу первого и уводит его дельты в чужой узел.
//   2. КЛЮЧИ ХРАНИЛИЩА СТРОЯТСЯ ИЗ ПРЕФИКСА. Хардкод `vk.` заставил бы три
//      панели делить одну ячейку: выбор глубины в одной молча менял бы её в
//      остальных.
//   3. ПОДПИСИ ОШИБОК: общая таблица + карта панели поверх неё, и неизвестный
//      код НЕ роняет попап, а показывает сообщение сервера.
//   4. ИНВАРИАНТ XSS ПО ИСХОДНИКУ. `vk_analysis.test.mjs` читает исходник
//      обёртки — после выноса поведения запретное свойство могло бы появиться
//      здесь и остаться незамеченным.
//   5. ЯДРО НЕ ЗНАЕТ ПРО КОНКРЕТНЫЙ МЕССЕНДЖЕР: ни одного импорта из js/vk/**
//      и ни одного упоминания его оверлея.
//
// Запуск: node tmux-web/tests/frontend/messenger_analysis_core.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

// ---------------------------------------------------------------------------
// Минимальный DOM: ядру нужен только `document` под слушатель Esc и под
// createElement/createTextNode строк треда. Полноценный стаб панели здесь был
// бы лишним — сценарии с разметкой закрыты тестами VK.
// ---------------------------------------------------------------------------

const docListeners = new Map();
globalThis.document = {
    listeners: docListeners,
    addEventListener(type, fn) {
        if (!docListeners.has(type)) docListeners.set(type, []);
        docListeners.get(type).push(fn);
    },
    removeEventListener(type, fn) {
        const list = docListeners.get(type) || [];
        const i = list.indexOf(fn);
        if (i >= 0) list.splice(i, 1);
    },
    createElement: (tag) => ({ tagName: tag, childNodes: [], appendChild() {}, textContent: '' }),
    createTextNode: (t) => ({ nodeType: 3, textContent: String(t) }),
};

/** Хранилище с памятью: без неё проверка «ключи разные» была бы пустой. */
const store = new Map();
globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => { store.set(k, String(v)); },
    removeItem: (k) => { store.delete(k); },
};

const CORE_URL = new URL('../../static/js/messenger/analysis-core.js', import.meta.url);
const C = await import(CORE_URL.href);
const STACK = await import(
    new URL('../../static/js/messenger/esc-stack.js', import.meta.url).href);

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

/** Транспорт-заглушка: ядру важно только то, что вызовы возвращают Promise. */
function stubApi() {
    const calls = [];
    const record = (name) => (...args) => {
        calls.push({ name, args });
        return Promise.resolve({});
    };
    return {
        calls,
        getPrompt: record('getPrompt'),
        putPrompt: record('putPrompt'),
        getSavedAnalysis: record('getSavedAnalysis'),
        clearAnalysis: record('clearAnalysis'),
        clearThread: record('clearThread'),
        start: record('start'),
        cancel: record('cancel'),
    };
}

// ---------------------------------------------------------------------------
// 1. Ключи хранилища строятся из префикса
// ---------------------------------------------------------------------------

eq(C.depthStorageKey('vk'), 'vk.analysis.depth', 'ключ глубины VK — тот же, что был до выноса ядра');
eq(C.collapseStorageKey('vk'), 'vk.analysis.collapsed', 'ключ сворачивания VK — тот же, что был');
ok(C.depthStorageKey('tg') !== C.depthStorageKey('vk'), 'глубина: у панелей РАЗНЫЕ ячейки');
ok(C.collapseStorageKey('b24') !== C.collapseStorageKey('vk'),
    'сворачивание: у панелей РАЗНЫЕ ячейки');

// ---------------------------------------------------------------------------
// 2. Подписи ошибок: таблица ядра, карта панели поверх, неизвестный код
// ---------------------------------------------------------------------------

ok(C.analysisErrorText({ code: 'claude_unavailable' }).includes('Claude CLI'),
    'ошибка: claude_unavailable подсказывает про установку CLI');
ok(C.analysisErrorText({ code: 'invalid_state' }).includes('уже идёт'),
    'ошибка: invalid_state говорит про идущий анализ');
eq(C.analysisErrorText({ code: 'not_connected' }, { not_connected: 'нет связи с VK' }),
    'нет связи с VK', 'ошибка: карта панели перекрывает общую подпись');
eq(C.analysisErrorText({ code: 'storage_failed' }, { not_connected: 'нет связи с VK' }),
    C.analysisErrorText({ code: 'storage_failed' }),
    'ошибка: карта панели не сбивает подписи, которых в ней нет');
eq(C.analysisErrorText({ code: 'нет такого кода', message: 'что-то пошло не так' }),
    'что-то пошло не так', 'ошибка: неизвестный код показывает сообщение сервера');
eq(C.analysisErrorText({ code: 'нет такого кода' }), 'неизвестная ошибка',
    'ошибка: неизвестный код без сообщения — общая формулировка');
eq(C.analysisErrorText(null), 'неизвестная ошибка', 'ошибка: null не роняет разбор');
// Наследование прототипа не должно подсовывать «подпись» из Object.prototype.
eq(C.analysisErrorText({ code: 'toString', message: 'сервер сказал так' }), 'сервер сказал так',
    'ошибка: код из Object.prototype не считается известным');

// ---------------------------------------------------------------------------
// 3. Изоляция экземпляров: две панели на одной странице слепы друг к другу
// ---------------------------------------------------------------------------

const apiA = stubApi();
const apiB = stubApi();
const panelA = C.createAnalysisPanel({
    prefix: 'vk', refs: {}, api: apiA, activeDialogId: () => null,
});
const panelB = C.createAnalysisPanel({
    prefix: 'tg', refs: {}, api: apiB, activeDialogId: () => null,
});

panelA.init({});
panelB.init({});

panelA.open('111');
panelB.open('222');

eq(panelA.debugState().dialogId, '111', 'изоляция: первая панель держит свою беседу');
eq(panelB.debugState().dialogId, '222', 'изоляция: вторая панель держит свою');
ok(apiA.calls.some((c) => c.name === 'getSavedAnalysis' && c.args[0] === '111'),
    'изоляция: разбор запрошен по беседе ПЕРВОЙ панели');
ok(apiB.calls.every((c) => c.args[0] !== '111'),
    'изоляция: вторая панель по чужой беседе не ходила');

panelA.close();
eq(panelA.isOpen(), false, 'изоляция: закрытие первой панели её и закрыло');
eq(panelB.isOpen(), true, 'изоляция: вторая панель осталась открытой');

// Кадр чужой беседы не трогает ни одну из панелей.
panelA.onStarted({ type: 'analysis_started', run_id: 'x', dialog_id: '999', kind: 'analyze' });
eq(panelA.debugState().runId, null, 'изоляция: кадр чужой беседы не перехватил run');

// Сворачивание пишется в СВОЮ ячейку.
panelA.setOutCollapsed(true);
eq(store.get('vk.analysis.collapsed'), '1', 'сворачивание: первая панель записала свой ключ');
eq(store.has('tg.analysis.collapsed'), false, 'сворачивание: чужая ячейка не тронута');
eq(panelB.debugState().collapsed, false, 'сворачивание: вторая панель осталась развёрнутой');

// ---------------------------------------------------------------------------
// 4. teardown снимает слой Esc ровно своей панели
// ---------------------------------------------------------------------------
//
// ⚠ Считать слушатели `keydown` на документе тут больше нельзя, и это не
// послабление, а суть исправления forge-o0sdg: Esc обрабатывает ОДИН слушатель
// общего реестра (`messenger/esc-stack.js`) на все окна страницы. Пока два
// попапа собственными слушателями делили документ, ни один из них не мог
// оставить нажатие себе — `stopPropagation()` между слушателями одного узла не
// действует, и одно нажатие закрывало два окна. Поэтому проверяется теперь
// СОСТАВ РЕЕСТРА, а на документе слушатель ровно один при любом числе панелей.

const stackLayers = () => STACK._debugEscStack(globalThis.document).layers.map((l) => l.name);

eq((docListeners.get('keydown') || []).length, 1,
    'на документе ОДИН слушатель Esc — сколько бы попапов ни жило на странице');
eq(stackLayers(), ['vk-analysis-popup', 'tg-analysis-popup'],
    'обе панели зарегистрированы слоями реестра');

panelA.teardown();
eq(stackLayers(), ['tg-analysis-popup'], 'teardown: снят слой ровно своей панели');
eq(panelA.debugState().bound, false, 'teardown: панель считается развязанной');
eq(panelA.debugState().dialogId, null, 'teardown: состояние беседы очищено');
eq(panelB.debugState().bound, true, 'teardown: вторая панель не задета');

// Повторный init второго слоя не заводит.
panelA.init({});
panelA.init({});
eq(stackLayers(), ['tg-analysis-popup', 'vk-analysis-popup'],
    'повторный init: слой ровно один');
eq((docListeners.get('keydown') || []).length, 1,
    'повторный init: слушатель на документе по-прежнему один');

// ---------------------------------------------------------------------------
// 5. Мусорные кадры не роняют обработчики
// ---------------------------------------------------------------------------

const junk = [
    null, undefined, 0, '', 'analysis_delta', [], {},
    { type: 'analysis_delta' }, { run_id: 'r1' },
    { type: 'analysis_done', dialog_id: null, run_id: null },
];
let threw = null;
try {
    for (const j of junk) {
        panelB.onStarted(j);
        panelB.onDelta(j);
        panelB.onDone(j);
        panelB.onError(j);
    }
} catch (e) {
    threw = e;
}
eq(threw, null, 'мусор: ни один обработчик не бросил исключение');

// ---------------------------------------------------------------------------
// 6. Инвариант XSS и независимость от конкретного мессенджера — по исходнику
// ---------------------------------------------------------------------------

const source = readFileSync(CORE_URL, 'utf8');
ok(!source.includes('inner' + 'HTML'),
    'XSS: в ядре нет записи разметки строкой — текст модели попадает в DOM только текстом');
ok(!/from\s+'\.\.\/(vk|telegram|bitrix)\//.test(source),
    'ядро не импортирует ничего из модулей конкретной панели');
ok(!/captcha/i.test(source),
    'ядро не знает про оверлей конкретной панели — только про isBlockingOverlayOpen');
ok(source.includes('isBlockingOverlayOpen'),
    'самопроверка: развязка с оверлеем в ядре всё-таки есть');
ok(!/\$vk[A-Z]/.test(source), 'ядро не тянет ref\'ы панели VK');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/messenger_analysis_core)`);
process.exit(failures ? 1 : 0);
