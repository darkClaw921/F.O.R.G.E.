// tmux-web — VK plugin: чистые функции форматирования и сортировки.
//
// Модуль СПЕЦИАЛЬНО не импортирует ни DOM, ни core/*: он грузится как есть в
// Node и покрыт регресс-тестом tmux-web/tests/frontend/vk_format.test.mjs.
// Всё, что можно проверить без браузера, живёт здесь; dialogs.js и chat.js
// остаются тонкими рисовальщиками.
//
// Инвариант id: dialogId — СТРОКА (peer_id беседы равен 2_000_000_000 +
// chat_id), messageId и cmid — числа. Сравнивать dialogId можно только
// строками.
//
// ============================================================================
// ЧЕМ ЭТОТ ФАЙЛ ОТЛИЧАЕТСЯ ОТ js/telegram/format.js
// ============================================================================
//
// УБРАНО:
//   * весь блок тем — переехал в js/messenger/theme-core.js (он общий для
//     двух панелей, и держать две копии парсера цвета незачем);
//   * вся папочная логика (folderSlug/folderSlugs/dialogCategory/
//     categoryMatches/dialogInFolder/dialogMatchesDir/dialogsForDir/dirUnread/
//     mergeArchive/FOLDER_LIST_LIMIT) — у VK папок нет вовсе, их место заняли
//     ЧЕТЫРЕ СЕРВЕРНЫХ ФИЛЬТРА, а серверный фильтр нельзя посчитать на фронте:
//     переключение «директории» это ДРУГОЙ ЗАПРОС (см. dirEntries).
//
// ПЕРЕПИСАНО:
//   * dialogStatusLine — [CHAT] N members вместо телеграмного «N msgs»:
//     у VK нумерация сообщений СКВОЗНАЯ ПО АККАУНТУ, и «id последнего
//     сообщения» как оценка объёма беседы просто врёт;
//   * splitDialogs — сообщества это peer_id < 0;
//   * mediaKindLabel/mediaGlyph/MIME_EXT/KIND_EXT — словарь видов VK;
//   * MAX_UPLOAD_BYTES стал картой UPLOAD_LIMITS (у VK лимит зависит от вида
//     вложения) + uploadRejectReason(file, kind, limits).
//
// ПОЯВИЛОСЬ ВПЕРВЫЕ:
//   * peerKind(peerId) — тип собеседника выводится из САМОГО id (это то
//     место, где VK удобнее Telegram);
//   * cmidOf(msg) — conversation_message_id;
//   * captchaFromError(err) / captchaRetryBody(sid, key) — механика повтора
//     после кода 14;
//   * vkErrorLabel(code) — человеческий текст по числовому коду VK.
//
// КЛЮЧЕВОЕ: mediaKey ПОЛУЧИЛ ИНДЕКС ВЛОЖЕНИЯ — mediaKey(dialogId, messageId,
// index, kind). Телеграмный ключ без индекса не уникален: два фото в одном
// сообщении получили бы один ключ, и второе затёрло бы первое и в mediaCache,
// и в учёте objectURL.

/**
 * Нижняя граница peer_id групповых бесед у VK.
 *
 * Дублирует `dto::CHAT_PEER_BASE` бэкенда сознательно: это константа
 * ПРОТОКОЛА VK, а не наша договорённость, и тянуть её сетевым запросом было бы
 * странно. Расхождение поймал бы тест vk_format (границы 1999999999/2000000000).
 */
export const CHAT_PEER_BASE = 2000000000;

/**
 * Тип собеседника ПО ЕГО ИДЕНТИФИКАТОРУ.
 *
 * Это то место, где VK устроен удобнее Telegram: тип выводится из самого id,
 * без обращения к списку диалогов. Отсюда важное следствие для белого списка
 * (Ф10): видимость беседы, о которой пришло WS-событие и которой нет в
 * загруженном списке, решается ТОЧНО, а не консервативно.
 *
 * @param {string|number} peerId
 * @returns {'user'|'chat'|'community'}
 */
export function peerKind(peerId) {
    const n = Number(peerId);
    if (!Number.isFinite(n)) return 'user';
    if (n >= CHAT_PEER_BASE) return 'chat';
    return n < 0 ? 'community' : 'user';
}

/**
 * Тип диалога в лексике UI: из поля DTO, иначе из id.
 *
 * Бэкенд отдаёт `kind` в лексике VK (`user`|`group`|`chat`), а UI оперирует
 * `user`|`chat`|`community`. Перевод живёт ровно здесь — одна точка на весь
 * фронт.
 *
 * @param {object} d — DialogDto
 * @returns {'user'|'chat'|'community'}
 */
export function dialogKind(d) {
    const raw = d && typeof d.kind === 'string' ? d.kind : '';
    if (raw === 'group') return 'community';
    if (raw === 'chat' || raw === 'user') return raw;
    return peerKind(d && d.id);
}

/**
 * Человеческое имя беседы для колонки и шапки: `Иван Петров`.
 *
 * ЧТО БЫЛО ДО. Здесь жил терминальный слаг `slugName` (`durov@node`): имя
 * приводилось к нижнему регистру, а всё, что не `[a-z0-9_.-]`, сжималось в
 * `_`. У VK это било сильнее, чем у Telegram: русские названия бесед —
 * норма, а не исключение, и «Тестовое сообщество» превращалось в `anon@club`.
 * Полное имя существовало только в подсказке при наведении.
 *
 * ТЕПЕРЬ имя показывается КАК ЕСТЬ: кириллица, emoji и составные графемы не
 * трогаются вовсе — ни транслитерации, ни обрезки по кодовым точкам (она рвёт
 * суррогатные пары и ZWJ-последовательности). Единственная правка —
 * схлопывание пробелов: перевод строки в названии беседы разорвал бы
 * однострочную шапку сессии.
 *
 * Порядок запасных вариантов: title → @screen_name → peer_id. Он же объясняет
 * молчание [`displayNick`] при пустом title.
 *
 * @param {{username?: string, title?: string, id?: string}} d — DialogDto
 * @returns {string}
 */
export function displayName(d) {
    const title = cleanName(d && d.title);
    if (title) return title;
    const user = cleanNick(d && d.username);
    if (user) return `@${user}`;
    const id = d && d.id != null ? String(d.id) : '';
    return id || 'anon';
}

/**
 * Приписка ником: `@durov`, либо ПУСТАЯ строка.
 *
 * У VK `username` — это `screen_name` профиля или сообщества (проекция живёт
 * в `plugins/vk/src/dto.rs`). Его нет у обычных бесед и у части профилей, и
 * пустая строка здесь — значимый результат: старый слаг в этом случае лепил
 * ник из заголовка, и рядом с именем стояло то же имя, только испорченное.
 *
 * @param {{username?: string, title?: string}} d — DialogDto
 * @returns {string} `@ник` либо ''
 */
export function displayNick(d) {
    // Пустой title ⇒ ник уже занял место имени (см. displayName).
    if (!cleanName(d && d.title)) return '';
    const user = cleanNick(d && d.username);
    return user ? `@${user}` : '';
}

/** Схлопывает пробелы (в т.ч. перевод строки и NBSP) и обрезает края. */
function cleanName(v) {
    return v == null ? '' : String(v).replace(/\s+/g, ' ').trim();
}

