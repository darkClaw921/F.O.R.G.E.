// tmux-web/tests/frontend/home_day_grouping.test.js
//
// Регресс-тесты группировки «Недавних сессий» по дням (home.js).
//
// Что покрываем:
//   1. dayKeyFromUnix(sec) — Unix-СЕКУНДЫ → YYYY-MM-DD в локальной зоне.
//   2. groupByDay(records) — группировка по дню СОЗДАНИЯ (first_seen):
//      - записи одного дня попадают в одну группу;
//      - группы отсортированы по dayKey убыв. (свежие дни сверху);
//      - внутри группы записи отсортированы по first_seen убыв.
//   3. humanDayLabel(dayKey) — «Сегодня»/«Вчера»/человекочитаемая дата.
//   4. Инициализация сворачивания — по умолчанию раскрыт только сегодняшний
//      день, остальные свёрнуты; ручной выбор переживает перерендер.
//
// Запуск: node tmux-web/tests/frontend/home_day_grouping.test.js
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.
//
// ВАЖНО: тест реплицирует pure-логику из home.js (не подгружает home.js
// напрямую — тот завязан на DOM/ESM-импорты apiFetch/openSession). Любое
// изменение контракта dayKeyFromUnix / groupByDay / humanDayLabel / логики
// инициализации dayCollapsed должно одновременно обновлять и этот файл.

'use strict';

// =============================================================================
// Реплика pure-логики из home.js
// =============================================================================

function dayKeyFromUnix(sec) {
    const d = new Date((Number(sec) || 0) * 1000);
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    return `${y}-${m}-${day}`;
}

function todayKey() {
    return dayKeyFromUnix(Date.now() / 1000);
}

function yesterdayKey() {
    const d = new Date();
    d.setDate(d.getDate() - 1);
    return dayKeyFromUnix(d.getTime() / 1000);
}

function humanDayLabel(dayKey) {
    if (dayKey === todayKey()) return 'Сегодня';
    if (dayKey === yesterdayKey()) return 'Вчера';
    const [y, m, d] = dayKey.split('-').map(Number);
    const dt = new Date(y, m - 1, d);
    try {
        const s = dt.toLocaleDateString('ru-RU', {
            day: 'numeric', month: 'long', year: 'numeric', weekday: 'long',
        });
        const m2 = s.match(/^([^,]+),\s*(.+)$/);
        return m2 ? `${m2[2]}, ${m2[1]}` : s;
    } catch {
        return dayKey;
    }
}

function groupByDay(records) {
    const byDay = new Map();
    for (const rec of records) {
        const key = dayKeyFromUnix(rec.first_seen);
        let bucket = byDay.get(key);
        if (!bucket) {
            bucket = [];
            byDay.set(key, bucket);
        }
        bucket.push(rec);
    }
    const groups = Array.from(byDay.entries()).map(([dayKey, recs]) => {
        recs.sort((a, b) => (Number(b.first_seen) || 0) - (Number(a.first_seen) || 0));
        return { dayKey, recs };
    });
    groups.sort((a, b) => (a.dayKey < b.dayKey ? 1 : a.dayKey > b.dayKey ? -1 : 0));
    return groups;
}

// Реплика инициализации состояния сворачивания из renderHome.
function initCollapse(dayCollapsed, groups, today) {
    const validDays = new Set(groups.map((g) => g.dayKey));
    for (const { dayKey } of groups) {
        if (!dayCollapsed.has(dayKey)) {
            dayCollapsed.set(dayKey, dayKey !== today);
        }
    }
    for (const dayKey of dayCollapsed.keys()) {
        if (!validDays.has(dayKey)) dayCollapsed.delete(dayKey);
    }
    return dayCollapsed;
}

// =============================================================================
// Мини-фреймворк ассертов (стиль sidebar_grouping.test.js)
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

