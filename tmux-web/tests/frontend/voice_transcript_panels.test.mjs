// ⚠ СТРАЖ ПРИСУТСТВИЯ РАСШИФРОВКИ В ТРЁХ ЛЕНТАХ (Ф5 фичи голосовых).
//
// Соседний messenger_voice_transcript_core.test.mjs проверяет САМ МОДУЛЬ на
// голом DOM. Здесь проверяется другое и не менее важное: что три панели
// действительно ЗОВУТ его — из своих билдеров карточек, с правильными
// координатами вложения, — и что кадр, доехавший по сокету панели, доходит до
// карточки целиком, а не «почти».
//
// ПОЧЕМУ ТРИ ПРОЦЕССА. Панель резолвит DOM-ref'ы ОДИН раз при импорте
// (core/dom.js), поэтому в одном процессе может жить ровно один фейковый
// документ. Три панели — три процесса: файл перезапускает сам себя с именем
// панели аргументом. Тот же приём уже применён в meet_row_panels.
//
// ЧТО ЛОМАЕТСЯ ТИХО И ЛОВИТСЯ ЗДЕСЬ:
//   • панель перестала звать attach (блока просто нет — и ни один тест панели
//     этого не заметит, у них своя тема);
//   • блок рисуется под фотографией и документом, где расшифровывать нечего;
//   • координаты собраны не те (кадр придёт «не в ту» карточку, а запрос уйдёт
//     за чужое вложение);
//   • панель рассылает кадры под одним именем события, а карточка слушает
//     другое — расшифровка идёт, но экран молчит;
//   • у Telegram потерялся индекс 0 (у него вложение в сообщении одно, и
//     соблазн не передавать индекс вовсе — самый большой).
//
// Запуск: node tmux-web/tests/frontend/voice_transcript_panels.test.mjs

import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

// Страж CSS: селекторы берутся из настоящего css/<панель>.css и проверяются
// против узла, который панель реально построила (см. css_probe.mjs — почему
// обычный querySelector этот дефект не ловит).
import { readCss, selectorMatches, selectorsWith, stripComments } from './css_probe.mjs';

const PANEL = process.argv[2] || null;
const SELF = fileURLToPath(import.meta.url);

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
// Родитель: три дочерних процесса
// ---------------------------------------------------------------------------

if (!PANEL) {
    let bad = 0;
    for (const panel of ['vk', 'telegram', 'bitrix']) {
        const res = spawnSync(process.execPath, [SELF, panel], { encoding: 'utf8' });
        const out = (res.stdout || '') + (res.stderr || '');
        const lines = out.split('\n').filter((l) => l.startsWith('[FAIL]') || l.startsWith('[OK]'));
        for (const l of lines) console.log(l);
        if (res.status !== 0) {
            bad += 1;
            if (lines.length === 0) console.error(out.trim());
        }
    }
    if (bad === 0) console.log('[OK] voice_transcript_panels: блок расшифровки есть во всех трёх лентах');
    else console.error(`[FAIL] voice_transcript_panels: панелей с провалами — ${bad}`);
    process.exit(bad ? 1 : 0);
}

// ---------------------------------------------------------------------------
// Дочерний процесс: одна панель
// ---------------------------------------------------------------------------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Транспорт-счётчик вместо живых роутов панели. */
const calls = { transcript: [], transcribe: [] };
const fakeApi = {
    transcript: async (d, m, i) => {
        calls.transcript.push([String(d), String(m), Number(i)]);
        return { ready: false, text: '', language: '', engine: '', duration_ms: 0 };
    },
    transcribe: async (d, m, i) => {
        calls.transcribe.push([String(d), String(m), Number(i)]);
        return { run_id: 'run-1', attached: false, ready: false, engine: 'local' };
    },
};

const walk = (node, out = []) => {
    out.push(node);
    for (const c of node.childNodes || []) walk(c, out);
    return out;
};
const pick = (root, cls) =>
    walk(root).find((n) => String(n.className || '').split(' ').includes(cls)) || null;

/**
 * Одна и та же проверка для всех трёх панелей.
 *
 * @param {object} o
 * @param {string} o.plugin — имя панели в кадрах и ключах
 * @param {string} o.prefix — префикс классов блока (`vk-voice`, …)
 * @param {object} o.mod — модуль панели с экспортом `voiceTranscript`
 * @param {function} o.build — `(kind) => узел карточки` этой панели
 * @param {function} o.dispatchFrame — рассылка кадра так, как это делает панель
 * @param {string} o.dialogId
 * @param {number|string} o.messageId
 * @param {number} o.index — индекс вложения, который панель ОБЯЗАНА передать
 */
