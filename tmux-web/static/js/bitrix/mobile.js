// tmux-web — Bitrix24 plugin: off-canvas колонка диалогов на узких экранах.
//
// На десктопе `#b24-contacts` — обычная колонка внутри `#b24-body`. На ширине
// ≤768px она уезжает за левый край (`transform: translateX(-102%)`) и выезжает
// поверх ленты по кнопке ☰; открытое состояние — класс `b24-contacts-open` на
// `#bitrix` (css/bitrix.css, блок `@media (max-width: 768px)`).
//
// ПОЧЕМУ КЛАСС НА `#bitrix`, А НЕ НА САМОЙ КОЛОНКЕ: одним состоянием
// управляются ТРИ элемента — сама колонка, подложка `#b24-contacts-scrim` и
// `aria-expanded` кнопки ☰. Общий предок избавляет от рассинхронизации.
//
// ПОРОГ ПРОДУБЛИРОВАН: 768 живёт и здесь, и в `@media (max-width: 768px)`
// внутри css/bitrix.css. Иначе никак — CSS не читает JS-константы, а JS не
// должен парсить стили. Расхождение значений даёт худший из багов: колонка уже
// нарисована выезжающей, но `closeB24ContactsAfterSelect` считает экран
// широким и не закрывает её после выбора диалога. Меняете здесь — меняйте там
// же (комментарий-напоминание стоит в обоих местах).
//
// ЗАКРЫТИЕ: выбор диалога (главный сценарий — ради него колонку и открывали),
// клик по подложке, Esc, а также переход на десктопную ширину (иначе класс
// остался бы висеть и подложка перекрыла бы широкую раскладку).
//
// ЧЕГО МОДУЛЬ НЕ ДЕЛАЕТ. Он не трогает `hidden` у `#b24-contacts`: колонка
// обязана оставаться в потоке, потому что на десктопе это единственный список
// диалогов. Скрывает её только transform. И он не занимается ни лентой, ни
// полем ввода: полная ширина сессии и безопасные зоны (`safe-area-inset`,
// экранная клавиатура) — это правила CSS, а не состояние, и живут в
// css/bitrix.css рядом с самим медиа-запросом.

import {
    $b24, $b24Contacts, $b24ContactsScrim, $b24ContactsToggle, $b24TaskCard,
} from '../core/dom.js';

/** Граница, с которой колонка становится выезжающей. Синхронна css/bitrix.css. */
export const B24_MOBILE_MAX_WIDTH = 768;

/** Класс открытого состояния на `#bitrix`. */
export const B24_CONTACTS_OPEN_CLASS = 'b24-contacts-open';

/** Класс раскрытой карточки задачи (на самой карточке). */
export const B24_TASK_CARD_OPEN_CLASS = 'b24-task-card-open';

const local = {
    /** Обработчики навешены. */
    bound: false,
    /** MediaQueryList для отслеживания ухода на десктопную ширину. */
    mql: null,
};

/** Узкий ли сейчас экран (по тому же порогу, что и CSS). */
export function isB24NarrowScreen() {
    try {
        return window.matchMedia(`(max-width: ${B24_MOBILE_MAX_WIDTH}px)`).matches;
    } catch (_) {
        return false;
    }
}

/** Открыта ли колонка диалогов. */
export function isB24ContactsOpen() {
    return !!$b24 && $b24.classList.contains(B24_CONTACTS_OPEN_CLASS);
}

/**
 * Открывает/закрывает колонку.
 *
 * @param {boolean} open
 */
export function setB24ContactsOpen(open) {
    if (!$b24) return;
    const want = !!open;
    $b24.classList.toggle(B24_CONTACTS_OPEN_CLASS, want);
    if ($b24ContactsScrim) $b24ContactsScrim.hidden = !want;
    if ($b24ContactsToggle) {
        $b24ContactsToggle.setAttribute('aria-expanded', want ? 'true' : 'false');
    }
    // Фокус переносится в список только при ОТКРЫТИИ: без этого клавиатурная
    // навигация оставалась бы в titlebar'е, за пределами выехавшей колонки.
    if (want && $b24Contacts && typeof $b24Contacts.focus === 'function') {
        try { $b24Contacts.focus({ preventScroll: true }); } catch (_) { /* старый браузер */ }
    }
}

