// tmux-web — Telegram plugin: 3-этапный терминальный login-wizard.
//
// Шаги оверлея #tg-login:
//   1. creds    — api_id / api_hash / phone  → POST /api/telegram/login/start
//   2. code     — код из Telegram            → POST /api/telegram/login/code
//   3. password — пароль 2FA                 → POST /api/telegram/login/password
//
// ГЛАВНЫЙ ИНВАРИАНТ: видимый шаг определяется полем `state` ответа сервера
// (`not_configured | disconnected | code_sent | password_required |
// authorized`), а НЕ локальным счётчиком. Сервер — источник истины: он
// хранит токены логина, и только он знает, на каком шаге мы находимся.
// Единственное исключение — кнопка BACK, см. `local.forcedStep`.
//
// FLOOD_WAIT (риск №2 плана): при `code === 'flood_wait'` кнопка шага
// блокируется и тикает countdown из `retry_after_secs`. НИКАКИХ
// авто-ретраев ни на одном этапе логина — FLOOD_WAIT на логине банит
// аккаунт на часы, и каждая автоматическая попытка продлевает блокировку.
// Повторный запрос уходит ТОЛЬКО по явному клику пользователя после
// истечения countdown'а.
//
// Инвариант XSS: все сообщения ставятся через textContent.

import {
    $tgLogin, $tgLoginStepCreds, $tgLoginStepCode, $tgLoginStepPassword,
    $tgLoginError, $tgLoginCountdown, $tgLoginStatus,
    $tgApiId, $tgApiHash, $tgPhone, $tgLoginCredsSubmit,
    $tgCode, $tgLoginCodeSubmit, $tgLoginCodeBack,
    $tgPassword, $tgLoginPasswordSubmit, $tgLoginPasswordBack,
    $tgLoginPhone, $tgLoginHintText,
} from '../core/dom.js';

import { loginStart, loginCode, loginPassword } from './api.js';

/** Соответствие состояния сервера видимому шагу wizard'а. */
const STEP_BY_STATE = {
    not_configured: 'creds',
    disconnected: 'creds',
    code_sent: 'code',
    password_required: 'password',
};

const local = {
    /** Уже навесили обработчики. */
    bound: false,
    /** Запрос в полёте — поля и кнопки заблокированы. */
    busy: false,
    /** id таймера countdown'а FLOOD_WAIT. */
    timerId: null,
    /** Сколько секунд осталось ждать (0 ⇒ ждать не нужно). */
    waitLeft: 0,
    /** Шаг, выбранный пользователем кнопкой BACK (перебивает состояние). */
    forcedStep: null,
    /** Состояние, при котором был нажат BACK — чтобы понять, что оно сменилось. */
    forcedForState: null,
    /** Последнее применённое состояние сервера. */
    lastState: null,
    /** Колбэк «логин завершён» (ставится из main.js). */
    onAuthorized: null,
};

/** Все поля ввода wizard'а. */
function allInputs() {
    return [$tgApiId, $tgApiHash, $tgPhone, $tgCode, $tgPassword].filter(Boolean);
}

/** Все кнопки wizard'а. */
function allButtons() {
    return [
        $tgLoginCredsSubmit, $tgLoginCodeSubmit, $tgLoginCodeBack,
        $tgLoginPasswordSubmit, $tgLoginPasswordBack,
    ].filter(Boolean);
}

/**
 * Блокирует/разблокирует ввод на время запроса.
 *
 * Поля НЕ очищаются: при неверном коде/пароле пользователь должен видеть
 * то, что ввёл, и иметь возможность поправить одну цифру.
 *
 * @param {boolean} busy
 */
function setBusy(busy) {
    local.busy = busy;
    for (const el of allInputs()) el.disabled = busy;
    for (const el of allButtons()) el.disabled = busy;
    if ($tgLoginStatus) {
        $tgLoginStatus.hidden = !busy;
        $tgLoginStatus.textContent = busy ? '>> connecting…' : '';
    }
    // Пока тикает countdown, кнопки остаются заблокированными даже после
    // снятия busy.
    if (!busy && local.waitLeft > 0) lockForWait();
}

/** Блокирует кнопки шага, пока не истечёт FLOOD_WAIT. */
function lockForWait() {
    for (const el of allButtons()) el.disabled = true;
}

/**
 * Возвращает кнопкам работоспособность, если ждать больше нечего.
 *
 * Единственное место, снимающее блокировку: и истечение countdown'а, и его
 * досрочная остановка обязаны разблокировать кнопки, иначе wizard остаётся
 * мёртвым до перезагрузки страницы.
 */
function unlockButtonsIfIdle() {
    if (local.busy || local.waitLeft > 0) return;
    for (const el of allButtons()) el.disabled = false;
}

/**
 * Показывает ошибку строкой `>> ERR: …`.
 * @param {string} message
 */
function showError(message) {
    if (!$tgLoginError) return;
    $tgLoginError.hidden = false;
    $tgLoginError.textContent = '>> ERR: ' + String(message || 'unknown error');
}

