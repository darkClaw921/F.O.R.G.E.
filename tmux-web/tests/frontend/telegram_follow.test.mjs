// Регресс-тест «следования» за сообщением бота — ЧИСТАЯ логика.
//
// Грузит РЕАЛЬНЫЙ static/js/telegram/follow.js через await import(): модуль
// намеренно без DOM и без сети (тот же приём, что у telegram_reply.test.mjs),
// поэтому решения «следовать ли», «докуда скроллить» и «не отменил ли
// пользователь следование прокруткой» проверяются здесь без всякого стенда.
//
// UI-половина (доскролл в onEdited/onNewMessage, жизненный цикл ResizeObserver,
// prefers-reduced-motion) живёт в соседнем telegram_scroll_ui.test.mjs.
//
// Запуск: node tmux-web/tests/frontend/telegram_follow.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const base = new URL('../../static/js/telegram/', import.meta.url).href;
const F = await import(`${base}follow.js`);

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
// 1. Константы контракта
// ---------------------------------------------------------------------------

ok(F.FOLLOW_TTL_MS >= 10000 && F.FOLLOW_TTL_MS <= 15000,
    'TTL: окно следования в требуемых 10-15с');
ok(F.FOLLOW_SCROLL_PAD >= 0, 'зазор: не отрицательный');
ok(F.FOLLOW_CANCEL_UP_PX > 0, 'порог отмены: строго положительный (иначе гасило бы дребезгом)');

// ---------------------------------------------------------------------------
// 2. Регистрация и срок жизни
// ---------------------------------------------------------------------------

F.clearFollow();
eq(F.followTarget(T0), null, 'старт: без нажатия следования нет');
eq(F.isFollowing('777000', T0), false, 'старт: за диалогом не следим');
eq(F.followsMessage('777000', 4, T0), false, 'старт: за сообщением не следим');

eq(F.noteFollow('777000', 4, T0), true, 'нажатие: следование включилось');
eq(F.followTarget(T0), { dialogId: '777000', messageId: 4 }, 'нажатие: цель — диалог и сообщение');
eq(F.isFollowing('777000', T0), true, 'нажатие: следим за диалогом');
eq(F.followsMessage('777000', 4, T0), true, 'нажатие: следим за нажатым сообщением');

// Числовой id диалога приходит и строкой, и числом (WS против REST).
eq(F.isFollowing(777000, T0), true, 'нажатие: id диалога сверяется строкой, а не типом');

// Границы окна.
eq(F.followTarget(T0 + F.FOLLOW_TTL_MS - 1), { dialogId: '777000', messageId: 4 },
    'срок: за миллисекунду до конца следование ещё живо');
eq(F.followTarget(T0 + F.FOLLOW_TTL_MS), { dialogId: '777000', messageId: 4 },
    'срок: ровно на границе следование ещё живо');
eq(F.followTarget(T0 + F.FOLLOW_TTL_MS + 1), null, 'срок: за границей следования нет');
eq(F.isFollowing('777000', T0 + F.FOLLOW_TTL_MS + 1), false, 'срок: истёкшее не тянет ленту');

// Часы, прыгнувшие НАЗАД (коррекция NTP, ноутбук проснулся), следование не рвут:
// тот же выбор, что у format.js::toastAllowed.
eq(F.followTarget(T0 - 60_000), { dialogId: '777000', messageId: 4 },
    'часы: скачок назад следование не отменяет');

// ---------------------------------------------------------------------------
// 3. Адресность: чужой диалог и чужое сообщение
// ---------------------------------------------------------------------------

eq(F.isFollowing('42', T0), false, 'адресность: за чужим диалогом не следим');
eq(F.followsMessage('42', 4, T0), false,
    'адресность: тот же message_id в ЧУЖОМ диалоге ленту не дёргает');
eq(F.followsMessage('777000', 5, T0), false, 'адресность: чужое сообщение своего диалога');
eq(F.isFollowing(null, T0), false, 'адресность: пустой диалог — не следим');
eq(F.isFollowing(undefined, T0), false, 'адресность: undefined-диалог — не следим');

// ---------------------------------------------------------------------------
// 4. Перецеливание, продление и отмена
// ---------------------------------------------------------------------------

F.noteFollow('777000', 9, T0 + 5000);
eq(F.followTarget(T0 + 5000), { dialogId: '777000', messageId: 9 },
    'пачка нажатий: актуально последнее');
eq(F.followsMessage('777000', 4, T0 + 5000), false, 'пачка нажатий: прошлая цель снята');
eq(F.followTarget(T0 + F.FOLLOW_TTL_MS + 1000), { dialogId: '777000', messageId: 9 },
    'пачка нажатий: окно продлено от последнего нажатия');

eq(F.clearFollow(), true, 'отмена: вернула «следование было»');
eq(F.followTarget(T0 + 5000), null, 'отмена: цели больше нет');
eq(F.clearFollow(), false, 'отмена: идемпотентна');

// Кнопка reply-клавиатуры принадлежит ЧАТУ, а не сообщению: messageId == 0.
F.noteFollow('777000', 0, T0);
eq(F.followTarget(T0), { dialogId: '777000', messageId: 0 },
    'reply-клавиатура: следование за диалогом целиком');
eq(F.followsMessage('777000', 4, T0), true, 'reply-клавиатура: подходит любое сообщение чата');
eq(F.followsMessage('777000', 99, T0), true, 'reply-клавиатура: и другое тоже');
eq(F.followsMessage('42', 4, T0), false, 'reply-клавиатура: но только своего чата');

