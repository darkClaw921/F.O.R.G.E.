// ⚠ СТРАЖ КАРТОЧКИ РАСШИФРОВКИ (Ф5 фичи голосовых).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/messenger/voice-transcript-core.js
// поверх крошечного фейкового документа. Живого сервера здесь нет и быть не
// должно: транспорт подсовывается через `configure({api})`, и каждый его вызов
// считается — половина смысла этого файла в том, СКОЛЬКО запросов уходит.
//
// ЧТО ЛОМАЕТСЯ ТИХО И ЛОВИТСЯ ЗДЕСЬ:
//
//   • состояние карточки живёт в узлах, а не в модуле — тогда перерисовка
//     ленты стирает уже посчитанный (то есть уже оплаченный) текст;
//   • перерисовка посреди прогона начинает ВТОРОЙ прогон;
//   • лента из сотни голосовых даёт сотню запросов готового текста;
//   • кадр чужого вложения дорисовывает нашу карточку;
//   • в причину отказа просачивается хвост stderr движка — с путями до
//     бинарей, а то и со следом ключа облака;
//   • длинный текст перестал сворачиваться и выдавил ленту с экрана;
//   • текст расшифровки (дословная речь собеседника — такой же чужой текст,
//     как тело сообщения) попал в DOM разметкой, а не текстом.
//
// Мутационная проверка: поменяйте COLLAPSE_CHARS или EAGER_FETCH_BUDGET —
// тест обязан покраснеть; уберите проверку ключа в onFrame — покраснеет тоже.
//
// Запуск: node tmux-web/tests/frontend/messenger_voice_transcript_core.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const V = await import(new URL('../../static/js/messenger/voice-transcript-core.js', import.meta.url).href);

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');

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
// Фейковый документ: узлы, классы, кнопки со слушателями
// ---------------------------------------------------------------------------

function makeDoc() {
    return {
        createElement(tag) {
            const node = {
                tagName: String(tag),
                className: '',
                childNodes: [],
                dataset: {},
                style: {},
                attributes: {},
                listeners: {},
                hidden: false,
                disabled: false,
                textContent: '',
                title: '',
                type: '',
            };
            node.appendChild = (child) => { node.childNodes.push(child); return child; };
            node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
            node.getAttribute = (k) => (k in node.attributes ? node.attributes[k] : null);
            node.addEventListener = (type, fn) => {
                (node.listeners[type] = node.listeners[type] || []).push(fn);
            };
            node.click = () => {
                for (const fn of node.listeners.click || []) fn({ stopPropagation() {} });
            };
            return node;
        },
    };
}

const walk = (node, out = []) => {
    out.push(node);
    for (const c of node.childNodes || []) walk(c, out);
    return out;
};
const pick = (root, cls) =>
    walk(root).find((n) => String(n.className || '').split(' ').includes(cls)) || null;

const doc = makeDoc();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Карточка панели «vk» со стандартным префиксом классов теста. */
const P = 'zz-voice';
const card = (ui, o) => ui.attach({ doc, kind: 'voice', index: 0, ...o });

/** Транспорт-счётчик. */
function fakeApi(over) {
    const calls = { transcript: [], transcribe: [] };
    const api = {
        transcript: async (d, m, i) => {
            calls.transcript.push([String(d), String(m), i]);
            return (over && over.transcript) ? over.transcript(d, m, i) : { ready: false, text: '' };
        },
        transcribe: async (d, m, i, opt) => {
            calls.transcribe.push([String(d), String(m), i, opt || null]);
            if (over && over.transcribe) return over.transcribe(d, m, i, opt);
            return { run_id: 'r1', attached: false, ready: false, engine: 'local' };
        },
    };
    return { api, calls };
}

function newUi(over) {
    const { api, calls } = fakeApi(over);
    const ui = V.createVoiceTranscript({ plugin: 'zz', classPrefix: P, frameEvent: 'zz:voice' });
    ui.configure({ api, observe: false });
    return { ui, calls };
}

