// tmux-web — VK plugin: REST-клиент.
//
// Тонкие обёртки поверх apiFetch (core/api.js) через общий разборщик конверта
// js/messenger/http-envelope.js. Authorization подставляет подменённый
// window.fetch из core/auth.js — здесь заголовков нет вовсе.
//
// ============================================================================
// ПУТИ — ЗАМОРОЖЕННЫЙ КОНТРАКТ
// ============================================================================
//
// Таблица ниже обязана совпадать ДОСЛОВНО с шапкой plugins/vk/src/routes/mod.rs:
// расхождение даёт молчаливый 404, а не ошибку сборки.
//
//   GET  /api/vk/status                              getStatus
//   POST /api/vk/login/token                         loginToken
//   POST /api/vk/login/verify                        verifyLogin
//   POST /api/vk/logout                              logout
//   POST /api/vk/captcha                             submitCaptcha
//   GET  /api/vk/captcha/:sid                        captchaImageUrl
//   GET  /api/vk/filters                             getFilters            (Ф4)
//   GET  /api/vk/dialogs                             getDialogs            (Ф4)
//   GET  /api/vk/dialogs/:id/messages                getMessages           (Ф4)
//   POST /api/vk/dialogs/:id/messages                sendMessage           (Ф4)
//   GET  /api/vk/dialogs/:id/messages/:mid           getMessage            (Ф4)
//   POST /api/vk/dialogs/:id/read                    markRead              (Ф4)
//   POST /api/vk/dialogs/:id/typing                  sendTyping            (Ф4)
//   POST /api/vk/dialogs/:id/messages/:mid/button    pressButton           (Ф9)
//   GET  /api/vk/media/:peer/:mid/:idx               mediaUrl / fetchMedia (Ф7)
//
// `dialogs`, а НЕ `conversations`: решение Ф1.1, см. js/vk/state.js.
//
// ============================================================================
// ПРАВИЛА, ПЕРЕНЕСЁННЫЕ ИЗ TELEGRAM ДОСЛОВНО
// ============================================================================
//
// 1. Решения принимаются по `err.code`, а не по тексту: тексты меняются (в них
//    подмешивается формулировка VK), коды — контракт.
// 2. У `loginToken` и `pressButton` автоповторов НЕТ ни при каких условиях.
// 3. В потребителях модуль импортируется как `import * as vkApi` — тогда
//    закэшированная старая версия даёт `undefined is not a function` в одном
//    месте, а не SyntaxError, роняющий весь модуль вместе с панелью.
//
// НОВОЕ ОТНОСИТЕЛЬНО TELEGRAM: каждый мутирующий вызов принимает
// необязательные `{captchaSid, captchaKey}` и подставляет их в тело как есть,
// а при коде `captcha_required` ошибка получает поле `err.captcha = {sid,img}`
// — его ловит js/vk/captcha.js и повторяет запрос ровно один раз.
//
// Инвариант «браузер на серверы VK не ходит»: в модуле нет ни одной ссылки на
// api.vk.com, vk.com или vk.ru. И картинка captcha, и медиа идут через прокси
// бэкенда.

import { apiFetch } from '../core/api.js';
import { createCaller, jsonInit } from '../messenger/http-envelope.js';

/**
 * Единственная точка выхода модуля в сеть.
 *
 * Ф4 обернёт её планировщиком js/vk/ratelimit.js (клиентский бюджет под 3 rps
 * сервера); менять сигнатуру для этого не придётся.
 */
const call = createCaller(apiFetch);

/**
 * Добавляет ответ на captcha в тело запроса.
 *
 * Имена полей — серверные (`captcha_sid`/`captcha_key`), а не camelCase:
 * это провод, и он совпадает с параметрами самого VK.
 *
 * @param {object} payload
 * @param {?{captchaSid?: string, captchaKey?: string}} opts
 * @returns {object} тот же payload
 */
function withCaptchaFields(payload, opts) {
    const sid = opts && opts.captchaSid;
    const key = opts && opts.captchaKey;
    // Половина ответа бесполезна: VK отвергнет её кодом 100, а пользователь
    // увидит «неверные параметры» вместо «введите символы ещё раз».
    if (sid && key) {
        payload.captcha_sid = String(sid);
        payload.captcha_key = String(key);
    }
    return payload;
}

