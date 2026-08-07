// Регресс-тест чистой логики панели «Битрикс24» (Ф4).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/bitrix/format.js через
// `await import()`. Фейковый DOM не нужен: модуль не импортирует ни узлов, ни
// сети, ни состояния, а рендер сегментов принимает документ АРГУМЕНТОМ — ровно
// ради того, чтобы его можно было проверить здесь.
//
// Что покрыто:
//   • разбор дат портала (ATOM со смещением) и относительный возраст;
//   • превью последнего сообщения: своё / системное / чужое в группе, файл без
//     текста, обрезка по СИМВОЛАМ (эмодзи не разрезаются пополам);
//   • счётчики и заголовок секции;
//   • директории: `dirMatches` обязан совпадать с серверным `dir_matches`;
//   • склейка и сортировка страниц списка;
//   • РЕНДЕР BB-СЕГМЕНТОВ УЗЛАМИ: генерируются узлы, а не строка HTML, и
//     `<script>` из текста сообщения попадает в документ ТЕКСТОМ.
//
// Запуск: node tmux-web/tests/frontend/bitrix_format.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/bitrix/format.js', import.meta.url).href;
const F = await import(modUrl);

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
// Крошечный фейковый документ: ровно то, чем пользуется renderSegments.
//
// Свой, а не bitrix_dom_stub.mjs: стаб ставит глобальный `document` и тянет за
// собой разметку панели, а здесь проверяется ЧИСТАЯ функция, которой документ
// приходит аргументом. Заведи она глобальную зависимость — этот тест перестал
// бы её ловить.
// ---------------------------------------------------------------------------

function makeDoc() {
    const create = (tag) => ({
        nodeType: 1,
        tagName: String(tag).toLowerCase(),
        className: '',
        dataset: {},
        childNodes: [],
        appendChild(child) {
            this.childNodes.push(child);
            return child;
        },
        get textContent() {
            return this.childNodes.map((c) => c.textContent).join('');
        },
    });
    return {
        createElement: create,
        createTextNode: (data) => ({ nodeType: 3, tagName: null, textContent: String(data) }),
    };
}

/** Все узлы дерева в порядке обхода. */
function walk(node, out = []) {
    out.push(node);
    for (const c of (node.childNodes || [])) walk(c, out);
    return out;
}

// ---------------------------------------------------------------------------
// Время
// ---------------------------------------------------------------------------

{
    const iso = '2026-08-05T20:14:43+03:00';
    const ms = F.parseDate(iso);
    ok(ms === Date.parse(iso), 'дата портала (ATOM со смещением) разбирается');
    eq(F.parseDate('чепуха'), null, 'мусор даёт null, а не NaN');
    eq(F.parseDate(''), null, 'пустая строка — это «даты нет»');
    eq(F.parseDate(null), null, 'null не роняет разбор');
    eq(F.parseDate(false), null, 'портал шлёт false вместо даты — тоже null');

    eq(F.formatTime('чепуха'), '', 'без даты время не выдумывается');
    ok(/^\d{2}:\d{2}$/.test(F.formatTime(iso)), 'время суток в формате ЧЧ:ММ');

    const now = Date.parse('2026-08-05T21:00:00+03:00');
    eq(F.relativeAge('2026-08-05T20:59:40+03:00', now), 'только что', 'возраст: 20 секунд');
    eq(F.relativeAge('2026-08-05T20:55:00+03:00', now), '5м', 'возраст: минуты');
    eq(F.relativeAge('2026-08-05T18:00:00+03:00', now), '3ч', 'возраст: часы');
    eq(F.relativeAge('2026-08-03T21:00:00+03:00', now), '2д', 'возраст: дни');
    eq(F.relativeAge('2026-07-09T03:14:47+03:00', now), '09.07', 'старше недели — дата');
    eq(F.relativeAge(null, now), '', 'без даты возраст пуст');
    // Часы браузера могут отставать от портала — «отрицательный» возраст не
    // должен превращаться в «-1м».
    eq(F.relativeAge('2026-08-05T21:05:00+03:00', now), 'только что', 'будущее не даёт минуса');

    eq(F.dayKey('чепуха'), '', 'ключ дня без даты пуст');
    ok(/^\d{4}-\d{2}-\d{2}$/.test(F.dayKey(iso)), 'ключ дня — локальная дата');
}

