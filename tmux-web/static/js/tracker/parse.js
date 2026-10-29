// tmux-web — плагин forge-tracker: разбор строки нижнего prompt'а `tracker:~$`.
//
// Грамматика взята из макета (~/Documents/ForgeTracker.pen, экраны list и
// «Состояния и перенос»):
//
//     Свести дизайн трекера !1 #forge ~4ч @work due:18:00
//
//   !N        приоритет 0..4 (0 — самый высокий, как у `br` и TODO хоста)
//   #тег      тег; их может быть несколько
//   ~2ч       оценка; единицы ч/h/час и м/m/мин, дробные через точку или
//             запятую; голое число — минуты
//   @список   список (TaskList)
//   due:…     срок: HH:MM, YYYY-MM-DD, YYYY-MM-DDTHH:MM, today/tomorrow,
//             сегодня/завтра
//
// Остаток строки — заголовок задачи.
//
// ЧЕТЫРЕ РЕШЕНИЯ, КОТОРЫЕ ЗДЕСЬ ЗАФИКСИРОВАНЫ
//
// 1. НЕИЗВЕСТНЫЙ ТОКЕН ОСТАЁТСЯ В ЗАГОЛОВКЕ. `!7`, `~абв`, `due:никогда`,
//    одинокие `#`, `@` и `~` возвращаются в текст как есть. Молчаливая
//    потеря куска фразы хуже любого «не понял»: человек печатает задачу, а не
//    заполняет форму, и `C#` в заголовке обязан выжить (токеном считается
//    только слово, НАЧИНАЮЩЕЕСЯ со значащего символа).
//
// 2. ТОКЕН — ЭТО ЦЕЛОЕ СЛОВО. Строка режется по пробелам, и `x!1` тегом или
//    приоритетом не становится. Иначе адреса, хеши и смайлики в заголовке
//    разбирались бы как разметка.
//
// 3. ПОВТОР ПОБЕЖДАЕТ ПОСЛЕДНИМ. `!2 … !0` даёт приоритет 0. Так ведёт себя
//    правка уже набранной строки: человек дописывает исправление в конец, а
//    не ищет старый токен.
//
// 4. СПИСОК ИЗ `@…` СОЗДАЁТСЯ, ЕСЛИ ЕГО НЕТ. Решение принято здесь и
//    задокументировано намеренно (критерий приёмки задачи требует его
//    зафиксировать): альтернатива — молча положить задачу в inbox, то есть
//    выполнить не то, что написал человек. Создание списка обратимо одним
//    удалением, потеря адресата — нет. Сам вызов делает js/tracker/main.js:
//    этот модуль сетевых операций не выполняет и возвращает только ИМЯ.
//
// Модуль чистый: ни DOM, ни fetch, ни глобального состояния — поэтому его
// целиком покрывает node-тест tmux-web/tests/frontend/tracker_parse.test.mjs.

/** Максимальный приоритет (совпадает с MAX_PRIORITY в models.rs). */
export const MAX_PRIORITY = 4;

/** Единицы оценки: сколько минут в одной единице. */
const ESTIMATE_UNITS = [
    [['ч', 'час', 'часа', 'часов', 'h', 'hr', 'hrs', 'hour', 'hours'], 60],
    [['м', 'мин', 'минут', 'минуты', 'm', 'min', 'mins', 'minute', 'minutes'], 1],
    [['д', 'дн', 'день', 'дня', 'дней', 'd', 'day', 'days'], 8 * 60],
];

/**
 * Оценка из тела токена `~…`.
 *
 * @param {string} body — то, что идёт после `~`
 * @returns {number|null} минуты (>0) или `null`, если разобрать нельзя
 */
export function parseEstimate(body) {
    const m = /^(\d+(?:[.,]\d+)?)\s*([A-Za-zА-Яа-яЁё]*)$/.exec(String(body || '').trim());
    if (!m) return null;
    const value = parseFloat(m[1].replace(',', '.'));
    if (!Number.isFinite(value) || value <= 0) return null;
    const unit = m[2].toLowerCase();
    // Голое число — минуты: «~90» читается как полтора часа, а не как 90
    // часов. Это то же соглашение, что у estimate_min в модели.
    let perUnit = unit === '' ? 1 : null;
    if (perUnit === null) {
        for (const [names, minutes] of ESTIMATE_UNITS) {
            if (names.includes(unit)) { perUnit = minutes; break; }
        }
    }
    if (perUnit === null) return null;
    const total = Math.round(value * perUnit);
    return total > 0 ? total : null;
}

