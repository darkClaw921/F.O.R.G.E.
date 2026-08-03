// tmux-web — VK plugin: экран логина (оверлей #vk-login).
//
// Сильно проще телеграмного: не wizard из трёх шагов, а ОДИН экран. У VK нет
// ни кода из SMS, ни пароля 2FA — есть готовый access-токен, который
// пользователь добывает сам.
//
// ============================================================================
// АВТОМАТ ОТОБРАЖЕНИЯ (source of truth — поле `state` ответа сервера)
// ============================================================================
//
//   not_configured    → форма ввода, без ошибки
//   checking          → «проверяем…», форма заблокирована, ошибки НЕТ
//   disconnected      → форма + «нет связи с api.vk.com, повторите»
//   invalid_token     → форма + «токен неверен или отозван (VK 5)»
//   no_messages_scope → форма + «у токена нет права messages (VK 15)» + про scope
//   captcha_required  → форма + строка про captcha; картинку показывает
//                       ОТДЕЛЬНЫЙ оверлей #vk-captcha (js/vk/captcha.js)
//   authorized        → оверлей скрыт, поле ОЧИЩЕНО
//
// `renderLoginState(dto)` ИДЕМПОТЕНТНА: её зовут и из ответа
// `GET /api/vk/status`, и из WS-кадра `login_state`, и из ответа каждого
// POST'а логина. Повторный вызов с тем же dto не должен ни сбрасывать поле,
// ни перезапускать countdown, ни навешивать вторых слушателей.
//
// ============================================================================
// ДВА ПРАВИЛА, КОТОРЫЕ ЛЕГКО ПОТЕРЯТЬ
// ============================================================================
//
// 1. **Разбор вставленного значения живёт ТОЛЬКО на бэкенде.** Здесь нет ни
//    одного `split('#')`, ни одной проверки «похоже на токен». Второй парсер
//    был бы вторым местом, где можно ошибиться в обращении с секретом, и он
//    неизбежно разъехался бы с серверным.
// 2. **Секрет не задерживается в DOM.** После успеха значение `#vk-token`
//    очищается немедленно — иначе оно доступно любому расширению браузера и
//    попадает в скриншот devtools.
//
// Подсказка «где взять токен» приходит С БЭКЕНДА (`status.hint`): правила
// выдачи scope `messages` у VK меняются, а разметку браузер держит в кэше PWA
// и может не обновлять неделями. В index.html — только контейнер с безопасным
// дефолтом.
//
// Инвариант XSS: все тексты ставятся через textContent.

import {
    $vkLogin, $vkLoginStepToken, $vkLoginHint, $vkToken, $vkLoginTokenSubmit,
    $vkLoginStatus, $vkLoginError, $vkLoginCountdown,
} from '../core/dom.js';

import { loginToken, verifyLogin, submitCaptcha } from './api.js';
import { requestCaptcha } from './captcha.js';
import { vkState } from './state.js';

/**
 * Тексты состояний. Отдельной таблицей, а не строками в коде: их читает
 * человек, который вставил токен и получил отказ, и от точности формулировки
 * зависит, что он сделает дальше.
 *
 * `no_messages_scope` — самый важный: без объяснения про standalone-
 * приложения пользователь будет вставлять ОДИН И ТОТ ЖЕ рабочий токен по
 * кругу.
 */
export const STATE_TEXT = {
    disconnected: 'нет связи с api.vk.com — проверьте сеть и повторите',
    invalid_token: 'токен неверен, протух или отозван (VK 5) — вставьте новый',
    no_messages_scope: 'у токена нет права messages (VK 15). Токен рабочий, дело в правах: обычным standalone-приложениям VK это право не выдаёт, поэтому нужен токен, полученный под app_id клиента с поддержкой личных сообщений',
    captcha_required: 'VK просит подтвердить, что вы человек — введите символы с картинки',
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
    /** Уже показываем captcha для этого sid — второй раз не открываем. */
    captchaSid: null,
    /** Колбэк «логин завершён» (ставится из main.js). */
    onAuthorized: null,
};

/** Все поля ввода экрана. */
function inputs() {
    return [$vkToken].filter(Boolean);
}

/** Все кнопки экрана. */
function buttons() {
    return [$vkLoginTokenSubmit].filter(Boolean);
}

/**
 * Блокирует ввод на время запроса.
 *
 * Поле НЕ очищается: при отказе пользователь должен видеть то, что вставил, —
 * иначе повтор означает «сходить в браузер и скопировать адресную строку
 * заново».
 *
 * @param {boolean} busy
 */
