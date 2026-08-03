// Регресс-тест чистой логики панели VK (js/vk/format.js).
//
// Модуль не трогает DOM и не импортирует core/*, поэтому грузится в Node как
// есть — стенд не нужен.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. format.js — единственное место фронта VK, где
// живут решения, которые нельзя увидеть глазами на экране: границы диапазонов
// peer_id, ключ медиа-кэша, троттлинги и санитизация имени файла. Ошибка в
// любом из них выглядит не как поломка, а как «странное поведение».
//
// Особый состав проверок:
//
//   1. mediaKey РАЗЛИЧАЕТ ИНДЕКСЫ — у сообщения VK до десяти вложений, и без
//      индекса второе фото затирало бы первое в кэше и в учёте objectURL;
//   2. dialogStatusLine НЕ СОДЕРЖИТ счётчика сообщений — у VK message.id
//      сквозной по аккаунту, и телеграмное «N msgs» здесь просто ложь;
//   3. peerKind на границах 1999999999 / 2000000000 / -1 — на них держится и
//      секция сообществ, и белый список Ф10;
//   4. sanitizeDownloadName против bidi-override и «../» — имя файла приходит
//      от чужого аккаунта.
//
// Запуск: node tmux-web/tests/frontend/vk_format.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const F = await import(new URL('../../static/js/vk/format.js', import.meta.url).href);

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
// Диапазоны peer_id.
// ---------------------------------------------------------------------------

eq(F.peerKind(1), 'user', 'peerKind: положительный — пользователь');
eq(F.peerKind(1999999999), 'user', 'peerKind: 1999999999 — ещё пользователь');
eq(F.peerKind(2000000000), 'chat', 'peerKind: 2000000000 — уже беседа');
eq(F.peerKind(2000000007), 'chat', 'peerKind: беседа');
eq(F.peerKind(-1), 'community', 'peerKind: отрицательный — сообщество');
eq(F.peerKind(-55), 'community', 'peerKind: сообщество');
eq(F.peerKind('2000000007'), 'chat', 'peerKind: строковый id');
eq(F.peerKind('мусор'), 'user', 'peerKind: мусор не роняет разбор');
eq(F.CHAT_PEER_BASE, 2000000000, 'граница бесед совпадает с бэкендом');

// Лексика DTO (`group`) переводится в лексику UI (`community`).
eq(F.dialogKind({ id: '-55', kind: 'group' }), 'community', 'dialogKind: group → community');
eq(F.dialogKind({ id: '1234', kind: 'user' }), 'user', 'dialogKind: user');
eq(F.dialogKind({ id: '2000000007', kind: 'chat' }), 'chat', 'dialogKind: chat');
eq(F.dialogKind({ id: '-9' }), 'community', 'dialogKind: без поля — по id');

// ---------------------------------------------------------------------------
// Имя и @ник в строке беседы, статус-строка.
//
// ЗДЕСЬ БЫЛ slugName. Строка списка показывала терминальный слаг
// (`durov@node`), и у VK это било сильнее, чем у Telegram: русские названия
// бесед — норма, и «Тестовое сообщество» превращалось в `anon@club`. Теперь
// имя показывается как есть, а ник (screen_name) идёт приписью.
// ---------------------------------------------------------------------------

eq(F.displayName({ id: '1234', kind: 'user', title: 'Павел Дуров', username: 'durov' }),
    'Павел Дуров', 'имя человека: кириллица как есть');
eq(F.displayNick({ id: '1234', kind: 'user', title: 'Павел Дуров', username: 'durov' }),
    '@durov', 'ник человека: screen_name с ведущим @');
eq(F.displayName({ id: '-55', kind: 'group', title: 'Тестовое сообщество' }),
    'Тестовое сообщество', 'имя сообщества: прежний слаг давал anon@club');
eq(F.displayNick({ id: '-55', kind: 'group', title: 'Тестовое сообщество' }), '',
    'ник сообщества: screen_name нет — приписки нет');
eq(F.displayNick({ id: '-55', kind: 'group', title: 'Клуб', username: 'test_club' }),
    '@test_club', 'ник клуба');
eq(F.displayName({ id: '2000000007', kind: 'chat', title: 'Project X' }), 'Project X',
    'имя беседы');
eq(F.displayNick({ id: '2000000007', kind: 'chat', title: 'Project X' }), '',
    'у беседы screen_name не бывает вовсе');