/** Двузначное число строкой. */
const pad2 = (n) => String(n).padStart(2, '0');

/** Локальные дата-время → RFC 3339 в UTC (сервер принимает и то, и другое). */
function localToRfc3339(date) {
    return new Date(date.getTime() - date.getMilliseconds()).toISOString().replace(/\.\d{3}Z$/, 'Z');
}

/** Голая дата локального дня в формате YYYY-MM-DD. */
function localDate(date) {
    return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`;
}

/**
 * Срок из тела токена `due:…`.
 *
 * Возвращает либо голую дату `YYYY-MM-DD` (когда время суток не названо —
 * Гантт рисует по дням, и выдумывать полночь незачем), либо полный RFC 3339.
 *
 * @param {string} body — то, что идёт после `due:`
 * @param {Date} [now] — «сейчас» (в тестах задаётся явно)
 * @returns {string|null}
 */
export function parseDue(body, now) {
    const raw = String(body || '').trim();
    if (!raw) return null;
    const base = now instanceof Date ? new Date(now.getTime()) : new Date();
    const word = raw.toLowerCase();

    if (word === 'today' || word === 'сегодня') return localDate(base);
    if (word === 'tomorrow' || word === 'завтра') {
        const d = new Date(base.getTime());
        d.setDate(d.getDate() + 1);
        return localDate(d);
    }

    // HH:MM — сегодняшний день. Дату НЕ переносим на завтра, даже если время
    // уже прошло: «due:09:00», набранное в 10 утра, чаще значит просроченное
    // сегодня, чем запланированное на завтра, и просроченное видно сразу.
    let m = /^(\d{1,2}):(\d{2})$/.exec(raw);
    if (m) {
        const h = Number(m[1]);
        const min = Number(m[2]);
        if (h > 23 || min > 59) return null;
        const d = new Date(base.getFullYear(), base.getMonth(), base.getDate(), h, min, 0, 0);
        return localToRfc3339(d);
    }

    // YYYY-MM-DD[THH:MM] (разделитель `T` или пробела здесь быть не может —
    // строка уже разрезана по пробелам).
    m = /^(\d{4})-(\d{2})-(\d{2})(?:[Tt](\d{1,2}):(\d{2}))?$/.exec(raw);
    if (m) {
        const y = Number(m[1]);
        const mo = Number(m[2]);
        const day = Number(m[3]);
        if (mo < 1 || mo > 12 || day < 1 || day > 31) return null;
        if (m[4] === undefined) return `${m[1]}-${m[2]}-${m[3]}`;
        const h = Number(m[4]);
        const min = Number(m[5]);
        if (h > 23 || min > 59) return null;
        const d = new Date(y, mo - 1, day, h, min, 0, 0);
        return localToRfc3339(d);
    }

    // DD.MM[.YYYY] — привычная запись; год по умолчанию текущий.
    m = /^(\d{1,2})\.(\d{1,2})(?:\.(\d{4}))?$/.exec(raw);
    if (m) {
        const day = Number(m[1]);
        const mo = Number(m[2]);
        const y = m[3] ? Number(m[3]) : base.getFullYear();
        if (mo < 1 || mo > 12 || day < 1 || day > 31) return null;
        return `${y}-${pad2(mo)}-${pad2(day)}`;
    }

    return null;
}

/**
 * Разбор строки prompt'а.
 *
 * @param {string} line — то, что набрано в строке
 * @param {Date} [now] — «сейчас» для относительных сроков (в тестах задаётся)
 * @returns {{
 *   ok: boolean, title: string, priority: (number|null), tags: string[],
 *   estimate_min: (number|null), list: (string|null), due: (string|null),
 *   unknown: string[]
 * }} `ok:false` — заголовок пуст, задачу создавать нечем.
 */
export function parsePrompt(line, now) {
    const out = {
        ok: false,
        title: '',
        priority: null,
        tags: [],
        estimate_min: null,
        list: null,
        due: null,
        /** Токены, которые выглядели разметкой, но не разобрались. */
        unknown: [],
    };
    const src = String(line == null ? '' : line);
    if (!src.trim()) return out;

    const words = src.trim().split(/\s+/);
    const rest = [];

    for (const w of words) {
        const head = w[0];

        if (head === '!' && w.length > 1) {
            const n = /^!(\d)$/.exec(w);
            if (n && Number(n[1]) <= MAX_PRIORITY) {
                out.priority = Number(n[1]);
                continue;
            }
            out.unknown.push(w);
            rest.push(w);
            continue;
        }

        if (head === '#' && w.length > 1) {
            const tag = w.slice(1);
            // Тег — слово, а не «всё после решётки»: `#1`, `#форж`, `#bug-1`
            // законны, `#!!!` нет.
            if (/^[\wА-Яа-яЁё][\wА-Яа-яЁё-]*$/.test(tag)) {
                if (!out.tags.includes(tag)) out.tags.push(tag);
                continue;
            }
            out.unknown.push(w);
            rest.push(w);
            continue;
        }

        if (head === '@' && w.length > 1) {
            // Имя списка берём как есть: пробелов в нём быть не может по
            // построению (строка уже разрезана по пробелам), а всё остальное
            // — дело пользователя.
            out.list = w.slice(1);
            continue;
        }

        if (head === '~' && w.length > 1) {
            const est = parseEstimate(w.slice(1));
            if (est !== null) {
                out.estimate_min = est;
                continue;
            }
            out.unknown.push(w);
            rest.push(w);
            continue;
        }

        const lower = w.toLowerCase();
        if (lower.startsWith('due:') && w.length > 4) {
            const due = parseDue(w.slice(4), now);
            if (due !== null) {
                out.due = due;
                continue;
            }
            out.unknown.push(w);
            rest.push(w);
            continue;
        }

        rest.push(w);
    }

    out.title = rest.join(' ').trim();
    out.ok = out.title.length > 0;
    return out;
}

