// tmux-web — Bitrix24 plugin: клавиатурная навигация по панели.
//
// Задача модуля: пройти весь путь «выбрать диалог → прочитать → открыть
// вложение → ответить → отправить → выполнить действие над задачей» вообще не
// трогая мышь. Чистая логика (реестр действий, разбор комбинаций, конфликты)
// вынесена в keys.js и общий `messenger/hotkeys-core.js` — здесь только DOM,
// фокус и состояние курсора.
//
// ============================================================================
// КАК МОДУЛЬ УЖИВАЕТСЯ С ОСТАЛЬНЫМИ ОБРАБОТЧИКАМИ
// ============================================================================
//
// 1. `static/hotkeys.js` (vim-навигация по сессиям) занимает j/k/h/l/g/1/2/3/?
//    глобально. Пока открыта вкладка «Битрикс24», он уступает — гейт
//    [`b24OwnsKeyboard`] вызывается им через `window.ForgeApp.b24Keys`.
//    УСТУПАЕТ НЕ «ВСЕГДА, КОГДА ПЛАГИН ВКЛЮЧЁН», а только когда фокус реально
//    внутри панели (или нигде): кликнув в сайдбар сессий, пользователь снова
//    получает vim. Имя `b24Keys` панель регистрирует в реестре
//    `window.ForgeApp.keyboardOwners` САМА (см. [`initB24Keynav`]) — реестр
//    введён в hotkeys.js ровно на этой, третьей по счёту, панели.
// 2. Панели Telegram и VK владеют клавиатурой по такому же контракту и в то же
//    время. Конфликт невозможен по построению: `switchTab` держит непогашенным
//    ровно один пейн, а `panelVisible()` каждой панели смотрит на СВОЙ
//    `#telegram` / `#vk` / `#bitrix`. Три document-слушателя в capture — цена в
//    один `if` на нажатие.
// 3. Слушатель ставится в capture-фазе на document, но НЕ глушит события
//    вслепую: `preventDefault`/`stopPropagation` зовутся только когда действие
//    ДЕЙСТВИТЕЛЬНО ВЫПОЛНЕНО. Ложное `true` съело бы клавишу у соседей, ничего
//    не выполнив.
// 4. Внутри полей ввода одиночные клавиши не перехватываются вовсе — иначе
//    сообщение с буквой «j» было бы не набрать (hotkeys-core::comboAllowedIn).
// 5. Escape в поле ввода при активном режиме ответа/правки НЕ перехватывается:
//    его гасит собственный обработчик `chat.js` (он же и знает, что именно
//    отменять). Перехват поверх него означал бы «Esc иногда снимает цитату, а
//    иногда просто уводит фокус» — поведение, которое невозможно объяснить.
//
// ============================================================================
// ОТЛИЧИЯ ОТ VK-ШНОГО БЛИЗНЕЦА
// ============================================================================
//
// • НЕТ captcha-оверлея (у портала его не бывает вовсе) — гейт видимости
//   проверяет только `#bitrix` и `#b24-login`.
// • ЕСТЬ ЗОНА КАРТОЧКИ ЗАДАЧИ (`#b24-task-card`): она висит НАД лентой и несёт
//   кнопки действий портала. Ни у Telegram, ни у VK такой зоны нет.
// • Действия над сообщением выполняются НАЖАТИЕМ КНОПКИ СТРОКИ
//   (`.b24-line-btn[data-action]`), а не прямым вызовом `chat.js`. Так
//   клавиатура и мышь идут одним путём, а «действие недоступно» (чужое
//   сообщение, удалённое, ещё летящее) выражается отсутствием кнопки — то есть
//   честным `false`, при котором клавиша достаётся соседям.
//
// ============================================================================
// КУРСОР
// ============================================================================
//
// Курсор хранится не ссылкой на узел, а ИДЕНТИФИКАТОРОМ (dialogId / message
// id): и колонка, и лента перерисовываются целиком при каждой подгрузке, и
// ссылка на узел протухала бы после любого обновления. Ключ ленты — `message.id`
// (в отличие от VK, где им был `cmid`): у портала идентификатор сообщения
// глобальный, второй нумерации внутри чата нет. Класс `b24-kb-cursor`
// навешивается на найденный по id узел при каждом перемещении.

import { inputMode } from './chat.js';
import {
    actionForEvent, groupedRows, isEditableTarget, resolveBindings,
} from './keys.js';
import { isB24NarrowScreen, setB24ContactsOpen } from './mobile.js';

/** CSS-класс курсора клавиатуры. */
export const KB_CURSOR_CLASS = 'b24-kb-cursor';