/**
 * Ник без ведущих «@»: бэкенд отдаёт screen_name голым, но чужой источник
 * (ручной ввод в белом списке, старый кеш) вполне может принести «@durov», и
 * тогда в строке оказалось бы «@@durov».
 */
function cleanNick(v) {
    return cleanName(v).replace(/^@+/, '');
}

/**
 * Человеческий возраст события: `12m`, `3h`, `2d`.
 *
 * Секунды показываются как `now` — «0s назад» в терминальном UI читается хуже,
 * чем «сейчас».
 *
 * @param {number} ageSecs
 * @returns {string}
 */
export function relativeAge(ageSecs) {
    const s = Math.max(0, Math.floor(Number(ageSecs) || 0));
    if (s < 60) return 'now';
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
}

/**
 * Вторая строка элемента списка: статус беседы.
 *
 * - user: `[ONLINE]` / `[IDLE] last 3h` / `[OFFLINE]`;
 * - chat: `[CHAT] 4 members` (или просто `[CHAT]`, если участники неизвестны);
 * - community: `[CLUB]`.
 *
 * СЧЁТЧИКА СООБЩЕНИЙ ЗДЕСЬ НЕТ И БЫТЬ НЕ МОЖЕТ. У Telegram «N msgs» берётся из
 * id последнего сообщения, потому что в группах и каналах нумерация начинается
 * с единицы и монотонна. У VK `message.id` СКВОЗНОЙ ПО ВСЕМУ АККАУНТУ: в новой
 * беседе первое же сообщение получит id вроде 74212, и «74212 msgs» — это
 * просто ложь на экране. Внутрибеседная нумерация (`cmid`) в DialogDto не
 * ездит, а гнать за ней отдельный запрос при бюджете 3 rps незачем.
 *
 * @param {object} d — DialogDto
 * @param {{online?: boolean, wasOnline?: ?number}} [presence]
 * @param {number} [nowSecs] — для детерминизма в тестах
 * @returns {string}
 */
export function dialogStatusLine(d, presence, nowSecs) {
    const kind = dialogKind(d);
    if (kind === 'chat') {
        const n = Number(d && d.members);
        return Number.isFinite(n) && n > 0 ? `[CHAT] ${n} members` : '[CHAT]';
    }
    if (kind === 'community') return '[CLUB]';
    if (presence && presence.online) return '[ONLINE]';
    const was = presence && presence.wasOnline;
    if (Number.isFinite(was) && was > 0) {
        const now = Number.isFinite(nowSecs) ? nowSecs : Math.floor(Date.now() / 1000);
        return `[IDLE] last ${relativeAge(now - was)}`;
    }
    return '[OFFLINE]';
}

/**
 * Делит беседы на «узлы» (люди + групповые беседы) и «сообщества».
 *
 * Сообщество у VK — это peer_id < 0, и определяется оно по id, а не по полю
 * DTO: событие WS может принести беседу, которой в списке ещё нет.
 *
 * Порядок внутри групп сохраняется как пришёл с сервера: VK уже отсортировал
 * список по своей `sort_id`, а пересортировка на фронте ломала бы это ожидание.
 *
 * @param {Array<object>} dialogs
 * @returns {{nodes: Array<object>, communities: Array<object>}}
 */
export function splitDialogs(dialogs) {
    const nodes = [];
    const communities = [];
    for (const d of Array.isArray(dialogs) ? dialogs : []) {
        if (!d || typeof d !== 'object') continue;
        if (dialogKind(d) === 'community') communities.push(d);
        else nodes.push(d);
    }
    return { nodes, communities };
}

/**
 * Заголовок секции узлов: `// ~/all — 12 NODES`.
 *
 * @param {number} count
 * @param {string} [dirName='~/all']
 * @returns {string}
 */
export function nodesTitle(count, dirName) {
    const n = Math.max(0, Number(count) || 0);
    return `// ${String(dirName || '') || '~/all'} — ${n} NODES`;
}

/**
 * Суммарное непрочитанное по незаглушённым беседам.
 *
 * @param {Array<object>} dialogs
 * @param {Set<string>|Array<string>} [muted] — id заглушённых
 * @returns {number}
 */
export function unreadTotal(dialogs, muted) {
    const skip = muted instanceof Set ? muted : new Set(Array.isArray(muted) ? muted : []);
    let total = 0;
    for (const d of Array.isArray(dialogs) ? dialogs : []) {
        if (!d || skip.has(String(d.id))) continue;
        const n = Number(d.unread_count) || 0;
        if (n > 0) total += n;
    }
    return total;
}

/** Текст бейджа непрочитанного: пусто при 0, `99+` при переполнении. */
export function unreadBadgeText(total) {
    const n = Number(total) || 0;
    if (n <= 0) return '';
    return n > 99 ? '99+' : String(n);
}

// ---------------------------------------------------------------------------
// Секция // FILTERS (у Telegram — // DIRECTORIES).
// ---------------------------------------------------------------------------

/**
 * Четыре серверных фильтра — запасной список на случай, если
 * `GET /api/vk/filters` ещё не ответил (или ответил мусором).
 *
 * Ключи обязаны совпадать с `DIALOG_FILTERS` бэкенда: незнакомый ключ сервер
 * молча приводит к `all`, и пункт колонки перестал бы фильтровать, оставаясь
 * на экране.
 */
export const DEFAULT_FILTERS = [
    { key: 'all', title: 'Все' },
    { key: 'unread', title: 'Непрочитанные' },
    { key: 'important', title: 'Важные' },
    { key: 'unanswered', title: 'Без ответа' },
];

/**
 * Модель секции // FILTERS: `~/all`, `~/unread`, `~/important`, `~/unanswered`.
 *
 * ГЛАВНОЕ ОТЛИЧИЕ ОТ ПАПОК TELEGRAM: это НЕ фильтр загруженного списка, а
 * ПАРАМЕТР ЗАПРОСА `messages.getConversations`. Поэтому здесь нет ни
 * `dialogsForDir`, ни `dirUnread` — посчитать содержимое серверного фильтра, не
 * сделав запрос, невозможно. По той же причине не переносится и приглушение
 * пустых пунктов (`emptyDirKeys`): клиент не знает, пуст ли фильтр.
 *
 * @param {Array<{key: string, title: string}>} [filters] — ответ /api/vk/filters
 * @returns {Array<{key: string, name: string, title: string}>}
 */
export function dirEntries(filters) {
    const list = (Array.isArray(filters) ? filters : [])
        .filter((f) => f && typeof f === 'object' && typeof f.key === 'string' && f.key);
    const src = list.length ? list : DEFAULT_FILTERS;
    return src.map((f) => ({
        key: String(f.key),
        name: `~/${String(f.key)}`,
        title: String(f.title || f.key),
    }));
}

/**
 * Показывать ли счётчик непрочитанного у пункта фильтра.
 *
 * Только у `~/all` и `~/unread`: остальные два — транзиентные выборки, и
 * счётчик по ним считался бы по СЛУЧАЙНОЙ странице, а не по аккаунту. Число,
 * которое меняется от того, куда пользователь ткнул, хуже отсутствующего.
 *
 * @param {string} key
 * @returns {boolean}
 */
export function dirShowsCount(key) {
    return key === 'all' || key === 'unread';
}

/** Терминальная метка заглушённой беседы (без цветных emoji, как и глифы). */
export const MUTED_MARK = '[M]';

