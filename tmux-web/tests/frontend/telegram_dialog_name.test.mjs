// Регресс-тест НАЗВАНИЯ ЧАТА в колонке Telegram: имя + приглушённый @ник.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. До него строка списка показывала терминальный
// слаг (`ivan_petrov@node`, format.js::slugName): имя приводилось к нижнему
// регистру, а всё, что не [a-z0-9_.-], сжималось в `_`. Для кириллицы это
// означало строку из одних подчёркиваний, которую функция заменяла на `anon`
// — «Иван Петров» в колонке выглядел как `anon@node`. Настоящее имя жило
// только в подсказке при наведении. Пользовательская приёмка: «ники должны
// быть второстепенными, показывайте имена».
//
// Проверяется ТРИ слоя разом, потому что дефект возможен в каждом:
//
//   1. РАЗМЕТКА (dialogs.js::buildRow) — имя, ник отдельным узлом, отсутствие
//      узла при отсутствии ника, осмысленная подсказка, сохранность меток;
//   2. РАСКЛАДКА (css/telegram.css) — порядок жертв при нехватке места: ник
//      ужимается первым, метки не ужимаются никогда, имя получает ellipsis.
//      Node не считает каскад, поэтому проверяются САМИ ПРАВИЛА файла: числа,
//      переписанные в тест руками, разошлись бы с CSS молча;
//   3. ЧИТАЕМОСТЬ (те же токены) — приглушённый ник обязан оставаться текстом,
//      а не фоном. Токен --t-faint-text был объявлен как var(--t-grn-faint),
//      то есть цветом РАМОК: 1.29–1.59:1. Формула контраста берётся из общего
//      слоя (js/messenger/theme-core.js), третьей копии математики цвета в
//      проекте не заводится.
//
// Стаб DOM обязан стоять раньше первого импорта js/telegram/*: `core/dom.js`
// резолвит ref'ы один раз на верхнем уровне модуля.
//
// Запуск: node tmux-web/tests/frontend/telegram_dialog_name.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

import {
    $channelList, $dialogList, $sessionHead, userSettings,
} from './telegram_dom_stub.mjs';

const base = new URL('../../static/js/telegram/', import.meta.url).href;
const dialogs = await import(`${base}dialogs.js`);
const chat = await import(`${base}chat.js`);
const { invalidateVisibilityFilter, tgState } = await import(`${base}state.js`);
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
    // Кириллическое имя + ник: главный случай приёмки.
    {
        id: '10', kind: 'user', title: 'Иван Петров', username: 'ivan_petrov',
        bot: false, contact: true, unread_count: 3, muted: false, archived: false,
    },
    // Группы @username'а не имеют — приписки быть не должно.
    {
        id: '11', kind: 'group', title: 'Дев Чат',
        unread_count: 0, muted: false, archived: false,
    },
    // Бот и заглушённый чат: метки обязаны пережить появление ника.
    {
        id: '12', kind: 'user', title: 'Робот Помощник', username: 'helper_bot',
        bot: true, contact: false, unread_count: 5, muted: true, archived: false,
    },
    // Имени нет вовсе — его место занимает ник, и вторым он не повторяется.
    {
        id: '13', kind: 'user', title: '', username: 'nameless',
        bot: false, contact: false, unread_count: 0, muted: false, archived: false,
    },
    // Длинное имя: обрезкой занимается CSS, формат отдаёт строку целиком.
    {
        id: '14', kind: 'user', title: LONG, username: 'infra_team',
        bot: false, contact: true, unread_count: 0, muted: false, archived: false,
    },
    // Составная графема + emoji: показываются как есть.
    {
        id: '15', kind: 'user', title: '\u{1F1F7}\u{1F1FA} Соня \u{1F525}', username: 'sofia',
        bot: false, contact: true, unread_count: 0, muted: false, archived: false,
    },
    // Канал живёт во втором списке — правила у него те же.
    {
        id: '16', kind: 'channel', title: 'Новости платформы', username: 'chan_news',
        unread_count: 2, muted: false, archived: false,
    },
];

userSettings.telegram_filter_enabled = false;
invalidateVisibilityFilter();
tgState.folders = [];
tgState.dialogs = CHATS;
tgState.activeDir = 'inbox';
dialogs.renderAll();

