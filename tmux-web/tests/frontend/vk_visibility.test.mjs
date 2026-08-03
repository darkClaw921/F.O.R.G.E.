// Регресс-тест белого списка видимых бесед VK (Ф10 плагина VK).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/vk/visibility.js через
// `await import()`. Стаб DOM не нужен: модуль зависит только от format.js,
// а тот намеренно не импортирует ни DOM, ни core/*.
//
// Что покрыто: разбор настроек в рабочий фильтр, правило «включён, но пусто =
// показываем всё», видимость по типу peer'а и поимённо, ТОЧНОЕ решение по
// беседе вне загруженного списка (главное отличие от телеграмного близнеца, у
// которого решение консервативное), сводка со склонениями и чистка
// исчезнувших бесед.
//
// Запуск: node tmux-web/tests/frontend/vk_visibility.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/vk/visibility.js', import.meta.url).href;
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
function settings(enabled, kinds, peers) {
    return {
        vk_filter_enabled: enabled,
        vk_visible_kinds: kinds || [],
        vk_visible_peers: peers || [],
    };
}

// Беседы всех трёх типов. peer_id: < 0 — сообщество, < 2e9 — человек,
// >= 2e9 — беседа (2_000_000_000 + chat_id).
const D = {
    sofia: { id: '777', title: 'София', kind: 'user' },
    petr: { id: '1024', title: 'Пётр', kind: 'user' },
    devs: { id: '2000000042', title: 'Разработка', kind: 'chat' },
    ops: { id: '2000000043', title: 'Дежурные', kind: 'chat' },
    club: { id: '-70', title: 'Клуб', kind: 'group' },
};
const ALL = [D.sofia, D.petr, D.devs, D.ops, D.club];

// ---------- normalizeFilter ----------

{
    const f = V.normalizeFilter(null);
    eq(f.enabled, false, 'разбор: null → выключен');
    eq(f.active, false, 'разбор: null → не действует');
    eq(f.kinds, [], 'разбор: пустой список типов');
    eq(f.peers, [], 'разбор: пустой список бесед');
}
eq(V.normalizeFilter({}).active, false, 'разбор: пустые настройки → не действует');
eq(V.normalizeFilter('мусор').active, false, 'разбор: строка вместо объекта не роняет');
eq(V.normalizeFilter(settings(true, [], [])).enabled, true, 'разбор: тумблер прочитан');

// ГЛАВНОЕ ПРАВИЛО ФАЗЫ: включённый тумблер с пустым выбором показывал бы
// пустую панель, и пользователь прочитал бы это как поломку, а не как свою
// настройку. Мутация `active = enabled` краснит именно этот ассерт.
eq(V.normalizeFilter(settings(true, [], [])).active, false,
    'разбор: включён, но ничего не выбрано → фильтр НЕ действует');
eq(V.normalizeFilter(settings(true, ['chat'], [])).active, true,
    'разбор: включён + выбран тип → действует');
eq(V.normalizeFilter(settings(true, [], ['777'])).active, true,
    'разбор: включён + выбрана беседа поимённо → действует');
eq(V.normalizeFilter(settings(false, ['chat'], ['777'])).active, false,
    'разбор: выключенный тумблер не действует даже с непустым выбором');

// Дефолт настройки — ВЫКЛЮЧЕНО, поэтому гейт строго `=== true`.
eq(V.normalizeFilter({ vk_filter_enabled: 'true', vk_visible_kinds: ['chat'] }).active, false,
    'разбор: строка "true" не включает фильтр (гейт === true, дефолт ВЫКЛ)');
eq(V.normalizeFilter({ vk_filter_enabled: 1, vk_visible_kinds: ['chat'] }).active, false,
    'разбор: 1 не включает фильтр');

// Чистка списков.
{
    const f = V.normalizeFilter(settings(true, ['  chat  ', 'chat', '', 'user'], [777, ' 777 ', null]));
    eq(f.kinds, ['chat', 'user'], 'разбор: типы обрезаны, дубли схлопнуты, порядок сохранён');
    eq(f.peers, ['777'], 'разбор: числовой id стал строкой, дубль схлопнут, null отброшен');
    ok(f.kindSet.has('chat') && f.peerSet.has('777'), 'разбор: Set-ы собраны из очищенных списков');
}
eq(V.normalizeFilter({ vk_filter_enabled: true, vk_visible_kinds: 'chat' }).kinds, [],
    'разбор: не-массив вместо списка типов не роняет');

// ---------- filterActive ----------

eq(V.filterActive(null), false, 'действует: null → нет');
eq(V.filterActive({}), false, 'действует: объект без active → нет');
eq(V.filterActive(V.normalizeFilter(settings(true, ['user'], []))), true, 'действует: да');

// ---------- dialogVisible: выключенный фильтр показывает всё ----------

