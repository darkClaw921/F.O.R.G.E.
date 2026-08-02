// tmux-web — Telegram plugin: оркестратор вкладки.
//
// Экспорты:
//   initTelegramGate() — вызывается из bootstrap на старте страницы:
//       спрашивает GET /api/telegram/status и показывает кнопку ✈, если
//       плагин включён. Плагин выключен (404) — это НЕ ошибка: кнопка
//       просто остаётся скрытой, в консоль не шумим.
//   initTelegram()     — первое открытие вкладки (вызывается из tabs.js).
//   suspendTelegram()/resumeTelegram() — реакция на visibilitychange.
//   teardownTelegram() — beforeunload.
//   _debugState()      — снимок состояния для e2e (по образцу echo/main.js).
//
// Инвариант XSS: любые данные (имена диалогов, тексты сообщений) попадают
// в DOM только через textContent. innerHTML в модулях плагина не
// используется.

import {
    $telegramToggle, $tgUnreadBadge, $telegram, $tgSelfUser, $tgLogout,
    $tgSessionHead, $tgMode, $tgLat, $tgTyping, $tgTitlebar,
} from '../core/dom.js';
import { notify } from '../echo/notifications.js';

import { getStatus, logout as apiLogout, markRead } from './api.js';
import * as chat from './chat.js';
import * as dialogs from './dialogs.js';
import {
    LAT_WINDOW, READ_DEBOUNCE_MS, TYPING_RESET_MS, dialogsForDir, mutedIds, readAllowed,
    smoothedLatency, toastAllowed, truncate, typingLine, unreadBadgeText, unreadTotal,
} from './format.js';
import { initKeynav, resetKeynav, teardownKeynav } from './keynav.js';
import {
    initLoginWizard, renderLoginState, resetLoginWizard, stopCountdown,
} from './login.js';
import { onDownloadProgress, teardownMedia } from './media.js';
import { closeTgContactsAfterSelect, initTelegramMobile } from './mobile.js';
import {
    clearTgAccountData, currentVisibilityFilter, invalidateVisibilityFilter, tgState,
} from './state.js';
import { dialogIdVisible, visibleDialogs } from './visibility.js';
import { initUpload, onUploadProgress, teardownUpload } from './upload.js';
import { connectTelegramWs, disconnectTelegramWs, isTelegramWsOpen } from './ws.js';

/** Не чаще одного тоста на диалог за это окно, мс. */
const TOAST_THROTTLE_MS = 5000;

const local = {
    /** initTelegram() уже отработал. */
    initialized: false,
    /** initTelegramGate() уже отработал (успешно или нет). */
    gated: false,
    /** Обработчик кнопки выхода навешен. */
    logoutBound: false,
    /** Собранная карта WS-обработчиков (строится один раз). */
    handlers: null,
    /** dialog_id → когда показывали прошлый тост (мс). */
    lastToastAt: new Map(),
    /**
     * Диалоги, по которым тосты и бейдж заглушены.
     *
     * Производная от поля muted в DialogDto (источник истины — сам список):
     * пересобирается в recomputeUnread при каждом пересчёте, поэтому
     * загрузка диалогов и WS `notify_settings` обновляют её автоматически.
     *
     * ВАЖНО: список неполон — `GET /dialogs` отдаёт верхнюю страницу, а WS
     * вещает события по ВСЕМ чатам аккаунта. Про диалоги вне страницы этот Set
     * не знает ничего, поэтому один он гейтом быть не может (см. flagsExtra).
     */
    muted: new Set(),
    /**
     * `dialog_id` → {muted, archived} для диалогов ВНЕ загруженной страницы.
     *
     * Заполняется полями кадров WS (`new_message`, `notify_settings`), где
     * сервер уже посчитал mute с наследованием глобальных настроек. Именно это
     * закрывает жалобу «уведомления приходят от ненужных чатов»: до сих пор
     * фронт про такой чат не знал ничего и считал его незаглушённым.
     *
     * Запись живёт, пока диалог не появится в списке: там значение свежее и
     * авторитетнее, и recomputeUnread такую запись снимает.
     */
    flagsExtra: new Map(),
    /**
     * `dialog_id` → непрочитанное по диалогам ВНЕ загруженной страницы.
     *
     * dialogs.bumpUnread по такому чату молча выходит (DialogDto нет), из-за
     * чего бейдж занижался ровно там, где тост, наоборот, показывался. Считаем
     * здесь — но только по «звенящим» диалогам (не muted и не archived).
     */
    pendingUnread: new Map(),
    /** Список диалогов уже загружен хотя бы раз. */
    dialogsLoaded: false,
    /** Идёт перезагрузка списка после notify_defaults_changed. */
    defaultsReloading: false,
    /** `dialogId|userId` → {dialogId, label, timer} — кто сейчас печатает. */
    typing: new Map(),
    /** dialog_id → максимальный message_id, ожидающий отметки о прочтении. */
    readPending: new Map(),
    /** Таймер debounce отметки о прочтении. */
    readTimer: null,
    /** dialog_id → максимальный message_id, который УЖЕ отмечен прочитанным. */
    readSent: new Map(),
    /** Окно последних замеров задержки для сглаживания LAT. */
    latSamples: [],
};

/**
 * Показывает/скрывает кнопку вкладки.
 * @param {boolean} visible
 */
function setToggleVisible(visible) {
    if (!$telegramToggle) return;
    $telegramToggle.hidden = !visible;
}

/**
 * Обновляет бейдж непрочитанного на кнопке ✈.
 * @param {number} count
 */
export function setUnreadBadge(count) {
    tgState.unreadTotal = Number.isFinite(count) && count > 0 ? count : 0;
    if (!$tgUnreadBadge) return;
    const text = unreadBadgeText(tgState.unreadTotal);
    if (!text) {
        $tgUnreadBadge.hidden = true;
        $tgUnreadBadge.textContent = '';
        return;
    }
    $tgUnreadBadge.hidden = false;
    $tgUnreadBadge.textContent = text;
}

/**
 * Непрочитанное для бейджа на кнопке ✈: без заглушённых и без архива.
 *
 * Отдельно от `unreadTotal` (format.js) СОЗНАТЕЛЬНО: та функция считает по
 * переданному списку и используется ещё и для счётчика `~/archive`, где архив
 * как раз считать надо. Бейдж же обязан совпадать со счётчиком `~/inbox`
 * (dirUnread с тем же селектором), иначе цифра на ✈ и цифра у `~/inbox`
 * расходятся ровно на непрочитанное в архиве — и пользователь ищет письмо,
 * которого в ленте нет.
 *
 * @param {Array<object>} dialogs
 * @param {Set<string>|Array<string>} [muted]
 * @returns {number}
 */
