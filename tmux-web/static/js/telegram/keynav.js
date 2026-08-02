// tmux-web / Telegram — клавиатурная навигация по панели.
//
// Задача модуля: пройти весь путь «выбрать чат → прочитать → ответить →
// отправить → нажать кнопку бота» вообще не трогая мышь. Чистая логика
// (реестр действий, разбор комбинаций, конфликты) вынесена в keys.js — здесь
// только DOM, фокус и состояние курсора.
//
// ── Как модуль уживается с остальными обработчиками
//
// 1. `static/hotkeys.js` (vim-навигация по сессиям) занимает j/k/h/l/g/1/2/3/?
//    глобально. Пока открыта вкладка Telegram, он уступает — гейт
//    `telegramOwnsKeyboard` вызывается им через `window.ForgeApp.telegramKeys`.
//    Уступает не «всегда, когда плагин включён», а только когда фокус реально
//    внутри панели (или нигде): кликнув в сайдбар сессий, пользователь снова
//    получает vim.
// 2. Слушатель ставится в capture-фазе на document, но НЕ глушит события
//    вслепую: `preventDefault`/`stopPropagation` зовутся только когда действие
//    действительно выполнено.
// 3. Внутри полей ввода одиночные клавиши не перехватываются вовсе — иначе
//    сообщение с буквой «j» было бы не набрать (см. keys.js::comboAllowedIn).
//
// ── Курсор
//
// Курсор хранится не ссылкой на узел, а идентификатором (dialogId /
// message_id): и колонка чатов, и лента перерисовываются целиком при каждой
// подгрузке, и ссылка на узел протухала бы после любого обновления. Класс
// `tg-kb-cursor` навешивается на найденный по id узел при каждом перемещении.

import { startReply } from './chat.js';
import {
    actionForEvent, groupedRows, isEditableTarget, resolveBindings,
} from './keys.js';
import { isTgNarrowScreen, setTgContactsOpen } from './mobile.js';
import { replyTarget } from './reply.js';
import { tgState } from './state.js';

/** CSS-класс курсора клавиатуры. */
export const KB_CURSOR_CLASS = 'tg-kb-cursor';

