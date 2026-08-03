// Регресс-тест чистой логики вкладки Telegram (Фаза 3).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/format.js через
// `await import()` — он специально не импортирует ни DOM, ни core/*, поэтому
// работает в Node как есть. Любое изменение контракта (слаги, статус-строки,
// дедупликация сообщений, якоря скролла, троттлинг тостов) валит этот файл.
//
// Запуск: node tmux-web/tests/frontend/telegram_format.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const modUrl = new URL('../../static/js/telegram/format.js', import.meta.url).href;
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

// ---------- displayName / displayNick ----------
//
// ЗДЕСЬ БЫЛ slugName. Строка списка показывала терминальный слаг
// (`sofia@node`), и кириллическое имя в нём гибло целиком: «Дев Чат» →
// `anon@group`. Пользователь попросил обратное — имя основным цветом, ник
// приписью, — поэтому функции две и обе ничего не «нормализуют».
eq(F.displayName({ title: 'Sofia Petrova', username: 'sofia' }), 'Sofia Petrova',
    'имя: показывается как есть');
eq(F.displayNick({ title: 'Sofia Petrova', username: 'sofia' }), '@sofia', 'ник: с ведущим @');
eq(F.displayName({ title: 'Дев Чат', kind: 'group' }), 'Дев Чат',
    'имя: кириллица НЕ транслитерируется (прежний слаг давал anon@group)');
eq(F.displayNick({ title: 'Дев Чат', kind: 'group' }), '',
    'ник: username нет — приписки нет, слаг из имени не выдумывается');
eq(F.displayName({ title: 'News!!! Feed', kind: 'channel' }), 'News!!! Feed',
    'имя: пунктуация сохраняется');
eq(F.displayName({ title: '  Много   пробелов \n тут ' }), 'Много пробелов тут',
    'имя: пробелы схлопываются — шапка сессии однострочная');
eq(F.displayName({ username: 'sofia', id: '777' }), '@sofia',
    'имя: без title его место занимает ник');
eq(F.displayNick({ username: 'sofia', id: '777' }), '',
    'ник: не дублируется, когда он уже стал именем');
eq(F.displayName({ title: '   ', username: 'sofia' }), '@sofia',
    'имя: title из одних пробелов считается пустым');
eq(F.displayName({ id: '777000' }), '777000', 'имя: ни title, ни ника — остаётся id');
eq(F.displayName({}), 'anon', 'имя: пустой DTO не роняет рендер списка');
eq(F.displayNick({ title: 'X', username: '@sofia' }), '@sofia',
    'ник: ведущий @ из чужого источника не удваивается');
eq(F.displayName({ title: 'x'.repeat(80) }).length, 80,
    'имя: длину режет CSS (ellipsis), а не формат — обрезка рвала бы графемы');
// Разметку функция НЕ вычищает, и это осознанно: в DOM данные попадают только
// через textContent (см. telegram_dialog_name.test.mjs), а «санация» показала
// бы пользователю имя, отличное от настоящего.
eq(F.displayName({ title: '<img onerror=alert(1)>' }), '<img onerror=alert(1)>',
    'имя: строка не «санируется» — за безопасность отвечает textContent');

// ---------- relativeAge / dialogStatusLine ----------
eq(F.relativeAge(5), 'now', 'age: секунды → now');
eq(F.relativeAge(600), '10m', 'age: минуты');
eq(F.relativeAge(7200), '2h', 'age: часы');
eq(F.relativeAge(200000), '2d', 'age: дни');

eq(F.dialogStatusLine({ kind: 'user' }, { online: true }), '[ONLINE]', 'status: онлайн');
eq(F.dialogStatusLine({ kind: 'user' }, { online: false, wasOnline: 1000 }, 1000 + 7200),
    '[IDLE] last 2h', 'status: последний визит');
eq(F.dialogStatusLine({ kind: 'user' }, null), '[OFFLINE]', 'status: присутствие неизвестно');
eq(F.dialogStatusLine({ kind: 'group', last_message: { id: 142 } }), '[GROUP] 142 msgs',
    'status: группа со счётчиком');
eq(F.dialogStatusLine({ kind: 'channel' }), '[CHANNEL]', 'status: канал без сообщений');
// В ЛС id сообщений глобальны для аккаунта — счётчик там врал бы.
ok(!F.dialogStatusLine({ kind: 'user', last_message: { id: 999 } }, { online: true }).includes('999'),
    'status: у пользователя счётчик msgs не показывается');

// ---------- splitDialogs / nodesTitle ----------
const dialogs = [
    { id: '1', kind: 'user', unread_count: 2 },
    { id: '-100200', kind: 'channel', unread_count: 5 },
    { id: '-300', kind: 'group', unread_count: 0 },
    null,
];
const split = F.splitDialogs(dialogs);
eq(split.nodes.map((d) => d.id), ['1', '-300'], 'split: люди и группы в nodes');
eq(split.channels.map((d) => d.id), ['-100200'], 'split: каналы отдельно');
eq(F.nodesTitle(split.nodes.length), '// ~/inbox — 2 NODES', 'заголовок секции узлов');
eq(F.nodesTitle(0), '// ~/inbox — 0 NODES', 'заголовок при пустом списке');

// ---------- unreadTotal / unreadBadgeText ----------
eq(F.unreadTotal(dialogs), 7, 'unread: сумма по всем диалогам');
eq(F.unreadTotal(dialogs, ['-100200']), 2, 'unread: заглушённые не считаются');
eq(F.unreadBadgeText(0), '', 'бейдж: ноль скрывается');
eq(F.unreadBadgeText(7), '7', 'бейдж: обычное число');
eq(F.unreadBadgeText(100), '99+', 'бейдж: переполнение');

// ---------- даты и время ----------
const t = Math.floor(new Date(2026, 6, 29, 12, 3, 41).getTime() / 1000);
eq(F.formatTime(t), '12:03:41', 'время в формате HH:MM:SS');
eq(F.dayKey(t), '2026-07-29', 'ключ дня (локальная зона)');
eq(F.dateSepLabel('2026-07-29'), '── 2026-07-29 ──', 'подпись разделителя');
ok(F.needsDateSep(null, { date: t }), 'разделитель перед первым сообщением');
ok(!F.needsDateSep({ date: t }, { date: t + 60 }), 'нет разделителя внутри одного дня');
const nextDay = Math.floor(new Date(2026, 6, 30, 0, 0, 1).getTime() / 1000);
ok(F.needsDateSep({ date: t }, { date: nextDay }), 'разделитель на границе суток');

// ---------- keyHex / sessionHeadText ----------
eq(F.keyHex('-1001234567890').length, 8, 'keyHex: 8 hex-символов');
eq(F.keyHex('-1001234567890'), F.keyHex('-1001234567890'), 'keyHex: детерминирован');
ok(F.keyHex('1') !== F.keyHex('2'), 'keyHex: разные id — разные ключи');
eq(F.sessionHeadText(null, 0), 'SESSION ▸ —', 'шапка без диалога');
const headed = { id: '777000', title: 'Sofia Petrova', username: 'sofia', kind: 'user' };
eq(
    F.sessionHeadText(headed, 42),
    `SESSION ▸ Sofia Petrova @sofia · ⌁ 42 PACKETS · ◇ KEY 0x${F.keyHex('777000')}`,
    'шапка сессии: имя, ник, хвост'
);
eq(
    F.sessionHeadText({ id: '777000', title: 'Дев Чат', kind: 'group' }, 42),
    `SESSION ▸ Дев Чат · ⌁ 42 PACKETS · ◇ KEY 0x${F.keyHex('777000')}`,
    'шапка сессии: без ника — и без лишнего пробела перед разделителем'
);