const rows = new Map($dialogList.children.map((li) => [li.dataset.dialogId, li]));
const chanRow = $channelList.children[0];

/** Первая строка элемента: `<span class="tg-dialog-name">`. */
const nameOf = (li) => li.children[0];
const titleNode = (li) => nameOf(li).querySelector('.tg-dialog-title');
const nickNode = (li) => nameOf(li).querySelector('.tg-dialog-nick');

// ---------------------------------------------------------------------------
// 1. Разметка строки
// ---------------------------------------------------------------------------

eq(rows.size, 6, 'колонка: шесть узлов (канал ушёл во второй список)');

{
    const li = rows.get('10');
    eq(titleNode(li).textContent, 'Иван Петров',
        'имя показывается кириллицей, а не слагом anon@node');
    eq(nickNode(li).textContent, '@ivan_petrov', 'ник приписан следом');
    ok(nameOf(li).textContent.startsWith('Иван Петров'),
        `имя первым, ник вторым: ${nameOf(li).textContent}`);
    ok(nameOf(li).children.indexOf(titleNode(li)) < nameOf(li).children.indexOf(nickNode(li)),
        'порядок узлов: имя, затем ник');
    eq(li.title, 'Иван Петров (@ivan_petrov)',
        'подсказка несёт имя И ник — дублировать видимое было бы шумом');
}

{
    const li = rows.get('11');
    eq(titleNode(li).textContent, 'Дев Чат', 'группа: имя как есть');
    eq(nickNode(li), null, 'группа: ника нет — нет и узла (пустой span дал бы дыру от margin)');
    eq(nameOf(li).textContent, 'Дев Чат', 'группа: в строке ровно имя, без «@» и без слага');
    eq(li.title, 'Дев Чат', 'подсказка без ника — только имя, без пустых скобок');
}

{
    const li = rows.get('12');
    eq(titleNode(li).textContent, 'Робот Помощник', 'бот: имя как есть');
    eq(nickNode(li).textContent, '@helper_bot', 'бот: ник на месте');
    ok(nameOf(li).textContent.includes('[BOT]'), `метка бота уцелела: ${nameOf(li).textContent}`);
    ok(nameOf(li).textContent.includes('[M]'), `метка mute уцелела: ${nameOf(li).textContent}`);
    ok(li.className.includes('tg-muted'), 'заглушённая строка помечена классом');
    // Счётчик непрочитанного живёт во ВТОРОЙ строке и под давление первой не
    // попадает вовсе — это и есть гарантия «ник его не вытеснит».
    eq(nameOf(li).querySelector('.tg-dialog-unread'), null,
        'счётчик непрочитанного не в строке имени');
    ok(li.children[1].textContent.includes('(5)'), 'счётчик — во второй строке');
}

{
    const li = rows.get('13');
    eq(titleNode(li).textContent, '@nameless', 'без title его место занимает ник');
    eq(nickNode(li), null, 'ник не повторяется вторым, когда он уже стал именем');
    eq(li.title, '@nameless', 'подсказка не превращается в «@nameless (@nameless)»');
}

{
    const li = rows.get('14');
    eq(titleNode(li).textContent, LONG, 'длинное имя доезжает целиком — режет его CSS');
    eq(nickNode(li).textContent, '@infra_team', 'у длинного имени ник всё равно нарисован');
    eq(li.title, `${LONG} (@infra_team)`, 'подсказка возвращает то, что обрежет колонка');
}

{
    const li = rows.get('15');
    eq(titleNode(li).textContent, '\u{1F1F7}\u{1F1FA} Соня \u{1F525}',
        'флаг и emoji не транслитерируются и не рвутся');
}

{
    eq(chanRow.dataset.dialogId, '16', 'канал: во втором списке');
    eq(titleNode(chanRow).textContent, 'Новости платформы', 'канал: имя как есть');
    eq(nickNode(chanRow).textContent, '@chan_news', 'канал: ник приписью');
}

// ---------------------------------------------------------------------------
// 1b. Шапка сессии: тот же принцип, что и в строке списка
// ---------------------------------------------------------------------------