const local = {
    bound: false,
    /** Зона курсора: 'dialogs' | 'log' | null. */
    zone: null,
    /** id диалога под курсором (строка) — переживает перерисовку колонки. */
    dialogCursor: null,
    /** message.id под курсором (строка) — переживает перерисовку ленты. */
    msgCursor: null,
    /** Узел оверлея шпаргалки. */
    helpEl: null,
    /** Кэш разбора привязок: пересчитывать на каждое нажатие незачем. */
    cacheKey: null,
    cacheResolved: null,
    /** Колбэк для коротких уведомлений (инжектится из main.js). */
    onNotice: null,
    /** Последнее выполненное действие — для тестов и диагностики. */
    lastAction: null,
};

// ---------------- доступ к настройкам ----------------

/**
 * Настройки пользователя из публичного контракта.
 *
 * Читаются ЛЕНИВО в момент нажатия — ровно как в `hotkeys.js`: модуль может
 * загрузиться раньше, чем приедет ответ `/api/user-settings`, и снимок,
 * взятый на старте, застыл бы навсегда.
 */
function userSettings() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    return (app && app.state && app.state.userSettings) || null;
}

/**
 * Включены ли хоткеи.
 *
 * Сравнение `!== false`, а НЕ `=== true`: дефолт этой настройки — ВКЛЮЧЕНО
 * (`user_settings.rs::default_bitrix_hotkeys_enabled`), поэтому отсутствие поля
 * (старый файл настроек, backend не ответил) обязано означать «работают», а не
 * «выключены».
 *
 * Свод гейтов дефолтов, который тут легче всего перепутать:
 *   cmd_hints_enabled        === true  (дефолт ВЫКЛ, hotkeys.js);
 *   telegram_hotkeys_enabled !== false (дефолт ВКЛ);
 *   vk_hotkeys_enabled       !== false (дефолт ВКЛ);
 *   bitrix_hotkeys_enabled   !== false (дефолт ВКЛ, здесь);
 *   bitrix_filter_enabled    === true  (дефолт ВЫКЛ, visibility.js).
 */
export function hotkeysEnabled() {
    const s = userSettings();
    if (!s) return true;
    return s.bitrix_hotkeys_enabled !== false;
}

/** Сырая карта переопределений из настроек. */
function overridesFromSettings() {
    const s = userSettings();
    const raw = s && s.bitrix_hotkeys;
    return (raw && typeof raw === 'object') ? raw : null;
}

/**
 * Разобранные привязки с кэшем.
 *
 * Ключ кэша — сериализованная карта переопределений, а НЕ ссылка на объект
 * настроек. Разница принципиальна: сохранённые настройки начинают действовать
 * сразу, без перезагрузки страницы, независимо от того, положил ли
 * user-settings-api новый объект или мутировал старый. Пересчёт при этом
 * происходит только когда карта реально изменилась, а нажатий между
 * сохранениями тысячи.
 */
export function currentBindings() {
    const raw = overridesFromSettings();
    let key;
    try {
        key = JSON.stringify(raw || {});
    } catch (_) {
        key = null;
    }
    if (key !== null && key === local.cacheKey && local.cacheResolved) {
        return local.cacheResolved;
    }
    const resolved = resolveBindings(raw);
    local.cacheKey = key;
    local.cacheResolved = resolved;
    return resolved;
}

/**
 * Сбрасывает кэш привязок.
 *
 * Страховка, а не обязательный шаг: кэш и так самообновляется по содержимому
 * карты (см. [`currentBindings`]). Нужна на случай, когда сериализация карты не
 * удалась, и как явная точка в контракте для формы настроек.
 */
export function invalidateBindings() {
    local.cacheKey = null;
    local.cacheResolved = null;
}

// ---------------- элементы панели ----------------

function el(id) {
    if (typeof document === 'undefined' || !document.getElementById) return null;
    return document.getElementById(id);
}

function panel() {
    return el('bitrix');
}

/** Видна ли вкладка «Битрикс24» и готова ли она к навигации. */
export function panelVisible() {
    const p = panel();
    if (!p) return false;
    if (p.hidden === true) return false;
    // Оверлей подключения закрывает панель целиком: пока в нём, навигация по
    // несуществующим диалогам смысла не имеет.
    const login = el('b24-login');
    if (login && login.hidden === false) return false;
    // Попап анализа: в нём два поля ввода (вопрос модели и заметка о чате), и
    // без этой ветки vim-навигация забирала бы буквы `j`/`k` себе — вопрос
    // стало бы физически не набрать (следствие читает static/hotkeys.js).
    // Esc при этом продолжает закрывать попап: его слушатель живёт в ядре
    // попапа (js/messenger/analysis-core.js), а не здесь.
    const analysis = el('b24-analysis');
    if (analysis && analysis.hidden === false) return false;
    return true;
}

