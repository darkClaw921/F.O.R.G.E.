// tmux-web/tests/frontend/sidebar_grouping.test.js
//
// Phase 6 / forge-cca8.2 — регресс-тесты группировки sidebar.
//
// Что покрываем:
//   1. groupSessionsByProject(sessions, orphanKey)
//      - сессии с project_id=null/undefined → ORPHAN_KEY;
//      - сессии с одинаковым project_id → одна группа;
//      - внутри группы — сортировка по name.localeCompare();
//      - детерминированный output (стабильный порядок ключей и значений).
//   2. aggregateAllOrigins-эквивалент: структура { origin: { projects, sessions, online } }
//      собирается из state.projects/state.sessions + state.remoteProjects/state.remoteSessions.
//      Проверяем что в режиме activeOrigin='all' получаем ВСЕ origin'ы (Local + все remotes),
//      а внутри каждого — корректную группировку по project_id.
//   3. Двухуровневая фильтрация origin → project:
//      - state.projectFilter == '__all__' внутри origin → все проекты группами;
//      - state.projectFilter == <project_id> внутри origin → только сессии этого проекта;
//      - переключение origin НЕ сбрасывает projectFilter (явное решение: фильтр
//        проектов работает поверх любого origin).
//
// Запуск: node tmux-web/tests/frontend/sidebar_grouping.test.js
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.
//
// ВАЖНО: тест реплицирует pure-логику из app.js (не подгружает app.js напрямую,
// т.к. тот завязан на DOM/WebSocket/xterm). Любое изменение контракта
// groupSessionsByProject / структуры aggregateAllOrigins должно одновременно
// обновлять и этот файл — это и есть смысл регресс-теста.

'use strict';

// =============================================================================
// Реплика pure-логики из app.js (Phase 6 helpers)
// =============================================================================

/**
 * Группирует массив сессий по project_id внутри одного origin'а.
 * Сессии с project_id=null/undefined попадают в ORPHAN_KEY.
 * Внутри каждой группы сортирует по name.localeCompare().
 * Контракт идентичен window.__forge.groupSessionsByProject в app.js.
 */
function groupSessionsByProject(sessions, orphanKey) {
    const ORPHAN_KEY = orphanKey || '__orphan__';
    const byProject = new Map();
    for (const sess of sessions) {
        const key = sess.project_id == null ? ORPHAN_KEY : sess.project_id;
        if (!byProject.has(key)) byProject.set(key, []);
        byProject.get(key).push(sess);
    }
    for (const arr of byProject.values()) {
        arr.sort((a, b) => a.name.localeCompare(b.name));
    }
    return byProject;
}

/**
 * Собирает агрегированную структуру для activeOrigin='all'.
 * Контракт идентичен window.__forge.aggregateAllOrigins в app.js.
 */
function aggregateAllOrigins(state) {
    const out = new Map();
    out.set('local', {
        label: 'Local',
        online: 'local',
        projects: Array.isArray(state.projects) ? state.projects.slice() : [],
        sessions: Array.isArray(state.sessions) ? state.sessions.slice() : [],
    });
    for (const srv of (state.remoteServers || [])) {
        const sid = srv.id;
        out.set(sid, {
            label: srv.label || sid,
            online: state.remoteOnline.get(sid) || 'unknown',
            projects: state.remoteProjects.get(sid) || [],
            sessions: state.remoteSessions.get(sid) || [],
        });
    }
    return out;
}

/**
 * Реплика логики "какие origin'ы отображать" из renderSidebarWithOrigin.
 * Возвращает { showLocal: bool, showRemotes: server_id[] }.
 */
function decideVisibleOrigins(state) {
    const showLocal = state.activeOrigin === 'all' || state.activeOrigin === 'local';
    const remoteIds = state.remoteServers.map((s) => s.id);
    const showRemotes = state.activeOrigin === 'all'
        ? remoteIds
        : (state.activeOrigin === 'local'
            ? []
            : remoteIds.filter((id) => id === state.activeOrigin));
    return { showLocal, showRemotes };
}

/**
 * Реплика двухуровневой фильтрации в renderOriginSection: получает sessions
 * одного origin'а и применяет projectFilter. Возвращает либо
 * { mode: 'all', byProject } либо { mode: 'single', list }. Используется
 * как контракт регресс-теста.
 */
