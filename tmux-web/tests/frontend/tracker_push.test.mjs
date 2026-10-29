// Регрессионный тест переноса задачи трекера в TODO вкладки Tasks
// (`static/js/tracker/to-todo.js`).
//
// Проверяется ТОЛЬКО то, что не трогает DOM: сборка списка корней, выбор
// корня по умолчанию и текст описания будущей TODO-карточки. Сам диалог,
// клики и запрос `POST /api/todos` проверяются живым гейтом в браузере.
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. корни — это РАЗНЫЕ cwd открытых сессий tmux без повторов: концепции
//      «проект» в devforge нет, и второй справочник корней завёл бы
//      расхождение с тем, по чему фильтруется вкладка Tasks;
//   2. вариант «корень активной сессии tmux» присутствует ВСЕГДА, даже когда
//      сессии нет: пропав, он молча менял бы умолчание;
//   3. умолчание — активная сессия, но запомненный прошлый выбор её
//      перебивает (критерий «выбранный корень подставляется в следующий
//      перенос»);
//   4. когда выбирать не из чего, `pickRoot` возвращает null — на этом
//      держится погашенная кнопка PUSH вместо заведомого 400 от сервера;
//   5. описание собирается из заметок, полей и чек-листа, а пустые поля строк
//      не занимают; формат зафиксирован — по нему читают перенесённые карточки.
//
// Запуск: node tmux-web/tests/frontend/tracker_push.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    shortenPath, collectRoots, pickRoot, buildDescription, contextOf,
    SESSION_ROOT_LABEL,
} from '../../static/js/tracker/to-todo.js';

const SESSIONS = [
    { name: 'forge', path: '/Users/igor/claudeWorkspace/F.O.R.G.E.' },
    { name: 'sage', path: '/Users/igor/claudeWorkspace/S.A.G.E.' },
    // Второе окно того же корня — повтора в списке быть не должно.
    { name: 'forge2', path: '/Users/igor/claudeWorkspace/F.O.R.G.E.' },
    // Сессия без cwd: сервер такие отдаёт, и в список они попадать не должны.
    { name: 'noop' },
];

test('домашний префикс сокращается до ~, чужие пути остаются как есть', () => {
    assert.equal(shortenPath('/Users/igor/claudeWorkspace/F.O.R.G.E.'),
        '~/claudeWorkspace/F.O.R.G.E.');
    assert.equal(shortenPath('/home/igor/work'), '~/work');
    assert.equal(shortenPath('/Users/igor'), '~');
    // Не домашний путь угадывать нельзя: длинный честный лучше красивого лживого.
    assert.equal(shortenPath('/opt/services/api'), '/opt/services/api');
    assert.equal(shortenPath(''), '');
});

test('корни — разные cwd сессий, без повторов и без пустых', () => {
    const roots = collectRoots(SESSIONS, null);
    const projects = roots.filter((r) => r.kind === 'project');
    assert.deepEqual(projects.map((r) => r.path), [
        '/Users/igor/claudeWorkspace/F.O.R.G.E.',
        '/Users/igor/claudeWorkspace/S.A.G.E.',
    ]);
});

test('вариант «корень активной сессии» есть всегда и стоит последним', () => {
    const withSession = collectRoots(SESSIONS, '/Users/igor/claudeWorkspace/S.A.G.E.');
    const last = withSession[withSession.length - 1];
    assert.equal(last.kind, 'session');
    assert.equal(last.label, SESSION_ROOT_LABEL);
    assert.equal(last.path, '/Users/igor/claudeWorkspace/S.A.G.E.');

    // Сессии нет — вариант остаётся, но выбрать его нельзя.
    const without = collectRoots(SESSIONS, null);
    const tail = without[without.length - 1];
    assert.equal(tail.kind, 'session');
    assert.equal(tail.path, null);
});