/** Прячет строку ошибки. */
function clearError() {
    if (!$tgLoginError) return;
    $tgLoginError.hidden = true;
    $tgLoginError.textContent = '';
}

/**
 * Форматирует остаток ожидания: `MM:SS`, а для многочасовых блокировок —
 * `HH:MM:SS`.
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

/** Останавливает countdown и убирает его строку. */
export function stopCountdown() {
    if (local.timerId !== null) {
        clearInterval(local.timerId);
        local.timerId = null;
    }
    local.waitLeft = 0;
    if ($tgLoginCountdown) {
        $tgLoginCountdown.hidden = true;
        $tgLoginCountdown.textContent = '';
    }
    unlockButtonsIfIdle();
}

/**
 * Запускает живой countdown FLOOD_WAIT.
 *
 * По истечении кнопки разблокируются, но запрос НЕ уходит сам — ждём
 * клика пользователя (см. шапку модуля).
 *
 * @param {number} secs
 */
function startCountdown(secs) {
    stopCountdown();
    const total = Math.max(0, Math.floor(Number(secs) || 0));
    if (total <= 0 || !$tgLoginCountdown) return;

    local.waitLeft = total;
    lockForWait();
    const render = () => {
        $tgLoginCountdown.hidden = false;
        $tgLoginCountdown.textContent = `>> FLOOD_WAIT: retry in ${formatWait(local.waitLeft)}`;
    };
    render();
    local.timerId = setInterval(() => {
        local.waitLeft -= 1;
        if (local.waitLeft <= 0) {
            // stopCountdown сам разблокирует кнопки (но запрос НЕ отправляет —
            // повтор инициирует пользователь).
            stopCountdown();
            return;
        }
        render();
    }, 1000);
}

/**
 * Переключает видимый шаг.
 * @param {'creds'|'code'|'password'} step
 */
function showStep(step) {
    if ($tgLoginStepCreds) $tgLoginStepCreds.hidden = step !== 'creds';
    if ($tgLoginStepCode) $tgLoginStepCode.hidden = step !== 'code';
    if ($tgLoginStepPassword) $tgLoginStepPassword.hidden = step !== 'password';
}

/**
 * Применяет LoginStateDto к оверлею логина.
 *
 * Вызывается и из ответа `GET /api/telegram/status`, и из WS-события
 * `login_state` — поэтому обязана быть идемпотентной: повторный вызов с
 * тем же dto ничего не должен ломать (в частности, не сбрасывать поля и
 * не перезапускать countdown).
 *
 * @param {{state: string, phone: ?string, hint: ?string, error: ?string}} dto
 */
export function renderLoginState(dto) {
    if (!dto || typeof dto !== 'object') return;
    const state = dto.state || 'not_configured';
    const changed = state !== local.lastState;
    local.lastState = state;

    // BACK действует до ближайшей смены состояния на сервере.
    if (local.forcedStep && (changed || state !== local.forcedForState)) {
        local.forcedStep = null;
        local.forcedForState = null;
    }

    if (state === 'authorized') {
        stopCountdown();
        clearError();
        setBusy(false);
        if ($tgLogin) $tgLogin.hidden = true;
        // Секреты не задерживаются в DOM после успешного входа.
        if ($tgApiHash) $tgApiHash.value = '';
        if ($tgPassword) $tgPassword.value = '';
        if ($tgCode) $tgCode.value = '';
        return;
    }

    if ($tgLogin) $tgLogin.hidden = false;
    showStep(local.forcedStep || STEP_BY_STATE[state] || 'creds');

    if ($tgLoginPhone) $tgLoginPhone.textContent = dto.phone ? String(dto.phone) : '';
    if ($tgLoginHintText) {
        $tgLoginHintText.textContent = dto.hint ? `Подсказка: ${dto.hint}` : '';
    }

    // Ошибку из состояния показываем только при СМЕНЕ состояния: иначе
    // повторный снапшот статуса затирал бы более свежий текст ошибки шага.
    if (changed) {
        if (dto.error) showError(dto.error);
        else clearError();
    }
}

/**
 * Единая обработка ошибки шага логина.
 *
 * Решения принимаются по КОДУ (контракт), а не по тексту:
 *   flood_wait    → countdown, кнопки заблокированы, авто-ретрая нет;
 *   invalid_state → сессия логина устарела, возврат на шаг 1;
 *   not_connected → нет связи с Telegram;
 *   остальное     → текст сервера как есть (например неверный код).
 *
 * @param {Error & {code?: string, retryAfterSecs?: number}} err
 */
function handleStepError(err) {
    const code = err && err.code;
    if (code === 'flood_wait') {
        const secs = Number(err.retryAfterSecs) || 0;
        showError(`Telegram просит подождать ${formatWait(secs)} — повтор только вручную`);
        startCountdown(secs);
        return;
    }
    if (code === 'invalid_state') {
        showError('сессия логина устарела, начните заново');
        local.forcedStep = 'creds';
        local.forcedForState = local.lastState;
        showStep('creds');
        return;
    }
    if (code === 'not_connected') {
        showError('нет соединения с Telegram');
        return;
    }
    showError((err && err.message) || 'запрос не удался');
}

