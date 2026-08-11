// Регресс-тест вкладки «Контекст чата» попапа анализа (js/bitrix/analysis.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/bitrix_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы `$b24Analysis*` в
// null, и тест проверял бы пустоту).
//
// Близнец vk_analysis_prompt.test.mjs и telegram_analysis_prompt.test.mjs.
// Поведение живёт в общем ядре js/messenger/analysis-core.js, но ПРОВОДКА порта
// своя (пути `/api/bitrix/…`, подписи ошибок плагина, имя чата из `b24State`),
// и рвётся она молча.
//
// ЧТО ИМЕННО ЗДЕСЬ ЗАЩИЩЕНО. Заметка о чате — единственная часть попапа,
// которая переживает перезагрузку страницы, поэтому её легко сломать молча:
//
//   1. GET уходит ЛЕНИВО (на первом входе на вкладку) и РОВНО ОДИН РАЗ на чат:
//      попап открывают и закрывают десятками нажатий, и перечитывать
//      неизменившуюся заметку незачем — тем более что бюджет портала 2 rps
//      общий на всю панель;
//   2. смена чата ЗАСТАВЛЯЕТ перечитать: иначе второй чат показал бы заметку
//      первого, и пользователь отредактировал бы чужой текст;
//   3. счётчик символов считает CODE POINTS и берёт потолок ИЗ ОТВЕТА СЕРВЕРА
//      (`max_chars`), а не из константы фронта: счётчик, разошедшийся с
//      серверным лимитом, обещал бы место, которого нет;
//   4. SAVE шлёт текст, обрезанный ТЕМ ЖЕ правилом, что и на сервере
//      (`prompts::sanitize`: trim → срез по code points → trim_end), — иначе
//      дописанная фраза пропала бы молча;
//   5. CLEAR шлёт пустую строку (единственный способ стереть заметку: отдельного
//      DELETE у роута нет);
//   6. статус сохранения не молчит: пользователь обязан видеть, чем кончился
//      PUT, — «сохранено», «заметка стёрта» или строка ошибки;
//   7. ошибка показывается по `err.code`, а не по тексту, и коды здесь
//      ПОРТАЛЬНЫЕ: `flood_wait` (как у Telegram, а не VK-шный `rate_limited`)
//      и свой `scope_missing` (409), которого нет ни у одного соседа.
//
// ЧЕГО ЗДЕСЬ НЕТ ПО СРАВНЕНИЮ С VK-версией (три группы, все неприменимы к
// Битрикс24, а не «забыты»):
//
//   * НЕПРИМЕНИМО — CAPTCHA. У VK Esc может принадлежать блокирующему оверлею
//     captcha, стоящему ВЫШЕ попапа. У портала такого оверлея нет вовсе
//     (`isBlockingOverlayOpen: () => false` в js/bitrix/analysis.js): вебхук
//     либо подключён, либо панель закрыта экраном подключения ЦЕЛИКОМ, и попап
//     из-под него не открыть. Поэтому Esc ВСЕГДА закрывает попап — это и
//     проверяется ниже, без ветки «оверлей открыт».
//   * НЕПРИМЕНИМО — КЛИЕНТСКИЙ ЛИМИТЕР 3 rps. VK-версия поднимает
//     js/vk/ratelimit.js и приземляет `minIntervalMs`, потому что запросы
//     панели VK идут через очередь на токен. У Битрикс24 клиентского лимитера
//     нет: интенсивность душит сам портал (состояние `throttled` панели), а
//     вызовы идут прямо через js/bitrix/api.js.
//   * НЕПРИМЕНИМО — ПРОГРЕСС ВЫГРУЗКИ. Пункт задачи br про кадр прогресса не
//     применяется: в задаче 3.4 выбрано УМЕНЬШЕНИЕ ГЛУБИНЫ (50/100/200), а не
//     прогресс-кадр, и к заметке чата он не относился бы в любом случае.
//
// Запуск: node tmux-web/tests/frontend/bitrix_analysis_prompt.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $analysis, $analysisClose, $analysisError, $analysisPaneCtx,
    $analysisPaneRun, $analysisPrompt, $analysisPromptClear,
    $analysisPromptCount, $analysisPromptSave, $analysisPromptStatus,
    $analysisTabCtx, $analysisTabRun, $analysisTarget, $document, dispatch,
    flush, jsonRes, netCalls, setNet,
} from './bitrix_dom_stub.mjs';

