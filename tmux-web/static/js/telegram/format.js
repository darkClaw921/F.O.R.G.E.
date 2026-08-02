// tmux-web — Telegram plugin: чистые функции форматирования и сортировки.
//
// Модуль СПЕЦИАЛЬНО не импортирует ни DOM, ни core/*: он грузится как есть в
// Node и покрыт регресс-тестом tmux-web/tests/frontend/telegram_format.test.js
// (`node <file>`). Всё, что можно проверить без браузера, живёт здесь;
// dialogs.js и chat.js остаются тонкими рисовальщиками.
//
// Инвариант id: dialog_id — СТРОКА (64-битные значения Telegram не влезают в
// Number), message_id — число. Сравнивать dialog_id можно только строками.

/** Сколько символов слага влезает в колонку контактов. */
export const SLUG_MAX = 24;

/** Суффикс слага по типу диалога: sofia@node, devs@group, news@chan. */
const KIND_SUFFIX = { user: 'node', group: 'group', channel: 'chan' };

/**
 * Терминальный слаг имени: `sofia@node`.
 *
 * Берётся @username (он уже в нужном алфавите), иначе title. Всё, что не
 * [a-z0-9_.-], сжимается в `_`; пустой результат заменяется на `anon`, чтобы
 * в списке не появлялись безымянные строки (у удалённых аккаунтов имени нет).
 *
 * @param {{username?: string, title?: string, kind?: string, id?: string}} d
 * @returns {string}
 */
export function slugName(d) {
    const raw = (d && (d.username || d.title)) || '';
    let slug = String(raw)
        .toLowerCase()
        .replace(/\s+/g, '_')
        .replace(/[^a-z0-9_.-]+/g, '_')
        .replace(/_{2,}/g, '_')
        .replace(/^[_.-]+|[_.-]+$/g, '');
    if (!slug) slug = 'anon';
    if (slug.length > SLUG_MAX) slug = slug.slice(0, SLUG_MAX);
    const suffix = KIND_SUFFIX[(d && d.kind) || 'user'] || 'node';
    return `${slug}@${suffix}`;
}

/**
 * Человеческий возраст события: `12m`, `3h`, `2d`.
 *
 * Секунды показываются как `now` — «0s назад» в терминальном UI читается
 * хуже, чем «сейчас».
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
 * Вторая строка элемента списка: статус диалога.
 *
 * - user: `[ONLINE]` / `[IDLE] last 3h` / `[OFFLINE]`;
 * - group/channel: `[GROUP] 142 msgs` / `[CHANNEL] 87 msgs`.
 *
 * «msgs» берётся из id последнего сообщения: в группах и каналах нумерация
 * начинается с 1 и монотонна, поэтому id последнего — честная оценка объёма.
 * В личных чатах id глобальны для аккаунта, и такой счётчик врал бы, поэтому
 * там его нет.
 *
 * @param {object} d — DialogDto
 * @param {{online?: boolean, wasOnline?: ?number}} [presence]
 * @param {number} [nowSecs] — для детерминизма в тестах
 * @returns {string}
 */
export function dialogStatusLine(d, presence, nowSecs) {
    const kind = (d && d.kind) || 'user';
    if (kind === 'group' || kind === 'channel') {
        const tag = kind === 'group' ? '[GROUP]' : '[CHANNEL]';
        const count = d && d.last_message && d.last_message.id > 0 ? d.last_message.id : 0;
        return count > 0 ? `${tag} ${count} msgs` : tag;
    }
    if (presence && presence.online) return '[ONLINE]';
    const was = presence && presence.wasOnline;
    if (Number.isFinite(was) && was > 0) {
        const now = Number.isFinite(nowSecs) ? nowSecs : Math.floor(Date.now() / 1000);
        return `[IDLE] last ${relativeAge(now - was)}`;
    }
    return '[OFFLINE]';
}

/**
 * Делит диалоги на «узлы» (люди + группы) и «каналы».
 *
 * Порядок внутри групп сохраняется как пришёл с сервера: Telegram уже отдал
 * закреплённые сверху, а пересортировка на фронте ломала бы это ожидание.
 *
 * @param {Array<object>} dialogs
 * @returns {{nodes: Array<object>, channels: Array<object>}}
 */
export function splitDialogs(dialogs) {
    const nodes = [];
    const channels = [];
    for (const d of Array.isArray(dialogs) ? dialogs : []) {
        if (!d || typeof d !== 'object') continue;
        if (d.kind === 'channel') channels.push(d);
        else nodes.push(d);
    }
    return { nodes, channels };
}

/**
 * Заголовок секции узлов: `// ~/inbox — 12 NODES`.
 *
 * @param {number} count
 * @param {string} [dirName='~/inbox'] — имя активной папки (`~/work`)
 * @returns {string}
 */
export function nodesTitle(count, dirName) {
    const n = Math.max(0, Number(count) || 0);
    const name = String(dirName || '') || '~/inbox';
    return `// ${name} — ${n} NODES`;
}

/**
 * Суммарное непрочитанное по немьюченным диалогам.
 *
 * @param {Array<object>} dialogs
 * @param {Set<string>|Array<string>} [muted] — id заглушённых диалогов
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
// Папки (// DIRECTORIES) и заглушённые диалоги.
//
// Чистая логика секции папок: слаги имён, принадлежность диалога папке,
// счётчики непрочитанного по папке и производный Set заглушённых. DOM-рендер
// живёт в dialogs.js, данные приходят из GET /api/telegram/folders и полей
// DialogDto muted/archived/folder_id.
// ---------------------------------------------------------------------------

/** Терминальная метка заглушённого диалога (без цветных emoji, как и глифы). */
export const MUTED_MARK = '[M]';

/**
 * Метка mute для строки списка: `[M]` либо пустая строка.
 *
 * @param {boolean} muted
 * @returns {string}
 */
export function mutedMark(muted) {
    return muted ? MUTED_MARK : '';
}

/** Терминальная метка диалога с ботом — в одном алфавите с [M]. */
export const BOT_MARK = '[BOT]';