export function badgeUnread(dialogs, muted) {
    // Белый список настроек применяется и здесь: цифра на ✈ обязана совпадать
    // с тем, что реально видно в колонке. Иначе бейдж звал бы в чат, которого
    // пользователь у себя не показывает.
    const allowed = visibleDialogs(dialogs, currentVisibilityFilter(), tgState.folders);
    return unreadTotal(dialogsForDir(allowed, { type: 'inbox' }), muted);
}

/**
 * Пересчитывает бейдж по списку диалогов.
 *
 * Единственный путь обновления бейджа: и `new_message`, и `read`, и открытие
 * диалога сначала правят `unread_count` в `tgState.dialogs`, а потом зовут
 * пересчёт. Иначе два независимых счётчика неизбежно разъезжаются.
 *
 * Слагаемых два:
 *
 * 1. загруженная страница диалогов — без заглушённых и без архива;
 * 2. `local.pendingUnread` — чаты, которых на странице нет вовсе (WS вещает по
 *    всему аккаунту, а `/dialogs` отдаёт только верхушку).
 *
 * Диалог, появившийся в списке, из производных карт вычищается здесь же:
 * список авторитетнее WS-кадров, и без этой чистки одно и то же сообщение
 * посчиталось бы дважды.
 */
export function recomputeUnread() {
    local.muted = mutedIds(tgState.dialogs);
    for (const d of tgState.dialogs || []) {
        if (!d) continue;
        const id = String(d.id);
        local.flagsExtra.delete(id);
        local.pendingUnread.delete(id);
    }
    let total = badgeUnread(tgState.dialogs, local.muted);
    for (const n of local.pendingUnread.values()) total += Number(n) || 0;
    setUnreadBadge(total);
    return tgState.unreadTotal;
}

/**
 * Значение флага из WS-кадра с откатом на локальное знание.
 *
 * Явный `typeof === 'boolean'` обязателен: у кадра СТАРОГО сервера поля нет
 * вовсе, и наивное `!frame.muted` превратило бы «не знаю» в «не заглушён» —
 * то есть дало бы регресс ровно в ту сторону, на которую жалуется
 * пользователь.
 *
 * @param {*} value — поле кадра
 * @param {*} fallback — что знаем сами (список диалогов / прошлые кадры)
 * @returns {boolean}
 */
export function frameFlag(value, fallback) {
    return typeof value === 'boolean' ? value : !!fallback;
}

/**
 * «Звенит» ли диалог: показывать тост и учитывать сообщение в бейдже.
 *
 * Источник истины — поля УРОВНЯ КАДРА `new_message` (`muted`/`archived`): их
 * считает сервер, с наследованием глобальных настроек уведомлений (mute всей
 * категории «личные чаты» с телефона фронту иначе не виден) и по ВСЕМ чатам
 * аккаунта, а не только по загруженной странице списка.
 *
 * Кадр перебивает локальное знание в обе стороны: `muted:false` от сервера
 * означает «здесь mute сняли», даже если наш список диалогов ещё говорит
 * обратное. Локальные значения используются только когда полей в кадре нет.
 *
 * @param {?object} frame — WS-кадр целиком
 * @param {boolean} localMuted
 * @param {boolean} localArchived
 * @returns {boolean}
 */
export function dialogRings(frame, localMuted, localArchived) {
    const f = frame && typeof frame === 'object' ? frame : {};
    if (frameFlag(f.muted, localMuted)) return false;
    if (frameFlag(f.archived, localArchived)) return false;
    return true;
}

/**
 * Публикует контракт для формы настроек: применить новый белый список.
 *
 * Через `window.ForgeApp`, а не импортом — по тем же причинам, что и
 * `telegramKeys` в keynav.js: модалка настроек открывается и без включённого
 * плагина, и статический импорт панели в её граф был бы лишним.
 *
 * `refresh` делает три вещи, и все три обязательны: сбрасывает кэш разбора,
 * перерисовывает колонку (список и счётчики папок) и пересчитывает бейдж ✈.
 * Без последнего цифра на кнопке ещё считала бы только что скрытые чаты.
 */
function publishFilterContract() {
    if (typeof window === 'undefined') return;
    const app = window.ForgeApp || (window.ForgeApp = {});
    app.telegramFilter = {
        refresh: () => {
            invalidateVisibilityFilter();
            dialogs.renderAll();
            recomputeUnread();
        },
    };
}

/**
 * Показывается ли чат при действующем белом списке из настроек.
 *
 * Отдельно от [`dialogRings`] СОЗНАТЕЛЬНО: та функция чистая, решает только по
 * кадру и локальным флагам и покрыта тестами в этом качестве. Видимость же
 * зависит от настроек и от загруженного списка, то есть от состояния вкладки.
 *
 * При выключенном фильтре всегда `true` — поведение до фичи.
 */
function dialogShown(dialogId) {
    return dialogIdVisible(
        dialogId,
        currentVisibilityFilter(),
        tgState.dialogs,
        tgState.folders,
    );
}

/**
 * Заглушён ли диалог по всему, что знает вкладка.
 *
 * Сначала список (там значение с сервера и оно свежее всего), затем карта
 * диалогов вне страницы. Неизвестный чат считается незаглушённым — но до этого
 * дело доходит только у старого сервера, который полей в кадре не шлёт.
 */
function isMutedLocally(dialogId) {
    const id = String(dialogId);
    if (local.muted.has(id)) return true;
    const extra = local.flagsExtra.get(id);
    return !!(extra && extra.muted);
}

/** Архивный ли диалог по всему, что знает вкладка (см. isMutedLocally). */
function isArchivedLocally(dialogId) {
    const id = String(dialogId);
    const d = dialogs.findDialog(id);
    if (d) return !!d.archived;
    const extra = local.flagsExtra.get(id);
    return !!(extra && extra.archived);
}