/**
 * Владеет ли панель клавиатурой прямо сейчас.
 *
 * Спрашивает `hotkeys.js`, чтобы решить, гасить ли свою vim-навигацию. Фокус
 * вне панели (сайдбар сессий, модалка настроек) означает «не владеет»: иначе
 * открытая вкладка отняла бы j/k у всего остального интерфейса.
 */
export function b24OwnsKeyboard() {
    if (!panelVisible()) return false;
    if (!hotkeysEnabled()) return false;
    const active = (typeof document !== 'undefined') ? document.activeElement : null;
    if (!active || active === document.body) return true;
    const p = panel();
    if (p && typeof p.contains === 'function' && p.contains(active)) return true;
    return false;
}

// ---------------- курсор ----------------

function dialogRows() {
    const list = el('b24-dialog-list');
    if (!list || typeof list.querySelectorAll !== 'function') return [];
    return Array.from(list.querySelectorAll('.b24-dialog'));
}

function messageRows() {
    const log = el('b24-log');
    if (!log || typeof log.querySelectorAll !== 'function') return [];
    // Системные строки курсору не нужны: на них нечего ответить и нечего
    // открыть. Фильтр по классу, а не селектором `:not(...)`: DOM-стенд
    // node-теста разбирает простые селекторы, а составные — нет.
    return Array.from(log.querySelectorAll('.b24-line'))
        .filter((row) => row.classList && !row.classList.contains('b24-line-system'));
}

function dirRows() {
    const list = el('b24-dir-list');
    if (!list || typeof list.querySelectorAll !== 'function') return [];
    return Array.from(list.querySelectorAll('.b24-dir'));
}

/**
 * По одной кнопке OPEN на каждое вложение строки.
 *
 * Шаг курсора в зоне вложений — это шаг по ВЛОЖЕНИЯМ, а не по кнопкам: у
 * карточки их две (открыть и сохранить), и хождение по обеим означало бы два
 * нажатия `j` на одно вложение. Берём первую `.b24-media-btn` карточки —
 * `attachments.js` добавляет её раньше остальных.
 */
function mediaButtons(row) {
    if (!row || typeof row.querySelectorAll !== 'function') return [];
    return Array.from(row.querySelectorAll('.b24-media-card'))
        .map((card) => (typeof card.querySelector === 'function' ? card.querySelector('.b24-media-btn') : null))
        .filter(Boolean);
}

/** Кнопки действий карточки задачи (принять / начать / завершить …). */
function taskButtons() {
    const card = el('b24-task-card');
    if (!card || card.hidden === true || typeof card.querySelectorAll !== 'function') return [];
    return Array.from(card.querySelectorAll('.b24-task-btn')).filter((b) => !b.disabled);
}

/**
 * Кнопка действия в строке под курсором.
 *
 * `null` означает «портал этого не разрешает»: у чужого сообщения нет ни
 * правки, ни удаления, у удалённого нет вовсе ничего, у ещё летящего — тоже.
 * Возврат `null` доходит до `runAction` как `false`, и клавиша достаётся
 * соседним обработчикам вместо того, чтобы пропасть впустую.
 *
 * @param {?object} row
 * @param {string} action — значение `data-action` (`like`|`reply`|`edit`|…)
 * @returns {?object}
 */
function rowActionButton(row, action) {
    if (!row || typeof row.querySelectorAll !== 'function') return null;
    const found = Array.from(row.querySelectorAll('.b24-line-btn'))
        .find((b) => b.dataset && b.dataset.action === action);
    return found || null;
}

function clearCursorClass() {
    const p = panel();
    if (!p || typeof p.querySelectorAll !== 'function') return;
    for (const node of Array.from(p.querySelectorAll('.' + KB_CURSOR_CLASS))) {
        node.classList.remove(KB_CURSOR_CLASS);
    }
}

function markCursor(node) {
    clearCursorClass();
    if (!node) return;
    node.classList.add(KB_CURSOR_CLASS);
    // Строки ленты не в таб-порядке (их сотни) — фокусируем программно, чтобы
    // работали скринридер и последующие Enter/стрелки.
    if (node.tabIndex === undefined || node.tabIndex < 0) node.tabIndex = -1;
    try { node.focus({ preventScroll: true }); } catch (_) { try { node.focus(); } catch (_) { /* нет фокуса */ } }
    if (typeof node.scrollIntoView === 'function') {
        try { node.scrollIntoView({ block: 'nearest' }); } catch (_) { node.scrollIntoView(); }
    }
}

