// ⚠ СТРАЖ ВКЛАДОК КАРТОЧКИ ВСТРЕЧИ (фронтовая Ф4 forge-meet, задача ФФ4.3).
//
// Три вкладки: саммари, расшифровка, участники. Каркас проверяет сосед
// (meet_popup.test.mjs); здесь — содержимое и четыре обещания, которые ломаются
// ТИХО и дорого:
//
//   1. ДЕЛЬТЫ ПРИМЕНЯЮТСЯ ПАЧКОЙ ЗА КАДР. Расшифровка приезжает несколькими
//      кадрами в секунду. Вставка каждой дельты по отдельности — перерисовка на
//      каждую реплику; на часовой встрече это 600–1200 перерисовок, и заметно
//      это будет не автору, а тому, у кого встреча длинная и машина слабая.
//      Проверяется НАБЛЮДАЕМЫМ: до кадра в DOM нет ни строки, после — все.
//   2. АВТОПРОКРУТКА ЧЕРЕЗ ОБЩИЙ createFollow. Решение принимается по
//      НАПРАВЛЕНИЮ прокрутки и по замеру «были ли у края» ДО вставки: свой
//      велосипед даёт ровно тот дефект «уехал вверх, а лента всё равно тянет
//      вниз», который в ленте уже чинили.
//   3. ПОТОЛОК 2000 СТРОК И ЧЕСТНАЯ СТРОКА О СРЕЗЕ. Виртуализации нет
//      намеренно; вместо неё потолок. Молча выброшенное начало расшифровки
//      читалось бы как потеря данных.
//   4. ПЕРЕКЛЮЧЕНИЕ ВКЛАДОК НИЧЕГО НЕ ТЕРЯЕТ. Панели прячутся атрибутом, а не
//      пересобираются: пересборка стирала бы и прокрутку, и набранный поиск —
//      а уходят на саммари и возвращаются именно затем, чтобы читать дальше.
//
// Плюс: саммари рендерится ОБЩИМ renderMarkdownText (второго парсера markdown в
// проекте не заводится), провал саммари виден текстом и кнопкой пересчёта, а
// участники несут длительность речи, переименование и прослушивание.
//
// Запуск: node tmux-web/tests/frontend/meet_popup_tabs.test.mjs

import { dispatch, makeDoc } from './meet_popup_dom.mjs';

const POPUP = await import(new URL('../../static/js/messenger/meet-popup.js', import.meta.url).href);
const STORE = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);

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

const KEY = 'telegram|777000|7';
const MEETING = 'm-1';
const sleep = () => new Promise((r) => setTimeout(r, 0));

function putBadge(status, extra) {
    STORE.applyBadges('telegram', '777000', [Object.assign({
        chat_id: '777000',
        message_id: 7,
        meeting_id: MEETING,
        status,
        summary_status: 'none',
        platform: 'telemost',
        started_at: 1700000000,
        ended_at: 1700003600,
        duration_ms: 3600000,
    }, extra || {})], [7]);
}

function detail() {
    return {
        key: KEY,
        panel: 'telegram',
        chatId: '777000',
        messageId: 7,
        meet: { url: 'https://telemost.yandex.ru/j/60497752704486', title: 'Созвон', platform: 'telemost' },
        state: STORE.peekMeetState(KEY),
    };
}

function seg(idx, text, over) {
    return Object.assign({
        id: idx,
        meeting_id: MEETING,
        idx,
        start_ms: idx * 1000,
        end_ms: idx * 1000 + 900,
        text,
        participant_id: 1,
        speaker_name: 'Софья',
        track_idx: 0,
        confidence: 0.9,
        overlapped: false,
    }, over || {});
}

/** Стенд: включённый стор, фейковый документ, карточка со своим api. */
function stand(api) {
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    const env = makeDoc(['telegram']);
    const calls = [];
    const conf = Object.assign({
        getMeeting: async () => { calls.push(['getMeeting']); return { id: MEETING, status: 'ready' }; },
        getSummary: async () => { calls.push(['getSummary']); return { text: '' }; },
        getTranscript: async (id, opts) => {
            calls.push(['getTranscript', opts]);
            return { segments: [], total: 0 };
        },
    }, api || {});
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: conf,
        raf: env.raf, caf: env.caf,
    });
    return { env, popup, calls, api: conf };
}

// ===========================================================================
// 1. Дельты: пачкой за кадр, а не по одной
// ===========================================================================