// Unix-секунды для локального полудня заданной календарной даты (устойчиво к TZ:
// полдень не перескакивает через границу суток даже при UTC-сдвигах).
function noonUnix(y, m, d) {
    return Math.floor(new Date(y, m - 1, d, 12, 0, 0).getTime() / 1000);
}

// =============================================================================
// Тесты
// =============================================================================

group('dayKeyFromUnix', () => {
    eq('полдень 2026-07-21 → 2026-07-21', dayKeyFromUnix(noonUnix(2026, 7, 21)), '2026-07-21');
    eq('полдень 2026-01-05 (паддинг) → 2026-01-05', dayKeyFromUnix(noonUnix(2026, 1, 5)), '2026-01-05');
    eq('0/NaN → epoch-день (год 1970)', dayKeyFromUnix(0).startsWith('19'), true);
});

group('groupByDay: разбиение по first_seen', () => {
    const recs = [
        { name: 'echo', first_seen: noonUnix(2026, 7, 21) },   // сегодняшний в примере
        { name: 'forge', first_seen: noonUnix(2026, 7, 20) },
        { name: 'docs', first_seen: noonUnix(2026, 7, 20) },
        { name: 'old', first_seen: noonUnix(2026, 7, 19) },
    ];
    const groups = groupByDay(recs);

    eq('три группы-дня', groups.length, 3);
    eq('порядок дней убыв.', groups.map((g) => g.dayKey), ['2026-07-21', '2026-07-20', '2026-07-19']);
    eq('в дне 21 — одна сессия', groups[0].recs.map((r) => r.name), ['echo']);
    eq('в дне 20 — две сессии', groups[1].recs.length, 2);
    eq('в дне 19 — одна сессия', groups[2].recs.map((r) => r.name), ['old']);
});

group('groupByDay: сортировка внутри группы по first_seen убыв.', () => {
    const morning = noonUnix(2026, 7, 20) - 3 * 3600; // 09:00
    const evening = noonUnix(2026, 7, 20) + 6 * 3600;  // 18:00
    const recs = [
        { name: 'morning', first_seen: morning },
        { name: 'evening', first_seen: evening },
    ];
    const groups = groupByDay(recs);
    eq('одна группа', groups.length, 1);
    eq('свежесозданная (evening) — первой', groups[0].recs.map((r) => r.name), ['evening', 'morning']);
});

group('humanDayLabel', () => {
    assert('сегодня → «Сегодня»', humanDayLabel(todayKey()) === 'Сегодня');
    assert('вчера → «Вчера»', humanDayLabel(yesterdayKey()) === 'Вчера');
    const label = humanDayLabel('2026-07-19');
    assert('произвольная дата содержит «июля»', label.includes('июля'), label);
    assert('произвольная дата содержит год', label.includes('2026'), label);
});

group('инициализация сворачивания', () => {
    const today = '2026-07-21';
    const groups = [
        { dayKey: '2026-07-21', recs: [] },
        { dayKey: '2026-07-20', recs: [] },
        { dayKey: '2026-07-19', recs: [] },
    ];
    const dc = initCollapse(new Map(), groups, today);
    eq('сегодня раскрыт (collapsed=false)', dc.get('2026-07-21'), false);
    eq('вчера свёрнут', dc.get('2026-07-20'), true);
    eq('позавчера свёрнут', dc.get('2026-07-19'), true);

    // Ручной выбор переживает повторную инициализацию (перерендер/poll).
    dc.set('2026-07-20', false); // пользователь развернул вчера
    initCollapse(dc, groups, today);
    eq('ручное разворачивание вчера сохранилось', dc.get('2026-07-20'), false);

    // День, исчезнувший из истории, вычищается.
    const shrunk = [{ dayKey: '2026-07-21', recs: [] }];
    initCollapse(dc, shrunk, today);
    eq('исчезнувший день удалён из состояния', dc.has('2026-07-20'), false);
    eq('оставшийся день на месте', dc.has('2026-07-21'), true);
});

// =============================================================================
// Итог
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