const frame = (phase, extra) => ({
    phase,
    plugin: 'zz',
    dialogId: '1',
    messageId: '2',
    index: 0,
    frame: { run_id: 'r1', dialog_id: '1', message_id: 2, index: 0, ...extra },
});

// ---------------------------------------------------------------------------
// 1. Чистые функции
// ---------------------------------------------------------------------------

eq(V.transcriptKey('vk', '1', 2, 0), 'vk/1/2/0', 'ключ склеен из плагина и тройки координат');
eq(V.transcriptKey('vk', '1', 2, 0) === V.transcriptKey('telegram', '1', 2, 0), false,
    'плагин входит в ключ — иначе кадр Telegram дорисует карточку VK');
ok(V.isAudioKind('voice') && V.isAudioKind('audio'), 'голосовое и аудио — оба со звуком');
ok(!V.isAudioKind('photo') && !V.isAudioKind('') && !V.isAudioKind(null),
    'фотография, пустой вид и отсутствие вида звуком не считаются');
ok(!V.needsCollapse('x'.repeat(V.COLLAPSE_CHARS)), 'ровно потолок не сворачивается');
ok(V.needsCollapse('x'.repeat(V.COLLAPSE_CHARS + 1)), 'на символ длиннее — сворачивается');
eq(V.errorText('voice_no_key'), V.ERROR_TEXT.voice_no_key, 'причина берётся по коду');
eq(V.errorText('чего-то новенькое'), V.ERROR_FALLBACK, 'неизвестный код даёт общую фразу');
eq(V.errorText(null), V.ERROR_FALLBACK, 'отсутствие кода тоже даёт общую фразу');
// Прототипное свойство не должно притворяться кодом ошибки.
eq(V.errorText('constructor'), V.ERROR_FALLBACK, 'ключ из прототипа не считается кодом');

// ---------------------------------------------------------------------------
// 2. Четыре состояния и переходы между ними
// ---------------------------------------------------------------------------

{
    const { ui } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    ok(root !== null, 'у голосового блок расшифровки есть');
    eq(root.dataset.voiceState, 'idle', 'исходное состояние — «расшифровки нет»');
    eq(pick(root, `${P}-run`).textContent, 'Расшифровать', 'в idle кнопка предлагает расшифровать');
    eq(pick(root, `${P}-run`).hidden, false, 'кнопка видна');
    eq(pick(root, `${P}-text`).hidden, true, 'текста нет — узел текста спрятан');
    eq(pick(root, `${P}-status`).textContent, '', 'в idle строка состояния молчит');

    ui.onFrame(frame('started', { engine: 'cloud' }));
    eq(root.dataset.voiceState, 'running', 'кадр started переводит в «идёт»');
    ok(pick(root, `${P}-status`).textContent.includes('облако'),
        'движок назван ДО того, как голосовое ушло в чужое облако');
    eq(pick(root, `${P}-run`).disabled, true, 'во время прогона кнопка заблокирована');
    eq(root.attributes['aria-busy'], 'true', 'на время прогона выставлен aria-busy');
    eq(pick(root, `${P}-progress`).hidden, false, 'полоса прогресса показана');
    eq(pick(root, `${P}-bar`).style.width, '0%',
        'до первой дельты полоса пустая — правдоподобным числом неизвестное не подменяем');

    ui.onFrame(frame('delta', { percent: 40 }));
    ok(pick(root, `${P}-status`).textContent.includes('40 %'), 'проценты видны в строке состояния');
    eq(pick(root, `${P}-bar`).style.width, '40%', 'полоса доехала до сорока процентов');
    ui.onFrame(frame('delta', { percent: 10 }));
    ok(pick(root, `${P}-status`).textContent.includes('40 %'),
        'опоздавший кадр НЕ откатывает полосу назад');
    eq(pick(root, `${P}-text`).textContent, '', 'дельты не пишут текста — его несёт только done');

    ui.onFrame(frame('done', { text: 'привет', language: 'ru', engine: 'cloud', duration_ms: 3000 }));
    eq(root.dataset.voiceState, 'ready', 'кадр done переводит в «готово»');
    eq(pick(root, `${P}-text`).textContent, 'привет', 'текст расшифровки виден');
    eq(pick(root, `${P}-text`).hidden, false, 'узел текста показан');
    eq(root.attributes['aria-busy'], 'false', 'после финала aria-busy снят');
    eq(pick(root, `${P}-progress`).hidden, true, 'на готовом тексте полоса убрана');
    eq(pick(root, `${P}-run`).hidden, true, 'на готовом тексте кнопка запуска убрана');

    ui.onFrame(frame('error', { code: 'voice_no_key', message: '/usr/local/bin/whisper-cli: ключ sk-XXX' }));
    eq(root.dataset.voiceState, 'error', 'кадр error переводит в «ошибка»');
    eq(pick(root, `${P}-status`).textContent, V.ERROR_TEXT.voice_no_key,
        'показана человеческая причина по коду');
    eq(pick(root, `${P}-run`).textContent, 'Повторить', 'после отказа кнопка предлагает повтор');
    const shown = pick(root, `${P}-status`).textContent;
    ok(!shown.includes('/usr/') && !shown.includes('sk-') && !shown.includes('whisper-cli'),
        'ни пути, ни следа ключа, ни имени бинаря на экран не попали');
}

