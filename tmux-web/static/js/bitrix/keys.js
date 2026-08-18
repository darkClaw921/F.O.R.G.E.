// tmux-web — Bitrix24 plugin: реестр действий клавиатуры.
//
// Движка комбинаций здесь НЕТ: он живёт в `js/messenger/hotkeys-core.js` и
// общий на все панели-мессенджеры. Здесь только то, что у панели своё, —
// список действий, их дефолтные клавиши и порядок групп; плюс тонкие обёртки,
// подставляющие реестр первым аргументом, чтобы вызывающий код панели
// (`keynav.js`, форма настроек) не таскал `B24_ACTIONS` через все вызовы.
//
// Файл ЧИСТЫЙ: ни DOM, ни сети, ни импортов из соседних модулей вкладки, — тот
// же принцип, что у `format.js` и `visibility.js`. DOM-часть живёт в
// `keynav.js`, покрытие — `tests/frontend/bitrix_keys.test.mjs`.
//
// ============================================================================
// ОБРАБОТКА ИДЁТ ПО `event.code`, А НЕ ПО `event.key`
// ============================================================================
//
// Панель мессенджера — место, где по-русски печатают чаще, чем по-английски.
// При русской раскладке `event.key` физической клавиши J вернёт `'о'`, и
// хоткей `j` умер бы ровно у целевой аудитории. Ядро сопоставляет по
// `event.code` (физическая клавиша), `event.key` остаётся фолбэком для клавиш
// без кода. Здесь это упомянуто, потому что менять реестр будут отсюда, а не
// из ядра.
//
// ============================================================================
// ЧЕМ РЕЕСТР ОТЛИЧАЕТСЯ ОТ VK-ШНОГО (`js/vk/keys.js::VK_ACTIONS`)
// ============================================================================
//
// 1. `dir.next` / `dir.prev` ходят по ЧЕТЫРЁМ ЛОКАЛЬНЫМ директориям
//    (`~/all` / `~/chats` / `~/tasks` / `~/unread`). У VK это СЕРВЕРНЫЕ
//    фильтры, и каждое переключение стоило запроса; здесь портал отдаёт ОДИН
//    список `im.recent.get`, а нарезка считается на клиенте. Значит удерживать
//    `]` нажатым безопасно — сети за этим нет вовсе, и подсказка строки об
//    этом прямо говорит.
// 2. ДОБАВЛЕНО `msg.edit`: портал разрешает автору править своё сообщение
//    (`im.v2.Message.update`), у VK такого действия в панели не было.
// 3. ДОБАВЛЕН `msg.like`: лайк — ЕДИНСТВЕННАЯ реакция, доступная вебхуку
//    (§6 плана). Набора эмодзи нет и не будет, поэтому одна клавиша покрывает
//    всю тему реакций.
// 4. ДОБАВЛЕНА ГРУППА «Задача» (`task.card`, `task.actions`) — того, чего нет
//    ни у Telegram, ни у VK: чат задачи это обычный IM-чат с
//    `entity_type="TASKS_TASK"`, над лентой которого висит карточка с
//    матрицей действий. Без клавиш до неё пришлось бы тянуться мышью, а это
//    самая частая операция в директории `~/tasks`.
// 5. УБРАН `msg.quote`: в ленте Битрикс24 цитаты отдельным узлом НЕТ —
//    ответ живёт полосой над полем ввода (`#b24-prompt-note`), и «перейти к
//    оригиналу» переходить некуда.
//
// Всё остальное — тот же состав и те же дефолты, что у соседей. Это не лень, а
// требование сопровождения: пользователь, привыкший к `j/k/r/y` в Telegram и
// VK, обязан получить их же здесь, иначе три панели одного приложения ведут
// себя как три разные программы.

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
 * Действия, доступные с клавиатуры во вкладке «Битрикс24».
 *
 * `group` — только для группировки в шпаргалке и настройках. `fixed: true`
 * означает «не переназначается»: такие клавиши зашиты в поведение панели
 * (Escape отменяет), и переезд сломал бы выход из режима.
 *
 * Дефолты подобраны так, чтобы не конфликтовать между собой и не требовать
 * модификаторов: keyboard-only работа предполагает частые нажатия, а
 * двухклавишные аккорды на каждый шаг утомляют. Совпадение с дефолтами
 * соседних панелей — намеренное (см. шапку файла).
 */
