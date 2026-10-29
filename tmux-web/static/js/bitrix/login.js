// tmux-web — Bitrix24 plugin: экран подключения (оверлей #b24-login).
//
// Один шаг и одно поле: адрес входящего вебхука. Ни кода из SMS, ни пароля,
// ни captcha — у портала этого нет вовсе.
//
// ============================================================================
// АВТОМАТ ОТОБРАЖЕНИЯ (source of truth — поле `state` ответа сервера)
// ============================================================================
//
//   not_configured      → форма ввода, без ошибки
//   checking            → «проверяем…», форма заблокирована, ошибки НЕТ
//   invalid_url         → форма + «это не адрес входящего вебхука»
//   invalid_credentials → форма + «портал отверг вебхук (INVALID_CREDENTIALS)»
//   scope_missing       → форма + перечисление недостающих прав (im / task)
//   throttled           → форма + обратный отсчёт, кнопка заблокирована
//   disconnected        → форма + «нет связи с порталом, повторите»
//   authorized          → оверлей скрыт, ПОЛЕ ОЧИЩЕНО
//
// `renderLoginState(dto)` ИДЕМПОТЕНТНА: её зовут из ответа `GET
// /api/bitrix/status`, из WS-кадра `login_state` и из ответа каждого POST'а
// подключения. Повторный вызов с тем же dto не сбрасывает поле, не
// перезапускает обратный отсчёт и не навешивает вторых слушателей.
//
// ============================================================================
// ТРИ ПРАВИЛА, КОТОРЫЕ ЛЕГКО ПОТЕРЯТЬ
// ============================================================================
//
// 1. **Разбор вставленного адреса живёт ТОЛЬКО на бэкенде.** Здесь нет ни
//    одного `split('/rest/')`, ни одной проверки «похоже на вебхук». Второй
//    парсер был бы вторым местом, где можно ошибиться с секретом, и он
//    неизбежно разъехался бы с серверным.
// 2. **Секрет не задерживается в DOM и не попадает в сторадж.** После успеха
//    значение `#b24-webhook` очищается немедленно: иначе оно доступно любому
//    расширению браузера и попадает в скриншот devtools. Ни `localStorage`,
//    ни `sessionStorage` здесь не упоминаются вовсе.
// 3. **Автоповтора нет.** Пока тикает обратный отсчёт `throttled` (Р16), ни
//    один запрос не уходит: повтор продлевал бы троттлинг портала.
//
// Тексты подсказки и предупреждения про выход приходят С БЭКЕНДА (`hint` и
// `logout_hint` в `/api/bitrix/status`): интерфейс портала меняется, а
// разметку браузер держит в кэше PWA неделями. В index.html — только
// безопасные дефолты.
//
// ОГРАНИЧЕНИЕ Р15, о котором обязан сказать экран: вебхук выпускается от
// имени ОДНОГО сотрудника портала, и в remote-режиме все, кто открыл этот
// devforge, увидят его переписку. Текст приходит в `hint` и дублируется
// статическим блоком разметки на случай старого сервера.
//
// Инвариант XSS панели: все тексты ставятся через textContent.
//
// ============================================================================
// ДВА РЕЖИМА ОДНОГО ЭКРАНА (мультипортальность, Ф8)
// ============================================================================
//
//   'connect' — подключение АДРЕСУЕМОГО портала (`POST /login/webhook`).
//               Экран открыт всегда, пока состояние не `authorized`.
//   'add'     — добавление ЕЩЁ ОДНОГО портала (`POST /portals`), кнопка `+`
//               строки вкладок (js/bitrix/portals.js). Открывается явно
//               вызовом `openAddPortal()` и закрывается успехом или CANCEL.
//
// Второго экрана НЕ заводится, и это решение по существу: экран подключения —
// единственное место, где через фронт проходит СЕКРЕТ, и вся работа с ним
// (блокировка формы, обратный отсчёт, немедленная очистка поля) написана
// здесь один раз. Копия этой работы во втором экране разъехалась бы с
// оригиналом ровно в той детали, которую забудут перенести.
//
// В режиме 'add' `renderLoginState` НЕ ДЕЛАЕТ НИЧЕГО. Снимки `/status` и кадры
// `login_state` в этот момент описывают ПЕРВЫЙ портал (он подключён и
// здоров), и любой из них закрыл бы экран прямо под руками, стерев введённое.
//
// Неудачная проверка вебхука портал НЕ откатывает: `invalid_credentials`,
// `scope_missing`, `throttled` и `disconnected` — состояния уже подключённого
// портала, вкладка появляется и показывает, что не так. Поэтому успешный
// ответ `POST /portals` закрывает экран в ЛЮБОМ из них.

