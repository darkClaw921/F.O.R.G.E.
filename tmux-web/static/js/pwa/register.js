/*
 * register.js — регистрация Service Worker + ненавязчивый update-flow.
 *
 * Импортируется лениво из bootstrap.js ТОЛЬКО при enabled=true. Экспортирует
 * registerServiceWorker(), которую bootstrap вызывает после инжекта manifest/meta.
 *
 * Update-flow БЕЗ авто-skipWaiting:
 *   • register('/sw.js', {scope:'/'});
 *   • на 'updatefound' ждём installing → state==='installed';
 *   • если есть navigator.serviceWorker.controller (т.е. это обновление, а не
 *     первая установка) → показываем баннер «Доступно обновление — Обновить»;
 *   • клик по «Обновить» → reg.waiting.postMessage({type:'SKIP_WAITING'});
 *   • новый SW вызывает skipWaiting()→activate→'controllerchange';
 *   • на 'controllerchange' (однократно, guard refreshing) → location.reload().
 *
 * Первая установка (controller отсутствует) баннер НЕ показывает — нечего
 * обновлять, шелл просто прекэшируется в фоне.
 */

'use strict';

// Guard от петли перезагрузок: controllerchange может сработать несколько раз.
let refreshing = false;

/**
 * Зарегистрировать SW и навесить update-flow.
 * @returns {Promise<ServiceWorkerRegistration|null>}
 */
export async function registerServiceWorker() {
    if (!('serviceWorker' in navigator)) {
        return null;
    }

    // Один reload при смене активного контроллера (после SKIP_WAITING).
    navigator.serviceWorker.addEventListener('controllerchange', () => {
        if (refreshing) return;
        refreshing = true;
        window.location.reload();
    });

    let reg;
    try {
        reg = await navigator.serviceWorker.register('/sw.js', { scope: '/' });
    } catch (err) {
        console.warn('[pwa] SW registration failed:', err);
        return null;
    }

    // Если уже есть ожидающий SW (страница загрузилась, пока новый ждал) —
    // и есть активный контроллер, значит это обновление: показываем баннер.
    if (reg.waiting && navigator.serviceWorker.controller) {
        showUpdateBanner(reg);
    }

    // Новый SW начал устанавливаться.
    reg.addEventListener('updatefound', () => {
        const installing = reg.installing;
        if (!installing) return;
        installing.addEventListener('statechange', () => {
            if (
                installing.state === 'installed' &&
                navigator.serviceWorker.controller
            ) {
                // Есть controller → это апдейт (не первая установка) → баннер.
                showUpdateBanner(reg);
            }
        });
    });

    return reg;
}

// ─────────────────────────── update-баннер ────────────────────────────

let bannerEl = null;

/**
 * Показать ненавязчивый баннер обновления. Стили — .pwa-update-banner из
 * css/pwa.css. По клику «Обновить» постим SKIP_WAITING ожидающему SW.
 * @param {ServiceWorkerRegistration} reg
 */
function showUpdateBanner(reg) {
    if (bannerEl) return; // уже показан

    const banner = document.createElement('div');
    banner.className = 'pwa-update-banner';
    banner.setAttribute('role', 'status');

    const text = document.createElement('span');
    text.className = 'pwa-banner-text';
    text.textContent = 'Доступно обновление';
    banner.appendChild(text);

    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'pwa-banner-btn';
    btn.textContent = 'Обновить';
    btn.addEventListener('click', () => {
        const waiting = reg.waiting;
        if (waiting) {
            waiting.postMessage({ type: 'SKIP_WAITING' });
            // controllerchange сделает один reload; до него — выключаем кнопку.
            btn.disabled = true;
            btn.textContent = 'Обновляем…';
        } else {
            // На редкий случай гонки — просто перезагружаемся.
            window.location.reload();
        }
    });
    banner.appendChild(btn);

    const dismiss = document.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'pwa-banner-dismiss';
    dismiss.setAttribute('aria-label', 'Скрыть');
    dismiss.textContent = '×';
    dismiss.addEventListener('click', () => hideUpdateBanner());
    banner.appendChild(dismiss);

    document.body.appendChild(banner);
    bannerEl = banner;

    // requestAnimationFrame — чтобы CSS-transition отработал появление.
    requestAnimationFrame(() => banner.classList.add('pwa-banner-show'));
}

function hideUpdateBanner() {
    if (!bannerEl) return;
    const el = bannerEl;
    bannerEl = null;
    el.classList.remove('pwa-banner-show');
    setTimeout(() => {
        try {
            el.remove();
        } catch (_) {}
    }, 240);
}
