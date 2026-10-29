// Регресс-тест общей звуковой дорожки голосовых (Ф2 фичи голосовых).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/messenger/waveform-core.js и
// РЕАЛЬНУЮ разметку карточек Bitrix (js/bitrix/attachments.js) поверх
// крошечного фейкового документа: `buildMediaCard` принимает `doc` параметром
// именно ради этого.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. До Ф2 волну умел рисовать только VK, Telegram
// всегда рисовал шум, а Bitrix не имел ветки настоящей огибающей вовсе. Общий
// модуль существует ровно затем, чтобы одна и та же запись выглядела одинаково
// во всех трёх панелях; отсюда два вида проверок:
//
//   • ПОВЕДЕНИЕ: непустой `waveform` рисуется столбиками по значениям массива,
//     пустой — прежней ДЕТЕРМИНИРОВАННОЙ заглушкой (регресс не допускается);
//   • СТРАЖ ЕДИНСТВЕННОСТИ: ни в одной панели не осталось своей копии ветки —
//     иначе три вида карточки неминуемо разъедутся.
//
// Мутационная проверка: поменяйте WAVE_BARS или WAVE_MIN_PX — тест обязан
// покраснеть; уберите ветку настоящей огибающей — покраснеет тоже.
//
// Запуск: node tmux-web/tests/frontend/messenger_waveform_core.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const W = await import(new URL('../../static/js/messenger/waveform-core.js', import.meta.url).href);
const B24 = await import(new URL('../../static/js/bitrix/attachments.js', import.meta.url).href);

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

function read(rel) {
    return readFileSync(new URL(rel, import.meta.url), 'utf8');
}

// ---------------------------------------------------------------------------
// Крошечный фейковый документ: ровно то, чем пользуется разметка карточки.
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
                hidden: false,
                disabled: false,
                textContent: '',
                title: '',
                type: '',
                alt: '',
            };
            node.appendChild = (child) => {
                node.childNodes.push(child);
                return child;
            };
            node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
            return node;
        },
    };
}

/** Все узлы поддерева. */
function walk(node, out = []) {
    out.push(node);
    for (const c of node.childNodes || []) walk(c, out);
    return out;
}

/** Первый узел с классом. */
function pick(root, cls) {
    return walk(root).find((n) => String(n.className || '').split(' ').includes(cls)) || null;
}

/** Высоты столбиков волны, числами. */
function heightsOf(wave) {
    return (wave.childNodes || []).map((b) => Number(String(b.style.height).replace('px', '')));
}

// ---------------------------------------------------------------------------
// 1. Чистые функции: настоящая огибающая против заглушки.
// ---------------------------------------------------------------------------

// Массив короче числа столбиков рисуется КАК ЕСТЬ — по столбику на значение.
eq(W.waveHeights([0, 255], 1), [W.WAVE_MIN_PX, W.WAVE_MIN_PX + W.WAVE_RANGE_PX],
    'столбики соответствуют значениям массива: тишина и потолок');

// Нормализация — по максимуму САМОГО массива: тихая запись не обязана быть
// полоской у нижнего края.
eq(W.waveHeights([10, 20], 1), [W.WAVE_MIN_PX + W.WAVE_RANGE_PX / 2, W.WAVE_MIN_PX + W.WAVE_RANGE_PX],
    'нормализация идёт по пику самого массива, а не по потолку шкалы');

// Массив длиннее потолка панели прореживается, а не рвёт карточку.
const long = W.waveHeights(Array.from({ length: 200 }, (_, i) => i), 1);
ok(long.length <= W.WAVE_BARS, `массив из 200 отсчётов прорежен до ${W.WAVE_BARS}: ${long.length}`);
ok(long.every((h) => h >= W.WAVE_MIN_PX && h <= W.WAVE_MIN_PX + W.WAVE_RANGE_PX),
    'все столбики настоящей волны в пределах карточки');
ok(W.waveHeights(Array.from({ length: 1000 }, () => 5), 1).length <= W.WAVE_BARS,
    'даже тысяча отсчётов не ломает рендер');

// Мусор в массиве — не исключение и не NaN в стиле.
ok(W.waveHeights([null, undefined, 'x', 7], 1).every((h) => Number.isFinite(h)),
    'мусорные значения не дают NaN в высоте столбика');

// Пусто ⇒ ЗАГЛУШКА, ровно WAVE_BARS столбиков и всегда одна и та же.
const noise = W.waveHeights([], W.noiseSeed(7001, 0));
ok(noise.length === W.WAVE_BARS, `заглушка рисует ${W.WAVE_BARS} столбиков: ${noise.length}`);
eq(W.waveHeights([], W.noiseSeed(7001, 0)), noise, 'заглушка ДЕТЕРМИНИРОВАНА: карточка не «дышит»');
ok(JSON.stringify(W.waveHeights([], W.noiseSeed(7001, 1))) !== JSON.stringify(noise),
    'два вложения одного сообщения выглядят по-разному');
ok(JSON.stringify(W.waveHeights([], W.noiseSeed(7002, 0))) !== JSON.stringify(noise),
    'два сообщения выглядят по-разному');
ok(noise.every((h) => h >= W.WAVE_MIN_PX && h < W.WAVE_MIN_PX + W.WAVE_RANGE_PX),
    'заглушка укладывается в ту же высоту, что настоящая волна');
// Не-массив и пустой массив неотличимы: «огибающей нет».
eq(W.waveHeights(null, W.noiseSeed(7001, 0)), noise, 'отсутствующее поле — та же заглушка');

// Соль без индекса (Telegram: вложение в сообщении одно) НЕ равна соли с нулём:
// иначе рисунок всех уже нарисованных заглушек Telegram изменился бы.
ok(W.noiseSeed(7001) !== W.noiseSeed(7001, 0),
    'соль без индекса отличается от соли с индексом 0');

