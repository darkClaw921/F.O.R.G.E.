// tmux-web — полноэкранный дашборд «Аналитика Claude Code» (Фаза 5).
//
// Отдельная вью #analytics в #main — сосед #daily-summary. Показывает, КАК
// человек работает с ассистентом: сколько сообщений, куда уходит время,
// сколько стоит, и главное — сколько он думает перед тем, как согласиться.
// Источник — индекс транскриптов `~/.claude/projects/**/*.jsonl`, собранный
// бэкендом (`tmux-web/src/analytics/*`, Фазы 1–3).
//
// МЕХАНИКА ПОКАЗА — ТОЧНАЯ КОПИЯ «Сводки дня»
// (`js/daily-summary/daily-summary.js::showDailySummary`): вью занимает #main
// целиком, поэтому при открытии скрывает #home и placeholder терминала, а при
// закрытии зовёт fetchSessions() — тот сам решает, показать #home или активную
// сессию. Своей «истории вью» в devforge нет, и заводить её ради одного
// экрана — лишняя сущность.
//
// НИКАКОГО innerHTML. Весь дашборд строится узлами: подписи проектов и
// инструментов приходят из транскриптов, то есть с диска пользователя, и
// путь до `innerHTML` для таких строк не должен существовать в принципе.
//
// СЕТЬ ТОЛЬКО ЧЕРЕЗ `js/analytics/api.js`. Свой `fetch` здесь означал бы 401 в
// remote-режиме: заголовки авторизации и CSRF подставляет подменённый
// `window.fetch` из `core/auth.js`, и единственная дверь к нему — `apiFetch`.
//
// ЧАРТЫ — СВОЙ SVG-КИТ (`js/analytics/chart.js`, Фаза 4). Цвета кит берёт из
// CSS-переменных темы в момент отрисовки, поэтому смена темы обрабатывается
// одним `redraw()` по каждому чарту; наблюдатель за `style` на <html> ниже —
// тот же приём, что у панелей мессенджеров (`js/vk/theme.js`).
//
// УСТАРЕВШИЕ ОТВЕТЫ. Быстрое переключение периода порождает гонку: ответ на
// «7 дней» может прийти после ответа на «90 дней». Поэтому у каждого запроса
// есть номер, и рисуется только ответ с текущим номером — счётчик сдвигается
// и при закрытии вью, так что после ухода со страницы не дорисовывается
// ничего.

import {
    $analytics, $analyticsBack, $analyticsRange, $analyticsProject,
    $analyticsRescan, $analyticsMeta, $analyticsProgress, $analyticsError,
    $analyticsKpi, $analyticsGrid, $home,
} from '../core/dom.js';
import { showPlaceholder } from '../terminal/xterm.js';
import { fetchSessions } from '../sessions/sessions.js';
import {
    getAnalyticsStatus, getAnalyticsProjects, getAnalyticsSummary, rescanAnalytics,
} from './api.js';
import {
    barChart, lineChart, hbars, donut, stackedArea, histogram, heatmap, band,
    formatCount, formatCompact, formatMs, formatUsd, formatPercent, formatDay,
} from './chart.js';
import {
    normalizeRange, rangeToQuery, bucketFor, computeDelta, pauseBucketLabels,
    msgLenBucketLabels, ratioSeries, isEmptySummary, progressPercent, viewMode,
    weekdayHourMatrix, hasHeat,
} from './view-core.js';

// ---------------------------------------------------------------------------
// Состояние модуля
// ---------------------------------------------------------------------------

/** Ключи localStorage: выбор фильтров переживает закрытие вью и перезагрузку. */
const LS_RANGE = 'forge.analytics.range';
const LS_PROJECT = 'forge.analytics.project';

/** Интервал поллинга статуса индексации (мс). */
const POLL_MS = 1500;

/** Защита от повторной навески обработчиков на кнопки и селекторы. */
let _bound = false;
/** Вью на экране. */
let _visible = false;
/** Номер последнего запроса — старшие ответы игнорируются. */
let _seq = 0;
/** Таймер поллинга статуса. */
let _pollTimer = null;
/** Наблюдатель за темой приложения. */
let _themeObserver = null;
/** Хендлы чартов текущей отрисовки — для redraw() при смене темы. */
let _charts = [];
/** Метрика в карточке «топ проектов»: messages | cost | lines. */
let _projectMetric = 'messages';
/** Последний удачный ответ summary — нужен переключателю метрики без запроса. */
let _lastSummary = null;
/** Список проектов уже загружен (селектор заполняется один раз за показ). */
let _projectsLoaded = false;

// ---------------------------------------------------------------------------
// Хранение выбора фильтров
// ---------------------------------------------------------------------------

/**
 * Читает сохранённое значение.
 *
 * localStorage может быть недоступен (Safari private mode, отключённые куки) —
 * это не ошибка, просто выбор не переживёт перезагрузку. Тот же компромисс,
 * что у режима темы панелей (`js/vk/theme.js::loadVkThemeMode`).
 */
function lsGet(key) {
    try {
        return window.localStorage.getItem(key);
    } catch (_) {
        return null;
    }
}

function lsSet(key, value) {
    try {
        window.localStorage.setItem(key, value);
    } catch (_) { /* no-op */ }
}

/** Текущий период из селектора (или из localStorage до его инициализации). */
function currentRange() {
    if ($analyticsRange && $analyticsRange.value) return normalizeRange($analyticsRange.value);
    return normalizeRange(lsGet(LS_RANGE));
}

/** Текущий проект: пустая строка — «все проекты». */
function currentProject() {
    if ($analyticsProject) return $analyticsProject.value || '';
    return lsGet(LS_PROJECT) || '';
}

