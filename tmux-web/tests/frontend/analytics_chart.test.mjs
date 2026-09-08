// Тест SVG-кита аналитики (Фаза 4): tmux-web/static/js/analytics/chart.js.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Сломанный график не падает — он рисуется. Атрибут
// `width="NaN"` или `height="-12"` браузер молча игнорирует, и пользователь
// видит пустой прямоугольник там, где ждал данные; ошибки в консоли при этом
// нет. Ровно три входа ломают чарты в первый же день эксплуатации и ровно они
// здесь проверяются у КАЖДОГО из семи типов:
//
//   • пустой ряд — первый скан ещё идёт, данных нет;
//   • одна точка — период «сегодня» или новый проект;
//   • все нули — период выбран, а активности в нём не было.
//
// Плюс два договора, которые нельзя проверить глазами:
//
//   • ЦВЕТА ИЗ ТЕМЫ. Подменяем `getComputedStyle` метками-сентинелами и
//     требуем, чтобы в `fill`/`stroke` попали ТОЛЬКО они. Отдельный прогон с
//     пустой темой: ни одного хекса не появляется даже когда переменных нет.
//   • ОТЗЫВЧИВОСТЬ. `viewBox` + `preserveAspectRatio` + `width="100%"`: без них
//     график перестаёт тянуться за карточкой на узком экране.
//
// Мутационная проверка: уберите `clamp` в любом чарте — покраснеет проверка
// отрицательных координат; замените `currentColor` на хекс — покраснеет
// проверка темы; поменяйте порядок SERIES_TOKENS — покраснеет страж палитры.
//
// DOM здесь свой, крошечный: кит намеренно берёт документ из
// `container.ownerDocument`, поэтому jsdom не нужен.
//
// Запуск: node tmux-web/tests/frontend/analytics_chart.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const C = await import(new URL('../../static/js/analytics/chart.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

function eq(actual, expected, label) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    checks += 1;
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

// ---------------------------------------------------------------------------
// Фейковый документ: ровно то, чем пользуется кит.
// ---------------------------------------------------------------------------

function makeDoc(vars) {
    const doc = {
        documentElement: null,
        defaultView: {
            getComputedStyle() {
                return { getPropertyValue: (name) => (vars && vars[name]) || '' };
            },
        },
        createElementNS(ns, tag) {
            const n = {
                namespaceURI: ns,
                tagName: String(tag),
                childNodes: [],
                attributes: {},
                style: {},
                textContent: '',
                parentNode: null,
                get firstChild() {
                    return n.childNodes[0] || null;
                },
                appendChild(child) {
                    child.parentNode = n;
                    n.childNodes.push(child);
                    return child;
                },
                removeChild(child) {
                    n.childNodes = n.childNodes.filter((x) => x !== child);
                    child.parentNode = null;
                    return child;
                },
                setAttribute(k, v) {
                    n.attributes[k] = String(v);
                },
                getAttribute(k) {
                    return k in n.attributes ? n.attributes[k] : null;
                },
            };
            return n;
        },
    };
    doc.documentElement = doc.createElementNS('http://www.w3.org/1999/xhtml', 'html');
    return doc;
}

function makeContainer(doc) {
    const el = doc.createElementNS('http://www.w3.org/1999/xhtml', 'div');
    el.ownerDocument = doc;
    return el;
}

function walk(n, fn) {
    fn(n);
    for (const c of n.childNodes || []) walk(c, fn);
}

function nodes(root) {
    const out = [];
    walk(root, (n) => out.push(n));
    return out;
}

function textOf(root) {
    return nodes(root)
        .map((n) => n.textContent || '')
        .join('\n');
}

// Сентинелы: по значению видно, из какой переменной цвет пришёл.
const THEME = {
    '--fg': 'ink',
    '--fg-dim': 'inkdim',
    '--border': 'grid',
    '--bg-elev': 'surface',
    '--accent': 's1',
    '--warn': 's2',
    '--agent-accent': 's3',
    '--success': 's4',
    '--danger': 's5',
};
const THEME_VALUES = new Set(Object.values(THEME));
const NON_COLOR = new Set(['none', 'currentColor', '']);

const GEOM_ATTRS = ['x', 'y', 'width', 'height', 'r', 'rx', 'ry', 'cx', 'cy', 'stroke-width', 'fill-opacity'];

/**
 * Единый разбор нарисованного SVG.
 * Возвращает список нарушений — пустой список и есть «чарт валиден».
 */
