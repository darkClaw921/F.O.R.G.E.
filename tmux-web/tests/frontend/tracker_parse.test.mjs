// Регрессия: разбор строки prompt'а `tracker:~$` и выборка режима list
// (плагин forge-tracker, фаза 4).
//
//   node tmux-web/tests/frontend/tracker_parse.test.mjs
//
// Оба модуля под тестом чистые: ни DOM, ни fetch на верхнем уровне, поэтому
// импортируются в голом node без единого стаба. `render-list.js` трогает
// document только внутри функций отрисовки, которые здесь не зовутся.

import assert from 'node:assert/strict';
import test from 'node:test';

import {
    parsePrompt, parseEstimate, parseDue, toCreatePayload, previewOf, formatEstimate,
} from '../../static/js/tracker/parse.js';
import {
    filterTasks, sortTasks, splitDone, fmtDuration, parseTs, inDir,
} from '../../static/js/tracker/render-list.js';

// «Сейчас» для всех тестов со сроками: вторник, 8 сентября 2026, 14:02
// местного времени. Вторник выбран намеренно — понедельник и воскресенье
// сделали бы границу недели неотличимой от границы дня.
const NOW = new Date(2026, 8, 8, 14, 2, 0, 0);

// ---------------------------------------------------------------- parse.js

test('строка со ВСЕМИ токенами сразу — строка из макета', () => {
    const p = parsePrompt('Свести дизайн трекера !1 #forge ~4ч @work due:18:00', NOW);
    assert.equal(p.ok, true);
    assert.equal(p.title, 'Свести дизайн трекера');
    assert.equal(p.priority, 1);
    assert.deepEqual(p.tags, ['forge']);
    assert.equal(p.estimate_min, 240);
    assert.equal(p.list, 'work');
    assert.equal(p.due, new Date(2026, 8, 8, 18, 0, 0, 0).toISOString().replace(/\.\d{3}Z$/, 'Z'));
    assert.deepEqual(p.unknown, []);
});

test('вторая строка из макета', () => {
    const p = parsePrompt('Созвон с командой !2 #meet ~1ч due:18:00', NOW);
    assert.equal(p.title, 'Созвон с командой');
    assert.equal(p.priority, 2);
    assert.deepEqual(p.tags, ['meet']);
    assert.equal(p.estimate_min, 60);
    assert.equal(p.list, null);
});

test('токены собираются в любом порядке и в середине фразы', () => {
    const p = parsePrompt('!0 Починить #bug flood control @forge ~90м', NOW);
    assert.equal(p.title, 'Починить flood control');
    assert.equal(p.priority, 0);
    assert.equal(p.estimate_min, 90);
    assert.equal(p.list, 'forge');
});

test('несколько тегов, повтор не дублируется', () => {
    const p = parsePrompt('Задача #a #b #a', NOW);
    assert.deepEqual(p.tags, ['a', 'b']);
});

test('повтор токена побеждает последним', () => {
    const p = parsePrompt('Задача !2 !0 @one @two', NOW);
    assert.equal(p.priority, 0);
    assert.equal(p.list, 'two');
});

test('неизвестный токен НЕ ломает разбор и остаётся в заголовке', () => {
    const p = parsePrompt('Учить C# !7 ~абв due:никогда #!!! @', NOW);
    assert.equal(p.ok, true);
    // `C#` и `@` не начинаются со значащего символа / не имеют тела —
    // остаются текстом; `!7`, `~абв`, `due:никогда`, `#!!!` не разобрались и
    // тоже вернулись в заголовок.
    assert.equal(p.title, 'Учить C# !7 ~абв due:никогда #!!! @');
    assert.equal(p.priority, null);
    assert.equal(p.estimate_min, null);
    assert.equal(p.due, null);
    assert.deepEqual(p.tags, []);
    assert.deepEqual(p.unknown, ['!7', '~абв', 'due:никогда', '#!!!']);
});

test('токен — целое слово: x!1 и a#b разметкой не становятся', () => {
    const p = parsePrompt('версия x!1 и файл a#b', NOW);
    assert.equal(p.title, 'версия x!1 и файл a#b');
    assert.equal(p.priority, null);
    assert.deepEqual(p.tags, []);
});

test('пустая строка задачу не создаёт', () => {
    for (const raw of ['', '   ', null, undefined]) {
        assert.equal(parsePrompt(raw, NOW).ok, false);
    }
});

test('строка из одних токенов задачу не создаёт: заголовок пуст', () => {
    const p = parsePrompt('!1 #tag ~2ч @work', NOW);
    assert.equal(p.ok, false);
    assert.equal(p.title, '');
});

