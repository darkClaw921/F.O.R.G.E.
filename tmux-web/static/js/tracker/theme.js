// tmux-web — трекер: собственная тема панели (тумблер ◐).
//
// ЗАЧЕМ ОТДЕЛЬНАЯ ТЕМА. Панель #tracker нарисована в терминальной палитре и
// НЕ наследует токены приложения: все её цвета объявлены на `#tracker` в
// css/tracker.css. Значит переключение темы devforge её не касается — и это
// правильно, пока пользователь сидит в тёмной теме. На светлой теме
// приложения чёрная панель выглядит инородно, поэтому у вкладки есть свой
// переключатель, ровно как у Telegram, VK и Битрикс24.
//
// ТРИ РЕЖИМА (localStorage `forge.trackerTheme`, по умолчанию 'auto'):
//   auto  — светлота панели выводится из темы ПРИЛОЖЕНИЯ;
//   dark  — всегда тёмная, тема приложения игнорируется;
//   light — всегда светлая, тема приложения игнорируется.
//
// КАК РАБОТАЕТ auto. Глобального флага «сейчас светлая тема» в devforge нет:
// js/themes/api.js::applyTheme просто расставляет CSS-переменные (--bg и
// прочие) на `:root`. Поэтому светлота вычисляется из вычисленного значения
// `--bg`: разбор цвета и яркость — чистые функции ОБЩЕГО СЛОЯ
// (js/messenger/theme-core.js, покрыт Node-тестом), решение — resolveTheme.
// Своей копии этой математики здесь нет и быть не должно: третья копия молча
// отстанет на первом же нераспознанном формате цвета.
//
// ПОЧЕМУ ОБЩИЙ СЛОЙ НАЗЫВАЕТСЯ `messenger`, ЕСЛИ ТРЕКЕР НЕ МЕССЕНДЖЕР. Имя
// каталога — след происхождения (первыми туда переехали Telegram и VK), а не
// ограничение: в theme-core.js нет ни одной сущности мессенджера, там только
// математика цвета и перебор режимов. Переименовывать каталог ради одного
// нового потребителя дороже, чем принять исторические имена — правка задела
// бы три панели и их тесты.
//
// КАК ЛОВИТСЯ СМЕНА ТЕМЫ ПРИЛОЖЕНИЯ. applyTheme пишет переменные через
// `root.style.setProperty`, то есть меняет атрибут `style` у `<html>`.
// MutationObserver на этот атрибут — единственная развязка, которая не тянет
// плагин в модуль тем (обратный импорт замкнул бы граф модулей).
//
// РЕЖИМ ХРАНИТСЯ ЗДЕСЬ, А НЕ В СОСТОЯНИИ ПАНЕЛИ. У VK для этого есть
// `vkState`, у трекера общего состояния нет вовсе: js/tracker/main.js держит
// всё в приватном `local` и ГРУЗИТСЯ ЛЕНИВО — при первом открытии вкладки.
// Тема же обязана быть применена ДО этого: панель размечена в index.html
// всегда, и на светлой теме приложения она мелькнула бы чёрной. Поэтому
// модуль автономен: статический импорт из bootstrap, собственный `local`,
// ноль связей с main.js.
//
// Применение вызывается из: initTrackerTheme (старт страницы), tabs.js (вход
// на вкладку) и самого наблюдателя.

import { $tracker, $trkThemeToggle } from '../core/dom.js';
import {
    nextThemeMode, normalizeThemeMode, resolveTheme, themeToggleTitle,
} from '../messenger/theme-core.js';

/** Ключ localStorage. Один на все вкладки браузера — так и задумано. */
export const TRACKER_THEME_STORAGE_KEY = 'forge.trackerTheme';

const local = {
    /** Текущий режим ('auto' | 'dark' | 'light'). */
    mode: 'auto',
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
export function loadTrackerThemeMode() {
    try {
        return normalizeThemeMode(window.localStorage.getItem(TRACKER_THEME_STORAGE_KEY));
    } catch (_) {
        return 'auto';
    }
}

/**
 * Сохраняет режим.
 * @param {*} mode
 */
export function saveTrackerThemeMode(mode) {
    try {
        window.localStorage.setItem(TRACKER_THEME_STORAGE_KEY, normalizeThemeMode(mode));
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
export function applyTrackerTheme() {
    const mode = normalizeThemeMode(local.mode);
    const resolved = resolveTheme(mode, mode === 'auto' ? appBackgroundColor() : null);
    if ($tracker && local.applied !== resolved) {
        $tracker.setAttribute('data-tracker-theme', resolved);
    }
    local.applied = resolved;
    if ($trkThemeToggle) {
        // Режим виден и в подписи (для человека), и в атрибуте (для CSS и
        // e2e): в auto-режиме на тёмной теме приложения смена режима на
        // 'dark' не меняет data-tracker-theme, и отличить их можно только так.
        $trkThemeToggle.setAttribute('data-tracker-theme-mode', mode);
        const title = themeToggleTitle(mode, resolved);
        $trkThemeToggle.title = title;
        $trkThemeToggle.setAttribute('aria-label', title);
    }
    return resolved;
}

/**
 * Ставит режим явно (используется тумблером и e2e-тестами).
 *
 * @param {*} mode
 * @returns {'dark'|'light'}
 */
export function setTrackerThemeMode(mode) {
    local.mode = normalizeThemeMode(mode);
    saveTrackerThemeMode(local.mode);
    return applyTrackerTheme();
}

/**
 * Следующий режим по кругу — то, что делает клик по ◐.
 * @returns {'auto'|'dark'|'light'} новый режим
 */
export function cycleTrackerThemeMode() {
    const next = nextThemeMode(local.mode);
    setTrackerThemeMode(next);
    return next;
}

/**
 * Инициализация: восстановить режим, навесить тумблер, начать следить за
 * темой приложения и применить результат.
 *
 * Идемпотентна — вызывается из bootstrap один раз, но повторный вызов
 * (например, из тестов) не создаёт второго наблюдателя и второго обработчика.
 *
 * ГЕЙТ ПЛАГИНА ЗДЕСЬ НЕ ПРОВЕРЯЕТСЯ. Разметка #tracker есть в index.html
 * всегда, а панель без `--tracker` просто скрыта: проставить ей атрибут темы
 * ничего не стоит и ничего не ломает, зато не требует ждать ответа
 * `/api/tracker/status`, то есть исключает мелькание чёрным.
 */
export function initTrackerTheme() {
    local.mode = loadTrackerThemeMode();

    if ($trkThemeToggle && !local.bound) {
        local.bound = true;
        $trkThemeToggle.addEventListener('click', () => {
            cycleTrackerThemeMode();
        });
    }

    if (!local.observer && typeof MutationObserver === 'function') {
        local.observer = new MutationObserver(() => {
            // Только в auto тема приложения вообще что-то значит.
            if (normalizeThemeMode(local.mode) === 'auto') applyTrackerTheme();
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

    applyTrackerTheme();
}

/** Снимает наблюдателя (beforeunload / тесты). */
export function teardownTrackerTheme() {
    if (local.observer) {
        try { local.observer.disconnect(); } catch (_) { /* уже отключён */ }
        local.observer = null;
    }
}

/** Снимок для e2e/диагностики. */
export function _debugTrackerTheme() {
    return {
        mode: normalizeThemeMode(local.mode),
        applied: local.applied,
        appBg: appBackgroundColor(),
        observing: !!local.observer,
    };
}
