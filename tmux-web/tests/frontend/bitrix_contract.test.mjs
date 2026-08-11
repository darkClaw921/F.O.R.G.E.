// СТРАЖ КОНТРАКТА ПАНЕЛИ «БИТРИКС24» (Фаза 1).
//
// Панель разнесена по трём независимым артефактам, между которыми нет ни
// компилятора, ни типов:
//
//   • `plugins/bitrix/src/ws/protocol.rs` — набор кадров WS (сервер);
//   • `static/js/bitrix/ws.js`            — константа SERVER_FRAMES (фронт);
//   • `static/index.html` + `static/js/core/dom.js` — разметка и DOM-ref'ы.
//
// Расхождение любой пары НЕ ломает сборку: незнакомый кадр уходит в
// `console.debug`, а `getElementById` несуществующего узла возвращает `null`.
// То есть цена ошибки — молча неработающая вкладка у пользователя, симптом
// без диагноза. Этот тест превращает её в красный прогон.
//
// Проверяются ТРИ инварианта:
//
//   1. Набор тегов кадров в protocol.rs (тест `server_frame_tags_are_frozen`)
//      совпадает с SERVER_FRAMES во фронте — по составу и по порядку.
//   2. Каждый id, который резолвит блок `$b24*` в js/core/dom.js, реально
//      присутствует в index.html. DOM-ref'ы вычисляются ОДИН РАЗ при импорте
//      модуля, поэтому опечатка в id даёт `null` навсегда — панель просто не
//      реагирует, без единой ошибки в консоли.
//   3. Панель не красит соседей: в css/bitrix.css нет ни одного селектора с
//      префиксом чужих панелей.
//
// Сравнение текстовое: `js/bitrix/ws.js` импортирует `core/auth.js`, который
// на верхнем уровне трогает `localStorage` и подменяет `window.fetch`, — в
// голом Node он не грузится, и городить браузерные заглушки ради одной
// константы дороже, чем прочитать файл.
//
// Запуск: node tmux-web/tests/frontend/bitrix_contract.test.mjs
// Код возврата 0 — расхождений нет, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');

let failures = 0;
let checks = 0;

function eq(name, actual, expected) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) {
        console.log(`[ok]   ${name}`);
        return;
    }
    failures += 1;
    console.log(`[FAIL] ${name}\n       actual:   ${a}\n       expected: ${e}`);
}

function ok(name, cond, detail) {
    checks += 1;
    if (cond) {
        console.log(`[ok]   ${name}`);
        return;
    }
    failures += 1;
    console.log(`[FAIL] ${name}${detail ? `\n       ${detail}` : ''}`);
}

// ---------------------------------------------------------------------------
// 1. Кадры WS: protocol.rs ↔ js/bitrix/ws.js
// ---------------------------------------------------------------------------

const protocolRs = read('../../../plugins/bitrix/src/ws/protocol.rs');
const wsJs = read('../../static/js/bitrix/ws.js');

