// Регресс-тест ответов панели VK — UI (js/vk/chat.js) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// ЧИСТАЯ логика ответов (нормализация, превью, ключ цели) живёт в
// vk_reply.test.mjs; здесь проверяется всё, что требует дерева и событий:
//
//   1. ПОЛОСА ОТВЕТА над #vk-prompt — появляется, стоит вплотную к форме,
//      снимается ✕ и Esc, но Esc уступает открытому лайтбоксу;
//   2. ВЫБОР ЦЕЛИ — двойной клик по строке и кнопка «↰», со стоп-листом
//      (кнопки, ссылки, цитата, системные строки, строка «в пути»);
//   3. ЦИТАТА В СТРОКЕ и ПЕРЕХОД К ОРИГИНАЛУ по cmid — включая оригинал ВНЕ
//      загруженной страницы (догрузка + честная системная строка);
//   4. ОТПРАВКА — `reply_to_cmid` в теле POST, цитата в optimistic-строке до
//      подтверждения, полоса держится до успеха, [retry] в ТО ЖЕ сообщение;
//   5. АВТОСБРОС ЦЕЛИ — удаление оригинала, смена беседы, resetChat.
//
// МУТАЦИОННАЯ ПРОВЕРКА: отправить `reply_to_id` вместо `reply_to_cmid` —
// краснеет раздел 4; не сбрасывать цель после успешной отправки — краснеет
// он же.
//
// Запуск: node tmux-web/tests/frontend/vk_reply_ui.test.mjs

import {
    $log, $input, $prompt, $lightbox, $document,
    dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/reply.js', import.meta.url).href);
const RL = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit). Здесь он только
// мешал бы: каждый запрос ждал бы настоящие 380 мс.
RL.vkLimits.minIntervalMs = 1;
RL.vkLimits.burst = 100;

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

const settle = async (ms) => {
    await new Promise((r) => setTimeout(r, ms === undefined ? 5 : ms));
    await flush();
};

/** Строки сообщений (спейсер, системные строки и ошибки не в счёт). */
const lines = () => $log.children.filter((n) => n.className.includes('vk-line')
    && !n.className.includes('vk-line-system')
    && !n.className.includes('vk-line-error'));

const systemLines = () => $log.children.filter((n) => n.className.includes('vk-line-system'));

const bar = () => $prompt.parentNode.children.find((n) => n.className === 'vk-reply-bar') || null;

const rowByCmid = (cmid) => lines().find((n) => n.dataset.cmid === String(cmid)) || null;

function message(id, extra) {
    return Object.assign({
        id,
        dialog_id: '2000000007',
        out: false,
        date: 1_700_000_000 + id,
        sender: { id: '1234', first_name: 'Павел', last_name: 'Дуров', screen_name: 'durov' },
        text: `сообщение ${id}`,
        media: null,
        attachments: [],
        reply_to_id: null,
        reply_to: null,
        edit_date: null,
        reply_markup: null,
        cmid: id - 6000,
        fwd: [],
        action: null,
        payload: null,
    }, extra || {});
}

const DIALOG = { id: '2000000007', kind: 'chat', title: 'Проектная' };
S.vkState.dialogs = [DIALOG];
S.vkState.selfUser = { id: '999', first_name: 'Я', screen_name: 'me' };

C.initChat({ onRead: () => {}, presenceOf: () => null });

const history = (msgs, hasMore) => async (url, init) => {
    if (init && init.method === 'POST') return jsonRes(200, { ok: true });
    if (url.includes('/messages')) return jsonRes(200, { messages: msgs, has_more: !!hasMore });
    return jsonRes(200, {});
};

// ---------------------------------------------------------------------------
// 1. Полоса ответа: показ, позиция, снятие
// ---------------------------------------------------------------------------

setNet(history([message(7001), message(7002)], false));
await C.openDialog('2000000007', DIALOG);
await settle();
eq(lines().length, 2, 'история нарисована');
ok(bar() === null, 'без выбора цели полосы в разметке нет вовсе');