async function checkPanel(o) {
    const { plugin, prefix, mod, build, dispatchFrame, dialogId, messageId, index, css } = o;

    eq(typeof mod.voiceTranscript, 'object', `${plugin}: панель держит общий модуль расшифровки`);
    eq(mod.VOICE_FRAME_EVENT, `${plugin}:voice-transcribe`,
        `${plugin}: имя события кадров объявлено там же, где карточка`);
    mod.voiceTranscript.configure({ api: fakeApi, observe: false });

    // --- блок есть у звука и только у звука
    const voiceCard = build('voice');
    const block = pick(voiceCard, prefix);
    ok(block !== null, `${plugin}: под голосовым есть блок расшифровки`);
    if (!block) return;
    eq(block.dataset.voiceKey, `${plugin}/${dialogId}/${messageId}/${index}`,
        `${plugin}: координаты вложения в ключе блока — те самые`);
    ok(pick(voiceCard, `${prefix}-run`) !== null, `${plugin}: кнопка «Расшифровать» на месте`);

    ok(pick(build('audio'), prefix) !== null,
        `${plugin}: у обычного аудио блок тоже есть (решение Ф4 — разбирается и оно)`);
    for (const kind of ['photo', 'image', 'video', 'document', 'file']) {
        ok(pick(build(kind), prefix) === null,
            `${plugin}: у вложения «${kind}» блока расшифровки нет вовсе`);
    }

    // --- кадр панели доходит до карточки ТЕМ ЖЕ путём, что в браузере
    const fresh = build('voice');
    const freshBlock = pick(fresh, prefix);
    dispatchFrame('started', { engine: 'cloud' });
    eq(freshBlock.dataset.voiceState, 'running', `${plugin}: кадр started доехал до карточки`);
    dispatchFrame('delta', { percent: 55 });
    ok(pick(fresh, `${prefix}-status`).textContent.includes('55 %'),
        `${plugin}: проценты видны в строке состояния`);
    dispatchFrame('done', {
        text: 'здравствуйте, это расшифровка', language: 'ru', engine: 'cloud', duration_ms: 4000,
    });
    eq(freshBlock.dataset.voiceState, 'ready', `${plugin}: кадр done доехал до карточки`);
    eq(pick(fresh, `${prefix}-text`).textContent, 'здравствуйте, это расшифровка',
        `${plugin}: текст расшифровки виден под волной`);

    // --- перерисовка ленты текст сохраняет
    const again = build('voice');
    await sleep(5);
    eq(pick(again, `${prefix}-text`).textContent, 'здравствуйте, это расшифровка',
        `${plugin}: перерисовка ленты готовый текст сохраняет`);

    // --- КАЖДЫЙ селектор панели совпадает с настоящим узлом блока
    //
    // Опечатка в имени класса (`-toggle` против `-collapse`) не ломает ни
    // одного теста разметки: узел есть, просто он не покрашен и не свёрнут.
    // Ловится это только сверкой селекторов CSS с построенным деревом — и
    // сверять надо ВО ВСЕХ состояниях: правило `[data-voice-state="error"]`
    // на готовой карточке не совпадёт с чем угодно.
    {
        const text = stripComments(readCss(import.meta.url, css));
        const selectors = selectorsWith(text, `.${prefix}`);
        ok(selectors.length >= 8, `${plugin}: селекторы блока расшифровки в CSS найдены`);

        // Узлы ЖИВЫЕ: все представления одного ключа перерисовываются на каждый
        // кадр, поэтому «снять слепок дерева и сверить потом» не работает —
        // сверять надо в тот момент, когда карточка в нужном состоянии.
        const matched = new Set();
        const judged = new Set();
        const probe = (node) => {
            for (const n of walk(node)) {
                for (const sel of selectors) {
                    const verdict = selectorMatches(sel, n);
                    // `null` — селектор матчеру не по зубам (`:not`,
                    // `:disabled`): такие не считаем ни за, ни против.
                    if (verdict === null) continue;
                    judged.add(sel);
                    if (verdict === true) matched.add(sel);
                }
            }
        };

        probe(fresh);                                       // готово, короткий текст
        const longCard = build('voice');
        dispatchFrame('done', { text: 'я'.repeat(600), language: 'ru', engine: 'local' });
        probe(longCard);                                    // готово, длинный свёрнут
        const errCard = build('voice');
        dispatchFrame('error', { code: 'voice_no_key', message: 'нет ключа' });
        probe(errCard);                                     // отказ
        const runCard = build('voice');
        dispatchFrame('started', { engine: 'local' });
        probe(runCard);                                     // идёт

        ok(judged.size >= 8, `${plugin}: разобрано достаточно селекторов (${judged.size})`);
        eq([...judged].filter((sel) => !matched.has(sel)), [],
            `${plugin}: каждый селектор блока совпадает с настоящим узлом карточки`);
    }

    // --- запрос ушёл по правильным координатам, и ровно один
    const asked = calls.transcript.filter(
        (c) => c[0] === String(dialogId) && c[1] === String(messageId) && c[2] === index);
    ok(asked.length <= 1, `${plugin}: готовый текст спрошен не больше одного раза`);
    eq(calls.transcribe.length, 0, `${plugin}: сама собой расшифровка не запускалась`);
    mod.voiceTranscript.reset();
}