/** Закрывает колонку (идемпотентно). */
export function closeB24Contacts() {
    if (isB24ContactsOpen()) setB24ContactsOpen(false);
}

/** Переключает колонку. */
export function toggleB24Contacts() {
    setB24ContactsOpen(!isB24ContactsOpen());
}

/**
 * Закрытие после выбора диалога.
 *
 * Отдельная функция, а не прямой `closeB24Contacts` из точки сборки: на
 * десктопе закрывать нечего, и лишний тоггл класса на каждом клике по списку
 * не нужен.
 */
export function closeB24ContactsAfterSelect() {
    if (isB24NarrowScreen()) closeB24Contacts();
}

/** Раскрыта ли карточка задачи (на узком экране она свёрнута по умолчанию). */
export function isB24TaskCardOpen() {
    return !!$b24TaskCard && $b24TaskCard.classList.contains(B24_TASK_CARD_OPEN_CLASS);
}

/**
 * Раскрывает/сворачивает карточку задачи.
 *
 * Класс живёт НА КАРТОЧКЕ, а не на панели: карточка одна, и общий предок тут
 * ничего бы не синхронизировал — только увёл бы состояние дальше от того, к
 * чему оно относится.
 *
 * На широком экране карточка развёрнута всегда (правило схлопывания живёт
 * внутри `@media (max-width: 768px)`), поэтому класс там ни на что не влияет и
 * снимать его при ресайзе не нужно.
 *
 * @param {boolean} open
 */
export function setB24TaskCardOpen(open) {
    if (!$b24TaskCard) return;
    $b24TaskCard.classList.toggle(B24_TASK_CARD_OPEN_CLASS, !!open);
}

/**
 * Навешивает обработчики. Идемпотентна — зовётся из `initB24()`.
 */
export function initB24Mobile() {
    if (local.bound) return;
    local.bound = true;

    if ($b24ContactsToggle) {
        $b24ContactsToggle.addEventListener('click', () => toggleB24Contacts());
    }
    if ($b24ContactsScrim) {
        $b24ContactsScrim.addEventListener('click', () => closeB24Contacts());
    }

    // Тап по карточке задачи раскрывает её и сворачивает обратно.
    //
    // Клики по КНОПКАМ карточки (принять / начать / завершить) обязаны идти
    // мимо: «завершил задачу и заодно свернул карточку» — не то, чего ждут, а
    // главное, обратный порядок («свернул, потому что промахнулся мимо
    // кнопки») выглядел бы как пропавшее действие.
    if ($b24TaskCard) {
        $b24TaskCard.addEventListener('click', (e) => {
            if (!isB24NarrowScreen()) return;
            const target = e && e.target;
            if (target && typeof target.closest === 'function' && target.closest('button')) return;
            setB24TaskCardOpen(!isB24TaskCardOpen());
        });
    }

    // Esc: обработчик на документе, но реагирует ТОЛЬКО когда колонка открыта.
    // Лайтбокс тоже слушает Esc — конфликта нет: при открытом лайтбоксе колонка
    // диалогов открытой быть не может (он перекрывает панель целиком).
    if (typeof document !== 'undefined' && document.addEventListener) {
        document.addEventListener('keydown', (e) => {
            if (!e || e.key !== 'Escape') return;
            if (!isB24ContactsOpen()) return;
            closeB24Contacts();
        });
    }

    // Поворот экрана / ресайз окна до десктопной ширины: снимаем состояние,
    // чтобы подложка не осталась висеть поверх широкой раскладки.
    try {
        local.mql = window.matchMedia(`(max-width: ${B24_MOBILE_MAX_WIDTH}px)`);
        const onChange = (e) => { if (!e.matches) closeB24Contacts(); };
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
export function _debugB24Mobile() {
    return {
        narrow: isB24NarrowScreen(),
        open: isB24ContactsOpen(),
        scrimHidden: $b24ContactsScrim ? $b24ContactsScrim.hidden : null,
        taskCardOpen: isB24TaskCardOpen(),
        bound: local.bound,
        maxWidth: B24_MOBILE_MAX_WIDTH,
    };
}