import {
    $b24Login, $b24LoginCancel, $b24LoginCountdown, $b24LoginError,
    $b24LoginHint, $b24LoginStatus, $b24LoginStepWebhook, $b24LoginTitle,
    $b24LoginWebhookSubmit, $b24Logout, $b24LogoutNote, $b24Webhook,
} from '../core/dom.js';

import * as b24Api from './api.js';

/**
 * Заголовок экрана по режиму.
 *
 * Экран ОДИН на оба сценария (см. шапку модуля): и первое подключение, и
 * добавление второго портала проносят через фронт один и тот же секрет, и
 * второй экран означал бы второе место, где его можно забыть очистить.
 */
export const SCREEN_TITLE = {
    connect: 'BITRIX24 // CONNECT',
    add: 'BITRIX24 // ADD PORTAL',
};

/**
 * Тексты состояний. Отдельной таблицей, а не строками в коде: их читает
 * человек, который вставил вебхук и получил отказ, и от точности
 * формулировки зависит, что он сделает дальше.
 *
 * Сервер почти всегда присылает свой `error` — он точнее (в нём перечислены
 * недостающие права). Эта таблица работает, когда текста нет.
 */
export const STATE_TEXT = {
    invalid_url: 'это не адрес входящего вебхука — нужен вид https://портал.bitrix24.ru/rest/1/код/',
    invalid_credentials: 'портал отверг вебхук: он неверен, отозван или принадлежит другому пользователю',
    scope_missing: 'у вебхука не отмечены нужные права — пересоздайте его с «Чат и уведомления» (im) и «Задачи» (task)',
    throttled: 'портал ограничивает частоту запросов — подождите и повторите',
    disconnected: 'нет связи с порталом — проверьте сеть и повторите',
};

const local = {
    /** Обработчики уже навешены. */
    bound: false,
    /** Запрос в полёте — поля и кнопка заблокированы. */
    busy: false,
    /** id таймера обратного отсчёта. */
    timerId: null,
    /** Сколько секунд осталось ждать (0 ⇒ ждать не нужно). */
    waitLeft: 0,
    /** Последнее применённое состояние сервера. */
    lastState: null,
    /** Колбэк «подключились» (ставится из main.js). */
    onAuthorized: null,
    /** Колбэк «отключились» (ставится из main.js). */
    onLoggedOut: null,
    /**
     * Режим экрана: 'connect' (подключение адресуемого портала) либо 'add'
     * (добавление ЕЩЁ ОДНОГО портала кнопкой `+` строки вкладок).
     *
     * Различие ровно в двух вещах: куда уезжает вебхук (`/login/webhook` либо
     * `POST /portals`) и что происходит после успеха. Всё остальное —
     * блокировка формы, обратный отсчёт, тексты ошибок, очистка секрета —
     * общее, и разводить это по двум экранам было бы удвоением работы с
     * секретом ради косметики.
     */
    mode: 'connect',
    /** Колбэк «портал добавлен»: (portalId) => void (ставится из main.js). */
    onPortalAdded: null,
    /** Колбэк «добавление отменено» (ставится из main.js). */
    onAddCancelled: null,
};

/** Все поля ввода экрана. */
function inputs() {
    return [$b24Webhook].filter(Boolean);
}

/** Все кнопки экрана. */
function buttons() {
    return [$b24LoginWebhookSubmit, $b24LoginCancel].filter(Boolean);
}

