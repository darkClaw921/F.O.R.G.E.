// tmux-web — Bitrix24 plugin: точка сборки вкладки.
//
// ДВА ВХОДА, РАЗВЕДЁННЫХ НАМЕРЕННО:
//
//   initB24Gate()  ← core/bootstrap.js, ОДИН РАЗ на загрузке страницы.
//                    Спрашивает GET /api/bitrix/status. 404 = плагин выключен,
//                    и это НЕ ошибка: кнопка Ⓑ остаётся скрытой, в консоль
//                    ничего не пишется (штатный путь для всех, кто запускает
//                    devforge без --bitrix). 200 ⇒ кнопка показывается и
//                    поднимается ПОСТОЯННЫЙ WS.
//   initB24()      ← js/tabs/tabs.js, на КАЖДОМ входе на вкладку.
//                    Идемпотентна (флаг local.inited): навешивает слушатели
//                    панели ровно один раз.
//
// ПОЧЕМУ WS ПОДНИМАЕТСЯ В ГЕЙТЕ, А НЕ ПРИ ОТКРЫТИИ ВКЛАДКИ. Бейдж
// непрочитанного на кнопке и тосты о новых сообщениях обязаны работать, пока
// пользователь сидит в terminal/tasks. Привязка сокета к вкладке означала бы,
// что уведомления появляются только после первого захода в панель.
//
// КАК ПАНЕЛЬ УЗНАЁТ, ЧТО ОНА АКТИВНА. Своего роутера у неё нет — источник
// истины `$b24.hidden`, которым управляет tabs.js. Панель читает его там, где
// решает «шуметь или нет»: в тостах и (с Ф6) в отметке о прочтении.
//
// ЕДИНСТВЕННЫЙ ПУТЬ ОБНОВЛЕНИЯ БЕЙДЖА — recomputeUnread(). Второй писатель в
// #b24-unread-badge разъезжается с первым неизбежно, поэтому setUnreadBadge
// приватна.

import { notify } from '../echo/notifications.js';
import {
    $b24, $b24AnalysisToggle, $b24Mode, $b24SelfUser, $b24Titlebar,
    $b24UnreadBadge, $bitrixToggle,
} from '../core/dom.js';
import {
    initAnalysis, onAnalysisDelta, onAnalysisDone, onAnalysisError,
    onAnalysisStarted, openAnalysis, teardownAnalysis, _debugAnalysisState,
} from './analysis.js';
import * as b24Api from './api.js';
import {
    appendMessage, applyLikes, currentDialogId, initChat, markDeleted,
    openDialog, resetChat, scheduleRead, _debugChatState,
} from './chat.js';
import {
    applyDialogsPatch, bumpDialog, initDialogs, loadDialogs, loadFilters, renderAll,
    renderDirs, resetDialogs, selectDialog, _debugDialogsState,
} from './dialogs.js';
import { findDialog, truncate, unreadTotal } from './format.js';
import {
    initB24Keynav, resetB24Keynav, teardownB24Keynav, _debugKeynavState,
} from './keynav.js';
import {
    dropMessageMedia, initMedia, onDownloadProgress, teardownMedia, _debugMediaState,
} from './media.js';
import {
    closeB24ContactsAfterSelect, initB24Mobile, _debugB24Mobile,
} from './mobile.js';
import {
    initB24Login, renderLoginState, resetLogin, setLoginHint, setLogoutHint,
    _debugLogin,
} from './login.js';
import {
    b24State, clearB24AccountData, currentVisibilityFilter,
    invalidateVisibilityFilter, _debugB24State,
} from './state.js';
import {
    applyTaskUpdate, initTasks, loadTasks, resetTasks, showTaskFor,
    _debugTasksState,
} from './tasks.js';
import {
    initUpload, onUploadProgress, teardownUpload, _debugUploadState,
} from './upload.js';
import { _debugB24Theme } from './theme.js';
import { dialogIdVisible, filterActive, visibleUnread } from './visibility.js';
import {
    connectB24Ws, disconnectB24Ws, isB24WsOpen, _debugWsState,
} from './ws.js';

/**
 * Путь гейта — реэкспорт из `js/bitrix/api.js`, где живёт вся таблица роутов.
 *
 * Оставлен именем модуля ради обратной совместимости: на него ссылались
 * фаза 1 и её тесты, а второе место с этой строкой — второй шанс разъехаться
 * с шапкой `plugins/bitrix/src/routes/mod.rs`.
 */
export const STATUS_PATH = b24Api.PATHS.status;