// ---------------------------------------------------------------------------
// Мелкие DOM-помощники (без innerHTML)
// ---------------------------------------------------------------------------

/** `<tag class=... >текст</tag>`. */
function el(tag, className, textContent) {
    const n = document.createElement(tag);
    if (className) n.className = className;
    if (textContent !== undefined && textContent !== null) n.textContent = String(textContent);
    return n;
}

/** Очистка узла без innerHTML. */
function clearNode(n) {
    if (!n) return;
    while (n.firstChild) n.removeChild(n.firstChild);
}

// ---------------------------------------------------------------------------
// Форматирование значений плиток
// ---------------------------------------------------------------------------

/** Число с одним знаком после запятой: «3,4». */
function formatFixed1(v) {
    const n = Number(v);
    if (!Number.isFinite(n)) return '0';
    return (Math.round(n * 10) / 10).toFixed(1).replace('.', ',');
}

/** Момент последнего обновления индекса человеческим текстом. */
function formatUpdatedAt(ms) {
    const n = Number(ms) || 0;
    if (n <= 0) return 'индекс ещё не собирался';
    try {
        return `индекс обновлён ${new Date(n).toLocaleString('ru-RU')}`;
    } catch (_) {
        return 'индекс обновлён';
    }
}

// ---------------------------------------------------------------------------
// KPI-плитки
// ---------------------------------------------------------------------------

/**
 * Состав плиток дашборда.
 *
 * `better` задаёт, какое направление считать хорошим, и от него зависит цвет
 * дельты — НЕ от знака. Для большинства метрик направление бессмысленно
 * (больше сообщений — не хорошо и не плохо), поэтому там null и нейтральный
 * серый.
 *
 * Первые две плитки — `hero`: медианная пауза перед ответом и доля ответов
 * быстрее десяти секунд. Это главный вопрос, ради которого дашборд затевался
 * («читаю ли я код, прежде чем согласиться»), и он не должен теряться среди
 * двадцати одинаковых прямоугольников.
 */
const KPI_TILES = [
    {
        key: 'medianPauseMs', label: 'Медианная пауза перед ответом',
        format: formatMs, better: 'up', hero: true,
        note: 'паузы дольше 30 мин исключены',
    },
    {
        key: 'shareUnder10s', label: 'Ответов быстрее 10 с — «не читая»',
        format: formatPercent, better: 'down', hero: true,
    },
    {
        key: 'ackShare', label: 'Односложных подтверждений', format: formatPercent,
        better: 'down', hero: true, note: '«ок», «продолжай» — согласие без формулировки',
    },
    { key: 'p90PauseMs', label: 'p90 паузы', format: formatMs, better: 'up' },
    { key: 'awayCount', label: 'Пауз «отошёл» (>30 мин)', format: formatCount, better: null },
    {
        key: 'ttr', label: 'Разнообразие речи (TTR)', format: formatPercent, better: 'up',
        note: 'различных слов среди всех — оценка ±5 %',
    },
    { key: 'repeatShare', label: 'Повторяющихся промптов', format: formatPercent, better: 'down' },
    { key: 'slashShare', label: 'Слэш-команд', format: formatPercent, better: null },
    {
        key: 'nightShare', label: 'Ночная активность (00–06)', format: formatPercent, better: null,
        note: (kpi) => `${formatCount(kpi.nightMessages)} сообщений в ночные часы, локальное время`,
    },
    {
        key: 'toolEntropyBits', label: 'Энтропия инструментов', format: (v) => `${formatFixed1(v)} бит`,
        better: null, note: '0 — весь период одним инструментом',
    },
    { key: 'humanMessages', label: 'Сообщений человека', format: formatCount, better: null },
    { key: 'sessions', label: 'Сессий', format: formatCount, better: null },
    { key: 'messagesPerSession', label: 'Сообщений на сессию', format: formatFixed1, better: null },
    { key: 'avgMsgLen', label: 'Средняя длина сообщения', format: (v) => `${formatCount(v)} зн.`, better: null },
    { key: 'medianMsgLen', label: 'Медианная длина сообщения', format: (v) => `${formatCount(v)} зн.`, better: null },
    { key: 'genMs', label: 'Время генерации', format: formatMs, better: null },
    { key: 'toolMs', label: 'Время инструментов', format: formatMs, better: null },
    { key: 'humanMs', label: 'Человеко-время (сумма пауз)', format: formatMs, better: null },
    { key: 'tokensIn', label: 'Токены in', format: formatCompact, better: null },
    { key: 'tokensOut', label: 'Токены out', format: formatCompact, better: null },
    { key: 'tokensCacheRead', label: 'Токены cache-read', format: formatCompact, better: null },
    { key: 'tokensCacheCreate', label: 'Токены cache-create', format: formatCompact, better: null },
    { key: 'costUsd', label: 'Стоимость', format: formatUsd, better: null },
    {
        key: 'linesAdded', label: 'Строк +/−', better: null,
        format: (v, kpi) => `+${formatCount(v)} / −${formatCount(kpi.linesRemoved)}`,
    },
    { key: 'toolCalls', label: 'Вызовов инструментов', format: formatCount, better: null },
    { key: 'interruptions', label: 'Прерываний', format: formatCount, better: null },
    { key: 'compacts', label: 'Компактов контекста', format: formatCount, better: null },
    { key: 'subagents', label: 'Запусков субагентов', format: formatCount, better: null },
];

