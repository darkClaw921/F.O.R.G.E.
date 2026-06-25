/*
 * push.js — UI Web Push в devforge (Фаза 5).
 *
 * Лениво импортируется из bootstrap.js ТОЛЬКО при enabled === true (opt-in).
 * Добавляет секцию «Push-уведомления» с тогглом в существующий Settings-модал
 * (вкладка Notifications). Реальная доставка пушей сделана в Фазе 3 (backend),
 * здесь — пользовательский флоу подписки/отписки.
 *
 * Ключевые требования платформы:
 *   • Notification.requestPermission() ВЫЗЫВАЕТСЯ СИНХРОННО в обработчике клика
 *     (iOS Safari требует прямой user-gesture; нельзя await'ить до запроса);
 *   • pushManager.subscribe({ userVisibleOnly:true, applicationServerKey })
 *     где applicationServerKey = urlBase64ToUint8Array(vapidPublicKey);
 *   • iOS web push работает ТОЛЬКО в установленном на «Домой» PWA (standalone),
 *     поэтому в браузерном iOS тоггл задизейблен с подсказкой установить.
 *
 * Сетевое:
 *   • POST /api/push/subscribe   — тело = subscription.toJSON() (browser-формат
 *     { endpoint, keys:{p256dh, auth} }), Content-Type application/json,
 *     credentials same-origin (пройдёт csrf_guard; в remote-mode токен
 *     добавляется подменённым window.fetch из core/auth.js).
 *   • POST /api/push/unsubscribe — тело { endpoint }.
 *   • POST /api/push/test        — тестовый пуш (кнопка появляется при активной
 *     подписке).
 */

'use strict';

import { isStandalone, isIos } from './install.js';

const PANEL_SELECTOR = '#ps-panel-notifications';
const SECTION_ID = 'pwa-push-section';

let modalObserver = null;

/**
 * Точка входа. Вызывается из bootstrap.js. Навешивает MutationObserver на
 * <body>, который при открытии Settings-модала инжектит push-секцию во вкладку
 * Notifications. Идемпотентна.
 */
export function initPush() {
    if (window.__FORGE_PWA_PUSH_INIT) return;
    window.__FORGE_PWA_PUSH_INIT = true;

    if (!pushSupported()) return;

    // Если модал уже открыт к моменту инициализации — инжектим сразу.
    tryInjectIntoOpenModal();

    // Settings-модал создаётся императивно (modal.js не диспатчит событий),
    // поэтому ловим его появление через MutationObserver на body.
    modalObserver = new MutationObserver((mutations) => {
        for (const m of mutations) {
            for (const node of m.addedNodes) {
                if (!(node instanceof HTMLElement)) continue;
                if (node.querySelector && node.querySelector(PANEL_SELECTOR)) {
                    injectPushSection(node.querySelector(PANEL_SELECTOR));
                }
            }
        }
    });
    modalObserver.observe(document.body, { childList: true, subtree: false });
}

/** Поддержка Web Push: SW + PushManager + Notification API. */
function pushSupported() {
    return (
        'serviceWorker' in navigator
        && 'PushManager' in window
        && 'Notification' in window
    );
}

function tryInjectIntoOpenModal() {
    const panel = document.querySelector(PANEL_SELECTOR);
    if (panel) injectPushSection(panel);
}

// ───────────────────────────── DOM-инжект ─────────────────────────────

/**
 * Вставляет секцию push в панель Notifications Settings-модала. Идемпотентно
 * (по SECTION_ID). После вставки синхронизирует UI с реальным состоянием
 * (Notification.permission + getSubscription()).
 */