{
    const off = V.normalizeFilter(settings(false, [], []));
    for (const d of ALL) ok(V.dialogVisible(d, off), `выключен: ${d.title} видна`);
    eq(V.visibleDialogs(ALL, off).length, ALL.length, 'выключен: список отдаётся целиком');
    // Включён, но пусто — тот же результат: панель не пустеет.
    const emptyOn = V.normalizeFilter(settings(true, [], []));
    eq(V.visibleDialogs(ALL, emptyOn).map((d) => d.id), ALL.map((d) => d.id),
        'включён с пустым выбором: видно ВСЁ');
}

// ---------- dialogVisible: по типу peer'а ----------

{
    const onlyChats = V.normalizeFilter(settings(true, ['chat'], []));
    eq(V.visibleDialogs(ALL, onlyChats).map((d) => d.id), ['2000000042', '2000000043'],
        'тип chat: остаются только беседы');
    ok(!V.dialogVisible(D.sofia, onlyChats), 'тип chat: личный чат скрыт');
    ok(!V.dialogVisible(D.club, onlyChats), 'тип chat: сообщество скрыто');

    const onlyUsers = V.normalizeFilter(settings(true, ['user'], []));
    eq(V.visibleDialogs(ALL, onlyUsers).map((d) => d.id), ['777', '1024'],
        'тип user: остаются только личные');

    const onlyClubs = V.normalizeFilter(settings(true, ['community'], []));
    eq(V.visibleDialogs(ALL, onlyClubs).map((d) => d.id), ['-70'],
        'тип community: остаются только сообщества (peer_id < 0)');

    const two = V.normalizeFilter(settings(true, ['user', 'community'], []));
    eq(V.visibleDialogs(ALL, two).map((d) => d.id), ['777', '1024', '-70'],
        'два типа: объединение, порядок сервера сохранён');
}

// Тип берётся из peer_id, а НЕ из DialogDto.kind: два источника типа рано или
// поздно разошлись бы, и беседа осталась бы в колонке, перестав звенеть.
{
    const onlyChats = V.normalizeFilter(settings(true, ['chat'], []));
    ok(V.dialogVisible({ id: '2000000099', kind: 'user' }, onlyChats),
        'тип решает id, а не поле kind DTO (id из диапазона бесед)');
    ok(!V.dialogVisible({ id: '55', kind: 'chat' }, onlyChats),
        'тип решает id, а не поле kind DTO (id из диапазона людей)');
}

// Границы диапазонов VK — те же, что в format.js::peerKind.
{
    const chats = V.normalizeFilter(settings(true, ['chat'], []));
    const users = V.normalizeFilter(settings(true, ['user'], []));
    ok(V.dialogVisible({ id: '2000000000' }, chats), 'граница: 2000000000 — беседа');
    ok(V.dialogVisible({ id: '1999999999' }, users), 'граница: 1999999999 — человек');
    ok(!V.dialogVisible({ id: '1999999999' }, chats), 'граница: 1999999999 не беседа');
}

// ---------- dialogVisible: поимённо ----------

{
    const named = V.normalizeFilter(settings(true, [], ['777']));
    ok(V.dialogVisible(D.sofia, named), 'поимённо: отмеченная беседа видна');
    ok(!V.dialogVisible(D.petr, named), 'поимённо: неотмеченная скрыта');

    // Поимённый выбор сильнее типа: беседа видна, даже если её тип не отмечен.
    const mixed = V.normalizeFilter(settings(true, ['chat'], ['777']));
    eq(V.visibleDialogs(ALL, mixed).map((d) => d.id), ['777', '2000000042', '2000000043'],
        'смешанный выбор: тип + отмеченная поимённо беседа чужого типа');
}

// Мусор вместо DTO при действующем фильтре скрыт (показать его нечем).
{
    const on = V.normalizeFilter(settings(true, ['user'], []));
    ok(!V.dialogVisible(null, on), 'действующий фильтр: null скрыт');
    ok(!V.dialogVisible({}, on), 'действующий фильтр: DTO без id скрыт');
    ok(V.dialogVisible(null, V.normalizeFilter(settings(false, [], []))),
        'выключенный фильтр: даже null «виден» — поведение до фичи');
}

// ---------- dialogIdVisible: ТОЧНОЕ решение по неизвестной беседе ----------
//
// Отличие от Telegram, ради которого фаза и затевалась: там принадлежность
// чата папке из одного id не выводится, и неизвестный чат приходилось скрывать
// консервативно. У VK тип известен всегда — из самого peer_id.

