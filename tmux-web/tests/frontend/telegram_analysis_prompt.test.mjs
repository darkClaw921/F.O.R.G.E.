// Регресс-тест вкладки «Контекст чата» попапа анализа (js/telegram/analysis.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/telegram_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы в null).
//
// Близнец vk_analysis_prompt.test.mjs. Поведение живёт в общем ядре
// js/messenger/analysis-core.js, но ПРОВОДКА порта своя (пути
// `/api/telegram/…`, подписи ошибок плагина, имя чата из tgState), и рвётся
// она молча.
//
// ЧТО ИМЕННО ЗДЕСЬ ЗАЩИЩЕНО. Заметка о чате — единственная часть попапа,
// которая переживает перезагрузку страницы, поэтому её легко сломать молча:
//
//   1. GET уходит ЛЕНИВО (на первом входе на вкладку) и РОВНО ОДИН РАЗ на чат:
//      попап открывают и закрывают десятками нажатий, и перечитывать
//      неизменившуюся заметку незачем;
//   2. смена чата ЗАСТАВЛЯЕТ перечитать: иначе второй чат показал бы заметку
//      первого, и пользователь отредактировал бы чужой текст;
//   3. SAVE шлёт текст, обрезанный ТЕМ ЖЕ правилом, что и на сервере
//      (`prompts::sanitize`: trim → срез по code points → trim_end), — иначе
//      дописанная фраза пропала бы молча;
//   4. CLEAR шлёт пустую строку (единственный способ стереть заметку);
//   5. ошибка показывается по `err.code`, а не по тексту, и коды здесь
//      ТЕЛЕГРАМОВСКИЕ (`flood_wait` вместо VK-шного `rate_limited`).
//
// ЧЕГО ЗДЕСЬ НЕТ ПО СРАВНЕНИЮ С VK-версией (ровно две группы, обе неприменимы
// к Telegram, а не «забыты»):
//
//   * НЕПРИМЕНИМО — CAPTCHA. У VK Esc может принадлежать блокирующему оверлею
//     captcha, стоящему ВЫШЕ попапа. У Telegram такого оверлея нет вовсе
//     (`isBlockingOverlayOpen: () => false`), поэтому Esc всегда закрывает
//     попап — это и проверяется ниже, без ветки «оверлей открыт».
//   * НЕПРИМЕНИМО — КЛИЕНТСКИЙ ЛИМИТЕР 3 rps. VK-версия поднимает
//     js/vk/ratelimit.js и приземляет `minIntervalMs`, потому что запросы
//     панели VK идут через очередь на токен. У Telegram лимитера нет: вызовы
//     идут прямо через js/telegram/api.js.
//
// Запуск: node tmux-web/tests/frontend/telegram_analysis_prompt.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $analysis, $analysisClose, $analysisError, $analysisPrompt,
    $analysisPromptClear, $analysisPromptCount, $analysisPromptSave,
    $analysisPromptStatus, $analysisTabCtx, $analysisTabRun, $analysisTarget,
    $document, dispatch, flush, jsonRes, netCalls, setNet,
} from './telegram_dom_stub.mjs';

/** Даёт доехать микрозадачам транспорта и кадру анимации стаба. */
async function settle() {
    for (let i = 0; i < 4; i += 1) {
        await flush();
        await new Promise((resolve) => setTimeout(resolve, 3));
    }
}

const A = await import(new URL('../../static/js/telegram/analysis.js', import.meta.url).href);
const S = await import(new URL('../../static/js/telegram/state.js', import.meta.url).href);

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

// Чат известен панели: шапка попапа обязана показывать имя, а не id.
S.tgState.dialogs = [{ id: '1234', title: 'Подрядчик', username: 'petr' }];
S.tgState.activeDialogId = '1234';

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
eq(String($analysisTarget.textContent), 'Подрядчик',
    'открытие: шапка называет чат ИМЕНЕМ из tgState, а не его id');
eq(promptCalls().length, 0, 'ленивость: на вкладке «Анализ» заметка не запрашивается');
eq(A._debugAnalysisState().promptLoaded, false, 'ленивость: флаг загрузки ещё не поднят');

// ---------------------------------------------------------------------------
// 2. Первый вход на «Контекст» — ровно один GET
// ---------------------------------------------------------------------------

dispatch($analysisTabCtx, 'click', {});
await settle();

eq(promptCalls().length, 1, 'первый вход: ушёл один GET');
eq(promptCalls()[0].method, 'GET', 'первый вход: именно GET');
eq(promptCalls()[0].url, '/api/telegram/dialogs/1234/prompt',
    'первый вход: путь совпадает с серверным контрактом плагина Telegram');
