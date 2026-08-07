// Регресс-тест ОТПРАВКИ ФАЙЛОВ панели «Битрикс24» (Ф9) на фейковом DOM.
//
// Грузит РЕАЛЬНЫЕ модули `js/bitrix/{upload,chat,format,api,state}.js` поверх
// bitrix_dom_stub.mjs. Стаб обязан импортироваться ПЕРВЫМ: `core/dom.js`
// резолвит все `$b24*` один раз на верхнем уровне модуля.
//
// Что покрыто:
//   • ЧИСТЫЕ ФУНКЦИИ format.js: MIME по расширению, вид карточки, имя файла из
//     буфера, подписи этапов и очереди, отказ по размеру;
//   • ФАЗА `portal` НЕ ПОКАЗЫВАЕТ ПРОЦЕНТОВ (Р8) — ни в подписи, ни в полосе;
//   • ЛЕСТНИЦА ОТПРАВКИ: XHR на /api/bitrix/upload с percent-encoded именем и
//     Bearer вручную (XHR идёт мимо подменённого fetch), затем POST
//     /messages с `upload_id`;
//   • КЛИЕНТСКАЯ ПРОВЕРКА РАЗМЕРА: файл больше `max_upload_bytes` не порождает
//     НИ ОДНОГО запроса — 30 МБ ради 413 по сети не гоняются;
//   • ОТМЕНА: на фазе приёма рвёт XHR, на фазе портала кнопки уже нет
//     (`upload_id` потреблён сервером, прерывать нечем);
//   • ПОВТОР — это НОВАЯ загрузка с первого этапа: `upload_id` одноразовый
//     (Р9), и повторное использование дало бы 409;
//   • ОЧЕРЕДЬ строго последовательная: второй файл ждёт первого;
//   • Ctrl+V перехватывается ТОЛЬКО когда в буфере файл;
//   • drag&drop поднимает и опускает оверлей по вложенности;
//   • teardown отпускает objectURL'ы миниатюр и рвёт летящие запросы.
//
// Запуск: node tmux-web/tests/frontend/bitrix_upload.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { dispatch, els, flush, jsonRes, netCalls, setNet } from './bitrix_dom_stub.mjs';

// ---------------------------------------------------------------------------
// Окружение, которого нет в Node: токен, objectURL, XHR, File.
// ---------------------------------------------------------------------------

// Токен читается getAuthToken() в момент отправки, поэтому подменяем ДО
// импорта модулей — иначе заголовок Authorization проверить нечем.
globalThis.localStorage = { getItem: () => 'TESTTOKEN', setItem() {}, removeItem() {} };

const urls = { created: 0, revoked: [], live: new Set() };
globalThis.URL.createObjectURL = () => {
    urls.created += 1;
    const u = `blob:b24-up/${urls.created}`;
    urls.live.add(u);
    return u;
};
globalThis.URL.revokeObjectURL = (u) => {
    urls.revoked.push(u);
    urls.live.delete(u);
};

/** Все созданные XHR по порядку — журнал заливок. */
const xhrs = [];

class FakeXhr {
    constructor() {
        this.headers = {};
        this.status = 0;
        this.responseText = '';
        this.aborted = false;
        this.sent = null;
        this.upload = { onprogress: null };
        xhrs.push(this);
    }

    open(method, url) {
        this.method = method;
        this.url = url;
    }

    setRequestHeader(k, v) {
        this.headers[String(k).toLowerCase()] = String(v);
    }

    send(body) {
        this.sent = body;
    }

    abort() {
        this.aborted = true;
        if (this.onabort) this.onabort();
    }

    /** Ответ сервера — как если бы он пришёл. */
    respond(status, obj) {
        this.status = status;
        this.responseText = typeof obj === 'string' ? obj : JSON.stringify(obj);
        if (this.onload) this.onload();
    }

    /** Кадр прогресса отправки браузером. */
    progress(loaded, total) {
        if (this.upload.onprogress) {
            this.upload.onprogress({ lengthComputable: true, loaded, total });
        }
    }
}
globalThis.XMLHttpRequest = FakeXhr;

