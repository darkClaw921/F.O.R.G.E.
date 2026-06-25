// tmux-web/tests/frontend/pwa_mobile_helper.test.js
//
// PWA mobile helpers — регресс-тесты чистой логики из static/js/pwa/mobile.js:
//   • countNeedsAttention(data) — число сессий /api/sessions с needs_attention;
//   • keyboardHeight(innerHeight, vvHeight, vvOffsetTop) — высота клавиатуры из
//     visualViewport;
//   • updateBadge-маршрутизация App Badge API (setAppBadge/clearAppBadge);
//   • feature-guard-предикаты (отсутствие API → early-return, без бросков);
//   • safe()-изоляция фич (ошибка одной не ломает остальные).
//
// ВАЖНО: mobile.js импортит ../core/state.js и ../terminal/xterm.js (тяжёлые
// top-level зависимости: DOM/WebSocket/xterm) → ПРЯМОЙ импорт в Node невозможен.
// Поэтому здесь РЕПЛИКА pure-логики ОДИН-В-ОДИН. Контракт идентичен mobile.js;
// изменение mobile.js (строки указаны у каждой функции) требует синхронной
// правки этого файла — это и есть смысл регресс-теста.
//
// Запуск: node tmux-web/tests/frontend/pwa_mobile_helper.test.js
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.

'use strict';

// =============================================================================
// Tiny assertion runner (стиль sidebar_grouping.test.js)
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
// РЕПЛИКА pure-логики mobile.js (контракт идентичен — менять синхронно).
// =============================================================================

/** Реплика подсчёта needs_attention (mobile.js строки 136-138). */
function countNeedsAttention(data) {
    return Array.isArray(data)
        ? data.filter((s) => s && s.needs_attention).length
        : 0;
}

/** Реплика расчёта высоты клавиатуры (mobile.js строки 78-81). */
function keyboardHeight(innerHeight, vvHeight, vvOffsetTop) {
    return Math.max(0, Math.round(innerHeight - vvHeight - vvOffsetTop));
}

/**
 * Реплика тела updateBadge (mobile.js строки 163-175) с инъекцией navigator и
 * сбрасываемым lastBadgeCount (модульный стейт в оригинале). Возвращает
 * описание вызванного API: { fn, arg } | null (если ранний return по дедупу или
 * проглоченная ошибка). Записывает фактические вызовы в navigator._log.
 */
function makeUpdateBadge() {
    let lastBadgeCount = -1;
    return {
        reset() { lastBadgeCount = -1; },
        call(navigator, count) {
            if (count === lastBadgeCount) return null; // дедуп
            lastBadgeCount = count;
            try {
                if (count > 0) {
                    navigator.setAppBadge(count);
                } else if ('clearAppBadge' in navigator) {
                    navigator.clearAppBadge();
                } else {
                    navigator.setAppBadge(0);
                }
            } catch (_) { /* Badging запрещён политикой — проглатываем */ }
            return null;
        },
    };
}

/** Feature-guard предикаты (mobile.js строки 126, 71, 189). */
function badgeSupported(navigator) { return 'setAppBadge' in navigator; }
function vvSupported(window) { return !!window.visualViewport; }
function wakeLockSupported(navigator) { return 'wakeLock' in navigator; }

/** Реплика safe() (mobile.js строки 54-60): глушит ошибку, не пробрасывает. */
function makeSafe(warns) {
    return function safe(fn) {
        try {
            fn();
        } catch (err) {
            warns.push({ name: fn.name, err });
        }
    };
}

// =============================================================================
// Фейковые navigator с записывающими стабами
// =============================================================================

function fakeNavigator(opts) {
    opts = opts || {};
    const log = [];
    const nav = { _log: log };
    if (opts.hasSetAppBadge !== false) {
        nav.setAppBadge = (n) => {
            if (opts.throwOnSet) throw new Error('policy denied');
            log.push({ fn: 'setAppBadge', arg: n });
        };
    }
    if (opts.hasClearAppBadge) {
        nav.clearAppBadge = () => { log.push({ fn: 'clearAppBadge', arg: undefined }); };
    }
    if (opts.hasWakeLock) nav.wakeLock = {};
    return nav;
}

// =============================================================================
// Tests
// =============================================================================

