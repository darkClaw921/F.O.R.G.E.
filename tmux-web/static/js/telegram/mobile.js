// tmux-web — Telegram plugin: off-canvas колонка контактов на узких экранах.
//
// На десктопе `#tg-contacts` — обычная колонка 322px внутри `#tg-body`. На
// ширине ≤768px она уезжает за левый край (`transform: translateX(-100%)`) и
// выезжает поверх ленты по кнопке ☰; открытое состояние — класс
// `tg-contacts-open` на `#telegram` (css/telegram.css).
//
// ПОЧЕМУ КЛАСС НА `#telegram`, А НЕ НА САМОЙ КОЛОНКЕ: одним состоянием
// управляются ТРИ элемента — сама колонка, подложка `#tg-contacts-scrim` и
// `aria-expanded` кнопки. Общий предок избавляет от рассинхронизации.
//
// Закрытие: выбор диалога (главный сценарий — ради него панель и открывали),
// клик по подложке, Esc, а также переход на десктопную ширину (иначе класс
// остался бы висеть и на широком экране ничего бы не сломал, но и смысла бы
// не имел).
//
// Модуль намеренно НЕ трогает `hidden` у `#tg-contacts`: колонка обязана
// оставаться в потоке, потому что на десктопе это единственный список
// диалогов. Скрывает её только transform.

import { $telegram, $tgContacts, $tgContactsToggle, $tgContactsScrim } from '../core/dom.js';

/** Граница, с которой колонка становится выезжающей. Синхронна telegram.css. */
export const TG_MOBILE_MAX_WIDTH = 768;

/** Класс открытого состояния на `#telegram`. */
export const TG_CONTACTS_OPEN_CLASS = 'tg-contacts-open';

const local = {
    /** Обработчики навешены. */
    bound: false,
    /** MediaQueryList для отслеживания ухода на десктопную ширину. */
    mql: null,
};

/** Узкий ли сейчас экран (по тому же порогу, что и CSS). */
export function isTgNarrowScreen() {
    try {
        return window.matchMedia(`(max-width: ${TG_MOBILE_MAX_WIDTH}px)`).matches;
    } catch (_) {
        return false;
    }
}

/** Открыта ли колонка контактов. */
export function isTgContactsOpen() {
    return !!$telegram && $telegram.classList.contains(TG_CONTACTS_OPEN_CLASS);
}

/**
 * Открывает/закрывает колонку.
 *
 * @param {boolean} open
 */
export function setTgContactsOpen(open) {
    if (!$telegram) return;
    const want = !!open;
    $telegram.classList.toggle(TG_CONTACTS_OPEN_CLASS, want);
    if ($tgContactsScrim) $tgContactsScrim.hidden = !want;
    if ($tgContactsToggle) $tgContactsToggle.setAttribute('aria-expanded', want ? 'true' : 'false');
    // Фокус переносится в список только при открытии по кнопке: без этого
    // клавиатурная навигация оставалась бы в titlebar'е, за пределами
    // выехавшей панели.
    if (want && $tgContacts && typeof $tgContacts.focus === 'function') {
        try { $tgContacts.focus({ preventScroll: true }); } catch (_) { /* старый браузер */ }
    }
}

/** Закрывает колонку (идемпотентно). */
export function closeTgContacts() {
    if (isTgContactsOpen()) setTgContactsOpen(false);
}

/** Переключает колонку. */
export function toggleTgContacts() {
    setTgContactsOpen(!isTgContactsOpen());
}

/**
 * Закрытие после выбора диалога.
 *
 * Отдельная функция, а не прямой `closeTgContacts` из main.js: на десктопе
 * закрывать нечего, и лишний класс-тоггл на каждом клике по списку не нужен.
 */
export function closeTgContactsAfterSelect() {
    if (isTgNarrowScreen()) closeTgContacts();
}

/**
 * Навешивает обработчики. Идемпотентна — вызывается из initTelegram().
 */
export function initTelegramMobile() {
    if (local.bound) return;
    local.bound = true;

    if ($tgContactsToggle) {
        $tgContactsToggle.addEventListener('click', () => toggleTgContacts());
    }
    if ($tgContactsScrim) {
        $tgContactsScrim.addEventListener('click', () => closeTgContacts());
    }

    // Esc: обработчик на документе, но реагирует ТОЛЬКО когда панель открыта.
    // Лайтбокс медиа тоже слушает Esc — конфликта нет: он закрывается первым
    // и колонка контактов при открытом лайтбоксе быть открытой не может.
    document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape') return;
        if (!isTgContactsOpen()) return;
        closeTgContacts();
    });

    // Поворот экрана / ресайз окна до десктопной ширины: снимаем состояние,
    // чтобы подложка не осталась висеть поверх широкой раскладки.
    try {
        local.mql = window.matchMedia(`(max-width: ${TG_MOBILE_MAX_WIDTH}px)`);
        const onChange = (e) => { if (!e.matches) closeTgContacts(); };
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
export function _debugMobile() {
    return {
        narrow: isTgNarrowScreen(),
        open: isTgContactsOpen(),
        scrimHidden: $tgContactsScrim ? $tgContactsScrim.hidden : null,
    };
}