{
    const row = rowByCmid(1002);
    const focusBefore = $input.focusCount;
    dispatch($log, 'dblclick', { target: row });

    const b = bar();
    ok(b && !b.hidden, 'двойной клик по строке показал полосу ответа');
    eq(b.querySelector('.vk-reply-bar-name').textContent, 'durov', 'в полосе — автор оригинала');
    eq(b.querySelector('.vk-reply-bar-text').textContent, 'сообщение 7002', 'и превью');
    ok($prompt.previousElementSibling === b, 'полоса стоит ВПЛОТНУЮ к форме ввода');
    ok($input.focusCount > focusBefore, 'фокус ушёл в поле ввода — это начало набора');
    eq(R._debugReplyState().cmid, 1002, 'цель адресуется cmid, а не message_id');
    eq(R._debugReplyState().messageId, 7002, 'глобальный id едет рядом справочно');

    // ✕ снимает цель.
    const close = b.querySelector('.vk-reply-cancel');
    ok(close, 'у полосы есть кнопка отмены');
    eq(close.getAttribute('aria-label'), 'Отменить ответ', 'кнопка объявляет себя скринридеру');
    dispatch(close, 'click');
    ok(bar().hidden, 'полоса спрятана');
    eq(R._debugReplyState().cmid, null, 'цель снята');
}

// ---------------------------------------------------------------------------
// 2. Esc: снимает ответ, но уступает лайтбоксу
// ---------------------------------------------------------------------------

{
    dispatch($log, 'dblclick', { target: rowByCmid(1001) });
    eq(R._debugReplyState().cmid, 1001, 'цель выбрана');

    // Открытый лайтбокс забирает Esc себе: этот обработчик регистрируется
    // раньше лайтбоксного, и без гарда одно нажатие снимало бы полосу вместо
    // закрытия картинки.
    $lightbox.hidden = false;
    const guarded = dispatch($document, 'keydown', { key: 'Escape' });
    eq(R._debugReplyState().cmid, 1001, 'при открытом лайтбоксе Esc цель НЕ снимает');
    eq(guarded.defaultPrevented, false, 'и событие не глотает');
    $lightbox.hidden = true;

    const e = dispatch($document, 'keydown', { key: 'Escape' });
    eq(R._debugReplyState().cmid, null, 'Esc снял цель');
    eq(e.defaultPrevented, true, 'и погасил событие — иначе его перехватят соседи');

    // Без активной цели Esc чужой и не гасится.
    const idle = dispatch($document, 'keydown', { key: 'Escape' });
    eq(idle.defaultPrevented, false, 'без цели Esc проходит насквозь');
    const other = dispatch($document, 'keydown', { key: 'Enter' });
    eq(other.defaultPrevented, false, 'чужие клавиши не трогаем');
}

// ---------------------------------------------------------------------------
// 3. Стоп-лист двойного клика и кнопка «ответить»
// ---------------------------------------------------------------------------

{
    const row = rowByCmid(1001);
    const btn = row.children.find((n) => n.className === 'vk-reply-btn');
    ok(btn, 'у строки с cmid есть кнопка «ответить»');
    eq(btn.textContent, '↰', 'глиф кнопки');
    eq(btn.getAttribute('aria-label'), 'Ответить: durov', 'кнопка называет собеседника');
    ok(row.children[row.children.length - 1] === btn,
        'кнопка — последний потомок (она вне потока)');

    // Двойной клик ПО КНОПКЕ ответ не начинает: у неё свой обработчик.
    dispatch($log, 'dblclick', { target: btn });
    eq(R._debugReplyState().cmid, null, 'двойной клик по кнопке в стоп-листе');

    // Одинарный клик по кнопке — начинает.
    const ev = dispatch(btn, 'click');
    eq(R._debugReplyState().cmid, 1001, 'клик по кнопке выбрал цель');
    eq(ev.propagationStopped, true, 'клик не всплывает в делегированные обработчики ленты');
    C.cancelReply();

    // Системная строка `_vk` не несёт — отвечать на неё нечем.
    const sys = systemLines()[0];
    dispatch($log, 'dblclick', { target: sys });
    eq(R._debugReplyState().cmid, null, 'системная строка целью не становится');

    // Клик мимо строк (спейсер).
    const spacer = $log.children.find((n) => n.className === 'vk-log-spacer');
    dispatch($log, 'dblclick', { target: spacer });
    eq(R._debugReplyState().cmid, null, 'клик по спейсеру ответа не начинает');
}

// ---------------------------------------------------------------------------
// 4. Цитата в строке и переход к оригиналу
// ---------------------------------------------------------------------------

