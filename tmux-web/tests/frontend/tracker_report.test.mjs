// Регрессионный тест экрана сводки трекера
// (`static/js/tracker/render-report.js`).
//
// Проверяется ТОЛЬКО то, что не трогает DOM: окно периода, символьные полосы,
// подписи KPI, признак пустого периода и CSV. Отрисовка, переключение периода
// кнопками и сама загрузка файла проверяются живым гейтом в браузере
// (Ф9 ГЕЙТ).
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. окно недели начинается с ПОНЕДЕЛЬНИКА и полуоткрыто `[from, to)` — та
//      же граница, что у `WEEK` в статус-баре и у окна по умолчанию роута;
//   2. ключ дня строится из ЛОКАЛЬНЫХ полей даты: через `new Date("2026-09-01")`
//      день уехал бы на сутки назад западнее Гринвича;
//   3. полоса `// BY DAY` абсолютная (знак = 15 мин), полоса `// BY LIST` —
//      относительная (самая долгая строка занимает всю ширину);
//   4. KPI ТОЛЬКО ПЕРЕПИСЫВАЮТСЯ из ответа сервера, ничего не пересчитывая:
//      подмена `avg_day_seconds` обязана появиться на экране как есть;
//   5. пустой период — отдельное состояние, а не нули;
//   6. CSV содержит ровно те же числа, что экран, и разделён `;`.
//
// Запуск: node tmux-web/tests/frontend/tracker_report.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    BAR_UNIT, BAR_WIDTH, LIST_BAR_WIDTH, PERIODS,
    dayBar, dayKey, dayRowLabel, dateOfKey, fmtDelta, fmtHm, isEmptyReport,
    kpiCards, pct, reportToCsv, shareBar, shiftAnchor, windowFor, windowLabel,
    csvFileName,
} from '../../static/js/tracker/render-report.js';

/** Вторник 08.09.2026. */
const TUE = new Date(2026, 8, 8, 15, 30, 0);

// ------------------------------------------------------------------ окно

test('окно недели: понедельник — понедельник, полуоткрытое', () => {
    const w = windowFor('week', TUE);
    assert.equal(dayKey(w.from), '2026-09-07', 'понедельник той же недели');
    assert.equal(dayKey(w.to), '2026-09-14', 'граница не включается');
    assert.equal(windowLabel(w), 'week 07.09 — 13.09');
});

test('окно дня и месяца', () => {
    const d = windowFor('day', TUE);
    assert.equal(dayKey(d.from), '2026-09-08');
    assert.equal(dayKey(d.to), '2026-09-09');
    assert.equal(windowLabel(d), 'day 08.09');

    const m = windowFor('month', TUE);
    assert.equal(dayKey(m.from), '2026-09-01');
    assert.equal(dayKey(m.to), '2026-10-01');
    assert.equal(windowLabel(m), 'month 01.09 — 30.09');
});

test('сдвиг периода идёт встык и не зависит от длины соседних месяцев', () => {
    const week = windowFor('week', TUE);
    const prev = windowFor('week', shiftAnchor('week', week.from, -1));
    assert.equal(dayKey(prev.from), '2026-08-31');
    assert.equal(dayKey(prev.to), dayKey(week.from), 'окна встык, без нахлёста');

    const month = windowFor('month', TUE);
    const back = windowFor('month', shiftAnchor('month', month.from, -1));
    assert.equal(dayKey(back.from), '2026-08-01');
    assert.equal(dayKey(back.to), '2026-09-01');
});

test('ключ дня локальный: обратный разбор даёт тот же день', () => {
    assert.equal(dayKey(dateOfKey('2026-09-01')), '2026-09-01');
    assert.equal(dateOfKey('не дата'), null);
    assert.equal(dayRowLabel('2026-09-01'), 'вт 01.09');
    assert.equal(dayRowLabel('2026-09-07'), 'пн 07.09');
});

test('список периодов — ровно три, как в макете', () => {
    assert.deepEqual(PERIODS, ['day', 'week', 'month']);
});

// ----------------------------------------------------------------- полосы

test('полоса дня: знак = 15 минут, ширина 16 знаков', () => {
    assert.equal(BAR_UNIT, 900);
    assert.equal(BAR_WIDTH, 16);

    // 3:08 из макета = 188 минут ≈ 12,5 единиц → 13 знаков.
    const bar = dayBar(3 * 3600 + 8 * 60);
    assert.equal(bar.filled, 13);
    assert.equal(bar.text.length, BAR_WIDTH);
    assert.equal(bar.text, '█'.repeat(13) + '░'.repeat(3));
    assert.equal(bar.over, false);

    assert.equal(dayBar(0).filled, 0);
    assert.equal(dayBar(0).text, '░'.repeat(BAR_WIDTH));
});

test('полоса дня упирается в потолок и говорит об этом, а не врёт', () => {
    const bar = dayBar(9 * 3600);
    assert.equal(bar.filled, BAR_WIDTH);
    assert.equal(bar.text.length, BAR_WIDTH);
    assert.equal(bar.over, true, '9 часов больше четырёх часов полосы');
});

test('полоса списка относительная: самая долгая строка — во всю ширину', () => {
    const max = 10 * 3600 + 2 * 60;
    assert.equal(shareBar(max, max).length, LIST_BAR_WIDTH);
    assert.equal(shareBar(5 * 3600 + 47 * 60, max).length, 7);
    assert.equal(shareBar(1 * 3600 + 44 * 60, max).length, 2);
    assert.equal(shareBar(0, max), '', 'нулевое время полосы не рисует');
    assert.equal(shareBar(60, max).length, 1, 'ненулевое время — минимум знак');
    assert.equal(shareBar(60, 0), '', 'без максимума полосы нет');
});