/** Одна плитка со значением и (если есть с чем сравнивать) дельтой. */
function renderTile(spec, kpi, prev) {
    const tile = el('div', `analytics-tile${spec.hero ? ' analytics-tile-hero' : ''}`);
    tile.appendChild(el('div', 'analytics-tile-label', spec.label));
    const raw = Number(kpi[spec.key]) || 0;
    tile.appendChild(el('div', 'analytics-tile-value', spec.format(raw, kpi)));

    const delta = computeDelta(raw, Number(prev[spec.key]) || 0, spec.better);
    if (delta) {
        const cls = delta.tone === 'good' ? ' is-good' : delta.tone === 'bad' ? ' is-bad' : '';
        tile.appendChild(el('div', `analytics-tile-delta${cls}`, `${delta.text} к прошлому периоду`));
    }
    if (spec.note) {
        const note = typeof spec.note === 'function' ? spec.note(kpi) : spec.note;
        tile.appendChild(el('div', 'analytics-tile-note', note));
    }
    return tile;
}

/** Вся сетка плиток. */
function renderKpi(summary) {
    if (!$analyticsKpi) return;
    clearNode($analyticsKpi);
    const kpi = summary.kpi || {};
    const prev = summary.kpiPrevious || {};
    for (const spec of KPI_TILES) $analyticsKpi.appendChild(renderTile(spec, kpi, prev));
}

// ---------------------------------------------------------------------------
// Карточки чартов
// ---------------------------------------------------------------------------

/**
 * Карточка сетки: заголовок, пояснение и хост(ы) для SVG.
 *
 * @param {string} title
 * @param {{note?: string, wide?: boolean, plots?: number, pair?: boolean}} [o]
 * @returns {{card: Element, plots: Element[], head: Element}}
 */
function card(title, o = {}) {
    const node = el('div', `analytics-card${o.wide ? ' analytics-card-wide' : ''}`);
    const head = el('div', 'analytics-card-head');
    head.appendChild(el('h3', 'analytics-card-title', title));
    if (o.note) head.appendChild(el('span', 'analytics-card-note', o.note));
    node.appendChild(head);

    const count = Math.max(1, Number(o.plots) || 1);
    const plots = [];
    const host = o.pair ? el('div', 'analytics-plot-pair') : node;
    if (o.pair) node.appendChild(host);
    // ЗАГОЛОВОК КАРТОЧКИ НАЗЫВАЕТ ОДИН ЧАРТ, А НЕ ТРИ.
    //
    // Кит намеренно не рисует легенду для единственной серии: её называет
    // заголовок карточки. Но в карточке с несколькими плотами заголовок один
    // на всех, и второй-третий график оставались без имени — «Разнообразие»
    // показывало шкалу 0..4 и столбцы 0..15, о которых нечем было догадаться.
    // Поэтому у многоплотной карточки каждый плот получает свою подпись.
    const labels = Array.isArray(o.labels) ? o.labels : [];
    for (let i = 0; i < count; i += 1) {
        const wrap = count > 1 && labels[i] ? el('div', 'analytics-plot-slot') : null;
        if (wrap) {
            wrap.appendChild(el('div', 'analytics-plot-label', labels[i]));
            host.appendChild(wrap);
        }
        const plot = el('div', 'analytics-plot');
        (wrap || host).appendChild(plot);
        plots.push(plot);
    }
    return { card: node, plots, head };
}

/** Рисует чарт и запоминает хендл — чтобы перекрасить его при смене темы. */
function draw(fn, container, data, opts) {
    const handle = fn(container, data, opts);
    _charts.push(handle);
    return handle;
}

/** Сколько строк показываем в справочных списках из ~/.claude.json. */
const GLOBALS_TOP = 10;
/** Высота строки горизонтальных полос: ниже подписи начинают налезать. */
const ROW_H = 26;

/*
 * ШИРИНУ ЧАРТА ЗАДАЁТ КОНТЕЙНЕР, А НЕ ЭТИ КОНСТАНТЫ.
 *
 * `viewBox` масштабирует и кегль: чарт с фиксированной шириной 720 в узком
 * слоте на 278px сжимался в 0.39 раза, и подписи осей падали до 3.9px. Кит
 * теперь меряет контейнер сам (`measureWidth`), поэтому ширина здесь НЕ
 * задаётся — остаётся только высота, потому что она не выводится из данных.
 *
 * Высота — единственная геометрия, которую стоит назначать снаружи: у полос она
 * считается от числа строк, у широких временных рядов задаётся пропорцией.
 */
/** Высота широкой карточки: 90 дневных столбцов остаются читаемыми. */
const WIDE = { height: 300 };
/** Высота обычной карточки. */
const NARROW = { height: 260 };

