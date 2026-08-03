// Регресс-тест клиентского планировщика запросов VK (js/vk/ratelimit.js).
//
// Модуль чистый: время, планировщик отложенных вызовов и фабрика
// AbortController инжектятся, поэтому весь тест идёт на ВИРТУАЛЬНЫХ часах —
// он не спит ни миллисекунды и не зависит от загрузки машины.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Планировщик решает не ту задачу, что серверный
// троттлер: он не «укладывается в лимит» (это делает Bucket на бэкенде), а НЕ
// СТАВИТ В ОЧЕРЕДЬ ЗАПРОСЫ, КОТОРЫЕ УЖЕ НЕАКТУАЛЬНЫ. Проверяется ровно это:
//
//   1. не больше BURST запросов в скользящую секунду;
//   2. пользовательский приоритет обгоняет фоновый;
//   3. коалесcинг по ключу ОТМЕНЯЕТ предыдущий запрос (проверяется вызов
//      abort, а не «мы про него забыли»);
//   4. dropStale отбрасывает ответ покинутой беседы;
//   5. при поднятом оверлее captcha фоновые запросы заморожены — иначе они
//      породят вторую captcha.
//
// Запуск: node tmux-web/tests/frontend/vk_ratelimit.test.mjs

const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

/** Даёт микрозадачам добежать (промисы резолвятся не синхронно). */
const flush = () => new Promise((r) => setTimeout(r, 0));

/**
 * Виртуальные часы + планировщик отложенных вызовов.
 *
 * `advance(ms)` двигает время и выполняет всё, чему подошёл срок, — в порядке
 * срабатывания, как это сделал бы браузер.
 */
function clock() {
    let t = 1000;
    const timers = [];
    return {
        now: () => t,
        schedule(fn, ms) {
            const at = t + Math.max(0, Number(ms) || 0);
            const id = { at, fn };
            timers.push(id);
            return id;
        },
        async advance(ms) {
            const until = t + ms;
            for (;;) {
                timers.sort((a, b) => a.at - b.at);
                const next = timers[0];
                if (!next || next.at > until) break;
                timers.shift();
                t = next.at;
                next.fn();
                await flush();
            }
            t = until;
            await flush();
        },
        pending: () => timers.length,
    };
}

// ---------------------------------------------------------------------------
// 1. Token bucket: залп BURST, дальше — по одному за интервал.
// ---------------------------------------------------------------------------

{
    const c = clock();
    const lim = R.createLimiter({ minIntervalMs: 400, burst: 3, now: c.now, schedule: c.schedule });
    const done = [];
    for (let i = 0; i < 6; i += 1) {
        lim.acquire('user').then(() => done.push(i));
    }
    await flush();
    eq(done, [0, 1, 2], 'залп ровно BURST — открытие вкладки честно стоит трёх запросов');
    eq(lim.size(), 3, 'остальные ждут');

    await c.advance(400);
    eq(done, [0, 1, 2, 3], 'через интервал — ещё один');
    await c.advance(400);
    eq(done, [0, 1, 2, 3, 4], 'и ещё один');
    await c.advance(400);
    eq(done, [0, 1, 2, 3, 4, 5], 'очередь разошлась');
    eq(lim.size(), 0, 'очередь пуста');
}

// Скользящая секунда: за любую секунду выпускается не больше 3 запросов сверх
// стартового залпа. Это и есть смысл лимита 3 rps.
{
    const c = clock();
    const lim = R.createLimiter({ minIntervalMs: 400, burst: 3, now: c.now, schedule: c.schedule });
    const at = [];
    for (let i = 0; i < 12; i += 1) {
        lim.acquire('user').then(() => at.push(c.now()));
    }
    await flush();
    await c.advance(4000);
    eq(at.length, 12, 'все двенадцать в итоге выпущены');
    // Считаем самое плотное окно в одну секунду ПОСЛЕ стартового залпа.
    const tail = at.slice(3);
    let worst = 0;
    for (let i = 0; i < tail.length; i += 1) {
        const inWindow = tail.filter((x) => x >= tail[i] && x < tail[i] + 1000).length;
        if (inWindow > worst) worst = inWindow;
    }
    ok(worst <= 3, `в скользящую секунду не больше 3 запросов (худшее окно: ${worst})`);
}

