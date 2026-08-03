// Регресс-тест НАЗВАНИЯ БЕСЕДЫ в колонке VK: имя + приглушённый @ник.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. До него строка списка показывала терминальный
// слаг (`durov@node`, format.js::slugName): имя приводилось к нижнему
// регистру, а всё, что не [a-z0-9_.-], сжималось в `_`. У VK это било сильнее,
// чем у Telegram: русские названия бесед — норма, и «Тестовое сообщество»
// превращалось в `anon@club`. Настоящее имя жило только в подсказке при
// наведении. Пользовательская приёмка: «ники должны быть второстепенными,
// показывайте имена».
//
// Проверяется ТРИ слоя разом, потому что дефект возможен в каждом:
//
//   1. РАЗМЕТКА (dialogs.js::buildRow) — имя, ник отдельным узлом, отсутствие
//      узла при отсутствии screen_name, осмысленная подсказка, сохранность
//      меток [BOT] и [M];
//   2. РАСКЛАДКА (css/vk.css) — порядок жертв при нехватке места: ник
//      ужимается первым, метки не ужимаются никогда, имя получает ellipsis.
//      Node не считает каскад, поэтому проверяются САМИ ПРАВИЛА файла: числа,
//      переписанные в тест руками, разошлись бы с CSS молча;
//   3. ЧИТАЕМОСТЬ — приглушённый ник обязан оставаться текстом, а не фоном.
//      Токен --v-faint-text уже чинили (он был псевдонимом рамочного
//      --v-acc-faint, 1.1–1.3:1), и соседний vk_theme_contrast.test.mjs
//      стережёт сам токен; здесь проверяется, что ЦВЕТ НИКА берётся именно из
//      него и остаётся читаемым на фоне колонки и подсвеченной строки.
//      Формула контраста — из общего слоя (js/messenger/theme-core.js).
//
// Стаб DOM обязан стоять раньше первого импорта js/vk/*: `core/dom.js`
// резолвит ref'ы один раз на верхнем уровне модуля.
//
// Запуск: node tmux-web/tests/frontend/vk_dialog_name.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

import { $communityList, $dialogList } from './vk_dom_stub.mjs';

const userSettings = {};
globalThis.window.ForgeApp = { state: { userSettings } };

const base = new URL('../../static/js/vk/', import.meta.url).href;
const D = await import(`${base}dialogs.js`);
const S = await import(`${base}state.js`);
const { contrastRatio, parseCssColor } = await import(
    new URL('../../static/js/messenger/theme-core.js', import.meta.url).href
);

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

// ---------------------------------------------------------------------------
// Данные
// ---------------------------------------------------------------------------

/** Имя на 70 знаков: столько в колонку не влезает ни при какой ширине. */
const LONG = 'Отдел разработки инфраструктуры и надёжности серверов ' +
    'платформы devforge';

const CHATS = [
    // Кириллическое имя + screen_name: главный случай приёмки.
    {
        id: '1234', kind: 'user', title: 'Павел Дуров', username: 'durov',
        bot: false, unread_count: 3, muted: false,
    },
    // У беседы screen_name не бывает вовсе — приписки быть не должно.
    {
        id: '2000000007', kind: 'chat', title: 'Проектная беседа', username: null,
        bot: false, members: 4, unread_count: 0, muted: false,
    },
    // Сообщество: [BOT] + [M] обязаны пережить появление ника.
    {
        id: '-55', kind: 'group', title: 'Тестовое сообщество', username: 'test_club',
        bot: true, unread_count: 40, muted: true,
    },
    // Имени нет вовсе — его место занимает ник, и вторым он не повторяется.
    {
        id: '77', kind: 'user', title: '', username: 'nameless',
        bot: false, unread_count: 0, muted: false,
    },
    // Длинное имя: обрезкой занимается CSS, формат отдаёт строку целиком.
    {
        id: '88', kind: 'user', title: LONG, username: 'infra_team',
        bot: false, unread_count: 0, muted: false,
    },
    // Составная графема + emoji: показываются как есть.
    {
        id: '99', kind: 'user', title: '\u{1F1F7}\u{1F1FA} Соня \u{1F525}', username: 'sofia',
        bot: false, unread_count: 0, muted: false,
    },
];

