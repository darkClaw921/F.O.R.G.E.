// Тест логики дашборда аналитики (Фаза 5):
// tmux-web/static/js/analytics/view-core.js + стражи по тексту analytics.js.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Дашборд ломается тихо в четырёх местах, и ни
// одно из них не даёт ошибки в консоли:
//
//   • ГРАНИЦЫ ПЕРИОДА. «7 дней» легко превращаются в восемь колонок (сегодня
//     минус семь), и никто этого не замечает — данные-то есть.
//   • ДЕЛЬТА К ПУСТОМУ ПЕРИОДУ. Первая неделя работы даёт previous = 0; без
//     защиты плитка показывает «+Infinity%» или «NaN%» и выглядит как поломка
//     самих цифр.
//   • ПОДПИСИ БАКЕТОВ. Границы гистограмм живут на сервере (PAUSE_BUCKET_-
//     EDGES_MS, MSG_LEN_EDGES). Хардкод подписей на фронте пережил бы правку
//     констант в rollup.rs молча — и график подписывал бы не то, что рисует.
//   • РАЗВИЛКА scanning/ready. Ready при идущем фоновом доборе обязан
//     показывать ДАННЫЕ; подмена их прогресс-баром прячет готовый дашборд.
//
// Плюс два договора файла вью, которые проверяются по тексту (импортировать
// analytics.js в Node нельзя — он тянет core/dom.js, а тот на первой строке
// зовёт document.getElementById):
//
//   • ни одного innerHTML — подписи проектов и инструментов приходят с диска;
//   • ни одного своего fetch — сеть только через js/analytics/api.js, иначе в
//     remote-режиме будет 401 (заголовки ставит подменённый window.fetch).
//
// Запуск: node tmux-web/tests/frontend/analytics_view.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const V = await import(new URL('../../static/js/analytics/view-core.js', import.meta.url).href);

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
// Период
// ---------------------------------------------------------------------------

eq(V.normalizeRange('7'), '7', 'normalizeRange пропускает валидный период');
eq(V.normalizeRange('42'), '30', 'мусор из localStorage → умолчание');
eq(V.normalizeRange(null), '30', 'null → умолчание');
eq(V.normalizeRange('all'), 'all', '«всё время» — валидное значение');

{
    // 2026-09-06 — среда; «7 дней» это она сама и шесть предыдущих дней.
    const now = new Date(2026, 8, 6, 12, 0, 0);
    eq(V.rangeToQuery('7', now), { from: '2026-08-31', to: '2026-09-06' },
        '«7 дней» — сегодня и шесть предыдущих, ровно семь дат');
    eq(V.rangeToQuery('30', now), { from: '2026-08-08', to: '2026-09-06' },
        '«30 дней» включает сегодня');
    eq(V.rangeToQuery('all', now), {},
        '«всё время» не шлёт границ — их подставит сервер, иначе уедет предыдущий период');
}

{
    // Переход через границу месяца и года считается календарём, а не вычитанием
    // миллисекунд: летнее время сдвинуло бы сутки.
    const ny = new Date(2027, 0, 2, 3, 0, 0);
    eq(V.rangeToQuery('7', ny).from, '2026-12-27', 'период переходит через Новый год');
}

eq(V.bucketFor('7'), 'day', 'до 90 дней — дневные корзины');
eq(V.bucketFor('90'), 'day', '90 дней всё ещё дни');
eq(V.bucketFor('all'), 'week', '«всё время» — недели, иначе ось в частокол');

// ---------------------------------------------------------------------------
// Дельты
// ---------------------------------------------------------------------------

eq(V.computeDelta(10, 0), null, 'пустой предыдущий период — дельты нет, а не «+∞»');
eq(V.computeDelta(0, 0), null, 'оба нуля — сравнивать нечего');
eq(V.computeDelta(5, NaN), null, 'NaN не превращается в дельту');

{
    const up = V.computeDelta(150, 100, null);
    eq(up.percent, 50, 'рост на половину — 50%');
    eq(up.text, '+50%', 'подпись со знаком');
    eq(up.tone, 'neutral', 'без указанного смысла окраски нет');
}

{
    // Медианная пауза: расти — хорошо (человек читает код перед согласием).
    const good = V.computeDelta(120000, 60000, 'up');
    eq(good.tone, 'good', 'рост медианной паузы окрашен как хороший');
    const bad = V.computeDelta(30000, 60000, 'up');
    eq(bad.tone, 'bad', 'падение медианной паузы — плохой сигнал');
    eq(bad.text, '−50%', 'падение подписано минусом (U+2212)');
}

