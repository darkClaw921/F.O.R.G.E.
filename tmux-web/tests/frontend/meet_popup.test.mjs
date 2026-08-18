// ⚠ СТРАЖ КАРТОЧКИ ВСТРЕЧИ — КАРКАС (фронтовая Ф4 forge-meet, задача ФФ4.2).
//
// Грузит РЕАЛЬНЫЙ модуль static/js/messenger/meet-popup.js на фейковом
// документе (meet_popup_dom.mjs). Содержимое вкладок и плеер проверяют соседи
// (meet_popup_tabs, meet_popup_player); здесь — каркас и всё, что ломается
// ТИХО:
//
//   1. РАЗМЕТКА СТРОИТСЯ УЗЛАМИ. В карточку попадают название встречи из чужого
//      чата, имена участников из DOM чужой страницы и текст расшифровки. Сборка
//      строкой означала бы разметку из этих источников — поэтому `innerHTML`
//      запрещён и проверяется ПО ИСХОДНИКУ.
//   2. ПОПАП — ПОСЛЕДНИЙ ПОТОМОК ПАНЕЛИ. Не `body`: за пределами `#telegram`
//      токены палитры не резолвятся, и карточка окажется бесцветной. И именно
//      последний: при равном `z-index` побеждает порядок в DOM, а карточка
//      обязана лечь поверх оставленного открытым попапа анализа.
//   3. КЛИК ВСЕГДА ОТКРЫВАЕТ КАРТОЧКУ, а не стартует запись. Бот входит в чужую
//      встречу, и его видят все участники: подтверждение здесь функция, а не
//      трение. Регрессия «кнопка сразу пишет» стоила бы неловкости на живом
//      созвоне и никаким тестом вида не ловится.
//   4. Esc ЗАКРЫВАЕТ РОВНО ОДНО ОКНО. Без `stopPropagation` то же нажатие
//      закрыло бы заодно попап анализа и снятие цитаты.
//   5. TEARDOWN СНИМАЕТ СЛУШАТЕЛЬ С ДОКУМЕНТА. Переживший выгрузку в bfcache,
//      он стреляет по мёртвому DOM — этот дефект уже ловили в панелях VK и
//      Битрикса.
//   6. ПОПАП НЕ ЗАВОДИТ СВОИХ ИНТЕРВАЛОВ. Живой таймер записи — один на
//      документ, он уже тикает в сторе; второй расходился бы с первым на глазах.
//
// Запуск: node tmux-web/tests/frontend/meet_popup.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

import { dispatch, makeDoc, makeTimers } from './meet_popup_dom.mjs';

const MODULE_URL = new URL('../../static/js/messenger/meet-popup.js', import.meta.url);
const POPUP = await import(MODULE_URL.href);
const STORE = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);
const ROW = await import(new URL('../../static/js/messenger/meet-row.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function ok(cond, label, detail) {
    checks += 1;
    if (cond) return;
    failures += 1;
    console.error(`[FAIL] ${label}${detail ? `\n       ${detail}` : ''}`);
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) return;
    failures += 1;
    console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
}

const MEET_URL = 'https://telemost.yandex.ru/j/60497752704486';
const KEY = 'telegram|777000|7';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Запись состояния в кэше стора — так же, как её кладёт батч ленты. */
function putBadge(status, extra) {
    STORE.applyBadges('telegram', '777000', [Object.assign({
        chat_id: '777000',
        message_id: 7,
        meeting_id: 'm-1',
        status,
        summary_status: 'none',
        platform: 'telemost',
        started_at: null,
        ended_at: null,
        duration_ms: null,
    }, extra || {})], [7]);
}

function detail(state) {
    return {
        key: KEY,
        panel: 'telegram',
        chatId: '777000',
        messageId: 7,
        meet: { url: MEET_URL, title: 'Созвон', platform: 'telemost', localKey: 'telemost:60497752704486' },
        state: state === undefined ? STORE.peekMeetState(KEY) : state,
    };
}

