// tmux-web — Echo notifications (toast UI) — general-purpose компонент
// (Phase 5c).
//
// Хотя файл лежит в `js/echo/`, по плану он используется не только Echo:
// любые модули могут импортировать `notify` для показа toast'а.
//
// API:
//   notify({ level: 'info'|'warn'|'error', title, body, ttl=5000 })
//
// Контейнер #echo-toasts из index.html (P5.14) должен присутствовать в DOM.
// Если его нет — fallback: пишем в console.

import { $echoToasts } from '../core/dom.js';
import { state } from '../core/state.js';

const MAX_TOASTS = 5;
const DEFAULT_TTL_MS = 5000;
const _queue = [];

/**
 * Проверяет user-settings: показывать ли Echo-нотификации.
 *
 * Дефолт (settings не загружены / поле отсутствует) — `true`, чтобы
 * нулевая конфигурация поведение не меняла. Только явный `false`
 * приглушает toast'ы.
 */
function isEchoNotificationsEnabled() {
    const us = state.userSettings;
    if (!us) return true;
    if (typeof us.echo_notifications_enabled === 'boolean') {
        return us.echo_notifications_enabled;
    }
    return true;
}

/**
 * Показать toast.
 *
 * @param {{level?: string, title?: string, body?: string, ttl?: number}} opts
 * @returns {HTMLElement|null} элемент toast'а (для тестов / progress-обновления)
 */
export function notify(opts) {
    const level = (opts && opts.level) || 'info';
    const title = (opts && opts.title) || '';
    const body = (opts && opts.body) || '';
    const ttl = (opts && opts.ttl) || DEFAULT_TTL_MS;
    if (!$echoToasts) {
        console.log(`[toast/${level}]`, title, body);
        return null;
    }
    // Cap queue: вытесняем самый старый toast.
    while (_queue.length >= MAX_TOASTS) {
        const old = _queue.shift();
        try { old.remove(); } catch (_) {}
    }
    const toast = document.createElement('div');
    toast.className = `echo-toast echo-toast-${level}`;
    toast.setAttribute('role', 'alert');

    if (title) {
        const t = document.createElement('div');
        t.className = 'echo-toast-title';
        t.textContent = title;
        toast.appendChild(t);
    }
    if (body) {
        const b = document.createElement('div');
        b.className = 'echo-toast-body';
        b.textContent = body;
        toast.appendChild(b);
    }
    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'echo-toast-close';
    close.textContent = '×';
    close.title = 'Закрыть';
    close.addEventListener('click', () => dismiss(toast));
    toast.appendChild(close);

    $echoToasts.appendChild(toast);
    _queue.push(toast);

    // requestAnimationFrame чтобы CSS transition отработал
    requestAnimationFrame(() => toast.classList.add('echo-toast-show'));

    if (ttl > 0) {
        setTimeout(() => dismiss(toast), ttl);
    }
    return toast;
}

function dismiss(toast) {
    if (!toast || !toast.parentNode) return;
    toast.classList.remove('echo-toast-show');
    toast.classList.add('echo-toast-hide');
    setTimeout(() => {
        try { toast.remove(); } catch (_) {}
        const idx = _queue.indexOf(toast);
        if (idx >= 0) _queue.splice(idx, 1);
    }, 300);
}

/** Очистить все активные toasts (для тестов / переключения вкладки). */
export function clearToasts() {
    while (_queue.length) {
        const t = _queue.shift();
        try { t.remove(); } catch (_) {}
    }
}

/**
 * Маппинг serverMsg уровня (info/warn/error) → toast level. Используется
 * handlers.notification в main.js.
 */
export function notifyFromServerMsg(msg) {
    // Уважаем user-setting echo_notifications_enabled. Если выключено —
    // toast не показывается, но событие всё равно проходит через WS handler
    // (consumer может писать в лог и т.п.).
    if (!isEchoNotificationsEnabled()) {
        return null;
    }
    return notify({
        level: msg.level || 'info',
        title: msg.title || '',
        body: msg.body || '',
    });
}