function auditSvg(svg, label) {
    const bad = [];
    for (const n of nodes(svg)) {
        for (const [k, v] of Object.entries(n.attributes || {})) {
            if (/NaN|undefined|null|Infinity/.test(v)) bad.push(`${label}: <${n.tagName} ${k}="${v}">`);
            if (GEOM_ATTRS.includes(k) && !String(v).endsWith('%') && Number(v) < 0) {
                bad.push(`${label}: отрицательное <${n.tagName} ${k}="${v}">`);
            }
            if (k === 'd' && !/^[MmLlAaHhVvZzCcSs0-9eE.,\s-]+$/.test(v)) {
                bad.push(`${label}: путь с мусором <path d="${v}">`);
            }
        }
    }
    return bad;
}

/** Цвета фигур: всё, что попало в fill/stroke. */
function colorsUsed(svg) {
    const out = new Set();
    for (const n of nodes(svg)) {
        for (const k of ['fill', 'stroke']) {
            const v = n.attributes ? n.attributes[k] : undefined;
            if (v !== undefined && !NON_COLOR.has(v)) out.add(v);
        }
    }
    return out;
}

// ---------------------------------------------------------------------------
// 1. Примитивы: шкалы и деления
// ---------------------------------------------------------------------------

{
    const zero = C.niceTicks(0, 0);
    ok(zero.ticks.length > 1, 'niceTicks[0,0]: делений больше одного');
    ok(zero.max > zero.min, 'niceTicks[0,0]: шкала не схлопнулась');
    ok(zero.ticks.every((t) => Number.isFinite(t)), 'niceTicks[0,0]: все деления конечны');

    const one = C.niceTicks(7, 7);
    ok(one.ticks.length > 1, 'niceTicks[7,7]: делений больше одного');
    ok(one.max >= 7, 'niceTicks[7,7]: точка внутри шкалы');

    const wide = C.niceTicks(0, 1234);
    ok(wide.max >= 1234, 'niceTicks: верх покрывает максимум');
    ok(wide.ticks.every((t, i, a) => i === 0 || t > a[i - 1]), 'niceTicks: деления возрастают');

    // Вырожденный домен не даёт деления на ноль: точка ложится в середину.
    const s = C.linearScale(0, 0, 100, 200);
    eq(s(0), 150, 'linearScale: вырожденный домен → середина диапазона');
    ok(Number.isFinite(C.linearScale(0, 10, 0, 100)(NaN)), 'linearScale: NaN на входе не даёт NaN на выходе');

    eq(C.size(-5), 0, 'size: отрицательный размер обнуляется');
    eq(C.finite('x', 3), 3, 'finite: нечисло → запасное значение');
    eq(C.clamp(15, 0, 10), 10, 'clamp: зажим сверху');

    eq(C.formatDay('2026-09-06'), '06.09', 'formatDay: ISO-дата → дд.мм');
    eq(C.formatMs(820), '820 мс', 'formatMs: миллисекунды');
    eq(C.formatMs(12345), '12,3 с', 'formatMs: секунды');
    eq(C.formatMs(300000), '5,0 мин', 'formatMs: минуты');
    eq(C.formatUsd(0), '$0', 'formatUsd: ноль');
    ok(C.formatUsd(0.004) !== '$0', 'formatUsd: мелкая сумма не схлопывается в ноль');
    eq(C.formatPercent(0.423), '42%', 'formatPercent: доля → проценты');
    eq(C.formatCompact(1500), '1,5k', 'formatCompact: тысячи');
}

// ---------------------------------------------------------------------------
// 2. Палитра серий
// ---------------------------------------------------------------------------

{
    // Порядок слотов проверен валидатором палитры (ΔE 10.9 при дейтеранопии,
    // 33.1 при обычном зрении на худшей соседней паре). Перестановка ухудшает
    // цифры, поэтому список зафиксирован здесь.
    eq(C.SERIES_TOKENS, ['--accent', '--warn', '--agent-accent', '--success', '--danger'], 'порядок слотов палитры зафиксирован');
    eq(C.SERIES_MAX, 5, 'цветных серий не больше пяти');

    const folded = C.foldSeries(
        Array.from({ length: 9 }, (_, i) => ({ label: 'm' + i, value: i + 1 })),
    );
    eq(folded.length, C.SERIES_MAX, 'foldSeries: хвост свёрнут до SERIES_MAX позиций');
    eq(folded[folded.length - 1].label, 'Другое', 'foldSeries: последняя позиция — «Другое»');
    eq(folded[folded.length - 1].value, 5 + 6 + 7 + 8 + 9, 'foldSeries: значения хвоста просуммированы');

    const doc = makeDoc(THEME);
    const colors = C.themeColors(makeContainer(doc));
    eq(colors.at(0), 's1', 'themeColors: слот 1 — --accent');
    eq(colors.at(4), 's5', 'themeColors: слот 5 — --danger');
    eq(colors.at(9), 'inkdim', 'themeColors: сверх палитры — приглушённый «Другое»');

    // Тема имеет право задать свои слоты.
    const custom = C.themeColors(makeContainer(makeDoc(Object.assign({}, THEME, { '--chart-1': 'brand' }))));
    eq(custom.at(0), 'brand', 'themeColors: --chart-1 перебивает --accent');

    // Пустая тема: последний рубеж — currentColor, а не хекс.
    const bare = C.themeColors(makeContainer(makeDoc({})));
    eq(bare.at(0), 'currentColor', 'themeColors: без переменных — currentColor');
    eq(bare.ink, 'currentColor', 'themeColors: текст без переменных — currentColor');
}

