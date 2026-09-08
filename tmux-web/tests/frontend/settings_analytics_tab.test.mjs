// ⚠ СТРАЖ ВКЛАДКИ НАСТРОЕК «АНАЛИТИКА» (Фаза 6 дашборда аналитики, задача .3).
//
// Грузит РЕАЛЬНЫЙ модуль static/js/settings/analytics-tab.js на фейковом
// документе. Сеть приходит инъекцией, поэтому ни браузера, ни сервера не нужно
// — и заодно доказано, что вкладка не ходит никуда, кроме переданных функций.
//
// ЗАЧЕМ ЭТОТ ТЕСТ. Вкладка держит четыре обещания, и каждое ломается ТИХО:
//
//   1. РЕГИСТРАЦИЯ В МОДАЛКЕ. `settings/modal.js` хардкодит каждую вкладку в
//      СЕМИ местах (импорт, кнопка, панель, селектор контента, объект
//      состояния, ветка в showTab, whitelist initialTab) — реестра там нет
//      намеренно, и файл сам об этом предупреждает в шапке. Забудь одну точку
//      — вкладка не откроется, а проверки формы этого не заметят: они работают
//      с модулем напрямую.
//   2. ПРОГРЕСС ВМЕСТО НУЛЕЙ. Индексация ленивая: первый запрос к API её
//      запускает, и до конца прохода сервер отвечает 200 с `state:"scanning"`.
//      Нарисуй вкладка в этот момент плитки — они будут нулевыми, и первое
//      открытие выглядит как «аналитика сломана», а не «идёт первый скан».
//   3. `ready` ПРИ ФОНОВОМ ДОБОРЕ — ЭТО ДАННЫЕ. Индекс поднят из кэша, в фоне
//      дочитываются хвосты. Прогресс-бар поверх готовых цифр прятал бы рабочую
//      сводку ради технической подробности.
//   4. СЕТИ СВОЕЙ НЕТ. Ни `fetch`, ни `innerHTML`: первое означало бы 401 в
//      remote-режиме мимо `apiFetch`, второе — сборку разметки из значений,
//      пришедших с сервера.
//
// Мутационная проверка: убери любую из семи точек из modal.js — покраснеет
// раздел 6; убери ветку `mode === 'progress'` — покраснеет раздел 3.
//
// Запуск: node tmux-web/tests/frontend/settings_analytics_tab.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';
import { makeDoc, makeEl } from './meet_popup_dom.mjs';

let failures = 0;
let checks = 0;

function ok(cond, label, detail) {
    checks += 1;
    if (cond) return;
    failures += 1;
    console.error(`[FAIL] ${label}${detail ? `\n       ${detail}` : ''}`);
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) return;
    failures += 1;
    console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
}

// `makeDoc` возвращает конверт `{ doc, hosts, … }`, а не сам документ.
const DOM = makeDoc([]);
globalThis.document = DOM.doc;
globalThis.window = globalThis;

const TAB = await import(new URL('../../static/js/settings/analytics-tab.js', import.meta.url).href);

// ---------------------------------------------------------------------------
// Ответы сервера — той же формы, что отдают роуты Фазы 3.
// ---------------------------------------------------------------------------

/** «Сегодня» фиксировано: границы периода обязаны быть воспроизводимы. */
const NOW = new Date('2026-09-30T12:00:00');

function summary(over = {}) {
    return Object.assign({
        state: 'ready',
        updatedAt: 1_759_000_000_000,
        progress: { running: false, done: 12, total: 12 },
        kpi: {
            medianPauseMs: 42_000,
            shareUnder10s: 0.25,
            humanMessages: 120,
            sessions: 8,
            costUsd: 3.5,
            tokensIn: 1000,
            tokensOut: 2000,
        },
        kpiPrevious: {
            medianPauseMs: 84_000,
            shareUnder10s: 0.5,
            humanMessages: 60,
            sessions: 4,
            costUsd: 1.75,
            tokensIn: 500,
            tokensOut: 1000,
        },
        series: {
            buckets: ['2026-09-01', '2026-09-02', '2026-09-03'],
            humanMessages: [3, 5, 4],
        },
    }, over);
}