/**
 * Синхронизирует локальные флаги диалога с кадром `new_message`.
 *
 * Кадр авторитетнее списка: страницу `/dialogs` мы читали давно, а чат могли
 * заглушить или убрать в архив с телефона минуту назад. Диалог со страницы
 * патчится точечно (через штатный `dialogs.onNotifySettings`, чтобы не
 * дублировать логику строки и счётчиков папок), диалог вне страницы —
 * запоминается в `flagsExtra`.
 *
 * Кадр без обоих полей (старый сервер) не трогает ничего: перезаписать знание
 * списка выдуманным `false` было бы хуже, чем не знать.
 */
function rememberFrameFlags(dialogId, frame) {
    const id = String(dialogId);
    const f = frame && typeof frame === 'object' ? frame : {};
    const muted = typeof f.muted === 'boolean' ? f.muted : null;
    const archived = typeof f.archived === 'boolean' ? f.archived : null;
    if (muted === null && archived === null) return false;

    const d = dialogs.findDialog(id);
    if (!d) {
        const prev = local.flagsExtra.get(id) || { muted: false, archived: false };
        local.flagsExtra.set(id, {
            muted: muted === null ? prev.muted : muted,
            archived: archived === null ? prev.archived : archived,
        });
        return true;
    }
    if (muted !== null && !!d.muted !== muted) {
        d.muted = muted;
        dialogs.onNotifySettings({ dialog_id: id, muted });
    }
    if (archived !== null && !!d.archived !== archived) {
        // Смена папки меняет саму ПРИНАДЛЕЖНОСТЬ строки к списку
        // (~/inbox ⇄ ~/archive) — точечным патчем это не выражается.
        d.archived = archived;
        dialogs.renderAll();
    }
    return true;
}

/**
 * Учитывает непрочитанное по диалогу, которого нет в загруженной странице.
 *
 * `dialogs.bumpUnread` по такому чату молча выходит: DialogDto нет, править
 * нечего. Из-за этого бейдж занижался там же, где тост, наоборот, всплывал.
 * Считаем сами — и ровно по тому же гейту, что и тост, чтобы цифра на ✈ и
 * поведение уведомлений не расходились.
 *
 * «Не звенит» означает не просто «это сообщение не считаем», а «весь чат из
 * бейджа выпал»: кадр с muted/archived — это первое, из чего вкладка узнаёт,
 * что чат заглушили или убрали в архив с телефона (updateNotifySettings до нас
 * может и не дойти — например, при архивации). Накопленное до этого момента
 * непрочитанное обязано уйти вместе с ним, иначе цифра на ✈ продолжает считать
 * ровно тот чат, от которого пользователь только что отписался, и снять её
 * можно только перезагрузкой вкладки. Обработчик `notify_settings` делает то же
 * самое (см. forgetPendingUnread там) — здесь это вторая, кадровая нога.
 */
function noteUnknownUnread(dialogId, frame) {
    const id = String(dialogId);
    if (!id || dialogs.findDialog(id)) return false;
    if (!dialogRings(frame, isMutedLocally(id), isArchivedLocally(id))) {
        forgetPendingUnread(id);
        return false;
    }
    // Скрытый белым списком чат не должен копить непрочитанное: цифра на ✈
    // вела бы в чат, которого в колонке нет.
    if (!dialogShown(id)) {
        forgetPendingUnread(id);
        return false;
    }
    local.pendingUnread.set(id, (local.pendingUnread.get(id) || 0) + 1);
    return true;
}

/** Снимает накопленное непрочитанное по диалогу вне страницы (его прочитали). */
function forgetPendingUnread(dialogId) {
    local.pendingUnread.delete(String(dialogId));
}

/** Переключает интерфейс на вкладку Telegram. */
function focusTelegramTab() {
    // Через клик по кнопке, а не switchTab(): tabs.js уже импортирует этот
    // модуль, и обратный импорт замкнул бы граф модулей в цикл.
    if ($telegramToggle && typeof $telegramToggle.click === 'function') {
        $telegramToggle.click();
    }
}

/**
 * Тост о входящем сообщении.
 *
 * Условия показа (все обязательны):
 * - сообщение чужое (`out: false`) — свои исходящие никогда не всплывают;
 * - диалог «звенит»: не заглушён и не в архиве (см. dialogRings);
 * - либо вкладка Telegram не активна, либо открыт ДРУГОЙ диалог (иначе
 *   пользователь и так смотрит на это сообщение);
 * - по этому диалогу не было тоста последние 5с (иначе поток сообщений
 *   заливает экран).
 *
 * Гейт mute держится на полях КАДРА, а не на локальном Set: Set собирается из
 * загруженной страницы `/dialogs`, то есть знает про верхнюю сотню чатов, а
 * события WS приходят по всему аккаунту. Именно поэтому раньше «ненужный» чат
 * (заглушённый, заглушённый глобально по категории или архивный), не попавший
 * в страницу, считался незаглушённым и исправно всплывал тостом. Кадр
 * `new_message` несёт готовое решение сервера — его и слушаем; локальные
 * значения остаются только фолбэком для старого сервера, который этих полей
 * не шлёт.
 *
 * Клик по тосту переключает на вкладку и открывает диалог.
 *
 * @param {object} msg — MessageDto
 * @param {?object} [frame] — WS-кадр целиком (поля muted/archived)
 */
function maybeToast(msg, frame) {
    if (!msg || msg.out) return null;
    const id = String(msg.dialog_id || '');
    if (!id) return null;
    if (!dialogRings(frame, isMutedLocally(id), isArchivedLocally(id))) return null;
    // «Не показывать чат» означает и «не всплывать от него тостом»: иначе
    // спрятанный чат продолжал бы дёргать пользователя, а открыть его кликом
    // по тосту было бы некуда — в колонке его нет.
    if (!dialogShown(id)) return null;

    const onTelegramTab = !!$telegram && !$telegram.hidden;
    const isActive = onTelegramTab && String(tgState.activeDialogId) === id;
    if (isActive) return null;

    const now = Date.now();
    if (!toastAllowed(local.lastToastAt.get(id) || 0, now, TOAST_THROTTLE_MS)) return null;
    local.lastToastAt.set(id, now);

    const dialog = dialogs.findDialog(id);
    const title = (dialog && dialog.title) || (msg.sender && msg.sender.first_name) || id;
    const toast = notify({
        level: 'info',
        title: truncate(title, 40),
        body: truncate(msg.text || '[media]', 80),
    });
    if (toast) {
        toast.style.cursor = 'pointer';
        toast.addEventListener('click', (e) => {
            // Клик по кресту закрывает тост — не перехватываем его.
            if (e.target && e.target.classList
                && e.target.classList.contains('echo-toast-close')) return;
            focusTelegramTab();
            openDialogById(id);
        });
    }
    return toast;
}