// Части и склейка обязаны совпадать символ в символ: chat.js рисует шапку
// узлами (ник красится серым), а описание формы шапки остаётся одно.
const hp = F.sessionHeadParts(headed, 42);
eq(hp.name, 'SESSION ▸ Sofia Petrova', 'части шапки: имя');
eq(hp.nick, '@sofia', 'части шапки: ник отдельным куском — его и красят');
ok(hp.tail.startsWith(' · '), 'части шапки: хвост несёт свой разделитель');
eq(`${hp.name} ${hp.nick}${hp.tail}`, F.sessionHeadText(headed, 42),
    'части шапки: склейка равна sessionHeadText');
eq(F.sessionHeadParts({ id: '777000', title: 'Дев Чат', kind: 'group' }, 42).nick, '',
    'части шапки: без username ник пуст');
eq(F.sessionHeadParts(null, 0), { name: 'SESSION ▸ —', nick: '', tail: '' },
    'части шапки: диалог не открыт');

// ---------- nickOf / truncate ----------
eq(F.nickOf({ out: true, sender: { username: 'me' } }), 'you@local', 'ник: своё сообщение');
eq(F.nickOf({ out: false, sender: { username: 'sofia' } }), 'sofia', 'ник: @username собеседника');
eq(F.nickOf({ out: false, sender: { first_name: 'Sofia' } }), 'Sofia', 'ник: имя, если нет username');
eq(F.nickOf({ out: false, sender: null }, { title: 'News' }), 'News', 'ник: пост канала → title');
eq(F.nickOf({ out: false }, null), 'unknown', 'ник: неизвестный отправитель');
eq(F.truncate('  много   пробелов  ', 80), 'много пробелов', 'truncate: схлопывает пробелы');
eq(F.truncate('abcdefghij', 5), 'abcd…', 'truncate: обрезка с многоточием');

// ---------- insertMessage (дедупликация) ----------
let list = [];
eq(F.insertMessage(list, { id: 2 }).inserted, true, 'insert: первое сообщение');
eq(F.insertMessage(list, { id: 3 }).inserted, true, 'insert: следующее в конец');
eq(F.insertMessage(list, { id: 1 }).inserted, true, 'insert: старое в начало');
eq(list.map((m) => m.id), [1, 2, 3], 'insert: порядок по возрастанию сохраняется');
// Главный инвариант: своё же сообщение, пришедшее ВТОРОЙ раз через WS.
const dup = F.insertMessage(list, { id: 2 });
eq(dup.inserted, false, 'insert: дубль по id отвергается');
eq(list.length, 3, 'insert: дубль не увеличивает ленту');
eq(F.insertMessage(list, { id: 'x' }).inserted, false, 'insert: нечисловой id игнорируется');

// ---------- prependMessages / oldestId ----------
const current = [{ id: 10 }, { id: 11 }];
const merged = F.prependMessages([{ id: 8 }, { id: 9 }, { id: 10 }], current);
eq(merged.map((m) => m.id), [8, 9, 10, 11], 'prepend: пересечение страниц отбрасывается');
eq(F.oldestId(merged), 8, 'oldestId: минимальный id — курсор before_id');
eq(F.oldestId([]), null, 'oldestId: пустая лента');

// ---------- скролл ----------
ok(F.isAtBottom({ scrollTop: 960, scrollHeight: 1000, clientHeight: 40 }), 'stick: у нижней границы');
ok(!F.isAtBottom({ scrollTop: 100, scrollHeight: 1000, clientHeight: 40 }), 'stick: пользователь отскроллил');
ok(F.shouldLoadOlder({ scrollTop: 10 }, { hasMore: true, loading: false }), 'подгрузка: у верха при has_more');
ok(!F.shouldLoadOlder({ scrollTop: 10 }, { hasMore: true, loading: true }), 'подгрузка: не дублируется при loading');
ok(!F.shouldLoadOlder({ scrollTop: 10 }, { hasMore: false, loading: false }), 'подгрузка: нечего грузить');
ok(!F.shouldLoadOlder({ scrollTop: 900 }, { hasMore: true, loading: false }), 'подгрузка: далеко от верха');

// ---------- троттлинг тостов ----------
ok(F.toastAllowed(0, 1000), 'тост: первый показывается');
ok(!F.toastAllowed(1000, 3000), 'тост: внутри окна 5с подавляется');
ok(F.toastAllowed(1000, 6001), 'тост: после окна снова можно');

// ---------- индикатор набора (Фаза 5) ----------
// Отправка: не чаще раза в 4с на диалог, но смена диалога сбрасывает окно.
ok(F.typingAllowed(0, 1000, true), 'typing: первый запрос уходит всегда');
ok(!F.typingAllowed(1000, 2000, true), 'typing: внутри окна 4с подавляется');
ok(F.typingAllowed(1000, 1000 + F.TYPING_THROTTLE_MS, true), 'typing: по истечении окна снова можно');
ok(F.typingAllowed(1000, 1100, false), 'typing: смена диалога сбрасывает троттлинг');
eq(F.TYPING_RESET_MS, 6000, 'typing: чужой индикатор гаснет через 6с');

// Приём: единственное/несколько имён, свёртка длинного списка.
eq(F.typingLine([]), '', 'typing: никто не печатает — пустая строка');
eq(F.typingLine(['sofia']), '>> sofia is typing…', 'typing: один — is');
eq(F.typingLine(['a', 'b']), '>> a, b are typing…', 'typing: двое — are');
eq(F.typingLine(['a', 'b', 'c']), '>> a, b, c are typing…', 'typing: три имени показываются целиком');
eq(F.typingLine(['a', 'b', 'c', 'd', 'e']), '>> a, b, c +2 are typing…', 'typing: свыше трёх — +N');
eq(F.typingLine(['a', '', '  ', 'b']), '>> a, b are typing…', 'typing: пустые имена отбрасываются');

// ---------- присутствие в шапке и статусбаре (Фаза 5) ----------
const headDialog = { id: '777000', kind: 'user', username: 'sofia' };
ok(!F.sessionHeadText(headDialog, 3).includes('ONLINE'),
    'head: без известного присутствия статус не выдумывается');
ok(F.sessionHeadText(headDialog, 3, { online: true }).includes('[ONLINE]'),
    'head: онлайн виден в шапке сессии');
ok(F.sessionHeadText({ id: '-100123', kind: 'channel' }, 3, { online: true })
    .indexOf('[ONLINE]') === -1, 'head: у канала присутствия не бывает');
ok(F.sessionHeadText(headDialog, 3, { online: true }).includes('⌁ 3 PACKETS'),
    'head: счётчик сообщений на месте');
eq(F.sessionHeadText(null, 0), 'SESSION ▸ —', 'head: диалог не открыт');

eq(F.onlineCounterText(0, 0), '', 'статусбар: пока статусов нет — пусто, а не «0 ONLINE»');
eq(F.onlineCounterText(0, 5), '0 ONLINE', 'статусбар: статусы известны — показываем ноль честно');
eq(F.onlineCounterText(3, 12), '3 ONLINE', 'статусбар: счётчик в сети');