/** Журналируемый клиент api: карточка не ходит в сеть сама. */
function fakeApi(over) {
    const calls = [];
    const log = (name) => (...args) => {
        calls.push({ name, args });
        return Promise.resolve({});
    };
    return Object.assign({
        calls,
        getMeeting: log('getMeeting'),
        getSummary: log('getSummary'),
        getTranscript: (id, opts) => {
            calls.push({ name: 'getTranscript', args: [id, opts] });
            return Promise.resolve({ segments: [], total: 0 });
        },
        startMeeting: (payload) => {
            calls.push({ name: 'startMeeting', args: [payload] });
            return Promise.resolve({ meeting_id: 'm-9', attached: false });
        },
        stopMeeting: log('stopMeeting'),
        cancelMeeting: log('cancelMeeting'),
        confirmMeeting: log('confirmMeeting'),
        showMeetingWindow: log('showMeetingWindow'),
        retryMeeting: log('retryMeeting'),
    }, over || {});
}

// ===========================================================================
// 1. Каркас разметки
// ===========================================================================

{
    STORE.resetMeetStore();
    const env = makeDoc(['telegram']);
    // В панели уже что-то живёт (попап анализа в index.html — ровно такой же
    // сосед): карточка обязана встать ПОСЛЕ него.
    const neighbour = env.doc.createElement('div');
    neighbour.className = 'tg-analysis';
    env.host.appendChild(neighbour);

    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf,
    });

    ok(popup.root.classList.contains(POPUP.MEET_POPUP_CLASS),
        'корень несёт класс meet-popup — по нему его находит гейт keynav');
    ok(!popup.root.classList.contains('modal-overlay'),
        'и НЕ несёт .modal-overlay: он тянет глобальные токены и position: fixed');
    eq(popup.root.hidden, true, 'карточка создаётся закрытой');
    eq(popup.root.getAttribute('role'), 'dialog', 'узел объявлен диалогом');
    eq(popup.root.id, 'tg-meet-popup', 'id панельный — ради e2e и ручной отладки');
    // ⚠ Сравнение узлов ТОЛЬКО через `ok(a === b)`: `JSON.stringify` падает на
    // циклической ссылке childNodes ↔ parentNode.
    ok(env.host.children[env.host.children.length - 1] === popup.root,
        'карточка — ПОСЛЕДНИЙ потомок панели: при равном z-index побеждает порядок в DOM');
    ok(popup.root.parentNode === env.host, 'и лежит внутри панели, а не в body');

    // Вкладки и панели вкладок собраны сразу: переключение не пересобирает DOM,
    // иначе терялись бы прокрутка расшифровки и набранный поиск.
    eq(popup.root.querySelectorAll('.meet-tab').length, 3, 'три вкладки');
    eq(popup.root.querySelectorAll('.meet-pane').length, 3, 'три панели вкладок');
    eq(POPUP.MEET_TABS, ['summary', 'transcript', 'people'], 'порядок вкладок закреплён');
    const acts = popup.root.querySelectorAll('.meet-act')
        .filter((n) => n.dataset.meetAct)
        .map((n) => n.dataset.meetAct);
    eq(acts.sort(), ['cancel', 'confirm', 'record', 'retry', 'shot', 'stop', 'window'],
        'полоса действий собрана целиком — видимостью управляет статус');

    popup.teardown();
}

// ===========================================================================
// 2. Клик по кнопке ленты открывает карточку, а не стартует запись
// ===========================================================================