ok(W.hasRealWaveform({ waveform: [1] }) === true, 'hasRealWaveform: непустой массив');
ok(W.hasRealWaveform({ waveform: [] }) === false, 'hasRealWaveform: пустой массив');
ok(W.hasRealWaveform({}) === false, 'hasRealWaveform: поля нет вовсе');
ok(W.hasRealWaveform(null) === false, 'hasRealWaveform: вложения нет');

// ---------------------------------------------------------------------------
// 2. Узел волны.
// ---------------------------------------------------------------------------

const doc = makeDoc();
const node = W.buildWave({ waveform: [1, 2, 3], className: 'x-wave', seed: 1, doc });
eq(node.className, 'x-wave', 'имя класса задаёт панель');
eq(node.attributes['aria-hidden'], 'true', 'волна скрыта от скринридера: это картинка, а не текст');
eq(node.childNodes.length, 3, 'по столбику на значение массива');
ok(node.childNodes.every((b) => /^\d+px$/.test(b.style.height)), 'высота задана в пикселях');

// ---------------------------------------------------------------------------
// 3. Карточка Bitrix: настоящая волна и заглушка.
// ---------------------------------------------------------------------------

const media = (extra) => ({
    index: 1, file_id: 95, kind: 'voice', name: 'voice.mp3', size: 1024, has_full: true, ...extra,
});
const msg = { id: 7001, dialog_id: 'chat123' };

const real = B24.buildMediaCard(media({ waveform: [0, 128, 255] }), makeDoc(), msg);
const realWave = pick(real, 'b24-media-wave');
ok(realWave !== null, 'карточка голосового Bitrix рисует волну');
eq(heightsOf(realWave), W.waveHeights([0, 128, 255], 0),
    'столбики Bitrix соответствуют значениям массива');
ok(pick(real, 'b24-media-icon').hidden === true,
    'при волне generic-иконка спрятана — иначе в теле карточки два изображения');

const stub = B24.buildMediaCard(media({ waveform: [] }), makeDoc(), msg);
const stubWave = pick(stub, 'b24-media-wave');
ok(stubWave !== null, 'без огибающей волна всё равно есть — заглушка');
eq(heightsOf(stubWave), W.waveHeights([], W.noiseSeed(7001, 1)),
    'пустой waveform даёт прежнюю детерминированную заглушку');
eq(heightsOf(pick(B24.buildMediaCard(media({ waveform: [] }), makeDoc(), msg), 'b24-media-wave')),
    heightsOf(stubWave), 'заглушка Bitrix стабильна между перерисовками');

// Аудиофайл (не голосовое) тоже с волной, а обычный файл и картинка — без неё.
ok(pick(B24.buildMediaCard(media({ kind: 'audio' }), makeDoc(), msg), 'b24-media-wave') !== null,
    'у аудио волна тоже рисуется');
for (const kind of ['file', 'image', 'video', 'video_note']) {
    ok(pick(B24.buildMediaCard(media({ kind }), makeDoc(), msg), 'b24-media-wave') === null,
        `у вложения «${kind}» волны нет`);
}

// Массив длиннее потолка и мусор вместо массива не ломают карточку.
const huge = B24.buildMediaCard(
    media({ waveform: Array.from({ length: 500 }, (_, i) => i % 256) }), makeDoc(), msg,
);
ok(pick(huge, 'b24-media-wave').childNodes.length <= W.WAVE_BARS,
    'длинный массив прорежен и в карточке Bitrix');
ok(pick(B24.buildMediaCard(media({ waveform: 'мусор' }), makeDoc(), msg),
    'b24-media-wave').childNodes.length === W.WAVE_BARS,
    'не-массив в поле waveform даёт заглушку, а не пустую карточку');

// Сообщения без идентификатора (черновик, синтезированная строка) тоже рисуются.
ok(pick(B24.buildMediaCard(media({ waveform: [] }), makeDoc(), null), 'b24-media-wave')
    .childNodes.length === W.WAVE_BARS, 'карточка без msg рисует заглушку, а не падает');

// ---------------------------------------------------------------------------
// 4. Страж единственности: ветка живёт в общем модуле, а не в трёх копиях.
// ---------------------------------------------------------------------------

const panels = {
    'vk/media.js': read('../../static/js/vk/media.js'),
    'telegram/media.js': read('../../static/js/telegram/media.js'),
    'bitrix/attachments.js': read('../../static/js/bitrix/attachments.js'),
};
for (const [name, src] of Object.entries(panels)) {
    ok(src.includes("messenger/waveform-core.js"),
        `${name} рисует волну общим модулем`);
    // Магическая константа соли — след своей копии заглушки.
    ok(!src.includes('2654435761'), `${name} не хранит своей копии генератора шума`);
    ok(!/<<\s*13/.test(src), `${name} не хранит своей копии xorshift`);
}

// Стили волны есть у всех трёх панелей: иначе столбики схлопнутся в точку.
for (const [css, cls] of [
    ['../../static/css/vk.css', '.vk-media-wave'],
    ['../../static/css/telegram.css', '.tg-media-wave'],
    ['../../static/css/bitrix.css', '.b24-media-wave'],
]) {
    const text = read(css);
    ok(text.includes(`${cls} {`), `${cls} описан в ${css.split('/').pop()}`);
    ok(text.includes(`${cls} span {`), `${cls} span описан в ${css.split('/').pop()}`);
}

console.log(`${checks - failures}/${checks} проверок прошло (frontend/messenger_waveform_core)`);
if (failures > 0) console.error(`${failures} проверок упало`);
process.exit(failures > 0 ? 1 : 0);