/** Минимальный File: конструктора File в Node нет. */
function fakeFile(name, size, type) {
    return { name, size, type, __blob: true };
}

const { b24State } = await import('../../static/js/bitrix/state.js');
const chat = await import('../../static/js/bitrix/chat.js');
const fmt = await import('../../static/js/bitrix/format.js');
const upload = await import('../../static/js/bitrix/upload.js');

const $uploads = els.get('b24-uploads');
const $dropzone = els.get('b24-dropzone');
const $input = els.get('b24-input');
const $attach = els.get('b24-attach');
const $fileInput = els.get('b24-file-input');
const $panel = els.get('bitrix');

// ---------------------------------------------------------------------------
// Ассерты
// ---------------------------------------------------------------------------

let failures = 0;
let checks = 0;

function ok(name, cond, detail) {
    checks += 1;
    if (cond) {
        console.log(`[ok]   ${name}`);
        return;
    }
    failures += 1;
    console.log(`[FAIL] ${name}${detail ? `\n       ${detail}` : ''}`);
}

function eq(name, actual, expected) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    ok(name, a === e, `actual: ${a}\n       expected: ${e}`);
}

/** Текст всех статусов очереди в порядке карточек. */
function statuses() {
    return $uploads.children.map((row) => {
        const foot = row.children.find((c) => c.className.includes('b24-upload-foot'));
        if (!foot) return '';
        const st = foot.children.find((c) => c.className.includes('b24-upload-status'));
        return st ? st.textContent : '';
    });
}

/** Узел полосы прогресса первой карточки. */
function firstProgress() {
    const row = $uploads.children[0];
    if (!row) return null;
    const foot = row.children.find((c) => c.className.includes('b24-upload-foot'));
    return foot ? foot.children.find((c) => c.className.includes('b24-upload-progress')) : null;
}

/** Кнопки первой карточки по классу. */
function firstButtons() {
    const row = $uploads.children[0];
    if (!row) return [];
    const out = [];
    for (const child of row.children) {
        for (const c of child.children || []) {
            if (c.tagName === 'button') out.push(c);
        }
    }
    return out;
}

function resetAll() {
    upload.teardownUpload();
    xhrs.length = 0;
    netCalls.length = 0;
}

// ===========================================================================
// 1. Чистые функции format.js
// ===========================================================================

eq('MIME берётся у браузера, если он его назвал', fmt.uploadMime('a.bin', 'image/png'), 'image/png');
eq('MIME догадывается по расширению, когда браузер молчит', fmt.uploadMime('DSC1.JPG', ''), 'image/jpeg');
eq('незнакомое расширение — октет-стрим', fmt.uploadMime('archive.qqq', ''), 'application/octet-stream');

eq('картинка — карточка вида image', fmt.uploadKind('image/png'), 'image');
eq('видео — video', fmt.uploadKind('video/mp4'), 'video');
eq('аудио — audio', fmt.uploadKind('audio/ogg'), 'audio');
eq('pdf — файл', fmt.uploadKind('application/pdf'), 'file');
ok(
    'svg отправляется файлом, а не картинкой',
    fmt.uploadKind('image/svg+xml') === 'file',
    'портал хранит svg документом; «картинка, которая не открывается лайтбоксом» читается как поломка',
);

const pasted = fmt.pastedFileName('image/png', new Date(2026, 7, 5, 9, 3, 7));
eq('имя файла из буфера содержит дату и время', pasted, 'pasted_20260805_090307.png');
ok('имя из буфера получает расширение по MIME', fmt.pastedFileName('image/webp').endsWith('.webp'));