{
    STORE.resetMeetStore();
    ROW.resetMeetRow();
    POPUP.teardownMeetPopups();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }), batchDelayMs: 5 });

    const env = makeDoc(['telegram']);
    const api = fakeApi();
    POPUP.installMeetPopups({ api, doc: env.doc });
    eq(POPUP._debugMeetPopups().installed, true, 'реестр включён');
    eq(POPUP._debugMeetPopups().panels, [], 'но ни одного узла до первого клика не создано');

    const row = env.doc.createElement('div');
    row.className = 'tg-line';
    env.host.appendChild(row);
    const btn = ROW.attachMeetButton(row, {
        panel: 'telegram',
        msg: { id: 7, dialog_id: '777000', text: `созвон ${MEET_URL}` },
        chatId: '777000',
        doc: env.doc,
    });
    ok(!!btn, 'кнопка встречи построена');
    eq(btn.dataset.meetStatus, 'none', 'состояние «можно записать»');

    btn.click();
    await sleep(0);

    eq(POPUP._debugMeetPopups().panels, ['telegram'], 'экземпляр создан лениво — по первому клику');
    const inst = POPUP.openMeetPopup(detail(null));
    eq(inst.isOpen(), true, 'карточка открыта');
    eq(inst.root.hidden, false, 'и видима');
    eq(api.calls.filter((c) => c.name === 'startMeeting').length, 0,
        'КЛИК НЕ СТАРТУЕТ ЗАПИСЬ: бот входит в чужую встречу, подтверждение обязательно');
    ok(!inst.refs.acts.record.hidden, 'зато кнопка ЗАПИСАТЬ показана');
    ok(inst.refs.acts.stop.hidden, 'а ОСТАНОВИТЬ при отсутствующей записи спрятана');

    // Теперь явное подтверждение — вот оно и стартует запись.
    inst.refs.acts.record.click();
    await sleep(0);
    const started = api.calls.filter((c) => c.name === 'startMeeting');
    eq(started.length, 1, 'нажатие ЗАПИСАТЬ ушло на сервер ровно один раз');
    eq(started[0].args[0].url, MEET_URL, 'на сервер уехал канонический адрес встречи');
    eq(started[0].args[0].plugin, 'telegram', 'и плагин из ключа строки');
    eq(String(started[0].args[0].chat_id), '777000', 'и чат');
    eq(started[0].args[0].message_id, 7, 'и сообщение');
    eq(inst.debugState().meetingId, 'm-9', 'идентификатор встречи запомнен');

    // ⚠ Без linkMeeting кадр `meeting_update` было бы не к чему применить:
    // MeetingRow источника не несёт. Проверяется тем, что кадр меняет состояние
    // ИМЕННО ЭТОЙ строки ленты.
    STORE.applyMeetingUpdate({
        id: 'm-9', status: 'recording', summary_status: 'none', platform: 'telemost',
        started_at: 1700000000, ended_at: null, duration_ms: null,
    });
    const linked = STORE.peekMeetState(KEY);
    eq(linked && linked.status, 'recording',
        'связь строка↔встреча установлена: кадр обновил именно эту строку');

    POPUP.teardownMeetPopups();
}

// ===========================================================================
// 3. Esc: закрывает ровно одно окно
// ===========================================================================

{
    STORE.resetMeetStore();
    const env = makeDoc(['telegram']);
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf,
    });

    // Пока карточка закрыта, Esc принадлежит остальному интерфейсу.
    const idle = env.keydown('Escape');
    eq(idle.defaultPrevented, false, 'у закрытой карточки Esc не перехватывается');
    eq(idle.propagationStopped, false, 'и не гасится — иначе Esc сломался бы у всей панели');

    popup.open(detail(null));
    const hit = env.keydown('Escape');
    eq(popup.isOpen(), false, 'Esc закрыл карточку');
    eq(popup.root.hidden, true, 'узел спрятан атрибутом');
    eq(hit.defaultPrevented, true, 'нажатие погашено');
    eq(hit.propagationStopped, true,
        'и НЕ всплывает: одно нажатие Esc обязано закрыть ровно одно окно');

    // Чужая клавиша карточку не трогает.
    popup.open(detail(null));
    const other = env.keydown('Enter');
    eq(popup.isOpen(), true, 'Enter карточку не закрывает');
    eq(other.propagationStopped, false, 'и не гасится');
    popup.close();
    popup.teardown();
}

// ===========================================================================
// 4. Подложка закрывает, коробка — нет
// ===========================================================================

{
    STORE.resetMeetStore();
    const env = makeDoc(['telegram']);
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf,
    });
    popup.open(detail(null));

    const card = popup.root.querySelector('.meet-card');
    dispatch(popup.root, 'click', { target: card });
    eq(popup.isOpen(), true, 'клик по карточке НЕ закрывает — иначе не нажать ни одной кнопки');

    dispatch(popup.root, 'click', { target: popup.root });
    eq(popup.isOpen(), false, 'клик по подложке закрывает');
    popup.teardown();
}