{
    const { env, popup } = stand();
    putBadge('transcribing');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();

    STORE.applyMeetTranscriptDelta({ meeting_id: MEETING, segments: [seg(1, 'первая'), seg(2, 'вторая')] });
    STORE.applyMeetTranscriptDelta({ meeting_id: MEETING, segments: [seg(3, 'третья')] });
    STORE.applyMeetTranscriptDelta({ meeting_id: MEETING, segments: [seg(4, 'четвёртая')] });

    eq(popup.debugState().buffered, 4, 'три кадра дельт накоплены в буфере');
    eq(popup.root.querySelectorAll('.meet-seg').length, 0,
        'и НИ ОДНА строка ещё не в DOM: вставка по дельте была бы перерисовкой на реплику');
    eq(popup.debugState().framePending, true, 'кадр отрисовки запрошен ровно один');

    eq(env.runFrames(), 1, 'кадров запрошено ровно ОДИН на три дельты');
    eq(popup.debugState().stats.flushes, 1, 'буфер слит один раз');
    eq(popup.root.querySelectorAll('.meet-seg').length, 4, 'все четыре реплики в DOM');
    eq(popup.debugState().buffered, 0, 'буфер пуст');

    // Повторная дельта (переподключение, resync) не удваивает строки.
    STORE.applyMeetTranscriptDelta({ meeting_id: MEETING, segments: [seg(4, 'четвёртая'), seg(5, 'пятая')] });
    env.runFrames();
    eq(popup.root.querySelectorAll('.meet-seg').length, 5,
        'повтор сегмента не создаёт дубль — ключ у строки есть');

    // Чужая встреча в карточку не попадает.
    STORE.applyMeetTranscriptDelta({ meeting_id: 'm-другая', segments: [seg(9, 'чужая')] });
    env.runFrames();
    eq(popup.root.querySelectorAll('.meet-seg').length, 5,
        'дельта чужой встречи отфильтрована: соединение одно на вкладку');

    const first = popup.root.querySelector('.meet-seg');
    eq(first.querySelector('.meet-seg-time').textContent, '0:01', 'метка времени в M:SS');
    eq(first.querySelector('.meet-seg-name').textContent, 'Софья', 'имя спикера из сегмента');
    eq(first.querySelector('.meet-seg-text').textContent, 'первая', 'текст реплики');
    popup.teardown();
}

// ===========================================================================
// 2. Автопрокрутка: общий createFollow, ручная прокрутка вверх её отменяет
// ===========================================================================

{
    const { env, popup } = stand();
    putBadge('transcribing');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();

    // Геометрия эмулируется ЧЕСТНО: `scrollHeight` растёт ВМЕСТЕ со вставкой
    // строк, как в браузере. Это и есть причина, по которой «у нижнего края?»
    // замеряется ДО мутации: выставь высоту руками до вставки — и получишь
    // зелёный тест на коде, который в браузере не догоняет ничего.
    const list = popup.refs.transcript;
    const ROW_PX = 20;
    Object.defineProperty(list, 'scrollHeight', {
        configurable: true,
        get: () => list.children.length * ROW_PX,
    });
    list.clientHeight = 40;
    list.scrollTop = 0;

    const push = (from, count) => {
        const batch = [];
        for (let i = from; i < from + count; i += 1) batch.push(seg(i, `r${i}`));
        STORE.applyMeetTranscriptDelta({ meeting_id: MEETING, segments: batch });
        env.runFrames();
    };

    // Первая пачка: были у нижнего края (список пуст) — лента догоняет.
    push(1, 5);
    eq(list.scrollTop, 60, 'у нижнего края лента догоняет расшифровку');

    // Пользователь уехал ВВЕРХ — следование обязано погаснуть.
    list.scrollTop = 10;
    dispatch(list, 'scroll');
    push(6, 5);
    eq(list.scrollTop, 10,
        'уехавшего вверх лента НЕ тянет вниз — иначе читать историю встречи невозможно');

    // Вернулся к краю — следование возобновилось.
    list.scrollTop = 160;
    dispatch(list, 'scroll');
    push(11, 5);
    eq(list.scrollTop, 260, 'вернувшемуся к краю лента снова догоняет');
    popup.teardown();
}

// ===========================================================================
// 3. Потолок 2000 строк и пагинация
// ===========================================================================