/**
 * Обогащает ошибку полем `.captcha`, если сервер попросил символы с картинки.
 *
 * Сам URL картинки (`captcha_img`) в `<img src>` НЕ ставится — он ведёт на
 * серверы VK. Адрес для показа даёт `captchaImageUrl(sid)`.
 *
 * @param {Error & {code?: string, body?: object}} err
 * @returns {Error} тот же err
 */
function markCaptcha(err) {
    if (!err || err.code !== 'captcha_required') return err;
    const e = (err.body && err.body.error) || {};
    if (e.captcha_sid) {
        err.captcha = { sid: String(e.captcha_sid), img: e.captcha_img || null };
    }
    return err;
}

/** Обёртка вызова: ошибки проходят через markCaptcha. */
async function callC(path, init) {
    try {
        return await call(path, init);
    } catch (err) {
        throw markCaptcha(err);
    }
}

/** Кодирует сегмент пути (peer_id бывает отрицательным, id — строки). */
function seg(value) {
    return encodeURIComponent(String(value == null ? '' : value));
}

// ---------------------------------------------------------------------------
// Состояние плагина и логин.
// ---------------------------------------------------------------------------

/**
 * GET /api/vk/status.
 *
 * Единственный роут, отвечающий 200 в любом состоянии логина. **404 означает
 * «плагин выключен»** (роуты не зарегистрированы, devforge запущен без
 * `--vk`) — это НЕ ошибка, и обрабатывать её обязан вызывающий: по ней гейт
 * прячет кнопку Ⓥ.
 *
 * @returns {Promise<{enabled: boolean, login: object, api_version: string,
 *   rate_limit_rps: number, hint: string}>}
 */
export async function getStatus() {
    return call('/api/vk/status');
}

/**
 * POST /api/vk/login/token — отдать серверу то, что вставил пользователь.
 *
 * Принимается АДРЕСНАЯ СТРОКА ЦЕЛИКОМ, её фрагмент или сам токен: разбор
 * живёт ТОЛЬКО на бэкенде (plugins/vk/src/login.rs::parse_token_input).
 * Второй парсер здесь был бы вторым местом, где можно ошибиться в обращении с
 * секретом, поэтому фронт не смотрит внутрь значения вообще — даже чтобы
 * «подсказать формат».
 *
 * АВТОПОВТОРОВ НЕТ. Мусор — это `400 invalid_input`, и повторять его
 * бессмысленно; всё остальное пользователь инициирует кнопкой сам.
 *
 * @param {{token: string}} payload
 * @returns {Promise<object>} LoginStateDto
 */
export async function loginToken(payload) {
    const raw = (payload && payload.token) || '';
    return call('/api/vk/login/token', jsonInit('POST', { access_token: raw }));
}

/**
 * POST /api/vk/login/verify — перепроверить уже сохранённый токен.
 *
 * Нужен кнопке «повторить» на экране `disconnected` и ручной проверке после
 * того, как пользователь поправил права токена на стороне VK.
 *
 * @param {{captchaSid?: string, captchaKey?: string}} [opts]
 * @returns {Promise<object>} LoginStateDto
 */
export async function verifyLogin(opts) {
    return callC('/api/vk/login/verify', jsonInit('POST', withCaptchaFields({}, opts)));
}

/**
 * POST /api/vk/logout — отвязать аккаунт.
 *
 * Сервер стирает токен ЛОКАЛЬНО и в VK не ходит: у пользовательского токена
 * нет метода «отозвать себя». Успешен из любого состояния.
 *
 * @returns {Promise<object>} LoginStateDto (`not_configured`)
 */
export async function logout() {
    return call('/api/vk/logout', jsonInit('POST', {}));
}

/**
 * POST /api/vk/captcha — отдать символы с картинки.
 *
 * Сервер перепроверяет токен с ответом и при успехе будит realtime-воркера,
 * поэтому отдельного «переподключиться» после captcha не нужно.
 *
 * @param {{sid: string, key: string}} payload
 * @returns {Promise<object>} LoginStateDto
 */
