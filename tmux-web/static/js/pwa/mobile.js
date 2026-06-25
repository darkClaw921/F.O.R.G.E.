/*
 * mobile.js — мобильные улучшения PWA devforge (Фаза 5).
 *
 * Лениво импортируется из bootstrap.js ТОЛЬКО при enabled === true (opt-in).
 * Каждая фича feature-detected и обёрнута в try/catch — отсутствие API на
 * платформе не должно ломать остальные фичи или страницу.
 *
 * КРИТИЧНЫЕ (обязательны):
 *   1. safe-area — основные правила в css/pwa.css; JS добавляет body-класс
 *      .pwa-active, чтобы эти правила применялись только при enabled.
 *   2. visualViewport keyboard — при сжатии вьюпорта экранной клавиатурой
 *      выставляем CSS-var --pwa-keyboard-height и пересчитываем xterm
 *      (state.fitAddon.fit() + scheduleResizeFromTerm()), чтобы ввод в
 *      терминал работал над клавиатурой.
 *   3. overscroll — overscroll-behavior:contain на #terminal/#tasks-board
 *      (правила в css/pwa.css), блокирует pull-to-refresh (иначе случайный
 *      reload рвёт WebSocket).
 *
 * NICE-TO-HAVE:
 *   • App Badge — navigator.setAppBadge(count), count = число сессий с
 *     needs_attention (лёгкий поллинг /api/sessions);
 *   • Screen Wake Lock — тоггл «Не гасить экран», re-acquire на visibilitychange;
 *   • online/offline баннер — navigator.onLine + события online/offline,
 *     синхронизируется со status-dot.
 *   • vibrate — уже реализован в sw.js (push-уведомления), здесь НЕ дублируется.
 */

'use strict';

import { state } from '../core/state.js';
import { scheduleResizeFromTerm } from '../terminal/xterm.js';

/**
 * Точка входа. Вызывается из bootstrap.js. Инициализирует все мобильные фичи;
 * каждая — в своём try/catch. Идемпотентна.
 */
export function initMobile() {
    if (window.__FORGE_PWA_MOBILE_INIT) return;
    window.__FORGE_PWA_MOBILE_INIT = true;

    // Маркер: правила safe-area/overscroll из css/pwa.css применяются только
    // когда на <body> есть .pwa-active (т.е. PWA включено) — строгий opt-in.
    try {
        document.body.classList.add('pwa-active');
    } catch (_) { /* нет body — невозможно, но безопасно */ }

    safe(initKeyboardViewport);
    safe(initAppBadge);
    safe(initWakeLock);
    safe(initOnlineBanner);
}

/** Обёртка: запускает фичу, глуша любые ошибки (изоляция фич). */
function safe(fn) {
    try {
        fn();
    } catch (err) {
        console.warn('[pwa] mobile feature init failed:', fn.name, err);
    }
}

// ───────────────── 1. visualViewport: экранная клавиатура ──────────────

/**
 * Слушает visualViewport resize/scroll. При появлении экранной клавиатуры
 * (visualViewport.height < layout height) выставляет --pwa-keyboard-height и
 * пересчитывает раскладку xterm, чтобы строка ввода была видна над клавиатурой.
 */
function initKeyboardViewport() {
    const vv = window.visualViewport;
    if (!vv) return; // нет API (десктоп/старый браузер) — фича недоступна.

    let raf = 0;
    const apply = () => {
        raf = 0;
        // Разница между layout-viewport и visual-viewport ≈ высота клавиатуры
        // (плюс offsetTop при прокрутке вьюпорта).
        const keyboard = Math.max(
            0,
            Math.round(window.innerHeight - vv.height - vv.offsetTop),
        );
        const root = document.documentElement;
        if (keyboard > 0) {
            root.style.setProperty('--pwa-keyboard-height', keyboard + 'px');
            document.body.classList.add('pwa-keyboard-open');
        } else {
            root.style.setProperty('--pwa-keyboard-height', '0px');
            document.body.classList.remove('pwa-keyboard-open');
        }
        refitTerminal();
    };

    const schedule = () => {
        if (raf) return;
        raf = requestAnimationFrame(apply);
    };

    vv.addEventListener('resize', schedule);
    vv.addEventListener('scroll', schedule);
    // Первичный расчёт.
    apply();
}

/** Пересчитывает раскладку xterm под новый размер вьюпорта. */
function refitTerminal() {
    try {
        if (state.fitAddon) state.fitAddon.fit();
    } catch (_) { /* контейнер 0×0 / xterm не готов */ }
    try {
        if (state.term) scheduleResizeFromTerm();
    } catch (_) { /* нет WS / терминала */ }
}

// ─────────────────────────── 2. App Badge ──────────────────────────────

let badgePollTimer = 0;
let lastBadgeCount = -1;

/**
 * App Badge API: число на иконке установленного приложения = количество сессий
 * с needs_attention. Источник — /api/sessions (тот же, что у сайдбара). Лёгкий
 * самостоятельный поллинг (раз в 5с), чтобы не завязываться на app-модули.
 * Останавливается, когда вкладка скрыта (экономия), возобновляется на показе.
 */
