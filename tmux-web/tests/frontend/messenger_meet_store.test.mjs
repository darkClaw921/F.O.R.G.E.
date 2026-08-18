// ⚠ СТРАЖ СТОРА ВСТРЕЧ (фронтовая Ф2 forge-meet, задача ФФ2.2).
//
// Грузит РЕАЛЬНЫЙ модуль static/js/messenger/meet-store.js в голом Node:
// у него нет ни DOM, ни импорта сети — транспорт приходит инъекцией, таймеры
// подменяются через configureMeetStore. Ни сервера, ни браузера не нужно.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Стор держит три обещания, каждое из которых
// ломается ТИХО:
//
//   1. ЧТЕНИЕ СИНХРОННОЕ. Стоит getMeetState вернуть Promise (или начать
//      отдавать null там, где кэш прогрет), и кнопка встречи замигает на
//      каждой перерисовке ленты. Битрикс пересобирает ленту целиком по каждому
//      чиху, поэтому мигать будет постоянно — и выглядеть как дефект рендера.
//   2. БАТЧ, А НЕ N+1. Двадцать промахов обязаны дать ОДИН запрос диапазоном.
//      Регрессия здесь не «медленно», а «пятьдесят запросов на прокрутку».
//   3. ПОЛЛИНГА НЕТ. Терминальные статусы не перезапрашиваются НИКОГДА —
//      ни при перерисовке, ни при resync. Регрессия даёт постоянный фоновый
//      трафик, которого никто не заметит, пока лента не станет длинной.
//
// Запуск: node tmux-web/tests/frontend/messenger_meet_store.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const S = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
/** Пауза коалесцинга в тестах: короче боевых 40 мс, чтобы файл шёл быстро. */
const DELAY = 5;
const WAIT = 40;

/** Журналируемый транспорт батча. */
function makeTransport(answer) {
    const calls = [];
    const fn = async (q) => {
        calls.push(q);
        return answer ? answer(q) : { badges: [] };
    };
    return { fn, calls };
}

function badge(messageId, status, extra) {
    return Object.assign({
        chat_id: '7',
        message_id: messageId,
        meeting_id: `m-${messageId}`,
        status,
        summary_status: 'none',
        platform: 'telemost',
        started_at: null,
        ended_at: null,
        duration_ms: null,
    }, extra || {});
}

const K = (id, chat = '7', plugin = 'telegram') => S.meetKey(plugin, chat, id);

// ==========================================================================
// 1. Ключ строки ленты — тройка, а не ссылка
// ==========================================================================

{
    eq(S.meetKey('telegram', '7', 100), 'telegram|7|100', 'ключ собирается из тройки (plugin, chat_id, message_id)');
    eq(S.parseMeetKey('telegram|7|100'), { plugin: 'telegram', chatId: '7', messageId: 100 }, 'и разбирается обратно');
    eq(S.parseMeetKey('telegram|7|abc'), null, 'нечисловой message_id — не ключ (серверу нельзя послать min_id=NaN)');
    eq(S.parseMeetKey('мусор'), null, 'мусор не разбирается');
    // Один и тот же message_id в разных чатах и панелях — разные строки.
    ok(S.meetKey('telegram', '7', 100) !== S.meetKey('vk', '7', 100), 'панель входит в ключ');
    ok(S.meetKey('telegram', '7', 100) !== S.meetKey('telegram', '8', 100), 'чат входит в ключ');
    // Ссылка на встречу в ключ НЕ входит: идентичность комнаты считает бэкенд.
    ok(!/https?:/.test(S.meetKey('telegram', '7', 100)), 'адрес встречи в ключ не входит вовсе');
    // `pruned` — Ф7: ретенция удалила файлы. Статус терминален, и опрашивать
    // его так же бессмысленно, как `ready`.
    eq(S.TERMINAL_STATUSES, ['ready', 'failed', 'cancelled', 'pruned'], 'список терминальных статусов совпадает с MeetStatus::is_terminal');
    eq(S.ACTIVE_STATUSES, ['queued', 'launching', 'joining', 'waiting_admit', 'recording', 'leaving'],
        'список активных статусов совпадает с MeetStatus::ACTIVE');
}