{
    // Доля «не читая»: падать — хорошо. Знак тот же, смысл обратный.
    const good = V.computeDelta(0.2, 0.4, 'down');
    eq(good.tone, 'good', 'падение доли быстрых ответов — хороший сигнал');
    ok(V.computeDelta(0.4, 0.2, 'down').tone === 'bad', 'рост доли быстрых ответов — плохой');
}

eq(V.computeDelta(100, 100, 'up').text, 'без изменений', 'ровное значение не рисует «+0%»');

// ---------------------------------------------------------------------------
// Подписи бакетов
// ---------------------------------------------------------------------------

eq(V.bucketLabels([]), [], 'нет границ — нет подписей (а не «<undefined»)');
eq(V.bucketLabels([1, 2]), ['<1', '1–2', '>2'], 'N границ дают N+1 подпись');

// Границы ровно те, что в src/analytics/rollup.rs::PAUSE_BUCKET_EDGES_MS.
eq(V.pauseBucketLabels([5000, 15000, 60000, 300000]),
    ['<5 с', '5 с–15 с', '15 с–1 мин', '1 мин–5 мин', '>5 мин'],
    'подписи гистограммы пауз считаются от границ сервера');

// Границы ровно те, что в src/analytics/mod.rs::MSG_LEN_EDGES.
eq(V.msgLenBucketLabels([100, 300, 1000, 3000]),
    ['<100', '100–300', '300–1000', '1000–3000', '>3000'],
    'подписи длин сообщений — от границ сервера');

// ---------------------------------------------------------------------------
// Производные ряды
// ---------------------------------------------------------------------------

eq(V.ratioSeries([5, 0, 3], [10, 0, 6]), [0.5, 0, 0.5],
    'нулевой знаменатель даёт 0, а не NaN и не дыру в линии');
eq(V.ratioSeries(null, null), [], 'отсутствующие ряды не роняют расчёт');
eq(V.ratioSeries([1, 2], [4]), [0.25, 0], 'ряды разной длины выравниваются по длинному');

// ---------------------------------------------------------------------------
// Пустой период
// ---------------------------------------------------------------------------

ok(V.isEmptySummary({ series: {}, kpi: {} }), 'ответ без серий и KPI считается пустым');
ok(V.isEmptySummary(null), 'null тоже пуст, а не исключение');
ok(!V.isEmptySummary({ series: { buckets: ['2026-09-01'] }, kpi: {} }),
    'есть корзины — период не пустой');
ok(!V.isEmptySummary({ series: {}, kpi: { humanMessages: 3 } }),
    'есть сообщения — период не пустой');

// ---------------------------------------------------------------------------
// Прогресс и развилка режима
// ---------------------------------------------------------------------------

eq(V.progressPercent({ done: 0, total: 0 }), 0,
    'total=0 в начале прохода — это 0%, а не «готово»');
eq(V.progressPercent({ done: 5, total: 10 }), 50, 'половина файлов — 50%');
eq(V.progressPercent({ done: 99, total: 10 }), 100, 'перелёт зажимается сотней');

{
    const scanning = V.viewMode({ state: 'scanning', progress: { running: true } });
    eq(scanning.mode, 'progress', 'индекса ещё нет — только прогресс-бар');
    ok(scanning.polling, 'во время первого скана вью поллит статус');

    const idle = V.viewMode({ state: 'idle', progress: { running: false } });
    eq(idle.mode, 'progress', 'idle тоже не рисует нули как данные');
    ok(idle.polling, 'из idle статус всё равно поллится — запрос запускает скан');

    // ГЛАВНАЯ РАЗВИЛКА: индекс готов, в фоне идёт добор хвостов.
    const refreshing = V.viewMode({ state: 'ready', progress: { running: true } });
    eq(refreshing.mode, 'data', 'ready при фоновом доборе показывает ДАННЫЕ, а не прогресс-бар');
    ok(refreshing.refreshing, 'но помечает, что цифры сейчас обновятся');
    ok(refreshing.polling, 'и ждёт окончания прохода, чтобы перерисоваться');

    const ready = V.viewMode({ state: 'ready', progress: { running: false } });
    eq(ready.mode, 'data', 'готовый индекс — данные');
    ok(!ready.polling, 'по готовому индексу поллинга нет — сеть молчит');
}

// ---------------------------------------------------------------------------
// Стражи файла вью (проверка по тексту)
// ---------------------------------------------------------------------------

const viewSrc = readFileSync(new URL('../../static/js/analytics/analytics.js', import.meta.url), 'utf8');

/** Код без комментариев: шапка файла рассказывает про запреты их же словами. */
function stripComments(src) {
    return src.replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/(^|[^:])\/\/[^\n]*/g, '$1');
}
const viewCode = stripComments(viewSrc);