eq(F.displayName({ id: '1', kind: 'user', title: '   ', username: 'durov' }), '@durov',
    'title из одних пробелов считается пустым — его место занимает ник');
eq(F.displayNick({ id: '1', kind: 'user', username: 'durov' }), '',
    'ник не дублируется, когда он уже стал именем');
eq(F.displayName({ id: '1', kind: 'user' }), '1', 'ни title, ни ника — остаётся peer_id');
eq(F.displayName(null), 'anon', 'null-диалог не роняет рендер колонки');
eq(F.displayNick(null), '', 'null-диалог не роняет рендер колонки');
eq(F.displayName({ id: '1', title: '  Беседа   тестовая \n ' }), 'Беседа тестовая',
    'пробелы схлопываются — шапка сессии однострочная');
eq(F.displayNick({ id: '1', title: 'X', username: '@durov' }), '@durov',
    'ведущий @ из чужого источника не удваивается');
eq(F.displayName({ id: '1', title: 'a'.repeat(80) }).length, 80,
    'длину режет CSS (ellipsis), а не формат');
eq(F.displayName({ id: '1', title: '\u{1F1F7}\u{1F1FA} Соня' }), '\u{1F1F7}\u{1F1FA} Соня',
    'составная графема (флаг) не рвётся');
eq(F.displayName({ id: '1', title: '<img onerror=alert(1)>' }), '<img onerror=alert(1)>',
    'строка не «санируется» — за безопасность отвечает textContent');

// Шапка сессии: имя, ник отдельным куском, хвост со своим разделителем.
const headed = { id: '1234', kind: 'user', title: 'Павел Дуров', username: 'durov' };
eq(F.sessionHeadText(headed, 42),
    `SESSION ▸ Павел Дуров @durov · ⌁ 42 PACKETS · ◇ KEY 0x${F.keyHex('1234')}`,
    'шапка сессии: имя, ник, хвост');
eq(F.sessionHeadText({ id: '-55', kind: 'group', title: 'Клуб' }, 0),
    `SESSION ▸ Клуб · ⌁ 0 PACKETS · ◇ KEY 0x${F.keyHex('-55')}`,
    'шапка сессии: без ника — и без лишнего пробела перед разделителем');
const hp = F.sessionHeadParts(headed, 42);
eq(hp.name, 'SESSION ▸ Павел Дуров', 'части шапки: имя');
eq(hp.nick, '@durov', 'части шапки: ник отдельным куском — его и красят');
ok(hp.tail.startsWith(' · '), 'части шапки: хвост несёт свой разделитель');
eq(`${hp.name} ${hp.nick}${hp.tail}`, F.sessionHeadText(headed, 42),
    'части шапки: склейка равна sessionHeadText');
eq(F.sessionHeadParts(null, 0), { name: 'SESSION ▸ —', nick: '', tail: '' },
    'части шапки: беседа не открыта');

eq(F.dialogStatusLine({ id: '2000000007', kind: 'chat', members: 4 }), '[CHAT] 4 members',
    'беседа: число участников');
eq(F.dialogStatusLine({ id: '2000000007', kind: 'chat' }), '[CHAT]',
    'беседа без участников — просто тег');
eq(F.dialogStatusLine({ id: '-55', kind: 'group' }), '[CLUB]', 'сообщество');
eq(F.dialogStatusLine({ id: '1', kind: 'user' }, { online: true }), '[ONLINE]', 'человек в сети');
eq(F.dialogStatusLine({ id: '1', kind: 'user' }), '[OFFLINE]', 'присутствие неизвестно');
eq(F.dialogStatusLine({ id: '1', kind: 'user' }, { online: false, wasOnline: 1000 }, 1000 + 3 * 3600),
    '[IDLE] last 3h', 'человек был в сети 3 часа назад');

// МУТАЦИОННЫЙ ЯКОРЬ: телеграмный счётчик «N msgs» по id последнего сообщения
// не имеет права вернуться — у VK message.id сквозной по аккаунту.
for (const d of [
    { id: '2000000007', kind: 'chat', members: 4, last_message: { id: 74212 } },
    { id: '-55', kind: 'group', last_message: { id: 98765 } },
]) {
    const line = F.dialogStatusLine(d);
    ok(!/msgs/.test(line), `статус-строка без «msgs»: ${line}`);
    ok(!line.includes(String(d.last_message.id)), `id последнего сообщения не в статусе: ${line}`);
}