/**
 * Окно тишины между тостами ОДНОГО диалога, мс.
 *
 * Без него активный чат заливает экран стеком уведомлений: события приходят
 * пачками по несколько штук за цикл опроса.
 */
export const TOAST_THROTTLE_MS = 20000;

/** Префикс заголовка тоста — по нему видно, чья это панель. */
export const TOAST_PREFIX = 'Б24 · ';

const local = {
    /** Гейт уже отработал (повторный вызов — no-op). */
    gated: false,
    /** Слушатели панели навешены. */
    inited: false,
    /** Последний режим связи, показанный в статусбаре. */
    mode: 'offline',
    /**
     * Сколько раз сокет открывался.
     *
     * Отличает ПЕРВОЕ открытие (данные грузит гейт) от ПЕРЕподключения, после
     * которого состояние обязано быть перечитано целиком: пока сокет был мёртв,
     * события шли мимо и догнать их нечем.
     */
    wsOpens: 0,
    /** Когда по этому диалогу последний раз всплывал тост (ключ — диалог). */
    toastAt: new Map(),
};

/** Показать/скрыть кнопку вкладки. */
function setToggleVisible(visible) {
    if ($bitrixToggle) $bitrixToggle.hidden = !visible;
    if (!visible && $b24UnreadBadge) $b24UnreadBadge.hidden = true;
}

/**
 * Приватная запись в бейдж. Публичный путь ровно один — recomputeUnread().
 * @param {number} count
 */
function setUnreadBadge(count) {
    if (!$b24UnreadBadge) return;
    const n = Number.isFinite(count) ? Math.max(0, Math.trunc(count)) : 0;
    if (n <= 0) {
        $b24UnreadBadge.hidden = true;
        $b24UnreadBadge.textContent = '';
        return;
    }
    $b24UnreadBadge.hidden = false;
    // XSS-инвариант панели: только textContent, никакого innerHTML.
    $b24UnreadBadge.textContent = n > 99 ? '99+' : String(n);
}

/**
 * ЕДИНСТВЕННАЯ точка пересчёта бейджа непрочитанного.
 *
 * Источник — серверные счётчики (`counters`), а не сумма по загруженной
 * странице списка: у портала три независимых счётчика плюс уведомления, и
 * складывать их на клиенте по видимым строкам означало бы показывать
 * заниженное число до первой прокрутки.
 *
 * С Фазы 11 здесь же будет учитываться белый список чатов: при ДЕЙСТВУЮЩЕМ
 * фильтре серверный `total` игнорируется целиком — расщепить готовое число на
 * видимую и скрытую части нечем.
 */
export function recomputeUnread() {
    const filter = currentVisibilityFilter();
    if (filterActive(filter)) {
        // ТОЧКА 3 из перечня в visibility.js: при действующем белом списке
        // серверный `total` игнорируется ЦЕЛИКОМ. Портал отдаёт готовое число
        // по всему аккаунту, и расщепить его на видимую и скрытую части нечем
        // — считаем по загруженным видимым строкам, честно занижая счётчик
        // вместо того, чтобы показывать непрочитанное из скрытых чатов.
        setUnreadBadge(visibleUnread(b24State.dialogs, filter));
        return;
    }
    // Без фильтра источник — серверные счётчики: у портала их три плюс
    // уведомления, и сумма по загруженной странице была бы занижена до первой
    // прокрутки. Локальный расчёт остаётся запасным путём, пока счётчики не
    // приехали.
    const total = Number(b24State.counters.total) || 0;
    setUnreadBadge(total > 0 ? total : unreadTotal(b24State.dialogs));
}

/**
 * Контракт для формы настроек: применить изменения белого списка СРАЗУ.
 *
 * Публикуется как `window.ForgeApp.b24Filter` (Р1) — форма настроек не может
 * импортировать ES-модуль панели.
 */
export function publishFilterContract() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    if (!app) return;
    app.b24Filter = {
        refresh() {
            invalidateVisibilityFilter();
            renderAll();
            renderDirs();
            recomputeUnread();
        },
    };
}

/**
 * ОДНА функция ставит обе индикации связи: бейдж в статусбаре и класс на
 * titlebar. Две отдельные точки записи разъехались бы при первом же
 * реконнекте.
 *
 * @param {'connected'|'reconnecting'|'offline'|'throttled'} mode
 */