/**
 * Открывает диалог: подсветка в колонке + загрузка истории + сброс unread.
 *
 * Точка входа и для клика по списку, и для клика по тосту.
 */
export function openDialogById(dialogId) {
    const id = String(dialogId);
    // Индикатор чужого набора относится к ПРОШЛОМУ диалогу: гасим строку и
    // снимаем таймеры до переключения, иначе при быстром переборе диалогов
    // они копятся и мигают в новой ленте.
    clearTypingIndicator();
    // На узком экране колонка контактов выехала поверх ленты — после выбора
    // диалога она обязана уйти, иначе пользователь смотрит на список вместо
    // только что открытой переписки. На десктопе — no-op.
    closeTgContactsAfterSelect();
    // Курсор клавиатуры указывал на сообщение ПОКИНУТОГО диалога: в новой
    // ленте тот же message_id принадлежит другому сообщению (нумерация у
    // каждого чата своя), и первое же j/k увело бы не туда.
    if (String(tgState.activeDialogId || '') !== id) resetKeynav();
    dialogs.selectDialog(id);
    const dialog = dialogs.findDialog(id);
    // Открытый диалог считается прочитанным: гасим счётчик локально сразу
    // (сервер подтвердит событием read/in из POST /read). Диалога может не
    // быть в загруженной странице — тогда гасить надо накопленное нами.
    dialogs.setUnread(id, 0);
    forgetPendingUnread(id);
    recomputeUnread();
    chat.openDialog(id, dialog);
}

/**
 * Карта обработчиков WS-кадров.
 *
 * Покрывает ВСЕ варианты `TgServerMsg` (plugins/telegram/src/ws/protocol.rs),
 * включая те, что начнут приходить в следующих фазах: заглушки стоят
 * намеренно, чтобы новый тип события не сыпал «no handler» в консоль.
 * Неизвестный тип обрабатывается в ws.js (debug-лог, без исключения).
 */
export function buildHandlers() {
    if (local.handlers) return local.handlers;
    local.handlers = {
        // --- соединение ---
        open: () => {
            setMode(tgState.loginState === 'authorized' ? 'connected' : 'offline');
            // Пока WS лежал, события терялись — перечитываем состояние.
            refreshAll();
        },
        // Первые обрывы — RECONNECTING (лестница backoff'а ещё не пройдена,
        // связь скорее всего вернётся через секунду). Когда ступени кончились,
        // честнее сказать OFFLINE: попытки продолжаются, но пользователь уже
        // должен понимать, что сервер не отвечает.
        close: (info) => setMode(info && info.exhausted ? 'offline' : 'reconnecting'),
        lat: (ms) => setLatency(ms),
        resync: () => {
            chat.systemLine('resync');
            refreshAll();
        },

        // --- логин ---
        login_state: (msg) => applyLoginState(msg),

        // --- сообщения ---
        // Кадр приходит целиком: кроме MessageDto в нём есть поля уровня
        // кадра `muted`/`archived` — решение СЕРВЕРА про то, звенит ли диалог.
        // Оно посчитано с наследованием глобальных настроек уведомлений и
        // известно даже для чатов, которых фронт никогда не показывал,
        // поэтому и тост, и бейдж принимают решение по нему, а не по
        // локальному Set (тот знает только загруженную страницу списка).
        new_message: (frame) => {
            const dto = frame && frame.message;
            if (!dto) return;
            const id = String(dto.dialog_id || '');
            const isActive = String(tgState.activeDialogId) === id;
            // Флаги запоминаем ДО отрисовки: и строка списка, и бейдж ниже
            // должны работать уже по свежему состоянию.
            rememberFrameFlags(id, frame);
            const drawn = chat.onNewMessage(dto);
            dialogs.onNewMessage(dto);
            if (isActive && drawn && !dto.out) {
                // Диалог открыт — ставим в очередь отметку о прочтении.
                // Она уйдёт ОДНИМ запросом на пачку сообщений и только если
                // вкладка действительно видима (см. scheduleRead).
                scheduleRead(id, dto.id);
            }
            if (!dto.out && !isActive) noteUnknownUnread(id, frame);
            recomputeUnread();
            maybeToast(dto, frame);
        },
        message_edited: (msg) => {
            if (msg && msg.message) chat.onEdited(msg.message);
        },
        message_deleted: (msg) => chat.onDeleted(msg),

        // --- присутствие ---
        typing: (msg) => onTyping(msg),
        online: (msg) => {
            dialogs.onOnline(msg);
            // Открытый диалог показывает статус ещё и в шапке сессии.
            if (msg && String(msg.user_id) === String(tgState.activeDialogId)) {
                chat.refreshHead();
            }
        },
        read: (msg) => {
            // `in` — прочитали мы (возможно, с телефона): гасим счётчик и
            // запоминаем границу, чтобы не слать свою отметку следом.
            if (msg && msg.direction === 'in') noteReadFromServer(msg.dialog_id, msg.max_id);
            dialogs.onRead(msg);
            // `out` — прочитали нас; ленты это касается, счётчиков нет.
            chat.onRead(msg);
            recomputeUnread();
        },

        // --- папки и mute ---
        // Mute переключили (с любого устройства): точечный патч строки +
        // пересчёт бейджа — Set заглушённых пересоберётся в recomputeUnread.
        //
        // Диалога может не быть в загруженной странице (onNotifySettings тогда
        // вернёт false) — его mute запоминаем отдельно, иначе тосты по нему
        // продолжат приходить, а именно на это жалуется пользователь.
        notify_settings: (msg) => {
            const known = dialogs.onNotifySettings(msg);
            if (msg && msg.dialog_id != null) {
                const id = String(msg.dialog_id);
                const muted = !!msg.muted;
                if (!known) {
                    const prev = local.flagsExtra.get(id) || { muted: false, archived: false };
                    local.flagsExtra.set(id, { muted, archived: prev.archived });
                }
                // Заглушённый чат из бейджа выпадает целиком, включая уже
                // накопленное по нему непрочитанное.
                if (muted) forgetPendingUnread(id);
            }
            recomputeUnread();
        },
        // Глобальные настройки уведомлений («заглушить все личные чаты» с
        // телефона): сервер прислал новые дефолты категорий.
        notify_defaults_changed: (msg) => onNotifyDefaultsChanged(msg),
        // Папки создали/удалили/пересортировали — перечитать GET /folders
        // и перерисовать секцию // DIRECTORIES вместе со списком.
        folders_changed: () => {
            dialogs.loadFolders().catch(() => {
                // not_connected — секция обновится со следующим resync.
            });
        },

        // --- прогресс передачи файлов ---
        // Вторая нога отправки (devforge → Telegram): карточка ищется по
        // upload_id, чужие и уже завершённые события игнорируются самим
        // обработчиком.
        upload_progress: (msg) => onUploadProgress(msg),
        // Прогресс «сервер тянет файл из Telegram»: карточка ищется по
        // (dialog_id, message_id), события не про открытый диалог
        // игнорируются самим обработчиком.
        download_progress: (msg) => onDownloadProgress(msg),
    };
    return local.handlers;
}

