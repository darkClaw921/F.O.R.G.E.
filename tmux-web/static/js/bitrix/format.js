// tmux-web — Bitrix24 plugin: ЧИСТЫЕ функции форматирования и рендера.
//
// Модуль не импортирует ни DOM-узлы, ни сеть, ни состояние — только принимает
// аргументы и возвращает значения. Ровно поэтому он целиком грузится в голом
// Node и покрыт тестом tmux-web/tests/frontend/bitrix_format.test.mjs.
//
// ============================================================================
// XSS-ИНВАРИАНТ ПАНЕЛИ
// ============================================================================
//
// `innerHTML` в js/bitrix/* запрещён ПОЛНОСТЬЮ. Текст сообщений приходит с
// портала, где его пишут люди и боты, и единственный безопасный способ показать
// его — узлы: `createElement` + `textContent`. Поэтому бэкенд и отдаёт BB-коды
// СТРУКТУРНЫМИ СЕГМЕНТАМИ (plugins/bitrix/src/bbcode.rs), а не готовым HTML:
// собрать HTML-строку тут просто не из чего.
//
// `renderSegments` принимает `doc` аргументом (по умолчанию — глобальный
// `document`). Это не украшательство: так функция остаётся чистой и
// проверяется в Node без фейкового глобального документа.
//
// ============================================================================
// ЗАМОРОЖЕННЫЙ СЛОВАРЬ ВИДОВ ДИАЛОГА
// ============================================================================
//
// `user | chat | task | sg | openline | copilot | collab` — дословно
// `plugins/bitrix/src/dto.rs::kind`. Незнакомый вид не ошибка: он рисуется как
// обычный чат (портал добавляет типы чатов между релизами).

/** Виды диалога, известные панели (совпадает с dto.rs::kind). */
export const DIALOG_KINDS = ['user', 'chat', 'task', 'sg', 'openline', 'copilot', 'collab'];

/** Директории колонки — совпадает с DIRS в plugins/bitrix/src/routes/dialogs.rs. */
export const DIRS = ['all', 'chats', 'tasks', 'unread'];

/** Длина превью последнего сообщения в строке списка, символов. */
export const PREVIEW_CHARS = 90;

/** Сколько секунд считать «только что». */
const JUST_NOW_SECS = 45;

// ---------------------------------------------------------------------------
// Время
// ---------------------------------------------------------------------------

/**
 * Разбирает дату портала (ATOM: `2026-08-05T20:14:43+03:00`) в миллисекунды.
 *
 * Портал всегда присылает смещение, поэтому часовой пояс браузера ничего не
 * ломает. Мусор и пустая строка дают `null` — «даты нет» лучше, чем `NaN`,
 * который потом молча превращается в «01.01.1970».
 *
 * @param {?string} iso
 * @returns {?number} миллисекунды либо null
 */
export function parseDate(iso) {
    if (typeof iso !== 'string' || !iso) return null;
    const ms = Date.parse(iso);
    return Number.isFinite(ms) ? ms : null;
}

/**
 * Время суток `ЧЧ:ММ` для строки ленты и списка.
 *
 * @param {?string} iso
 * @returns {string} пустая строка, если даты нет
 */