// ---------------------------------------------------------------------------
// 3. Все чарты × три обязательных состояния
// ---------------------------------------------------------------------------

const week = () => Array.from({ length: 7 }, (_, r) => Array.from({ length: 24 }, (_, c) => (r + c) % 5));

const CASES = [
    // [имя, функция, данные пустые, данные «одна точка», данные «все нули», данные боевые]
    [
        'barChart',
        C.barChart,
        { labels: [], values: [] },
        { labels: ['2026-09-01'], values: [7], overlay: { label: 'сессии', values: [1] } },
        { labels: ['2026-09-01', '2026-09-02'], values: [0, 0], overlay: { label: 'сессии', values: [0, 0] } },
        { labels: ['2026-09-01', '2026-09-02', '2026-09-03'], values: [12, 4, 30], overlay: { label: 'сессии', values: [1, 0, 2] } },
    ],
    [
        'lineChart',
        C.lineChart,
        { labels: [], series: [] },
        { labels: ['2026-09-01'], series: [{ label: 'p50', values: [1200] }] },
        { labels: ['a', 'b'], series: [{ label: 'p50', values: [0, 0] }, { label: 'p90', values: [0, 0] }] },
        { labels: ['a', 'b', 'c'], series: [{ label: 'p50', values: [10, 20, 15] }, { label: 'p90', values: [40, 55, 50] }] },
    ],
    [
        'hbars',
        C.hbars,
        [],
        [{ label: 'Bash', value: 10, secondary: 3 }],
        [{ label: 'Bash', value: 0 }, { label: 'Read', value: 0 }],
        [{ label: 'Bash', value: 120, secondary: 8 }, { label: 'Read', value: 90 }, { label: 'Edit', value: 40 }],
    ],
    [
        'donut',
        C.donut,
        [],
        [{ label: 'opus', value: 5 }],
        [{ label: 'opus', value: 0 }, { label: 'haiku', value: 0 }],
        Array.from({ length: 9 }, (_, i) => ({ label: 'model-' + i, value: i + 1 })),
    ],
    [
        'stackedArea',
        C.stackedArea,
        { labels: [], series: [] },
        { labels: ['2026-09-01'], series: [{ label: 'in', values: [5] }, { label: 'out', values: [2] }] },
        { labels: ['a', 'b', 'c'], series: [{ label: 'in', values: [0, 0, 0] }, { label: 'out', values: [0, 0, 0] }] },
        { labels: ['a', 'b', 'c'], series: [{ label: 'in', values: [5, 7, 1] }, { label: 'out', values: [2, 0, 9] }, { label: 'cache', values: [50, 40, 60] }] },
    ],
    [
        'histogram',
        C.histogram,
        { labels: [], values: [] },
        { labels: ['<5 c'], values: [4] },
        { labels: ['<5 c', '5–15 c', '>60 c'], values: [0, 0, 0] },
        { labels: ['<5 c', '5–15 c', '15–60 c', '1–5 мин', '>5 мин'], values: [40, 22, 9, 3, 1] },
    ],
    [
        'heatmap',
        C.heatmap,
        { matrix: [] },
        { matrix: [[3]] },
        { matrix: Array.from({ length: 7 }, () => new Array(24).fill(0)) },
        { matrix: week() },
    ],
    [
        'band',
        C.band,
        { labels: [], values: [] },
        { labels: ['2026-09-01'], values: [['2.1.227']] },
        { labels: ['a', 'b', 'c'], values: [[], [], []] },
        {
            labels: ['2026-09-01', '2026-09-02', '2026-09-03', '2026-09-04'],
            values: [['2.1.227'], ['2.1.227'], ['2.1.227', '2.1.230'], ['2.1.230']],
        },
    ],
];

