/*
 * install.js — UI установки PWA devforge (Фаза 5).
 *
 * Лениво импортируется из bootstrap.js ТОЛЬКО при enabled === true
 * (строгий opt-in). Без флага --pwa этот модуль не загружается вовсе.
 *
 * Поведение:
 *   • Android / desktop (Chromium): ловим событие `beforeinstallprompt`,
 *     `preventDefault()` (чтобы браузер не показал свой мини-инфобар),
 *     сохраняем event и показываем кастомный чип «Установить приложение»
 *     в #settings-bar. Клик → `deferredPrompt.prompt()` → `userChoice`.
 *   • `appinstalled` → скрываем чип (приложение уже стоит).
 *   • iOS (iPhone/iPad) и не standalone: `beforeinstallprompt` там не
 *     существует, поэтому показываем одноразовую текстовую подсказку
 *     «Поделиться → На экран „Домой“». Закрытие подсказки запоминается в
 *     localStorage и больше не показывается.
 *
 * Все стили — из css/pwa.css (`.pwa-install-chip`, см. ниже добавленный
 * `.pwa-ios-hint`). Файл самодостаточен: не зависит от других js-модулей
 * приложения (echo/main и т.п.), чтобы безопасно грузиться рано.
 */

'use strict';

const IOS_HINT_DISMISS_KEY = 'forge-pwa-ios-install-dismissed';

/** Сохранённый event beforeinstallprompt (доступен до prompt()). */
let deferredPrompt = null;
/** Ссылка на чип установки, чтобы скрывать его по appinstalled. */
let installChip = null;

/**
 * Точка входа модуля. Вызывается из bootstrap.js (initInstall()).
 * Идемпотентна: повторный вызов не плодит обработчиков/элементов.
 */
export function initInstall() {
    if (window.__FORGE_PWA_INSTALL_INIT) return;
    window.__FORGE_PWA_INSTALL_INIT = true;

    // Уже установлено и запущено как приложение — ничего не показываем.
    if (isStandalone()) return;

    window.addEventListener('beforeinstallprompt', onBeforeInstallPrompt);
    window.addEventListener('appinstalled', onAppInstalled);

    // iOS не поддерживает beforeinstallprompt — показываем текстовую подсказку.
    // Делаем это с небольшой задержкой, чтобы не мешать первой отрисовке.
    if (isIos() && !isStandalone()) {
        maybeShowIosHint();
    }
}

// ─────────────────────────── beforeinstallprompt ──────────────────────────

function onBeforeInstallPrompt(ev) {
    // Запрещаем браузеру показывать собственный мини-инфобар — управляем сами.
    ev.preventDefault();
    deferredPrompt = ev;
    showInstallChip();
}

function onAppInstalled() {
    deferredPrompt = null;
    hideInstallChip();
}

// ────────────────────────────── install-chip ──────────────────────────────

/** Показывает кастомную кнопку «Установить приложение» в #settings-bar. */
function showInstallChip() {
    if (installChip) {
        installChip.hidden = false;
        return;
    }
    const bar = document.getElementById('settings-bar') || document.body;

    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'pwa-install-chip';
    chip.id = 'pwa-install-chip';
    chip.title = 'Установить приложение на устройство';
    chip.setAttribute('aria-label', 'Установить приложение');
    chip.innerHTML = '<span aria-hidden="true">⬇</span><span>Установить</span>';

    chip.addEventListener('click', onInstallClick);

    bar.appendChild(chip);
    installChip = chip;
}

function hideInstallChip() {
    if (installChip) installChip.hidden = true;
}

async function onInstallClick() {
    if (!deferredPrompt) {
        // Событие уже использовано/недоступно — просто прячем кнопку.
        hideInstallChip();
        return;
    }
    try {
        deferredPrompt.prompt();
        const choice = await deferredPrompt.userChoice;
        // Независимо от выбора, event одноразовый — больше prompt() не вызвать.
        if (choice && choice.outcome === 'accepted') {
            hideInstallChip();
        }
    } catch (err) {
        console.warn('[pwa] install prompt failed:', err);
    } finally {
        deferredPrompt = null;
    }
}

// ────────────────────────────── iOS-подсказка ─────────────────────────────

/**
 * Показывает одноразовую подсказку для iOS Safari: «Поделиться → На экран
 * „Домой“». На iOS нет beforeinstallprompt, установка делается вручную через
 * меню «Поделиться». Закрытие сохраняется в localStorage и не показывается
 * повторно.
 */
function maybeShowIosHint() {
    if (isDismissed()) return;
    if (document.getElementById('pwa-ios-hint')) return;

    const hint = document.createElement('div');
    hint.className = 'pwa-ios-hint';
    hint.id = 'pwa-ios-hint';
    hint.setAttribute('role', 'note');

    const text = document.createElement('span');
    text.className = 'pwa-ios-hint-text';
    // ↑ (стрелка вверх) символизирует кнопку «Поделиться» в Safari.
    text.innerHTML = 'Установить на «Домой»: нажмите '
        + '<span class="pwa-ios-hint-icon" aria-hidden="true">↑</span> '
        + '«Поделиться» → «На экран „Домой“».';

    const dismiss = document.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'pwa-ios-hint-dismiss';
    dismiss.setAttribute('aria-label', 'Скрыть подсказку');
    dismiss.textContent = '×';
    dismiss.addEventListener('click', () => {
        setDismissed();
        hint.remove();
    });

    hint.appendChild(text);
    hint.appendChild(dismiss);
    document.body.appendChild(hint);

    // Плавное появление (класс добавляется в следующем кадре).
    requestAnimationFrame(() => hint.classList.add('pwa-banner-show'));
}

function isDismissed() {
    try {
        return localStorage.getItem(IOS_HINT_DISMISS_KEY) === '1';
    } catch (_) {
        return false;
    }
}

function setDismissed() {
    try {
        localStorage.setItem(IOS_HINT_DISMISS_KEY, '1');
    } catch (_) {
        /* приватный режим / localStorage недоступен — подсказку покажем снова */
    }
}

// ──────────────────────────── platform-detect ─────────────────────────────

/** Запущено ли приложение в standalone-режиме (установлено на устройство). */
export function isStandalone() {
    // navigator.standalone — нестандартный iOS-флаг; display-mode — стандарт.
    return (
        (window.matchMedia
            && window.matchMedia('(display-mode: standalone)').matches)
        || (window.matchMedia
            && window.matchMedia('(display-mode: minimal-ui)').matches)
        || window.navigator.standalone === true
    );
}

/** iOS-детект (iPhone / iPad / iPod), включая iPadOS, маскирующийся под Mac. */
export function isIos() {
    const ua = window.navigator.userAgent || '';
    const isAppleMobile = /iphone|ipad|ipod/i.test(ua);
    // iPadOS 13+ отдаёт «Macintosh» UA, но имеет тач — отличаем по maxTouchPoints.
    const isIpadOS = /macintosh/i.test(ua)
        && typeof navigator.maxTouchPoints === 'number'
        && navigator.maxTouchPoints > 1;
    return isAppleMobile || isIpadOS;
}