{
    const { env, popup } = stand();
    putBadge('transcribing');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();

    const flood = [];
    for (let i = 1; i <= 2500; i += 1) flood.push(seg(i, `реплика ${i}`));
    STORE.applyMeetTranscriptDelta({ meeting_id: MEETING, segments: flood });
    env.runFrames();

    eq(POPUP.TRANSCRIPT_CAP, 2000, 'потолок закреплён константой');
    eq(popup.root.querySelectorAll('.meet-seg').length, 2000, 'в DOM ровно потолок строк');
    eq(popup.debugState().dropped, 500, 'срезано начало, а не конец');
    eq(popup.refs.cap.hidden, false, 'о срезе сказано вслух');
    ok(/2000/.test(popup.refs.cap.textContent) && /2500/.test(popup.refs.cap.textContent),
        'строка о срезе называет и показанное, и полное число', popup.refs.cap.textContent);
    const rows = popup.root.querySelectorAll('.meet-seg');
    eq(rows[0].querySelector('.meet-seg-text').textContent, 'реплика 501',
        'первой осталась 501-я: живую расшифровку смотрят с конца');
    popup.teardown();
}

{
    // Пагинация: страницами по TRANSCRIPT_PAGE, кнопка исчезает на последней.
    const pages = [];
    const { env, popup } = stand({
        getTranscript: async (id, opts) => {
            pages.push(opts.offset);
            const from = opts.offset;
            const out = [];
            for (let i = from + 1; i <= Math.min(from + opts.limit, 1200); i += 1) out.push(seg(i, `p${i}`));
            return { segments: out, total: 1200 };
        },
    });
    putBadge('ready');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();
    await sleep();

    eq(POPUP.TRANSCRIPT_PAGE, 500, 'страница закреплена константой');
    eq(pages, [0], 'первая страница запрошена с нулевого смещения');
    eq(popup.debugState().rows, 500, 'показана первая страница');
    eq(popup.refs.more.hidden, false, 'кнопка «ещё» видна: на сервере есть непоказанное');
    ok(/500/.test(popup.refs.more.textContent), 'и называет, сколько ещё будет',
        popup.refs.more.textContent);

    popup.refs.more.click();
    await sleep();
    await sleep();
    eq(pages, [0, 500], 'вторая страница запрошена со смещением 500');
    eq(popup.debugState().rows, 1000, 'строк стало вдвое больше');

    popup.refs.more.click();
    await sleep();
    await sleep();
    eq(popup.debugState().rows, 1200, 'третья страница дочитала остаток');
    eq(popup.refs.more.hidden, true, 'и кнопка исчезла — дочитывать больше нечего');
    env.runFrames();
    popup.teardown();
}

// ===========================================================================
// 4. Поиск по расшифровке
// ===========================================================================

{
    const { env, popup } = stand();
    putBadge('ready');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();
    STORE.applyMeetTranscriptDelta({
        meeting_id: MEETING,
        segments: [seg(1, 'обсудили бюджет'), seg(2, 'потом сроки'), seg(3, 'бюджет ещё раз')],
    });
    env.runFrames();

    popup.refs.search.value = 'бюджет';
    dispatch(popup.refs.search, 'input');
    const visible = popup.root.querySelectorAll('.meet-seg').filter((n) => !n.hidden);
    eq(visible.length, 2, 'поиск оставил только совпавшие строки');
    eq(popup.refs.searchCount.textContent, '2', 'и посчитал их');
    eq(visible[0].dataset.meetHit, '1', 'совпадение помечено атрибутом — по нему его красит CSS');

    popup.refs.search.value = '';
    dispatch(popup.refs.search, 'input');
    eq(popup.root.querySelectorAll('.meet-seg').filter((n) => !n.hidden).length, 3,
        'пустой запрос возвращает все строки');
    eq(popup.refs.searchCount.textContent, '', 'и снимает счётчик');
    popup.teardown();
}

// ===========================================================================
// 5. Переключение вкладок ничего не теряет
// ===========================================================================

{
    const { env, popup } = stand();
    putBadge('ready');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();
    STORE.applyMeetTranscriptDelta({
        meeting_id: MEETING,
        segments: [seg(1, 'раз'), seg(2, 'два'), seg(3, 'три')],
    });
    env.runFrames();
    popup.refs.search.value = 'два';
    dispatch(popup.refs.search, 'input');
    popup.refs.transcript.scrollTop = 137;

    const before = popup.refs.transcript;
    popup.setTab('summary');
    eq(popup.refs.panes.transcript.hidden, true, 'панель расшифровки спрятана атрибутом');
    eq(popup.refs.panes.summary.hidden, false, 'а саммари показана');
    eq(popup.refs.tabs.summary.getAttribute('aria-selected'), 'true', 'вкладка отмечена для скринридера');
    eq(popup.refs.tabs.transcript.getAttribute('aria-selected'), 'false', 'а прошлая снята');

    popup.setTab('transcript');
    ok(popup.refs.transcript === before, 'узел списка ТОТ ЖЕ — панель не пересобиралась');
    eq(popup.refs.transcript.scrollTop, 137, 'прокрутка на месте');
    eq(popup.refs.search.value, 'два', 'запрос поиска на месте');
    eq(popup.root.querySelectorAll('.meet-seg').filter((n) => !n.hidden).length, 1,
        'и фильтр всё ещё применён');
    popup.teardown();
}

