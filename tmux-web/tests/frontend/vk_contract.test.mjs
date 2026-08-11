// СТРАЖ КОНТРАКТА ПАНЕЛИ «ВКОНТАКТЕ» (АЧ-Ф6).
//
// Панель собрана из артефактов, между которыми НЕТ ни компилятора, ни типов,
// ни единого теста, который бы их сводил:
//
//   • `plugins/vk/src/ws/protocol.rs`  — enum `VkServerMsg` (кадры WS);
//   • `static/js/vk/ws.js`             — константа `SERVER_FRAMES`;
//   • `static/js/core/dom.js`          — блок `$vk*` (DOM-ref'ы);
//   • `static/index.html`              — разметка панели;
//   • `plugins/vk/src/routes/mod.rs`   — таблица путей REST;
//   • `static/js/vk/api.js`            — та же таблица глазами фронта;
//   • `static/css/vk.css`              — стили ТОЛЬКО этой панели.
//
// Расхождение любой пары НЕ ломает сборку и НЕ пишет ни строчки в консоль:
//   — незнакомый кадр WS уходит в `console.debug` и молча теряется;
//   — `getElementById` с опечаткой возвращает `null` НАВСЕГДА (ref'ы
//     вычисляются один раз при импорте модуля), и кнопка просто не реагирует;
//   — разъехавшийся путь даёт 404, который гейт панели читает как «плагин
//     выключен» и оставляет вкладку скрытой;
//   — чужой селектор в vk.css красит соседнюю панель, и виноватой выглядит
//     она.
//
// Цена каждой ошибки — молча неработающая вкладка у пользователя: симптом без
// диагноза. Этот тест превращает её в красный прогон с указанием КОНКРЕТНОГО
// расхождения (какой кадр лишний, какой id отсутствует, какой путь разъехался).
//
// Сравнение ТЕКСТОВОЕ, а не через импорт модулей: `js/vk/ws.js` тянет
// `core/auth.js`, который на верхнем уровне трогает `localStorage` и подменяет
// `window.fetch`. В голом Node он не грузится, и городить браузерные заглушки
// ради одной константы дороже, чем прочитать файл. Тот же приём у Битрикса
// (`bitrix_contract.test.mjs`), этот файл — его близнец для VK.
//
// Запуск: node tmux-web/tests/frontend/vk_contract.test.mjs
// Код возврата 0 — расхождений нет, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');

let failures = 0;
let checks = 0;

function eq(name, actual, expected, hint) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) {
        console.log(`[ok]   ${name}`);
        return;
    }
    failures += 1;
    console.log(
        `[FAIL] ${name}\n       actual:   ${a}\n       expected: ${e}${hint ? `\n       ${hint}` : ''}`,
    );
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

/** Вырезает блок-комментарии и строчные `//` — в шапках соседи упоминаются намеренно. */
function stripComments(src) {
    return src
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .split('\n')
        .filter((l) => !l.trimStart().startsWith('//'))
        .join('\n');
}

// ===========================================================================
// 1. Кадры WS: enum VkServerMsg ↔ SERVER_FRAMES
//
// У VK нет замороженного `vec![...]` со списком тегов (как у Битрикса) —
// источник истины здесь сам enum, а `#[serde(rename_all = "snake_case")]`
// делает тег из имени варианта механически. Поэтому и разбираем варианты.
// ===========================================================================

const protocolRs = read('../../../plugins/vk/src/ws/protocol.rs');
const wsJs = read('../../static/js/vk/ws.js');