eq($analysisPrompt.value, 'старая заметка', 'первый вход: текст подставлен в поле');
eq($analysisPromptCount.textContent, '14 / 4000', 'первый вход: счётчик символов от max_chars сервера');
eq(A._debugAnalysisState().tab, 'ctx', 'первый вход: вкладка переключена');
ok(String($analysisPromptStatus.textContent).length > 0,
    'первый вход: статус заметки не молчит после удачной загрузки');

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

// Чистые функции обрезки — то же правило, что у сервера.
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
eq(puts[0].url, '/api/telegram/dialogs/1234/prompt', 'SAVE: PUT по тому же пути, что и GET');
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
        return errRes(500, 'storage_failed', 'io error: /home/u/.config/devforge/telegram-prompts.json');
    }
    return jsonRes(200, promptDto('', null, 10));
});

$analysisPrompt.value = 'не сохранится';
dispatch($analysisPromptSave, 'click', {});
await settle();

eq($analysisError.hidden, false, 'ошибка: строка ошибки показана');
ok(String($analysisError.textContent).includes('права'),
    'ошибка: текст выбран по коду storage_failed, а не скопирован с сервера');
ok(!String($analysisError.textContent).includes('telegram-prompts.json'),
    'ошибка: путь с сервера пользователю не показывается');
eq(A.analysisErrorText({ code: 'claude_unavailable' }).includes('Claude CLI'), true,
    'ошибка: claude_unavailable подсказывает про установку CLI');
eq(A.analysisErrorText({ code: 'invalid_state' }).includes('уже идёт'), true,
    'ошибка: invalid_state говорит про идущий анализ');
// Коды плагина Telegram: подписи, где стоит имя мессенджера, — свои, и код
// ограничения скорости тоже свой (`flood_wait`, а не VK-шный `rate_limited`).
ok(A.analysisErrorText({ code: 'flood_wait' }).includes('Telegram'),
    'ошибка: flood_wait — код Telegram, подпись называет мессенджер');
ok(A.analysisErrorText({ code: 'not_authorized' }).includes('Telegram'),
    'ошибка: not_authorized называет мессенджер, а не безымянный «сервис»');
ok(A.analysisErrorText({ code: 'not_connected' }).includes('Telegram'),
    'ошибка: not_connected называет мессенджер');
ok(A.analysisErrorText({ code: 'invalid_input' }).length > 0,
    'ошибка: invalid_input имеет человеческую подпись');
eq(A.analysisErrorText({ code: 'unknown_dialog', message: 'чат не найден' }), 'чат не найден',
    'ошибка: unknown_dialog общей подписи не имеет — показываем сообщение сервера');
ok(notices.length > 0, 'ошибка: пользователь дополнительно уведомлён тостом');

// ---------------------------------------------------------------------------
// 6. Смена чата — заметка перечитывается
// ---------------------------------------------------------------------------

setNet(async (url) => {
    if (url === '/api/telegram/dialogs/777/prompt') {
        return jsonRes(200, promptDto('заметка второго чата', 1700000200, 4000));
    }
    if (/\/prompt$/.test(url)) return jsonRes(200, promptDto('старая заметка', 1700000000, 4000));
    return jsonRes(200, {});
});

// Уходим на вкладку «Анализ»: иначе открытие второго чата сразу же дёрнет
// заметку (вкладка контекста осталась бы активной), и «сброшенный флаг» было
// бы не увидеть — он поднимается тем же кадром.
dispatch($analysisTabRun, 'click', {});
await settle();

const before = promptCalls().length;
A.openAnalysis('777');

eq(A._debugAnalysisState().dialogId, '777', 'смена чата: id обновлён');
eq(A._debugAnalysisState().promptLoaded, false, 'смена чата: флаг загрузки сброшен');
eq($analysisPrompt.value, '', 'смена чата: поле очищено от чужого текста');
eq($analysisTarget.textContent, '777', 'смена чата: незнакомый чат назван своим id');
eq(promptCalls().length - before, 0, 'смена чата: сама по себе запроса не шлёт');

dispatch($analysisTabCtx, 'click', {});
await settle();
eq(promptCalls().length - before, 1, 'смена чата: заметка перечитана ровно одним запросом');
eq($analysisPrompt.value, 'заметка второго чата', 'смена чата: показана заметка НОВОГО чата');

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
    // У Telegram блокирующего оверлея нет вовсе, поэтому Esc ВСЕГДА
    // принадлежит попапу — ветки «оверлей перехватил» здесь не существует
    // (см. шапку: captcha неприменима).
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
eq(A._debugAnalysisState().dialogId, null, 'teardown: состояние чата очищено');

// Повторный init поднимает всё обратно и НЕ вешает второй слушатель.
A.initAnalysis({});
A.initAnalysis({});
eq(($document.listeners.get('keydown') || []).length, listenersBefore,
    'повторный init: слушатель ровно один');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/telegram_analysis_prompt)`);
process.exit(failures ? 1 : 0);