// ---------------------------------------------------------------------------
// 3. Чужие кадры
// ---------------------------------------------------------------------------

{
    const { ui } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    ui.onFrame({ ...frame('done', { text: 'чужое' }), messageId: '999' });
    eq(root.dataset.voiceState, 'idle', 'кадр другого сообщения нашу карточку не трогает');
    ui.onFrame({ ...frame('done', { text: 'чужое' }), index: 3 });
    eq(root.dataset.voiceState, 'idle', 'кадр другого вложения того же сообщения — тоже мимо');
    ui.onFrame({ ...frame('done', { text: 'чужое' }), dialogId: '77' });
    eq(root.dataset.voiceState, 'idle', 'кадр другой беседы — мимо');
    ui.onFrame({ ...frame('done', { text: 'чужое' }), plugin: 'yy' });
    eq(root.dataset.voiceState, 'idle', 'кадр соседней панели — мимо');
    ui.onFrame({ phase: 'done', plugin: 'zz', dialogId: '1', messageId: '2', index: 0 });
    eq(root.dataset.voiceState, 'idle', 'кадр без тела игнорируется, а не роняет модуль');
    ui.onFrame(frame('какая-то новая фаза', { text: 'что-то' }));
    eq(root.dataset.voiceState, 'idle', 'незнакомая фаза ничего не меняет');
}

// ---------------------------------------------------------------------------
// 4. Сворачивание длинного текста
// ---------------------------------------------------------------------------

{
    const { ui } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    ui.onFrame(frame('done', { text: 'коротко' }));
    const textEl = pick(root, `${P}-text`);
    const toggle = pick(root, `${P}-toggle`);
    eq(toggle.hidden, true, 'короткий текст не предлагает разворачивания');
    ok(!String(textEl.className).includes(`${P}-text-collapsed`), 'короткий текст не свёрнут');

    const long = 'а'.repeat(V.COLLAPSE_CHARS + 50);
    ui.onFrame(frame('done', { text: long }));
    eq(toggle.hidden, false, 'длинный текст даёт кнопку разворачивания');
    eq(toggle.textContent, 'Показать полностью', 'подпись кнопки в свёрнутом виде');
    eq(toggle.attributes['aria-expanded'], 'false', 'свёрнутое состояние объявлено через aria');
    ok(String(textEl.className).includes(`${P}-text-collapsed`), 'длинный текст свёрнут классом');
    eq(textEl.textContent, long, 'в DOM лежит ПОЛНЫЙ текст — сворачивание рисованное, не обрезка');

    toggle.click();
    ok(!String(textEl.className).includes(`${P}-text-collapsed`), 'нажатие разворачивает');
    eq(toggle.textContent, 'Свернуть', 'подпись кнопки в развёрнутом виде');
    eq(toggle.attributes['aria-expanded'], 'true', 'развёрнутое состояние объявлено через aria');
    toggle.click();
    ok(String(textEl.className).includes(`${P}-text-collapsed`), 'повторное нажатие сворачивает обратно');
}

