// Регрессионный тест диаграммы Гантта трекера
// (`static/js/tracker/render-gantt.js`).
//
// Проверяется ТОЛЬКО то, что не трогает DOM: ось дней, плановый отрезок
// задачи, символьная строка полосы, дорожка вех и группировка по спискам.
// Сама отрисовка и переключение периода кнопками проверяются живым гейтом в
// браузере (Ф7 ГЕЙТ).
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. полоса и шапка дней строятся из ОДНОЙ сетки в `CELL` знаков на день —
//      разъезд ширины означал бы ромб срока не над своим днём;
//   2. задача с оценкой 1 ч и фактом 4:38 — это превышение (сценарий приёмки
//      из плана), а задача БЕЗ оценки превышением не бывает никогда;
//   3. `▼` стоит на сегодняшнем дне и только если сегодня попало в окно;
//   4. полоса, начавшаяся до окна, ОБРЕЗАЕТСЯ, а не теряется;
//   5. задача без `start` и без `due` не роняет расчёт — у неё просто нет
//      плановой полосы;
//   6. порядок задач в группе тот же, что в ленте и на доске (ручной `order`).
//
// Запуск: node tmux-web/tests/frontend/tracker_gantt.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    CELL, buildDays, buildLane, buildTrack, dayIndexOf, dayKey,
    groupBars, isOver, planSpan, windowFor,
} from '../../static/js/tracker/render-gantt.js';

/** Окно 01.09.2026 — 14.09.2026, «сегодня» = 08.09.2026. */
const TODAY = new Date(2026, 8, 8, 12, 0, 0);
const DAYS = buildDays(new Date(2026, 8, 1), 14, TODAY);

/** Строка полосы целиком — так проще сверять с макетом глазами. */
function line(runs) {
    return runs.map((r) => r.text).join('');
}

/** Класс знака под индексом. */
function clsAt(runs, i) {
    let n = 0;
    for (const r of runs) {
        if (i < n + r.text.length) return r.cls;
        n += r.text.length;
    }
    return null;
}

// ------------------------------------------------------------------ ось дней

test('ось дней: 14 клеток, локальные ключи, метка сегодня', () => {
    assert.equal(DAYS.length, 14);
    assert.equal(DAYS[0].key, '2026-09-01');
    assert.equal(DAYS[0].label, '01');
    assert.equal(DAYS[13].key, '2026-09-14');
    assert.equal(DAYS.filter((d) => d.today).length, 1);
    assert.equal(DAYS[7].today, true, '08.09 — восьмой день окна');
});

test('окно от якоря: первый день — сам якорь, конец эксклюзивен', () => {
    const w = windowFor(new Date(2026, 8, 1, 18, 30), 14);
    assert.equal(dayKey(w.from), '2026-09-01');
    assert.equal(dayKey(w.to), '2026-09-15');
});

test('голая дата читается как ЛОКАЛЬНАЯ полночь, а не как UTC', () => {
    // Ровно та же ловушка, что в render-list.js::parseTs: `new Date('2026-09-03')`
    // в положительных поясах дало бы 02.09.
    assert.equal(dayIndexOf(DAYS, '2026-09-03'), 2);
});

// --------------------------------------------------------------- план полосы

test('start и due заданы: полоса ровно между ними', () => {
    const span = planSpan({ start: '2026-09-03', due: '2026-09-05' }, DAYS);
    assert.deepEqual(span, { from: 2, to: 4 });
});

test('только due: полоса откладывается НАЗАД на длину оценки', () => {
    // 16 ч оценки = два восьмичасовых дня, срок — правый край.
    const span = planSpan({ due: '2026-09-05', estimate_min: 16 * 60 }, DAYS);
    assert.deepEqual(span, { from: 3, to: 4 });
});

test('только start: полоса идёт ВПЕРЁД на длину оценки', () => {
    const span = planSpan({ start: '2026-09-02', estimate_min: 60 }, DAYS);
    assert.deepEqual(span, { from: 1, to: 1 }, 'часовая задача — один день');
});

test('ни start, ни due: плановой полосы нет', () => {
    assert.equal(planSpan({ estimate_min: 120, actual_seconds: 900 }, DAYS), null);
});

test('полоса, начавшаяся до окна, обрезается слева, а не теряется', () => {
    const span = planSpan({ start: '2026-08-28', due: '2026-09-02' }, DAYS);
    assert.deepEqual(span, { from: 0, to: 1 });
});

test('полоса целиком мимо окна не рисуется', () => {
    assert.equal(planSpan({ start: '2026-07-01', due: '2026-07-10' }, DAYS), null);
    assert.equal(planSpan({ start: '2026-12-01', due: '2026-12-10' }, DAYS), null);
});

// -------------------------------------------------------------- строка полосы

test('ширина строки полосы = ширина шапки дней', () => {
    const runs = buildTrack({ start: '2026-09-01', due: '2026-09-02' }, DAYS, {});
    assert.equal(line(runs).length, DAYS.length * CELL);
});

test('заливка — доля факта от оценки; ромб срока стоит в своём дне', () => {
    const bar = {
        start: '2026-09-03', due: '2026-09-04', estimate_min: 8 * 60, actual_seconds: 4 * 3600,
    };
    const runs = buildTrack(bar, DAYS, {});
    const s = line(runs);
    // Полоса на двух днях = 8 знаков, факт — половина оценки → 4 знака `█`.
    assert.equal(s.slice(2 * CELL, 2 * CELL + 4), '████');
    assert.equal(s.slice(2 * CELL + 4, 3 * CELL + 3), '▒▒▒');
    assert.equal(s[3 * CELL + CELL - 1], '◆', 'ромб — последний знак дня срока');
    assert.equal(clsAt(runs, 2 * CELL), 'fact');
    assert.equal(clsAt(runs, 2 * CELL + 4), 'plan');
});