// ---------------------------------------------------------------------------
// Разделение списка и заголовки.
// ---------------------------------------------------------------------------

const dialogs = [
    { id: '1234', kind: 'user', unread_count: 2 },
    { id: '2000000007', kind: 'chat', unread_count: 1 },
    { id: '-55', kind: 'group', unread_count: 5, muted: true },
];
const split = F.splitDialogs(dialogs);
eq(split.nodes.map((d) => d.id), ['1234', '2000000007'], 'узлы: люди и беседы');
eq(split.communities.map((d) => d.id), ['-55'], 'сообщества: peer_id < 0');
eq(F.splitDialogs(null).nodes, [], 'splitDialogs терпит null');

eq(F.nodesTitle(12, '~/unread'), '// ~/unread — 12 NODES', 'заголовок секции узлов');
eq(F.nodesTitle(0), '// ~/all — 0 NODES', 'заголовок по умолчанию');

eq(F.unreadTotal(dialogs), 8, 'сумма непрочитанного');
eq(F.unreadTotal(dialogs, F.mutedIds(dialogs)), 3, 'заглушённые в сумму не входят');
eq([...F.mutedIds(dialogs)], ['-55'], 'Set заглушённых');
eq(F.unreadBadgeText(0), '', 'бейдж пуст при нуле');
eq(F.unreadBadgeText(7), '7', 'бейдж');
eq(F.unreadBadgeText(1000), '99+', 'бейдж переполнен');
eq(F.mutedMark(true), '[M]', 'метка mute');
eq(F.mutedMark(false), '', 'метки нет');
eq(F.botMark(true), '[BOT]', 'метка сообщества');

// ---------------------------------------------------------------------------
// Секция // FILTERS: четыре СЕРВЕРНЫХ фильтра.
// ---------------------------------------------------------------------------

const dirs = F.dirEntries();
eq(dirs.map((e) => e.key), ['all', 'unread', 'important', 'unanswered'], 'фильтры по умолчанию');
eq(dirs.map((e) => e.name), ['~/all', '~/unread', '~/important', '~/unanswered'], 'имена пунктов');
eq(F.dirEntries([{ key: 'unread', title: 'Непрочитанные' }]).length, 1, 'список приходит с сервера');
eq(F.dirEntries([{ title: 'без ключа' }]).map((e) => e.key), ['all', 'unread', 'important', 'unanswered'],
    'мусор с сервера откатывается на встроенный список');
eq(F.dirShowsCount('all'), true, 'счётчик у ~/all');
eq(F.dirShowsCount('unread'), true, 'счётчик у ~/unread');
eq(F.dirShowsCount('important'), false, 'у транзиентного фильтра счётчика нет');
eq(F.dirShowsCount('unanswered'), false, 'у транзиентного фильтра счётчика нет');

// Папочной логики Telegram в модуле быть НЕ должно: серверный фильтр нельзя
// посчитать на фронте, и оставленная «на всякий случай» функция врала бы.
for (const gone of [
    'folderSlug', 'folderSlugs', 'dialogCategory', 'dialogInFolder',
    'dialogMatchesDir', 'dialogsForDir', 'dirUnread', 'mergeArchive', 'FOLDER_LIST_LIMIT',
]) {
    eq(F[gone], undefined, `папочная логика убрана: ${gone}`);
}
// Тема живёт в общем слое messenger/theme-core.js — второй копии быть не должно.
for (const gone of ['parseCssColor', 'relativeLuminance', 'resolveTgTheme', 'normalizeTgThemeMode']) {
    eq(F[gone], undefined, `тема не дублируется: ${gone}`);
}

eq(F.mergeDialogs([{ id: '1' }, { id: '2' }], [{ id: '2', unread_count: 3 }]).map((d) => d.id),
    ['2', '1'], 'свежая страница идёт первой, старое дописывается в хвост');

// ---------------------------------------------------------------------------
// Время, даты, лента.
// ---------------------------------------------------------------------------

const t = new Date(2026, 6, 29, 12, 3, 41).getTime() / 1000;
eq(F.formatTime(t), '12:03:41', 'время в локальной зоне');
eq(F.dayKey(t), '2026-07-29', 'ключ дня');
eq(F.dateSepLabel('2026-07-29'), '── 2026-07-29 ──', 'подпись разделителя');
eq(F.needsDateSep(null, { date: t }), true, 'разделитель перед первым сообщением');
eq(F.needsDateSep({ date: t }, { date: t + 60 }), false, 'в один день разделителя нет');
eq(F.needsDateSep({ date: t }, { date: t + 86400 }), true, 'на границе суток — есть');