// ---------------------------------------------------------------------------
// 5. Запуск: один запрос на сколько угодно нажатий
// ---------------------------------------------------------------------------

{
    const { ui, calls } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    const run = pick(root, `${P}-run`);
    run.click();
    eq(root.dataset.voiceState, 'running', 'нажатие сразу переводит карточку в «идёт»');
    run.click();
    run.click();
    await sleep(0);
    eq(calls.transcribe.length, 1, 'три нажатия подряд дают РОВНО один запуск');
    eq(calls.transcribe[0].slice(0, 3), ['1', '2', 0], 'координаты вложения уехали правильные');
    eq(calls.transcribe[0][3], null, 'обычный запуск идёт без force');

    ui.onFrame(frame('done', { text: 'готово' }));
    const before = calls.transcribe.length;
    run.click();
    await sleep(0);
    eq(calls.transcribe.length, before, 'на готовом тексте кнопка спрятана и не стреляет');
}

{
    // Отказ POST'а разбирается ПО КОДУ, а не по тексту.
    const err = new Error('whisper-cli: ggml_metal_init: no device /Users/имя/models');
    err.code = 'metal_unavailable';
    const { ui } = newUi({ transcribe: async () => { throw err; } });
    const root = card(ui, { dialogId: '1', messageId: '2' });
    pick(root, `${P}-run`).click();
    await sleep(0);
    eq(root.dataset.voiceState, 'error', 'отказ запуска показан состоянием ошибки');
    eq(pick(root, `${P}-status`).textContent, V.ERROR_TEXT.metal_unavailable,
        'причина взята из таблицы кодов');
    ok(!pick(root, `${P}-status`).textContent.includes('/Users/'),
        'путь из сообщения движка на экран не попал');
    eq(pick(root, `${P}-run`).disabled, false, 'после отказа кнопка снова нажимается');
}

{
    // Уже посчитанная расшифровка приходит прямо в ответе POST'а.
    const { ui, calls } = newUi({
        transcribe: async () => ({ run_id: null, attached: false, ready: true, engine: 'local', text: 'было готово' }),
    });
    const root = card(ui, { dialogId: '1', messageId: '2' });
    pick(root, `${P}-run`).click();
    await sleep(0);
    eq(root.dataset.voiceState, 'ready', 'ready в ответе POST сразу даёт готовую карточку');
    eq(pick(root, `${P}-text`).textContent, 'было готово', 'текст из ответа показан');
    eq(calls.transcribe.length, 1, 'второго запроса не понадобилось');
}

// ---------------------------------------------------------------------------
// 6. Перерисовка ленты
// ---------------------------------------------------------------------------

{
    const { ui, calls } = newUi();
    const first = card(ui, { dialogId: '1', messageId: '2' });
    await sleep(0);
    eq(calls.transcript.length, 1, 'на новой карточке один запрос готового текста');
    ui.onFrame(frame('done', { text: 'сказанное', language: 'ru', engine: 'local' }));

    const second = card(ui, { dialogId: '1', messageId: '2' });
    await sleep(0);
    eq(second.dataset.voiceState, 'ready', 'перерисовка ленты сохраняет готовый текст');
    eq(pick(second, `${P}-text`).textContent, 'сказанное', 'и сам текст на месте');
    eq(calls.transcript.length, 1, 'перерисовка не порождает второго запроса');
    ok(first !== second, 'узлы действительно разные — это перерисовка, а не тот же блок');

    // Кадр после перерисовки доезжает до НОВОГО узла.
    ui.onFrame(frame('done', { text: 'уточнили', language: 'ru', engine: 'local' }));
    eq(pick(second, `${P}-text`).textContent, 'уточнили', 'кадр рисуется в свежем узле');
}