// ---------- отметка о прочтении (Фаза 5) ----------
ok(F.readAllowed('visible', false), 'read: видимая вкладка с открытой панелью — отмечаем');
ok(!F.readAllowed('hidden', false), 'read: скрытая вкладка браузера НЕ отмечает прочитанным');
ok(!F.readAllowed('visible', true), 'read: панель Telegram закрыта — не отмечаем');
ok(!F.readAllowed('prerender', false), 'read: prerender — тоже не «смотрит» пользователь');
eq(F.READ_DEBOUNCE_MS, 800, 'read: окно коалесценции 800мс');

// ---------- оценка задержки по дрейфу ping'ов ----------
eq(F.latencyFromPingDrift(0, 1000, 15000), null, 'lat: первый ping эталона не имеет');
eq(F.latencyFromPingDrift(1000, 1000 + 15080, 15000), 80, 'lat: дрейф интервала = задержка');
eq(F.latencyFromPingDrift(1000, 1000 + 14900, 15000), 0, 'lat: отрицательный дрейф → 0');
eq(F.latencyFromPingDrift(1000, 1000 + 60000, 15000), null, 'lat: спящая вкладка не считается задержкой');

// ---------- сглаживание LAT (Фаза 6) ----------
eq(F.LAT_WINDOW, 5, 'lat: окно сглаживания — 5 замеров');
eq(F.smoothedLatency([]), null, 'lat: без замеров значения нет');
eq(F.smoothedLatency(null), null, 'lat: не массив → null');
eq(F.smoothedLatency([42]), 42, 'lat: единственный замер показывается как есть');
eq(F.smoothedLatency([10, 20, 30]), 20, 'lat: среднее по окну');
eq(F.smoothedLatency([10, 20, 31]), 20, 'lat: среднее округляется');
eq(F.smoothedLatency([10, NaN, 20]), 15, 'lat: мусорные замеры отбрасываются');
eq(F.smoothedLatency([NaN, undefined]), null, 'lat: одни мусорные замеры → null');
ok(F.smoothedLatency([10, 10, 10, 10, 500]) < 500,
    'lat: одиночный выброс не выносит показание целиком');

// ---------- тема панели (Фаза 6) ----------
eq(F.normalizeTgThemeMode('light'), 'light', 'theme: валидный режим проходит как есть');
eq(F.normalizeTgThemeMode('LIGHT'), 'light', 'theme: регистр не важен');
eq(F.normalizeTgThemeMode('solarized'), 'auto', 'theme: мусор из localStorage → auto');
eq(F.normalizeTgThemeMode(null), 'auto', 'theme: пусто → auto');
eq(F.nextTgThemeMode('auto'), 'dark', 'theme: цикл auto → dark');
eq(F.nextTgThemeMode('dark'), 'light', 'theme: цикл dark → light');
eq(F.nextTgThemeMode('light'), 'auto', 'theme: цикл light → auto');
eq(F.nextTgThemeMode('чушь'), 'dark', 'theme: цикл от мусора стартует как от auto');

eq(F.parseCssColor('#fff'), { r: 255, g: 255, b: 255 }, 'color: короткий hex');
eq(F.parseCssColor('#0A0'), { r: 0, g: 170, b: 0 }, 'color: короткий hex в верхнем регистре');
eq(F.parseCssColor('#0f0f'), { r: 0, g: 255, b: 0 }, 'color: короткий hex с альфой');
eq(F.parseCssColor('#123456'), { r: 18, g: 52, b: 86 }, 'color: полный hex');
eq(F.parseCssColor('#12345680'), { r: 18, g: 52, b: 86 }, 'color: hex с альфой — альфа игнорируется');
eq(F.parseCssColor('  #123456  '), { r: 18, g: 52, b: 86 }, 'color: пробелы вокруг значения');
eq(F.parseCssColor('rgb(1, 2, 3)'), { r: 1, g: 2, b: 3 }, 'color: rgb() через запятую');
eq(F.parseCssColor('rgba(1,2,3,0.5)'), { r: 1, g: 2, b: 3 }, 'color: rgba() — альфа игнорируется');
eq(F.parseCssColor('rgb(10 20 30 / 40%)'), { r: 10, g: 20, b: 30 }, 'color: современная запись со слэшем');
eq(F.parseCssColor('rgb(100%, 0%, 50%)'), { r: 255, g: 0, b: 128 }, 'color: проценты');
eq(F.parseCssColor('rgb(300, -5, 3)'), { r: 255, g: 0, b: 3 }, 'color: выход за диапазон обрезается');
eq(F.parseCssColor('white'), { r: 255, g: 255, b: 255 }, 'color: именованный белый');
eq(F.parseCssColor('black'), { r: 0, g: 0, b: 0 }, 'color: именованный чёрный');
eq(F.parseCssColor('transparent'), null, 'color: transparent не цвет фона');
eq(F.parseCssColor('oklch(0.7 0.1 200)'), null, 'color: неподдержанное пространство → null');
eq(F.parseCssColor('#12345'), null, 'color: пятизначный hex невалиден');
eq(F.parseCssColor('rgb(1, 2)'), null, 'color: неполный rgb → null');
eq(F.parseCssColor('rgb(a, b, c)'), null, 'color: нечисловые компоненты → null');
eq(F.parseCssColor(''), null, 'color: пустая строка → null');
eq(F.parseCssColor(undefined), null, 'color: undefined → null');
eq(F.parseCssColor({}), null, 'color: объект → null');

eq(F.relativeLuminance({ r: 0, g: 0, b: 0 }), 0, 'lum: чёрный — 0');
ok(Math.abs(F.relativeLuminance({ r: 255, g: 255, b: 255 }) - 1) < 1e-9, 'lum: белый — 1');
ok(F.relativeLuminance({ r: 0, g: 255, b: 0 }) > 0.7, 'lum: зелёный весит больше всех');
eq(F.relativeLuminance(null), null, 'lum: без цвета — null');
eq(F.relativeLuminance({ r: 1, g: 2 }), null, 'lum: неполный цвет — null');

eq(F.resolveTgTheme('dark', '#ffffff'), 'dark', 'resolve: явная тёмная игнорирует тему приложения');
eq(F.resolveTgTheme('light', '#000000'), 'light', 'resolve: явная светлая игнорирует тему приложения');
eq(F.resolveTgTheme('auto', '#101418'), 'dark', 'resolve: auto на тёмном фоне');
eq(F.resolveTgTheme('auto', '#f5f5f5'), 'light', 'resolve: auto на светлом фоне');
eq(F.resolveTgTheme('auto', 'rgb(250 250 250)'), 'light', 'resolve: auto понимает rgb()');
eq(F.resolveTgTheme('auto', 'oklch(0.9 0 0)'), F.TG_THEME_FALLBACK,
    'resolve: нераспознанный цвет → запасная тема');
eq(F.resolveTgTheme('auto', ''), 'dark', 'resolve: переменная ещё не вычислена → тёмная');
eq(F.TG_THEME_MODES.length, 3, 'theme: ровно три режима');

ok(F.tgThemeToggleTitle('auto', 'dark').includes('авто'), 'title: auto подписан как авто');
ok(F.tgThemeToggleTitle('auto', 'light').includes('светлая'), 'title: auto показывает вычисленную тему');
ok(F.tgThemeToggleTitle('dark', 'dark').includes('тёмная'), 'title: dark подписан');
ok(F.tgThemeToggleTitle('light', 'light').includes('светлая'), 'title: light подписан');

// ---------- краевые случаи: имена из реального Telegram ----------
eq(F.displayName(null), 'anon', 'имя: null-диалог не роняет рендер списка');
eq(F.displayNick(null), '', 'ник: null-диалог не роняет рендер списка');
eq(F.displayName({ title: 'مرحبا', kind: 'user' }), 'مرحبا', 'имя: RTL показывается как есть');
eq(F.displayName({ title: '\u{1F525}\u{1F525}' }), '\u{1F525}\u{1F525}',
    'имя: из одних emoji — тоже имя (прежний слаг давал anon)');
