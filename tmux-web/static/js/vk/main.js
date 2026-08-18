// tmux-web — VK plugin: точка сборки вкладки.
//
// Что здесь живёт:
//
//   initVkGate() — вызывается из bootstrap на старте страницы: спрашивает
//       GET /api/vk/status, показывает кнопку Ⓥ, если плагин включён, и
//       ПОДНИМАЕТ ПОСТОЯННЫЙ WS. Плагин выключен (404) — это НЕ ошибка:
//       кнопка просто остаётся скрытой, в консоль не шумим (иначе у всех, кто
//       запускает devforge без --vk, консоль засорялась бы на каждой
//       загрузке).
//   initVk()     — первое открытие вкладки (вызывается из tabs.js).
//   buildHandlers() — карта кадров WS (см. plugins/vk/src/ws/protocol.rs).
//   recomputeUnread() — ЕДИНСТВЕННЫЙ путь обновления бейджа на кнопке Ⓥ.
//   maybeToast() — тост о входящем с префиксом «VK · ».
//   publishFilterContract() — `window.ForgeApp.vkFilter.refresh` для формы
//       настроек: белый список обязан подействовать сразу после Save.
//   openDialogById() — общая точка входа для клика по списку и по тосту.
//   suspendVk()/resumeVk() — реакция на visibilitychange.
//   teardownVk() — beforeunload.
//   _debugState() — снимок состояния для e2e (по образцу echo/main.js).
//
// ПОЧЕМУ WS ПОДНИМАЕТСЯ В ГЕЙТЕ, А НЕ ПРИ ОТКРЫТИИ ВКЛАДКИ. Бейдж
// непрочитанного на кнопке Ⓥ и тосты о новых сообщениях обязаны работать,
// пока пользователь сидит в terminal/tasks. Если ждать первого входа в панель,
// они появятся ровно тогда, когда уже не нужны.
//
// ЧЕМ КАРТА КАДРОВ ОТЛИЧАЕТСЯ ОТ ТЕЛЕГРАМНОЙ.
//   + unread_total    — VK отдаёт общий счётчик готовым (событие LongPoll 80),
//                       и бейдж перестаёт быть суммой по загруженной странице;
//   + history_cleared — событие 13 не несёт id удалённых сообщений, только
//                       границу, поэтому message_deleted тут неприменим;
//   + dialog_changed  — события 51/52: перечитывается строка списка;
//   + analysis_*      — четвёрка кадров разбора чата через Claude; к LongPoll
//                       отношения не имеет вовсе, это поток локального
//                       процесса, вещаемый тем же сокетом;
//   − notify_defaults_changed — глобальных категорий mute у VK нет, глушение
//                       только per-peer, и ветки их пересчёта здесь тоже нет;
//   − folders_changed — папок нет, их место заняли 4 серверных фильтра.
//
// БЕЛЫЙ СПИСОК БЕСЕД применяется в ЧЕТЫРЁХ местах, и пропуск любого = утечка
// скрытой беседы наружу. Две точки здесь — бейдж (`recomputeUnread`) и тосты
// с накоплением непрочитанного (`maybeToast`/`noteUnknownUnread`), ещё две в
// `dialogs.js` — колонка и счётчики серверных фильтров.
//
// Что приезжает в следующих фазах: клавиатурная навигация (Ф11).
//
// Инвариант XSS: любые данные (имена бесед, тексты сообщений) попадают в DOM
// только через textContent. innerHTML в модулях плагина не используется.

import {
    $vkToggle, $vkUnreadBadge, $vkMode, $vkTitlebar, $vkLat, $vkSelfUser,
    $vkLogout, $vkTyping, $vkAnalysisToggle,
} from '../core/dom.js';
import { notify } from '../echo/notifications.js';

// `import * as vkApi` — защита от SyntaxError на закэшированной старой версии
// модуля: отсутствующая функция даёт «undefined is not a function» в одном
// месте, а не роняет весь модуль вместе с панелью.
import * as vkApi from './api.js';
import {
    initAnalysis, openAnalysis, teardownAnalysis, _debugAnalysisState,
    onAnalysisStarted, onAnalysisDelta, onAnalysisDone, onAnalysisError,
} from './analysis.js';
import { closeCaptcha, initCaptcha } from './captcha.js';
import * as vkChat from './chat.js';
import * as vkMedia from './media.js';
import * as vkUpload from './upload.js';
import * as vkDialogs from './dialogs.js';
import {
    mutedIds, readAllowed, toastAllowed, truncate, typingLine,
    unreadTotal, READ_DEBOUNCE_MS, TYPING_RESET_MS,
} from './format.js';
import { initKeynav, resetKeynav, teardownKeynav } from './keynav.js';
import { initLogin, renderLoginState, resetLogin, setLoginHint } from './login.js';
import { initVkMobile } from './mobile.js';
import {
    clearVkAccountData, currentVisibilityFilter, invalidateVisibilityFilter, resetVkState,
    vkState, _debugVkState,
} from './state.js';
import { dialogIdVisible, filterActive, visibleDialogs } from './visibility.js';
import { connectVkWs, disconnectVkWs, isVkWsOpen, _debugWsState } from './ws.js';
import { $vk } from '../core/dom.js';

/** Не чаще одного тоста на беседу за это окно, мс. */
export const TOAST_THROTTLE_MS = 5000;

/**
 * Префикс заголовка тоста.
 *
 * НЕ косметика. Тосты Telegram и VK падают в ОДИН стек `#echo-toasts`, и при
 * двух включённых панелях «Пётр Петров» без префикса не отвечает на
 * единственный вопрос, который у пользователя есть в этот момент: куда идти
 * отвечать. Клик по тосту переключает на нужную вкладку, но нажать его нужно
 * осознанно.
 */
export const TOAST_PREFIX = 'VK · ';