/** Узел диалога под курсором (или активного диалога, если курсор не ставили). */
function cursorDialogNode() {
    const rows = dialogRows();
    if (!rows.length) return null;
    const wanted = local.dialogCursor !== null ? String(local.dialogCursor) : null;
    if (wanted) {
        const hit = rows.find((r) => r.dataset && String(r.dataset.dialogId) === wanted);
        if (hit) return hit;
    }
    const active = rows.find((r) => r.classList && r.classList.contains('active'));
    if (active) return active;
    return rows[0];
}

/** Узел сообщения под курсором (или последнего сообщения). */
function cursorMessageNode() {
    const rows = messageRows();
    if (!rows.length) return null;
    if (local.msgCursor !== null) {
        const hit = rows.find((r) => r.dataset && String(r.dataset.msgId) === String(local.msgCursor));
        if (hit) return hit;
    }
    return rows[rows.length - 1];
}

function setDialogCursor(node) {
    if (!node) return false;
    local.zone = 'dialogs';
    local.dialogCursor = node.dataset ? String(node.dataset.dialogId) : null;
    markCursor(node);
    return true;
}

function setMessageCursor(node) {
    if (!node) return false;
    local.zone = 'log';
    local.msgCursor = node.dataset ? String(node.dataset.msgId) : null;
    markCursor(node);
    return true;
}

/** Снимает курсор и подсветку (Escape, смена диалога, teardown). */
export function clearCursor() {
    clearCursorClass();
    local.zone = null;
    local.msgCursor = null;
}

/**
 * Зона, в которой сейчас находится пользователь.
 *
 * Считается по РЕАЛЬНОМУ фокусу, а не по сохранённому `local.zone`: мышь и Tab
 * двигают фокус мимо нас, и врать о положении курсора после этого нельзя.
 *
 * Порядок проверок — от вложенного к внешнему: карточка вложения и клавиатура
 * бота лежат ВНУТРИ `#b24-log`, и проверь мы ленту первой, обе зоны были бы
 * недостижимы.
 */
export function activeZone() {
    const active = (typeof document !== 'undefined') ? document.activeElement : null;
    if (active && typeof active.closest === 'function') {
        if (active.closest('#b24-prompt')) return 'input';
        if (active.closest('#b24-task-card')) return 'task';
        if (active.closest('.b24-kb')) return 'keyboard';
        if (active.closest('.b24-media-card')) return 'media';
        if (active.closest('#b24-log')) return 'log';
        if (active.closest('#b24-contacts')) return 'dialogs';
    }
    return local.zone;
}

// ---------------- действия ----------------

function notice(text) {
    if (typeof local.onNotice === 'function') {
        try { local.onNotice(text); } catch (_) { /* уведомление — не критично */ }
    }
}

function focusDialogs() {
    // На узком экране колонка уезжает за край: фокусировать невидимое — значит
    // подарить пользователю «мёртвые» нажатия.
    if (isB24NarrowScreen()) setB24ContactsOpen(true);
    return setDialogCursor(cursorDialogNode());
}

function focusLog() {
    return setMessageCursor(cursorMessageNode());
}

function focusInput() {
    const input = el('b24-input');
    if (!input) return false;
    try { input.focus(); } catch (_) { return false; }
    return true;
}

function moveWithin(rows, current, delta) {
    if (!rows.length) return null;
    const idx = current ? rows.indexOf(current) : -1;
    if (idx < 0) return delta > 0 ? rows[0] : rows[rows.length - 1];
    const next = idx + delta;
    if (next < 0 || next >= rows.length) return null;
    return rows[next];
}

function navMove(delta) {
    const zone = activeZone();
    if (zone === 'dialogs') {
        const rows = dialogRows();
        const next = moveWithin(rows, cursorDialogNode(), delta);
        // Упёрлись в край списка — курсор остаётся на месте, но нажатие
        // считаем обработанным: иначе оно провалилось бы в vim и увело фокус на
        // сессию в сайдбаре.
        return next ? setDialogCursor(next) : true;
    }
    if (zone === 'keyboard') return moveButtonFocus(buttonsOf('.b24-kb'), delta);
    if (zone === 'media') return moveMediaFocus(delta);
    if (zone === 'task') return moveButtonFocus(taskButtons(), delta);
    // Всё остальное (в том числе курсор не ставили) — лента.
    const rows = messageRows();
    const next = moveWithin(rows, cursorMessageNode(), delta);
    if (next) return setMessageCursor(next);
    if (delta < 0) {
        // Верх ленты: докручиваем вверх и отдаём подгрузку истории штатному
        // обработчику скролла (chat.js::onScroll) — дублировать loadOlder здесь
        // значило бы завести вторую точку правды о пагинации.
        scrollLogBy(-1);
    }
    return true;
}

