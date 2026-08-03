// Регресс-тест общего слоя «следования» — ФАБРИКА createFollow.
//
// Грузит РЕАЛЬНЫЙ static/js/messenger/follow-core.js через await import():
// модуль намеренно без DOM, без сети и без часов (`nowMs` инжектируется), так
// что весь его контракт проверяется без всякого стенда.
//
// ГЛАВНАЯ ПРОВЕРКА ФАЙЛА — независимость инстансов. Именно ради неё модуль и
// существует в виде фабрики: общий модульный синглтон на две панели дал бы
// коллизию ключей (`peer_id` беседы VK `2000000001` и `dialog_id` супергруппы
// Telegram `2000000001` — это РАЗНЫЕ чаты, а сравниваются они строкой), и
// нажатие кнопки в одной панели тянуло бы ленту другой.
//
// МУТАЦИОННАЯ ПРОВЕРКА: если вернуть состояние в модульную переменную (общее
// на все инстансы), раздел 1 обязан покраснеть.
//
// UI-половина (доскролл, ResizeObserver, prefers-reduced-motion) живёт в
// vk_scroll_ui.test.mjs / vk_scroll_edge.test.mjs.
//
// Запуск: node tmux-web/tests/frontend/messenger_follow_factory.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const base = new URL('../../static/js/messenger/', import.meta.url).href;
const F = await import(`${base}follow-core.js`);

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

// Инъекция часов: реальный Date.now() сделал бы тест зависимым от скорости
// машины, а окно следования измеряется секундами.
const T0 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// 0. Контракт модуля
// ---------------------------------------------------------------------------

eq(typeof F.createFollow, 'function', 'наружу отдана ФАБРИКА, а не готовый объект');
ok(F.FOLLOW_TTL_MS >= 10000 && F.FOLLOW_TTL_MS <= 15000,
    'TTL: окно следования в требуемых 10-15с');
ok(F.FOLLOW_SCROLL_PAD >= 0, 'зазор: не отрицательный');
ok(F.FOLLOW_CANCEL_UP_PX > 0, 'порог отмены: строго положительный (иначе гасило бы дребезгом)');

{
    const api = F.createFollow();
    const expected = [
        'noteFollow', 'clearFollow', 'followTarget', 'isFollowing', 'followsMessage',
        'scrollDecision', 'scrollTargetForRow', 'cancelledByScroll', 'scrollBehavior', '_debug',
    ];
    for (const name of expected) {
        eq(typeof api[name], 'function', `инстанс отдаёт ${name}()`);
    }
    // Ни одной экспортированной функции состояния на уровне модуля: она
    // означала бы общий стейт «для удобства», то есть ровно тот дефект,
    // против которого написан раздел 1.
    eq(typeof F.noteFollow, 'undefined', 'модуль НЕ экспортирует noteFollow напрямую');
    eq(typeof F.followTarget, 'undefined', 'модуль НЕ экспортирует followTarget напрямую');
}

// ---------------------------------------------------------------------------
// 1. НЕЗАВИСИМОСТЬ ИНСТАНСОВ (ключевая проверка файла)
// ---------------------------------------------------------------------------