// ==========================================================================
// 2. Коалесцинг: двадцать промахов = один запрос диапазоном
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({ badges: [badge(105, 'recording')] }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    const reads = [];
    for (let i = 0; i < 20; i += 1) reads.push(S.getMeetState(K(100 + i)));
    ok(reads.every((r) => r === null), 'промах отдаёт null СИНХРОННО, а не Promise');
    ok(reads.every((r) => !(r && typeof r.then === 'function')), 'getMeetState не возвращает Promise ни при каких условиях');
    eq(t.calls.length, 0, 'до истечения паузы не ушло ничего');

    await sleep(WAIT);
    eq(t.calls.length, 1, '20 промахов подряд дали ОДИН запрос');
    eq(t.calls[0], { plugin: 'telegram', chat_id: '7', min_id: 100, max_id: 119 },
        'запрос идёт ДИАПАЗОНОМ min..max, а не списком id');
}

// ==========================================================================
// 3. Один запрос на чат, а не на строку — и на каждый чат свой
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({ badges: [] }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    for (let i = 0; i < 10; i += 1) S.getMeetState(K(100 + i, '7'));
    for (let i = 0; i < 10; i += 1) S.getMeetState(K(500 + i, '9'));
    for (let i = 0; i < 10; i += 1) S.getMeetState(K(300 + i, '7', 'vk'));
    await sleep(WAIT);

    eq(t.calls.length, 3, 'три разных ленты — три запроса, а не тридцать');
    eq(t.calls.map((c) => `${c.plugin}/${c.chat_id}`), ['telegram/7', 'telegram/9', 'vk/7'], 'по одному на пару (плагин, чат)');
}

// ==========================================================================
// 4. Ключи в полёте не дублируются
// ==========================================================================

{
    S.resetMeetStore();
    let release;
    const gate = new Promise((r) => { release = r; });
    const t = makeTransport(async () => { await gate; return { badges: [] }; });
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    S.getMeetState(K(100));
    await sleep(WAIT);
    eq(t.calls.length, 1, 'первый запрос ушёл');
    // Лента перерисовалась, пока ответ не пришёл.
    for (let i = 0; i < 5; i += 1) S.getMeetState(K(100));
    await sleep(WAIT);
    eq(t.calls.length, 1, 'пять перерисовок во время полёта не добавили ни одного запроса');
    release();
    await sleep(WAIT);
    eq(t.calls.length, 1, 'и после ответа тоже — состояние уже в кэше');
}

// ==========================================================================
// 5. АНТИ-МИГАНИЕ: прогретый кэш даёт тот же ответ при повторной сборке
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({ badges: [badge(120, 'recording', { started_at: 1000 })] }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    // Спрашиваем ДВЕ строки, сервер отвечает про одну: вторая обязана
    // закэшироваться как «встречи нет».
    S.getMeetState(K(120));
    S.getMeetState(K(121));
    await sleep(WAIT);

    const first = S.getMeetState(K(120));
    const second = S.getMeetState(K(120));
    const third = S.getMeetState(K(120));
    eq(first && first.status, 'recording', 'прогретый кэш отдаёт статус сразу');
    ok(first === second && second === third, 'три сборки строки подряд дают ТОТ ЖЕ объект (глиф не может мигнуть)');
    eq(t.calls.length, 1, 'повторные сборки не порождают запросов');

    // «Встречи нет» — это тоже ЗНАНИЕ, а не промах.
    const none = S.getMeetState(K(121));
    eq(none && none.status, 'none', 'сообщение без встречи закэшировано как «можно записать»');
    const noneAgain = S.getMeetState(K(121));
    ok(none === noneAgain, 'и оно тоже стабильно между сборками');
    await sleep(WAIT);
    eq(t.calls.length, 1, 'отрицательный ответ не превращается в вечный повторный запрос');
}