ok(!/\binnerHTML\b/.test(viewCode),
    'analytics.js не использует innerHTML: подписи проектов и инструментов приходят с диска');
ok(!/(^|[^.\w])fetch\s*\(/.test(viewCode),
    'analytics.js не зовёт fetch напрямую — только через js/analytics/api.js');
ok(/from '\.\/api\.js'/.test(viewSrc), 'analytics.js берёт сеть из api.js');
ok(/export function showAnalytics/.test(viewSrc) && /export function hideAnalytics/.test(viewSrc),
    'вью экспортирует showAnalytics/hideAnalytics — на них завязаны public-api и диплинк');
ok(/clearInterval/.test(viewSrc),
    'поллинг гасится clearInterval — иначе закрытое вью продолжает ходить в сеть');
ok(/removeEventListener\('keydown'/.test(viewSrc),
    'обработчик Esc снимается при закрытии — иначе он накапливается');

// Метрики второго эшелона (Фаза 7): чарты 2, 11, 13 и справочные плитки.
//
// Проверяется по тексту, а не отрисовкой: карточки рисуются из живого ответа
// сервера, а страж должен ловить именно ПРОПАЖУ карточки при рефакторинге.
ok(/heatmap,\s*band/.test(viewSrc) || (/\bheatmap\b/.test(viewSrc) && /\bband\b/.test(viewSrc)),
    'вью импортирует heatmap и band — чарты 2 и 13');
ok(/weekdayHourMatrix\(/.test(viewSrc),
    'матрица активности разворачивается через view-core, а не вручную во вью');
ok(/s\.ttr/.test(viewSrc) && /s\.ackShare/.test(viewSrc) && /s\.toolEntropyBits/.test(viewSrc)
    && /s\.uniqueProjects/.test(viewSrc) && /s\.slashShare/.test(viewSrc) && /s\.repeatShare/.test(viewSrc),
    'карточка разнообразия рисует все шесть метрик автопилота');
ok(/analytics-card-hint/.test(viewSrc),
    'под карточкой разнообразия есть пояснение — числа сами по себе ничего не говорят');
ok(/s\.versions/.test(viewSrc), 'чарт версий CLI читает series.versions');
ok(/summary\.globals/.test(viewSrc),
    'справочные плитки берутся из globals ответа summary');
ok(/за всё время/.test(viewSrc),
    'глобальные счётчики подписаны как не зависящие от периода и проекта');
ok(/nightShare/.test(viewSrc) && /nightHours/.test(viewSrc),
    'ночная активность показана и берёт границу ночи с сервера');

// Точки входа снаружи: public-api и диплинк ?view=analytics.
const publicApi = readFileSync(new URL('../../static/js/public-api.js', import.meta.url), 'utf8');
ok(/showAnalytics:\s*showAnalytics/.test(publicApi), 'ForgeApp.showAnalytics зарегистрирован');
ok(/hideAnalytics:\s*hideAnalytics/.test(publicApi), 'ForgeApp.hideAnalytics зарегистрирован');

const bootstrap = readFileSync(new URL('../../static/js/pwa/bootstrap.js', import.meta.url), 'utf8');
ok(/view === 'analytics'/.test(bootstrap), 'диплинк ?view=analytics разбирается в bootstrap');
ok(/view === 'daily-summary'/.test(bootstrap), 'диплинк ?view=daily-summary не сломан');

// Разметка и стили на месте: без них вью не показывается вовсе.
const html = readFileSync(new URL('../../static/index.html', import.meta.url), 'utf8');
for (const id of ['analytics', 'analytics-range', 'analytics-project', 'analytics-grid',
    'analytics-kpi', 'analytics-progress', 'analytics-back', 'analytics-rescan']) {
    ok(html.includes(`id="${id}"`), `в index.html есть #${id}`);
}
ok(/id="analytics"[^>]*style="display:none"/.test(html), '#analytics скрыт по умолчанию');

const dom = readFileSync(new URL('../../static/js/core/dom.js', import.meta.url), 'utf8');
ok(/\$analyticsGrid/.test(dom) && /\$analyticsRange/.test(dom), 'ref-ы вью есть в core/dom.js');

const styleCss = readFileSync(new URL('../../static/style.css', import.meta.url), 'utf8');
ok(styleCss.includes("@import url('./css/analytics.css')"), 'css/analytics.css подключён в style.css');

const analyticsCss = readFileSync(new URL('../../static/css/analytics.css', import.meta.url), 'utf8');
ok(!/#[0-9a-fA-F]{3,8}\b/.test(analyticsCss),
    'в analytics.css нет литералов цвета — иначе дашборд не переживёт смену темы');

console.log(`\nanalytics_view: проверок ${checks}, провалов ${failures}`);
process.exit(failures ? 1 : 0);