test('проценты и формат Ч:ММ', () => {
    assert.equal(pct(3600, 7200), 50);
    assert.equal(pct(1, 0), 0);
    assert.equal(fmtHm(21 * 3600 + 47 * 60 + 59), '21:47');
    assert.equal(fmtHm(0), '0:00');
    assert.equal(fmtDelta(3 * 3600 + 12 * 60), '+3:12');
    assert.equal(fmtDelta(-40 * 60), '−0:40');
    assert.equal(fmtDelta(0), '±0:00');
});

// -------------------------------------------------------------------- KPI

const KPI = {
    total_seconds: 21 * 3600 + 47 * 60,
    days: 7,
    avg_day_seconds: 3 * 3600 + 7 * 60,
    prev_total_seconds: 18 * 3600 + 35 * 60,
    closed_tasks: 9,
    touched_tasks: 17,
    max_focus_seconds: 2 * 3600 + 14 * 60,
    max_focus_task_id: 't1',
    max_focus_day: '2026-09-08',
};

const VIEW = {
    ...windowFor('week', TUE),
    byDay: {
        total_seconds: KPI.total_seconds,
        rows: [
            { key: '2026-09-07', label: '2026-09-07', seconds: 3 * 3600 + 8 * 60 },
            { key: '2026-09-08', label: '2026-09-08', seconds: 2 * 3600 + 12 * 60 },
        ],
    },
    byList: {
        total_seconds: 5 * 3600 + 20 * 60,
        rows: [
            { key: 'l1', label: 'work', seconds: 4 * 3600 },
            { key: '', label: 'без списка', seconds: 1 * 3600 + 20 * 60 },
        ],
    },
    byTask: {
        total_seconds: 5 * 3600 + 20 * 60,
        rows: [{ key: 't1', label: 'Свести дизайн трекера', seconds: 5 * 3600 + 12 * 60 }],
    },
    kpi: KPI,
    todayKey: '2026-09-08',
    tasksById: new Map([['t1', { id: 't1', title: 'Свести дизайн трекера', tags: ['forge'] }]]),
    listsById: new Map([['l1', { id: 'l1', name: 'work' }]]),
};

test('KPI переписываются из ответа сервера, без своей арифметики', () => {
    const cards = kpiCards(KPI, VIEW);
    assert.deepEqual(cards.map((c) => c.label), ['// TOTAL', '// AVG/DAY', '// CLOSED', '// MAX FOCUS']);
    assert.equal(cards[0].value, '21:47');
    assert.equal(cards[0].hint, '+3:12 к прошлой');
    assert.equal(cards[1].value, '3:07');
    assert.equal(cards[2].value, '9/17');
    assert.equal(cards[3].value, '2:14');
    assert.equal(cards[3].hint, 'вт · #forge', 'день недели и тег задачи фокуса');

    // Экран НЕ пересчитывает среднее: подменённое сервером число попадает на
    // карточку как есть — иначе на фронте завелась бы вторая редакция правил.
    const odd = kpiCards({ ...KPI, avg_day_seconds: 60 }, VIEW);
    assert.equal(odd[1].value, '0:01');
});

test('без прошлого периода подпись TOTAL говорит про длину окна', () => {
    const cards = kpiCards({ ...KPI, prev_total_seconds: null }, VIEW);
    assert.equal(cards[0].hint, '7 дн. в периоде');
});

test('пустой период — отдельное состояние', () => {
    assert.equal(isEmptyReport(VIEW), false);
    assert.equal(isEmptyReport({ byDay: { rows: [], total_seconds: 0 } }), true);
});

// -------------------------------------------------------------------- CSV

test('CSV повторяет экран: те же строки, ; как разделитель', () => {
    const csv = reportToCsv(VIEW);
    const lines = csv.split('\r\n');
    assert.equal(lines[0], '"Раздел";"Ключ";"Название";"Время";"Минуты";"Доля %"');
    assert.ok(lines[1].includes('week 07.09 — 13.09'), 'период подписан');
    assert.ok(csv.includes('"KPI";"total";"TOTAL (+3:12 к прошлой)";"21:47"'));
    assert.ok(csv.includes('"KPI";"closed";"CLOSED (задач за неделю)";"9/17"'));

    // Строка дня: то же время, что на экране, плюс минуты числом.
    assert.ok(csv.includes('"По дням";"2026-09-07";"пн 07.09";"3:08:00";"188"'));
    // Доля списка считается от итога СВОЕГО разреза.
    assert.ok(csv.includes('"По спискам";"l1";"work";"4:00:00";"240";"75"'));
    assert.ok(csv.includes('"По задачам";"t1";"Свести дизайн трекера";"5:12:00";"312";"98"'));
});

test('CSV экранирует кавычки в названии и имя файла несёт период', () => {
    const view = {
        ...VIEW,
        byTask: { total_seconds: 60, rows: [{ key: 't2', label: 'Задача "в кавычках"', seconds: 60 }] },
    };
    assert.ok(reportToCsv(view).includes('"Задача ""в кавычках"""'));
    assert.equal(csvFileName(VIEW), 'tracker-report-2026-09-07_2026-09-13.csv');
});