{
    const vk = F.createFollow();
    const tg = F.createFollow();

    // Совпадающий id — это и есть та самая коллизия: 2000000001 у VK беседа,
    // у Telegram супергруппа, и обе панели живут в одной вкладке.
    vk.noteFollow('2000000001', 514, T0);

    eq(vk.isFollowing('2000000001', T0), true, 'инстанс VK следит за своей беседой');
    eq(tg.isFollowing('2000000001', T0), false,
        'инстанс Telegram НЕ подхватил чужое следование по совпадающему id');
    eq(tg.followTarget(T0), null, 'у второго инстанса цели нет вовсе');
    eq(tg._debug().dialogId, null, 'состояние второго инстанса пусто');

    // Обратное направление: собственное следование Telegram не перебивает VK.
    tg.noteFollow('2000000001', 77, T0);
    eq(vk.followsMessage('2000000001', 514, T0), true, 'VK по-прежнему следит за cmid 514');
    eq(vk.followsMessage('2000000001', 77, T0), false, 'и не подхватил чужое сообщение');
    eq(tg.followsMessage('2000000001', 77, T0), true, 'Telegram следит за своим message_id');
    eq(tg.followsMessage('2000000001', 514, T0), false, 'и не видит чужого');

    // Снятие в одном инстансе не гасит другой — это второй способ, которым
    // общий синглтон ломал бы панели друг о друга.
    eq(vk.clearFollow(), true, 'VK снял своё следование');
    eq(vk.followTarget(T0), null, 'у VK цели больше нет');
    eq(tg.isFollowing('2000000001', T0), true, 'следование Telegram снятие ПЕРЕЖИЛО');

    // Третий инстанс, созданный позже, тоже стартует чистым.
    const third = F.createFollow();
    eq(third.followTarget(T0), null, 'новый инстанс стартует без цели');
    eq(third.isFollowing('2000000001', T0), false, 'и не видит уже существующих');

    // Настройки инстансов не общие: у VK окно короче, и оно обязано истечь
    // раньше, не тронув Telegram.
    const short = F.createFollow({ ttlMs: 1000 });
    short.noteFollow('999', 1, T0);
    eq(short.followTarget(T0 + 1500), null, 'короткое окно истекло по своему ttlMs');
    eq(tg.isFollowing('2000000001', T0 + 1500), true, 'у соседа своё окно, оно ещё живо');
}

// ---------------------------------------------------------------------------
// 2. Регистрация и срок жизни
// ---------------------------------------------------------------------------

{
    const f = F.createFollow();
    eq(f.followTarget(T0), null, 'старт: без нажатия следования нет');
    eq(f.isFollowing('2000000001', T0), false, 'старт: за беседой не следим');
    eq(f.followsMessage('2000000001', 4, T0), false, 'старт: за сообщением не следим');

    eq(f.noteFollow('2000000001', 514, T0), true, 'нажатие: следование включилось');
    eq(f.followTarget(T0), { dialogId: '2000000001', messageId: 514 }, 'цель зафиксирована');
    eq(f.followTarget(T0 + F.FOLLOW_TTL_MS - 1), { dialogId: '2000000001', messageId: 514 },
        'внутри окна цель жива');
    eq(f.followTarget(T0 + F.FOLLOW_TTL_MS), { dialogId: '2000000001', messageId: 514 },
        'ровно на границе окна ещё следим (строгое >)');
    eq(f.followTarget(T0 + F.FOLLOW_TTL_MS + 1), null, 'за границей окна цели нет');

    // Часы, прыгнувшие назад, следование не рвут: «ещё не истекло».
    eq(f.followTarget(T0 - 60_000), { dialogId: '2000000001', messageId: 514 },
        'скачок часов назад следование не гасит');

    // Числовой id приводится к строке — панель отдаёт его как угодно.
    f.noteFollow(2000000001, 514, T0);
    eq(f.isFollowing('2000000001', T0), true, 'числовой id и строковый — один диалог');
    eq(f.noteFollow('', 1, T0), false, 'пустой диалог следование не включает');
    eq(f.noteFollow(null, 1, T0), false, 'null тоже');
    // Неудачное включение НЕ затирает прежнюю цель.
    eq(f.isFollowing('2000000001', T0), true, 'после отказа прежняя цель на месте');

    // Перецеливание и продление пачкой нажатий.
    f.noteFollow('2000000001', 514, T0);
    f.noteFollow('2000000001', 515, T0 + 5000);
    eq(f.followTarget(T0 + 5000).messageId, 515, 'актуально ПОСЛЕДНЕЕ нажатие');
    eq(f.followTarget(T0 + F.FOLLOW_TTL_MS + 1000) !== null, true,
        'окно продлилось от момента последнего нажатия');

    // messageId == 0 — следим за беседой целиком (кнопка reply-клавиатуры).
    f.noteFollow('2000000001', 0, T0);
    eq(f.followsMessage('2000000001', 999, T0), true, 'нулевая цель = любое сообщение беседы');
    f.noteFollow('2000000001', -5, T0);
    eq(f.followTarget(T0).messageId, 0, 'отрицательный id вырождается в «беседа целиком»');

    eq(f.clearFollow(), true, 'снятие сообщает, что цель была');
    eq(f.clearFollow(), false, 'повторное снятие идемпотентно');
    eq(f.followTarget(T0), null, 'после снятия цели нет');
}