// ---------------------------------------------------------------------------
// 2. Приоритеты: пользователь обгоняет фон, даже встав в очередь позже.
// ---------------------------------------------------------------------------

{
    const c = clock();
    const lim = R.createLimiter({ minIntervalMs: 400, burst: 1, now: c.now, schedule: c.schedule });
    const order = [];
    lim.acquire('user').then(() => order.push('first-user'));   // съедает единственный токен
    await flush();
    lim.acquire('bg').then(() => order.push('bg'));             // встал раньше
    lim.acquire('user').then(() => order.push('user'));         // но пропускаем этого
    await flush();
    eq(order, ['first-user'], 'токенов больше нет — оба ждут');

    await c.advance(400);
    eq(order, ['first-user', 'user'], 'ПОЛЬЗОВАТЕЛЬСКИЙ обгоняет фоновый, вставший раньше');
    await c.advance(400);
    eq(order, ['first-user', 'user', 'bg'], 'фоновый доезжает следом');
}

// ---------------------------------------------------------------------------
// 3. Заморозка фона на время captcha.
// ---------------------------------------------------------------------------

{
    const c = clock();
    let captchaOpen = true;
    const lim = R.createLimiter({
        minIntervalMs: 400,
        burst: 3,
        now: c.now,
        schedule: c.schedule,
        frozen: () => captchaOpen,
    });
    const done = [];
    lim.acquire('bg').then(() => done.push('bg'));
    lim.acquire('user').then(() => done.push('user'));
    await flush();
    eq(done, ['user'],
        'при открытом оверлее captcha фон заморожен — иначе он породит вторую captcha');

    await c.advance(2000);
    eq(done, ['user'], 'заморозка не снимается сама по времени');

    captchaOpen = false;
    lim.pump();
    await flush();
    eq(done, ['user', 'bg'], 'после закрытия captcha фон выпущен');
}

// ---------------------------------------------------------------------------
// 4. Коалесcинг по ключу: предыдущий запрос ОТМЕНЯЕТСЯ.
// ---------------------------------------------------------------------------

/** Фейковый AbortController: считает вызовы abort. */
function fakeControllers() {
    const made = [];
    return {
        made,
        make() {
            const c = {
                aborted: false,
                signal: { aborted: false },
                abort() {
                    this.aborted = true;
                    this.signal.aborted = true;
                },
            };
            made.push(c);
            return c;
        },
    };
}

{
    const f = fakeControllers();
    const co = R.createCoalescer({ controller: f.make });
    const seen = [];
    let release1 = null;

    const p1 = co.submit('history:1234', (signal) => new Promise((res) => {
        seen.push(signal);
        release1 = res;
    }));
    eq(co.size(), 1, 'запрос в полёте');
    eq(f.made[0].aborted, false, 'первый ещё живой');

    // Пользователь переключился на ту же беседу ещё раз (двойной клик).
    const p2 = co.submit('history:1234', () => Promise.resolve('второй'));
    ok(f.made[0].aborted, 'ПРЕДЫДУЩИЙ ЗАПРОС ОТМЕНЁН, а не просто забыт');
    eq(f.made.length, 2, 'у нового запроса свой контроллер');
    ok(seen[0] === f.made[0].signal, 'fn получает signal — его обязан передать в fetch');

    release1('первый');
    eq(await p1, 'первый', 'отменённый промис всё равно завершается — данные могли доехать');
    eq(await p2, 'второй', 'актуальный запрос доехал');
    await flush();
    eq(co.size(), 0, 'карта полёта очищена');
}