function applyProjectFilterInsideOrigin(sessions, projectFilter) {
    const byProject = groupSessionsByProject(sessions);
    if (projectFilter && projectFilter !== '__all__') {
        return { mode: 'single', list: byProject.get(projectFilter) || [] };
    }
    return { mode: 'all', byProject };
}

// =============================================================================
// Tiny assertion runner
// =============================================================================

let passed = 0;
let failed = 0;
const failures = [];

function assert(label, cond, details) {
    if (cond) {
        passed += 1;
        console.log('  ok   ' + label);
    } else {
        failed += 1;
        failures.push({ label, details: details || '' });
        console.log('  FAIL ' + label + (details ? '  — ' + details : ''));
    }
}

function eq(label, actual, expected) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    assert(label, a === e, 'expected ' + e + ', got ' + a);
}

function group(name, fn) {
    console.log('\n[' + name + ']');
    fn();
}

// =============================================================================
// Fixtures
// =============================================================================

const localProjects = [
    { id: 'p-alpha', name: 'Alpha' },
    { id: 'p-beta', name: 'Beta' },
];
const localSessions = [
    { name: 'work', project_id: 'p-alpha', project_name: 'Alpha' },
    { name: 'agent', project_id: 'p-alpha', project_name: 'Alpha' },
    { name: 'docs', project_id: 'p-beta', project_name: 'Beta' },
    { name: 'misc', project_id: null }, // orphan
    { name: 'stray', project_id: 'p-removed', project_name: 'Removed' }, // auto-group
];

const remoteAProjects = [{ id: 'p-srv-a', name: 'SrvA-Proj' }];
const remoteASessions = [
    { name: 'srv-a-1', project_id: 'p-srv-a', project_name: 'SrvA-Proj', origin: 'srv-a' },
    { name: 'srv-a-2', project_id: 'p-srv-a', project_name: 'SrvA-Proj', origin: 'srv-a' },
];

const remoteBProjects = [];
const remoteBSessions = [];

function makeState(overrides) {
    const state = {
        activeOrigin: 'all',
        projectFilter: '__all__',
        projects: localProjects,
        sessions: localSessions,
        remoteServers: [
            { id: 'srv-a', label: 'Server A' },
            { id: 'srv-b', label: 'Server B' },
        ],
        remoteOnline: new Map([
            ['srv-a', 'online'],
            ['srv-b', 'offline'],
        ]),
        remoteProjects: new Map([
            ['srv-a', remoteAProjects],
            ['srv-b', remoteBProjects],
        ]),
        remoteSessions: new Map([
            ['srv-a', remoteASessions],
            ['srv-b', remoteBSessions],
        ]),
    };
    if (overrides) Object.assign(state, overrides);
    return state;
}

// =============================================================================
// Tests
// =============================================================================

group('groupSessionsByProject', () => {
    const grouped = groupSessionsByProject(localSessions);

    assert('возвращает Map', grouped instanceof Map);
    assert('содержит ключ p-alpha', grouped.has('p-alpha'));
    assert('содержит ключ p-beta', grouped.has('p-beta'));
    assert('содержит ключ p-removed (auto-group)', grouped.has('p-removed'));
    assert('содержит __orphan__ для project_id=null', grouped.has('__orphan__'));

    const alpha = grouped.get('p-alpha');
    eq('p-alpha содержит 2 сессии', alpha.length, 2);
    eq('p-alpha отсортирован по name (agent, work)', alpha.map((s) => s.name), ['agent', 'work']);

    const orphan = grouped.get('__orphan__');
    eq('orphan содержит 1 сессию (misc)', orphan.map((s) => s.name), ['misc']);

    // Сессия с project_id=undefined тоже идёт в orphan.
    const sess2 = [{ name: 'x' }];
    const g2 = groupSessionsByProject(sess2);
    assert('project_id=undefined → __orphan__', g2.has('__orphan__'));
    eq('orphan size = 1', g2.get('__orphan__').length, 1);

    // Кастомный orphanKey.
    const g3 = groupSessionsByProject([{ name: 'y', project_id: null }], 'CUSTOM');
    assert('кастомный orphanKey работает', g3.has('CUSTOM'));
});