const local = {
    bound: false,
    /** Зона курсора: 'dialogs' | 'log' | null. */
    zone: null,
    /** id чата под курсором (строка) — переживает перерисовку колонки. */
    dialogCursor: null,
    /** message_id под курсором — переживает перерисовку ленты. */
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
 * Читаются лениво в момент нажатия — ровно как в `hotkeys.js`: модуль может
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
 * Сравнение `!== false`, а не `=== true`: дефолт этой настройки — ВКЛЮЧЕНО
 * (см. `user_settings.rs`), поэтому отсутствие поля (старый файл, backend не
 * ответил) обязано означать «работают», а не «выключены».
 */
export function hotkeysEnabled() {
    const s = userSettings();
    if (!s) return true;
    return s.telegram_hotkeys_enabled !== false;
}

/** Сырая карта переопределений из настроек. */
function overridesFromSettings() {
    const s = userSettings();
    const raw = s && s.telegram_hotkeys;
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
 * карты (см. [`currentBindings`]). Нужна на случай, когда сериализация карты
 * не удалась, и как явная точка в контракте для формы настроек.
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
    return el('telegram');
}

/** Видна ли вкладка Telegram. */
export function panelVisible() {
    const p = panel();
    if (!p) return false;
    if (p.hidden === true) return false;
    // Оверлей логина закрывает панель целиком: пока в нём, навигация по
    // несуществующим чатам смысла не имеет.
    const login = el('tg-login');
    if (login && login.hidden === false) return false;
    return true;
}

/**
 * Владеет ли Telegram клавиатурой прямо сейчас.
 *
 * Спрашивает `hotkeys.js`, чтобы решить, гасить ли свою vim-навигацию. Фокус
 * вне панели (сайдбар сессий, модалка настроек) означает «не владеет»: иначе
 * открытая вкладка Telegram отняла бы j/k у всего остального интерфейса.
 */
export function telegramOwnsKeyboard() {
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
    const p = panel();
    if (!p || typeof p.querySelectorAll !== 'function') return [];
    return Array.from(p.querySelectorAll('#tg-dialog-list .tg-dialog, #tg-channel-list .tg-channel'));
}

function messageRows() {
    const log = el('tg-log');
    if (!log || typeof log.querySelectorAll !== 'function') return [];
    // Системные строки, ошибки и заглушки удалённых сообщений курсору не
    // нужны: на них нечего ответить и нечего открыть.
    return Array.from(log.querySelectorAll('.tg-line')).filter((row) => row._tg && row._tg.msg);
}

function dirRows() {
    const list = el('tg-dir-list');
    if (!list || typeof list.querySelectorAll !== 'function') return [];
    return Array.from(list.querySelectorAll('.tg-dir'));
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

/** Узел чата под курсором (или активного чата, если курсор не ставили). */
function cursorDialogNode() {
    const rows = dialogRows();
    if (!rows.length) return null;
    const wanted = local.dialogCursor !== null ? String(local.dialogCursor) : null;
    if (wanted) {
        const hit = rows.find((r) => r.dataset && String(r.dataset.dialogId) === wanted);
        if (hit) return hit;
    }
    const activeId = tgState.activeDialogId !== null ? String(tgState.activeDialogId) : null;
    if (activeId) {
        const hit = rows.find((r) => r.dataset && String(r.dataset.dialogId) === activeId);
        if (hit) return hit;
    }
    return rows[0];
}

/** Узел сообщения под курсором (или последнего сообщения). */
function cursorMessageNode() {
    const rows = messageRows();
    if (!rows.length) return null;
    if (local.msgCursor !== null) {
        const hit = rows.find((r) => r._tg.msg && Number(r._tg.msg.id) === Number(local.msgCursor));
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
    local.msgCursor = (node._tg && node._tg.msg) ? Number(node._tg.msg.id) : null;
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
 * Считается по реальному фокусу, а не по сохранённому `local.zone`: мышь и Tab
 * двигают фокус мимо нас, и врать о положении курсора после этого нельзя.
 */
export function activeZone() {
    const active = (typeof document !== 'undefined') ? document.activeElement : null;
    if (active && typeof active.closest === 'function') {
        if (active.closest('#tg-prompt')) return 'input';
        if (active.closest('.tg-kb-bar')) return 'keyboard';
        if (active.closest('#tg-log')) return 'log';
        if (active.closest('#tg-contacts')) return 'dialogs';
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
    // На узком экране колонка чатов уезжает за край: фокусировать невидимое —
    // значит подарить пользователю «мёртвые» нажатия.
    if (isTgNarrowScreen()) setTgContactsOpen(true);
    return setDialogCursor(cursorDialogNode());
}

function focusLog() {
    return setMessageCursor(cursorMessageNode());
}

function focusInput() {
    const input = el('tg-input');
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
        // считаем обработанным: иначе оно провалилось бы в vim и увело фокус
        // на сессии в сайдбаре.
        return next ? setDialogCursor(next) : true;
    }
    if (zone === 'keyboard') return moveKeyboardFocus(delta);
    // Везде остальное (в том числе курсор не ставили) — лента.
    const rows = messageRows();
    const current = cursorMessageNode();
    const next = moveWithin(rows, current, delta);
    if (next) return setMessageCursor(next);
    if (delta < 0) {
        // Верх ленты: докручиваем вверх и отдаём подгрузку истории штатному
        // обработчику скролла (chat.js::bindScroll) — дублировать loadOlder
        // здесь значило бы завести вторую точку правды о пагинации.
        scrollLogBy(-1);
    }
    return true;
}

function moveKeyboardFocus(delta) {
    const bar = document.querySelector('.tg-kb-bar');
    if (!bar) return false;
    const btns = Array.from(bar.querySelectorAll('.tg-kb-btn'));
    if (!btns.length) return false;
    const idx = btns.indexOf(document.activeElement);
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
        // На узком экране колонка перекрывает ленту — после выбора чата её
        // закрывает сам dialogs.js, нам остаётся увести курсор в ленту.
        local.zone = 'log';
        local.msgCursor = null;
        return true;
    }
    const active = document.activeElement;
    // Фокус уже на кнопке (клавиатура бота, кнопка в сообщении) — НЕ трогаем:
    // <button> активируется Enter'ом штатно, силами браузера. Нажать её здесь
    // самим значило бы поставить наш click рядом с нативным и рискнуть
    // отправить боту ДВА callback'а на одно нажатие — а это у бота может быть
    // и «купить», и «удалить». Возвращаем false, чтобы событие не гасилось.
    if (active && typeof active.closest === 'function' && active.closest('.tg-kb-btn')) {
        return false;
    }
    const row = cursorMessageNode();
    if (!row) return false;
    // Приоритет: медиа (его открытие — самое ожидаемое действие на строке с
    // картинкой), затем первая кнопка бота, затем цитата.
    //
    // Кликаем именно по `.tg-media-body`: обработчик открытия висит на теле
    // карточки (media.js:353), а не на её корне — клик по корню ничего бы не
    // сделал.
    const media = row.querySelector('.tg-media-body');
    if (media) { media.click(); return true; }
    const btn = row.querySelector('.tg-kb-btn');
    if (btn) { btn.click(); return true; }
    const quote = row.querySelector('.tg-reply-quote');
    if (quote) { quote.click(); return true; }
    return false;
}

function replyToCursor() {
    const row = cursorMessageNode();
    if (!row || !row._tg || !row._tg.msg) return false;
    setMessageCursor(row);
    return startReply(row._tg.msg);
}

function jumpToQuote() {
    const row = cursorMessageNode();
    if (!row) return false;
    const quote = row.querySelector('.tg-reply-quote');
    if (!quote) return false;
    quote.click();
    return true;
}

function copyCursorText() {
    const row = cursorMessageNode();
    if (!row || !row._tg || !row._tg.text) return false;
    const text = String(row._tg.text.textContent || '');
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
    const inRow = row ? row.querySelector('.tg-kb-btn') : null;
    // Кнопки бывают двух видов: inline под сообщением и reply-клавиатура над
    // полем ввода. Второй — фолбэк, когда у сообщения под курсором своих нет.
    const bar = document.querySelector('.tg-kb-bar-rows .tg-kb-btn');
    const target = inRow || bar;
    if (!target) {
        notice('У этого сообщения нет кнопок');
        return true;
    }
    try { target.focus(); } catch (_) { return false; }
    return true;
}

function scrollLogBy(direction) {
    const log = el('tg-log');
    if (!log) return false;
    const step = Math.max(80, Math.floor((log.clientHeight || 400) * 0.9));
    log.scrollTop = Math.max(0, log.scrollTop + step * direction);
    return true;
}

function scrollLogBottom() {
    const log = el('tg-log');
    if (!log) return false;
    log.scrollTop = log.scrollHeight;
    // Курсор уезжает вместе с лентой: иначе следующее j/k вернуло бы экран
    // туда, где пользователь только что был, и прыжок выглядел бы поломкой.
    //
    // Именно setMessageCursor, а не присваивание local.msgCursor: подсветка
    // обязана переехать вместе с курсором, иначе видимая рамка и логическая
    // позиция разъезжаются, и следующий шаг курсора выглядит скачком.
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
    next.click();
    // Список чатов пересобран — курсор на прежний id больше не наводится.
    local.dialogCursor = null;
    return true;
}

function nextUnread() {
    const rows = dialogRows();
    if (!rows.length) return false;
    const unread = rows.filter((r) => {
        const badge = r._tg && r._tg.unread;
        return badge && String(badge.textContent || '').trim() !== '';
    });
    if (!unread.length) {
        notice('Непрочитанных чатов нет');
        return true;
    }
    const current = cursorDialogNode();
    const after = unread.filter((r) => rows.indexOf(r) > rows.indexOf(current));
    // По кругу: дойдя до конца списка, начинаем сначала — иначе последний
    // непрочитанный стал бы тупиком.
    const target = after.length ? after[0] : unread[0];
    return setDialogCursor(target);
}

function onEscape() {
    if (helpVisible()) { hideHelp(); return true; }
    const zone = activeZone();
    if (zone === 'input') {
        // Полосу ответа гасит собственный обработчик chat.js (он же вернёт
        // фокус в поле) — перехватывать Escape поверх него значило бы отменять
        // не то, чего ждёт пользователь.
        if (replyTarget(tgState.activeDialogId)) return false;
        const input = el('tg-input');
        if (input && typeof input.blur === 'function') { try { input.blur(); } catch (_) { /* ok */ } }
        return focusLog();
    }
    if (zone === 'keyboard') return focusLog();
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
    overlay.className = 'tg-keys-help';
    overlay.id = 'tg-keys-help';

    const card = document.createElement('div');
    card.className = 'tg-keys-help-card';

    const title = document.createElement('div');
    title.className = 'tg-keys-help-title';
    title.textContent = '// ГОРЯЧИЕ КЛАВИШИ';
    card.appendChild(title);

    for (const group of groupedRows(overridesFromSettings())) {
        const gEl = document.createElement('div');
        gEl.className = 'tg-keys-help-group';
        const gTitle = document.createElement('div');
        gTitle.className = 'tg-keys-help-group-title';
        gTitle.textContent = group.name;
        gEl.appendChild(gTitle);
        for (const row of group.items) {
            const line = document.createElement('div');
            line.className = 'tg-keys-help-row';
            const key = document.createElement('kbd');
            key.className = 'tg-keys-help-key';
            key.textContent = row.combo ? row.label : '—';
            if (!row.combo) key.classList.add('tg-keys-help-key-none');
            const label = document.createElement('span');
            label.className = 'tg-keys-help-label';
            label.textContent = row.title;
            line.appendChild(key);
            line.appendChild(label);
            if (row.hint) {
                const hint = document.createElement('span');
                hint.className = 'tg-keys-help-hint';
                hint.textContent = row.hint;
                line.appendChild(hint);
            }
            gEl.appendChild(line);
        }
        card.appendChild(gEl);
    }

    const foot = document.createElement('div');
    foot.className = 'tg-keys-help-foot';
    foot.textContent = 'Переназначить и выключить — Настройки → Горячие клавиши. Esc — закрыть.';
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
 * событие будет погашено. Ложное `true` вредно: оно съело бы клавишу у
 * соседних обработчиков, ничего не выполнив.
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
        case 'msg.reply': return replyToCursor();
        case 'msg.quote': return jumpToQuote();
        case 'msg.copy': return copyCursorText();
        case 'msg.buttons': return focusButtons();
        case 'log.bottom': return scrollLogBottom();
        case 'log.older': return scrollLogBy(-1);
        case 'ui.attach': return clickById('tg-attach');
        case 'ui.theme': return clickById('tg-theme-toggle');
        case 'ui.help': return toggleHelp();
        case 'ui.escape': return onEscape();
        default: return false;
    }
}

function onKeyDown(e) {
    if (!panelVisible() || !hotkeysEnabled()) return;
    // Лайтбокс перекрывает панель и живёт по своим правилам (стрелки листают
    // галерею, Esc закрывает) — вмешиваться в него нельзя.
    const lightbox = el('tg-lightbox');
    if (lightbox && lightbox.hidden === false) return;
    // Модалка настроек (в том числе запись новой комбинации) рисуется поверх
    // всей страницы: там наши клавиши обязаны молчать.
    if (typeof document.querySelector === 'function' && document.querySelector('.modal-overlay')) return;

    const editing = isEditableTarget(e.target);
    const id = actionForEvent(e, currentBindings(), { editing });
    if (!id) return;
    if (!runAction(id)) return;
    e.preventDefault();
    e.stopPropagation();
}

/**
 * Навешивает обработчик. Идемпотентно.
 *
 * @param {{onNotice?: function(string): void}} [opts]
 */
export function initKeynav(opts) {
    if (opts && typeof opts.onNotice === 'function') local.onNotice = opts.onNotice;
    if (local.bound) return;
    if (typeof document === 'undefined' || !document.addEventListener) return;
    local.bound = true;
    // Capture: наш обработчик должен опередить построчные слушатели ленты
    // (Enter на цитате) и делегаты списка чатов.
    document.addEventListener('keydown', onKeyDown, true);
    // Контракт для hotkeys.js — он классический скрипт и импортировать модуль
    // не может, поэтому спрашивает через window.
    if (typeof window !== 'undefined') {
        const app = window.ForgeApp || (window.ForgeApp = {});
        app.telegramKeys = {
            owns: telegramOwnsKeyboard,
            enabled: hotkeysEnabled,
            invalidate: invalidateBindings,
        };
    }
}

/** Смена диалога/выход: курсор прошлого чата больше не действителен. */
export function resetKeynav() {
    clearCursor();
    local.dialogCursor = null;
    hideHelp();
}

/** Полная остановка (beforeunload). */
export function teardownKeynav() {
    resetKeynav();
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
        owns: telegramOwnsKeyboard(),
    };
}
