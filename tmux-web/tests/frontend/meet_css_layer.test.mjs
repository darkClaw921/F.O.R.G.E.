// ⚠ СТРАЖ СЛОЯ АЛИАСОВ `--m-*` (фронтовая Ф3 forge-meet, задача ФФ3.4).
//
// css/meet.css красит кнопку встречи в ТРЁХ панелях, у каждой своя палитра и
// свои две темы. Файл написан целиком на алиасах `--m-*`, а сам слой объявлен
// по разу на `#telegram`, `#vk` и `#bitrix`.
//
// ЧТО ЛОМАЕТСЯ ТИХО (и потому проверяется здесь):
//
//   1. АЛИАС ЗАБЫТ В ОДНОЙ ИЗ ТРЁХ ПАНЕЛЕЙ. CSS не падает — свойство просто
//      теряется. В Битриксе кнопка окажется без рамки, и узнает об этом
//      пользователь, а не автор.
//   2. АЛИАС ССЫЛАЕТСЯ НА ТОКЕН, КОТОРОГО У ПАНЕЛИ НЕТ. Ровно так и начинается
//      «в VK кнопка «готово» бесцветная»: зелёного в синей палитре нет вовсе.
//   3. ХАРДКОД ЦВЕТА. Литерал переживёт тёмную тему и умрёт в светлой — то
//      есть дефект проявится у того, кто переключит тему, и никогда у автора.
//   4. ПОРЯДОК @import. meet.css обязан идти ПОСЛЕ файлов панелей: он
//      ссылается на их токены и обязан побеждать при равной специфичности.
//
// Запуск: node tmux-web/tests/frontend/meet_css_layer.test.mjs

import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');
const strip = (css) => css.replace(/\/\*[\s\S]*?\*\//g, '');

const meetCss = strip(read('../../static/css/meet.css'));
const styleCss = strip(read('../../static/style.css'));
const panelCss = {
    '#telegram': strip(read('../../static/css/telegram.css')),
    '#vk': strip(read('../../static/css/vk.css')),
    '#bitrix': strip(read('../../static/css/bitrix.css')),
};

let failures = 0;
let checks = 0;

function ok(cond, label, detail) {
    checks += 1;
    if (cond) return;
    failures += 1;
    console.error(`[FAIL] ${label}${detail ? `\n       ${detail}` : ''}`);
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) return;
    failures += 1;
    console.error(`[FAIL] ${label}\n       получено:  ${a}\n       ожидалось: ${e}`);
}

/** Тело правила по точному селектору. */
function blockOf(css, selector) {
    const re = new RegExp(`(^|\\})\\s*${selector.replace(/[#.]/g, '\\$&')}\\s*\\{([^}]*)\\}`, 'm');
    const m = re.exec(css);
    return m ? m[2] : null;
}

/** Объявленные в блоке алиасы: имя → значение. */
function aliasesIn(body) {
    const out = new Map();
    for (const m of body.matchAll(/(--m-[-\w]+)\s*:\s*([^;]+);/g)) {
        out.set(m[1], m[2].trim());
    }
    return out;
}

// ===========================================================================
// 1. Слой объявлен ТРИЖДЫ и одинаково
// ===========================================================================

const layers = new Map();
for (const panel of Object.keys(panelCss)) {
    const body = blockOf(meetCss, panel);
    ok(!!body, `слой алиасов объявлен на ${panel}`);
    layers.set(panel, aliasesIn(body || ''));
}

const names = Array.from(layers.values()).map((m) => Array.from(m.keys()).sort());
ok(names[0].length > 0, 'алиасы в слое вообще есть');
for (let i = 1; i < names.length; i += 1) {
    eq(names[i], names[0],
        `набор алиасов у ${Object.keys(panelCss)[i]} совпадает с ${Object.keys(panelCss)[0]}`);
}

// ===========================================================================
// 2. Каждый алиас указывает на СУЩЕСТВУЮЩИЙ токен своей панели
// ===========================================================================

for (const [panel, aliases] of layers) {
    for (const [name, value] of aliases) {
        const refs = Array.from(value.matchAll(/var\((--[-\w]+)\)/g)).map((m) => m[1]);
        ok(refs.length > 0, `${panel} ${name}: значение — ссылка на токен панели, а не литерал`, value);
        for (const ref of refs) {
            ok(new RegExp(`${ref}\\s*:`).test(panelCss[panel]),
                `${panel} ${name} ссылается на существующий токен ${ref}`,
                'токена нет в палитре панели: свойство молча потеряется, а в светлой теме не переопределится');
        }
    }
}

// ===========================================================================
// 3. Каждый использованный алиас объявлен (и наоборот — мёртвых нет)
// ===========================================================================

const used = new Set(
    Array.from(meetCss.matchAll(/var\((--m-[-\w]+)\)/g)).map((m) => m[1]),
);
const declared = new Set(names[0]);
for (const name of used) {
    ok(declared.has(name), `использованный алиас ${name} объявлен во всех трёх блоках`);
}
for (const name of declared) {
    ok(used.has(name), `объявленный алиас ${name} где-то используется`,
        'мёртвый алиас — это три строки, которые никто не сопровождает');
}

// ===========================================================================
// 4. Ни одного литерала цвета вне слоя алиасов
// ===========================================================================

