// ⚠ СТРАЖ ПЛЕЕРА КАРТОЧКИ ВСТРЕЧИ (фронтовая Ф4 forge-meet, задача ФФ4.4).
//
// Плеер архивной записи держит четыре обещания, и три из них ломаются молча:
//
//   1. ЗВУК ГРУЗИТСЯ `fetch`'ЕМ И ПОДСТАВЛЯЕТСЯ КАК `blob:`. Атрибут `src` у
//      `<audio>` не несёт заголовков, а `/api/*` требует Bearer — прямая ссылка
//      просто не воспроизведётся в remote-mode. Ровно тот дефект, ради которого
//      свой инвариант объявил `telegram/media.js`.
//   2. `URL.revokeObjectURL` ОБЯЗАТЕЛЕН. Архивный микс AAC 32 кбит/с — около
//      14 МБ на час. Без отзыва каждая прослушанная встреча остаётся в памяти
//      вкладки до перезагрузки страницы; за день работы PWA это десятки
//      мегабайт, и заметит это не автор, а тот, у кого вкладка живёт неделями.
//   3. ЗАГРУЗКА ТОЛЬКО ПО ЯВНОМУ НАЖАТИЮ. Открытие карточки не имеет права
//      утянуть десяток мегабайт по мобильной сети — отсюда `preload="none"` и
//      отдельная кнопка.
//   4. ОТСУТСТВУЮЩИЙ АРХИВ ОБЪЯСНЯЕТСЯ СЛОВАМИ. Пустой `<audio>` без причины
//      читается как поломка интерфейса, а причина ровно одна и называется
//      прямо (сведения не было — скорее всего, нет ffmpeg).
//
// ⚠ ЧЕГО ЗДЕСЬ НЕТ. Перемотка по часовой записи — свойство браузера, а не
// нашего кода: её невозможно проверить на фейковом документе. Проверяется то,
// что ЕЁ ОБЕСПЕЧИВАЕТ: источником стал `blob:`, а не потоковый адрес. Сама
// перемотка — пункт живой приёмки (гейт ФФ4.5).
//
// Запуск: node tmux-web/tests/frontend/meet_popup_player.test.mjs

import { makeDoc } from './meet_popup_dom.mjs';

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
        summary_status: 'ready',
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

/** Журнал выданных и отозванных `blob:`-ссылок. */
function urlBook() {
    const made = [];
    const revoked = [];
    return {
        made,
        revoked,
        api: {
            createObjectURL: (blob) => {
                const url = `blob:meet/${made.length + 1}`;
                made.push({ url, blob });
                return url;
            },
            revokeObjectURL: (url) => { revoked.push(url); },
        },
        /** Что выдано и не отозвано — то и висит в памяти вкладки. */
        leaked: () => made.map((m) => m.url).filter((u) => !revoked.includes(u)),
    };
}

function stand(api, urls) {
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    const env = makeDoc(['telegram']);
    const calls = [];
    const conf = Object.assign({
        getMeeting: async () => ({ id: MEETING, status: 'ready' }),
        getSummary: async () => ({ text: '' }),
        getTranscript: async () => ({ segments: [], total: 0 }),
        fetchMeetingAudio: async (id, opts) => {
            calls.push({ id, opts: opts || null });
            return { size: 14 * 1024 * 1024, type: 'audio/mp4' };
        },
    }, api || {});
    const popup = POPUP.createMeetPopup({
        panel: 'telegram', host: env.host, doc: env.doc, api: conf,
        raf: env.raf, caf: env.caf, urls: urls || undefined,
    });
    return { env, popup, calls };
}

// ===========================================================================
// 1. Ни байта до явного нажатия
// ===========================================================================