/**
 * Метка бота для строки списка: `[BOT]` либо пустая строка.
 *
 * Бот и человек в Telegram — оба `kind === 'user'`, и без метки собеседника не
 * отличить: одинаковый слаг, одинаковая статус-строка. При этом папка «Боты»
 * показывает ровно их (см. [`dialogInFolder`]), и пользователь обязан видеть в
 * колонке ту же классификацию, по которой отфильтрован список.
 *
 * @param {boolean} bot — DialogDto.bot
 * @returns {string}
 */
export function botMark(bot) {
    return bot ? BOT_MARK : '';
}

/**
 * Set из id заглушённых диалогов — производная от поля `muted` DialogDto.
 *
 * ЕДИНСТВЕННЫЙ источник истины про mute — сам список диалогов (поле muted
 * приходит в /dialogs и патчится WS-событием `notify_settings`), поэтому Set
 * каждый раз пересобирается, а не хранится отдельно: две копии состояния
 * неизбежно разъехались бы.
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
 * Слаг имени папки для секции // DIRECTORIES: `работа`, `dev_chats`.
 *
 * Расширение slugName: юникод-буквы и цифры СОХРАНЯЮТСЯ (`\p{L}\p{N}`) —
 * папки у пользователей называются по-русски, и «работа» → `folder_2` было бы
 * потерей смысла. Emoji и прочие символы сжимаются в `_`. Пустой результат
 * (имя из одних emoji) заменяется на `folder_<id>`, чтобы папка не пропала.
 *
 * @param {{id?: number, title?: string}} folder
 * @returns {string}
 */
export function folderSlug(folder) {
    const raw = (folder && folder.title) || '';
    let slug = String(raw)
        .toLowerCase()
        .replace(/\s+/g, '_')
        .replace(/[^\p{L}\p{N}_.-]+/gu, '_')
        .replace(/_{2,}/g, '_')
        .replace(/^[_.-]+|[_.-]+$/g, '');
    if (!slug) slug = folder && folder.id != null ? `folder_${folder.id}` : 'folder';
    if (slug.length > SLUG_MAX) slug = slug.slice(0, SLUG_MAX);
    return slug;
}

/**
 * Слаги для СПИСКА папок с дедупликацией: два «Work» дают `work` и `work_2`.
 *
 * Зарезервированы `inbox` и `archive` — их занимают штатные пункты секции,
 * и папка с таким именем получает суффикс, а не сливается с ними визуально.
 *
 * @param {Array<object>} folders
 * @returns {Array<string>} слаги в порядке входного списка
 */
export function folderSlugs(folders) {
    const seen = new Set(['inbox', 'archive']);
    return (Array.isArray(folders) ? folders : []).map((f) => {
        const base = folderSlug(f);
        let slug = base;
        let n = 2;
        while (seen.has(slug)) {
            slug = `${base}_${n}`;
            n += 1;
        }
        seen.add(slug);
        return slug;
    });
}

/**
 * Модель секции // DIRECTORIES: `~/inbox`, пользовательские папки в порядке
 * Telegram, затем `~/archive`.
 *
 * @param {Array<object>} folders — из GET /api/telegram/folders
 * @returns {Array<{key: string, name: string, emoticon: string, folder: ?object}>}
 *   key — 'inbox' | 'archive' | 'folder:<id>' (кладётся в data-dir)
 */
export function dirEntries(folders) {
    const list = (Array.isArray(folders) ? folders : [])
        .filter((f) => f && typeof f === 'object');
    const slugs = folderSlugs(list);
    const entries = [{ key: 'inbox', name: '~/inbox', emoticon: '', folder: null }];
    list.forEach((f, i) => entries.push({
        key: `folder:${f.id}`,
        name: `~/${slugs[i]}`,
        emoticon: typeof f.emoticon === 'string' ? f.emoticon : '',
        folder: f,
    }));
    entries.push({ key: 'archive', name: '~/archive', emoticon: '', folder: null });
    return entries;
}

/**
 * Категория диалога в терминах флагов папки Telegram.
 *
 * Возвращает ровно ОДНО значение — категории у Telegram взаимоисключающие, и
 * бот, сидящий в контактах, относится к `bot`, а не к `contact` (в TDLib
 * проверка `is_user_bot` стоит раньше `is_user_contact`).
 *
 * `'user'` — это НЕ категория, а «сервер не сказал»: DialogDto без полей
 * bot/contact приходит от плагина версии до D3. Для такого диалога
 * классификация невозможна, и [`dialogInFolder`] деградирует в прежнее
 * поведение (см. там).
 *
 * @param {object} d — DialogDto
 * @returns {'group'|'channel'|'bot'|'contact'|'non_contact'|'user'}
 */
export function dialogCategory(d) {
    if (!d || typeof d !== 'object') return 'user';
    const kind = d.kind || 'user';
    if (kind === 'group') return 'group';
    if (kind === 'channel') return 'channel';
    // Старый сервер: полей нет вовсе. Отличать «нет поля» от `false` обязательно —
    // иначе каждый человек и каждый бот уехали бы в non_contacts.
    if (typeof d.bot !== 'boolean' && typeof d.contact !== 'boolean') return 'user';
    if (d.bot === true) return 'bot';
    return d.contact === true ? 'contact' : 'non_contact';
}

/**
 * Совпала ли категория диалога с категорийными флагами папки.
 *
 * @param {string} category — результат [`dialogCategory`]
 * @param {object} folder
 * @returns {boolean}
 */
function categoryMatches(category, folder) {
    switch (category) {
        case 'group': return !!folder.groups;
        case 'channel': return !!folder.broadcasts;
        case 'bot': return !!folder.bots;
        case 'contact': return !!folder.contacts;
        case 'non_contact': return !!folder.non_contacts;
        // Категория неизвестна (DTO старого сервера): любой user-флаг ловит
        // такой диалог. Показать лишнего в папке безопаснее, чем спрятать
        // переписку, — пропажа чата выглядит как потеря данных.
        default: return !!(folder.contacts || folder.non_contacts || folder.bots);
    }
}