// ---------------------------------------------------------------------------
// Строки, имена, значки
// ---------------------------------------------------------------------------

{
    eq(F.truncate('короткая', 20), 'короткая', 'короткая строка не трогается');
    eq(F.truncate('абвгдежз', 4), 'абв…', 'длинная обрезается с многоточием');
    eq(Array.from(F.truncate('абвгдежз', 4)).length, 4, 'длина считается в СИМВОЛАХ');
    eq(F.truncate(null, 5), '', 'null не роняет');
    // Эмодзи занимает две единицы UTF-16: `slice` разрезал бы его пополам и
    // оставил в DOM сломанный символ.
    const emoji = '🙂🙂🙂🙂';
    const cut = F.truncate(emoji, 3);
    ok(!cut.includes('�') && Array.from(cut).length === 3, 'эмодзи не режется пополам');

    eq(F.displayName({ id: 'chat7', title: 'Задача' }), 'Задача', 'заголовок из title');
    eq(F.displayName({ id: 'chat7', title: '   ' }), 'chat7', 'пустой title заменяется id');
    eq(F.displayName(null), '', 'без диалога заголовка нет');

    eq(F.dialogKind({ kind: 'task' }), 'task', 'вид берётся из DTO');
    eq(F.dialogKind({ kind: 'что-то новое' }), 'chat', 'незнакомый вид — обычный чат');
    eq(F.dialogKind('user'), 'user', 'вид можно передать строкой');
    eq(F.kindGlyph({ kind: 'task' }), '#', 'у задачи свой глиф');
    eq(F.kindGlyph({ kind: 'user' }), '@', 'у личного диалога свой глиф');
    ok(F.DIALOG_KINDS.every((k) => typeof F.kindGlyph(k) === 'string'),
        'глиф есть у каждого вида словаря');
}

// ---------------------------------------------------------------------------
// Превью последнего сообщения
// ---------------------------------------------------------------------------

{
    const base = { id: 'chat3', kind: 'chat', unread_count: 0 };
    const msg = (over) => ({ ...base, last_message: { text: '', author_id: '1', ...over } });

    eq(F.previewText(msg({ text: 'привет', author_name: 'Максим' })),
        'Максим: привет', 'в групповом чате перед текстом имя автора');
    eq(F.previewText({ ...msg({ text: 'привет', author_name: 'Максим' }), kind: 'user' }),
        'привет', 'в личном диалоге имя не дублируется');
    eq(F.previewText(msg({ text: 'привет', out: true, author_name: 'Я' })),
        'Вы: привет', 'своё сообщение помечается «Вы:»');
    eq(F.previewText(msg({ text: 'Игорь добавил права', system: true, author_id: '0' })),
        'Игорь добавил права', 'у системного сообщения автора нет');
    eq(F.previewText(msg({ text: '', has_file: true })),
        '[файл]', 'файл без подписи — не пустая строка');
    eq(F.previewText(msg({ text: '  много\n  пробелов\t' , author_name: 'А' })),
        'А: много пробелов', 'переносы схлопываются в пробел');
    eq(F.previewText({ ...base, last_message: null }), '', 'без сообщения превью пусто');
    eq(F.previewText(null), '', 'без диалога превью пусто');
    ok(Array.from(F.previewText(msg({ text: 'я'.repeat(500) }))).length <= F.PREVIEW_CHARS,
        'превью обрезается по PREVIEW_CHARS');
}

// ---------------------------------------------------------------------------
// Счётчики и метки
// ---------------------------------------------------------------------------

{
    eq(F.unreadBadgeText(0), '', 'нулевой счётчик не рисуется');
    eq(F.unreadBadgeText(7), '7', 'обычный счётчик');
    eq(F.unreadBadgeText(150), '99+', 'больше сотни — 99+');
    eq(F.unreadBadgeText(-3), '', 'отрицательный счётчик не бывает');
    eq(F.unreadBadgeText(NaN), '', 'NaN не попадает в DOM');

    const now = Date.parse('2026-08-05T21:00:00+03:00');
    eq(F.dialogMark({ unread_count: 3, date_update: '2026-08-05T20:55:00+03:00' }, now),
        '3', 'непрочитанное важнее возраста');
    eq(F.dialogMark({ unread_count: 0, date_update: '2026-08-05T20:55:00+03:00' }, now),
        '5м', 'без непрочитанного показывается возраст');

    eq(F.nodesTitle('tasks', 2), '// ~/tasks — 2 NODES', 'заголовок секции');
    eq(F.nodesTitle('чепуха', 0), '// ~/all — 0 NODES', 'незнакомая директория — all');
}

