// Регресс-тест белого списка видимых чатов и папок (forge-c2dt).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/visibility.js через
// `await import()`. Стаб DOM не нужен: модуль зависит только от format.js,
// а тот намеренно не импортирует ни DOM, ни core/*.
//
// Что покрыто: разбор настроек в рабочий фильтр, правило «включён, но пусто =
// показываем всё», видимость по поимённому выбору и по папкам (включая
// служебные inbox/archive), строгость к чатам вне загруженной страницы,
// пометка пустых пунктов навигации, сводка для UI и чистка исчезнувших чатов.
//
// Запуск: node tmux-web/tests/frontend/telegram_visibility.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/telegram/visibility.js', import.meta.url).href;
const V = await import(modUrl);

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

/** Настройки в том виде, в каком их отдаёт /api/user-settings. */
function settings(enabled, folders, dialogs) {
    return {
        telegram_filter_enabled: enabled,
        telegram_visible_folders: folders || [],
        telegram_visible_dialogs: dialogs || [],
    };
}

/** Папка аккаунта (dialogFilter) в формате DTO плагина. */
const WORK = { id: 12, title: 'Работа', include_ids: ['100', '101'] };
const BOTS = { id: 34, title: 'Боты', bots: true };
const FOLDERS = [WORK, BOTS];

const D = {
    inWork: { id: '100', title: 'Коллега', kind: 'user', contact: true },
    alsoWork: { id: '101', title: 'Отдел', kind: 'group' },
    bot: { id: '200', title: 'Бот', kind: 'user', bot: true },
    stranger: { id: '300', title: 'Незнакомец', kind: 'user' },
    archived: { id: '400', title: 'Старый', kind: 'user', archived: true },
};
const ALL = [D.inWork, D.alsoWork, D.bot, D.stranger, D.archived];

// ---------- normalizeFilter ----------

{
    const f = V.normalizeFilter(null);
    eq(f.enabled, false, 'разбор: null → выключен');
    eq(f.active, false, 'разбор: null → не действует');
    eq(f.folders, [], 'разбор: пустой список папок');
    eq(f.dialogs, [], 'разбор: пустой список чатов');
}
eq(V.normalizeFilter({}).active, false, 'разбор: пустые настройки → не действует');
eq(V.normalizeFilter('мусор').active, false, 'разбор: строка вместо объекта не роняет');
eq(V.normalizeFilter(settings(true, [], [])).enabled, true, 'разбор: тумблер прочитан');
// Ключевое правило: включённый тумблер с пустым выбором показывал бы пустую
// панель, и пользователь прочитал бы это как поломку.
eq(V.normalizeFilter(settings(true, [], [])).active, false,
    'разбор: включён, но ничего не выбрано → фильтр НЕ действует');
eq(V.normalizeFilter(settings(true, ['inbox'], [])).active, true,
    'разбор: включён + выбрана папка → действует');
eq(V.normalizeFilter(settings(true, [], ['100'])).active, true,
    'разбор: включён + выбран чат → действует');
// Выключенный тумблер с непустым выбором — обычное состояние: пользователь
// временно отключил фильтр, не потеряв список.
eq(V.normalizeFilter(settings(false, ['inbox'], ['100'])).active, false,
    'разбор: выключён, но выбор есть → не действует');
eq(V.normalizeFilter(settings(false, ['inbox'], ['100'])).folders, ['inbox'],
    'разбор: выбор сохраняется и при выключенном тумблере');

// Мусор в списках.
{
    const f = V.normalizeFilter(settings(true, ['  inbox  ', '', 'inbox', null], [100, '100', '  ']));
    eq(f.folders, ['inbox'], 'разбор: пробелы обрезаны, дубли и пустые убраны (папки)');
    eq(f.dialogs, ['100'], 'разбор: число приведено к строке, дубль убран (чаты)');
    eq(V.normalizeFilter({ telegram_visible_folders: 'нет' }).folders, [],
        'разбор: не-массив не роняет');
}

// ---------- filterActive / dialogVisible при выключенном фильтре ----------

eq(V.filterActive(null), false, 'действует: null → нет');
eq(V.filterActive(V.normalizeFilter(settings(true, ['inbox']))), true, 'действует: да');

{
    const off = V.normalizeFilter(null);
    ok(V.dialogVisible(D.stranger, off, FOLDERS), 'выключен: видно всё');
    eq(V.visibleDialogs(ALL, off, FOLDERS).length, ALL.length, 'выключен: список не режется');
    // Копия, а не тот же массив: вызывающий сортирует и режет результат.
    ok(V.visibleDialogs(ALL, off, FOLDERS) !== ALL, 'выключен: возвращается копия списка');
    ok(V.dialogIdVisible('999', off, [], FOLDERS), 'выключен: неизвестный чат тоже виден');
}