userSettings.vk_filter_enabled = false;
S.vkState.dialogs = CHATS;
S.vkState.activeDir = 'all';
D.renderAll();

const rows = new Map($dialogList.children.map((li) => [li.dataset.dialogId, li]));
const clubRow = $communityList.children[0];

/** Первая строка элемента: `<span class="vk-dialog-name">`. */
const nameOf = (li) => li.children[0];
const titleNode = (li) => nameOf(li).querySelector('.vk-dialog-title');
const nickNode = (li) => nameOf(li).querySelector('.vk-dialog-nick');

// ---------------------------------------------------------------------------
// 1. Разметка строки
// ---------------------------------------------------------------------------

eq(rows.size, 5, 'колонка: пять узлов (сообщество ушло во второй список)');

{
    const li = rows.get('1234');
    eq(titleNode(li).textContent, 'Павел Дуров',
        'имя показывается кириллицей, а не слагом durov@node');
    eq(nickNode(li).textContent, '@durov', 'ник (screen_name) приписан следом');
    ok(nameOf(li).textContent.startsWith('Павел Дуров'),
        `имя первым, ник вторым: ${nameOf(li).textContent}`);
    ok(nameOf(li).children.indexOf(titleNode(li)) < nameOf(li).children.indexOf(nickNode(li)),
        'порядок узлов: имя, затем ник');
    eq(li.title, 'Павел Дуров (@durov)',
        'подсказка несёт имя И ник — дублировать видимое было бы шумом');
}

{
    const li = rows.get('2000000007');
    eq(titleNode(li).textContent, 'Проектная беседа', 'беседа: имя как есть');
    eq(nickNode(li), null, 'беседа: ника нет — нет и узла (пустой span дал бы дыру от margin)');
    eq(nameOf(li).textContent, 'Проектная беседа', 'беседа: в строке ровно имя, без слага');
    eq(li.title, 'Проектная беседа', 'подсказка без ника — только имя, без пустых скобок');
}

{
    eq(clubRow.dataset.dialogId, '-55', 'сообщество: во втором списке');
    eq(titleNode(clubRow).textContent, 'Тестовое сообщество',
        'сообщество: прежний слаг давал anon@club');
    eq(nickNode(clubRow).textContent, '@test_club', 'сообщество: ник на месте');
    ok(nameOf(clubRow).textContent.includes('[BOT]'),
        `метка бота уцелела: ${nameOf(clubRow).textContent}`);
    ok(nameOf(clubRow).textContent.includes('[M]'),
        `метка mute уцелела: ${nameOf(clubRow).textContent}`);
    ok(clubRow.className.includes('vk-muted'), 'заглушённая строка помечена классом');
    // Счётчик непрочитанного живёт во ВТОРОЙ строке и под давление первой не
    // попадает вовсе — это и есть гарантия «ник его не вытеснит».
    eq(nameOf(clubRow).querySelector('.vk-dialog-unread'), null,
        'счётчик непрочитанного не в строке имени');
    ok(clubRow.children[1].textContent.includes('(40)'), 'счётчик — во второй строке');
}

{
    const li = rows.get('77');
    eq(titleNode(li).textContent, '@nameless', 'без title его место занимает ник');
    eq(nickNode(li), null, 'ник не повторяется вторым, когда он уже стал именем');
    eq(li.title, '@nameless', 'подсказка не превращается в «@nameless (@nameless)»');
}

{
    const li = rows.get('88');
    eq(titleNode(li).textContent, LONG, 'длинное имя доезжает целиком — режет его CSS');
    eq(nickNode(li).textContent, '@infra_team', 'у длинного имени ник всё равно нарисован');
    eq(li.title, `${LONG} (@infra_team)`, 'подсказка возвращает то, что обрежет колонка');
}