function setBusy(busy) {
    local.busy = busy;
    for (const el of inputs()) el.disabled = busy;
    for (const el of buttons()) el.disabled = busy;
    if ($vkLoginStatus) {
        $vkLoginStatus.hidden = !busy;
        $vkLoginStatus.textContent = busy ? '>> connecting…' : '';
    }
    // Пока тикает countdown, кнопка остаётся заблокированной и после busy.
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
    if (!$vkLoginError) return;
    $vkLoginError.hidden = false;
    $vkLoginError.textContent = '>> ERR: ' + String(message || 'unknown error');
}

/** Прячет строку ошибки. */
function clearError() {
    if (!$vkLoginError) return;
    $vkLoginError.hidden = true;
    $vkLoginError.textContent = '';
}

/**
 * Форматирует остаток ожидания: `MM:SS`, а для многочасовых лимитов —
 * `HH:MM:SS`.
 *
 * У VK обратный отсчёт нужен трём кодам: 6 (слишком много запросов в
 * секунду), 9 (flood control, ~минута) и 29 (квота метода, час).
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
    if ($vkLoginCountdown) {
        $vkLoginCountdown.hidden = true;
        $vkLoginCountdown.textContent = '';
    }
    unlockIfIdle();
}

/**
 * Запускает живой обратный отсчёт.
 *
 * По истечении кнопка разблокируется, но запрос НЕ уходит сам: автоповтор
 * flood-лимитов только продлевает блокировку, поэтому повтор всегда
 * инициирует пользователь.
 *
 * @param {number} secs
 */
