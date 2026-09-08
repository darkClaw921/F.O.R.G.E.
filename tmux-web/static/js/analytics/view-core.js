// tmux-web — чистая логика дашборда аналитики (Фаза 5).
//
// ЗАЧЕМ ОТДЕЛЬНЫЙ ФАЙЛ. Всё, что в дашборде можно посчитать неправильно —
// границы периода, дельта к предыдущему периоду, подписи бакетов гистограмм,
// доля thinking-токенов — не требует ни DOM, ни сети. Держать это внутри
// `analytics.js` означало бы, что проверить формулу можно только глазами в
// браузере: модуль вью импортирует `core/dom.js`, который на первой же строке
// зовёт `document.getElementById`, и в Node не грузится вовсе.
//
// Поэтому граница проведена так: здесь — арифметика и форматирование, там —
// узлы и запросы. Этот файл грузится в Node как есть и покрыт
// `tests/frontend/analytics_view.test.mjs`.
//
// Ни одного импорта: ни DOM, ни localStorage, ни fetch.

/** Допустимые значения селектора периода. */
export const RANGES = ['7', '30', '90', 'all'];

/** Период по умолчанию — месяц: достаточно для трендов, не превращает ось в частокол. */
export const DEFAULT_RANGE = '30';

/**
 * Приводит значение селектора к одному из [`RANGES`].
 *
 * Мусор из localStorage (в том числе записанный прошлой версией) превращается
 * в умолчание, а не роняет вью.
 *
 * @param {*} range
 * @returns {'7'|'30'|'90'|'all'}
 */
export function normalizeRange(range) {
    const r = String(range === undefined || range === null ? '' : range);
    return RANGES.includes(r) ? r : DEFAULT_RANGE;
}

/** `Date` → `YYYY-MM-DD` в ЛОКАЛЬНОЙ зоне (дни индекса — календарные, не UTC). */
export function dayKey(date) {
    const y = date.getFullYear();
    const m = String(date.getMonth() + 1).padStart(2, '0');
    const d = String(date.getDate()).padStart(2, '0');
    return `${y}-${m}-${d}`;
}

/**
 * Границы запроса для выбранного периода.
 *
 * `all` НЕ отправляет ни `from`, ни `to`: сервер сам подставит весь диапазон
 * индекса. Подставлять сюда «1970-01-01» было бы хуже — предыдущий период для
 * дельт считается от переданных границ, и дельта уехала бы в бессмыслицу.
 *
 * Период «7 дней» — это сегодня и шесть предыдущих дней, а не «сегодня минус
 * семь»: иначе на экране восемь колонок под подписью «7 дней».
 *
 * @param {'7'|'30'|'90'|'all'} range
 * @param {Date} [now] — «сегодня»; параметр ради тестируемости
 * @returns {{from?: string, to?: string}}
 */
export function rangeToQuery(range, now = new Date()) {
    const r = normalizeRange(range);
    if (r === 'all') return {};
    const days = Number(r);
    const to = new Date(now.getTime());
    const from = new Date(now.getTime());
    from.setDate(from.getDate() - (days - 1));
    return { from: dayKey(from), to: dayKey(to) };
}

/**
 * Корзина серий под период.
 *
 * До 90 дней включительно — дни. «Всё время» — недели: на полугодовом архиве
 * дневная ось даёт под две сотни колонок шириной в пиксель, где не видно ни
 * тренда, ни отдельных значений.
 *
 * @param {'7'|'30'|'90'|'all'} range
 * @returns {'day'|'week'}
 */
export function bucketFor(range) {
    return normalizeRange(range) === 'all' ? 'week' : 'day';
}

// ==========================================================================
// Дельты к предыдущему периоду
// ==========================================================================

/**
 * Дельта метрики к предыдущему периоду такой же длины.
 *
 * ПОЧЕМУ НЕ ВСЕГДА ЕСТЬ ДЕЛЬТА. Предыдущий период может быть пустым — так
 * выглядит первая неделя работы или свежий проект. Деление на ноль дало бы
 * «+∞%» или «NaN%», а это не информация, а мусор в плитке. Пустой предыдущий
 * период честнее показать отсутствием дельты.
 *
 * ЗНАК НЕ РАВЕН СМЫСЛУ. Рост стоимости — не «хорошо», падение медианной паузы
 * — не «отлично»: это ровно тот сигнал «не читая соглашаюсь», ради которого
 * дашборд и затевался. Поэтому окраска задаётся метрикой через `better`
 * ('up' — расти хорошо, 'down' — падать хорошо, null — нейтрально), а не
 * знаком разницы.
 *
 * @param {number} current
 * @param {number} previous
 * @param {'up'|'down'|null} [better]
 * @returns {{ratio: number, percent: number, direction: 1|0|-1,
 *            tone: 'good'|'bad'|'neutral', text: string} | null}
 */
