// Регресс-тест вкладки «Контекст чата» попапа анализа (js/vk/analysis.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/vk_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы в null).
//
// ЧТО ИМЕННО ЗДЕСЬ ЗАЩИЩЕНО. Заметка о беседе — единственная часть попапа,
// которая переживает перезагрузку страницы, поэтому её легко сломать молча:
//
//   1. GET уходит ЛЕНИВО (на первом входе на вкладку) и РОВНО ОДИН РАЗ на
//      беседу: бюджет панели — 3 rps на токен, и лишний запрос на каждое
//      переключение вкладки крал бы его у ленты;
//   2. смена беседы ЗАСТАВЛЯЕТ перечитать: иначе вторая беседа показала бы
//      заметку первой, и пользователь отредактировал бы чужой текст;
//   3. SAVE шлёт текст, обрезанный ТЕМ ЖЕ правилом, что и на сервере
//      (`prompts::sanitize`: trim → срез по code points → trim_end), — иначе
//      дописанная фраза пропала бы молча;
//   4. CLEAR шлёт пустую строку (единственный способ стереть заметку);
//   5. ошибка показывается по `err.code`, а не по тексту.
//
// Запуск: node tmux-web/tests/frontend/vk_analysis_prompt.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $analysis, $analysisClose, $analysisError, $analysisPrompt,
    $analysisPromptClear, $analysisPromptCount, $analysisPromptSave,
    $analysisTabCtx, $analysisTabRun, $analysisTarget, $document, dispatch,
    flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

/**
 * Даёт доехать И микрозадачам, И таймеру планировщика.
 *
 * Одного `flush` мало: очередь js/vk/ratelimit.js после исчерпания burst'а
 * взводит `setTimeout(…, 1)`, который срабатывает ПОЗЖЕ `setTimeout(…, 0)`, —
 * тест на одном flush'е зеленел бы через раз.
 */
async function settle() {
    for (let i = 0; i < 4; i += 1) {
        await flush();
        await new Promise((resolve) => setTimeout(resolve, 3));
    }
}