const local = {
    /** initVk() уже отработал. */
    initialized: false,
    /** initVkGate() уже отработал (успешно или нет). */
    gated: false,
    /** Обработчики кадров WS (собираются один раз). */
    handlers: null,
    /** Таймер дебаунса отметки о прочтении. */
    readTimer: null,
    /** dialog_id → последний отмеченный прочитанным id (защита от повторов). */
    readSent: new Map(),
    /** Список бесед уже загружен в этой сессии логина. */
    dataLoaded: false,
    /**
     * Соединение WS уже поднималось хотя бы раз (forge-oy0c).
     *
     * Отличает ПЕРВОЕ подключение от РЕКОННЕКТА. После первого историю только
     * что прочитали по REST, догонять нечего. После реконнекта поток кадров
     * прерывался, и всё, что пришло в это окно, в открытой ленте не появилось
     * бы никогда — там нужен догон.
     */
    everConnected: false,
    /** dialog_id → когда показывали прошлый тост (мс). */
    lastToastAt: new Map(),
    /**
     * Беседы, по которым тосты и бейдж заглушены.
     *
     * Производная от `muted` в DialogDto: пересобирается в recomputeUnread, то
     * есть и загрузка списка, и кадр `notify_settings` обновляют её сами.
     *
     * Список НЕПОЛОН: `GET /dialogs` отдаёт верхнюю страницу, а WS вещает по
     * ВСЕМУ аккаунту — про беседу вне страницы этот Set не знает ничего.
     * Поэтому один он гейтом быть не может, см. `flagsExtra`.
     */
    muted: new Set(),
    /**
     * `dialog_id` → `{muted}` для бесед ВНЕ загруженной страницы.
     *
     * Заполняется полями кадров (`new_message.muted`, `notify_settings`), где
     * решение принял СЕРВЕР. Именно это закрывает «тосты от заглушённых
     * бесед»: до сих пор фронт про такую беседу не знал ничего и считал её
     * звенящей. Запись живёт, пока беседа не появится в списке — там значение
     * свежее и авторитетнее.
     */
    flagsExtra: new Map(),
    /**
     * `dialog_id` → непрочитанное по беседам ВНЕ загруженной страницы.
     *
     * `dialogs.bumpUnread` по такой беседе молча выходит (DialogDto нет), и
     * без этой карты бейдж занижался бы ровно там, где тост, наоборот,
     * показывается.
     */
    pendingUnread: new Map(),
    /**
     * Последнее значение кадра `unread_total` (событие LongPoll 80) или null.
     *
     * НОВОЕ относительно Telegram: VK отдаёт общий счётчик готовым, и бейдж
     * перестаёт быть суммой по загруженной странице списка.
     */
    serverUnread: null,
    /**
     * Беседы, в которых непрочитанное появилось ПОСЛЕ последнего серверного
     * счётчика.
     *
     * Мост между двумя источниками. Событие 80 приходит на каждое изменение
     * счётчика — и на приход сообщения, и на прочтение, — но приходит СЛЕДОМ,
     * отдельным кадром. Без этой поправки бейдж на доли секунды показывал бы
     * прошлое значение при каждом новом сообщении, а «на глаз» это выглядит
     * как «бейдж не работает».
     *
     * Считаются БЕСЕДЫ, а не сообщения: что именно нумерует счётчик VK
     * (беседы или сообщения), из протокола не следует, и «+1 за беседу» — та
     * оценка, которая не может ошибиться в разы. Точное значение приезжает
     * следующим же кадром.
     */
    unreadSinceServer: new Set(),
    /** `dialogId|userId` → {dialogId, label, timer} — кто сейчас печатает. */
    typing: new Map(),
};

/**
 * Отмечает беседу прочитанной — с дебаунсом и без повторов.
 *
 * Дебаунс обязателен: в открытую беседу прилетает пачка сообщений, и без него
 * ушло бы двадцать запросов вместо одного при бюджете 3 rps. Память о
 * последнем отмеченном id — вторая половина той же защиты: прокрутка ленты
 * зовёт этот путь на каждом кадре скролла.
 *
 * Чтение НЕ отправляется, пока вкладка в фоне или панель закрыта: иначе
 * собеседник видел бы «прочитано», пока пользователь сидит в терминале.
 *
 * @param {string} dialogId
 * @param {number} maxId
 */
function scheduleRead(dialogId, maxId) {
    const id = String(dialogId);
    const max = Number(maxId) || 0;
    if (!id || max <= 0) return;
    const visibility = (typeof document !== 'undefined' && document.visibilityState) || 'visible';
    if (!readAllowed(visibility, $vk ? $vk.hidden : false)) return;
    if ((local.readSent.get(id) || 0) >= max) return;
    if (local.readTimer) clearTimeout(local.readTimer);
    local.readTimer = setTimeout(() => {
        local.readTimer = null;
        if ((local.readSent.get(id) || 0) >= max) return;
        local.readSent.set(id, max);
        vkApi.markRead(id, max)
            .then(() => {
                vkDialogs.setUnread(id, 0);
                forgetPendingUnread(id);
                recomputeUnread();
            })
            .catch(() => {
                // Не удалось — забываем отметку, чтобы следующая попытка
                // прошла: иначе беседа осталась бы «прочитанной» только у нас.
                local.readSent.delete(id);
            });
    }, READ_DEBOUNCE_MS);
}

/**
 * Загружает данные аккаунта при первом входе после авторизации.
 *
 * Идемпотентна в пределах сессии логина: `applyLoginState` зовёт её на каждом
 * подтверждении состояния (снапшот `/status`, ответ POST'а, кадр WS), а список
 * бесед стоит запроса из бюджета 3 rps.
 */
async function ensureData() {
    if (local.dataLoaded || vkState.loginState !== 'authorized') return;
    local.dataLoaded = true;
    try {
        await vkDialogs.loadDialogs();
        recomputeUnread();
    } catch (_) {
        // not_connected/flood_wait — панель покажет пустую колонку, повтор
        // придёт со следующим resync или открытием вкладки.
        local.dataLoaded = false;
    }
}

/** Три состояния соединения — единственный источник и бейджа, и точки. */
const CONN_MODES = ['connected', 'reconnecting', 'offline'];