{
    const reply = message(7003, {
        text: 'это ответ',
        reply_to: {
            message_id: 7001, cmid: 1001, sender: 'Павел Дуров', sender_id: '1234',
            text: 'сообщение 7001', media_kind: null, unavailable: false,
        },
    });
    setNet(history([message(7001), message(7002), reply], false));
    await C.openDialog('2000000007', DIALOG);
    await settle();

    const row = rowByCmid(1003);
    const quote = row.querySelector('.vk-reply-quote');
    ok(quote, 'у ответа нарисована цитата');
    ok(row.children[0] === quote, 'цитата — ПЕРВЫЙ потомок: «на что отвечают» читается раньше');
    eq(quote.querySelector('.vk-reply-quote-name').textContent, 'Павел Дуров', 'автор оригинала');
    eq(quote.querySelector('.vk-reply-quote-text').textContent, 'сообщение 7001', 'превью');
    eq(quote.dataset.replyTo, '1001', 'ключ перехода — cmid');
    eq(quote.dataset.replyMid, '7001', 'глобальный id — только для дозапроса');
    eq(quote.getAttribute('role'), 'button', 'цитата объявлена кнопкой');
    eq(quote.tabIndex, 0, 'и попадает в таб-порядок');

    // Переход к оригиналу: строка в ленте ⇒ прокрутка + подсветка.
    $log.scrollHeight = 1000;
    $log.clientHeight = 200;
    $log.scrollTop = 500;
    dispatch($log, 'click', { target: quote });
    await settle();
    const original = rowByCmid(1001);
    ok(original.className.includes('vk-msg-highlight'), 'оригинал подсвечен');
    eq(C._debugChatState().flashCmid, 1001, 'подсветка запомнена по cmid, а не по узлу');

    // Enter на цитате работает как клик (цитата в таб-порядке).
    C.onDeleted({ dialog_id: '2000000007', message_ids: [] }); // no-op, лента цела
    const keyed = dispatch($log, 'keydown', { key: 'Enter', target: quote });
    eq(keyed.defaultPrevented, true, 'Enter на цитате перехвачен');

    // Пробел тоже — иначе он прокрутил бы ленту на экран вниз.
    const spaced = dispatch($log, 'keydown', { key: ' ', target: quote });
    eq(spaced.defaultPrevented, true, 'пробел на цитате не прокручивает ленту');
}

// ---------------------------------------------------------------------------
// 5. Недоступный оригинал и цитата без cmid
// ---------------------------------------------------------------------------

{
    const gone = message(7010, {
        reply_to: {
            message_id: null, cmid: null, sender: null, sender_id: null,
            text: null, media_kind: null, unavailable: true,
        },
    });
    setNet(history([gone], false));
    await C.openDialog('2000000007', DIALOG);
    await settle();

    const quote = rowByCmid(1010).querySelector('.vk-reply-quote');
    eq(quote.querySelector('.vk-reply-quote-text').textContent, 'сообщение недоступно',
        'удалённый оригинал рисуется текстом, а не пустой цитатой');
    eq(quote.dataset.replyTo, undefined, 'переходить некуда — data-reply-to не выставлен');
    eq(quote.getAttribute('role'), null, 'и кнопкой цитата не объявлена');

    // Клик по такой цитате ничего не делает и системных строк не плодит.
    const before = systemLines().length;
    dispatch($log, 'click', { target: quote });
    await settle();
    eq(systemLines().length, before, 'клик по нерабочей цитате молчит');
}

// ---------------------------------------------------------------------------
// 6. Переход к оригиналу ВНЕ загруженной страницы
// ---------------------------------------------------------------------------