eq('подпись приёма содержит точный процент', fmt.uploadStageLabel('client', 50, 200), 'ПРИЁМ 25%');
ok(
    'подпись фазы portal НЕ содержит ни одной цифры (Р8)',
    !/\d/.test(fmt.uploadStageLabel('portal', 100, 100)),
    `подпись: ${fmt.uploadStageLabel('portal', 100, 100)} — процентов у одношаговой заливки не существует`,
);
eq('очередь из одного не нумеруется', fmt.uploadQueueLabel(1, 1), 'В ОЧЕРЕДИ');
eq('очередь из нескольких показывает позицию', fmt.uploadQueueLabel(2, 3), 'В ОЧЕРЕДИ 2/3');

ok('пустой файл отвергается до сети', fmt.uploadRejectReason(0, 1024) !== null);
ok('файл в пределах лимита проходит', fmt.uploadRejectReason(1024, 2048) === null);
ok('файл больше лимита отвергается', fmt.uploadRejectReason(4096, 2048) !== null);
ok(
    'неизвестный лимит не отсекает ничего',
    fmt.uploadRejectReason(4096, 0) === null,
    'сервер ещё не ответил — решение принимает он сам (413)',
);

// ===========================================================================
// 2. Полный цикл отправки одного файла
// ===========================================================================

b24State.maxUploadBytes = 25 * 1024 * 1024;
resetAll();

const items = upload.sendFiles([fakeFile('отчёт.pdf', 2048, 'application/pdf')], {
    dialogId: 'chat27',
    caption: 'вот файл',
});
ok('файл принят в очередь', items.length === 1);
ok('контейнер очереди показан', $uploads.hidden === false);
ok('карточка построена', $uploads.children.length === 1);
await flush();

ok('заливка пошла ровно одним XHR', xhrs.length === 1, `xhr: ${xhrs.length}`);
const xhr = xhrs[0];
eq('адрес приёма совпадает с контрактом', xhr.url, '/api/bitrix/upload');
eq('метод — POST', xhr.method, 'POST');
eq('тело едет октет-стримом', xhr.headers['content-type'], 'application/octet-stream');
eq(
    'имя файла percent-encoded (заголовки HTTP — ASCII)',
    xhr.headers['x-forge-file-name'],
    encodeURIComponent('отчёт.pdf'),
);
eq(
    'Bearer поставлен ВРУЧНУЮ: XHR идёт мимо подменённого fetch',
    xhr.headers.authorization,
    'Bearer TESTTOKEN',
);
ok('в теле — сам файл, а не JSON', xhr.sent && xhr.sent.name === 'отчёт.pdf');
eq('ни одного fetch до конца приёма', netCalls.length, 0);

xhr.progress(1024, 2048);
eq('прогресс приёма — точные проценты', statuses(), ['ПРИЁМ 50%']);

// Ответ приёма: сервер выдал upload_id.
setNet(async () => jsonRes(200, {
    id: 777,
    message: { id: 777, dialog_id: 'chat27', text: 'вот файл', segments: [], media: [] },
    echoed: true,
}));
xhr.respond(200, {
    upload_id: 'u-1', file_name: 'отчёт.pdf', size: 2048, mime: 'application/pdf',
});
await flush(10);

ok('после приёма ушёл ровно один запрос отправки', netCalls.length === 1, JSON.stringify(netCalls));
const send = netCalls[0];
eq('отправка идёт тем же роутом, что и текст', send.url, '/api/bitrix/dialogs/chat27/messages');
eq('в теле — upload_id', send.body.upload_id, 'u-1');
eq('подпись уехала полем text', send.body.text, 'вот файл');
ok('карточка снята после успеха', $uploads.children.length === 0);
ok('контейнер спрятан, когда очередь пуста', $uploads.hidden === true);
eq('очередь пуста', upload._debugUploadState().queued, 0);

// ===========================================================================
// 3. Фаза portal: кадр WS, отсутствие процентов, недоступность отмены
// ===========================================================================

resetAll();
upload.sendFiles([fakeFile('big.bin', 4096, 'application/octet-stream')], { dialogId: 'chat27' });
await flush();