const list = [{ id: 1 }, { id: 3 }];
eq(F.insertMessage(list, { id: 2 }).inserted, true, 'вставка в середину');
eq(list.map((m) => m.id), [1, 2, 3], 'порядок по возрастанию id');
eq(F.insertMessage(list, { id: 2 }).inserted, false, 'дубликат не вставляется');
eq(F.insertMessage(list, { id: null }).inserted, false, 'сообщение без id не вставляется');
eq(F.prependMessages([{ id: 0 }, { id: 2 }], list).map((m) => m.id), [0, 1, 2, 3],
    'пересечения страниц отбрасываются');
eq(F.oldestId(list), 1, 'курсор подгрузки — минимальный message.id');
eq(F.oldestId([]), null, 'пустая лента — курсора нет');

// Пагинация идёт по message.id, но cmid тоже доступен — он адресует ответы.
eq(F.cmidOf({ cmid: 512 }), 512, 'cmid сообщения');
eq(F.cmidOf({ cmid: 0 }), null, 'нулевой cmid — это «нет»');
eq(F.cmidOf({}), null, 'без cmid');
eq(F.oldestCmid([{ id: 5, cmid: 9 }, { id: 6, cmid: 3 }]), 3, 'минимальный cmid');

eq(F.isAtBottom({ scrollTop: 900, scrollHeight: 1000, clientHeight: 100 }), true, 'лента внизу');
eq(F.isAtBottom({ scrollTop: 0, scrollHeight: 1000, clientHeight: 100 }), false, 'лента вверху');
eq(F.shouldLoadOlder({ scrollTop: 10 }, { hasMore: true, loading: false }), true, 'пора грузить');
eq(F.shouldLoadOlder({ scrollTop: 10 }, { hasMore: true, loading: true }), false, 'уже грузим');
eq(F.shouldLoadOlder({ scrollTop: 10 }, { hasMore: false, loading: false }), false, 'грузить нечего');

eq(F.nickOf({ out: true }), 'you@local', 'своё сообщение');
eq(F.nickOf({ sender: { screen_name: 'durov', first_name: 'Павел' } }), 'durov', 'screen_name важнее имени');
eq(F.nickOf({ sender: { first_name: 'Павел' } }), 'Павел', 'имя');
eq(F.nickOf({}, { title: 'Клуб' }), 'Клуб', 'от имени сообщества');
eq(F.nickOf({}), 'unknown', 'ничего не известно');

eq(F.truncate('  привет   мир  ', 80), 'привет мир', 'схлопывание пробелов');
eq(F.truncate('привет мир', 6), 'приве…', 'обрезка');
ok(Array.from(F.truncate('😀'.repeat(10), 3)).length === 3, 'обрезка не рвёт суррогатные пары');

// ---------------------------------------------------------------------------
// Троттлинги.
// ---------------------------------------------------------------------------

eq(F.toastAllowed(0, 1000), true, 'первый тост показываем');
eq(F.toastAllowed(1000, 2000), false, 'второй тост в окне — нет');
eq(F.toastAllowed(1000, 7000), true, 'после окна — снова да');

eq(F.typingAllowed(0, 1000, true), true, 'первый typing');
eq(F.typingAllowed(1000, 2000, true), false, 'в окне троттлинга');
eq(F.typingAllowed(1000, 1000 + F.TYPING_THROTTLE_MS, true), true, 'на границе окна');
eq(F.typingAllowed(1000, 1001, false), true, 'смена беседы сбрасывает троттлинг');
ok(F.TYPING_THROTTLE_MS < F.TYPING_RESET_MS,
    'окно отправки короче окна сброса — иначе индикатор мигал бы');

eq(F.readAllowed('visible', false), true, 'вкладка видима, панель открыта');
eq(F.readAllowed('hidden', false), false, 'вкладка в фоне — не читаем');
eq(F.readAllowed('visible', true), false, 'панель скрыта — не читаем');

