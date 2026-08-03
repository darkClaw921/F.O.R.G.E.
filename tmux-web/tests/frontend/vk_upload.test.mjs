// Регресс-тест ОТПРАВКИ ФАЙЛОВ панели VK (js/vk/upload.js) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// Что здесь проверяется и почему именно это:
//
//   1. XHR ИДЁТ МИМО ПОДМЕНЁННОГО window.fetch, поэтому заголовок
//      `Authorization: Bearer` ставится ВРУЧНУЮ. Это конкретный исправленный
//      дефект (в remote-mode отправка файлов иначе работает только на
//      localhost), а не стилистика, — поэтому тест смотрит и на заголовки
//      XHR, и на то, что путь загрузки НЕ появился среди fetch-запросов.
//   2. НЕСКОЛЬКО ФАЙЛОВ = ОДНО СООБЩЕНИЕ. Главное отличие от Telegram: очередь
//      собирает `uploadIds[]` и отправляет их одним `sendMessage` (альбом VK —
//      три запроса вместо пятнадцати). Больше десяти вложений VK не примет,
//      поэтому пачки нарезаются по 10.
//   3. ДВА ЭТАПА ПРОГРЕССА: `LOCAL %` из `xhr.upload.onprogress` и `VK %` из
//      кадров WS `upload_progress`, которые у VK приходят ПОСЛЕ нажатия
//      «отправить» (конвейер живёт внутри POST /messages).
//   4. ОТКАЗ ПО ЛИМИТУ ТИПА: у фотографии предел жёстче, чем у документа, и
//      файл одного и того же размера в одной ветке отвергается, а в другой —
//      нет. Отказ происходит ДО сети.
//   5. ОТМЕНА: карточка снимается, XHR обрывается, остальная пачка уезжает.
//   6. ИМЯ ФАЙЛА С КИРИЛЛИЦЕЙ И ПРОБЕЛАМИ доезжает целым (percent-encoding).
//
// МУТАЦИОННАЯ ПРОВЕРКА: убрать ручной Authorization — краснеет раздел 1;
// отправлять каждый файл своим сообщением — раздел 2; читать лимит одним
// числом вместо карты по видам — раздел 4.
//
// Запуск: node tmux-web/tests/frontend/vk_upload.test.mjs