/** Имена вариантов `enum VkServerMsg` в порядке объявления, в snake_case. */
function rustFrameTags(src) {
    const head = src.indexOf('pub enum VkServerMsg {');
    if (head < 0) throw new Error('в protocol.rs не найден `pub enum VkServerMsg`');
    const body = src.slice(head);
    const end = body.indexOf('\n}\n');
    if (end < 0) throw new Error('не найден конец объявления VkServerMsg');
    const lines = body.slice(0, end).split('\n');

    // Явный `#[serde(rename = "...")]` на варианте сломал бы вывод тега из
    // имени — тогда этот разборщик врал бы, и лучше упасть громко.
    for (const line of lines) {
        if (/^\s+#\[serde\(rename\s*=/.test(line)) {
            throw new Error(`вариант с явным serde(rename): ${line.trim()}`);
        }
    }
    ok(
        'теги кадров выводятся из имён вариантов (rename_all = snake_case)',
        src.includes('#[serde(tag = "type", rename_all = "snake_case")]'),
        'без этого атрибута теги в JSON были бы CamelCase, и разбор ниже врал бы',
    );

    return lines
        // Варианты объявлены на ЧЕТЫРЁХ пробелах; поля внутри — на восьми,
        // поэтому отступ здесь и есть признак уровня.
        .map((l) => /^ {4}([A-Z][A-Za-z0-9]*)\s*[({,]/.exec(l))
        .filter(Boolean)
        .map((m) => m[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
}

/** Список строк из `export const SERVER_FRAMES = [...]`. */
function jsFrameTags(src) {
    const marker = 'export const SERVER_FRAMES = [';
    const start = src.indexOf(marker);
    if (start < 0) throw new Error('в js/vk/ws.js не найдена константа SERVER_FRAMES');
    const end = src.indexOf('];', start);
    const block = src.slice(start + marker.length, end);
    return [...block.matchAll(/'([a-z_]+)'/g)].map((m) => m[1]);
}

const rustTags = rustFrameTags(protocolRs);
const jsTags = jsFrameTags(wsJs);

ok('в protocol.rs объявлен непустой набор кадров', rustTags.length > 0, `найдено ${rustTags.length}`);
eq(
    'SERVER_FRAMES совпадает с VkServerMsg (состав И ПОРЯДОК)',
    jsTags,
    rustTags,
    'порядок значим: js/vk/ws.js пишется таблицей по этому enum\'у, и перестановка превращает diff двух панелей в шум',
);

ok(
    'первый кадр соединения — login_state',
    rustTags[0] === 'login_state',
    `первым объявлен ${rustTags[0]}: WS обязан слать состояние подключения снимком`,
);
ok(
    'resync и ping объявлены (Lagged и heartbeat)',
    rustTags.includes('resync') && rustTags.includes('ping'),
);

// Кадры анализа чата (АЧ-Ф3) — отдельным блоком: их порядок заморожен в
// rust-тесте `analysis_frames_sit_between_dialog_changed_and_resync`, и та же
// четвёрка обязана стоять подряд во фронте.
eq(
    'кадры анализа стоят подряд между dialog_changed и resync',
    rustTags.slice(rustTags.indexOf('dialog_changed') + 1, rustTags.indexOf('resync')),
    ['analysis_started', 'analysis_delta', 'analysis_done', 'analysis_error'],
);

// Кадры Telegram, которых у VK нет намеренно: их появление во фронте означало
// бы обработчик, который никогда не сработает.
for (const alien of ['notify_defaults_changed', 'folders_changed']) {
    ok(
        `кадр ${alien} не воскрес (у VK его нет)`,
        !jsTags.includes(alien) && !rustTags.includes(alien),
    );
}

// ===========================================================================
// 2. DOM-контракт: блок $vk* в dom.js ↔ index.html
// ===========================================================================

const domJs = read('../../static/js/core/dom.js');
const indexHtml = read('../../static/index.html');

const vkIds = [...domJs.matchAll(/export const \$vk\w*\s*=\s*document\.getElementById\('([^']+)'\)/g)]
    .map((m) => m[1]);

ok('блок $vk* в dom.js не пуст', vkIds.length > 40, `найдено ${vkIds.length} ref'ов`);
ok(
    'все ref\'ы блока адресуют узлы панели',
    vkIds.every((id) => id === 'vk' || id.startsWith('vk-')),
    `чужие id: ${vkIds.filter((id) => id !== 'vk' && !id.startsWith('vk-')).join(', ')}`,
);

const missing = vkIds.filter((id) => !indexHtml.includes(`id="${id}"`));
eq(
    'каждый ref из dom.js существует в index.html',
    missing,
    [],
    'ref вычисляется ОДИН раз при импорте: опечатка даёт null навсегда, без ошибки в консоли',
);

const duplicated = vkIds.filter((id) => indexHtml.split(`id="${id}"`).length - 1 !== 1);
eq('ни один id панели не объявлен дважды', duplicated, []);

// Попап анализа (АЧ-Ф4) — отдельно и поимённо: его узлы добавлялись позже
// всех, и именно они с наибольшей вероятностью разъедутся при правке разметки.
const analysisIds = vkIds.filter((id) => id.startsWith('vk-analysis'));
ok(
    'узлы попапа анализа объявлены (кнопка, попап, обе вкладки, оба пейна)',
    ['vk-analysis-toggle', 'vk-analysis', 'vk-analysis-tab-run', 'vk-analysis-tab-ctx',
        'vk-analysis-pane-run', 'vk-analysis-pane-ctx'].every((id) => analysisIds.includes(id)),
    `есть: ${analysisIds.join(', ')}`,
);
ok(
    'попап анализа скрыт в разметке атрибутом hidden',
    /<div id="vk-analysis"[^>]*\shidden/.test(indexHtml),
    'попап обязан быть в разметке ВСЕГДА (ref\'ы резолвятся при импорте) и закрыт по умолчанию',
);
ok(
    'панель присутствует в разметке всегда и скрыта как вкладка',
    /<div id="vk" class="tab-pane" hidden/.test(indexHtml),
);
ok(
    'кнопка вкладки скрыта — её показывает только гейт по /api/vk/status',
    /<button id="vk-toggle"[^>]*\shidden/.test(indexHtml),
);

// ===========================================================================
// 3. Контракт путей: routes/mod.rs ↔ js/vk/api.js
//
// Самая тихая из поломок: фронт получает 404, гейт читает его как «плагин
// выключен» и молча оставляет кнопку вкладки скрытой.
//
// Имена path-параметров у двух таблиц РАЗНЫЕ намеренно (`:peer_id` против
// `:peer` — серверная и клиентская лексика), поэтому сравниваются пути с
// обезличенными параметрами: значим СОСТАВ маршрутов, а не то, как назван
// сегмент.
// ===========================================================================

const routesRs = read('../../../plugins/vk/src/routes/mod.rs');
const apiJs = read('../../static/js/vk/api.js');

/** `/api/vk/dialogs/:id/messages/:mid` → `/api/vk/dialogs/:/messages/:`. */
const normalize = (p) => p.replace(/:[a-z_]+/g, ':');

/** Пары «метод путь» из markdown-таблицы в шапке routes/mod.rs. */
function rustRoutes(src) {
    return [...src.matchAll(/^\/\/! \| (GET|POST|PUT|DELETE)\s+\| `([^`]+)`/gm)]
        .map((m) => `${m[1]} ${normalize(m[2])}`);
}

/** Пары «метод путь» из замороженной таблицы в шапке api.js. */
function jsRoutes(src) {
    const head = src.slice(0, src.indexOf('import '));
    return [...head.matchAll(/^\/\/\s{3}(GET|POST|PUT|DELETE)\s+(\/[^\s]+)/gm)]
        .map((m) => `${m[1]} ${normalize(m[2])}`);
}

const rustPaths = rustRoutes(routesRs);
const jsPaths = jsRoutes(apiJs);

ok('таблица путей в routes/mod.rs не пуста', rustPaths.length >= 15, `найдено ${rustPaths.length}`);
ok('таблица путей в api.js не пуста', jsPaths.length >= 15, `найдено ${jsPaths.length}`);

// Маршруты, которых во фронтовой таблице api.js нет ОСОЗНАННО. Список закрытый:
// любое новое расхождение обязано покраснеть, а не молча пополнить исключения.
const NOT_IN_API_JS = [
    // Не REST вовсе: соединение живёт в js/vk/ws.js.
    'GET /ws/vk',
    // Сырое тело + заголовок с именем файла — вызов живёт в js/vk/upload.js
    // (UPLOAD_PATH), потому что идёт мимо разборщика конверта.
    'POST /api/vk/upload',
];

eq(
    'каждый путь из api.js перечислен в таблице routes/mod.rs',
    jsPaths.filter((p) => !rustPaths.includes(p)),
    [],
    'путь во фронте, которого нет на сервере, — это 404, прочитанный гейтом как «плагин выключен»',
);
eq(
    'каждый серверный путь либо есть в api.js, либо назван в списке исключений',
    rustPaths.filter((p) => !jsPaths.includes(p) && !NOT_IN_API_JS.includes(p)),
    [],
    'роут без клиента — мёртвый код; если это намеренно, впиши его в NOT_IN_API_JS с объяснением',
);
eq(
    'список исключений не протух (каждое исключение всё ещё есть на сервере)',
    NOT_IN_API_JS.filter((p) => !rustPaths.includes(p)),
    [],
);

// Четыре роута анализа чата (АЧ-Ф1/Ф3) — поимённо: они самые новые.
for (const route of [
    'GET /api/vk/dialogs/:/prompt',
    'PUT /api/vk/dialogs/:/prompt',
    'POST /api/vk/dialogs/:/analysis',
    'POST /api/vk/dialogs/:/analysis/cancel',
]) {
    ok(`роут анализа объявлен обеими сторонами: ${route}`,
        rustPaths.includes(route) && jsPaths.includes(route));
}

// UPLOAD_PATH из upload.js — вторая точка выхода фронта в сеть, и она обязана
// совпадать с сервером так же строго, как таблица api.js.
ok(
    'UPLOAD_PATH из js/vk/upload.js совпадает с серверным маршрутом',
    read('../../static/js/vk/upload.js').includes("UPLOAD_PATH = '/api/vk/upload'")
        && rustPaths.includes('POST /api/vk/upload'),
);

// Инвариант «браузер на серверы VK не ходит»: и captcha, и медиа идут через
// прокси бэкенда. Прямая ссылка вернула бы CORS-ошибку и утекла бы в Referer.
for (const host of ['api.vk.com', 'vk.com', 'vk.ru']) {
    ok(
        `js/vk/api.js не ссылается на ${host}`,
        !stripComments(apiJs).includes(host),
        'и картинка captcha, и медиа обязаны идти через прокси devforge',
    );
}

// ===========================================================================
// 4. Изоляция стилей: панель не красит соседей
// ===========================================================================

const vkCss = read('../../static/css/vk.css');
const cssCode = stripComments(vkCss);

for (const foreign of ['.tg-', '#tg-', '#telegram', '.b24-', '#bitrix']) {
    ok(
        `css/vk.css не содержит селекторов ${foreign}`,
        !cssCode.includes(foreign),
        'панели не должны уметь красить друг друга: виноватой выглядит соседняя',
    );
}

ok(
    'стили попапа анализа живут в vk.css',
    cssCode.includes('#vk-analysis'),
    'попап — часть панели VK, его правила не должны разъезжаться по общим файлам',
);

// ===========================================================================

console.log(`\n${checks - failures}/${checks} checks passed (frontend/vk_contract)`);
if (failures > 0) process.exit(1);
