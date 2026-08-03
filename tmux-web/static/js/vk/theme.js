// tmux-web — VK plugin: собственная тема вкладки (тумблер ◐).
//
// ЗАЧЕМ ОТДЕЛЬНАЯ ТЕМА. Панель VK нарисована в терминальной палитре и НЕ
// наследует токены приложения: все её цвета объявлены на `#vk` в
// css/vk.css. Значит переключение темы devforge её не касается — и это
// правильно, пока пользователь сидит в тёмной теме. Но на светлой теме
// приложения чёрная панель выглядит инородно, поэтому у вкладки есть свой
// переключатель.
//
// ТРИ РЕЖИМА (localStorage `forge.vkTheme`, по умолчанию 'auto'):
//   auto  — светлота панели выводится из темы ПРИЛОЖЕНИЯ;
//   dark  — всегда тёмная, тема приложения игнорируется;
//   light — всегда светлая, тема приложения игнорируется.
//
// КАК РАБОТАЕТ auto. Глобального флага «сейчас светлая тема» в devforge нет:
// js/themes/api.js::applyTheme просто расставляет CSS-переменные (--bg и
// прочие) на `:root`. Поэтому светлота вычисляется из вычисленного значения
// `--bg`: разбор цвета и яркость — чистые функции ОБЩЕГО СЛОЯ
// (js/messenger/theme-core.js, покрыт Node-тестом), решение — resolveTheme.
//
// КАК ЛОВИТСЯ СМЕНА ТЕМЫ ПРИЛОЖЕНИЯ. applyTheme пишет переменные через
// `root.style.setProperty`, то есть меняет атрибут `style` у `<html>`.
// MutationObserver на этот атрибут — единственная развязка, которая не тянет
// плагин в модуль тем (обратный импорт замкнул бы граф модулей).
//
// ВТОРОЙ НАБЛЮДАТЕЛЬ НА <html> — ПРИЕМЛЕМО. Такой же уже есть у панели
// Telegram. Связать их означало бы править js/telegram/theme.js, а стоимость
// одного дополнительного наблюдателя (один callback на смену темы, то есть
// на действие пользователя раз в месяц) нулевая.
//
// Применение вызывается из: initVkTheme (старт страницы), tabs.js (вход на
// вкладку) и самого наблюдателя.

import { $vk, $vkThemeToggle } from '../core/dom.js';
import {
    nextThemeMode, normalizeThemeMode, resolveTheme, themeToggleTitle,
} from '../messenger/theme-core.js';
import { vkState } from './state.js';

/** Ключ localStorage. Один на все вкладки браузера — так и задумано. */
export const VK_THEME_STORAGE_KEY = 'forge.vkTheme';

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
export function loadVkThemeMode() {
    try {
        return normalizeThemeMode(window.localStorage.getItem(VK_THEME_STORAGE_KEY));
    } catch (_) {
        return 'auto';
    }
}

/**
 * Сохраняет режим.
 * @param {*} mode
 */
export function saveVkThemeMode(mode) {
    try {
        window.localStorage.setItem(VK_THEME_STORAGE_KEY, normalizeThemeMode(mode));
    } catch (_) { /* приватный режим — переживём */ }
}

/**
 * Вычисленное значение `--bg` приложения.
 *
 * Пустая строка — штатный случай: переменная объявлена в base.css, но
 * getComputedStyle до применения стилей может её ещё не видеть. Тогда
 * resolveTheme возьмёт запасную тёмную.
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
export function applyVkTheme() {
    const mode = normalizeThemeMode(vkState.themeMode);
    const resolved = resolveTheme(mode, mode === 'auto' ? appBackgroundColor() : null);
    if ($vk && local.applied !== resolved) {
        $vk.setAttribute('data-vk-theme', resolved);
    }
    local.applied = resolved;
    if ($vkThemeToggle) {
        // Режим виден и в подписи (для человека), и в атрибуте (для CSS и
        // e2e): в auto-режиме на тёмной теме приложения смена режима на
        // 'dark' не меняет data-vk-theme, и отличить их можно только так.
        $vkThemeToggle.setAttribute('data-vk-theme-mode', mode);
        const title = themeToggleTitle(mode, resolved);
        $vkThemeToggle.title = title;
        $vkThemeToggle.setAttribute('aria-label', title);
    }
    return resolved;
}

/**
 * Ставит режим явно (используется тумблером и e2e-тестами).
 *
 * @param {*} mode
 * @returns {'dark'|'light'}
 */
export function setVkThemeMode(mode) {
    vkState.themeMode = normalizeThemeMode(mode);
    saveVkThemeMode(vkState.themeMode);
    return applyVkTheme();
}

/**
 * Следующий режим по кругу — то, что делает клик по ◐.
 * @returns {'auto'|'dark'|'light'} новый режим
 */
export function cycleVkThemeMode() {
    const next = nextThemeMode(vkState.themeMode);
    setVkThemeMode(next);
    return next;
}

/**
 * Инициализация: восстановить режим, навесить тумблер, начать следить за
 * темой приложения и применить результат.
 *
 * Идемпотентна — вызывается из bootstrap один раз, но повторный вызов
 * (например, из тестов) не создаёт второго наблюдателя и второго обработчика.
 */
export function initVkTheme() {
    vkState.themeMode = loadVkThemeMode();

    if ($vkThemeToggle && !local.bound) {
        local.bound = true;
        $vkThemeToggle.addEventListener('click', () => {
            cycleVkThemeMode();
        });
    }

    if (!local.observer && typeof MutationObserver === 'function') {
        local.observer = new MutationObserver(() => {
            // Только в auto тема приложения вообще что-то значит.
            if (normalizeThemeMode(vkState.themeMode) === 'auto') applyVkTheme();
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

    applyVkTheme();
}

/** Снимает наблюдателя (beforeunload / тесты). */
export function teardownVkTheme() {
    if (local.observer) {
        try { local.observer.disconnect(); } catch (_) { /* уже отключён */ }
        local.observer = null;
    }
}

/** Снимок для e2e/диагностики. */
export function _debugVkTheme() {
    return {
        mode: normalizeThemeMode(vkState.themeMode),
        applied: local.applied,
        appBg: appBackgroundColor(),
        observing: !!local.observer,
    };
}