{
    const book = urlBook();
    const { popup, calls } = stand(null, book.api);
    putBadge('ready');
    popup.open(detail());
    await sleep();
    await sleep();

    eq(popup.refs.player.hidden, false, 'у готовой записи плеер показан');
    eq(popup.refs.audio.preload, 'none',
        'preload=none: открытие карточки не имеет права утянуть 14 МБ по мобильной сети');
    eq(calls.length, 0, 'и ни одного запроса за звуком до нажатия');
    eq(popup.refs.audio.hidden, true, 'сам элемент спрятан, пока источника нет');
    eq(book.made.length, 0, 'ни одной blob-ссылки не выдано');

    popup.refs.playerLoad.click();
    await sleep();
    eq(calls.length, 1, 'нажатие ушло за звуком ровно один раз');
    eq(calls[0].id, MEETING, 'за нужной встречей');
    eq(calls[0].opts, null, 'без трека — то есть за общим миксом');
    eq(book.made.length, 1, 'выдана одна blob-ссылка');
    eq(popup.refs.audio.src, 'blob:meet/1',
        'источник плеера — blob:, а НЕ /api/... : у атрибута src нет Bearer-токена');
    eq(popup.refs.audio.hidden, false, 'плеер показан');
    popup.teardown();
}

// ===========================================================================
// 2. Отзыв objectURL: закрытие, повторная загрузка, teardown
// ===========================================================================

{
    const book = urlBook();
    const { popup } = stand(null, book.api);
    putBadge('ready');
    popup.open(detail());
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();
    eq(book.leaked().length, 1, 'пока карточка открыта, ссылка жива — иначе нечего играть');

    popup.close();
    eq(book.revoked, ['blob:meet/1'], 'ЗАКРЫТИЕ ОТЗЫВАЕТ ССЫЛКУ');
    eq(book.leaked(), [], 'в памяти вкладки не осталось ничего');
    eq(popup.refs.audio.src, '', 'и источник снят: отозванная ссылка в src даёт «плеер не играет»');

    // Второе прослушивание той же встречи — новая ссылка, старая уже отозвана.
    popup.open(detail());
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();
    eq(book.made.length, 2, 'вторая загрузка выдала новую ссылку');
    eq(book.leaked(), ['blob:meet/2'], 'и висит ровно одна — прошлая отозвана');

    popup.teardown();
    eq(book.leaked(), [], 'teardown отзывает и её: выгрузка страницы не должна оставлять мегабайты');
}

{
    // Переключение между треками не копит ссылки: каждая следующая загрузка
    // отзывает предыдущую ДО того, как выдать новую.
    const book = urlBook();
    const { popup, calls } = stand(null, book.api);
    putBadge('ready');
    popup.open(detail());
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();
    eq(calls.length, 3, 'три нажатия — три загрузки');
    eq(book.leaked().length, 1, 'а живая ссылка одна: остальные отозваны по ходу');
    popup.teardown();
}

// ===========================================================================
// 3. Архива нет — объяснение словами, а не пустой элемент
// ===========================================================================

{
    const book = urlBook();
    const { popup } = stand({
        fetchMeetingAudio: async () => {
            const err = new Error('HTTP 404');
            err.status = 404;
            throw err;
        },
    }, book.api);
    putBadge('ready');
    popup.open(detail());
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();

    eq(popup.refs.audio.hidden, true, 'пустого плеера не показывается');
    eq(popup.refs.playerNote.hidden, false, 'вместо него — объяснение');
    ok(/ffmpeg/i.test(popup.refs.playerNote.textContent),
        'и оно называет настоящую причину: сведения не было', popup.refs.playerNote.textContent);
    eq(book.made.length, 0, 'ссылок при провале не выдаётся');
    eq(popup.refs.playerLoad.disabled, false, 'кнопка снова нажимаема — попытку можно повторить');
    popup.teardown();
}

{
    // Прочая ошибка (500, обрыв) — тоже словами, но своими.
    const { popup } = stand({
        fetchMeetingAudio: async () => {
            const err = new Error('сеть моргнула');
            err.status = 500;
            throw err;
        },
    }, urlBook().api);
    putBadge('ready');
    popup.open(detail());
    await sleep();
    popup.refs.playerLoad.click();
    await sleep();
    ok(/сеть моргнула/.test(popup.refs.playerNote.textContent),
        'сообщение сервера доносится до человека', popup.refs.playerNote.textContent);
    ok(!/ffmpeg/i.test(popup.refs.playerNote.textContent),
        'и его не подменяет рассказ про ffmpeg — причина другая');
    popup.teardown();
}

// ===========================================================================
// 4. Пер-трековое прослушивание — тот же механизм
// ===========================================================================