/** Список строк из `assert_eq!(tags, vec![...])` в замороженном тесте. */
function rustFrameTags(src) {
    const marker = 'assert_eq!(\n            tags,\n            vec![';
    const start = src.indexOf(marker);
    if (start < 0) {
        throw new Error('в protocol.rs не найден замороженный список тегов');
    }
    const end = src.indexOf(']', start);
    const block = src.slice(start + marker.length, end);
    return [...block.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
}

/** Список строк из `export const SERVER_FRAMES = [...]`. */
function jsFrameTags(src) {
    const marker = 'export const SERVER_FRAMES = [';
    const start = src.indexOf(marker);
    if (start < 0) {
        throw new Error('в js/bitrix/ws.js не найдена константа SERVER_FRAMES');
    }
    const end = src.indexOf('];', start);
    const block = src.slice(start + marker.length, end);
    return [...block.matchAll(/'([a-z_]+)'/g)].map((m) => m[1]);
}

const rustTags = rustFrameTags(protocolRs);
const jsTags = jsFrameTags(wsJs);

ok('в protocol.rs объявлен непустой набор кадров', rustTags.length > 0);
eq('SERVER_FRAMES совпадает с protocol.rs (состав и порядок)', jsTags, rustTags);
ok(
    'первый кадр соединения — login_state',
    rustTags[0] === 'login_state',
    `первым объявлен ${rustTags[0]}: WS обязан слать состояние подключения снимком`,
);
ok(
    'resync и ping объявлены (Lagged и heartbeat)',
    rustTags.includes('resync') && rustTags.includes('ping'),
);

// ---------------------------------------------------------------------------
// 2. DOM-контракт: dom.js ↔ index.html
// ---------------------------------------------------------------------------

const domJs = read('../../static/js/core/dom.js');
const indexHtml = read('../../static/index.html');

const b24Ids = [...domJs.matchAll(/getElementById\('([^']+)'\)/g)]
    .map((m) => m[1])
    .filter((id) => id.startsWith('b24-') || id === 'bitrix' || id === 'bitrix-toggle');

ok('блок $b24* в dom.js не пуст', b24Ids.length > 20, `найдено ${b24Ids.length} ref'ов`);

const missing = b24Ids.filter((id) => !indexHtml.includes(`id="${id}"`));
eq('каждый ref из dom.js существует в index.html', missing, []);

const duplicated = b24Ids.filter((id) => {
    const hits = indexHtml.split(`id="${id}"`).length - 1;
    return hits !== 1;
});
eq('ни один id панели не объявлен дважды', duplicated, []);

ok(
    'панель присутствует в разметке ВСЕГДА и скрыта атрибутом hidden',
    /<div id="bitrix" class="tab-pane" hidden data-b24-theme="dark">/.test(indexHtml),
    'разметка обязана быть безусловной: ref\'ы резолвятся один раз при импорте dom.js',
);
ok(
    'кнопка вкладки в разметке скрыта — её показывает только гейт по /api/bitrix/status',
    /<button id="bitrix-toggle"[^>]*\shidden>/.test(indexHtml),
);

// ---------------------------------------------------------------------------
// 3. Изоляция стилей: панель не красит соседей
// ---------------------------------------------------------------------------

const bitrixCss = read('../../static/css/bitrix.css');
/** Строки CSS без комментариев — в шапке файла соседи упоминаются намеренно. */
const cssCode = bitrixCss.replace(/\/\*[\s\S]*?\*\//g, '');

for (const foreign of ['#telegram', '#vk', '.tg-', '.vk-', '#tg-', '#vk-']) {
    ok(
        `css/bitrix.css не содержит селекторов ${foreign}`,
        !cssCode.includes(foreign),
        'панели не должны уметь красить друг друга',
    );
}

ok(
    'светлая тема переопределяет палитру на самой панели',
    cssCode.includes('#bitrix[data-b24-theme="light"]'),
);

// ---------------------------------------------------------------------------
// 4. Контракт путей: js/bitrix/api.js ↔ таблица в routes/mod.rs
//
// Расхождение здесь — самая тихая из возможных поломок: фронт получает 404,
// глотает его гейтом (404 = «плагин выключен») и молча оставляет кнопку
// вкладки скрытой. Ни ошибки сборки, ни строчки в консоли.
// ---------------------------------------------------------------------------

const apiJs = read('../../static/js/bitrix/api.js');
const routesRs = read('../../../plugins/bitrix/src/routes/mod.rs');

/** Значения объекта PATHS во фронте — то, куда панель реально ходит. */
const jsPaths = [...apiJs.matchAll(/^\s{4}\w+:\s*'(\/api\/bitrix\/[^']+)',$/gm)].map((m) => m[1]);

ok('объект PATHS во фронте не пуст', jsPaths.length >= 3, `найдено ${jsPaths.length}`);

const unknownPaths = jsPaths.filter((p) => !routesRs.includes(`\`${p}\``));
eq('каждый путь из api.js перечислен в таблице routes/mod.rs', unknownPaths, []);

/** Роуты подключения обязаны быть смонтированы, а не только описаны. */
for (const mounted of ['/api/bitrix/login/webhook', '/api/bitrix/logout']) {
    ok(
        `${mounted} смонтирован в routes/login.rs`,
        read('../../../plugins/bitrix/src/routes/login.rs').includes(`"${mounted}"`),
    );
}

/** Имя поля тела запроса подключения — тоже контракт (разъезд даёт 422). */
ok(
    'имя поля тела подключения совпадает с WebhookReq',
    apiJs.includes('{ webhook:')
        && read('../../../plugins/bitrix/src/routes/login.rs').includes('pub webhook: String'),
);

/** Секрет не должен оседать в браузере ни на секунду. */
const loginJs = read('../../static/js/bitrix/login.js');
for (const [name, src] of [['api.js', apiJs], ['login.js', loginJs]]) {
    const code = src.replace(/\/\/.*$/gm, '');
    ok(
        `${name} не трогает localStorage/sessionStorage`,
        !code.includes('localStorage') && !code.includes('sessionStorage'),
        'вебхук — это секрет целиком: сторадж читается расширениями браузера',
    );
}
ok(
    'login.js очищает поле ввода при переходе в authorized',
    /state === 'authorized'[\s\S]{0,800}\$b24Webhook\.value = ''/.test(loginJs),
    'секрет не должен задерживаться в DOM после успеха',
);
// Строки комментариев выброшены НАМЕРЕННО: шапка модуля объясняет этот самый
// запрет и цитирует `split('/rest/')` словами — поиск по сырому тексту
// находил бы объяснение вместо кода.
const loginCode = loginJs
    .split('\n')
    .filter((l) => !l.trimStart().startsWith('//') && !l.trimStart().startsWith('*'))
    .join('\n');
for (const parser of ['.split(', '.match(', 'new RegExp', '.replace(']) {
    ok(
        `login.js не разбирает вставленное значение (${parser})`,
        !loginCode.includes(parser),
        'разбор трёх форм адреса живёт ТОЛЬКО на бэкенде: второй парсер неизбежно разъедется с серверным',
    );
}

// ---------------------------------------------------------------------------

console.log(`\n${checks - failures}/${checks} checks passed`);
if (failures > 0) process.exit(1);
