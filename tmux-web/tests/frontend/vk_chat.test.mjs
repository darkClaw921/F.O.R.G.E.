// Регресс-тест ленты панели VK (js/vk/chat.js) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ:
//
//   1. ОТПРАВКА. Optimistic-строка появляется мгновенно и заменяется реальным
//      DTO без дубля; ошибка сети убирает «в пути» строку и даёт рабочий
//      [retry] — висящая строка врала бы про отправку;
//   2. ДЕДУПЛИКАЦИЯ. Своё сообщение приходит ВТОРОЙ раз кадром `new_message`,
//      и в ленте обязана остаться одна строка;
//   3. БЫСТРОЕ ПЕРЕКЛЮЧЕНИЕ. Десять открытий подряд оставляют РОВНО ОДНУ
//      актуальную загрузку — у VK бюджет 3 rps, и очередь из десяти историй
//      это три секунды чужих данных на экране;
//   4. ПОДГРУЗКА ВВЕРХ не дублирует сообщения и держит якорь скролла;
//   5. XSS: текст сообщения попадает в DOM только через textContent.
//
// Запуск: node tmux-web/tests/frontend/vk_chat.test.mjs

import {
    $log, $input, $prompt, $sessionHead,
    dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit). Здесь он только
// мешал бы: каждый запрос ждал бы настоящие 380 мс, и тест измерял бы выдержку
// вместо поведения ленты. Тайминги — данные, ровно как на бэкенде.
R.vkLimits.minIntervalMs = 1;
R.vkLimits.burst = 100;

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

/** Строки сообщений (спейсер и системные строки не в счёт). */
const lines = () => $log.children.filter((n) => n.className.includes('vk-line')
    && !n.className.includes('vk-line-system')
    && !n.className.includes('vk-line-error'));