export function setMode(mode) {
    const known = ['connected', 'reconnecting', 'offline', 'throttled'];
    const next = known.includes(mode) ? mode : 'offline';
    local.mode = next;
    if ($b24Mode) {
        $b24Mode.textContent = next.toUpperCase();
        for (const m of known) $b24Mode.classList.toggle(`b24-mode-${m}`, m === next);
    }
    if ($b24Titlebar) {
        for (const m of known) $b24Titlebar.classList.toggle(`b24-conn-${m}`, m === next);
    }
}

/**
 * ОДНА точка на три источника состояния подключения: ответ `/status`, ответ
 * POST'ов подключения и кадр `login_state` из WS. Идемпотентна.
 *
 * Делится ровно надвое: здесь — состояние вкладки (данные аккаунта, бейдж,
 * метка связи), в `js/bitrix/login.js` — сам оверлей. Разделение не
 * косметическое: оверлей знает про блокировку формы и обратный отсчёт, а
 * панель — про кэш медиа и счётчики, и смешение этих забот в одной функции
 * означало бы, что при каждом снапшоте `/status` форма моргает.
 *
 * @param {*} dto — LoginStateDto (plugins/bitrix/src/login.rs)
 */
export function applyLoginState(dto) {
    const state = (dto && typeof dto.state === 'string') ? dto.state : 'not_configured';
    const wasAuthorized = b24State.loginState === 'authorized';
    b24State.loginState = state;
    b24State.throttled = state === 'throttled';

    if (state === 'authorized') {
        b24State.selfUser = (dto && dto.user) || null;
    } else if (wasAuthorized) {
        // Падение из authorized — данные аккаунта больше не наши.
        clearB24AccountData();
        resetDialogs();
        resetChat();
        // Карточка задачи принадлежала ПРОШЛОМУ вебхуку: у другого сотрудника
        // портала и права другие, и сама задача может быть невидима.
        resetTasks();
        recomputeUnread();
    }

    // Оверлей подключения закрывает панель во всех состояниях, кроме
    // authorized: работать с лентой без вебхука всё равно нельзя.
    renderLoginState(dto || { state });

    if ($b24SelfUser) {
        const u = b24State.selfUser;
        const name = u ? [u.name, u.last_name].filter(Boolean).join(' ').trim() : '';
        $b24SelfUser.textContent = state === 'authorized' ? name : '';
    }
    // Троттлинг портала (Р16) — своя метка в статусбаре: «медленно» и
    // «оборвалось» лечатся по-разному.
    if (state === 'throttled') setMode('throttled');
}

/**
 * Открыта ли сейчас вкладка Битрикс24.
 *
 * Своего роутера у панели нет — источник истины `$b24.hidden`, которым
 * управляет tabs.js.
 */
function onB24Tab() {
    return !!$b24 && !$b24.hidden;
}

/**
 * Всплывающее уведомление о входящем сообщении (Ф7).
 *
 * Гейты, все обязательны:
 *
 * - **сообщение чужое** — своё исходящее не всплывает никогда;
 * - **диалог не заглушён** — источник истины поле `muted` КАДРА, а не локальный
 *   список: сервер знает `chat.mute_list`, а во фронте загружена только
 *   страница списка, и заглушённый чат может быть за её пределами;
 * - **диалог проходит белый список** (Ф11) — иначе тост звал бы туда, чего нет
 *   в колонке;
 * - **это не открытая лента при открытой вкладке** — пользователь и так на неё
 *   смотрит;
 * - **окно тишины [`TOAST_THROTTLE_MS`]** на диалог — поток сообщений не должен
 *   заливать экран.
 *
 * Канал общий с Echo/Telegram/VK (`#echo-toasts`): четвёртого стека
 * уведомлений в проекте не заводится.
 *
 * @param {object} frame — кадр `message_new` целиком
 * @returns {?object} узел тоста, если он показан
 */
export function maybeToast(frame) {
    const msg = frame && frame.message;
    if (!msg || msg.out || msg.system) return null;
    const id = String(frame.dialog_id || msg.dialog_id || '');
    if (!id) return null;
    if (frame.muted) return null;
    if (!dialogIdVisible(id, currentVisibilityFilter(), b24State.dialogs)) return null;
    if (onB24Tab() && String(b24State.activeDialogId) === id) return null;

    const now = Date.now();
    const last = local.toastAt.get(id) || 0;
    if (now - last < TOAST_THROTTLE_MS) return null;
    local.toastAt.set(id, now);

    const dialog = findDialog(b24State.dialogs, id);
    const sender = msg.sender || {};
    const name = (dialog && dialog.title) || sender.name || `#${id}`;
    const toast = notify({
        level: 'info',
        title: `${TOAST_PREFIX}${truncate(name, 40)}`,
        body: truncate(msg.text || '[вложение]', 80),
    });
    if (toast && typeof toast.addEventListener === 'function') {
        toast.style.cursor = 'pointer';
        toast.addEventListener('click', (e) => {
            // Клик по кресту закрывает тост — не перехватываем его.
            const cls = (e && e.target && e.target.classList) || null;
            if (cls && cls.contains('echo-toast-close')) return;
            if ($bitrixToggle && typeof $bitrixToggle.click === 'function') $bitrixToggle.click();
            selectDialog(id);
        });
    }
    return toast;
}

