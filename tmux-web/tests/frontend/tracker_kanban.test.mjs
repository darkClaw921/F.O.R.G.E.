// Регрессионный тест доски трекера (`static/js/tracker/render-kanban.js`) и
// общей перестановки (`planOrder` из `static/js/tracker/render-list.js`).
//
// Проверяется ТОЛЬКО то, что не трогает DOM: раскладка задач по колонкам и
// пересборка глобального порядка. Отрисовка карточек и сам drag'n'drop
// проверяются живым гейтом в браузере — мышь без браузера не подделать.
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. колонки доски — это значения `TaskStatus` из
//      plugins/tracker/src/models.rs, а не свой справочник: разъезд имён
//      означал бы 400 от `PATCH .../status` на каждый перенос;
//   2. задача с незнакомым статусом попадает в `inbox`, а НЕ исчезает с доски;
//   3. `planOrder` отправляет ПОЛНЫЙ порядок всех задач снимка, а не одной
//      колонки: список одной лишь выборки увёл бы невидимые задачи в хвост;
//   4. внутри колонки задачи стоят по ручному `order`, а не по приоритету и не
//      по времени выполнения — иначе перестановка мышью молча отменялась бы.
//
// Запуск: node tmux-web/tests/frontend/tracker_kanban.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import { COLUMNS, groupByStatus } from '../../static/js/tracker/render-kanban.js';
import { planOrder } from '../../static/js/tracker/render-list.js';

/** Пять задач: по одной в каждой колонке, порядок нарочно вперемешку. */
function tasks() {
    return [
        { id: 'a', status: 'inbox', order: 3, priority: 0, title: 'a' },
        { id: 'b', status: 'today', order: 1, priority: 2, title: 'b' },
        { id: 'c', status: 'doing', order: 4, priority: 1, title: 'c' },
        { id: 'd', status: 'waiting', order: 0, priority: 3, title: 'd' },
        { id: 'e', status: 'done', order: 2, priority: 2, title: 'e', done_at: '2026-09-08T10:00:00Z' },
    ];
}

test('колонки доски совпадают с TaskStatus сервера и стоят в порядке макета', () => {
    // Строки взяты из `TaskStatus::as_str` (plugins/tracker/src/models.rs) и
    // из макета «Экран — KANBAN». Ровно эти значения уходят в PATCH .../status.
    assert.deepEqual(COLUMNS.map((c) => c.id), ['inbox', 'today', 'doing', 'waiting', 'done']);
    assert.deepEqual(COLUMNS.map((c) => c.label),
        ['// inbox', '// today', '// doing', '// waiting', '// done']);
});

test('каждая задача попадает в колонку своего статуса', () => {
    const by = groupByStatus(tasks());
    assert.deepEqual(by.inbox.map((t) => t.id), ['a']);
    assert.deepEqual(by.today.map((t) => t.id), ['b']);
    assert.deepEqual(by.doing.map((t) => t.id), ['c']);
    assert.deepEqual(by.waiting.map((t) => t.id), ['d']);
    assert.deepEqual(by.done.map((t) => t.id), ['e']);
});

test('пустая колонка есть в раскладке, а не отсутствует', () => {
    const by = groupByStatus([{ id: 'x', status: 'today', order: 0 }]);
    COLUMNS.forEach((c) => assert.ok(Array.isArray(by[c.id]), `нет колонки ${c.id}`));
    assert.equal(by.done.length, 0);
});

test('задача с незнакомым статусом уезжает в inbox, а не пропадает', () => {
    // Стор новее фронта (например, добавили колонку) — потерянная задача хуже
    // задачи не в той колонке.
    const by = groupByStatus([{ id: 'z', status: 'archived', order: 0 }]);
    assert.deepEqual(by.inbox.map((t) => t.id), ['z']);
    const total = COLUMNS.reduce((n, c) => n + by[c.id].length, 0);
    assert.equal(total, 1);
});

test('внутри колонки порядок ручной, а не по приоритету', () => {
    const by = groupByStatus([
        { id: 'p0', status: 'today', order: 5, priority: 0 },
        { id: 'p3', status: 'today', order: 1, priority: 3 },
        { id: 'p1', status: 'today', order: 3, priority: 1 },
    ]);
    assert.deepEqual(by.today.map((t) => t.id), ['p3', 'p1', 'p0']);
});

test('колонка done не пересортировывается по done_at', () => {
    // splitDone в ленте кладёт свежевыполненные сверху; на доске так нельзя —
    // колонку done тоже можно перетаскивать, и своя сортировка отменяла бы
    // перестановку молча.
    const by = groupByStatus([
        { id: 'old', status: 'done', order: 0, done_at: '2026-09-01T10:00:00Z' },
        { id: 'new', status: 'done', order: 1, done_at: '2026-09-08T10:00:00Z' },
    ]);
    assert.deepEqual(by.done.map((t) => t.id), ['old', 'new']);
});

test('planOrder отдаёт ПОЛНЫЙ порядок всех задач, а не одной колонки', () => {
    // Порядок по `order`: d(0) b(1) e(2) a(3) c(4). Кладём `c` перед `b`.
    const ids = planOrder(tasks(), 'c', 'b', false);
    assert.deepEqual(ids, ['d', 'c', 'b', 'e', 'a']);
    assert.equal(ids.length, 5, 'в порядке обязаны быть ВСЕ задачи снимка');
});

test('planOrder кладёт после соседки, когда отпустили ниже её середины', () => {
    assert.deepEqual(planOrder(tasks(), 'c', 'b', true), ['d', 'b', 'c', 'e', 'a']);
});

test('planOrder отказывается двигать в пустоту', () => {
    assert.equal(planOrder(tasks(), 'c', 'c', false), null, 'сам на себя');
    assert.equal(planOrder(tasks(), 'c', null, false), null, 'соседки нет');
    assert.equal(planOrder(tasks(), 'c', 'нет-такой', false), null, 'соседки нет в снимке');
});

test('planOrder не теряет задачу при переносе в конец', () => {
    const ids = planOrder(tasks(), 'd', 'c', true);
    assert.deepEqual(ids, ['b', 'e', 'a', 'c', 'd']);
    assert.equal(new Set(ids).size, 5);
});