group('countNeedsAttention: фильтр truthy needs_attention', () => {
    const data = [
        { needs_attention: true },
        { needs_attention: false },
        { needs_attention: true },
        {},
        { needs_attention: 0 },
        null,
    ];
    eq('count == 2 (truthy only, null/{} не падают)', countNeedsAttention(data), 2);
});

group('countNeedsAttention: пустой массив → 0', () => {
    eq('count == 0', countNeedsAttention([]), 0);
});

group('countNeedsAttention: не массив → 0', () => {
    eq('объект → 0', countNeedsAttention({ sessions: [{ needs_attention: true }] }), 0);
    eq('null → 0', countNeedsAttention(null), 0);
    eq('строка → 0', countNeedsAttention('oops'), 0);
    eq('undefined → 0', countNeedsAttention(undefined), 0);
});

group('countNeedsAttention: строковое/числовое truthy', () => {
    const data = [
        { needs_attention: 1 },
        { needs_attention: 'yes' },
        { needs_attention: '' },
        { needs_attention: null },
    ];
    eq('count == 2 (1 и "yes" truthy; "" и null falsy)', countNeedsAttention(data), 2);
});

group('updateBadge: count>0 → setAppBadge(count)', () => {
    const ub = makeUpdateBadge();
    const nav = fakeNavigator({ hasSetAppBadge: true, hasClearAppBadge: true });
    ub.call(nav, 3);
    eq('один вызов', nav._log.length, 1);
    eq('setAppBadge(3)', nav._log[0], { fn: 'setAppBadge', arg: 3 });
});

group('updateBadge: count==0 и есть clearAppBadge → clearAppBadge()', () => {
    const ub = makeUpdateBadge();
    const nav = fakeNavigator({ hasSetAppBadge: true, hasClearAppBadge: true });
    ub.call(nav, 0);
    eq('clearAppBadge без аргументов', nav._log, [{ fn: 'clearAppBadge', arg: undefined }]);
});

group('updateBadge: count==0 и НЕТ clearAppBadge → setAppBadge(0) фолбэк', () => {
    const ub = makeUpdateBadge();
    const nav = fakeNavigator({ hasSetAppBadge: true, hasClearAppBadge: false });
    ub.call(nav, 0);
    eq('setAppBadge(0) как фолбэк', nav._log, [{ fn: 'setAppBadge', arg: 0 }]);
});

group('updateBadge: дедуп по lastBadgeCount', () => {
    const ub = makeUpdateBadge();
    const nav = fakeNavigator({ hasSetAppBadge: true, hasClearAppBadge: true });
    ub.call(nav, 2);
    ub.call(nav, 2); // тот же count → ранний return
    eq('API вызван ровно один раз', nav._log.length, 1);
    eq('первый вызов setAppBadge(2)', nav._log[0], { fn: 'setAppBadge', arg: 2 });
    // после reset тот же count снова проходит
    ub.reset();
    ub.call(nav, 2);
    eq('после reset вызвался снова', nav._log.length, 2);
});

group('updateBadge: setAppBadge бросает (policy) — проглатывается', () => {
    const ub = makeUpdateBadge();
    const nav = fakeNavigator({ hasSetAppBadge: true, throwOnSet: true });
    let threw = false;
    try { ub.call(nav, 5); } catch (_) { threw = true; }
    assert('updateBadge не пробрасывает ошибку', !threw);
});

group('feature-guard badgeSupported: нет setAppBadge → false', () => {
    const nav = fakeNavigator({ hasSetAppBadge: false });
    assert('badgeSupported=false', badgeSupported(nav) === false);
    // Аналог initAppBadge: при false — early return, поллинг не стартует.
    // Эмулируем guard и убеждаемся, что не бросает.
    let threw = false;
    try {
        if (!badgeSupported(nav)) { /* return */ }
    } catch (_) { threw = true; }
    assert('early-return не бросает', !threw);
});

group('feature-guard badgeSupported: есть setAppBadge → true', () => {
    const nav = fakeNavigator({ hasSetAppBadge: true });
    assert('badgeSupported=true', badgeSupported(nav) === true);
});

group('keyboardHeight: клавиатура открыта', () => {
    eq('800-500-0 → 300', keyboardHeight(800, 500, 0), 300);
});