/**
 * Показывает/прячет кнопку Ⓥ в сайдбаре.
 * @param {boolean} visible
 */
function setToggleVisible(visible) {
    if (!$vkToggle) return;
    $vkToggle.hidden = !visible;
}

/**
 * Пишет число в бейдж на кнопке Ⓥ.
 *
 * Вызывать напрямую вправе только [`recomputeUnread`] — это и есть инвариант
 * «единственный путь обновления бейджа». Второй писатель неизбежно разъедется
 * с первым, и снять расхождение можно будет только перезагрузкой страницы.
 *
 * @param {number} count
 * @returns {number} применённое значение
 */
function setUnreadBadge(count) {
    vkState.unreadTotal = Number.isFinite(count) && count > 0 ? count : 0;
    if ($vkUnreadBadge) {
        const n = vkState.unreadTotal;
        $vkUnreadBadge.textContent = n > 99 ? '99+' : String(n);
        $vkUnreadBadge.hidden = n <= 0;
    }
    return vkState.unreadTotal;
}

/**
 * Значение флага из кадра WS с откатом на локальное знание.
 *
 * Явный `typeof === 'boolean'` обязателен: у кадра СТАРОГО сервера поля нет
 * вовсе, и наивное `!frame.muted` превратило бы «не знаю» в «не заглушена» —
 * то есть вернуло бы тосты от заглушённых бесед.
 *
 * @param {*} value — поле кадра
 * @param {*} fallback — что знаем сами
 * @returns {boolean}
 */
export function frameFlag(value, fallback) {
    return typeof value === 'boolean' ? value : !!fallback;
}

/** Заглушена ли беседа по нашим собственным данным. */
function isMutedLocally(dialogId) {
    const id = String(dialogId);
    if (local.muted.has(id)) return true;
    const extra = local.flagsExtra.get(id);
    return !!(extra && extra.muted);
}

/**
 * Звенит ли беседа: решение СЕРВЕРА из кадра, локальное знание — фолбэк.
 *
 * `archived` у VK всегда `false` (архива нет), но поле читается ради
 * идентичности с телеграмным близнецом: удалив его, пришлось бы разветвлять
 * обработчик, который в остальном совпадает построчно.
 */
function dialogRings(frame, mutedFallback) {
    return !frameFlag(frame && frame.muted, mutedFallback)
        && !frameFlag(frame && frame.archived, false);
}

/**
 * Публикует контракт для формы настроек: применить новый белый список.
 *
 * Через `window.ForgeApp`, а не импортом: модалка настроек открывается и без
 * включённого плагина, и статический импорт панели в её граф был бы лишним.
 * Приём тот же, что у телеграмного близнеца (`app.telegramFilter`).
 *
 * `refresh` делает ТРИ вещи, и все три обязательны: сбрасывает кэш разбора
 * (иначе сохранённые настройки не подействуют до перезагрузки страницы),
 * перерисовывает колонку (список И счётчики фильтров) и пересчитывает бейдж.
 * Без последнего цифра на Ⓥ продолжала бы считать только что скрытые беседы.
 */
function publishFilterContract() {
    if (typeof window === 'undefined') return;
    const app = window.ForgeApp || (window.ForgeApp = {});
    app.vkFilter = {
        refresh: () => {
            invalidateVisibilityFilter();
            vkDialogs.renderAll();
            recomputeUnread();
        },
    };
}

/**
 * Проходит ли беседа белый список видимости.
 *
 * Скрытая беседа не имеет права ни звенеть тостом, ни копить непрочитанное:
 * цифра на Ⓥ вела бы в беседу, которой в колонке нет.
 *
 * Отдельно от [`dialogRings`] СОЗНАТЕЛЬНО: та функция чистая, решает только по
 * кадру и локальным флагам. Видимость же зависит от настроек пользователя.
 *
 * У VK предикат ТОЧЕН даже для беседы вне загруженного списка: тип
 * (`user`/`chat`/`community`) выводится из самого `peer_id`, а не ищется в
 * списке (подробности — в шапке `visibility.js`). У телеграмного близнеца
 * здесь стоит консервативное «не знаю папку ⇒ скрываем».
 */
function dialogShown(dialogId) {
    return dialogIdVisible(dialogId, currentVisibilityFilter(), vkState.dialogs);
}

/**
 * Пересчитывает бейдж непрочитанного.
 *
 * ЕДИНСТВЕННЫЙ путь обновления бейджа: и `new_message`, и `read`, и открытие
 * беседы сначала правят состояние, а потом зовут пересчёт. Два независимых
 * счётчика разъезжаются неизбежно.
 *
 * Источников числа два, и приоритет у серверного:
 *
 * 1. **кадр `unread_total`** (событие LongPoll 80) — VK считает по ВСЕМУ
 *    аккаунту, а не по загруженной странице. К нему добавляется поправка
 *    `unreadSinceServer` — беседы, в которых непрочитанное появилось уже
 *    после этого кадра (событие 80 приходит следом, отдельным кадром);
 * 2. **локальный расчёт** — до первого серверного счётчика: сумма по
 *    загруженной странице (без заглушённых) плюс `pendingUnread` для бесед,
 *    которых на странице нет вовсе.
 *
 * Беседа, появившаяся в списке, из производных карт вычищается здесь же:
 * список авторитетнее кадров, и без чистки одно и то же сообщение
 * посчиталось бы дважды.
 *
 * ТРЕТЬЯ ИЗ ЧЕТЫРЁХ ТОЧЕК БЕЛОГО СПИСКА — и здесь у него есть цена, которую
 * надо назвать вслух. При ДЕЙСТВУЮЩЕМ фильтре серверный счётчик игнорируется
 * целиком: VK считает его по ВСЕМУ аккаунту, включая беседы, которые
 * пользователь просил не показывать, а расщепить готовое число на видимую и
 * скрытую части нечем. Значит бейдж возвращается к сумме по загруженной
 * странице плюс накопленному — ровно как у телеграмного близнеца, у которого
 * серверного счётчика нет вовсе. Это честнее противоположного размена: цифра,
 * ведущая в беседу, которой в колонке нет, читается как поломка.
 *
 * @returns {number} применённое значение бейджа
 */