export function computeDelta(current, previous, better = null) {
    const cur = Number(current);
    const prev = Number(previous);
    if (!Number.isFinite(cur) || !Number.isFinite(prev)) return null;
    if (prev === 0) return null;
    const ratio = (cur - prev) / Math.abs(prev);
    if (!Number.isFinite(ratio)) return null;
    const percent = Math.round(ratio * 1000) / 10;
    const direction = percent > 0 ? 1 : percent < 0 ? -1 : 0;
    let tone = 'neutral';
    if (direction !== 0 && (better === 'up' || better === 'down')) {
        const good = better === 'up' ? direction > 0 : direction < 0;
        tone = good ? 'good' : 'bad';
    }
    const sign = direction > 0 ? '+' : direction < 0 ? '−' : '';
    const abs = Math.abs(percent);
    const shown = abs >= 100 ? String(Math.round(abs)) : String(abs).replace('.', ',');
    return {
        ratio,
        percent,
        direction,
        tone,
        text: direction === 0 ? 'без изменений' : `${sign}${shown}%`,
    };
}

// ==========================================================================
// Подписи бакетов гистограмм
// ==========================================================================

/** Короткая подпись длительности для оси: «5 с», «1 мин», «5 мин». */
function shortMs(ms) {
    const n = Math.max(0, Number(ms) || 0);
    if (n < 60_000) return `${Math.round(n / 1000)} с`;
    if (n < 3_600_000) {
        const min = n / 60_000;
        return `${Math.round(min * 10) / 10}`.replace('.', ',') + ' мин';
    }
    return `${Math.round((n / 3_600_000) * 10) / 10}`.replace('.', ',') + ' ч';
}

/**
 * Подписи гистограммы по границам: N границ → N+1 подписей.
 *
 * Границы приходят С СЕРВЕРА (`pauseBucketEdgesMs`, `msgLenEdges`) именно
 * затем, чтобы подписи не разъезжались с расчётом. Хардкод «<5 с, 5–15…» на
 * фронте пережил бы смену констант в `rollup.rs` молча — и график показывал бы
 * не то, что подписано.
 *
 * @param {number[]} edges — верхние границы бакетов по возрастанию
 * @param {(v:number)=>string} [fmt] — форматтер значения границы
 * @returns {string[]}
 */
export function bucketLabels(edges, fmt = (v) => String(v)) {
    const list = Array.isArray(edges) ? edges.map(Number).filter(Number.isFinite) : [];
    if (list.length === 0) return [];
    const out = [`<${fmt(list[0])}`];
    for (let i = 1; i < list.length; i += 1) out.push(`${fmt(list[i - 1])}–${fmt(list[i])}`);
    out.push(`>${fmt(list[list.length - 1])}`);
    return out;
}

/** Подписи гистограммы пауз: `<5 с`, `5 с–15 с`, …, `>5 мин`. */
export function pauseBucketLabels(edges) {
    return bucketLabels(edges, shortMs);
}

/** Подписи гистограммы длин сообщений в символах. */
export function msgLenBucketLabels(edges) {
    return bucketLabels(edges, (v) => String(Math.round(v)));
}

// ==========================================================================
// Производные ряды
// ==========================================================================

/**
 * Поэлементное отношение двух рядов, 0..1.
 *
 * Нулевой знаменатель даёт 0, а не NaN: день без вывода — это «доли нет», и
 * дыра в линии читалась бы как потерянные данные.
 *
 * @param {number[]} num
 * @param {number[]} den
 * @returns {number[]}
 */
export function ratioSeries(num, den) {
    const a = Array.isArray(num) ? num : [];
    const b = Array.isArray(den) ? den : [];
    const n = Math.max(a.length, b.length);
    const out = [];
    for (let i = 0; i < n; i += 1) {
        const d = Number(b[i]) || 0;
        const v = Number(a[i]) || 0;
        out.push(d > 0 ? v / d : 0);
    }
    return out;
}