/**
 * WS `notify_defaults_changed` — сменились ГЛОБАЛЬНЫЕ дефолты уведомлений
 * (users / chats / broadcasts).
 *
 * Пересчитать mute поимённо на фронте нельзя: дефолт применяется ко всем
 * диалогам с УНАСЛЕДОВАННЫМИ настройками, а какие из них унаследованные —
 * знает только сервер (в `DialogDto.muted` приходит уже разрешённое
 * значение, без признака «откуда взялось»). Перечислять их в кадре сервер
 * тоже не может — их бывают тысячи. Поэтому единственный честный путь —
 * перечитать список диалогов и пересобрать бейдж по нему.
 *
 * Производные карты сбрасываются: их значения посчитаны по СТАРЫМ дефолтам и
 * с этого момента врут. Свежие прилетят с ближайшими кадрами.
 *
 * @param {{users?: boolean, chats?: boolean, broadcasts?: boolean}} msg
 * @returns {boolean} запущена ли перезагрузка списка
 */
export function onNotifyDefaultsChanged(msg) {
    tgState.notifyDefaults = {
        users: !!(msg && msg.users),
        chats: !!(msg && msg.chats),
        broadcasts: !!(msg && msg.broadcasts),
    };
    local.flagsExtra.clear();
    local.pendingUnread.clear();
    // Кадры могут прийти пачкой (пользователь щёлкает тумблеры в настройках
    // телефона) — вторая перезагрузка поверх незавершённой первой только
    // приблизила бы FLOOD_WAIT.
    if (local.defaultsReloading) {
        recomputeUnread();
        return false;
    }
    local.defaultsReloading = true;
    dialogs.loadDialogs()
        .then(() => { local.dialogsLoaded = true; })
        .catch(() => { /* not_connected — обновит следующий resync */ })
        .then(() => {
            local.defaultsReloading = false;
            recomputeUnread();
        });
    return true;
}

// ---------------------------------------------------------------------------
// Отметка о прочтении
// ---------------------------------------------------------------------------

/**
 * Ставит диалог в очередь на отметку о прочтении.
 *
 * Три триггера ведут сюда: открытие диалога, докрутка ленты до низа и приход
 * нового сообщения в открытый диалог. Все они склеиваются debounce'ом
 * ([`READ_DEBOUNCE_MS`]) и коалесцируются по максимальному `message_id`:
 * открыть чат с двадцатью непрочитанными обязано стоить ОДИН запрос, а не
 * двадцать.
 *
 * Скрытая вкладка (или открытая, но не на Telegram) не отмечает ничего —
 * см. `readAllowed`.
 *
 * @param {string} dialogId
 * @param {number} maxId — 0 ⇒ «прочитать всё»
 * @returns {boolean} принято ли в очередь
 */
export function scheduleRead(dialogId, maxId) {
    const hidden = !!($telegram && $telegram.hidden);
    const visibility = typeof document !== 'undefined' ? document.visibilityState : 'visible';
    if (!readAllowed(visibility, hidden)) return false;

    const id = String(dialogId);
    const want = Math.max(0, Number(maxId) || 0);
    const already = local.readSent.get(id) || 0;
    // Уже отмечали до этого id — повторный запрос ничего не изменит (и на
    // прокрутке ленты их было бы по одному на каждое событие scroll).
    if (want > 0 && want <= already) return false;

    const pending = local.readPending.get(id) || 0;
    local.readPending.set(id, Math.max(pending, want));
    if (local.readTimer) return true;
    local.readTimer = setTimeout(() => {
        local.readTimer = null;
        flushRead();
    }, READ_DEBOUNCE_MS);
    return true;
}

/**
 * Отправляет накопленные отметки о прочтении.
 *
 * Локальный сброс счётчика делается ПОСЛЕ успеха: сервер разошлёт ещё и
 * `read`/`in` всем вкладкам, но своя собственная не должна ждать эха, чтобы
 * бейдж погас без задержки.
 */
async function flushRead() {
    const batch = Array.from(local.readPending.entries());
    local.readPending.clear();
    for (const [id, maxId] of batch) {
        try {
            await markRead(id, maxId);
            local.readSent.set(id, Math.max(local.readSent.get(id) || 0, maxId));
            dialogs.setUnread(id, 0);
            forgetPendingUnread(id);
            recomputeUnread();
        } catch (_) {
            // not_connected / flood_wait — не критично: следующее открытие
            // диалога или новое сообщение попробуют снова.
        }
    }
}

/**
 * Запоминает, что диалог прочитан ИЗВНЕ (другое устройство прислало
 * `read`/`in`).
 *
 * Без этого наша вкладка при первом же скролле отправила бы «свою» отметку
 * до того же id — лишний запрос, который ничего не меняет.
 */
function noteReadFromServer(dialogId, maxId) {
    const id = String(dialogId);
    const v = Math.max(0, Number(maxId) || 0);
    if (v > 0) local.readSent.set(id, Math.max(local.readSent.get(id) || 0, v));
    local.readPending.delete(id);
    // Диалога может не быть в загруженной странице — dialogs.onRead такой
    // счётчик обнулить не может, а бейдж обязан погаснуть.
    forgetPendingUnread(id);
}