eq(F.typingLine([]), '', 'никто не печатает');
eq(F.typingLine(['a']), '>> a is typing…', 'один');
eq(F.typingLine(['a', 'b']), '>> a, b are typing…', 'двое');
eq(F.typingLine(['a', 'b', 'c', 'd']), '>> a, b, c +1 are typing…', 'больше трёх сворачивается');

eq(F.latencyFromPingDrift(0, 1000, 15000), null, 'первый ping эталона не имеет');
eq(F.latencyFromPingDrift(1000, 1000 + 15080, 15000), 80, 'дрейф интервала = задержка');
eq(F.latencyFromPingDrift(1000, 1000 + 14000, 15000), 0, 'отрицательный дрейф → 0');
eq(F.latencyFromPingDrift(1000, 1000 + 60000, 15000), null, 'спящая вкладка не считается');
eq(F.smoothedLatency([10, 20, 30]), 20, 'среднее по окну');
eq(F.smoothedLatency([]), null, 'замеров нет');

// ---------------------------------------------------------------------------
// Медиа: КЛЮЧ С ИНДЕКСОМ.
// ---------------------------------------------------------------------------

eq(F.mediaKey('1234', 7001, 0, 'thumb'), '1234/7001/0/thumb', 'ключ превью');
eq(F.mediaKey('1234', 7001, 0, 'full'), '1234/7001/0/full', 'ключ полного файла');
eq(F.mediaKey('-55', 42, 3, 'thumb'), '-55/42/3/thumb', 'сообщество и третье вложение');

// ГЛАВНАЯ ПРОВЕРКА ФАЗЫ: индекс обязан различать вложения одного сообщения.
// Убери index из mediaKey — этот блок покраснеет первым.
const keys = new Set();
for (let i = 0; i < 10; i += 1) keys.add(F.mediaKey('1234', 7001, i, 'thumb'));
eq(keys.size, 10, 'десять вложений одного сообщения дают ДЕСЯТЬ разных ключей');
ok(F.mediaKey('1234', 7001, 0, 'thumb') !== F.mediaKey('1234', 7001, 1, 'thumb'),
    'два фото одного сообщения не делят ключ кэша');
eq(F.mediaKey('1234', 7001, 'мусор', 'thumb'), '1234/7001/0/thumb', 'нечисловой индекс → 0');
eq(F.mediaKey('1234', 7001, 2, 'что-то'), '1234/7001/2/thumb', 'неизвестный вид → thumb');

eq(F.mediaFileName({ id: 7001 }, { index: 1, kind: 'photo' }), 'photo_7001_1.jpg',
    'имя генерируется с индексом');
eq(F.mediaFileName({ id: 7001 }, { index: 0, kind: 'document', file_name: 'смета.pdf' }), 'смета.pdf',
    'имя из VK важнее сгенерированного');
ok(F.mediaFileName({ id: 7 }, { index: 0, kind: 'photo' })
    !== F.mediaFileName({ id: 7 }, { index: 1, kind: 'photo' }),
    'два фото одного сообщения сохраняются под РАЗНЫМИ именами');
eq(F.mediaFileName({ id: 7 }, { index: 0, kind: 'voice' }), 'voice_7_0.ogg', 'голосовое — ogg');
eq(F.mediaFileName({ id: 7 }, { index: 0, kind: 'graffiti' }), 'graffiti_7_0.png', 'граффити — png');

eq(F.formatBytes(null), '', 'размер не пришёл');
eq(F.formatBytes(0), '0 B', 'нулевой размер — честный ноль');
eq(F.formatBytes(512), '512 B', 'байты');
eq(F.formatBytes(840000), '840 KB', 'килобайты');
eq(F.formatBytes(1200000), '1.2 MB', 'мегабайты с десятыми');
eq(F.formatBytes(999950), '1.0 MB', 'перекат через тысячу');
eq(F.durationLabel(null), '', 'длительности нет');
eq(F.durationLabel(7), '0:07', 'секунды');
eq(F.durationLabel(3671), '1:01:11', 'часы');
eq(F.mediaMetaLine({ width: 1280, height: 720, size: 1200000 }), '1280×720 · 1.2 MB', 'метаданные');
eq(F.progressPercent(50, 100), 50, 'половина');
eq(F.progressPercent(100, 100), 100, 'готово');
eq(F.progressPercent(99.9, 100), 99, 'до финального события — не больше 99');
eq(F.progressPercent(5, null), null, 'размер неизвестен');
eq(F.progressLabel(5, null), '5 B…', 'подпись без процента');