test('оценка: единицы, дробные, голое число', () => {
    assert.equal(parseEstimate('2ч'), 120);
    assert.equal(parseEstimate('2h'), 120);
    assert.equal(parseEstimate('90м'), 90);
    assert.equal(parseEstimate('90'), 90);
    assert.equal(parseEstimate('1.5ч'), 90);
    assert.equal(parseEstimate('1,5ч'), 90);
    assert.equal(parseEstimate('1д'), 480);
    assert.equal(parseEstimate('30мин'), 30);
    assert.equal(parseEstimate('0ч'), null);
    assert.equal(parseEstimate('абв'), null);
});

test('срок: время, дата, дата со временем, слова, DD.MM', () => {
    const iso = (d) => d.toISOString().replace(/\.\d{3}Z$/, 'Z');
    assert.equal(parseDue('18:00', NOW), iso(new Date(2026, 8, 8, 18, 0)));
    assert.equal(parseDue('2026-09-12', NOW), '2026-09-12');
    assert.equal(parseDue('2026-09-12T09:30', NOW), iso(new Date(2026, 8, 12, 9, 30)));
    assert.equal(parseDue('today', NOW), '2026-09-08');
    assert.equal(parseDue('сегодня', NOW), '2026-09-08');
    assert.equal(parseDue('завтра', NOW), '2026-09-09');
    assert.equal(parseDue('12.09', NOW), '2026-09-12');
    assert.equal(parseDue('12.09.2027', NOW), '2027-09-12');
    assert.equal(parseDue('25:61', NOW), null);
    assert.equal(parseDue('никогда', NOW), null);
});

test('due:09:00 в 14:02 остаётся СЕГОДНЯШНИМ, а не уезжает на завтра', () => {
    const p = parsePrompt('Задача due:09:00', NOW);
    assert.ok(p.due.startsWith('2026-09-08') || p.due.startsWith('2026-09-0'));
    const d = new Date(p.due);
    assert.equal(d.getDate(), 8);
});

test('toCreatePayload не кладёт незаданные поля', () => {
    const body = toCreatePayload(parsePrompt('Просто задача', NOW), null, null);
    assert.deepEqual(body, { title: 'Просто задача' });
    assert.equal('priority' in body, false);
    assert.equal('due' in body, false);
});

test('toCreatePayload кладёт всё заданное, list_id и status — снаружи', () => {
    const body = toCreatePayload(parsePrompt('Задача !1 #a ~2ч due:2026-09-12', NOW), 'L1', 'today');
    assert.deepEqual(body, {
        title: 'Задача',
        priority: 1,
        tags: ['a'],
        estimate_min: 120,
        due: '2026-09-12',
        list_id: 'L1',
        status: 'today',
    });
});

test('превью показывает распознанные поля и остаток текста', () => {
    const pv = previewOf(parsePrompt('Задача !1 #a ~2ч @work !9', NOW));
    const byKey = Object.fromEntries(pv.map((p) => [p.key, p.value]));
    assert.equal(byKey.title, 'Задача !9');
    assert.equal(byKey.prio, 'P1');
    assert.equal(byKey.list, 'work');
    assert.equal(byKey.tags, '#a');
    assert.equal(byKey.est, '~2ч');
    assert.equal(byKey['as text'], '!9');
});

test('formatEstimate', () => {
    assert.equal(formatEstimate(240), '~4ч');
    assert.equal(formatEstimate(90), '~1ч30м');
    assert.equal(formatEstimate(45), '~45м');
    assert.equal(formatEstimate(0), '—');
});

// ------------------------------------------------------------ render-list.js

const mkTask = (over) => ({
    id: 'T', list_id: null, title: 't', notes: null, status: 'inbox', priority: 2,
    tags: [], estimate_min: null, start: null, due: null, order: 0,
    created_at: '2026-09-01T10:00:00Z', updated_at: '2026-09-01T10:00:00Z', done_at: null,
    ...over,
});

const ctx = (dir, over) => ({
    dir, listId: null, tag: null, now: NOW, runningIds: new Set(), ...over,
});

test('fmtDuration', () => {
    assert.equal(fmtDuration(0), '0:00:00');
    assert.equal(fmtDuration(2539), '0:42:19');
    assert.equal(fmtDuration(3600 * 3 + 42 * 60 + 19), '3:42:19');
});

