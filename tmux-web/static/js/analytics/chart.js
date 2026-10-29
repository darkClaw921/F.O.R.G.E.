// tmux-web — SVG-кит дашборда аналитики (Фаза 4).
//
// ЗАЧЕМ СВОЙ КИТ. В devforge нет ни одной chart-библиотеки и заводить её
// нельзя: весь фронт едет внутрь бинаря через rust-embed, а сторонний рантайм
// (десятки килобайт, свой DOM-слой, свои цвета) сломал бы и размер, и главное —
// тему. Тема здесь пользовательская: 11 переменных из `themes.rs` меняются
// одним `setProperty` на `<html>`, и график обязан перекраситься вместе с
// остальным UI. Библиотека с собственной палитрой этого не умеет.
//
// ПРАВИЛА ЭТОГО ФАЙЛА (нарушение любого — регресс, тест это ловит):
//
//   • узлы ТОЛЬКО через `createElementNS`; `innerHTML` не встречается нигде;
//   • ни одного хардкод-цвета: всё через CSS-переменные темы, последний
//     рубеж — `currentColor`, а не хекс;
//   • отзывчивость через `viewBox` + `preserveAspectRatio`: SVG тянется на
//     100% ширины контейнера, внутренние координаты — «единицы графика»,
//     пиксели в разметку не попадают;
//   • три обязательных состояния у КАЖДОГО чарта: пустой ряд (заглушка «нет
//     данных»), одна точка, все значения нули. Ни NaN в атрибутах, ни
//     отрицательных координат.
//
// ЦВЕТА СЕРИЙ. Порядок слотов не случайный: `--accent → --warn →
// --agent-accent → --success → --danger` проверен валидатором палитры
// (адаптеры протанопии/дейтеранопии, ΔE OKLab): худшая СОСЕДНЯЯ пара даёт
// ΔE 10.9 при дейтеранопии и 33.1 при обычном зрении — обе выше порога 8/15.
// Перестановка слотов ухудшает цифры, поэтому порядок менять нельзя. Больше
// пяти серий кит не раскрашивает НИКОГДА (генерировать шестой оттенок — прямой
// путь к неразличимой паре): хвост сворачивается в «Другое» приглушённым
// цветом, см. `foldSeries`. Тема вправе задать свои слоты `--chart-1…--chart-5`
// — они имеют приоритет.
//
// ИДЕНТИЧНОСТЬ НЕ ТОЛЬКО ЦВЕТОМ. У каждого чарта с двумя и более сериями есть
// легенда, а у каждой фигуры — `<title>` с подписью и значением: цвет остаётся
// вспомогательным каналом, а не единственным.
//
// ТЕСТИРУЕМОСТЬ. Ни `document`, ни `getComputedStyle` не берутся из глобалей
// напрямую: документ — это `container.ownerDocument`, окно —
// `ownerDocument.defaultView`. Поэтому весь кит рисуется на крошечном фейковом
// DOM в `tests/frontend/analytics_chart.test.mjs` без jsdom.

const SVG_NS = 'http://www.w3.org/2000/svg';

/** Максимум цветных серий. Всё сверх — «Другое». */
export const SERIES_MAX = 5;

/**
 * Слоты палитры серий в порядке назначения.
 * Индекс i сначала ищется как `--chart-{i+1}`, потом падает на этот список.
 */
export const SERIES_TOKENS = ['--accent', '--warn', '--agent-accent', '--success', '--danger'];

/** Геометрия по умолчанию в единицах viewBox. */
const DEFAULT_WIDTH = 720;
const DEFAULT_HEIGHT = 260;
/** Пропорция и нижняя граница высоты, когда её считают по ширине. */
const DEFAULT_RATIO = 260 / 720;
const MIN_HEIGHT = 170;

/**
 * Фактическая ширина контейнера в CSS-пикселях, либо 0.
 *
 * `clientWidth` есть только у живого документа: в тестах на фейковом DOM его
 * нет, и тогда работает фолбэк на `opts.width`/`DEFAULT_WIDTH`.
 */
function measureWidth(container) {
    if (!container) return 0;
    const w = Number(container.clientWidth);
    return Number.isFinite(w) && w > 0 ? Math.round(w) : 0;
}
const DEFAULT_PAD = { top: 16, right: 16, bottom: 30, left: 52 };

/** Зазор между заливками (полосы, сегменты стека) — «дышащая» подложка. */
const GAP = 2;

// ==========================================================================
// DOM-примитивы
// ==========================================================================

/** Документ контейнера. Глобальный `document` — только последний рубеж. */
function docOf(container, opts) {
    if (opts && opts.doc) return opts.doc;
    if (container && container.ownerDocument) return container.ownerDocument;
    if (typeof document !== 'undefined') return document;
    return null;
}

/** Окно контейнера — источник `getComputedStyle`. */
function viewOf(container, opts) {
    if (opts && opts.win) return opts.win;
    const doc = docOf(container, opts);
    if (doc && doc.defaultView) return doc.defaultView;
    if (typeof window !== 'undefined') return window;
    return null;
}

/**
 * Узел SVG. Единственная точка создания элементов во всём ките.
 *
 * @param {Document} doc
 * @param {string} tag
 * @param {Object<string,string|number>} [attrs]
 * @returns {Element}
 */
export function node(doc, tag, attrs) {
    const n = doc.createElementNS(SVG_NS, tag);
    if (attrs) {
        for (const k of Object.keys(attrs)) {
            const v = attrs[k];
            if (v === undefined || v === null) continue;
            n.setAttribute(k, String(v));
        }
    }
    return n;
}

/** Текстовый узел SVG с подписью. */
function text(doc, str, attrs) {
    const n = node(doc, 'text', attrs);
    n.textContent = String(str);
    return n;
}

/** Очистка контейнера без `innerHTML`. */
export function clear(el) {
    if (!el) return;
    while (el.firstChild) el.removeChild(el.firstChild);
}

/**
 * Подсказка: `<title>` внутри фигуры + класс подсветки при наведении.
 *
 * `<title>` работает и без JS (в том числе в скриншоте страницы), класс
 * `is-hover` даёт CSS зацепку для подсветки. Обработчики навешиваются только
 * если DOM их поддерживает — фейковый документ теста их не имеет.
 */
export function tip(doc, target, label) {
    if (!target || label === undefined || label === null || label === '') return target;
    const t = node(doc, 'title');
    t.textContent = String(label);
    target.appendChild(t);
    if (typeof target.addEventListener === 'function' && target.classList) {
        target.addEventListener('mouseenter', () => target.classList.add('is-hover'));
        target.addEventListener('mouseleave', () => target.classList.remove('is-hover'));
    }
    return target;
}

// ==========================================================================
// Слой наведения: вертикаль по ближайшей корзине + подсказка со всеми рядами
// ==========================================================================

/**
 * ПОЧЕМУ ОТДЕЛЬНЫЙ СЛОЙ, А НЕ `<title>` НА ФИГУРАХ.
 *
 * Нативная подсказка SVG требует попасть курсором в саму фигуру — в точку
 * линии радиусом 5px при тридцати днях на графике — и показывается браузером
 * через ~секунду. На практике это читается как «при наведении ничего не
 * происходит». Плюс она называет ОДИН ряд, тогда как вопрос к дневному графику
 * почти всегда «что было в этот день у всех рядов сразу».
 *
 * Слой ловит движение по всей области данных, привязывается к ближайшей
 * корзине по X и рисует вертикаль, точки на рядах и коробку со значениями.
 * `<title>` на фигурах остаётся: он работает без мыши — для скринридера.
 *
 * @param {Object} ctx — контекст `frame()`
 * @param {Object} spec
 * @param {number} spec.len — число корзин
 * @param {(i:number)=>number} spec.xAt — центр корзины по X
 * @param {(i:number)=>{title:string,items:Array<{color:string,label:string,
 *        value:string,y?:number}>}} spec.rowAt — что показать для корзины
 */
