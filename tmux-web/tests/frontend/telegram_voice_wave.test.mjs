// Регресс-тест ЗВУКОВОЙ ДОРОЖКИ голосовых в панели Telegram (Ф2 фичи голосовых).
//
// Близнец блока про Bitrix из messenger_waveform_core.test.mjs, но на РЕАЛЬНОЙ
// карточке: `buildMediaCard` панели Telegram рисует на глобальном `document`,
// поэтому нужен telegram_dom_stub.mjs — и импортировать его обязательно ПЕРВЫМ,
// раньше `js/telegram/media.js`: `core/dom.js` резолвит все `$tg*` один раз на
// верхнем уровне модуля.
//
// Что доказывается:
//   • голосовое с огибающей (её Telegram присылает в
//     `DocumentAttributeAudio.waveform`) рисуется НАСТОЯЩЕЙ волной;
//   • аудио без атрибута рисуется волной, посчитанной на бэке, когда она уже
//     доехала в `media.waveform`, и прежней ДЕТЕРМИНИРОВАННОЙ заглушкой, пока
//     не доехала;
//   • у видов без звука волны нет вовсе.
//
// Запуск: node tmux-web/tests/frontend/telegram_voice_wave.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import './telegram_dom_stub.mjs';

const M = await import(new URL('../../static/js/telegram/media.js', import.meta.url).href);
const W = await import(new URL('../../static/js/messenger/waveform-core.js', import.meta.url).href);

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
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    checks += 1;
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

/** Все узлы поддерева. */
function walk(node, out = []) {
    out.push(node);
    for (const c of node.childNodes || []) walk(c, out);
    return out;
}

function pick(root, cls) {
    return walk(root).find((n) => String(n.className || '').split(' ').includes(cls)) || null;
}

function heightsOf(wave) {
    return (wave.childNodes || []).map((b) => Number(String(b.style.height).replace('px', '')));
}

const msg = (media) => ({ id: 7001, dialog_id: '-1001234', media });

// ---------- голосовое с огибающей от Telegram ----------
const real = M.buildMediaCard(msg({ kind: 'voice', mime: 'audio/ogg', waveform: [0, 128, 255] }));
const realWave = pick(real, 'tg-media-wave');
ok(realWave !== null, 'карточка голосового рисует волну');
eq(heightsOf(realWave), W.waveHeights([0, 128, 255], 0),
    'столбики соответствуют значениям массива, а не шуму');
ok(pick(real, 'tg-media-icon').hidden === true,
    'при волне generic-иконка спрятана');
eq(realWave.getAttribute('aria-hidden'), 'true', 'волна скрыта от скринридера');

// ---------- аудио без атрибута: заглушка, пока бэкенд не досчитал ----------
const stub = M.buildMediaCard(msg({ kind: 'audio', mime: 'audio/mpeg', waveform: [] }));
const stubWave = pick(stub, 'tg-media-wave');
ok(stubWave !== null, 'без огибающей волна всё равно есть — заглушка');
eq(heightsOf(stubWave), W.waveHeights([], W.noiseSeed(7001)),
    'пустой waveform даёт ПРЕЖНЮЮ заглушку (соль без индекса — у Telegram вложение одно)');
eq(heightsOf(pick(M.buildMediaCard(msg({ kind: 'audio', waveform: [] })), 'tg-media-wave')),
    heightsOf(stubWave), 'заглушка детерминирована: карточка не «дышит» при перерисовке');
// Поля waveform нет вовсе (старый кэш ленты, ответ до Ф2) — тоже заглушка.
eq(heightsOf(pick(M.buildMediaCard(msg({ kind: 'audio' })), 'tg-media-wave')),
    heightsOf(stubWave), 'отсутствующее поле неотличимо от пустого');

// ---------- посчитанная на бэке огибающая доехала ----------
const computed = Array.from({ length: 200 }, (_, i) => (i < 100 ? 10 : 250));
const backend = M.buildMediaCard(msg({ kind: 'audio', waveform: computed }));
const backendWave = pick(backend, 'tg-media-wave');
eq(heightsOf(backendWave), W.waveHeights(computed, 0),
    'посчитанная на бэке волна рисуется как есть');
ok(backendWave.childNodes.length <= W.WAVE_BARS,
    `массив из 200 отсчётов прорежен до ${W.WAVE_BARS}`);
ok(JSON.stringify(heightsOf(backendWave)) !== JSON.stringify(heightsOf(stubWave)),
    'настоящая волна отличается от заглушки — иначе ветка не работает');

// ---------- виды без звука ----------
for (const kind of ['photo', 'video', 'document', 'sticker', 'other']) {
    ok(pick(M.buildMediaCard(msg({ kind })), 'tg-media-wave') === null,
        `у вложения «${kind}» волны нет`);
}
ok(M.buildMediaCard(msg(null)) === null, 'сообщение без вложения карточки не даёт');

// ---------- итог ----------
console.log(`${checks - failures}/${checks} проверок прошло (frontend/telegram_voice_wave)`);
if (failures > 0) console.error(`${failures} проверок упало`);
process.exit(failures > 0 ? 1 : 0);