// ==========================================================================
// 6. ПОЛЛИНГА НЕТ: терминальный статус не перезапрашивается никогда
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({
        badges: [badge(130, 'ready'), badge(131, 'failed'), badge(132, 'cancelled'), badge(133, 'transcribing')],
    }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    for (const id of [130, 131, 132, 133]) S.getMeetState(K(id));
    await sleep(WAIT);
    eq(t.calls.length, 1, 'первый батч');

    // Сто перерисовок ленты — обычное дело для Битрикса.
    for (let pass = 0; pass < 100; pass += 1) {
        for (const id of [130, 131, 132, 133]) S.getMeetState(K(id));
    }
    await sleep(WAIT);
    eq(t.calls.length, 1, 'сто перерисовок не дали ни одного дополнительного запроса');

    for (const id of [130, 131, 132]) {
        ok(S.getMeetState(K(id)).terminal === true, `статус строки ${id} помечен терминальным`);
    }
    eq(S.getMeetState(K(133)).terminal, false, 'transcribing терминальным не считается');

    // RESYNC: нетерминальные добираются ОДНИМ батчем, терминальные не трогаются.
    const requeued = S.resyncMeet();
    eq(requeued, 1, 'в очередь на добор попала ровно одна запись — нетерминальная');
    for (const id of [130, 131, 132]) {
        ok(S.peekMeetState(K(id)) !== null, `терминальная запись ${id} пережила resync`);
    }
    eq(S.peekMeetState(K(133)), null, 'нетерминальная запись сброшена');
    await sleep(WAIT);
    eq(t.calls.length, 2, 'resync дал ОДИН добор, а не шквал');
    eq(t.calls[1], { plugin: 'telegram', chat_id: '7', min_id: 133, max_id: 133 }, 'добор идёт тем же диапазонным запросом');
}

// ==========================================================================
// 7. Диапазон шире MAX_BADGE_RANGE режется, а не отправляется как есть
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({ badges: [] }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    eq(S.MAX_BADGE_RANGE, 1000, 'потолок диапазона совпадает с MAX_BADGE_RANGE бэкенда');
    // Лента после прыжка в начало переписки: id разъехались на десятки тысяч.
    S.getMeetState(K(1));
    S.getMeetState(K(500));
    S.getMeetState(K(50000));
    await sleep(WAIT);
    eq(t.calls.length, 2, 'разброс шире потолка разрезан на два запроса');
    eq(t.calls.map((c) => [c.min_id, c.max_id]), [[1, 500], [50000, 50000]], 'каждый отрезок укладывается в потолок');
    ok(t.calls.every((c) => c.max_id - c.min_id + 1 <= S.MAX_BADGE_RANGE), 'ни один запрос не шире потолка (иначе сервер ответил бы 400)');

    // Границы разреза проверяются точно: 1000 id — это ещё один запрос.
    eq(S.chunkRanges([1, 1000]).length, 1, 'ровно 1000 в ширину — один отрезок');
    eq(S.chunkRanges([1, 1001]).length, 2, '1001 в ширину — уже два');
}

// ==========================================================================
// 8. Сбой транспорта не кэшируется и не превращается в поллинг
// ==========================================================================

{
    S.resetMeetStore();
    let fail = true;
    const t = makeTransport(async () => {
        if (fail) throw Object.assign(new Error('сеть моргнула'), { status: 500 });
        return { badges: [badge(140, 'ready')] };
    });
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });
    // Провал батча пишется в console.debug — здесь он ожидаемый, глушим,
    // чтобы стек не выглядел падением теста.
    const origDebug = console.debug;
    const debugged = [];
    console.debug = (...a) => debugged.push(a);

    S.getMeetState(K(140));
    await sleep(WAIT);
    eq(t.calls.length, 1, 'запрос ушёл');
    ok(debugged.length === 1, 'о сбое сказано в debug, а не в warn — это не ошибка пользователя');
    eq(S.peekMeetState(K(140)), null, 'ошибка НЕ закэширована — иначе строка навсегда осталась бы пустой');
    eq(S._debugMeetStore().inflight, 0, 'ключ снят с полёта');

    // Повтор привязан к действию (перерисовке), а не к таймеру: сам по себе
    // стор второй раз не пойдёт.
    await sleep(WAIT);
    eq(t.calls.length, 1, 'без перерисовки повтора нет — это не поллинг');

    fail = false;
    S.getMeetState(K(140));
    await sleep(WAIT);
    eq(t.calls.length, 2, 'следующая перерисовка пробует снова');
    eq(S.getMeetState(K(140)).status, 'ready', 'и на этот раз состояние приезжает');
    console.debug = origDebug;
}