export function startCountdown(secs) {
    stopCountdown();
    const total = Math.max(0, Math.floor(Number(secs) || 0));
    if (total <= 0 || !$vkLoginCountdown) return;

    local.waitLeft = total;
    lockForWait();
    const render = () => {
        $vkLoginCountdown.hidden = false;
        $vkLoginCountdown.textContent = `>> VK RATE LIMIT: retry in ${formatWait(local.waitLeft)}`;
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
 * Ставит подсказку «где взять токен» из ответа сервера.
 *
 * Пустое значение НЕ затирает дефолт из разметки: старый сервер (или ответ из
 * кэша) не должен оставлять форму совсем без объяснений.
 *
 * @param {?string} hint
 */
export function setLoginHint(hint) {
    if (!$vkLoginHint) return;
    const text = typeof hint === 'string' ? hint.trim() : '';
    if (!text) return;
    $vkLoginHint.textContent = text;
}

/**
 * Применяет LoginStateDto к оверлею.
 *
 * ИДЕМПОТЕНТНА — см. шапку модуля.
 *
 * @param {{state: string, error: ?string, user: ?object,
 *   captcha_sid: ?string, captcha_img: ?string}} dto
 */
export function renderLoginState(dto) {
    if (!dto || typeof dto !== 'object') return;
    const state = dto.state || 'not_configured';
    const changed = state !== local.lastState;
    local.lastState = state;
    vkState.loginState = state;

    if (state === 'authorized') {
        stopCountdown();
        clearError();
        setBusy(false);
        local.captchaSid = null;
        vkState.selfUser = dto.user || null;
        if ($vkLogin) $vkLogin.hidden = true;
        // СЕКРЕТ НЕ ЗАДЕРЖИВАЕТСЯ В DOM.
        if ($vkToken) $vkToken.value = '';
        if (changed && typeof local.onAuthorized === 'function') {
            try {
                local.onAuthorized(dto);
            } catch (e) {
                console.warn('[vk-login] onAuthorized threw', e);
            }
        }
        return;
    }

    vkState.selfUser = null;
    if ($vkLogin) $vkLogin.hidden = false;
    if ($vkLoginStepToken) $vkLoginStepToken.hidden = false;

    if (state === 'checking') {
        // Промежуточное состояние: форма заблокирована, но ошибку НЕ рисуем —
        // «проверяем» и «не получилось» это разные сообщения, и показывать их
        // одновременно значит пугать пользователя на ровном месте.
        clearError();
        setBusy(true);
        return;
    }
    setBusy(false);

    // Ошибку показываем при СМЕНЕ состояния: повторный снапшот статуса иначе
    // затирал бы более свежий текст ошибки шага.
    if (changed) {
        const text = dto.error || STATE_TEXT[state] || '';
        if (text) showError(text);
        else clearError();
    }

    if (state === 'captcha_required' && dto.captcha_sid) {
        // Картинка живёт в ОТДЕЛЬНОМ оверлее: код 14 прилетает на любой метод,
        // а не только на логин.
        void handleCaptchaState(String(dto.captcha_sid));
    }
}

/**
 * Показывает captcha и, получив ответ, продолжает логин.
 *
 * Отмена (Esc / CANCEL) резолвит промис `null` — и мы ОБЯЗАНЫ сказать об этом
 * строкой, а не молчать: молчание после закрытия оверлея выглядит как
 * зависшая панель.
 *
 * @param {string} sid
 */
async function handleCaptchaState(sid) {
    // Один и тот же sid не открываем дважды: `login_state` приходит и
    // снапшотом, и кадром WS.
    if (local.captchaSid === sid) return;
    local.captchaSid = sid;

    const answer = await requestCaptcha({ sid });
    if (!answer) {
        showError('проверка отменена — нажмите CONNECT, чтобы повторить');
        local.captchaSid = null;
        return;
    }
    await submit(() => submitCaptcha(answer));
    local.captchaSid = null;
}

/**
 * Единая обработка ошибки запроса логина.
 *
 * Решения принимаются по КОДУ (контракт), а не по тексту:
 *   flood_wait       → обратный отсчёт, кнопка заблокирована, автоповтора нет;
 *   invalid_input    → мусор во вставленном значении;
 *   not_authorized   → токена нет (например, после logout в соседней вкладке);
 *   captcha_required → показать картинку и продолжить;
 *   not_connected    → нет связи;
 *   остальное        → текст сервера как есть.
 *
 * @param {Error & {code?: string, retryAfterSecs?: number, captcha?: object}} err
 */
function handleError(err) {
    const code = err && err.code;
    if (code === 'flood_wait') {
        const secs = Number(err.retryAfterSecs) || 0;
        showError(`VK просит подождать ${formatWait(secs)} — повтор только вручную`);
        startCountdown(secs);
        return;
    }
    if (code === 'captcha_required' && err.captcha && err.captcha.sid) {
        void handleCaptchaState(String(err.captcha.sid));
        showError(STATE_TEXT.captcha_required);
        return;
    }
    if (code === 'invalid_input') {
        showError((err && err.message) || 'во вставленном значении нет access_token');
        return;
    }
    if (code === 'not_authorized') {
        showError('токен не привязан — вставьте адресную строку целиком');
        return;
    }
    if (code === 'not_connected') {
        showError('нет связи с api.vk.com');
        return;
    }
    showError((err && err.message) || 'запрос не удался');
}

/**
 * Отправляет запрос логина и применяет результат.
 *
 * @param {() => Promise<object>} fn — вызов api.js, возвращающий LoginStateDto
 */
async function submit(fn) {
    if (local.busy) return;
    // Пока тикает обратный отсчёт, ни один запрос не уходит: это и есть
    // запрет на автоповтор flood-лимитов.
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
 * Поле пустое — это НЕ ошибка, а «повторить проверку тем, что уже сохранено»:
 * на экранах `disconnected` и `no_messages_scope` пользователю нечего
 * вставлять заново, он лишь просит попробовать ещё раз. Пустое поле при
 * `not_configured` — единственный случай, когда нужна подсказка.
 */
function submitToken() {
    const raw = ($vkToken && $vkToken.value) ? $vkToken.value : '';
    if (!raw.trim()) {
        if (local.lastState && local.lastState !== 'not_configured') {
            return submit(() => verifyLogin());
        }
        showError('вставьте адресную строку целиком (или сам access_token)');
        return undefined;
    }
    return submit(() => loginToken({ token: raw }));
}

/**
 * Навешивает обработчики экрана. Идемпотентна.
 *
 * @param {{onAuthorized?: (dto: object) => void}} [opts]
 */
export function initLogin(opts) {
    if (opts && typeof opts.onAuthorized === 'function') {
        local.onAuthorized = opts.onAuthorized;
    }
    if (local.bound) return;
    local.bound = true;

    if ($vkToken) {
        $vkToken.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter') return;
            e.preventDefault();
            submitToken();
        });
    }
    if ($vkLoginTokenSubmit) $vkLoginTokenSubmit.addEventListener('click', submitToken);
}

/**
 * Сброс экрана: гасит таймер и чистит секрет.
 *
 * Вызывается при teardown вкладки и при logout — таймер обязан умереть вместе
 * с оверлеем, иначе он тикал бы в фоне навсегда.
 */
export function resetLogin() {
    stopCountdown();
    clearError();
    setBusy(false);
    local.lastState = null;
    local.captchaSid = null;
    if ($vkToken) $vkToken.value = '';
}

/** Снимок внутреннего состояния для e2e/диагностики. */
export function _debugLogin() {
    return {
        bound: local.bound,
        busy: local.busy,
        waitLeft: local.waitLeft,
        timerActive: local.timerId !== null,
        lastState: local.lastState,
        captchaSid: local.captchaSid,
        tokenInDom: !!($vkToken && $vkToken.value),
        overlayHidden: $vkLogin ? !!$vkLogin.hidden : null,
        errorText: $vkLoginError ? $vkLoginError.textContent : null,
    };
}