/**
 * Индикатор «печатает…» с автосбросом по каждому участнику.
 *
 * Сброс по таймеру обязателен: Telegram НЕ присылает «перестал печатать»,
 * он просто перестаёт присылать typing. Без таймера индикатор залипал бы
 * навсегда.
 *
 * Состояние — по КЛЮЧУ `диалог|пользователь`, а не одна строка на весь
 * плагин: в группе печатать могут несколько человек одновременно, и каждый
 * гаснет по своему таймеру. События неактивных диалогов тоже запоминаются
 * (переключение на такой диалог сразу покажет актуальную строку), но в DOM
 * попадает только активный.
 *
 * @param {{dialog_id: string, user_id: string, action: string}} msg
 */
function onTyping(msg) {
    if (!msg || !msg.dialog_id || !msg.user_id) return;
    const dialogId = String(msg.dialog_id);
    const key = `${dialogId}|${msg.user_id}`;
    const prev = local.typing.get(key);
    if (prev && prev.timer) clearTimeout(prev.timer);

    if (msg.action === 'cancel') {
        local.typing.delete(key);
        renderTyping();
        return;
    }

    const timer = setTimeout(() => {
        local.typing.delete(key);
        renderTyping();
    }, TYPING_RESET_MS);
    local.typing.set(key, { dialogId, label: typingLabel(dialogId, msg.user_id), timer });
    renderTyping();
}

/**
 * Как подписать печатающего.
 *
 * В личном чате `dialog_id === user_id`, поэтому имя собеседника — это title
 * диалога. Сверка id ОБЯЗАТЕЛЬНА: без неё любой участник группы (а в
 * теории и второй человек в том же событии) получал бы имя самого диалога,
 * и строка выглядела бы как «Sofia, Sofia are typing…».
 *
 * Для участника группы имени у нас нет — Telegram присылает только user_id, а
 * тянуть профиль ради индикатора значит делать RPC на каждое нажатие клавиши
 * собеседником. Поэтому: свой диалог с этим человеком (если он есть в
 * списке) → его title, иначе сам id.
 */
function typingLabel(dialogId, userId) {
    const uid = String(userId);
    const dialog = dialogs.findDialog(dialogId);
    if (dialog && dialog.kind === 'user' && dialog.title && String(dialog.id) === uid) {
        return String(dialog.title);
    }
    const known = dialogs.findDialog(uid);
    if (known && known.title) return String(known.title);
    return uid;
}

/** Перерисовывает #tg-typing по активному диалогу. */
function renderTyping() {
    if (!$tgTyping) return;
    const active = tgState.activeDialogId ? String(tgState.activeDialogId) : null;
    const names = [];
    for (const entry of local.typing.values()) {
        if (entry.dialogId === active) names.push(entry.label);
    }
    $tgTyping.textContent = typingLine(names);
}

/**
 * Гасит индикатор и снимает ВСЕ таймеры.
 *
 * Вызывается при смене диалога, уходе страницы в фон и logout'е: без этого
 * быстрое переключение диалогов оставляло бы висящие setTimeout'ы со
 * ссылками на DOM.
 */
export function clearTypingIndicator() {
    for (const entry of local.typing.values()) {
        if (entry.timer) clearTimeout(entry.timer);
    }
    local.typing.clear();
    if ($tgTyping) $tgTyping.textContent = '';
}

/**
 * Перечитывает состояние с сервера (после реконнекта/resync).
 *
 * Диалоги — всегда; история — только для открытого диалога. Ошибки
 * глотаются: если сервер ещё не готов, следующий `open`/`resync` повторит.
 */
export async function refreshAll() {
    if (!tgState.enabled) return;
    try {
        const status = await getStatus();
        if (status && status.enabled === true) applyLoginState(status);
        if (!status || status.state !== 'authorized') return;
    } catch (_) {
        return;
    }
    try {
        await dialogs.loadDialogs();
        local.dialogsLoaded = true;
        recomputeUnread();
        const active = tgState.activeDialogId;
        if (active) chat.openDialog(String(active), dialogs.findDialog(String(active)));
    } catch (_) {
        // not_authorized / not_connected — состояние покажет login_state.
    }
}

/**
 * Поднимает постоянное WS-соединение, если оно уместно.
 *
 * Уместно = плагин включён И аккаунт авторизован. Соединение НЕ привязано к
 * вкладке: бейдж и тосты обязаны работать из terminal/tasks, а сервер вещает
 * события без фильтрации именно ради этого.
 */
function ensureWs() {
    if (!tgState.enabled || tgState.loginState !== 'authorized') return;
    if (isTelegramWsOpen()) return;
    connectTelegramWs(buildHandlers());
}

/**
 * Собирает подпись пользователя для titlebar: `@username`, иначе имя,
 * иначе телефон, иначе id.
 *
 * @param {?{id?: string, username?: string, first_name?: string, phone?: string}} u
 * @returns {string}
 */
export function selfUserLabel(u) {
    if (!u || typeof u !== 'object') return '';
    if (u.username) return '@' + u.username;
    if (u.first_name) return String(u.first_name);
    if (u.phone) return String(u.phone);
    return u.id ? String(u.id) : '';
}

/**
 * Применяет LoginStateDto к UI вкладки.
 *
 * ЕДИНАЯ точка: вызывается и ответом `GET /api/telegram/status`, и
 * WS-событием `login_state`. Поэтому обязана быть идемпотентной —
 * повторный вызов с тем же dto ничего не ломает (в частности, не сбрасывает
 * введённые поля и не перезапускает countdown FLOOD_WAIT).
 *
 * Раскладка:
 *   not_configured | disconnected | code_sent | password_required
 *       → оверлей #tg-login на нужном шаге, кнопка выхода скрыта;
 *   authorized
 *       → оверлей скрыт, #tg-self-user заполнен, выход доступен.
 *
 * @param {{state: string, phone: ?string, hint: ?string, error: ?string,
 *   user: ?object}} dto
 */