export async function submitCaptcha(payload) {
    const sid = (payload && payload.sid) || '';
    const key = (payload && payload.key) || '';
    return call('/api/vk/captcha', jsonInit('POST', { sid: String(sid), key: String(key) }));
}

/**
 * Адрес картинки captcha — ВСЕГДА через прокси бэкенда.
 *
 * Прямой `captcha_img` из ошибки в `<img src>` не ставится никогда: это
 * отправило бы браузер пользователя на серверы VK со всеми его cookie. То же
 * правило, что для медиа.
 *
 * @param {string} sid
 * @returns {string}
 */
export function captchaImageUrl(sid) {
    return `/api/vk/captcha/${seg(sid)}`;
}

// ---------------------------------------------------------------------------
// Данные (роуты приезжают в Ф4/Ф7/Ф9 — обёртки заведены сразу, чтобы
// потребители не правились в чужой фазе).
// ---------------------------------------------------------------------------

/**
 * GET /api/vk/filters — четыре серверных фильтра бесед.
 *
 * Заменяют телеграмные папки: у VK это параметр метода
 * `messages.getConversations`, а не пользовательская структура.
 *
 * @returns {Promise<{filters: Array<object>}>}
 */
export async function getFilters() {
    return call('/api/vk/filters');
}

/**
 * GET /api/vk/dialogs — страница списка бесед.
 *
 * `signal` — из js/vk/ratelimit.js::createCoalescer: быстрое переключение
 * серверных фильтров обязано ОТМЕНЯТЬ предыдущий запрос, а не копить их в
 * очереди 3 rps.
 *
 * @param {{limit?: number, offset?: number, filter?: string,
 *   signal?: AbortSignal}} [opts]
 * @returns {Promise<{dialogs: Array<object>, has_more: boolean}>}
 */
export async function getDialogs(opts) {
    const p = new URLSearchParams();
    const o = opts || {};
    if (Number.isFinite(o.limit) && o.limit > 0) p.set('limit', String(o.limit));
    if (Number.isFinite(o.offset) && o.offset > 0) p.set('offset', String(o.offset));
    if (o.filter) p.set('filter', String(o.filter));
    const qs = p.toString();
    return call('/api/vk/dialogs' + (qs ? `?${qs}` : ''), o.signal ? { signal: o.signal } : undefined);
}

/**
 * GET /api/vk/dialogs/:id/messages — страница истории.
 *
 * `beforeId` — это `message.id`, а НЕ `conversation_message_id`: параметр VK
 * называется `start_message_id` и принимает глобальный идентификатор
 * сообщения. Подстановка туда cmid дала бы страницу из чужого места истории.
 *
 * @param {string} dialogId
 * @param {{limit?: number, beforeId?: number, signal?: AbortSignal}} [opts]
 * @returns {Promise<{messages: Array<object>, has_more: boolean}>}
 */
export async function getMessages(dialogId, opts) {
    const p = new URLSearchParams();
    const o = opts || {};
    if (Number.isFinite(o.limit) && o.limit > 0) p.set('limit', String(o.limit));
    if (Number.isFinite(o.beforeId) && o.beforeId > 0) p.set('before_id', String(o.beforeId));
    const qs = p.toString();
    return call(
        `/api/vk/dialogs/${seg(dialogId)}/messages` + (qs ? `?${qs}` : ''),
        o.signal ? { signal: o.signal } : undefined,
    );
}

/**
 * GET /api/vk/dialogs/:id/messages/:mid — одно сообщение.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @returns {Promise<object>} MessageDto
 */
export async function getMessage(dialogId, messageId) {
    return call(`/api/vk/dialogs/${seg(dialogId)}/messages/${seg(messageId)}`);
}

/**
 * POST /api/vk/dialogs/:id/messages — отправка.
 *
 * `uploadIds` (альбом) и `uploadId` (один файл) — разные поля: у VK до 10
 * вложений в сообщении, и сервер собирает их одним конвейером.
 *
 * Цель ответа адресуется `conversation_message_id` (`replyToCmid`): у VK
 * именно он одинаков у всех участников беседы и именно его несёт цитата.
 * `replyToId` (глобальный `message.id`) оставлен для случая, когда cmid у
 * фронта нет вовсе; при обоих заданных сервер выбирает cmid.
 *
 * @param {string} dialogId
 * @param {{text?: string, uploadId?: string, uploadIds?: Array<string>,
 *   replyToCmid?: number, replyToId?: number, payload?: string,
 *   captchaSid?: string, captchaKey?: string}} opts
 * @returns {Promise<object>} MessageDto
 */