// ===========================================================================
// 5. Подписка на стор живёт ровно пока карточка открыта
// ===========================================================================

{
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    const env = makeDoc(['telegram']);
    const base = STORE._debugMeetStore().subscribers;
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf,
    });
    eq(STORE._debugMeetStore().subscribers, base,
        'закрытая карточка на стор не подписана — событий про чужие встречи ей не нужно');

    popup.open(detail(null));
    eq(STORE._debugMeetStore().subscribers, base + 1, 'открытая подписана ровно одним подписчиком');
    popup.open(detail(null));
    eq(STORE._debugMeetStore().subscribers, base + 1,
        'повторное открытие не удваивает подписку (хоткей нажимают дважды)');

    popup.close();
    eq(STORE._debugMeetStore().subscribers, base,
        'закрытие снимает подписку: иначе за сессию их накопились бы десятки');
    popup.teardown();
}

// ===========================================================================
// 6. teardown снимает всё, что переживает выгрузку страницы
// ===========================================================================

{
    STORE.resetMeetStore();
    const env = makeDoc(['telegram']);
    eq(env.docListeners('keydown'), 0, 'до создания карточки слушателей на документе нет');
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf,
    });
    eq(env.docListeners('keydown'), 1, 'карточка повесила ровно один слушатель Esc');

    popup.open(detail(null));
    popup.teardown();
    eq(env.docListeners('keydown'), 0,
        'teardown СНЯЛ слушатель: переживший bfcache, он стрелял бы по мёртвому DOM');
    eq(popup.root.parentNode, null, 'и узел убран из панели');
    eq(popup.debugState().escBound, false, 'ссылка на слушатель обнулена');

    // Мёртвый попап на нажатие не реагирует.
    popup.open(detail(null));
    const after = env.keydown('Escape');
    eq(after.propagationStopped, false, 'после teardown нажатие не перехватывается');
}

// ===========================================================================
// 7. Своих интервалов карточка не заводит
// ===========================================================================

{
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    const env = makeDoc(['telegram']);
    const timers = makeTimers();
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf, timers: timers.api,
    });
    putBadge('recording', { started_at: 1699999940 });
    popup.open(detail());
    popup.close();
    popup.teardown();

    eq(timers.stats.setInterval, 0,
        'ни одного setInterval: живой таймер записи — ОДИН на документ и живёт в сторе');
    eq(timers.pending.size, 0, 'и ни одного невыключенного таймера после teardown');
}

// ===========================================================================
// 8. ⚠ СТРАЖ ИСХОДНИКА
// ===========================================================================

{
    const raw = readFileSync(MODULE_URL, 'utf8');
    // Комментарии вырезаются ПЕРЕД проверкой: шапка модуля объясняет, почему
    // innerHTML запрещён, и упоминает его по имени. Страж, спотыкающийся о
    // собственное объяснение, пришлось бы отключить — а отключённый страж не
    // ловит ничего.
    //
    // ⚠ ПОРЯДОК ВЫРЕЗАНИЯ ВАЖЕН. Сначала строчные комментарии, потом блочные:
    // в шапке есть строка про `/api/*`, и блочный стрипер, запущенный первым,
    // принимает её за открытие комментария и съедает всё до ближайшего `*/` —
    // вместе с блоком import. Пойман живьём: страж «саммари рендерится общим
    // renderMarkdownText» краснел на исправном коде.
    const src = raw.replace(/(^|[^:])\/\/.*$/gm, '$1').replace(/\/\*[\s\S]*?\*\//g, '');
    ok(raw.includes('innerHTML'), 'шапка про innerHTML на месте (иначе страж проверяет не то)');

    for (const banned of ['innerHTML', 'outerHTML', 'insertAdjacentHTML']) {
        ok(!src.includes(banned), `в коде нет ${banned} — разметка строится узлами`);
    }
    // Документ приходит ПАРАМЕТРОМ. Глобальный `document` допускается ровно как
    // запасное значение по умолчанию, но ни одного вызова через него быть не
    // должно — иначе модуль перестанет тестироваться на фейковом документе.
    for (const call of ['createElement', 'createTextNode', 'addEventListener', 'getElementById']) {
        ok(!new RegExp(`(^|[^.\\w])document\\.${call}\\b`).test(src),
            `нет прямого document.${call} — документ берётся из cfg.doc`);
    }
    // Ни своего markdown, ни своего следования: обе реализации в проекте уже
    // есть, и вторая копия разошлась бы с первой на первой же правке.
    ok(/import\s*\{[^}]*renderMarkdownText[^}]*\}\s*from\s*'\.\/markdown\.js'/.test(src),
        'саммари рендерится общим renderMarkdownText');
    ok(/import\s*\{[^}]*createFollow[^}]*\}\s*from\s*'\.\/follow-core\.js'/.test(src),
        'автопрокрутка — общий createFollow');
    ok(!/function\s+parseMarkdown\b/.test(src), 'своего парсера markdown в модуле нет');
    ok(!/setInterval\s*\(/.test(src), 'своих интервалов модуль не заводит');
    // Отзыв objectURL обязателен — без него каждая прослушанная встреча
    // остаётся в памяти вкладки до перезагрузки.
    ok(/revokeObjectURL/.test(src), 'отзыв objectURL в модуле есть');
}