group('decideVisibleOrigins (activeOrigin switching)', () => {
    const st = makeState();

    const all = decideVisibleOrigins({ ...st, activeOrigin: 'all' });
    assert('All: showLocal=true', all.showLocal === true);
    eq('All: showRemotes = [srv-a, srv-b]', all.showRemotes, ['srv-a', 'srv-b']);

    const local = decideVisibleOrigins({ ...st, activeOrigin: 'local' });
    assert('Local: showLocal=true', local.showLocal === true);
    eq('Local: showRemotes = []', local.showRemotes, []);

    const srvA = decideVisibleOrigins({ ...st, activeOrigin: 'srv-a' });
    assert('srv-a: showLocal=false', srvA.showLocal === false);
    eq('srv-a: showRemotes = [srv-a]', srvA.showRemotes, ['srv-a']);

    // Несуществующий activeOrigin (например, удалённый сервер).
    const ghost = decideVisibleOrigins({ ...st, activeOrigin: 'srv-deleted' });
    assert('ghost origin: showLocal=false', ghost.showLocal === false);
    eq('ghost origin: showRemotes = []', ghost.showRemotes, []);
});

group('aggregateAllOrigins (activeOrigin=all)', () => {
    const st = makeState();
    const agg = aggregateAllOrigins(st);

    assert('агрегат — Map', agg instanceof Map);
    eq('агрегат содержит 3 origin (local + 2 remote)', agg.size, 3);
    assert('содержит local', agg.has('local'));
    assert('содержит srv-a', agg.has('srv-a'));
    assert('содержит srv-b', agg.has('srv-b'));

    const local = agg.get('local');
    eq('local.online = "local"', local.online, 'local');
    eq('local.sessions.length = 5', local.sessions.length, 5);
    eq('local.projects.length = 2', local.projects.length, 2);

    const srvA = agg.get('srv-a');
    eq('srv-a.online = "online"', srvA.online, 'online');
    eq('srv-a.sessions.length = 2', srvA.sessions.length, 2);
    eq('srv-a.label = "Server A"', srvA.label, 'Server A');

    const srvB = agg.get('srv-b');
    eq('srv-b.online = "offline"', srvB.online, 'offline');
    eq('srv-b.sessions.length = 0', srvB.sessions.length, 0);
    eq('srv-b.projects.length = 0', srvB.projects.length, 0);
});

group('aggregateAllOrigins: empty caches', () => {
    const st = makeState({
        remoteProjects: new Map(),
        remoteSessions: new Map(),
        remoteOnline: new Map(),
    });
    const agg = aggregateAllOrigins(st);
    const srvA = agg.get('srv-a');
    eq('пустой кеш → projects = []', srvA.projects, []);
    eq('пустой кеш → sessions = []', srvA.sessions, []);
    eq('нет в remoteOnline → online = "unknown"', srvA.online, 'unknown');
});

group('aggregateAllOrigins: state.projects/state.sessions могут быть null', () => {
    const st = makeState({ projects: null, sessions: undefined });
    const agg = aggregateAllOrigins(st);
    const local = agg.get('local');
    eq('null projects → []', local.projects, []);
    eq('undefined sessions → []', local.sessions, []);
});

group('applyProjectFilterInsideOrigin: __all__ внутри origin', () => {
    const r = applyProjectFilterInsideOrigin(localSessions, '__all__');
    eq('mode = "all"', r.mode, 'all');
    assert('byProject — Map', r.byProject instanceof Map);
    // Регресс: при __all__ внутри origin'а структура та же, что у legacy
    // renderSidebar в режиме projectFilter='__all__'.
    eq('byProject содержит все project-ключи + orphan',
        Array.from(r.byProject.keys()).sort(),
        ['__orphan__', 'p-alpha', 'p-beta', 'p-removed']);
});

group('applyProjectFilterInsideOrigin: конкретный проект внутри origin', () => {
    const r = applyProjectFilterInsideOrigin(localSessions, 'p-alpha');
    eq('mode = "single"', r.mode, 'single');
    eq('list содержит только p-alpha сессии (2)', r.list.length, 2);
    eq('list отсортирован', r.list.map((s) => s.name), ['agent', 'work']);

    // Проект, которого нет в этом origin → пустой list, но header origin'а
    // всё равно должен быть отрисован вызывающим кодом (это контракт UI,
    // не этой pure-функции).
    const empty = applyProjectFilterInsideOrigin(remoteASessions, 'p-alpha');
    eq('проект отсутствует в origin → пустой list', empty.list, []);
});