export function formatTime(iso) {
    const ms = parseDate(iso);
    if (ms == null) return '';
    const d = new Date(ms);
    return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/**
 * Относительный возраст: `только что`, `5м`, `3ч`, `2д`, дальше — дата.
 *
 * Короткие формы, а не «5 минут назад»: колонка узкая, и подпись обязана
 * помещаться рядом со счётчиком непрочитанного.
 *
 * @param {?string} iso
 * @param {number} [nowMs]
 * @returns {string}
 */
export function relativeAge(iso, nowMs = Date.now()) {
    const ms = parseDate(iso);
    if (ms == null) return '';
    const secs = Math.max(0, Math.floor((nowMs - ms) / 1000));
    if (secs < JUST_NOW_SECS) return 'только что';
    if (secs < 3600) return `${Math.floor(secs / 60)}м`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}ч`;
    if (secs < 7 * 86400) return `${Math.floor(secs / 86400)}д`;
    const d = new Date(ms);
    return `${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')}`;
}

/** Ключ дня для разделителей ленты (Ф5) — локальная дата, а не UTC. */
export function dayKey(iso) {
    const ms = parseDate(iso);
    if (ms == null) return '';
    const d = new Date(ms);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

// ---------------------------------------------------------------------------
// Строки и имена
// ---------------------------------------------------------------------------

/**
 * Обрезает строку по СИМВОЛАМ (не по code unit'ам) и вешает многоточие.
 *
 * `Array.from` вместо `slice` намеренно: эмодзи и часть кириллических символов
 * занимают две единицы UTF-16, и `slice` умеет разрезать их пополам, оставляя
 * в DOM «сломанный» символ.
 *
 * @param {?string} s
 * @param {number} max
 * @returns {string}
 */
export function truncate(s, max) {
    const text = String(s == null ? '' : s);
    const chars = Array.from(text);
    if (chars.length <= max) return text;
    return `${chars.slice(0, Math.max(0, max - 1)).join('').trimEnd()}…`;
}

/**
 * Заголовок строки списка. Пустого заголовка не бывает: подписью становится
 * идентификатор — иначе строка выглядит как поломка.
 *
 * @param {?object} d — DialogDto
 * @returns {string}
 */
export function displayName(d) {
    if (!d) return '';
    const title = String(d.title == null ? '' : d.title).trim();
    return title || String(d.id == null ? '' : d.id);
}

/**
 * Вид диалога, приведённый к известному словарю.
 *
 * @param {?object|string} d — DialogDto либо сам вид
 * @returns {string}
 */
export function dialogKind(d) {
    const kind = typeof d === 'string' ? d : (d && d.kind);
    return DIALOG_KINDS.includes(kind) ? kind : 'chat';
}

/**
 * Значок вида диалога. ASCII-глифы, а не эмодзи: панель моноширинная, а
 * эмодзи в моноширинном шрифте разъезжаются по ширине и ломают колонку.
 *
 * @param {?object|string} d
 * @returns {string}
 */
export function kindGlyph(d) {
    switch (dialogKind(d)) {
        case 'task': return '#';
        case 'user': return '@';
        case 'sg': return '&';
        case 'openline': return '~';
        case 'copilot': return '*';
        case 'collab': return '+';
        default: return ':';
    }
}

/**
 * Подпись под заголовком строки: превью последнего сообщения.
 *
 * Правила ровно те же, что у соседних панелей:
 * - своё сообщение помечается `Вы: `;
 * - системное (author_id == 0) идёт без подписи автора — его «автор» это сам
 *   портал;
 * - в групповом чате перед текстом ставится имя автора, в личном — нет
 *   (собеседник и так один, и его имя уже в заголовке);
 * - сообщение без текста, но с файлом, показывается как `[файл]` — пустая
 *   строка читалась бы как «сообщений нет».
 *
 * @param {?object} d — DialogDto
 * @returns {string}
 */
export function previewText(d) {
    const m = d && d.last_message;
    if (!m) return '';
    let text = String(m.text == null ? '' : m.text).replace(/\s+/g, ' ').trim();
    if (!text && m.has_file) text = '[файл]';
    if (!text) return '';
    if (m.system) return truncate(text, PREVIEW_CHARS);
    if (m.out) return truncate(`Вы: ${text}`, PREVIEW_CHARS);
    const kind = dialogKind(d);
    const author = kind === 'user' ? '' : String(m.author_name || '').trim();
    return truncate(author ? `${author}: ${text}` : text, PREVIEW_CHARS);
}

/**
 * Правая колонка строки: счётчик непрочитанного либо возраст.
 *
 * Одно поле на две сущности намеренно: показывать и то, и другое в 260-пиксельной
 * колонке некуда, а непрочитанное важнее.
 *
 * @param {?object} d
 * @param {number} [nowMs]
 * @returns {string}
 */
export function dialogMark(d, nowMs = Date.now()) {
    if (!d) return '';
    const unread = Number(d.unread_count) || 0;
    if (unread > 0) return unreadBadgeText(unread);
    return relativeAge(d.date_update, nowMs);
}

/** Текст бейджа непрочитанного: больше 99 — «99+». */
export function unreadBadgeText(count) {
    const n = Number.isFinite(count) ? Math.max(0, Math.trunc(count)) : 0;
    if (n <= 0) return '';
    return n > 99 ? '99+' : String(n);
}

/** Заголовок секции списка: `// ~/tasks — 2 NODES`. */
export function nodesTitle(dir, count) {
    const key = DIRS.includes(dir) ? dir : 'all';
    const n = Number.isFinite(count) ? Math.max(0, Math.trunc(count)) : 0;
    return `// ~/${key} — ${n} NODES`;
}

// ---------------------------------------------------------------------------
// Список
// ---------------------------------------------------------------------------

/**
 * Попадает ли диалог в директорию.
 *
 * ПОСТРОЧНАЯ КОПИЯ `dir_matches` из plugins/bitrix/src/routes/dialogs.rs:
 * серверная страница и локальное переключение директории обязаны давать
 * одинаковый набор строк, иначе пользователь увидит, как список меняется сам
 * при прокрутке.
 *
 * @param {string} dir
 * @param {?object} d
 * @returns {boolean}
 */
export function dirMatches(dir, d) {
    if (!d) return false;
    switch (dir) {
        case 'chats': return dialogKind(d) !== 'task';
        case 'tasks': return dialogKind(d) === 'task';
        case 'unread': return (Number(d.unread_count) || 0) > 0;
        default: return true;
    }
}

/** Сколько строк в каждой директории — для счётчиков колонки. */
export function dirCounts(dialogs) {
    const list = Array.isArray(dialogs) ? dialogs : [];
    const out = {};
    for (const dir of DIRS) out[dir] = list.filter((d) => dirMatches(dir, d)).length;
    return out;
}

/** Сумма непрочитанного по загруженным строкам (запасной путь для бейджа). */
export function unreadTotal(dialogs) {
    return (Array.isArray(dialogs) ? dialogs : [])
        .reduce((sum, d) => sum + (Number(d && d.unread_count) || 0), 0);
}

/** Строка списка по идентификатору (идентификаторы — строки). */
export function findDialog(dialogs, id) {
    const key = String(id == null ? '' : id);
    return (Array.isArray(dialogs) ? dialogs : []).find((d) => d && String(d.id) === key) || null;
}

/**
 * Порядок строк: закреплённые сверху, дальше по дате изменения.
 *
 * Тот же порядок отдаёт сервер — сортировка здесь нужна для строк, приехавших
 * WS-кадром `dialogs_patch` (Ф7): вставлять их в середину списка «на глаз»
 * означало бы, что после каждого сообщения порядок слегка меняется.
 *
 * @param {Array<object>} dialogs
 * @returns {Array<object>} НОВЫЙ массив
 */
export function sortDialogs(dialogs) {
    return (Array.isArray(dialogs) ? dialogs : []).slice().sort((a, b) => {
        const pinned = Number(!!(b && b.pinned)) - Number(!!(a && a.pinned));
        if (pinned !== 0) return pinned;
        const da = String((a && a.date_update) || '');
        const db = String((b && b.date_update) || '');
        if (da === db) return 0;
        return da < db ? 1 : -1;
    });
}

/**
 * Приклеивает страницу к загруженному списку, обновляя совпавшие строки.
 *
 * Обновление, а не «добавить если нет»: повторный запрос той же страницы
 * приносит СВЕЖИЕ счётчики и последние сообщения, и выбрасывать их значило бы
 * показывать устаревший список до перезагрузки вкладки.
 *
 * @param {Array<object>} existing
 * @param {Array<object>} page
 * @returns {Array<object>} НОВЫЙ массив в порядке сортировки
 */
export function mergeDialogs(existing, page) {
    const out = (Array.isArray(existing) ? existing : []).slice();
    const index = new Map(out.map((d, i) => [String(d && d.id), i]));
    for (const d of (Array.isArray(page) ? page : [])) {
        if (!d || d.id == null) continue;
        const key = String(d.id);
        if (index.has(key)) out[index.get(key)] = d;
        else {
            index.set(key, out.length);
            out.push(d);
        }
    }
    return sortDialogs(out);
}

// ---------------------------------------------------------------------------
// BB-сегменты → УЗЛЫ (никакого HTML)
// ---------------------------------------------------------------------------

/** Вид сегмента → тег и класс. Совпадает с bbcode.rs::kind. */
const SEGMENT_TAGS = {
    b: ['strong', 'b24-seg-b'],
    i: ['em', 'b24-seg-i'],
    u: ['span', 'b24-seg-u'],
    s: ['span', 'b24-seg-s'],
    code: ['code', 'b24-seg-code'],
    quote: ['span', 'b24-seg-quote'],
    url: ['span', 'b24-seg-url'],
    user: ['span', 'b24-seg-user'],
    chat: ['span', 'b24-seg-chat'],
    disk: ['span', 'b24-seg-disk'],
    timestamp: ['span', 'b24-seg-time'],
};

/**
 * Подпись момента времени из сегмента `[TIMESTAMP=…]`.
 *
 * Портал кладёт туда unix-время СЕКУНДАМИ (живой пример из системной строки
 * чата задачи: «Крайний срок задачи, 1783180800»), но встречается и обычная
 * дата строкой — разбираются обе формы.
 *
 * @param {?string|number} value
 * @returns {string} пустая строка, если значение не разбирается
 */
export function timestampLabel(value) {
    if (value == null || value === '') return '';
    const raw = String(value).trim();
    // Десять-одиннадцать цифр — это секунды, а не миллисекунды: `new Date`
    // без умножения показал бы 1970 год.
    const ms = /^\d{9,11}$/.test(raw) ? Number(raw) * 1000 : parseDate(raw);
    if (ms == null || !Number.isFinite(ms)) return '';
    const d = new Date(ms);
    const pad = (n) => String(n).padStart(2, '0');
    return `${pad(d.getDate())}.${pad(d.getMonth() + 1)}.${d.getFullYear()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * Плоский текст дерева сегментов — для превью и для `title`.
 *
 * @param {Array<object>} segments
 * @returns {string}
 */
export function segmentsText(segments) {
    return (Array.isArray(segments) ? segments : []).map((seg) => {
        if (!seg || typeof seg !== 'object') return '';
        if (seg.kind === 'text') return String(seg.text == null ? '' : seg.text);
        if (seg.kind === 'br') return '\n';
        const inner = segmentsText(seg.children);
        return inner || String(seg.value == null ? '' : seg.value);
    }).join('');
}

/**
 * Собирает УЗЛЫ по дереву сегментов.
 *
 * Возвращает фрагмент-контейнер (`<span class="b24-segs">`), внутри которого
 * ни одной строки HTML: только `createElement` и текстовые узлы. Даже
 * `<script>alert(1)</script>` в тексте сообщения попадёт в документ как ТЕКСТ.
 *
 * ССЫЛКИ НЕ СТАНОВЯТСЯ `<a href>`. Адрес приходит от собеседника, а `href`
 * с `javascript:`/`data:` — это исполнение чужого кода по клику. Ссылка
 * рисуется текстом с классом `b24-seg-url`; открытием (Ф5) занимается
 * обработчик, который проверяет схему.
 *
 * `[USER=15]` и `[CHAT=27]` кладут идентификатор в `dataset` — по нему Ф5
 * подставит свежее имя из кэша и повесит переход в диалог.
 *
 * @param {Array<object>} segments — bbcode::Segment[]
 * @param {object} [doc] — документ (аргумент ради чистоты и Node-тестов)
 * @param {number} [depth] — служебный ограничитель вложенности
 * @returns {object} узел-контейнер
 */
export function renderSegments(segments, doc = globalThis.document, depth = 0) {
    const box = doc.createElement('span');
    box.className = depth === 0 ? 'b24-segs' : 'b24-seg-inner';
    if (depth > 8) return box;

    for (const seg of (Array.isArray(segments) ? segments : [])) {
        if (!seg || typeof seg !== 'object') continue;
        if (seg.kind === 'text') {
            box.appendChild(doc.createTextNode(String(seg.text == null ? '' : seg.text)));
            continue;
        }
        if (seg.kind === 'br') {
            box.appendChild(doc.createElement('br'));
            continue;
        }
        const [tag, cls] = SEGMENT_TAGS[seg.kind] || ['span', 'b24-seg-unknown'];
        const node = doc.createElement(tag);
        node.className = cls;
        if (seg.value != null && seg.value !== '') {
            // dataset, а не href/src: значение пришло от собеседника.
            node.dataset.value = String(seg.value);
        }
        const children = Array.isArray(seg.children) ? seg.children : [];
        if (children.length > 0) {
            node.appendChild(renderSegments(children, doc, depth + 1));
        } else if (seg.value != null) {
            // `[TIMESTAMP=1783180800]` без содержимого — это МОМЕНТ ВРЕМЕНИ, и
            // показывать его десятизначным числом («Крайний срок задачи,
            // 1783180800») бессмысленно: живьём такие сегменты приходят в
            // системных строках чатов задач.
            const text = seg.kind === 'timestamp'
                ? (timestampLabel(seg.value) || String(seg.value))
                : String(seg.value);
            node.appendChild(doc.createTextNode(text));
        }
        box.appendChild(node);
    }
    return box;
}

// ---------------------------------------------------------------------------
// Лента: список сообщений, прокрутка, разделители дней
// ---------------------------------------------------------------------------

/**
 * Зазор у нижнего края, при котором лента ещё считается «внизу», px.
 *
 * Ноль не годится: субпиксельная вёрстка и масштаб браузера оставляют
 * дробный хвост, и `scrollTop + clientHeight` почти никогда не равен
 * `scrollHeight` в точности.
 */
export const BOTTOM_SLACK_PX = 24;

/** За сколько пикселей до верха ленты начинается догрузка истории. */
export const OLDER_TRIGGER_PX = 160;

/**
 * Вставляет сообщение в список, отсортированный ПО ВОЗРАСТАНИЮ id.
 *
 * Сообщение с уже известным id ЗАМЕНЯЕТ старую версию, а не добавляется рядом:
 * одно и то же сообщение приезжает из истории, из realtime-кадра и из
 * догрузки, и каждое такое совпадение без замены превращалось бы в дубль
 * строки.
 *
 * Мутирует переданный массив и возвращает его же — так делают все панели: за
 * ленту отвечает один модуль, копия списка на каждое сообщение стоила бы
 * заметно дороже.
 *
 * @param {Array<object>} list — сообщения по возрастанию id
 * @param {object} msg — MessageDto
 * @returns {Array<object>} тот же список
 */
export function insertMessage(list, msg) {
    if (!Array.isArray(list) || !msg || msg.id == null) return list || [];
    const id = Number(msg.id);
    for (let i = list.length - 1; i >= 0; i -= 1) {
        const cur = Number(list[i].id);
        if (cur === id) {
            list[i] = msg;
            return list;
        }
        if (cur < id) {
            list.splice(i + 1, 0, msg);
            return list;
        }
    }
    list.unshift(msg);
    return list;
}

/**
 * Добавляет страницу истории в список без дублей.
 *
 * Страницы приходят с пересечением по границе (`before_id` берётся у самой
 * старой строки, и портал возвращает её соседей), поэтому склейка обязана
 * схлопывать совпадения по id, а не доверять границам.
 *
 * @param {Array<object>} list
 * @param {Array<object>} page
 * @returns {Array<object>} тот же список
 */
export function mergeMessages(list, page) {
    for (const msg of (Array.isArray(page) ? page : [])) insertMessage(list, msg);
    return list;
}

/** Идентификатор самого старого сообщения списка (0, если список пуст). */
export function oldestId(list) {
    const first = Array.isArray(list) && list.length > 0 ? list[0] : null;
    return first && first.id != null ? Number(first.id) : 0;
}

/** Идентификатор самого нового сообщения списка (0, если список пуст). */
export function newestId(list) {
    const last = Array.isArray(list) && list.length > 0 ? list[list.length - 1] : null;
    return last && last.id != null ? Number(last.id) : 0;
}

/**
 * Стоит ли лента у нижнего края.
 *
 * ЧИТАЕТСЯ ОДИН РАЗ — в обработчике прокрутки, и результат запоминается
 * защёлкой (`chat.js`). Вычислять это в момент вставки строки нельзя: высота
 * ленты в этот момент уже изменилась, и ответ будет про новую ленту, а не про
 * намерение пользователя.
 *
 * @param {?object} el — узел с прокруткой
 * @param {number} [pad]
 * @returns {boolean}
 */
export function isAtBottom(el, pad = BOTTOM_SLACK_PX) {
    if (!el) return true;
    const left = Number(el.scrollHeight) - Number(el.scrollTop) - Number(el.clientHeight);
    return !Number.isFinite(left) || left <= pad;
}

/**
 * Пора ли догружать историю (пользователь у верхнего края).
 *
 * @param {?object} el
 * @param {number} [px]
 * @returns {boolean}
 */
export function shouldLoadOlder(el, px = OLDER_TRIGGER_PX) {
    if (!el) return false;
    return Number(el.scrollTop) <= px;
}

/**
 * Нужен ли разделитель дня между двумя сообщениями.
 *
 * Сравниваются ЛОКАЛЬНЫЕ даты (`dayKey`), а не UTC: пользователь живёт в своём
 * часовом поясе, и «вчера» для него заканчивается в свою полночь.
 *
 * @param {?object} prev — предыдущее сообщение (null для первого)
 * @param {?object} cur
 * @returns {boolean}
 */
export function needsDateSep(prev, cur) {
    const now = dayKey(cur && cur.date);
    if (!now) return false;
    if (!prev) return true;
    return dayKey(prev.date) !== now;
}

/**
 * Подпись разделителя дня: `сегодня`, `вчера` либо `05.08.2026`.
 *
 * @param {?string} iso
 * @param {number} [nowMs]
 * @returns {string}
 */
export function dateSepLabel(iso, nowMs = Date.now()) {
    const key = dayKey(iso);
    if (!key) return '';
    const today = dayKey(new Date(nowMs).toISOString());
    const yesterday = dayKey(new Date(nowMs - 86400000).toISOString());
    if (key === today) return 'сегодня';
    if (key === yesterday) return 'вчера';
    const [y, m, d] = key.split('-');
    return `${d}.${m}.${y}`;
}

/**
 * Подпись автора строки ленты.
 *
 * У системного сообщения автора нет вовсе — подписывать его именем портала
 * («Битрикс24 пишет: Игорь создал чат») значило бы выдумывать отправителя.
 *
 * @param {?object} msg — MessageDto
 * @returns {string}
 */
export function senderLabel(msg) {
    if (!msg || msg.system) return '';
    const s = msg.sender;
    if (!s) return '';
    const name = String(s.name == null ? '' : s.name).trim();
    return name || String(s.id == null ? '' : s.id);
}

/**
 * Размер файла человеческой строкой.
 *
 * @param {?number} bytes
 * @returns {string} пустая строка, если размера нет
 */
export function formatBytes(bytes) {
    const n = Number(bytes);
    if (!Number.isFinite(n) || n <= 0) return '';
    const units = ['Б', 'КБ', 'МБ', 'ГБ'];
    let value = n;
    let i = 0;
    while (value >= 1024 && i < units.length - 1) {
        value /= 1024;
        i += 1;
    }
    const digits = value < 10 && i > 0 ? 1 : 0;
    return `${value.toFixed(digits)} ${units[i]}`;
}

/**
 * Подпись вида вложения — совпадает со словарём `attach.rs::media_kind`.
 *
 * @param {?string} kind
 * @returns {string}
 */
export function mediaKindLabel(kind) {
    switch (kind) {
        case 'image': return 'изображение';
        case 'video': return 'видео';
        case 'video_note': return 'видеосообщение';
        case 'audio': return 'аудио';
        case 'voice': return 'голосовое';
        default: return 'файл';
    }
}

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

/**
 * Человеческая подпись к коду ошибки бэкенда.
 *
 * Решения принимаются по `code`, а не по тексту: тексты несут дословную
 * формулировку портала и меняются вместе с ним, коды — контракт
 * (plugins/bitrix/src/routes/error.rs).
 *
 * @param {?string} code
 * @returns {string}
 */
export function errorLabel(code) {
    switch (code) {
        case 'not_authorized': return 'Портал не подключён';
        case 'scope_missing': return 'У вебхука не хватает прав';
        case 'unsupported_auth': return 'Метод недоступен входящему вебхуку';
        case 'not_connected': return 'Нет связи с порталом';
        case 'flood_wait': return 'Портал ограничивает частоту запросов';
        case 'invalid_input': return 'Неверные параметры запроса';
        case 'unknown_dialog': return 'Диалог недоступен';
        case 'unknown_message': return 'Сообщение недоступно';
        case 'unknown_task': return 'Задача недоступна';
        case 'send_forbidden': return 'Писать в этот чат нельзя';
        case 'invalid_state': return 'Сейчас это действие невозможно';
        case 'method_unavailable': return 'Портал не поддерживает эту операцию';
        case 'bitrix_error': return 'Портал ответил ошибкой';
        default: return 'Ошибка';
    }
}

// ---------------------------------------------------------------------------
// Медиа (Ф8)
// ---------------------------------------------------------------------------

/**
 * Ключ вложения — АДРЕС КАРТОЧКИ во всех трёх местах сразу.
 *
 * Формат `<dialog>/<message>/<index>` дословно совпадает с
 * `media_cache::CacheKey::id` на бэкенде и с полем `key` кадра
 * `download_progress`. Совпадение обязательно: без него прогресс второго файла
 * сообщения прилетал бы в карточку первого, а ключи в логах двух половин
 * системы не сходились бы при разборе инцидента.
 *
 * ИНДЕКС обязателен и здесь: у сообщения Битрикс24 вложений несколько, и
 * `<dialog>/<message>` развёл бы их по одному ключу — второй objectURL затёр
 * бы первый и утёк бы навсегда.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number|string} index
 * @returns {string}
 */
export function mediaKey(dialogId, messageId, index) {
    const i = Number(index);
    return `${dialogId}/${messageId}/${Number.isFinite(i) ? Math.trunc(i) : 0}`;
}

/**
 * Ключ аватара в том же кэше objectURL.
 *
 * Префикс `avatar/` совпадает с серверным `CacheKey::Avatar::id` и физически не
 * может столкнуться с ключом вложения: у того первый сегмент — идентификатор
 * диалога, то есть цифры либо `chat…`/`sg…`.
 *
 * @param {number|string} userId
 * @returns {string}
 */
export function avatarKey(userId) {
    return `avatar/${userId}`;
}

/**
 * Показывается ли вложение лайтбоксом.
 *
 * @param {?string} kind
 * @returns {boolean}
 */
export function isImageKind(kind) {
    return kind === 'image';
}

/**
 * Играется ли вложение встроенным плеером браузера.
 *
 * `video_note` (кружок) — это обычное видео с другой рамкой, поэтому он тоже
 * здесь: платформа отдаёт его файлом, а не потоком.
 *
 * @param {?string} kind
 * @returns {boolean}
 */
export function isPlayableKind(kind) {
    return kind === 'audio' || kind === 'voice' || kind === 'video' || kind === 'video_note';
}

/**
 * Каким тегом играть вложение: `audio` или `video`.
 *
 * @param {?string} kind
 * @returns {string}
 */
export function playerTag(kind) {
    return (kind === 'video' || kind === 'video_note') ? 'video' : 'audio';
}

/**
 * Процент выполнения либо `null`, если общий размер неизвестен.
 *
 * `null` — не «ноль процентов», а «процентов не существует»: карточка рисует
 * по нему бегущую полосу вместо числа. Ровно тот же смысл, что у флага
 * `indeterminate` в кадре `upload_progress` (Р8).
 *
 * @param {number} received
 * @param {?number} total
 * @returns {?number} 0..100
 */
export function progressPercent(received, total) {
    const t = Number(total);
    const r = Number(received);
    if (!Number.isFinite(t) || t <= 0 || !Number.isFinite(r)) return null;
    return Math.max(0, Math.min(100, Math.round((r / t) * 100)));
}

/**
 * Подпись прогресса: `45% · 1.2 МБ` либо просто объём.
 *
 * @param {number} received
 * @param {?number} total
 * @returns {string}
 */
export function progressLabel(received, total) {
    const pct = progressPercent(received, total);
    const size = formatBytes(received);
    if (pct === null) return size || '…';
    return size ? `${pct}% · ${size}` : `${pct}%`;
}

/**
 * Безопасное имя для `<a download>`.
 *
 * Имя пришло из портала, то есть от чужого сотрудника, а `download` браузеры
 * трактуют как ПОДСКАЗКУ ПУТИ: `../../.bashrc` там недопустим. Берётся только
 * последний сегмент, всё небезопасное заменяется подчёркиванием.
 *
 * @param {?string} raw
 * @returns {string}
 */
export function sanitizeDownloadName(raw) {
    const s = String(raw == null ? '' : raw).trim();
    const last = s.split(/[/\\]/).pop() || '';
    // `\p{L}`/`\p{N}`, а НЕ `\w`: последний в JS означает `[A-Za-z0-9_]`, и
    // кириллическое имя файла превратилось бы в цепочку подчёркиваний. Убираем
    // только то, что опасно в пути: разделители, управляющие символы, кавычки.
    const cleaned = last.replace(/[^\p{L}\p{N}._-]+/gu, '_').replace(/^\.+|\.+$/g, '');
    return cleaned || 'file';
}

// ---------------------------------------------------------------------------
// Отправка файлов (Ф9)
// ---------------------------------------------------------------------------

/**
 * Этапы одной отправки. Первые два дословно совпадают с полем `phase` кадра
 * `upload_progress` (`plugins/bitrix/src/progress.rs`), и это совпадение —
 * контракт: кадр применяется к карточке БЕЗ таблицы перевода.
 *
 * `queued` и `sent`/`failed` — чисто клиентские: у сервера нет ни очереди, ни
 * понятия «отправлено», он отвечает на конкретный запрос.
 */
export const UPLOAD_STAGES = ['queued', 'client', 'portal', 'sent', 'failed'];

/**
 * MIME файла: сначала то, что сказал браузер, иначе догадка по расширению.
 *
 * Догадка нужна не для сервера (он определяет тип сам и заново), а для
 * КАРТОЧКИ: браузер оставляет `type` пустым у файлов с незнакомым расширением
 * и у части перетащенных из архиваторов, и без запасного пути каждый такой
 * файл рисовался бы «файлом» даже будучи картинкой.
 *
 * @param {?string} name
 * @param {?string} type
 * @returns {string}
 */
export function uploadMime(name, type) {
    const declared = String(type || '').trim().toLowerCase();
    if (declared) return declared;
    const ext = String(name || '').toLowerCase().split('.').pop();
    switch (ext) {
        case 'png': return 'image/png';
        case 'jpg':
        case 'jpeg': return 'image/jpeg';
        case 'gif': return 'image/gif';
        case 'webp': return 'image/webp';
        case 'svg': return 'image/svg+xml';
        case 'mp4': return 'video/mp4';
        case 'mov': return 'video/quicktime';
        case 'webm': return 'video/webm';
        case 'mp3': return 'audio/mpeg';
        case 'ogg': return 'audio/ogg';
        case 'wav': return 'audio/wav';
        case 'pdf': return 'application/pdf';
        default: return 'application/octet-stream';
    }
}

/**
 * Вид карточки отправки по MIME.
 *
 * Словарь НАМЕРЕННО беднее серверного `attach.rs::media_kind`: у отправки нет
 * ни `voice`, ни `video_note` — их порождает запись в мессенджере портала, а
 * не выбор файла в браузере. Приписать их здесь значило бы обещать вид
 * вложения, которого после заливки не будет.
 *
 * @param {?string} mime
 * @returns {'image'|'video'|'audio'|'file'}
 */
export function uploadKind(mime) {
    const m = String(mime || '').toLowerCase();
    // SVG отправляется файлом: браузер покажет его миниатюрой, а портал
    // хранит как документ, и «картинка, которая не открывается лайтбоксом»
    // читалась бы как поломка.
    if (m === 'image/svg+xml') return 'file';
    if (m.startsWith('image/')) return 'image';
    if (m.startsWith('video/')) return 'video';
    if (m.startsWith('audio/')) return 'audio';
    return 'file';
}

/**
 * Имя для файла из буфера обмена.
 *
 * Скриншот приезжает либо вовсе без имени, либо общим `image.png` — таким же
 * у каждого следующего. В переписке они сливаются в неразличимый список,
 * поэтому имя собирается из даты и времени.
 *
 * @param {?string} mime
 * @param {Date} [now]
 * @returns {string}
 */
export function pastedFileName(mime, now = new Date()) {
    const p = (n) => String(n).padStart(2, '0');
    const stamp = `${now.getFullYear()}${p(now.getMonth() + 1)}${p(now.getDate())}`
        + `_${p(now.getHours())}${p(now.getMinutes())}${p(now.getSeconds())}`;
    const sub = String(mime || '').toLowerCase().split('/')[1] || 'bin';
    // `image/svg+xml` → `svg`, `application/vnd.…` → без хвоста после `+`/`;`.
    const ext = sub.split('+')[0].split(';')[0].replace(/[^a-z0-9]/g, '') || 'bin';
    return `pasted_${stamp}.${ext}`;
}

/**
 * Подпись состояния карточки отправки.
 *
 * ФАЗА `portal` НЕ ПОКАЗЫВАЕТ ПРОЦЕНТОВ ВООБЩЕ (Р8): заливка в Битрикс24
 * одношаговая (`im.v2.File.upload` с base64 в теле) и обратной связи не даёт.
 * Рисовать здесь число значило бы придумать его.
 *
 * @param {string} stage
 * @param {number} [sent]
 * @param {?number} [total]
 * @returns {string}
 */
export function uploadStageLabel(stage, sent, total) {
    switch (stage) {
        case 'client': {
            const pct = progressPercent(sent, total);
            return pct === null ? 'ПРИЁМ' : `ПРИЁМ ${pct}%`;
        }
        case 'portal': return 'ПОРТАЛ …';
        case 'sent': return 'ОТПРАВЛЕН';
        case 'failed': return 'НЕ ОТПРАВЛЕН';
        default: return 'В ОЧЕРЕДИ';
    }
}

/**
 * Подпись места в очереди: `В ОЧЕРЕДИ 2/3`.
 *
 * Очередь строго последовательная (два параллельных base64-тела по 33 МБ — это
 * 66 МБ в памяти сервера разом), поэтому позиция ОБЯЗАНА быть видна: иначе
 * второй файл выглядит зависшим.
 *
 * @param {number} pos — 1-based
 * @param {number} total
 * @returns {string}
 */
export function uploadQueueLabel(pos, total) {
    const t = Number(total) || 0;
    if (t <= 1) return 'В ОЧЕРЕДИ';
    return `В ОЧЕРЕДИ ${Math.max(1, Number(pos) || 1)}/${t}`;
}

/**
 * Причина отказа ДО отправки либо `null`, если файл годится.
 *
 * Клиентская проверка нужна не вместо серверной, а до неё: гнать 30 МБ по сети
 * ради 413 — это минута ожидания и трафик там, где ответ известен заранее.
 * Значение лимита приходит с сервера (`max_upload_bytes` в `/api/bitrix/status`),
 * а не зашито здесь: разметку и скрипты браузер держит в кэше PWA неделями, и
 * своя копия числа разъехалась бы с конфигом при первом же его изменении.
 *
 * Пустой файл отвергается тоже: портал отвечает на него `FILE_EMPTY`, и узнать
 * об этом после заливки — худший из возможных способов.
 *
 * @param {number} size
 * @param {?number} maxBytes
 * @returns {?string}
 */
export function uploadRejectReason(size, maxBytes) {
    const s = Number(size);
    if (!Number.isFinite(s) || s <= 0) return 'файл пустой';
    const max = Number(maxBytes);
    if (!Number.isFinite(max) || max <= 0) return null;
    if (s > max) {
        return `файл ${formatBytes(s)} больше лимита ${formatBytes(max)}`;
    }
    return null;
}

// ---------------------------------------------------------------------------
// Задачи (Ф10)
// ---------------------------------------------------------------------------

/**
 * Подписи кнопок действий над задачей.
 *
 * Словарь ЗАКРЫТ, но список кнопок — НЕТ: набор действий приезжает с сервера в
 * `task.actions` и меняется вместе со статусом задачи (после «Начать»
 * появляется «Пауза», а сама «Начать» исчезает). Здесь только человеческие
 * подписи к машинным ключам портала; незнакомый ключ рисуется как есть — это
 * лучше, чем спрятать кнопку, которую портал разрешил.
 *
 * Ключи дословно совпадают с `ACTION_METHODS` в plugins/bitrix/src/tasks.rs.
 */
export const TASK_ACTION_LABELS = {
    start: 'Начать',
    pause: 'Пауза',
    complete: 'Завершить',
    defer: 'Отложить',
    renew: 'Возобновить',
    approve: 'Принять',
    disapprove: 'Вернуть',
};

/**
 * Подпись кнопки действия.
 *
 * @param {string} action — ключ действия портала
 * @returns {string}
 */
export function taskActionLabel(action) {
    const key = String(action == null ? '' : action);
    return TASK_ACTION_LABELS[key] || key;
}

/**
 * Строка срока задачи.
 *
 * Просрочку решает СЕРВЕР (`overdue`): у портала нет такого поля вовсе, оно
 * вычисляется как `deadline < now && status < 5`, и второй экземпляр этого
 * правила во фронте разъехался бы с первым при первой же правке.
 *
 * @param {?object} task — TaskDto
 * @param {number} [nowMs]
 * @returns {?string} `null`, если срока нет
 */
export function taskDeadlineLabel(task, nowMs = Date.now()) {
    const iso = task && task.deadline;
    const ms = parseDate(iso);
    if (ms == null) return null;
    const d = new Date(ms);
    const date = `${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')}`;
    const time = formatTime(iso);
    const sameYear = d.getFullYear() === new Date(nowMs).getFullYear();
    const stamp = sameYear ? `${date} ${time}` : `${date}.${d.getFullYear()} ${time}`;
    return task.overdue ? `просрочено ${stamp}` : `до ${stamp}`;
}

/**
 * Карточка задачи для строки списка диалогов.
 *
 * Отдельная карта, а не поле диалога: у задачи бывает НЕ быть чата вовсе (Р20),
 * и вешать карточку на строку списка означало бы терять такие задачи целиком.
 *
 * @param {object} tasks — Map task_id → TaskDto
 * @param {?object} dialog — DialogDto
 * @returns {?object} TaskDto либо null
 */
export function taskOfDialog(tasks, dialog) {
    if (!tasks || !dialog || dialog.kind !== 'task') return null;
    const id = dialog.task_id == null ? '' : String(dialog.task_id);
    if (!id) return null;
    return tasks.get(id) || null;
}

/**
 * Текст подписи «обсуждение не начато» для задачи без чата (Р20).
 *
 * Ветка обязательная, а не редкая: свежесозданная задача часто ещё без чата, и
 * пустая лента без объяснения читается как поломка.
 *
 * @param {?object} task — TaskDto
 * @returns {?string} `null`, если чат есть
 */
export function taskNoChatNote(task) {
    if (!task || task.dialog_id) return null;
    return 'Обсуждение задачи ещё не начато: чат появится после первого комментария в Битрикс24.';
}

/**
 * Сводка карточки одной строкой — для заголовка сессии и для отладки.
 *
 * @param {?object} task — TaskDto
 * @returns {string}
 */
export function taskSummary(task) {
    if (!task) return '';
    const parts = [`#${task.id}`, task.status_label || ''];
    if (task.overdue) parts.push('просрочено');
    if (task.priority_key === 'high') parts.push('высокий приоритет');
    return parts.filter(Boolean).join(' · ');
}