export async function sendMessage(dialogId, opts) {
    const o = opts || {};
    const body = {};
    if (typeof o.text === 'string') body.text = o.text;
    if (o.uploadId) body.upload_id = String(o.uploadId);
    if (Array.isArray(o.uploadIds) && o.uploadIds.length) {
        body.upload_ids = o.uploadIds.map(String);
    }
    // Явный null тоже значим: «ответ снят», а не «поле не передали».
    if (o.replyToCmid !== undefined) body.reply_to_cmid = o.replyToCmid;
    if (o.replyToId !== undefined) body.reply_to_id = o.replyToId;
    if (o.payload) body.payload = String(o.payload);
    return callC(
        `/api/vk/dialogs/${seg(dialogId)}/messages`,
        jsonInit('POST', withCaptchaFields(body, o)),
    );
}

/**
 * POST /api/vk/dialogs/:id/read — отметить прочитанным.
 *
 * @param {string} dialogId
 * @param {number} maxId
 * @param {{captchaSid?: string, captchaKey?: string}} [opts]
 * @returns {Promise<object>}
 */
export async function markRead(dialogId, maxId, opts) {
    const body = {};
    if (Number.isFinite(maxId) && maxId > 0) body.max_id = maxId;
    return callC(
        `/api/vk/dialogs/${seg(dialogId)}/read`,
        jsonInit('POST', withCaptchaFields(body, opts)),
    );
}

/**
 * POST /api/vk/dialogs/:id/typing — «печатает…».
 *
 * VK держит статус около 10 секунд, поэтому вызывать его на каждую букву
 * нельзя: при лимите 3 rps набор текста съел бы весь бюджет.
 *
 * @param {string} dialogId
 * @param {'typing'|'audiomessage'} [action]
 * @returns {Promise<object>}
 */
export async function sendTyping(dialogId, action) {
    return call(
        `/api/vk/dialogs/${seg(dialogId)}/typing`,
        jsonInit('POST', { action: action || 'typing' }),
    );
}

/**
 * POST /api/vk/dialogs/:id/messages/:mid/button — нажать кнопку бота.
 *
 * Координаты, а не payload: одинаковый payload встречается в разных кнопках
 * одной клавиатуры, и сервер обязан знать, какая именно нажата.
 *
 * АВТОПОВТОРОВ НЕТ: повторное нажатие — действие пользователя.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {{row: number, col: number, captchaSid?: string, captchaKey?: string}} opts
 * @returns {Promise<object>}
 */
export async function pressButton(dialogId, messageId, opts) {
    const o = opts || {};
    const body = { row: Number(o.row) || 0, col: Number(o.col) || 0 };
    return callC(
        `/api/vk/dialogs/${seg(dialogId)}/messages/${seg(messageId)}/button`,
        jsonInit('POST', withCaptchaFields(body, o)),
    );
}

/**
 * Адрес вложения — через прокси бэкенда, всегда.
 *
 * Индекс обязателен: у сообщения VK до 10 вложений, и без него второе фото
 * затирало бы первое и в кэше, и в ключе objectURL.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} index
 * @param {'thumb'|'full'} [kind]
 * @returns {string}
 */
export function mediaUrl(dialogId, messageId, index, kind) {
    const k = kind === 'full' ? 'full' : 'thumb';
    return `/api/vk/media/${seg(dialogId)}/${seg(messageId)}/${seg(index)}?kind=${k}`;
}

/**
 * Сырой Response вложения — для прогресса через `body.getReader()`.
 *
 * Возвращает именно Response, а не тело: разбирать поток обязан вызывающий,
 * иначе прогресс скачивания посчитать не из чего.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} index
 * @param {'thumb'|'full'} [kind]
 * @returns {Promise<Response>}
 */
export async function fetchMedia(dialogId, messageId, index, kind) {
    return apiFetch(mediaUrl(dialogId, messageId, index, kind));
}