{
    const book = urlBook();
    const { popup, calls } = stand(null, book.api);
    putBadge('ready');
    popup.open(detail());
    await sleep();
    STORE.applyMeetParticipants({
        meeting_id: MEETING,
        participants: [
            { id: 2, meeting_id: MEETING, participant_key: 'track-2', display_name: null, track_idx: 1, is_bot: false },
        ],
    });
    popup.setTab('people');
    const listen = popup.root.querySelectorAll('button').find((b) => b.dataset.meetAct === 'listen');
    ok(!!listen, 'кнопка прослушивания участника есть');

    listen.click();
    await sleep();
    eq(calls.length, 1, 'запрос ушёл');
    eq(calls[0].opts, { track: 1 }, 'с номером трека — тот же роут, другой параметр');
    eq(popup.refs.audio.src, 'blob:meet/1', 'и звук подставлен тем же blob-механизмом');

    popup.close();
    eq(book.leaked(), [], 'пер-трековая ссылка отзывается так же, как общая');
    popup.teardown();
}

// ===========================================================================
// 5. Плеера нет там, где играть нечего
// ===========================================================================

{
    const { popup, calls } = stand(null, urlBook().api);
    putBadge('recording', { ended_at: null, duration_ms: null });
    popup.open(detail());
    await sleep();
    eq(popup.refs.player.hidden, true,
        'во время записи плеера нет: архива ещё не существует, а кнопка обещала бы обратное');
    eq(calls.length, 0, 'и запросов за звуком тоже нет');

    putBadge('ready');
    popup.open(detail());
    await sleep();
    eq(popup.refs.player.hidden, false, 'у готовой записи плеер вернулся');
    popup.teardown();
}

// ===========================================================================
// 6. ⚠ СТРАЖ ИСХОДНИКА: источник снимается АТРИБУТОМ, а не пустой строкой
// ===========================================================================
//
// `audio.src = ''` — это ОТНОСИТЕЛЬНЫЙ адрес, который браузер разрешает в адрес
// самой страницы: элемент уходит грузить HTML документа и печатает ошибку
// декодирования в консоль. На фейковом документе оба варианта неразличимы
// (свойство и там и там становится пустым), поэтому инвариант держится
// исходником — иначе он не держится ничем до живой пробы в браузере.

{
    const { readFileSync } = await import('node:fs');
    const raw = readFileSync(new URL('../../static/js/messenger/meet-popup.js', import.meta.url), 'utf8');
    // Сначала строчные комментарии, потом блочные: в шапке есть `/api/*`.
    const src = raw.replace(/(^|[^:])\/\/.*$/gm, '$1').replace(/\/\*[\s\S]*?\*\//g, '');
    ok(/removeAttribute\('src'\)/.test(src),
        'источник снимается removeAttribute(\'src\')');
    for (const node of ['audio', 'shot']) {
        ok(!new RegExp(`refs\\.${node}\\.src\\s*=\\s*['\`]['\`]`).test(src),
            `refs.${node}.src не присваивается пустая строка — она грузила бы саму страницу`);
    }
}

// ===========================================================================
// 7. Клиент api собирает адрес трека сам
// ===========================================================================

{
    // Проверяется ИМЕННО адрес: роут один, а трек — параметр запроса. Отдельный
    // роут здесь был бы расхождением с таблицей `routes/mod.rs`.
    const seen = [];
    globalThis.window = globalThis.window || {};
    globalThis.fetch = async (url) => {
        seen.push(String(url));
        return { ok: true, status: 200, blob: async () => ({ size: 1 }) };
    };
    globalThis.window.fetch = globalThis.fetch;
    globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
    globalThis.location = {
        protocol: 'http:', host: 'localhost', href: 'http://localhost/', origin: 'http://localhost',
        pathname: '/', search: '', hash: '',
    };
    const API = await import(new URL('../../static/js/meet/api.js', import.meta.url).href);
    await API.fetchMeetingAudio('m-1');
    await API.fetchMeetingAudio('m-1', { track: 2 });
    eq(seen, ['/api/meet/meetings/m-1/audio', '/api/meet/meetings/m-1/audio?track=2'],
        'адрес трека — тот же роут с параметром запроса');
}

// ===========================================================================

STORE.resetMeetStore();
if (failures === 0) console.log(`[OK] meet_popup_player: ${checks} проверок`);
else console.error(`[FAIL] meet_popup_player: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