/** Метка mute для строки списка: `[M]` либо пустая строка. */
export function mutedMark(muted) {
    return muted ? MUTED_MARK : '';
}

/** Терминальная метка сообщества — в одном алфавите с [M]. */
export const BOT_MARK = '[BOT]';

/**
 * Метка бота для строки списка.
 *
 * У VK нет флага «бот»: сообщество и есть бот с точки зрения UI, и бэкенд
 * ставит `bot = (kind === "group")`. Метка нужна ровно затем же, зачем в
 * Telegram, — чтобы собеседник, который отвечает автоматом, был отличим.
 */
export function botMark(bot) {
    return bot ? BOT_MARK : '';
}

/**
 * Set из id заглушённых бесед — производная от поля `muted` DialogDto.
 *
 * ЕДИНСТВЕННЫЙ источник истины про mute — сам список: у VK глушение только
 * per-peer (`push_settings`), глобальных категорийных дефолтов, как в Telegram,
 * не существует, поэтому и второму состоянию взяться неоткуда.
 *
 * @param {Array<object>} dialogs
 * @returns {Set<string>}
 */
export function mutedIds(dialogs) {
    const s = new Set();
    for (const d of Array.isArray(dialogs) ? dialogs : []) {
        if (d && typeof d === 'object' && d.muted) s.add(String(d.id));
    }
    return s;
}

/**
 * Вливает ДОГРУЖЕННУЮ страницу бесед в текущий список.
 *
 * Свежий ответ авторитетен и идёт первым: в нём порядок VK и актуальные
 * muted/unread. Беседы, которых в ответе нет, дописываются в хвост — открытая
 * прямо сейчас беседа обязана пережить догрузку.
 *
 * @param {Array<object>} current
 * @param {Array<object>} fresh
 * @returns {Array<object>}
 */
export function mergeDialogs(current, fresh) {
    const next = (Array.isArray(fresh) ? fresh : []).filter((d) => d && typeof d === 'object');
    const ids = new Set(next.map((d) => String(d.id)));
    const rest = (Array.isArray(current) ? current : []).filter(
        (d) => d && typeof d === 'object' && !ids.has(String(d.id))
    );
    return next.concat(rest);
}

// ---------------------------------------------------------------------------
// Время и даты.
// ---------------------------------------------------------------------------