eq(F.displayName({ title: 'Café Ménu' }), 'Café Ménu', 'имя: диакритика сохраняется');
eq(F.displayName({ title: '\u{1F1F7}\u{1F1FA} Соня' }), '\u{1F1F7}\u{1F1FA} Соня',
    'имя: составная графема (флаг из двух кодовых точек) не рвётся');
eq(F.displayName({ title: '\u{1F469}\u200D\u{1F4BB}' }), '\u{1F469}\u200D\u{1F4BB}',
    'имя: ZWJ-последовательность остаётся одной графемой');
eq(F.displayNick({ title: 'X', username: 'SOFIA' }), '@SOFIA',
    'ник: регистр username сохраняется — это его настоящее написание');

// ---------- краевые случаи: границы relativeAge ----------
eq(F.relativeAge(59), 'now', 'age: 59с ещё now');
eq(F.relativeAge(60), '1m', 'age: ровно минута');
eq(F.relativeAge(3599), '59m', 'age: 59м59с ещё минуты');
eq(F.relativeAge(3600), '1h', 'age: ровно час');
eq(F.relativeAge(86399), '23h', 'age: 23:59:59 ещё часы');
eq(F.relativeAge(86400), '1d', 'age: ровно сутки');
eq(F.relativeAge(-5), 'now', 'age: отрицательный возраст (рассинхрон часов) → now');
eq(F.relativeAge(NaN), 'now', 'age: NaN → now, а не «NaNm»');

// ---------- краевые случаи: рассинхрон часов клиента и сервера ----------
eq(F.dialogStatusLine({ kind: 'user' }, { online: false, wasOnline: 2000 }, 1500),
    '[IDLE] last now', 'status: wasOnline в будущем не рисует отрицательный возраст');
eq(F.dialogStatusLine({ kind: 'user' }, { online: false, wasOnline: 0 }, 1500),
    '[OFFLINE]', 'status: wasOnline=0 — «неизвестно», а не 1970 год');
eq(F.dialogStatusLine({ kind: 'group', last_message: { id: 0 } }), '[GROUP]',
    'status: id=0 последнего сообщения не рисует «0 msgs»');

// ---------- краевые случаи: мусор в списке диалогов ----------
eq(F.splitDialogs('oops'), { nodes: [], channels: [] }, 'split: не-массив не роняет рендер');
eq(F.splitDialogs([{ id: '1', kind: 'user' }, 'x', 5]).nodes.length, 1,
    'split: скалярный мусор внутри списка отбрасывается');
eq(F.unreadTotal([{ id: '1', unread_count: 2 }, { id: '2', unread_count: 5 }], new Set(['2'])), 2,
    'unread: muted как Set (боевой путь) фильтрует так же, как массив');
eq(F.unreadTotal([{ id: '1', unread_count: -3 }, { id: '2', unread_count: 4 }]), 4,
    'unread: отрицательный счётчик не уменьшает сумму');
eq(F.unreadBadgeText(-1), '', 'бейдж: отрицательное значение скрывается');
eq(F.unreadBadgeText(99), '99', 'бейдж: ровно 99 ещё без плюса');

// ---------- краевые случаи: полночь, смена года, до-эпохальное время ----------
const midnight = Math.floor(new Date(2026, 0, 1, 0, 0, 0).getTime() / 1000);
eq(F.formatTime(midnight), '00:00:00', 'время: полночь с ведущими нулями');
const dec31 = Math.floor(new Date(2026, 11, 31, 23, 59, 59).getTime() / 1000);
const jan1 = Math.floor(new Date(2027, 0, 1, 0, 0, 0).getTime() / 1000);
eq(F.dayKey(dec31), '2026-12-31', 'день: канун Нового года');
eq(F.dayKey(jan1), '2027-01-01', 'день: месяц и день с ведущими нулями после смены года');
ok(F.needsDateSep({ date: dec31 }, { date: jan1 }), 'разделитель: граница годов — новый день');
const preEpoch = Math.floor(new Date(1969, 5, 15, 12, 0, 0).getTime() / 1000);
ok(preEpoch < 0, 'до-эпохальный timestamp действительно отрицательный');
eq(F.dayKey(preEpoch), '1969-06-15', 'день: отрицательный timestamp (форвард старого поста) не ломает ключ');
ok(!F.needsDateSep({ date: 1 }, null), 'разделитель: null вместо next — false, а не throw');

// ---------- краевые случаи: keyHex и шапка сессии ----------
eq(F.keyHex(''), F.keyHex(null), 'keyHex: пустая строка и null дают один и тот же ключ');
eq(F.keyHex('').length, 8, 'keyHex: даже для пустого id — 8 hex-символов');
ok(F.sessionHeadText({ id: '1', username: 'a', kind: 'user' }, -5).includes('⌁ 0 PACKETS'),
    'head: отрицательный счётчик прижимается к нулю');

// ---------- краевые случаи: nickOf ----------
eq(F.nickOf({ out: false, sender: { id: 777 } }), '777', 'ник: без имени остаётся числовой id строкой');
eq(F.nickOf(null, null), 'unknown', 'ник: null-сообщение не роняет рендер');

// ---------- краевые случаи: WS-доставка вне очереди ----------
const midList = [{ id: 1 }, { id: 5 }];
eq(F.insertMessage(midList, { id: 3 }).index, 1, 'insert: сообщение между существующими встаёт в середину');
eq(midList.map((m) => m.id), [1, 3, 5], 'insert: порядок после вставки в середину');
eq(F.insertMessage(null, { id: 7 }).inserted, true, 'insert: не-массив заменяется пустой лентой без throw');

// ---------- краевые случаи: двойной старт подгрузки истории ----------
const race = [{ id: 10 }];
const onceMerged = F.prependMessages([{ id: 9 }, { id: 8 }], race);
eq(onceMerged.map((m) => m.id), [8, 9, 10], 'prepend: несортированная страница сортируется');
const twiceMerged = F.prependMessages([{ id: 9 }, { id: 8 }], onceMerged);
eq(twiceMerged.map((m) => m.id), [8, 9, 10], 'prepend: та же страница второй раз (гонка скролла) не дублирует');
eq(F.oldestId([{ id: NaN }, { id: 3 }, null, { id: 1 }]), 1, 'oldestId: NaN и null не становятся курсором');

// ---------- краевые случаи: точные пороги скролла ----------
ok(F.isAtBottom({ scrollTop: 0, scrollHeight: 140, clientHeight: 100 }),
    'stick: зазор ровно в порог (40) считается низом');
ok(F.isAtBottom(null), 'stick: метрик ещё нет (лента не отрисована) — прижимаем');
ok(!F.shouldLoadOlder({ scrollTop: 200 }, { hasMore: true, loading: false }),
    'подгрузка: ровно на пороге 200 не срабатывает (строго <)');
ok(!F.shouldLoadOlder({ scrollTop: 150 }, { hasMore: true, loading: false, threshold: 100 }),
    'подгрузка: пользовательский порог уважается');

// ---------- краевые случаи: часы прыгнули назад (NTP-коррекция) ----------
ok(!F.toastAllowed(5000, 1000), 'тост: прыжок часов назад подавляет показ, а не роняет');

// ---------- краевые случаи: typingLine с пользовательским cap ----------
eq(F.typingLine(['a', 'b', 'c', 'd'], 1), '>> a +3 are typing…', 'typing: cap=1 сворачивает всех, кроме первого');

