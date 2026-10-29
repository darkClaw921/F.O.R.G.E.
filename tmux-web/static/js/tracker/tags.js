// tmux-web — плагин forge-tracker: чистая логика редактора тегов карточки.
//
// ТЕГА КАК СУЩНОСТИ В МОДЕЛИ НЕТ. Это важно знать до чтения кода ниже, иначе
// половина файла выглядит недоделанной. В `plugins/tracker/src/models.rs` тег
// живёт РОВНО как строка в `task.tags: Vec<String>` — ни таблицы тегов, ни
// идентификаторов, ни эндпоинтов `/api/tracker/tags` не существует и заводить
// их не планировалось. Отсюда два следствия, на которых стоит весь модуль:
//
//   1. «СОЗДАТЬ НОВЫЙ ТЕГ» — ЭТО ПРОСТО ДОБАВИТЬ РАНЕЕ НЕ ВСТРЕЧАВШУЮСЯ
//      СТРОКУ В `task.tags` ЭТОЙ ЗАДАЧИ. Никакого CRUD за пунктом «создать
//      новый: #имя» не стоит и искать его не нужно: пункт нужен человеку как
//      подтверждение («да, я понимаю, что такого тега ещё не было»), а не
//      системе как отдельная операция. Уходит наружу всё тем же PATCH
//      `{tags: [...]}`, что и раньше;
//   2. СПИСОК ИЗВЕСТНЫХ ТЕГОВ — ЭТО ПРОИЗВОДНАЯ ОТ ЗАДАЧ. Единственный
//      источник — снимок `/api/tracker/state`: те же теги, что питают блок
//      `// TAGS` в сайдбаре (`main.js`, фильтр `local.tag`). Второй источник
//      (свой запрос, свой кеш, свой стор) здесь намеренно НЕ заводится: две
//      копии одного списка неминуемо разъезжаются, и подсказка предлагала бы
//      тег, которого в сайдбаре уже нет. Отсюда `knownTags(tasks)` —
//      пересчёт из снимка на каждое открытие карточки.
//
// ПОЧЕМУ ОТДЕЛЬНЫЙ ФАЙЛ. Ровно по той же причине, что и `fields.js`: карточка
// (`modals.js`) тянет `../core/dom.js`, который дёргает `getElementById` на
// уровне модуля, — значит в голом node она неимпортируема и НИЧТО из неё не
// покрывается тестом. А нормализация, дедуп и выбор подсказок — это как раз
// то место, где ошибка тихая: задвоенный `##forge`, потерянный порядок или
// дубль `Forge`/`forge` не падают, они просто портят данные. Поэтому тут нет
// ни DOM, ни сети, и всё покрыто tests/frontend/tracker_tags.test.mjs.
//
// РЕГИСТР: СРАВНИВАЕМ БЕЗ УЧЁТА, ХРАНИМ КАК НАБРАНО. `Forge` и `forge` — один
// тег (иначе сайдбар показывал бы две строки, а фильтр — половину задач), но
// подменять уже сохранённое написание на «канонное» мы не вправе: тег мог
// прийти из prompt-строки, из br или из чужого клиента, и молча переписать
// его регистр значит изменить чужие данные ради косметики.

/** Сколько подсказок показываем разом. Список должен читаться, а не листаться. */
export const SUGGEST_LIMIT = 8;

/**
 * Строка → тег модели.
 *
 * Ведущие `#` срезаются ВСЕ (человек набирает `#`, а поле уже показывает
 * решётку у чипа — иначе получилось бы `##forge`), пробелы по краям тоже:
 * внутри тега пробел невозможен, он разделитель (см. `splitTags`).
 *
 * @param {*} raw
 * @returns {string} тег без решётки или `''`, если добавлять нечего
 */