test('ненулевой факт всегда виден хотя бы одним знаком', () => {
    const bar = { start: '2026-09-03', due: '2026-09-04', estimate_min: 40 * 60, actual_seconds: 60 };
    const s = line(buildTrack(bar, DAYS, {}));
    assert.equal(s[2 * CELL], '█');
});

test('СЦЕНАРИЙ ПРИЁМКИ: оценка 1 ч, факт 4:38 — превышение, вся полоса красная', () => {
    const bar = {
        task_id: 'vk', title: 'Flood control VK: код 9',
        start: '2026-09-05', due: '2026-09-05',
        estimate_min: 60, actual_seconds: 4 * 3600 + 38 * 60,
    };
    assert.equal(isOver(bar, bar.actual_seconds), true);
    const runs = buildTrack(bar, DAYS, {});
    const s = line(runs);
    assert.equal(s.slice(4 * CELL, 4 * CELL + 3), '███', 'планового хвоста у сорванного плана нет');
    assert.equal(clsAt(runs, 4 * CELL), 'over');
    assert.equal(clsAt(runs, 4 * CELL + CELL - 1), 'over', 'ромб срока красный вместе с полосой');
});

test('без оценки превышения не бывает никогда', () => {
    const bar = { start: '2026-09-05', due: '2026-09-05', actual_seconds: 40 * 3600 };
    assert.equal(isOver(bar, bar.actual_seconds), false);
    assert.equal(clsAt(buildTrack(bar, DAYS, {}), 4 * CELL), 'fact');
});

test('оценка есть, факта нет — полоса чисто плановая', () => {
    const bar = { start: '2026-09-05', due: '2026-09-06', estimate_min: 180, actual_seconds: 0 };
    const runs = buildTrack(bar, DAYS, {});
    assert.equal(clsAt(runs, 4 * CELL), 'plan');
    assert.equal(line(runs)[4 * CELL], '▒');
});

test('живое время побеждает флаг сервера: полоса краснеет сразу', () => {
    // `over_estimate` посчитан сервером на момент запроса; идущий таймер мог
    // перевалить оценку уже после него.
    const bar = {
        start: '2026-09-05', due: '2026-09-05', estimate_min: 60,
        actual_seconds: 30 * 60, over_estimate: false,
    };
    assert.equal(clsAt(buildTrack(bar, DAYS, { seconds: 2 * 3600 }), 4 * CELL), 'over');
});

// --------------------------------------------------------------- дорожка вех

test('дорожка: ▼ на сегодня, ◆ в дни вех', () => {
    const ms = [
        { id: 'm1', title: 'дизайн принят', date: '2026-09-10' },
        { id: 'm2', title: 'релиз 0.9', date: '2026-09-12' },
    ];
    const s = line(buildLane(ms, DAYS, TODAY));
    assert.equal(s.length, DAYS.length * CELL);
    assert.equal(s[7 * CELL], '▼');
    assert.equal(s[9 * CELL], '◆');
    assert.equal(s[11 * CELL], '◆');
});

test('веха вне окна в дорожку не попадает', () => {
    const s = line(buildLane([{ id: 'm', title: 'далеко', date: '2026-10-01' }], DAYS, TODAY));
    assert.equal(s.replace(/[ ▼]/g, ''), '', 'кроме ▼ в дорожке ничего нет');
});

test('▼ не рисуется, если сегодня вне окна', () => {
    const past = buildDays(new Date(2026, 7, 1), 7, TODAY);
    assert.equal(line(buildLane([], past, TODAY)).includes('▼'), false);
});

test('веха и сегодня в один день не затирают друг друга', () => {
    const s = line(buildLane([{ id: 'm', title: 'демо', date: '2026-09-08' }], DAYS, TODAY));
    assert.equal(s[7 * CELL], '▼');
    assert.equal(s[7 * CELL + 1], '◆');
});

// --------------------------------------------------------------- группировка

test('группы идут в порядке списков, безсписочные — последними', () => {
    const lists = [{ id: 'w', name: 'work' }, { id: 'f', name: 'forge' }];
    const bars = [
        { task_id: '1', list_id: 'f', title: 'f1' },
        { task_id: '2', list_id: null, title: 'o1' },
        { task_id: '3', list_id: 'w', title: 'w1' },
    ];
    const tasksById = new Map(bars.map((b) => [b.task_id, { id: b.task_id, order: 1 }]));
    const g = groupBars(bars, tasksById, lists);
    assert.deepEqual(g.map((x) => x.name), ['work', 'forge', 'без списка']);
    assert.equal(g[2].bars.length, 1);
});

test('внутри группы порядок — ручной order, а не приоритет', () => {
    const lists = [{ id: 'w', name: 'work' }];
    const bars = [
        { task_id: 'a', list_id: 'w', title: 'a' },
        { task_id: 'b', list_id: 'w', title: 'b' },
    ];
    const tasksById = new Map([
        ['a', { id: 'a', order: 2, priority: 0 }],
        ['b', { id: 'b', order: 1, priority: 3 }],
    ]);
    const g = groupBars(bars, tasksById, lists);
    assert.deepEqual(g[0].bars.map((x) => x.task_id), ['b', 'a']);
});

test('пустая группа не рисуется, а неизвестный список не теряет задачу', () => {
    const lists = [{ id: 'w', name: 'work' }, { id: 'z', name: 'zzz' }];
    const bars = [{ task_id: 'a', list_id: 'ghost', title: 'a' }];
    const tasksById = new Map([['a', { id: 'a', order: 1 }]]);
    const g = groupBars(bars, tasksById, lists);
    assert.deepEqual(g.map((x) => x.name), ['без списка']);
});