/** Кнопки контейнера, внутри которого сейчас фокус. */
function buttonsOf(containerSelector) {
    const active = (typeof document !== 'undefined') ? document.activeElement : null;
    const box = (active && typeof active.closest === 'function') ? active.closest(containerSelector) : null;
    if (!box || typeof box.querySelectorAll !== 'function') return [];
    return Array.from(box.querySelectorAll('.b24-kb-btn')).filter((b) => !b.disabled);
}

function moveButtonFocus(btns, delta) {
    if (!btns.length) return false;
    const idx = btns.indexOf(document.activeElement);
    const next = btns[Math.max(0, Math.min(btns.length - 1, (idx < 0 ? 0 : idx + delta)))];
    if (!next) return false;
    try { next.focus(); } catch (_) { return false; }
    return true;
}

function moveMediaFocus(delta) {
    const active = document.activeElement;
    const card = (active && typeof active.closest === 'function') ? active.closest('.b24-media-card') : null;
    const row = (card && typeof card.closest === 'function') ? card.closest('.b24-line') : null;
    const btns = mediaButtons(row);
    if (!btns.length) return false;
    const idx = btns.indexOf(active);
    const next = btns[Math.max(0, Math.min(btns.length - 1, (idx < 0 ? 0 : idx + delta)))];
    if (!next) return false;
    try { next.focus(); } catch (_) { return false; }
    return true;
}

function activate() {
    const zone = activeZone();
    if (zone === 'dialogs') {
        const node = cursorDialogNode();
        if (!node) return false;
        node.click();
        // На узком экране колонка перекрывает ленту — после выбора её закрывает
        // сам dialogs.js, нам остаётся увести курсор в ленту.
        local.zone = 'log';
        local.msgCursor = null;
        return true;
    }
    const active = document.activeElement;
    // Фокус уже на кнопке (клавиатура бота, вложение, действие задачи) — НЕ
    // трогаем: `<button>` активируется Enter'ом штатно, силами браузера.
    // Нажать её здесь самим значило бы поставить свой click рядом с нативным и
    // рискнуть отправить порталу ДВА действия на одно нажатие — а это у задачи
    // может быть и «завершить», и «отложить». Возвращаем false, чтобы событие
    // не гасилось.
    if (active && typeof active.closest === 'function'
        && (active.closest('.b24-kb-btn') || active.closest('.b24-media-btn')
            || active.closest('.b24-task-btn'))) {
        return false;
    }
    const row = cursorMessageNode();
    if (!row) return false;
    // Приоритет: вложение (его открытие — самое ожидаемое действие на строке с
    // картинкой), затем кнопка бота.
    //
    // Открываем именно кнопкой, а не кликом по телу карточки: тело реагирует
    // только у превью и проигрываемых видов (media.js), и на документе клик по
    // нему был бы пустышкой — то есть съеденной клавишей без единого видимого
    // следствия.
    const openable = mediaButtons(row).find((b) => !b.disabled);
    if (openable) { openable.click(); return true; }
    const btn = row.querySelector('.b24-kb-btn');
    if (btn && !btn.disabled) { btn.click(); return true; }
    return false;
}

/**
 * Нажимает кнопку действия у строки под курсором.
 *
 * Одна функция на `msg.reply` / `msg.edit` / `msg.like`: разница между ними —
 * только значение `data-action`, а поведение (курсор встаёт на строку, кнопки
 * нет — действие не выполнено) общее.
 *
 * @param {string} action
 * @param {string} denied — что сказать, если портал этого не разрешает
 * @returns {boolean}
 */
function pressRowAction(action, denied) {
    const row = cursorMessageNode();
    if (!row) return false;
    const btn = rowActionButton(row, action);
    if (!btn) {
        notice(denied);
        // true: сообщение показано, значит нажатие не пропало впустую. Гасим —
        // иначе следом сработал бы vim и увёл фокус в сайдбар, спрятав только
        // что показанное уведомление.
        return true;
    }
    setMessageCursor(row);
    btn.click();
    return true;
}

function copyCursorText() {
    const row = cursorMessageNode();
    const node = row && typeof row.querySelector === 'function' ? row.querySelector('.b24-line-text') : null;
    const text = node ? String(node.textContent || '') : '';
    if (!text) return false;
    const nav = (typeof navigator !== 'undefined') ? navigator : null;
    if (!nav || !nav.clipboard || typeof nav.clipboard.writeText !== 'function') {
        notice('Копирование недоступно в этом браузере');
        return true;
    }
    nav.clipboard.writeText(text).then(
        () => notice('Скопировано'),
        () => notice('Не удалось скопировать'),
    );
    return true;
}