// ---------------------------------------------------------------------------
// Директории (построчная копия серверного dir_matches)
// ---------------------------------------------------------------------------

{
    const task = { id: 'chat7', kind: 'task', unread_count: 0 };
    const chat = { id: 'chat3', kind: 'chat', unread_count: 0 };
    const user = { id: '19', kind: 'user', unread_count: 3 };
    const all = [task, chat, user];

    ok(F.dirMatches('all', task) && F.dirMatches('all', user), 'all — весь список');
    ok(F.dirMatches('tasks', task) && !F.dirMatches('tasks', chat), 'tasks — только задачи');
    ok(F.dirMatches('chats', chat) && F.dirMatches('chats', user), 'chats — всё, кроме задач');
    ok(!F.dirMatches('chats', task), 'чат задачи в chats не попадает');
    ok(F.dirMatches('unread', user) && !F.dirMatches('unread', chat), 'unread — по счётчику');
    ok(F.dirMatches('выдумка', task), 'незнакомая директория показывает всё');
    ok(!F.dirMatches('all', null), 'пустая строка не «попадает» никуда');

    eq(F.dirCounts(all), { all: 3, chats: 2, tasks: 1, unread: 1 }, 'счётчики директорий');
    eq(F.dirCounts(null), { all: 0, chats: 0, tasks: 0, unread: 0 }, 'пустой список не роняет');
    eq(F.unreadTotal(all), 3, 'сумма непрочитанного по строкам');
    eq(F.unreadTotal([{ unread_count: 'мусор' }]), 0, 'нечисло не ломает сумму');
}

// ---------------------------------------------------------------------------
// Склейка и порядок списка
// ---------------------------------------------------------------------------

{
    const d = (id, date, over) => ({ id, date_update: date, unread_count: 0, ...over });
    const page1 = [d('chat3', '2026-07-16T15:10:43+03:00'), d('chat7', '2026-08-05T20:14:43+03:00')];

    eq(F.sortDialogs(page1).map((x) => x.id), ['chat7', 'chat3'], 'порядок — по дате, свежие сверху');
    eq(F.sortDialogs([d('a', '2026-01-01T00:00:00+03:00', { pinned: true }),
        d('b', '2026-08-05T20:14:43+03:00')]).map((x) => x.id),
    ['a', 'b'], 'закреплённые всегда сверху');

    const merged = F.mergeDialogs(page1, [d('chat3', '2026-08-06T10:00:00+03:00', { unread_count: 5 })]);
    eq(merged.length, 2, 'повтор строки не удваивает список');
    eq(merged[0].id, 'chat3', 'обновлённая строка поднялась по дате');
    eq(merged[0].unread_count, 5, 'страница приносит СВЕЖИЕ счётчики');
    eq(F.mergeDialogs(page1, [d('19', '2026-08-04T14:49:37+03:00')]).length, 3,
        'новая строка добавляется');
    eq(F.mergeDialogs(null, null), [], 'null не роняет склейку');
    ok(F.mergeDialogs(page1, []) !== page1, 'склейка возвращает НОВЫЙ массив');
    eq(F.mergeDialogs(page1, [{ title: 'без id' }]).length, 2, 'строка без id отбрасывается');
    eq(F.findDialog(page1, 'chat7').id, 'chat7', 'поиск по идентификатору-строке');
    eq(F.findDialog(page1, 'нет'), null, 'ненайденный диалог даёт null');
}

// ---------------------------------------------------------------------------
// BB-сегменты: УЗЛЫ, а не HTML
// ---------------------------------------------------------------------------