test('parseTs: голая дата читается как ЛОКАЛЬНАЯ полночь, а не как UTC', () => {
    const d = parseTs('2026-09-08');
    assert.equal(d.getFullYear(), 2026);
    assert.equal(d.getMonth(), 8);
    assert.equal(d.getDate(), 8);
    assert.equal(d.getHours(), 0);
});

test('~/today: колонки today и doing, срок сегодня и идущий таймер', () => {
    assert.equal(inDir(mkTask({ status: 'today' }), 'today', ctx('today')), true);
    assert.equal(inDir(mkTask({ status: 'doing' }), 'today', ctx('today')), true);
    assert.equal(inDir(mkTask({ due: '2026-09-08' }), 'today', ctx('today')), true);
    assert.equal(inDir(mkTask({ status: 'waiting' }), 'today', ctx('today')), false);
    assert.equal(inDir(mkTask({ status: 'inbox' }), 'today', ctx('today')), false);
    assert.equal(
        inDir(mkTask({ id: 'R', status: 'inbox' }), 'today',
            ctx('today', { runningIds: new Set(['R']) })),
        true,
    );
});

test('~/today: срок завтра НЕ попадает, а ~/week — попадает', () => {
    const t = mkTask({ due: '2026-09-09' });
    assert.equal(inDir(t, 'today', ctx('today')), false);
    assert.equal(inDir(t, 'week', ctx('week')), true);
});

test('~/week: срок следующего понедельника уже за границей недели', () => {
    // Неделя начинается 07.09 (пн) и заканчивается 13.09 (вс).
    assert.equal(inDir(mkTask({ due: '2026-09-13' }), 'week', ctx('week')), true);
    assert.equal(inDir(mkTask({ due: '2026-09-14' }), 'week', ctx('week')), false);
});

test('~/inbox — только колонка inbox, ~/all — всё', () => {
    assert.equal(inDir(mkTask({ status: 'inbox' }), 'inbox', ctx('inbox')), true);
    assert.equal(inDir(mkTask({ status: 'doing' }), 'inbox', ctx('inbox')), false);
    for (const st of ['inbox', 'today', 'doing', 'waiting', 'done']) {
        assert.equal(inDir(mkTask({ status: st }), 'all', ctx('all')), true);
    }
});

test('выполненное видно в той директории, к чьему окну относится done_at', () => {
    const doneToday = mkTask({ status: 'done', done_at: '2026-09-08T11:00:00Z' });
    const doneLastWeek = mkTask({ status: 'done', done_at: '2026-08-30T11:00:00Z' });
    assert.equal(inDir(doneToday, 'today', ctx('today')), true);
    assert.equal(inDir(doneToday, 'week', ctx('week')), true);
    assert.equal(inDir(doneLastWeek, 'today', ctx('today')), false);
    assert.equal(inDir(doneLastWeek, 'week', ctx('week')), false);
});

test('фильтры по списку и по тегу складываются с директорией', () => {
    const tasks = [
        mkTask({ id: 'a', status: 'today', list_id: 'L1', tags: ['x'] }),
        mkTask({ id: 'b', status: 'today', list_id: 'L2', tags: ['x'] }),
        mkTask({ id: 'c', status: 'today', list_id: 'L1', tags: ['y'] }),
    ];
    assert.deepEqual(
        filterTasks(tasks, ctx('today', { listId: 'L1' })).map((t) => t.id),
        ['a', 'c'],
    );
    assert.deepEqual(
        filterTasks(tasks, ctx('today', { tag: 'x' })).map((t) => t.id),
        ['a', 'b'],
    );
    assert.deepEqual(
        filterTasks(tasks, ctx('today', { listId: 'L1', tag: 'y' })).map((t) => t.id),
        ['c'],
    );
});

test('порядок ленты задаёт order, а НЕ приоритет', () => {
    const tasks = [
        mkTask({ id: 'a', priority: 3, order: 0 }),
        mkTask({ id: 'b', priority: 0, order: 1 }),
        mkTask({ id: 'c', priority: 1, order: 2 }),
    ];
    assert.deepEqual(sortTasks(tasks).map((t) => t.id), ['a', 'b', 'c']);
});

test('splitDone отделяет сделанное, свежее сверху', () => {
    const { open, done } = splitDone([
        mkTask({ id: 'a' }),
        mkTask({ id: 'b', status: 'done', done_at: '2026-09-08T10:00:00Z' }),
        mkTask({ id: 'c', status: 'done', done_at: '2026-09-08T12:00:00Z' }),
    ]);
    assert.deepEqual(open.map((t) => t.id), ['a']);
    assert.deepEqual(done.map((t) => t.id), ['c', 'b']);
});