{
    const onlyChats = V.normalizeFilter(settings(true, ['chat'], []));
    // Беседы 2000000777 нет ни в одном загруженном списке — и всё равно она
    // ТОЧНО проходит фильтр по типу.
    ok(V.dialogIdVisible('2000000777', onlyChats, ALL),
        'неизвестная беседа + фильтр по типу chat → ВИДНА (точное решение)');
    ok(V.dialogIdVisible('2000000777', onlyChats, []),
        'неизвестная беседа: пустой загруженный список ничего не меняет');
    ok(!V.dialogIdVisible('999999', onlyChats, ALL),
        'неизвестный ЛИЧНЫЙ чат при фильтре по типу chat → скрыт (тоже точно)');

    const onlyUsers = V.normalizeFilter(settings(true, ['user'], []));
    ok(V.dialogIdVisible('999999', onlyUsers, []),
        'неизвестный личный чат при фильтре по типу user → виден');
    ok(!V.dialogIdVisible('-999', onlyUsers, []),
        'неизвестное сообщество при фильтре по типу user → скрыто');

    // Строгость сохраняется там, где информации действительно нет: поимённый
    // режим про незнакомый id сказать ничего не может.
    const named = V.normalizeFilter(settings(true, [], ['777']));
    ok(V.dialogIdVisible('777', named, []), 'поимённо: отмеченный id виден без списка');
    ok(!V.dialogIdVisible('2000000777', named, ALL),
        'поимённый режим: неотмеченная беседа скрыта (строгость сохранена)');
    ok(!V.dialogIdVisible('', named, ALL), 'кадр без адреса скрыт');
    ok(!V.dialogIdVisible(null, named, ALL), 'кадр с null-адресом скрыт');

    // Выключенный фильтр — поведение до фичи.
    const off = V.normalizeFilter(settings(false, [], []));
    ok(V.dialogIdVisible('2000000777', off, []), 'выключен: любая беседа видна');
    ok(V.dialogIdVisible('', off, []), 'выключен: даже пустой id не скрывается');

    // Загруженное DTO не спорит с id: решение совпадает в обе стороны.
    ok(V.dialogIdVisible('777', V.normalizeFilter(settings(true, ['user'], [])), ALL),
        'загруженная беседа: решение по типу id совпадает с решением по DTO');
    eq(V.dialogIdVisible('2000000042', onlyChats, ALL),
        V.dialogVisible(D.devs, onlyChats),
        'dialogIdVisible и dialogVisible дают ОДИН ответ для загруженной беседы');
}

// ---------- filterSummary ----------

eq(V.filterSummary(V.normalizeFilter(settings(false, ['chat'], ['777']))),
    'Показаны все беседы.', 'сводка: выключенный фильтр');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], []))),
    'Ничего не выбрано — пока показаны все беседы.',
    'сводка: включён с пустым выбором прямо говорит, что показано всё');
eq(V.filterSummary(V.normalizeFilter(settings(true, ['chat'], []))),
    'Показаны только: 1 тип.', 'сводка: один тип');
eq(V.filterSummary(V.normalizeFilter(settings(true, ['chat', 'user'], []))),
    'Показаны только: 2 типа.', 'сводка: два типа');
eq(V.filterSummary(V.normalizeFilter(settings(true, ['chat', 'user', 'community'], []))),
    'Показаны только: 3 типа.', 'сводка: три типа');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], ['1']))),
    'Показаны только: 1 беседа.', 'сводка: одна беседа');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], ['1', '2']))),
    'Показаны только: 2 беседы.', 'сводка: две беседы');
eq(V.filterSummary(V.normalizeFilter(settings(true, [], ['1', '2', '3', '4', '5']))),
    'Показаны только: 5 бесед.', 'сводка: пять бесед');
eq(V.filterSummary(V.normalizeFilter(settings(true, [],
    Array.from({ length: 11 }, (_, i) => String(i))))),
'Показаны только: 11 бесед.', 'сводка: 11 — исключение из правила «оканчивается на 1»');
eq(V.filterSummary(V.normalizeFilter(settings(true, [],
    Array.from({ length: 21 }, (_, i) => String(i))))),
'Показаны только: 21 беседа.', 'сводка: 21 — форма «беседа»');
eq(V.filterSummary(V.normalizeFilter(settings(true, ['chat'], ['1', '2']))),
    'Показаны только: 1 тип и 2 беседы.', 'сводка: типы и беседы вместе');
eq(V.filterSummary(null), 'Показаны все беседы.', 'сводка: null не роняет');

// ---------- VISIBLE_KINDS / kindLabel ----------

eq(V.VISIBLE_KINDS.map((k) => k.key), ['user', 'chat', 'community'],
    'типы: ровно три ключа в порядке показа формы');
ok(V.VISIBLE_KINDS.every((k) => typeof k.label === 'string' && k.label.length > 0),
    'типы: у каждого есть человеческое название');
eq(V.kindLabel('chat'), 'Беседы', 'название типа chat');
eq(V.kindLabel('нет такого'), 'нет такого', 'неизвестный тип отдаётся как есть');

// ---------- pruneMissingPeers ----------

{
    const kept = V.pruneMissingPeers(['777', '999999', '2000000042'], ALL);
    eq(kept, ['777', '2000000042'], 'чистка: несуществующий id убран, порядок сохранён');
    eq(V.pruneMissingPeers(['777'], []), [], 'чистка: пустой список бесед убирает всё');
    eq(V.pruneMissingPeers(null, ALL), [], 'чистка: null не роняет');
    eq(V.pruneMissingPeers(['777', '777'], ALL), ['777'], 'чистка: дубль схлопывается');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/vk_visibility)`);
if (failures > 0) process.exit(1);