{
    const doc = makeDoc();
    const segments = [
        { kind: 'text', text: 'привет ', value: null, children: [] },
        { kind: 'b', text: '', value: null, children: [{ kind: 'text', text: 'мир', value: null, children: [] }] },
        { kind: 'br', text: '', value: null, children: [] },
        { kind: 'user', text: '', value: '15', children: [{ kind: 'text', text: 'Максим', value: null, children: [] }] },
        { kind: 'url', text: '', value: null, children: [{ kind: 'text', text: 'https://example.com', value: null, children: [] }] },
    ];
    const box = F.renderSegments(segments, doc);

    ok(typeof box === 'object' && box.nodeType === 1, 'рендер возвращает УЗЕЛ, а не строку');
    ok(typeof box !== 'string', 'строки HTML модуль не собирает вовсе');
    eq(box.className, 'b24-segs', 'корневой класс контейнера');

    const nodes = walk(box);
    ok(nodes.some((n) => n.tagName === 'strong'), '[B] стал элементом strong');
    ok(nodes.some((n) => n.tagName === 'br'), '[BR] стал элементом br');
    const user = nodes.find((n) => n.className === 'b24-seg-user');
    ok(!!user, 'упоминание получило свой класс');
    eq(user.dataset.value, '15', 'идентификатор упоминания уехал в dataset');
    const url = nodes.find((n) => n.className === 'b24-seg-url');
    ok(!!url, 'ссылка получила свой класс');
    ok(url.tagName !== 'a', 'ссылка НЕ становится <a href>: адрес пришёл от собеседника');
    ok(nodes.every((n) => n.href === undefined && n.src === undefined),
        'ни один узел не получает href/src из данных портала');
    // `<br>` собственного текста не несёт — перенос виден только в
    // `segmentsText` (ниже), где он и нужен: для превью и подсказок.
    eq(box.textContent, 'привет мирМаксимhttps://example.com', 'текст собирается из узлов');

    // ГЛАВНОЕ: разметка из текста сообщения попадает в документ ТЕКСТОМ.
    const evil = [{ kind: 'text', text: '<script>alert(1)</script> & "кавычки"', value: null, children: [] }];
    const evilBox = F.renderSegments(evil, doc);
    const textNodes = walk(evilBox).filter((n) => n.nodeType === 3);
    eq(textNodes.length, 1, 'опасный текст остался ОДНИМ текстовым узлом');
    eq(textNodes[0].textContent, '<script>alert(1)</script> & "кавычки"',
        'текст не экранируется и не исполняется — он просто текст');
    ok(!walk(evilBox).some((n) => n.tagName === 'script'), 'элемент script не создаётся');

    // Незнакомый вид сегмента не роняет рендер и не теряет текст.
    const unknown = F.renderSegments(
        [{ kind: 'выдумка', text: '', value: 'x', children: [{ kind: 'text', text: 'внутри', value: null, children: [] }] }],
        doc,
    );
    eq(unknown.textContent, 'внутри', 'незнакомый сегмент сохраняет содержимое');
    ok(walk(unknown).some((n) => n.className === 'b24-seg-unknown'), 'незнакомый сегмент помечен классом');

    // Пустое и мусорное — без исключений.
    eq(F.renderSegments([], doc).childNodes.length, 0, 'пустой список даёт пустой контейнер');
    eq(F.renderSegments(null, doc).childNodes.length, 0, 'null не роняет рендер');
    eq(F.renderSegments([null, 42, 'строка'], doc).childNodes.length, 0, 'мусор пропускается');

    // Плоский текст дерева — для превью и title.
    eq(F.segmentsText(segments), 'привет мир\nМаксимhttps://example.com', 'плоский текст дерева');
    eq(F.segmentsText([{ kind: 'user', value: '15', children: [] }]), '15',
        'у сегмента без содержимого текстом становится значение');
    eq(F.segmentsText(null), '', 'null не роняет сборку текста');
}

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

{
    eq(F.errorLabel('not_authorized'), 'Портал не подключён', 'подпись по коду');
    eq(F.errorLabel('scope_missing'), 'У вебхука не хватает прав', 'scope_missing — своя подпись');
    ok(F.errorLabel('что-то новое').length > 0, 'незнакомый код тоже получает подпись');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_format)`);
if (failures > 0) process.exit(1);
