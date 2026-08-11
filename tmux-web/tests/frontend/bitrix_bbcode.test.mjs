// СТРАЖ БЕЗОПАСНОГО РЕНДЕРА BB-СЕГМЕНТОВ панели «Битрикс24» (Ф5).
//
// Что охраняется: текст сообщения приходит с портала, где его пишут люди и
// боты, и попасть в документ он обязан ТОЛЬКО узлами. Формально это выражается
// тремя утверждениями, каждое из которых проверяется ниже:
//
//   1. `renderSegments` СОЗДАЁТ УЗЛЫ (`createElement` / `createTextNode`), а
//      не возвращает строку HTML. Строка означала бы, что где-то во фронте
//      есть присваивание `innerHTML`.
//   2. `[B]<img src=x onerror=alert(1)>[/B]` попадает в документ ТЕКСТОМ:
//      разметка внутри BB-тега — это содержимое, а не разметка.
//   3. Незнакомый BB-тег отображается КАК ЕСТЬ, вместе со скобками: молча
//      выбрасывать неизвестное — значит незаметно менять смысл сообщения
//      (`[SPOILER]пароль[/SPOILER]` превратился бы в «пароль»).
//
// Отдельный файл, а не часть bitrix_format.test.mjs: тот проверяет, что рендер
// РАБОТАЕТ, этот — что он БЕЗОПАСЕН. Первый упадёт от смены классов, второй
// обязан падать только от настоящей дыры.
//
// Документ — свой крошечный фейк: `renderSegments` принимает его аргументом
// ровно ради этого, и глобального `document` здесь нет вовсе. Заведи функция
// глобальную зависимость — этот тест перестал бы её ловить.
//
// Запуск: node tmux-web/tests/frontend/bitrix_bbcode.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const F = await import(new URL('../../static/js/bitrix/format.js', import.meta.url).href);
const A = await import(new URL('../../static/js/bitrix/attachments.js', import.meta.url).href);

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
// Фейковый документ, который ЗАПРЕЩАЕТ всё, кроме узлов.
//
// `innerHTML` объявлен геттером-ловушкой: любая попытка присвоить его
// немедленно валит тест, а не всплывает где-то позже пустой строкой.
// ---------------------------------------------------------------------------

const created = { elements: [], texts: [] };

function makeDoc() {
    const create = (tag) => {
        const node = {
            nodeType: 1,
            tagName: String(tag).toLowerCase(),
            className: '',
            dataset: {},
            style: {},
            childNodes: [],
            appendChild(child) {
                this.childNodes.push(child);
                return child;
            },
            get textContent() {
                return this.childNodes.map((c) => c.textContent).join('');
            },
            set textContent(v) {
                this.childNodes = [{ nodeType: 3, tagName: null, textContent: String(v) }];
            },
        };
        Object.defineProperty(node, 'innerHTML', {
            set() {
                failures += 1;
                console.error('[FAIL] модуль присвоил innerHTML — это запрещено в панели');
            },
            get() {
                return '';
            },
        });
        created.elements.push(node);
        return node;
    };
    return {
        createElement: create,
        createTextNode: (data) => {
            const node = { nodeType: 3, tagName: null, textContent: String(data) };
            created.texts.push(node);
            return node;
        },
    };
}

/** Все узлы дерева в порядке обхода. */
function walk(node, out = []) {
    out.push(node);
    for (const c of (node.childNodes || [])) walk(c, out);
    return out;
}

// ---------------------------------------------------------------------------
// 1. Рендер создаёт УЗЛЫ, а не строку
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    // То, что отдаёт `bbcode.rs::to_segments` для
    // «[B]жирно[/B] и [USER=15 REPLACE]Максим[/USER]».
    const segments = [
        { kind: 'b', text: '', value: null, children: [{ kind: 'text', text: 'жирно', children: [] }] },
        { kind: 'text', text: ' и ', children: [] },
        { kind: 'user', text: '', value: '15', children: [{ kind: 'text', text: 'Максим', children: [] }] },
    ];
    const box = F.renderSegments(segments, doc);

    ok(box && box.nodeType === 1, 'результат — УЗЕЛ, а не строка');
    ok(typeof box !== 'string', 'функция не возвращает HTML-строку');
    eq(box.className, 'b24-segs', 'корневой контейнер имеет свой класс');
    eq(box.textContent, 'жирно и Максим', 'текст собран из узлов');

    const nodes = walk(box);
    ok(nodes.some((n) => n.tagName === 'strong'), '[B] стал элементом strong');
    const user = nodes.find((n) => n.className === 'b24-seg-user');
    ok(!!user, '[USER] получил свой класс');
    eq(user.dataset.value, '15', 'идентификатор уехал в dataset, а не в href');
    ok(!('href' in user), 'у сегмента нет href вовсе');
}