function injectPushSection(panel) {
    if (!panel || panel.querySelector('#' + SECTION_ID)) return;

    const section = document.createElement('div');
    section.className = 'pwa-push-section';
    section.id = SECTION_ID;

    const heading = document.createElement('h3');
    heading.textContent = 'Push-уведомления';
    section.appendChild(heading);

    const desc = document.createElement('p');
    desc.className = 'pwa-push-desc';
    desc.textContent = 'Получать пуш на это устройство, когда сессия Claude '
        + 'требует внимания — даже при закрытой вкладке.';
    section.appendChild(desc);

    // Тоггл (checkbox в стиле switch).
    const row = document.createElement('label');
    row.className = 'pwa-push-toggle-row';
    const toggle = document.createElement('input');
    toggle.type = 'checkbox';
    toggle.id = 'pwa-push-toggle';
    toggle.className = 'pwa-push-toggle';
    const toggleText = document.createElement('span');
    toggleText.textContent = 'Включить push на этом устройстве';
    row.appendChild(toggle);
    row.appendChild(toggleText);
    section.appendChild(row);

    // Статус/подсказка.
    const status = document.createElement('div');
    status.className = 'pwa-push-status';
    status.id = 'pwa-push-status';
    section.appendChild(status);

    // Кнопка теста (показывается только при активной подписке).
    const testBtn = document.createElement('button');
    testBtn.type = 'button';
    testBtn.className = 'pwa-push-test-btn';
    testBtn.id = 'pwa-push-test-btn';
    testBtn.textContent = 'Отправить тест';
    testBtn.hidden = true;
    section.appendChild(testBtn);

    panel.appendChild(section);

    const ui = { toggle, status, testBtn };

    // ВАЖНО: requestPermission должен вызываться СИНХРОННО в обработчике клика
    // (iOS). Поэтому слушаем 'click' и для granted-кейса не делаем await до
    // requestPermission. Используем 'change' только для чтения нового значения.
    toggle.addEventListener('change', () => onToggleChange(ui));
    testBtn.addEventListener('click', () => onTestClick(ui));

    // Отразить текущее состояние при открытии.
    refreshState(ui);
}

// ──────────────────────────── состояние UI ────────────────────────────

/**
 * Синхронизирует тоггл/статус/кнопку теста с реальным состоянием:
 * Notification.permission + pushManager.getSubscription(). Также применяет
 * iOS-ограничение (push только в standalone).
 */
async function refreshState(ui) {
    // iOS вне standalone: push недоступен — дизейблим с подсказкой.
    if (isIos() && !isStandalone()) {
        ui.toggle.checked = false;
        ui.toggle.disabled = true;
        setStatus(ui, 'Чтобы получать push на iPhone/iPad, сначала установите '
            + 'приложение на «Домой» (кнопка «Поделиться» → «На экран „Домой“»).',
        'warn');
        ui.testBtn.hidden = true;
        return;
    }

    if (Notification.permission === 'denied') {
        ui.toggle.checked = false;
        ui.toggle.disabled = true;
        setStatus(ui, 'Уведомления заблокированы в настройках браузера. '
            + 'Разрешите их для этого сайта, чтобы включить push.', 'warn');
        ui.testBtn.hidden = true;
        return;
    }

    ui.toggle.disabled = false;

    let sub = null;
    try {
        const reg = await navigator.serviceWorker.ready;
        sub = await reg.pushManager.getSubscription();
    } catch (_) {
        sub = null;
    }

    const active = !!sub && Notification.permission === 'granted';
    ui.toggle.checked = active;
    ui.testBtn.hidden = !active;

    if (active) {
        setStatus(ui, 'Push включён на этом устройстве.', 'ok');
    } else {
        setStatus(ui, '', '');
    }
}

function setStatus(ui, text, kind) {
    ui.status.textContent = text || '';
    ui.status.className = 'pwa-push-status' + (kind ? ' pwa-push-status-' + kind : '');
}

// ────────────────────────── toggle handlers ───────────────────────────

function onToggleChange(ui) {
    if (ui.toggle.checked) {
        enablePush(ui);
    } else {
        disablePush(ui);
    }
}

/**
 * Включение push. КРИТИЧНО: Notification.requestPermission() вызывается
 * синхронно в стеке обработчика клика (без предшествующего await) — иначе
 * iOS отклонит запрос (теряется user-gesture). requestPermission возвращает
 * Promise, но сам ВЫЗОВ синхронный; дальнейшая работа — уже в .then-цепочке.
 */