{
    const { ui, calls } = newUi();
    const first = card(ui, { dialogId: '1', messageId: '2' });
    pick(first, `${P}-run`).click();
    await sleep(0);
    const asked = calls.transcript.length;
    const second = card(ui, { dialogId: '1', messageId: '2' });
    await sleep(0);
    eq(second.dataset.voiceState, 'running', 'перерисовка посреди прогона показывает прогон');
    eq(calls.transcribe.length, 1, 'перерисовка посреди прогона НЕ начинает второй прогон');
    eq(calls.transcript.length, asked, 'и не идёт спрашивать готовый текст у запущенного вложения');
    ui.onFrame(frame('delta', { percent: 70 }));
    ok(pick(second, `${P}-status`).textContent.includes('70 %'), 'прогресс продолжается в новом узле');
}

{
    // Кадр про вложение, которого сейчас нет на экране, не теряется: карточка
    // покажет актуальное состояние, как только доедет до ленты.
    const { ui, calls } = newUi();
    ui.onFrame(frame('started', { engine: 'local' }));
    const root = card(ui, { dialogId: '1', messageId: '2' });
    await sleep(0);
    eq(root.dataset.voiceState, 'running', 'карточка нарисована уже в состоянии «идёт»');
    eq(calls.transcript.length, 0, 'про идущее вложение готовый текст не спрашивают');
}

{
    // РЕГРЕСС forge-cvoice-f5: КАРТОЧКА, ЕЩЁ НЕ ВСТАВЛЕННАЯ В ДОКУМЕНТ.
    //
    // Панель зовёт `attach` при СБОРКЕ карточки — в этот момент и блок, и сама
    // карточка, и строка ленты ещё отцеплены. Первая версия модуля чистила
    // представления по `isConnected === false` прямо в рендере и выбрасывала
    // такое представление первым же кадром: на экране навсегда оставалось
    // «Расшифровать». На фейковом документе дефект невидим (там нет
    // `isConnected`) — поймала живая проба в headless Chrome
    // (voice_transcript_live). Здесь он закреплён и без браузера.
    const { ui } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    root.isConnected = false;
    ui.onFrame(frame('done', { text: 'доехало', language: 'ru', engine: 'local' }));
    eq(root.dataset.voiceState, 'ready', 'кадр рисуется и в ещё не вставленной карточке');
    eq(pick(root, `${P}-text`).textContent, 'доехало', 'и текст в ней виден');

    // А вот ПРЕЖНЕЕ отцепленное представление после перерисовки выбрасывается.
    const second = card(ui, { dialogId: '1', messageId: '2' });
    second.isConnected = false;
    ui.onFrame(frame('done', { text: 'обновилось', language: 'ru', engine: 'local' }));
    eq(pick(second, `${P}-text`).textContent, 'обновилось', 'свежая карточка обновилась');
    eq(pick(root, `${P}-text`).textContent, 'доехало',
        'отцепленная карточка прежней ленты больше не перерисовывается');
}

// ---------------------------------------------------------------------------
// 7. Сотня голосовых — не сотня запросов
// ---------------------------------------------------------------------------

{
    const { ui, calls } = newUi();
    for (let i = 0; i < 100; i += 1) card(ui, { dialogId: '1', messageId: String(1000 + i) });
    await sleep(0);
    ok(calls.transcript.length <= V.EAGER_FETCH_BUDGET,
        `лента из 100 голосовых уложилась в потолок запросов (ушло ${calls.transcript.length})`);
    ok(calls.transcript.length > 0, 'но верхние карточки всё-таки спрошены');
    eq(calls.transcribe.length, 0, 'и НИ ОДНОЙ расшифровки не запущено само собой');
}

{
    // Вкладка, открытая ПОСРЕДИ чужого прогона, `started` уже не увидит:
    // первый же кадр прогресса обязан сам перевести карточку в «идёт».
    const { ui, calls } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    ui.onFrame(frame('delta', { percent: 30 }));
    eq(root.dataset.voiceState, 'running', 'дельта без started тоже включает «идёт»');
    ok(pick(root, `${P}-status`).textContent.includes('30 %'), 'и показывает проценты');
    await sleep(0);
    eq(calls.transcribe.length, 0, 'подхват чужого прогона ничего не запускает');
}