/**
 * Есть ли в ответе вообще что показывать.
 *
 * Пустой период — штатная ситуация (человек не работал, либо выбран проект без
 * активности), и вью обязан объяснить это словами, а не пустой сеткой из
 * карточек с надписью «нет данных» в каждой.
 *
 * @param {Object} summary — ответ `GET /api/analytics/summary`
 * @returns {boolean}
 */
export function isEmptySummary(summary) {
    const s = (summary && summary.series) || {};
    const kpi = (summary && summary.kpi) || {};
    const len = (v) => (Array.isArray(v) ? v.length : 0);
    return (
        len(s.buckets) === 0 &&
        len(s.tools) === 0 &&
        len(s.models) === 0 &&
        len(s.projects) === 0 &&
        !(Number(kpi.humanMessages) > 0)
    );
}

/**
 * Прогресс индексации в процентах 0..100.
 *
 * `total === 0` в самом начале прохода — это «сколько файлов, ещё неизвестно»,
 * а не «готово»: показываем 0, иначе полоса на долю секунды прыгает в конец.
 *
 * @param {{done?: number, total?: number}} progress
 * @returns {number}
 */
export function progressPercent(progress) {
    const p = progress || {};
    const total = Number(p.total) || 0;
    const done = Number(p.done) || 0;
    if (total <= 0) return 0;
    const pct = (done / total) * 100;
    return Math.max(0, Math.min(100, Math.round(pct)));
}

/**
 * Что рисовать по ответу API: данные или прогресс-бар.
 *
 * ГЛАВНАЯ РАЗВИЛКА ВЬЮ. Пока индекса нет вообще (`idle`/`scanning`) — только
 * прогресс: рисовать нули было бы враньём. Но `ready` при идущем фоновом
 * доборе (`progress.running`) — это ПОЛНОЦЕННЫЕ данные: индекс поднят из
 * кэша, добираются лишь хвосты дописанных файлов. Подменять их прогресс-баром
 * значило бы прятать готовый дашборд ради технической подробности, поэтому
 * такой случай рисует данные и лишь помечает, что цифры сейчас обновятся.
 *
 * Поллинг нужен в обоих случаях: и чтобы дождаться первого индекса, и чтобы
 * перерисовать дашборд по окончании фонового прохода.
 *
 * @param {{state?: string, progress?: Object}} summary
 * @returns {{mode: 'progress'|'data', polling: boolean, refreshing: boolean}}
 */
export function viewMode(summary) {
    const state = String((summary && summary.state) || '');
    const running = !!(summary && summary.progress && summary.progress.running);
    if (state !== 'ready') {
        return { mode: 'progress', polling: true, refreshing: false };
    }
    return { mode: 'data', polling: running, refreshing: running };
}

/**
 * Транспонирует матрицу активности из формата бэкенда в формат чарта.
 *
 * Бэкенд отдаёт `[час][день недели]` (24×7) — так её дешевле накапливать в
 * дневном бакете. Тепловая карта кита рисует СТРОКАМИ дни недели, а
 * столбцами часы (7×24), как календарь. Разворот живёт здесь, а не в чарте:
 * это про формат данных, а не про геометрию.
 *
 * Часы уже в локальной зоне машины — бэкенд раскладывает таймстемпы
 * транскриптов (UTC) по локальному времени, и фронт не пересчитывает их
 * повторно.
 *
 * @param {number[][]} hourXWeekday — 24 строки по 7 чисел
 * @returns {number[][]} 7 строк по 24 числа; при пустом входе — нули
 */
export function weekdayHourMatrix(hourXWeekday) {
    const src = Array.isArray(hourXWeekday) ? hourXWeekday : [];
    const out = [];
    for (let d = 0; d < 7; d += 1) {
        const row = [];
        for (let h = 0; h < 24; h += 1) {
            const cell = Array.isArray(src[h]) ? Number(src[h][d]) : 0;
            row.push(Number.isFinite(cell) ? cell : 0);
        }
        out.push(row);
    }
    return out;
}

/**
 * Есть ли в матрице активности хоть одно ненулевое значение.
 *
 * Ровная карта нулей — валидное состояние (человек не писал за период), но
 * рисовать её как «данные» нельзя: получилась бы карта равномерного цвета,
 * которую легко принять за равномерную активность.
 *
 * @param {number[][]} matrix
 * @returns {boolean}
 */
export function hasHeat(matrix) {
    if (!Array.isArray(matrix)) return false;
    return matrix.some((row) => Array.isArray(row) && row.some((v) => Number(v) > 0));
}