// ---------- краевые случаи: граница отсечки спящей вкладки ----------
eq(F.latencyFromPingDrift(1000, 1000 + 15000 + 30000, 15000), 30000, 'lat: дрейф ровно 30с ещё считается');
eq(F.latencyFromPingDrift(1000, 1000 + 15000 + 30001, 15000), null, 'lat: 30с + 1мс — уже спящая вкладка');

// ---------- краевые случаи: середина шкалы яркости ----------
eq(F.parseCssColor('gray'), { r: 128, g: 128, b: 128 }, 'color: именованный gray');
eq(F.resolveTgTheme('auto', '#808080'), 'light', 'resolve: середина шкалы (L≈0.502) уходит в светлую');

// ---------- краевые случаи: спуфинг имени файла и лимиты ФС ----------
// U+202E разворачивает хвост строки: «invoice<U+202E>fdp.exe» глазами читается
// как «invoiceexe.pdf», оставаясь исполняемым .exe. После санитизации
// bidi-символ обязан исчезнуть, а честное имя — остаться.
eq(F.sanitizeDownloadName('invoice\u202Efdp.exe'), 'invoicefdp.exe', 'sanitize: U+202E (RTL-override) вычищен — спуфинг расширения невозможен');
eq(
    F.sanitizeDownloadName('a\u202A\u202B\u202C\u202D\u2066\u2067\u2068\u2069\u200B\u200C\u200D\u200E\u200F\uFEFFb.txt'),
    'ab.txt',
    'sanitize: всё семейство bidi/невидимых удалено'
);
eq(F.sanitizeDownloadName('badname.txt'), 'bad__name.txt', 'sanitize: C1-контролы (0x80-0x9F) заменены');
{
    const enc = new TextEncoder();
    const capped = F.sanitizeDownloadName('а'.repeat(300) + '.pdf'); // кириллица: 2 байта/символ → 604 байта
    ok(enc.encode(capped).length <= 255, 'sanitize: имя обрезано до ≤255 байт UTF-8 (лимит ext4/APFS)');
    ok(capped.endsWith('.pdf'), 'sanitize: при обрезке расширение сохранено');
}

// ---------- краевые случаи: суррогатные пары в truncate ----------
eq(F.truncate('😀😀😀', 2), '😀…', 'truncate: режет по code points, без одинокого суррогата «�»');
eq(F.truncate('😀😀😀', 4), '😀😀😀', 'truncate: 3 code points влезают в лимит 4, строка не тронута');

// ---------- краевые случаи: перекат тысячи в formatBytes ----------
eq(F.formatBytes(999999), '1.0 MB', 'bytes: 999 999 Б — перекат в MB, а не «1000 KB»');
eq(F.formatBytes(999499), '999 KB', 'bytes: 999 499 Б остаётся в KB');
eq(F.formatBytes(999950000000), '1.0 TB', 'bytes: перекат работает и на границе GB→TB');

// ---------- папки: folderSlug ----------
eq(F.folderSlug({ id: 2, title: 'Work Chats' }), 'work_chats', 'fslug: латиница, пробелы → _');
eq(F.folderSlug({ id: 2, title: 'Работа' }), 'работа', 'fslug: юникод-буквы сохраняются — папка это «путь», а не имя человека');
eq(F.folderSlug({ id: 3, title: '💼 Работа' }), 'работа', 'fslug: emoji сжимается и срезается с края');
eq(F.folderSlug({ id: 4, title: '🔥🔥' }), 'folder_4', 'fslug: имя из одних emoji → folder_<id>');
eq(F.folderSlug({ id: 5, title: '' }), 'folder_5', 'fslug: пустое имя → folder_<id>');
eq(F.folderSlug({ title: '' }), 'folder', 'fslug: пустое имя без id не роняет рендер');
eq(F.folderSlug({ id: 6, title: '..Dev..' }), 'dev', 'fslug: ведущие/хвостовые точки срезаются');
ok(F.folderSlug({ id: 7, title: 'x'.repeat(80) }).length <= F.SLUG_MAX, 'fslug: длина обрезается по SLUG_MAX');
ok(!F.folderSlug({ id: 8, title: '<img onerror=alert(1)>' }).includes('<'), 'fslug: разметка вычищается');

// ---------- папки: folderSlugs (дубликаты имён) ----------
eq(
    F.folderSlugs([{ id: 1, title: 'Work' }, { id: 2, title: 'work' }, { id: 3, title: 'Work' }]),
    ['work', 'work_2', 'work_3'],
    'fslugs: дубликаты получают суффиксы _2, _3'
);
eq(F.folderSlugs([{ id: 1, title: 'Inbox' }]), ['inbox_2'], 'fslugs: имя inbox зарезервировано штатным пунктом');
eq(F.folderSlugs([{ id: 1, title: 'Archive' }]), ['archive_2'], 'fslugs: имя archive зарезервировано штатным пунктом');
eq(F.folderSlugs([]), [], 'fslugs: аккаунт без папок');
eq(F.folderSlugs(null), [], 'fslugs: не-массив не роняет рендер');

// ---------- папки: dirEntries ----------
{
    const entries = F.dirEntries([
        { id: 2, title: 'Работа', emoticon: '💼' },
        { id: 9, title: 'News', emoticon: null },
    ]);
    eq(entries.map((e) => e.key), ['inbox', 'folder:2', 'folder:9', 'archive'],
        'dirs: порядок inbox → папки Telegram → archive');
    eq(entries[0].name, '~/inbox', 'dirs: штатный inbox');
    eq(entries[1].name, '~/работа', 'dirs: имя папки в терминальном стиле ~/<slug>');
    eq(entries[1].emoticon, '💼', 'dirs: emoticon папки прокидывается');
    eq(entries[2].emoticon, '', 'dirs: null-emoticon превращается в пустую строку');
    eq(entries[3].name, '~/archive', 'dirs: архив замыкает список');
    eq(entries[1].folder.id, 2, 'dirs: объект папки доступен для фильтрации');
}
eq(F.dirEntries([]).map((e) => e.key), ['inbox', 'archive'], 'dirs: без папок — только штатные пункты');
eq(F.dirEntries(null).map((e) => e.key), ['inbox', 'archive'], 'dirs: не-массив не роняет рендер');
eq(F.dirEntries([null, 'x', { id: 2, title: 'A' }]).length, 3, 'dirs: мусор в списке папок отбрасывается');

// ---------- папки: dialogInFolder — явные списки ----------
const fUser = { id: '111', kind: 'user', unread_count: 1 };
const fGroup = { id: '-200', kind: 'group', unread_count: 0 };
const fChan = { id: '-1003', kind: 'channel', unread_count: 5 };
ok(F.dialogInFolder(fUser, { include_ids: ['111'] }), 'folder: include_ids входит без единого флага');
ok(F.dialogInFolder(fUser, { pinned_ids: ['111'] }), 'folder: pinned_ids входит без единого флага');
ok(!F.dialogInFolder(fUser, { include_ids: ['222'] }), 'folder: чужой id не входит');
ok(!F.dialogInFolder(fUser, { include_ids: ['111'], exclude_ids: ['111'] }),
    'folder: exclude_ids сильнее include (рассинхрон — безопаснее скрыть)');
ok(!F.dialogInFolder(fUser, { contacts: true, exclude_ids: ['111'] }),
    'folder: exclude_ids сильнее категории');