/**
 * Принадлежит ли диалог ПОЛЬЗОВАТЕЛЬСКОЙ папке Telegram.
 *
 * Порядок правил (семантика Telegram):
 *
 * 1. `exclude_ids` — абсолютное исключение, сильнее include: в Telegram чат
 *    не может быть в обоих списках, но при рассинхроне безопаснее скрыть;
 * 2. `pinned_ids` ∪ `include_ids` входят ВСЕГДА — минуя и категории, и
 *    exclude_*-флаги (явно добавленный muted-чат остаётся в папке);
 * 3. флаги категорий — ровно ОДНА категория на диалог (см. [`dialogCategory`]):
 *    group → `groups`, channel → `broadcasts`, бот → `bots` (и только он,
 *    даже если бот в контактах), человек в контактах → `contacts`, остальные
 *    люди → `non_contacts`;
 * 4. exclude_muted/exclude_read/exclude_archived убирают из категорийных
 *    совпадений muted, прочитанные (unread_count == 0) и архивные.
 *
 * СОВМЕСТИМОСТЬ: DialogDto без полей `bot`/`contact` (плагин версии до D3,
 * закешированный фронт против старого сервера) классифицировать нельзя,
 * поэтому для него сохраняется прежнее поведение — user-диалог подпадает под
 * ЛЮБОЙ из флагов contacts/non_contacts/bots. Как только сервер начинает
 * присылать флаги, папка «Боты» показывает ботов, а «Контакты» — людей.
 *
 * Общая папка-ссылка (Chatlist) приходит со всеми флагами false и работает
 * чисто по include/pinned — это покрывается правилом 2.
 *
 * @param {object} d — DialogDto
 * @param {object} folder — папка из GET /api/telegram/folders
 * @returns {boolean}
 */
export function dialogInFolder(d, folder) {
    if (!d || typeof d !== 'object' || !folder || typeof folder !== 'object') return false;
    const id = String(d.id);
    const has = (arr) => Array.isArray(arr) && arr.some((x) => String(x) === id);
    if (has(folder.exclude_ids)) return false;
    if (has(folder.pinned_ids) || has(folder.include_ids)) return true;
    if (!categoryMatches(dialogCategory(d), folder)) return false;
    if (folder.exclude_muted && d.muted) return false;
    if (folder.exclude_read && !(Number(d.unread_count) > 0)) return false;
    if (folder.exclude_archived && d.archived) return false;
    return true;
}

/**
 * Попадает ли диалог в пункт секции // DIRECTORIES.
 *
 * - inbox (и любой неизвестный селектор): все неархивированные;
 * - archive: только `archived === true`;
 * - folder: правила пользовательской папки (см. dialogInFolder).
 *
 * @param {object} d — DialogDto
 * @param {?{type: string, folder?: object}} dir
 * @returns {boolean}
 */
export function dialogMatchesDir(d, dir) {
    if (!d || typeof d !== 'object') return false;
    const t = dir && dir.type;
    if (t === 'archive') return !!d.archived;
    if (t === 'folder') return dialogInFolder(d, dir.folder);
    return !d.archived;
}

/**
 * Диалоги, видимые в выбранной папке (порядок сервера сохраняется).
 *
 * @param {Array<object>} dialogs
 * @param {?{type: string, folder?: object}} dir
 * @returns {Array<object>}
 */
export function dialogsForDir(dialogs, dir) {
    return (Array.isArray(dialogs) ? dialogs : [])
        .filter((d) => dialogMatchesDir(d, dir));
}

/**
 * Amber-счётчик папки: непрочитанное по её диалогам БЕЗ заглушённых.
 *
 * @param {Array<object>} dialogs
 * @param {?{type: string, folder?: object}} dir
 * @param {Set<string>|Array<string>} [muted]
 * @returns {number}
 */
export function dirUnread(dialogs, dir, muted) {
    return unreadTotal(dialogsForDir(dialogs, dir), muted);
}

/**
 * Сколько диалогов просить у сервера под пользовательскую папку.
 *
 * Пользовательские папки (dialogFilter) фильтруются НА ФРОНТЕ, из уже
 * загруженного списка. Стартовая выборка — 100 верхних диалогов, и чат с
 * ботом, до которого сто дней не доходили руки, в неё просто не попадает:
 * папка «Боты» выглядит пустой, хотя боты есть. Отсюда 300 — это дефолт
 * сервера для запросов с явным `?folder=` (DEFAULT_FOLDER_DIALOGS в
 * routes/dialogs.rs); передаём его явно, чтобы связь была видна в коде, а не
 * держалась на совпадении двух дефолтов. Потолок сервера — 500.
 */
export const FOLDER_LIST_LIMIT = 300;

/**
 * Вливает ДОГРУЖЕННУЮ страницу диалогов в текущий список.
 *
 * Свежий ответ авторитетен и идёт первым: в нём порядок Telegram (закреплённые
 * сверху) и актуальные muted/unread/archived. Диалоги, которых в ответе нет,
 * НЕ выбрасываются, а дописываются в хвост: страница архива, подтянутая ранее
 * по `?folder=archive`, и открытый прямо сейчас диалог обязаны пережить
 * догрузку — иначе выбор папки молча стирал бы часть колонки.
 *
 * Дедупликация по id обязательна: пересечение страниц — норма, а дубль в
 * списке даёт две строки на один чат и две записи в `local.rows`.
 *
 * @param {Array<object>} current — текущий tgState.dialogs
 * @param {Array<object>} fresh — ответ GET /api/telegram/dialogs
 * @returns {Array<object>} новый список
 */
export function mergeDialogs(current, fresh) {
    const next = (Array.isArray(fresh) ? fresh : []).filter((d) => d && typeof d === 'object');
    const ids = new Set(next.map((d) => String(d.id)));
    const rest = (Array.isArray(current) ? current : []).filter(
        (d) => d && typeof d === 'object' && !ids.has(String(d.id))
    );
    return next.concat(rest);
}