// ===========================================================================
// 9. Полоса действий: таблица статусов
// ===========================================================================

{
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    const env = makeDoc(['telegram']);
    const api = fakeApi();
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api,
        raf: env.raf, caf: env.caf,
    });

    const visible = () => Object.keys(popup.refs.acts)
        .filter((k) => !popup.refs.acts[k].hidden).sort();

    putBadge('recording', { started_at: 1700000000 });
    popup.open(detail());
    eq(visible(), ['cancel', 'shot', 'stop', 'window'], 'во время записи: стоп, окно, скриншот, прерывание');

    putBadge('waiting_admit');
    popup.open(detail());
    eq(visible(), ['cancel', 'confirm', 'window'], 'в лобби появляется «Я ВНУТРИ»');

    putBadge('ready', { summary_status: 'ready' });
    popup.open(detail());
    eq(visible(), [], 'у готовой встречи с готовым саммари повторять нечего');

    putBadge('ready', { summary_status: 'failed' });
    popup.open(detail());
    eq(visible(), ['retry'],
        'а вот провал саммари даёт кнопку повтора: расшифровка при этом цела');

    putBadge('failed');
    popup.open(detail());
    eq(visible(), ['retry'], 'у упавшей встречи — повтор');

    // Кнопки действительно ходят в сеть с нужным идентификатором.
    putBadge('recording', { started_at: 1700000000 });
    popup.open(detail());
    popup.refs.acts.stop.click();
    await sleep(0);
    const stops = api.calls.filter((c) => c.name === 'stopMeeting');
    eq(stops.length, 1, 'ОСТАНОВИТЬ ушла на сервер');
    eq(stops[0].args[0], 'm-1', 'с идентификатором встречи из состояния строки');

    popup.teardown();
}

// ===========================================================================
// 10. Смена строки сбрасывает содержимое
// ===========================================================================

{
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    const env = makeDoc(['telegram']);
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: fakeApi(),
        raf: env.raf, caf: env.caf,
    });
    putBadge('ready');
    popup.open(detail());
    popup.setTab('transcript');
    popup.refs.search.value = 'привет';
    dispatch(popup.refs.search, 'input');
    eq(popup.debugState().query, 'привет', 'запрос поиска запомнен');

    popup.open({
        key: 'telegram|777000|8',
        panel: 'telegram',
        chatId: '777000',
        messageId: 8,
        meet: { url: MEET_URL, title: 'Другой созвон', platform: 'telemost' },
        state: null,
    });
    eq(popup.debugState().query, '',
        'другая строка — карточка чистая: остаточная расшифровка чужой встречи хуже пустоты');
    eq(popup.debugState().rows, 0, 'и без строк расшифровки прошлой встречи');
    eq(popup.debugState().meetingId, null, 'и без прошлого идентификатора');
    popup.teardown();
}

// ===========================================================================

STORE.resetMeetStore();
if (failures === 0) console.log(`[OK] meet_popup: ${checks} проверок`);
else console.error(`[FAIL] meet_popup: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
