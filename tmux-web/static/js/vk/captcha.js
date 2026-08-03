// tmux-web — VK plugin: оверлей captcha (#vk-captcha). Аналога в Telegram нет.
//
// ============================================================================
// ПОЧЕМУ ОВЕРЛЕЙ ОТДЕЛЬНЫЙ, А НЕ ШАГ ЛОГИНА
// ============================================================================
//
// VK возвращает код 14 на ЛЮБОЙ метод, а не только на проверку токена: он
// может прилететь посреди отправки сообщения, при пометке прочитанным, при
// загрузке страницы истории. Шаг внутри #vk-login означал бы, что ради
// символов с картинки пользователю показывают экран входа, будто его
// разлогинило. Поэтому #vk-captcha живёт на своём слое (z-index 7) поверх
// всего, включая #vk-login.
//
// ============================================================================
// ЧЕТЫРЕ ПРАВИЛА (каждое — отдельный тест в vk_captcha.test.mjs)
// ============================================================================
//
// 1. **Картинка грузится ЧЕРЕЗ ПРОКСИ** `GET /api/vk/captcha/:sid`, а не по
//    прямому `captcha_img`. Прямой адрес отправил бы браузер пользователя на
//    серверы VK со всеми его cookie — то же правило, что для медиа.
// 2. **Повтор ровно ОДИН.** Второй `captcha_required` подряд показывает новую
//    картинку (чтобы состояние не осталось протухшим), но автоповтор НЕ
//    зацикливается: ошибка уходит вызывающему.
// 3. **Пока оверлей открыт, фоновые запросы заморожены** — иначе они
//    сгенерируют вторую captcha, и пользователь будет разгадывать их
//    бесконечно. Здесь заводится флаг `isCaptchaOpen()`; планировщик
//    js/vk/ratelimit.js читает его в Ф4.
// 4. **Отмена резолвит `null`.** Вызывающий ОБЯЗАН показать системную строку:
//    молчание после закрытия оверлея выглядит как зависшая панель.
//
// ЧЕСТНО О ПОКРЫТИИ: на пользовательском токене captcha возникает почти
// исключительно при массовых рассылках, то есть в разработке этот путь скорее
// всего ни разу не сработает вживую. Его покрывают стенд
// (plugins/vk/tests/vk_stub.rs), юнит-тест маппинга кода 14 и этот модуль с
// node-тестом — живого срабатывания мы не ждём и не изображаем, будто оно
// было.
//
// Инвариант XSS: все тексты ставятся через textContent, адрес картинки
// собирается из sid функцией api.js::captchaImageUrl.

import {
    $vkCaptcha, $vkCaptchaImg, $vkCaptchaKey, $vkCaptchaSubmit,
    $vkCaptchaCancel, $vkCaptchaError,
} from '../core/dom.js';

import { captchaImageUrl } from './api.js';
import { vkState } from './state.js';

const local = {
    /** Обработчики уже навешены. */
    bound: false,
    /** resolve текущего ожидания или null. */
    resolve: null,
    /** sid, который сейчас показан. */
    sid: null,
    /** Сколько раз оверлей открывался — для диагностики и тестов. */
    shown: 0,
};

/**
 * Открыт ли оверлей прямо сейчас.
 *
 * ЧИТАЕТСЯ ПЛАНИРОВЩИКОМ ЗАПРОСОВ (js/vk/ratelimit.js, Ф4): пока captcha на
 * экране, фоновые запросы не выпускаются. Иначе автообновление списка бесед
 * породило бы вторую captcha, пока пользователь разбирает первую.
 *
 * @returns {boolean}
 */
export function isCaptchaOpen() {
    return local.resolve !== null;
}

/** Показывает строку ошибки внутри оверлея. */
function showError(message) {
    if (!$vkCaptchaError) return;
    $vkCaptchaError.hidden = false;
    $vkCaptchaError.textContent = '>> ERR: ' + String(message || 'unknown error');
}

/** Прячет строку ошибки. */
function clearError() {
    if (!$vkCaptchaError) return;
    $vkCaptchaError.hidden = true;
    $vkCaptchaError.textContent = '';
}

/**
 * Завершает ожидание и прячет оверлей.
 *
 * Промис резолвится РОВНО ОДИН РАЗ: повторный вызов (Esc после клика по
 * CANCEL) не должен ни бросать, ни резолвить второй раз.
 *
 * @param {?{sid: string, key: string}} answer
 */
function finish(answer) {
    const resolve = local.resolve;
    local.resolve = null;
    local.sid = null;
    vkState.captcha = null;

    if ($vkCaptcha) $vkCaptcha.hidden = true;
    if ($vkCaptchaKey) $vkCaptchaKey.value = '';
    // Освобождаем картинку: одноразовая, и оставленный src тянул бы прошлую
    // задачу в следующий показ до загрузки новой.
    if ($vkCaptchaImg) $vkCaptchaImg.removeAttribute('src');
    clearError();

    if (typeof resolve === 'function') resolve(answer);
}

/** Отмена: Esc, CANCEL или клик по фону. */
function cancel() {
    if (!isCaptchaOpen()) return;
    finish(null);
}

/** Отправка введённых символов. */
function accept() {
    if (!isCaptchaOpen()) return;
    const key = ($vkCaptchaKey && $vkCaptchaKey.value ? $vkCaptchaKey.value : '').trim();
    if (!key) {
        showError('введите символы с картинки');
        return;
    }
    finish({ sid: local.sid, key });
}