/**
 * Блокирует ввод на время запроса.
 *
 * Поле НЕ очищается: при отказе пользователь должен видеть то, что вставил, —
 * иначе повтор означает «сходить в портал и скопировать адрес заново».
 *
 * @param {boolean} busy
 */
function setBusy(busy) {
    local.busy = busy;
    for (const el of inputs()) el.disabled = busy;
    for (const el of buttons()) el.disabled = busy;
    if ($b24LoginStatus) {
        $b24LoginStatus.textContent = busy ? '>> connecting…' : '';
    }
    // Пока тикает обратный отсчёт, кнопка остаётся заблокированной и после
    // снятия busy.
    if (!busy && local.waitLeft > 0) lockForWait();
}

/** Блокирует кнопку, пока не истечёт ожидание. */
function lockForWait() {
    for (const el of buttons()) el.disabled = true;
}

/** Возвращает кнопке работоспособность, если ждать больше нечего. */
function unlockIfIdle() {
    if (local.busy || local.waitLeft > 0) return;
    for (const el of buttons()) el.disabled = false;
}

/**
 * Показывает ошибку строкой `>> ERR: …`.
 * @param {string} message
 */
function showError(message) {
    if (!$b24LoginError) return;
    $b24LoginError.hidden = false;
    $b24LoginError.textContent = '>> ERR: ' + String(message || 'unknown error');
}

/** Прячет строку ошибки. */
function clearError() {
    if (!$b24LoginError) return;
    $b24LoginError.hidden = true;
    $b24LoginError.textContent = '';
}

/**
 * Форматирует остаток ожидания: `MM:SS`, а для длинных пауз — `HH:MM:SS`.
 *
 * У портала два источника ожидания: интенсивность (`QUERY_LIMIT_EXCEEDED`,
 * около секунды) и `operating`-время метода (`OPERATION_TIME_LIMIT`, до
 * десяти минут).
 *
 * @param {number} secs
 * @returns {string}
 */