// ---------------------------------------------------------------------------
// 2. XSS-строка попадает в документ ТЕКСТОМ
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    const payload = '<img src=x onerror=alert(1)>';
    // `[B]<img …>[/B]`: разметка внутри BB-тега — это СОДЕРЖИМОЕ.
    const box = F.renderSegments(
        [{ kind: 'b', text: '', children: [{ kind: 'text', text: payload, children: [] }] }],
        doc,
    );

    const nodes = walk(box);
    ok(!nodes.some((n) => n.tagName === 'img'), 'элемент img не создан');
    const texts = nodes.filter((n) => n.nodeType === 3).map((n) => n.textContent);
    ok(texts.includes(payload), 'полезная нагрузка лежит в ТЕКСТОВОМ узле как есть');
    eq(box.textContent, payload, 'текст сообщения не изменился ни на символ');

    // `javascript:` в адресе ссылки не превращается в кликабельный переход:
    // сегмент `url` — это span с данными, а не <a href>.
    const link = F.renderSegments(
        [{ kind: 'url', text: '', value: 'javascript:alert(1)', children: [] }],
        doc,
    );
    const node = walk(link).find((n) => n.className === 'b24-seg-url');
    ok(!!node && node.tagName === 'span', 'ссылка рисуется span, а не <a>');
    ok(!('href' in node), 'href не появляется даже у сегмента-ссылки');
    eq(node.dataset.value, 'javascript:alert(1)', 'адрес остаётся данными');
}

// ---------------------------------------------------------------------------
// 3. Незнакомый тег виден КАК ЕСТЬ
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    // Так `bbcode.rs` отдаёт незнакомый тег: обычным текстом, вместе со
    // скобками (см. шапку bbcode.rs — молчаливое выбрасывание меняло бы смысл).
    const box = F.renderSegments(
        [{ kind: 'text', text: '[SPOILER]пароль[/SPOILER]', children: [] }],
        doc,
    );
    eq(box.textContent, '[SPOILER]пароль[/SPOILER]', 'незнакомый тег виден целиком');

    // А незнакомый ВИД сегмента (портал добавил новый) рисуется нейтральным
    // узлом, а не теряется.
    const odd = F.renderSegments(
        [{ kind: 'hologram', text: '', value: 'x', children: [{ kind: 'text', text: 'нечто', children: [] }] }],
        doc,
    );
    const node = walk(odd).find((n) => n.className === 'b24-seg-unknown');
    ok(!!node, 'незнакомый вид сегмента получает нейтральный класс');
    eq(odd.textContent, 'нечто', 'его содержимое не теряется');
}

// ---------------------------------------------------------------------------
// 3b. `[TIMESTAMP]` показывается ДАТОЙ, а не десятизначным числом
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    // Живой пример из системной строки чата задачи: «Крайний срок задачи,
    // 1783180800». Число в ленте — это не «строго», а нечитаемо.
    const box = F.renderSegments([{ kind: 'timestamp', text: '', value: '1783180800', children: [] }], doc);
    ok(!box.textContent.includes('1783180800'), 'unix-время не показывается числом');
    ok(/^\d{2}\.\d{2}\.\d{4} \d{2}:\d{2}$/.test(box.textContent), `дата собрана человеческой строкой: ${box.textContent}`);

    const node = walk(box).find((n) => n.className === 'b24-seg-time');
    eq(node.dataset.value, '1783180800', 'исходное значение осталось в dataset');

    eq(F.timestampLabel(''), '', 'пустое значение не превращается в 1970 год');
    eq(F.timestampLabel('чепуха'), '', 'мусор не разбирается');
    ok(F.timestampLabel('2026-08-05T20:14:43+03:00').startsWith('05.08.2026'), 'дата строкой тоже разбирается');
    // Значение, которое не разобралось, обязано остаться видимым как есть.
    const raw = F.renderSegments([{ kind: 'timestamp', text: '', value: 'позже', children: [] }], doc);
    eq(raw.textContent, 'позже', 'неразобранное значение показывается как есть');
}