eq(F.mediaKindLabel('graffiti'), 'GRAFFITI', 'подпись граффити');
eq(F.mediaKindLabel('unsupported'), 'UNKNOWN', 'неизвестный вид подписан, а не скрыт');
eq(F.mediaGlyph('voice'), '∿', 'глиф голосового');
eq(F.mediaGlyph('unsupported'), '?', 'глиф неизвестного');
// canShowThumb ВЕРИТ СЕРВЕРУ и не держит собственного списка видов.
//
// Дефект, ради которого это переписано: здесь стоял белый список
// ['photo','video','document','sticker','graffiti'], в который не попал вид
// `other`. Из-за него карточка ссылки с превью 1200×630 не делала запрос
// вовсе и показывала значок-заглушку — «превью ссылок не работают».
//
// Бэкенд считает has_thumb как `url_for(att, Thumb).is_some()`, то есть тем же
// кодом, который потом ищет адрес (plugins/vk/src/attach.rs). Второй список
// здесь означал бы второй источник истины — ровно то, что и разъехалось.
eq(F.canShowThumb({ has_thumb: true, kind: 'photo' }), true, 'превью фото');
eq(F.canShowThumb({ has_thumb: false, kind: 'photo' }), false, 'сервер сказал «превью нет»');
eq(F.canShowThumb({ has_thumb: true, kind: 'other' }), true,
    'РЕГРЕСС: у ссылки превью бывает, и запрос обязан уйти');
eq(F.canShowThumb({ has_thumb: false, kind: 'other' }), false,
    'опрос/товар: сервер сказал «нет» — не просим');
eq(F.canShowThumb({ has_thumb: true, kind: 'что-то-новое' }), true,
    'вид, которого фронт ещё не знает, тоже получает превью — решает сервер');
eq(F.canShowThumb(null), false, 'без медиа');
eq(F.canShowThumb({}), false, 'флага нет — не просим');
eq(F.canShowThumb({ has_thumb: 'yes', kind: 'photo' }), false, 'строка — не true');
eq(F.isImageKind('graffiti'), true, 'граффити открывается в лайтбоксе');
eq(F.isPlayableKind('voice'), true, 'голосовое воспроизводится');
eq(F.isPlayableKind('video'), false,
    'видео у VK не воспроизводится в панели — файла во вложении нет, только страница-плеер');

// ---------------------------------------------------------------------------
// Санитизация имени файла (данные из чужого аккаунта).
// ---------------------------------------------------------------------------

eq(F.sanitizeDownloadName('../../.bashrc'), 'bashrc', 'traversal и ведущая точка убраны');
eq(F.sanitizeDownloadName('C:\\Windows\\system32\\evil.dll'), 'evil.dll', 'windows-путь');
eq(F.sanitizeDownloadName('invoice\u202Efdp.exe'), 'invoicefdp.exe', 'bidi-override вырезан');
eq(F.sanitizeDownloadName(''), 'download.bin', 'пустое имя — запасное');
eq(F.sanitizeDownloadName('a<b>c:d"e|f?g*h'), 'a_b_c_d_e_f_g_h', 'спецсимволы ФС');
const long = F.sanitizeDownloadName('я'.repeat(300) + '.pdf');
ok(new TextEncoder().encode(long).length <= 255, 'имя влезает в 255 байт ФС');
ok(long.endsWith('.pdf'), 'расширение сохраняется при обрезке');

// ---------------------------------------------------------------------------
// Загрузка файлов: лимиты ПО ВИДАМ.
// ---------------------------------------------------------------------------

eq(F.uploadMime('a.pdf', ''), 'application/pdf', 'MIME по расширению');
eq(F.uploadMime('a.pdf', 'application/pdf'), 'application/pdf', 'MIME от браузера');
eq(F.uploadMime('noext', ''), 'application/octet-stream', 'без расширения');
eq(F.uploadKind('image/jpeg'), 'photo', 'фото');
eq(F.uploadKind('image/gif'), 'doc', 'gif уходит документом — иначе VK потеряет анимацию');
eq(F.uploadKind('video/mp4'), 'video', 'видео');
eq(F.uploadKind('application/pdf'), 'doc', 'документ');