{
    // Готовый текст, приехавший ответом GET, рисуется без единого кадра.
    const { ui } = newUi({
        transcript: async () => ({
            ready: true, text: 'из файла', language: 'ru', engine: 'local', duration_ms: 1000,
        }),
    });
    const root = card(ui, { dialogId: '1', messageId: '2' });
    await sleep(0);
    eq(root.dataset.voiceState, 'ready', 'готовый текст с диска показан сам');
    eq(pick(root, `${P}-text`).textContent, 'из файла', 'и текст тот самый');
}

{
    // Отказ GET'а не красит карточку: «готового нет» — обычное состояние.
    const { ui } = newUi({ transcript: async () => { throw new Error('HTTP 500'); } });
    const root = card(ui, { dialogId: '1', messageId: '2' });
    await sleep(0);
    eq(root.dataset.voiceState, 'idle', 'сломанный GET оставляет карточку с рабочей кнопкой');
    eq(pick(root, `${P}-run`).disabled, false, 'кнопка после отказа GET снова нажимается');
}

// ---------------------------------------------------------------------------
// 8. Не-звук блока не получает
// ---------------------------------------------------------------------------

{
    const { ui } = newUi();
    for (const kind of ['photo', 'image', 'video', 'file', 'document', 'link', 'other', '']) {
        eq(card(ui, { dialogId: '1', messageId: '2', kind }), null,
            `у вложения «${kind}» блока расшифровки нет вовсе`);
    }
    ok(card(ui, { dialogId: '1', messageId: '3', kind: 'audio' }) !== null,
        'у обычного аудио блок есть — то же решение, что у авторазбора Ф4');
}

// ---------------------------------------------------------------------------
// 9. Доступность
// ---------------------------------------------------------------------------

{
    const { ui } = newUi();
    const root = card(ui, { dialogId: '1', messageId: '2' });
    const run = pick(root, `${P}-run`);
    const toggle = pick(root, `${P}-toggle`);
    eq(run.attributes.type, 'button', 'кнопка запуска — button, а не submit');
    eq(toggle.attributes.type, 'button', 'кнопка разворачивания — button');
    ok(String(run.attributes.title || '').length > 0, 'у кнопки запуска есть подсказка');
    eq(pick(root, `${P}-status`).attributes.role, 'status', 'строка состояния — role=status');
    eq(pick(root, `${P}-status`).attributes['aria-live'], 'polite',
        'живая строка вежливая: перебивать чтение самого сообщения нельзя');
    eq(pick(root, `${P}-label`).textContent, 'Расшифровка', 'у блока есть видимая подпись');
    // Спрятан от озвучки ровно один узел — полоса: она картинка того же
    // числа, что уже сказано словами.
    eq(walk(root).filter((n) => n.attributes['aria-hidden'] === 'true')
        .map((n) => n.className), [`${P}-progress`],
        'от скринридера спрятана только полоса прогресса');
    eq(root.dataset.voiceKey, 'zz/1/2/0', 'ключ вложения проставлен в data-атрибут');
}

// ---------------------------------------------------------------------------
// 10. Два инстанса слепы друг к другу
// ---------------------------------------------------------------------------

{
    const a = newUi();
    const b = newUi();
    const rootA = card(a.ui, { dialogId: '1', messageId: '2' });
    const rootB = card(b.ui, { dialogId: '1', messageId: '2' });
    a.ui.onFrame(frame('done', { text: 'только А' }));
    eq(rootA.dataset.voiceState, 'ready', 'своя карточка обновилась');
    eq(rootB.dataset.voiceState, 'idle', 'карточка соседнего инстанса не тронута');
    eq(pick(rootB, `${P}-text`).textContent, '', 'и текст к соседу не утёк');
}

// ---------------------------------------------------------------------------
// 11. Инвариант XSS: разметка строкой не пишется нигде
// ---------------------------------------------------------------------------