/** Все чарты дашборда по номерам плана. */
function renderCharts(summary) {
    if (!$analyticsGrid) return;
    clearNode($analyticsGrid);
    const s = summary.series || {};
    const labels = Array.isArray(s.buckets) ? s.buckets : [];
    const weekly = (summary.range && summary.range.bucket) === 'week';
    const bucketNote = weekly ? 'по неделям' : 'по дням';

    // --- Чарт 3. Пауза перед ответом. Первым после KPI: это главный вопрос.
    {
        const c = card('Пауза перед ответом', {
            wide: true,
            note: `паузы дольше 30 мин («отошёл») исключены — их ${formatCount((summary.kpi || {}).awayCount)}`,
            plots: 2,
            labels: ['Распределение пауз', 'Медиана и p90 по дням'],
            pair: true,
        });
        draw(histogram, c.plots[0], {
            values: s.pauseBuckets || [],
            labels: pauseBucketLabels(s.pauseBucketEdgesMs || []),
        }, Object.assign({ title: 'Распределение пауз', emptyText: 'пауз за период нет' }, NARROW));
        draw(lineChart, c.plots[1], {
            labels,
            series: [
                { label: 'медиана', values: s.pauseP50Ms || [] },
                { label: 'p90', values: s.pauseP90Ms || [] },
            ],
        }, Object.assign({
            title: `Медиана и p90 ${bucketNote}`,
            formatY: formatMs,
            formatValue: formatMs,
            emptyText: 'нет данных за период',
        }, NARROW));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 1. Сообщения и новые сессии.
    {
        const c = card('Сообщения и новые сессии', { wide: true, note: bucketNote });
        draw(barChart, c.plots[0], {
            labels,
            label: 'сообщения',
            values: s.humanMessages || [],
            overlay: { label: 'новые сессии', values: s.sessions || [] },
        }, Object.assign({ emptyText: 'за период сообщений нет' }, WIDE));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 4. Токены и стоимость.
    //
    // Стоимость отдельной линией под стеком, а не второй осью поверх него:
    // стек уже занят четырьмя рядами, и пятая кривая на своей шкале сделала бы
    // картинку нечитаемой.
    {
        const c = card('Токены и стоимость', {
            wide: true, note: bucketNote, plots: 2,
            labels: ['Токены по дням', 'Стоимость, $'],
        });
        draw(stackedArea, c.plots[0], {
            labels,
            series: [
                { label: 'in', values: s.tokensIn || [] },
                { label: 'out', values: s.tokensOut || [] },
                { label: 'cache-read', values: s.cacheRead || [] },
                { label: 'cache-create', values: s.cacheCreate || [] },
            ],
        }, Object.assign({ formatY: formatCompact, formatValue: formatCompact, emptyText: 'токенов за период нет' }, WIDE));
        draw(lineChart, c.plots[1], {
            labels,
            series: [{ label: 'стоимость', values: s.costUsd || [] }],
        }, Object.assign({ formatY: formatUsd, formatValue: formatUsd, emptyText: 'стоимость за период неизвестна' },
            WIDE, { height: 180 }));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 5. Куда уходит время.
    {
        const c = card('Куда уходит время', {
            wide: true,
            note: 'генерация · инструменты · человек читает',
        });
        draw(stackedArea, c.plots[0], {
            labels,
            series: [
                { label: 'генерация', values: s.genMs || [] },
                { label: 'инструменты', values: s.toolMs || [] },
                { label: 'человек читает', values: s.humanMs || [] },
            ],
        }, Object.assign({ formatY: formatMs, formatValue: formatMs, emptyText: 'нет данных за период' }, WIDE));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 6. Топ инструментов.
    {
        const tools = (s.tools || []).map((t) => ({
            label: t.name, value: t.calls, secondary: t.interrupted,
        }));
        const c = card('Топ инструментов', { note: 'внутри полосы — прерванные вызовы' });
        draw(hbars, c.plots[0], { items: tools }, Object.assign({
            label: 'вызовов',
            secondaryLabel: 'из них прервано',
            emptyText: 'инструменты за период не вызывались',
        }, NARROW, { height: 40 + ROW_H * Math.min(tools.length || 1, 12) }));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 7. Модели: доля по сообщениям и по стоимости.
    {
        const models = s.models || [];
        // Широкая: имена моделей длинные (`claude-haiku-4-5-20251001`), и в
        // трети сетки легенда двух колец усекается почти целиком. Здесь же
        // «слева/справа» из подписи — правда, потому что пара в ряд остаётся
        // привилегией широкой карточки.
        const c = card('Модели', {
            wide: true, note: 'слева — по сообщениям, справа — по стоимости', plots: 2, pair: true,
        });
        draw(donut, c.plots[0], {
            parts: models.map((m) => ({ label: m.name, value: m.messages })),
        }, { title: 'По сообщениям', formatValue: formatCount, emptyText: 'моделей за период нет', height: 260 });
        draw(donut, c.plots[1], {
            parts: models.map((m) => ({ label: m.name, value: m.costUsd })),
        }, { title: 'По стоимости', formatValue: formatUsd, emptyText: 'стоимость неизвестна', height: 260 });
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 8. Топ проектов с переключателем метрики.
    {
        const c = card('Топ проектов', { wide: true });
        const projects = s.projects || [];
        const switcher = el('div', 'analytics-card-switch');
        const plot = c.plots[0];
        const metrics = [
            { id: 'messages', label: 'сообщения', pick: (p) => p.messages, fmt: formatCount },
            { id: 'cost', label: 'стоимость', pick: (p) => p.costUsd, fmt: formatUsd },
            { id: 'lines', label: 'строки', pick: (p) => p.linesAdded + p.linesRemoved, fmt: formatCount },
        ];
        const paint = () => {
            const m = metrics.find((x) => x.id === _projectMetric) || metrics[0];
            for (const b of switcher.children) {
                b.classList.toggle('is-active', b.dataset.metric === m.id);
            }
            // Перерисовка карточки заменяет только её чарт, но хендл в _charts
            // должен остаться один — иначе смена темы перекрашивала бы призраки
            // прошлых отрисовок.
            _charts = _charts.filter((h) => !h.__projects);
            const items = projects
                .map((p) => ({ label: p.displayName || p.id, value: Number(m.pick(p)) || 0 }))
                .sort((a, b) => b.value - a.value);
            const handle = draw(hbars, plot, { items }, Object.assign({
                label: m.label,
                formatValue: m.fmt,
                emptyText: 'проектов с активностью за период нет',
            }, WIDE, { height: 40 + ROW_H * Math.min(items.length || 1, 15) }));
            handle.__projects = true;
        };
        for (const m of metrics) {
            const btn = el('button', 'analytics-switch-btn', m.label);
            btn.type = 'button';
            btn.dataset.metric = m.id;
            btn.addEventListener('click', () => {
                _projectMetric = m.id;
                paint();
            });
            switcher.appendChild(btn);
        }
        c.head.appendChild(switcher);
        paint();
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 9. Длина сообщений.
    {
        const c = card('Длина сообщений', {
            note: 'в символах', plots: 2, pair: true,
            labels: ['Распределение', 'Средняя по дням'],
        });
        draw(histogram, c.plots[0], {
            values: s.msgLenBuckets || [],
            labels: msgLenBucketLabels(s.msgLenEdges || []),
        }, Object.assign({ title: 'Распределение', emptyText: 'сообщений за период нет' }, NARROW));
        draw(lineChart, c.plots[1], {
            labels,
            series: [{ label: 'средняя длина', values: s.msgLenAvg || [] }],
        }, Object.assign({
            title: `Средняя ${bucketNote}`,
            formatY: formatCompact,
            formatValue: (v) => `${formatCount(v)} зн.`,
            emptyText: 'нет данных за период',
        }, NARROW));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 10. Длительность хода ассистента.
    {
        const c = card('Длительность хода ассистента', { note: bucketNote });
        draw(lineChart, c.plots[0], {
            labels,
            series: [
                { label: 'p50', values: s.turnP50Ms || [] },
                { label: 'p90', values: s.turnP90Ms || [] },
            ],
        }, Object.assign({ formatY: formatMs, formatValue: formatMs, emptyText: 'нет данных за период' }, NARROW));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 12. Доля thinking в выводе.
    //
    // Доля, а не абсолютные токены: абсолютное число повторяет чарт 4 и ничего
    // нового не сообщает, а доля показывает, СКОЛЬКО ассистент думает.
    {
        const c = card('Доля thinking в выводе', { note: bucketNote });
        draw(lineChart, c.plots[0], {
            labels,
            series: [{ label: 'thinking / out', values: ratioSeries(s.thinking, s.tokensOut) }],
        }, Object.assign({
            formatY: formatPercent,
            formatValue: formatPercent,
            emptyText: 'нет данных за период',
        }, NARROW));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 2. Тепловая карта «час × день недели».
    //
    // Матрица приходит за ВЕСЬ период (24×7) и разворачивается в 7×24 —
    // календарной ориентации, где строка это день недели. Часы локальные:
    // бэкенд раскладывает UTC-таймстемпы транскриптов по локальной зоне
    // машины, и второй раз их никто не сдвигает.
    {
        const matrix = weekdayHourMatrix(s.hourXWeekday);
        const nightHours = Number(s.nightHours) || 6;
        const kpi = summary.kpi || {};
        const c = card('Когда вы работаете', {
            wide: true,
            note: `час × день недели, локальное время · ночью (00–0${nightHours}) `
                + `${formatCount(kpi.nightMessages)} сообщений — ${formatPercent(kpi.nightShare)} за период`,
        });
        draw(heatmap, c.plots[0], {
            matrix: hasHeat(matrix) ? matrix : [],
        }, Object.assign({
            nightUntil: nightHours,
            nightLabel: `ночь 00–0${nightHours}`,
            emptyText: 'за период сообщений нет',
        }, WIDE, { height: 260 }));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 11. Разнообразие: признаки автопилота.
    //
    // Три плота вместо одного: доли (0..1), энтропия (биты) и число проектов
    // живут в разных единицах, и одна общая ось Y сделала бы из них кашу.
    {
        const c = card('Разнообразие — признаки автопилота', {
            wide: true,
            note: bucketNote,
            plots: 3,
            labels: [
                'Доли: разнообразие речи, повторы, команды, подтверждения',
                'Энтропия инструментов, бит — 0 значит весь день одним',
                'Проектов за день',
            ],
        });
        draw(lineChart, c.plots[0], {
            labels,
            series: [
                { label: 'TTR (различных слов)', values: s.ttr || [] },
                { label: 'повторы промптов', values: s.repeatShare || [] },
                { label: 'слэш-команды', values: s.slashShare || [] },
                { label: 'односложные «ок»', values: s.ackShare || [] },
            ],
        }, Object.assign({
            formatY: formatPercent,
            formatValue: formatPercent,
            emptyText: 'нет данных за период',
        }, WIDE, { height: 240 }));
        draw(lineChart, c.plots[1], {
            labels,
            series: [{ label: 'энтропия инструментов, бит', values: s.toolEntropyBits || [] }],
        }, Object.assign({
            formatY: formatFixed1,
            formatValue: (v) => `${formatFixed1(v)} бит`,
            emptyText: 'инструменты за период не вызывались',
        }, WIDE, { height: 180 }));
        draw(barChart, c.plots[2], {
            labels,
            label: 'проектов за день',
            values: s.uniqueProjects || [],
        }, Object.assign({
            formatY: formatCount,
            formatValue: formatCount,
            emptyText: 'нет данных за период',
        }, WIDE, { height: 180 }));

        const hint = el('div', 'analytics-card-hint');
        hint.appendChild(el('p', null,
            'TTR — доля различных слов среди всех, что вы написали за день. Её падение '
            + 'вместе с ростом доли односложных подтверждений означает автопилот: вы всё '
            + 'реже формулируете задачу и всё чаще просто соглашаетесь.'));
        hint.appendChild(el('p', null,
            'Энтропия инструментов показывает, сузился ли репертуар работы: ноль — весь '
            + 'день одним инструментом. TTR — оценка (±5 % на больших словарях), она '
            + 'читается как тренд, а не как точное число.'));
        c.card.appendChild(hint);
        $analyticsGrid.appendChild(c.card);
    }

    // --- Чарт 13. Версии CLI по дням: видно, когда обновлялись.
    {
        const c = card('Версии Claude Code', {
            wide: true,
            note: 'день без данных о версии — нейтральный',
        });
        draw(band, c.plots[0], {
            labels,
            values: s.versions || [],
        }, Object.assign({ emptyText: 'нет данных за период' }, WIDE, { height: 90 }));
        $analyticsGrid.appendChild(c.card);
    }

    // --- Справочные плитки из ~/.claude.json.
    renderGlobals(summary);

    // ЧАРТ ЗНАЕТ СВОЮ ШИРИНУ ТОЛЬКО В ДОКУМЕНТЕ.
    //
    // Кит меряет контейнер, чтобы единица `viewBox` равнялась CSS-пикселю и
    // подписи не масштабировались вместе с графикой. Но карточка попадает в
    // сетку ПОСЛЕ отрисовки своих чартов, и в этот момент мерить нечего: без
    // повторного прохода каждый чарт остаётся с запасной шириной 720 и потом
    // растягивается сеткой — в широкой карточке в полтора раза, вместе с
    // кеглем. Второй проход стоит одну перерисовку и снимает вопрос целиком.
    redrawAll();
}

/** Таймер отложенной перерисовки после изменения размера окна. */
let _resizeTimer = null;

/**
 * Ширина чарта зашита в `viewBox` в момент отрисовки, поэтому изменение окна
 * растягивает готовую картинку вместе с кеглем. Перерисовка возвращает
 * масштаб 1:1. Задержка — чтобы не перерисовывать на каждом кадре протяжки.
 */
function onResize() {
    if (_resizeTimer) clearTimeout(_resizeTimer);
    _resizeTimer = setTimeout(() => {
        _resizeTimer = null;
        if (_visible) redrawAll();
    }, 180);
}

/** Перерисовать все чарты текущей отрисовки: тема, ширина, что угодно. */
function redrawAll() {
    for (const h of _charts) {
        try {
            h.redraw();
        } catch (_) { /* один сломанный чарт не должен ронять остальные */ }
    }
}

/**
 * Глобальные счётчики Claude Code из `~/.claude.json`.
 *
 * Это НЕ метрика периода: файл хранит счётчики за всё время и не знает ни про
 * даты, ни про проекты. Поэтому карточка стоит последней и подписана явно —
 * иначе её числа неизбежно сравнивали бы с числами дашборда.
 *
 * Файла нет или он битый — карточки просто не будет (бэкенд не отдаёт поле).
 */
function renderGlobals(summary) {
    const g = summary.globals;
    if (!g || !$analyticsGrid) return;
    const groups = [
        { items: g.skills, label: 'Навыки' },
        { items: g.plugins, label: 'Плагины' },
        { items: g.tools, label: 'Инструменты' },
    ].filter((x) => Array.isArray(x.items) && x.items.length > 0);
    if (groups.length === 0) return;

    const c = card('Глобальные счётчики Claude Code', {
        wide: true,
        note: 'из ~/.claude.json, за всё время — период и проект на них не влияют',
        plots: groups.length,
        pair: groups.length === 2,
    });
    // Не больше GLOBALS_TOP строк на список: справочная плитка не должна
    // перевешивать сам дашборд, а полосы, сжатые до высоты подписи, налезают
    // друг на друга и не читаются.
    groups.forEach((group, i) => {
        const items = group.items
            .map((it) => ({ label: it.name, value: Number(it.usageCount) || 0 }))
            .slice(0, GLOBALS_TOP);
        draw(hbars, c.plots[i], { items }, Object.assign({
            title: group.label,
            label: 'использований',
            formatValue: formatCount,
            emptyText: 'нет данных',
        }, NARROW, { height: 40 + ROW_H * Math.max(items.length, 1) }));
    });
    $analyticsGrid.appendChild(c.card);
}

/** Пустой период: объяснение вместо сетки карточек «нет данных» в каждой. */
function renderEmpty() {
    if (!$analyticsGrid) return;
    clearNode($analyticsGrid);
    if ($analyticsKpi) clearNode($analyticsKpi);
    const box = el('div', 'analytics-empty');
    box.appendChild(el('div', 'analytics-empty-icon', '📊'));
    box.appendChild(el('div', 'analytics-empty-title', 'За выбранный период данных нет'));
    box.appendChild(el('div', 'analytics-empty-hint',
        'Расширьте период или выберите другой проект — индекс собирается по '
        + 'транскриптам ~/.claude/projects.'));
    $analyticsGrid.appendChild(box);
}

// ---------------------------------------------------------------------------
// Прогресс индексации и ошибки
// ---------------------------------------------------------------------------

/** Прогресс-бар первого прохода индексации. */
function renderProgress(progress) {
    if (!$analyticsProgress) return;
    clearNode($analyticsProgress);
    $analyticsProgress.hidden = false;
    const p = progress || {};
    const pct = progressPercent(p);
    const total = Number(p.total) || 0;
    const done = Number(p.done) || 0;

    $analyticsProgress.appendChild(el('div', 'analytics-progress-label',
        total > 0 ? `Индексация транскриптов: ${done} из ${total} (${pct}%)`
            : 'Индексация транскриптов: считаем файлы…'));
    const track = el('div', 'analytics-progress-track');
    const fill = el('div', 'analytics-progress-fill');
    fill.style.width = `${pct}%`;
    track.appendChild(fill);
    $analyticsProgress.appendChild(track);
    $analyticsProgress.appendChild(el('div', 'analytics-progress-hint',
        'Первый проход читает ~/.claude/projects целиком; дальше индекс поднимается из кэша мгновенно.'));
}

/** Тонкая пометка «идёт фоновый добор» поверх уже показанных данных. */
function renderRefreshing(progress) {
    if (!$analyticsProgress) return;
    clearNode($analyticsProgress);
    $analyticsProgress.hidden = false;
    const p = progress || {};
    const total = Number(p.total) || 0;
    $analyticsProgress.appendChild(el('div', 'analytics-progress-hint',
        total > 0
            ? `Данные ниже актуальны; в фоне дочитываются файлы (${Number(p.done) || 0} из ${total}).`
            : 'Данные ниже актуальны; в фоне идёт обновление индекса.'));
}

function hideProgress() {
    if (!$analyticsProgress) return;
    clearNode($analyticsProgress);
    $analyticsProgress.hidden = true;
}

/** Сетевая ошибка: сообщение и кнопка «Повторить» — вью не роняем. */
function renderError(message) {
    if (!$analyticsError) return;
    clearNode($analyticsError);
    $analyticsError.hidden = false;
    $analyticsError.appendChild(el('span', null, message));
    const retry = el('button', 'analytics-error-retry', 'Повторить');
    retry.type = 'button';
    retry.addEventListener('click', () => {
        hideError();
        load();
    });
    $analyticsError.appendChild(retry);
}

function hideError() {
    if (!$analyticsError) return;
    clearNode($analyticsError);
    $analyticsError.hidden = true;
}

// ---------------------------------------------------------------------------
// Загрузка данных
// ---------------------------------------------------------------------------

/** Подзаголовок: период, проект и момент сборки индекса. */
function renderMeta(summary) {
    if (!$analyticsMeta) return;
    clearNode($analyticsMeta);
    const r = summary.range || {};
    const parts = [];
    if (r.from && r.to) parts.push(`${formatDay(r.from)} — ${formatDay(r.to)}`);
    parts.push(r.project ? projectTitle(r.project) : 'все проекты');
    parts.push(formatUpdatedAt(summary.updatedAt));
    $analyticsMeta.textContent = parts.join(' · ');
}

/** Человеческое имя проекта по id — из уже заполненного селектора. */
function projectTitle(id) {
    if ($analyticsProject) {
        for (const opt of $analyticsProject.options) {
            if (opt.value === id) return opt.textContent;
        }
    }
    return id;
}

/** Отрисовка ответа целиком. */
function render(summary) {
    _lastSummary = summary;
    _charts = [];
    const mode = viewMode(summary);

    if (mode.mode === 'progress') {
        renderProgress(summary.progress);
        if ($analyticsKpi) clearNode($analyticsKpi);
        if ($analyticsGrid) clearNode($analyticsGrid);
        if ($analyticsMeta) $analyticsMeta.textContent = '';
    } else {
        if (mode.refreshing) renderRefreshing(summary.progress);
        else hideProgress();
        renderMeta(summary);
        if (isEmptySummary(summary)) {
            renderEmpty();
        } else {
            renderKpi(summary);
            renderCharts(summary);
        }
    }

    if (mode.polling) startPolling();
    else stopPolling();
}

/**
 * Запрашивает summary по текущим фильтрам и рисует ответ.
 *
 * Устаревшие ответы отбрасываются по номеру запроса — см. шапку файла.
 */
async function load() {
    if (!_visible) return;
    const seq = ++_seq;
    const range = currentRange();
    const project = currentProject();
    const query = Object.assign({ bucket: bucketFor(range) }, rangeToQuery(range));
    if (project) query.project = project;
    try {
        const data = await getAnalyticsSummary(query);
        if (seq !== _seq || !_visible) return;
        hideError();
        render(data);
    } catch (err) {
        if (seq !== _seq || !_visible) return;
        stopPolling();
        renderError(`Не удалось загрузить аналитику: ${err && err.message ? err.message : err}`);
    }
}

/** Заполняет селектор проектов; вызывается один раз за показ вью. */
async function loadProjects() {
    if (!$analyticsProject || _projectsLoaded) return;
    let list = [];
    try {
        list = await getAnalyticsProjects();
    } catch (_) {
        // Список проектов — удобство, а не условие работы: при ошибке остаётся
        // «Все проекты», и дашборд продолжает жить.
        return;
    }
    if (!Array.isArray(list) || !_visible) return;
    _projectsLoaded = true;
    const saved = lsGet(LS_PROJECT) || '';
    clearNode($analyticsProject);
    const all = document.createElement('option');
    all.value = '';
    all.textContent = 'Все проекты';
    $analyticsProject.appendChild(all);
    for (const p of list) {
        const opt = document.createElement('option');
        opt.value = p.id;
        opt.textContent = `${p.displayName || p.id} · ${p.messages || 0}`;
        $analyticsProject.appendChild(opt);
    }
    // Сохранённый проект мог исчезнуть с диска — тогда молча остаёмся на «Все».
    if (saved && Array.prototype.some.call($analyticsProject.options, (o) => o.value === saved)) {
        $analyticsProject.value = saved;
        load();
    }
}

// ---------------------------------------------------------------------------
// Поллинг статуса индексации
// ---------------------------------------------------------------------------

/**
 * Поллит `GET /api/analytics/status` и перезагружает summary, когда индекс
 * готов (или когда закончился фоновый добор).
 *
 * Таймер живёт только пока вью на экране: `hideAnalytics` его гасит, поэтому
 * после закрытия сетевых запросов не остаётся.
 */
function startPolling() {
    if (_pollTimer || !_visible) return;
    _pollTimer = setInterval(async () => {
        if (!_visible) { stopPolling(); return; }
        let st;
        try {
            st = await getAnalyticsStatus();
        } catch (_) {
            // Разовая сетевая ошибка во время скана — не повод убивать вью;
            // следующий тик попробует снова.
            return;
        }
        if (!_visible) { stopPolling(); return; }
        const running = !!(st && st.state === 'scanning');
        if (running) {
            // Пока данных нет вовсе — двигаем полосу; иначе обновляем пометку.
            const shown = viewMode(_lastSummary || {});
            if (shown.mode === 'progress') renderProgress(st);
            else renderRefreshing(st);
            return;
        }
        stopPolling();
        load();
    }, POLL_MS);
}

function stopPolling() {
    if (_pollTimer) {
        clearInterval(_pollTimer);
        _pollTimer = null;
    }
}

// ---------------------------------------------------------------------------
// Тема
// ---------------------------------------------------------------------------

/**
 * Перекрашивает чарты при смене темы приложения.
 *
 * Глобального события «тема сменилась» в devforge нет: `themes/api.js::
 * applyTheme` просто расставляет CSS-переменные на <html> через
 * `style.setProperty`. Наблюдатель за атрибутом `style` — та же развязка, что
 * у панелей мессенджеров, и она не тянет модуль тем в аналитику.
 */
function startThemeWatch() {
    if (_themeObserver || typeof MutationObserver !== 'function') return;
    _themeObserver = new MutationObserver(redrawAll);
    try {
        _themeObserver.observe(document.documentElement, {
            attributes: true, attributeFilter: ['style', 'class'],
        });
    } catch (_) {
        _themeObserver = null;
    }
}

function stopThemeWatch() {
    if (!_themeObserver) return;
    try {
        _themeObserver.disconnect();
    } catch (_) { /* no-op */ }
    _themeObserver = null;
}

// ---------------------------------------------------------------------------
// Обработчики
// ---------------------------------------------------------------------------

function _onKeydown(ev) {
    if (ev.key === 'Escape') {
        ev.stopPropagation();
        closeAnalytics();
    }
}

/** Навеска обработчиков — ровно один раз за жизнь страницы. */
function bindControls() {
    if (_bound) return;
    _bound = true;

    if ($analyticsBack) $analyticsBack.addEventListener('click', () => closeAnalytics());

    if ($analyticsRange) {
        $analyticsRange.value = normalizeRange(lsGet(LS_RANGE));
        $analyticsRange.addEventListener('change', () => {
            lsSet(LS_RANGE, currentRange());
            load();
        });
    }
    if ($analyticsProject) {
        $analyticsProject.addEventListener('change', () => {
            lsSet(LS_PROJECT, currentProject());
            load();
        });
    }
    if ($analyticsRescan) {
        $analyticsRescan.addEventListener('click', async () => {
            $analyticsRescan.disabled = true;
            try {
                const st = await rescanAnalytics();
                hideError();
                renderProgress(st);
                if ($analyticsKpi) clearNode($analyticsKpi);
                if ($analyticsGrid) clearNode($analyticsGrid);
                _lastSummary = { state: 'scanning', progress: st };
                startPolling();
            } catch (err) {
                renderError(`Не удалось запустить пересбор: ${err && err.message ? err.message : err}`);
            } finally {
                $analyticsRescan.disabled = false;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Публичный API вью
// ---------------------------------------------------------------------------

/**
 * Показывает дашборд аналитики.
 *
 * @param {'7'|'30'|'90'|'all'} [range] — период; по умолчанию сохранённый
 */
export function showAnalytics(range) {
    if (!$analytics) return;
    bindControls();
    if (range !== undefined && range !== null && $analyticsRange) {
        $analyticsRange.value = normalizeRange(range);
        lsSet(LS_RANGE, currentRange());
    }

    // Вью занимает #main целиком — соседей прячем, чтобы не накладывались.
    try {
        if (window.ForgeApp && typeof window.ForgeApp.hideDailySummary === 'function') {
            window.ForgeApp.hideDailySummary();
        }
    } catch (_) { /* no-op */ }

    $analytics.style.display = 'flex';
    if ($home) $home.style.display = 'none';
    showPlaceholder(false);
    _visible = true;
    document.addEventListener('keydown', _onKeydown, true);
    window.addEventListener('resize', onResize);
    startThemeWatch();

    hideError();
    loadProjects();
    load();
}

/** Скрывает дашборд: гасит таймеры, наблюдателей и обработчики. */
export function hideAnalytics() {
    if (!$analytics) return;
    $analytics.style.display = 'none';
    _visible = false;
    // Сдвиг счётчика — ответ уже отправленного запроса не дорисуется.
    _seq += 1;
    stopPolling();
    stopThemeWatch();
    document.removeEventListener('keydown', _onKeydown, true);
    window.removeEventListener('resize', onResize);
    if (_resizeTimer) {
        clearTimeout(_resizeTimer);
        _resizeTimer = null;
    }
    _charts = [];
}

/**
 * Закрывает вью и возвращает к основному экрану.
 *
 * По образцу `daily-summary.js::closeDailySummary`: fetchSessions() сам решит,
 * показать #home или активную сессию.
 */
export function closeAnalytics() {
    hideAnalytics();
    fetchSessions();
}