/**
 * Вливает свежую страницу архива (`GET /dialogs?folder=archive`) в общий
 * список: архивная часть заменяется целиком, неархивная остаётся как была.
 *
 * Дедупликация по id обязательна: диалог мог переехать в архив между
 * загрузками, и без неё он оказался бы в списке дважды. Поле `archived`
 * проставляется принудительно — ответ архивного запроса авторитетен.
 *
 * @param {Array<object>} current — текущий tgState.dialogs
 * @param {Array<object>} archived — ответ ?folder=archive
 * @returns {Array<object>} новый список
 */
export function mergeArchive(current, archived) {
    const arch = (Array.isArray(archived) ? archived : [])
        .filter((d) => d && typeof d === 'object')
        .map((d) => (d.archived ? d : { ...d, archived: true }));
    const ids = new Set(arch.map((d) => String(d.id)));
    const rest = (Array.isArray(current) ? current : []).filter(
        (d) => d && typeof d === 'object' && !d.archived && !ids.has(String(d.id))
    );
    return rest.concat(arch);
}

/**
 * `HH:MM:SS` в локальной зоне.
 *
 * @param {number} unixSecs
 * @returns {string}
 */
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
 *
 * @param {number} unixSecs
 * @returns {string}
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

/**
 * Нужен ли разделитель даты между двумя сообщениями.
 *
 * @param {?object} prev — предыдущее сообщение (null ⇒ начало ленты)
 * @param {object} next
 * @returns {boolean}
 */
export function needsDateSep(prev, next) {
    if (!next) return false;
    if (!prev) return true;
    return dayKey(prev.date) !== dayKey(next.date);
}

/**
 * Короткий детерминированный хеш dialog_id (FNV-1a, 8 hex-символов).
 *
 * Чисто декоративный «◇ KEY 0x…» в шапке сессии: даёт диалогу стабильный
 * визуальный отпечаток, не раскрывая сам id.
 *
 * @param {string} dialogId
 * @returns {string}
 */
