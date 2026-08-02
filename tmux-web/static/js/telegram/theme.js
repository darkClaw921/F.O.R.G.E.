// tmux-web — Telegram plugin: собственная тема вкладки (тумблер ◐).
//
// ЗАЧЕМ ОТДЕЛЬНАЯ ТЕМА. Панель Telegram нарисована в терминальной палитре
// hablo-taste и НЕ наследует токены приложения: все её цвета объявлены на
// `#telegram` в css/telegram.css. Значит переключение темы devforge её не
// касается — и это правильно, пока пользователь сидит в тёмной теме. Но на
// светлой теме приложения чёрная панель выглядит инородно, поэтому у вкладки
// есть свой переключатель.
//
// ТРИ РЕЖИМА (localStorage `forge.telegramTheme`, по умолчанию 'auto'):
//   auto  — светлота панели выводится из темы ПРИЛОЖЕНИЯ;
//   dark  — всегда тёмная, тема приложения игнорируется;
//   light — всегда светлая, тема приложения игнорируется.
//
// КАК РАБОТАЕТ auto. Глобального флага «сейчас светлая тема» в devforge нет:
// js/themes/api.js::applyTheme просто расставляет CSS-переменные (--bg и
// прочие) на `:root`. Поэтому светлота вычисляется из вычисленного значения
// `--bg`: разбор цвета и яркость — чистые функции в format.js (покрыты
// Node-тестом), решение — resolveTgTheme.
//
// КАК ЛОВИТСЯ СМЕНА ТЕМЫ ПРИЛОЖЕНИЯ. applyTheme пишет переменные через
// `root.style.setProperty`, то есть меняет атрибут `style` у `<html>`.
// MutationObserver на этот атрибут — единственная развязка, которая не тянет
// плагин в модуль тем (обратный импорт замкнул бы граф модулей).
//
// Применение вызывается из: initTelegramTheme (старт страницы), tabs.js (вход
// на вкладку) и самого наблюдателя.

import { $telegram, $tgThemeToggle } from '../core/dom.js';
import {
    nextTgThemeMode, normalizeTgThemeMode, resolveTgTheme, tgThemeToggleTitle,
} from './format.js';
import { tgState } from './state.js';

/** Ключ localStorage. Один на все вкладки браузера — так и задумано. */
export const TG_THEME_STORAGE_KEY = 'forge.telegramTheme';

const local = {
    /** Обработчик тумблера навешен. */
    bound: false,
    /** MutationObserver за темой приложения. */
    observer: null,
    /** Последняя применённая тема ('dark' | 'light' | null). */
    applied: null,
};

/**
 * Читает сохранённый режим.
 *
 * localStorage может быть недоступен (Safari private mode, отключённые
 * куки) — это не ошибка, просто режим не переживёт перезагрузку.
 *
 * @returns {'auto'|'dark'|'light'}
 */
export function loadTgThemeMode() {
    try {
        return normalizeTgThemeMode(window.localStorage.getItem(TG_THEME_STORAGE_KEY));
    } catch (_) {
        return 'auto';
    }
}

/**
 * Сохраняет режим.
 * @param {*} mode
 */
export function saveTgThemeMode(mode) {
    try {
        window.localStorage.setItem(TG_THEME_STORAGE_KEY, normalizeTgThemeMode(mode));
    } catch (_) { /* приватный режим — переживём */ }
}

/**
 * Вычисленное значение `--bg` приложения.
 *
 * Пустая строка — штатный случай: переменная объявлена в base.css, но
 * getComputedStyle до применения стилей может её ещё не видеть. Тогда
 * resolveTgTheme возьмёт запасную тёмную.
 *
 * @returns {string}
 */
export function appBackgroundColor() {
    try {
        return getComputedStyle(document.documentElement)
            .getPropertyValue('--bg')
            .trim();
    } catch (_) {
        return '';
    }
}

/**
 * Применяет тему к панели.
 *
 * Идемпотентна и дешёвая: вызывается на каждом входе на вкладку и на каждой
 * мутации `style` у `<html>`, поэтому DOM трогается только при реальном
 * изменении.
 *
 * @returns {'dark'|'light'} применённая тема
 */
export function applyTgTheme() {
    const mode = normalizeTgThemeMode(tgState.themeMode);
    const resolved = resolveTgTheme(mode, mode === 'auto' ? appBackgroundColor() : null);
    if ($telegram && local.applied !== resolved) {
        $telegram.setAttribute('data-tg-theme', resolved);
    }
    local.applied = resolved;
    if ($tgThemeToggle) {
        // Режим виден и в подписи (для человека), и в атрибуте (для CSS и
        // e2e): в auto-режиме на тёмной теме приложения смена режима на
        // 'dark' не меняет data-tg-theme, и отличить их можно только так.
        $tgThemeToggle.setAttribute('data-tg-theme-mode', mode);
        const title = tgThemeToggleTitle(mode, resolved);
        $tgThemeToggle.title = title;
        $tgThemeToggle.setAttribute('aria-label', title);
    }
    return resolved;
}

/**
 * Ставит режим явно (используется тумблером и e2e-тестами).
 *
 * @param {*} mode
 * @returns {'dark'|'light'}
 */
export function setTgThemeMode(mode) {
    tgState.themeMode = normalizeTgThemeMode(mode);
    saveTgThemeMode(tgState.themeMode);
    return applyTgTheme();
}

/**
 * Следующий режим по кругу — то, что делает клик по ◐.
 * @returns {'auto'|'dark'|'light'} новый режим
 */
export function cycleTgThemeMode() {
    const next = nextTgThemeMode(tgState.themeMode);
    setTgThemeMode(next);
    return next;
}

/**
 * Инициализация: восстановить режим, навесить тумблер, начать следить за
 * темой приложения и применить результат.
 *
 * Идемпотентна — вызывается из bootstrap один раз, но повторный вызов
 * (например, из тестов) не создаёт второго наблюдателя и второго обработчика.
 */
export function initTelegramTheme() {
    tgState.themeMode = loadTgThemeMode();

    if ($tgThemeToggle && !local.bound) {
        local.bound = true;
        $tgThemeToggle.addEventListener('click', () => {
            cycleTgThemeMode();
        });
    }

    if (!local.observer && typeof MutationObserver === 'function') {
        local.observer = new MutationObserver(() => {
            // Только в auto тема приложения вообще что-то значит.
            if (normalizeTgThemeMode(tgState.themeMode) === 'auto') applyTgTheme();
        });
        try {
            local.observer.observe(document.documentElement, {
                attributes: true,
                attributeFilter: ['style', 'class', 'data-theme'],
            });
        } catch (_) {
            local.observer = null;
        }
    }

    applyTgTheme();
}

/** Снимает наблюдателя (beforeunload / тесты). */
export function teardownTelegramTheme() {
    if (local.observer) {
        try { local.observer.disconnect(); } catch (_) { /* уже отключён */ }
        local.observer = null;
    }
}

/** Снимок для e2e/диагностики. */
export function _debugTheme() {
    return {
        mode: normalizeTgThemeMode(tgState.themeMode),
        applied: local.applied,
        appBg: appBackgroundColor(),
        observing: !!local.observer,
    };
}