// ==========================================================================
// 9. Кадры WS доходят до кэша через индекс meeting_id → строки
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({ badges: [badge(150, 'recording', { meeting_id: 'm-1', started_at: 1000 })] }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    S.getMeetState(K(150));
    await sleep(WAIT);
    eq(S.getMeetState(K(150)).status, 'recording', 'состояние из батча');

    const seen = [];
    const off = S.subscribeMeet((ev) => seen.push(ev.type));

    // MeetingRow НЕ несёт тройку-адрес: связь известна только по meeting_id,
    // и её дал батч выше.
    S.applyMeetingUpdate({ id: 'm-1', status: 'transcribing', summary_status: 'none', platform: 'telemost', started_at: 1000, ended_at: 2000, duration_ms: 1000 });
    eq(S.getMeetState(K(150)).status, 'transcribing', 'кадр meeting_update долетел до строки ленты');
    ok(seen.includes('meeting') && seen.includes('states'), 'подписчики получили и событие встречи, и событие строк');
    eq(t.calls.length, 1, 'обновление по WS не породило запроса — поллинга нет');

    // Встреча, о которой ни одна строка не знает, не теряется: индикатор
    // записи обязан работать с любой вкладки.
    S.applyMeetingUpdate({ id: 'm-99', status: 'recording', summary_status: 'none', platform: 'zoom', started_at: 500, ended_at: null, duration_ms: null });
    eq(S.getMeeting('m-99').status, 'recording', 'незнакомая встреча попала в реестр');
    ok(S.liveMeetings().some((m) => m.id === 'm-99'), 'и считается живой');

    off();
    const before = seen.length;
    S.applyMeetDeps({ chrome: { ok: false } });
    eq(seen.length, before, 'после отписки подписчик больше ничего не получает');
    eq(S.getMeetDeps().chrome.ok, false, 'но состояние зависимостей обновилось');
}

// ==========================================================================
// 10. Клик пользователя связывает встречу со строкой (linkMeeting)
// ==========================================================================

{
    S.resetMeetStore();
    const t = makeTransport(() => ({ badges: [] }));
    S.configureMeetStore({ fetchStates: t.fn, batchDelayMs: DELAY });

    // До клика строка пуста; ответ POST /api/meet/meetings даёт meeting_id.
    S.linkMeeting('m-new', K(160), { id: 'm-new', status: 'queued', summary_status: 'none', platform: 'telemost', started_at: null, ended_at: null, duration_ms: null });
    eq(S.getMeetState(K(160)).status, 'queued', 'строка получила состояние сразу после клика, без похода за батчем');
    eq(t.calls.length, 0, 'и без единого запроса');

    S.applyMeetingUpdate({ id: 'm-new', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 7000, ended_at: null, duration_ms: null });
    eq(S.getMeetState(K(160)).status, 'recording', 'дальнейшие кадры уже находят строку');
    eq(S.getMeetState(K(160)).startedAtMs, 7000000, 'метка старта доехала в МИЛЛИСЕКУНДАХ (провод отдаёт секунды) — по ней считается таймер');
}

// ==========================================================================
// 11. Живой таймер: один интервал на документ, счёт от startedAt
// ==========================================================================

