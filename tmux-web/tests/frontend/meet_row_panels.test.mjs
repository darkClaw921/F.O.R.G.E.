// ⚠ СТРАЖ ПРИСУТСТВИЯ КНОПКИ ВСТРЕЧИ В ТРЁХ ЛЕНТАХ (фронтовая Ф3 forge-meet).
//
// Соседний messenger_meet_row.test.mjs проверяет САМ МОДУЛЬ на голом DOM.
// Здесь проверяется другое и не менее важное: что три панели действительно
// ЗОВУТ его — из своих билдеров строк, с правильным чатом и в правильный
// контейнер, — и что клавиатурная навигация находит кнопку.
//
// ПОЧЕМУ ТРИ ПРОЦЕССА. Панель резолвит DOM-ref'ы ОДИН раз при импорте
// (core/dom.js), поэтому в одном процессе может жить ровно один фейковый
// документ. Три панели — три процесса: файл перезапускает сам себя с именем
// панели аргументом. Тот же приём уже применён в telegram_toast_focus.
//
// ЧТО ЛОМАЕТСЯ ТИХО И ЛОВИТСЯ ЗДЕСЬ:
//   • панель перестала звать attachMeetButton (кнопки просто нет — но ни один
//     тест панели этого не заметит, у них своя тема);
//   • кнопка положена не в тот контейнер (в Telegram и VK — мимо обёртки, и
//     тогда она либо не видна, либо ломает верстку строки);
//   • ключ собран не из того чата (батч уйдёт за чужой диапазон id, и
//     состояние никогда не приедет);
//   • хоткей ищет кнопку по классу вместо data-action.
//
// Запуск: node tmux-web/tests/frontend/meet_row_panels.test.mjs

import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const PANEL = process.argv[2] || null;
const SELF = fileURLToPath(import.meta.url);
const MEET_URL = 'https://telemost.yandex.ru/j/60497752704486';

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

// ---------------------------------------------------------------------------
// Родитель: запускает три дочерних процесса
// ---------------------------------------------------------------------------

if (!PANEL) {
    let bad = 0;
    for (const panel of ['telegram', 'vk', 'bitrix']) {
        const res = spawnSync(process.execPath, [SELF, panel], { encoding: 'utf8' });
        const out = (res.stdout || '') + (res.stderr || '');
        const lines = out.split('\n').filter((l) => l.startsWith('[FAIL]') || l.startsWith('[OK]'));
        for (const l of lines) console.log(l);
        if (res.status !== 0) {
            bad += 1;
            if (lines.length === 0) console.error(out.trim());
        }
    }
    if (bad === 0) console.log('[OK] meet_row_panels: кнопка встречи есть во всех трёх лентах');
    else console.error(`[FAIL] meet_row_panels: панелей с провалами — ${bad}`);
    process.exit(bad ? 1 : 0);
}

// ---------------------------------------------------------------------------
// Дочерний процесс: одна панель
// ---------------------------------------------------------------------------

const STORE = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);
const ROW = await import(new URL('../../static/js/messenger/meet-row.js', import.meta.url).href);

/** Транспорт батча: живого сервера здесь нет и быть не должно. */
const batches = [];
STORE.configureMeetStore({
    fetchStates: async (q) => { batches.push(q); return { badges: [] }; },
    batchDelayMs: 5,
});

let clicked = 0;
ROW.configureMeetRow({ open: () => { clicked += 1; } });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Кнопка встречи в строке — ИМЕННО по data-action, как её ищет клавиатура. */
function meetBtn(row) {
    return Array.from(row.querySelectorAll('button')).find(
        (b) => b.dataset && b.dataset.action === 'meet',
    ) || null;
}

/**
 * Комбинация Shift+M доезжает до действия `msg.meet`.
 *
 * Проверяется НЕ таблица дефолтов, а весь путь события: `keyFromEvent` берёт
 * `event.code` (русская раскладка не должна ломать хоткей), модификаторы
 * складываются в канонический вид, и уже он ищется в карте. Ошибка на любом
 * шаге даёт молчаливо неработающую клавишу.
 */