eq(F.uploadRejectReason({ size: 0, name: 'a.jpg' }), 'файл пустой', 'пустой файл');
eq(F.uploadRejectReason({ size: 1024, name: 'a.jpg', type: 'image/jpeg' }), null, 'фото проходит');
ok(F.uploadRejectReason({ size: F.UPLOAD_LIMITS.photo + 1, name: 'a.jpg', type: 'image/jpeg' }),
    'фото больше лимита фото — отказ');
eq(F.uploadRejectReason({ size: F.UPLOAD_LIMITS.photo + 1, name: 'a.pdf', type: 'application/pdf' }),
    null, 'тот же размер документом проходит — лимиты РАЗНЫЕ по видам');
ok(F.UPLOAD_LIMITS.photo < F.UPLOAD_LIMITS.doc, 'у фото лимит жёстче, чем у документа');

eq(F.uploadStageLabel('queued'), 'QUEUED', 'этап очереди');
eq(F.uploadStageLabel('local', 50, 100), 'LOCAL 50%', 'этап до devforge');
eq(F.uploadStageLabel('vk', 50, 100), 'VK 50%', 'этап до VK');
eq(F.uploadQueueLabel(2, 3), 'QUEUED 2/3', 'позиция в очереди');
ok(F.pastedFileName('image/png', Date.UTC(2026, 0, 2, 3, 4, 5)).endsWith('.png'),
    'имя вставленной картинки');

eq(F.parseApiError(409, '{"error":{"code":"not_authorized","message":"nope"}}').code,
    'not_authorized', 'код ошибки из конверта');
eq(F.parseApiError(502, '<html>').message, 'HTTP 502', 'не-JSON тело');
eq(F.parseApiError(429, '{"error":{"code":"flood_wait","message":"w","retry_after_secs":60}}')
    .retryAfterSecs, 60, 'обратный отсчёт');

// ---------------------------------------------------------------------------
// Captcha и коды VK.
// ---------------------------------------------------------------------------

const capt = F.captchaFromError({ code: 'captcha_required', captcha: { sid: '239123' } });
eq(capt.sid, '239123', 'sid из ошибки');
eq(capt.imgUrl, '/api/vk/captcha/239123', 'картинка идёт ЧЕРЕЗ ПРОКСИ бэкенда');
ok(!capt.imgUrl.includes('vk.com') && !capt.imgUrl.includes('api.vk'),
    'прямой адрес VK в src не попадает');
eq(F.captchaFromError({ code: 'invalid_input' }), null, 'другая ошибка — не captcha');
eq(F.captchaFromError(null), null, 'нет ошибки — нет captcha');
eq(F.captchaFromError({
    code: 'captcha_required',
    body: { error: { captcha_sid: '77' } },
}).sid, '77', 'sid читается и из тела ответа');

eq(F.captchaRetryBody('239', 'abc'), { captchaSid: '239', captchaKey: 'abc' }, 'тело повтора');
eq(F.captchaRetryBody('239', '  '), null, 'половина ответа бесполезна');
eq(F.captchaRetryBody('', 'abc'), null, 'без sid повторять нечего');

// ---------------------------------------------------------------------------
// safeUrl — белый список схем (единственный внешний переход панели)
//
// Функция живёт здесь, а пользуются ею ДВОЕ: media.js (страница плеера видео)
// и botkb.js (кнопка open_link сообщества). Оба адреса приходят от чужой
// стороны, поэтому проверка схемы — не гигиена, а граница XSS: javascript:
// в window.open исполняется с правами вкладки.
// ---------------------------------------------------------------------------

const NULBYTE = String.fromCharCode(0);
const RTLO = String.fromCharCode(0x202E);

eq(F.safeUrl('https://vk.com/video-1_2'), 'https://vk.com/video-1_2', 'https проходит');
eq(F.safeUrl('http://example.com'), 'http://example.com', 'http проходит');
eq(F.safeUrl('vk://vk.com/im'), 'vk://vk.com/im', 'vk-deeplink проходит');
eq(F.safeUrl('javascript:alert(1)'), null, 'javascript: отвергается');
eq(F.safeUrl('JaVaScRiPt:alert(1)'), null, 'регистр схемы не спасает');
eq(F.safeUrl('  javascript:alert(1)  '), null, 'пробелы вокруг не спасают');
eq(F.safeUrl('java' + NULBYTE + 'script:alert(1)'), null, 'NUL внутри схемы не спасает');
eq(F.safeUrl('jav\tascript:alert(1)'), null, 'таб внутри схемы не спасает');
eq(F.safeUrl(RTLO + 'javascript:alert(1)'), null, 'bidi-override перед схемой');
eq(F.safeUrl('data:text/html,<script>x</script>'), null, 'data: запрещён');
eq(F.safeUrl('blob:https://evil/x'), null, 'blob: запрещён');
eq(F.safeUrl('file:///etc/passwd'), null, 'file: запрещён');
eq(F.safeUrl('vbscript:msgbox'), null, 'vbscript: запрещён');
eq(F.safeUrl('//evil.com/x'), null, 'protocol-relative без схемы отвергается');
eq(F.safeUrl('/relative/path'), null, 'относительный путь отвергается');
eq(F.safeUrl('https:evil'), null, 'https без authority отвергается');
eq(F.safeUrl(''), null, 'пустая строка');
eq(F.safeUrl(null), null, 'null');
eq(F.safeUrl(42), null, 'не строка');

