// tmux-web / VK — реестр действий клавиатуры.
//
// Движка комбинаций здесь НЕТ: он живёт в `js/messenger/hotkeys-core.js` и
// общий на все панели-мессенджеры. Здесь только то, что у панели своё, —
// список действий, их дефолтные клавиши и порядок групп; плюс тонкие обёртки,
// подставляющие реестр первым аргументом, чтобы вызывающий код панели
// (`keynav.js`, форма настроек) не таскал `VK_ACTIONS` через все вызовы.
//
// Файл чистый: ни DOM, ни сети, ни импортов из соседних модулей вкладки, — тот
// же принцип, что у `format.js` и `reply.js`. DOM-часть живёт в `keynav.js`.
//
// ── Отличия реестра от телеграмного (`js/telegram/keys.js::TG_ACTIONS`)
//
// 1. `dir.next` / `dir.prev` ходят по ЧЕТЫРЁМ СЕРВЕРНЫМ фильтрам VK
//    (`all` / `unread` / `important` / `unanswered`), а не по папкам аккаунта:
//    у VK папок нет, а фильтр — это параметр `messages.getConversations`,
//    то есть КАЖДОЕ переключение стоит сетевого запроса. Дебаунс и уважение к
//    лимиту 3 rps живут в `dialogs.js::selectDir`; отсюда же следует, что
//    удерживать `]` нажатым — плохая идея, и это отмечено подсказкой строки.
// 2. `msg.buttons` остаётся, но у VK нажимаются только кнопки `text` и
//    `open_link`: `callback`/`vkpay`/`open_app`/`location` требуют
//    group-токена и рисуются ОТКЛЮЧЁННЫМИ (`botkb.js`). Фокус на такую
//    кнопку встанет, нажатие ничего не сделает — это честнее, чем прятать её.
// 3. ДОБАВЛЕНО `msg.attachments`: у VK на одном сообщении до десяти вложений
//    (`msg.attachments` — массив), и «открыть N-е» без клавиатуры недостижимо.
//    У Telegram такого действия не было и быть не могло: там `msg.media` —
//    ОДИН объект, и `nav.activate` покрывал весь случай.
//
// Всё остальное — тот же состав и те же дефолты. Это не лень, а требование
// сопровождения: пользователь, привыкший к `j/k/r/y` в Telegram, обязан
// получить их же в VK, иначе две панели одного приложения ведут себя как две
// разные программы.

import {
    actionForEvent as coreActionForEvent,
    bindingRows as coreBindingRows,
    comboAllowedIn,
    comboFromEvent,
    defaultBindings as coreDefaultBindings,
    findAction as coreFindAction,
    formatCombo,
    groupedRows as coreGroupedRows,
    isEditableTarget,
    keyFromEvent,
    normalizeCombo,
    overridesFromBindings as coreOverridesFromBindings,
    resolveBindings as coreResolveBindings,
    sanitizeOverrides as coreSanitizeOverrides,
} from '../messenger/hotkeys-core.js';

/**
 * Действия, доступные с клавиатуры во вкладке VK.
 *
 * `group` — только для группировки в шпаргалке и настройках. `fixed: true`
 * означает «не переназначается»: такие клавиши зашиты в поведение панели
 * (Escape отменяет), и переезд сломал бы выход из режима.
 *
 * Дефолты подобраны так, чтобы не конфликтовать между собой и не требовать
 * модификаторов: keyboard-only работа предполагает частые нажатия, а
 * двухклавишные аккорды на каждый шаг утомляют. Совпадение с телеграмными
 * дефолтами — намеренное (см. шапку файла).
 */