// Числовой id в списке папки сравнивается со строковым id диалога.
ok(F.dialogInFolder(fUser, { include_ids: [111] }), 'folder: id сравниваются как строки');

// ---------- папки: dialogInFolder — категории по kind ----------
ok(F.dialogInFolder(fGroup, { groups: true }), 'folder: groups ловит группу');
ok(!F.dialogInFolder(fGroup, { contacts: true, broadcasts: true, bots: true }),
    'folder: группа не подпадает под чужие флаги');
ok(F.dialogInFolder(fChan, { broadcasts: true }), 'folder: broadcasts ловит канал');
ok(!F.dialogInFolder(fChan, { groups: true }), 'folder: канал не группа');
// DTO старого сервера (без bot/contact): категория неизвестна, поэтому любой
// user-флаг ловит такой диалог — прежнее поведение, см. dialogInFolder.
ok(F.dialogInFolder(fUser, { contacts: true }), 'folder: contacts ловит user старого DTO');
ok(F.dialogInFolder(fUser, { non_contacts: true }), 'folder: non_contacts ловит user старого DTO');
ok(F.dialogInFolder(fUser, { bots: true }), 'folder: bots ловит user старого DTO (полей нет — не классифицируем)');
ok(!F.dialogInFolder(fUser, { groups: true, broadcasts: true }), 'folder: user не подпадает под groups/broadcasts');
ok(!F.dialogInFolder(fUser, {}), 'folder: без флагов и include — не входит');

// ---------- папки: dialogCategory (D3 — боты, контакты, неконтакты) ----------
const catBot = { id: '201', kind: 'user', bot: true, contact: false, unread_count: 1 };
const catBotContact = { id: '202', kind: 'user', bot: true, contact: true, unread_count: 1 };
const catContact = { id: '203', kind: 'user', bot: false, contact: true, unread_count: 1 };
const catStranger = { id: '204', kind: 'user', bot: false, contact: false, unread_count: 1 };
const catGroup = { id: '-205', kind: 'group', bot: false, contact: false, unread_count: 1 };
const catChan = { id: '-1006', kind: 'channel', bot: false, contact: false, unread_count: 1 };
eq(F.dialogCategory(catBot), 'bot', 'cat: bot');
eq(F.dialogCategory(catBotContact), 'bot', 'cat: бот в контактах — всё равно bot (как в TDLib)');
eq(F.dialogCategory(catContact), 'contact', 'cat: человек в контактах');
eq(F.dialogCategory(catStranger), 'non_contact', 'cat: человек не в контактах');
eq(F.dialogCategory(catGroup), 'group', 'cat: группа');
eq(F.dialogCategory(catChan), 'channel', 'cat: канал');
eq(F.dialogCategory(fUser), 'user', 'cat: DTO без полей — «не знаем», а не non_contact');
eq(F.dialogCategory({ id: '1', kind: 'user', contact: true }), 'contact',
    'cat: есть только contact — bot трактуется как false');
eq(F.dialogCategory({ id: '1', kind: 'user', bot: true }), 'bot',
    'cat: есть только bot — contact трактуется как false');
eq(F.dialogCategory(null), 'user', 'cat: null не роняет классификацию');

// ---------- папки: dialogInFolder — категории по bot/contact ----------
ok(F.dialogInFolder(catBot, { bots: true }), 'folder: bots ловит бота');
ok(!F.dialogInFolder(catBot, { contacts: true, non_contacts: true }),
    'folder: бот не подпадает под contacts/non_contacts');
// Главный кейс жалобы наоборот: бот-контакт не должен утекать в папку «Контакты».
ok(!F.dialogInFolder(catBotContact, { contacts: true }),
    'folder: бот в контактах НЕ попадает в папку только с contacts');
ok(F.dialogInFolder(catBotContact, { bots: true }),
    'folder: бот в контактах попадает в папку с bots');
ok(F.dialogInFolder(catContact, { contacts: true }), 'folder: contacts ловит контакта');
ok(!F.dialogInFolder(catContact, { bots: true, non_contacts: true }),
    'folder: контакт не подпадает под bots/non_contacts');
ok(F.dialogInFolder(catStranger, { non_contacts: true }), 'folder: non_contacts ловит неконтакта');
ok(!F.dialogInFolder(catStranger, { contacts: true, bots: true }),
    'folder: неконтакт не подпадает под contacts/bots');
ok(!F.dialogInFolder(catBot, { groups: true, broadcasts: true }),
    'folder: бот не группа и не канал');
ok(F.dialogInFolder(catGroup, { groups: true }), 'folder: группа с новыми полями ловится groups');
ok(F.dialogInFolder(catChan, { broadcasts: true }), 'folder: канал с новыми полями ловится broadcasts');
// Папка со всеми флагами («все чаты, кроме…») обязана вмещать все категории.
{
    const all = {
        contacts: true, non_contacts: true, groups: true, broadcasts: true, bots: true,
    };
    ok([catBot, catBotContact, catContact, catStranger, catGroup, catChan, fUser]
        .every((d) => F.dialogInFolder(d, all)),
        'folder: папка со всеми флагами вмещает все категории, включая старый DTO');
}
// Папка «только боты» не должна тащить за собой людей и группы.
{
    const onlyBots = { bots: true };
    eq([catBot, catBotContact, catContact, catStranger, catGroup, catChan]
        .filter((d) => F.dialogInFolder(d, onlyBots)).map((d) => d.id),
        ['201', '202'], 'folder: папка bots отбирает ровно ботов');
}
// exclude_*-флаги применяются ПОСЛЕ категории и к ботам тоже.
{
    const mutedBot = { id: '206', kind: 'user', bot: true, contact: false, muted: true, unread_count: 3 };
    const readBot = { id: '207', kind: 'user', bot: true, contact: false, unread_count: 0 };
    const archBot = { id: '208', kind: 'user', bot: true, contact: false, archived: true, unread_count: 2 };
    ok(!F.dialogInFolder(mutedBot, { bots: true, exclude_muted: true }),
        'folder: exclude_muted убирает заглушённого бота');
    ok(F.dialogInFolder(mutedBot, { bots: true }), 'folder: без exclude_muted заглушённый бот виден');
    ok(!F.dialogInFolder(readBot, { bots: true, exclude_read: true }),
        'folder: exclude_read убирает прочитанного бота');
    ok(F.dialogInFolder(catBot, { bots: true, exclude_read: true }),
        'folder: непрочитанный бот переживает exclude_read');
    ok(!F.dialogInFolder(archBot, { bots: true, exclude_archived: true }),
        'folder: exclude_archived убирает архивного бота');
    ok(F.dialogInFolder(archBot, { bots: true }), 'folder: без exclude_archived архивный бот виден');
    // Явные списки сильнее и категории, и exclude_*-флагов — и для ботов.
    ok(F.dialogInFolder(mutedBot, { contacts: true, include_ids: ['206'], exclude_muted: true }),
        'folder: include тащит бота в папку без флага bots, минуя exclude_muted');
    ok(!F.dialogInFolder(catBot, { bots: true, exclude_ids: ['201'] }),
        'folder: exclude_ids сильнее категории bots');
}

// ---------- список: botMark ----------
eq(F.BOT_MARK, '[BOT]', 'bot: терминальная метка — [BOT], в одном алфавите с [M]');
eq(F.botMark(true), '[BOT]', 'bot: метка для бота');
eq(F.botMark(false), '', 'bot: человек без метки');
eq(F.botMark(undefined), '', 'bot: старый DTO без поля — метки нет');