{
    S.resetMeetStore();
    const fake = {
        intervals: 0,
        cleared: 0,
        handle: null,
        cb: null,
        nowValue: 10000,
    };
    S.configureMeetStore({
        fetchStates: async () => ({ badges: [] }),
        batchDelayMs: DELAY,
        timers: {
            setInterval: (fn) => { fake.intervals += 1; fake.cb = fn; fake.handle = { id: fake.intervals }; return fake.handle; },
            clearInterval: () => { fake.cleared += 1; fake.cb = null; },
            now: () => fake.nowValue,
        },
    });

    eq(fake.intervals, 0, 'на странице без записей не тикает ничего');

    S.applyMeetingUpdate({ id: 'a', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 4000, ended_at: null, duration_ms: null });
    eq(fake.intervals, 1, 'первая живая встреча заводит интервал');
    S.applyMeetingUpdate({ id: 'b', status: 'joining', summary_status: 'none', platform: 'zoom', started_at: null, ended_at: null, duration_ms: null });
    eq(fake.intervals, 1, 'вторая живая встреча НЕ заводит второй интервал (один на документ)');

    const ticks = [];
    const off = S.subscribeMeet((ev) => { if (ev.type === 'tick') ticks.push(ev.at); });
    fake.cb();
    eq(ticks, [10000], 'тик расходится подписчикам');

    // Цифра считается ВЫЧИТАНИЕМ: перезагрузка вкладки её не обнулит.
    eq(S.elapsedMs(4000, 10000), 6000, 'elapsedMs = now - startedAt');
    eq(S.elapsedMs(4000), 6000, 'без явного now берутся часы стора');
    eq(S.elapsedMs(null), 0, 'без метки старта — ноль, а не NaN');
    eq(S.elapsedMs(20000, 10000), 0, 'часы, отставшие от сервера, дают 0, а не отрицательное время');

    S.applyMeetingUpdate({ id: 'a', status: 'ready', summary_status: 'ok', platform: 'telemost', started_at: 4000, ended_at: 9000, duration_ms: 5000 });
    eq(fake.cleared, 0, 'пока жива вторая встреча, интервал не гасится');
    S.applyMeetingUpdate({ id: 'b', status: 'cancelled', summary_status: 'none', platform: 'zoom', started_at: null, ended_at: null, duration_ms: null });
    eq(fake.cleared, 1, 'последняя живая встреча погасила интервал');
    eq(S._debugMeetStore().ticking, false, 'таймера больше нет');

    S.applyMeetingUpdate({ id: 'c', status: 'recording', summary_status: 'none', platform: 'meet', started_at: 8000, ended_at: null, duration_ms: null });
    eq(fake.intervals, 2, 'новая запись заводит интервал заново');
    S.stopMeetTicker();
    eq(fake.cleared, 2, 'stopMeetTicker гасит его на выгрузке страницы');
    off();
}

// ==========================================================================
// 12. Очередь whisper видна честно: queued при статусе transcribing
// ==========================================================================

{
    S.resetMeetStore();
    S.configureMeetStore({ fetchStates: async () => ({ badges: [] }), batchDelayMs: DELAY });
    S.linkMeeting('m-q', K(170), { id: 'm-q', status: 'transcribing', summary_status: 'none', platform: 'telemost', started_at: 1, ended_at: 2, duration_ms: 1 });

    S.applyMeetProgress({ meeting_id: 'm-q', phase: 'transcribing', queued: true, queue_position: 2 });
    eq(S.getMeetState(K(170)).status, 'transcribing', 'статус НЕ подменяется очередью — отдельного статуса «в очереди» на бэкенде нет');
    eq(S.getMeetProgress('m-q'), { phase: 'transcribing', done: null, total: null, queued: true, queuePosition: 2 },
        'а позиция в очереди живёт отдельным индексом');

    S.applyMeetProgress({ meeting_id: 'm-q', phase: 'transcribing', done: 30, total: 100, queued: false });
    eq(S.getMeetProgress('m-q').queued, false, 'слот получен — очередь снята');
    eq(S.getMeetProgress('m-q').done, 30, 'счётчики прогресса читаются');

    S.applyMeetingUpdate({ id: 'm-q', status: 'ready', summary_status: 'ok', platform: 'telemost', started_at: 1, ended_at: 2, duration_ms: 1 });
    eq(S.getMeetProgress('m-q'), null, 'на терминальном статусе прогресс выброшен');
}

// ==========================================================================
// 13. Снапшот и падение подписчика
// ==========================================================================