export const VK_ACTIONS = [
    // ── Перемещение фокуса ──
    {
        id: 'focus.dialogs',
        title: 'Фокус на список бесед',
        group: 'Навигация',
        combo: 'd',
        hint: 'Курсор встаёт на активную беседу; дальше j/k.',
    },
    {
        id: 'focus.log',
        title: 'Фокус на ленту сообщений',
        group: 'Навигация',
        combo: 'm',
        hint: 'Курсор встаёт на последнее сообщение.',
    },
    {
        id: 'focus.input',
        title: 'Фокус на поле ввода',
        group: 'Навигация',
        combo: 'i',
        hint: 'Escape возвращает из поля обратно в ленту.',
    },
    {
        id: 'nav.next',
        title: 'Следующий элемент (вниз)',
        group: 'Навигация',
        combo: 'j',
        hint: 'Беседа, сообщение или вложение — смотря где курсор.',
    },
    {
        id: 'nav.prev',
        title: 'Предыдущий элемент (вверх)',
        group: 'Навигация',
        combo: 'k',
        hint: 'В начале ленты подгружает историю.',
    },
    {
        id: 'nav.activate',
        title: 'Открыть / нажать',
        group: 'Навигация',
        combo: 'Enter',
        hint: 'Открыть беседу, вложение или кнопку сообщества.',
    },
    {
        id: 'dialog.unread',
        title: 'Следующая непрочитанная беседа',
        group: 'Навигация',
        combo: 'u',
        hint: 'По кругу, начиная от текущей.',
    },
    {
        id: 'dir.next',
        title: 'Следующий фильтр',
        group: 'Навигация',
        combo: ']',
        hint: 'Фильтры VK серверные: каждое переключение — запрос.',
    },
    {
        id: 'dir.prev',
        title: 'Предыдущий фильтр',
        group: 'Навигация',
        combo: '[',
    },

    // ── Работа с сообщением под курсором ──
    {
        id: 'msg.reply',
        title: 'Ответить на сообщение',
        group: 'Сообщение',
        combo: 'r',
        hint: 'Полоса ответа + фокус в поле ввода.',
    },
    {
        id: 'msg.quote',
        title: 'Перейти к цитируемому оригиналу',
        group: 'Сообщение',
        combo: 'o',
    },
    {
        id: 'msg.copy',
        title: 'Скопировать текст сообщения',
        group: 'Сообщение',
        combo: 'y',
    },
    {
        id: 'msg.buttons',
        title: 'Перейти к кнопкам сообщества',
        group: 'Сообщение',
        combo: 'b',
        hint: 'Кнопки callback и vkpay отключены: их нельзя нажать с личным токеном.',
    },
    {
        id: 'msg.attachments',
        title: 'Перейти к вложениям сообщения',
        group: 'Сообщение',
        combo: 'v',
        hint: 'Дальше j/k по вложениям и ⏎ — открыть N-е.',
    },
    {
        // Shift+M, а не одиночная клавиша: `v` здесь занята вложениями, `m` в
        // Telegram — фокусом на ленту, а одна и та же комбинация обязана
        // значить одно и то же во всех трёх панелях.
        id: 'msg.meet',
        title: 'Карточка встречи по ссылке',
        group: 'Сообщение',
        combo: 'Shift+m',
        hint: 'На сообщении со ссылкой на встречу. Плагин выключен — клавиша остаётся свободной.',
    },

    // ── Лента и панель ──
    {
        id: 'log.bottom',
        title: 'В конец ленты',
        group: 'Лента',
        combo: 'Shift+g',
    },
    {
        id: 'log.older',
        title: 'Подгрузить историю выше',
        group: 'Лента',
        combo: 'g',
    },
    {
        id: 'ui.attach',
        title: 'Прикрепить файл',
        group: 'Панель',
        combo: 'a',
    },
    {
        id: 'ui.theme',
        title: 'Сменить тему панели',
        group: 'Панель',
        combo: 't',
    },
    {
        id: 'ui.help',
        title: 'Показать эту шпаргалку',
        group: 'Панель',
        combo: 'Shift+/',
    },
    {
        id: 'ui.escape',
        title: 'Отмена / выйти из поля',
        group: 'Панель',
        combo: 'Escape',
        fixed: true,
        hint: 'Не переназначается: это выход из любого режима.',
    },
];

/** Порядок групп в шпаргалке и настройках. */
export const VK_ACTION_GROUPS = ['Навигация', 'Сообщение', 'Лента', 'Панель'];

// ── Чистые функции движка: реестр им не нужен, реэкспорт как есть ──
export {
    comboAllowedIn, comboFromEvent, formatCombo, isEditableTarget, keyFromEvent, normalizeCombo,
};

/** Действие по id или null. */
export function findAction(id) {
    return coreFindAction(VK_ACTIONS, id);
}

/** Карта дефолтных привязок: id → комбинация. */
export function defaultBindings() {
    return coreDefaultBindings(VK_ACTIONS);
}

/**
 * Отбрасывает из карты переопределений всё, что нельзя применить.
 *
 * @param {?object} raw — сырая карта из `user_settings.vk_hotkeys`
 * @returns {Map<string, string>}
 */
export function sanitizeOverrides(raw) {
    return coreSanitizeOverrides(raw, VK_ACTIONS);
}

/**
 * Итоговые привязки с учётом переопределений (два прохода, см. ядро).
 *
 * @param {?object} overrides
 */
export function resolveBindings(overrides) {
    return coreResolveBindings(VK_ACTIONS, overrides);
}

/** Строки для таблицы настроек и шпаргалки. */
export function bindingRows(overrides) {
    return coreBindingRows(VK_ACTIONS, overrides);
}

/** Карта переопределений для backend: ТОЛЬКО отличия от дефолта. */
export function overridesFromBindings(bindings) {
    return coreOverridesFromBindings(VK_ACTIONS, bindings);
}

/** Группы со строками — для шпаргалки и настроек. */
export function groupedRows(overrides) {
    return coreGroupedRows(VK_ACTIONS, VK_ACTION_GROUPS, overrides);
}

/**
 * Действие для события — или null.
 *
 * @param {?object} e — KeyboardEvent
 * @param {{byCombo: Map<string,string>}} resolved — результат resolveBindings
 * @param {{editing?: boolean}} [ctx]
 */
export function actionForEvent(e, resolved, ctx) {
    return coreActionForEvent(e, resolved, ctx);
}
