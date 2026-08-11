// Регресс-тест общего слоя мессенджеров: разбор и БЕЗОПАСНЫЙ рендер Markdown.
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/messenger/markdown.js через
// `await import()` — модуль намеренно не импортирует ни DOM, ни core/*,
// поэтому в Node работает как есть, а `renderMarkdown` получает документ
// параметром и проверяется на фейковом DOM (сделан здесь же, по мотивам
// vk_dom_stub.mjs, но без реестра id — рендереру он не нужен).
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Текст, который разбирает модуль, порождён
// моделью ИЗ ПЕРЕПИСКИ С ПОСТОРОННИМИ ЛЮДЬМИ — это недоверенный ввод. Секция
// 3 (безопасность ссылок) и секция 4 (grep по исходнику на innerHTML) —
// не «покрытие», а СТРАЖИ: они обязаны краснеть, если кто-то заменит
// `new URL` префиксным регекспом или соберёт узел строкой разметки.
//
// Пять секций:
//   1. Парсер, блоки и инлайн (чистые функции, без DOM).
//   2. Рендерер на фейковом DOM.
//   3. БЕЗОПАСНОСТЬ ССЫЛОК — поимённо каждый вектор обхода.
//   4. Исходник модуля: ни innerHTML, ни insertAdjacentHTML, ни outerHTML,
//      ни document.write.
//   5. Устойчивость: мусор, длинный вход, кириллица и эмодзи, таймаут.
//
// Запуск: node tmux-web/tests/frontend/messenger_markdown.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const modPath = new URL('../../static/js/messenger/markdown.js', import.meta.url);
const M = await import(modPath.href);

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

// ==========================================================================
// Фейковый DOM: ровно тот срез, которым пользуется renderMarkdown.
// ==========================================================================

function makeText(data) {
    return { nodeType: 3, tagName: null, parentNode: null, childNodes: [], textContent: String(data) };
}