const A = await import(new URL('../../static/js/vk/analysis.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик — настоящий, но с минимальным шагом: 380 мс на запрос превратили
// бы тест в десятисекундный. Проверяется не он (для этого есть
// vk_ratelimit.test.mjs), а то, что модуль вообще проходит через очередь.
//
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

/** Запросы к роуту заметки, в порядке отправки. */
function promptCalls() {
    return netCalls.filter((c) => /\/prompt$/.test(c.url));
}

/** Ответ роута заметки. */
function promptDto(text, updatedAt, maxChars) {
    return { prompt: text, updated_at: updatedAt == null ? null : updatedAt, max_chars: maxChars || 4000 };
}

/** Ошибка в конверте плагина. */
function errRes(status, code, message) {
    return jsonRes(status, { error: { code, message: message || code } });
}

const notices = [];
A.initAnalysis({ onNotice: (t) => notices.push(t) });

// Беседа известна панели: шапка попапа обязана показывать имя, а не id.
S.vkState.dialogs = [{ id: '1234', title: 'Подрядчик', type: 'user', first_name: 'Пётр', last_name: 'Смирнов' }];
S.vkState.activeDialogId = '1234';

// ---------------------------------------------------------------------------
// 1. Ленивость: пока вкладка «Контекст» не открыта, запроса нет
// ---------------------------------------------------------------------------

setNet(async (url) => {
    if (/\/prompt$/.test(url)) return jsonRes(200, promptDto('старая заметка', 1700000000, 4000));
    return jsonRes(200, {});
});

A.openAnalysis();
await settle();

eq($analysis.hidden, false, 'открытие: попап показан');
ok(A.isAnalysisOpen(), 'открытие: isAnalysisOpen() === true');
ok(String($analysisTarget.textContent).length > 0, 'открытие: шапка называет беседу');
eq(promptCalls().length, 0, 'ленивость: на вкладке «Анализ» заметка не запрашивается');
eq(A._debugAnalysisState().promptLoaded, false, 'ленивость: флаг загрузки ещё не поднят');

// ---------------------------------------------------------------------------
// 2. Первый вход на «Контекст» — ровно один GET
// ---------------------------------------------------------------------------

dispatch($analysisTabCtx, 'click', {});
await settle();

eq(promptCalls().length, 1, 'первый вход: ушёл один GET');
eq(promptCalls()[0].method, 'GET', 'первый вход: именно GET');
eq(promptCalls()[0].url, '/api/vk/dialogs/1234/prompt', 'первый вход: путь совпадает с серверным контрактом');
eq($analysisPrompt.value, 'старая заметка', 'первый вход: текст подставлен в поле');
eq($analysisPromptCount.textContent, '14 / 4000', 'первый вход: счётчик символов от max_chars сервера');
eq(A._debugAnalysisState().tab, 'ctx', 'первый вход: вкладка переключена');

// Возврат на «Анализ» и обратно — второго запроса быть не должно.
dispatch($analysisTabRun, 'click', {});
await settle();
dispatch($analysisTabCtx, 'click', {});
await settle();
eq(promptCalls().length, 1, 'повторное переключение: второго GET нет');

// ---------------------------------------------------------------------------
// 3. SAVE: PUT с текстом, обрезанным по правилу сервера
// ---------------------------------------------------------------------------

// max_chars намеренно маленький: обрезка обязана быть видимой в теле запроса.
setNet(async (url, init) => {
    if (/\/prompt$/.test(url) && init.method === 'PUT') {
        const sent = JSON.parse(init.body).prompt;
        return jsonRes(200, promptDto(sent, 1700000100, 10));
    }
    if (/\/prompt$/.test(url)) return jsonRes(200, promptDto('', null, 10));
    return jsonRes(200, {});
});

// Сервер сообщил новый потолок только что — эмулируем через повторное чтение
// после смены беседы (ниже), а пока проверим чистую функцию обрезки.
eq(A.clampPrompt('  привет мир  ', 10), 'привет мир', 'clampPrompt: trim по краям');
eq(A.clampPrompt('привет большой мир', 10), 'привет бол', 'clampPrompt: срез по символам');
eq(A.clampPrompt('привет      большой', 8), 'привет', 'clampPrompt: trim_end у среза');
eq(A.charCount('раз🙂'), 4, 'charCount: emoji считается одним code point');

$analysisPrompt.value = 'новая заметка';
dispatch($analysisPrompt, 'input', {});
eq($analysisPromptCount.textContent, '13 / 4000', 'ввод: счётчик пересчитан');
eq(A._debugAnalysisState().promptDirty, true, 'ввод: заметка помечена изменённой');

dispatch($analysisPromptSave, 'click', {});
await settle();

const puts = promptCalls().filter((c) => c.method === 'PUT');
eq(puts.length, 1, 'SAVE: ушёл один PUT');
eq(puts[0].body, { prompt: 'новая заметка' }, 'SAVE: тело — {prompt}');
eq(A._debugAnalysisState().promptDirty, false, 'SAVE: флаг изменённости снят');
eq(A._debugAnalysisState().maxChars, 10, 'SAVE: потолок обновлён ответом сервера');

// Ctrl+Enter в поле сохраняет так же, как кнопка.
$analysisPrompt.value = 'через клавиатуру';
dispatch($analysisPrompt, 'keydown', { key: 'Enter', ctrlKey: true });
await settle();
const puts2 = promptCalls().filter((c) => c.method === 'PUT');
eq(puts2.length, 2, 'Ctrl+Enter: ушёл второй PUT');
eq(puts2[1].body, { prompt: 'через клав' }, 'Ctrl+Enter: текст обрезан по max_chars сервера');

// Просто Enter (без модификатора) — это перенос строки, а не сохранение.
$analysisPrompt.value = 'ещё строка';
dispatch($analysisPrompt, 'keydown', { key: 'Enter' });
await settle();
eq(promptCalls().filter((c) => c.method === 'PUT').length, 2, 'Enter без модификатора не сохраняет');

// ---------------------------------------------------------------------------
// 4. CLEAR: пустая строка и пустое поле
// ---------------------------------------------------------------------------

dispatch($analysisPromptClear, 'click', {});
await settle();

const puts3 = promptCalls().filter((c) => c.method === 'PUT');
eq(puts3.length, 3, 'CLEAR: ушёл PUT');
eq(puts3[2].body, { prompt: '' }, 'CLEAR: тело — пустая строка');
eq($analysisPrompt.value, '', 'CLEAR: поле очищено');

// ---------------------------------------------------------------------------
// 5. Ошибка показывается по коду, а не по тексту
// ---------------------------------------------------------------------------

setNet(async (url, init) => {
    if (/\/prompt$/.test(url) && init.method === 'PUT') {
        return errRes(500, 'storage_failed', 'io error: /home/u/.config/devforge/vk-prompts.json');
    }
    return jsonRes(200, promptDto('', null, 10));
});

$analysisPrompt.value = 'не сохранится';
dispatch($analysisPromptSave, 'click', {});
await settle();

eq($analysisError.hidden, false, 'ошибка: строка ошибки показана');
ok(String($analysisError.textContent).includes('права'),
    'ошибка: текст выбран по коду storage_failed, а не скопирован с сервера');
ok(!String($analysisError.textContent).includes('vk-prompts.json'),
    'ошибка: путь с сервера пользователю не показывается');
eq(A.analysisErrorText({ code: 'claude_unavailable' }).includes('Claude CLI'), true,
    'ошибка: claude_unavailable подсказывает про установку CLI');
eq(A.analysisErrorText({ code: 'invalid_state' }).includes('уже идёт'), true,
    'ошибка: invalid_state говорит про идущий анализ');
ok(notices.length > 0, 'ошибка: пользователь дополнительно уведомлён тостом');

// ---------------------------------------------------------------------------
// 6. Смена беседы — заметка перечитывается
// ---------------------------------------------------------------------------

setNet(async (url) => {
    if (url === '/api/vk/dialogs/777/prompt') return jsonRes(200, promptDto('заметка второй беседы', 1700000200, 4000));
    if (/\/prompt$/.test(url)) return jsonRes(200, promptDto('старая заметка', 1700000000, 4000));
    return jsonRes(200, {});
});

// Уходим на вкладку «Анализ»: иначе открытие второй беседы сразу же дёрнет
// заметку (вкладка контекста осталась бы активной), и «сброшенный флаг» было
// бы не увидеть — он поднимается тем же кадром.
dispatch($analysisTabRun, 'click', {});
await settle();

const before = promptCalls().length;
A.openAnalysis('777');

eq(A._debugAnalysisState().dialogId, '777', 'смена беседы: id обновлён');
eq(A._debugAnalysisState().promptLoaded, false, 'смена беседы: флаг загрузки сброшен');
eq($analysisPrompt.value, '', 'смена беседы: поле очищено от чужого текста');
eq($analysisTarget.textContent, '777', 'смена беседы: незнакомая беседа названа своим id');
eq(promptCalls().length - before, 0, 'смена беседы: сама по себе запроса не шлёт');

dispatch($analysisTabCtx, 'click', {});
await settle();
eq(promptCalls().length - before, 1, 'смена беседы: заметка перечитана ровно одним запросом');
eq($analysisPrompt.value, 'заметка второй беседы', 'смена беседы: показана заметка НОВОЙ беседы');

// ---------------------------------------------------------------------------
// 7. Закрытие: крестик, подложка, Esc
// ---------------------------------------------------------------------------

dispatch($analysisClose, 'click', {});
eq($analysis.hidden, true, 'крестик закрывает попап');
ok(!A.isAnalysisOpen(), 'крестик: isAnalysisOpen() === false');

A.openAnalysis('777');
dispatch($analysis, 'click', { target: $analysis });
eq($analysis.hidden, true, 'клик по подложке закрывает попап');

A.openAnalysis('777');
{
    const e = dispatch($document, 'keydown', { key: 'Escape' });
    eq($analysis.hidden, true, 'Esc закрывает попап');
    eq(e.defaultPrevented, true, 'Esc: событие погашено');
    eq(e.propagationStopped, true, 'Esc: не проваливается к лайтбоксу под попапом');
}

// Закрытый попап Esc не трогает — иначе он съедал бы клавишу у ленты.
{
    const e = dispatch($document, 'keydown', { key: 'Escape' });
    eq(e.defaultPrevented, false, 'Esc при закрытом попапе не перехватывается');
}

// ---------------------------------------------------------------------------
// 8. teardown снимает слушатель с документа
// ---------------------------------------------------------------------------

const listenersBefore = ($document.listeners.get('keydown') || []).length;
A.teardownAnalysis();
eq(($document.listeners.get('keydown') || []).length, listenersBefore - 1,
    'teardown: слушатель Esc снят с документа');
eq(A._debugAnalysisState().bound, false, 'teardown: модуль считается развязанным');
eq(A._debugAnalysisState().dialogId, null, 'teardown: состояние беседы очищено');

// Повторный init поднимает всё обратно и НЕ вешает второй слушатель.
A.initAnalysis({});
A.initAnalysis({});
eq(($document.listeners.get('keydown') || []).length, listenersBefore,
    'повторный init: слушатель ровно один');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/vk_analysis_prompt)`);
process.exit(failures ? 1 : 0);