export function formatWait(secs) {
    const total = Math.max(0, Math.floor(Number(secs) || 0));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    const pad = (n) => String(n).padStart(2, '0');
    return h > 0 ? `${pad(h)}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

/** Останавливает обратный отсчёт и убирает его строку. */
export function stopCountdown() {
    if (local.timerId !== null) {
        clearInterval(local.timerId);
        local.timerId = null;
    }
    local.waitLeft = 0;
    if ($b24LoginCountdown) {
        $b24LoginCountdown.hidden = true;
        $b24LoginCountdown.textContent = '';
    }
    unlockIfIdle();
}

/**
 * Запускает живой обратный отсчёт.
 *
 * По истечении кнопка разблокируется, но запрос НЕ уходит сам: автоповтор
 * троттлинга только продлевает его, поэтому повтор всегда инициирует
 * пользователь.
 *
 * @param {number} secs
 */
export function startCountdown(secs) {
    stopCountdown();
    const total = Math.max(0, Math.floor(Number(secs) || 0));
    if (total <= 0 || !$b24LoginCountdown) return;

    local.waitLeft = total;
    lockForWait();
    const render = () => {
        $b24LoginCountdown.hidden = false;
        $b24LoginCountdown.textContent = `>> PORTAL RATE LIMIT: retry in ${formatWait(local.waitLeft)}`;
    };
    render();
    local.timerId = setInterval(() => {
        local.waitLeft -= 1;
        if (local.waitLeft <= 0) {
            stopCountdown();
            return;
        }
        render();
    }, 1000);
}

/**
 * Ставит подсказку «где взять вебхук» из ответа сервера.
 *
 * Пустое значение НЕ затирает дефолт из разметки: старый сервер (или ответ из
 * кэша PWA) не должен оставлять форму совсем без объяснений.
 *
 * @param {?string} hint
 */
export function setLoginHint(hint) {
    if (!$b24LoginHint) return;
    const text = typeof hint === 'string' ? hint.trim() : '';
    if (!text) return;
    $b24LoginHint.textContent = text;
}

/**
 * Ставит предупреждение про выход: на портале вебхук останется действующим.
 *
 * @param {?string} hint
 */
export function setLogoutHint(hint) {
    if (!$b24LogoutNote) return;
    const text = typeof hint === 'string' ? hint.trim() : '';
    if (!text) return;
    $b24LogoutNote.textContent = text;
}

/**
 * Применяет LoginStateDto к оверлею. ИДЕМПОТЕНТНА — см. шапку модуля.
 *
 * @param {{state: string, error: ?string, retry_after_secs: ?number,
 *   user: ?object}} dto
 */
export function renderLoginState(dto) {
    if (!dto || typeof dto !== 'object') return;
    // РЕЖИМ ДОБАВЛЕНИЯ ЗАЩИЩЁН от снимков состояния. Пока пользователь вводит
    // вебхук второго портала, `/status` и кадры `login_state` продолжают
    // описывать ПЕРВЫЙ (он подключён и здоров) — и, не будь этой строки,
    // ближайший из них закрыл бы экран прямо под руками, стерев введённое.
    if (local.mode === 'add') return;
    const state = typeof dto.state === 'string' ? dto.state : 'not_configured';
    const changed = state !== local.lastState;
    local.lastState = state;

    if (state === 'authorized') {
        stopCountdown();
        clearError();
        setBusy(false);
        if ($b24Login) $b24Login.hidden = true;
        if ($b24Logout) $b24Logout.hidden = false;
        // СЕКРЕТ НЕ ЗАДЕРЖИВАЕТСЯ В DOM.
        if ($b24Webhook) $b24Webhook.value = '';
        if (changed && typeof local.onAuthorized === 'function') {
            try {
                local.onAuthorized(dto);
            } catch (e) {
                console.warn('[b24-login] onAuthorized threw', e);
            }
        }
        return;
    }

    if ($b24Login) $b24Login.hidden = false;
    if ($b24LoginStepWebhook) $b24LoginStepWebhook.hidden = false;
    if ($b24Logout) $b24Logout.hidden = true;

    if (state === 'checking') {
        // Промежуточное состояние: форма заблокирована, но ошибку НЕ рисуем —
        // «проверяем» и «не получилось» это разные сообщения, и показывать их
        // одновременно значит пугать пользователя на ровном месте.
        clearError();
        setBusy(true);
        return;
    }
    setBusy(false);

    if (state === 'throttled') {
        // Обратный отсчёт перезапускается только при СМЕНЕ состояния: иначе
        // повторный снапшот `/status` каждый раз откатывал бы таймер назад.
        if (changed) startCountdown(Number(dto.retry_after_secs) || 0);
    } else {
        stopCountdown();
    }

    // Ошибку показываем при смене состояния: повторный снапшот иначе затирал
    // бы более свежий текст ошибки шага.
    if (changed) {
        const text = dto.error || STATE_TEXT[state] || '';
        if (text) showError(text);
        else clearError();
    }
}

/**
 * Единая обработка ошибки запроса подключения.
 *
 * Решения принимаются по КОДУ (контракт `plugins/bitrix/src/routes/error.rs`),
 * а не по тексту:
 *   invalid_input  → мусор во вставленном адресе;
 *   flood_wait     → обратный отсчёт, кнопка заблокирована, автоповтора нет;
 *   scope_missing  → у вебхука не отмечены права;
 *   not_authorized → портал отверг вебхук;
 *   not_connected  → нет связи;
 *   остальное      → текст сервера как есть.
 *
 * @param {Error & {code?: string, retryAfterSecs?: number}} err
 */
function handleError(err) {
    const code = err && err.code;
    if (code === 'flood_wait') {
        const secs = Number(err.retryAfterSecs) || 0;
        showError(`портал просит подождать ${formatWait(secs)} — повтор только вручную`);
        startCountdown(secs);
        return;
    }
    if (code === 'invalid_input') {
        showError((err && err.message) || STATE_TEXT.invalid_url);
        return;
    }
    if (code === 'scope_missing') {
        showError((err && err.message) || STATE_TEXT.scope_missing);
        return;
    }
    if (code === 'not_authorized') {
        showError((err && err.message) || STATE_TEXT.invalid_credentials);
        return;
    }
    if (code === 'not_connected') {
        showError(STATE_TEXT.disconnected);
        return;
    }
    showError((err && err.message) || 'запрос не удался');
}

/**
 * Отправляет запрос и применяет результат.
 *
 * @param {() => Promise<object>} fn — вызов api.js, возвращающий LoginStateDto
 */
async function submit(fn) {
    if (local.busy) return;
    // Пока тикает обратный отсчёт, ни один запрос не уходит: это и есть
    // запрет на автоповтор троттлинга.
    if (local.waitLeft > 0) return;
    clearError();
    setBusy(true);
    try {
        const dto = await fn();
        renderLoginState(dto);
    } catch (err) {
        handleError(err);
    } finally {
        setBusy(false);
    }
}

/**
 * Нажатие CONNECT.
 *
 * Пустое поле — не запрос, а подсказка: перепроверять «то, что уже
 * сохранено», у портала нечем — отдельного роута verify нет, а повторная
 * отправка того же вебхука это тот же самый POST, который пользователь и так
 * может сделать, вставив адрес.
 */
function submitWebhook() {
    const raw = ($b24Webhook && $b24Webhook.value) ? $b24Webhook.value : '';
    if (!raw.trim()) {
        showError('вставьте адрес входящего вебхука целиком');
        return undefined;
    }
    if (local.mode === 'add') return submitAddPortal(raw);
    return submit(() => b24Api.loginWebhook(raw));
}

/**
 * Нажатие CONNECT в режиме добавления портала.
 *
 * НЕУДАЧНАЯ ПРОВЕРКА ВЕБХУКА ПОРТАЛ НЕ ОТКАТЫВАЕТ. `invalid_credentials`,
 * `scope_missing`, `throttled` и `disconnected` — это СОСТОЯНИЯ уже
 * подключённого портала: вкладка появляется и показывает, что именно не так.
 * Поэтому успешный ответ здесь закрывает экран в любом из них, а не только в
 * `authorized`, — иначе вебхук пришлось бы вставлять второй раз, то есть
 * проносить секрет через фронт дважды.
 *
 * АВТОПОВТОРОВ НЕТ: пока тикает обратный отсчёт `throttled`, `submit` не
 * отправляет ничего вовсе.
 *
 * @param {string} raw — то, что вставил пользователь
 */
function submitAddPortal(raw) {
    return submit(async () => {
        const portal = await b24Api.addPortal(raw);
        const id = (portal && portal.id) ? String(portal.id) : '';
        // СЕКРЕТ НЕ ЗАДЕРЖИВАЕТСЯ В DOM — та же строка, что и в `authorized`.
        if ($b24Webhook) $b24Webhook.value = '';
        leaveAddMode();
        if (id && typeof local.onPortalAdded === 'function') {
            try {
                local.onPortalAdded(id);
            } catch (e) {
                console.warn('[b24-login] onPortalAdded threw', e);
            }
        }
        // `submit` применит это к оверлею уже в обычном режиме: у добавленного
        // портала состояние своё, и показать его обязан тот же автомат.
        return (portal && portal.login) || { state: 'not_configured' };
    });
}

/**
 * Открыть экран в режиме добавления ЕЩЁ ОДНОГО портала (кнопка `+`).
 *
 * Экран тот же самый — см. `SCREEN_TITLE` и шапку модуля.
 */
export function openAddPortal() {
    local.mode = 'add';
    local.lastState = null;
    stopCountdown();
    clearError();
    setBusy(false);
    if ($b24Webhook) $b24Webhook.value = '';
    if ($b24LoginTitle) $b24LoginTitle.textContent = SCREEN_TITLE.add;
    if ($b24LoginCancel) $b24LoginCancel.hidden = false;
    if ($b24Login) $b24Login.hidden = false;
    if ($b24LoginStepWebhook) $b24LoginStepWebhook.hidden = false;
    if ($b24Webhook && typeof $b24Webhook.focus === 'function') $b24Webhook.focus();
}

/** Вернуть экран в обычный режим (успех либо отмена). */
function leaveAddMode() {
    local.mode = 'connect';
    local.lastState = null;
    if ($b24LoginTitle) $b24LoginTitle.textContent = SCREEN_TITLE.connect;
    if ($b24LoginCancel) $b24LoginCancel.hidden = true;
    // Секрет не переживает выход из режима ни при каком исходе.
    if ($b24Webhook) $b24Webhook.value = '';
}

/**
 * Нажатие CANCEL — уйти из режима добавления, ничего не отправив.
 *
 * Оверлей после отмены закрывается только если текущий портал подключён:
 * иначе за ним пустая панель, и прятать экран было бы некуда возвращаться.
 */
export function cancelAddPortal() {
    if (local.mode !== 'add') return;
    leaveAddMode();
    clearError();
    if (typeof local.onAddCancelled === 'function') {
        try {
            local.onAddCancelled();
        } catch (e) {
            console.warn('[b24-login] onAddCancelled threw', e);
        }
    }
}

/**
 * Нажатие ⏻ — отвязать портал.
 *
 * Экспортируется, потому что кнопка живёт в титульной строке панели, а не в
 * оверлее: вешает её main.js вместе с остальными элементами шапки.
 */
export function submitLogout() {
    return submit(() => b24Api.logout()).then(() => {
        if (typeof local.onLoggedOut === 'function') {
            try {
                local.onLoggedOut();
            } catch (e) {
                console.warn('[b24-login] onLoggedOut threw', e);
            }
        }
    });
}

/**
 * Навешивает обработчики экрана. ИДЕМПОТЕНТНА: повторный вызов только
 * обновляет колбэки и не плодит слушателей.
 *
 * @param {{onAuthorized?: (dto: object) => void, onLoggedOut?: () => void}} [opts]
 */
export function initB24Login(opts) {
    if (opts && typeof opts.onAuthorized === 'function') {
        local.onAuthorized = opts.onAuthorized;
    }
    if (opts && typeof opts.onLoggedOut === 'function') {
        local.onLoggedOut = opts.onLoggedOut;
    }
    if (opts && typeof opts.onPortalAdded === 'function') {
        local.onPortalAdded = opts.onPortalAdded;
    }
    if (opts && typeof opts.onAddCancelled === 'function') {
        local.onAddCancelled = opts.onAddCancelled;
    }
    if (local.bound) return;
    local.bound = true;

    if ($b24Webhook) {
        $b24Webhook.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter') return;
            e.preventDefault();
            submitWebhook();
        });
    }
    if ($b24LoginWebhookSubmit) {
        $b24LoginWebhookSubmit.addEventListener('click', submitWebhook);
    }
    if ($b24LoginCancel) {
        $b24LoginCancel.addEventListener('click', cancelAddPortal);
    }
    if ($b24Logout) {
        $b24Logout.addEventListener('click', () => {
            void submitLogout();
        });
    }
}

/**
 * Сброс экрана: гасит таймер и чистит секрет.
 *
 * Вызывается при teardown вкладки — таймер обязан умереть вместе с оверлеем,
 * иначе он тикал бы в фоне навсегда.
 */
export function resetLogin() {
    stopCountdown();
    clearError();
    setBusy(false);
    // Режим добавления принадлежал ПРОШЛОЙ странице: переживи он её, первый же
    // снимок `/status` был бы проигнорирован, и панель осталась бы за экраном
    // подключения без единого объяснения.
    leaveAddMode();
    local.lastState = null;
    if ($b24Webhook) $b24Webhook.value = '';
}

/** Снимок внутреннего состояния для e2e/диагностики. */
export function _debugLogin() {
    return {
        bound: local.bound,
        busy: local.busy,
        mode: local.mode,
        waitLeft: local.waitLeft,
        timerActive: local.timerId !== null,
        lastState: local.lastState,
        // ФАКТ наличия, а не значение: снимок уезжает в e2e-логи.
        webhookInDom: !!($b24Webhook && $b24Webhook.value),
        overlayHidden: $b24Login ? !!$b24Login.hidden : null,
        errorText: $b24LoginError ? $b24LoginError.textContent : null,
    };
}