{
    const li = rows.get('99');
    eq(titleNode(li).textContent, '\u{1F1F7}\u{1F1FA} Соня \u{1F525}',
        'флаг и emoji не транслитерируются и не рвутся');
}

// ---------------------------------------------------------------------------
// 2. Раскладка: правила css/vk.css
// ---------------------------------------------------------------------------

const CSS_PATH = new URL('../../static/css/vk.css', import.meta.url);
// Комментарии срезаются ДО разбора: файл объясняет каждое своё решение и пишет
// в комментариях те же имена токенов, что и в объявлениях. Наивный разбор
// принял бы фразу «--v-active: наведение» за объявление и подменил бы значение
// токена куском прозы — тест позеленел бы на мусоре.
const css = readFileSync(CSS_PATH, 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

/**
 * Объявления одного правила по ТОЧНОМУ селектору.
 *
 * Тело берётся до первой закрывающей скобки: вложенных блоков внутри правила
 * не бывает, а полноценный парсер CSS здесь был бы отдельной библиотекой с
 * собственными багами.
 */
function ruleOf(selector) {
    const at = css.indexOf(`${selector} {`);
    if (at < 0) throw new Error(`правило «${selector}» не найдено в css/vk.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/([a-z-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
    return out;
}

/** Коэффициент сжатия из сокращения `flex: <grow> <shrink> <basis>`. */
function shrinkOf(rule) {
    const m = /^(\d+)\s+(\d+)\s+/.exec(String(rule.flex || ''));
    return m ? Number(m[2]) : null;
}

const nameRule = ruleOf('.vk-dialog-name');
const titleRule = ruleOf('.vk-dialog-title');
const nickRule = ruleOf('.vk-dialog-nick');
const botRule = ruleOf('.vk-dialog-bot');
const markRule = ruleOf('.vk-muted-mark');

eq(nameRule.display, 'flex', 'строка имени — флекс (иначе порядок жертв не задать)');
eq(nameRule['min-width'], '0', 'строка имени умеет ужиматься (min-width: 0)');
eq(nameRule.overflow, 'hidden', 'строка имени клипует лишнее');
eq(nameRule['white-space'], 'nowrap', 'строка имени не переносится');

eq(titleRule['text-overflow'], 'ellipsis', 'длинное имя обрезается многоточием');
eq(titleRule['min-width'], '0', 'имя умеет ужиматься (без min-width: 0 флекс не даст)');
eq(titleRule.overflow, 'hidden', 'ellipsis требует overflow: hidden на том же узле');

eq(nickRule['text-overflow'], 'ellipsis', 'ник тоже обрезается многоточием, а не клипуется');
eq(nickRule['min-width'], '0', 'ник умеет ужиматься до нуля');
ok(shrinkOf(nickRule) > shrinkOf(titleRule) * 10,
    `ник ужимается РАНЬШЕ имени: shrink ${shrinkOf(nickRule)} против ${shrinkOf(titleRule)}`);
ok(shrinkOf(titleRule) > 0, 'имя всё-таки ужимается — иначе оно вытолкнет метки');
eq(shrinkOf(botRule), 0, 'метка [BOT] не ужимается никогда');
eq(shrinkOf(markRule), 0, 'метка [M] не ужимается никогда');
ok(nickRule['margin-left'] !== undefined,
    'отступ ника задан CSS, а не пробелом в тексте (пробельный узел во флексе не рендерится)');

// Шапка сессии СОЗНАТЕЛЬНО осталась однострочным блоком: она жертвует хвостом
// (⌁ PACKETS · ◇ KEY), а не именем открытой беседы.
const headRule = ruleOf('#vk-session-head');
eq(headRule['text-overflow'], 'ellipsis', 'шапка обрезается справа');
eq(headRule.display, undefined, 'шапка не флекс — её порядок жертв другой и менять его незачем');
eq(ruleOf('.vk-session-nick').color, 'var(--v-faint-text)', 'ник в шапке — приглушённый');

// ---------------------------------------------------------------------------
// 3. Читаемость приглушённого ника
// ---------------------------------------------------------------------------

/** Порог WCAG для мелкого текста. Строка списка — 12px, послаблений нет. */
const AA_TEXT = 4.5;

function tokensOf(selector) {
    const at = css.indexOf(`${selector} {`);
    if (at < 0) throw new Error(`блок «${selector}» не найден в css/vk.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
    return out;
}

function resolve(tokens, name) {
    const raw = tokens[name];
    if (raw === undefined) return null;
    const ref = /^var\(\s*(--[a-z0-9-]+)\s*\)$/.exec(raw);
    return ref ? (tokens[ref[1]] ?? null) : raw;
}

/**
 * Яркость через контраст к чёрному: (L + 0.05) / 0.05 монотонна по L, а
 * гамма-скорректированная яркость в общем слое приватная — и правильно, что
 * приватная: наружу торчит ровно порог доступности.
 */
const brightness = (hex) => contrastRatio(hex, '#000000');

const dark = tokensOf('#vk');
const light = tokensOf('#vk[data-vk-theme="light"]');

// Цвет ника берётся из ТОКЕНА приглушённого текста, а не из рамочного: именно
// этой подменой токен был испорчен когда-то (см. vk_theme_contrast.test.mjs).
eq(nickRule.color, 'var(--v-faint-text)', 'ник красится токеном приглушённого ТЕКСТА');
ok(nickRule.color !== 'var(--v-acc-faint)', 'ник не красится цветом РАМОК');

const faintDark = resolve(dark, '--v-faint-text');
ok(parseCssColor(faintDark) !== null, `тёмная тема: --v-faint-text — цвет (${faintDark})`);

// Фоны, на которых лежит ник: колонка это --v-panel, подсвеченная и активная
// строка — --v-active, шапка сессии — --v-bg, полосы — --v-bar.
for (const bg of ['--v-bg', '--v-panel', '--v-bar', '--v-active']) {
    const ratio = contrastRatio(faintDark, resolve(dark, bg));
    ok(ratio >= AA_TEXT,
        `тёмная тема: ник на ${bg} даёт ${ratio && ratio.toFixed(2)}:1, нужно ${AA_TEXT}`);
}

// Иерархия: приписка не ярче основного текста строки (--v-acc-dim). VK этим и
// отличается от телеграмной панели — там для подсвеченной строки понадобился
// отдельный токен, здесь одного хватило.
ok(brightness(faintDark) <= brightness(resolve(dark, '--v-acc-dim')) + 1e-9,
    'тёмная тема: ник не ярче имени — он отличается обесцвеченностью, а не светом');

// Светлая тема: ник обязан читаться и не становиться контрастнее имени.
const faintLight = resolve(light, '--v-faint-text');
for (const bg of ['--v-bg', '--v-panel', '--v-bar', '--v-active']) {
    const ratio = contrastRatio(faintLight, resolve(light, bg));
    ok(ratio >= 3,
        `светлая тема: ник на ${bg} даёт ${ratio && ratio.toFixed(2)}:1, минимум 3:1`);
}
ok(contrastRatio(faintLight, resolve(light, '--v-bg'))
    < contrastRatio(resolve(light, '--v-acc-dim'), resolve(light, '--v-bg')),
    'светлая тема: ник бледнее имени (в светлой теме «тише» значит меньше контраста)');

// ---------------------------------------------------------------------------
// Итог
// ---------------------------------------------------------------------------

if (failures) {
    console.error(`\n${failures} из ${checks} провалились (frontend/vk_dialog_name)`);
    process.exit(1);
}
console.log(`${checks}/${checks} ассертов прошло (frontend/vk_dialog_name)`);