export function keyHex(dialogId) {
    const s = String(dialogId == null ? '' : dialogId);
    let h = 0x811c9dc5;
    for (let i = 0; i < s.length; i += 1) {
        h ^= s.charCodeAt(i);
        // FNV-prime, через сложения сдвигов — Math.imul даёт то же, но
        // читаемость важнее микрооптимизации.
        h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h.toString(16).padStart(8, '0');
}

/**
 * Текст шапки сессии: `SESSION ▸ sofia@node · [ONLINE] · ⌁ 42 PACKETS · ◇ KEY 0xdeadbeef`.
 *
 * Присутствие показывается ТОЛЬКО для личных чатов и только когда оно
 * известно: у групп и каналов его не бывает, а «[OFFLINE]» на диалоге, по
 * которому Telegram вообще не присылал статуса, — вранье (статусы приходят не
 * по всем контактам и только при изменении).
 *
 * @param {?object} dialog — DialogDto (null ⇒ ничего не открыто)
 * @param {number} count — сколько сообщений загружено
 * @param {{online?: boolean, wasOnline?: ?number}} [presence]
 * @param {number} [nowSecs] — для детерминизма в тестах
 * @returns {string}
 */
export function sessionHeadText(dialog, count, presence, nowSecs) {
    if (!dialog) return 'SESSION ▸ —';
    const n = Math.max(0, Number(count) || 0);
    const parts = [`SESSION ▸ ${slugName(dialog)}`];
    const kind = dialog.kind || 'user';
    if (presence && kind !== 'group' && kind !== 'channel') {
        parts.push(dialogStatusLine(dialog, presence, nowSecs));
    }
    parts.push(`⌁ ${n} PACKETS`);
    parts.push(`◇ KEY 0x${keyHex(dialog.id)}`);
    return parts.join(' · ');
}

/**
 * Правая ячейка статусбара: сколько контактов в сети.
 *
 * Пустая строка, если сеть присутствия ещё не наполнилась: «0 ONLINE» на
 * свежеоткрытой вкладке — это не «никого нет», а «Telegram ещё ничего не
 * присылал», и показывать это как факт нельзя.
 *
 * @param {number} online — сколько контактов online
 * @param {number} known — по скольким контактам статус вообще известен
 * @returns {string}
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
 * Своё сообщение — всегда `you@local` (терминальная традиция: «я» выглядит
 * одинаково во всех диалогах). Для чужого берётся @username, затем имя,
 * затем — название диалога (посты от имени канала приходят без sender).
 *
 * @param {object} msg — MessageDto
 * @param {?object} [dialog] — DialogDto для fallback'а
 * @returns {string}
 */
export function nickOf(msg, dialog) {
    if (msg && msg.out) return 'you@local';
    const s = msg && msg.sender;
    if (s) {
        if (s.username) return String(s.username);
        if (s.first_name) return String(s.first_name);
        if (s.id) return String(s.id);
    }
    if (dialog && dialog.title) return String(dialog.title);
    return 'unknown';
}

/**
 * Обрезка текста для тоста/превью с многоточием.
 *
 * @param {string} text
 * @param {number} max
 * @returns {string}
 */
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

/**
 * Вставляет сообщение в список, отсортированный по возрастанию id.
 *
 * Возвращает `{list, inserted}`: `inserted === false` означает «такой id уже
 * есть» — ровно тот случай, когда своё же отправленное сообщение приходит
 * ВТОРОЙ раз через WS `new_message`. Дедупликация обязана быть здесь, в
 * чистой функции: в DOM-коде её легко потерять при рефакторинге.
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
 * стартовать дважды (быстрый скролл), и без фильтра лента получила бы
 * дубликаты.
 *
 * @param {Array<object>} older — старая страница (возрастание id)
 * @param {Array<object>} list — текущая лента (возрастание id)
 * @returns {Array<object>} новая лента
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
 * Прижата ли лента к низу (в пределах порога).
 *
 * @param {{scrollTop: number, scrollHeight: number, clientHeight: number}} m
 * @param {number} [threshold=40]
 * @returns {boolean}
 */
export function isAtBottom(m, threshold) {
    if (!m) return true;
    const gap = m.scrollHeight - m.scrollTop - m.clientHeight;
    return gap <= (Number.isFinite(threshold) ? threshold : 40);
}

/**
 * Пора ли подгружать историю вверх.
 *
 * @param {{scrollTop: number}} m
 * @param {{hasMore: boolean, loading: boolean, threshold?: number}} st
 * @returns {boolean}
 */
export function shouldLoadOlder(m, st) {
    if (!m || !st || !st.hasMore || st.loading) return false;
    return m.scrollTop < (Number.isFinite(st.threshold) ? st.threshold : 200);
}

/**
 * Пропускать ли тост по троттлингу «не чаще одного на диалог за 5с».
 *
 * @param {number} lastAtMs — когда показывали прошлый тост этого диалога (0 ⇒ не было)
 * @param {number} nowMs
 * @param {number} [windowMs=5000]
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
 * Telegram держит статус ~6с сам, поэтому чаще одного запроса в 4с слать
 * нечего: это лишний трафик и лишний повод получить FLOOD_WAIT.
 */
export const TYPING_THROTTLE_MS = 4000;

/**
 * Через сколько гаснет ЧУЖОЙ индикатор набора, мс.
 *
 * Сброс по таймеру обязателен: Telegram НЕ присылает «перестал печатать»,
 * он просто перестаёт слать typing. Без таймера индикатор залипал бы навсегда.
 */
export const TYPING_RESET_MS = 6000;

/**
 * Пора ли отправить «печатает…» серверу.
 *
 * Смена диалога сбрасывает троттлинг: собеседник в НОВОМ чате обязан увидеть
 * индикатор сразу, а не через остаток чужого окна.
 *
 * @param {number} lastAtMs — когда слали прошлый раз (0 ⇒ не слали)
 * @param {number} nowMs
 * @param {boolean} sameDialog — тот же ли диалог, что в прошлый раз
 * @param {number} [windowMs=TYPING_THROTTLE_MS]
 * @returns {boolean}
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
 * За это время в открытый диалог успевает прилететь пачка сообщений, и
 * вместо двадцати запросов уходит один — с максимальным `message_id`.
 */
export const READ_DEBOUNCE_MS = 800;

/**
 * Можно ли сейчас отмечать сообщения прочитанными.
 *
 * Оба условия обязательны:
 *
 * - вкладка браузера видима (`document.visibilityState === 'visible'`);
 * - панель Telegram открыта внутри devforge.
 *
 * Иначе сообщения «прочитались» бы в фоне: пользователь ушёл в терминал или
 * свернул браузер, а собеседник видит две галочки и ждёт ответа. Это ровно то
 * поведение, которого от мессенджера НЕ ждут.
 *
 * @param {string} visibilityState — `document.visibilityState`
 * @param {boolean} panelHidden — `#telegram.hidden`
 * @returns {boolean}
 */
export function readAllowed(visibilityState, panelHidden) {
    return String(visibilityState) === 'visible' && !panelHidden;
}

/**
 * Строка индикатора набора под лентой.
 *
 * Один печатающий — «is», несколько — «are»; больше трёх имён не
 * показываем (в шумной группе строка иначе разъезжается на весь экран),
 * остальные сворачиваются в «+N».
 *
 * @param {Array<string>} names
 * @param {number} [max=3]
 * @returns {string} пустая строка, если никто не печатает
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
    const verb = list.length === 1 ? 'is' : 'are';
    return `>> ${who} ${verb} typing…`;
}

/**
 * Оценка сетевой задержки по интервалу между серверными ping'ами.
 *
 * Полноценный RTT замерить нельзя: сервер не эхом отвечает на наш pong, а
 * шлёт ping по своему таймеру. Зато ДРЕЙФ интервала — это как раз задержка
 * доставки: если ping'и приходят через 15.08с вместо 15с, значит кадр шёл
 * ~80мс. Отрицательный дрейф (кадр пришёл раньше) означает, что задержка
 * упала — показываем 0. Первый ping эталона не имеет, поэтому `null`.
 *
 * @param {number} prevPingAtMs — 0 ⇒ первый ping
 * @param {number} nowMs
 * @param {number} intervalMs — серверный период ping'а
 * @returns {?number} миллисекунды или null
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
 * Оценка по дрейфу ping'ов шумит на десятки миллисекунд (планировщик
 * браузера, GC, загрузка машины). Показывать сырое значение — значит показывать
 * мигающее число, которое пользователь читает как «связь скачет». Среднее по
 * пяти тактам (это ~75 секунд при ping'е раз в 15с) меняется медленно и
 * отражает реальную тенденцию.
 *
 * Медиана была бы устойчивее к выбросам, но на окне из пяти значений разница
 * незаметна, а среднее очевиднее при чтении кода.
 *
 * @param {number[]} samples
 * @returns {?number} целые миллисекунды или null, если замеров нет
 */
export function smoothedLatency(samples) {
    if (!Array.isArray(samples)) return null;
    const valid = samples.filter((n) => Number.isFinite(n));
    if (valid.length === 0) return null;
    const sum = valid.reduce((acc, n) => acc + n, 0);
    return Math.round(sum / valid.length);
}

// ---------------------------------------------------------------------------
// Медиа (Фаза 4). Здесь — только ЧИСТАЯ логика карточек: имена, подписи,
// проценты, ключи кэша. Всё, что касается DOM, fetch и objectURL, живёт в
// js/telegram/media.js; благодаря этому функции ниже грузятся в Node и
// покрыты регресс-тестом tmux-web/tests/frontend/telegram_media.test.mjs.
// ---------------------------------------------------------------------------

/** Порядок единиц для formatBytes. */
const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/**
 * Человекочитаемый размер: `512 B`, `840 KB`, `1.2 MB`.
 *
 * Десятичные единицы (1000, не 1024) — как в дизайне и как показывает сам
 * Telegram: пользователь сравнивает цифру с тем, что видит в мобильном
 * клиенте, а не с выводом `ls -l`. Одна цифра после запятой начиная с KB,
 * байты — целыми.
 *
 * @param {?number} bytes
 * @returns {string} пустая строка, если размер неизвестен
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
    // 1.2 MB, но 12 MB: лишний нуль после запятой только шумит.
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

/**
 * Длительность как `M:SS` (или `H:MM:SS` для длинного видео).
 *
 * @param {?number} secs
 * @returns {string} пустая строка, если длительности нет
 */
export function durationLabel(secs) {
    // Как и в formatBytes: `null` не должен превращаться в «0:00».
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
 * Формат тот же, что у серверного `media_cache::CacheKey::id`
 * (`<dialog_id>/<message_id>/<kind>`): ключи в логах фронта и бэкенда
 * совпадают, а события `download_progress` сопоставляются с карточкой без
 * преобразований.
 *
 * @param {string|number} dialogId
 * @param {number} messageId
 * @param {'thumb'|'full'} kind
 * @returns {string}
 */
export function mediaKey(dialogId, messageId, kind) {
    return `${dialogId}/${messageId}/${kind === 'thumb' ? 'thumb' : 'full'}`;
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

/** Расширение по умолчанию для вида вложения (когда MIME не пришёл). */
const KIND_EXT = {
    photo: 'jpg',
    video: 'mp4',
    voice: 'ogg',
    audio: 'mp3',
    sticker: 'webp',
};

/**
 * Имя файла для карточки и для SAVE.
 *
 * У фото, голосовых и стикеров Telegram имени не присылает вовсе, а пустую
 * строку рисовать нельзя — карточка выглядела бы сломанной. Поэтому имя
 * генерируется из вида вложения и message_id: `photo_42.jpg`, `voice_7.ogg`.
 *
 * @param {object} msg — MessageDto с непустым `media`
 * @returns {string}
 */
export function mediaFileName(msg) {
    const media = (msg && msg.media) || {};
    const given = typeof media.file_name === 'string' ? media.file_name.trim() : '';
    if (given) return given;
    const kind = String(media.kind || 'file');
    const ext = MIME_EXT[String(media.mime || '')] || KIND_EXT[kind] || 'bin';
    const id = Number.isFinite(msg && msg.id) ? msg.id : 0;
    return `${kind}_${id}.${ext}`;
}

/**
 * Имя для сохранения на диск.
 *
 * Санитизация обязательна: имя приходит из Telegram, то есть от чужого
 * аккаунта, а атрибут `download` у `<a>` браузеры понимают как подсказку
 * пути. Убираются разделители пути, управляющие символы, спецсимволы ФС и
 * ведущие точки (иначе `../../.bashrc` или скрытый файл).
 *
 * @param {string} name
 * @param {string} [fallback='download.bin']
 * @returns {string}
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
        // «invoice‮fdp.exe» читается глазами как «invoiceexe.pdf»,
        // оставаясь исполняемым .exe — классический спуфинг расширения.
        .replace(/[\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g, '')
        .replace(/^\.+/, '')
        .trim();
    return capUtf8Bytes(cleaned, 255) || fb;
}

/**
 * Обрезает строку до лимита в БАЙТАХ UTF-8, не разрывая code points и
 * сохраняя расширение: файловые системы (ext4/APFS/NTFS) ограничивают имя
 * 255 байтами, и «щедрое» имя из Telegram молча уронило бы сохранение.
 *
 * @param {string} name
 * @param {number} maxBytes
 * @returns {string}
 */
function capUtf8Bytes(name, maxBytes) {
    const enc = new TextEncoder();
    if (enc.encode(name).length <= maxBytes) return name;
    const dot = name.lastIndexOf('.');
    // Расширение сохраняем, только если оно правдоподобной длины.
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
 * @param {string} kind
 * @returns {string}
 */
export function mediaKindLabel(kind) {
    const map = {
        photo: 'PHOTO',
        video: 'VIDEO',
        document: 'FILE',
        sticker: 'STICKER',
        voice: 'VOICE',
        audio: 'AUDIO',
        other: 'DATA',
    };
    return map[String(kind || '').toLowerCase()] || 'DATA';
}

/**
 * Глиф вида вложения (терминальная стилистика, без цветных emoji).
 *
 * @param {string} kind
 * @returns {string}
 */
export function mediaGlyph(kind) {
    const map = {
        photo: '▣',
        video: '▶',
        document: '▤',
        sticker: '◆',
        voice: '∿',
        audio: '♪',
        other: '◌',
    };
    return map[String(kind || '').toLowerCase()] || '◌';
}

/**
 * Строка метаданных под именем файла: `1280×720 · 1.2 MB`, `0:07 · 24 KB`.
 *
 * @param {object} media — MediaDto
 * @returns {string}
 */
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
 * вместе с финальным событием, иначе индикатор показывает готовность раньше,
 * чем файл доступен.
 *
 * @param {number} received
 * @param {?number} total
 * @returns {?number}
 */
export function progressPercent(received, total) {
    const r = Number(received);
    const t = Number(total);
    if (!Number.isFinite(r) || r < 0) return null;
    if (!Number.isFinite(t) || t <= 0) return null;
    if (r >= t) return 100;
    return Math.max(0, Math.min(99, Math.floor((r / t) * 100)));
}

/**
 * Подпись прогресса: `49%`, либо принятый объём, если размер неизвестен.
 *
 * @param {number} received
 * @param {?number} total
 * @returns {string}
 */
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
 * `404 no_media`, а делать его на каждую карточку длинной истории — чистые
 * потери. Виды без превью (голосовые, `other`) тоже отсекаются здесь.
 *
 * @param {object} media — MediaDto
 * @returns {boolean}
 */
export function canShowThumb(media) {
    const m = media || {};
    if (!m.has_thumb) return false;
    return ['photo', 'video', 'document', 'sticker'].includes(String(m.kind));
}

/**
 * Воспроизводимое ли вложение (для него OPEN означает «включить плеер»).
 *
 * @param {string} kind
 * @returns {boolean}
 */
export function isPlayableKind(kind) {
    return ['video', 'voice', 'audio'].includes(String(kind || '').toLowerCase());
}

/**
 * Открывается ли вложение в лайтбоксе (полноразмерная картинка).
 *
 * @param {string} kind
 * @returns {boolean}
 */
export function isImageKind(kind) {
    return ['photo', 'sticker'].includes(String(kind || '').toLowerCase());
}

// ---------------------------------------------------------------------------
// Отправка файлов (Фаза 5) — чистая логика
// ---------------------------------------------------------------------------

/**
 * Клиентский предел размера файла, байты (2 ГиБ).
 *
 * Дублирует `max_upload_bytes` сервера СОЗНАТЕЛЬНО: локальная проверка нужна
 * только чтобы не гнать заведомо отвергнутый гигабайт по сети. Источник
 * истины — сервер: он отвечает `413 too_large` по `Content-Length`, ещё не
 * читая тело, и этот ответ обрабатывается как обычная ошибка отправки.
 */
export const MAX_UPLOAD_BYTES = 2 * 1024 * 1024 * 1024;

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
 * некоторых приложений), а вид карточки и глиф выбираются именно по MIME.
 * Сервер угадывает MIME независимо и тоже по расширению — расхождения между
 * подписью карточки и способом отправки не будет.
 *
 * @param {string} name
 * @param {string} [type] — `File.type`
 * @returns {string}
 */
export function uploadMime(name, type) {
    const given = String(type || '').trim().toLowerCase();
    if (given) return given;
    const ext = String(name || '').split('.').pop();
    if (!ext || ext === name) return 'application/octet-stream';
    return EXT_MIME[ext.toLowerCase()] || 'application/octet-stream';
}

/**
 * Вид вложения по MIME — тот же словарь `kind`, что у принимаемых медиа
 * (`photo | video | audio | document`), чтобы карточка отправки выглядела
 * ровно как карточка приёма.
 *
 * @param {string} mime
 * @returns {'photo'|'video'|'audio'|'document'}
 */
export function uploadKind(mime) {
    const m = String(mime || '').toLowerCase();
    if (m.startsWith('image/')) return 'photo';
    if (m.startsWith('video/')) return 'video';
    if (m.startsWith('audio/')) return 'audio';
    return 'document';
}

/**
 * Имя для картинки, вставленной из буфера обмена.
 *
 * У скриншота из буфера имени нет вовсе (`File.name` — пустая строка или
 * «image.png» у всех подряд), поэтому имя генерируется из времени: файлы в
 * Telegram не перезатирают друг друга и видно, когда сделан снимок.
 *
 * @param {string} mime
 * @param {number} [nowMs]
 * @returns {string}
 */
export function pastedFileName(mime, nowMs) {
    const ms = Number.isFinite(nowMs) ? nowMs : Date.now();
    const d = new Date(ms);
    const p = (v) => String(v).padStart(2, '0');
    const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}`
        + `_${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
    const sub = String(mime || '').toLowerCase().split('/')[1] || 'png';
    // `image/svg+xml` → `svg`, `image/jpeg` → `jpeg`.
    const ext = sub.split('+')[0].replace(/[^a-z0-9]/g, '') || 'png';
    return `pasted_${stamp}.${ext}`;
}

/**
 * Подпись состояния карточки отправки.
 *
 * Два этапа названы по-разному не для красоты: пользователь должен видеть,
 * что «LOCAL 100 %» — это ещё не отправлено, файл только дошёл до devforge, а
 * дальше идёт вторая, обычно более медленная нога до Telegram.
 *
 * @param {'queued'|'local'|'tg'|'sent'|'failed'} stage
 * @param {number} [sent]
 * @param {?number} [total]
 * @returns {string}
 */
export function uploadStageLabel(stage, sent, total) {
    switch (String(stage)) {
        case 'queued':
            return 'QUEUED';
        case 'local':
            return `LOCAL ${progressLabel(sent, total)}`;
        case 'tg':
            return `TG ${progressLabel(sent, total)}`;
        case 'sent':
            return '✓ SENT';
        case 'failed':
            return '!! FAILED';
        default:
            return '';
    }
}

/**
 * Позиция в очереди: `QUEUED 2/3` (первый файл заливается, остальные ждут).
 *
 * @param {number} position — 1-based
 * @param {number} total
 * @returns {string}
 */
export function uploadQueueLabel(position, total) {
    const p = Math.max(1, Number(position) || 1);
    const t = Math.max(p, Number(total) || p);
    return t > 1 ? `QUEUED ${p}/${t}` : 'QUEUED';
}

/**
 * Причина отказа ДО сетевого запроса или `null`, если файл можно отправлять.
 *
 * Пустой файл отсекается здесь же: Telegram отвергает нулевой размер
 * (`FILE_PARTS_INVALID`), и честнее сказать это сразу.
 *
 * @param {{size?: number, name?: string}} file
 * @param {number} [limit=MAX_UPLOAD_BYTES]
 * @returns {?string}
 */
export function uploadRejectReason(file, limit) {
    const max = Number.isFinite(limit) && limit > 0 ? limit : MAX_UPLOAD_BYTES;
    const size = Number(file && file.size);
    if (!Number.isFinite(size)) return 'размер файла неизвестен';
    if (size <= 0) return 'файл пустой';
    if (size > max) return `файл ${formatBytes(size)} больше лимита ${formatBytes(max)}`;
    return null;
}

/**
 * Разбирает тело ошибки плагина в единый вид.
 *
 * Тот же контракт, что в api.js (`{error:{code,message,retry_after_secs}}`),
 * но для XHR: `xhr.responseText` — строка, а решения принимаются по КОДУ.
 * Невалидный JSON — норма (ответ прокси, 502 от nginx), тогда остаётся статус.
 *
 * @param {number} status
 * @param {string} body
 * @returns {{status: number, code: ?string, message: string, retryAfterSecs: ?number}}
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
// Тема панели (js/telegram/theme.js)
//
// Вся арифметика цвета живёт здесь, а не в theme.js: theme.js трогает DOM и
// localStorage, поэтому в Node не грузится, а парсер цвета обязан быть покрыт
// тестами — он читает произвольную пользовательскую тему devforge.
// ---------------------------------------------------------------------------

/** Порядок обхода тумблером ◐. */
export const TG_THEME_MODES = ['auto', 'dark', 'light'];

/** Тема, которая применяется, когда режим `auto`, а цвет распознать не вышло. */
export const TG_THEME_FALLBACK = 'dark';

/** Немногочисленные именованные цвета, которые реально встречаются в темах. */
const NAMED_COLORS = {
    black: { r: 0, g: 0, b: 0 },
    white: { r: 255, g: 255, b: 255 },
    silver: { r: 192, g: 192, b: 192 },
    gray: { r: 128, g: 128, b: 128 },
    grey: { r: 128, g: 128, b: 128 },
};

/**
 * Приводит режим к одному из [`TG_THEME_MODES`].
 *
 * Любой мусор (в том числе значение из localStorage, записанное прошлой
 * версией) превращается в 'auto' — режим по умолчанию.
 *
 * @param {*} mode
 * @returns {'auto'|'dark'|'light'}
 */
export function normalizeTgThemeMode(mode) {
    const m = String(mode == null ? '' : mode).toLowerCase();
    return TG_THEME_MODES.includes(m) ? m : 'auto';
}

/**
 * Следующий режим по кругу: auto → dark → light → auto.
 *
 * @param {*} mode
 * @returns {'auto'|'dark'|'light'}
 */
export function nextTgThemeMode(mode) {
    const idx = TG_THEME_MODES.indexOf(normalizeTgThemeMode(mode));
    return TG_THEME_MODES[(idx + 1) % TG_THEME_MODES.length];
}

/**
 * Разбирает CSS-цвет в компоненты 0..255.
 *
 * Поддержаны формы, которые реально может отдать `getComputedStyle` или
 * пользовательская тема: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`,
 * `rgb(r,g,b)`, `rgba(r,g,b,a)`, современная запись `rgb(r g b / a)`,
 * проценты внутри `rgb()` и горстка именованных цветов.
 *
 * Всё остальное (`oklch()`, `color()`, `transparent`, градиент) — `null`:
 * вызывающий обязан иметь запасной вариант, а не гадать по мусору.
 *
 * @param {*} value
 * @returns {?{r: number, g: number, b: number}}
 */
export function parseCssColor(value) {
    const raw = String(value == null ? '' : value).trim().toLowerCase();
    if (!raw) return null;

    if (Object.prototype.hasOwnProperty.call(NAMED_COLORS, raw)) {
        return { ...NAMED_COLORS[raw] };
    }

    if (raw.charAt(0) === '#') {
        const hex = raw.slice(1);
        if (/^[0-9a-f]{3,4}$/.test(hex)) {
            return {
                r: parseInt(hex.charAt(0) + hex.charAt(0), 16),
                g: parseInt(hex.charAt(1) + hex.charAt(1), 16),
                b: parseInt(hex.charAt(2) + hex.charAt(2), 16),
            };
        }
        if (/^[0-9a-f]{6}$/.test(hex) || /^[0-9a-f]{8}$/.test(hex)) {
            return {
                r: parseInt(hex.slice(0, 2), 16),
                g: parseInt(hex.slice(2, 4), 16),
                b: parseInt(hex.slice(4, 6), 16),
            };
        }
        return null;
    }

    const m = /^rgba?\(([^)]*)\)$/.exec(raw);
    if (!m) return null;
    // И запятая, и пробел, и слэш альфы: `rgb(1,2,3)`, `rgb(1 2 3 / .5)`.
    const parts = m[1].split('/')[0].split(/[\s,]+/).filter((s) => s.length > 0);
    if (parts.length < 3) return null;
    const comp = parts.slice(0, 3).map((p) => {
        if (p.endsWith('%')) {
            const pct = Number.parseFloat(p.slice(0, -1));
            return Number.isFinite(pct) ? (pct * 255) / 100 : NaN;
        }
        return Number.parseFloat(p);
    });
    if (comp.some((n) => !Number.isFinite(n))) return null;
    const clamp = (n) => Math.min(255, Math.max(0, Math.round(n)));
    return { r: clamp(comp[0]), g: clamp(comp[1]), b: clamp(comp[2]) };
}

/**
 * Относительная яркость 0..1 по формуле 0.2126R + 0.7152G + 0.0722B на
 * НОРМИРОВАННЫХ (0..1) компонентах.
 *
 * Гамма-коррекция (sRGB → linear) намеренно НЕ применяется: нужен не
 * контрастный коэффициент по WCAG, а грубое «светлая тема или тёмная», и
 * простая формула даёт тот же ответ на всех штатных темах devforge, зато
 * читается без сносок.
 *
 * @param {?{r: number, g: number, b: number}} color
 * @returns {?number}
 */
export function relativeLuminance(color) {
    if (!color || typeof color !== 'object') return null;
    const { r, g, b } = color;
    if (![r, g, b].every((n) => Number.isFinite(n))) return null;
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
}

/**
 * Итоговая тема панели: что положить в `#telegram[data-tg-theme]`.
 *
 * В режимах 'dark'/'light' цвет приложения не смотрим вовсе — пользователь
 * зафиксировал вид панели явно. В 'auto' решает яркость `--bg` приложения:
 * L > 0.5 ⇒ светлая. Нераспознанный цвет ⇒ [`TG_THEME_FALLBACK`].
 *
 * @param {*} mode
 * @param {*} appBg — значение CSS-переменной --bg
 * @returns {'dark'|'light'}
 */
export function resolveTgTheme(mode, appBg) {
    const m = normalizeTgThemeMode(mode);
    if (m !== 'auto') return m;
    const lum = relativeLuminance(parseCssColor(appBg));
    if (lum == null) return TG_THEME_FALLBACK;
    return lum > 0.5 ? 'light' : 'dark';
}

/**
 * Подпись тумблера ◐: режим и — для 'auto' — что он сейчас выбрал.
 *
 * @param {*} mode
 * @param {*} resolved — 'dark' | 'light'
 * @returns {string}
 */
export function tgThemeToggleTitle(mode, resolved) {
    const m = normalizeTgThemeMode(mode);
    const r = resolved === 'light' ? 'светлая' : 'тёмная';
    if (m === 'auto') return `Тема панели: авто (сейчас ${r}) — клик для тёмной`;
    if (m === 'dark') return 'Тема панели: тёмная — клик для светлой';
    return 'Тема панели: светлая — клик для авто';
}