function hoverLayer(ctx, spec) {
    const { doc, over, colors, x0, y0, x1, y1, svg, W, H } = ctx;
    const len = Math.max(0, Math.floor(finite(spec && spec.len, 0)));
    if (!len || typeof spec.xAt !== 'function' || typeof spec.rowAt !== 'function') return;
    // Фейковый документ тестов не умеет события — слой там просто не нужен.
    if (typeof svg.addEventListener !== 'function' || typeof svg.getBoundingClientRect !== 'function') return;

    // ЛОВЕЦ СОБЫТИЙ. В SVG события рождаются только на нарисованном: пустое
    // место внутри графика курсора не замечает, и `mousemove` на самом svg не
    // приходит вовсе. Поэтому область данных накрывается прозрачным
    // прямоугольником — он и есть источник наведения.
    const catcher = node(doc, 'rect', {
        class: 'fchart-hover-catch',
        x: r2(x0), y: r2(y0),
        width: r2(Math.max(1, size(x1 - x0))),
        height: r2(Math.max(1, size(y1 - y0))),
        fill: 'transparent',
        'pointer-events': 'all',
    });
    over.appendChild(catcher);

    const layer = node(doc, 'g', { class: 'fchart-hover', visibility: 'hidden' });
    const rule = node(doc, 'line', {
        class: 'fchart-hover-rule',
        x1: r2(x0), x2: r2(x0), y1: r2(y0), y2: r2(y1),
        stroke: colors.inkDim, 'stroke-width': 1, 'stroke-dasharray': '3 3',
    });
    layer.appendChild(rule);
    const dots = node(doc, 'g', { class: 'fchart-hover-dots' });
    layer.appendChild(dots);
    const box = node(doc, 'g', { class: 'fchart-hover-box' });
    const boxBg = node(doc, 'rect', {
        class: 'fchart-hover-bg', x: 0, y: 0, width: 10, height: 10, rx: 6,
        fill: colors.surface, stroke: colors.grid, 'stroke-width': 1, 'fill-opacity': 0.97,
    });
    box.appendChild(boxBg);
    layer.appendChild(box);
    over.appendChild(layer);

    const CH = 5.6;
    const LINE_H = 14;
    const PAD = 8;

    const show = (i) => {
        const row = spec.rowAt(i);
        if (!row) return;
        const x = size(spec.xAt(i));
        rule.setAttribute('x1', r2(x));
        rule.setAttribute('x2', r2(x));

        clear(dots);
        const items = Array.isArray(row.items) ? row.items : [];
        for (const it of items) {
            if (!Number.isFinite(Number(it.y))) continue;
            dots.appendChild(node(doc, 'circle', {
                cx: r2(x), cy: r2(it.y), r: 4,
                fill: it.color, stroke: colors.surface, 'stroke-width': 2,
            }));
        }

        // Коробка: заголовок + строка на ряд. Ширину считаем по знакам —
        // измерять текст в SVG можно только в живом документе, а слой обязан
        // одинаково работать и при первой отрисовке, и после перерисовки.
        const lines = [String(row.title || '')].concat(
            items.map((it) => `${it.label ? it.label + ': ' : ''}${it.value}`),
        );
        const wBox = Math.min(
            size(x1 - x0),
            Math.round(Math.max(...lines.map((l) => l.length)) * CH + PAD * 2 + 14),
        );
        const hBox = PAD * 2 + LINE_H * lines.length;
        // Слева от вертикали, если справа не помещается.
        const bx = x + 12 + wBox <= x1 ? x + 12 : Math.max(x0, x - 12 - wBox);
        const by = Math.min(Math.max(y0, y0 + 4), Math.max(y0, y1 - hBox));

        while (box.childNodes.length > 1) box.removeChild(box.childNodes[box.childNodes.length - 1]);
        boxBg.setAttribute('x', r2(bx));
        boxBg.setAttribute('y', r2(by));
        boxBg.setAttribute('width', r2(wBox));
        boxBg.setAttribute('height', r2(hBox));

        box.appendChild(text(doc, lines[0], {
            x: r2(bx + PAD), y: r2(by + PAD + 9),
            fill: colors.inkDim, 'font-size': 10,
        }));
        items.forEach((it, k) => {
            const ty = by + PAD + 9 + LINE_H * (k + 1);
            box.appendChild(node(doc, 'rect', {
                x: r2(bx + PAD), y: r2(ty - 7), width: 8, height: 8, rx: 2, fill: it.color,
            }));
            box.appendChild(text(doc, lines[k + 1], {
                x: r2(bx + PAD + 12), y: r2(ty),
                fill: colors.ink, 'font-size': 10,
            }));
        });
        layer.setAttribute('visibility', 'visible');
    };

    const hide = () => layer.setAttribute('visibility', 'hidden');

    /** Экранные координаты → единицы viewBox (масштаб может быть не 1:1). */
    const toLocal = (evt) => {
        const r = svg.getBoundingClientRect();
        if (!r || !r.width || !r.height) return null;
        return { x: (evt.clientX - r.left) * (W / r.width), y: (evt.clientY - r.top) * (H / r.height) };
    };

    const onMove = (evt) => {
        const p = toLocal(evt);
        if (!p) return;
        if (p.x < x0 - 8 || p.x > x1 + 8 || p.y < y0 - 8 || p.y > y1 + 8) {
            hide();
            return;
        }
        // Ближайшая корзина: расстояние до центра, а не деление на шаг —
        // у одной корзины шага нет вовсе.
        let best = 0;
        let bestD = Infinity;
        for (let i = 0; i < len; i += 1) {
            const d = Math.abs(size(spec.xAt(i)) - p.x);
            if (d < bestD) {
                bestD = d;
                best = i;
            }
        }
        show(best);
    };

    catcher.addEventListener('mousemove', onMove);
    catcher.addEventListener('mouseleave', hide);
    if (catcher.style) catcher.style.cursor = 'crosshair';
}

// ==========================================================================
// Числа: ни NaN, ни отрицательных координат
// ==========================================================================

/** Конечное число или запасное значение. Единственный барьер против NaN. */
export function finite(v, fallback = 0) {
    const n = typeof v === 'number' ? v : Number(v);
    return Number.isFinite(n) ? n : fallback;
}

/** Неотрицательный размер: ширина/высота/радиус не имеют права быть < 0. */
export function size(v) {
    const n = finite(v, 0);
    return n > 0 ? n : 0;
}

/** Зажим в отрезок. */
export function clamp(v, lo, hi) {
    const n = finite(v, lo);
    if (n < lo) return lo;
    if (n > hi) return hi;
    return n;
}

/** Округление до 2 знаков — короткие атрибуты и стабильные снапшоты. */
function r2(v) {
    return Math.round(finite(v, 0) * 100) / 100;
}

// ==========================================================================
// Шкалы и деления
// ==========================================================================

/**
 * Линейная шкала домена в диапазон координат.
 *
 * Вырожденный домен (d0 === d1, в том числе [0,0]) НЕ схлопывается в деление
 * на ноль: точка ложится в середину диапазона.
 *
 * @returns {(v:number)=>number}
 */
export function linearScale(d0, d1, r0, r1) {
    const a = finite(d0, 0);
    const b = finite(d1, 0);
    const p = finite(r0, 0);
    const q = finite(r1, 0);
    const span = b - a;
    if (span === 0) {
        const mid = (p + q) / 2;
        return () => mid;
    }
    return (v) => p + ((finite(v, a) - a) / span) * (q - p);
}

/** «Круглый» шаг: 1, 2, 5, 10 · 10^n. */
function niceStep(raw) {
    const x = Math.abs(finite(raw, 1));
    if (x <= 0) return 1;
    const exp = Math.floor(Math.log10(x));
    const pow = Math.pow(10, exp);
    const f = x / pow;
    let nice;
    if (f <= 1) nice = 1;
    else if (f <= 2) nice = 2;
    else if (f <= 5) nice = 5;
    else nice = 10;
    return nice * pow;
}

/**
 * Человеческие деления оси.
 *
 * Гарантии (проверяются тестом): делений ВСЕГДА больше одного, значения
 * конечны, `max > min`. Диапазон [0,0] — то есть «все значения нули» —
 * превращается в 0..1, а не в пустую ось: шкала не схлопывается.
 *
 * @param {number} min
 * @param {number} max
 * @param {number} [target] — желаемое число делений
 * @returns {{min:number,max:number,step:number,ticks:number[]}}
 */
export function niceTicks(min, max, target = 5) {
    let lo = finite(min, 0);
    let hi = finite(max, 0);
    if (hi < lo) {
        const t = lo;
        lo = hi;
        hi = t;
    }
    if (hi === lo) {
        // Одна точка или сплошные нули: даём осмысленный отрезок вокруг.
        if (hi === 0) {
            lo = 0;
            hi = 1;
        } else if (hi > 0) {
            lo = Math.min(0, hi);
            hi = hi * 1.25;
        } else {
            hi = 0;
            lo = lo * 1.25;
        }
    }
    const want = Math.max(2, Math.floor(finite(target, 5)));
    const step = niceStep((hi - lo) / want);
    const start = Math.floor(lo / step) * step;
    const end = Math.ceil(hi / step) * step;
    const ticks = [];
    // Верхняя граница цикла — защита от бесконечности на кривых входах.
    for (let v = start, i = 0; v <= end + step / 1000 && i < 1000; v += step, i += 1) {
        ticks.push(Math.abs(v) < step / 1e6 ? 0 : Math.round(v * 1e6) / 1e6);
    }
    if (ticks.length < 2) ticks.push(start + step);
    return { min: ticks[0], max: ticks[ticks.length - 1], step, ticks };
}

// ==========================================================================
// Форматтеры
// ==========================================================================

/** 1234 → «1,2k»; 1234567 → «1,2M». */
export function formatCompact(v) {
    const n = finite(v, 0);
    const abs = Math.abs(n);
    if (abs < 1000) return String(Math.round(n * 100) / 100);
    if (abs < 1e6) return (n / 1e3).toFixed(1).replace('.', ',') + 'k';
    if (abs < 1e9) return (n / 1e6).toFixed(1).replace('.', ',') + 'M';
    return (n / 1e9).toFixed(1).replace('.', ',') + 'G';
}

/** Целое с неразрывными разделителями тысяч. */
export function formatCount(v) {
    const n = Math.round(finite(v, 0));
    return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ' ');
}

/** Миллисекунды → «820 мс» / «12,3 с» / «4,2 мин» / «1,5 ч». */
export function formatMs(v) {
    const n = Math.max(0, finite(v, 0));
    if (n < 1000) return `${Math.round(n)} мс`;
    if (n < 60_000) return `${(n / 1000).toFixed(1).replace('.', ',')} с`;
    if (n < 3_600_000) return `${(n / 60_000).toFixed(1).replace('.', ',')} мин`;
    return `${(n / 3_600_000).toFixed(1).replace('.', ',')} ч`;
}

/** Доллары: мелкие суммы не должны схлопываться в «$0». */
export function formatUsd(v) {
    const n = finite(v, 0);
    const abs = Math.abs(n);
    if (abs >= 100) return `$${Math.round(n)}`;
    if (abs >= 1) return `$${n.toFixed(2)}`;
    if (abs === 0) return '$0';
    return `$${n.toFixed(3)}`;
}

/** Доля 0..1 → «42%». */
export function formatPercent(v) {
    return `${Math.round(clamp(finite(v, 0), 0, 1) * 100)}%`;
}

/** `YYYY-MM-DD` → `DD.MM`; всё прочее возвращается как есть. */
export function formatDay(label) {
    const s = String(label === undefined || label === null ? '' : label);
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
    return m ? `${m[3]}.${m[2]}` : s;
}

// ==========================================================================
// Тема
// ==========================================================================

/**
 * Минимальный контраст заливки к подложке карточки.
 *
 * 3:1 — порог WCAG для нетекстовой графики (SC 1.4.11). Ниже него ряд не
 * «менее заметен», а физически не читается на некоторых экранах и при ярком
 * свете.
 */