// ---------------------------------------------------------------------------
// webUrl / linkDomain — карточка вложения `link`
//
// Адрес сюда приходит НЕ от нас, а от собеседника: он его набрал руками в
// сообщении. Поэтому белый список тут уже, чем у safeUrl — только http/https:
// `vk://` из чужого сообщения не «удобный переход в приложение», а вопрос
// «что именно откроет установленный клиент».
// ---------------------------------------------------------------------------

eq(F.webUrl('https://psph.bitrix24.ru/crm/contact/details/27307/'),
    'https://psph.bitrix24.ru/crm/contact/details/27307/', 'https проходит');
eq(F.webUrl('http://example.com/x'), 'http://example.com/x', 'http проходит');
eq(F.webUrl('vk://vk.com/im'), null, 'vk-deeplink НЕ проходит: схему выбрал собеседник');
eq(F.webUrl('javascript:alert(1)'), null, 'javascript: отвергается');
eq(F.webUrl('java' + NULBYTE + 'script:alert(1)'), null, 'NUL внутри схемы не спасает');
eq(F.webUrl(RTLO + 'javascript:alert(1)'), null, 'bidi-override перед схемой');
eq(F.webUrl('data:text/html,<script>x</script>'), null, 'data: запрещён');
eq(F.webUrl('blob:https://evil/x'), null, 'blob: запрещён');
eq(F.webUrl('file:///etc/passwd'), null, 'file: запрещён');
eq(F.webUrl('vbscript:msgbox'), null, 'vbscript: запрещён');
eq(F.webUrl('//evil.com/x'), null, 'protocol-relative отвергается');
eq(F.webUrl('https:evil'), null, 'https без authority отвергается');
eq(F.webUrl(''), null, 'пустая строка');
eq(F.webUrl(null), null, 'null');

eq(F.linkDomain('https://psph.bitrix24.ru/crm/contact/details/27307/'), 'psph.bitrix24.ru',
    'на карточке — домен, а не адрес на полсотни символов');
eq(F.linkDomain('https://www.example.com/a/b?c=1#d'), 'example.com', 'www. снимается');
eq(F.linkDomain('HTTPS://Example.COM/A'), 'example.com', 'домен в нижнем регистре');
eq(F.linkDomain('https://example.com:8443/x'), 'example.com', 'порт в подпись не идёт');
eq(F.linkDomain('http://xn--80ak6aa92e.com/'), 'xn--80ak6aa92e.com',
    'punycode НЕ разворачивается: омограф не должен притворяться настоящим доменом');
eq(F.linkDomain('javascript:alert(1)'), null, 'у недопустимой схемы домена нет');
eq(F.linkDomain('vk://vk.com/im'), null, 'vk-deeplink домена не даёт');
eq(F.linkDomain(null), null, 'null');

eq(F.vkErrorLabel(5), 'токен недействителен или отозван', 'код 5');
eq(F.vkErrorLabel(14), 'нужно ввести символы с картинки', 'код 14');
eq(F.vkErrorLabel(15), 'у токена нет права messages', 'код 15');
eq(F.vkErrorLabel(6), 'слишком много запросов в секунду', 'код 6');
eq(F.vkErrorLabel(9), 'слишком часто — VK включил flood control', 'код 9');
eq(F.vkErrorLabel(4242), 'VK ошибка 4242', 'незнакомый код не выдумывается');
eq(F.vkErrorLabel('мусор'), '', 'не число — пусто');

// ---------------------------------------------------------------------------

console.log(`vk_format: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