function checkCombo(keys) {
    const resolved = keys.resolveBindings(null);
    eq(resolved.conflicts, [], 'Shift+M ни с чем не конфликтует');
    eq(keys.actionForEvent({ code: 'KeyM', key: 'M', shiftKey: true }, resolved, {}), 'msg.meet',
        'Shift+M опознан как действие «карточка встречи»');
    eq(keys.actionForEvent({ code: 'KeyM', key: 'ь', shiftKey: true }, resolved, {}), 'msg.meet',
        'русская раскладка хоткей не ломает — сопоставление идёт по event.code');
    const plain = keys.actionForEvent({ code: 'KeyM', key: 'm', shiftKey: false }, resolved, {});
    ok(plain !== 'msg.meet', 'одиночная `m` карточку встречи НЕ открывает');
    eq(keys.actionForEvent({ code: 'KeyM', key: 'M', shiftKey: true }, resolved, { editing: true }), null,
        'в поле ввода Shift+M — обычная заглавная буква, а не хоткей');
}

if (PANEL === 'telegram') {
    const stub = await import('./telegram_dom_stub.mjs');
    // Лента и поле ввода в общем стабе не объявлены (он писался под колонку и
    // попап), а chat.js без них не нарисует ни строки.
    for (const id of ['tg-log', 'tg-prompt', 'tg-input', 'tg-send', 'tg-lightbox']) {
        if (!stub.els.get(id)) {
            const n = stub.makeEl(id === 'tg-input' ? 'textarea' : 'div');
            n.attributes.id = id;
            stub.els.set(id, n);
        }
    }
    const chat = await import('../../static/js/telegram/chat.js');
    const { tgState } = await import('../../static/js/telegram/state.js');
    const keynav = await import('../../static/js/telegram/keynav.js');

    const dialog = { id: '777000', title: 'Sofia', kind: 'user' };
    tgState.dialogs = [dialog];
    tgState.activeDialogId = '777000';
    const msgs = [
        { id: 7, dialog_id: '777000', out: false, date: 1700000000, text: `созвон ${MEET_URL}`, sender: { id: '4242', username: 'sofia' }, media: null },
        { id: 8, dialog_id: '777000', out: false, date: 1700000100, text: 'без ссылки', sender: { id: '4242', username: 'sofia' }, media: null },
    ];
    tgState.messages.set('777000', msgs);
    chat.renderHistory(msgs, dialog);

    const log = stub.els.get('tg-log');
    const rows = log.children.filter((n) => n.className.includes('tg-line'));
    const withMeet = rows.find((r) => r.dataset.msgId === '7');
    const without = rows.find((r) => r.dataset.msgId === '8');
    const btn = meetBtn(withMeet);
    ok(!!btn, 'telegram: кнопка встречи появилась на строке со ссылкой');
    eq(meetBtn(without), null, 'telegram: на строке без ссылки кнопки нет');
    eq(btn.dataset.meetKey, 'telegram|777000|7', 'telegram: ключ собран из чата и id сообщения');
    ok(btn.parentNode.className === 'tg-line-actions',
        'telegram: кнопка положена в обёртку действий, а не прямо в строку');
    ok(withMeet._tg && withMeet._tg.msg,
        'telegram: строка сохранила `_tg` — иначе messageRows() выбросил бы её из vim-навигации');
    // Кнопка ответа обязана остаться на месте и остаться последней.
    const reply = withMeet.querySelector('.tg-reply-btn');
    ok(!!reply && reply.parentNode === btn.parentNode, 'telegram: кнопка ответа рядом, в той же обёртке');
    ok(btn.parentNode.children.indexOf(btn) < btn.parentNode.children.indexOf(reply),
        'telegram: встреча стоит ПЕРЕД ответом — ответ остался у правого края');

    await sleep(30);
    eq(batches.length, 1, 'telegram: промахи ушли ОДНИМ батчем на чат, а не по запросу на строку');
    eq(batches[0].plugin, 'telegram', 'telegram: батч запрошен за свою панель');
    eq(String(batches[0].chat_id), '777000', 'telegram: и за свой чат');

    // Хоткей. Курсор ставится ПУБЛИЧНЫМИ действиями: `focus.log` встаёт на
    // последнюю строку (она без ссылки), `nav.prev` поднимает на строку со
    // ссылкой. Так проверяется ровно тот путь, которым ходит пользователь.
    keynav.runAction('focus.log');
    eq(keynav.runAction('msg.meet'), false,
        'telegram: на строке без встречи хоткей возвращает false — событие не поглощается');
    eq(clicked, 0, 'telegram: и ничего не нажимает');
    keynav.runAction('nav.prev');
    eq(keynav.runAction('msg.meet'), true, 'telegram: хоткей нашёл кнопку и нажал её');
    eq(clicked, 1, 'telegram: обработчик кнопки вызван ровно один раз');
    checkCombo(await import('../../static/js/telegram/keys.js'));

    // Плагин выключен (запуск без `--meet`): кнопок нет и клавиша свободна.
    // Проверяется в ЭТОМ же процессе, потому что панель с её документом здесь
    // одна: гасим стор и пересобираем ленту.
    STORE.resetMeetStore();
    chat.renderHistory(msgs, dialog);
    const offRows = stub.els.get('tg-log').children.filter((n) => n.className.includes('tg-line'));
    eq(offRows.filter((r) => meetBtn(r)).length, 0,
        'telegram: при выключенном плагине кнопок встречи в ленте нет ни одной');
    keynav.runAction('focus.log');
    keynav.runAction('nav.prev');
    eq(keynav.runAction('msg.meet'), false,
        'telegram: при выключенном плагине хоткей возвращает false — Shift+M остаётся свободной');
    eq(clicked, 1, 'telegram: и ничего не нажимает');
} else if (PANEL === 'vk') {
    const stub = await import('./vk_dom_stub.mjs');
    const C = await import('../../static/js/vk/chat.js');
    const S = await import('../../static/js/vk/state.js');
    const keynav = await import('../../static/js/vk/keynav.js');

    const dialog = { id: '2000000001', title: 'Беседа', kind: 'chat' };
    S.vkState.dialogs = [dialog];
    S.vkState.activeDialogId = '2000000001';
    const msgs = [
        { id: 5001, cmid: 1001, dialog_id: '2000000001', out: false, date: 1700000000, text: `созвон ${MEET_URL}`, sender: { id: '1', name: 'durov' }, attachments: [] },
        { id: 5002, cmid: 1002, dialog_id: '2000000001', out: false, date: 1700000100, text: 'без ссылки', sender: { id: '1', name: 'durov' }, attachments: [] },
    ];
    S.vkState.messages.set('2000000001', msgs);
    C.renderHistory(msgs, dialog);

    const rows = stub.$log.children.filter((n) => String(n.className).includes('vk-line'));
    const withMeet = rows.find((r) => r.dataset.msgId === '5001');
    const without = rows.find((r) => r.dataset.msgId === '5002');
    const btn = meetBtn(withMeet);
    ok(!!btn, 'vk: кнопка встречи появилась на строке со ссылкой');
    eq(meetBtn(without), null, 'vk: на строке без ссылки кнопки нет');
    eq(btn.dataset.meetKey, 'vk|2000000001|5001',
        'vk: ключ собран из id СООБЩЕНИЯ (не cmid) — им же адресуется диапазон батча');
    ok(btn.parentNode.className === 'vk-line-actions',
        'vk: кнопка положена в обёртку действий, а не прямо в строку');
    ok(withMeet._vk && withMeet._vk.msg,
        'vk: строка сохранила `_vk` — иначе messageRows() выбросил бы её из vim-навигации');
    const reply = withMeet.querySelector('.vk-reply-btn');
    ok(!!reply && reply.parentNode === btn.parentNode, 'vk: кнопка ответа рядом, в той же обёртке');

    await sleep(30);
    eq(batches.length, 1, 'vk: промахи ушли ОДНИМ батчем на чат');
    eq(batches[0].plugin, 'vk', 'vk: батч запрошен за свою панель');

    keynav.runAction('focus.log');
    eq(keynav.runAction('msg.meet'), false, 'vk: на строке без встречи хоткей возвращает false');
    eq(clicked, 0, 'vk: и ничего не нажимает');
    keynav.runAction('nav.prev');
    eq(keynav.runAction('msg.meet'), true, 'vk: хоткей нашёл кнопку и нажал её');
    eq(clicked, 1, 'vk: обработчик кнопки вызван ровно один раз');
    checkCombo(await import('../../static/js/vk/keys.js'));

    STORE.resetMeetStore();
    C.renderHistory(msgs, dialog);
    const offRows = stub.$log.children.filter((n) => String(n.className).includes('vk-line'));
    eq(offRows.filter((r) => meetBtn(r)).length, 0,
        'vk: при выключенном плагине кнопок встречи в ленте нет ни одной');
    keynav.runAction('focus.log');
    keynav.runAction('nav.prev');
    eq(keynav.runAction('msg.meet'), false,
        'vk: при выключенном плагине хоткей возвращает false — Shift+M остаётся свободной');
} else if (PANEL === 'bitrix') {
    const stub = await import('./bitrix_dom_stub.mjs');
    const chat = await import('../../static/js/bitrix/chat.js');
    const keynav = await import('../../static/js/bitrix/keynav.js');

    const mk = (id, text, segments) => ({
        id,
        dialog_id: 'chat55',
        out: false,
        date: '2026-08-15T01:13:20+03:00',
        sender: { id: '15', name: 'Максим Крылов', color: '#3e99ce', bot: false },
        system: false,
        text,
        segments,
        media: [], cards: [], reply_markup: null, edited: false, likes: [],
        action: null, unread: false, reply_to: null, fwd: null,
    });
    const msgs = [
        // Ссылка приезжает СЕГМЕНТОМ `url` — так портал оборачивает голый адрес
        // при включённом превью (см. meet-core.js, три формы ссылки Битрикса).
        mk(900, `созвон ${MEET_URL}`, [
            { kind: 'text', text: 'созвон ', value: null, children: [] },
            { kind: 'url', text: MEET_URL, value: MEET_URL, children: [] },
        ]),
        mk(901, 'без ссылки', [{ kind: 'text', text: 'без ссылки', value: null, children: [] }]),
    ];
    stub.setNet(async () => stub.jsonRes(200, { messages: msgs, has_more: false }));
    chat.initChat({});
    await chat.openDialog('chat55', { id: 'chat55', title: 'Чат', kind: 'chat', can_write: true });
    await stub.flush(8);

    const rows = stub.$log.childNodes.filter((n) => String(n.className || '') === 'b24-line');
    const withMeet = rows.find((r) => r.dataset.msgId === '900');
    const without = rows.find((r) => r.dataset.msgId === '901');
    const btn = meetBtn(withMeet);
    ok(!!btn, 'bitrix: кнопка встречи появилась на строке со ссылкой');
    eq(meetBtn(without), null, 'bitrix: на строке без ссылки кнопки нет');
    eq(btn.dataset.meetKey, 'bitrix|chat55|900', 'bitrix: ключ собран из чата и id сообщения');
    ok(btn.parentNode.className === 'b24-line-actions',
        'bitrix: кнопка положена в СУЩЕСТВУЮЩИЙ контейнер действий — обёртка здесь не понадобилась');
    const kept = Array.from(withMeet.querySelectorAll('button'))
        .map((b) => b.dataset.action).filter(Boolean);
    eq(kept, ['like', 'reply', 'meet'],
        'bitrix: прежние действия строки на месте, встреча добавлена последней');

    await sleep(30);
    eq(batches.length, 1, 'bitrix: промахи ушли ОДНИМ батчем на чат');
    eq(String(batches[0].chat_id), 'chat55', 'bitrix: чат портала уезжает строкой, как он и выглядит');

    // Хоткей у Битрикса не потребовал НИ ОДНОЙ новой строки поиска: готовый
    // rowActionButton(row, 'meet') находит кнопку по data-action.
    keynav.runAction('focus.log');
    eq(keynav.runAction('msg.meet'), false,
        'bitrix: на строке без встречи хоткей возвращает false — сообщений без ссылки '
        + 'большинство, и уведомление на каждое нажатие было бы шумом');
    eq(clicked, 0, 'bitrix: и ничего не нажимает');
    keynav.runAction('nav.prev');
    eq(keynav.runAction('msg.meet'), true, 'bitrix: хоткей нажал кнопку через rowActionButton');
    eq(clicked, 1, 'bitrix: обработчик кнопки вызван ровно один раз');
    checkCombo(await import('../../static/js/bitrix/keys.js'));

    STORE.resetMeetStore();
    chat.renderHistory(msgs);
    const offRows = stub.$log.childNodes.filter((n) => String(n.className || '') === 'b24-line');
    eq(offRows.filter((r) => meetBtn(r)).length, 0,
        'bitrix: при выключенном плагине кнопок встречи в ленте нет ни одной');
    keynav.runAction('focus.log');
    keynav.runAction('nav.prev');
    eq(keynav.runAction('msg.meet'), false,
        'bitrix: при выключенном плагине хоткей возвращает false — Shift+M остаётся свободной');
}

if (failures === 0) console.log(`[OK] meet_row_panels/${PANEL}: ${checks} проверок`);
else console.error(`[FAIL] meet_row_panels/${PANEL}: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