const MIN_CONTRAST = 3;

/** Разбирает `#rgb`, `#rrggbb`, `rgb(...)`/`rgba(...)` в [r,g,b] 0..255. */
function parseColor(v) {
    const s = String(v || '').trim();
    if (!s) return null;
    if (s[0] === '#') {
        const h = s.slice(1);
        if (h.length === 3) {
            const n = parseInt(h, 16);
            if (!Number.isFinite(n)) return null;
            return [((n >> 8) & 15) * 17, ((n >> 4) & 15) * 17, (n & 15) * 17];
        }
        if (h.length === 6 || h.length === 8) {
            const n = parseInt(h.slice(0, 6), 16);
            if (!Number.isFinite(n)) return null;
            return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
        }
        return null;
    }
    const m = s.match(/^rgba?\(([^)]+)\)$/i);
    if (!m) return null;
    const parts = m[1].split(/[\s,/]+/).filter(Boolean).slice(0, 3).map(Number);
    if (parts.length < 3 || parts.some((x) => !Number.isFinite(x))) return null;
    return parts.map((x) => Math.max(0, Math.min(255, Math.round(x))));
}

/** Относительная яркость по WCAG. */
function relLuminance(rgb) {
    const f = (c) => {
        const x = c / 255;
        return x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4;
    };
    return 0.2126 * f(rgb[0]) + 0.7152 * f(rgb[1]) + 0.0722 * f(rgb[2]);
}