const body = meetCss
    .replace(/#telegram\s*\{[^}]*\}/g, '')
    .replace(/#vk\s*\{[^}]*\}/g, '')
    .replace(/#bitrix\s*\{[^}]*\}/g, '');
eq(Array.from(body.matchAll(/#[0-9a-fA-F]{3,8}\b/g)).map((m) => m[0]), [],
    'в правилах нет hex-литералов — панели живут в своих палитрах');
eq(Array.from(body.matchAll(/\b(?:rgb|hsl)a?\(/g)).map((m) => m[0]), [],
    'и функциональных литералов цвета тоже нет');

// ===========================================================================
// 5. Порядок @import: meet.css ПОСЛЕ трёх панелей
// ===========================================================================

const at = (name) => styleCss.indexOf(`css/${name}.css`);
ok(at('meet') > 0, 'meet.css подключён в style.css');
for (const p of ['telegram', 'vk', 'bitrix']) {
    ok(at('meet') > at(p), `meet.css импортируется после ${p}.css`,
        'алиасы ссылаются на токены панели, а общие правила обязаны побеждать при равной специфичности');
}

// ===========================================================================
// 6. Обязательная ветка «меньше движения»
// ===========================================================================

ok(/@media\s*\(prefers-reduced-motion:\s*reduce\)/.test(meetCss),
    'ветка prefers-reduced-motion есть');
const reduced = /@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{([\s\S]*?)\n\}/.exec(meetCss);
ok(reduced && /animation:\s*none/.test(reduced[1]),
    'она гасит пульсацию, а не только переходы');
// Селектор правила с пульсацией обязан ЗАКАНЧИВАТЬСЯ глифом: пульсация,
// поднятая на кнопку (и тем более на строку), утащила бы за собой подпись с
// живым таймером — мигающие цифры читать невозможно.
{
    const pulse = /([^{}]+)\{[^{}]*animation:\s*meet-pulse[^{}]*\}/.exec(meetCss);
    ok(!!pulse, 'правило с пульсацией найдено');
    ok(pulse && /\.meet-glyph$/.test(pulse[1].trim()),
        'пульсирует ИМЕННО глиф — селектор заканчивается .meet-glyph',
        pulse ? `селектор: ${pulse[1].trim()}` : '');
}
ok(/\.meet-label\[hidden\]\s*\{[^}]*display:\s*none/.test(meetCss),
    'у пустой подписи явно погашен display — `[hidden]` не работает у элемента со своим display');

// ===========================================================================
// 7. Карточка встречи (попап) — фронтовая Ф4
// ===========================================================================
//
// Три обещания попапа, каждое из которых ломается ТИХО:
//
//   • `position: absolute`, а не `fixed`. `fixed` считается от области
//     просмотра, а не от панели: попап уехал бы поверх шапки приложения и
//     боковой панели, а токены палитры при этом продолжали бы резолвиться
//     (узел-то остался внутри `#telegram`) — то есть выглядел бы «почти
//     правильно» и никакой тест на цвета этого не увидел бы.
//   • `z-index: 6` — слой попапа анализа. Меньше — попап уйдёт под полосу
//     ввода, больше — накроет модалки приложения (7).
//   • у каждого узла со СВОИМ `display` есть явное правило `[hidden]`.
//     Атрибут `hidden` — это `display: none` из таблицы по умолчанию, и любое
//     наше `display: flex` его перебивает. Модуль прячет узлы именно
//     атрибутом, поэтому пропущенное правило означает «закрытый попап виден».

{
    const popup = blockOf(meetCss, '.meet-popup');
    ok(!!popup, 'правило .meet-popup есть');
    ok(popup && /position:\s*absolute/.test(popup),
        '.meet-popup позиционируется absolute — от панели, а не от области просмотра');
    ok(!/position:\s*fixed/.test(meetCss),
        'в файле нет ни одного position: fixed — он вырвал бы попап из панели');
    ok(popup && /z-index:\s*6\b/.test(popup),
        '.meet-popup лежит на слое 6 — там же, где попап анализа');

    // Узлы, которые модуль прячет атрибутом `hidden`. Список ЯВНЫЙ: узнать его
    // из CSS нельзя (правило `display` есть и у тех, кого никогда не прячут), а
    // из JS — только угадыванием. Зато он проверяется в ОБЕ стороны: ниже
    // требуется правило `[hidden]` для каждого пункта, а лишних правил
    // `[hidden]` в файле быть не должно.
    const HIDEABLE = [
        'meet-label', 'meet-popup', 'meet-meta-item', 'meet-player', 'meet-audio',
        'meet-player-note', 'meet-pane', 'meet-summary-error', 'meet-transcript-cap',
        'meet-seg', 'meet-more', 'meet-act', 'meet-error', 'meet-person-name',
        'meet-person-rename',
    ];
    for (const cls of HIDEABLE) {
        const re = new RegExp(`\\.${cls}\\[hidden\\]\\s*\\{[^}]*display:\\s*none`);
        ok(re.test(meetCss), `.${cls}[hidden] явно погашен`,
            'без этого правила атрибут hidden проигрывает нашему display');
    }
    const declaredHidden = Array.from(meetCss.matchAll(/\.([-\w]+)\[hidden\]/g)).map((m) => m[1]);
    for (const cls of new Set(declaredHidden)) {
        ok(HIDEABLE.includes(cls), `правило .${cls}[hidden] относится к узлу, который и правда прячут`,
            'либо узел добавили в модуль и забыли в списке теста, либо правило мёртвое');
    }

    // И третья сторона: список не должен ссылаться на узлы, которых в модулях
    // нет. Иначе он тихо протухает — правила остаются, а прятать уже нечего.
    const js = read('../../static/js/messenger/meet-popup.js') + read('../../static/js/messenger/meet-row.js');
    for (const cls of HIDEABLE) {
        ok(js.includes(cls), `класс ${cls} действительно встречается в модулях карточки и кнопки`,
            'список прячущихся узлов протух: правило есть, узла нет');
    }
}

// ===========================================================================

if (failures === 0) console.log(`[OK] meet_css_layer: ${checks} проверок`);
else console.error(`[FAIL] meet_css_layer: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
