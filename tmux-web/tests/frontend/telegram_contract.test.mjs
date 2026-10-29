// СТРАЖ КОНТРАКТА ПАНЕЛИ «TELEGRAM».
//
// Панель собрана из артефактов, между которыми НЕТ ни компилятора, ни общих
// типов, ни единого теста, который бы их сводил:
//
//   • `plugins/telegram/src/ws/protocol.rs` — enum `TgServerMsg` (кадры WS);
//   • `static/js/telegram/ws.js`            — константа `SERVER_FRAMES`;
//   • `static/js/core/dom.js`               — блок `$tg*` (DOM-ref'ы);
//   • `static/index.html`                   — разметка панели;
//   • `plugins/telegram/src/routes/*.rs`    — axum-роутер и таблица путей;
//   • `static/js/telegram/api.js`           — та же поверхность глазами фронта;
//   • `static/css/telegram.css`             — стили ТОЛЬКО этой панели.
//
// Расхождение любой пары НЕ ломает сборку и НЕ пишет ни строчки в консоль:
//   — незнакомый кадр WS уходит в `console.debug` и молча теряется (транспорт
//     намеренно терпим к неизвестным типам — см. шапку ws.js);
//   — `getElementById` с опечаткой возвращает `null` НАВСЕГДА (ref'ы
//     вычисляются один раз при импорте модуля), и кнопка просто не реагирует;
//   — разъехавшийся путь даёт 404, который гейт панели читает как «плагин
//     выключен» и оставляет вкладку скрытой;
//   — чужой селектор в telegram.css красит соседнюю панель (VK или Битрикс),
//     и виноватой выглядит она.
//
// Цена каждой ошибки — молча неработающая вкладка у пользователя: симптом без
// диагноза. Этот тест превращает её в красный прогон с указанием КОНКРЕТНОГО
// расхождения (какой кадр лишний, какой id отсутствует, какой путь разъехался).
//
// Сравнение ТЕКСТОВОЕ, а не через импорт модулей: `js/telegram/ws.js` тянет
// `core/auth.js`, который на верхнем уровне трогает `localStorage` и подменяет
// `window.fetch`. В голом Node он не грузится, и городить браузерные заглушки
// ради одной константы дороже, чем прочитать файл. Тот же приём у VK
// (`vk_contract.test.mjs`) и Битрикса (`bitrix_contract.test.mjs`), этот файл —
// их близнец для Telegram.
//
// Запуск: node tmux-web/tests/frontend/telegram_contract.test.mjs
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

/**
 * Вырезает блок-комментарии и строчные `//` (и `//!` в Rust).
 *
 * Без этого проверки врут в обе стороны: в шапках файлов соседние панели
 * упоминаются НАМЕРЕННО («не адресуем #vk-*»), а JSDoc'и api.js перечисляют
 * пути прозой — и то, и другое попало бы в выборку как «настоящий код».
 */
function stripComments(src) {
    return src
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .split('\n')
        .filter((l) => !l.trimStart().startsWith('//'))
        .join('\n');
}

// ===========================================================================
// 1. Кадры WS: enum TgServerMsg ↔ SERVER_FRAMES
//
// Источник истины — сам enum: `#[serde(tag = "type", rename_all = "snake_case")]`
// делает тег из имени варианта механически, поэтому разбираем имена вариантов.
//
// Порядок значим не меньше состава: `js/telegram/ws.js` пишется таблицей по
// этому enum'у, и перестановка превращает будущий diff в шум — глазами уже не
// видно, добавили кадр или просто переставили.
// ===========================================================================

const protocolRs = read('../../../plugins/telegram/src/ws/protocol.rs');
const wsJs = read('../../static/js/telegram/ws.js');