for (const [name, fn, dataEmpty, dataOne, dataZero, dataReal] of CASES) {
    const doc = makeDoc(THEME);
    const states = [
        ['пусто', dataEmpty],
        ['одна точка', dataOne],
        ['все нули', dataZero],
        ['боевые данные', dataReal],
    ];
    for (const [state, data] of states) {
        const el = makeContainer(doc);
        let api = null;
        let thrown = null;
        try {
            api = fn(el, data, {});
        } catch (e) {
            thrown = e;
        }
        ok(!thrown, `${name} / ${state}: рисуется без исключения${thrown ? ' — ' + thrown.message : ''}`);
        if (!api) continue;

        // Один <svg> в контейнере — и он же корень чарта.
        eq(el.childNodes.length, 1, `${name} / ${state}: в контейнере ровно один узел`);
        eq(api.svg.tagName, 'svg', `${name} / ${state}: корень — <svg>`);
        eq(api.svg.namespaceURI, 'http://www.w3.org/2000/svg', `${name} / ${state}: корень в пространстве имён SVG`);

        // Отзывчивость.
        ok(/^0 0 \d+(\.\d+)? \d+(\.\d+)?$/.test(api.svg.getAttribute('viewBox') || ''), `${name} / ${state}: есть корректный viewBox`);
        eq(api.svg.getAttribute('preserveAspectRatio'), 'xMidYMid meet', `${name} / ${state}: есть preserveAspectRatio`);
        eq(api.svg.getAttribute('width'), '100%', `${name} / ${state}: ширина 100%, а не пиксели`);

        // Геометрия.
        const bad = auditSvg(api.svg, `${name} / ${state}`);
        ok(bad.length === 0, `${name} / ${state}: ни NaN, ни отрицательных координат\n       ${bad.join('\n       ')}`);

        // Цвета — только из темы.
        const used = [...colorsUsed(api.svg)];
        const alien = used.filter((c) => !THEME_VALUES.has(c));
        ok(alien.length === 0, `${name} / ${state}: цвета только из CSS-переменных (чужие: ${alien.join(', ')})`);
    }

    // Пустой ряд обязан объяснить себя пользователю.
    const elEmpty = makeContainer(doc);
    const empty = fn(elEmpty, dataEmpty, {});
    ok(textOf(empty.svg).includes('нет данных'), `${name}: пустой ряд рисует заглушку «нет данных»`);

    // Все нули: ось живая — делений больше одного (у heatmap оси нет).
    if (name !== 'heatmap' && name !== 'donut' && name !== 'hbars' && name !== 'band') {
        const elZero = makeContainer(doc);
        const zero = fn(elZero, dataZero, {});
        const ticks = nodes(zero.svg).filter((n) => (n.attributes || {}).class === 'fchart-tick');
        ok(ticks.length > 1, `${name}: на нулевых данных шкала не схлопнулась (делений ${ticks.length})`);
    }

    // update() перерисовывает НА МЕСТЕ, а не плодит вторую канву.
    const elUpd = makeContainer(doc);
    const api = fn(elUpd, dataEmpty, {});
    const first = api.svg;
    api.update(dataReal);
    eq(elUpd.childNodes.length, 1, `${name}: update() не оставляет старую канву`);
    ok(api.svg !== first, `${name}: update() построил новую канву`);
    ok(auditSvg(api.svg, name).length === 0, `${name}: после update() геометрия валидна`);
}

// ---------------------------------------------------------------------------
// 4. Подсказки и легенды — идентичность не только цветом
// ---------------------------------------------------------------------------

