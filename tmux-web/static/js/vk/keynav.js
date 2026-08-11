// tmux-web / VK — клавиатурная навигация по панели.
//
// Задача модуля: пройти весь путь «выбрать беседу → прочитать → открыть
// вложение → ответить → отправить → нажать кнопку сообщества» вообще не
// трогая мышь. Чистая логика (реестр действий, разбор комбинаций, конфликты)
// вынесена в keys.js и общий `messenger/hotkeys-core.js` — здесь только DOM,
// фокус и состояние курсора.
//
// ── Как модуль уживается с остальными обработчиками
//
// 1. `static/hotkeys.js` (vim-навигация по сессиям) занимает j/k/h/l/g/1/2/3/?
//    глобально. Пока открыта вкладка VK, он уступает — гейт
//    `vkOwnsKeyboard` вызывается им через `window.ForgeApp.vkKeys`
//    (см. `hotkeys.js::PANEL_KEY_CONTRACTS`). Уступает не «всегда, когда
//    плагин включён», а только когда фокус реально внутри панели (или нигде):
//    кликнув в сайдбар сессий, пользователь снова получает vim.
// 2. Панель Telegram владеет клавиатурой по такому же контракту и в то же
//    время. Конфликт невозможен по построению: `switchTab` держит
//    непогашенным ровно один пейн, а `panelVisible()` каждой панели смотрит на
//    СВОЙ `#telegram` / `#vk`. Два document-слушателя в capture — цена в один
//    `if` на нажатие.
// 3. Слушатель ставится в capture-фазе на document, но НЕ глушит события
//    вслепую: `preventDefault`/`stopPropagation` зовутся только когда действие
//    действительно выполнено. Ложное `true` съело бы клавишу у соседей,
//    ничего не выполнив.
// 4. Внутри полей ввода одиночные клавиши не перехватываются вовсе — иначе
//    сообщение с буквой «j» было бы не набрать (см. hotkeys-core::comboAllowedIn).
//
// ── Отличие от телеграмного близнеца: CAPTCHA
//
// У VK есть третий оверлей, которого у Telegram нет вовсе: `#vk-captcha`.
// Он всплывает на ЛЮБОЙ метод API (не только на вход), и пока он открыт,
// пользователь набирает код в его поле. Владение клавиатурой на это время
// снимается — иначе `j`/`k` уводили бы курсор по ленте за спиной у формы,
// которую человек сейчас заполняет.
//
// ── Курсор
//
// Курсор хранится не ссылкой на узел, а идентификатором (dialogId /
// conversation_message_id): и колонка бесед, и лента перерисовываются целиком
// при каждой подгрузке, и ссылка на узел протухала бы после любого обновления.
// Ключ ленты — именно `cmid`, а не `message_id`: он и адресует ответы, и
// переживает переоткрытие беседы (`chat.js::rowsByCmid`). Класс
// `vk-kb-cursor` навешивается на найденный по id узел при каждом перемещении.

import { startReply } from './chat.js';
import { cmidOf } from './format.js';
import {
    actionForEvent, groupedRows, isEditableTarget, resolveBindings,
} from './keys.js';
import { isVkNarrowScreen, setVkContactsOpen } from './mobile.js';
import { replyTarget } from './reply.js';
import { vkState } from './state.js';

/** CSS-класс курсора клавиатуры. */
export const KB_CURSOR_CLASS = 'vk-kb-cursor';