{
    const src = read('../../static/js/messenger/voice-transcript-core.js');
    // Свойства перечислены по кускам, чтобы сам страж не содержал искомой
    // строки целиком: иначе он краснел бы на себе.
    for (const bad of ['inner' + 'HTML', 'outer' + 'HTML', 'insertAdjacent' + 'HTML', '.write(']) {
        ok(!src.includes(bad), `модуль не пишет разметку через ${bad}`);
    }
    ok(src.includes('textContent'), 'тексты кладутся через textContent');
    // Ни одного упоминания конкретного мессенджера: модуль общий.
    for (const name of ['vk', 'telegram', 'bitrix', 'b24', 'tg-']) {
        const hits = src.toLowerCase().split('\n')
            .filter((l) => !l.trim().startsWith('//') && !l.trim().startsWith('*'))
            .filter((l) => l.includes(name));
        eq(hits, [], `в коде модуля нет упоминаний «${name}»`);
    }
}

// ---------------------------------------------------------------------------
// 12. Страж единственности: ветка живёт в общем модуле, а не в трёх копиях
// ---------------------------------------------------------------------------
//
// Тот же страж, что у звуковой дорожки (messenger_waveform_core): три копии
// одной карточки разъезжаются не «может быть», а неизбежно — и первым же
// признаком расхождения будет разный текст кнопки в двух панелях.

{
    const panels = {
        'vk/media.js': read('../../static/js/vk/media.js'),
        'telegram/media.js': read('../../static/js/telegram/media.js'),
        'bitrix/media.js': read('../../static/js/bitrix/media.js'),
    };
    for (const [name, src] of Object.entries(panels)) {
        ok(src.includes('messenger/voice-transcript-core.js'),
            `${name} рисует расшифровку общим модулем`);
        ok(src.includes('voiceTranscript.attach('),
            `${name} действительно зовёт общий модуль из билдера карточки`);
        // Подписи и коды — след своей копии карточки.
        for (const literal of ['Расшифровать', 'Показать полностью', 'voice_no_key', 'metal_unavailable']) {
            ok(!src.includes(literal), `${name} не хранит своей копии подписи «${literal}»`);
        }
        // Разбор кадров по фазам — тоже дело модуля, а не панели.
        ok(!src.includes("'delta'") && !src.includes("'started'"),
            `${name} не разбирает кадры расшифровки сам`);
    }
    // Карточка Bitrix строится в attachments.js, и он обязан остаться ЧИСТЫМ:
    // ни запросов, ни обработчиков — иначе его нельзя будет проверять в голом
    // Node. Расшифровка подцепляется в media.js, рядом с прочим оживлением.
    ok(!read('../../static/js/bitrix/attachments.js').includes('voice-transcript-core'),
        'bitrix/attachments.js остался без запросов и обработчиков');
}

// Стили блока есть у всех трёх панелей — иначе он рисуется голым текстом,
// а свёрнутый длинный текст не сворачивается вовсе.
for (const [css, prefix] of [
    ['../../static/css/vk.css', '.vk-voice'],
    ['../../static/css/telegram.css', '.tg-voice'],
    ['../../static/css/bitrix.css', '.b24-voice'],
]) {
    const text = read(css);
    const file = css.split('/').pop();
    for (const suffix of ['', '-status', '-text', '-text-collapsed', '-progress', '-bar', '-run', '-toggle']) {
        ok(text.includes(`${prefix}${suffix} `) || text.includes(`${prefix}${suffix},`)
            || text.includes(`${prefix}${suffix}:`) || text.includes(`${prefix}${suffix}[`),
            `${prefix}${suffix} описан в ${file}`);
    }
    ok(text.includes(`${prefix} [hidden]`),
        `${file}: [hidden] задан явно — у свёрнутого текста свой display`);
    // Ни одного литерала цвета: тема панели живёт в токенах.
    const block = text.slice(text.indexOf(`${prefix} {`));
    ok(!/#[0-9a-fA-F]{3,8}\b/.test(block) && !/\brgba?\(/.test(block),
        `${file}: блок расшифровки красится ТОЛЬКО токенами панели`);
}


console.log(`${checks - failures}/${checks} проверок прошло (frontend/messenger_voice_transcript_core)`);
if (failures > 0) console.error(`${failures} проверок упало`);
process.exit(failures > 0 ? 1 : 0);