/** Контраст двух цветов по WCAG: 1..21. */
function contrastRatio(a, b) {
    const la = relLuminance(a);
    const lb = relLuminance(b);
    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

function mixRgb(a, b, t) {
    return [0, 1, 2].map((i) => Math.round(a[i] + (b[i] - a[i]) * t));
}

function toHex(rgb) {
    return '#' + rgb.map((c) => c.toString(16).padStart(2, '0')).join('');
}

/**
 * Доводит цвет ряда до читаемого контраста с подложкой.
 *
 * ЗАЧЕМ. Палитра берётся из ПОЛЬЗОВАТЕЛЬСКОЙ темы, а темы бывают и тёмные, и
 * светлые. Один и тот же `--success` (#5acf90) даёт 8.9:1 на тёмном фоне и
 * 1.6:1 на светлом — то есть на светлой теме зелёный ряд просто исчезает.
 * Подбирать фиксированные цвета нельзя (тему настраивал пользователь), поэтому
 * цвет не подменяется, а ДОВОДИТСЯ: смешивается с чёрным на светлом фоне и с
 * белым на тёмном ровно настолько, насколько нужно для порога. Оттенок при
 * этом сохраняется — синий остаётся синим, зелёный зелёным.
 *
 * Если цвет или фон не разбираются (`currentColor`, `color-mix(...)`), цвет
 * возвращается как есть: гадать вслепую хуже, чем не трогать.
 *
 * @param {string} color — цвет ряда
 * @param {string} surface — цвет подложки карточки
 * @param {number} [min] — целевой контраст
 */
export function contrastify(color, surface, min = MIN_CONTRAST) {
    const c = parseColor(color);
    const bg = parseColor(surface);
    if (!c || !bg) return color;
    if (contrastRatio(c, bg) >= min) return color;
    // К чему тянуть: на светлой подложке — к чёрному, на тёмной — к белому.
    const target = relLuminance(bg) > 0.4 ? [0, 0, 0] : [255, 255, 255];
    let best = c;
    for (let t = 0.05; t <= 1.0001; t += 0.05) {
        best = mixRgb(c, target, t);
        if (contrastRatio(best, bg) >= min) break;
    }
    return toHex(best);
}

/**
 * Из нескольких цветов темы выбирает самый читаемый ПОВЕРХ заливки.
 *
 * Подпись внутри полосы («Топ инструментов») лежит на цвете серии, а не на
 * подложке карточки: обычный `--fg` на синей заливке нечитаем. Кандидаты — уже
 * имеющиеся цвета темы (текст и подложка карточки), а не чёрный с белым:
 * хардкод пережил бы смену темы и выглядел бы как баг у того, кто её настроил.
 * На синей заливке светлой темы побеждает подложка, тёмной — цвет текста.
 *
 * Неразбираемая заливка — `null`, вызывающий берёт обычный `ink`.
 */
export function pickReadable(fill, candidates) {
    const bg = parseColor(fill);
    if (!bg) return null;
    let best = null;
    let bestRatio = -1;
    for (const cand of candidates) {
        const c = parseColor(cand);
        if (!c) continue;
        const r = contrastRatio(c, bg);
        if (r > bestRatio) {
            bestRatio = r;
            best = cand;
        }
    }
    return best;
}

/**
 * Цвета из CSS-переменных темы.
 *
 * Образец — `js/echo/stats.js`, но БЕЗ хексовых фолбэков: хардкод-цвет пережил
 * бы смену темы и выглядел бы как баг ровно у того пользователя, который тему
 * и настроил. Последний рубеж — `currentColor`.
 *
 * @param {Element} container
 * @param {Object} [opts] — `doc`/`win`/`styleRoot` для тестов
 */
export function themeColors(container, opts = {}) {
    const doc = docOf(container, opts);
    const view = viewOf(container, opts);
    const root = opts.styleRoot || (doc && doc.documentElement) || container;
    let cs = null;
    if (view && typeof view.getComputedStyle === 'function' && root) {
        try {
            cs = view.getComputedStyle(root);
        } catch (e) {
            cs = null;
        }
    }
    const raw = (name) => {
        if (!cs || typeof cs.getPropertyValue !== 'function') return '';
        const v = cs.getPropertyValue(name);
        return v ? String(v).trim() : '';
    };
    const pick = (...names) => {
        for (const n of names) {
            const v = raw(n);
            if (v) return v;
        }
        return 'currentColor';
    };
    // Подложка карточки: относительно неё и считается читаемость рядов.
    const surface = pick('--bg-elev', '--bg');
    const readable = (v) => contrastify(v, surface);
    const series = [];
    for (let i = 0; i < SERIES_MAX; i += 1) {
        series.push(readable(pick(`--chart-${i + 1}`, SERIES_TOKENS[i])));
    }
    return {
        ink: pick('--fg'),
        inkDim: pick('--fg-dim', '--fg'),
        grid: pick('--border', '--fg-dim'),
        surface,
        accent: readable(pick('--accent')),
        warn: readable(pick('--warn')),
        danger: readable(pick('--danger')),
        series,
        /** Цвет серии по индексу: сверх SERIES_MAX — приглушённое «Другое». */
        at(i) {
            const k = Math.floor(finite(i, 0));
            return k >= 0 && k < series.length ? series[k] : this.inkDim;
        },
    };
}

/**
 * Свёртка длинного списка в SERIES_MAX цветных позиций + «Другое».
 *
 * Девятая серия не получает сгенерированный оттенок никогда: пара цветов,
 * неразличимая при дейтеранопии, — это молча испорченный график.
 *
 * @param {Array<{label:string,value:number}>} items
 * @param {number} [max]
 * @param {string} [otherLabel]
 */
export function foldSeries(items, max = SERIES_MAX, otherLabel = 'Другое') {
    const list = Array.isArray(items) ? items.slice() : [];
    if (list.length <= max) return list;
    const head = list.slice(0, max - 1);
    const tail = list.slice(max - 1);
    const rest = tail.reduce((acc, it) => acc + finite(it && it.value, 0), 0);
    head.push({ label: otherLabel, value: rest, folded: tail.length });
    return head;
}

// ==========================================================================
// Каркас чарта
// ==========================================================================

/**
 * Пустая канва: очищенный контейнер, `<svg>` с viewBox, слои сетки/данных.
 *
 * @returns {{doc:Document,svg:Element,grid:Element,plot:Element,over:Element,
 *            colors:Object,W:number,H:number,pad:Object,
 *            innerW:number,innerH:number,x0:number,y0:number,x1:number,y1:number}}
 */
function frame(container, opts = {}) {
    const doc = docOf(container, opts);
    if (!doc) throw new Error('chart: нет документа для отрисовки');
    const colors = opts.colors || themeColors(container, opts);
    // ЕДИНИЦА viewBox = ОДИН CSS-ПИКСЕЛЬ, А НЕ ДОЛЯ ОТ 720.
    //
    // `viewBox` масштабирует ВСЁ содержимое, включая кегль подписей. Чарт с
    // фиксированным `viewBox` 720 в контейнере шириной 278px (узкая карточка с
    // двумя плотами) сжимался в 0.39 раза, и подписи осей превращались в 3.9px
    // — нечитаемые. Поэтому ширина берётся у САМОГО контейнера, и масштаб
    // всегда 1:1: где бы чарт ни стоял, шрифт остаётся 10px.
    //
    // `opts.width` перебивает измерение (тесты и явные размеры), а фолбэк на
    // `DEFAULT_WIDTH` нужен фейковому документу без вёрстки, где `clientWidth`
    // отсутствует. Высота — из `opts.height` либо по пропорции ширины, но не
    // ниже `MIN_HEIGHT`: плоский чарт нечитаем так же, как мелкий.
    const measured = size(measureWidth(container));
    const W = size(opts.width) || measured || DEFAULT_WIDTH;
    const H = size(opts.height)
        || (measured ? Math.max(MIN_HEIGHT, Math.round(measured * DEFAULT_RATIO)) : DEFAULT_HEIGHT);
    const pad = Object.assign({}, DEFAULT_PAD, opts.padding || {});
    const legendH = opts.legend === false ? 0 : size(opts.legendHeight);
    const top = size(pad.top) + legendH;
    const left = size(pad.left);
    const right = size(pad.right);
    const bottom = size(pad.bottom);
    const innerW = size(W - left - right);
    const innerH = size(H - top - bottom);

    clear(container);
    const svg = node(doc, 'svg', {
        class: `fchart${opts.class ? ' ' + opts.class : ''}`,
        viewBox: `0 0 ${W} ${H}`,
        preserveAspectRatio: 'xMidYMid meet',
        width: '100%',
        role: 'img',
        'aria-label': opts.title || opts.ariaLabel || '',
    });
    // Ширина/высота в CSS — контейнер решает; в разметке пикселей нет.
    if (svg.style) svg.style.width = '100%';
    const grid = node(doc, 'g', { class: 'fchart-grid' });
    const plot = node(doc, 'g', { class: 'fchart-plot' });
    const over = node(doc, 'g', { class: 'fchart-over' });
    svg.appendChild(grid);
    svg.appendChild(plot);
    svg.appendChild(over);
    container.appendChild(svg);

    return {
        doc,
        svg,
        grid,
        plot,
        over,
        colors,
        opts,
        W,
        H,
        pad: { top, right, bottom, left },
        innerW,
        innerH,
        x0: left,
        y0: top,
        x1: left + innerW,
        y1: top + innerH,
    };
}

/** Заглушка «нет данных» — общая для всех чартов. */
function empty(ctx, message) {
    const { doc, plot, colors, W, H } = ctx;
    plot.appendChild(
        text(doc, message || 'нет данных', {
            x: r2(W / 2),
            y: r2(H / 2),
            class: 'fchart-empty',
            fill: colors.inkDim,
            'text-anchor': 'middle',
            'dominant-baseline': 'middle',
            'font-size': 12,
        }),
    );
    return ctx;
}

/** Ось Y с делениями и горизонтальной сеткой. */
function axisY(ctx, scale, ticks, fmt, side = 'left', color) {
    const { doc, grid, colors, x0, x1 } = ctx;
    const f = typeof fmt === 'function' ? fmt : formatCompact;
    const ink = color || colors.inkDim;
    for (const t of ticks) {
        const y = r2(scale(t));
        if (side === 'left') {
            grid.appendChild(
                node(doc, 'line', {
                    class: 'fchart-gridline',
                    x1: r2(x0),
                    y1: y,
                    x2: r2(x1),
                    y2: y,
                    stroke: colors.grid,
                    'stroke-width': 1,
                }),
            );
        }
        grid.appendChild(
            text(doc, f(t), {
                x: r2(side === 'left' ? x0 - 6 : x1 + 6),
                y,
                class: 'fchart-tick',
                fill: ink,
                'text-anchor': side === 'left' ? 'end' : 'start',
                'dominant-baseline': 'middle',
                'font-size': 10,
            }),
        );
    }
}

/**
 * Ось X с прореженными подписями.
 *
 * Подписи прореживаются по ширине: 30 дат в 720 единицах не помещаются, а
 * наложенные подписи хуже отсутствующих.
 */
function axisX(ctx, labels, xAt, fmt) {
    const { doc, grid, colors, y1, innerW } = ctx;
    const f = typeof fmt === 'function' ? fmt : formatDay;
    const n = labels.length;
    if (n === 0) return;
    const perLabel = 46;
    const room = Math.max(1, Math.floor(size(innerW) / perLabel));
    const step = Math.max(1, Math.ceil(n / room));
    grid.appendChild(
        node(doc, 'line', {
            class: 'fchart-axis',
            x1: r2(ctx.x0),
            y1: r2(y1),
            x2: r2(ctx.x1),
            y2: r2(y1),
            stroke: colors.grid,
            'stroke-width': 1,
        }),
    );
    for (let i = 0; i < n; i += 1) {
        if (i % step !== 0 && i !== n - 1) continue;
        grid.appendChild(
            text(doc, f(labels[i], i), {
                x: r2(xAt(i)),
                y: r2(y1 + 14),
                class: 'fchart-tick',
                fill: colors.inkDim,
                'text-anchor': 'middle',
                'font-size': 10,
            }),
        );
    }
}

/**
 * Легенда: квадрат цвета + подпись текстовым токеном.
 *
 * Подпись НИКОГДА не красится цветом серии — текст носит цвет текста, а
 * идентичность несёт квадратик рядом. Легенда обязательна от двух серий.
 */
function legend(ctx, entries) {
    const { doc, over, colors, x0, x1, pad } = ctx;
    if (!entries || entries.length < 2) return;

    // ЛЕГЕНДА НЕ УЕЗЖАЕТ ЗА КРАЙ И НЕ СДВИГАЕТ ГРАФИК.
    //
    // Одной строкой помещаются два-три коротких имени; четыре модели вида
    // `claude-haiku-4-5-20251001` не помещаются никуда, и последняя запись
    // просто обрезалась границей svg — серия оставалась без названия. Перенос
    // на вторую строку решал бы это ценой наезда на область данных: место под
    // легенду резервируется в `frame()` до того, как записи станут известны.
    // Поэтому подписи УСЕКАЮТСЯ по своей доле ширины, а полное имя остаётся в
    // подсказке. Ширина оценивается по числу знаков: точная метрика требует
    // измерения в живом документе, которого у тестов нет.
    const CH = 5.6;
    const SWATCH = 12;
    const GAP_ITEM = 16;
    const avail = Math.max(80, size(x1) - size(x0));
    const widthOf = (label) => SWATCH + CH * String(label).length + GAP_ITEM;
    const total = entries.reduce((acc, e) => acc + widthOf(e.label), 0);

    let render = entries.map((e) => ({ color: e.color, label: String(e.label), full: String(e.label) }));
    if (total > avail) {
        const share = avail / entries.length;
        const maxChars = Math.max(4, Math.floor((share - SWATCH - GAP_ITEM) / CH));
        render = render.map((e) => (e.label.length > maxChars
            ? Object.assign({}, e, { label: e.label.slice(0, maxChars - 1) + '…' })
            : e));
    }

    const y = Math.max(10, size(pad.top) - 6);
    let x = x0;
    for (const e of render) {
        const g = node(doc, 'g', { class: 'fchart-legend-item' });
        g.appendChild(
            node(doc, 'rect', {
                x: r2(x),
                y: r2(y - 7),
                width: 8,
                height: 8,
                rx: 2,
                fill: e.color,
            }),
        );
        g.appendChild(
            text(doc, e.label, {
                x: r2(x + SWATCH),
                y: r2(y),
                class: 'fchart-legend-label',
                fill: colors.ink,
                'dominant-baseline': 'middle',
                'font-size': 10,
            }),
        );
        if (e.label !== e.full) tip(doc, g, e.full);
        over.appendChild(g);
        x += widthOf(e.label);
    }
}

/**
 * Обёртка чарта: общий жизненный цикл и `update()` без пересоздания контейнера.
 *
 * @param {(ctx:Object,data:any,opts:Object)=>void} render
 * @returns {(container:Element,data:any,opts?:Object)=>{update:Function,redraw:Function,svg:Element}}
 */
function chartFactory(render, defaults = {}) {
    return function chart(container, data, opts = {}) {
        let curData = data;
        let curOpts = Object.assign({}, defaults, opts);
        let ctx = null;
        const draw = () => {
            ctx = frame(container, curOpts);
            render(ctx, curData, curOpts);
            return ctx;
        };
        draw();
        return {
            /** Перерисовка на месте: те же контейнер и опции, новые данные. */
            update(nextData, nextOpts) {
                if (nextData !== undefined) curData = nextData;
                if (nextOpts) curOpts = Object.assign({}, curOpts, nextOpts);
                draw();
                return this;
            },
            /** Перерисовка теми же данными — например после смены темы. */
            redraw() {
                draw();
                return this;
            },
            get svg() {
                return ctx ? ctx.svg : null;
            },
        };
    };
}

// ==========================================================================
// Нормализация входа
// ==========================================================================

/** Числовой ряд без дыр: null/undefined/NaN → 0. */
function nums(arr) {
    return Array.isArray(arr) ? arr.map((v) => finite(v, 0)) : [];
}

/** Подписи ряда: недостающие добираются пустыми строками. */
function labelsOf(data, n) {
    const src = data && Array.isArray(data.labels) ? data.labels : [];
    const out = [];
    for (let i = 0; i < n; i += 1) out.push(src[i] === undefined ? '' : src[i]);
    return out;
}

/** Серии `[{label, values}]` из разных допустимых форм входа. */
function seriesOf(data) {
    if (!data) return [];
    if (Array.isArray(data.series)) {
        return data.series
            .filter(Boolean)
            .map((s, i) => ({ label: s.label === undefined ? `Ряд ${i + 1}` : s.label, values: nums(s.values) }));
    }
    if (Array.isArray(data.values)) return [{ label: data.label || '', values: nums(data.values) }];
    if (Array.isArray(data)) return [{ label: '', values: nums(data) }];
    return [];
}

/** Максимум по всем сериям (>= 0). */
function maxOf(series) {
    let m = 0;
    for (const s of series) for (const v of s.values) if (v > m) m = v;
    return m;
}

/** Полоса с закруглённым «дальним» концом: ближний край прирастает к базе. */
function barPath(x, y, w, h, r = 4) {
    const X = r2(x);
    const Y = r2(y);
    const W = r2(size(w));
    const H = r2(size(h));
    const rad = Math.min(r, W / 2, H);
    if (!(H > 0) || !(W > 0)) return `M${X} ${r2(Y + H)}h${W}`;
    if (rad <= 0) return `M${X} ${Y}h${W}v${H}h${-W}Z`;
    return (
        `M${X} ${r2(Y + H)}V${r2(Y + rad)}` +
        `A${r2(rad)} ${r2(rad)} 0 0 1 ${r2(X + rad)} ${Y}` +
        `H${r2(X + W - rad)}` +
        `A${r2(rad)} ${r2(rad)} 0 0 1 ${r2(X + W)} ${r2(Y + rad)}` +
        `V${r2(Y + H)}Z`
    );
}

/** Тот же приём, но полоса растёт вправо (горизонтальные бары). */
function hbarPath(x, y, w, h, r = 4) {
    const X = r2(x);
    const Y = r2(y);
    const W = r2(size(w));
    const H = r2(size(h));
    const rad = Math.min(r, H / 2, W);
    if (!(H > 0) || !(W > 0)) return `M${X} ${Y}v${H}`;
    if (rad <= 0) return `M${X} ${Y}h${W}v${H}h${-W}Z`;
    return (
        `M${X} ${Y}H${r2(X + W - rad)}` +
        `A${r2(rad)} ${r2(rad)} 0 0 1 ${r2(X + W)} ${r2(Y + rad)}` +
        `V${r2(Y + H - rad)}` +
        `A${r2(rad)} ${r2(rad)} 0 0 1 ${r2(X + W - rad)} ${r2(Y + H)}` +
        `H${X}Z`
    );
}

// ==========================================================================
// 1. barChart — столбцы по дням (+ необязательная наложенная линия)
// ==========================================================================

/**
 * Столбцы по корзинам с необязательной наложенной линией на СВОЕЙ шкале.
 *
 * ПРО ВТОРУЮ ОСЬ. Две оси Y — вообще говоря приём сомнительный: соотношение
 * кривых зависит от выбора шкал, а не от данных, и читатель этого не видит.
 * Здесь она нужна ровно под чарт 1 плана («сообщения по дням» + «новые сессии»,
 * величины разного порядка в одном контексте) и включается ЯВНО через
 * `opts.overlay`/`data.overlay`. По умолчанию её нет; если ряды сравнимы —
 * рисуйте два чарта, а не вторую ось.
 *
 * @param {Element} container
 * @param {{labels:string[],values:number[],label?:string,
 *          overlay?:{label?:string,values:number[],format?:Function}}} data
 * @param {Object} [opts] — `formatY`, `formatX`, `formatValue`, `emptyText`
 */
export const barChart = chartFactory((ctx, data, opts) => {
    const values = nums(data && (data.values || data.bars));
    const labels = labelsOf(data, values.length);
    if (values.length === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, x1, y0, y1, innerW } = ctx;
    const overlay = data && data.overlay && Array.isArray(data.overlay.values) ? data.overlay : null;
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCount;

    const yTicks = niceTicks(0, maxOf([{ values }]), opts.yTicks || 4);
    const yScale = linearScale(yTicks.min, yTicks.max, y1, y0);
    axisY(ctx, yScale, yTicks.ticks, opts.formatY || formatCompact);

    const n = values.length;
    const band = size(innerW) / n;
    const bw = Math.max(1, band - GAP);
    const xAt = (i) => x0 + band * i + band / 2;
    axisX(ctx, labels, xAt, opts.formatX);

    const color = opts.color || colors.at(0);
    for (let i = 0; i < n; i += 1) {
        const v = values[i];
        const yTop = clamp(yScale(v), y0, y1);
        const h = size(y1 - yTop);
        const bar = node(doc, 'path', {
            class: 'fchart-bar',
            d: barPath(x0 + band * i + (band - bw) / 2, yTop, bw, h),
            fill: color,
        });
        tip(doc, bar, `${formatDay(labels[i])}: ${fmtV(v)}`);
        plot.appendChild(bar);
    }

    const barRow = (i) => ({
        color,
        label: (data && data.label) || opts.label || 'значение',
        value: fmtV(values[i]),
        y: clamp(yScale(values[i]), y0, y1),
    });

    if (!overlay) {
        hoverLayer(ctx, {
            len: n,
            xAt,
            rowAt: (i) => ({ title: formatDay(labels[i]), items: [barRow(i)] }),
        });
        return ctx;
    }

    // Вторая шкала считается ТОЛЬКО по своему ряду — с первой не связана.
    const ov = nums(overlay.values);
    const oTicks = niceTicks(0, maxOf([{ values: ov }]), opts.yTicks || 4);
    const oScale = linearScale(oTicks.min, oTicks.max, y1, y0);
    const oColor = opts.overlayColor || colors.at(1);
    axisY(ctx, oScale, oTicks.ticks, overlay.format || formatCompact, 'right', oColor);

    const pts = [];
    for (let i = 0; i < Math.min(n, ov.length); i += 1) {
        pts.push([xAt(i), clamp(oScale(ov[i]), y0, y1)]);
    }
    if (pts.length === 1) {
        plot.appendChild(
            node(doc, 'circle', {
                class: 'fchart-dot',
                cx: r2(pts[0][0]),
                cy: r2(pts[0][1]),
                r: 4,
                fill: oColor,
                stroke: colors.surface,
                'stroke-width': 2,
            }),
        );
    } else if (pts.length > 1) {
        plot.appendChild(
            node(doc, 'path', {
                class: 'fchart-line',
                d: pts.map((p, i) => `${i ? 'L' : 'M'}${r2(p[0])} ${r2(p[1])}`).join(''),
                fill: 'none',
                stroke: oColor,
                'stroke-width': 2,
                'stroke-linejoin': 'round',
                'stroke-linecap': 'round',
            }),
        );
    }
    for (let i = 0; i < pts.length; i += 1) {
        const hot = node(doc, 'circle', {
            class: 'fchart-hot',
            cx: r2(pts[i][0]),
            cy: r2(pts[i][1]),
            r: 5,
            fill: 'none',
            'pointer-events': 'all',
        });
        tip(doc, hot, `${overlay.label || 'ряд 2'} · ${formatDay(labels[i])}: ${(overlay.format || formatCount)(ov[i])}`);
        ctx.over.appendChild(hot);
    }

    legend(ctx, [
        { label: (data && data.label) || opts.label || 'значение', color },
        { label: overlay.label || 'ряд 2', color: oColor },
    ]);

    hoverLayer(ctx, {
        len: n,
        xAt,
        rowAt: (i) => {
            const items = [barRow(i)];
            if (i < ov.length) {
                items.push({
                    color: oColor,
                    label: overlay.label || 'ряд 2',
                    value: (overlay.format || formatCount)(ov[i]),
                    y: clamp(oScale(ov[i]), y0, y1),
                });
            }
            return { title: formatDay(labels[i]), items };
        },
    });
    return ctx;
}, { legendHeight: 8 });

// ==========================================================================
// 2. lineChart — одна или несколько линий
// ==========================================================================

/**
 * Линии по общим корзинам: медиана и p90 пауз, p50/p90 длительности хода,
 * средняя длина сообщения.
 *
 * Одна точка рисуется маркером (линии из одной точки не бывает), полностью
 * нулевые ряды ложатся на низ шкалы 0..1 — ось при этом не схлопывается.
 */
export const lineChart = chartFactory((ctx, data, opts) => {
    const series = seriesOf(data).slice(0, SERIES_MAX);
    const len = series.reduce((m, s) => Math.max(m, s.values.length), 0);
    if (series.length === 0 || len === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, y0, y1, innerW } = ctx;
    const labels = labelsOf(data, len);
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCompact;

    const yTicks = niceTicks(0, maxOf(series), opts.yTicks || 4);
    const yScale = linearScale(yTicks.min, yTicks.max, y1, y0);
    axisY(ctx, yScale, yTicks.ticks, opts.formatY || fmtV);

    // Одна корзина — точка ровно посередине поля, а не в углу.
    const xAt = len === 1 ? () => x0 + size(innerW) / 2 : (i) => x0 + (size(innerW) / (len - 1)) * i;
    axisX(ctx, labels, xAt, opts.formatX);

    series.forEach((s, si) => {
        const color = colors.at(si);
        const pts = [];
        for (let i = 0; i < len; i += 1) {
            const v = s.values[i];
            if (v === undefined) continue;
            pts.push([xAt(i), clamp(yScale(v), y0, y1), i, v]);
        }
        if (pts.length > 1) {
            plot.appendChild(
                node(doc, 'path', {
                    class: 'fchart-line',
                    d: pts.map((p, i) => `${i ? 'L' : 'M'}${r2(p[0])} ${r2(p[1])}`).join(''),
                    fill: 'none',
                    stroke: color,
                    'stroke-width': 2,
                    'stroke-linejoin': 'round',
                    'stroke-linecap': 'round',
                }),
            );
        }
        // Маркеры: всегда при одной точке, иначе только на коротких рядах,
        // чтобы 90 дней не превратились в пунктир из кружков.
        const showDots = pts.length === 1 || pts.length <= 14;
        for (const p of pts) {
            if (showDots) {
                plot.appendChild(
                    node(doc, 'circle', {
                        class: 'fchart-dot',
                        cx: r2(p[0]),
                        cy: r2(p[1]),
                        r: 4,
                        fill: color,
                        stroke: colors.surface,
                        'stroke-width': 2,
                    }),
                );
            }
            const hot = node(doc, 'circle', {
                class: 'fchart-hot',
                cx: r2(p[0]),
                cy: r2(p[1]),
                r: 5,
                fill: 'none',
                'pointer-events': 'all',
            });
            tip(doc, hot, `${s.label ? s.label + ' · ' : ''}${formatDay(labels[p[2]])}: ${fmtV(p[3])}`);
            ctx.over.appendChild(hot);
        }
    });

    legend(ctx, series.map((s, i) => ({ label: s.label, color: colors.at(i) })));

    hoverLayer(ctx, {
        len,
        xAt,
        rowAt: (i) => ({
            title: formatDay(labels[i]),
            items: series
                .filter((s) => s.values[i] !== undefined)
                .map((s, si) => ({
                    color: colors.at(si),
                    label: s.label,
                    value: fmtV(s.values[i]),
                    y: clamp(yScale(s.values[i]), y0, y1),
                })),
        }),
    });
    return ctx;
}, { legendHeight: 8 });

// ==========================================================================
// 3. hbars — горизонтальные полосы с долей внутри
// ==========================================================================

/**
 * Горизонтальные полосы: топ инструментов, топ проектов.
 *
 * `secondary` рисуется ВНУТРИ полосы статусным цветом `--warn` (доля
 * прерванных вызовов) — это состояние, а не ещё одна серия, поэтому цвет взят
 * из статусной части палитры и не занимает слот серии.
 */
export const hbars = chartFactory((ctx, data, opts) => {
    const items = (Array.isArray(data) ? data : (data && data.items) || [])
        .filter(Boolean)
        .map((it) => ({
            label: it.label === undefined ? '' : String(it.label),
            value: finite(it.value, 0),
            secondary: finite(it.secondary, 0),
        }));
    if (items.length === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, x1, y0, innerW, innerH } = ctx;
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCount;
    const max = items.reduce((m, it) => Math.max(m, it.value), 0);
    const scale = linearScale(0, max > 0 ? max : 1, 0, size(innerW) - 56);
    const rowH = size(innerH) / items.length;
    const barH = Math.max(1, Math.min(22, rowH - GAP * 2));
    const color = opts.color || colors.at(0);
    const secColor = opts.secondaryColor || colors.warn;
    let hasSecondary = false;

    items.forEach((it, i) => {
        const y = y0 + rowH * i + (rowH - barH) / 2;
        const w = size(scale(it.value));
        const g = node(doc, 'g', { class: 'fchart-hbar' });
        // Дорожка во всю строку: без неё курсор ловится только самой полосой,
        // а у хвоста списка она в двадцать раз короче строки — наведение
        // «не работает» ровно там, где список длиннее всего.
        g.appendChild(
            node(doc, 'rect', {
                class: 'fchart-hbar-track',
                x: r2(x0),
                y: r2(y0 + rowH * i),
                width: r2(Math.max(1, size(x1 - x0))),
                height: r2(Math.max(1, rowH)),
                rx: 4,
                fill: colors.ink,
                'fill-opacity': 0,
            }),
        );
        g.appendChild(
            node(doc, 'path', {
                class: 'fchart-bar',
                d: hbarPath(x0, y, w, barH),
                fill: color,
            }),
        );
        if (it.secondary > 0) {
            hasSecondary = true;
            const sw = size(Math.min(w, scale(Math.min(it.secondary, it.value))));
            g.appendChild(
                node(doc, 'path', {
                    class: 'fchart-bar-secondary',
                    d: hbarPath(x0, y, sw, barH),
                    fill: secColor,
                }),
            );
        }
        // ПОДПИСЬ ВНУТРИ ПОЛОСЫ ЧИТАЕМА ТОЛЬКО НА САМОЙ ПОЛОСЕ. Названия здесь
        // длинные (`mcp__claude-in-chrome__computer`), а полосы после первой
        // короткие — цветом `--fg` подпись ложилась поверх синей заливки у
        // лидеров и становилась нечитаемой. Поэтому решение построчное: текст,
        // который умещается в полосу, красится контрастно к заливке; тот, что
        // не умещается, уезжает правее её конца на обычную подложку. Ширина
        // строки оценивается по числу знаков — точная метрика в SVG требует
        // измерения в живом документе, которого у тестов нет.
        const labelW = it.label.length * 5.6;
        const inside = w >= labelW + 12;
        const labelFill = inside
            ? (pickReadable(color, [colors.ink, colors.surface]) || colors.ink)
            : colors.ink;
        g.appendChild(
            text(doc, it.label, {
                x: r2(inside ? x0 + 6 : x0 + w + 6),
                y: r2(y + barH / 2),
                class: 'fchart-hbar-label',
                fill: labelFill,
                'dominant-baseline': 'middle',
                'font-size': 10,
            }),
        );
        g.appendChild(
            text(doc, fmtV(it.value), {
                x: r2(x1),
                y: r2(y + barH / 2),
                class: 'fchart-hbar-value',
                fill: colors.inkDim,
                'text-anchor': 'end',
                'dominant-baseline': 'middle',
                'font-size': 10,
            }),
        );
        tip(
            doc,
            g,
            it.secondary > 0
                ? `${it.label}: ${fmtV(it.value)} (${opts.secondaryLabel || 'из них'} ${fmtV(it.secondary)})`
                : `${it.label}: ${fmtV(it.value)}`,
        );
        plot.appendChild(g);
    });

    if (hasSecondary) {
        legend(ctx, [
            { label: opts.label || 'всего', color },
            { label: opts.secondaryLabel || 'из них прервано', color: secColor },
        ]);
    }
    return ctx;
}, { legendHeight: 8, padding: { left: 8, bottom: 8 } });

// ==========================================================================
// 4. donut — доли
// ==========================================================================

/** Точка на окружности; угол отсчитывается от 12 часов по часовой стрелке. */
function polar(cx, cy, radius, angle) {
    return [cx + radius * Math.sin(angle), cy - radius * Math.cos(angle)];
}

/** Сегмент кольца между углами a0 и a1. */
function ringPath(cx, cy, rOut, rIn, a0, a1) {
    const large = a1 - a0 > Math.PI ? 1 : 0;
    const p0 = polar(cx, cy, rOut, a0);
    const p1 = polar(cx, cy, rOut, a1);
    const p2 = polar(cx, cy, rIn, a1);
    const p3 = polar(cx, cy, rIn, a0);
    return (
        `M${r2(p0[0])} ${r2(p0[1])}` +
        `A${r2(rOut)} ${r2(rOut)} 0 ${large} 1 ${r2(p1[0])} ${r2(p1[1])}` +
        `L${r2(p2[0])} ${r2(p2[1])}` +
        `A${r2(rIn)} ${r2(rIn)} 0 ${large} 0 ${r2(p3[0])} ${r2(p3[1])}Z`
    );
}

/**
 * Кольцевая диаграмма долей: модели по сообщениям и по стоимости.
 *
 * Долей больше пяти не бывает — хвост сворачивается в «Другое» (`foldSeries`).
 * Сумма нулевая (всё по нулям) — рисуется пустое кольцо цветом рамки и «0» в
 * центре: деления на ноль в этой ветке нет.
 */
export const donut = chartFactory((ctx, data, opts) => {
    const raw = (Array.isArray(data) ? data : (data && data.parts) || [])
        .filter(Boolean)
        .map((p) => ({ label: p.label === undefined ? '' : String(p.label), value: Math.max(0, finite(p.value, 0)) }));
    if (raw.length === 0) return empty(ctx, opts.emptyText);

    const parts = foldSeries(raw, SERIES_MAX, opts.otherLabel || 'Другое');
    const { doc, plot, colors, x0, y0, innerW, innerH } = ctx;
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCount;
    const total = parts.reduce((acc, p) => acc + p.value, 0);

    const rOut = Math.max(1, Math.min(size(innerW), size(innerH)) / 2 - 4);
    const rIn = Math.max(0.5, rOut * 0.62);
    const cx = x0 + size(innerW) / 2;
    const cy = y0 + size(innerH) / 2;

    if (total <= 0) {
        plot.appendChild(
            node(doc, 'circle', {
                class: 'fchart-donut-empty',
                cx: r2(cx),
                cy: r2(cy),
                r: r2((rOut + rIn) / 2),
                fill: 'none',
                stroke: colors.grid,
                'stroke-width': r2(size(rOut - rIn)),
            }),
        );
        plot.appendChild(
            text(doc, opts.emptyText || 'нет данных', {
                x: r2(cx),
                y: r2(cy),
                class: 'fchart-donut-total',
                fill: colors.inkDim,
                'text-anchor': 'middle',
                'dominant-baseline': 'middle',
                'font-size': 11,
            }),
        );
        legend(ctx, parts.map((p, i) => ({ label: p.label, color: colors.at(i) })));
        return ctx;
    }

    let a0 = 0;
    parts.forEach((p, i) => {
        const share = p.value / total;
        const a1 = a0 + share * Math.PI * 2;
        const color = colors.at(i);
        let seg;
        if (share >= 0.9999) {
            // Единственная доля: полный круг дугой не рисуется (a0 === a1).
            seg = node(doc, 'circle', {
                class: 'fchart-arc',
                cx: r2(cx),
                cy: r2(cy),
                r: r2((rOut + rIn) / 2),
                fill: 'none',
                stroke: color,
                'stroke-width': r2(size(rOut - rIn)),
            });
        } else if (share > 0) {
            seg = node(doc, 'path', {
                class: 'fchart-arc',
                d: ringPath(cx, cy, rOut, rIn, a0, a1),
                fill: color,
                stroke: colors.surface,
                'stroke-width': GAP,
            });
        }
        if (seg) {
            tip(doc, seg, `${p.label}: ${fmtV(p.value)} · ${formatPercent(share)}`);
            plot.appendChild(seg);
        }
        a0 = a1;
    });

    plot.appendChild(
        text(doc, opts.centerLabel || fmtV(total), {
            x: r2(cx),
            y: r2(cy),
            class: 'fchart-donut-total',
            fill: colors.ink,
            'text-anchor': 'middle',
            'dominant-baseline': 'middle',
            'font-size': 13,
        }),
    );
    legend(ctx, parts.map((p, i) => ({ label: p.label, color: colors.at(i) })));
    return ctx;
}, { legendHeight: 8, padding: { left: 8, bottom: 8 } });

// ==========================================================================
// 5. stackedArea — стек по корзинам
// ==========================================================================

/**
 * Стек площадей по общим корзинам: токены (in / out / cache-read /
 * cache-create) и «куда уходит время» (генерация · инструменты · человек
 * читает).
 *
 * Ряды складываются в порядке `series`, сверху вниз рисуется тот же порядок,
 * что и в легенде: иначе легенда и картинка расходятся, и читателю приходится
 * угадывать. Одна корзина — площади не бывает, рисуются столбики стека, чтобы
 * значение не пропало с экрана.
 */
export const stackedArea = chartFactory((ctx, data, opts) => {
    const series = seriesOf(data).slice(0, SERIES_MAX);
    const len = series.reduce((m, s) => Math.max(m, s.values.length), 0);
    if (series.length === 0 || len === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, y0, y1, innerW } = ctx;
    const labels = labelsOf(data, len);
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCompact;

    // Верх шкалы — максимум СУММЫ по корзине, а не максимум ряда.
    let maxTotal = 0;
    const totals = [];
    for (let i = 0; i < len; i += 1) {
        let t = 0;
        for (const s of series) t += Math.max(0, finite(s.values[i], 0));
        totals.push(t);
        if (t > maxTotal) maxTotal = t;
    }
    const yTicks = niceTicks(0, maxTotal, opts.yTicks || 4);
    const yScale = linearScale(yTicks.min, yTicks.max, y1, y0);
    axisY(ctx, yScale, yTicks.ticks, opts.formatY || fmtV);

    const single = len === 1;
    const band = size(innerW) / len;
    const xAt = single ? () => x0 + size(innerW) / 2 : (i) => x0 + (size(innerW) / (len - 1)) * i;
    axisX(ctx, labels, single ? () => x0 + size(innerW) / 2 : xAt, opts.formatX);

    // Накопленные низы: нижняя граница каждого следующего ряда.
    const base = new Array(len).fill(0);
    series.forEach((s, si) => {
        const color = colors.at(si);
        const lo = base.slice();
        for (let i = 0; i < len; i += 1) base[i] = lo[i] + Math.max(0, finite(s.values[i], 0));

        if (single) {
            const bw = Math.max(1, Math.min(48, band - GAP));
            const yTop = clamp(yScale(base[0]), y0, y1);
            const yBot = clamp(yScale(lo[0]), y0, y1);
            const seg = node(doc, 'rect', {
                class: 'fchart-stack',
                x: r2(x0 + size(innerW) / 2 - bw / 2),
                y: r2(yTop),
                width: r2(bw),
                height: r2(size(yBot - yTop)),
                fill: color,
                stroke: colors.surface,
                'stroke-width': GAP,
            });
            tip(doc, seg, `${s.label} · ${formatDay(labels[0])}: ${fmtV(Math.max(0, finite(s.values[0], 0)))}`);
            plot.appendChild(seg);
            return;
        }

        const top = [];
        const bottom = [];
        for (let i = 0; i < len; i += 1) {
            top.push(`${i ? 'L' : 'M'}${r2(xAt(i))} ${r2(clamp(yScale(base[i]), y0, y1))}`);
            bottom.unshift(`L${r2(xAt(i))} ${r2(clamp(yScale(lo[i]), y0, y1))}`);
        }
        const area = node(doc, 'path', {
            class: 'fchart-area',
            d: `${top.join('')}${bottom.join('')}Z`,
            fill: color,
            'fill-opacity': 0.85,
            stroke: colors.surface,
            'stroke-width': 1,
        });
        tip(doc, area, s.label);
        plot.appendChild(area);
    });

    // Подсказка стека называет ВЕСЬ день: сумма по рядам — то, ради чего стек
    // и рисуют, а отдельная полоса без соседей ничего не говорит.
    {
        const stackBase = new Array(len).fill(0);
        const tops = series.map((s) => {
            const row = [];
            for (let i = 0; i < len; i += 1) {
                stackBase[i] += Math.max(0, finite(s.values[i], 0));
                row.push(clamp(yScale(stackBase[i]), y0, y1));
            }
            return row;
        });
        hoverLayer(ctx, {
            len,
            xAt,
            rowAt: (i) => ({
                title: formatDay(labels[i]),
                items: series.map((s, si) => ({
                    color: colors.at(si),
                    label: s.label,
                    value: fmtV(Math.max(0, finite(s.values[i], 0))),
                    y: tops[si][i],
                })),
            }),
        });
    }

    // Наведение по корзине: одна подсказка с полным разрезом стека.
    for (let i = 0; i < len; i += 1) {
        const cx = single ? x0 + size(innerW) / 2 : xAt(i);
        const hot = node(doc, 'rect', {
            class: 'fchart-hot',
            x: r2(Math.max(x0, cx - band / 2)),
            y: r2(y0),
            width: r2(size(Math.min(band, ctx.x1 - Math.max(x0, cx - band / 2)))),
            height: r2(size(y1 - y0)),
            fill: 'none',
            'pointer-events': 'all',
        });
        const lines = series.map((s) => `${s.label}: ${fmtV(Math.max(0, finite(s.values[i], 0)))}`);
        tip(doc, hot, `${formatDay(labels[i])}\n${lines.join('\n')}\nвсего: ${fmtV(totals[i])}`);
        ctx.over.appendChild(hot);
    }

    legend(ctx, series.map((s, i) => ({ label: s.label, color: colors.at(i) })));
    return ctx;
}, { legendHeight: 8 });

// ==========================================================================
// 6. histogram — распределение по бакетам
// ==========================================================================

/**
 * Гистограмма бакетов: паузы перед ответом (<5 с, 5–15, 15–60, 1–5 мин,
 * >5 мин) и длины сообщений.
 *
 * Пустой бакет НЕ исчезает: столбик нулевой высоты остаётся вместе с подписью
 * — «в этом диапазоне ноль» это факт, а дыра в оси читается как ошибка. Все
 * бакеты по нулям дают шкалу 0..1, а не пустую ось.
 */
export const histogram = chartFactory((ctx, data, opts) => {
    const values = nums(data && (data.values || data.buckets));
    if (values.length === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, y0, y1, innerW } = ctx;
    const labels = labelsOf(data, values.length);
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCount;
    const total = values.reduce((a, b) => a + b, 0);

    const yTicks = niceTicks(0, maxOf([{ values }]), opts.yTicks || 4);
    const yScale = linearScale(yTicks.min, yTicks.max, y1, y0);
    axisY(ctx, yScale, yTicks.ticks, opts.formatY || formatCompact);

    const n = values.length;
    const band = size(innerW) / n;
    const bw = Math.max(1, band - GAP);
    axisX(ctx, labels, (i) => x0 + band * i + band / 2, opts.formatX || ((l) => String(l)));

    const color = opts.color || colors.at(0);
    for (let i = 0; i < n; i += 1) {
        const v = values[i];
        const yTop = clamp(yScale(v), y0, y1);
        const h = size(y1 - yTop);
        const bar = node(doc, 'path', {
            class: 'fchart-bar',
            d: barPath(x0 + band * i + (band - bw) / 2, yTop, bw, h),
            fill: color,
        });
        const share = total > 0 ? v / total : 0;
        tip(doc, bar, `${labels[i]}: ${fmtV(v)}${total > 0 ? ` · ${formatPercent(share)}` : ''}`);
        plot.appendChild(bar);
        // Прямая подпись только на непустых столбиках: число над нулём —
        // лишний шум, число над каждым столбиком — тоже.
        if (v > 0 && n <= 8) {
            plot.appendChild(
                text(doc, fmtV(v), {
                    x: r2(x0 + band * i + band / 2),
                    y: r2(Math.max(y0, yTop - 4)),
                    class: 'fchart-value',
                    fill: colors.inkDim,
                    'text-anchor': 'middle',
                    'font-size': 10,
                }),
            );
        }
    }

    hoverLayer(ctx, {
        len: n,
        xAt: (i) => x0 + band * i + band / 2,
        rowAt: (i) => ({
            title: String(labels[i] === undefined ? '' : labels[i]),
            items: [{
                color,
                label: '',
                // Доля идёт ПОСЛЕ числа: «572 · 34%» читается как ответ,
                // «34%: 572» — как перепутанные местами подпись и значение.
                value: total > 0
                    ? `${fmtV(values[i])} · ${formatPercent(values[i] / total)}`
                    : fmtV(values[i]),
                y: clamp(yScale(values[i]), y0, y1),
            }],
        }),
    });
    return ctx;
});

// ==========================================================================
// 7. heatmap — час × день недели
// ==========================================================================

/** Дни недели строками: понедельник сверху — как в календаре. */
const WEEKDAYS = ['Пн', 'Вт', 'Ср', 'Чт', 'Пт', 'Сб', 'Вс'];

/**
 * Тепловая карта «час × день недели».
 *
 * Интенсивность — ОДИН оттенок (`--accent`) с ростом непрозрачности от
 * минимума к максимуму: последовательная величина требует одного тона, а не
 * радуги. Матрица из нулей даёт ровный минимум по всей карте — деления на ноль
 * в этой ветке нет (`span <= 0 → t = 0`).
 *
 * Ночные часы 00–06 подчёркнуты снизу отдельной скобкой с подписью: «работа
 * ночью» — самостоятельный сигнал плана, и его видно без пересчёта в уме.
 *
 * @param {{matrix:number[][], rowLabels?:string[], colLabels?:string[]}} data
 *        матрица 7×24: строка — день недели, столбец — час.
 */
export const heatmap = chartFactory((ctx, data, opts) => {
    const src = (data && (data.matrix || data.rows)) || (Array.isArray(data) ? data : []);
    const matrix = Array.isArray(src) ? src.filter(Array.isArray).map(nums) : [];
    const rows = matrix.length;
    const cols = matrix.reduce((m, r) => Math.max(m, r.length), 0);
    if (rows === 0 || cols === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, y0, innerW, innerH } = ctx;
    const fmtV = typeof opts.formatValue === 'function' ? opts.formatValue : formatCount;
    const rowLabels = (data && data.rowLabels) || WEEKDAYS;
    const colLabels = (data && data.colLabels) || null;

    let max = 0;
    let min = Infinity;
    for (const row of matrix) {
        for (let c = 0; c < cols; c += 1) {
            const v = finite(row[c], 0);
            if (v > max) max = v;
            if (v < min) min = v;
        }
    }
    if (!Number.isFinite(min)) min = 0;
    const span = max - min;

    const legendH = 18;
    const gridH = size(innerH - legendH);
    const cellW = size(innerW) / cols;
    const cellH = gridH / rows;
    const color = opts.color || colors.accent;
    const nightUntil = opts.nightUntil === undefined ? 6 : Math.max(0, finite(opts.nightUntil, 6));

    for (let r = 0; r < rows; r += 1) {
        for (let c = 0; c < cols; c += 1) {
            const v = finite(matrix[r][c], 0);
            const t = span > 0 ? clamp((v - min) / span, 0, 1) : 0;
            const cell = node(doc, 'rect', {
                class: 'fchart-cell',
                x: r2(x0 + cellW * c),
                y: r2(y0 + cellH * r),
                width: r2(size(cellW - 1)),
                height: r2(size(cellH - 1)),
                rx: 2,
                fill: color,
                // Минимум остаётся видимым пятном, а не дырой в подложке.
                'fill-opacity': r2(0.08 + 0.92 * t),
            });
            tip(doc, cell, `${rowLabels[r] || r}, ${colLabels ? colLabels[c] : `${c}:00`}: ${fmtV(v)}`);
            plot.appendChild(cell);
        }
    }

    // Подписи строк слева и часов снизу (каждый третий, чтобы не слипались).
    for (let r = 0; r < rows; r += 1) {
        plot.appendChild(
            text(doc, rowLabels[r] === undefined ? String(r) : rowLabels[r], {
                x: r2(Math.max(0, x0 - 6)),
                y: r2(y0 + cellH * r + cellH / 2),
                class: 'fchart-tick',
                fill: colors.inkDim,
                'text-anchor': 'end',
                'dominant-baseline': 'middle',
                'font-size': 10,
            }),
        );
    }
    const colStep = cols > 12 ? 3 : 1;
    for (let c = 0; c < cols; c += colStep) {
        plot.appendChild(
            text(doc, colLabels ? colLabels[c] : String(c), {
                x: r2(x0 + cellW * c + cellW / 2),
                y: r2(y0 + gridH + 10),
                class: 'fchart-tick',
                fill: colors.inkDim,
                'text-anchor': 'middle',
                'font-size': 10,
            }),
        );
    }

    // Скобка ночных часов.
    if (nightUntil > 0 && !colLabels) {
        const width = size(cellW * Math.min(nightUntil, cols));
        ctx.over.appendChild(
            node(doc, 'line', {
                class: 'fchart-night',
                x1: r2(x0),
                y1: r2(y0 + gridH + 14),
                x2: r2(x0 + width),
                y2: r2(y0 + gridH + 14),
                stroke: colors.inkDim,
                'stroke-width': 1,
            }),
        );
        ctx.over.appendChild(
            text(doc, opts.nightLabel || `ночь 00–0${Math.round(nightUntil)}`, {
                x: r2(x0 + width + 6),
                y: r2(y0 + gridH + 14),
                class: 'fchart-night-label',
                fill: colors.inkDim,
                'dominant-baseline': 'middle',
                'font-size': 10,
            }),
        );
    }

    // Легенда шкалы: пять ступеней той же непрозрачности + границы значений.
    const swW = 14;
    const legendY = y0 + gridH + (nightUntil > 0 ? 22 : 14);
    const legendX = ctx.x1 - swW * 5 - 4;
    ctx.over.appendChild(
        text(doc, fmtV(min), {
            x: r2(Math.max(x0, legendX - 6)),
            y: r2(legendY + 5),
            class: 'fchart-tick',
            fill: colors.inkDim,
            'text-anchor': 'end',
            'dominant-baseline': 'middle',
            'font-size': 10,
        }),
    );
    for (let i = 0; i < 5; i += 1) {
        ctx.over.appendChild(
            node(doc, 'rect', {
                class: 'fchart-scale',
                x: r2(Math.max(x0, legendX) + swW * i),
                y: r2(legendY),
                width: swW - 1,
                height: 9,
                rx: 2,
                fill: color,
                'fill-opacity': r2(0.08 + 0.92 * (i / 4)),
            }),
        );
    }
    ctx.over.appendChild(
        text(doc, fmtV(max), {
            x: r2(Math.max(x0, legendX) + swW * 5 + 2),
            y: r2(legendY + 5),
            class: 'fchart-tick',
            fill: colors.inkDim,
            'dominant-baseline': 'middle',
            'font-size': 10,
        }),
    );
    return ctx;
}, { padding: { left: 30, bottom: 12 } });

// ==========================================================================
// 8. band — тонкая полоса категорий по корзинам (версии CLI)
// ==========================================================================

/**
 * Тонкая полоса: одна ячейка на корзину, оттенок — по порядку появления.
 *
 * Ради чарта 13 плана («версии Claude Code по дням»): важен не размер, а
 * МОМЕНТ смены — когда человек обновился. Поэтому здесь нет ни оси Y, ни
 * значений: полоса, подписи дат под сменами и подсказка с точной версией.
 *
 * ПОЧЕМУ ОДИН ТОН, А НЕ ПАЛИТРА. Версий CLI за квартал набирается три
 * десятка. Пять цветов палитры по кругу превращают полосу в конфетти, а
 * «пять цветных плюс серое остальное» — в серую ленту, где смены не видно
 * вовсе. Один тон с растущей непрозрачностью по хронологии читается как
 * движение вперёд: чем правее и темнее, тем новее версия, а каждая смена —
 * видимая ступень. Тот же приём, что у тепловой карты.
 *
 * Правила:
 * * корзина без категории рисуется НЕЙТРАЛЬНЫМ цветом подложки, а не
 *   пропускается — дыра в полосе читалась бы как «этого дня не было»;
 * * несколько категорий в одной корзине (день обновления) делят её ячейку
 *   по вертикали, а подсказка перечисляет все;
 * * первая и последняя категории подписаны по краям — легенда на три
 *   десятка версий бесполезна.
 *
 * @param {{labels:string[], values:Array<string|string[]>}} data
 * @param {Object} [opts] — `emptyText`, `formatX`, `neutralLabel`
 */
export const band = chartFactory((ctx, data, opts) => {
    const labels = Array.isArray(data && data.labels) ? data.labels : [];
    const rawValues = Array.isArray(data && data.values) ? data.values : [];
    const cells = labels.map((_, i) => {
        const v = rawValues[i];
        if (Array.isArray(v)) return v.filter((x) => x !== null && x !== undefined && x !== '').map(String);
        return v === null || v === undefined || v === '' ? [] : [String(v)];
    });
    if (labels.length === 0) return empty(ctx, opts.emptyText);

    const { doc, plot, colors, x0, y0, innerW, innerH } = ctx;

    // Категории в порядке ПЕРВОГО появления; непрозрачность растёт по этому же
    // порядку, так что хронология читается слева направо.
    const ordered = [];
    for (const list of cells) for (const v of list) if (!ordered.includes(v)) ordered.push(v);
    const color = opts.color || colors.accent;
    const opacityOf = (name) => {
        const i = ordered.indexOf(name);
        if (i < 0 || ordered.length <= 1) return 1;
        return r2(0.3 + 0.7 * (i / (ordered.length - 1)));
    };

    const bandH = Math.min(size(innerH), 18);
    const yTop = y0 + Math.max(0, (size(innerH) - bandH) / 2);
    const cellW = size(innerW) / labels.length;

    for (let i = 0; i < labels.length; i += 1) {
        const list = cells[i];
        const x = x0 + cellW * i;
        if (list.length === 0) {
            const cell = node(doc, 'rect', {
                class: 'fchart-band-cell is-empty',
                x: r2(x),
                y: r2(yTop),
                width: r2(size(cellW)),
                height: r2(bandH),
                fill: colors.grid,
                'fill-opacity': 0.35,
            });
            tip(doc, cell, `${labels[i]}: ${opts.neutralLabel || 'версия неизвестна'}`);
            plot.appendChild(cell);
            continue;
        }
        const partH = bandH / list.length;
        for (let k = 0; k < list.length; k += 1) {
            const cell = node(doc, 'rect', {
                class: 'fchart-band-cell',
                x: r2(x),
                y: r2(yTop + partH * k),
                width: r2(size(cellW)),
                height: r2(size(partH)),
                fill: color,
                'fill-opacity': opacityOf(list[k]),
            });
            tip(doc, cell, `${labels[i]}: ${list.join(', ')}`);
            plot.appendChild(cell);
        }
    }

    // Подписи корзин, где версия сменилась, — ради них полоса и рисуется.
    //
    // Подписи, налезающие друг на друга, читаются хуже, чем их отсутствие,
    // поэтому между соседними держится минимальный зазор: при ежедневных
    // обновлениях остаются только те даты, что реально помещаются.
    const fmtX = typeof opts.formatX === 'function' ? opts.formatX : formatDay;
    const MIN_LABEL_GAP = 44;
    let prev = null;
    let lastLabelX = -Infinity;
    for (let i = 0; i < labels.length; i += 1) {
        const key = cells[i].join(',');
        if (!key || key === prev) continue;
        const x = x0 + cellW * i;
        if (prev !== null && x - lastLabelX >= MIN_LABEL_GAP) {
            lastLabelX = x;
            ctx.over.appendChild(
                text(doc, fmtX(labels[i]), {
                    x: r2(Math.min(x, ctx.x1 - 24)),
                    y: r2(yTop + bandH + 11),
                    class: 'fchart-tick',
                    fill: colors.inkDim,
                    'text-anchor': 'middle',
                    'font-size': 9,
                }),
            );
        }
        prev = key;
    }

    // Крайние версии подписаны текстом: это то, что человек хочет узнать с
    // одного взгляда — «с какой на какую я переехал за период».
    if (ordered.length > 0) {
        const edges = [
            { label: ordered[0], x: x0, anchor: 'start' },
            { label: ordered[ordered.length - 1], x: ctx.x1, anchor: 'end' },
        ];
        for (const e of edges.slice(0, ordered.length === 1 ? 1 : 2)) {
            ctx.over.appendChild(
                text(doc, e.label, {
                    x: r2(e.x),
                    y: r2(Math.max(10, yTop - 6)),
                    class: 'fchart-legend-label',
                    fill: colors.ink,
                    'text-anchor': e.anchor,
                    'font-size': 10,
                }),
            );
        }
    }
    return ctx;
}, { padding: { left: 12, right: 12, bottom: 18 }, legendHeight: 16 });