export function normalizeTag(raw) {
    return String(raw == null ? '' : raw).trim().replace(/^#+/, '').trim();
}

/**
 * Разбор произвольной строки в список тегов: `#a, b  #c` → `['a','b','c']`.
 *
 * Нужен в двух местах: вставка из буфера в поле ввода и совместимость
 * `buildPatch` со старой формой поля (одна строка `#тег #тег`).
 *
 * @param {*} raw
 * @returns {string[]}
 */
export function splitTags(raw) {
    return String(raw == null ? '' : raw)
        .split(/[\s,]+/)
        .map(normalizeTag)
        .filter(Boolean);
}

/**
 * Есть ли тег в списке (без учёта регистра).
 *
 * @param {string[]} list
 * @param {string} tag
 */
export function hasTag(list, tag) {
    const key = normalizeTag(tag).toLowerCase();
    if (!key) return false;
    return (Array.isArray(list) ? list : []).some((t) => normalizeTag(t).toLowerCase() === key);
}

/**
 * Список + тег. ЧИСТАЯ: исходный массив не меняется.
 *
 * Пустое значение и дубликат (без учёта регистра) не добавляются — функция
 * возвращает НОВЫЙ массив с тем же содержимым, а `added: false` объясняет
 * вызывающему, почему ничего не появилось (поле ввода в этом случае всё
 * равно очищается: человек «уже добавил» этот тег).
 *
 * Порядок сохраняется: новый тег уходит в конец, существующие не двигаются.
 *
 * @param {string[]} list
 * @param {*} raw
 * @returns {{tags: string[], added: boolean, tag: string}}
 */
export function addTag(list, raw) {
    const base = normalizeList(list);
    const tag = normalizeTag(raw);
    if (!tag || hasTag(base, tag)) return { tags: base, added: false, tag };
    return { tags: [...base, tag], added: true, tag };
}

/**
 * Список без тега по индексу (крестик на чипе). ЧИСТАЯ.
 *
 * @param {string[]} list
 * @param {number} index
 * @returns {string[]}
 */
export function removeTagAt(list, index) {
    const base = normalizeList(list);
    const i = Number(index);
    if (!Number.isInteger(i) || i < 0 || i >= base.length) return base;
    return base.filter((_, k) => k !== i);
}

/** Список без последнего тега (Backspace на пустом вводе). ЧИСТАЯ. */
export function popTag(list) {
    const base = normalizeList(list);
    return base.slice(0, Math.max(0, base.length - 1));
}

/**
 * Все теги, которые вообще встречаются в снимке, по алфавиту.
 *
 * Тот же расчёт, что у блока `// TAGS` в сайдбаре (`main.js`), и намеренно
 * тот же источник — задачи снимка. Регистр первого встреченного написания
 * сохраняется, повтор в другом регистре отбрасывается: в подсказках `Forge`
 * и `forge` двумя строками — это предложение завести дубль.
 *
 * @param {Array<object>} tasks `snapshot.tasks`
 * @returns {string[]}
 */
export function knownTags(tasks) {
    const seen = new Map();
    (Array.isArray(tasks) ? tasks : []).forEach((t) => {
        const list = t && Array.isArray(t.tags) ? t.tags : [];
        list.forEach((raw) => {
            const tag = normalizeTag(raw);
            if (!tag) return;
            const key = tag.toLowerCase();
            if (!seen.has(key)) seen.set(key, tag);
        });
    });
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
}

/**
 * Что показать в выпадающем списке под полем ввода.
 *
 * Уже навешенные теги из подсказок ВЫЧЕРКНУТЫ: предлагать то, что и так
 * висит чипом, — предлагать ничего не делающее действие.
 *
 * Совпадения с начала идут выше совпадений внутри слова (набирая `for`,
 * человек ждёт сперва `forge`, а не `refactor`), внутри группы — алфавит.
 *
 * Пункт «создать новый» (`create`) появляется, только когда набранного тега
 * НЕТ ни среди известных, ни среди уже навешенных. Ещё раз, чтобы не искать
 * несуществующее: «создать» здесь означает лишь «добавить новую строку в
 * `task.tags`» — отдельной сущности тега в модели нет.
 *
 * @param {string[]} known все известные теги (`knownTags`)
 * @param {*} query сырой ввод поля (можно с `#`)
 * @param {string[]} current уже навешенные на задачу теги
 * @param {number} [limit]
 * @returns {{items: string[], create: (string|null), query: string}}
 */
export function suggestTags(known, query, current, limit) {
    const q = normalizeTag(query);
    const key = q.toLowerCase();
    const chosen = normalizeList(current);
    const max = Number.isInteger(limit) && limit > 0 ? limit : SUGGEST_LIMIT;

    const pool = normalizeList(known).filter((t) => !hasTag(chosen, t));
    const matched = key
        ? pool.filter((t) => t.toLowerCase().includes(key))
        : pool.slice();
    matched.sort((a, b) => {
        if (key) {
            const pa = a.toLowerCase().startsWith(key) ? 0 : 1;
            const pb = b.toLowerCase().startsWith(key) ? 0 : 1;
            if (pa !== pb) return pa - pb;
        }
        return a.localeCompare(b);
    });

    const exists = !key
        || hasTag(known, q)
        || hasTag(chosen, q);
    return { items: matched.slice(0, max), create: exists ? null : q, query: q };
}

/**
 * Индекс подсказки после нажатия стрелки.
 *
 * Список замкнут в кольцо (внизу списка ↓ возвращает наверх): в списке из
 * трёх строк упереться в край и не понять почему — хуже, чем прокрутиться.
 * `-1` означает «ничего не выделено» и возможен только при пустом списке.
 *
 * @param {number} index текущий индекс
 * @param {number} delta `+1` / `-1`
 * @param {number} count длина списка
 * @returns {number}
 */
export function moveSuggest(index, delta, count) {
    const n = Number(count);
    if (!Number.isInteger(n) || n <= 0) return -1;
    const i = Number.isInteger(Number(index)) ? Number(index) : -1;
    const d = Number(delta) < 0 ? -1 : 1;
    if (i < 0) return d > 0 ? 0 : n - 1;
    return ((i + d) % n + n) % n;
}

/** Копия списка: нормализованные непустые строки, порядок и регистр как были. */
function normalizeList(list) {
    return (Array.isArray(list) ? list : []).map(normalizeTag).filter(Boolean);
}