// Отправку подвешиваем: ответ придёт только после проверок фазы portal.
let releaseSend = null;
setNet(() => new Promise((resolve) => {
    releaseSend = () => resolve(jsonRes(200, { id: 5, message: { id: 5, dialog_id: 'chat27', segments: [], media: [] } }));
}));
xhrs[0].respond(200, { upload_id: 'u-2', file_name: 'big.bin', size: 4096, mime: 'application/octet-stream' });
await flush();

eq('этап сменился на portal сразу, не дожидаясь кадра', statuses(), ['ПОРТАЛ …']);
ok(
    'полоса бежит, а не показывает процент',
    firstProgress().className.includes('b24-upload-indeterminate'),
);
const cancelBtn = firstButtons().find((b) => b.className.includes('b24-upload-cancel'));
ok(
    'кнопка отмены скрыта на фазе portal',
    cancelBtn && cancelBtn.hidden === true,
    'upload_id уже потреблён сервером — прерывать нечем, а неработающая кнопка хуже отсутствующей',
);

// Кадр WS с чужим upload_id не должен трогать карточку.
upload.onUploadProgress({ upload_id: 'ЧУЖОЙ', phase: 'portal', sent: 1, total: 4096, indeterminate: true });
eq('чужой кадр прогресса игнорируется', statuses(), ['ПОРТАЛ …']);
// Кадр фазы client не применяется: свой прогресс приёма вкладка знает точнее.
upload.onUploadProgress({ upload_id: 'u-2', phase: 'client', sent: 1, total: 4096, indeterminate: false });
eq('кадр фазы client не откатывает карточку назад', statuses(), ['ПОРТАЛ …']);

releaseSend();
await flush(10);
ok('после ответа портала карточка снята', $uploads.children.length === 0);

// ===========================================================================
// 4. Клиентская проверка размера: НИ ОДНОГО запроса
// ===========================================================================

resetAll();
b24State.maxUploadBytes = 1024;
upload.sendFiles([fakeFile('huge.iso', 5 * 1024 * 1024, 'application/octet-stream')], {
    dialogId: 'chat27',
});
await flush();
eq('слишком большой файл не порождает XHR', xhrs.length, 0);
eq('и не порождает fetch', netCalls.length, 0);
ok('карточка показывает отказ', statuses()[0] === 'НЕ ОТПРАВЛЕН');
const rejectRow = $uploads.children[0];
const rejectErr = rejectRow.children.find((c) => c.className.includes('b24-upload-error'));
ok('причина отказа видна текстом', rejectErr && rejectErr.textContent.includes('лимит'));
ok(
    'повтора у отказа по размеру нет: он ничего не починит',
    !firstButtons().some((b) => b.className.includes('b24-upload-retry')),
);
b24State.maxUploadBytes = 25 * 1024 * 1024;

// ===========================================================================
// 5. Отмена на фазе приёма рвёт XHR
// ===========================================================================

resetAll();
const [cancelItem] = upload.sendFiles([fakeFile('a.png', 2048, 'image/png')], { dialogId: 'chat27' });
await flush();
ok('заливка началась', xhrs.length === 1);
upload.cancelUpload(cancelItem);
await flush();
ok('XHR оборван', xhrs[0].aborted === true);
ok('карточка снята', $uploads.children.length === 0);
eq('очередь пуста', upload._debugUploadState().queued, 0);
eq('ни одного запроса отправки не ушло', netCalls.length, 0);

// ===========================================================================
// 6. Ошибка → повтор начинается С ПЕРВОГО ЭТАПА (Р9)
// ===========================================================================

resetAll();
upload.sendFiles([fakeFile('doc.pdf', 1024, 'application/pdf')], { dialogId: 'chat27' });
await flush();
xhrs[0].respond(503, { error: { code: 'not_connected', message: 'портал недоступен' } });
await flush(10);

eq('карточка помечена как неотправленная', statuses(), ['НЕ ОТПРАВЛЕН']);
eq('автоповтора нет: запросов по-прежнему один', xhrs.length, 1);
const retryBtn = firstButtons().find((b) => b.className.includes('b24-upload-retry'));
ok('кнопка повтора предложена пользователю', !!retryBtn);