export const B24_ACTIONS = [
    // ── Перемещение фокуса ──
    {
        id: 'focus.dialogs',
        title: 'Фокус на список диалогов',
        group: 'Навигация',
        combo: 'd',
        hint: 'Курсор встаёт на активный диалог; дальше j/k.',
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
        hint: 'Диалог, сообщение или вложение — смотря где курсор.',
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
        hint: 'Открыть диалог, вложение или кнопку бота.',
    },
    {
        id: 'dialog.unread',
        title: 'Следующий непрочитанный диалог',
        group: 'Навигация',
        combo: 'u',
        hint: 'По кругу, начиная от текущего.',
    },
    {
        id: 'dir.next',
        title: 'Следующая директория',
        group: 'Навигация',
        combo: ']',
        hint: 'Директории локальные: переключение не стоит запроса к порталу.',
    },
    {
        id: 'dir.prev',
        title: 'Предыдущая директория',
        group: 'Навигация',
        combo: '[',
    },

    // ── Работа с сообщением под курсором ──
    {
        id: 'msg.reply',
        title: 'Ответить на сообщение',
        group: 'Сообщение',
        combo: 'r',
        hint: 'Полоса ответа над полем ввода + фокус в поле.',
    },
    {
        id: 'msg.edit',
        title: 'Изменить своё сообщение',
        group: 'Сообщение',
        combo: 'e',
        hint: 'Только своё: портал правку чужого не разрешает.',
    },
    {
        id: 'msg.like',
        title: 'Лайк сообщения',
        group: 'Сообщение',
        combo: 'l',
        hint: 'Единственная реакция, доступная вебхуку.',
    },
    {
        id: 'msg.copy',
        title: 'Скопировать текст сообщения',
        group: 'Сообщение',
        combo: 'y',
    },
    {
        id: 'msg.buttons',
        title: 'Перейти к кнопкам бота',
        group: 'Сообщение',
        combo: 'b',
        hint: 'Нажимаются только кнопки-команды; остальные нарисованы отключёнными.',
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

    // ── Карточка задачи над лентой ──
    {
        id: 'task.card',
        title: 'Перейти к карточке задачи',
        group: 'Задача',
        combo: 'c',
        hint: 'Работает в чате задачи; в обычном чате карточки нет.',
    },
    {
        id: 'task.actions',
        title: 'Перейти к действиям задачи',
        group: 'Задача',
        combo: 'x',
        hint: 'Кнопки принять / начать / завершить — те, что портал сейчас разрешает.',
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
export const B24_ACTION_GROUPS = ['Навигация', 'Сообщение', 'Задача', 'Лента', 'Панель'];

// ── Чистые функции движка: реестр им не нужен, реэкспорт как есть ──
export {
    comboAllowedIn, comboFromEvent, formatCombo, isEditableTarget, keyFromEvent, normalizeCombo,
};

/** Действие по id или null. */
export function findAction(id) {
    return coreFindAction(B24_ACTIONS, id);
}

/** Карта дефолтных привязок: id → комбинация. */
export function defaultBindings() {
    return coreDefaultBindings(B24_ACTIONS);
}

/**
 * Отбрасывает из карты переопределений всё, что нельзя применить.
 *
 * @param {?object} raw — сырая карта из `user_settings.bitrix_hotkeys`
 * @returns {Map<string, string>}
 */
export function sanitizeOverrides(raw) {
    return coreSanitizeOverrides(raw, B24_ACTIONS);
}

/**
 * Итоговые привязки с учётом переопределений (два прохода, см. ядро).
 *
 * @param {?object} overrides
 */
export function resolveBindings(overrides) {
    return coreResolveBindings(B24_ACTIONS, overrides);
}

/** Строки для таблицы настроек и шпаргалки. */
export function bindingRows(overrides) {
    return coreBindingRows(B24_ACTIONS, overrides);
}

/** Карта переопределений для backend: ТОЛЬКО отличия от дефолта. */
export function overridesFromBindings(bindings) {
    return coreOverridesFromBindings(B24_ACTIONS, bindings);
}

/** Группы со строками — для шпаргалки и настроек. */
export function groupedRows(overrides) {
    return coreGroupedRows(B24_ACTIONS, B24_ACTION_GROUPS, overrides);
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