export function recomputeUnread() {
    local.muted = mutedIds(vkState.dialogs);
    for (const d of vkState.dialogs || []) {
        if (!d) continue;
        const id = String(d.id);
        local.flagsExtra.delete(id);
        local.pendingUnread.delete(id);
    }

    const filter = currentVisibilityFilter();
    let total;
    if (local.serverUnread != null && !filterActive(filter)) {
        total = local.serverUnread + local.unreadSinceServer.size;
    } else {
        total = unreadTotal(visibleDialogs(vkState.dialogs, filter), local.muted);
        for (const [id, n] of local.pendingUnread) {
            // Накопленное по беседе, которую только что скрыли настройками,
            // из бейджа обязано уйти немедленно: карта переживает сохранение
            // настроек, а `refresh` зовёт именно этот пересчёт.
            if (!dialogShown(id)) continue;
            total += Number(n) || 0;
        }
    }
    return setUnreadBadge(Math.max(0, total));
}

/**
 * Запоминает непрочитанное по беседе ВНЕ загруженной страницы.
 *
 * @returns {boolean} посчитали ли
 */
function noteUnknownUnread(dialogId, frame) {
    const id = String(dialogId);
    if (!id || vkDialogs.findDialog(id)) return false;
    if (!dialogRings(frame, isMutedLocally(id)) || !dialogShown(id)) {
        forgetPendingUnread(id);
        return false;
    }
    local.pendingUnread.set(id, (local.pendingUnread.get(id) || 0) + 1);
    return true;
}

/** Снимает накопленное непрочитанное по беседе (её прочитали/открыли). */
function forgetPendingUnread(dialogId) {
    const id = String(dialogId);
    local.pendingUnread.delete(id);
    local.unreadSinceServer.delete(id);
}

/** Переключает интерфейс на вкладку VK. */
function focusVkTab() {
    // Через клик по кнопке, а не switchTab(): tabs.js уже импортирует этот
    // модуль, и обратный импорт замкнул бы граф модулей в цикл.
    if ($vkToggle && typeof $vkToggle.click === 'function') $vkToggle.click();
}

/**
 * Тост о входящем сообщении.
 *
 * Гейты (все обязательны):
 *
 * - сообщение чужое — своё исходящее никогда не всплывает;
 * - беседа звенит: решение сервера из кадра, локальное знание — фолбэк;
 * - беседа проходит белый список (иначе тост звал бы туда, чего нет в колонке);
 * - НЕ активная беседа при открытой вкладке — пользователь и так на неё
 *   смотрит;
 * - по этой беседе не было тоста последние [`TOAST_THROTTLE_MS`] — иначе
 *   поток сообщений заливает экран.
 *
 * Заголовок с префиксом [`TOAST_PREFIX`]; канал — общий `echo/notifications`,
 * третьего стека уведомлений в проекте не заводится.
 *
 * @param {object} msg — MessageDto
 * @param {?object} [frame] — кадр `new_message` целиком (поля muted/archived)
 * @returns {?object} узел тоста, если он показан
 */
export function maybeToast(msg, frame) {
    if (!msg || msg.out) return null;
    const id = String(msg.dialog_id || '');
    if (!id) return null;
    if (!dialogRings(frame, isMutedLocally(id))) return null;
    if (!dialogShown(id)) return null;

    const onVkTab = !!$vk && !$vk.hidden;
    if (onVkTab && String(vkState.activeDialogId) === id) return null;

    const now = Date.now();
    if (!toastAllowed(local.lastToastAt.get(id) || 0, now, TOAST_THROTTLE_MS)) return null;
    local.lastToastAt.set(id, now);

    const dialog = vkDialogs.findDialog(id);
    const sender = msg.sender || {};
    const name = (dialog && dialog.title)
        || [sender.first_name, sender.last_name].filter(Boolean).join(' ').trim()
        || (sender.screen_name ? '@' + sender.screen_name : '')
        || id;
    const toast = notify({
        level: 'info',
        title: TOAST_PREFIX + truncate(name, 40),
        body: truncate(msg.text || '[media]', 80),
    });
    if (toast) {
        toast.style.cursor = 'pointer';
        toast.addEventListener('click', (e) => {
            // Клик по кресту закрывает тост — не перехватываем его.
            if (e.target && e.target.classList
                && e.target.classList.contains('echo-toast-close')) return;
            focusVkTab();
            openDialogById(id);
        });
    }
    return toast;
}

/**
 * Открывает беседу: подсветка в колонке + лента + сброс непрочитанного.
 *
 * Точка входа и для клика по списку, и для клика по тосту.
 *
 * @param {string} dialogId
 */
export function openDialogById(dialogId) {
    const id = String(dialogId);
    // Индикатор чужого набора относится к ПОКИНУТОЙ беседе.
    clearTypingFor(vkState.activeDialogId);
    // Курсор клавиатуры указывал на сообщение ПОКИНУТОЙ беседы: cmid
    // нумеруется в КАЖДОЙ беседе с единицы, поэтому в новой ленте тот же
    // номер принадлежит другому сообщению — и первое же j/k увело бы не туда.
    if (String(vkState.activeDialogId || '') !== id) resetKeynav();
    vkDialogs.selectDialog(id);
    const dialog = vkDialogs.findDialog(id);
    // Открытая беседа считается прочитанной: гасим счётчик локально сразу,
    // сервер подтвердит кадром `read`/`in` из POST /read.
    vkDialogs.setUnread(id, 0);
    forgetPendingUnread(id);
    recomputeUnread();
    void vkChat.openDialog(id, dialog);
    // Строка «печатает…» относится теперь к НОВОЙ ленте. Без перерисовки
    // здесь беседа, в которой собеседник печатает прямо сейчас, открывалась бы
    // с пустой строкой — до следующего кадра, то есть до нескольких секунд.
    renderTyping();
}

// ---------------------------------------------------------------------------
// «Печатает…»
// ---------------------------------------------------------------------------