{
    const doc = makeDoc(THEME);
    const el = makeContainer(doc);
    const api = C.lineChart(el, {
        labels: ['2026-09-01', '2026-09-02'],
        series: [{ label: 'медиана', values: [10, 20] }, { label: 'p90', values: [40, 50] }],
    });
    const titles = nodes(api.svg).filter((n) => n.tagName === 'title');
    ok(titles.length >= 4, 'lineChart: у каждой точки есть <title> с подписью');
    ok(titles.some((t) => t.textContent.includes('медиана')), 'lineChart: подсказка называет серию');
    const legendLabels = nodes(api.svg)
        .filter((n) => (n.attributes || {}).class === 'fchart-legend-label')
        .map((n) => n.textContent);
    eq(legendLabels, ['медиана', 'p90'], 'lineChart: две серии — легенда обязательна');
    // Текст легенды носит текстовый токен, а не цвет серии.
    const legendFills = nodes(api.svg)
        .filter((n) => (n.attributes || {}).class === 'fchart-legend-label')
        .map((n) => n.attributes.fill);
    eq(legendFills, ['ink', 'ink'], 'lineChart: подпись легенды окрашена текстовым токеном');

    // Одна серия — легенды нет: её называет заголовок карточки.
    const single = C.lineChart(makeContainer(doc), { labels: ['a', 'b'], series: [{ label: 'один', values: [1, 2] }] });
    eq(
        nodes(single.svg).filter((n) => (n.attributes || {}).class === 'fchart-legend-label').length,
        0,
        'lineChart: одна серия — легенда не рисуется',
    );

    // Вторая ось barChart масштабируется независимо от первой.
    const bars = C.barChart(makeContainer(doc), {
        labels: ['a', 'b'],
        values: [1000, 2000],
        overlay: { label: 'сессии', values: [1, 2] },
    });
    const rightTicks = nodes(bars.svg)
        .filter((n) => (n.attributes || {}).class === 'fchart-tick' && n.attributes['text-anchor'] === 'start')
        .map((n) => n.textContent);
    ok(rightTicks.length > 1, 'barChart: у наложенной линии своя ось справа');
    ok(
        rightTicks.every((t) => Number(String(t).replace(',', '.').replace('k', '')) <= 10),
        `barChart: правая ось масштабируется по своему ряду (${rightTicks.join(', ')})`,
    );

    // Heatmap на нулях: ровный минимум, а не деление на ноль.
    const heat = C.heatmap(makeContainer(doc), { matrix: Array.from({ length: 7 }, () => new Array(24).fill(0)) });
    const cells = nodes(heat.svg).filter((n) => (n.attributes || {}).class === 'fchart-cell');
    eq(cells.length, 7 * 24, 'heatmap: нарисованы все 168 ячеек');
    const opacities = new Set(cells.map((n) => n.attributes['fill-opacity']));
    eq([...opacities], ['0.08'], 'heatmap: на нулевой матрице все ячейки в минимуме шкалы');
    ok(textOf(heat.svg).includes('ночь'), 'heatmap: ночные часы подписаны отдельно');

    // Гистограмма: пустой бакет сохраняет подпись.
    const hist = C.histogram(makeContainer(doc), { labels: ['<5 c', '5–15 c'], values: [3, 0] });
    const barsDrawn = nodes(hist.svg).filter((n) => (n.attributes || {}).class === 'fchart-bar');
    eq(barsDrawn.length, 2, 'histogram: пустой бакет остаётся столбиком');
    ok(textOf(hist.svg).includes('5–15 c'), 'histogram: подпись пустого бакета на месте');
}

// ---------------------------------------------------------------------------
// 4b. band — полоса версий CLI (чарт 13)
// ---------------------------------------------------------------------------

{
    const doc = makeDoc(THEME);
    const api = C.band(makeContainer(doc), {
        labels: ['2026-09-01', '2026-09-02', '2026-09-03', '2026-09-04'],
        values: [['2.1.227'], [], ['2.1.227', '2.1.230'], ['2.1.230']],
    });
    const cells = nodes(api.svg).filter((n) => String((n.attributes || {}).class || '').startsWith('fchart-band-cell'));
    // 1 + 1(пустая) + 2(день обновления) + 1 = 5 ячеек.
    eq(cells.length, 5, 'band: день с двумя версиями делит ячейку, пустой день рисуется');
    const neutral = cells.filter((n) => n.attributes.class.includes('is-empty'));
    eq(neutral.length, 1, 'band: день без версии — ровно одна нейтральная ячейка');
    eq(neutral[0].attributes.fill, 'grid', 'band: нейтральная ячейка красится токеном подложки');
    const titles = nodes(api.svg).filter((n) => n.tagName === 'title').map((t) => t.textContent);
    ok(titles.some((t) => t.includes('2.1.227, 2.1.230')), 'band: тултип перечисляет все версии дня');
    ok(titles.some((t) => t.includes('версия неизвестна')), 'band: у пустого дня тултип объясняет пустоту');
    const edgeLabels = nodes(api.svg)
        .filter((n) => (n.attributes || {}).class === 'fchart-legend-label')
        .map((n) => n.textContent);
    eq(edgeLabels, ['2.1.227', '2.1.230'], 'band: по краям подписаны первая и последняя версии периода');

    // Три десятка версий за период — именно это даёт живой архив. Полоса
    // обязана остаться читаемой: один тон, ступени непрозрачности по
    // хронологии, серый только у дней без данных.
    const many = C.band(makeContainer(doc), {
        labels: Array.from({ length: 12 }, (_, i) => `2026-09-${String(i + 1).padStart(2, '0')}`),
        values: Array.from({ length: 12 }, (_, i) => [`2.1.${200 + i}`]),
    });
    const manyCells = nodes(many.svg).filter((n) => String((n.attributes || {}).class || '') === 'fchart-band-cell');
    eq(manyCells.length, 12, 'band: ячейка на каждый день');
    const grey = manyCells.filter((n) => n.attributes.fill === 'grid');
    eq(grey.length, 0, 'band: у дней с версией нейтрального цвета нет');
    const ops = manyCells.map((n) => Number(n.attributes['fill-opacity']));
    ok(ops[0] < ops[ops.length - 1], 'band: непрозрачность растёт по хронологии');
    eq(new Set(ops).size, 12, 'band: каждая версия — своя ступень шкалы');
    const manyLabels = nodes(many.svg).filter((n) => (n.attributes || {}).class === 'fchart-legend-label')
        .map((n) => n.textContent);
    eq(manyLabels, ['2.1.200', '2.1.211'], 'band: подписаны только крайние версии периода');
}