function enablePush(ui) {
    const vapid = getVapidKey();
    if (!vapid) {
        ui.toggle.checked = false;
        setStatus(ui, 'Сервер не предоставил VAPID-ключ — push недоступен.', 'warn');
        return;
    }

    setStatus(ui, 'Запрашиваем разрешение…', '');

    // Синхронный вызов в обработчике клика (iOS-требование).
    const permPromise = Notification.requestPermission();

    permPromise
        .then(async (permission) => {
            if (permission !== 'granted') {
                ui.toggle.checked = false;
                setStatus(ui,
                    permission === 'denied'
                        ? 'Разрешение отклонено. Включить push можно только '
                          + 'разрешив уведомления для этого сайта.'
                        : 'Разрешение не выдано.',
                    'warn');
                ui.toggle.disabled = permission === 'denied';
                return;
            }

            const reg = await navigator.serviceWorker.ready;
            let appServerKey;
            try {
                appServerKey = urlBase64ToUint8Array(vapid);
            } catch (e) {
                ui.toggle.checked = false;
                setStatus(ui, 'Некорректный VAPID-ключ.', 'warn');
                return;
            }

            const sub = await reg.pushManager.subscribe({
                userVisibleOnly: true,
                applicationServerKey: appServerKey,
            });

            const ok = await postSubscribe(sub);
            if (ok) {
                ui.toggle.checked = true;
                ui.testBtn.hidden = false;
                setStatus(ui, 'Push включён на этом устройстве.', 'ok');
            } else {
                // Backend не принял — откатываем подписку, чтобы UI был честным.
                try { await sub.unsubscribe(); } catch (_) {}
                ui.toggle.checked = false;
                ui.testBtn.hidden = true;
                setStatus(ui, 'Сервер не сохранил подписку. Попробуйте позже.', 'warn');
            }
        })
        .catch((err) => {
            console.warn('[pwa] enablePush failed:', err);
            ui.toggle.checked = false;
            ui.testBtn.hidden = true;
            setStatus(ui, 'Не удалось включить push: ' + (err && err.message ? err.message : err), 'warn');
        });
}

/** Выключение push: unsubscribe() + POST /api/push/unsubscribe({endpoint}). */
async function disablePush(ui) {
    setStatus(ui, 'Отключаем…', '');
    try {
        const reg = await navigator.serviceWorker.ready;
        const sub = await reg.pushManager.getSubscription();
        if (sub) {
            const endpoint = sub.endpoint;
            // Сначала снимаем браузерную подписку, затем чистим на сервере.
            try { await sub.unsubscribe(); } catch (_) {}
            await postUnsubscribe(endpoint);
        }
        ui.toggle.checked = false;
        ui.testBtn.hidden = true;
        setStatus(ui, '', '');
    } catch (err) {
        console.warn('[pwa] disablePush failed:', err);
        setStatus(ui, 'Не удалось отключить push: ' + (err && err.message ? err.message : err), 'warn');
        // Перечитываем реальное состояние (вдруг подписка всё же снята).
        refreshState(ui);
    }
}

async function onTestClick(ui) {
    setStatus(ui, 'Отправляем тестовый пуш…', '');
    try {
        const r = await fetch('/api/push/test', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            credentials: 'same-origin',
            body: '{}',
        });
        if (r.ok) {
            const data = await r.json().catch(() => null);
            const sent = data && typeof data.sent === 'number' ? data.sent : '?';
            setStatus(ui, 'Тест отправлен (доставлено подписок: ' + sent + ').', 'ok');
        } else {
            setStatus(ui, 'Тест не отправлен (HTTP ' + r.status + ').', 'warn');
        }
    } catch (err) {
        setStatus(ui, 'Ошибка отправки теста: ' + (err && err.message ? err.message : err), 'warn');
    }
}

// ──────────────────────────────── сеть ─────────────────────────────────

async function postSubscribe(subscription) {
    try {
        const r = await fetch('/api/push/subscribe', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            credentials: 'same-origin',
            // subscription.toJSON() даёт { endpoint, expirationTime, keys:{p256dh,auth} }
            body: JSON.stringify(subscription.toJSON()),
        });
        return r.ok;
    } catch (err) {
        console.warn('[pwa] postSubscribe failed:', err);
        return false;
    }
}

async function postUnsubscribe(endpoint) {
    try {
        await fetch('/api/push/unsubscribe', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            credentials: 'same-origin',
            body: JSON.stringify({ endpoint }),
        });
    } catch (err) {
        console.warn('[pwa] postUnsubscribe failed:', err);
    }
}

// ──────────────────────────────── utils ────────────────────────────────

/** VAPID public key (base64url) из bootstrap (window.__FORGE_PWA). */
function getVapidKey() {
    return (window.__FORGE_PWA && window.__FORGE_PWA.vapidPublicKey) || null;
}

/**
 * Стандартный helper: VAPID public key в base64url → Uint8Array для
 * applicationServerKey. base64url ('-'/'_', без паддинга) → base64
 * ('+'/'/' + '='-паддинг до кратности 4) → atob → байты.
 */
export function urlBase64ToUint8Array(base64String) {
    const padding = '='.repeat((4 - (base64String.length % 4)) % 4);
    const base64 = (base64String + padding)
        .replace(/-/g, '+')
        .replace(/_/g, '/');
    const rawData = atob(base64);
    const outputArray = new Uint8Array(rawData.length);
    for (let i = 0; i < rawData.length; ++i) {
        outputArray[i] = rawData.charCodeAt(i);
    }
    return outputArray;
}