function makeEl(tag) {
    const node = {
        nodeType: 1,
        tagName: String(tag).toLowerCase(),
        childNodes: [],
        parentNode: null,
        style: {},
        attributes: {},
        className: '',
    };
    Object.defineProperty(node, 'textContent', {
        get: () => node.childNodes.map((c) => c.textContent).join(''),
    });
    node.appendChild = (child) => {
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.setAttribute = (k, v) => { node.attributes[k] = String(v); };
    node.getAttribute = (k) => (k in node.attributes ? node.attributes[k] : null);
    return node;
}

function makeDoc() {
    return {
        createElement: (tag) => makeEl(tag),
        createTextNode: (t) => makeText(t),
        createDocumentFragment: () => {
            const f = makeEl('#fragment');
            f.nodeType = 11;
            return f;
        },
    };
}

/** Дерево тегов фрагмента: 'p>strong,em' — для компактных ассертов. */
function tags(node) {
    return node.childNodes
        .filter((c) => c.nodeType === 1)
        .map((c) => c.tagName);
}

function find(node, tag) {
    if (node.nodeType === 1 && node.tagName === tag) return node;
    for (const c of node.childNodes) {
        const r = find(c, tag);
        if (r) return r;
    }
    return null;
}

function findAll(node, tag, acc = []) {
    if (node.nodeType === 1 && node.tagName === tag) acc.push(node);
    for (const c of node.childNodes) findAll(c, tag, acc);
    return acc;
}

// ==========================================================================
// СЕКЦИЯ 1. Парсер — блоки
// ==========================================================================

eq(M.parseMarkdown(''), [], 'пустая строка — пустой AST');
eq(M.parseMarkdown(null), [], 'не строка — пустой AST');
eq(M.parseMarkdown('\n\n   \n'), [], 'только пустые строки — пустой AST');

{
    const ast = M.parseMarkdown('обычный текст');
    eq(ast.length, 1, 'абзац: один блок');
    eq(ast[0].type, 'paragraph', 'абзац: тип');
    eq(ast[0].text, 'обычный текст', 'абзац: текст сохранён');
}

{
    const ast = M.parseMarkdown('первый\nвторой\n\nтретий');
    eq(ast.map((b) => b.type), ['paragraph', 'paragraph'], 'пустая строка делит абзацы');
    eq(ast[0].text, 'первый\nвторой', 'абзац склеивает строки через \\n');
}

eq(M.parseMarkdown('a\r\nb').length, 1, 'CRLF нормализуется (один абзац)');
eq(M.parseMarkdown('a\rb')[0].text, 'a\nb', 'одиночный CR нормализуется');

for (let lvl = 1; lvl <= 6; lvl += 1) {
    const ast = M.parseMarkdown(`${'#'.repeat(lvl)} Заголовок`);
    eq(ast[0].type, 'heading', `заголовок h${lvl}: тип`);
    eq(ast[0].level, lvl, `заголовок h${lvl}: уровень`);
    eq(ast[0].text, 'Заголовок', `заголовок h${lvl}: текст`);
}
eq(M.parseMarkdown('####### seven')[0].type, 'paragraph', '7 решёток — абзац, не заголовок');
eq(M.parseMarkdown('####### seven')[0].text, '####### seven', '7 решёток: текст не потерян');
eq(M.parseMarkdown('#нет пробела')[0].type, 'paragraph', 'решётка без пробела — абзац');
eq(M.parseMarkdown('## Итог ##')[0].text, 'Итог', 'закрывающие решётки срезаются');

eq(M.parseMarkdown('---')[0].type, 'hr', 'hr: дефисы');
eq(M.parseMarkdown('***')[0].type, 'hr', 'hr: звёздочки');
eq(M.parseMarkdown('___')[0].type, 'hr', 'hr: подчёркивания');
eq(M.parseMarkdown('- - -')[0].type, 'hr', 'hr: с пробелами');
eq(M.parseMarkdown('--')[0].type, 'paragraph', 'два дефиса — не hr');

{
    const ast = M.parseMarkdown('> первая\n> вторая');
    eq(ast.length, 1, 'цитата из двух строк — ОДИН блок');
    eq(ast[0].type, 'blockquote', 'цитата: тип');
    eq(ast[0].blocks.length, 1, 'цитата: внутри один абзац');
    eq(ast[0].blocks[0].text, 'первая\nвторая', 'цитата: содержимое склеено');
}
{
    const ast = M.parseMarkdown('> внешняя\n> > вложенная');
    eq(ast[0].type, 'blockquote', 'вложенная цитата: внешний блок');
    ok(ast[0].blocks.some((b) => b.type === 'blockquote'), 'вложенная цитата: внутренний блок');
}
{
    const ast = M.parseMarkdown('> ## Заголовок в цитате');
    eq(ast[0].blocks[0].type, 'heading', 'внутри цитаты разбираются блоки');
}

// ---------- списки ----------
{
    const ast = M.parseMarkdown('- один\n- два\n- три');
    eq(ast.length, 1, 'ul: один блок списка');
    eq(ast[0].type, 'list', 'ul: тип');
    eq(ast[0].ordered, false, 'ul: не нумерованный');
    eq(ast[0].items.map((it) => it.text), ['один', 'два', 'три'], 'ul: три пункта');
}
{
    const ast = M.parseMarkdown('3. третий\n4. четвёртый');
    eq(ast[0].ordered, true, 'ol: нумерованный');
    eq(ast[0].start, 3, 'ol: стартовый номер сохранён');
    eq(ast[0].items.length, 2, 'ol: два пункта');
}
eq(M.parseMarkdown('1) скобка')[0].ordered, true, 'ol: маркер со скобкой');
{
    const ast = M.parseMarkdown('- дефис\n* звезда');
    eq(ast.length, 2, 'смена маркера начинает НОВЫЙ список');
    eq(ast.map((b) => b.marker), ['-', '*'], 'два списка со своими маркерами');
}
{
    const ast = M.parseMarkdown('- один\n1. номер');
    eq(ast.length, 2, 'смена типа списка начинает новый список');
    eq(ast[1].ordered, true, 'второй список нумерованный');
}
{
    const ast = M.parseMarkdown('- верх\n  - вложенный\n  - второй вложенный\n- второй верх');
    eq(ast.length, 1, 'вложенность: один внешний список');
    eq(ast[0].items.length, 2, 'вложенность: два внешних пункта, а не четыре');
    eq(ast[0].items[0].children.length, 1, 'вложенность: вложенный список у первого пункта');
    eq(ast[0].items[0].children[0].items.map((i) => i.text), ['вложенный', 'второй вложенный'],
        'вложенность: пункты вложенного списка');
}
{
    const ast = M.parseMarkdown('- пункт\n    1. номер внутри');
    eq(ast[0].items[0].children[0].ordered, true, 'ol внутри ul');
}
{
    const ast = M.parseMarkdown('1. номер\n    - маркер внутри');
    eq(ast[0].items[0].children[0].ordered, false, 'ul внутри ol');
}
{
    const ast = M.parseMarkdown('- a\n  продолжение');
    eq(ast[0].items.length, 1, 'продолжение строки: один пункт');
    eq(ast[0].items[0].text, 'a\nпродолжение', 'продолжение строки: две строки текста');
}
{
    const ast = M.parseMarkdown('вводный текст\n- пункт');
    eq(ast.map((b) => b.type), ['paragraph', 'list'], 'список сразу после абзаца без пустой строки');
}
{
    const ast = M.parseMarkdown('-');
    ok(Array.isArray(ast), 'одинокий дефис не роняет парсер');
}

// ---------- блоки кода ----------
{
    const ast = M.parseMarkdown('```js\nconst a = 1;\n```');
    eq(ast[0].type, 'code', 'код: тип');
    eq(ast[0].lang, 'js', 'код: язык из строки открытия');
    eq(ast[0].text, 'const a = 1;', 'код: содержимое');
}
eq(M.parseMarkdown('```\nx\n```')[0].lang, null, 'код без языка: lang = null');
{
    const ast = M.parseMarkdown('```\n**не жирный** и [ссылка](http://x)\n```');
    eq(ast[0].text, '**не жирный** и [ссылка](http://x)', 'внутри кода инлайн НЕ разбирается');
    ok(!('inline' in ast[0]), 'у блока кода нет инлайн-поля');
}
{
    const ast = M.parseMarkdown('```py\nhvost = 1\nбез закрытия');
    eq(ast.length, 1, 'незакрытый блок кода — один блок');
    eq(ast[0].text, 'hvost = 1\nбез закрытия', 'незакрытый блок кода дочитан до конца (хвост не потерян)');
}
{
    const ast = M.parseMarkdown('~~~sql\nSELECT 1\n~~~');
    eq(ast[0].type, 'code', '~~~ поддержано как альтернативное ограждение');
    eq(ast[0].lang, 'sql', '~~~: язык');
}

// ---------- таблицы ----------
{
    const ast = M.parseMarkdown('| a | b |\n| --- | ---: |\n| 1 | 2 |\n| 3 | 4 |');
    eq(ast[0].type, 'table', 'таблица: тип');
    eq(ast[0].header.map((c) => c.text), ['a', 'b'], 'таблица: заголовок');
    eq(ast[0].align, [null, 'right'], 'таблица: выравнивание из разделителя');
    eq(ast[0].rows.length, 2, 'таблица: две строки данных');
    eq(ast[0].rows[1].map((c) => c.text), ['3', '4'], 'таблица: содержимое строк');
}
{
    const ast = M.parseMarkdown('| l | c | r |\n| :-- | :-: | --: |\n| 1 | 2 | 3 |');
    eq(ast[0].align, ['left', 'center', 'right'], 'таблица: все три выравнивания');
}
{
    const ast = M.parseMarkdown('| a | b | c |\n| - | - | - |\n| 1 |\n| 1 | 2 | 3 | 4 |');
    eq(ast[0].rows[0].map((c) => c.text), ['1', '', ''], 'короткая строка добивается пустыми');
    eq(ast[0].rows[1].map((c) => c.text), ['1', '2', '3'], 'длинная строка обрезается по заголовку');
}
{
    const ast = M.parseMarkdown('| a | b |\n| 1 | 2 |');
    ok(ast.every((b) => b.type !== 'table'), 'без строки-разделителя это НЕ таблица');
}
{
    const ast = M.parseMarkdown('| a | b |\n| - | - |\n| раз \\| два | 2 |');
    eq(ast[0].rows[0][0].text, 'раз | два', 'экранированная черта не режет ячейку');
}
{
    const ast = M.parseMarkdown('| a |\n| - |\n| **жирная** |');
    eq(ast[0].rows[0][0].inline[0].type, 'strong', 'содержимое ячеек уходит в инлайн-парсер');
}

// ==========================================================================
// СЕКЦИЯ 1b. Парсер — инлайн
// ==========================================================================

const inl = (s) => M.parseInline(s);

eq(inl('просто текст'), [{ type: 'text', value: 'просто текст' }], 'текст без разметки — один узел');
eq(inl('**жирный**'), [{ type: 'strong', children: [{ type: 'text', value: 'жирный' }] }], 'жирный');
eq(inl('*курсив*'), [{ type: 'em', children: [{ type: 'text', value: 'курсив' }] }], 'курсив звёздочкой');
eq(inl('_курсив_'), [{ type: 'em', children: [{ type: 'text', value: 'курсив' }] }], 'курсив подчёркиванием');
eq(inl('snake_case_имя'), [{ type: 'text', value: 'snake_case_имя' }], '_ внутри слова НЕ курсив');
eq(inl('~~вычеркнуто~~'), [{ type: 'strike', children: [{ type: 'text', value: 'вычеркнуто' }] }], 'зачёркнутый');
eq(inl('`код`'), [{ type: 'code', value: 'код' }], 'инлайн-код');

{
    const r = inl('`**не жирный**`');
    eq(r, [{ type: 'code', value: '**не жирный**' }], 'код имеет наивысший приоритет');
}
{
    const r = inl('**жирный с *курсивом* внутри**');
    eq(r[0].type, 'strong', 'вложенность: внешний strong');
    ok(r[0].children.some((c) => c.type === 'em'), 'вложенность: em внутри strong');
}
eq(inl('**без пары'), [{ type: 'text', value: '**без пары' }], 'незакрытый ** остаётся текстом');
eq(inl('текст ~~ хвост'), [{ type: 'text', value: 'текст ~~ хвост' }], 'незакрытый ~~ остаётся текстом');
eq(inl('a * b'), [{ type: 'text', value: 'a * b' }], 'одинокая звёздочка — текст');
eq(inl('`незакрытый код'), [{ type: 'text', value: '`незакрытый код' }], 'незакрытый бэктик — текст');

eq(inl('\\*звёздочка\\*'), [{ type: 'text', value: '*звёздочка*' }], 'экранирование звёздочки');
eq(inl('\\_'), [{ type: 'text', value: '_' }], 'экранирование подчёркивания');
eq(inl('\\`'), [{ type: 'text', value: '`' }], 'экранирование бэктика');
eq(inl('\\[не ссылка](x)'), [{ type: 'text', value: '[не ссылка](x)' }], 'экранирование скобки');
eq(inl('\\\\'), [{ type: 'text', value: '\\' }], 'экранирование обратного слэша');

{
    const r = inl('[текст](http://example.com)');
    eq(r[0].type, 'link', 'ссылка: тип');
    eq(r[0].href, 'http://example.com', 'ссылка: URL кладётся СЫРЫМ');
    eq(r[0].children, [{ type: 'text', value: 'текст' }], 'ссылка: текст');
}
{
    const r = inl('[**жирная** ссылка](http://x.tld)');
    eq(r[0].children[0].type, 'strong', 'текст ссылки проходит инлайн-разбор');
}
eq(inl('[текст]('), [{ type: 'text', value: '[текст](' }], 'незакрытая ссылка — текст');
eq(inl('[текст'), [{ type: 'text', value: '[текст' }], 'незакрытая скобка — текст');
{
    const r = inl('[t](http://x/(y))');
    eq(r[0].href, 'http://x/(y)', 'вложенные скобки в URL');
}

ok(Array.isArray(inl('***')), '*** не роняет парсер');
ok(Array.isArray(inl('~~~~')), '~~~~ не роняет парсер');
ok(Array.isArray(inl('****')), '**** не роняет парсер');
eq(inl('***').map((n) => n.type).join(''), 'text', '*** остаётся текстом');

// ==========================================================================
// СЕКЦИЯ 2. Рендерер на фейковом DOM
// ==========================================================================

const doc = makeDoc();
const render = (src) => M.renderMarkdownText(src, doc);

eq(M.renderMarkdown([], doc).childNodes.length, 0, 'пустой AST — пустой фрагмент');
eq(M.renderMarkdown(null, doc).childNodes.length, 0, 'не массив — пустой фрагмент');

eq(tags(render('текст')), ['p'], 'абзац → p');
eq(render('текст').textContent, 'текст', 'абзац: текст на месте');
eq(tags(render('## Заголовок')), ['h2'], 'заголовок → h2');
eq(tags(render('###### Шесть')), ['h6'], 'заголовок 6 уровня → h6');
eq(tags(render('---')), ['hr'], 'hr → hr');
eq(tags(render('> цитата')), ['blockquote'], 'цитата → blockquote');
eq(tags(render('- a\n- b')), ['ul'], 'ul → ul');
eq(tags(render('1. a')), ['ol'], 'ol → ol');
eq(findAll(render('- a\n- b'), 'li').length, 2, 'ul: два li');

{
    const frag = render('3. три');
    eq(find(frag, 'ol').getAttribute('start'), '3', 'ol: атрибут start');
}
eq(find(render('1. один'), 'ol').getAttribute('start'), null, 'ol со start=1 без атрибута');

{
    const frag = render('- верх\n  - низ');
    const outer = find(frag, 'ul');
    const li = outer.childNodes[0];
    ok(li.childNodes.some((c) => c.tagName === 'ul'), 'вложенный ul лежит внутри li');
}

{
    const frag = render('```js\nconst a = 1;\n```');
    eq(tags(frag), ['pre'], 'блок кода → pre');
    const code = find(frag, 'code');
    eq(code.getAttribute('data-lang'), 'js', 'блок кода: язык в data-lang');
    eq(code.textContent, 'const a = 1;', 'блок кода: содержимое текстом');
}
eq(find(render('```<script>\nx\n```'), 'code').getAttribute('data-lang'), null,
    'мусорный «язык» в data-lang не попадает');

{
    const frag = render('**ж** *к* `к` ~~з~~');
    const p = frag.childNodes[0];
    eq(tags(p), ['strong', 'em', 'code', 's'], 'инлайн: strong/em/code/s');
}

{
    const frag = render('| a | b | c |\n| :-- | :-: | --: |\n| 1 | 2 | 3 |');
    eq(tags(frag), ['div'], 'таблица обёрнута в div-контейнер');
    eq(frag.childNodes[0].className, 'md-table-wrap', 'контейнер таблицы имеет класс для скролла');
    ok(find(frag, 'table'), 'таблица: table внутри контейнера');
    ok(find(frag, 'thead') && find(frag, 'tbody'), 'таблица: thead и tbody');
    const ths = findAll(frag, 'th');
    eq(ths.length, 3, 'таблица: три th');
    eq(ths.map((t) => t.style.textAlign), ['left', 'center', 'right'],
        'выравнивание — через style.textAlign, а не атрибут align');
    eq(ths.map((t) => t.getAttribute('align')), [null, null, null], 'атрибут align не используется');
    eq(findAll(frag, 'td').length, 3, 'таблица: три td');
}

{
    // Повторный рендер одного AST обязан давать НЕЗАВИСИМЫЕ узлы: панель
    // рисует один и тот же разбор и в попапе, и в треде.
    const ast = M.parseMarkdown('**раз**');
    const a = M.renderMarkdown(ast, doc);
    const b = M.renderMarkdown(ast, doc);
    ok(a.childNodes[0] !== b.childNodes[0], 'повторный рендер даёт независимые узлы');
    eq(a.textContent, b.textContent, 'повторный рендер даёт тот же текст');
}

// ==========================================================================
// СЕКЦИЯ 3. БЕЗОПАСНОСТЬ ССЫЛОК — страж
// ==========================================================================
//
// Здесь недоверенный текст становится кликабельным элементом, и это
// единственное такое место в модуле. Каждый вектор проверяется ПОИМЁННО и на
// двух уровнях: safeHref (решение) и рендер (последствие — текст, не <a>).

const blocked = [
    ['javascript:alert(1)', 'javascript:'],
    ['JavaScript:alert(1)', 'JavaScript: смешанный регистр'],
    ['JAVASCRIPT:alert(1)', 'JAVASCRIPT: верхний регистр'],
    ['JaVaScRiPt:alert(1)', 'JaVaScRiPt: чередование регистра'],
    [' javascript:alert(1)', 'ведущий пробел перед схемой'],
    ['\njavascript:alert(1)', 'ведущий перевод строки'],
    ['\tjavascript:alert(1)', 'ведущая табуляция'],
    ['java\nscript:alert(1)', 'перевод строки ВНУТРИ схемы'],
    ['java\tscript:alert(1)', 'табуляция внутри схемы'],
    ['javascript\t:alert(1)', 'табуляция перед двоеточием'],
    [' javascript:alert(1)', 'ведущий управляющий символ'],
    ['data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==', 'data: с base64-документом'],
    ['data:text/plain,hi', 'data: даже безобидный'],
    ['vbscript:msgbox(1)', 'vbscript:'],
    ['file:///etc/passwd', 'file:'],
    ['about:blank', 'about:'],
    ['blob:https://x/1', 'blob:'],
    ['%6aavascript:alert(1)', 'URL-кодированная схема'],
    ['&#106;avascript:alert(1)', 'HTML-сущность в схеме'],
    ['/api/vk/dialogs/1', 'относительный путь запрещён'],
    ['../secret', 'относительный путь вверх запрещён'],
    ['//example.com/x', 'протокол-относительный запрещён'],
    ['', 'пустая строка'],
    ['не ссылка вовсе', 'произвольный текст'],
];

for (const [raw, label] of blocked) {
    eq(M.safeHref(raw), null, `safeHref блокирует: ${label}`);
}
eq(M.safeHref(null), null, 'safeHref: null');
eq(M.safeHref(undefined), null, 'safeHref: undefined');
eq(M.safeHref(42), null, 'safeHref: не строка');
eq(M.safeHref({ toString: () => 'http://x' }), null, 'safeHref: объект не приводится к строке');

ok(M.safeHref('http://example.com/a?b=1#c'), 'safeHref пропускает http:');
ok(M.safeHref('https://example.com'), 'safeHref пропускает https:');
ok(M.safeHref('HTTPS://EXAMPLE.COM'), 'safeHref пропускает HTTPS: в верхнем регистре');

for (const [raw, label] of blocked) {
    if (raw === '') continue;
    const frag = render(`[текст ссылки](${raw})`);
    ok(!find(frag, 'a'), `рендер НЕ создаёт <a> для: ${label}`);
    ok(frag.textContent.includes('текст ссылки'),
        `текст запрещённой ссылки остаётся на экране: ${label}`);
}

{
    const frag = render('[кликай](https://example.com/x)');
    const a = find(frag, 'a');
    ok(a, 'разрешённая ссылка становится <a>');
    eq(a.getAttribute('href'), 'https://example.com/x', 'href нормализован и сохранён');
    eq(a.getAttribute('target'), '_blank', 'target="_blank"');
    eq(a.getAttribute('rel'), 'noopener noreferrer', 'rel="noopener noreferrer"');
    eq(a.textContent, 'кликай', 'текст ссылки на месте');
}

{
    // Мутационная проверка стража: если бы допустимость решалась регекспом по
    // началу строки, ' javascript:' и 'java\nscript:' прошли бы. Здесь они
    // обязаны быть текстом — и текст обязан совпадать с исходным.
    const frag = render('[X]( javascript:alert(1))');
    ok(!find(frag, 'a'), 'обход пробелом не создаёт ссылку');
    eq(frag.textContent, 'X', 'обход пробелом: виден только текст ссылки');
}

// ==========================================================================
// СЕКЦИЯ 4. Исходник модуля — структурная гарантия инварианта
// ==========================================================================

{
    const src = readFileSync(modPath, 'utf8');
    // Проверяются ВХОЖДЕНИЯ в коде; в шапке файла запрещённые имена названы
    // словами внутри комментария, поэтому комментарии отбрасываются.
    const code = src
        .split('\n')
        .filter((l) => !/^\s*(\/\/|\*|\/\*)/.test(l))
        .join('\n');
    for (const bad of ['innerHTML', 'insertAdjacentHTML', 'outerHTML', 'document.write']) {
        ok(!code.includes(bad), `в коде модуля нет ${bad}`);
    }
    ok(!/\bdocument\b/.test(code), 'модуль не обращается к глобальному document');
    ok(src.includes('new URL('), 'проверка ссылок идёт через new URL');
    // Самопроверка стража: он обязан УМЕТЬ найти запрещённое.
    ok('const x = el.innerHTML;'.includes('innerHTML'), 'самопроверка: страж отличает innerHTML');
}

// ==========================================================================
// СЕКЦИЯ 5. Устойчивость
// ==========================================================================

const started = Date.now();

{
    const long = 'слово '.repeat(20000);
    const ast = M.parseMarkdown(long);
    eq(ast.length, 1, 'очень длинная строка: один абзац');
    ok(render(long).childNodes.length === 1, 'очень длинная строка рендерится');
}
{
    const stars = '*'.repeat(5000);
    ok(Array.isArray(M.parseMarkdown(stars)), 'строка из 5000 звёздочек не роняет и не вешает');
}
{
    const nested = Array.from({ length: 10 }, (_, i) => `${'  '.repeat(i)}- уровень ${i}`).join('\n');
    const ast = M.parseMarkdown(nested);
    eq(ast.length, 1, 'вложенность в 10 уровней: один блок');
    let node = ast[0];
    let levels = 1;
    while (node.items[0] && node.items[0].children.length > 0) {
        node = node.items[0].children[0];
        levels += 1;
    }
    eq(levels, 10, 'вложенность в 10 уровней разобрана целиком');
    ok(findAll(render(nested), 'ul').length === 10, 'вложенность в 10 уровней рендерится');
}
{
    const deepQuote = `${'> '.repeat(60)}текст`;
    ok(Array.isArray(M.parseMarkdown(deepQuote)), 'цитата в 60 уровней не роняет парсер');
    ok(render(deepQuote), 'цитата в 60 уровней рендерится');
}
{
    const garbage = '|||\n---\n```\n> > >\n- - -\n[](\n**~~`_*';
    ok(Array.isArray(M.parseMarkdown(garbage)), 'мусорный вход не роняет парсер');
    ok(render(garbage), 'мусорный вход рендерится');
}
{
    const mixed = '# Привет 👋\n\n- пункт с эмодзи 🚀 и **жирным**\n- ещё 🇷🇺\n\n> цитата — тире';
    const frag = render(mixed);
    ok(frag.textContent.includes('👋'), 'эмодзи не теряются');
    ok(frag.textContent.includes('🇷🇺'), 'составные эмодзи не теряются');
    ok(frag.textContent.includes('цитата — тире'), 'кириллица и тире на месте');
}

const elapsed = Date.now() - started;
ok(elapsed < 5000, `секция устойчивости укладывается в 5 с (фактически ${elapsed} мс)`);

// ==========================================================================

if (failures === 0) console.log(`[OK] messenger_markdown: ${checks} проверок`);
else console.error(`[FAIL] messenger_markdown: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