{
    const reply = message(7100, {
        text: 'ответ на старое',
        reply_to: {
            message_id: 6500, cmid: 500, sender: 'Павел Дуров', sender_id: '1234',
            text: 'старое', media_kind: null, unavailable: false,
        },
    });
    // Первая страница — только ответ; страница вверх приносит оригинал.
    let older = 0;
    setNet(async (url, init) => {
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (url.includes('before_id=')) {
            older += 1;
            return jsonRes(200, {
                messages: [message(6500, { cmid: 500, text: 'старое' })],
                has_more: false,
            });
        }
        if (url.includes('/messages')) return jsonRes(200, { messages: [reply], has_more: true });
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();
    eq(lines().length, 1, 'на странице только ответ');

    dispatch($log, 'click', { target: rowByCmid(1100).querySelector('.vk-reply-quote') });
    await settle(30);
    eq(older, 1, 'догружена ровно одна страница вверх — оригинал нашёлся на ней');
    ok(rowByCmid(500), 'оригинал теперь в ленте');
    ok(rowByCmid(500).className.includes('vk-msg-highlight'),
        'подсветка ПЕРЕЖИЛА перерисовку ленты (renderHistory вернул класс по cmid)');
}

{
    // Оригинал не нашёлся за REPLY_JUMP_PAGES страниц — спрашиваем сервер про
    // одно сообщение и показываем его текст строкой.
    const reply = message(7200, {
        text: 'ответ на очень старое',
        reply_to: {
            message_id: 6000, cmid: 100, sender: 'Павел Дуров', sender_id: '1234',
            text: 'очень старое', media_kind: null, unavailable: false,
        },
    });
    let pages = 0;
    let byId = 0;
    setNet(async (url, init) => {
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (/\/messages\/\d+$/.test(url)) {
            byId += 1;
            return jsonRes(200, {
                message: message(6000, { cmid: 100, text: 'очень старое' }),
            });
        }
        if (url.includes('before_id=')) {
            pages += 1;
            // Каждая страница приносит новое сообщение, но НЕ оригинал.
            return jsonRes(200, {
                messages: [message(6900 - pages, { cmid: 900 - pages })],
                has_more: true,
            });
        }
        if (url.includes('/messages')) return jsonRes(200, { messages: [reply], has_more: true });
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();

    dispatch($log, 'click', { target: rowByCmid(1200).querySelector('.vk-reply-quote') });
    await settle(80);
    eq(pages, C.REPLY_JUMP_PAGES,
        'догрузка ограничена потолком — у VK бюджет 3 rps, бесконечный скролл его съел бы');
    eq(byId, 1, 'ровно один дозапрос за конкретным сообщением');
    ok(systemLines().some((n) => n.textContent.includes('↳ #100 durov: очень старое')),
        `оригинал показан строкой лога: ${systemLines().map((n) => n.textContent).join(' | ')}`);
}

{
    // Оригинал удалён: сервер отвечает unknown_message — говорим об этом
    // прямо, а не «не удалось открыть».
    const reply = message(7300, {
        reply_to: {
            message_id: 5000, cmid: 50, sender: 'Павел Дуров', sender_id: '1234',
            text: 'нет его', media_kind: null, unavailable: false,
        },
    });
    setNet(async (url, init) => {
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (/\/messages\/\d+$/.test(url)) {
            return jsonRes(404, { error: { code: 'unknown_message', message: 'нет такого' } });
        }
        if (url.includes('/messages')) return jsonRes(200, { messages: [reply], has_more: false });
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();
    dispatch($log, 'click', { target: rowByCmid(1300).querySelector('.vk-reply-quote') });
    await settle(30);
    ok(systemLines().some((n) => n.textContent.includes('сообщение недоступно: #50 (удалено)')),
        'удалённый оригинал назван удалённым');
}

{
    // Цитата без глобального id (инлайненный оригинал VK его не несёт) и вне
    // загруженной страницы: дозапрос невозможен — честная строка вместо
    // молчания.
    const reply = message(7400, {
        reply_to: {
            message_id: null, cmid: 40, sender: 'Павел Дуров', sender_id: '1234',
            text: 'без id', media_kind: null, unavailable: false,
        },
    });
    let byId = 0;
    setNet(async (url, init) => {
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (/\/messages\/\d+$/.test(url)) { byId += 1; return jsonRes(200, {}); }
        if (url.includes('/messages')) return jsonRes(200, { messages: [reply], has_more: false });
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();
    dispatch($log, 'click', { target: rowByCmid(1400).querySelector('.vk-reply-quote') });
    await settle(20);
    eq(byId, 0, 'без глобального id дозапрос невозможен — messages.getById понимает только его');
    ok(systemLines().some((n) => n.textContent.includes('сообщение #40 не найдено')),
        'вместо молчания — честная системная строка');
}

// ---------------------------------------------------------------------------
// 7. Отправка ответа: reply_to_cmid в теле, цитата до подтверждения, [retry]
// ---------------------------------------------------------------------------

{
    setNet(async (url, init) => {
        if (init && init.method === 'POST' && url.includes('/messages')) {
            return jsonRes(200, {
                message: message(7500, {
                    out: true, text: 'отвечаю', cmid: 1500, sender: { id: '999' },
                    reply_to: {
                        message_id: 7002, cmid: 1002, sender: 'Павел Дуров', sender_id: '1234',
                        text: 'сообщение 7002', media_kind: null, unavailable: false,
                    },
                }),
            });
        }
        if (url.includes('/messages')) {
            return jsonRes(200, { messages: [message(7001), message(7002)], has_more: false });
        }
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();

    dispatch($log, 'dblclick', { target: rowByCmid(1002) });
    eq(R._debugReplyState().cmid, 1002, 'цель выбрана');

    const before = netCalls.length;
    $input.value = 'отвечаю';
    dispatch($prompt, 'submit');

    // Цитата обязана появиться СРАЗУ вместе с optimistic-строкой: если ждать
    // сервера, подпись «отвечаю Васе» мигнёт в момент reconcile.
    const pending = lines().find((n) => n.dataset.tempId);
    ok(pending, 'optimistic-строка появилась мгновенно');
    const optimisticQuote = pending.querySelector('.vk-reply-quote');
    ok(optimisticQuote, 'и уже несёт цитату');
    eq(optimisticQuote.querySelector('.vk-reply-quote-text').textContent, 'сообщение 7002',
        'превью цитаты — из выбранной цели, а не «…»');
    ok(!bar().hidden, 'полоса держится до подтверждения отправки');

    await settle(20);
    const post = netCalls.slice(before).find((c) => c.method === 'POST');
    eq(post.body.reply_to_cmid, 1002,
        'на сервер ушёл conversation_message_id — им VK адресует ответ в беседе');
    eq(post.body.reply_to_id, undefined, 'глобальный id в теле не отправляется');
    ok(bar().hidden, 'после подтверждения полоса снята — иначе следующее сообщение уйдёт в ветку');
    eq(R._debugReplyState().cmid, null, 'и цель сброшена');

    const sent = lines().find((n) => n.dataset.cmid === '1500');
    ok(sent, 'реальный DTO заменил optimistic-строку');
    ok(sent.querySelector('.vk-reply-quote'), 'у подтверждённой строки цитата на месте');
}

{
    // Ошибка сети: полоса ОСТАЁТСЯ (пользователь жмёт [retry]), а повтор
    // уходит в ТО ЖЕ сообщение.
    let attempts = 0;
    setNet(async (url, init) => {
        if (init && init.method === 'POST' && url.includes('/messages')) {
            attempts += 1;
            if (attempts === 1) {
                return jsonRes(503, { error: { code: 'not_connected', message: 'нет связи' } });
            }
            return jsonRes(200, {
                message: message(7600, { out: true, text: 'повтор', cmid: 1600 }),
            });
        }
        if (url.includes('/messages')) {
            return jsonRes(200, { messages: [message(7001), message(7002)], has_more: false });
        }
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();

    dispatch($log, 'dblclick', { target: rowByCmid(1002) });
    $input.value = 'повтор';
    dispatch($prompt, 'submit');
    await settle(20);

    ok(!bar().hidden, 'после ошибки полоса на месте — иначе [retry] ушёл бы без ответа');
    eq(R._debugReplyState().cmid, 1002, 'цель тоже');

    // Пользователь успевает перенацелить полосу — повтор обязан уйти по
    // ЗАФИКСИРОВАННОЙ цели, а не по текущей.
    dispatch($log, 'dblclick', { target: rowByCmid(1001) });
    eq(R._debugReplyState().cmid, 1001, 'цель перенацелена');

    const err = $log.children.find((n) => n.className.includes('vk-line-error'));
    const retry = err.children.find((n) => n.className === 'vk-retry-btn');
    const before = netCalls.length;
    dispatch(retry, 'click');
    await settle(20);

    const post = netCalls.slice(before).find((c) => c.method === 'POST');
    eq(post.body.reply_to_cmid, 1002,
        'повтор ушёл в ТО ЖЕ сообщение — цель зафиксирована замыканием, а не прочитана из полосы');
    eq(R._debugReplyState().cmid, 1001, 'а текущая цель повтором не тронута');
    eq(attempts, 2, 'ровно две попытки — автоповторов нет');
    C.cancelReply();
}

{
    // Явный null — «отправить БЕЗ ответа», даже если полоса что-то показывает.
    setNet(async (url, init) => {
        if (init && init.method === 'POST' && url.includes('/messages')) {
            return jsonRes(200, { message: message(7700, { out: true, cmid: 1700 }) });
        }
        if (url.includes('/messages')) {
            return jsonRes(200, { messages: [message(7001)], has_more: false });
        }
        return jsonRes(200, {});
    });
    await C.openDialog('2000000007', DIALOG);
    await settle();
    dispatch($log, 'dblclick', { target: rowByCmid(1001) });

    const before = netCalls.length;
    await C.send('без ответа', null);
    await settle(20);
    const post = netCalls.slice(before).find((c) => c.method === 'POST');
    eq(post.body.reply_to_cmid, undefined, 'явный null отправляет сообщение вне ветки');
    ok(!bar().hidden, 'и полосу не гасит — цель выбирал пользователь, а не отправка');
    C.cancelReply();
}

// ---------------------------------------------------------------------------
// 8. Автосброс цели: удаление оригинала, смена беседы, resetChat
// ---------------------------------------------------------------------------

{
    setNet(history([message(7001), message(7002)], false));
    await C.openDialog('2000000007', DIALOG);
    await settle();

    dispatch($log, 'dblclick', { target: rowByCmid(1002) });
    eq(R._debugReplyState().cmid, 1002, 'цель выбрана');
    C.onDeleted({ dialog_id: '2000000007', message_ids: [7002] });
    eq(R._debugReplyState().cmid, null,
        'оригинал удалён — цель снята (VK отверг бы такую отправку уже после Enter)');
    ok(bar().hidden, 'и полоса спрятана');

    // Удаление ДРУГОГО сообщения цель не трогает.
    dispatch($log, 'dblclick', { target: rowByCmid(1001) });
    C.onDeleted({ dialog_id: '2000000007', message_ids: [7777] });
    eq(R._debugReplyState().cmid, 1001, 'чужое удаление цель не сбрасывает');

    // Смена беседы.
    setNet(history([message(8001, { dialog_id: '1234', cmid: 2001 })], false));
    await C.openDialog('1234', { id: '1234', kind: 'user', title: 'Павел' });
    await settle();
    eq(R._debugReplyState().cmid, null, 'смена беседы сбрасывает цель');
    ok(bar().hidden, 'полоса спрятана вместе с ней');
    eq(C._debugChatState().flashCmid, null, 'и подсветка от прошлого перехода тоже');

    // resetChat (logout).
    dispatch($log, 'dblclick', { target: rowByCmid(2001) });
    eq(R._debugReplyState().cmid, 2001, 'цель в новой беседе');
    C.resetChat();
    eq(R._debugReplyState().cmid, null, 'logout снимает цель');
    ok(bar().hidden, 'и полосу');
}

// ---------------------------------------------------------------------------
// 9. XSS: превью цитаты — только текстом
// ---------------------------------------------------------------------------

{
    const evil = '<img src=x onerror=alert(1)>';
    const reply = message(7900, {
        reply_to: {
            message_id: 7001, cmid: 1001, sender: evil, sender_id: '1234',
            text: evil, media_kind: null, unavailable: false,
        },
    });
    setNet(history([message(7001), reply], false));
    await C.openDialog('2000000007', DIALOG);
    await settle();

    const quote = rowByCmid(1900).querySelector('.vk-reply-quote');
    eq(quote.querySelector('.vk-reply-quote-text').textContent, evil, 'разметка доехала как ТЕКСТ');
    ok(!quote.children.some((n) => n.tagName === 'img'), 'из данных собеседника не вырос <img>');

    // Полоса ответа подписывается автором ЦЕЛИ, поэтому разметку надо
    // подставить именно в его имя.
    C.onNewMessage(message(7901, { text: evil, sender: { id: '1234', screen_name: evil } }));
    dispatch($log, 'dblclick', { target: rowByCmid(1901) });
    eq(bar().querySelector('.vk-reply-bar-name').textContent, evil,
        'в полосе — тоже текст (имя автора приходит из VK)');
    eq(bar().querySelector('.vk-reply-bar-text').textContent, evil, 'и превью — текст');
    ok(!bar().children.some((n) => n.tagName === 'img'), 'и там разметка не выросла');
    C.cancelReply();
}

console.log(`vk_reply_ui: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