const local = {
    bound: false,
    /** Зона курсора: 'dialogs' | 'log' | null. */
    zone: null,
    /** id беседы под курсором (строка) — переживает перерисовку колонки. */
    dialogCursor: null,
    /** cmid под курсором — переживает перерисовку ленты. */
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
 * Сравнение `!== false`, а НЕ `=== true`: дефолт этой настройки — ВКЛЮЧЕНО
 * (`user_settings.rs::default_vk_hotkeys_enabled`), поэтому отсутствие поля
 * (старый файл, backend не ответил) обязано означать «работают», а не
 * «выключены».
 *
 * Свод гейтов дефолтов, который тут легко перепутать:
 *   cmd_hints_enabled        === true  (дефолт ВЫКЛ, hotkeys.js);
 *   telegram_hotkeys_enabled !== false (дефолт ВКЛ);
 *   vk_hotkeys_enabled       !== false (дефолт ВКЛ, здесь);
 *   vk_filter_enabled        === true  (дефолт ВЫКЛ, visibility.js).
 */
export function hotkeysEnabled() {
    const s = userSettings();
    if (!s) return true;
    return s.vk_hotkeys_enabled !== false;
}

/** Сырая карта переопределений из настроек. */
function overridesFromSettings() {
    const s = userSettings();
    const raw = s && s.vk_hotkeys;
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
    return el('vk');
}

/** Видна ли вкладка VK и готова ли она к навигации. */
export function panelVisible() {
    const p = panel();
    if (!p) return false;
    if (p.hidden === true) return false;
    // Оверлей логина закрывает панель целиком: пока в нём, навигация по
    // несуществующим беседам смысла не имеет.
    const login = el('vk-login');
    if (login && login.hidden === false) return false;
    // Captcha — отличие VK от Telegram: она всплывает на ЛЮБОЙ метод API, и
    // пока пользователь набирает код, клавиши принадлежат её полю.
    const captcha = el('vk-captcha');
    if (captcha && captcha.hidden === false) return false;
    // Попап анализа: в нём два поля ввода (вопрос модели и заметка о беседе), и
    // без этой ветки vim-навигация забирала бы буквы `j`/`k` себе — вопрос
    // стало бы физически не набрать (следствие читает static/hotkeys.js:117-131).
    const analysis = el('vk-analysis');
    if (analysis && analysis.hidden === false) return false;
    return true;
}

/**
 * Владеет ли VK клавиатурой прямо сейчас.
 *
 * Спрашивает `hotkeys.js`, чтобы решить, гасить ли свою vim-навигацию. Фокус
 * вне панели (сайдбар сессий, модалка настроек) означает «не владеет»: иначе
 * открытая вкладка VK отняла бы j/k у всего остального интерфейса.
 */
export function vkOwnsKeyboard() {
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
    return Array.from(p.querySelectorAll('#vk-dialog-list .vk-dialog, #vk-community-list .vk-community'));
}

function messageRows() {
    const log = el('vk-log');
    if (!log || typeof log.querySelectorAll !== 'function') return [];
    // Системные строки, ошибки и заглушки удалённых сообщений курсору не
    // нужны: на них нечего ответить и нечего открыть.
    return Array.from(log.querySelectorAll('.vk-line')).filter((row) => row._vk && row._vk.msg);
}

function dirRows() {
    const list = el('vk-dir-list');
    if (!list || typeof list.querySelectorAll !== 'function') return [];
    return Array.from(list.querySelectorAll('.vk-dir'));
}

/**
 * По одной кнопке OPEN на каждое вложение строки.
 *
 * Шаг курсора в зоне вложений — это шаг по ВЛОЖЕНИЯМ, а не по кнопкам: у
 * карточки их две (OPEN и SAVE), и хождение по обеим означало бы два нажатия
 * j на одно вложение. OPEN — первая `.vk-media-btn` в подвале карточки
 * (`media.js` добавляет её раньше SAVE).
 */
function mediaButtons(row) {
    if (!row || typeof row.querySelectorAll !== 'function') return [];
    return Array.from(row.querySelectorAll('.vk-media-card'))
        .map((card) => (typeof card.querySelector === 'function' ? card.querySelector('.vk-media-btn') : null))
        .filter(Boolean);
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

/** Узел беседы под курсором (или активной беседы, если курсор не ставили). */
function cursorDialogNode() {
    const rows = dialogRows();
    if (!rows.length) return null;
    const wanted = local.dialogCursor !== null ? String(local.dialogCursor) : null;
    if (wanted) {
        const hit = rows.find((r) => r.dataset && String(r.dataset.dialogId) === wanted);
        if (hit) return hit;
    }
    const activeId = vkState.activeDialogId !== null ? String(vkState.activeDialogId) : null;
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
        const hit = rows.find((r) => cmidOf(r._vk.msg) === local.msgCursor);
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
    local.msgCursor = (node._vk && node._vk.msg) ? cmidOf(node._vk.msg) : null;
    markCursor(node);
    return true;
}

/** Снимает курсор и подсветку (Escape, смена беседы, teardown). */
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
 *
 * Порядок проверок — от вложенного к внешнему: карточка вложения лежит ВНУТРИ
 * `#vk-log`, и проверь мы ленту первой, зона вложений была бы недостижима.
 */
export function activeZone() {
    const active = (typeof document !== 'undefined') ? document.activeElement : null;
    if (active && typeof active.closest === 'function') {
        if (active.closest('#vk-prompt')) return 'input';
        if (active.closest('.vk-kb-bar')) return 'keyboard';
        if (active.closest('.vk-media-card')) return 'media';
        if (active.closest('#vk-log')) return 'log';
        if (active.closest('#vk-contacts')) return 'dialogs';
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
    // На узком экране колонка бесед уезжает за край: фокусировать невидимое —
    // значит подарить пользователю «мёртвые» нажатия.
    if (isVkNarrowScreen()) setVkContactsOpen(true);
    return setDialogCursor(cursorDialogNode());
}

function focusLog() {
    return setMessageCursor(cursorMessageNode());
}

function focusInput() {
    const input = el('vk-input');
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
        // на сессию в сайдбаре.
        return next ? setDialogCursor(next) : true;
    }
    if (zone === 'keyboard') return moveKeyboardFocus(delta);
    if (zone === 'media') return moveMediaFocus(delta);
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
    const bar = document.querySelector('.vk-kb-bar');
    if (!bar) return false;
    const btns = Array.from(bar.querySelectorAll('.vk-kb-btn'));
    if (!btns.length) return false;
    const idx = btns.indexOf(document.activeElement);
    const next = btns[Math.max(0, Math.min(btns.length - 1, (idx < 0 ? 0 : idx + delta)))];
    if (!next) return false;
    try { next.focus(); } catch (_) { return false; }
    return true;
}

function moveMediaFocus(delta) {
    const active = document.activeElement;
    const card = (active && typeof active.closest === 'function') ? active.closest('.vk-media-card') : null;
    const row = (card && typeof card.closest === 'function') ? card.closest('.vk-line') : null;
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
        // На узком экране колонка перекрывает ленту — после выбора беседы её
        // закрывает сам dialogs.js, нам остаётся увести курсор в ленту.
        local.zone = 'log';
        local.msgCursor = null;
        return true;
    }
    const active = document.activeElement;
    // Фокус уже на кнопке (клавиатура сообщества, OPEN/SAVE вложения) — НЕ
    // трогаем: <button> активируется Enter'ом штатно, силами браузера. Нажать
    // её здесь самим значило бы поставить наш click рядом с нативным и
    // рискнуть отправить сообществу ДВА нажатия на одно — а это у бота может
    // быть и «купить», и «удалить». Возвращаем false, чтобы событие не гасилось.
    if (active && typeof active.closest === 'function'
        && (active.closest('.vk-kb-btn') || active.closest('.vk-media-btn'))) {
        return false;
    }
    const row = cursorMessageNode();
    if (!row) return false;
    // Приоритет: вложение (его открытие — самое ожидаемое действие на строке
    // с картинкой), затем кнопка сообщества, затем цитата.
    //
    // Открываем именно кнопкой OPEN, а не кликом по телу карточки: тело
    // реагирует только у превью и проигрываемых видов (media.js), и на
    // документе клик по нему был бы пустышкой — то есть съеденной клавишей
    // без единого видимого следствия.
    const openable = mediaButtons(row).find((b) => !b.disabled);
    if (openable) { openable.click(); return true; }
    const btn = row.querySelector('.vk-kb-btn');
    if (btn) { btn.click(); return true; }
    const quote = row.querySelector('.vk-reply-quote');
    if (quote) { quote.click(); return true; }
    return false;
}

function replyToCursor() {
    const row = cursorMessageNode();
    if (!row || !row._vk || !row._vk.msg) return false;
    setMessageCursor(row);
    return startReply(row._vk.msg);
}

function jumpToQuote() {
    const row = cursorMessageNode();
    if (!row) return false;
    const quote = row.querySelector('.vk-reply-quote');
    if (!quote) return false;
    quote.click();
    return true;
}

function copyCursorText() {
    const row = cursorMessageNode();
    if (!row || !row._vk || !row._vk.text) return false;
    const text = String(row._vk.text.textContent || '');
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
    const inRow = row ? row.querySelector('.vk-kb-btn') : null;
    // Кнопки бывают двух видов: inline под сообщением и reply-клавиатура над
    // полем ввода. Второй — фолбэк, когда у сообщения под курсором своих нет.
    const bar = document.querySelector('.vk-kb-bar-rows .vk-kb-btn');
    const target = inRow || bar;
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
 * Действие, которого у Telegram не было: там `msg.media` — ОДИН объект, и
 * `nav.activate` покрывал весь случай. У VK вложений до десяти, и без этого
 * шага открыть, скажем, третье фото галереи с клавиатуры было бы нечем.
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

function scrollLogBy(direction) {
    const log = el('vk-log');
    if (!log) return false;
    const step = Math.max(80, Math.floor((log.clientHeight || 400) * 0.9));
    log.scrollTop = Math.max(0, log.scrollTop + step * direction);
    return true;
}

function scrollLogBottom() {
    const log = el('vk-log');
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
    // У VK «директория» — это СЕРВЕРНЫЙ фильтр, то есть клик уходит запросом
    // (dialogs.js::selectDir с дебаунсом 300 мс и уважением к 3 rps). Гасить
    // здесь ничего не надо: дебаунс живёт в одном месте, и второй в keynav
    // означал бы две точки правды о частоте запросов.
    next.click();
    // Список бесед пересобран — курсор на прежний id больше не наводится.
    local.dialogCursor = null;
    return true;
}

function nextUnread() {
    const rows = dialogRows();
    if (!rows.length) return false;
    const unread = rows.filter((r) => {
        const badge = r._vk && r._vk.unread;
        return badge && String(badge.textContent || '').trim() !== '';
    });
    if (!unread.length) {
        notice('Непрочитанных бесед нет');
        return true;
    }
    const current = cursorDialogNode();
    const after = unread.filter((r) => rows.indexOf(r) > rows.indexOf(current));
    // По кругу: дойдя до конца списка, начинаем сначала — иначе последняя
    // непрочитанная стала бы тупиком.
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
        if (replyTarget(vkState.activeDialogId)) return false;
        const input = el('vk-input');
        if (input && typeof input.blur === 'function') { try { input.blur(); } catch (_) { /* ok */ } }
        return focusLog();
    }
    if (zone === 'keyboard' || zone === 'media') return focusLog();
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
    overlay.className = 'vk-keys-help';
    overlay.id = 'vk-keys-help';

    const card = document.createElement('div');
    card.className = 'vk-keys-help-card';

    const title = document.createElement('div');
    title.className = 'vk-keys-help-title';
    title.textContent = '// ГОРЯЧИЕ КЛАВИШИ';
    card.appendChild(title);

    for (const group of groupedRows(overridesFromSettings())) {
        const gEl = document.createElement('div');
        gEl.className = 'vk-keys-help-group';
        const gTitle = document.createElement('div');
        gTitle.className = 'vk-keys-help-group-title';
        gTitle.textContent = group.name;
        gEl.appendChild(gTitle);
        for (const row of group.items) {
            const line = document.createElement('div');
            line.className = 'vk-keys-help-row';
            const key = document.createElement('kbd');
            key.className = 'vk-keys-help-key';
            key.textContent = row.combo ? row.label : '—';
            if (!row.combo) key.classList.add('vk-keys-help-key-none');
            const label = document.createElement('span');
            label.className = 'vk-keys-help-label';
            label.textContent = row.title;
            line.appendChild(key);
            line.appendChild(label);
            if (row.hint) {
                const hint = document.createElement('span');
                hint.className = 'vk-keys-help-hint';
                hint.textContent = row.hint;
                line.appendChild(hint);
            }
            gEl.appendChild(line);
        }
        card.appendChild(gEl);
    }

    const foot = document.createElement('div');
    foot.className = 'vk-keys-help-foot';
    foot.textContent = 'Переназначить и выключить — Настройки → VK. Esc — закрыть.';
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
        case 'msg.attachments': return focusAttachments();
        case 'log.bottom': return scrollLogBottom();
        case 'log.older': return scrollLogBy(-1);
        case 'ui.attach': return clickById('vk-attach');
        case 'ui.theme': return clickById('vk-theme-toggle');
        case 'ui.help': return toggleHelp();
        case 'ui.escape': return onEscape();
        default: return false;
    }
}

function onKeyDown(e) {
    if (!panelVisible() || !hotkeysEnabled()) return;
    // Лайтбокс перекрывает панель и живёт по своим правилам (стрелки листают
    // галерею вложений, Esc закрывает) — вмешиваться в него нельзя.
    const lightbox = el('vk-lightbox');
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
    // (Enter на цитате) и делегаты списка бесед.
    document.addEventListener('keydown', onKeyDown, true);
    // Контракт для hotkeys.js — он классический скрипт и импортировать модуль
    // не может, поэтому спрашивает через window. Имя `vkKeys` перечислено в
    // `hotkeys.js::PANEL_KEY_CONTRACTS` рядом с телеграмным.
    if (typeof window !== 'undefined') {
        const app = window.ForgeApp || (window.ForgeApp = {});
        app.vkKeys = {
            owns: vkOwnsKeyboard,
            enabled: hotkeysEnabled,
            invalidate: invalidateBindings,
        };
    }
}

/** Смена беседы/выход: курсор прошлой беседы больше не действителен. */
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
        owns: vkOwnsKeyboard(),
    };
}