if (PANEL === 'vk') {
    const stub = await import('./vk_dom_stub.mjs');
    const M = await import(new URL('../../static/js/vk/media.js', import.meta.url).href);
    const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
    S.vkState.dialogs = [{ id: '1234', kind: 'user', title: 'Павел', username: 'durov' }];
    S.vkState.activeDialogId = '1234';
    stub.setNet(async () => stub.jsonRes(200, {}));

    const att = (kind) => ({
        index: 2, kind, mime: null, size: 2048, file_name: null, title: null,
        width: null, height: null, duration_secs: 7, has_thumb: false, has_full: true,
        player_url: null, link_url: null, waveform: [1, 5, 9],
    });
    const msg = (kind) => ({
        id: 7050, dialog_id: '1234', out: false, date: 1700000000,
        sender: { id: '1234', first_name: 'Павел', screen_name: 'durov' },
        text: '', media: att(kind), attachments: [att(kind)], reply_to_id: null, reply_to: null,
        edit_date: null, reply_markup: null, cmid: 1, fwd: [], action: null, payload: null,
    });
    // Карточка строится штатным путём панели — вместе с волной и подвалом.
    const build = (kind) => {
        M.resetMediaCards();
        return M.buildMediaCard(msg(kind), att(kind), []);
    };
    await checkPanel({
        plugin: 'vk',
        css: '../../static/css/vk.css',
        prefix: 'vk-voice',
        mod: M,
        build,
        dialogId: '1234',
        messageId: 7050,
        index: 2,
        dispatchFrame: (phase, extra) => stub.dispatch(globalThis.document, M.VOICE_FRAME_EVENT, {
            detail: {
                phase,
                plugin: 'vk',
        css: '../../static/css/vk.css',
                dialogId: '1234',
                messageId: '7050',
                index: 2,
                frame: { run_id: 'run-1', dialog_id: '1234', message_id: 7050, index: 2, ...extra },
            },
        }),
    });
}

if (PANEL === 'telegram') {
    const stub = await import('./telegram_dom_stub.mjs');
    const M = await import(new URL('../../static/js/telegram/media.js', import.meta.url).href);

    const msg = (kind) => ({
        id: 8100,
        dialog_id: '-1001234',
        media: { kind, mime: 'audio/ogg', size: 2048, duration_secs: 7, waveform: [1, 5, 9] },
    });
    const build = (kind) => M.buildMediaCard(msg(kind));
    await checkPanel({
        plugin: 'telegram',
        css: '../../static/css/telegram.css',
        prefix: 'tg-voice',
        mod: M,
        build,
        dialogId: '-1001234',
        messageId: 8100,
        // У Telegram вложение в сообщении ОДНО, и индекс всегда нулевой —
        // ровно как в соли заглушки волны.
        index: 0,
        dispatchFrame: (phase, extra) => stub.dispatch(globalThis.document, M.VOICE_FRAME_EVENT, {
            detail: {
                phase,
                plugin: 'telegram',
        css: '../../static/css/telegram.css',
                dialogId: '-1001234',
                messageId: '8100',
                index: 0,
                frame: { run_id: 'run-1', dialog_id: '-1001234', message_id: 8100, index: 0, ...extra },
            },
        }),
    });
}

if (PANEL === 'bitrix') {
    const stub = await import('./bitrix_dom_stub.mjs');
    const M = await import(new URL('../../static/js/bitrix/media.js', import.meta.url).href);
    // Живой сети здесь нет. Ответ обязан быть УСПЕШНЫМ: карточка картинки
    // тянет превью сразу (в фейковом документе нет IntersectionObserver), а
    // ветка отказа зовёт `status.after` — метод, которого у стаба нет.
    stub.setNet(async () => ({
        ok: true,
        status: 200,
        headers: { get: (h) => (String(h).toLowerCase() === 'content-type' ? 'image/png' : null) },
        body: null,
        blob: async () => new globalThis.Blob(['AAAA']),
    }));

    const msg = (kind) => ({
        id: 9200,
        dialog_id: 'chat42',
        media: [{
            index: 1, kind, name: 'запись.mp3', size: 2048, file_id: 55,
            has_thumb: false, has_full: true, player_url: null, waveform: [1, 5, 9],
        }],
    });
    const build = (kind) => {
        const box = M.renderMediaGroup(msg(kind));
        return box ? box.childNodes[0] : { childNodes: [] };
    };
    await checkPanel({
        plugin: 'bitrix',
        css: '../../static/css/bitrix.css',
        prefix: 'b24-voice',
        mod: M,
        build,
        dialogId: 'chat42',
        messageId: 9200,
        index: 1,
        dispatchFrame: (phase, extra) => stub.dispatch(globalThis.document, M.VOICE_FRAME_EVENT, {
            detail: {
                phase,
                plugin: 'bitrix',
        css: '../../static/css/bitrix.css',
                dialogId: 'chat42',
                messageId: '9200',
                index: 1,
                frame: { run_id: 'run-1', dialog_id: 'chat42', message_id: 9200, index: 1, ...extra },
            },
        }),
    });
}

if (failures === 0) console.log(`[OK] ${PANEL}: ${checks} проверок`);
else console.error(`[FAIL] ${PANEL}: ${failures} из ${checks}`);
process.exit(failures > 0 ? 1 : 0);