import {
    $log, $input, $panel, $attach, $fileInput, $dropzone,
    dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

// ---------------------------------------------------------------------------
// Окружение, которого нет в стабе: токен в localStorage и XMLHttpRequest.
// ---------------------------------------------------------------------------

const AUTH_TOKEN = 'TEST-BEARER-TOKEN';
globalThis.localStorage = {
    getItem: (k) => (k === 'forge.authToken' ? AUTH_TOKEN : null),
    setItem() {},
    removeItem() {},
};

/** Журнал заливок: по одному объекту на каждый созданный XHR. */
const xhrs = [];

/**
 * Минимальный XMLHttpRequest.
 *
 * Ответ не приходит сам: тест дёргает `respond()`/`fail()`/`abort()` вручную —
 * иначе очередь пролетала бы мимо промежуточных состояний, которые и
 * проверяются.
 */
globalThis.XMLHttpRequest = class FakeXhr {
    constructor() {
        this.headers = {};
        this.status = 0;
        this.responseText = '';
        this.upload = {};
        this.sent = null;
        this.aborted = false;
        xhrs.push(this);
    }

    open(method, url) {
        this.method = method;
        this.url = url;
    }

    setRequestHeader(k, v) {
        this.headers[k] = v;
    }

    send(body) {
        this.sent = body;
    }

    abort() {
        this.aborted = true;
        if (this.onabort) this.onabort();
    }

    /** Успешный ответ сервера. */
    respond(status, obj) {
        this.status = status;
        this.responseText = typeof obj === 'string' ? obj : JSON.stringify(obj);
        if (this.onload) this.onload();
    }

    /** Обрыв соединения. */
    fail() {
        if (this.onerror) this.onerror();
    }

    /** Кадр локального прогресса. */
    progress(loaded, total) {
        if (this.upload.onprogress) {
            this.upload.onprogress({ loaded, total, lengthComputable: true });
        }
    }
};

const U = await import(new URL('../../static/js/vk/upload.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);
const RE = await import(new URL('../../static/js/vk/reply.js', import.meta.url).href);
const F = await import(new URL('../../static/js/vk/format.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit); здесь он только
// заставлял бы тест ждать настоящие 380 мс.
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

/** «Файл» без FileReader: модулю нужны только name/size/type. */
const file = (name, size, type) => ({ name, size, type });

/** Ждёт появления N-го XHR (заливка идёт последовательно). */
async function waitXhr(n) {
    for (let i = 0; i < 50 && xhrs.length < n; i += 1) await settle(1);
    return xhrs[n - 1];
}

/** Карточки отправки, живые в ленте. */
const cards = () => $log.querySelectorAll('.vk-upload-card');

/** Системные строки ленты. */
const systemLines = () => $log.children.filter((n) => n.className.includes('vk-line-system'));

/** Ответ POST /messages с настоящим DTO. */
function messageDto(id) {
    return {
        message: {
            id,
            dialog_id: '1234',
            out: true,
            date: 1_800_000_000,
            text: '',
            sender: null,
            attachments: [],
            media: null,
            reply_to: null,
            reply_to_id: null,
            edit_date: null,
            cmid: id,
            fwd: [],
            action: null,
            payload: null,
        },
    };
}

/** Сбрасывает всё между сценариями. */
function reset() {
    U.teardownUpload();
    xhrs.length = 0;
    netCalls.length = 0;
    $log.childNodes.slice().forEach((n) => {
        if (n.className && String(n.className).includes('vk-log-spacer')) return;
        n.remove();
    });
    $input.value = '';
    RE.clearReplyTarget();
}

// Беседа выбрана: без неё модуль честно откажется отправлять.
S.vkState.activeDialogId = '1234';
S.vkState.dialogs = [{ id: '1234', kind: 'user', title: 'Тест' }];
setNet(async (url) => (url.includes('/messages') ? jsonRes(200, messageDto(9001)) : jsonRes(200, {})));

U.initUpload();

// ===========================================================================
// 1. XHR мимо fetch: Authorization ставится вручную
// ===========================================================================

{
    reset();
    U.sendFiles([file('фото отпуск.jpg', 4096, 'image/jpeg')]);
    const xhr = await waitXhr(1);

    eq(xhr.method, 'POST', 'XHR: метод');
    eq(xhr.url, '/api/vk/upload', 'XHR: путь загрузки — VK-шный');
    eq(U.UPLOAD_PATH, '/api/vk/upload', 'экспортируемый путь совпадает с csrf-исключением');
    eq(
        xhr.headers.Authorization,
        `Bearer ${AUTH_TOKEN}`,
        'XHR идёт МИМО подменённого fetch ⇒ Bearer ставится вручную',
    );
    eq(xhr.headers['Content-Type'], 'application/octet-stream', 'XHR: сырое тело');
    eq(
        xhr.headers['X-Forge-File-Name'],
        encodeURIComponent('фото отпуск.jpg'),
        'имя файла едет percent-encoded заголовком',
    );
    eq(
        decodeURIComponent(xhr.headers['X-Forge-File-Name']),
        'фото отпуск.jpg',
        'кириллица и пробелы доезжают целыми',
    );
    ok(xhr.sent && xhr.sent.name === 'фото отпуск.jpg', 'телом уходит сам файл');
    ok(
        !netCalls.some((c) => c.url.includes('/api/vk/upload')),
        'заливка не имеет права идти через fetch: у него нет прогресса отправки',
    );

    xhr.respond(200, { upload_id: 'u-1', file_name: 'фото отпуск.jpg', size: 4096, mime: 'image/jpeg' });
    await settle(20);

    const sends = netCalls.filter((c) => c.method === 'POST' && c.url.includes('/dialogs/1234/messages'));
    eq(sends.length, 1, 'одно сообщение на один файл');
    eq(sends[0].body.upload_ids, ['u-1'], 'upload_ids — родная форма VK');
    eq(cards().length, 0, 'после успеха карточка снимается');
}

// ===========================================================================
// 2. Несколько файлов = ОДНО сообщение (альбом)
// ===========================================================================

{
    reset();
    $input.value = 'подпись к альбому';
    const accepted = U.sendFiles([
        file('a.jpg', 1000, 'image/jpeg'),
        file('b.png', 2000, 'image/png'),
        file('c.jpg', 3000, 'image/jpeg'),
    ]);
    eq(accepted, 3, 'приняты все три файла');
    eq($input.value, '', 'подпись забрана из поля ввода');
    eq(cards().length, 3, 'по карточке на файл');

    for (let i = 1; i <= 3; i += 1) {
        const xhr = await waitXhr(i);
        eq(xhrs.length, i, `заливка ${i}: очередь строго последовательная`);
        xhr.respond(200, { upload_id: `u-${i}`, file_name: `f${i}`, size: 100, mime: 'image/jpeg' });
        await settle(5);
    }
    await settle(20);

    const sends = netCalls.filter((c) => c.method === 'POST' && c.url.includes('/dialogs/1234/messages'));
    eq(sends.length, 1, 'ТРИ файла уехали ОДНИМ сообщением, а не тремя');
    eq(sends[0].body.upload_ids, ['u-1', 'u-2', 'u-3'], 'порядок файлов — порядок пользователя');
    eq(sends[0].body.text, 'подпись к альбому', 'подпись едет одна на сообщение');
    ok(!('upload_id' in sends[0].body), 'односоставное поле не дублирует список');
    eq(cards().length, 0, 'карточки сняты');
}

// ===========================================================================
// 3. Больше десяти файлов: нарезка на пачки (= на сообщения)
// ===========================================================================

{
    reset();
    const many = [];
    for (let i = 0; i < 12; i += 1) many.push(file(`p${i}.jpg`, 100, 'image/jpeg'));
    U.sendFiles(many, { caption: 'первая' });
    eq(U.MAX_PER_MESSAGE, 10, 'потолок вложений совпадает с серверным');
    eq(U._debugUploadState().batches, 2, '12 файлов — это две пачки');

    for (let i = 1; i <= 12; i += 1) {
        const xhr = await waitXhr(i);
        xhr.respond(200, { upload_id: `m-${i}`, file_name: `p${i}`, size: 100, mime: 'image/jpeg' });
        await settle(3);
    }
    await settle(30);

    const sends = netCalls.filter((c) => c.method === 'POST' && c.url.includes('/dialogs/1234/messages'));
    eq(sends.length, 2, 'два сообщения: 10 + 2');
    eq(sends[0].body.upload_ids.length, 10, 'первое сообщение — десять вложений');
    eq(sends[1].body.upload_ids.length, 2, 'второе — остаток');
    eq(sends[0].body.text, 'первая', 'подпись только у первого сообщения');
    eq(sends[1].body.text, '', 'повторять подпись у каждой пачки — спам');
}

// ===========================================================================
// 4. Отказ ПО ЛИМИТУ ТИПА — до сети
// ===========================================================================

{
    reset();
    const big = 60 * 1000 * 1000; // между лимитом фото (50 МБ) и документа (200 МБ)
    ok(big > F.UPLOAD_LIMITS.photo && big < F.UPLOAD_LIMITS.doc, 'размер выбран между лимитами');

    const acceptedPhoto = U.sendFiles([file('huge.jpg', big, 'image/jpeg')]);
    eq(acceptedPhoto, 0, 'фотография сверх своего лимита отвергается');
    eq(xhrs.length, 0, 'отказ происходит ДО сети');
    ok(
        systemLines().some((n) => n.textContent.includes('huge.jpg')),
        'пользователю сказано, что именно не отправлено',
    );

    const acceptedDoc = U.sendFiles([file('huge.pdf', big, 'application/pdf')]);
    eq(acceptedDoc, 1, 'ТОТ ЖЕ размер документом проходит: лимит зависит от вида');
    const xhr = await waitXhr(1);
    xhr.respond(200, { upload_id: 'd-1', file_name: 'huge.pdf', size: big, mime: 'application/pdf' });
    await settle(20);

    // Пустой файл отвергается там же и по той же причине.
    reset();
    eq(U.sendFiles([file('empty.txt', 0, 'text/plain')]), 0, 'пустой файл — тоже отказ до сети');
    eq(xhrs.length, 0, 'и снова без сети');
}

// ===========================================================================
// 5. Прогресс: LOCAL из XHR, VK из кадров WS (после «отправить»)
// ===========================================================================

{
    reset();
    // Ответ на POST /messages задерживается вручную: конвейер VK живёт ВНУТРИ
    // этого запроса, и без задержки промежуточные состояния («VK 0%», кадры
    // upload_progress) пролетали бы мимо теста.
    let release = null;
    setNet(async (url) => {
        if (!url.includes('/dialogs/1234/messages')) return jsonRes(200, {});
        return new Promise((resolve) => { release = () => resolve(jsonRes(200, messageDto(9002))); });
    });

    U.sendFiles([file('clip.mp4', 1000, 'video/mp4')]);
    const xhr = await waitXhr(1);

    const status = () => {
        const node = $log.querySelector('.vk-media-status');
        return node ? node.textContent : null;
    };
    xhr.progress(470, 1000);
    eq(status(), 'LOCAL 47%', 'первая нога — браузер → devforge');

    xhr.respond(200, { upload_id: 'p-1', file_name: 'clip.mp4', size: 1000, mime: 'video/mp4' });
    for (let i = 0; i < 50 && !release; i += 1) await settle(1);
    ok(release, 'POST /messages ушёл');
    eq(status(), 'VK 0%', 'карточка не имеет права застыть на «LOCAL 100%»');

    // Кадр WS приходит ВО ВРЕМЯ POST /messages — это и есть отличие VK от
    // Telegram, и индикатор обязан его показывать, а не гаснуть.
    U.onUploadProgress({ upload_id: 'p-1', sent: 600, total: 1000 });
    eq(status(), 'VK 60%', 'вторая нога — devforge → VK, кадром upload_progress');

    // Чужой upload_id игнорируется: кадры вещаются всем вкладкам без фильтра.
    U.onUploadProgress({ upload_id: 'someone-else', sent: 1, total: 1000 });
    eq(status(), 'VK 60%', 'чужой кадр не двигает нашу карточку');

    release();
    await settle(20);
    eq(cards().length, 0, 'после подтверждения карточка снимается');
    // И кадр, пришедший ПОСЛЕ успеха, ничего не воскрешает.
    U.onUploadProgress({ upload_id: 'p-1', sent: 1000, total: 1000 });
    eq(cards().length, 0, 'запоздавший кадр не возвращает карточку');

    // Возвращаем обычный обработчик сети остальным сценариям.
    setNet(async (url) => (url.includes('/messages') ? jsonRes(200, messageDto(9001)) : jsonRes(200, {})));
}

// ===========================================================================
// 6. Отмена
// ===========================================================================

{
    reset();
    U.sendFiles([file('one.jpg', 100, 'image/jpeg'), file('two.jpg', 100, 'image/jpeg')]);
    await waitXhr(1);
    eq(cards().length, 2, 'две карточки');

    // Отменяем ВТОРОЙ файл (он ещё в очереди) кнопкой CANCEL его карточки.
    const cancelButtons = $log.querySelectorAll('.vk-upload-cancel');
    eq(cancelButtons.length, 2, 'кнопка отмены у каждой карточки');
    dispatch(cancelButtons[1], 'click', {});
    await settle(2);
    eq(cards().length, 1, 'карточка отменённого файла снята');
    ok(
        systemLines().some((n) => n.textContent.includes('two.jpg')),
        'отмена объявлена в ленте',
    );

    xhrs[0].respond(200, { upload_id: 'c-1', file_name: 'one.jpg', size: 100, mime: 'image/jpeg' });
    await settle(20);
    const sends = netCalls.filter((c) => c.method === 'POST' && c.url.includes('/dialogs/1234/messages'));
    eq(sends.length, 1, 'оставшийся файл всё равно уехал');
    eq(sends[0].body.upload_ids, ['c-1'], 'отменённый файл в сообщение не попал');

    // Отмена ЕДИНСТВЕННОГО файла отменяет и сообщение целиком.
    reset();
    U.sendFiles([file('solo.jpg', 100, 'image/jpeg')]);
    await waitXhr(1);
    dispatch($log.querySelectorAll('.vk-upload-cancel')[0], 'click', {});
    await settle(10);
    ok(xhrs[0].aborted, 'активный XHR обрывается, а не догружается впустую');
    eq(U._debugUploadState().batches, 0, 'пустая пачка снята с очереди');
    eq(
        netCalls.filter((c) => c.method === 'POST' && c.url.includes('/dialogs/1234/messages')).length,
        0,
        'отправлять нечего — сообщения нет',
    );
}

// ===========================================================================
// 7. Провал и повтор
// ===========================================================================

{
    reset();
    U.sendFiles([file('bad.jpg', 100, 'image/jpeg')]);
    const xhr = await waitXhr(1);
    xhr.respond(413, { error: { code: 'too_large', message: 'file is too big' } });
    await settle(10);

    eq(cards().length, 1, 'карточка провала остаётся на экране');
    eq($log.querySelector('.vk-media-status').textContent, '!! FAILED', 'состояние видно');
    ok(
        systemLines().some((n) => n.textContent.includes('file is too big')),
        'причина отказа доехала до пользователя',
    );
    const retry = $log.querySelector('.vk-media-retry');
    ok(retry, 'предложен повтор');

    // Повтор начинается С ПЕРВОГО этапа: upload_id на сервере уже потрачен.
    dispatch(retry, 'click', {});
    const second = await waitXhr(2);
    eq(second.url, '/api/vk/upload', 'повтор заливает файл заново');
    second.respond(200, { upload_id: 'r-1', file_name: 'bad.jpg', size: 100, mime: 'image/jpeg' });
    await settle(20);
    eq(cards().length, 0, 'после удавшегося повтора карточка снята');
}

// ===========================================================================
// 8. Ответ файлом: cmid уезжает в теле, полоса снимается
// ===========================================================================

{
    reset();
    RE.setReplyTarget({
        dialogId: '1234',
        cmid: 514,
        messageId: 7001,
        author: 'Тест',
        preview: 'оригинал',
        media: null,
    });
    U.sendFiles([file('answer.jpg', 100, 'image/jpeg')]);
    eq(RE.replyTargetCmid('1234'), null, 'полоса снята в момент постановки в очередь');

    const xhr = await waitXhr(1);
    xhr.respond(200, { upload_id: 'a-1', file_name: 'answer.jpg', size: 100, mime: 'image/jpeg' });
    await settle(20);

    const sends = netCalls.filter((c) => c.method === 'POST' && c.url.includes('/dialogs/1234/messages'));
    eq(sends.length, 1, 'одно сообщение');
    eq(sends[0].body.reply_to_cmid, 514, 'адрес ответа у VK — conversation_message_id');
    ok(!('reply_to_id' in sends[0].body), 'глобальный id при известном cmid не отправляется');
}

// ===========================================================================
// 9. Способы выбрать файл: кнопка, drag&drop, буфер
// ===========================================================================

{
    reset();
    // Кнопка ⇪ открывает скрытый input.
    let clicked = 0;
    $fileInput.click = () => { clicked += 1; };
    dispatch($attach, 'click', {});
    eq(clicked, 1, 'кнопка ⇪ открывает выбор файла');

    // Оверлей drag&drop по ВСЕЙ панели, со счётчиком вложенности.
    const dt = { types: ['Files'], files: [file('drop.jpg', 100, 'image/jpeg')] };
    dispatch($panel, 'dragenter', { dataTransfer: dt });
    eq($dropzone.hidden, false, 'оверлей показан');
    dispatch($panel, 'dragenter', { dataTransfer: dt });
    dispatch($panel, 'dragleave', { dataTransfer: dt });
    eq($dropzone.hidden, false, 'вложенный dragleave не гасит оверлей');
    dispatch($panel, 'dragleave', { dataTransfer: dt });
    eq($dropzone.hidden, true, 'последний dragleave гасит');

    const dragover = dispatch($panel, 'dragover', { dataTransfer: dt });
    ok(dragover.defaultPrevented, 'без preventDefault на dragover браузер откроет файл сам');

    dispatch($panel, 'drop', { dataTransfer: dt });
    await settle(2);
    eq($dropzone.hidden, true, 'после drop оверлей снят');
    eq(xhrs.length, 1, 'файл из drop поставлен в очередь');
    xhrs[0].respond(200, { upload_id: 'dd-1', file_name: 'drop.jpg', size: 100, mime: 'image/jpeg' });
    await settle(20);

    // Текстовый перенос модуль не трогает вовсе.
    reset();
    const textDt = { types: ['text/plain'], files: [] };
    const ev = dispatch($panel, 'drop', { dataTransfer: textDt });
    ok(!ev.defaultPrevented, 'перенос текста — не наше дело');
    eq(xhrs.length, 0, 'и ничего не заливается');

    // Ctrl+V со скриншотом: имя генерируется, текст в буфере не перехватывается.
    reset();
    const pasteEv = dispatch($input, 'paste', {
        clipboardData: { files: [file('image.png', 300, 'image/png')], items: [] },
    });
    ok(pasteEv.defaultPrevented, 'вставку файла перехватываем');
    const pasted = await waitXhr(1);
    const pastedName = decodeURIComponent(pasted.headers['X-Forge-File-Name']);
    ok(/^pasted_\d{8}_\d{6}\.png$/.test(pastedName), `имя скриншота из даты: ${pastedName}`);
    pasted.respond(200, { upload_id: 'v-1', file_name: pastedName, size: 300, mime: 'image/png' });
    await settle(20);

    reset();
    const textPaste = dispatch($input, 'paste', { clipboardData: { files: [], items: [] } });
    ok(!textPaste.defaultPrevented, 'обычный текст вставляется как текст');
    eq(xhrs.length, 0, 'и ничего не заливается');
}

// ===========================================================================
// 10. Краевые случаи
// ===========================================================================

{
    reset();
    // Беседа не выбрана — отправлять некуда, и это честная системная строка.
    const active = S.vkState.activeDialogId;
    S.vkState.activeDialogId = null;
    eq(U.sendFiles([file('a.jpg', 100, 'image/jpeg')]), 0, 'без беседы файл не уходит');
    ok(
        systemLines().some((n) => n.textContent.includes('no dialog selected')),
        'пользователю сказано почему',
    );
    S.vkState.activeDialogId = active;

    // Пустой список — не ошибка и не сеть.
    reset();
    eq(U.sendFiles([]), 0, 'пустой список');
    eq(U.sendFiles(null), 0, 'отсутствующий список');
    eq(xhrs.length, 0, 'сети не было');

    // Обрыв соединения на заливке.
    reset();
    U.sendFiles([file('drop.bin', 100, 'application/octet-stream')]);
    const xhr = await waitXhr(1);
    xhr.fail();
    await settle(10);
    eq($log.querySelector('.vk-media-status').textContent, '!! FAILED', 'обрыв — это провал');
    ok(
        systemLines().some((n) => n.textContent.includes('обрыв соединения')),
        'причина названа',
    );

    // teardown снимает всё, включая активные XHR.
    reset();
    U.sendFiles([file('x.jpg', 100, 'image/jpeg')]);
    await waitXhr(1);
    U.teardownUpload();
    ok(xhrs[0].aborted, 'teardown обрывает активную заливку');
    eq(U._debugUploadState().batches, 0, 'очередь пуста');
    eq(U._debugUploadState().tracked, 0, 'реестр upload_id пуст');
    eq(cards().length, 0, 'карточек не осталось');
}

// ===========================================================================
// 11. Чистая логика: вид карточки по ветке конвейера
// ===========================================================================

eq(F.uploadCardKind('photo'), 'photo', 'карточка: фото');
eq(F.uploadCardKind('video'), 'video', 'карточка: видео');
eq(F.uploadCardKind('doc'), 'document', 'карточка: документ');
eq(F.uploadCardKind('audio_message'), 'voice', 'карточка: голосовое');
eq(F.uploadCardKind(''), 'document', 'карточка: неизвестное — документ');
// Перевод нужен ровно затем, чтобы карточка не рисовалась «◌ DATA».
eq(F.mediaGlyph(F.uploadCardKind('doc')), '▤', 'глиф документа найден');
eq(F.mediaKindLabel(F.uploadCardKind('audio_message')), 'VOICE', 'подпись голосового найдена');

console.log(`\n${checks - failures}/${checks} ассертов прошло (vk_upload)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