export function applyLoginState(dto) {
    if (!dto || typeof dto !== 'object') return;
    const prev = tgState.loginState;
    tgState.loginState = dto.state || 'not_configured';
    tgState.selfUser = dto.user || null;

    const authorized = tgState.loginState === 'authorized';

    if ($tgSelfUser) $tgSelfUser.textContent = selfUserLabel(dto.user);
    if ($tgLogout) $tgLogout.hidden = !authorized;

    // Оверлей и шаги wizard'а — целиком забота login.js.
    renderLoginState(dto);

    // Mode-бейдж: авторизация — необходимое условие CONNECTED, но
    // подтверждает его живой WS.
    if (!authorized) setMode('offline');
    else setMode(isTelegramWsOpen() ? 'connected' : 'reconnecting');

    // Разлогин (в том числе с другого устройства — сервер пришлёт
    // login_state disconnected) обязан вычистить данные аккаунта из памяти
    // вкладки, включая objectURL'ы медиа.
    if (!authorized && prev === 'authorized') {
        clearAccountData();
        disconnectTelegramWs();
        return;
    }

    // Вошли (в том числе через wizard в уже открытой странице) — поднимаем
    // постоянный WS и подтягиваем список диалогов.
    if (authorized) {
        ensureWs();
        if (prev !== 'authorized' || !local.dialogsLoaded) {
            dialogs.loadDialogs()
                .then(() => {
                    local.dialogsLoaded = true;
                    recomputeUnread();
                })
                .catch(() => { /* not_connected — обновит следующий resync */ });
        }
    }
}

/**
 * Очищает всё, что относилось к прошлому аккаунту (данные + UI).
 *
 * Ревокацию objectURL'ов делает `clearTgAccountData` в state.js — там же,
 * где эти URL'ы создаются, чтобы утечка после logout не могла появиться
 * из-за расхождения двух копий логики.
 */
export function clearAccountData() {
    // Незавершённые отправки принадлежат прошлому аккаунту: активный XHR
    // обрывается, очередь и её карточки исчезают.
    teardownUpload();
    clearTypingIndicator();
    // Отметки о прочтении прошлого аккаунта отправлять некуда и незачем.
    if (local.readTimer) {
        clearTimeout(local.readTimer);
        local.readTimer = null;
    }
    local.readPending.clear();
    local.readSent.clear();
    clearTgAccountData();
    setUnreadBadge(0);
    local.lastToastAt.clear();
    local.muted.clear();
    local.flagsExtra.clear();
    local.pendingUnread.clear();
    local.dialogsLoaded = false;
    // Присутствие прошлого аккаунта после входа в другой — дезинформация.
    dialogs.clearPresence();
    dialogs.clearActive();
    dialogs.renderAll();
    chat.resetChat();
    if ($tgSessionHead) $tgSessionHead.textContent = 'SESSION ▸ —';
}

/**
 * Выход из аккаунта по кнопке #tg-logout.
 *
 * Подтверждение обязательно: logout удаляет файл сессии на сервере, и
 * следующий вход потребует код из Telegram заново.
 */
export async function requestLogout() {
    // eslint-disable-next-line no-alert
    const ok = typeof window.confirm === 'function'
        ? window.confirm('>> confirm logout? Потребуется повторный вход по коду из Telegram.')
        : true;
    if (!ok) return;
    if ($tgLogout) $tgLogout.disabled = true;
    try {
        const dto = await apiLogout();
        resetLoginWizard();
        applyLoginState(dto);
    } catch (_) {
        // Даже если сервер ответил ошибкой, локально показываем реальное
        // состояние — его пришлёт следующий статус/WS-кадр.
        getStatus()
            .then((status) => { if (status) applyLoginState(status); })
            .catch(() => { /* плагин выключен */ });
    } finally {
        if ($tgLogout) $tgLogout.disabled = false;
    }
}

/** Три состояния соединения — единственный источник и бейджа, и точки. */
const CONN_MODES = ['connected', 'reconnecting', 'offline'];

/**
 * Отражает состояние соединения СРАЗУ в двух местах: Mode-бейдж
 * `#tg-statusbar` и пульс-точка в `#tg-titlebar`.
 *
 * Обе индикации ставятся одним вызовом намеренно: разъехавшиеся «CONNECTED» в
 * статусбаре и красная точка в шапке — худший вид вранья, чем отсутствие
 * индикации вовсе. Цвета живут в CSS (`.tg-mode-*` и `#tg-titlebar.tg-conn-*`).
 *
 * Вызовы событийные (колбэки open/close/lat из ws.js и applyLoginState) —
 * никакого поллинга: таймер тикал бы и в скрытой вкладке.
 *
 * @param {'connected'|'reconnecting'|'offline'} mode
 * @returns {'connected'|'reconnecting'|'offline'} применённое состояние
 */
export function setMode(mode) {
    const m = CONN_MODES.includes(mode) ? mode : 'offline';
    if ($tgMode) {
        $tgMode.textContent = m.toUpperCase();
        $tgMode.className = 'tg-mode tg-mode-' + m;
    }
    if ($tgTitlebar) {
        for (const known of CONN_MODES) {
            $tgTitlebar.classList.toggle('tg-conn-' + known, known === m);
        }
    }
    // Связи нет — прошлое значение задержки становится враньём.
    if (m !== 'connected' && tgState.lat != null) setLatency(null);
    return m;
}

/**
 * Обновляет LAT в статусбаре.
 *
 * Сглаживание скользящим средним по последним [`LAT_WINDOW`] замерам: оценка
 * идёт по дрейфу серверных ping'ов (ws.js::latencyFromPingDrift), а он шумит
 * на десятки миллисекунд из-за планировщика — без сглаживания цифра прыгала
 * бы на каждом такте и читалась как поломка.
 *
 * `null` (нет соединения) ОЧИЩАЕТ окно: усреднять замеры разных соединений
 * бессмысленно, а показывать старое значение при разорванном WS — вредно.
 *
 * @param {?number} ms
 * @returns {?number} сглаженное значение
 */
export function setLatency(ms) {
    if (Number.isFinite(ms)) {
        local.latSamples.push(Math.max(0, Math.round(ms)));
        if (local.latSamples.length > LAT_WINDOW) local.latSamples.shift();
        tgState.lat = smoothedLatency(local.latSamples);
    } else {
        local.latSamples.length = 0;
        tgState.lat = null;
    }
    renderLatency();
    return tgState.lat;
}

/**
 * Рисует текущее значение LAT, ничего не пересчитывая.
 *
 * Нужна отдельно от setLatency: при открытии вкладки значение уже могло быть
 * измерено (WS постоянный и живёт с любой вкладки), и повторный «замер» тем же
 * числом исказил бы окно сглаживания.
 */
function renderLatency() {
    if ($tgLat) $tgLat.textContent = tgState.lat == null ? '--' : `${tgState.lat}ms`;
}