/**
 * ПОЛНОЕ восстановление состояния панели.
 *
 * Зовётся по кадру `resync` (сервер: подписчик отстал — Р19 — либо курсор
 * событий протух — Р18) и после КАЖДОГО переподключения сокета.
 *
 * Именно полное, а не «догрузить хвост»: обрыв означает, что мы не знаем, что
 * именно пропустили. Пропущенным могло быть удаление или правка, а их «догрузка
 * снизу» не приносит по определению — она видит только НОВЫЕ сообщения.
 */
export function resyncPanel() {
    loadPanelData();
    const open = currentDialogId();
    if (open) openDialog(open, findDialog(b24State.dialogs, open));
}

/**
 * Карта обработчиков WS-кадров. Собирается ОДИН раз.
 *
 * Заглушки стоят даже для кадров, которых платформа не присылает НИКОГДА
 * (`typing`, `read`) и которые начнут приходить только в следующих фазах
 * (`task_update`, прогресс медиа): без них ws.js писал бы `no handler` в
 * консоль на каждое событие портала.
 */
export function buildHandlers() {
    const noop = () => {};
    return {
        // --- соединение ---
        open: () => {
            setMode('connected');
            local.wsOpens += 1;
            // ПЕРЕподключение (а не первое открытие) означает разрыв, за время
            // которого события прошли мимо: сокет закрыт — broadcast их не
            // догоняет. Поэтому состояние перечитывается целиком, а не
            // продолжается с места обрыва.
            if (local.wsOpens > 1) resyncPanel();
        },
        close: () => setMode(isB24WsOpen() ? 'connected' : 'reconnecting'),
        ping: noop,
        lat: noop,
        resync: () => resyncPanel(),

        // --- состояние подключения ---
        login_state: (msg) => applyLoginState(msg),

        // --- счётчики: единственный источник бейджа ---
        counters: (msg) => {
            b24State.counters = {
                total: Number(msg.total) || 0,
                chat: Number(msg.chat) || 0,
                dialog: Number(msg.dialog) || 0,
                tasks: Number(msg.tasks) || 0,
                notify: Number(msg.notify) || 0,
                // ВТОРОЕ семейство (Ф10): состояние самих задач. Его нельзя
                // складывать с `tasks` — то считает непрочитанные СООБЩЕНИЯ
                // чатов задач и обнуляется чтением ленты, а эти три числа
                // обнуляются действием над задачей.
                expired: Number(msg.expired) || 0,
                new_comments: Number(msg.new_comments) || 0,
                mentioned: Number(msg.mentioned) || 0,
            };
            recomputeUnread();
            renderDirs();
        },

        // --- лента и колонка ---
        dialogs_patch: (msg) => {
            if (!msg) return;
            applyDialogsPatch(msg.dialogs, msg.removed);
            recomputeUnread();
        },
        message_new: (msg) => {
            if (!msg || !msg.message) return;
            const id = String(msg.dialog_id);
            const open = onB24Tab() && String(b24State.activeDialogId) === id;
            // Строка вставляется в конец и прокручивает ленту, только если
            // пользователь стоял внизу (защёлка stick-to-bottom).
            if (String(currentDialogId()) === id) appendMessage(msg.message);

            // Непрочитанным считается ЧУЖОЕ сообщение в неоткрытой ленте:
            // своё непрочитанным не бывает, а открытую ленту читает пользователь
            // глазами — но отметку порталу шлёт ЯВНЫЙ вызов здесь (forge-uo7b),
            // а не сама лента: `appendMessage` не знает про видимость вкладки
            // Битрикс24 в devforge (`onB24Tab`), только про прокрутку. До этой
            // строки живое сообщение в открытом, но КОРОТКОМ диалоге (лента не
            // переполнена, `scrollTop` не меняется) не отмечалось прочитанным
            // никогда — в реальном браузере не рождалось события `scroll`,
            // единственного прежнего триггера `scheduleRead` (через `onScroll`).
            // Тот же путь, что у Telegram-плагина: `telegram/main.js::new_message`
            // → `scheduleRead` при активном диалоге.
            if (open && !msg.message.out) scheduleRead(id, msg.message.id);

            const counts = !msg.message.out && !open;
            if (!bumpDialog(msg.message, counts)) {
                // Строки нет в загруженной странице: диалог за её пределами
                // либо совсем новый. Список перечитываем — иначе чат остался
                // бы невидимым до перезагрузки вкладки.
                loadDialogs();
            }
            if (counts) {
                // Бейдж двигается СРАЗУ, не дожидаясь фолбэк-синхронизации
                // (она приедет через минуту и заменит оценку точным числом
                // портала).
                b24State.counters.total = (Number(b24State.counters.total) || 0) + 1;
            }
            recomputeUnread();
            maybeToast(msg);
        },
        message_update: (msg) => {
            if (!msg || !msg.message) return;
            if (String(currentDialogId()) !== String(msg.dialog_id)) return;
            // Сервер в этот же момент стирает свой файловый кэш вложений
            // (`recent_sync` зовёт `media_cache::invalidate`), а его ответ
            // помечен `immutable`. Не отпустить блоб здесь значит показывать
            // старое вложение поверх новой строки.
            dropMessageMedia(msg.dialog_id, msg.message.id);
            // Та же функция, что и у вставки: она заменяет строку по id и
            // перерисовывает ленту, когда правка коснулась показанной.
            appendMessage(msg.message);
        },
        message_delete: (msg) => {
            if (!msg) return;
            markDeleted(msg.dialog_id, msg.message_ids);
        },
        reaction: (msg) => {
            if (!msg) return;
            applyLikes(msg.dialog_id, msg.message_id, msg.likes);
        },
        presence: (msg) => {
            if (!msg) return;
            // Присутствие живёт в строке списка: у портала это свойство
            // СОБЕСЕДНИКА, а личный диалог адресуется его же идентификатором.
            const d = findDialog(b24State.dialogs, msg.user_id);
            if (!d) return;
            d.online = !!msg.online;
            renderAll();
        },

        // --- скачивание вложения (Ф8) ---
        //
        // Первая «нога» индикатора: сервер тянет файл из портала. Вторая —
        // чтение `response.body` в браузере — считается внутри media.js.
        download_progress: (msg) => onDownloadProgress(msg),

        // --- отправка файла (Ф9) ---
        //
        // Применяется только фаза `portal`: свой прогресс приёма вкладка знает
        // из XHR точнее и без сокета, а `upload_id` узнаёт лишь из ответа —
        // то есть когда фаза `client` уже кончилась. Кадры приёма нужны ВТОРОЙ
        // открытой вкладке, чтобы та видела занятую панель.
        upload_progress: (msg) => onUploadProgress(msg),

        // --- карточка задачи изменилась (Ф10) ---
        //
        // Кадр рассылается ПОСЛЕ действия — в том числе выполненного в другой
        // вкладке. Матрица прав в нём уже новая, поэтому шапка перерисовывается
        // целиком, а не правится по полям.
        task_update: (msg) => {
            if (!msg || !msg.task) return;
            applyTaskUpdate(msg.task);
        },

        // --- объявлены, но платформой не отправляются НИКОГДА (§6 плана) ---
        typing: noop,
        read: noop,

        // --- анализ чата через Claude (Ф12) ---
        //
        // Кадры вещаются БЕЗ фильтрации по чату — попап (js/bitrix/analysis.js)
        // сам решает, его ли это run, по паре (dialog_id, run_id), и сам копит
        // дельты до кадра анимации. Обработчики поэтому только делегируют и
        // DOM не трогают.
        analysis_started: (evt) => onAnalysisStarted(evt),
        analysis_delta: (evt) => onAnalysisDelta(evt),
        analysis_done: (evt) => onAnalysisDone(evt),
        analysis_error: (evt) => onAnalysisError(evt),

        // --- фоновая ошибка воркера ---
        error: (msg) => {
            if (msg && msg.code === 'flood_wait') {
                b24State.throttled = true;
                setMode('throttled');
            }
        },
    };
}