function focusButtons() {
    const row = cursorMessageNode();
    const target = row && typeof row.querySelector === 'function' ? row.querySelector('.b24-kb-btn') : null;
    if (!target) {
        notice('У этого сообщения нет кнопок');
        return true;
    }
    try { target.focus(); } catch (_) { return false; }
    return true;
}

/**
 * Переход к вложениям сообщения под курсором.
 *
 * У сообщения портала вложений бывает несколько (`files[]`), и без этого шага
 * открыть, скажем, третий файл с клавиатуры было бы нечем.
 */
function focusAttachments() {
    const row = cursorMessageNode();
    const btns = mediaButtons(row);
    if (!btns.length) {
        notice('У этого сообщения нет вложений');
        return true;
    }
    try { btns[0].focus(); } catch (_) { return false; }
    return true;
}

/**
 * Перевод фокуса на карточку задачи над лентой.
 *
 * В обычном чате карточки нет вовсе (она скрыта `hidden`), и это не ошибка:
 * сообщаем и гасим клавишу, чтобы уведомление не смело vim-навигацией.
 */
function focusTaskCard() {
    const card = el('b24-task-card');
    if (!card || card.hidden === true) {
        notice('Это не чат задачи');
        return true;
    }
    if (card.tabIndex === undefined || card.tabIndex < 0) card.tabIndex = -1;
    try { card.focus({ preventScroll: true }); } catch (_) { try { card.focus(); } catch (_) { return false; } }
    local.zone = 'task';
    return true;
}

/**
 * Перевод фокуса на кнопки действий задачи.
 *
 * Матрицу действий считает портал (`tasks.task.get` → `action`), и кнопок в
 * карточке ровно столько, сколько он разрешает СЕЙЧАС. Пустой набор —
 * нормальное состояние (наблюдатель задачи), а не сбой.
 */
function focusTaskActions() {
    const btns = taskButtons();
    if (!btns.length) {
        notice('Портал не разрешает сейчас ни одного действия');
        return true;
    }
    try { btns[0].focus(); } catch (_) { return false; }
    local.zone = 'task';
    return true;
}

function scrollLogBy(direction) {
    const log = el('b24-log');
    if (!log) return false;
    const step = Math.max(80, Math.floor((log.clientHeight || 400) * 0.9));
    log.scrollTop = Math.max(0, log.scrollTop + step * direction);
    return true;
}

function scrollLogBottom() {
    const log = el('b24-log');
    if (!log) return false;
    log.scrollTop = log.scrollHeight;
    // Курсор уезжает вместе с лентой: иначе следующее j/k вернуло бы экран
    // туда, где пользователь только что был, и прыжок выглядел бы поломкой.
    //
    // Именно setMessageCursor, а не присваивание local.msgCursor: подсветка
    // обязана переехать вместе с курсором, иначе видимая рамка и логическая
    // позиция разъезжаются.
    const rows = messageRows();
    if (rows.length) setMessageCursor(rows[rows.length - 1]);
    return true;
}

function clickById(id) {
    const node = el(id);
    if (!node) return false;
    node.click();
    return true;
}

function moveDir(delta) {
    const rows = dirRows();
    if (!rows.length) return false;
    const idx = rows.findIndex((r) => r.classList && r.classList.contains('active'));
    const base = idx < 0 ? 0 : idx;
    const next = rows[(base + delta + rows.length) % rows.length];
    if (!next) return false;
    // Директории у Битрикс24 ЛОКАЛЬНЫЕ: переключение не стоит запроса к порталу
    // (в отличие от VK, где это серверный фильтр). Дебаунса здесь поэтому нет и
    // не нужно.
    next.click();
    // Список пересобран — курсор на прежний id больше не наводится.
    local.dialogCursor = null;
    return true;
}

function nextUnread() {
    const rows = dialogRows();
    if (!rows.length) return false;
    const unread = rows.filter((r) => {
        const badge = typeof r.querySelector === 'function' ? r.querySelector('.b24-dialog-unread') : null;
        return badge && String(badge.textContent || '').trim() !== '';
    });
    if (!unread.length) {
        notice('Непрочитанных диалогов нет');
        return true;
    }
    const current = cursorDialogNode();
    const after = unread.filter((r) => rows.indexOf(r) > rows.indexOf(current));
    // По кругу: дойдя до конца списка, начинаем сначала — иначе последний
    // непрочитанный стал бы тупиком.
    return setDialogCursor(after.length ? after[0] : unread[0]);
}