// ---------- папки: dialogInFolder — exclude_* флаги ----------
const mutedUser = { id: '112', kind: 'user', muted: true, unread_count: 3 };
const readUser = { id: '113', kind: 'user', unread_count: 0 };
const archUser = { id: '114', kind: 'user', archived: true, unread_count: 2 };
ok(!F.dialogInFolder(mutedUser, { contacts: true, exclude_muted: true }), 'folder: exclude_muted убирает muted');
ok(F.dialogInFolder(mutedUser, { contacts: true }), 'folder: без exclude_muted muted остаётся');
ok(!F.dialogInFolder(readUser, { contacts: true, exclude_read: true }), 'folder: exclude_read убирает прочитанные');
ok(F.dialogInFolder(fUser, { contacts: true, exclude_read: true }), 'folder: непрочитанный переживает exclude_read');
ok(!F.dialogInFolder(archUser, { contacts: true, exclude_archived: true }), 'folder: exclude_archived убирает архив');
ok(F.dialogInFolder(archUser, { contacts: true }), 'folder: без exclude_archived архивный виден в папке');
// Явное включение сильнее флаговых исключений (семантика Telegram).
ok(F.dialogInFolder(mutedUser, { include_ids: ['112'], exclude_muted: true }),
    'folder: include минует exclude_muted');
ok(F.dialogInFolder(readUser, { pinned_ids: ['113'], exclude_read: true }),
    'folder: pinned минует exclude_read');
ok(F.dialogInFolder(archUser, { include_ids: ['114'], exclude_archived: true }),
    'folder: include минует exclude_archived');
// Chatlist (общая папка-ссылка): все флаги false, работает чисто по include.
ok(F.dialogInFolder(fUser, {
    include_ids: ['111'], exclude_ids: [], contacts: false, non_contacts: false,
    groups: false, broadcasts: false, bots: false,
    exclude_muted: false, exclude_read: false, exclude_archived: false,
}), 'folder: chatlist-папка работает чисто по include_ids');
ok(!F.dialogInFolder(null, { contacts: true }), 'folder: null-диалог не роняет фильтр');
ok(!F.dialogInFolder(fUser, null), 'folder: null-папка не роняет фильтр');

// ---------- папки: КРАЕВЫЕ случаи классификации ----------
// Мегагруппа — самый скользкий kind: у неё id из диапазона каналов
// (-100…), а в DialogDto бэкенд кладёт kind='group' (dto.rs::peer_kind
// раскладывает megagroup в Peer::Group). Фронт обязан смотреть ТОЛЬКО на kind,
// иначе рабочие супергруппы уедут из папки «Группы» в «Каналы».
{
    const mega = { id: '-1001234567890', kind: 'group', title: 'Супергруппа', unread_count: 4 };
    eq(F.dialogCategory(mega), 'group', 'cat: мегагруппа — группа, несмотря на channel-id');
    ok(F.dialogInFolder(mega, { groups: true }), 'folder: мегагруппа ловится флагом groups');
    ok(!F.dialogInFolder(mega, { broadcasts: true }),
        'folder: мегагруппа НЕ канал — broadcasts её не ловит');
    ok(!F.dialogInFolder(mega, { contacts: true, non_contacts: true, bots: true }),
        'folder: мегагруппа не подпадает под user-флаги');
    ok(!F.dialogInFolder({ ...mega, muted: true }, { groups: true, exclude_muted: true }),
        'folder: exclude_muted работает и на мегагруппе');
    eq(F.dialogsForDir([mega], { type: 'folder', folder: { groups: true } }).length, 1,
        'folder: мегагруппа доезжает до колонки через dialogsForDir');
}
// kind сильнее флагов bot/contact: канал с bot:true (мусорный DTO) остаётся
// каналом — иначе один битый ответ разбросал бы каналы по папке «Боты».
eq(F.dialogCategory({ id: '301', kind: 'channel', bot: true, contact: true }), 'channel',
    'cat: kind сильнее bot/contact');
eq(F.dialogCategory({ id: '302', kind: 'group', bot: true }), 'group',
    'cat: группа с флагом бота остаётся группой');
// Частичный DTO: хотя бы одно boolean-поле ⇒ сервер новый и классификация
// возможна. Отсутствующий флаг читается как false.
eq(F.dialogCategory({ id: '303', kind: 'user', bot: false }), 'non_contact',
    'cat: пришёл только bot:false — это человек не из контактов');
eq(F.dialogCategory({ id: '304', kind: 'user', contact: true }), 'contact',
    'cat: пришёл только contact:true — контакт');
eq(F.dialogCategory({ id: '305', kind: 'user', bot: true, contact: undefined }), 'bot',
    'cat: contact:undefined не мешает распознать бота');
// А вот НЕ-boolean значения — это «сервер не сказал»: классифицировать по ним
// нельзя, иначе строка "false" (истинная в JS) сделала бы ботом каждого.
eq(F.dialogCategory({ id: '306', kind: 'user', bot: 'true', contact: 'no' }), 'user',
    'cat: строковые флаги — старый/битый DTO, классификации нет');
eq(F.dialogCategory({ id: '307', kind: 'user', bot: 1, contact: false }), 'non_contact',
    'cat: bot числом ботом не считается (строгий boolean)');
eq(F.dialogCategory({ id: '308', bot: true, contact: false }), 'bot',
    'cat: kind по умолчанию user — бот распознаётся и без него');
eq(F.dialogCategory([]), 'user', 'cat: массив вместо DTO не роняет классификацию');
eq(F.dialogCategory('строка'), 'user', 'cat: строка вместо DTO не роняет классификацию');
eq(F.dialogCategory(undefined), 'user', 'cat: undefined не роняет классификацию');
// Диалог со строковым unread_count (JSON-число, пришедшее строкой) — проверка,
// что exclude_read считает по числу, а не по «непустой строке».
ok(!F.dialogInFolder({ id: '310', kind: 'user', bot: true, unread_count: '0' },
    { bots: true, exclude_read: true }), 'folder: unread_count "0" строкой — прочитанный');
ok(F.dialogInFolder({ id: '311', kind: 'user', bot: true, unread_count: '5' },
    { bots: true, exclude_read: true }), 'folder: unread_count "5" строкой — непрочитанный');
// Явные списки: exclude сильнее pinned (в Telegram чат не бывает в обоих,
// но при рассинхроне безопаснее скрыть, чем показать лишнее).
ok(!F.dialogInFolder({ id: '312', kind: 'user', bot: true },
    { bots: true, pinned_ids: ['312'], exclude_ids: ['312'] }),
    'folder: exclude_ids сильнее pinned_ids');
// Chatlist с ботом: без флагов и без include бот в общую папку-ссылку не идёт.
{
    const bot = { id: '313', kind: 'user', bot: true, contact: false, unread_count: 1 };
    ok(!F.dialogInFolder(bot, {
        contacts: false, non_contacts: false, groups: false, broadcasts: false, bots: false,
    }), 'folder: chatlist без include бота не показывает');
    ok(F.dialogInFolder(bot, { include_ids: [313] }),
        'folder: chatlist показывает бота по include_ids (id числом)');
}
// Неизвестный тип селектора (папку удалили на другом устройстве, ключ протух)
// ведёт себя как inbox: архив в нём не показывается, остальное — да.
ok(F.dialogMatchesDir({ id: '314', kind: 'user' }, { type: 'зомби' }),
    'dir: неизвестный тип селектора ведёт себя как inbox');
ok(!F.dialogMatchesDir({ id: '315', kind: 'user', archived: true }, { type: 'зомби' }),
    'dir: неизвестный тип селектора не показывает архив');