/**
 * WS `typing` — собеседник набирает текст или пишет голосовое.
 *
 * У VK нет события «перестал печатать»: он просто перестаёт слать кадры.
 * Поэтому каждая запись живёт по таймеру [`TYPING_RESET_MS`] — без него
 * строка залипла бы навсегда после первого же события.
 *
 * @param {{dialog_id: string, user_id: string, action: string}} evt
 */
export function onTyping(evt) {
    if (!evt || !evt.dialog_id || !evt.user_id) return;
    const dialogId = String(evt.dialog_id);
    const key = `${dialogId}|${evt.user_id}`;
    const prev = local.typing.get(key);
    if (prev && prev.timer) clearTimeout(prev.timer);

    const timer = setTimeout(() => {
        local.typing.delete(key);
        renderTyping();
    }, TYPING_RESET_MS);
    local.typing.set(key, {
        dialogId,
        label: typingLabel(dialogId, evt.user_id),
        voice: evt.action === 'record_voice',
        timer,
    });
    renderTyping();
}

/**
 * Как подписать печатающего.
 *
 * В личной беседе `dialog_id === user_id`, поэтому имя собеседника — это
 * title беседы. Сверка id ОБЯЗАТЕЛЬНА: без неё любой участник группы получил
 * бы имя самой беседы, и строка читалась бы как «Беседа, Беседа are typing…».
 *
 * Для участника группы имени у нас может не быть — тянуть профиль ради
 * индикатора значит платить запросом за каждое нажатие клавиши собеседником
 * при бюджете 3 rps. Поэтому: своя беседа с этим человеком → её title, иначе
 * сам id.
 */
function typingLabel(dialogId, userId) {
    const uid = String(userId);
    const dialog = vkDialogs.findDialog(dialogId);
    if (dialog && dialog.kind === 'user' && dialog.title && String(dialog.id) === uid) {
        return String(dialog.title);
    }
    const known = vkDialogs.findDialog(uid);
    if (known && known.title) return String(known.title);
    return uid;
}

/** Перерисовывает `#vk-typing` по активной беседе. */
function renderTyping() {
    if (!$vkTyping) return '';
    const active = vkState.activeDialogId ? String(vkState.activeDialogId) : null;
    const names = [];
    let voice = false;
    for (const entry of local.typing.values()) {
        if (entry.dialogId !== active) continue;
        names.push(entry.label);
        if (entry.voice) voice = true;
    }
    // Видов активности у VK ровно два, и голосовое важнее: «печатает» на
    // самом деле означало бы, что собеседник вот-вот пришлёт текст.
    const line = typingLine(names);
    const text = line && voice ? line.replace('typing…', 'recording voice…') : line;
    $vkTyping.textContent = text;
    return text;
}

/** Гасит индикатор набора по беседе (или весь, если id не задан). */
function clearTypingFor(dialogId) {
    const id = dialogId == null ? null : String(dialogId);
    for (const [key, entry] of Array.from(local.typing.entries())) {
        if (id !== null && entry.dialogId !== id) continue;
        if (entry.timer) clearTimeout(entry.timer);
        local.typing.delete(key);
    }
    renderTyping();
}

/**
 * Ставит индикацию состояния связи: бейдж в статусбаре и цвет пульсирующей
 * точки в titlebar'е.
 *
 * Обе индикации ставятся ОДНОЙ функцией намеренно — раздельные вызовы рано
 * или поздно разъезжаются, и пользователь видит «OFFLINE» рядом с зелёной
 * точкой «связь есть».
 *
 * @param {string} mode — 'connected' | 'reconnecting' | 'offline'
 * @returns {string} применённый режим
 */
export function setMode(mode) {
    const m = CONN_MODES.includes(mode) ? mode : 'offline';
    if ($vkMode) {
        $vkMode.textContent = m.toUpperCase();
        $vkMode.className = 'vk-mode vk-mode-' + m;
    }
    if ($vkTitlebar) {
        for (const known of CONN_MODES) {
            $vkTitlebar.classList.toggle('vk-conn-' + known, known === m);
        }
    }
    // Связи нет — прошлое значение задержки становится враньём.
    if (m !== 'connected' && vkState.lat != null) {
        vkState.lat = null;
        if ($vkLat) $vkLat.textContent = '—';
    }
    return m;
}

/** Пишет имя авторизованного аккаунта в титлбар (или прячет ячейку). */
function renderSelfUser() {
    if (!$vkSelfUser) return;
    const u = vkState.selfUser;
    if (!u) {
        $vkSelfUser.textContent = '';
        if ($vkLogout) $vkLogout.hidden = true;
        return;
    }
    const full = [u.first_name, u.last_name].filter(Boolean).join(' ').trim();
    const name = full || (u.screen_name ? '@' + u.screen_name : String(u.id || ''));
    $vkSelfUser.textContent = name;
    if ($vkLogout) $vkLogout.hidden = false;
}

/**
 * Сбрасывает ВСЁ производное от аккаунта: счётчики, индикаторы, троттлинг.
 *
 * Одной функцией, а не россыпью вызовов на каждой площадке (logout, смена
 * аккаунта, отзыв токена): забытая карта — это цифра на Ⓥ от прошлого
 * аккаунта, снять которую можно только перезагрузкой страницы.
 */
function resetDerivedCounters() {
    local.serverUnread = null;
    local.unreadSinceServer.clear();
    local.pendingUnread.clear();
    local.flagsExtra.clear();
    local.lastToastAt.clear();
    local.readSent.clear();
    if (local.readTimer) {
        clearTimeout(local.readTimer);
        local.readTimer = null;
    }
    clearTypingFor(null);
    recomputeUnread();
}

/**
 * Применяет LoginStateDto ко всей панели.
 *
 * ОДНА точка на три источника (снапшот `/status`, ответ POST'а логина, кадр
 * WS `login_state`) — иначе состояния разъезжаются: оверлей показывает одно,
 * титлбар другое.
 *
 * @param {object} dto
 */