/**
 * Навешивает обработчики оверлея. Идемпотентна.
 *
 * Слушатель Esc висит на document, а не на оверлее: фокус может быть где
 * угодно (в поле ввода сообщения, на кнопке), а Esc обязан работать всегда,
 * пока оверлей открыт.
 */
export function initCaptcha() {
    if (local.bound) return;
    local.bound = true;

    if ($vkCaptchaSubmit) $vkCaptchaSubmit.addEventListener('click', accept);
    if ($vkCaptchaCancel) $vkCaptchaCancel.addEventListener('click', cancel);
    if ($vkCaptchaKey) {
        $vkCaptchaKey.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter') return;
            e.preventDefault();
            accept();
        });
    }
    if ($vkCaptcha) {
        $vkCaptcha.addEventListener('click', (e) => {
            // Клик по подложке (а не по коробке) закрывает оверлей.
            if (e.target === $vkCaptcha) cancel();
        });
    }
    document.addEventListener('keydown', (e) => {
        if (!isCaptchaOpen()) return;
        if (e.key !== 'Escape') return;
        // Оверлей captcha выше лайтбокса и логина, поэтому Esc забирает
        // именно он и никому дальше не отдаёт.
        e.preventDefault();
        e.stopPropagation();
        cancel();
    });
}

/**
 * Показывает captcha и ждёт ответа пользователя.
 *
 * Принимает и ошибку из api.js (`err.captcha = {sid, img}`), и голый объект
 * `{sid}` — состояние логина приносит sid без ошибки.
 *
 * Повторный вызов, пока оверлей уже открыт с ТЕМ ЖЕ sid, возвращает тот же
 * промис ожидания: `login_state` приходит и снапшотом REST, и кадром WS, и
 * второй оверлей на ту же задачу был бы гонкой. Новый sid ЗАМЕНЯЕТ картинку,
 * а прошлое ожидание резолвится `null` — отвечать на протухшую задачу
 * бессмысленно.
 *
 * @param {{captcha?: {sid: string}, sid?: string}} source
 * @returns {Promise<?{sid: string, key: string}>} null ⇒ пользователь отменил
 */
export function requestCaptcha(source) {
    const sid = source && ((source.captcha && source.captcha.sid) || source.sid);
    if (!sid) return Promise.resolve(null);
    const nextSid = String(sid);

    if (isCaptchaOpen()) {
        if (local.sid === nextSid) return local.pending;
        // Пришла НОВАЯ задача: прошлую закрываем честной отменой.
        finish(null);
    }

    local.sid = nextSid;
    local.shown += 1;
    vkState.captcha = { sid: nextSid, pending: true };

    if ($vkCaptchaImg) {
        // ЧЕРЕЗ ПРОКСИ: прямой captcha_img из ошибки сюда не попадает никогда.
        $vkCaptchaImg.src = captchaImageUrl(nextSid);
    }
    if ($vkCaptchaKey) $vkCaptchaKey.value = '';
    clearError();
    if ($vkCaptcha) $vkCaptcha.hidden = false;
    if ($vkCaptchaKey && typeof $vkCaptchaKey.focus === 'function') $vkCaptchaKey.focus();

    local.pending = new Promise((resolve) => {
        local.resolve = resolve;
    });
    return local.pending;
}

/** Закрывает оверлей извне (logout, teardown вкладки). */
export function closeCaptcha() {
    cancel();
}

/**
 * Обёртка запроса: при коде `captcha_required` показать картинку, дождаться
 * ввода и повторить ОДИН раз.
 *
 * `fn` получает объект дополнительных полей и обязан подмешать их в запрос:
 *
 * ```js
 * await withCaptcha((extra) => vkApi.sendMessage(dialogId, { text, ...extra }));
 * ```
 *
 * Поведение по веткам:
 *   - первый вызов прошёл          → результат как есть;
 *   - captcha_required + ответ     → ровно ОДИН повтор с sid/key;
 *   - captcha_required + отмена    → бросается ИСХОДНАЯ ошибка (вызывающий
 *                                    покажет системную строку — молчать
 *                                    нельзя);
 *   - captcha_required дважды      → новая картинка показывается (состояние не
 *                                    остаётся протухшим), но автоповтор НЕ
 *                                    зацикливается: ошибка уходит наверх.
 *
 * @param {(extra: object) => Promise<*>} fn
 * @returns {Promise<*>}
 */
export async function withCaptcha(fn) {
    try {
        return await fn({});
    } catch (err) {
        if (!err || err.code !== 'captcha_required') throw err;
        const answer = await requestCaptcha(err);
        if (!answer) throw err;
        try {
            return await fn({ captchaSid: answer.sid, captchaKey: answer.key });
        } catch (again) {
            if (again && again.code === 'captcha_required') {
                // Показываем НОВУЮ задачу, но повторять сами больше не будем:
                // цикл «captcha → повтор → captcha» разгадывается бесконечно.
                void requestCaptcha(again);
            }
            throw again;
        }
    }
}

/** Снимок состояния для e2e/диагностики. */
export function _debugCaptcha() {
    return {
        bound: local.bound,
        open: isCaptchaOpen(),
        sid: local.sid,
        shown: local.shown,
        src: $vkCaptchaImg ? ($vkCaptchaImg.getAttribute('src') || null) : null,
        hidden: $vkCaptcha ? !!$vkCaptcha.hidden : null,
    };
}