/** `HH:MM:SS` в локальной зоне. */
export function formatTime(unixSecs) {
    const d = new Date((Number(unixSecs) || 0) * 1000);
    const p = (v) => String(v).padStart(2, '0');
    return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/**
 * Ключ календарного дня `YYYY-MM-DD` в ЛОКАЛЬНОЙ зоне.
 *
 * Локальная, а не UTC: разделитель дат должен совпадать с тем, что человек
 * видит на часах, иначе вечерние сообщения уезжали бы в «завтра».
 */
export function dayKey(unixSecs) {
    const d = new Date((Number(unixSecs) || 0) * 1000);
    const p = (v) => String(v).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Подпись разделителя дат: `── 2026-07-29 ──`. */
export function dateSepLabel(key) {
    return `── ${key} ──`;
}

/** Нужен ли разделитель даты между двумя сообщениями. */
export function needsDateSep(prev, next) {
    if (!next) return false;
    if (!prev) return true;
    return dayKey(prev.date) !== dayKey(next.date);
}

/**
 * Короткий детерминированный хеш dialogId (FNV-1a, 8 hex-символов).
 *
 * Чисто декоративный «◇ KEY 0x…» в шапке сессии: даёт беседе стабильный
 * визуальный отпечаток, не раскрывая сам id.
 */
export function keyHex(dialogId) {
    const s = String(dialogId == null ? '' : dialogId);
    let h = 0x811c9dc5;
    for (let i = 0; i < s.length; i += 1) {
        h ^= s.charCodeAt(i);
        h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h.toString(16).padStart(8, '0');
}

/**
 * Шапка сессии, РАЗОБРАННАЯ на части: `{ name, nick, tail }`.
 *
 * Разделение существует ради одного: @ник в шапке красится приглушённым, а
 * покрасить кусок строки можно только отдельным узлом. Склейка живёт в
 * [`sessionHeadText`] — он остаётся единственным описанием того, как шапка
 * выглядит целиком, и им же пользуются тесты.
 *
 * `tail` начинается с разделителя ` · `, чтобы склейка не решала, ставить его
 * или нет: при пустом `nick` части просто складываются встык.
 *
 * @param {?object} dialog — DialogDto (null ⇒ ничего не открыто)
 * @param {number} count
 * @param {{online?: boolean, wasOnline?: ?number}} [presence]
 * @param {number} [nowSecs]
 * @returns {{name: string, nick: string, tail: string}}
 */
export function sessionHeadParts(dialog, count, presence, nowSecs) {
    if (!dialog) return { name: 'SESSION ▸ —', nick: '', tail: '' };
    const n = Math.max(0, Number(count) || 0);
    const rest = [];
    if (presence && dialogKind(dialog) === 'user') {
        rest.push(dialogStatusLine(dialog, presence, nowSecs));
    }
    rest.push(`⌁ ${n} PACKETS`);
    rest.push(`◇ KEY 0x${keyHex(dialog.id)}`);
    return {
        name: `SESSION ▸ ${displayName(dialog)}`,
        nick: displayNick(dialog),
        tail: ` · ${rest.join(' · ')}`,
    };
}

/**
 * Текст шапки сессии одной строкой:
 * `SESSION ▸ Павел Дуров @durov · [ONLINE] · ⌁ 42 PACKETS · ◇ KEY 0x…`.
 *
 * Присутствие показывается ТОЛЬКО для личных бесед и только когда оно
 * известно: у бесед и сообществ его не бывает, а «[OFFLINE]» на собеседнике,
 * про которого VK ничего не присылал, — вранье.
 */
export function sessionHeadText(dialog, count, presence, nowSecs) {
    const p = sessionHeadParts(dialog, count, presence, nowSecs);
    return p.nick ? `${p.name} ${p.nick}${p.tail}` : `${p.name}${p.tail}`;
}

/**
 * Правая ячейка статусбара: сколько собеседников в сети.
 *
 * Пустая строка, если сеть присутствия ещё не наполнилась: «0 ONLINE» на
 * свежеоткрытой вкладке — это не «никого нет», а «VK ещё ничего не присылал».
 */
export function onlineCounterText(online, known) {
    const k = Math.max(0, Number(known) || 0);
    if (k === 0) return '';
    const n = Math.max(0, Number(online) || 0);
    return `${n} ONLINE`;
}

/**
 * Ник для строки лога.
 *
 * Своё сообщение — всегда `you@local`. Для чужого берётся screen_name, затем
 * имя, затем id, затем — название беседы (сообщения от имени сообщества
 * приходят с отрицательным from_id).
 */
export function nickOf(msg, dialog) {
    if (msg && msg.out) return 'you@local';
    const s = msg && msg.sender;
    if (s) {
        if (s.screen_name) return String(s.screen_name);
        if (s.first_name) return String(s.first_name);
        if (s.id) return String(s.id);
    }
    if (dialog && dialog.title) return String(dialog.title);
    return 'unknown';
}

/** Обрезка текста для тоста/превью с многоточием. */
export function truncate(text, max) {
    const s = String(text == null ? '' : text).replace(/\s+/g, ' ').trim();
    const n = Math.max(1, Number(max) || 80);
    if (s.length <= n) return s;
    // Режем по code points: slice() по UTF-16-юнитам разрубает суррогатную
    // пару, и от emoji в тосте остаётся одинокий суррогат «�».
    const points = Array.from(s);
    if (points.length <= n) return s;
    return points.slice(0, n - 1).join('') + '…';
}

// ---------------------------------------------------------------------------
// Лента.
// ---------------------------------------------------------------------------

/**
 * Вставляет сообщение в список, отсортированный по возрастанию id.
 *
 * `inserted === false` означает «такой id уже есть» — ровно тот случай, когда
 * своё же отправленное сообщение приходит ВТОРОЙ раз через WS `new_message`.
 *
 * @param {Array<object>} list — отсортирован по возрастанию id
 * @param {object} msg
 * @returns {{list: Array<object>, inserted: boolean, index: number}}
 */
export function insertMessage(list, msg) {
    const out = Array.isArray(list) ? list : [];
    if (!msg || !Number.isFinite(msg.id)) return { list: out, inserted: false, index: -1 };
    // Поиск с конца: в 99% случаев сообщение свежее всех — это O(1).
    for (let i = out.length - 1; i >= 0; i -= 1) {
        if (out[i].id === msg.id) return { list: out, inserted: false, index: i };
        if (out[i].id < msg.id) {
            out.splice(i + 1, 0, msg);
            return { list: out, inserted: true, index: i + 1 };
        }
    }
    out.unshift(msg);
    return { list: out, inserted: true, index: 0 };
}

/**
 * Приклеивает страницу истории В НАЧАЛО, отбрасывая пересечения.
 *
 * Сервер отдаёт страницы без пересечений, но защита нужна: подгрузка могла
 * стартовать дважды (быстрый скролл), и без фильтра лента получила бы дубликаты.
 */
export function prependMessages(older, list) {
    const current = Array.isArray(list) ? list : [];
    const seen = new Set(current.map((m) => m.id));
    const fresh = (Array.isArray(older) ? older : []).filter(
        (m) => m && Number.isFinite(m.id) && !seen.has(m.id)
    );
    fresh.sort((a, b) => a.id - b.id);
    return fresh.concat(current);
}

/**
 * Минимальный id в ленте — курсор `before_id` для подгрузки вверх.
 *
 * ИМЕННО `message.id`, а НЕ `cmid`. Соблазн пагинировать по
 * `conversation_message_id` понятен (он «внутри беседы» и выглядит логичнее),
 * но параметр VK называется `start_message_id` и принимает глобальный id
 * сообщения: подстановка туда cmid дала бы страницу из чужого места истории
 * либо пустоту. `cmid` в этом плагине адресует ОТВЕТЫ (Ф6), а не страницы.
 *
 * @param {Array<object>} list
 * @returns {?number}
 */
export function oldestId(list) {
    const arr = Array.isArray(list) ? list : [];
    let min = null;
    for (const m of arr) {
        if (!m || !Number.isFinite(m.id)) continue;
        if (min === null || m.id < min) min = m.id;
    }
    return min;
}

/**
 * `conversation_message_id` сообщения или `null`.
 *
 * Вторая нумерация VK — внутри беседы. Нужна ответам (Ф6) и будущим
 * edit/delete в беседах; первичным идентификатором остаётся `msg.id`.
 */
export function cmidOf(msg) {
    const n = Number(msg && msg.cmid);
    return Number.isFinite(n) && n > 0 ? n : null;
}

/** Минимальный cmid в ленте (для операций, адресуемых внутри беседы). */
export function oldestCmid(list) {
    let min = null;
    for (const m of Array.isArray(list) ? list : []) {
        const c = cmidOf(m);
        if (c === null) continue;
        if (min === null || c < min) min = c;
    }
    return min;
}

/** Прижата ли лента к низу (в пределах порога). */
export function isAtBottom(m, threshold) {
    if (!m) return true;
    const gap = m.scrollHeight - m.scrollTop - m.clientHeight;
    return gap <= (Number.isFinite(threshold) ? threshold : 40);
}

/** Пора ли подгружать историю вверх. */
export function shouldLoadOlder(m, st) {
    if (!m || !st || !st.hasMore || st.loading) return false;
    return m.scrollTop < (Number.isFinite(st.threshold) ? st.threshold : 200);
}

/**
 * Пропускать ли тост по троттлингу «не чаще одного на беседу за 5с».
 *
 * @returns {boolean} true ⇒ тост показывать МОЖНО
 */
export function toastAllowed(lastAtMs, nowMs, windowMs) {
    const w = Number.isFinite(windowMs) ? windowMs : 5000;
    const last = Number(lastAtMs) || 0;
    return !last || nowMs - last >= w;
}

/**
 * Окно троттлинга исходящего «печатает…», мс.
 *
 * VK держит статус около 10 секунд сам, поэтому чаще одного запроса в 8с слать
 * нечего. Число заметно больше телеграмных 4с не по вкусу, а по бюджету: лимит
 * 3 rps на токен, и набор длинного сообщения не имеет права съесть его на
 * индикатор, который собеседник всё равно видит непрерывно.
 */
export const TYPING_THROTTLE_MS = 8000;

/**
 * Через сколько гаснет ЧУЖОЙ индикатор набора, мс.
 *
 * Сброс по таймеру обязателен: VK НЕ присылает «перестал печатать», он просто
 * перестаёт слать событие. Без таймера индикатор залипал бы навсегда.
 */
export const TYPING_RESET_MS = 10000;

/**
 * Пора ли отправить «печатает…» серверу.
 *
 * Смена беседы сбрасывает троттлинг: собеседник в НОВОЙ беседе обязан увидеть
 * индикатор сразу, а не через остаток чужого окна.
 */
export function typingAllowed(lastAtMs, nowMs, sameDialog, windowMs) {
    if (!sameDialog) return true;
    const w = Number.isFinite(windowMs) ? windowMs : TYPING_THROTTLE_MS;
    const last = Number(lastAtMs) || 0;
    return !last || (Number(nowMs) || 0) - last >= w;
}

/**
 * Пауза перед отправкой отметки о прочтении, мс.
 *
 * За это время в открытую беседу успевает прилететь пачка сообщений, и вместо
 * двадцати запросов уходит один — с максимальным `message_id`.
 */
export const READ_DEBOUNCE_MS = 800;

/**
 * Можно ли сейчас отмечать сообщения прочитанными.
 *
 * Оба условия обязательны: вкладка браузера видима И панель VK открыта. Иначе
 * сообщения «прочитались» бы в фоне — пользователь ушёл в терминал, а
 * собеседник видит, что его прочли, и ждёт ответа.
 */
export function readAllowed(visibilityState, panelHidden) {
    return String(visibilityState) === 'visible' && !panelHidden;
}

/**
 * Строка индикатора набора под лентой.
 *
 * Один печатающий — «is», несколько — «are»; больше трёх имён не показываем,
 * остальные сворачиваются в «+N».
 */
export function typingLine(names, max) {
    const list = (Array.isArray(names) ? names : [])
        .map((n) => String(n == null ? '' : n).trim())
        .filter(Boolean);
    if (!list.length) return '';
    const cap = Math.max(1, Number(max) || 3);
    const shown = list.slice(0, cap);
    const rest = list.length - shown.length;
    const who = rest > 0 ? `${shown.join(', ')} +${rest}` : shown.join(', ');
    return `>> ${who} ${list.length === 1 ? 'is' : 'are'} typing…`;
}

/**
 * Оценка сетевой задержки по интервалу между серверными ping'ами.
 *
 * Полноценный RTT замерить нельзя: сервер не эхом отвечает на наш pong, а шлёт
 * ping по своему таймеру. Зато ДРЕЙФ интервала — это как раз задержка доставки.
 * Первый ping эталона не имеет, поэтому `null`.
 */
export function latencyFromPingDrift(prevPingAtMs, nowMs, intervalMs) {
    const prev = Number(prevPingAtMs) || 0;
    if (!prev) return null;
    const drift = nowMs - prev - (Number(intervalMs) || 0);
    if (!Number.isFinite(drift)) return null;
    // Полминуты дрейфа — это не задержка, а спящая вкладка/подвисший таймер.
    if (drift > 30000) return null;
    return Math.max(0, Math.round(drift));
}

/** Сколько последних замеров усредняется для LAT. */
export const LAT_WINDOW = 5;

/**
 * Сглаженная задержка: среднее по окну замеров.
 *
 * Сырое значение мигает на десятки миллисекунд (планировщик браузера, GC), и
 * пользователь читает это как «связь скачет».
 */
export function smoothedLatency(samples) {
    if (!Array.isArray(samples)) return null;
    const valid = samples.filter((n) => Number.isFinite(n));
    if (valid.length === 0) return null;
    return Math.round(valid.reduce((acc, n) => acc + n, 0) / valid.length);
}

// ---------------------------------------------------------------------------
// Медиа: чистая логика карточек (сам рендер — js/vk/media.js, Ф7).
// ---------------------------------------------------------------------------

/** Порядок единиц для formatBytes. */
const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/**
 * Человекочитаемый размер: `512 B`, `840 KB`, `1.2 MB`.
 *
 * Десятичные единицы (1000, не 1024) — как показывает сам VK: пользователь
 * сравнивает цифру с тем, что видит в мобильном клиенте, а не с `ls -l`.
 */
export function formatBytes(bytes) {
    // `Number(null)` — это 0, поэтому «размер не пришёл» проверяется ДО
    // приведения: иначе отсутствующий size рисовался бы честным «0 B».
    if (bytes === null || bytes === undefined || bytes === '') return '';
    const n = Number(bytes);
    if (!Number.isFinite(n) || n < 0) return '';
    if (n < 1000) return `${Math.round(n)} B`;
    let v = n;
    let i = 0;
    while (v >= 1000 && i < BYTE_UNITS.length - 1) {
        v /= 1000;
        i += 1;
    }
    let digits = v < 10 ? 1 : 0;
    // 999 950 Б округлилось бы в «1000 KB»: toFixed() довершает перекат через
    // тысячу уже ПОСЛЕ выбора единицы — переносим его в следующую вручную.
    if (Number(v.toFixed(digits)) >= 1000 && i < BYTE_UNITS.length - 1) {
        v /= 1000;
        i += 1;
        digits = v < 10 ? 1 : 0;
    }
    return `${v.toFixed(digits)} ${BYTE_UNITS[i]}`;
}

/** Длительность как `M:SS` (или `H:MM:SS` для длинного видео). */
export function durationLabel(secs) {
    if (secs === null || secs === undefined || secs === '') return '';
    const n = Math.floor(Number(secs));
    if (!Number.isFinite(n) || n < 0) return '';
    const p = (v) => String(v).padStart(2, '0');
    const h = Math.floor(n / 3600);
    const m = Math.floor((n % 3600) / 60);
    const s = n % 60;
    return h > 0 ? `${h}:${p(m)}:${p(s)}` : `${m}:${p(s)}`;
}

/**
 * Ключ медиа в кэше objectURL'ов и в data-атрибутах карточки.
 *
 * ИНДЕКС ВЛОЖЕНИЯ — ОБЯЗАТЕЛЬНАЯ ЧАСТЬ КЛЮЧА. У сообщения VK до десяти
 * вложений; телеграмный ключ `dialog/message/kind` их не различает, и второе
 * фото затирало бы первое и в `vkState.mediaCache`, и в учёте objectURL (то
 * есть один блоб остался бы неотозванным навсегда).
 *
 * Формат совпадает с серверным `media_cache::CacheKey::id`
 * (`<peer>/<mid>/<idx>/<kind>`): ключи в логах фронта и бэкенда одинаковы, а
 * кадры `download_progress` сопоставляются с карточкой без преобразований.
 *
 * @param {string|number} dialogId
 * @param {number} messageId
 * @param {number} index — 0..9
 * @param {'thumb'|'full'} kind
 * @returns {string}
 */
export function mediaKey(dialogId, messageId, index, kind) {
    const idx = Number.isFinite(Number(index)) ? Math.max(0, Math.floor(Number(index))) : 0;
    return `${dialogId}/${messageId}/${idx}/${kind === 'full' ? 'full' : 'thumb'}`;
}

/** Расширение по MIME для сгенерированных имён файлов. */
const MIME_EXT = {
    'image/jpeg': 'jpg',
    'image/png': 'png',
    'image/gif': 'gif',
    'image/webp': 'webp',
    'video/mp4': 'mp4',
    'video/quicktime': 'mov',
    'video/webm': 'webm',
    'audio/ogg': 'ogg',
    'audio/mpeg': 'mp3',
    'audio/mp4': 'm4a',
    'application/pdf': 'pdf',
    'application/zip': 'zip',
};

/**
 * Расширение по умолчанию для вида вложения VK (когда MIME не пришёл).
 *
 * Отличия от Telegram: у VK есть `graffiti` (png на прозрачном фоне), а
 * голосовое приезжает в ogg — тот же контейнер, но добывается из документа
 * особого типа.
 */
const KIND_EXT = {
    photo: 'jpg',
    video: 'mp4',
    voice: 'ogg',
    audio: 'mp3',
    sticker: 'png',
    graffiti: 'png',
};

/**
 * Имя файла для карточки и для SAVE.
 *
 * Принимает ВЛОЖЕНИЕ, а не сообщение: у сообщения VK их до десяти, и
 * «имя файла сообщения» — бессмысленное понятие. Индекс попадает в
 * сгенерированное имя, иначе два фото одного сообщения сохранились бы под
 * одним именем и второе перезаписало бы первое.
 *
 * @param {object} msg — MessageDto
 * @param {object} att — элемент msg.attachments
 * @returns {string}
 */
export function mediaFileName(msg, att) {
    const media = att || (msg && msg.media) || {};
    const given = typeof media.file_name === 'string' ? media.file_name.trim() : '';
    if (given) return given;
    const kind = String(media.kind || 'file');
    const ext = MIME_EXT[String(media.mime || '')] || KIND_EXT[kind] || 'bin';
    const id = Number.isFinite(msg && msg.id) ? msg.id : 0;
    const idx = Number.isFinite(Number(media.index)) ? Number(media.index) : 0;
    return `${kind}_${id}_${idx}.${ext}`;
}

/**
 * Имя для сохранения на диск.
 *
 * Санитизация обязательна: имя приходит из VK, то есть от чужого аккаунта, а
 * атрибут `download` у `<a>` браузеры понимают как подсказку пути.
 */
export function sanitizeDownloadName(name, fallback) {
    const fb = fallback || 'download.bin';
    const raw = String(name == null ? '' : name);
    // Только последний сегмент — и по '/', и по '\' (Windows-имена).
    const tail = raw.split(/[\\/]/).pop() || '';
    const cleaned = tail
        // C0-, C1-контролы и спецсимволы ФС.
        .replace(/[\x00-\x1f\x7f-\x9f<>:"|?*]/g, '_')
        // Bidi-override и невидимые: U+202E разворачивает хвост строки, и
        // «invoice‮fdp.exe» читается глазами как «invoiceexe.pdf», оставаясь
        // исполняемым .exe — классический спуфинг расширения.
        .replace(/[\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g, '')
        .replace(/^\.+/, '')
        .trim();
    return capUtf8Bytes(cleaned, 255) || fb;
}

/**
 * Обрезает строку до лимита в БАЙТАХ UTF-8, не разрывая code points и сохраняя
 * расширение: файловые системы ограничивают имя 255 байтами.
 */
function capUtf8Bytes(name, maxBytes) {
    const enc = new TextEncoder();
    if (enc.encode(name).length <= maxBytes) return name;
    const dot = name.lastIndexOf('.');
    const ext = dot > 0 && enc.encode(name.slice(dot)).length <= 16 ? name.slice(dot) : '';
    const budget = maxBytes - enc.encode(ext).length;
    let stem = '';
    let used = 0;
    for (const ch of ext ? name.slice(0, dot) : name) {
        const b = enc.encode(ch).length;
        if (used + b > budget) break;
        stem += ch;
        used += b;
    }
    return stem + ext;
}

/**
 * Подпись вида вложения для шапки карточки.
 *
 * Словарь ВИДОВ VK: к телеграмному набору добавились `graffiti` и
 * `unsupported` (тип вложения из будущего — карточка обязана нарисоваться
 * отключённой, а не исчезнуть).
 */
export function mediaKindLabel(kind) {
    const map = {
        photo: 'PHOTO',
        video: 'VIDEO',
        document: 'FILE',
        sticker: 'STICKER',
        graffiti: 'GRAFFITI',
        voice: 'VOICE',
        audio: 'AUDIO',
        other: 'DATA',
        unsupported: 'UNKNOWN',
    };
    return map[String(kind || '').toLowerCase()] || 'DATA';
}

/** Глиф вида вложения (терминальная стилистика, без цветных emoji). */
export function mediaGlyph(kind) {
    const map = {
        photo: '▣',
        video: '▶',
        document: '▤',
        sticker: '◆',
        graffiti: '✎',
        voice: '∿',
        audio: '♪',
        other: '◌',
        unsupported: '?',
    };
    return map[String(kind || '').toLowerCase()] || '◌';
}

/** Строка метаданных под именем файла: `1280×720 · 1.2 MB`, `0:07 · 24 KB`. */
export function mediaMetaLine(media) {
    const m = media || {};
    const parts = [];
    if (Number.isFinite(m.width) && Number.isFinite(m.height) && m.width > 0 && m.height > 0) {
        parts.push(`${m.width}×${m.height}`);
    }
    const dur = durationLabel(m.duration_secs);
    if (dur) parts.push(dur);
    const size = formatBytes(m.size);
    if (size) parts.push(size);
    return parts.join(' · ');
}

/**
 * Процент загрузки или `null`, если общий размер неизвестен.
 *
 * Промежуточные значения ограничены 99: «100 %» имеет право появиться только
 * вместе с финальным событием.
 */
export function progressPercent(received, total) {
    const r = Number(received);
    const t = Number(total);
    if (!Number.isFinite(r) || r < 0) return null;
    if (!Number.isFinite(t) || t <= 0) return null;
    if (r >= t) return 100;
    return Math.max(0, Math.min(99, Math.floor((r / t) * 100)));
}

/** Подпись прогресса: `49%`, либо принятый объём, если размер неизвестен. */
export function progressLabel(received, total) {
    const pct = progressPercent(received, total);
    if (pct !== null) return `${pct}%`;
    const got = formatBytes(received);
    return got ? `${got}…` : '…';
}

/**
 * Стоит ли пытаться грузить превью.
 *
 * `has_thumb === false` ⇒ сразу generic-иконка: запрос всё равно вернул бы
 * `404 no_media`.
 *
 * ============================================================================
 * ПОЧЕМУ ЗДЕСЬ БОЛЬШЕ НЕТ БЕЛОГО СПИСКА ВИДОВ
 * ============================================================================
 *
 * Раньше функция читалась так:
 *
 *     if (!m.has_thumb) return false;
 *     return ['photo','video','document','sticker','graffiti'].includes(m.kind);
 *
 * — то есть поверх серверного `has_thumb` стоял ВТОРОЙ, зашитый в браузер
 * список того, у кого превью «бывает». Ровно этот список и был дефектом:
 * `other` в него не попал, поэтому карточка ссылки с превью 1200×630 никогда
 * не делала запрос вовсе и вечно показывала значок-заглушку. Снаружи это
 * выглядело как «превью ссылок не работают».
 *
 * Дублировать серверное знание было нечем оправдать и до дефекта, а теперь
 * тем более: бэкенд считает `has_thumb` как `attach::url_for(att, Thumb)
 * .is_some()` — тем же кодом, который потом ищет адрес для скачивания (см.
 * `plugins/vk/src/attach.rs::media_from_attachment`). Обещание и способность
 * его выполнить там неразделимы по построению, значит верить флагу можно.
 *
 * НЕ добавляй сюда «страховочный» перечень видов: он снова разъедется с
 * сервером — теперь уже в другую сторону (сервер сможет отдать превью, а
 * браузер откажется его просить).
 */
export function canShowThumb(media) {
    return (media || {}).has_thumb === true;
}

/** Воспроизводимое ли вложение (для него OPEN означает «включить плеер»). */
export function isPlayableKind(kind) {
    return ['voice', 'audio'].includes(String(kind || '').toLowerCase());
}

/** Открывается ли вложение в лайтбоксе (полноразмерная картинка). */
export function isImageKind(kind) {
    return ['photo', 'sticker', 'graffiti'].includes(String(kind || '').toLowerCase());
}

/**
 * Схемы, которые разрешено открывать из панели.
 *
 * `vk:` оставлена намеренно — это deep-link в приложение VK (профиль, беседа,
 * запись), браузер отдаст его установленному клиенту. Всё остальное —
 * `javascript:`, `data:`, `blob:`, `file:`, `vbscript:` — запрещено.
 */
const ALLOWED_SCHEMES = new Set(['http', 'https', 'vk']);

/**
 * Проверяет внешний URL и возвращает его же, если открывать безопасно.
 *
 * Порядок операций критичен:
 *   1. вырезать управляющие и невидимые символы — браузер их игнорирует при
 *      разборе схемы, и `java\0script:alert(1)` для него `javascript:`;
 *   2. только потом разбирать схему регуляркой.
 * Обратный порядок даёт обход белого списка одним нулевым байтом.
 *
 * Относительные ссылки (`/x`, `//evil.com`) отвергаются: внешний адрес из DTO
 * всегда абсолютный, а `//evil.com` в `window.open` — переход на чужой хост по
 * текущей схеме.
 *
 * ЧТО ЭТО НЕ ОТМЕНЯЕТ: медиа VK через `safeUrl` не ходят вовсе. Байты
 * вложений отдаёт только прокси бэкенда (`/api/vk/media/...`); сюда попадают
 * ровно два внешних адреса, которые бэкенд отдаёт осознанно: `player_url`
 * видео (страница плеера, а не файл) и `link_url` вложения `link` (адрес,
 * который прислал собеседник).
 *
 * @param {*} raw
 * @returns {?string} нормализованный URL или null
 */
export function safeUrl(raw) {
    if (typeof raw !== 'string') return null;
    const clean = raw
        .replace(/[\u0000-\u0020\u007F-\u00A0\u200B-\u200F\u2028\u2029\u202A-\u202E\u2066-\u2069\uFEFF]/g, '')
        .trim();
    if (!clean) return null;
    const m = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(clean);
    if (!m) return null;
    const scheme = m[1].toLowerCase();
    if (!ALLOWED_SCHEMES.has(scheme)) return null;
    // http/https без authority («https:evil») браузеры разбирают
    // непредсказуемо — требуем канонический вид с непустым хостом.
    if ((scheme === 'http' || scheme === 'https') && !/^https?:\/\/[^/?#\s]+/i.test(clean)) {
        return null;
    }
    return clean;
}

/**
 * То же, что [safeUrl], но БЕЗ `vk:` — только `http`/`https`.
 *
 * Отдельная функция, а не параметр `safeUrl`, потому что случаи разные по
 * смыслу. `vk:` — это deep-link в приложение VK, и он уместен там, где адрес
 * построили мы сами (кнопка клавиатуры бота, страница плеера). Адрес вложения
 * `link` набрал СОБЕСЕДНИК: там `vk://` не «удобный переход в приложение», а
 * повод разбираться, что именно откроет установленный клиент.
 *
 * Через эту функцию проходит всё, что становится `href` карточки ссылки, —
 * второй рубеж поверх серверного `attach::http_url`. Рубежа именно два: у
 * бэкенда и фронта разные поводы ошибиться, и совпасть эти ошибки не обязаны.
 *
 * @param {*} raw
 * @returns {?string}
 */
export function webUrl(raw) {
    const url = safeUrl(raw);
    if (!url) return null;
    return /^https?:\/\//i.test(url) ? url : null;
}

/**
 * Домен ссылки для показа на карточке: `psph.bitrix24.ru`.
 *
 * На карточке рисуется ИМЕННО домен, а не полный адрес: у ссылок из CRM,
 * маркетплейсов и трекеров путь легко перерастает ширину панели, и карточка
 * либо расползается, либо обрезается посередине идентификатора — то есть
 * показывает мусор. Домен отвечает на единственный вопрос, который у
 * пользователя есть ДО клика: «куда это ведёт».
 *
 * `www.` снимается — он ничего не сообщает и съедает четыре символа.
 * Punycode НЕ разворачивается: `xn--80ak6aa92e.com` обязан выглядеть как
 * `xn--…`, иначе домен-омограф выдал бы себя за настоящий прямо в подписи.
 *
 * @param {*} raw
 * @returns {?string} домен или null, если ссылку открывать нельзя
 */
export function linkDomain(raw) {
    const url = webUrl(raw);
    if (!url) return null;
    let host = '';
    try {
        host = new URL(url).hostname;
    } catch (_) {
        // Строку уже проверил webUrl; сюда попадают только экзотические формы
        // authority, которые всё равно нечего показывать.
        return null;
    }
    host = String(host || '').toLowerCase().replace(/^www\./, '');
    return host || null;
}

// ---------------------------------------------------------------------------
// Отправка файлов (Ф8) — чистая логика.
// ---------------------------------------------------------------------------

/**
 * Клиентские пределы размера ПО ВИДАМ ВЛОЖЕНИЯ, байты.
 *
 * У Telegram предел один на всё (`MAX_UPLOAD_BYTES`), у VK он зависит от того,
 * каким методом файл уходит: фотография сжимается и лимитирована жёстче
 * документа, а голосовое вообще другой конвейер. Одно число здесь означало бы
 * либо ложный отказ на документе, либо пропущенный отказ на фото — то есть
 * бессмысленную загрузку с последующей ошибкой VK.
 *
 * Источник истины — сервер (`413 too_large` по `Content-Length`, ещё не читая
 * тело); эти цифры нужны только чтобы не гнать заведомо отвергнутое по сети.
 */
export const UPLOAD_LIMITS = {
    photo: 50 * 1000 * 1000,
    doc: 200 * 1000 * 1000,
    video: 2 * 1000 * 1000 * 1000,
    audio_message: 200 * 1000 * 1000,
};

/** Расширение → MIME для файлов, у которых браузер не сообщил тип. */
const EXT_MIME = {
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    png: 'image/png',
    gif: 'image/gif',
    webp: 'image/webp',
    heic: 'image/heic',
    mp4: 'video/mp4',
    mov: 'video/quicktime',
    webm: 'video/webm',
    mkv: 'video/x-matroska',
    mp3: 'audio/mpeg',
    ogg: 'audio/ogg',
    oga: 'audio/ogg',
    m4a: 'audio/mp4',
    wav: 'audio/wav',
    flac: 'audio/flac',
    pdf: 'application/pdf',
    zip: 'application/zip',
    txt: 'text/plain',
};

/**
 * MIME файла: то, что сказал браузер, иначе догадка по расширению.
 *
 * `File.type` бывает пустым (файл без расширения, редкий формат, drag&drop из
 * некоторых приложений), а вид карточки и ветка конвейера выбираются по MIME.
 */
export function uploadMime(name, type) {
    const given = String(type || '').trim().toLowerCase();
    if (given) return given;
    const ext = String(name || '').split('.').pop();
    if (!ext || ext === name) return 'application/octet-stream';
    return EXT_MIME[ext.toLowerCase()] || 'application/octet-stream';
}

/**
 * Вид ЗАГРУЗКИ по MIME — ключ карты [`UPLOAD_LIMITS`], то есть ветка
 * трёхшагового конвейера VK, а не вид карточки.
 *
 * `image/gif` уходит ДОКУМЕНТОМ намеренно: как «фото» VK потерял бы анимацию.
 *
 * @param {string} mime
 * @returns {'photo'|'video'|'doc'|'audio_message'}
 */
export function uploadKind(mime) {
    const m = String(mime || '').toLowerCase();
    if (m === 'image/gif' || m === 'image/svg+xml') return 'doc';
    if (m.startsWith('image/')) return 'photo';
    if (m.startsWith('video/')) return 'video';
    return 'doc';
}

/**
 * Вид КАРТОЧКИ по ветке конвейера.
 *
 * Две разные системы имён встречаются здесь ровно один раз: [`uploadKind`]
 * отвечает на вопрос «каким методом файл уедет в VK» (и по нему выбирается
 * лимит), а `mediaGlyph`/`mediaKindLabel` рисуют карточку и знают словарь
 * ПОЛУЧЕННЫХ вложений (`photo`/`video`/`document`/`voice`). Без этого
 * перевода документ показывался бы глифом «◌ DATA» — то есть «мы не поняли,
 * что это», хотя всё понято.
 *
 * @param {string} kind — ключ UPLOAD_LIMITS
 * @returns {'photo'|'video'|'document'|'voice'}
 */
export function uploadCardKind(kind) {
    switch (String(kind || '')) {
        case 'photo': return 'photo';
        case 'video': return 'video';
        case 'audio_message': return 'voice';
        default: return 'document';
    }
}

/**
 * Имя для картинки, вставленной из буфера обмена.
 *
 * У скриншота имени нет вовсе, поэтому оно генерируется из времени: файлы не
 * перезатирают друг друга, и видно, когда сделан снимок.
 */
export function pastedFileName(mime, nowMs) {
    const ms = Number.isFinite(nowMs) ? nowMs : Date.now();
    const d = new Date(ms);
    const p = (v) => String(v).padStart(2, '0');
    const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}`
        + `_${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
    const sub = String(mime || '').toLowerCase().split('/')[1] || 'png';
    const ext = sub.split('+')[0].replace(/[^a-z0-9]/g, '') || 'png';
    return `pasted_${stamp}.${ext}`;
}

/**
 * Подпись состояния карточки отправки.
 *
 * Этапы названы по-разному не для красоты: «LOCAL 100 %» — это ещё не
 * отправлено, файл только дошёл до devforge, а дальше идёт вторая (обычно
 * более медленная) нога до VK.
 */
export function uploadStageLabel(stage, sent, total) {
    switch (String(stage)) {
        case 'queued': return 'QUEUED';
        case 'local': return `LOCAL ${progressLabel(sent, total)}`;
        case 'vk': return `VK ${progressLabel(sent, total)}`;
        case 'sent': return '✓ SENT';
        case 'failed': return '!! FAILED';
        default: return '';
    }
}

/** Позиция в очереди: `QUEUED 2/3`. */
export function uploadQueueLabel(position, total) {
    const p = Math.max(1, Number(position) || 1);
    const t = Math.max(p, Number(total) || p);
    return t > 1 ? `QUEUED ${p}/${t}` : 'QUEUED';
}

/**
 * Причина отказа ДО сетевого запроса или `null`, если файл можно отправлять.
 *
 * Лимит выбирается ПО ВИДУ загрузки: см. [`UPLOAD_LIMITS`]. Пустой файл
 * отсекается здесь же — VK отвергает нулевой размер, и честнее сказать это
 * сразу.
 *
 * @param {{size?: number, name?: string, type?: string}} file
 * @param {string} [kind] — ключ UPLOAD_LIMITS; по умолчанию выводится из файла
 * @param {object} [limits=UPLOAD_LIMITS]
 * @returns {?string}
 */
export function uploadRejectReason(file, kind, limits) {
    const table = (limits && typeof limits === 'object') ? limits : UPLOAD_LIMITS;
    const k = kind || uploadKind(uploadMime(file && file.name, file && file.type));
    const max = Number.isFinite(table[k]) && table[k] > 0 ? table[k] : UPLOAD_LIMITS.doc;
    const size = Number(file && file.size);
    if (!Number.isFinite(size)) return 'размер файла неизвестен';
    if (size <= 0) return 'файл пустой';
    if (size > max) return `файл ${formatBytes(size)} больше лимита ${formatBytes(max)} для «${k}»`;
    return null;
}

/**
 * Разбирает тело ошибки плагина в единый вид.
 *
 * Тот же контракт, что в api.js (`{error:{code,message,retry_after_secs}}`), но
 * для XHR: `xhr.responseText` — строка, а решения принимаются по КОДУ.
 * Невалидный JSON — норма (ответ прокси, 502 от nginx), тогда остаётся статус.
 */
export function parseApiError(status, body) {
    let parsed = null;
    try {
        parsed = JSON.parse(String(body || ''));
    } catch (_) {
        parsed = null;
    }
    const e = (parsed && typeof parsed === 'object' && parsed.error) ? parsed.error : null;
    return {
        status: Number(status) || 0,
        code: (e && typeof e.code === 'string') ? e.code : null,
        message: (e && typeof e.message === 'string' && e.message)
            ? e.message
            : `HTTP ${Number(status) || 0}`,
        retryAfterSecs: (e && typeof e.retry_after_secs === 'number') ? e.retry_after_secs : null,
    };
}

// ---------------------------------------------------------------------------
// Captcha и коды VK — то, чего у Telegram не было вовсе.
// ---------------------------------------------------------------------------

/**
 * Достаёт реквизиты captcha из ошибки api.js.
 *
 * Возвращает `sid` и `imgUrl` — адрес НАШЕГО прокси, а не прямой `captcha_img`
 * с серверов VK: ставить чужой URL в `<img src>` значит отправить туда браузер
 * пользователя со всеми его cookie. То же правило, что для медиа.
 *
 * @param {?Error & {code?: string, captcha?: {sid: string}}} err
 * @returns {?{sid: string, imgUrl: string}}
 */
export function captchaFromError(err) {
    if (!err || err.code !== 'captcha_required') return null;
    const sid = err.captcha && err.captcha.sid
        ? String(err.captcha.sid)
        : String((err.body && err.body.error && err.body.error.captcha_sid) || '');
    if (!sid) return null;
    return { sid, imgUrl: `/api/vk/captcha/${encodeURIComponent(sid)}` };
}

/**
 * Тело повтора после ввода символов.
 *
 * Имена полей — СЕРВЕРНЫЕ (`captchaSid`/`captchaKey` превращает в
 * `captcha_sid`/`captcha_key` сам api.js): повтор после captcha — это ТОТ ЖЕ
 * запрос плюс два поля, и никакой отдельной ручки для него нет.
 *
 * @returns {?{captchaSid: string, captchaKey: string}} null, если половина пуста
 */
export function captchaRetryBody(sid, key) {
    const s = String(sid == null ? '' : sid).trim();
    const k = String(key == null ? '' : key).trim();
    // Половина ответа бесполезна: VK отвергнет её кодом 100, и пользователь
    // увидит «неверные параметры» вместо «введите символы ещё раз».
    if (!s || !k) return null;
    return { captchaSid: s, captchaKey: k };
}

/** Человеческий текст по числовому коду ошибки VK. */
const VK_ERROR_LABELS = {
    5: 'токен недействителен или отозван',
    6: 'слишком много запросов в секунду',
    9: 'слишком часто — VK включил flood control',
    10: 'внутренняя ошибка на стороне VK',
    14: 'нужно ввести символы с картинки',
    15: 'у токена нет права messages',
    17: 'VK требует подтверждения пользователя',
    18: 'страница удалена или заблокирована',
    29: 'исчерпан лимит метода',
    100: 'VK отверг один из параметров',
    113: 'неверный идентификатор пользователя',
    900: 'нельзя писать пользователю из чёрного списка',
    901: 'пользователь запретил сообщения',
    902: 'сообщения запрещены настройками приватности',
    913: 'слишком много пересланных сообщений',
    914: 'сообщение слишком длинное',
    917: 'нет доступа к этой беседе',
};

/**
 * Подпись ошибки VK по её числовому коду.
 *
 * Нужна там, где до фронта доезжает голый код (тексты VK приходят
 * по-английски и не всегда внятны). Незнакомый код отдаёт `VK ошибка N` — это
 * честнее выдуманной формулировки.
 *
 * @param {number|string} code
 * @returns {string}
 */
export function vkErrorLabel(code) {
    const n = Number(code);
    if (!Number.isFinite(n)) return '';
    return VK_ERROR_LABELS[n] || `VK ошибка ${n}`;
}