export function applyLoginState(dto) {
    if (!dto || typeof dto !== 'object') return;
    const wasAuthorized = vkState.loginState === 'authorized';
    renderLoginState(dto);
    renderSelfUser();
    if (wasAuthorized && dto.state !== 'authorized') {
        // Аккаунт отвалился (токен отозван, logout в соседней вкладке) —
        // данные прошлой сессии показывать нельзя.
        clearVkAccountData();
        resetDerivedCounters();
        local.dataLoaded = false;
        vkChat.resetChat();
        vkDialogs.resetDialogs();
    }
    if (dto.state === 'authorized' && local.initialized) void ensureData();
}

/**
 * Карта обработчиков кадров WS. Собирается один раз.
 *
 * Покрывает ВСЕ варианты `VkServerMsg` (plugins/vk/src/ws/protocol.rs) —
 * включая те, что начнут приходить в Ф7/Ф8: заглушки стоят намеренно, иначе
 * новый тип кадра сыпал бы «no handler» в консоль. Неизвестный тип разбирается
 * в ws.js (debug-лог, без исключения), а исключение самого обработчика ядро
 * ловит и не рвёт им соединение.
 */
export function buildHandlers() {
    if (local.handlers) return local.handlers;
    local.handlers = {
        // --- соединение ---
        open: () => {
            setMode('connected');
            // РЕКОННЕКТ (а не первое подключение): между обрывом и
            // восстановлением кадры не приходили, и открытая лента о них не
            // знает. Список бесед догоняется сам следующей загрузкой, а
            // историю открытой беседы догнать больше некому (forge-oy0c).
            if (local.everConnected) {
                void reloadData();
                void vkChat.refreshActive();
            }
            local.everConnected = true;
        },
        close: (info) => setMode(info && info.willReconnect ? 'reconnecting' : 'offline'),
        lat: (ms) => {
            if ($vkLat) $vkLat.textContent = `${ms}ms`;
        },
        // Клиент отстал (broadcast Lagged) или VK потерял историю (failed:3) —
        // перечитываем состояние по REST целиком.
        resync: () => {
            // Производные карты посчитаны по данным, которых больше нет.
            local.pendingUnread.clear();
            local.unreadSinceServer.clear();
            local.flagsExtra.clear();
            void refreshStatus();
            void reloadData();
            // Открытая лента — тоже состояние, и перечитывать её обязательно:
            // `reloadData` обновляет только КОЛОНКУ, а пропущенные кадры были
            // ровно про сообщения (forge-oy0c).
            void vkChat.refreshActive();
        },
        ping: () => {},

        // --- логин ---
        login_state: (msg) => applyLoginState(msg),

        // --- сообщения ---
        // Кадр приходит ЦЕЛИКОМ: рядом с MessageDto лежат `muted`/`archived` —
        // решение СЕРВЕРА о том, звенит ли беседа. Оно известно и для бесед,
        // которых фронт никогда не показывал, поэтому и тост, и бейдж
        // принимают решение по нему, а не по локальному Set.
        new_message: (frame) => {
            const dto = frame && frame.message;
            if (!dto) return;
            const id = String(dto.dialog_id || '');
            const isActive = String(vkState.activeDialogId) === id;
            rememberFrameFlags(id, frame);
            const drawn = vkChat.onNewMessage(dto);
            vkDialogs.onNewMessage(dto);
            if (isActive && drawn && !dto.out) {
                // Беседа открыта — ставим отметку о прочтении в очередь. Она
                // уйдёт ОДНИМ запросом на пачку и только при видимой вкладке.
                scheduleRead(id, dto.id);
            }
            if (!dto.out && !isActive) {
                noteUnknownUnread(id, frame);
                if (dialogRings(frame, isMutedLocally(id)) && dialogShown(id)) {
                    local.unreadSinceServer.add(id);
                }
            }
            recomputeUnread();
            maybeToast(dto, frame);
        },
        message_edited: (frame) => {
            if (frame && frame.message) vkChat.onEdited(frame.message);
        },
        message_deleted: (evt) => vkChat.onDeleted(evt),
        // НОВЫЙ кадр VK: у события 13 нет списка удалённых id, только граница,
        // поэтому `message_deleted` тут неприменим.
        history_cleared: (evt) => vkChat.onHistoryCleared(evt),

        // --- присутствие ---
        typing: (evt) => onTyping(evt),
        online: (evt) => {
            vkDialogs.onOnline(evt);
            // Открытая беседа показывает статус ещё и в шапке сессии.
            if (evt && String(evt.user_id) === String(vkState.activeDialogId)) {
                vkChat.refreshHead();
            }
        },
        read: (evt) => {
            // `in` — прочитали мы (возможно, с телефона): гасим счётчик и
            // запоминаем границу, чтобы не слать свою отметку следом.
            if (evt && evt.direction === 'in') noteReadFromServer(evt.dialog_id, evt.max_id);
            vkDialogs.onRead(evt);
            // `out` — прочитали нас; ленты это касается, счётчиков нет.
            vkChat.onRead(evt);
            recomputeUnread();
        },

        // --- счётчики и настройки ---
        // НОВЫЙ кадр VK: общий счётчик приходит готовым, и с этого момента
        // бейдж перестаёт быть суммой по загруженной странице списка.
        unread_total: (evt) => {
            const count = Number(evt && evt.count);
            local.serverUnread = Number.isFinite(count) ? Math.max(0, count) : null;
            // Всё, что мы насчитали сами, серверное значение уже учитывает.
            local.unreadSinceServer.clear();
            local.pendingUnread.clear();
            recomputeUnread();
        },
        // Глушение переключили с любого устройства. Глобальных категорийных
        // дефолтов у VK нет (кадра `notify_defaults_changed` тоже), поэтому
        // ветка ровно одна: точечный патч строки плюс пересчёт бейджа.
        notify_settings: (evt) => {
            const known = vkDialogs.onMuted(evt);
            if (evt && evt.dialog_id != null) {
                const id = String(evt.dialog_id);
                const muted = !!evt.muted;
                if (!known) local.flagsExtra.set(id, { muted });
                // Заглушённая беседа выпадает из бейджа целиком, включая уже
                // накопленное по ней непрочитанное.
                if (muted) forgetPendingUnread(id);
            }
            recomputeUnread();
        },
        // НОВЫЙ кадр VK (события 51/52): перечитывается СТРОКА списка, а не
        // вся панель — подробности и честное ограничение см. в dialogs.js.
        dialog_changed: (evt) => vkDialogs.onDialogChanged(evt),

        // --- анализ чата ---
        // Кадры разбора через Claude. Здесь только делегирование: попап
        // (js/vk/analysis.js) сам решает, его ли это run, — сервер вещает
        // события БЕЗ фильтрации по беседе (ws/mod.rs), и в открытый попап
        // одинаково прилетают кадры чужого разбора из соседней вкладки.
        analysis_started: (evt) => onAnalysisStarted(evt),
        analysis_delta: (evt) => onAnalysisDelta(evt),
        analysis_done: (evt) => onAnalysisDone(evt),
        analysis_error: (evt) => onAnalysisError(evt),

        // --- прогресс передачи файлов ---
        // Отправка (Ф8). ВАЖНО: у VK эти кадры приходят во время
        // `POST /messages`, а не во время `POST /upload` — конвейер
        // трёхшаговый и живёт внутри отправки сообщения. Обработчик обязан
        // слушать их и после нажатия «отправить».
        upload_progress: (evt) => vkUpload.onUploadProgress(evt),
        // Приём (Ф7): кадр несёт ИНДЕКС вложения — без него прогресс второго
        // фото сообщения прилетел бы в карточку первого.
        download_progress: (evt) => vkMedia.onDownloadProgress(evt),
    };
    return local.handlers;
}