function initAppBadge() {
    if (!('setAppBadge' in navigator)) return; // нет Badging API.

    const poll = async () => {
        try {
            const resp = await fetch('/api/sessions', {
                headers: { 'Accept': 'application/json' },
                credentials: 'same-origin',
            });
            if (!resp.ok) return;
            const data = await resp.json();
            const count = Array.isArray(data)
                ? data.filter((s) => s && s.needs_attention).length
                : 0;
            updateBadge(count);
        } catch (_) { /* сеть недоступна — бейдж не трогаем */ }
    };

    const start = () => {
        if (badgePollTimer) return;
        poll();
        badgePollTimer = setInterval(poll, 5000);
    };
    const stop = () => {
        if (badgePollTimer) {
            clearInterval(badgePollTimer);
            badgePollTimer = 0;
        }
    };

    document.addEventListener('visibilitychange', () => {
        if (document.hidden) stop();
        else start();
    });

    if (!document.hidden) start();
}

function updateBadge(count) {
    if (count === lastBadgeCount) return;
    lastBadgeCount = count;
    try {
        if (count > 0) {
            navigator.setAppBadge(count);
        } else if ('clearAppBadge' in navigator) {
            navigator.clearAppBadge();
        } else {
            navigator.setAppBadge(0);
        }
    } catch (_) { /* Badging может быть запрещён политикой */ }
}

// ───────────────────────── 3. Screen Wake Lock ─────────────────────────

let wakeLock = null;
let wakeLockWanted = false;

/**
 * Screen Wake Lock: тоггл «Не гасить экран» (полезно при мониторинге сессий на
 * телефоне). Кнопка добавляется в #settings-bar. Wake Lock автоматически
 * снимается ОС при скрытии вкладки, поэтому re-acquire на visibilitychange,
 * если пользователь его включал.
 */
function initWakeLock() {
    if (!('wakeLock' in navigator)) return; // нет Wake Lock API.

    const bar = document.getElementById('settings-bar');
    if (!bar) return;
    if (document.getElementById('pwa-wakelock-toggle')) return;

    const btn = document.createElement('button');
    btn.type = 'button';
    btn.id = 'pwa-wakelock-toggle';
    btn.className = 'pwa-wakelock-toggle';
    btn.title = 'Не гасить экран';
    btn.setAttribute('aria-label', 'Не гасить экран');
    btn.setAttribute('aria-pressed', 'false');
    btn.textContent = '☀';
    bar.appendChild(btn);

    const reflect = () => {
        const on = wakeLockWanted;
        btn.classList.toggle('active', on);
        btn.setAttribute('aria-pressed', on ? 'true' : 'false');
        btn.title = on ? 'Экран не гаснет (нажмите чтобы выключить)' : 'Не гасить экран';
    };

    btn.addEventListener('click', async () => {
        wakeLockWanted = !wakeLockWanted;
        reflect();
        if (wakeLockWanted) {
            await acquireWakeLock();
        } else {
            await releaseWakeLock();
        }
    });

    // Re-acquire при возврате вкладки в фокус (ОС снимает lock при скрытии).
    document.addEventListener('visibilitychange', async () => {
        if (!document.hidden && wakeLockWanted && wakeLock === null) {
            await acquireWakeLock();
        }
    });
}

async function acquireWakeLock() {
    try {
        wakeLock = await navigator.wakeLock.request('screen');
        wakeLock.addEventListener('release', () => {
            wakeLock = null;
        });
    } catch (err) {
        // Может упасть, если вкладка не видима / батарея low-power.
        wakeLock = null;
        console.warn('[pwa] wakeLock request failed:', err);
    }
}

async function releaseWakeLock() {
    try {
        if (wakeLock) await wakeLock.release();
    } catch (_) { /* уже снят */ }
    wakeLock = null;
}

// ───────────────────────── 4. online/offline баннер ────────────────────

let offlineBanner = null;

/**
 * Тонкий баннер «Офлайн — показаны сохранённые данные» (вверху), управляемый
 * navigator.onLine + события online/offline. Синхронизируется со status-dot:
 * при офлайне добавляет ему класс .pwa-offline (визуальная согласованность).
 */
function initOnlineBanner() {
    const ensureBanner = () => {
        if (offlineBanner) return offlineBanner;
        const el = document.createElement('div');
        el.className = 'pwa-offline-banner';
        el.id = 'pwa-offline-banner';
        el.setAttribute('role', 'status');
        el.textContent = 'Офлайн — показаны сохранённые данные';
        document.body.appendChild(el);
        offlineBanner = el;
        return el;
    };

    const apply = () => {
        const online = navigator.onLine;
        const banner = ensureBanner();
        banner.classList.toggle('pwa-banner-show', !online);
        const dot = document.getElementById('status-dot');
        if (dot) dot.classList.toggle('pwa-offline', !online);
    };

    window.addEventListener('online', apply);
    window.addEventListener('offline', apply);
    apply();
}