/**
 * Гейт кнопки ✈ по состоянию плагина на сервере.
 *
 * Вызывается один раз из bootstrap. Ошибка/404 — штатный случай
 * «плагин выключен»: кнопка остаётся скрытой, ничего не логируем.
 *
 * Если аккаунт уже авторизован, здесь же поднимается ПОСТОЯННЫЙ WS — не
 * дожидаясь открытия вкладки. Иначе бейдж и тосты появлялись бы только
 * после того, как пользователь хоть раз зашёл в Telegram, то есть ровно
 * тогда, когда они уже не нужны.
 */
export async function initTelegramGate() {
    if (local.gated) return;
    local.gated = true;
    try {
        const status = await getStatus();
        if (!status || status.enabled !== true) {
            tgState.enabled = false;
            setToggleVisible(false);
            return;
        }
        tgState.enabled = true;
        setToggleVisible(true);
        // applyLoginState сам поднимет WS и подтянет диалоги, если вошли.
        applyLoginState(status);
    } catch (_) {
        // 404 (плагин выключен) или недоступный сервер — кнопка скрыта.
        tgState.enabled = false;
        setToggleVisible(false);
    }
}

/**
 * Инициализация содержимого вкладки при первом открытии.
 * Идемпотентна.
 */
export function initTelegram() {
    if (local.initialized) return;
    local.initialized = true;
    if (!$telegram) return;

    setMode(isTelegramWsOpen() ? 'connected' : 'offline');
    renderLatency();
    if ($tgSessionHead) $tgSessionHead.textContent = 'SESSION ▸ —';

    // Wizard и кнопка выхода: обработчики навешиваются один раз.
    initLoginWizard({ onAuthorized: (dto) => applyLoginState(dto) });
    if ($tgLogout && !local.logoutBound) {
        local.logoutBound = true;
        $tgLogout.addEventListener('click', requestLogout);
    }

    // Список и лента: обработчики (клик по диалогу, скролл, prompt) вешаются
    // один раз, дальше всё живёт на событиях.
    dialogs.initDialogs({ onSelect: (id) => openDialogById(id) });
    chat.initChat({
        // chat.js сообщает «диалог просмотрен до maxId» (открытие и докрутка
        // ленты до низа); решение о запросе принимает scheduleRead.
        onRead: (id, maxId) => scheduleRead(id, maxId),
        // Присутствие живёт в dialogs.js — шапка сессии читает его оттуда.
        presenceOf: (id) => dialogs.presenceOf(id),
    });
    // Отправка файлов: picker ⇪, drag&drop по панели, вставка из буфера.
    initUpload();
    // Off-canvas контакты на узких экранах: ☰, подложка, Esc.
    initTelegramMobile();
    // Клавиатурная навигация по панели. Короткие ответы («Скопировано»,
    // «Непрочитанных нет») показываем тем же тостом, что и остальной Telegram,
    // — заводить второй канал сообщений ради двух строк не за чем.
    initKeynav({
        onNotice: (text) => notify({ level: 'info', title: 'Telegram', body: text, ttl: 1800 }),
    });
    publishFilterContract();

    // Переспрашиваем статус: вкладку могли открыть заметно позже гейта,
    // и состояние логина к этому моменту уже могло измениться.
    getStatus()
        .then((status) => {
            if (status && status.enabled === true) applyLoginState(status);
        })
        .catch(() => { /* плагин выключен — оверлей логина остаётся как есть */ });
}

/**
 * Страница ушла в фон (visibilitychange: hidden).
 *
 * WS закрывается: у скрытой страницы бейджа всё равно не видно, а держать
 * сокет и получать поток апдейтов «в никуда» — только расход батареи и
 * трафика. При возврате [`resumeTelegram`] переподключается и перечитывает
 * состояние, поэтому пропущенные события не теряются.
 *
 * ВАЖНО: это НЕ реакция на смену вкладки внутри devforge — при переходе на
 * terminal соединение обязано жить (см. tabs.js).
 */
export function suspendTelegram() {
    disconnectTelegramWs();
    clearTypingIndicator();
    setMode('offline');
}

/** Страница вернулась на передний план — восстановить соединение. */
export function resumeTelegram() {
    if (!tgState.enabled) return;
    getStatus()
        .then((status) => {
            if (status && status.enabled === true) applyLoginState(status);
        })
        .catch(() => { /* плагин выключен */ });
}

/** Полная остановка (beforeunload). */
export function teardownTelegram() {
    suspendTelegram();
    // Страница уходит: активная заливка обязана быть оборвана, иначе браузер
    // продолжит гнать файл, который уже некому принять.
    teardownUpload();
    // Таймер FLOOD_WAIT обязан умереть вместе со страницей: иначе он тикал
    // бы в фоне и держал ссылку на DOM.
    stopCountdown();
    // objectURL'ы медиа: без revoke бинарники живут в памяти до перезагрузки.
    // teardownMedia гасит ещё и лайтбокс с активным плеером и снимает
    // IntersectionObserver ленивых превью.
    teardownMedia();
    // Лента: ResizeObserver «следования» за сообщением бота и таймер его
    // истечения обязаны умереть вместе со страницей — иначе наблюдатель
    // переживёт её, держа ссылки на узлы, а таймер выстрелит в пустоту.
    chat.teardownChat();
    // Клавиатура: снимаем свой document-слушатель и оверлей шпаргалки, иначе
    // они переживут страницу вместе со ссылками на её узлы.
    teardownKeynav();
    clearTgAccountData();
    local.lastToastAt.clear();
    local.initialized = false;
}

/** Снимок состояния для e2e-тестов (по образцу echo/main.js). */
export function _debugState() {
    return {
        enabled: tgState.enabled,
        initialized: local.initialized,
        loginState: tgState.loginState,
        activeDialogId: tgState.activeDialogId,
        unreadTotal: tgState.unreadTotal,
        // Диагностика гейта уведомлений: сколько диалогов заглушено по
        // загруженной странице, сколько известно вне её и сколько
        // непрочитанного посчитано мимо списка.
        muted: local.muted.size,
        flagsExtra: local.flagsExtra.size,
        pendingUnread: local.pendingUnread.size,
        notifyDefaults: { ...tgState.notifyDefaults },
        lat: tgState.lat,
        latSamples: local.latSamples.slice(),
        mode: $tgMode ? $tgMode.textContent : null,
        wsOpen: isTelegramWsOpen(),
        dialogs: (tgState.dialogs || []).length,
    };
}