{
    chat.updateHead(CHATS[0], 3);
    ok($sessionHead.textContent.startsWith('SESSION ▸ Иван Петров @ivan_petrov · '),
        `шапка: имя основным, ник приписью — ${$sessionHead.textContent}`);
    ok($sessionHead.textContent.includes('⌁ 3 PACKETS'),
        'шапка: хвост со счётчиком пережил разбор на узлы');
    const nick = $sessionHead.querySelector('.tg-session-nick');
    ok(nick !== null, 'ник в шапке — ОТДЕЛЬНЫЙ узел: одной строкой его не покрасить');
    eq(nick.textContent, '@ivan_petrov', 'в узле ника — только ник');
    ok(!nick.textContent.includes('Иван'), 'имя в приглушённый узел не попало');
}

{
    chat.updateHead(CHATS[1], 0);
    ok($sessionHead.textContent.startsWith('SESSION ▸ Дев Чат · '),
        `шапка без ника: без лишнего пробела — ${$sessionHead.textContent}`);
    eq($sessionHead.querySelector('.tg-session-nick'), null,
        'ника нет — нет и узла в шапке');
}

{
    chat.updateHead(null, 0);
    eq($sessionHead.textContent, 'SESSION ▸ —', 'шапка при закрытом чате');
    eq($sessionHead.children.length, 0, 'закрытый чат не оставляет узлов от прошлого');
}

// ---------------------------------------------------------------------------
// 2. Раскладка: правила css/telegram.css
// ---------------------------------------------------------------------------