/**
 * Запоминает решение сервера о беседе ВНЕ загруженной страницы.
 *
 * Для беседы, которая в списке есть, запись не нужна: там значение свежее и
 * авторитетнее, и recomputeUnread такую запись снимает.
 */
function rememberFrameFlags(dialogId, frame) {
    const id = String(dialogId || '');
    if (!id || vkDialogs.findDialog(id)) return;
    if (!frame || typeof frame.muted !== 'boolean') return;
    local.flagsExtra.set(id, { muted: frame.muted });
}

/**
 * Запоминает, что беседу прочитали ИЗВНЕ (другое устройство прислало
 * `read`/`in`).
 *
 * Без этого наша вкладка при первой же прокрутке отправила бы «свою» отметку
 * до того же id — лишний запрос, который ничего не меняет.
 */
function noteReadFromServer(dialogId, maxId) {
    const id = String(dialogId);
    const v = Math.max(0, Number(maxId) || 0);
    if (v > 0) local.readSent.set(id, Math.max(local.readSent.get(id) || 0, v));
    forgetPendingUnread(id);
}

/**
 * Перечитывает `GET /api/vk/status` и применяет его.
 *
 * @returns {Promise<?object>} StatusDto или null, если плагин выключен
 */
async function refreshStatus() {
    try {
        const status = await vkApi.getStatus();
        if (!status || status.enabled !== true) return null;
        setLoginHint(status.hint);
        applyLoginState(status.login || { state: 'not_configured' });
        return status;
    } catch (_) {
        // 404 — плагин выключен; сеть недоступна — панель и так покажет
        // OFFLINE по состоянию WS. В обоих случаях шуметь в консоль нечем.
        return null;
    }
}

/**
 * Гейт кнопки Ⓥ по состоянию плагина на сервере.
 *
 * Вызывается один раз из bootstrap. Ошибка/404 — штатный случай «плагин
 * выключен»: кнопка остаётся скрытой, ничего не логируем.
 */
export async function initVkGate() {
    if (local.gated) return;
    local.gated = true;
    try {
        const status = await vkApi.getStatus();
        if (!status || status.enabled !== true) {
            vkState.enabled = false;
            setToggleVisible(false);
            return;
        }
        vkState.enabled = true;
        setToggleVisible(true);
        // Бейдж и тосты работают из любой вкладки devforge — значит и белый
        // список обязан применяться до первого открытия панели.
        publishFilterContract();
        setLoginHint(status.hint);
        applyLoginState(status.login || { state: 'not_configured' });
        // Постоянный WS — с этого момента бейдж и тосты работают из любой
        // вкладки devforge, а не только при открытой панели.
        setMode('offline');
        connectVkWs(buildHandlers());
    } catch (_) {
        // 404 (плагин выключен) или недоступный сервер — кнопка скрыта.
        vkState.enabled = false;
        setToggleVisible(false);
    }
}

/** Отвязать аккаунт: сервер стирает токен локально, панель — свои данные. */
async function doLogout() {
    try {
        const dto = await vkApi.logout();
        closeCaptcha();
        resetLogin();
        resetVkState();
        resetDerivedCounters();
        local.dataLoaded = false;
        vkChat.resetChat();
        vkDialogs.resetDialogs();
        applyLoginState(dto || { state: 'not_configured' });
    } catch (e) {
        console.warn('[vk] logout failed', e);
    }
}

/**
 * Инициализация содержимого вкладки при первом открытии.
 *
 * Идемпотентна: tabs.js зовёт её на КАЖДОМ входе на вкладку, и повторная
 * навеска слушателей дала бы двойные обработчики (двойная отправка сообщения
 * — самый заметный из возможных симптомов).
 */