/**
 * Гейт кнопки Ⓑ по состоянию плагина на сервере.
 *
 * Вызывается один раз из bootstrap. Ошибка/404 — штатный случай «плагин
 * выключен»: кнопка остаётся скрытой, ничего не логируем.
 */
export async function initB24Gate() {
    if (local.gated) return;
    local.gated = true;
    try {
        const status = await b24Api.getStatus();
        if (!status || status.enabled !== true) {
            b24State.enabled = false;
            setToggleVisible(false);
            return;
        }
        b24State.enabled = true;
        setToggleVisible(true);
        publishFilterContract();
        setLoginHint(status.hint);
        setLogoutHint(status.logout_hint);
        // Потолок размера файла (Р7) приходит С СЕРВЕРА и живёт в состоянии:
        // своя копия числа во фронте разъехалась бы с `max_upload_bytes` в
        // bitrix.toml при первом же его изменении, а скрипты браузер держит в
        // кэше PWA неделями.
        b24State.maxUploadBytes = Number(status.max_upload_bytes) || 0;
        // Слушатели экрана подключения навешиваются ЗДЕСЬ, а не в initB24():
        // оверлей должен работать с первого открытия вкладки, а initB24()
        // зовётся только при входе на неё.
        initB24Login({
            onAuthorized: () => {
                setMode(isB24WsOpen() ? 'connected' : 'reconnecting');
                loadPanelData();
            },
            onLoggedOut: () => {
                clearB24AccountData();
                resetDialogs();
                resetChat();
                resetTasks();
                recomputeUnread();
            },
        });
        applyLoginState(status.login || { state: 'not_configured' });
        // Постоянный WS — с этого момента бейдж и тосты работают из любой
        // вкладки devforge, а не только при открытой панели.
        setMode('offline');
        connectB24Ws(buildHandlers());
    } catch (_) {
        // 404 (плагин выключен) или недоступный сервер — кнопка скрыта.
        b24State.enabled = false;
        setToggleVisible(false);
    }
}