function onEscape() {
    if (helpVisible()) { hideHelp(); return true; }
    const zone = activeZone();
    if (zone === 'input') {
        // Режим ответа/правки гасит собственный обработчик chat.js (он же знает,
        // что именно отменять и стирать ли текст) — перехватывать Escape поверх
        // него значило бы отменять не то, чего ждёт пользователь.
        if (inputMode()) return false;
        const input = el('b24-input');
        if (input && typeof input.blur === 'function') { try { input.blur(); } catch (_) { /* ok */ } }
        return focusLog();
    }
    if (zone === 'keyboard' || zone === 'media' || zone === 'task') return focusLog();
    if (local.zone || document.querySelector('.' + KB_CURSOR_CLASS)) {
        clearCursor();
        return true;
    }
    return false;
}

// ---------------- шпаргалка ----------------

function helpVisible() {
    return !!(local.helpEl && !local.helpEl.hidden);
}

/**
 * Показывает оверлей со списком действующих комбинаций.
 *
 * Строится заново на каждый показ: пользователь мог переназначить клавиши в
 * настройках, и закэшированная карточка врала бы.
 */
export function showHelp() {
    const p = panel();
    if (!p) return false;
    if (local.helpEl) local.helpEl.remove();
    const overlay = document.createElement('div');
    overlay.className = 'b24-keys-help';
    overlay.id = 'b24-keys-help';

    const card = document.createElement('div');
    card.className = 'b24-keys-help-card';

    const title = document.createElement('div');
    title.className = 'b24-keys-help-title';
    title.textContent = '// ГОРЯЧИЕ КЛАВИШИ';
    card.appendChild(title);

    for (const group of groupedRows(overridesFromSettings())) {
        const gEl = document.createElement('div');
        gEl.className = 'b24-keys-help-group';
        const gTitle = document.createElement('div');
        gTitle.className = 'b24-keys-help-group-title';
        gTitle.textContent = group.name;
        gEl.appendChild(gTitle);
        for (const row of group.items) {
            const line = document.createElement('div');
            line.className = 'b24-keys-help-row';
            const key = document.createElement('kbd');
            key.className = 'b24-keys-help-key';
            key.textContent = row.combo ? row.label : '—';
            if (!row.combo) key.classList.add('b24-keys-help-key-none');
            const label = document.createElement('span');
            label.className = 'b24-keys-help-label';
            label.textContent = row.title;
            line.appendChild(key);
            line.appendChild(label);
            if (row.hint) {
                const hint = document.createElement('span');
                hint.className = 'b24-keys-help-hint';
                hint.textContent = row.hint;
                line.appendChild(hint);
            }
            gEl.appendChild(line);
        }
        card.appendChild(gEl);
    }

    const foot = document.createElement('div');
    foot.className = 'b24-keys-help-foot';
    foot.textContent = 'Переназначить и выключить — Настройки → Битрикс24. Esc — закрыть.';
    card.appendChild(foot);

    overlay.appendChild(card);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) hideHelp(); });
    p.appendChild(overlay);
    local.helpEl = overlay;
    return true;
}

/** Прячет шпаргалку. */
export function hideHelp() {
    if (!local.helpEl) return false;
    local.helpEl.remove();
    local.helpEl = null;
    return true;
}

function toggleHelp() {
    return helpVisible() ? hideHelp() : showHelp();
}

// ---------------- диспетчер ----------------

/**
 * Выполняет действие по его id.
 *
 * Возвращает `true`, если действие реально что-то сделало — только тогда
 * событие будет погашено. Ложное `true` вредно: оно съело бы клавишу у соседних
 * обработчиков, ничего не выполнив.
 *
 * @param {string} id
 * @returns {boolean}
 */
export function runAction(id) {
    local.lastAction = id;
    switch (id) {
        case 'focus.dialogs': return focusDialogs();
        case 'focus.log': return focusLog();
        case 'focus.input': return focusInput();
        case 'nav.next': return navMove(+1);
        case 'nav.prev': return navMove(-1);
        case 'nav.activate': return activate();
        case 'dialog.unread': return nextUnread();
        case 'dir.next': return moveDir(+1);
        case 'dir.prev': return moveDir(-1);
        case 'msg.reply': return pressRowAction('reply', 'Ответить на это сообщение нельзя');
        case 'msg.edit': return pressRowAction('edit', 'Портал разрешает править только своё сообщение');
        case 'msg.like': return pressRowAction('like', 'Лайкнуть это сообщение нельзя');
        case 'msg.copy': return copyCursorText();
        case 'msg.buttons': return focusButtons();
        case 'msg.attachments': return focusAttachments();
        case 'task.card': return focusTaskCard();
        case 'task.actions': return focusTaskActions();
        case 'log.bottom': return scrollLogBottom();
        case 'log.older': return scrollLogBy(-1);
        case 'ui.attach': return clickById('b24-attach');
        case 'ui.theme': return clickById('b24-theme-toggle');
        case 'ui.help': return toggleHelp();
        case 'ui.escape': return onEscape();
        default: return false;
    }
}