/** Инъекция: считает вызовы и хранит последние аргументы. */
function makeDeps(over = {}) {
    const calls = { summary: [], status: 0, rescan: 0, close: 0, show: 0, spark: [] };
    const timers = [];
    const deps = Object.assign({
        now: NOW,
        getAnalyticsSummary: async (q) => { calls.summary.push(q); return summary(); },
        getAnalyticsStatus: async () => { calls.status += 1; return { state: 'ready' }; },
        rescanAnalytics: async () => { calls.rescan += 1; return { state: 'scanning', done: 0, total: 9 }; },
        onClose: () => { calls.close += 1; },
        showAnalytics: () => { calls.show += 1; },
        sparkline: (container, data, opts) => { calls.spark.push({ container, data, opts }); },
        setTimeout: (fn) => { timers.push(fn); return timers.length; },
        clearTimeout: (id) => { timers[id - 1] = null; },
    }, over);
    return {
        deps,
        calls,
        timers,
        /** Выполнить один отложенный тик поллинга. */
        async tick() {
            const fn = timers.find(Boolean);
            if (!fn) return false;
            timers[timers.indexOf(fn)] = null;
            await fn();
            return true;
        },
    };
}

function nodes(root, out = []) {
    out.push(root);
    for (const c of root.childNodes || []) if (c.nodeType === 1) nodes(c, out);
    return out;
}

function textOf(root) {
    return root.textContent || '';
}

function tiles(refs) {
    return nodes(refs.kpi).filter((n) => n.classList.contains('analytics-tile'));
}

function tileText(tile, cls) {
    const n = nodes(tile).find((x) => x.classList.contains(cls));
    return n ? n.textContent : null;
}

// ===========================================================================
// 1. ГОТОВЫЙ ИНДЕКС: ШЕСТЬ ПЛИТОК, СПАРКЛАЙН, ПОДПИСЬ
// ===========================================================================

{
    const { deps, calls } = makeDeps();
    const host = makeEl('div');
    const refs = TAB.renderAnalyticsTab(host, deps);
    await refs.ready;

    eq(tiles(refs).length, TAB.TAB_TILES.length, 'плиток ровно столько, сколько объявлено в TAB_TILES');
    eq(refs.progress.hidden, true, 'при готовом индексе прогресс-бара нет');
    eq(refs.kpi.hidden, false, 'плитки показаны');

    // Период — ровно 30 дней, считая сегодняшний: иначе на экране 31 колонка
    // под подписью «30 дней».
    eq(calls.summary.length, 1, 'ровно один запрос summary при открытии');
    eq(calls.summary[0], { bucket: 'day', from: '2026-09-01', to: '2026-09-30' },
        'запрошены последние 30 дней дневными корзинами');

    const first = tiles(refs)[0];
    ok(/Медианная пауза/.test(tileText(first, 'analytics-tile-label')),
        'первая плитка — медианная пауза перед ответом (главный вопрос дашборда)');
    ok(first.classList.contains('analytics-tile-hero'),
        'и она hero — не теряется среди прочих');

    // ⚠ ЦВЕТ ДЕЛЬТЫ — ПО СМЫСЛУ МЕТРИКИ, А НЕ ПО ЗНАКУ. Пауза упала вдвое:
    // знак минус, но это ровно сигнал «соглашаюсь не читая», поэтому плохо.
    const pauseDelta = nodes(first).find((n) => n.classList.contains('analytics-tile-delta'));
    ok(pauseDelta && pauseDelta.classList.contains('is-bad'),
        'падение медианной паузы окрашено как плохое, а не как «минус — зелёный»',
        pauseDelta ? pauseDelta.className : 'дельты нет');

    const share = tiles(refs)[1];
    const shareDelta = nodes(share).find((n) => n.classList.contains('analytics-tile-delta'));
    ok(shareDelta && shareDelta.classList.contains('is-good'),
        'падение доли ответов «не читая» — хорошее');

    // Спарклайн получает именно ряд сообщений по дням и рисуется компактно.
    eq(calls.spark.length, 1, 'спарклайн нарисован один раз');
    eq(calls.spark[0].data.series[0].values, [3, 5, 4], 'в спарклайн ушли сообщения по дням');
    eq(calls.spark[0].opts.legend, false, 'легенды в полосе 90px нет');
    eq(refs.spark.hidden, false, 'контейнер спарклайна показан');

    ok(/индекс обновлён/.test(textOf(refs.meta)), 'сказано, когда индекс обновлялся', textOf(refs.meta));
}

// ===========================================================================
// 2. ПУСТОЙ ПЕРИОД ОБЪЯСНЁН СЛОВАМИ, А НЕ ШЕСТЬЮ НУЛЯМИ
// ===========================================================================

