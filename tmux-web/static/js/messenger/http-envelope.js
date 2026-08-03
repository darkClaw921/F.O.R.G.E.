// tmux-web — общий слой мессенджеров: разбор конверта ошибок REST.
//
// Вынесено из js/telegram/api.js (функции call/jsonInit). Сам telegram/api.js
// при этом НЕ РЕДАКТИРУЕТСЯ и продолжает жить со своей копией: панель
// Telegram работает у пользователей прямо сейчас, и переводить её на общий
// слой ради красоты означало бы рисковать рабочей вкладкой. Общий слой
// импортирует только панель VK.
//
// Контракт ошибок обоих плагинов одинаков:
//
//   {"error":{"code":"not_authorized","message":"…","retry_after_secs":42}}
//
// плюс необязательные поля VK: `captcha_sid`, `captcha_img`, `redirect_uri`
// (см. plugins/vk/src/routes/error.rs). Форма — НАДМНОЖЕСТВО телеграмной,
// поэтому один разборщик обслуживает обе панели.
//
// ГЛАВНЫЙ ИНВАРИАНТ: решения принимаются по .code, а НЕ по тексту ошибки.
// Тексты меняются (в них подмешивается дословная формулировка VK), коды —
// контракт, зафиксированный таблицей в шапке routes/error.rs.
//
// Модуль ЧИСТЫЙ: ни DOM, ни импортов панелей. `createCaller` получает fetch
// снаружи, поэтому модуль грузится в Node как есть и покрывается тестом
// tmux-web/tests/frontend/messenger_http_envelope.test.mjs.

/**
 * Собирает Error из тела ответа.
 *
 * Возвращает именно Error (а не объект): вызывающий код бросает его дальше, и
 * стек с местом вызова обязан сохраниться.
 *
 * Поля результата:
 *   .status         — HTTP-статус;
 *   .code           — машинный код из конверта (null, если тело не наше);
 *   .retryAfterSecs — число секунд из `retry_after_secs` (null, если нет);
 *   .body           — разобранное тело целиком (для полей, которых здесь нет).
 *
 * Тело не-JSON (страница 502 от прокси, пустой ответ) — штатный случай: в
 * message уезжает текст, в .code — null, и вызывающий увидит «HTTP 502»
 * вместо исключения при разборе.
 *
 * @param {number} status
 * @param {*} body — разобранный JSON, строка или null
 * @returns {Error & {status: number, code: ?string, retryAfterSecs: ?number, body: *}}
 */
export function parseErrorEnvelope(status, body) {
    const envelope = (body && typeof body === 'object' && body.error && typeof body.error === 'object')
        ? body.error
        : null;
    const text = (envelope && typeof envelope.message === 'string' && envelope.message)
        || (typeof body === 'string' && body)
        || `HTTP ${status}`;

    const err = new Error(text);
    err.status = status;
    err.code = (envelope && typeof envelope.code === 'string' && envelope.code) ? envelope.code : null;
    err.retryAfterSecs = (envelope && typeof envelope.retry_after_secs === 'number')
        ? envelope.retry_after_secs
        : null;
    err.body = body;
    return err;
}

/**
 * Тело JSON-запроса с заголовками.
 *
 * Вынесено отдельно, потому что повторяется в каждом POST'е панели, а
 * забытый Content-Type даёт 415 без внятного объяснения.
 *
 * @param {string} method
 * @param {object} [payload]
 * @returns {RequestInit}
 */
export function jsonInit(method, payload) {
    return {
        method,
        headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
        body: JSON.stringify(payload || {}),
    };
}

/**
 * Строит функцию запроса поверх переданного fetch-совместимого вызова.
 *
 * fetch приходит АРГУМЕНТОМ, а не импортом core/api.js, по двум причинам:
 * модуль остаётся загружаемым в Node (core/auth.js на верхнем уровне трогает
 * localStorage и подменяет window.fetch), и тест может подсунуть свой
 * транспорт, не патча глобальные объекты.
 *
 * @param {(path: string, init?: RequestInit) => Promise<Response>} apiFetch
 * @returns {(path: string, init?: RequestInit) => Promise<*>} распарсенное тело
 */
export function createCaller(apiFetch) {
    if (typeof apiFetch !== 'function') {
        throw new TypeError('createCaller: apiFetch must be a function');
    }
    return async function call(path, init) {
        const res = await apiFetch(path, init);
        const ct = (res.headers && typeof res.headers.get === 'function')
            ? (res.headers.get('content-type') || '')
            : '';
        let body = null;
        try {
            body = ct.includes('application/json') ? await res.json() : await res.text();
        } catch (_) {
            // Пустое или битое тело — оставляем null: ниже соберём ошибку по
            // статусу, а на успешном ответе просто вернём null (204 No Content
            // это штатный ответ, а не повод бросать исключение).
        }
        if (!res.ok) throw parseErrorEnvelope(res.status, body);
        return body;
    };
}