// ---------------------------------------------------------------------------
// 4c. Разворот матрицы активности (view-core)
// ---------------------------------------------------------------------------

{
    const V = await import(new URL('../../static/js/analytics/view-core.js', import.meta.url).href);
    // [час][день недели] → [день недели][час].
    const src = Array.from({ length: 24 }, (_, h) => Array.from({ length: 7 }, (_, d) => h * 10 + d));
    const m = V.weekdayHourMatrix(src);
    eq(m.length, 7, 'weekdayHourMatrix: семь строк — дни недели');
    eq(m[0].length, 24, 'weekdayHourMatrix: 24 столбца — часы');
    eq(m[3][5], 53, 'weekdayHourMatrix: значение [час][день] переехало в [день][час]');
    eq(V.weekdayHourMatrix(undefined).length, 7, 'weekdayHourMatrix: пустой вход даёт матрицу нулей');
    ok(!V.hasHeat(V.weekdayHourMatrix(undefined)), 'hasHeat: матрица нулей — не данные');
    ok(V.hasHeat(m), 'hasHeat: непустая матрица — данные');
}

// ---------------------------------------------------------------------------
// 4b. Читаемость рядов на ЛЮБОЙ теме
// ---------------------------------------------------------------------------
//
// Палитра берётся из пользовательской темы, а темы бывают и светлые. Цвет,
// прекрасно видимый на тёмной подложке, на светлой исчезает: #5acf90 даёт
// 8.9:1 на #161b22 и 1.6:1 на #e6e9ef. Ряд, которого не видно, — это молча
// потерянные данные, поэтому кит доводит цвет до порога сам.