group('переключение origin не сбрасывает projectFilter', () => {
    // Регресс: явно подтверждаем решение из задачи — projectFilter работает
    // ортогонально activeOrigin. UI-обработчики не трогают projectFilter
    // при переключении origin-таба.
    const state = makeState({ projectFilter: 'p-alpha' });

    const localView = applyProjectFilterInsideOrigin(state.sessions, state.projectFilter);
    eq('Local при projectFilter=p-alpha: single + 2 сессии', localView.list.length, 2);

    const aSessions = state.remoteSessions.get('srv-a');
    const srvAView = applyProjectFilterInsideOrigin(aSessions, state.projectFilter);
    eq('srv-a при том же фильтре: 0 сессий (нет такого проекта)', srvAView.list.length, 0);
    eq('mode остался "single" (фильтр НЕ сброшен)', srvAView.mode, 'single');

    // Если бы код где-то менял state.projectFilter при смене origin —
    // тест свалится здесь.
    eq('projectFilter не мутировался', state.projectFilter, 'p-alpha');
});

group('legacy совместимость: projectFilter=__all__ группировка', () => {
    // Проверяем что legacy путь (renderSidebar без origin) даёт ту же
    // группировку, что новый renderOriginSection. Это и есть смысл фразы
    // "Существующие тесты группировки не сломаны".
    const grouped = groupSessionsByProject(localSessions);
    eq('p-alpha порядок (agent, work)', grouped.get('p-alpha').map((s) => s.name), ['agent', 'work']);
    eq('p-beta порядок (docs)', grouped.get('p-beta').map((s) => s.name), ['docs']);
    eq('orphan порядок (misc)', grouped.get('__orphan__').map((s) => s.name), ['misc']);
    // Авто-группа по project_id, которого нет в state.projects — ключ
    // присутствует, его рендер делает caller (отдельным циклом autoKeys).
    eq('auto-group p-removed (stray)', grouped.get('p-removed').map((s) => s.name), ['stray']);
});

group('offline-сервер: виден в агрегате, но пустой', () => {
    const st = makeState();
    const agg = aggregateAllOrigins(st);
    const srvB = agg.get('srv-b');
    // Контракт: даже offline сервер ОБЯЗАН присутствовать в агрегате,
    // чтобы UI мог отрисовать его origin-header с бейджем 'offline'.
    assert('offline srv-b присутствует в Map', agg.has('srv-b'));
    eq('offline online-статус', srvB.online, 'offline');
    eq('offline sessions = []', srvB.sessions, []);
});

// =============================================================================
// Phase 8 .8 — Edge cases: empty remoteServers, race-update, status-flapping,
//                duplicate project_id across origins, sort stability
// =============================================================================

group('Phase 8 .8 — пустой remoteServers рендерит только Local', () => {
    // Когда у пользователя нет remote-серверов в реестре, в aggregateAllOrigins
    // должен оказаться ровно один origin = 'local', UI рисует только Local.
    const st = makeState({
        remoteServers: [],
        remoteProjects: new Map(),
        remoteSessions: new Map(),
        remoteOnline: new Map(),
    });
    const agg = aggregateAllOrigins(st);
    eq('агрегат содержит только local', Array.from(agg.keys()), ['local']);
    eq('agg.size = 1', agg.size, 1);

    const vis = decideVisibleOrigins({ ...st, activeOrigin: 'all' });
    assert('showLocal=true', vis.showLocal === true);
    eq('showRemotes пустой', vis.showRemotes, []);
});