// ---------------------------------------------------------------------------
// 4. Устойчивость: мусор вместо сегментов не роняет рендер
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    eq(F.renderSegments(null, doc).textContent, '', 'null не роняет рендер');
    eq(F.renderSegments([], doc).textContent, '', 'пустой список даёт пустой узел');
    eq(F.renderSegments([null, 42, 'строка'], doc).textContent, '', 'мусор в списке пропускается');

    // Глубина ограничена: дерево в тысячу уровней не должно переполнять стек.
    let deep = { kind: 'text', text: 'дно', children: [] };
    for (let i = 0; i < 200; i += 1) deep = { kind: 'quote', text: '', children: [deep] };
    const box = F.renderSegments([deep], doc);
    ok(box.nodeType === 1, 'глубокая вложенность не роняет рендер');
}

// ---------------------------------------------------------------------------
// 5. Карточки ATTACH рисуются теми же узлами
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    const cards = [{
        id: '1785950436',
        color: '#8d51eb',
        description: '',
        blocks: [
            {
                kind: 'message',
                text: '<b>не разметка</b>',
                segments: [{ kind: 'text', text: '<b>не разметка</b>', children: [] }],
                links: [],
                rows: [],
            },
            {
                kind: 'link',
                text: null,
                segments: [],
                links: [{ name: 'Ссылка', url: 'https://example.com', desc: 'описание' }],
                rows: [],
            },
            {
                kind: 'grid',
                text: null,
                segments: [],
                links: [],
                rows: [{ name: 'Поле', value: 'Значение', display: 'line', color: null }],
            },
        ],
    }];
    const box = A.buildCards(cards, doc);
    ok(!!box, 'карточка собралась');
    const nodes = walk(box);

    ok(nodes.some((n) => n.className === 'b24-card-message'), 'блок MESSAGE нарисован');
    ok(nodes.some((n) => n.className === 'b24-card-links'), 'блок LINK нарисован');
    ok(nodes.some((n) => n.className === 'b24-card-grid'), 'блок GRID нарисован');
    ok(!nodes.some((n) => n.tagName === 'a'), 'ссылка карточки НЕ является <a href>');
    ok(!nodes.some((n) => n.tagName === 'b'), 'разметка из текста блока не стала элементом');
    ok(box.textContent.includes('<b>не разметка</b>'), 'текст блока виден как есть');
    ok(box.textContent.includes('https://example.com'), 'адрес показан текстом');
    ok(box.textContent.includes('Поле') && box.textContent.includes('Значение'), 'строка GRID видна');

    // Пустая карточка не рисует пустой рамки.
    eq(A.buildCards([], doc), null, 'без карточек контейнера нет');
    eq(A.buildCards(null, doc), null, 'null не роняет сборку карточек');
    eq(A.buildCards([{ blocks: [] }], doc), null, 'карточка без блоков не рисуется');
}

// ---------------------------------------------------------------------------
// 6. Карточка вложения: подпись, размер, вид
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    const media = [{
        index: 0,
        file_id: 95,
        kind: 'file',
        name: 'forge_probe.txt',
        extension: 'txt',
        size: 500,
        has_thumb: false,
        has_full: true,
        player_url: null,
        waveform: [],
    }];
    const box = A.buildMediaList(media, doc);
    ok(!!box, 'карточка вложения собралась');
    ok(box.textContent.includes('forge_probe.txt'), 'имя файла показано');
    ok(box.textContent.includes('500 Б'), 'размер показан человеческой строкой');

    const card = walk(box).find((n) => typeof n.className === 'string' && n.className.split(' ').includes('b24-media-card'));
    eq(card.dataset.fileId, '95', 'file_id уехал в dataset — им адресуется прокси (Ф8)');
    eq(card.dataset.index, '0', 'индекс вложения сохранён: он часть ключа кэша');
    ok(!nodesHaveSrc(box), 'ни один узел карточки не несёт внешнего src');

    eq(A.buildMediaList([], doc), null, 'без вложений контейнера нет');
    eq(A.mediaLabel({ kind: 'image', size: 0 }), 'изображение', 'вложение без имени подписано видом');
    eq(F.formatBytes(0), '', 'нулевой размер не показывается');
    eq(F.formatBytes(1536), '1.5 КБ', 'килобайты с одним знаком');
}

function nodesHaveSrc(root) {
    return walk(root).some((n) => n && typeof n === 'object' && 'src' in n);
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_bbcode)`);
if (failures > 0) process.exit(1);