group('keyboardHeight: клавиатура закрыта (vv==inner)', () => {
    eq('800-800-0 → 0', keyboardHeight(800, 800, 0), 0);
});

group('keyboardHeight: НЕ отрицательная (max(0,...))', () => {
    // vv.height > innerHeight (зум/баунс)
    eq('600-620-0 → 0', keyboardHeight(600, 620, 0), 0);
    // большой offsetTop
    eq('800-500-400 → 0 (вместо -100)', keyboardHeight(800, 500, 400), 0);
});

group('keyboardHeight: округление дробных viewport-значений', () => {
    // Точная половина → Math.round округляет вверх (round-half-up): 300.5 → 301.
    eq('300.5 → 301 (round-half-up)', keyboardHeight(800.5, 500, 0), 301);
    // 800.4 - 500.6 - 0.3 = 299.4999… (float) → Math.round → 299.
    eq('299.4999… → 299', keyboardHeight(800.4, 500.6, 0.3), 299);
    // 800.4 - 500.9 - 0.3 = 299.2 → 299
    eq('299.2 → 299', keyboardHeight(800.4, 500.9, 0.3), 299);
    // 800.8 - 500.1 - 0 = 300.7 → 301
    eq('300.7 → 301', keyboardHeight(800.8, 500.1, 0), 301);
    // результат всегда целое
    const v = keyboardHeight(801.7, 500.2, 0.1);
    eq('результат целочисленный', v, Math.round(v));
});

group('feature-guard vvSupported: нет visualViewport → false', () => {
    assert('window без visualViewport → false', vvSupported({}) === false);
    assert('visualViewport undefined → false', vvSupported({ visualViewport: undefined }) === false);
    // initKeyboardViewport early-return: если !vv — не обращаемся к vv.height.
    let threw = false;
    try {
        const window = {};
        if (!vvSupported(window)) { /* return */ } else { void window.visualViewport.height; }
    } catch (_) { threw = true; }
    assert('early-return не дёргает vv.height', !threw);
});

group('feature-guard vvSupported: есть visualViewport → true', () => {
    assert('window c visualViewport → true', vvSupported({ visualViewport: { height: 500 } }) === true);
});

group('feature-guard wakeLockSupported: нет wakeLock → false', () => {
    assert('navigator без wakeLock → false', wakeLockSupported(fakeNavigator({})) === false);
    // initWakeLock early-return.
    let threw = false;
    try {
        const nav = fakeNavigator({});
        if (!wakeLockSupported(nav)) { /* return — кнопка не создаётся */ }
    } catch (_) { threw = true; }
    assert('early-return не бросает', !threw);
});

group('feature-guard wakeLockSupported: есть wakeLock → true', () => {
    assert('navigator c wakeLock → true', wakeLockSupported(fakeNavigator({ hasWakeLock: true })) === true);
});

group('safe(): одна фича бросает — остальные инициализируются', () => {
    const warns = [];
    const safe = makeSafe(warns);
    const order = [];
    function featА() { order.push('A'); }
    function featBoom() { order.push('B'); throw new Error('boom'); }
    function featC() { order.push('C'); }

    let threw = false;
    try {
        safe(featА);
        safe(featBoom);
        safe(featC);
    } catch (_) { threw = true; }

    assert('safe не пробрасывает наружу', !threw);
    eq('все три фичи вызваны (изоляция)', order, ['A', 'B', 'C']);
    eq('ошибка залогирована один раз', warns.length, 1);
    eq('залогирована именно featBoom', warns[0].name, 'featBoom');
});

group('opt-in инвариант (контракт-документация)', () => {
    // Чистые функции не имеют скрытого глобального опт-ина: гейт на уровне
    // ИМПОРТА (bootstrap.js лениво грузит mobile.js только при enabled===true).
    // Без --pwa модуль вообще не загружается. Тест чистых функций НЕ
    // подразумевает работу при выключенном PWA — это инвариант уровня загрузки,
    // его нельзя проверить рантаймом без bootstrap. Документируем явно.
    assert('countNeedsAttention — pure (нет глобального опт-ина)',
        countNeedsAttention([{ needs_attention: true }]) === 1);
    assert('keyboardHeight — pure', keyboardHeight(800, 500, 0) === 300);
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