/**
 * Инициализация панели — на каждом входе на вкладку, идемпотентна.
 *
 * Порядок навески слушателей станет значимым с Фазы 4 (колонка → лента →
 * медиа → загрузки → хоткеи); сейчас навешивать нечего, кроме гарантии, что
 * повторный вход не удваивает обработчики.
 */
export function initB24() {
    if (!b24State.enabled) return;
    // Колбэки ленты переустанавливаются на каждом входе (initChat
    // идемпотентен по слушателям, но не по колбэкам).
    initChat({
        onRead: () => {
            // Отметку о прочтении шлёт сама лента (окно 800 мс); сюда она
            // приходит уже ПОДТВЕРЖДЁННОЙ порталом. Обновляем то, чего лента
            // не знает: счётчики директорий и бейдж вкладки.
            loadFilters();
        },
    });
    if (local.inited) return;
    local.inited = true;
    // ПОРЯДОК НАВЕСКИ ЗНАЧИМ: анализ → мобилка → медиа → загрузки → задачи →
    // колонка → хоткеи.
    //
    // Попап анализа идёт ПЕРВЫМ: его слушатель Esc обязан зарегистрироваться
    // раньше слушателей колонки, лайтбокса и полосы ответа, иначе одно нажатие
    // закрывало бы то, что лежит ПОД попапом, а сам попап оставался бы на
    // экране. Чужой Esc он при этом не отнимает: пока попап закрыт,
    // обработчик выходит первой же строкой.
    initAnalysis({
        onNotice: (text) => showNotice(text),
    });
    // Кнопка ✦ в титлбаре. Навешивается здесь же, под флагом `inited`:
    // `teardownB24` его сбрасывает, и без общего гейта повторный вход навесил
    // бы второй слушатель, открывающий попап дважды за клик.
    if ($b24AnalysisToggle) {
        $b24AnalysisToggle.addEventListener('click', () => openAnalysis());
    }
    // Мобильная раскладка идёт следом: её слушатель Escape закрывает выехавшую
    // колонку, и он обязан оказаться в очереди РАНЬШЕ, чем Escape ленты и
    // лайтбокса, — иначе первый же Esc на узком экране снимал бы курсор
    // клавиатуры, оставляя колонку висеть поверх ленты.
    initB24Mobile();
    // Лайтбокс навешивается ОДИН раз на документ (Esc, стрелки, клик по фону) —
    // повторный вход на вкладку не должен удваивать обработчики, и `initMedia`
    // это гарантирует сам.
    initMedia();
    initUpload();
    // Карточка задачи (Ф10). Навешивается ДО колонки: выбор строки сразу же
    // зовёт `showTaskFor`, и слушатель кнопок к этому моменту обязан быть.
    initTasks({
        // После действия портал пишет в чат задачи системное сообщение о
        // смене статуса — лента обязана его показать. Ждать realtime нельзя:
        // событий задач вебхуку не приходит вовсе, а фолбэк-синхронизация
        // приедет через минуту.
        onReloadFeed: (dialogId) => {
            if (String(currentDialogId()) !== String(dialogId)) return;
            openDialog(dialogId, findDialog(b24State.dialogs, dialogId));
        },
        // Статус-точки и бейджи новых комментариев живут в СТРОКАХ колонки:
        // список диалогов такой информации не несёт, её источник — задачи.
        onTasksLoaded: () => renderAll(),
    });
    initDialogs({
        onSelect: (dialogId, dialog) => {
            b24State.activeDialogId = dialogId;
            openDialog(dialogId, dialog);
            // Шапка задачи ПРЯЧЕТСЯ для обычного чата — оставленная карточка
            // чужой задачи над чужой лентой опасна: кнопки в ней рабочие.
            showTaskFor(dialog);
            // Курсор клавиатуры адресовал строку ПРОШЛОЙ ленты: её узлов больше
            // нет, и наведение по сохранённому id попало бы в чужое сообщение.
            resetB24Keynav();
            // На узком экране колонка перекрывала ленту — ради неё её и
            // открывали, значит после выбора она обязана уехать обратно.
            closeB24ContactsAfterSelect();
        },
        // Директория `~/tasks` — единственное место, где статусы задач видны
        // списком, поэтому карточки тянутся при первом же входе в неё.
        onDir: (dir) => {
            if (dir === 'tasks') loadTasks();
        },
        // Счётчики портала приехали ответом `/filters` — бейдж вкладки обязан
        // показать их СРАЗУ, не дожидаясь кадра `counters` из WS. Писатель у
        // бейджа по-прежнему ровно один: `recomputeUnread`.
        onCounters: () => recomputeUnread(),
    });
    // Хоткеи — ПОСЛЕДНИМИ: их capture-слушатель обязан встать поверх уже
    // навешенных построчных обработчиков, а `owns()` — отвечать только когда
    // панели уже есть что показывать.
    initB24Keynav({ onNotice: (text) => showNotice(text) });
    loadPanelData();
}