{
    S.resetMeetStore();
    S.configureMeetStore({ fetchStates: async () => ({ badges: [] }), batchDelayMs: DELAY });

    S.applyMeetSnapshot({
        meetings: [{ id: 'm-s', status: 'recording', summary_status: 'none', platform: 'telemost', started_at: 1, ended_at: null, duration_ms: null }],
        deps: { whisper: { ok: true } },
        limits: { recordings: 1, whisper: 1, summaries: 1 },
    });
    eq(S.liveMeetings().length, 1, 'снапшот наполнил реестр активных');
    eq(S.getMeetLimits().whisper, 1, 'лимиты доехали');

    // Встреча завершилась, пока связи не было: следующий снапшот её не несёт.
    S.applyMeetSnapshot({ meetings: [], deps: null, limits: null });
    eq(S.liveMeetings().length, 0, 'реестр активных перезаписан целиком, а не слит');

    const got = [];
    S.subscribeMeet(() => { throw new Error('панель уронила обработчик'); });
    S.subscribeMeet((ev) => got.push(ev.type));
    const said = [];
    const origWarn = console.warn;
    console.warn = (...a) => said.push(a);
    S.applyMeetError({ meeting_id: 'm-s', code: 'chrome_unavailable', message: 'нет Chrome' });
    console.warn = origWarn;
    eq(got, ['error'], 'исключение одной панели не лишило обновления вторую');
    ok(said.length === 1, 'но про него сказано в консоли');
}

// ==========================================================================
// 14. Выключенный стор не делает ничего
// ==========================================================================

{
    S.resetMeetStore();
    eq(S.meetEnabled(), false, 'без configureMeetStore плагин выключен');
    for (let i = 0; i < 30; i += 1) eq(S.getMeetState(K(200 + i)), null, i === 0 ? 'чтение отдаёт null' : `чтение отдаёт null (${i})`);
    eq(S._debugMeetStore().pending, 0, 'в очередь не попало ничего');
    eq(S.resyncMeet(), 0, 'resync при выключенном плагине не ставит в очередь ничего');
    await sleep(WAIT);
    eq(S._debugMeetStore().flushPending, false, 'таймер батча не заводился');

    // Транспорт обязателен: гейт не имеет права «включить» стор пустышкой.
    let threw = false;
    try { S.configureMeetStore({}); } catch (_) { threw = true; }
    ok(threw, 'configureMeetStore без fetchStates бросает, а не включает стор молча');
    eq(S.meetEnabled(), false, 'и стор остаётся выключенным');
}

// ==========================================================================
// 15. Исходник: ни DOM, ни импортов панелей, ни window.ForgeApp
// ==========================================================================

{
    const { readFileSync } = await import('node:fs');
    const src = readFileSync(new URL('../../static/js/messenger/meet-store.js', import.meta.url), 'utf8');
    const code = src
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .split('\n')
        .filter((l) => !l.trimStart().startsWith('//') && !l.trimStart().startsWith('*'))
        .join('\n');

    ok(!/\bimport\s/.test(code), 'стор не импортирует НИЧЕГО — он нижний слой шины');
    ok(!/\bdocument\b/.test(code), 'ни одного обращения к document');
    ok(!/\bwindow\b/.test(code), 'ни одного обращения к window (и уж точно никакого ForgeApp)');
    ok(!/\bfetch\s*\(/.test(code), 'сеть приходит ИНЪЕКЦИЕЙ, своего fetch в модуле нет');
    ok(!/setInterval\s*\(\s*[^)]/.test(code.replace(/setInterval: \(fn, ms\) => setInterval\(fn, ms\)/, '')) || code.includes('t.setInterval('),
        'таймеры берутся из подменяемого объекта, а не напрямую');
    ok(!/\/api\/meet\//.test(code), 'адресов роутов в сторе нет — их знает только api.js');
}

// ==========================================================================

S.resetMeetStore();
if (failures === 0) console.log(`[OK] messenger_meet_store: ${checks} проверок`);
else console.error(`[FAIL] messenger_meet_store: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