/** Даёт доехать микрозадачам транспорта и кадру анимации стаба. */
async function settle() {
    for (let i = 0; i < 4; i += 1) {
        await flush();
        await new Promise((resolve) => setTimeout(resolve, 3));
    }
}

const A = await import(new URL('../../static/js/bitrix/analysis.js', import.meta.url).href);
const S = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);

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

/** Ошибка в конверте плагина (`{"error":{"code":…}}`). */
function errRes(status, code, message) {
    return jsonRes(status, { error: { code, message: message || code } });
}

const notices = [];
A.initAnalysis({ onNotice: (t) => notices.push(t) });

// Чат известен панели: шапка попапа обязана показывать имя, а не `chat27`.
// Имя берётся общими findDialog/displayName панели — вторым способом назвать
// чат он разъехался бы с колонкой при первой же её правке.
S.b24State.dialogs = [{ id: 'chat27', title: 'Подрядчик', kind: 'chat' }];
S.b24State.activeDialogId = 'chat27';

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
    'открытие: шапка называет чат ИМЕНЕМ из b24State, а не его id');
eq(promptCalls().length, 0, 'ленивость: на вкладке «Анализ» заметка не запрашивается');
eq(A._debugAnalysisState().promptLoaded, false, 'ленивость: флаг загрузки ещё не поднят');
eq($analysisPaneCtx.hidden, true, 'открытие: вкладка контекста спрятана');
eq($analysisPaneRun.hidden, false, 'открытие: показана вкладка анализа');

// ---------------------------------------------------------------------------
// 2. Первый вход на «Контекст» — ровно один GET
// ---------------------------------------------------------------------------

dispatch($analysisTabCtx, 'click', {});
await settle();

eq(promptCalls().length, 1, 'первый вход: ушёл один GET');
eq(promptCalls()[0].method, 'GET', 'первый вход: именно GET');
eq(promptCalls()[0].url, '/api/bitrix/dialogs/chat27/prompt',
    'первый вход: путь совпадает с серверным контрактом плагина Битрикс24');
eq($analysisPrompt.value, 'старая заметка', 'первый вход: текст подставлен в поле');
eq($analysisPromptCount.textContent, '14 / 4000',
    'первый вход: счётчик символов от max_chars СЕРВЕРА, а не от константы фронта');
eq(A._debugAnalysisState().tab, 'ctx', 'первый вход: вкладка переключена');
eq($analysisPaneCtx.hidden, false, 'первый вход: панель контекста показана');
eq($analysisPaneRun.hidden, true, 'первый вход: панель анализа спрятана');
eq($analysisTabCtx.getAttribute('aria-selected'), 'true',
    'первый вход: активная вкладка помечена aria-selected');
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
// И приезжает он ИЗ ОТВЕТА — фронт своего потолка не выдумывает.
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
eq(A.clampPrompt('раз🙂два', 4), 'раз🙂',
    'clampPrompt: срез идёт по code points — emoji не разрезается пополам');

$analysisPrompt.value = 'новая заметка';
dispatch($analysisPrompt, 'input', {});
eq($analysisPromptCount.textContent, '13 / 4000', 'ввод: счётчик пересчитан');
eq(A._debugAnalysisState().promptDirty, true, 'ввод: заметка помечена изменённой');

dispatch($analysisPromptSave, 'click', {});
await settle();

const puts = promptCalls().filter((c) => c.method === 'PUT');
eq(puts.length, 1, 'SAVE: ушёл один PUT');
eq(puts[0].url, '/api/bitrix/dialogs/chat27/prompt', 'SAVE: PUT по тому же пути, что и GET');
eq(puts[0].body, { prompt: 'новая заметка' }, 'SAVE: тело — {prompt}');
eq(A._debugAnalysisState().promptDirty, false, 'SAVE: флаг изменённости снят');
eq(A._debugAnalysisState().maxChars, 10, 'SAVE: потолок обновлён ответом сервера');
eq(String($analysisPromptStatus.textContent), 'сохранено',
    'SAVE: статус говорит об успехе — иначе PUT выглядел бы не сработавшим');