/**
 * Короткое уведомление от клавиатурной навигации («Скопировано», «У этого
 * сообщения нет вложений»).
 *
 * Канал общий с тостами панели (`js/echo/notifications.js`), но БЕЗ префикса
 * «Б24 · »: это ответ на собственное нажатие пользователя, а не событие
 * портала, и подписывать его именем панели значило бы делать вид, что пришло
 * уведомление извне.
 *
 * @param {string} text
 */
function showNotice(text) {
    notify({ level: 'info', title: truncate(String(text || ''), 80), body: '' });
}

/**
 * Загружает данные панели: список диалогов и директории.
 *
 * Один вход на три источника (вход на вкладку, успешное подключение,
 * возвращение из фона): иначе каждый из них обзавёлся бы своей парой вызовов,
 * и они разъехались бы по составу.
 */
export function loadPanelData() {
    if (b24State.loginState !== 'authorized') return;
    loadDialogs();
    loadFilters();
    // Карточки задач тянем только когда они видны: страница `tasks.task.list`
    // стоит целого запроса из бюджета в 2 rps, а вне директории `~/tasks`
    // показать её негде.
    if (b24State.activeDir === 'tasks') loadTasks();
}

/**
 * Уход страницы в фон: сокет закрываем. Бейджа всё равно не видно, а сервер
 * по исчезновению подписчиков уводит опрос портала в idle-каденс (Р4) —
 * бюджет 2 rps не тратится впустую.
 */
export function suspendB24() {
    if (!b24State.enabled) return;
    disconnectB24Ws();
    setMode('offline');
}

/**
 * Возврат страницы из фона: поднимаем сокет обратно.
 *
 * Догонять состояние здесь НЕ НУЖНО и нельзя: пока вкладка была в фоне, сокет
 * был закрыт и события прошли мимо — ровно тот случай, который обрабатывает
 * `open`-обработчик (он видит ПЕРЕоткрытие и зовёт `resyncPanel`). Второй
 * вызов загрузки здесь означал бы два одинаковых запроса списка подряд, то
 * есть двойную трату бюджета портала на каждое переключение вкладки браузера.
 */
export function resumeB24() {
    if (!b24State.enabled) return;
    connectB24Ws(buildHandlers());
}