retryBtn.click();
await flush();
eq('повтор — это НОВАЯ заливка с первого этапа', xhrs.length, 2);
eq('повтор идёт по тому же адресу приёма', xhrs[1].url, '/api/bitrix/upload');
ok(
    'старый upload_id не переиспользуется',
    netCalls.length === 0,
    'сервер потребил его при первой попытке — повтор с ним дал бы 409',
);
resetAll();

// ===========================================================================
// 7. Очередь строго последовательная
// ===========================================================================

resetAll();
upload.sendFiles(
    [fakeFile('one.txt', 10, 'text/plain'), fakeFile('two.txt', 10, 'text/plain')],
    { dialogId: 'chat27', caption: 'подпись' },
);
await flush();
eq('в очереди два файла', upload._debugUploadState().queued, 2);
eq('заливается только первый', xhrs.length, 1);
eq('второй ждёт очереди', statuses()[1], 'В ОЧЕРЕДИ 2/2');

setNet(async () => jsonRes(200, { id: 1, message: { id: 1, dialog_id: 'chat27', segments: [], media: [] } }));
xhrs[0].respond(200, { upload_id: 'q-1', file_name: 'one.txt', size: 10, mime: 'text/plain' });
await flush(12);
eq('второй пошёл только после первого', xhrs.length, 2);
ok(
    'подпись досталась ТОЛЬКО первому файлу',
    netCalls[0].body.text === 'подпись',
);
xhrs[1].respond(200, { upload_id: 'q-2', file_name: 'two.txt', size: 10, mime: 'text/plain' });
await flush(12);
eq('второе сообщение ушло без подписи', netCalls[1].body.text, '');
eq('очередь опустела', upload._debugUploadState().queued, 0);

// ===========================================================================
// 8. Три входа: кнопка, drag&drop, Ctrl+V
// ===========================================================================

resetAll();
// Вход через DOM не получает `dialogId` параметром — адресат берётся из ленты.
// Поэтому здесь диалог открывается по-настоящему: иначе проверялся бы не путь
// «выбрал файл → он в очереди», а отказ «сначала откройте диалог».
setNet(async () => jsonRes(200, { messages: [], has_more: false }));
await chat.openDialog('chat27', { id: 'chat27', title: 'Полигон' });
await flush(10);
netCalls.length = 0;

upload.initUpload();
upload.initUpload(); // идемпотентность
eq('слушатель клика по ⇪ навешен один раз', $attach.listenerCount('click'), 1);
eq('слушатель change навешен один раз', $fileInput.listenerCount('change'), 1);
eq('слушатель вставки навешен один раз', $input.listenerCount('paste'), 1);

let picked = false;
$fileInput.click = () => { picked = true; };
dispatch($attach, 'click');
ok('клик по ⇪ открывает скрытый input', picked);

// Выбор файла через input.
$fileInput.files = [fakeFile('from-picker.txt', 5, 'text/plain')];
dispatch($fileInput, 'change');
await flush();
ok('файл из ⇪ попал в очередь', upload._debugUploadState().names.includes('from-picker.txt'));
eq('value сброшен, иначе повторный выбор того же файла молчит', $fileInput.value, '');
resetAll();

// Drag&drop.
const dragEvt = () => ({ dataTransfer: { types: ['Files'], files: [] } });
dispatch($panel, 'dragenter', dragEvt());
ok('оверлей поднялся при перетаскивании файла', $dropzone.hidden === false);
dispatch($panel, 'dragenter', dragEvt());
dispatch($panel, 'dragleave', dragEvt());
ok('вложенный dragleave оверлей не гасит', $dropzone.hidden === false);
dispatch($panel, 'dragleave', dragEvt());
ok('последний dragleave гасит оверлей', $dropzone.hidden === true);

dispatch($panel, 'dragenter', { dataTransfer: { types: ['text/plain'] } });
ok(
    'перетаскивание текста оверлей НЕ поднимает',
    $dropzone.hidden === true,
    'зона не должна мешать выделению текста',
);

