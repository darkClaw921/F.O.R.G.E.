// Регрессионный тест конверсий полей карточки трекера
// (`static/js/tracker/fields.js`) и чистой части диалога вехи
// (`static/js/tracker/milestone.js`).
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. КРУГОВОЙ ПРОГОН НИЧЕГО НЕ МЕНЯЕТ. Карточка шлёт в PATCH только
//      действительно изменённые поля, сравнивая строки. Если `to*` → `from*`
//      вернут не ту же строку, открытая и тут же сохранённая карточка
//      переписала бы срок КАЖДОЙ задаче — и незаметно;
//   2. голая дата остаётся голой датой и не проваливается в UTC-полночь
//      (в зонах западнее Гринвича это сдвиг на сутки назад);
//   3. пустое поле = `null` = СБРОС, а не «оставить как было»;
//   4. оценка не из списка (`~90м`) открывается КАК ЕСТЬ в режиме «своё»;
//   5. правка одной подписи вехи не трогает её дату.
//
// Запуск: node --test tmux-web/tests/frontend/tracker_fields.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    ESTIMATE_CUSTOM, ESTIMATE_PRESETS, estimateChoice, estimateValue,
    fromDateInput, fromDueInput, fromLocalInput, toDateInput, toDueInput, toLocalInput,
} from '../../static/js/tracker/fields.js';
import { milestonePatch, milestoneReady } from '../../static/js/tracker/milestone.js';

// ------------------------------------------------------------------ start (date)

test('start: голая дата проходит круг без изменений', () => {
    assert.equal(toDateInput('2026-09-08'), '2026-09-08');
    assert.equal(fromDateInput(toDateInput('2026-09-08')), '2026-09-08');
});

test('start: RFC 3339 сводится к своему ЛОКАЛЬНОМУ дню', () => {
    const noon = new Date(2026, 8, 8, 12, 0, 0).toISOString();
    assert.equal(toDateInput(noon), '2026-09-08');
});

test('start: пустое и мусорное значение — это сброс (null)', () => {
    assert.equal(toDateInput(null), '');
    assert.equal(toDateInput(''), '');
    assert.equal(fromDateInput(''), null);
    assert.equal(fromDateInput('   '), null);
    assert.equal(fromDateInput('никогда'), null);
});

// -------------------------------------------------------------------- due

test('due: голая дата открывается полночью и возвращается голой датой', () => {
    assert.equal(toDueInput('2026-09-10'), '2026-09-10T00:00');
    assert.equal(fromDueInput('2026-09-10T00:00'), '2026-09-10');
    assert.equal(fromDueInput(toDueInput('2026-09-10')), '2026-09-10');
});

test('due: секунды в значении пикера не мешают распознать полночь', () => {
    assert.equal(fromDueInput('2026-09-10T00:00:00'), '2026-09-10');
});

test('due: момент со временем проходит круг с точностью до минуты', () => {
    const at18 = new Date(2026, 8, 9, 18, 0, 0);
    const raw = at18.toISOString();
    const shown = toDueInput(raw);
    assert.equal(shown, '2026-09-09T18:00');
    assert.equal(new Date(fromDueInput(shown)).getTime(), at18.getTime());
});

test('due: пустое поле — сброс', () => {
    assert.equal(toDueInput(''), '');
    assert.equal(fromDueInput(''), null);
    assert.equal(fromDueInput('  '), null);
});

test('datetime-local ручных отрезков остался прежним', () => {
    const at = new Date(2026, 8, 8, 14, 2, 0);
    assert.equal(toLocalInput(at.toISOString()), '2026-09-08T14:02');
    assert.equal(new Date(fromLocalInput('2026-09-08T14:02')).getTime(), at.getTime());
    assert.equal(fromLocalInput(''), null);
});

// ----------------------------------------------------------------- оценка

test('оценка: типовое значение открывается пунктом списка', () => {
    assert.deepEqual(estimateChoice(120), { mode: '120', custom: '' });
    assert.equal(estimateValue('120', ''), '120');
});

test('оценка: значение не из списка открывается в «своём» как есть', () => {
    assert.deepEqual(estimateChoice(90), { mode: ESTIMATE_CUSTOM, custom: '90' });
    assert.equal(estimateValue(ESTIMATE_CUSTOM, '90'), '90');
});

test('оценка: нет оценки — пустой пункт и пустое значение', () => {
    assert.deepEqual(estimateChoice(null), { mode: '', custom: '' });
    assert.deepEqual(estimateChoice(0), { mode: '', custom: '' });
    assert.equal(estimateValue('', '90'), '');
});

test('оценка: «своё» без числа — это сброс', () => {
    assert.equal(estimateValue(ESTIMATE_CUSTOM, ''), '');
    assert.equal(estimateValue(ESTIMATE_CUSTOM, 'абв'), '');
    assert.equal(estimateValue(ESTIMATE_CUSTOM, '0'), '');
});

test('оценка: список типовых длительностей не пуст и весь в минутах', () => {
    assert.ok(ESTIMATE_PRESETS.length >= 6);
    for (const [m, label] of ESTIMATE_PRESETS) {
        assert.equal(typeof m, 'number');
        assert.ok(m > 0);
        assert.ok(label.length);
    }
});

// -------------------------------------------------------------------- вехи

test('веха: правка подписи не трогает дату', () => {
    const ms = { id: 'a', title: 'релиз', date: '2026-09-12' };
    assert.deepEqual(milestonePatch(ms, { title: 'релиз 0.9', date: '2026-09-12' }),
        { title: 'релиз 0.9' });
});

test('веха: правка даты не трогает подпись', () => {
    const ms = { id: 'a', title: 'релиз', date: '2026-09-12' };
    assert.deepEqual(milestonePatch(ms, { title: 'релиз', date: '2026-09-14' }),
        { date: '2026-09-14' });
});

test('веха: ничего не изменили — пустой патч', () => {
    const ms = { id: 'a', title: 'релиз', date: '2026-09-12' };
    assert.deepEqual(milestonePatch(ms, { title: 'релиз', date: '2026-09-12' }), {});
});

test('веха: пустая подпись и негодная дата в патч не попадают', () => {
    const ms = { id: 'a', title: 'релиз', date: '2026-09-12' };
    assert.deepEqual(milestonePatch(ms, { title: '  ', date: 'завтра' }), {});
});

test('веха: создавать можно только с подписью и датой', () => {
    assert.equal(milestoneReady({ title: 'релиз', date: '2026-09-12' }), true);
    assert.equal(milestoneReady({ title: '', date: '2026-09-12' }), false);
    assert.equal(milestoneReady({ title: 'релиз', date: '' }), false);
    assert.equal(milestoneReady(null), false);
});