ok(F.dialogMatchesDir({ id: '316', kind: 'user' }, undefined),
    'dir: undefined-селектор не роняет фильтр');
// type:'folder' без самой папки — тоже протухший ключ: показывать «всё подряд»
// в такой момент нельзя, иначе колонка соврёт про содержимое папки.
ok(!F.dialogMatchesDir({ id: '317', kind: 'user' }, { type: 'folder' }),
    'dir: folder без folder-объекта не показывает ничего');

// ---------- папки: dialogMatchesDir / dialogsForDir ----------
const dirDialogs = [
    { id: '1', kind: 'user', unread_count: 2 },
    { id: '2', kind: 'user', muted: true, unread_count: 7 },
    { id: '-300', kind: 'group', unread_count: 1, archived: true },
    { id: '-1004', kind: 'channel', unread_count: 4 },
];
ok(F.dialogMatchesDir(dirDialogs[0], { type: 'inbox' }), 'dir: inbox — неархивированный входит');
ok(!F.dialogMatchesDir(dirDialogs[2], { type: 'inbox' }), 'dir: inbox — архивный не входит');
ok(F.dialogMatchesDir(dirDialogs[2], { type: 'archive' }), 'dir: archive — только archived');
ok(!F.dialogMatchesDir(dirDialogs[0], { type: 'archive' }), 'dir: archive — обычный не входит');
ok(F.dialogMatchesDir(dirDialogs[0], null), 'dir: неизвестный селектор ведёт себя как inbox');
ok(F.dialogMatchesDir(dirDialogs[3], { type: 'folder', folder: { broadcasts: true } }),
    'dir: folder делегирует dialogInFolder');
eq(F.dialogsForDir(dirDialogs, { type: 'inbox' }).map((d) => d.id), ['1', '2', '-1004'],
    'dir: inbox фильтрует архив, порядок сервера сохраняется');
eq(F.dialogsForDir(dirDialogs, { type: 'archive' }).map((d) => d.id), ['-300'],
    'dir: archive отдаёт только архивные');
eq(F.dialogsForDir('oops', { type: 'inbox' }), [], 'dir: не-массив не роняет рендер');

// ---------- папки: dirUnread (счётчик с учётом muted) ----------
eq(F.dirUnread(dirDialogs, { type: 'inbox' }), 13, 'dirUnread: сумма по inbox без учёта mute');
eq(F.dirUnread(dirDialogs, { type: 'inbox' }, new Set(['2'])), 6,
    'dirUnread: непрочитанное заглушённого не входит в счётчик папки');
eq(F.dirUnread(dirDialogs, { type: 'archive' }, new Set(['2'])), 1, 'dirUnread: архив считается отдельно');
eq(F.dirUnread(dirDialogs, { type: 'folder', folder: { broadcasts: true } }, new Set()), 4,
    'dirUnread: пользовательская папка считает только свои диалоги');

// ---------- mute: mutedIds / mutedMark ----------
eq(Array.from(F.mutedIds(dirDialogs)), ['2'], 'muted: Set собирается из поля muted DialogDto');
eq(F.mutedIds([]).size, 0, 'muted: пустой список — пустой Set');
eq(F.mutedIds(null).size, 0, 'muted: не-массив не роняет пересчёт');
eq(F.unreadTotal(dirDialogs, F.mutedIds(dirDialogs)), 7, 'muted: mutedIds стыкуется с unreadTotal (бейдж ✈)');
eq(F.MUTED_MARK, '[M]', 'muted: терминальная метка — [M], без цветных emoji');
eq(F.mutedMark(true), '[M]', 'muted: метка для заглушённого');
eq(F.mutedMark(false), '', 'muted: обычный диалог без метки');

// ---------- архив: mergeArchive ----------
{
    const current = [
        { id: '1', kind: 'user', archived: false },
        { id: '2', kind: 'user', archived: true, unread_count: 0 },
        { id: '3', kind: 'user' },
    ];
    const fresh = [
        { id: '2', kind: 'user', archived: true, unread_count: 9 },
        { id: '4', kind: 'user', archived: true },
    ];
    const merged = F.mergeArchive(current, fresh);
    eq(merged.map((d) => d.id), ['1', '3', '2', '4'],
        'merge: неархивные остаются в своём порядке, архив заменяется свежим');
    eq(merged.find((d) => d.id === '2').unread_count, 9, 'merge: архивная запись обновилась из ответа сервера');
    // Диалог переехал в архив между загрузками: без дедупа был бы дважды.
    const moved = F.mergeArchive([{ id: '5', kind: 'user' }], [{ id: '5', kind: 'user', archived: true }]);
    eq(moved.map((d) => d.id), ['5'], 'merge: переехавший в архив диалог не дублируется');
    ok(moved[0].archived === true, 'merge: ответ ?folder=archive авторитетен — archived проставлен');
    ok(F.mergeArchive(null, [{ id: '6' }])[0].archived === true,
        'merge: archived проставляется, даже если сервер поле не заполнил');
    eq(F.mergeArchive([{ id: '1' }], null).map((d) => d.id), ['1'], 'merge: пустой ответ не стирает список');
}

// ---------- папки: mergeDialogs (догрузка расширенного списка) ----------
{
    const current = [
        { id: '1', kind: 'user', unread_count: 1 },
        { id: '2', kind: 'user', archived: true, unread_count: 4 },
        { id: '3', kind: 'user', unread_count: 0 },
    ];
    const fresh = [
        { id: '3', kind: 'user', unread_count: 7 },
        { id: '9', kind: 'user', bot: true, contact: false, unread_count: 2 },
    ];
    const merged = F.mergeDialogs(current, fresh);
    eq(merged.map((d) => d.id), ['3', '9', '1', '2'],
        'mergeDialogs: свежая страница первой, незатронутые дописываются в хвост');
    eq(merged[0].unread_count, 7, 'mergeDialogs: свежий ответ авторитетен по полям');
    ok(merged.some((d) => d.id === '9' && d.bot === true),
        'mergeDialogs: догруженный бот появляется в списке — ради этого всё и затевалось');
    ok(merged.some((d) => d.id === '2' && d.archived === true),
        'mergeDialogs: ранее подтянутый архив переживает догрузку');
    eq(F.mergeDialogs(current, null).map((d) => d.id), ['1', '2', '3'],
        'mergeDialogs: пустой ответ не стирает список');
    eq(F.mergeDialogs(null, fresh).map((d) => d.id), ['3', '9'],
        'mergeDialogs: не-массив на входе не роняет догрузку');
    eq(F.mergeDialogs([{ id: '5' }, null, 'x'], [{ id: 5, kind: 'user' }]).map((d) => String(d.id)),
        ['5'], 'mergeDialogs: id сравниваются как строки, мусор отбрасывается');
    ok(F.FOLDER_LIST_LIMIT > 100 && F.FOLDER_LIST_LIMIT <= 500,
        'mergeDialogs: лимит выборки под папку больше стартовых 100 и не выше потолка сервера');
}

// ---------- заголовок секции узлов по активной папке ----------
eq(F.nodesTitle(3, '~/работа'), '// ~/работа — 3 NODES', 'заголовок: имя активной папки');
eq(F.nodesTitle(3), '// ~/inbox — 3 NODES', 'заголовок: без имени — прежний ~/inbox');
eq(F.nodesTitle(3, ''), '// ~/inbox — 3 NODES', 'заголовок: пустое имя откатывается на ~/inbox');

console.log(`\n${checks - failures}/${checks} ассертов прошло (${path.basename(here)})`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