// ---------------------------------------------------------------------------
// 3. Адресность
// ---------------------------------------------------------------------------

{
    const f = F.createFollow();
    f.noteFollow('2000000001', 514, T0);
    eq(f.isFollowing('-55', T0), false, 'чужая беседа — не следим');
    eq(f.followsMessage('-55', 514, T0), false,
        'ТОТ ЖЕ cmid в другой беседе — не наше сообщение (у VK cmid нумеруется в каждой беседе с нуля)');
    eq(f.followsMessage('2000000001', 513, T0), false, 'соседнее сообщение — не наше');
    eq(f.followsMessage('2000000001', 514, T0 + F.FOLLOW_TTL_MS + 1), false,
        'истёкшее окно адресность отменяет');
    eq(f.isFollowing(null, T0), false, 'null-диалог не совпадает ни с чем');
}

// ---------------------------------------------------------------------------
// 4. scrollDecision — все ветки
// ---------------------------------------------------------------------------

{
    const f = F.createFollow();
    eq(f.scrollDecision({ atBottom: true, following: true }), 'follow',
        'следование сильнее прилипания');
    eq(f.scrollDecision({ atBottom: false, following: true }), 'follow',
        'следование работает и при отлистанной вверх ленте — в этом весь смысл');
    eq(f.scrollDecision({ atBottom: true, following: false }), 'stick', 'прежнее прилипание');
    eq(f.scrollDecision({ atBottom: false, following: false }), 'none',
        'читающего историю не дёргаем');
    eq(f.scrollDecision(null), 'none', 'без контекста ленту не трогаем');
    eq(f.scrollDecision({}), 'none', 'пустой контекст — тоже');
    // Только строгие true: undefined из неинициализированного поля не должен
    // читаться как «да».
    eq(f.scrollDecision({ atBottom: 1, following: 1 }), 'none', 'truthy-но-не-true не считается');
}

// ---------------------------------------------------------------------------
// 5. scrollTargetForRow — зазор, потолок, «вверх не тянем»
// ---------------------------------------------------------------------------

{
    const f = F.createFollow();
    const log = { scrollTop: 100, scrollHeight: 1000, clientHeight: 300 };

    // Конец строки на 50px ниже кромки: цель = 100 + (350-300) + 8.
    eq(f.scrollTargetForRow(log, { top: 200, bottom: 350 }), 158, 'доскролл с зазором');
    eq(f.scrollTargetForRow(log, { top: 200, bottom: 350 }, 0), 150, 'зазор переопределяется');
    // Потолок: дальше `scrollHeight - clientHeight` уехать нельзя.
    eq(f.scrollTargetForRow(log, { top: 900, bottom: 5000 }), 700, 'цель зажата потолком');
    // Строка целиком видна — двигать нечего.
    eq(f.scrollTargetForRow(log, { top: 10, bottom: 200 }), null, 'видимую строку не догоняем');
    // Строка ВЫШЕ видимой области: вверх не тянем никогда.
    eq(f.scrollTargetForRow(log, { top: -500, bottom: -300 }), null,
        'вверх не тянем — иначе следование перебивало бы переход по цитате');
    // Раскладка неизвестна ⇒ цель самый низ.
    eq(f.scrollTargetForRow(log, null), 700, 'без координат строки — в самый низ');
    eq(f.scrollTargetForRow({ scrollTop: 700, scrollHeight: 1000, clientHeight: 300 }, null), null,
        'уже внизу — двигать нечего');
    // Лента короче экрана: потолок нулевой.
    eq(f.scrollTargetForRow({ scrollTop: 0, scrollHeight: 100, clientHeight: 300 },
        { top: 0, bottom: 90 }), null, 'лента короче экрана — прокрутки нет');
    eq(f.scrollTargetForRow(null, { top: 0, bottom: 10 }), null, 'без ленты решения нет');

    // Собственный зазор инстанса действует, а дефолтный — нет.
    const wide = F.createFollow({ scrollPad: 40 });
    eq(wide.scrollTargetForRow(log, { top: 200, bottom: 350 }), 190, 'зазор инстанса применён');
}