/**
 * Тело `POST /api/tracker/tasks` из результата разбора.
 *
 * Незаданные поля НЕ кладутся в тело вовсе: сервер применит свои умолчания
 * (приоритет 2, статус inbox), а `null` в JSON он трактовал бы как явный
 * сброс.
 *
 * @param {object} parsed — результат {@link parsePrompt}
 * @param {string|null} [listId] — уже разрешённый id списка (`@…` → TaskList)
 * @param {string|null} [status] — колонка канбана, если её навязывает фильтр
 */
export function toCreatePayload(parsed, listId, status) {
    const body = { title: parsed.title };
    if (parsed.priority !== null) body.priority = parsed.priority;
    if (parsed.tags.length) body.tags = parsed.tags;
    if (parsed.estimate_min !== null) body.estimate_min = parsed.estimate_min;
    if (parsed.due !== null) body.due = parsed.due;
    if (listId) body.list_id = listId;
    if (status) body.status = status;
    return body;
}

/**
 * Человекочитаемое превью разбора — то, что показывается НАД строкой ввода до
 * нажатия CREATE.
 *
 * @returns {Array<{key: string, value: string}>} пары для отрисовки
 */
export function previewOf(parsed) {
    const out = [];
    if (parsed.title) out.push({ key: 'title', value: parsed.title });
    if (parsed.priority !== null) out.push({ key: 'prio', value: `P${parsed.priority}` });
    if (parsed.list) out.push({ key: 'list', value: parsed.list });
    if (parsed.tags.length) out.push({ key: 'tags', value: parsed.tags.map((t) => `#${t}`).join(' ') });
    if (parsed.estimate_min !== null) out.push({ key: 'est', value: formatEstimate(parsed.estimate_min) });
    if (parsed.due !== null) out.push({ key: 'due', value: parsed.due });
    if (parsed.unknown.length) out.push({ key: 'as text', value: parsed.unknown.join(' ') });
    return out;
}

/** Минуты → `~4ч`, `~1ч30м`, `~45м` (форма из макета). */
export function formatEstimate(minutes) {
    const total = Math.max(0, Math.round(Number(minutes) || 0));
    if (!total) return '—';
    const h = Math.floor(total / 60);
    const m = total % 60;
    if (h && m) return `~${h}ч${m}м`;
    if (h) return `~${h}ч`;
    return `~${m}м`;
}
