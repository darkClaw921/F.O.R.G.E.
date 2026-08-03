// tmux-web — VK plugin: off-canvas колонка бесед на узких экранах.
//
// На десктопе `#vk-contacts` — обычная колонка 322px внутри `#vk-body`. На
// ширине ≤768px она уезжает за левый край (`transform: translateX(-100%)`) и
// выезжает поверх ленты по кнопке ☰; открытое состояние — класс
// `vk-contacts-open` на `#vk` (css/vk.css).
//
// ПОЧЕМУ КЛАСС НА `#vk`, А НЕ НА САМОЙ КОЛОНКЕ: одним состоянием управляются
// ТРИ элемента — сама колонка, подложка `#vk-contacts-scrim` и
// `aria-expanded` кнопки. Общий предок избавляет от рассинхронизации.
//
// ПОРОГ ПРОДУБЛИРОВАН: 768 живёт и здесь, и в `@media (max-width: 768px)`
// внутри css/vk.css. Иначе никак — CSS не читает JS-константы, а JS не должен
// парсить стили. Расхождение значений даёт худший из багов: колонка уже
// нарисована выезжающей, но `closeVkContactsAfterSelect` считает экран
// широким и не закрывает её после выбора беседы. Меняете здесь — меняйте там
// же (комментарий-напоминание стоит в обоих местах).
//
// Закрытие: выбор беседы (главный сценарий — ради него панель и открывали),
// клик по подложке, Esc, а также переход на десктопную ширину (иначе класс
// остался бы висеть и подложка перекрыла бы широкую раскладку).
//
// Модуль намеренно НЕ трогает `hidden` у `#vk-contacts`: колонка обязана
// оставаться в потоке, потому что на десктопе это единственный список бесед.
// Скрывает её только transform.

import { $vk, $vkContacts, $vkContactsToggle, $vkContactsScrim } from '../core/dom.js';

/** Граница, с которой колонка становится выезжающей. Синхронна css/vk.css. */
export const VK_MOBILE_MAX_WIDTH = 768;

/** Класс открытого состояния на `#vk`. */
export const VK_CONTACTS_OPEN_CLASS = 'vk-contacts-open';

const local = {
    /** Обработчики навешены. */
    bound: false,
    /** MediaQueryList для отслеживания ухода на десктопную ширину. */
    mql: null,
};

/** Узкий ли сейчас экран (по тому же порогу, что и CSS). */
export function isVkNarrowScreen() {
    try {
        return window.matchMedia(`(max-width: ${VK_MOBILE_MAX_WIDTH}px)`).matches;
    } catch (_) {
        return false;
    }
}

/** Открыта ли колонка бесед. */
export function isVkContactsOpen() {
    return !!$vk && $vk.classList.contains(VK_CONTACTS_OPEN_CLASS);
}

/**
 * Открывает/закрывает колонку.
 *
 * @param {boolean} open
 */
export function setVkContactsOpen(open) {
    if (!$vk) return;
    const want = !!open;
    $vk.classList.toggle(VK_CONTACTS_OPEN_CLASS, want);
    if ($vkContactsScrim) $vkContactsScrim.hidden = !want;
    if ($vkContactsToggle) $vkContactsToggle.setAttribute('aria-expanded', want ? 'true' : 'false');
    // Фокус переносится в список только при открытии по кнопке: без этого
    // клавиатурная навигация оставалась бы в titlebar'е, за пределами
    // выехавшей панели.
    if (want && $vkContacts && typeof $vkContacts.focus === 'function') {
        try { $vkContacts.focus({ preventScroll: true }); } catch (_) { /* старый браузер */ }
    }
}

/** Закрывает колонку (идемпотентно). */
export function closeVkContacts() {
    if (isVkContactsOpen()) setVkContactsOpen(false);
}

/** Переключает колонку. */
export function toggleVkContacts() {
    setVkContactsOpen(!isVkContactsOpen());
}

/**
 * Закрытие после выбора беседы.
 *
 * Отдельная функция, а не прямой `closeVkContacts` из main.js: на десктопе
 * закрывать нечего, и лишний класс-тоггл на каждом клике по списку не нужен.
 */
export function closeVkContactsAfterSelect() {
    if (isVkNarrowScreen()) closeVkContacts();
}

/**
 * Навешивает обработчики. Идемпотентна — вызывается из initVk().
 */
export function initVkMobile() {
    if (local.bound) return;
    local.bound = true;

    if ($vkContactsToggle) {
        $vkContactsToggle.addEventListener('click', () => toggleVkContacts());
    }
    if ($vkContactsScrim) {
        $vkContactsScrim.addEventListener('click', () => closeVkContacts());
    }

    // Esc: обработчик на документе, но реагирует ТОЛЬКО когда панель открыта.
    // Лайтбокс и оверлей captcha тоже слушают Esc — конфликта нет: они
    // закрываются первыми, а при открытом лайтбоксе/captcha колонка бесед
    // открытой быть не может.
    document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape') return;
        if (!isVkContactsOpen()) return;
        closeVkContacts();
    });

    // Поворот экрана / ресайз окна до десктопной ширины: снимаем состояние,
    // чтобы подложка не осталась висеть поверх широкой раскладки.
    try {
        local.mql = window.matchMedia(`(max-width: ${VK_MOBILE_MAX_WIDTH}px)`);
        const onChange = (e) => { if (!e.matches) closeVkContacts(); };
        if (typeof local.mql.addEventListener === 'function') {
            local.mql.addEventListener('change', onChange);
        } else if (typeof local.mql.addListener === 'function') {
            local.mql.addListener(onChange); // Safari < 14
        }
    } catch (_) {
        local.mql = null;
    }
}

/** Снимок для e2e/диагностики. */
export function _debugVkMobile() {
    return {
        narrow: isVkNarrowScreen(),
        open: isVkContactsOpen(),
        scrimHidden: $vkContactsScrim ? $vkContactsScrim.hidden : null,
    };
}