// Потолок в знаменателе — уже НОВЫЙ, приехавший этим же ответом (10), хотя
// текст уехал целиком: обрезка применяется к следующему сохранению, а счётчик
// обязан показать правду про лимит СРАЗУ. Знаменатель, застрявший на прошлом
// значении, обещал бы место, которого на сервере уже нет.
eq($analysisPromptCount.textContent, '13 / 10',
    'SAVE: знаменатель счётчика переехал на новый max_chars из ответа сервера');

// Ctrl+Enter в поле сохраняет так же, как кнопка.
$analysisPrompt.value = 'через клавиатуру';
dispatch($analysisPrompt, 'keydown', { key: 'Enter', ctrlKey: true });
await settle();
const puts2 = promptCalls().filter((c) => c.method === 'PUT');
eq(puts2.length, 2, 'Ctrl+Enter: ушёл второй PUT');
eq(puts2[1].body, { prompt: 'через клав' }, 'Ctrl+Enter: текст обрезан по max_chars сервера');
eq($analysisPrompt.value, 'через клав',
    'Ctrl+Enter: обрезанный текст подставлен обратно в поле — видно, что именно уехало');

// Cmd+Enter (macOS) — тот же жест: у портальной панели своей клавиатуры нет.
$analysisPrompt.value = 'через cmd';
dispatch($analysisPrompt, 'keydown', { key: 'Enter', metaKey: true });
await settle();
eq(promptCalls().filter((c) => c.method === 'PUT').length, 3, 'Cmd+Enter: сохраняет так же');

// Просто Enter (без модификатора) — это перенос строки, а не сохранение.
$analysisPrompt.value = 'ещё строка';
dispatch($analysisPrompt, 'keydown', { key: 'Enter' });
await settle();
eq(promptCalls().filter((c) => c.method === 'PUT').length, 3,
    'Enter без модификатора не сохраняет');

// ---------------------------------------------------------------------------
// 4. CLEAR: пустая строка и пустое поле
// ---------------------------------------------------------------------------

dispatch($analysisPromptClear, 'click', {});
await settle();

const puts3 = promptCalls().filter((c) => c.method === 'PUT');
eq(puts3.length, 4, 'CLEAR: ушёл PUT');
eq(puts3[3].method, 'PUT', 'CLEAR: именно PUT — отдельного DELETE у роута заметки нет');
eq(puts3[3].body, { prompt: '' }, 'CLEAR: тело — пустая строка');
eq($analysisPrompt.value, '', 'CLEAR: поле очищено');
eq(String($analysisPromptStatus.textContent), 'заметка стёрта',
    'CLEAR: статус отличает стирание от обычного сохранения');
eq($analysisPromptCount.textContent, '0 / 10', 'CLEAR: счётчик обнулён');

// ---------------------------------------------------------------------------
// 5. Ошибка показывается по коду, а не по тексту
// ---------------------------------------------------------------------------

setNet(async (url, init) => {
    if (/\/prompt$/.test(url) && init.method === 'PUT') {
        return errRes(500, 'storage_failed', 'io error: /home/u/.config/devforge/bitrix-prompts.json');
    }
    return jsonRes(200, promptDto('', null, 10));
});

$analysisPrompt.value = 'не сохранится';
dispatch($analysisPromptSave, 'click', {});
await settle();

eq($analysisError.hidden, false, 'ошибка: строка ошибки показана');
ok(String($analysisError.textContent).includes('права'),
    'ошибка: текст выбран по коду storage_failed, а не скопирован с сервера');
ok(!String($analysisError.textContent).includes('bitrix-prompts.json'),
    'ошибка: путь с сервера пользователю не показывается');
eq(String($analysisPromptStatus.textContent), '',
    'ошибка: статус НЕ утверждает «сохранено» — иначе пользователь ушёл бы с потерянным текстом');
eq(A.analysisErrorText({ code: 'claude_unavailable' }).includes('Claude CLI'), true,
    'ошибка: claude_unavailable подсказывает про установку CLI');
eq(A.analysisErrorText({ code: 'invalid_state' }).includes('уже идёт'), true,
    'ошибка: invalid_state говорит про идущий анализ');