/** Имена вариантов `enum TgServerMsg` в порядке объявления, в snake_case. */
function rustFrameTags(src) {
    const head = src.indexOf('pub enum TgServerMsg {');
    if (head < 0) throw new Error('в protocol.rs не найден `pub enum TgServerMsg`');
    const body = src.slice(head);
    const end = body.indexOf('\n}\n');
    if (end < 0) throw new Error('не найден конец объявления TgServerMsg');
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
        // поэтому отступ здесь и есть признак уровня. Хвост `(`, `{` или `,`
        // отсекает unit-варианты вроде `FoldersChanged,` от полей структур.
        .map((l) => /^ {4}([A-Z][A-Za-z0-9]*)\s*[({,]/.exec(l))
        .filter(Boolean)
        .map((m) => m[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
}

/** Список строк из `export const SERVER_FRAMES = [...]`. */
function jsFrameTags(src) {
    const marker = 'export const SERVER_FRAMES = [';
    const start = src.indexOf(marker);
    if (start < 0) throw new Error('в js/telegram/ws.js не найдена константа SERVER_FRAMES');
    const end = src.indexOf('];', start);
    const block = src.slice(start + marker.length, end);
    return [...block.matchAll(/'([a-z_]+)'/g)].map((m) => m[1]);
}

const rustTags = rustFrameTags(protocolRs);
const jsTags = jsFrameTags(wsJs);

ok('в protocol.rs объявлен непустой набор кадров', rustTags.length >= 18, `найдено ${rustTags.length}`);
eq(
    'SERVER_FRAMES совпадает с TgServerMsg (состав И ПОРЯДОК)',
    jsTags,
    rustTags,
    'кадр, о котором фронт не знает, уходит в console.debug и теряется молча',
);

ok(
    'первый кадр соединения — login_state',
    rustTags[0] === 'login_state',
    `первым объявлен ${rustTags[0]}: WS обязан слать состояние логина снимком сразу после апгрейда`,
);
ok(
    'resync и ping объявлены (Lagged и heartbeat)',
    rustTags.includes('resync') && rustTags.includes('ping'),
    'без resync клиент после переполнения broadcast остаётся с устаревшим состоянием навсегда',
);
eq(
    'resync и ping — два последних кадра',
    rustTags.slice(-2),
    ['resync', 'ping'],
    'служебные кадры стоят в хвосте у всех трёх мессенджеров: так три протокола читаются одинаково',
);

// Кадры анализа чата — отдельным блоком: их порядок заморожен комментарием в
// самом protocol.rs («ПОРЯДОК этих четырёх вариантов заморожен»), и та же
// четвёрка обязана стоять подряд непосредственно перед resync.
eq(
    'кадры анализа стоят подряд между folders_changed и resync',
    rustTags.slice(rustTags.indexOf('folders_changed') + 1, rustTags.indexOf('resync')),
    ['analysis_started', 'analysis_delta', 'analysis_done', 'analysis_error'],
    'вставка постороннего кадра в середину блока анализа разъедет ws.js и main.js::buildHandlers',
);

// Кадры, специфичные ИМЕННО для Telegram: их отсутствие означало бы, что
// платформенная особенность (папки, глобальный mute категорий) потерялась.
for (const own of ['folders_changed', 'notify_defaults_changed', 'notify_settings']) {
    ok(
        `кадр ${own} объявлен обеими сторонами`,
        rustTags.includes(own) && jsTags.includes(own),
    );
}

// Кадры VK/Битрикса, которых у Telegram нет намеренно: их появление во фронте
// означало бы обработчик, который никогда не сработает.
for (const alien of ['dialog_changed', 'counters', 'chat_updated']) {
    ok(
        `кадр ${alien} не воскрес (у Telegram его нет)`,
        !jsTags.includes(alien) && !rustTags.includes(alien),
    );
}

// Каждый кадр обязан иметь обработчик (или осознанно его не иметь) в
// main.js::buildHandlers — иначе кадр приходит и молча тонет.
const mainJs = read('../../static/js/telegram/main.js');
const handlersSrc = (() => {
    const i = mainJs.indexOf('function buildHandlers');
    if (i < 0) return '';
    // Конец — по закрывающей скобке САМОЙ карты (`\n    };`), а не по
    // фиксированному окну в символах: карта кадров растёт с каждой фичей, и
    // окно однажды обрезало бы её хвост, объявляя живые обработчики
    // отсутствующими. По `return local.handlers;` искать нельзя — первое
    // вхождение стоит в мемоизации на второй строке функции.
    const end = mainJs.indexOf('\n    };', i);
    return mainJs.slice(i, end < 0 ? i + 6000 : end);
})();
ok('в main.js найдена buildHandlers', handlersSrc.length > 0);
// `ping` разбирается самим транспортом (ws.js меряет по нему задержку и
// отвечает pong), поэтому обработчика в карте у него нет и быть не должно.
const FRAMES_WITHOUT_HANDLER = ['ping'];
eq(
    'у каждого кадра есть обработчик в buildHandlers (кроме служебного ping)',
    jsTags.filter((t) => !FRAMES_WITHOUT_HANDLER.includes(t) && !handlersSrc.includes(`${t}:`)),
    [],
    'кадр без обработчика доходит до браузера и молча теряется — симптом без диагноза',
);

// ===========================================================================
// 2. DOM-контракт: блок $tg* в dom.js ↔ index.html, В ОБЕ СТОРОНЫ
//
// Ref'ы вычисляются ОДИН раз при импорте модуля: опечатка в id даёт `null`
// навсегда, без единой строчки в консоли, и кнопка просто перестаёт нажиматься.
// Обратное направление (id в разметке без ref'а) ловит другую беду — мёртвый
// узел, который никто не читает: чаще всего это забытый после правки остаток.
// ===========================================================================

const domJs = read('../../static/js/core/dom.js');
const indexHtml = read('../../static/index.html');

/** Пары «имя ref'а → id» из блока `export const $tg… = getElementById('…')`. */
const tgRefs = [...domJs.matchAll(
    /export const (\$tg\w*)\s*=\s*document\.getElementById\('([^']+)'\)/g,
)].map((m) => ({ ref: m[1], id: m[2] }));

const tgIds = tgRefs.map((r) => r.id);

ok('блок $tg* в dom.js не пуст', tgIds.length > 60, `найдено ${tgIds.length} ref'ов`);
ok(
    'все ref\'ы блока адресуют узлы панели (префикс tg-)',
    tgIds.every((id) => id.startsWith('tg-')),
    `чужие id: ${tgIds.filter((id) => !id.startsWith('tg-')).join(', ')}`,
);

// ПРАВИЛО ИМЕНОВАНИЯ, выведенное по факту из существующего кода:
// `$tgAnalysisClearThread` → снять `$` → camelCase в kebab-case →
// `tg-analysis-clear-thread`. Оно держится на ВСЕХ ref'ах блока без единого
// исключения, и это не украшательство: пока правило механическое, id можно
// найти по имени ref'а (и наоборот) простым grep'ом, не листая dom.js.
const kebab = (ref) => ref.slice(1).replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();
eq(
    'имя ref\'а механически переводится в id (camelCase → kebab-case)',
    tgRefs.filter((r) => kebab(r.ref) !== r.id).map((r) => `${r.ref} ≠ ${r.id}`),
    [],
    'исключение из правила пришлось бы держать в голове при каждой правке разметки',
);

const missing = tgIds.filter((id) => !indexHtml.includes(`id="${id}"`));
eq(
    'каждый ref из dom.js существует в index.html',
    missing,
    [],
    'ref вычисляется ОДИН раз при импорте: опечатка даёт null навсегда, без ошибки в консоли',
);

const duplicated = tgIds.filter((id) => indexHtml.split(`id="${id}"`).length - 1 !== 1);
eq('ни один id панели не объявлен дважды', duplicated, [],
    'два узла с одним id — это getElementById, возвращающий ПЕРВЫЙ, а обновляется на глаз второй');

// Обратное направление: id `tg-*` из разметки, для которых нет ref'а.
// Список исключений ЗАКРЫТЫЙ: это чисто структурные узлы, к которым JS не
// обращается вовсе — они существуют только как якоря CSS-раскладки.
const ID_WITHOUT_REF = [
    // Корневая сетка панели: три строки (titlebar / body / statusbar).
    // Ссылок из JS нет — раскладку целиком делает telegram.css.
    'tg-body',
    'tg-statusbar',
    // Колонка чата (заголовок + лог + композер) как единый flex-контейнер:
    // адресуется только из CSS, в том числе для мобильной раскладки.
    'tg-session',
];
const htmlIds = [...new Set([...indexHtml.matchAll(/id="(tg-[^"]+)"/g)].map((m) => m[1]))];
eq(
    'каждый id панели из index.html либо имеет ref, либо назван в списке исключений',
    htmlIds.filter((id) => !tgIds.includes(id) && !ID_WITHOUT_REF.includes(id)),
    [],
    'id без ref\'а — либо забытый после правки узел, либо ref, потерянный при рефакторинге dom.js',
);
eq(
    'список исключений не протух (каждое исключение всё ещё есть в разметке)',
    ID_WITHOUT_REF.filter((id) => !htmlIds.includes(id)),
    [],
);

// Контейнер вкладки и её кнопка названы БЕЗ префикса `tg-` (`telegram` /
// `telegram-toggle`) — так же, как у VK (`vk` / `vk-toggle`): это узлы общей
// табличной раскладки хоста, а не внутренности панели. Поэтому их ref'ы
// (`$telegram`, `$telegramToggle`) не попадают в выборку выше и проверяются
// поимённо.
ok(
    'ref\'ы контейнера вкладки объявлены в dom.js',
    domJs.includes("export const $telegram = document.getElementById('telegram')")
        && domJs.includes("export const $telegramToggle = document.getElementById('telegram-toggle')"),
);
ok(
    'панель присутствует в разметке всегда и скрыта как вкладка',
    /<div id="telegram" class="tab-pane" hidden/.test(indexHtml),
    'панель обязана быть в DOM всегда: ref\'ы резолвятся при импорте, до любого гейта',
);
ok(
    'кнопка вкладки скрыта — её показывает только гейт по /api/telegram/status',
    /<button id="telegram-toggle"[^>]*\shidden/.test(indexHtml),
);

// Попап анализа — отдельно и поимённо: его узлы добавлялись позже всех, и
// именно они с наибольшей вероятностью разъедутся при правке разметки.
const analysisIds = tgIds.filter((id) => id.startsWith('tg-analysis'));
ok(
    'узлы попапа анализа объявлены (кнопка, попап, обе вкладки, оба пейна)',
    ['tg-analysis-toggle', 'tg-analysis', 'tg-analysis-tab-run', 'tg-analysis-tab-ctx',
        'tg-analysis-pane-run', 'tg-analysis-pane-ctx'].every((id) => analysisIds.includes(id)),
    `есть: ${analysisIds.join(', ')}`,
);
ok(
    'узлы попапа анализа зеркалят VK-шные ($vkAnalysis* ↔ $tgAnalysis*)',
    (() => {
        const vk = [...domJs.matchAll(/export const \$vkAnalysis(\w*)\s*=/g)].map((m) => m[1]).sort();
        const tg = [...domJs.matchAll(/export const \$tgAnalysis(\w*)\s*=/g)].map((m) => m[1]).sort();
        return JSON.stringify(vk) === JSON.stringify(tg);
    })(),
    'попап и его ядро (js/messenger/analysis-core.js) общие: разъезд имён означает «в VK работает, в Telegram нет»',
);
ok(
    'попап анализа скрыт в разметке атрибутом hidden',
    /<div id="tg-analysis"[^>]*\shidden/.test(indexHtml),
    'попап обязан быть в разметке ВСЕГДА (ref\'ы резолвятся при импорте) и закрыт по умолчанию',
);

// ===========================================================================
// 3. Контракт путей: таблица в routes/mod.rs ↔ axum-роутер ↔ js/telegram/api.js
//
// Самая тихая из поломок: фронт получает 404, гейт читает его как «плагин
// выключен» и молча оставляет кнопку вкладки скрытой.
//
// Проверяются ТРИ отношения, а не два:
//   таблица ↔ реально зарегистрированные `.route(...)` — иначе таблица
//     превращается в художественный текст, и сверять по ней бессмысленно;
//   таблица → api.js и api.js → таблица — собственно контракт фронта.
// ===========================================================================

const routesRs = read('../../../plugins/telegram/src/routes/mod.rs');
const apiJs = read('../../static/js/telegram/api.js');

/** `/api/telegram/dialogs/:id/messages/:mid` → `/api/telegram/dialogs/:/messages/:`. */
const normalize = (p) => p.replace(/:[a-z_]+/g, ':');

/** Пары «метод путь» из markdown-таблицы в шапке routes/mod.rs. */
function tableRoutes(src) {
    return [...src.matchAll(/^\/\/! \| (GET|POST|PUT|DELETE)\s+\| `([^`]+)`/gm)]
        .map((m) => `${m[1]} ${normalize(m[2])}`);
}

/**
 * Реально зарегистрированные роуты: все `.route("path", get(..).post(..))`
 * из `plugins/telegram/src/routes/*.rs`.
 *
 * Скобки считаются вручную, а не регуляркой: вызовы бывают многострочными
 * (`.route(\n  "…",\n  get(a).post(b),\n)`), и «до первой закрывающей» здесь
 * дало бы обрезанный хвост с потерянным методом.
 */
function registeredRoutes(files) {
    const out = [];
    for (const src of files) {
        const code = stripComments(src);
        let from = 0;
        for (;;) {
            const at = code.indexOf('.route(', from);
            if (at < 0) break;
            let depth = 0;
            let end = at + '.route('.length - 1;
            for (; end < code.length; end += 1) {
                if (code[end] === '(') depth += 1;
                else if (code[end] === ')') {
                    depth -= 1;
                    if (depth === 0) break;
                }
            }
            const seg = code.slice(at, end + 1);
            from = end + 1;
            const path = /"([^"]+)"/.exec(seg);
            if (!path) continue;
            const methods = [...seg.slice(path.index + path[0].length)
                .matchAll(/\b(get|post|put|delete|patch)\(/g)].map((m) => m[1].toUpperCase());
            for (const method of methods) out.push(`${method} ${normalize(path[1])}`);
        }
    }
    return out;
}

/**
 * Пары «метод путь», которые РЕАЛЬНО дёргает js/telegram/api.js.
 *
 * Разбор идёт по коду, а не по таблице в шапке (в отличие от VK — у Telegram
 * такой таблицы в api.js нет вовсе). Две тонкости:
 *   • шаблонные литералы бывают склеены через `+` на границе строк
 *     (`` `…/dialogs/${id}` + `/messages/${mid}` ``) — склеиваем обратно,
 *     иначе половина пути потерялась бы;
 *   • метод берётся из `jsonInit('METHOD', …)` того же вызова; окно поиска
 *     обрывается на первом `;`, то есть на конце ОПЕРАТОРА — иначе GET-вызов
 *     подцепил бы jsonInit соседней функции ниже по файлу.
 */
function apiJsRoutes(src) {
    const code = stripComments(src).replace(/`\s*\+\s*`/g, '');
    const out = [];
    for (const m of code.matchAll(/[`'](\/(?:api|ws)\/telegram[^`']*)[`']/g)) {
        const raw = m[1].replace(/\$\{[^}]*\}/g, ':').replace(/\?.*$/, '');
        const tail = code.slice(m.index + m[0].length);
        const stmt = tail.slice(0, tail.indexOf(';') < 0 ? 0 : tail.indexOf(';'));
        const method = /jsonInit\('(GET|POST|PUT|DELETE)'/.exec(stmt);
        out.push(`${method ? method[1] : 'GET'} ${normalize(raw)}`);
    }
    return [...new Set(out)];
}

const tablePaths = tableRoutes(routesRs);
const routerPaths = registeredRoutes([
    routesRs,
    read('../../../plugins/telegram/src/routes/login.rs'),
    read('../../../plugins/telegram/src/routes/dialogs.rs'),
    read('../../../plugins/telegram/src/routes/media.rs'),
    read('../../../plugins/telegram/src/routes/upload.rs'),
    read('../../../plugins/telegram/src/routes/analysis.rs'),
    read('../../../plugins/telegram/src/routes/voice.rs'),
]);
const jsPaths = apiJsRoutes(apiJs);

ok('таблица путей в routes/mod.rs не пуста', tablePaths.length >= 20, `найдено ${tablePaths.length}`);
ok('axum-роутер разобран', routerPaths.length >= 20, `найдено ${routerPaths.length}`);
ok('пути в api.js найдены', jsPaths.length >= 15, `найдено ${jsPaths.length}`);
eq('строки таблицы не дублируются', tablePaths.filter((p, i) => tablePaths.indexOf(p) !== i), []);

// --- 3a. Таблица ↔ реальный роутер -----------------------------------------
//
// WebSocket в таблицу не входит НАМЕРЕННО (сказано в её же шапке): у него
// другой контракт — кадры `TgServerMsg`, которые сверяет ось 1 этого файла.
const NOT_IN_TABLE = ['GET /ws/telegram'];

eq(
    'каждый зарегистрированный роут описан в таблице',
    routerPaths.filter((p) => !tablePaths.includes(p) && !NOT_IN_TABLE.includes(p)),
    [],
    'роут вне таблицы — это роут, которого не увидит ни ревьюер, ни этот тест',
);
eq(
    'в таблице нет строк без роута',
    tablePaths.filter((p) => !routerPaths.includes(p)),
    [],
    'строка-призрак хуже отсутствия таблицы: фронт напишут по ней и получат 404',
);
eq(
    'список NOT_IN_TABLE не протух (исключение всё ещё зарегистрировано)',
    NOT_IN_TABLE.filter((p) => !routerPaths.includes(p)),
    [],
);

// --- 3b. Таблица ↔ api.js --------------------------------------------------
//
// Маршруты, которых во фронтовом api.js нет ОСОЗНАННО. Список закрытый: любое
// новое расхождение обязано покраснеть, а не молча пополнить исключения.
const NOT_IN_API_JS = [
    // Сырое тело + заголовок с именем файла: вызов идёт мимо разборщика
    // конверта и живёт в js/telegram/upload.js (UPLOAD_PATH), потому что
    // нужен XHR с прогрессом, а не fetch.
    'POST /api/telegram/upload',
];

eq(
    'каждый путь из api.js перечислен в таблице routes/mod.rs',
    jsPaths.filter((p) => !tablePaths.includes(p)),
    [],
    'путь во фронте, которого нет на сервере, — это 404, прочитанный гейтом как «плагин выключен»',
);
eq(
    'каждый путь таблицы либо есть в api.js, либо назван в списке исключений',
    tablePaths.filter((p) => !jsPaths.includes(p) && !NOT_IN_API_JS.includes(p)),
    [],
    'роут без клиента — мёртвый код; если это намеренно, впиши его в NOT_IN_API_JS с объяснением',
);
eq(
    'список NOT_IN_API_JS не протух (каждое исключение всё ещё есть в таблице)',
    NOT_IN_API_JS.filter((p) => !tablePaths.includes(p)),
    [],
);

// Семь роутов анализа чата — поимённо: они самые новые и обязаны БАЙТ В БАЙТ
// повторять VK-шные (попап и его ядро общие, см. шапку блока анализа в api.js).
for (const route of [
    'GET /api/telegram/dialogs/:/prompt',
    'PUT /api/telegram/dialogs/:/prompt',
    'GET /api/telegram/dialogs/:/analysis',
    'POST /api/telegram/dialogs/:/analysis',
    'DELETE /api/telegram/dialogs/:/analysis',
    'POST /api/telegram/dialogs/:/analysis/cancel',
    'DELETE /api/telegram/dialogs/:/analysis/thread',
]) {
    ok(`роут анализа объявлен всеми тремя сторонами: ${route}`,
        tablePaths.includes(route) && routerPaths.includes(route) && jsPaths.includes(route),
        `таблица:${tablePaths.includes(route)} роутер:${routerPaths.includes(route)} api.js:${jsPaths.includes(route)}`);
}

// UPLOAD_PATH из upload.js — вторая точка выхода фронта в сеть (XHR ради
// прогресса), и она обязана совпадать с сервером так же строго, как api.js.
ok(
    'UPLOAD_PATH из js/telegram/upload.js совпадает с серверным маршрутом',
    read('../../static/js/telegram/upload.js').includes("UPLOAD_PATH = '/api/telegram/upload'")
        && routerPaths.includes('POST /api/telegram/upload'),
);

// Адрес сокета — третья точка выхода; в таблицу он не входит, но разъехаться
// с сервером может ровно так же.
ok(
    'js/telegram/ws.js подключается к тому же /ws/telegram, что зарегистрирован',
    wsJs.includes('/ws/telegram') && routerPaths.includes('GET /ws/telegram'),
);

// Инвариант «браузер на серверы Telegram не ходит»: и медиа, и аватарки идут
// через прокси бэкенда. Прямая ссылка вернула бы CORS-ошибку, а заодно утекла
// бы в Referer вместе с сессией.
for (const host of ['t.me', 'telegram.org', 'cdn-telegram.org']) {
    ok(
        `js/telegram/api.js не ссылается на ${host}`,
        !stripComments(apiJs).includes(host),
        'медиа обязаны идти через прокси devforge (/api/telegram/media/...)',
    );
}

// ===========================================================================
// 4. Изоляция стилей: панель не красит соседей
//
// telegram.css грузится всегда, вместе с vk.css и bitrix.css, и порядок
// подключения решает исход. Селектор, задевающий чужую панель, ломает ЕЁ — а
// разбираться пойдут в её файл.
// ===========================================================================

const tgCss = read('../../static/css/telegram.css');
const cssCode = stripComments(tgCss);

for (const foreign of ['#vk', '.vk-', '#b24', '.b24-', '#bitrix']) {
    ok(
        `css/telegram.css не содержит селекторов ${foreign}`,
        !cssCode.includes(foreign),
        'панели не должны уметь красить друг друга: виноватой выглядит соседняя',
    );
}

ok(
    'стили попапа анализа живут в telegram.css',
    cssCode.includes('#tg-analysis'),
    'попап — часть панели Telegram, его правила не должны разъезжаться по общим файлам',
);

// Палитра панели задана токенами `--t-*` в двух темах (data-tg-theme). Хардкод
// цвета в правилах попапа означал бы, что в светлой теме он выглядит иначе —
// и это заметят пользователем, а не тестом.
const analysisCss = cssCode.slice(cssCode.indexOf('#tg-analysis {'));
ok('блок правил попапа анализа найден', analysisCss.length > 200);
// Исключение: `rgba(0, 0, 0, …)` в box-shadow. Тень — не цвет палитры, а
// затемнение фона под карточкой; она одинакова в обеих темах и записана так
// же во ВСЕХ остальных «коробках поверх» этого файла (лайтбокс, dropzone,
// keys-help). Единый приём важнее формальной чистоты.
const hardcoded = [...analysisCss.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((m) => m[0]);
eq(
    'правила попапа анализа не содержат захардкоженных hex-цветов',
    hardcoded,
    [],
    'цвета берутся только из токенов --t-*: иначе попап разъедется со светлой темой панели',
);
ok(
    'единственная не-токенная краска попапа — тень rgba(0,0,0,…)',
    [...analysisCss.matchAll(/rgba\(([^)]*)\)/g)].every((m) => /^\s*0\s*,\s*0\s*,\s*0\s*,/.test(m[1])),
    'цветная rgba() — тот же хардкод, только записанный иначе',
);
ok(
    'попап анализа использует токены палитры --t-*',
    /var\(--t-[a-z-]+\)/.test(analysisCss),
);

// ===========================================================================

console.log(`\n${checks - failures}/${checks} checks passed (frontend/telegram_contract)`);
if (failures > 0) process.exit(1);