// ---------------------------------------------------------------------------
// 6. cancelledByScroll — по направлению, с порогом
// ---------------------------------------------------------------------------

{
    const f = F.createFollow();
    eq(f.cancelledByScroll(500, 400), true, 'ушли вверх на 100px — отмена');
    eq(f.cancelledByScroll(500, 500 - F.FOLLOW_CANCEL_UP_PX), false,
        'ровно на пороге — ещё не отмена (строгое >)');
    eq(f.cancelledByScroll(500, 500 - F.FOLLOW_CANCEL_UP_PX - 1), true, 'на пиксель дальше — отмена');
    eq(f.cancelledByScroll(500, 900), false, 'вниз — это наш собственный доскролл');
    eq(f.cancelledByScroll(500, 500), false, 'без движения отмены нет');
    eq(f.cancelledByScroll(undefined, 400), false, 'мусорный prev не даёт ложной отмены');
    eq(f.cancelledByScroll(500, 400, 1000), false, 'порог переопределяется');

    const twitchy = F.createFollow({ cancelUpPx: 2 });
    eq(twitchy.cancelledByScroll(500, 497), true, 'свой порог инстанса действует');
}

// ---------------------------------------------------------------------------
// 7. scrollBehavior и _debug
// ---------------------------------------------------------------------------

{
    const f = F.createFollow();
    eq(f.scrollBehavior(true), 'auto', 'prefers-reduced-motion ⇒ без анимации');
    eq(f.scrollBehavior(false), 'smooth', 'обычно — плавно');
    eq(f.scrollBehavior(undefined), 'smooth', 'неизвестно ⇒ как обычно');

    f.noteFollow('2000000001', 514, T0);
    eq(f._debug(), { dialogId: '2000000001', messageId: 514, untilMs: T0 + F.FOLLOW_TTL_MS, ttlMs: F.FOLLOW_TTL_MS },
        'снимок инстанса для диагностики');
    f.clearFollow();
    eq(f._debug().dialogId, null, 'после снятия снимок пуст');
}

// ---------------------------------------------------------------------------
// 8. Мусор в настройках фабрики
// ---------------------------------------------------------------------------

{
    // Ноль и отрицательный ttl означали бы следование, истекающее до первого
    // кадра, — молча заменяются дефолтом.
    for (const bad of [0, -1, NaN, '12000', null, undefined]) {
        const f = F.createFollow({ ttlMs: bad });
        f.noteFollow('1', 1, T0);
        eq(f._debug().untilMs, T0 + F.FOLLOW_TTL_MS, `ttlMs=${String(bad)} ⇒ дефолт`);
    }
    // Нулевой зазор законен: «вплотную к кромке» — осознанный выбор панели.
    const f = F.createFollow({ scrollPad: 0 });
    eq(f.scrollTargetForRow({ scrollTop: 100, scrollHeight: 1000, clientHeight: 300 },
        { top: 200, bottom: 350 }), 150, 'scrollPad: 0 — законное значение, а не мусор');
    eq(F.createFollow(null).noteFollow('1', 1, T0), true, 'фабрика без аргументов работает');
}

console.log(`messenger_follow_factory: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