// Коды плагина Битрикс24: подписи, где стоит имя мессенджера, — свои; код
// ограничения скорости — `flood_wait` (как у Telegram), а `scope_missing`
// вообще уникален для портала.
ok(A.analysisErrorText({ code: 'flood_wait' }).includes('Битрикс24'),
    'ошибка: flood_wait — код портала, подпись называет мессенджер');
ok(A.analysisErrorText({ code: 'flood_wait' }) !== A.analysisErrorText({ code: 'rate_limited' }),
    'ошибка: VK-шный rate_limited и портальный flood_wait — разные подписи');
ok(A.analysisErrorText({ code: 'scope_missing' }).includes('вебхук'),
    'ошибка: scope_missing зовёт чинить ВЕБХУК, а не «войти заново»');
ok(A.analysisErrorText({ code: 'not_authorized' }).includes('Битрикс24'),
    'ошибка: not_authorized называет мессенджер, а не безымянный «сервис»');
ok(A.analysisErrorText({ code: 'not_connected' }).includes('Битрикс24'),
    'ошибка: not_connected называет мессенджер');
ok(A.analysisErrorText({ code: 'invalid_input' }).length > 0,
    'ошибка: invalid_input имеет человеческую подпись');
eq(A.analysisErrorText({ code: 'unknown_dialog', message: 'чат не найден' }), 'чат не найден',
    'ошибка: unknown_dialog общей подписи не имеет — показываем сообщение сервера');
ok(notices.length > 0, 'ошибка: пользователь дополнительно уведомлён тостом');

// Упавший GET заметки тоже не оставляет чат без неё навсегда: флаг загрузки
// снимается, и следующий вход на вкладку пробует снова.
setNet(async (url) => {
    if (/\/prompt$/.test(url)) return errRes(429, 'flood_wait', 'Portal is busy');
    return jsonRes(200, {});
});
A.openAnalysis('chat55');
await settle();
const beforeRetry = promptCalls().length;
dispatch($analysisTabCtx, 'click', {});
await settle();
eq(promptCalls().length - beforeRetry, 1, 'упавший GET: запрос ушёл');
eq(A._debugAnalysisState().promptLoaded, false,
    'упавший GET: флаг загрузки снят — иначе чат остался бы без заметки навсегда');
ok(String($analysisError.textContent).includes('Битрикс24'),
    'упавший GET: показана портальная подпись flood_wait, а не сырое сообщение сервера');

setNet(async (url) => {
    if (/\/prompt$/.test(url)) return jsonRes(200, promptDto('со второй попытки', 1700000300, 4000));
    return jsonRes(200, {});
});
dispatch($analysisTabRun, 'click', {});
dispatch($analysisTabCtx, 'click', {});
await settle();
eq($analysisPrompt.value, 'со второй попытки',
    'упавший GET: повторный вход на вкладку дочитал заметку');

// ---------------------------------------------------------------------------
// 6. Смена чата — заметка перечитывается
// ---------------------------------------------------------------------------

setNet(async (url) => {
    if (url === '/api/bitrix/dialogs/sg12/prompt') {
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
A.openAnalysis('sg12');

eq(A._debugAnalysisState().dialogId, 'sg12', 'смена чата: id обновлён');
eq(A._debugAnalysisState().promptLoaded, false, 'смена чата: флаг загрузки сброшен');
eq($analysisPrompt.value, '', 'смена чата: поле очищено от чужого текста');
eq($analysisTarget.textContent, 'sg12', 'смена чата: незнакомый чат назван своим id');
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

A.openAnalysis('sg12');
dispatch($analysis, 'click', { target: $analysis });
eq($analysis.hidden, true, 'клик по подложке закрывает попап');

A.openAnalysis('sg12');
{
    // У портала блокирующего оверлея нет вовсе, поэтому Esc ВСЕГДА принадлежит
    // попапу — ветки «оверлей перехватил» здесь не существует (см. шапку:
    // captcha неприменима).
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
eq(A._debugAnalysisState().depth, A.DEFAULT_DEPTH,
    'teardown: глубина вернулась к портальному значению по умолчанию (100)');

// Повторный init поднимает всё обратно и НЕ вешает второй слушатель.
A.initAnalysis({});
A.initAnalysis({});
eq(($document.listeners.get('keydown') || []).length, listenersBefore,
    'повторный init: слушатель ровно один');

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_analysis_prompt)`);
process.exit(failures ? 1 : 0);