function message(id, extra) {
    return Object.assign({
        id,
        dialog_id: '1234',
        out: false,
        date: 1700000000 + id,
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

const DIALOG = { id: '1234', kind: 'user', title: 'Павел Дуров', username: 'durov' };
S.vkState.dialogs = [DIALOG];

const reads = [];
C.initChat({ onRead: (id, maxId) => reads.push([id, maxId]), presenceOf: () => ({ online: true }) });

// ---------------------------------------------------------------------------
// Открытие беседы.
// ---------------------------------------------------------------------------

setNet(async (url) => {
    if (url.includes('/messages')) {
        return jsonRes(200, { messages: [message(7001), message(7002)], has_more: true });
    }
    return jsonRes(200, {});
});

await C.openDialog('1234', DIALOG);
await settle();

eq(lines().length, 2, 'история нарисована');
eq(S.vkState.activeDialogId, '1234', 'беседа активна');
ok($log.children.some((n) => n.className.includes('vk-log-spacer')),
    'спейсер пережил очистку ленты');
ok($log.children.some((n) => n.className.includes('vk-date-sep')),
    'первая страница подписана датой');
ok($sessionHead.textContent.startsWith('SESSION ▸ Павел Дуров @durov'),
    `шапка: имя основным, ник приписью — ${$sessionHead.textContent}`);
ok($sessionHead.children.some((n) => n.className === 'vk-session-nick'),
    'ник в шапке — отдельный узел (его красит .vk-session-nick)');
ok($sessionHead.textContent.includes('⌁ 2 PACKETS'), 'счётчик сообщений в шапке');
eq(reads.at(-1), ['1234', 7002], 'открытие беседы помечает её прочитанной по максимальному id');

const first = lines()[0];
eq(first.dataset.msgId, '7001', 'id сообщения в data-атрибуте');
eq(first.dataset.cmid, '1001', 'cmid тоже — им адресуются ответы (Ф6)');
ok(first.textContent.includes('durov $ сообщение 7001'), `строка лога: ${first.textContent}`);
// Контейнер вложений заложен заранее (карточки — Ф7).
ok(first.querySelector('.vk-line-media') !== null, 'контейнер вложений в разметке есть');

// ---------------------------------------------------------------------------
// Быстрое переключение бесед: одна актуальная загрузка.
// ---------------------------------------------------------------------------

{
    const served = [];
    setNet(async (url) => {
        const m = /dialogs\/(-?\d+)\/messages/.exec(url);
        const id = m ? m[1] : '0';
        served.push(id);
        // Отвечаем сообщением, по тексту которого видно, чья это история.
        return jsonRes(200, {
            messages: [message(8000, { dialog_id: id, text: `история ${id}` })],
            has_more: false,
        });
    });

    const ids = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '10'];
    await Promise.all(ids.map((id) => C.openDialog(id, { id, kind: 'user', title: id })));
    await settle(50);

    eq(S.vkState.activeDialogId, '10', 'активна последняя беседа');
    const drawn = lines();
    eq(drawn.length, 1, 'в ленте ровно одна строка — чужие истории не дорисовались');
    ok(drawn[0].textContent.includes('история 10'),
        `нарисована история АКТИВНОЙ беседы: ${drawn[0].textContent}`);
}

// ---------------------------------------------------------------------------
// Подгрузка истории вверх.
// ---------------------------------------------------------------------------

{
    S.vkState.dialogs = [DIALOG];
    setNet(async (url) => {
        if (!url.includes('/messages')) return jsonRes(200, {});
        const before = /before_id=(\d+)/.exec(url);
        if (!before) {
            return jsonRes(200, { messages: [message(7005), message(7006)], has_more: true });
        }
        // Страница вверх: намеренно возвращаем и ПЕРЕСЕЧЕНИЕ (7005), чтобы
        // проверить дедупликацию prependMessages.
        return jsonRes(200, { messages: [message(7003), message(7004), message(7005)], has_more: false });
    });

    await C.openDialog('1234', DIALOG);
    await settle();
    eq(lines().length, 2, 'первая страница');

    $log.scrollHeight = 1000;
    $log.clientHeight = 200;
    $log.scrollTop = 0;
    const beforeCalls = netCalls.length;
    dispatch($log, 'scroll');
    await settle(20);

    const older = netCalls.slice(beforeCalls).find((c) => c.url.includes('before_id='));
    ok(older, 'подгрузка вверх запрошена');
    ok(older.url.includes('before_id=7005'),
        `курсор — минимальный message.id, а НЕ cmid: ${older.url}`);
    eq(lines().length, 4, 'страницы склеены без дубля 7005');
    eq(S.vkState.messages.get('1234').map((m) => m.id), [7003, 7004, 7005, 7006],
        'модель по возрастанию id, дубликат отброшен');
}

// ---------------------------------------------------------------------------
// Отправка: optimistic + reconcile.
// ---------------------------------------------------------------------------

{
    setNet(async (url, init) => {
        if (init && init.method === 'POST' && url.includes('/messages')) {
            return jsonRes(200, {
                message: message(7100, { out: true, text: 'привет', sender: { id: '999' } }),
            });
        }
        if (url.includes('/messages')) return jsonRes(200, { messages: [], has_more: false });
        return jsonRes(200, { ok: true });
    });

    await C.openDialog('1234', DIALOG);
    await settle();
    eq(lines().length, 0, 'пустая беседа');

    $input.value = 'привет';
    dispatch($prompt, 'submit');
    // Строка появляется СРАЗУ, до ответа сервера.
    const pending = lines();
    eq(pending.length, 1, 'optimistic-строка появилась мгновенно');
    ok(pending[0].className.includes('vk-msg-pending'), 'и помечена «в пути»');
    ok(pending[0].dataset.tempId, 'у неё временный id');
    eq($input.value, '', 'поле ввода очищено');

    await settle(20);
    const sent = lines();
    eq(sent.length, 1, 'строка одна — реальный DTO ЗАМЕНИЛ optimistic, а не добавился');
    eq(sent[0].dataset.msgId, '7100', 'у строки настоящий message_id');
    ok(!sent[0].className.includes('vk-msg-pending'), 'пометка «в пути» снята');
    eq(S.vkState.messages.get('1234').length, 1, 'сообщение в модели');

    // Кадр LongPoll с ТЕМ ЖЕ сообщением: дедупликация по id.
    eq(C.onNewMessage(message(7100, { out: true, text: 'привет' })), false,
        'повторное сообщение из WS не рисуется второй раз');
    eq(lines().length, 1, 'в ленте по-прежнему одна строка');

    const post = netCalls.filter((c) => c.method === 'POST' && c.url.includes('/messages'));
    eq(post.length, 1, 'ровно один POST');
    eq(post[0].body.text, 'привет', 'текст ушёл на сервер');
}

// ---------------------------------------------------------------------------
// Отправка: ошибка и повтор.
// ---------------------------------------------------------------------------

{
    let attempts = 0;
    setNet(async (url, init) => {
        if (init && init.method === 'POST' && url.includes('/messages')) {
            attempts += 1;
            if (attempts === 1) {
                return jsonRes(503, { error: { code: 'not_connected', message: 'нет связи' } });
            }
            return jsonRes(200, { message: message(7200, { out: true, text: 'повтор' }) });
        }
        if (url.includes('/messages')) return jsonRes(200, { messages: [], has_more: false });
        return jsonRes(200, { ok: true });
    });

    await C.openDialog('1234', DIALOG);
    await settle();
    $input.value = 'повтор';
    dispatch($prompt, 'submit');
    await settle(20);

    eq(lines().length, 0, 'строка «в пути» убрана — она врала бы про отправку');
    const err = $log.children.find((n) => n.className.includes('vk-line-error'));
    ok(err, 'показана строка ошибки');
    ok(err.textContent.includes('нет связи'), `текст ошибки от сервера: ${err.textContent}`);

    const retry = err.children.find((n) => n.className === 'vk-retry-btn');
    ok(retry, 'у ошибки есть [retry]');
    dispatch(retry, 'click');
    await settle(20);
    eq(lines().length, 1, 'повтор отправил сообщение');
    eq(lines()[0].dataset.msgId, '7200', 'и нарисовал реальный DTO');
    eq(attempts, 2, 'ровно две попытки — автоповторов нет');
}

// ---------------------------------------------------------------------------
// Realtime: входящее, правка, удаление.
// ---------------------------------------------------------------------------

{
    eq(C.onNewMessage(message(7300, { text: 'входящее' })), true, 'входящее дорисовано');
    eq(lines().length, 2, 'строка добавилась');
    eq(C.onNewMessage(message(7300)), false, 'дубль отброшен');
    // Сообщение из ДРУГОЙ беседы в открытую ленту не попадает.
    eq(C.onNewMessage(message(7301, { dialog_id: '-55' })), false, 'чужая беседа не рисуется');
    eq(lines().length, 2, 'лента не изменилась');

    C.onEdited(message(7300, { text: 'исправлено', edit_date: 1700000999 }));
    const edited = lines().find((n) => n.dataset.msgId === '7300');
    ok(edited.textContent.includes('исправлено'), 'текст обновлён');
    ok(edited.textContent.includes('(edited)'), 'приписка «(edited)»');

    C.onDeleted({ dialog_id: '1234', message_ids: [7300] });
    const purged = $log.children.find((n) => n.className.includes('vk-line-deleted'));
    ok(purged, 'удалённое заменено заглушкой');
    eq(purged.textContent, '<< message purged >>', 'текст заглушки');

    C.onRead({ dialog_id: '1234', direction: 'out', max_id: 7200 });
    const sys = $log.children.filter((n) => n.className.includes('vk-line-system'));
    ok(sys.some((n) => n.textContent.includes('peer read up to #7200')),
        'прочтение собеседником — системной строкой');
}

// ---------------------------------------------------------------------------
// «Печатает…»: троттлинг и отсутствие лишнего запроса на отмену.
// ---------------------------------------------------------------------------

{
    const before = netCalls.filter((c) => c.url.includes('/typing')).length;
    $input.value = 'а';
    dispatch($input, 'input');
    await settle();
    eq(netCalls.filter((c) => c.url.includes('/typing')).length, before + 1, 'первый typing ушёл');

    $input.value = 'аб';
    dispatch($input, 'input');
    await settle();
    eq(netCalls.filter((c) => c.url.includes('/typing')).length, before + 1,
        'второй в окне троттлинга не ушёл — бюджет 3 rps');

    // Очистка поля гасит статус ЛОКАЛЬНО: у messages.setActivity нет вида
    // «перестал печатать», и запрос на отмену был бы потрачен впустую.
    $input.value = '';
    dispatch($input, 'input');
    await settle();
    eq(netCalls.filter((c) => c.url.includes('/typing')).length, before + 1,
        'отмена набора В VK НЕ ХОДИТ');
}

// ---------------------------------------------------------------------------
// XSS и сброс.
// ---------------------------------------------------------------------------

{
    const evil = '<img src=x onerror=alert(1)>';
    C.onNewMessage(message(7400, { text: evil }));
    const row = lines().find((n) => n.dataset.msgId === '7400');
    ok(row.textContent.includes(evil), 'текст доехал как ТЕКСТ');
    // Разметка не превратилась в узлы: у строки ровно её собственные части.
    ok(!row.children.some((n) => n.tagName === 'img'), 'из текста не вырос <img>');

    C.resetChat();
    eq(lines().length, 0, 'лента очищена');
    eq($sessionHead.textContent, 'SESSION ▸ —', 'шапка сброшена');
    eq(C._debugChatState().rendered, 0, 'карта строк пуста');
}

console.log(`vk_chat: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