// Разные ключи друг друга не трогают: две беседы грузятся параллельно законно.
{
    const f = fakeControllers();
    const co = R.createCoalescer({ controller: f.make });
    co.submit('history:1', () => new Promise(() => {}));
    co.submit('history:2', () => new Promise(() => {}));
    eq(co.size(), 2, 'два ключа — два запроса');
    eq(f.made.filter((c) => c.aborted).length, 0, 'разные ключи не отменяют друг друга');
    eq(co.abortAll(), 2, 'abortAll отменяет всё (смена аккаунта, logout)');
    eq(co.size(), 0, 'после abortAll карта пуста');
    eq(f.made.filter((c) => c.aborted).length, 2, 'оба контроллера отменены');
}

// Завершившийся запрос не должен отменять СВЕЖИЙ с тем же ключом.
{
    const f = fakeControllers();
    const co = R.createCoalescer({ controller: f.make });
    let release = null;
    const slow = co.submit('k', () => new Promise((res) => { release = res; }));
    const fresh = co.submit('k', () => new Promise(() => {}));
    release('поздний ответ');
    await slow;
    await flush();
    eq(co.size(), 1, 'свежий запрос уцелел: старый снял ТОЛЬКО свой контроллер');
    eq(f.made[1].aborted, false, 'свежий не отменён завершением старого');
    void fresh;
}

// Отсутствие AbortController в окружении не должно ронять модуль.
{
    const co = R.createCoalescer({ controller: () => null });
    eq(await co.submit('k', (signal) => Promise.resolve(signal === undefined)), true,
        'без AbortController signal просто undefined');
    eq(co.abort('k'), false, 'отменять нечего');
}

// ---------------------------------------------------------------------------
// 5. Генерации: ответ покинутой беседы выбрасывается.
// ---------------------------------------------------------------------------

eq(R.dropStale(1, 1), false, 'то же поколение — ответ актуален');
eq(R.dropStale(1, 2), true, 'поколение сменилось — ответ выбросить');
eq(R.dropStale(undefined, 1), true, 'нет поколения — считаем устаревшим');
eq(R.dropStale(1, NaN), true, 'мусор — считаем устаревшим');

{
    const g = R.createGeneration();
    const first = g.bump();
    eq(g.current(), first, 'текущее поколение');
    eq(g.stale(first), false, 'свой ответ актуален');
    const second = g.bump();
    ok(second !== first, 'новое поколение отличается');
    eq(g.stale(first), true, 'ответ прошлой беседы устарел');
    eq(g.stale(second), false, 'ответ текущей — нет');
}

// Сквозной сценарий фазы: десять быстрых переключений бесед оставляют ровно
// одну актуальную загрузку.
{
    const f = fakeControllers();
    const co = R.createCoalescer({ controller: f.make });
    const g = R.createGeneration();
    const applied = [];

    for (let i = 0; i < 10; i += 1) {
        const gen = g.bump();
        // Все десять «уходят в сеть», но применить результат вправе только
        // последний — и по коалесcингу, и по поколению.
        co.submit('history', () => Promise.resolve(i)).then((v) => {
            if (!g.stale(gen)) applied.push(v);
        });
    }
    await flush();
    eq(applied, [9], 'быстрое прощёлкивание десяти бесед оставляет ОДНУ актуальную загрузку');
    eq(f.made.filter((c) => c.aborted).length, 9, 'девять устаревших запросов отменены');
}

// ---------------------------------------------------------------------------
// Константы под лимит VK.
// ---------------------------------------------------------------------------

eq(R.BURST, 3, 'залп равен лимиту VK (3 rps)');
ok(R.MIN_INTERVAL_MS >= 334,
    'интервал не меньше 1000/3 — иначе получим код 6 от VK на ровном месте');
ok(R.MIN_INTERVAL_MS <= 500, 'и не настолько больше, чтобы панель ощущалась медленной');

console.log(`vk_ratelimit: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