{
    const { deps, calls } = makeDeps({
        getAnalyticsSummary: async () => summary({
            kpi: {}, kpiPrevious: {}, series: { buckets: [], humanMessages: [] },
        }),
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;

    eq(tiles(refs).length, 0, 'плиток с нулями нет');
    ok(/активности Claude Code не найдено/.test(textOf(refs.kpi)),
        'вместо них — объяснение', textOf(refs.kpi));
    eq(refs.spark.hidden, true, 'и спарклайна из пустого ряда тоже нет');
    eq(calls.spark.length, 0, 'кит графиков на пустом ряде не зовётся');
}

// ===========================================================================
// 3. ⚠ ИДЁТ ПЕРВЫЙ СКАН — ПРОГРЕСС, А НЕ НУЛИ
// ===========================================================================

{
    const { deps, calls, tick, timers } = makeDeps({
        getAnalyticsSummary: async () => ({
            state: 'scanning',
            progress: { running: true, done: 3, total: 12 },
            kpi: {}, series: {},
        }),
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;

    eq(refs.progress.hidden, false, 'во время первого скана показан прогресс');
    eq(refs.kpi.hidden, true, 'а нулевых плиток нет');
    eq(refs.progressFill.style.width, '25%', 'полоса на 3 из 12');
    ok(/3 из 12/.test(textOf(refs.progressHint)), 'и числа названы', textOf(refs.progressHint));
    ok(timers.filter(Boolean).length > 0, 'запущен опрос status — иначе полоса замерла бы навсегда');

    // Скан закончился: следующий тик перечитывает summary.
    deps.getAnalyticsSummary = async () => summary();
    await tick();
    ok(calls.status >= 1, 'status опрошен');
    eq(refs.progress.hidden, true, 'по готовности прогресс убран');
    eq(tiles(refs).length, TAB.TAB_TILES.length, 'и плитки нарисованы');
}

// ===========================================================================
// 4. ⚠ `ready` + ФОНОВЫЙ ДОБОР — ПОКАЗЫВАЕМ ДАННЫЕ, А НЕ ПРОГРЕСС
// ===========================================================================

{
    const { deps, timers } = makeDeps({
        getAnalyticsSummary: async () => summary({
            state: 'ready', progress: { running: true, done: 5, total: 9 },
        }),
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;

    eq(refs.progress.hidden, true, 'готовые данные не прячутся за прогресс-бар');
    eq(tiles(refs).length, TAB.TAB_TILES.length, 'плитки на месте');
    ok(/обновятся/.test(textOf(refs.meta)), 'но сказано, что цифры сейчас обновятся', textOf(refs.meta));
    ok(timers.filter(Boolean).length > 0, 'и поллинг идёт — по концу добора будет перерисовка');
}

// ===========================================================================
// 5. КНОПКИ
// ===========================================================================

{
    // «Открыть в новой вкладке»: дашборд уезжает в отдельную вкладку браузера,
    // а текущая страница (с живым терминалом) остаётся нетронутой — модалка НЕ
    // закрывается и вью в этой странице НЕ показывается.
    const order = [];
    const opened = [];
    const { deps, calls } = makeDeps({
        onClose: () => order.push('close'),
        showAnalytics: () => order.push('show'),
        openWindow: (url, target) => {
            opened.push([url, target]);
            return { closed: false };
        },
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;

    refs.openBtn.click();
    // Без `noopener`: с ним window.open ВСЕГДА возвращает null, и успешное
    // открытие неотличимо от заблокированного — фолбэк подменял бы текущий экран.
    eq(opened, [['/?view=analytics', '_blank']], 'дашборд открыт новой вкладкой');
    eq(TAB.ANALYTICS_URL, '/?view=analytics', 'адрес дашборда — диплинк ?view=analytics');
    eq(order, [], 'текущая страница не тронута: ни закрытия модалки, ни подмены экрана');

    // Блокировщик всплывающих окон возвращает null. Молча ничего не делать
    // нельзя — кнопка обязана сработать хоть как-то, поэтому прежний путь.
    {
        const fallback = [];
        const { deps: d2 } = makeDeps({
            onClose: () => fallback.push('close'),
            showAnalytics: () => fallback.push('show'),
            openWindow: () => null,
        });
        const r2 = TAB.renderAnalyticsTab(makeEl('div'), d2);
        await r2.ready;
        r2.openBtn.click();
        eq(fallback, ['close', 'show'], 'вкладку заблокировали — дашборд открылся здесь');
    }

    // «Пересобрать индекс»: сервер отвечает сразу `scanning`, дальше поллинг.
    refs.rescanBtn.click();
    await Promise.resolve();
    await Promise.resolve();
    eq(calls.rescan, 1, 'пересбор запрошен');
    eq(refs.rescanBtn.disabled, true, 'кнопка заблокирована на время прохода');
    eq(refs.progress.hidden, false, 'и показан прогресс пересбора');
}

{
    // Сорвавшийся пересбор не оставляет вкладку с намертво выключенной кнопкой.
    const { deps } = makeDeps({
        rescanAnalytics: async () => { throw new Error('сеть отвалилась'); },
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;
    refs.rescanBtn.click();
    await Promise.resolve();
    await Promise.resolve();
    eq(refs.rescanBtn.disabled, false, 'после ошибки пересбор можно повторить');
    ok(/сеть отвалилась/.test(textOf(refs.status)), 'и причина показана', textOf(refs.status));
}

{
    // Упавший summary не роняет вкладку: кнопки обязаны остаться доступными.
    const { deps } = makeDeps({
        getAnalyticsSummary: async () => { throw new Error('HTTP 500'); },
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;
    ok(/HTTP 500/.test(textOf(refs.kpi)), 'ошибка показана текстом', textOf(refs.kpi));
    ok(refs.openBtn && refs.rescanBtn, 'кнопки на месте — вью и пересбор доступны');
}

{
    // Снятие вкладки гасит таймер: модалка закрывается, запросов после неё нет.
    const { deps, timers } = makeDeps({
        getAnalyticsSummary: async () => ({ state: 'scanning', progress: { done: 1, total: 4 } }),
    });
    const refs = TAB.renderAnalyticsTab(makeEl('div'), deps);
    await refs.ready;
    ok(timers.filter(Boolean).length > 0, 'поллинг запущен');
    refs.dispose();
    eq(timers.filter(Boolean).length, 0, 'dispose() снимает таймер поллинга');
}

// ===========================================================================
// 6. ⚠ ВКЛАДКА РЕАЛЬНО ЗАРЕГИСТРИРОВАНА В МОДАЛКЕ (СЕМЬ ТОЧЕК)
// ===========================================================================
//
// Проверка текстовая по исходнику: поднимать модалку целиком значит тянуть
// три десятка её импортов ради семи строк.

{
    const modal = readFileSync(new URL('../../static/js/settings/modal.js', import.meta.url), 'utf8');

    ok(/import \{ renderAnalyticsTab \} from '\.\/analytics-tab\.js'/.test(modal),
        '1/7: модалка импортирует рендерер вкладки');
    ok(/data-tab="analytics"/.test(modal), '2/7: кнопка вкладки в списке');
    ok(/data-panel="analytics"/.test(modal) && /id="ps-panel-analytics"/.test(modal),
        '3/7: панель вкладки');
    ok(/querySelector\('#ps-analytics-content'\)/.test(modal), '4/7: селектор контента');
    ok(/analyticsTabState\s*=\s*\{/.test(modal), '5/7: объект состояния (ленивый рендер)');
    ok(/name === 'analytics'/.test(modal) && /renderAnalyticsPanel\(\)/.test(modal),
        '6/7: ветка в showTab');
    ok(/initialTab === 'analytics'/.test(modal), '7/7: whitelist initialTab');

    // Сеть вкладке передаётся, а не берётся ею самой — и список обязан быть
    // полным: пропущенное имя вкладка переживает молча (кнопка пересбора
    // просто ничего не делает).
    for (const fn of ['getAnalyticsSummary', 'getAnalyticsStatus', 'rescanAnalytics']) {
        ok(new RegExp(`${fn}`).test(modal), `модалка передаёт вкладке ${fn}`);
    }
    ok(/from '\.\.\/analytics\/api\.js'/.test(modal),
        'и берёт их из клиента аналитики, а не из своего fetch');

    // Ленивый рендер — не экономия, а условие: первый запрос к API запускает
    // индексацию ~/.claude/projects, и она не должна стартовать у того, кто
    // зашёл в настройки поменять тему.
    ok(/if \(analyticsTabState\.loaded/.test(modal),
        'вкладка рисуется лениво — иначе скан стартует при открытии модалки');

    // Кнопка «на весь экран» закрывает модалку через onClose из модалки.
    ok(/onClose: close/.test(modal), 'вкладке передан колбэк закрытия модалки');
}

// ===========================================================================
// 7. ⚠ НИ СВОЕГО `fetch`, НИ `innerHTML`
// ===========================================================================

{
    const src = readFileSync(new URL('../../static/js/settings/analytics-tab.js', import.meta.url), 'utf8');
    const code = src.split('\n').filter((l) => !l.trimStart().startsWith('//')).join('\n');
    ok(!/\bfetch\s*\(/.test(code),
        'вкладка не зовёт fetch напрямую: мимо apiFetch это 401 в remote-режиме');
    ok(!/innerHTML/.test(code),
        'разметка строится узлами: значения приходят с сервера');
}

console.log(`${checks - failures}/${checks} проверок прошли`);
process.exit(failures ? 1 : 0);