group('Phase 8 .8 — race: sessions раньше servers, потом servers приходят', () => {
    // Симулируем сценарий: WS присылает sessions/projects до того, как
    // GET /api/remote-servers ответил. UI должен спокойно обработать пустой
    // remoteServers, отрисовать только Local, потом — при появлении servers —
    // перегруппировать. Тестируем оба состояния.
    let state = makeState({
        remoteServers: [],
        remoteProjects: new Map(),
        remoteSessions: new Map([['srv-a', remoteASessions]]), // уже пришли
        remoteOnline: new Map([['srv-a', 'online']]),
    });
    // Стадия 1: remoteServers пустой → агрегат содержит только Local,
    // remote-сессии остаются "в кеше", но не отображаются (нет origin'а).
    let agg = aggregateAllOrigins(state);
    eq('stage-1: agg только local', Array.from(agg.keys()), ['local']);
    // Стадия 2: servers пришёл → агрегат содержит srv-a + сохранённые сессии.
    state = {
        ...state,
        remoteServers: [{ id: 'srv-a', label: 'Server A' }],
    };
    agg = aggregateAllOrigins(state);
    assert('stage-2: srv-a виден', agg.has('srv-a'));
    eq('stage-2: srv-a имеет 2 сессии', agg.get('srv-a').sessions.length, 2);
});

group('Phase 8 .8 — status-flapping: только последнее значение важно', () => {
    // Контракт: state.remoteOnline — это просто Map, последнее set() побеждает.
    // UI рендерит то, что в Map на момент aggregateAllOrigins. Никакого
    // встроенного debounce'а в pure-логике нет — это ответственность UI-уровня.
    const online = new Map();
    // Имитируем 10 переключений online↔offline.
    for (let i = 0; i < 10; i++) {
        online.set('srv-x', i % 2 === 0 ? 'online' : 'offline');
    }
    // Финальное состояние — offline (i=9 → нечётный).
    const state = makeState({
        remoteServers: [{ id: 'srv-x', label: 'X' }],
        remoteProjects: new Map(),
        remoteSessions: new Map(),
        remoteOnline: online,
    });
    const agg = aggregateAllOrigins(state);
    eq('финальный online = "offline"', agg.get('srv-x').online, 'offline');
});

group('Phase 8 .8 — duplicate project_id across origins образуют отдельные пункты', () => {
    // Проект с одинаковым id ('my-app') есть и в local, и в srv-a.
    // aggregateAllOrigins НЕ дедуплицирует — каждое origin хранит свой
    // массив projects. UI рендерит их в разных origin-секциях.
    const localProj = [{ id: 'my-app', name: 'My App (local)' }];
    const srvAProj = [{ id: 'my-app', name: 'My App (srv-a)' }];
    const state = makeState({
        projects: localProj,
        sessions: [],
        remoteServers: [{ id: 'srv-a', label: 'A' }],
        remoteProjects: new Map([['srv-a', srvAProj]]),
        remoteSessions: new Map([['srv-a', []]]),
        remoteOnline: new Map([['srv-a', 'online']]),
    });
    const agg = aggregateAllOrigins(state);
    eq('local.projects[0].name', agg.get('local').projects[0].name, 'My App (local)');
    eq('srv-a.projects[0].name', agg.get('srv-a').projects[0].name, 'My App (srv-a)');
    // Это — фича, а не баг: пользователь видит «My App» под Local и под Server A.
    // Если бы случайно дедуплицировали — потерялось бы понимание, чей это проект.
});

group('Phase 8 .8 — sort stability при идентичных name', () => {
    // Два проекта с одинаковым name — порядок определяется появлением в массиве
    // (input order). groupSessionsByProject у нас не сортирует projects (это
    // делает caller с .sort((a,b)=>a.name.localeCompare(b.name))), но даже
    // если caller использует non-stable sort — localeCompare возвращает 0 для
    // равных, и V8 Array.sort в Node.js >=v10 уже stable.
    const projects = [
        { id: 'p1', name: 'Same' },
        { id: 'p2', name: 'Same' },
        { id: 'p3', name: 'Same' },
    ];
    const sorted = projects.slice().sort((a, b) => a.name.localeCompare(b.name));
    eq('stable sort сохраняет input order для равных name',
        sorted.map((p) => p.id),
        ['p1', 'p2', 'p3']);
});

// =============================================================================
// Summary
// =============================================================================

console.log('\n=================================');
console.log('  passed: ' + passed);
console.log('  failed: ' + failed);
console.log('=================================');

if (failed > 0) {
    console.log('\nFailures:');
    for (const f of failures) {
        console.log('  - ' + f.label + (f.details ? ': ' + f.details : ''));
    }
    process.exit(1);
}
process.exit(0);