// ===========================================================================
// 6. Саммари — общим renderMarkdownText
// ===========================================================================

{
    const { popup } = stand({
        getSummary: async () => ({ text: '# Итоги\n\nДоговорились про **бюджет**.' }),
    });
    putBadge('ready', { summary_status: 'ready' });
    popup.open(detail());
    await sleep();
    await sleep();

    const heading = popup.refs.summary.querySelector('.md-h');
    ok(!!heading, 'заголовок markdown разобран — рендерит общий renderMarkdownText');
    eq(heading && heading.tagName, 'h1', 'и стал настоящим заголовком, а не абзацем со звёздочками');
    ok(!!popup.refs.summary.querySelector('strong'), 'жирный фрагмент стал <strong>, а не звёздочками');
    eq(popup.refs.summaryError.hidden, true, 'ошибки нет');
    eq(popup.refs.summaryRetry.hidden, true, 'и пересчитывать нечего');
    popup.teardown();
}

{
    // Провал саммари: расшифровка при этом ЖИВА (на бэкенде переход
    // summarizing → failed запрещён намеренно), поэтому встреча остаётся
    // `ready`, и единственный след провала — summary_status.
    const calls = [];
    const { popup } = stand({
        getSummary: async () => ({ text: '', status: 'failed', error: 'claude не ответил' }),
        retryMeeting: async (id, from) => { calls.push([id, from]); return {}; },
    });
    putBadge('ready', { summary_status: 'failed' });
    popup.open(detail());
    await sleep();
    await sleep();

    eq(popup.refs.summaryError.hidden, false, 'провал саммари показан текстом');
    ok(/claude не ответил/.test(popup.refs.summaryError.textContent),
        'с причиной от сервера', popup.refs.summaryError.textContent);
    ok(/расшифровк/i.test(popup.refs.summaryError.textContent),
        'и с обещанием, что расшифровка цела: иначе человек решит, что потерял час записи');
    eq(popup.refs.summaryRetry.hidden, false, 'кнопка пересчёта показана');

    popup.refs.summaryRetry.click();
    await sleep();
    eq(calls, [[MEETING, 'summarizing']], 'пересчёт просит повтор ИМЕННО этапа саммари');
    popup.teardown();
}

// ===========================================================================
// 7. Участники
// ===========================================================================