// ---------- видимость по поимённому выбору ----------

{
    const f = V.normalizeFilter(settings(true, [], ['300']));
    ok(V.dialogVisible(D.stranger, f, FOLDERS), 'поимённо: выбранный чат виден');
    ok(!V.dialogVisible(D.inWork, f, FOLDERS), 'поимённо: невыбранный скрыт');
    eq(V.visibleDialogs(ALL, f, FOLDERS).map((d) => d.id), ['300'],
        'поимённо: в списке остаётся только он');
    // Порядок сервера обязан сохраняться — в колонке закреплённые идут сверху.
    const many = V.normalizeFilter(settings(true, [], ['400', '100']));
    eq(V.visibleDialogs(ALL, many, FOLDERS).map((d) => d.id), ['100', '400'],
        'поимённо: порядок исходного списка сохранён, а не порядок выбора');
}

// ---------- видимость по папкам ----------

{
    const f = V.normalizeFilter(settings(true, ['folder:12'], []));
    ok(V.dialogVisible(D.inWork, f, FOLDERS), 'папка: чат из include_ids виден');
    ok(V.dialogVisible(D.alsoWork, f, FOLDERS), 'папка: второй чат папки виден');
    ok(!V.dialogVisible(D.bot, f, FOLDERS), 'папка: бот вне папки скрыт');
    ok(!V.dialogVisible(D.stranger, f, FOLDERS), 'папка: посторонний скрыт');
}

{
    // Категорийная папка (боты) — правила те же, что у колонки.
    const f = V.normalizeFilter(settings(true, ['folder:34'], []));
    ok(V.dialogVisible(D.bot, f, FOLDERS), 'папка ботов: бот виден');
    ok(!V.dialogVisible(D.inWork, f, FOLDERS), 'папка ботов: человек скрыт');
}

{
    // Служебные пункты навигации.
    const inbox = V.normalizeFilter(settings(true, ['inbox'], []));
    ok(V.dialogVisible(D.stranger, inbox, FOLDERS), 'inbox: неархивный виден');
    ok(!V.dialogVisible(D.archived, inbox, FOLDERS), 'inbox: архивный скрыт');
    const archive = V.normalizeFilter(settings(true, ['archive'], []));
    ok(V.dialogVisible(D.archived, archive, FOLDERS), 'archive: архивный виден');
    ok(!V.dialogVisible(D.stranger, archive, FOLDERS), 'archive: неархивный скрыт');
}

{
    // Несколько папок разом + поимённый чат.
    const f = V.normalizeFilter(settings(true, ['folder:34', 'archive'], ['300']));
    eq(V.visibleDialogs(ALL, f, FOLDERS).map((d) => d.id).sort(), ['200', '300', '400'],
        'смешанный выбор: объединение папок и поимённых чатов');
}

{
    // Папка исчезла (удалена на другом устройстве) или ещё не загружена.
    const f = V.normalizeFilter(settings(true, ['folder:99'], []));
    ok(!V.dialogVisible(D.inWork, f, FOLDERS), 'исчезнувшая папка: совпадений не даёт');
    ok(!V.dialogVisible(D.inWork, f, []), 'папки не загружены: совпадений не даёт');
    // Но запись из настроек не теряется — папка может вернуться.
    eq(f.folders, ['folder:99'], 'исчезнувшая папка: остаётся в настройках');
}

eq(V.dialogVisible(null, V.normalizeFilter(settings(true, ['inbox'])), FOLDERS), false,
    'видимость: null-диалог при действующем фильтре скрыт');
eq(V.visibleDialogs(null, V.normalizeFilter(settings(true, ['inbox'])), FOLDERS), [],
    'видимость: не-массив не роняет');

// ---------- dialogIdVisible: чаты вне загруженной страницы ----------

{
    const f = V.normalizeFilter(settings(true, ['folder:12'], ['300']));
    ok(V.dialogIdVisible('100', f, ALL, FOLDERS), 'по id: известный чат из папки виден');
    ok(!V.dialogIdVisible('200', f, ALL, FOLDERS), 'по id: известный чат вне выбора скрыт');
    ok(V.dialogIdVisible('300', f, ALL, FOLDERS), 'по id: поимённо выбранный виден');
    // Главное правило строгости: WS вещает по всему аккаунту, и чат, которого
    // нет в загруженной странице, при действующем фильтре обязан молчать —
    // иначе фильтр «протекает» уведомлениями.
    ok(!V.dialogIdVisible('999', f, ALL, FOLDERS), 'по id: неизвестный чат скрыт');
    // Но поимённый выбор работает и без загруженного списка: он не требует DTO.
    ok(V.dialogIdVisible('300', f, [], FOLDERS), 'по id: поимённый выбор не требует списка');
    ok(!V.dialogIdVisible('', f, ALL, FOLDERS), 'по id: пустой id скрыт');
    ok(!V.dialogIdVisible(null, f, ALL, FOLDERS), 'по id: null скрыт');
    // Числовой id обязан совпасть со строковым: id приходят и числом, и строкой.
    ok(V.dialogIdVisible(300, f, ALL, FOLDERS), 'по id: число приводится к строке');
}