const dropEv = dispatch($panel, 'drop', {
    dataTransfer: { types: ['Files'], files: [fakeFile('dropped.png', 128, 'image/png')] },
});
await flush();
ok('drop перехвачен (иначе браузер откроет файл вместо страницы)', dropEv.defaultPrevented);
ok('брошенный файл попал в очередь', upload._debugUploadState().names.includes('dropped.png'));
ok('оверлей опущен после drop', $dropzone.hidden === true);
resetAll();

// Ctrl+V.
const pasteText = dispatch($input, 'paste', { clipboardData: { types: ['text/plain'], files: [], items: [] } });
ok(
    'вставка обычного текста НЕ перехватывается',
    !pasteText.defaultPrevented,
    'иначе Ctrl+V перестанет работать по назначению',
);
eq('и очередь не трогает', upload._debugUploadState().queued, 0);

const shot = fakeFile('image.png', 512, 'image/png');
const pasteFile = dispatch($input, 'paste', {
    clipboardData: { types: ['Files'], files: [], items: [{ kind: 'file', getAsFile: () => shot }] },
});
await flush();
ok('вставка файла перехвачена', pasteFile.defaultPrevented);
const pastedName = upload._debugUploadState().names[0] || '';
ok(
    'общее имя скриншота заменено на датированное',
    /^pasted_\d{8}_\d{6}\.png$/.test(pastedName),
    `имя: ${pastedName} — иначе все скриншоты в переписке называются одинаково`,
);

// ===========================================================================
// 9. Подпись из поля ввода и цитата ответа
// ===========================================================================

resetAll();
$input.value = 'подпись из поля';
upload.sendFiles([fakeFile('with-caption.txt', 4, 'text/plain')], { dialogId: 'chat27' });
await flush();
eq('поле ввода очищено: текст стал подписью', $input.value, '');
setNet(async () => jsonRes(200, { id: 9, message: { id: 9, dialog_id: 'chat27', segments: [], media: [] } }));
xhrs[0].respond(200, { upload_id: 'c-1', file_name: 'with-caption.txt', size: 4, mime: 'text/plain' });
await flush(10);
eq('подпись уехала в тело запроса', netCalls[0].body.text, 'подпись из поля');

// ===========================================================================
// 10. objectURL миниатюр и teardown
// ===========================================================================

resetAll();
const before = urls.created;
upload.sendFiles([fakeFile('pic.png', 256, 'image/png')], { dialogId: 'chat27' });
await flush();
ok('у картинки построена локальная миниатюра', urls.created === before + 1);
ok('живой objectURL ровно один', urls.live.size === 1);

upload.teardownUpload();
ok('teardown рвёт летящий запрос', xhrs[xhrs.length - 1].aborted === true);
ok('teardown отпускает objectURL миниатюры', urls.live.size === 0);
eq('очередь очищена', upload._debugUploadState().queued, 0);
ok('контейнер спрятан', $uploads.hidden === true);
ok('поколение выросло — зомби-цикл не возьмётся за новую очередь', upload._debugUploadState().epoch > 0);

// ===========================================================================
// 11. Без открытого диалога отправлять некуда
// ===========================================================================

resetAll();
// Ленту закрываем целиком: `sendFiles` без явного адресата берёт его у ленты,
// и открытый диалог из предыдущего блока сделал бы проверку бессмысленной.
chat.resetChat();
const none = upload.sendFiles([fakeFile('x.txt', 1, 'text/plain')]);
eq('без диалога файл в очередь не берётся', none.length, 0);
eq('и запросов не делает', xhrs.length, 0);
ok('пользователь получает объяснение', $uploads.children.length === 1);
resetAll();

// Модуль ленты обязан отдавать цель ответа наружу: своей копии режима ввода у
// отправки файла нет и быть не должно.
ok('chat.replyMode() существует и без режима отвечает null', chat.replyMode() === null);

console.log(`\n${checks - failures}/${checks} checks passed`);
if (failures > 0) process.exit(1);
