// Регрессионный тест живых часов трекера (`static/js/tracker/timer.js`).
//
// Проверяется ТОЛЬКО арифметика — та часть модуля, что не трогает DOM: опора
// времени, пересчёт длительности отрезка и живые суммы. Отрисовка блока
// `// RUNNING` проверяется живым гейтом в браузере.
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. время считается от `snap.now` СЕРВЕРА, а не от часов браузера —
//      расхождение часов не должно двигать секундомер;
//   2. время пункта чек-листа НЕ прибавляется к задаче вторым слагаемым:
//      один идущий отрезок даёт задаче ровно одну дельту, даже когда у него
//      заполнен `item_id`;
//   3. параллельные таймеры по разным задачам считаются независимо, а суммы
//      статус-бара растут на каждый из них.
//
// Запуск: node tmux-web/tests/frontend/tracker_timer.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    clockFromSnapshot, serverNow, sinceSnapshot, dayRolledOver,
    entryElapsed, liveTaskSeconds, liveItemSeconds, liveTotals,
} from '../../static/js/tracker/timer.js';

/** Снимок с двумя параллельными таймерами: задача A и пункт задачи B. */
function snapshot(nowIso) {
    return {
        now: nowIso,
        tasks: [{ id: 'A' }, { id: 'B' }],
        items: [{ id: 'i1', task_id: 'B' }],
        entries: [],
        active: [
            { id: 'e1', task_id: 'A', item_id: null, started_at: '2026-09-08T12:00:00Z' },
            { id: 'e2', task_id: 'B', item_id: 'i1', started_at: '2026-09-08T12:30:00Z' },
        ],
        task_seconds: { A: 3600, B: 1800 },
        item_seconds: { i1: 1800 },
        totals: { today_seconds: 5400, week_seconds: 9000 },
    };
}

const NOW = '2026-09-08T13:00:00Z';
const NOW_MS = Date.parse(NOW);

test('опора берётся из snap.now, а не с часов браузера', () => {
    // Часы браузера «спешат» на час: 13:00 на сервере, 14:00 у клиента.
    const clock = clockFromSnapshot(snapshot(NOW), NOW_MS + 3600_000);
    assert.equal(clock.snapNowMs, NOW_MS);
    // Через 10 секунд по часам браузера серверное «сейчас» — ровно +10 секунд.
    assert.equal(serverNow(clock, NOW_MS + 3600_000 + 10_000), NOW_MS + 10_000);
    assert.equal(sinceSnapshot(clock, NOW_MS + 3600_000 + 10_000), 10);
});

test('снимок без now не роняет расчёт', () => {
    const clock = clockFromSnapshot({}, NOW_MS);
    assert.equal(clock.snapNowMs, NOW_MS);
    assert.equal(sinceSnapshot(clock, NOW_MS + 5000), 5);
});

test('часы назад не идут', () => {
    const clock = clockFromSnapshot(snapshot(NOW), NOW_MS);
    assert.equal(sinceSnapshot(clock, NOW_MS - 60_000), 0);
});

test('длительность отрезка считается от started_at', () => {
    const snap = snapshot(NOW);
    assert.equal(entryElapsed(snap.active[0], NOW_MS), 3600);
    assert.equal(entryElapsed(snap.active[1], NOW_MS), 1800);
    // Закрытый отрезок «сейчас» не касается.
    assert.equal(entryElapsed(
        { started_at: '2026-09-08T09:00:00Z', stopped_at: '2026-09-08T10:00:00Z' },
        NOW_MS,
    ), 3600);
    // Перевёрнутый отрезок даёт ноль, а не отрицательное число.
    assert.equal(entryElapsed(
        { started_at: '2026-09-08T10:00:00Z', stopped_at: '2026-09-08T09:00:00Z' },
        NOW_MS,
    ), 0);
    assert.equal(entryElapsed(null, NOW_MS), 0);
    assert.equal(entryElapsed({ started_at: 'мусор' }, NOW_MS), 0);
});

test('два таймера тикают независимо', () => {
    const snap = snapshot(NOW);
    const clock = clockFromSnapshot(snap, NOW_MS);
    const live = liveTaskSeconds(snap, clock, NOW_MS + 20_000);
    assert.equal(live.A, 3600 + 20);
    assert.equal(live.B, 1800 + 20);

    // Остановили таймер задачи A: её сумма замерла, B продолжает расти.
    snap.active = snap.active.filter((e) => e.task_id !== 'A');
    const after = liveTaskSeconds(snap, clock, NOW_MS + 40_000);
    assert.equal(after.A, undefined, 'у остановленной задачи живой добавки нет');
    assert.equal(after.B, 1800 + 40);
});

test('время пункта входит в задачу ровно один раз', () => {
    const snap = snapshot(NOW);
    const clock = clockFromSnapshot(snap, NOW_MS);
    const byTask = liveTaskSeconds(snap, clock, NOW_MS + 30_000);
    const byItem = liveItemSeconds(snap, clock, NOW_MS + 30_000);

    // У задачи B идёт таймер ПУНКТА. Добавка к задаче — одна, а не две
    // (иначе минута работы по пункту засчиталась бы за две).
    assert.equal(byTask.B, 1800 + 30);
    assert.equal(byItem.i1, 1800 + 30);
    // Ровно то же число: время пункта — это ЧАСТЬ времени задачи.
    assert.equal(byTask.B, byItem.i1);
    // У задачи A пунктов нет вовсе.
    assert.equal(byItem.A, undefined);
});

test('суммы дня и недели растут на каждый идущий отрезок', () => {
    const snap = snapshot(NOW);
    const clock = clockFromSnapshot(snap, NOW_MS);
    const t = liveTotals(snap, clock, NOW_MS + 10_000);
    // Два идущих отрезка — две дельты по 10 секунд.
    assert.equal(t.today_seconds, 5400 + 20);
    assert.equal(t.week_seconds, 9000 + 20);

    snap.active = [];
    const idle = liveTotals(snap, clock, NOW_MS + 10_000);
    assert.equal(idle.today_seconds, 5400, 'без таймеров суммы стоят');
    assert.equal(idle.week_seconds, 9000);
});

test('смена локальных суток требует свежий снимок', () => {
    // Опорная точка — за минуту до локальной полуночи.
    const beforeMidnight = new Date();
    beforeMidnight.setHours(23, 59, 0, 0);
    const clock = clockFromSnapshot({ now: beforeMidnight.toISOString() }, beforeMidnight.getTime());
    assert.equal(dayRolledOver(clock, beforeMidnight.getTime() + 30_000), false);
    assert.equal(dayRolledOver(clock, beforeMidnight.getTime() + 120_000), true,
        'после полуночи добавка к TODAY больше не действует');
});