test('без сессий вообще список состоит из одного невыбираемого варианта', () => {
    const roots = collectRoots([], null);
    assert.equal(roots.length, 1);
    assert.equal(roots[0].path, null);
    assert.equal(pickRoot(roots, ''), null);
});

test('умолчание — активная сессия, запомненный выбор её перебивает', () => {
    const roots = collectRoots(SESSIONS, '/Users/igor/claudeWorkspace/F.O.R.G.E.');
    assert.equal(pickRoot(roots, '').kind, 'session');
    const saved = pickRoot(roots, '/Users/igor/claudeWorkspace/S.A.G.E.');
    assert.equal(saved.kind, 'project');
    assert.equal(saved.path, '/Users/igor/claudeWorkspace/S.A.G.E.');
});

test('запомненный корень, которого больше нет, умолчание не ломает', () => {
    const roots = collectRoots(SESSIONS, '/Users/igor/claudeWorkspace/F.O.R.G.E.');
    const picked = pickRoot(roots, '/Users/igor/gone');
    assert.equal(picked.kind, 'session');
});

test('без активной сессии умолчание — первый проект, а не пустота', () => {
    const roots = collectRoots(SESSIONS, null);
    const picked = pickRoot(roots, '');
    assert.equal(picked.kind, 'project');
    assert.equal(picked.path, '/Users/igor/claudeWorkspace/F.O.R.G.E.');
});

const TASK = {
    id: '3f1c9a20-0000-4000-8000-000000000000',
    title: 'Свести дизайн трекера',
    notes: 'палитра зелёная, не фиолетовая',
    status: 'doing',
    priority: 1,
    tags: ['forge', 'design'],
    estimate_min: 270,
    start: '2026-09-08',
    due: '2026-09-09',
    list_id: 'L1',
};

const SNAP = {
    lists: [{ id: 'L1', name: 'work' }],
    items: [
        { id: 'i2', task_id: TASK.id, title: 'отдать в реализацию', done: false, order: 1 },
        { id: 'i1', task_id: TASK.id, title: 'свести палитру', done: true, order: 0 },
        { id: 'i9', task_id: 'другая', title: 'чужой пункт', done: false, order: 0 },
    ],
    task_seconds: { [TASK.id]: 2539 },
};

test('контекст берётся из снимка: список, время и только свои пункты по порядку', () => {
    const ctx = contextOf(SNAP, TASK);
    assert.equal(ctx.listName, 'work');
    assert.equal(ctx.seconds, 2539);
    assert.deepEqual(ctx.items.map((i) => i.id), ['i1', 'i2']);
});

test('описание собирает заметки, поля и чек-лист в зафиксированном порядке', () => {
    const text = buildDescription(TASK, contextOf(SNAP, TASK));
    assert.deepEqual(text.split('\n'), [
        'палитра зелёная, не фиолетовая',
        '',
        '// TRACKER NODE 3f1c9a20',
        'приоритет: P1',
        'статус: doing',
        'список: work',
        'теги: #forge #design',
        'оценка: ~4ч30м',
        'начало: 2026-09-08',
        'срок: 2026-09-09',
        'потрачено: 0:42:19',
        '',
        '// CHECKLIST 1/2',
        '[x] свести палитру',
        '[ ] отдать в реализацию',
    ]);
});

test('пустые поля строк не занимают', () => {
    const bare = { id: 'abcdef0123', title: 'просто задача' };
    const text = buildDescription(bare, contextOf({}, bare));
    assert.deepEqual(text.split('\n'), [
        '// TRACKER NODE abcdef01',
        'приоритет: P2',
        'статус: inbox',
    ]);
});

test('описание не несёт обратной ссылки на todo — связи между ними нет', () => {
    // Правило фазы: перенос это КОПИЯ. Появление сюда id созданной карточки
    // означало бы связь, которой в модели нет и которую никто не поддерживает.
    const text = buildDescription(TASK, contextOf(SNAP, TASK));
    assert.ok(!/todo/i.test(text), text);
});