// Кнопка без диалога — не повод ни включать следование, ни ронять уже живое.
eq(F.noteFollow('', 4, T0), false, 'нажатие: без диалога следование не включается');
eq(F.followTarget(T0), { dialogId: '777000', messageId: 0 },
    'нажатие: неудачная регистрация не портит уже живое следование');
F.clearFollow();

// ---------------------------------------------------------------------------
// 5. Решение «скроллить ли»
// ---------------------------------------------------------------------------

eq(F.scrollDecision({ atBottom: false, following: false }), 'none',
    'решение: чужая правка при чтении истории ленту не трогает');
eq(F.scrollDecision({ atBottom: true, following: false }), 'stick',
    'решение: у нижней кромки работает прежнее прилипание');
eq(F.scrollDecision({ atBottom: false, following: true }), 'follow',
    'решение: после своего нажатия уезжаем вниз даже отлистав вверх');
eq(F.scrollDecision({ atBottom: true, following: true }), 'follow',
    'решение: следование сильнее прилипания (оно умеет в конец блока)');
eq(F.scrollDecision(null), 'none', 'решение: без контекста ленту не трогаем');

// ---------------------------------------------------------------------------
// 6. Куда скроллить
// ---------------------------------------------------------------------------

const log = { scrollTop: 100, scrollHeight: 1000, clientHeight: 400 };

// Строка целиком выше нижней кромки — двигать нечего.
eq(F.scrollTargetForRow(log, { top: 10, bottom: 120 }, 8), null,
    'цель: видимая строка ленту не двигает');
// Конец строки уехал под кромку на 60px ⇒ доскролл на 60 + зазор.
eq(F.scrollTargetForRow(log, { top: 300, bottom: 460 }, 8), 168,
    'цель: конец блока подтягивается под кромку с зазором');
eq(F.scrollTargetForRow(log, { top: 300, bottom: 460 }, 0), 160,
    'цель: нулевой зазор — ровно к кромке');
// Потолок прокрутки: ниже scrollHeight - clientHeight уехать нельзя.
eq(F.scrollTargetForRow(log, { top: 700, bottom: 5000 }, 8), 600,
    'цель: клампится потолком прокрутки (scrollHeight - clientHeight)');
// Вверх не тянем НИКОГДА — иначе следование перебивало бы переход по цитате.
eq(F.scrollTargetForRow(log, { top: -900, bottom: -800 }, 8), null,
    'цель: строку выше экрана следование не догоняет (не спорит с переходом по цитате)');
// Раскладка неизвестна (нет getBoundingClientRect) — честный фолбэк в самый низ.
eq(F.scrollTargetForRow(log, null, 8), 600, 'цель: без раскладки — потолок прокрутки');
eq(F.scrollTargetForRow({ scrollTop: 600, scrollHeight: 1000, clientHeight: 400 }, null, 8), null,
    'цель: уже у потолка — двигать нечего');
eq(F.scrollTargetForRow(null, { top: 0, bottom: 900 }, 8), null, 'цель: без ленты — null');
// Лента короче экрана (переписка на два сообщения): потолок 0, доскролла нет.
eq(F.scrollTargetForRow({ scrollTop: 0, scrollHeight: 200, clientHeight: 400 }, null, 8), null,
    'цель: лента короче экрана — прокручивать нечего');

// ---------------------------------------------------------------------------
// 7. Отмена ручной прокруткой
// ---------------------------------------------------------------------------

eq(F.cancelledByScroll(500, 500), false, 'прокрутка: без движения не отменяет');
eq(F.cancelledByScroll(500, 900), false, 'прокрутка: ВНИЗ не отменяет — это наш же доскролл');
eq(F.cancelledByScroll(500, 500 - F.FOLLOW_CANCEL_UP_PX), false,
    'прокрутка: ровно порог вверх — ещё дребезг, не отмена');
eq(F.cancelledByScroll(500, 500 - F.FOLLOW_CANCEL_UP_PX - 1), true,
    'прокрутка: вверх сверх порога — пользователь ушёл читать историю');
eq(F.cancelledByScroll(500, 0), true, 'прокрутка: в самый верх — отмена');
eq(F.cancelledByScroll(500, 480, 5), true, 'прокрутка: порог настраивается');

// ---------------------------------------------------------------------------
// 8. prefers-reduced-motion
// ---------------------------------------------------------------------------

eq(F.scrollBehavior(false), 'smooth', 'движение: по умолчанию плавно');
eq(F.scrollBehavior(true), 'auto', 'движение: reduce ⇒ мгновенно');
eq(F.scrollBehavior(undefined), 'smooth', 'движение: неизвестно ⇒ как по умолчанию');

// ---------------------------------------------------------------------------
// 9. Снимок для диагностики
// ---------------------------------------------------------------------------

F.clearFollow();
eq(F._debugFollowState(), { dialogId: null, messageId: null, untilMs: 0 },
    'снимок: пустое состояние');
F.noteFollow('777000', 4, T0);
eq(F._debugFollowState(), { dialogId: '777000', messageId: 4, untilMs: T0 + F.FOLLOW_TTL_MS },
    'снимок: цель и точный срок');
F.clearFollow();

console.log(`\n${checks - failures}/${checks} ассертов прошло (frontend/telegram_follow)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