/** beforeunload: рвём соединение и отпускаем objectURL'ы медиа. */
export function teardownB24() {
    disconnectB24Ws();
    // Capture-слушатель клавиатуры стоит на document и панель переживает: не
    // снять его значит оставить обработчик, который на следующей странице
    // будет искать несуществующие узлы на каждое нажатие.
    teardownB24Keynav();
    // Попап анализа держит свой слушатель Esc на document и кадр анимации для
    // склейки дельт — оба обязаны сняться здесь, иначе первый переживёт
    // страницу в bfcache, а второй выстрелит в мёртвый DOM.
    teardownAnalysis();
    // ДО очистки состояния: `teardownMedia` отзывает objectURL'ы, читая карту
    // из `b24State`, а `clearB24AccountData` её обнуляет. Обратный порядок
    // оставил бы каждый скачанный блоб в памяти вкладки до перезагрузки.
    teardownMedia();
    // Очередь отправки держит и летящие XHR, и objectURL'ы миниатюр — её
    // тоже надо отпустить, иначе страница уходит с висящими запросами.
    teardownUpload();
    // Шапка задачи держит ссылку на активную задачу и рабочие кнопки: оставить
    // её после ухода страницы значит оставить кнопки, которые уже некуда
    // нажимать.
    resetTasks();
    clearB24AccountData();
    // Таймер обратного отсчёта и вставленный секрет обязаны умереть вместе с
    // панелью: иначе первый тикал бы в фоне навсегда, а второй остался бы в
    // DOM.
    resetLogin();
    local.inited = false;
    // Окна тишины и счётчик открытий принадлежали ПРОШЛОЙ странице: после
    // перезагрузки первое открытие сокета не должно считаться реконнектом и
    // дёргать лишний resync.
    local.toastAt.clear();
    local.wsOpens = 0;
}

/**
 * Агрегат отладочных снимков ВСЕХ модулей панели — контракт для e2e.
 *
 * ЗАЧЕМ ОН ЕСТЬ. Playwright обязан проверять состояние панели, не разбирая
 * вёрстку: тест, который ищет третий `li` в четвёртом `div`, ломается от любой
 * косметической правки и перестаёт что-либо значить. Поэтому каждый модуль
 * отдаёт свой `_debug*`, а здесь они складываются в одну картину, которой
 * хватает на все сценарии `e2e/tests/ui/bitrix-ui.spec.ts`.
 *
 * СЕКРЕТА ЗДЕСЬ НЕТ И БЫТЬ НЕ МОЖЕТ. Вебхук — это ВЕСЬ URL целиком
 * (`https://<портал>/rest/<user_id>/<code>/`), он живёт только в
 * `~/.config/forge/bitrix.toml` на сервере и во фронт не приезжает вовсе: даже
 * `login.js::_debugLogin` отдаёт лишь состояние автомата и длину поля ввода.
 * Сторож — `bitrix_keys.test.mjs`, который сериализует результат целиком и
 * ищет в нём признаки секрета.
 *
 * Поля верхнего уровня — состояние точки сборки; вложенные объекты именуются
 * по модулям, чтобы тест читался как `state.dialogs.rows`, а не как
 * `state.rows2`.
 */
export function _debugState() {
    return {
        ...(_debugB24State()),
        media: _debugMediaState(),
        mode: local.mode,
        gated: local.gated,
        inited: local.inited,
        wsOpens: local.wsOpens,
        toastThrottled: local.toastAt.size,
        ws: _debugWsState(),
        login: _debugLogin(),
        dialogs: _debugDialogsState(),
        chat: _debugChatState(),
        upload: _debugUploadState(),
        tasks: _debugTasksState(),
        // Ф11: обвязка UX. Три модуля, у каждого своё состояние, невыводимое
        // из остальных: раскладка (узкий экран / выехавшая колонка), тема
        // (режим и применённое значение) и клавиатура (зона, курсор, владение).
        mobile: _debugB24Mobile(),
        theme: _debugB24Theme(),
        keys: _debugKeynavState(),
        // Ф12: попап анализа. Своё состояние (открытый чат, идущий run, тред,
        // свёрнутость) невыводимо из остальных модулей — кадры анализа в DOM
        // панели не оставляют следов до самого конца run'а.
        analysis: _debugAnalysisState(),
        // Действующий белый список — в разобранном виде, тем же вызовом,
        // которым его читают колонка, бейдж и тосты. Тест сверяет ЧЕТЫРЕ точки
        // применения именно через него.
        filter: (() => {
            const f = currentVisibilityFilter();
            return {
                enabled: f.enabled,
                active: f.active,
                kinds: f.kinds.slice(),
                peers: f.peers.slice(),
            };
        })(),
    };
}