// ---------- folderKey / folderByKey ----------

eq(V.folderKey(WORK), 'folder:12', 'ключ: строится из id');
eq(V.folderKey({ id: 0 }), 'folder:0', 'ключ: id=0 — валидный id, а не «пусто»');
eq(V.folderKey(null), null, 'ключ: null не роняет');
eq(V.folderKey({}), null, 'ключ: без id → null');
eq(V.folderByKey('folder:12', FOLDERS), WORK, 'поиск: папка найдена');
eq(V.folderByKey('folder:99', FOLDERS), null, 'поиск: неизвестная → null');
eq(V.folderByKey('inbox', FOLDERS), null, 'поиск: служебный пункт папкой не является');
eq(V.folderByKey('folder:12', null), null, 'поиск: без списка не роняет');

// ---------- emptyDirKeys ----------

{
    const entries = [
        { key: 'inbox', folder: null },
        { key: 'folder:12', folder: WORK },
        { key: 'folder:34', folder: BOTS },
        { key: 'archive', folder: null },
    ];
    const f = V.normalizeFilter(settings(true, ['folder:12'], []));
    const empty = V.emptyDirKeys(entries, ALL, f, FOLDERS);
    ok(!empty.has('inbox'), 'пустые пункты: inbox не пуст — видимые чаты неархивные');
    ok(!empty.has('folder:12'), 'пустые пункты: выбранная папка не пуста');
    ok(empty.has('folder:34'), 'пустые пункты: папка ботов пуста при этом выборе');
    ok(empty.has('archive'), 'пустые пункты: архив пуст');
    // При выключенном фильтре пунктов-пустышек не бывает по определению:
    // приглушать в этом режиме нечего.
    eq(V.emptyDirKeys(entries, ALL, V.normalizeFilter(null), FOLDERS).size, 0,
        'пустые пункты: выключенный фильтр ничего не помечает');
    eq(V.emptyDirKeys(null, ALL, f, FOLDERS).size, 0, 'пустые пункты: без entries не роняет');
}

// ---------- filterSummary ----------

eq(V.filterSummary(V.normalizeFilter(null)), 'Показаны все чаты.',
    'сводка: выключен');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], []))),
    'Ничего не выбрано — пока показаны все чаты.',
    'сводка: включён без выбора честно говорит, что фильтр не действует');
eq(V.filterSummary(V.normalizeFilter(settings(true, ['inbox'], ['1']))),
    'Показаны только: 1 папка и 1 чат.', 'сводка: единственное число');
eq(V.filterSummary(V.normalizeFilter(settings(true, ['inbox', 'archive'], ['1', '2', '3']))),
    'Показаны только: 2 папки и 3 чата.', 'сводка: 2–4 — форма «папки/чата»');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], ['1', '2', '3', '4', '5']))),
    'Показаны только: 5 чатов.', 'сводка: 5+ — форма «чатов», папки не упомянуты');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], Array.from({ length: 11 }, (_, i) => String(i))))),
    'Показаны только: 11 чатов.', 'сводка: 11 — исключение из правила «оканчивается на 1»');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], Array.from({ length: 21 }, (_, i) => String(i))))),
    'Показаны только: 21 чат.', 'сводка: 21 — форма «чат»');
eq(V.filterSummary(null), 'Показаны все чаты.', 'сводка: null не роняет');

// ---------- pruneMissingDialogs ----------

{
    const kept = V.pruneMissingDialogs(['100', '999', '200'], ALL);
    eq(kept, ['100', '200'], 'чистка: несуществующий id убран, порядок сохранён');
    eq(V.pruneMissingDialogs(['100'], []), [], 'чистка: пустой список чатов убирает всё');
    eq(V.pruneMissingDialogs(null, ALL), [], 'чистка: null не роняет');
    eq(V.pruneMissingDialogs(['100', '100'], ALL), ['100'], 'чистка: дубль схлопывается');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/telegram_visibility)`);
if (failures > 0) process.exit(1);