const CSS_PATH = new URL('../../static/css/telegram.css', import.meta.url);
// Комментарии срезаются ДО разбора: этот файл объясняет каждое своё решение и
// пишет в комментариях те же имена токенов, что и в объявлениях. Наивный
// разбор принял бы фразу «--t-active: наведение» за объявление и подменил бы
// значение токена куском прозы — тест позеленел бы на мусоре.
const css = readFileSync(CSS_PATH, 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

/**
 * Объявления одного правила по ТОЧНОМУ селектору.
 *
 * Селектор ищется в форме `<selector> {`, тело берётся до первой закрывающей
 * скобки: вложенных блоков внутри правила не бывает, а полноценный парсер CSS
 * здесь был бы отдельной библиотекой с собственными багами.
 */
function ruleOf(selector) {
    const at = css.indexOf(`${selector} {`);
    if (at < 0) throw new Error(`правило «${selector}» не найдено в css/telegram.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/([a-z-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
    return out;
}

/** Коэффициент сжатия из сокращения `flex: <grow> <shrink> <basis>`. */
function shrinkOf(rule) {
    const flex = String(rule.flex || '');
    const m = /^(\d+)\s+(\d+)\s+/.exec(flex);
    return m ? Number(m[2]) : null;
}

const nameRule = ruleOf('.tg-dialog-name');
const titleRule = ruleOf('.tg-dialog-title');
const nickRule = ruleOf('.tg-dialog-nick');
const botRule = ruleOf('.tg-dialog-bot');
const markRule = ruleOf('.tg-muted-mark');

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
ok(/margin-left/.test(String(nickRule['margin-left'] ? 'margin-left' : '')),
    'отступ ника задан CSS, а не пробелом в тексте (пробельный узел во флексе не рендерится)');

// Шапка сессии СОЗНАТЕЛЬНО осталась однострочным блоком: она жертвует хвостом
// (⌁ PACKETS · ◇ KEY), а не именем открытого чата.
const headRule = ruleOf('#tg-session-head');
eq(headRule['text-overflow'], 'ellipsis', 'шапка обрезается справа');
eq(headRule.display, undefined, 'шапка не флекс — её порядок жертв другой и менять его незачем');
eq(ruleOf('.tg-session-nick').color, 'var(--t-faint-text)', 'ник в шапке — приглушённый');

// ---------------------------------------------------------------------------
// 3. Читаемость приглушённого ника
// ---------------------------------------------------------------------------

/** Порог WCAG для мелкого текста. Строка списка — 12px, послаблений нет. */
const AA_TEXT = 4.5;

function tokensOf(selector) {
    const at = css.indexOf(`${selector} {`);
    if (at < 0) throw new Error(`блок «${selector}» не найден в css/telegram.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
    return out;
}

/** Разворачивает `var(--x)` по таблице токенов (одного уровня достаточно). */
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

const dark = tokensOf('#telegram');
const light = tokensOf('#telegram[data-tg-theme="light"]');

// Структурный щит: приглушённый ТЕКСТ — не цвет РАМОК. Именно этим дефектом
// токен и был испорчен (и в css/vk.css тем же самым — тот же щит стоит там).
for (const [name, tokens] of [['тёмная', dark], ['светлая', light]]) {
    ok(!/^var\(\s*--t-grn-faint\s*\)$/.test(String(tokens['--t-faint-text'])),
        `${name} тема: --t-faint-text не псевдоним рамочного --t-grn-faint`
        + ` (сейчас «${tokens['--t-faint-text']}»)`);
    ok(resolve(tokens, '--t-faint-text') !== resolve(tokens, '--t-grn-faint'),
        `${name} тема: значение --t-faint-text не совпадает с --t-grn-faint`);
}

const faintDark = resolve(dark, '--t-faint-text');
ok(parseCssColor(faintDark) !== null, `тёмная тема: --t-faint-text — цвет (${faintDark})`);

// Фоны, на которых ник лежит в покое: колонка это --t-panel, шапка сессии —
// --t-bg, полосы — --t-bar.
for (const bg of ['--t-bg', '--t-panel', '--t-bar']) {
    const ratio = contrastRatio(faintDark, resolve(dark, bg));
    ok(ratio >= AA_TEXT,
        `тёмная тема: ник на ${bg} даёт ${ratio && ratio.toFixed(2)}:1, нужно ${AA_TEXT}`);
}

// Иерархия: приписка не ярче основного текста строки (--t-grn-dim).
ok(brightness(faintDark) <= brightness(resolve(dark, '--t-grn-dim')) + 1e-9,
    'тёмная тема: ник не ярче имени — он отличается обесцвеченностью, а не светом');

// Подсвеченная строка (--t-active): фон светлее, и потолок поднимает уже не
// --t-grn-dim (4.03:1 — AA там недостижим в принципе), а --t-grn/--t-grn-hi,
// которыми красится имя под курсором и в активном чате.
const faintHi = resolve(dark, '--t-faint-text-hi');
ok(parseCssColor(faintHi) !== null, `тёмная тема: --t-faint-text-hi — цвет (${faintHi})`);
for (const bg of ['--t-bg', '--t-panel', '--t-bar', '--t-active']) {
    const ratio = contrastRatio(faintHi, resolve(dark, bg));
    ok(ratio >= AA_TEXT,
        `тёмная тема: ник подсвеченной строки на ${bg} даёт ${ratio && ratio.toFixed(2)}:1`);
}
ok(brightness(faintHi) < brightness(resolve(dark, '--t-grn')),
    'тёмная тема: ник подсвеченной строки заметно темнее имени (--t-grn)');
ok(css.includes('.tg-dialog:hover .tg-dialog-nick'),
    'правило подсветки ника существует — иначе токен --t-faint-text-hi мёртв');

// Светлая тема: палитра панели там вся живёт в диапазоне 3–4:1, и ник обязан
// оставаться ЧИТАЕМЫМ, не становясь контрастнее имени, которому подчинён.
const faintLight = resolve(light, '--t-faint-text');
const dimLight = resolve(light, '--t-grn-dim');
for (const bg of ['--t-bg', '--t-panel', '--t-bar', '--t-active']) {
    const ratio = contrastRatio(faintLight, resolve(light, bg));
    ok(ratio >= 3,
        `светлая тема: ник на ${bg} даёт ${ratio && ratio.toFixed(2)}:1, минимум 3:1`);
}
ok(contrastRatio(faintLight, resolve(light, '--t-bg'))
    < contrastRatio(dimLight, resolve(light, '--t-bg')),
    'светлая тема: ник бледнее имени (в светлой теме «тише» значит меньше контраста)');

// ---------------------------------------------------------------------------
// Итог
// ---------------------------------------------------------------------------

if (failures) {
    console.error(`\n${failures} из ${checks} провалились (frontend/telegram_dialog_name)`);
    process.exit(1);
}
console.log(`${checks}/${checks} ассертов прошло (frontend/telegram_dialog_name)`);