/**
 * Отправляет шаг wizard'а.
 *
 * @param {() => Promise<object>} fn — вызов api.js, возвращающий LoginStateDto
 */
async function submit(fn) {
    if (local.busy) return;
    // Пока тикает countdown, ни один шаг не уходит: это и есть запрет на
    // авто-ретрай FLOOD_WAIT.
    if (local.waitLeft > 0) return;
    clearError();
    setBusy(true);
    try {
        const dto = await fn();
        local.forcedStep = null;
        local.forcedForState = null;
        renderLoginState(dto);
        if (dto && dto.state === 'authorized' && typeof local.onAuthorized === 'function') {
            local.onAuthorized(dto);
        }
    } catch (err) {
        handleStepError(err);
    } finally {
        setBusy(false);
    }
}

/** Шаг 1: креды + телефон. */
function submitCreds() {
    const phone = ($tgPhone && $tgPhone.value ? $tgPhone.value : '').trim();
    if (!phone) {
        showError('укажите телефон в международном формате (+7…)');
        return;
    }
    const rawId = ($tgApiId && $tgApiId.value ? $tgApiId.value : '').trim();
    const rawHash = ($tgApiHash && $tgApiHash.value ? $tgApiHash.value : '').trim();
    const payload = { phone };
    if (rawId || rawHash) {
        const apiId = Number.parseInt(rawId, 10);
        if (!Number.isFinite(apiId) || apiId <= 0) {
            showError('api_id должен быть числом с my.telegram.org');
            return;
        }
        if (!rawHash) {
            showError('нужны оба значения: api_id и api_hash');
            return;
        }
        payload.api_id = apiId;
        payload.api_hash = rawHash;
    }
    return submit(() => loginStart(payload));
}

/** Шаг 2: код из Telegram. */
function submitCode() {
    const code = ($tgCode && $tgCode.value ? $tgCode.value : '').trim();
    if (!code) {
        showError('введите код из Telegram');
        return;
    }
    return submit(() => loginCode(code));
}

/** Шаг 3: пароль 2FA (пробелы могут быть частью пароля — не триммим). */
function submitPassword() {
    const password = $tgPassword && $tgPassword.value ? $tgPassword.value : '';
    if (!password) {
        showError('введите пароль двухфакторной защиты');
        return;
    }
    return submit(() => loginPassword(password));
}

/** BACK: локально вернуться на шаг ввода кредов/телефона. */
function goBackToCreds() {
    stopCountdown();
    clearError();
    local.forcedStep = 'creds';
    local.forcedForState = local.lastState;
    showStep('creds');
    if ($tgPhone) $tgPhone.focus();
}

/**
 * Навешивает обработчики wizard'а. Идемпотентна.
 *
 * @param {{onAuthorized?: (dto: object) => void}} [opts]
 */
export function initLoginWizard(opts) {
    if (opts && typeof opts.onAuthorized === 'function') {
        local.onAuthorized = opts.onAuthorized;
    }
    if (local.bound) return;
    local.bound = true;

    // Enter в любом поле шага = сабмит этого шага.
    const enter = (input, fn) => {
        if (!input) return;
        input.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter') return;
            e.preventDefault();
            fn();
        });
    };
    enter($tgApiId, submitCreds);
    enter($tgApiHash, submitCreds);
    enter($tgPhone, submitCreds);
    enter($tgCode, submitCode);
    enter($tgPassword, submitPassword);

    if ($tgLoginCredsSubmit) $tgLoginCredsSubmit.addEventListener('click', submitCreds);
    if ($tgLoginCodeSubmit) $tgLoginCodeSubmit.addEventListener('click', submitCode);
    if ($tgLoginPasswordSubmit) $tgLoginPasswordSubmit.addEventListener('click', submitPassword);
    if ($tgLoginCodeBack) $tgLoginCodeBack.addEventListener('click', goBackToCreds);
    if ($tgLoginPasswordBack) $tgLoginPasswordBack.addEventListener('click', goBackToCreds);
}

/**
 * Сброс wizard'а: гасит таймер и чистит секреты.
 *
 * Вызывается при teardown вкладки и при logout — таймер обязан умереть
 * вместе с оверлеем, иначе он тикал бы в фоне навсегда.
 */
export function resetLoginWizard() {
    stopCountdown();
    clearError();
    setBusy(false);
    local.forcedStep = null;
    local.forcedForState = null;
    local.lastState = null;
    if ($tgApiHash) $tgApiHash.value = '';
    if ($tgPassword) $tgPassword.value = '';
    if ($tgCode) $tgCode.value = '';
}

/** Снимок внутреннего состояния для e2e/отладки. */
export function _debugLogin() {
    return {
        bound: local.bound,
        busy: local.busy,
        waitLeft: local.waitLeft,
        timerActive: local.timerId !== null,
        forcedStep: local.forcedStep,
        lastState: local.lastState,
    };
}