export function initVk() {
    if (local.initialized) return;
    local.initialized = true;
    // Контракт для формы настроек. Публикуется и здесь, и в гейте: вкладку
    // могли ни разу не открыть, а настройки — уже сохранить.
    publishFilterContract();
    initVkMobile();
    initCaptcha();
    initLogin({ onAuthorized: () => { void refreshStatus(); } });
    // Колонка НЕ импортирует ленту: выбор беседы уходит колбэком, и граф
    // модулей остаётся ациклическим — единственная точка сборки здесь.
    vkDialogs.initDialogs({
        onSelect: (id) => openDialogById(id),
    });
    vkChat.initChat({ onRead: scheduleRead, presenceOf: vkDialogs.presenceOf });
    // Попап анализа — ДО лайтбокса. Порядок между ОКНАМИ панели с forge-o0sdg
    // ничего не решает: Esc им раздаёт общий реестр `messenger/esc-stack.js` по
    // объявленной глубине слоя. Но лайтбокс слоем не стал и слушает Esc сам,
    // поэтому реестр обязан оказаться в очереди раньше него.
    initAnalysis({
        onNotice: (text) => notify({ level: 'info', title: 'VK', body: text, ttl: 2600 }),
    });
    if ($vkAnalysisToggle) {
        $vkAnalysisToggle.addEventListener('click', () => openAnalysis());
    }
    // Лайтбокс — ПОСЛЕ ленты: полоса ответа регистрирует свой слой Esc в
    // `initChat` (см. chat.js::bindReplyUi), а лайтбокс слоем не стал и слушает
    // Esc сам. При открытой картинке полоса объявляет себя закрытой и нажатие
    // достаётся лайтбоксу.
    vkMedia.initMedia();
    // Отправка файлов — ПОСЛЕ ленты: ей нужны и поле ввода (Ctrl+V), и полоса
    // ответа, которую снимает chat.cancelReply().
    vkUpload.initUpload();
    // Клавиатурная навигация по панели. Короткие ответы («Скопировано»,
    // «Непрочитанных бесед нет») показываем тем же тостом, что и остальной VK,
    // — заводить второй канал сообщений ради двух строк не за чем. Префикс
    // «VK» обязателен: тосты обеих панелей падают в один стек.
    initKeynav({
        onNotice: (text) => notify({ level: 'info', title: 'VK', body: text, ttl: 1800 }),
    });
    if ($vkLogout) $vkLogout.addEventListener('click', () => { void doLogout(); });
    // Панель могли открыть до того, как гейт получил ответ, — перечитываем.
    void refreshStatus();
    void ensureData();
}

/**
 * Страница ушла в фон.
 *
 * Соединение закрывается по СКРЫТИЮ СТРАНИЦЫ, но НЕ по смене вкладки
 * devforge: оно постоянное, потому что бейдж и тосты должны работать из любой
 * вкладки.
 */
export function suspendVk() {
    disconnectVkWs();
    setMode('offline');
}

/**
 * Страница вернулась из фона.
 *
 * Соединение на время фона РВЁТСЯ ([`suspendVk`]), поэтому возврат — это не
 * только «подключиться заново»: пока связи не было, кадры `new_message` шли в
 * никуда. Список бесед и открытую ленту надо ДОГНАТЬ по REST, иначе сообщение,
 * пришедшее в открытую беседу, не появится в ней до перехода в другую беседу и
 * обратно (forge-oy0c).
 *
 * Сам догон живёт в обработчике кадра `open`, а не здесь: соединение
 * поднимается и после обрыва связи, без всякого фона, и две точки догона
 * означали бы два одинаковых запроса на один возврат.
 */
export function resumeVk() {
    if (!vkState.enabled) return;
    connectVkWs(buildHandlers());
    void refreshStatus();
}

/**
 * Перечитывает список бесед принудительно (resync, реконнект WS).
 *
 * Отдельно от [`ensureData`]: та защищена флагом «уже грузили», а здесь нужно
 * именно обновить — состояние могло уехать, пока связи не было.
 */
export async function reloadData() {
    if (vkState.loginState !== 'authorized') return;
    local.dataLoaded = true;
    try {
        await vkDialogs.loadDialogs();
        // Свежая страница списка авторитетнее всего, что мы насчитали сами:
        // пересчёт снимает `pendingUnread`/`flagsExtra` по появившимся в ней
        // беседам (иначе одно и то же сообщение посчиталось бы дважды).
        recomputeUnread();
    } catch (_) {
        local.dataLoaded = false;
    }
}

/** beforeunload: снять слушателей и закрыть соединения. */
export function teardownVk() {
    disconnectVkWs();
    closeCaptcha();
    resetLogin();
    // objectURL'ы не переживают выгрузку страницы осмысленно, но и мусорить
    // ими в bfcache незачем: инвариант «каждый созданный URL отозван» держится
    // на ОДНОЙ точке выхода.
    vkMedia.teardownMedia();
    // Активные XHR обрываются: без abort() браузер продолжал бы гнать уже
    // никому не нужный файл.
    vkUpload.teardownUpload();
    // Слушатель клавиш в capture на document переживает выгрузку страницы в
    // bfcache и стрелял бы по мёртвому DOM.
    teardownKeynav();
    // Попап анализа держит свой слушатель Esc на document и кадр анимации для
    // склейки дельт — оба обязаны сняться здесь.
    teardownAnalysis();
    // Таймеры «печатает…» и дебаунса прочтения переживают выгрузку страницы в
    // bfcache и стреляют по мёртвому DOM.
    clearTypingFor(null);
    if (local.readTimer) {
        clearTimeout(local.readTimer);
        local.readTimer = null;
    }
}

/** Снимок состояния для e2e/диагностики. */
export function _debugState() {
    return {
        ..._debugVkState(),
        gated: local.gated,
        initialized: local.initialized,
        toggleHidden: $vkToggle ? $vkToggle.hidden : null,
        wsOpen: isVkWsOpen(),
        ws: _debugWsState(),
        dialogs: vkDialogs._debugDialogsState(),
        chat: vkChat._debugChatState(),
        media: vkMedia._debugMediaState(),
        upload: vkUpload._debugUploadState(),
        analysis: _debugAnalysisState(),
        // Слагаемые бейджа — по ним видно, врёт ли счётчик и с какой стороны.
        serverUnread: local.serverUnread,
        unreadSinceServer: local.unreadSinceServer.size,
        pendingUnread: local.pendingUnread.size,
        flagsExtra: local.flagsExtra.size,
        muted: local.muted.size,
        typing: local.typing.size,
        toastThrottled: local.lastToastAt.size,
    };
}