{
    const lum = (hex) => {
        const h = hex.replace('#', '');
        const v = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
        const f = (c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
        return 0.2126 * f(v[0]) + 0.7152 * f(v[1]) + 0.0722 * f(v[2]);
    };
    const ratio = (a, b) => {
        const la = lum(a);
        const lb = lum(b);
        return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
    };

    const LIGHT = '#e6e9ef';
    const DARK = '#161b22';

    // Зелёный на светлой подложке — худший случай реальной темы.
    const fixed = C.contrastify('#5acf90', LIGHT);
    ok(ratio(fixed, LIGHT) >= 3,
        'зелёный ряд доведён до читаемости на светлой теме',
        `${fixed} → ${ratio(fixed, LIGHT).toFixed(2)}:1`);
    ok(fixed !== '#5acf90', 'и именно доведён, а не оставлен как был');

    // Оттенок сохраняется: зелёный остаётся зелёным, а не становится серым.
    {
        const h = fixed.replace('#', '');
        const [r, g, b] = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
        ok(g > r && g > b, 'оттенок сохранён — зелёный остался зелёным', fixed);
    }

    // На тёмной подложке тот же цвет уже читаем — трогать его незачем.
    eq(C.contrastify('#5acf90', DARK), '#5acf90', 'достаточный контраст не переделывается');

    // Тёмная тема лечится в другую сторону: фиолетовый тянется к белому.
    const dim = C.contrastify('#3b1d6e', DARK);
    ok(ratio(dim, DARK) >= 3, 'на тёмной подложке цвет осветляется', `${dim} → ${ratio(dim, DARK).toFixed(2)}:1`);

    // Неразбираемые значения не трогаем: гадать вслепую хуже.
    eq(C.contrastify('currentColor', LIGHT), 'currentColor', 'currentColor остаётся как есть');
    eq(C.contrastify('#2a7fff', 'color-mix(in srgb, red, blue)'), '#2a7fff', 'непонятная подложка — цвет не меняем');
}

// ---------------------------------------------------------------------------
// 4c. Подпись горизонтальной полосы читается всегда
// ---------------------------------------------------------------------------
//
// Названия в «Топ инструментов» длинные (`mcp__claude-in-chrome__computer`), а
// полосы после лидера короткие. Подпись цветом `--fg` внутри полосы ложилась
// поверх заливки серии и пропадала — ровно у самых частых инструментов.

{
    // Настоящие цвета светлой темы: на синей заливке тёмный `--fg` нечитаем.
    const doc = makeDoc({
        '--fg': '#4c4f69', '--fg-dim': '#6c6f85', '--border': '#ccd0da',
        '--bg-elev': '#e6e9ef', '--accent': '#1e66f5',
    });
    const api = C.hbars(makeContainer(doc), [
        { label: 'Bash', value: 1000 },
        { label: 'mcp__claude-in-chrome__computer', value: 5 },
    ]);

    const labels = nodes(api.svg).filter((n) => (n.attributes || {}).class === 'fchart-hbar-label');
    eq(labels.length, 2, 'обе подписи нарисованы');
    const short = labels.find((t) => t.textContent === 'Bash');
    const long = labels.find((t) => /chrome/.test(t.textContent));

    // Короткое имя умещается в длинную полосу — красится читаемо на заливке,
    // то есть НЕ обычным цветом текста.
    ok(short && short.attributes.fill !== '#4c4f69',
        'подпись внутри полосы не красится тёмным по синему',
        short ? String(short.attributes.fill) : 'подписи нет');

    // Длинное имя в короткую полосу не влезает — уезжает правее её конца, на
    // подложку карточки, и там обычный цвет текста как раз верен.
    ok(long && Number(long.attributes.x) > Number(short.attributes.x),
        'не поместившаяся подпись вынесена за полосу',
        long ? `short.x=${short.attributes.x} long.x=${long.attributes.x}` : 'подписи нет');
    eq(long.attributes.fill, '#4c4f69', 'вынесенная подпись — обычным цветом текста');
}

// ---------------------------------------------------------------------------
// 4d. Размер чарта берётся у контейнера, легенда не уезжает за край
// ---------------------------------------------------------------------------
//
// `viewBox` масштабирует ВСЁ, включая кегль. Чарт с фиксированной шириной 720
// в слоте на 278px сжимался в 0.39 раза, и подписи осей падали до 3.9px.

{
    const doc = makeDoc(THEME);
    const host = makeContainer(doc);
    host.clientWidth = 460;
    const api = C.lineChart(host, { labels: ['a', 'b'], series: [{ label: 'x', values: [1, 2] }] });
    eq(api.svg.attributes.viewBox, '0 0 460 170',
        'ширина viewBox равна ширине контейнера — масштаб 1:1, кегль не плывёт');

    // Явная высота уважается: у ленты версий она намеренно мала.
    const low = C.barChart(Object.assign(makeContainer(doc), { clientWidth: 900 }),
        { labels: ['a'], values: [1] }, { height: 90 });
    eq(low.svg.attributes.viewBox, '0 0 900 90', 'заданная высота не подменяется');

    // Контейнера без вёрстки (тесты, скрытый DOM) быть не должно достаточно,
    // чтобы чарт не нарисовался: работает запасная ширина.
    const bare = C.barChart(makeContainer(doc), { labels: ['a'], values: [1] });
    ok(/^0 0 720 /.test(bare.svg.attributes.viewBox),
        'без измеримого контейнера — запасная ширина', String(bare.svg.attributes.viewBox));

    // Легенда из длинных имён усекается по своей доле, а не режется краем svg.
    const narrow = makeContainer(doc);
    narrow.clientWidth = 380;
    const donut = C.donut(narrow, [
        { label: 'claude-haiku-4-5-20251001', value: 3 },
        { label: 'claude-sonnet-5-20250929', value: 2 },
        { label: 'claude-opus-5-1m-preview', value: 1 },
    ]);
    const labels = nodes(donut.svg)
        .filter((n) => (n.attributes || {}).class === 'fchart-legend-label')
        .map((n) => n.textContent);
    eq(labels.length, 3, 'все три модели названы');
    ok(labels.every((l) => l.length <= 22), 'длинные имена усечены', JSON.stringify(labels));
    ok(labels.some((l) => /…$/.test(l)), 'усечение помечено многоточием', JSON.stringify(labels));
}

// ---------------------------------------------------------------------------
// 4e. Наведение: вертикаль по ближайшему дню и все ряды в одной подсказке
// ---------------------------------------------------------------------------
//
// Нативный `<title>` требует попасть в точку радиусом 5px и показывается через
// секунду — на дневном графике это читается как «наведение не работает». Плюс
// он называет ОДИН ряд, а вопрос к такому графику — «что было в этот день у
// всех рядов». Слой ловит движение по всей области данных.
//
// ЛОВУШКА, НА КОТОРОЙ ЭТО УЖЕ ЛОМАЛОСЬ: в SVG события рождаются только на
// нарисованном. Без прозрачного прямоугольника-ловца курсор над пустым местом
// графика не порождает ничего, и слой молчит.

{
    // Документ с событиями и геометрией: минимум, которого нет у makeDoc.
    const doc = makeDoc(THEME);
    const baseNS = doc.createElementNS;
    doc.createElementNS = (ns, tag) => {
        const n = baseNS.call(doc, ns, tag);
        n.handlers = {};
        n.addEventListener = (type, fn) => { (n.handlers[type] = n.handlers[type] || []).push(fn); };
        n.getBoundingClientRect = () => ({ left: 0, top: 0, width: 600, height: 300 });
        return n;
    };
    const host = makeContainer(doc);
    host.clientWidth = 600;

    const api = C.lineChart(host, {
        labels: ['2026-09-01', '2026-09-02', '2026-09-03'],
        series: [
            { label: 'медиана', values: [10, 20, 30] },
            { label: 'p90', values: [40, 50, 60] },
        ],
    }, { height: 300 });

    const catcher = nodes(api.svg).find((n) => (n.attributes || {}).class === 'fchart-hover-catch');
    ok(catcher, 'ловец событий нарисован — иначе пустое место графика не реагирует');
    eq(catcher.attributes.fill, 'transparent', 'ловец прозрачен, но кликабелен');
    eq(catcher.attributes['pointer-events'], 'all', 'и ловит указатель');

    const layer = nodes(api.svg).find((n) => (n.attributes || {}).class === 'fchart-hover');
    eq(layer.attributes.visibility, 'hidden', 'до наведения слой скрыт');

    // Курсор у правого края: ближайший день — третий.
    catcher.handlers.mousemove.forEach((fn) => fn({ clientX: 580, clientY: 150 }));
    eq(layer.attributes.visibility, 'visible', 'наведение показывает слой');
    const texts = nodes(layer).filter((n) => n.tagName === 'text').map((n) => n.textContent);
    eq(texts[0], '03.09', 'заголовок подсказки — день под курсором');
    eq(texts.slice(1), ['медиана: 30', 'p90: 60'], 'в подсказке ВСЕ ряды этого дня, а не один');

    // У левого края области данных — первый день: выбирается ближайшая
    // корзина, а не деление на шаг (при одной корзине шага нет вовсе).
    catcher.handlers.mousemove.forEach((fn) => fn({ clientX: 60, clientY: 150 }));
    const first = nodes(layer).filter((n) => n.tagName === 'text').map((n) => n.textContent);
    eq(first[0], '01.09', 'у левого края — первый день');

    // Курсор ВНЕ области данных прячет слой: подсказка про день, над которым
    // мыши уже нет, — хуже, чем её отсутствие.
    catcher.handlers.mousemove.forEach((fn) => fn({ clientX: 5, clientY: 150 }));
    eq(layer.attributes.visibility, 'hidden', 'за пределами поля слой прячется');

    catcher.handlers.mouseleave.forEach((fn) => fn());
    eq(layer.attributes.visibility, 'hidden', 'курсор ушёл — слой скрылся');
}

// ---------------------------------------------------------------------------
// 5. Стражи файла: без библиотек, без innerHTML, без хардкод-цветов
// ---------------------------------------------------------------------------

{
    const src = readFileSync(new URL('../../static/js/analytics/chart.js', import.meta.url), 'utf8');
    ok(!/\.innerHTML/.test(src) && !/innerHTML\s*=/.test(src), 'chart.js: innerHTML не используется');
    ok(!/^\s*import\s/m.test(src), 'chart.js: ни одного импорта — кит самодостаточен');
    ok(!/from\s+['"]https?:/.test(src), 'chart.js: нет внешних библиотек');
    // Хардкод-цвет ищем в КОДЕ, а не в объяснениях: комментарий, называющий
    // конкретный #rrggbb из темы (например, разбор того, почему ряд исчезает на
    // светлом фоне), — это документация, а не палитра. Образец чистки — страж
    // вкладки «Встречи» в settings_meet_tab.test.mjs.
    const code = src
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .split('\n')
        .filter((l) => !l.trimStart().startsWith('//'))
        .join('\n');
    ok(!/#[0-9a-fA-F]{3,8}\b/.test(code), 'chart.js: ни одного хардкод-цвета');
    ok(!/\bcreateElement\s*\(/.test(src), 'chart.js: узлы только через createElementNS');
    ok(/preserveAspectRatio/.test(src) && /viewBox/.test(src), 'chart.js: отзывчивость через viewBox + preserveAspectRatio');

    // Пустая тема не должна порождать цвет из воздуха.
    const doc = makeDoc({});
    const api = C.barChart(makeContainer(doc), { labels: ['a', 'b'], values: [1, 2] });
    const used = [...colorsUsed(api.svg)];
    eq(used, [], 'без переменных темы в fill/stroke остаётся только currentColor');
}

console.log(`${checks - failures}/${checks} проверок прошли`);
if (failures) console.error(`${failures} проверок упали`);
process.exit(failures ? 1 : 0);