function onKeyDown(e) {
    if (!panelVisible() || !hotkeysEnabled()) return;
    // Лайтбокс перекрывает панель и живёт по своим правилам (стрелки листают
    // галерею вложений, Esc закрывает) — вмешиваться в него нельзя.
    const lightbox = el('b24-lightbox');
    if (lightbox && lightbox.hidden === false) return;
    // Модалка настроек (в том числе запись новой комбинации) рисуется поверх
    // всей страницы: там наши клавиши обязаны молчать.
    if (typeof document.querySelector === 'function' && document.querySelector('.modal-overlay')) return;

    const editing = isEditableTarget(e.target);
    const id = actionForEvent(e, currentBindings(), { editing });
    if (!id) return;
    // Гасим событие ТОЛЬКО когда действие реально выполнено: иначе клавиша
    // пропала бы у соседних обработчиков, ничего не сделав.
    if (!runAction(id)) return;
    e.preventDefault();
    e.stopPropagation();
}

/**
 * Регистрирует панель в реестре владельцев клавиатуры.
 *
 * `window.ForgeApp.keyboardOwners` — массив имён контрактов, который читает
 * `static/hotkeys.js` (классический скрипт: импортировать ES-модуль он не
 * может). Реестр введён ровно на этой панели — до неё имена были захардкожены
 * списком, и комментарий в hotkeys.js обещал реестр «когда появится третья».
 *
 * Массив создаётся ЗДЕСЬ, а не в hotkeys.js: тот исполняется раньше модулей, а
 * `js/public-api.js` затем заменяет `window.ForgeApp` целиком — всё, что
 * hotkeys.js положил бы туда на старте, было бы стёрто. Регистрация же
 * происходит при входе на вкладку, то есть заведомо позже.
 */
function registerKeyboardOwner() {
    if (typeof window === 'undefined') return;
    const app = window.ForgeApp || (window.ForgeApp = {});
    if (!Array.isArray(app.keyboardOwners)) app.keyboardOwners = [];
    if (app.keyboardOwners.indexOf('b24Keys') < 0) app.keyboardOwners.push('b24Keys');
}

/**
 * Навешивает обработчик. Идемпотентно.
 *
 * @param {{onNotice?: function(string): void}} [opts]
 */
export function initB24Keynav(opts) {
    if (opts && typeof opts.onNotice === 'function') local.onNotice = opts.onNotice;
    if (local.bound) return;
    if (typeof document === 'undefined' || !document.addEventListener) return;
    local.bound = true;
    // Capture: наш обработчик должен опередить построчные слушатели ленты и
    // делегаты списка диалогов.
    document.addEventListener('keydown', onKeyDown, true);
    // Контракт для hotkeys.js — он классический скрипт и импортировать модуль
    // не может, поэтому спрашивает через window.
    if (typeof window !== 'undefined') {
        const app = window.ForgeApp || (window.ForgeApp = {});
        app.b24Keys = {
            owns: b24OwnsKeyboard,
            enabled: hotkeysEnabled,
            invalidate: invalidateBindings,
        };
        registerKeyboardOwner();
    }
}

/** Смена диалога/выход: курсор прошлого диалога больше не действителен. */
export function resetB24Keynav() {
    clearCursor();
    local.dialogCursor = null;
    hideHelp();
}

/** Полная остановка (beforeunload). */
export function teardownB24Keynav() {
    resetB24Keynav();
    if (local.bound && typeof document !== 'undefined' && document.removeEventListener) {
        document.removeEventListener('keydown', onKeyDown, true);
    }
    local.bound = false;
    invalidateBindings();
}

/** Снимок состояния для тестов и отладки. */
export function _debugKeynavState() {
    return {
        bound: local.bound,
        zone: local.zone,
        activeZone: activeZone(),
        dialogCursor: local.dialogCursor,
        msgCursor: local.msgCursor,
        helpVisible: helpVisible(),
        lastAction: local.lastAction,
        enabled: hotkeysEnabled(),
        owns: b24OwnsKeyboard(),
    };
}