{
    const renames = [];
    const tracks = [];
    const { env, popup } = stand({
        patchSpeakers: async (id, payload) => { renames.push([id, payload]); return {}; },
        fetchMeetingAudio: async (id, opts) => { tracks.push(opts ? opts.track : null); return { size: 1 }; },
    });
    putBadge('ready');
    popup.open(detail());
    popup.setTab('transcript');
    await sleep();
    STORE.applyMeetTranscriptDelta({
        meeting_id: MEETING,
        segments: [
            seg(1, 'раз', { participant_id: 1, start_ms: 0, end_ms: 5000 }),
            seg(2, 'два', { participant_id: 1, start_ms: 6000, end_ms: 9000 }),
            // Без имени и с ДРУГИМ треком: подпись такого сегмента считается
            // из `track_idx`, и она обязана совпадать с тем, как участник
            // назван во вкладке участников («Участник 2»).
            seg(3, 'три', { participant_id: 2, speaker_name: null, track_idx: 1, start_ms: 10000, end_ms: 12000 }),
        ],
    });
    env.runFrames();

    STORE.applyMeetParticipants({
        meeting_id: MEETING,
        participants: [
            { id: 1, meeting_id: MEETING, participant_key: 'tile-1', display_name: 'Софья', track_idx: 0, is_bot: false, source: 'dom' },
            { id: 2, meeting_id: MEETING, participant_key: 'track-2', display_name: null, track_idx: 1, is_bot: false, source: 'ssrc' },
            { id: 3, meeting_id: MEETING, participant_key: 'bot', display_name: 'F.O.R.G.E.', track_idx: null, is_bot: true, source: 'manual' },
        ],
    });
    popup.setTab('people');

    const people = popup.root.querySelectorAll('.meet-person');
    eq(people.length, 3, 'все участники в списке, включая бота');
    eq(people[0].querySelector('.meet-person-name').textContent, 'Софья', 'имя найденного участника');
    eq(people[0].querySelector('.meet-person-spoke').textContent, '0:08',
        'длительность речи посчитана по загруженным сегментам');
    eq(people[0].querySelector('.meet-person-rename').hidden, true,
        'у найденного имени поля переименования нет: оно означало бы «мы не уверены»');

    eq(people[1].querySelector('.meet-person-name').textContent, 'Участник 2',
        'безымянный назван по треку — так же, как в расшифровке');
    eq(people[1].querySelector('.meet-person-rename').hidden, false,
        'и ему предложено переименование');
    eq(people[2].dataset.meetBot, '1', 'бот помечен атрибутом');

    // Прослушать участника — тот же fetch+blob, но с номером трека.
    const listen = people[1].querySelectorAll('button').find((b) => b.dataset.meetAct === 'listen');
    ok(!!listen, 'у участника с треком есть кнопка «послушать»');
    listen.click();
    await sleep();
    eq(tracks, [1], 'запрошен звук ИМЕННО этого трека');
    ok(!people[2].querySelectorAll('button').some((b) => b.dataset.meetAct === 'listen'),
        'а у участника без трека слушать нечего — кнопки нет');

    // Переименование по Enter уходит на PATCH speakers.
    const input = people[1].querySelector('.meet-person-rename');
    input.value = 'Пётр';
    dispatch(input, 'keydown', { key: 'Enter' });
    await sleep();
    eq(renames, [[MEETING, { participants: [{ id: 2, display_name: 'Пётр' }] }]],
        'переименование ушло на сервер одним участником');

    // ⚠ ОТВЕТ СЕРВЕРА НА ПЕРЕИМЕНОВАНИЕ ОБЯЗАН ПЕРЕПИСАТЬ УЖЕ НАРИСОВАННЫЕ
    // СТРОКИ РАСШИФРОВКИ.
    //
    // `PATCH /speakers` (Ф7) вещает ДВА кадра: `participants` и
    // `transcript_delta` с ТЕМИ ЖЕ сегментами и новым именем. Дедуп по `id`
    // молча выбрасывал бы второй, и получалось бы, что во вкладке участников
    // уже «Пётр», а в расшифровке рядом всё ещё «Участник 2» — до переоткрытия
    // карточки.
    popup.setTab('transcript');
    // Третий сегмент пришёл БЕЗ `speaker_name`, поэтому его подпись считается
    // из списка участников (`participant_id` → `display_name`), и «Участник 2»
    // там стоит именно потому, что имени у участника ещё нет.
    eq(popup.root.querySelectorAll('.meet-seg-name')[2].textContent, 'Участник 2',
        'до кадра сервера подпись прежняя');
    ok(popup.root.querySelectorAll('.meet-seg-name')[0].textContent === 'Софья',
        'а у сегмента с найденным именем подпись своя');
    STORE.applyMeetParticipants({
        meeting_id: MEETING,
        participants: [
            { id: 1, meeting_id: MEETING, participant_key: 'tile-1', display_name: 'Софья', track_idx: 0, is_bot: false, source: 'dom' },
            { id: 2, meeting_id: MEETING, participant_key: 'track-2', display_name: 'Пётр', track_idx: 1, is_bot: false, source: 'manual' },
        ],
    });
    STORE.applyMeetTranscriptDelta({
        meeting_id: MEETING,
        segments: [
            seg(3, 'три', { participant_id: 2, speaker_name: 'Пётр', start_ms: 10000, end_ms: 12000 }),
        ],
    });
    env.runFrames();
    const names = popup.root.querySelectorAll('.meet-seg-name').map((n) => n.textContent);
    eq(names.length, 3, 'повторный сегмент НЕ добавил четвёртой строки');
    eq(names[2], 'Пётр', 'подпись в расшифровке обновилась без перезагрузки карточки');
    popup.teardown();
}

// ===========================================================================

STORE.resetMeetStore();
if (failures === 0) console.log(`[OK] meet_popup_tabs: ${checks} проверок`);
else console.error(`[FAIL] meet_popup_tabs: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
