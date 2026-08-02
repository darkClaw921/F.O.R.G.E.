// tmux-web — Telegram plugin: REST-клиент.
//
// Паттерн повторяет js/echo/api.js: тонкие обёртки поверх apiFetch из
// core/api.js (Authorization подставляет подменённый window.fetch в
// core/auth.js). Отличие — разбор единого контракта ошибок плагина:
//
//   {"error":{"code":"not_authorized","message":"…","retry_after_secs":42}}
//
// Ошибка приходит как Error с полями .status, .code, .message и
// .retryAfterSecs — вызывающий код принимает решение по КОДУ, а не по
// тексту (тексты меняются, коды — контракт).

import { apiFetch } from '../core/api.js';

/**
 * Выполняет запрос и разбирает ответ.
 *
 * @param {string} path — путь под /api/telegram/*
 * @param {RequestInit} [init]
 * @returns {Promise<any>} распарсенное тело
 * @throws {Error} с полями status/code/retryAfterSecs при не-2xx
 */
async function call(path, init) {
    const res = await apiFetch(path, init);
    const ct = res.headers.get('content-type') || '';
    let body = null;
    try {
        body = ct.includes('application/json') ? await res.json() : await res.text();
    } catch (_) {
        // Пустое/битое тело — оставляем null, ниже соберём ошибку по статусу.
    }
    if (!res.ok) {
        const e = (body && typeof body === 'object' && body.error) ? body.error : null;
        const err = new Error(
            (e && e.message) || (typeof body === 'string' && body) || `HTTP ${res.status}`
        );
        err.status = res.status;
        err.code = (e && e.code) || null;
        err.retryAfterSecs = (e && typeof e.retry_after_secs === 'number')
            ? e.retry_after_secs
            : null;
        err.body = body;
        throw err;
    }
    return body;
}

function jsonInit(method, payload) {
    return {
        method,
        headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
        body: JSON.stringify(payload || {}),
    };
}

/**
 * GET /api/telegram/status.
 *
 * Единственный роут, отвечающий 200 в любом состоянии логина. 404
 * означает «плагин выключен» (роуты не зарегистрированы) — это НЕ ошибка,
 * и обрабатывать её должен вызывающий (см. initTelegramGate).
 *
 * @returns {Promise<{enabled: boolean, state: string, phone: ?string,
 *   hint: ?string, error: ?string, user: ?object}>}
 */
export async function getStatus() {
    return call('/api/telegram/status');
}

/**
 * POST /api/telegram/login/start — сохранить креды (если переданы) и
 * запросить код подтверждения.
 *
 * КРИТИЧНО: этот вызов НИКОГДА не ретраится автоматически. При
 * `code === 'flood_wait'` Telegram блокирует логин на часы, и каждая
 * повторная попытка продлевает блокировку (риск №2 плана). Повтор — только
 * по явному действию пользователя после истечения countdown'а.
 *
 * @param {{phone: string, api_id?: number, api_hash?: string}} payload
 * @returns {Promise<object>} LoginStateDto
 */
export async function loginStart(payload) {
    return call('/api/telegram/login/start', jsonInit('POST', payload));
}

/**
 * POST /api/telegram/login/code.
 *
 * Неверный код — это `400 invalid_input`; токен логина на сервере
 * сохраняется, поэтому wizard оставляет поле доступным для повтора.
 *
 * @param {string} code
 * @returns {Promise<object>} LoginStateDto
 */
export async function loginCode(code) {
    return call('/api/telegram/login/code', jsonInit('POST', { code }));
}

/**
 * POST /api/telegram/login/password (2FA).
 *
 * Неверный пароль — `400 invalid_input`; сервер получает от Telegram новый
 * токен и кладёт его обратно в состояние, поэтому повторный ввод работает
 * без перезапуска wizard'а.
 *
 * @param {string} password
 * @returns {Promise<object>} LoginStateDto
 */
export async function loginPassword(password) {
    return call('/api/telegram/login/password', jsonInit('POST', { password }));
}

/**
 * POST /api/telegram/logout — выход из аккаунта.
 *
 * Успешен из любого состояния: сервер гасит соединение, удаляет файл
 * сессии и возвращает актуальный LoginStateDto.
 *
 * @returns {Promise<object>} LoginStateDto
 */
export async function logout() {
    return call('/api/telegram/logout', jsonInit('POST', {}));
}

/**
 * GET /api/telegram/dialogs — список диалогов.
 *
 * Порядок — как отдал Telegram (закреплённые сверху); фронт НЕ
 * пересортировывает. Все id в ответе — строки (64-битные значения не влезают
 * в Number без потери точности).
 *
 * @param {number} [limit] — сервер клампит по своему потолку (500)
 * @param {'main'|'archive'|'all'} [folder] — без параметра сервер отдаёт всё
 *   (прежнее поведение); 'archive' — только архив, 'main' — без архива.
 *   Невалидное значение сервер отвергает 400, поэтому мусор не отправляем.
 * @returns {Promise<{dialogs: Array<object>}>}
 */
export async function getDialogs(limit, folder) {
    const p = new URLSearchParams();
    if (Number.isFinite(limit) && limit > 0) p.set('limit', String(limit));
    if (folder === 'main' || folder === 'archive' || folder === 'all') p.set('folder', folder);
    const qs = p.toString();
    return call('/api/telegram/dialogs' + (qs ? '?' + qs : ''));
}

/**
 * GET /api/telegram/folders — пользовательские папки аккаунта.
 *
 * Основной список (Default) в ответ не входит; аккаунт без папок — это
 * `{folders: []}`, а не ошибка. Все *_ids внутри — строки Bot API-формата,
 * те же, что DialogDto.id. Порядок — как в Telegram.
 *
 * @returns {Promise<{folders: Array<object>}>}
 */
export async function listFolders() {
    return call('/api/telegram/folders');
}

/**
 * GET /api/telegram/dialogs/:id/messages — история сообщений.
 *
 * Ответ отсортирован ПО ВОЗРАСТАНИЮ id, поэтому страницы просто
 * конкатенируются: свежая — в конец, `before_id`-страница — в начало.
 * `has_more` — эвристика «вернулось ровно limit».
 *
 * @param {string} dialogId
 * @param {{limit?: number, beforeId?: number}} [opts]
 * @returns {Promise<{messages: Array<object>, has_more: boolean}>}
 */
export async function getMessages(dialogId, opts) {
    const p = new URLSearchParams();
    const limit = opts && opts.limit;
    const beforeId = opts && opts.beforeId;
    if (Number.isFinite(limit) && limit > 0) p.set('limit', String(limit));
    if (Number.isFinite(beforeId) && beforeId > 0) p.set('before_id', String(beforeId));
    const qs = p.toString();
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/messages` + (qs ? '?' + qs : '')
    );
}

/**
 * GET /api/telegram/dialogs/:id/messages/:mid — ОДНО сообщение.
 *
 * Ответ ПЛОСКИЙ — сам MessageDto, без обёртки `{message: …}`, которую отдаёт
 * POST на тот же путь. Расхождение зафиксировано контрактом роута, поэтому
 * тут `resp.id`, а не `resp.message.id`.
 *
 * Зачем нужен: по клику на цитату надо показать оригинал, а он часто лежит
 * ВНЕ загруженной страницы истории. Догонять его через `getMessages` значило
 * бы тянуть историю страницами до нужного id — десятки запросов ради одной
 * строки; здесь это один запрос по известному id.
 *
 * Ошибки (решение принимается по `.code`, как и везде в модуле):
 * `404 unknown_message` — оригинал удалён, не принадлежит диалогу или id
 * битый (для цитаты это «сообщение недоступно» — конечное состояние, повтор
 * бессмыслен); `404 unknown_dialog`; `409 not_authorized`;
 * `429 flood_wait` (+`retryAfterSecs`); `503 not_connected` — вот его как раз
 * имеет смысл предложить повторить.
 *
 * Автоповтора здесь нет намеренно: переход к цитате инициирует пользователь,
 * и молчаливый ретрай при `flood_wait` только продлил бы блокировку.
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @returns {Promise<object>} MessageDto
 */
export async function getMessage(dialogId, messageId) {
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}`
        + `/messages/${encodeURIComponent(messageId)}`
    );
}

/**
 * POST /api/telegram/dialogs/:id/messages — отправка текста ИЛИ медиа.
 *
 * Ответ содержит ПОЛНЫЙ MessageDto с реальными id/date — на нём строится
 * reconcile optimistic-строки. `message.id <= 0` означает «Telegram не
 * подтвердил id»: строку надо снять и дождаться `new_message` по WS.
 *
 * `uploadId` — идентификатор из `POST /api/telegram/upload`. Сервер
 * ПОТРЕБЛЯЕТ его: повторный вызов с тем же значением вернёт
 * `409 invalid_state`, а не второе сообщение. Именно на этом этапе идёт
 * заливка файла в Telegram, поэтому запрос может занять минуты и всё это
 * время в WS сыплется `upload_progress`.
 *
 * `replyToId` кладётся в тело НЕЗАВИСИМО от того, текст это или медиа:
 * ответить можно и файлом, и картинкой, а сервер применяет `reply_to` к обеим
 * веткам одинаково. Поэтому проверка стоит отдельной строкой, а не внутри
 * `if (p.text)`. Нефинитное значение (undefined/NaN — «полосы ответа нет»)
 * просто не попадает в тело: сервер отвечает на битый `reply_to_id` 400, и
 * отправлять мусор ради «поля ради поля» нельзя.
 *
 * Ответ на удалённое сообщение Telegram принимает — приходит обычное
 * сообщение без цитаты, а не ошибка.
 *
 * @param {string} dialogId
 * @param {{text?: string, caption?: string, uploadId?: string,
 *   replyToId?: number}} payload
 * @returns {Promise<{message: object}>}
 */
export async function sendMessage(dialogId, payload) {
    const p = payload || {};
    const body = {};
    if (p.text) body.text = p.text;
    if (p.caption) body.caption = p.caption;
    if (p.uploadId) body.upload_id = p.uploadId;
    if (!body.text && !body.caption && !body.upload_id) body.text = '';
    if (Number.isFinite(p.replyToId)) body.reply_to_id = p.replyToId;
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/messages`,
        jsonInit('POST', body)
    );
}

/**
 * POST /api/telegram/dialogs/:id/read — отметить прочитанным.
 *
 * Без `maxId` прочитывает диалог целиком. Сервер сам разошлёт `read`/`in`
 * остальным вкладкам, поэтому локально бейдж можно не трогать.
 *
 * @param {string} dialogId
 * @param {number} [maxId]
 * @returns {Promise<{ok: boolean}>}
 */
export async function markRead(dialogId, maxId) {
    const body = Number.isFinite(maxId) && maxId > 0 ? { max_id: maxId } : {};
    return call(`/api/telegram/dialogs/${encodeURIComponent(dialogId)}/read`, jsonInit('POST', body));
}

/**
 * POST /api/telegram/dialogs/:id/typing — статус «печатает…».
 *
 * Не критичное действие: вызывающий код обязан ГЛОТАТЬ ошибки (в частности
 * `429 flood_wait`), иначе из-за индикатора набора ломался бы UI.
 *
 * @param {string} dialogId
 * @param {'typing'|'cancel'} [action='typing']
 * @returns {Promise<{ok: boolean}>}
 */
export async function sendTyping(dialogId, action) {
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/typing`,
        jsonInit('POST', { action: action || 'typing' })
    );
}

/**
 * POST /api/telegram/dialogs/:id/messages/:mid/callback — нажатие
 * inline-кнопки бота (`messages.getBotCallbackAnswer`).
 *
 * `data` — НЕПРОЗРАЧНЫЙ токен: base64 (standard, с padding) от callback-data
 * кнопки, как его отдал сервер в `MessageDto.reply_markup`. Фронт его НЕ
 * декодирует и НЕ конструирует сам — только гоняет обратно как есть. Для
 * game-кнопок `data` нет вовсе, вместо него `game: true`.
 *
 * КРИТИЧНО: этот вызов НИКОГДА не ретраится автоматически. Каждое нажатие
 * Telegram доставляет боту, и автоповтор при `429 flood_wait` продлевает
 * блокировку аккаунта (тот же класс риска, что у loginStart). Повтор — только
 * по явному повторному клику пользователя.
 *
 * Ответ: `{message, alert, url, cache_time}` — все четыре поля присутствуют
 * всегда (могут быть `null`), поэтому `undefined` и `null` не разъедутся.
 * Ошибки: `409 not_authorized|invalid_state`, `404 unknown_dialog|unknown_message`,
 * `400 invalid_input`, `429 flood_wait` (+`retryAfterSecs`), `502 telegram_error`,
 * `503 not_connected`, `504 bot_timeout` («бот не ответил»).
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @param {{data?: ?string, game?: boolean}} [opts]
 * @returns {Promise<{message: ?string, alert: boolean, url: ?string,
 *   cache_time: ?number}>}
 */
export async function sendCallback(dialogId, messageId, opts) {
    const o = opts || {};
    const body = {
        data: typeof o.data === 'string' && o.data ? o.data : null,
        game: o.game === true,
    };
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}`
        + `/messages/${encodeURIComponent(messageId)}/callback`,
        jsonInit('POST', body)
    );
}

/**
 * Путь к медиа сообщения.
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @param {'thumb'|'full'} kind
 * @returns {string}
 */
export function mediaUrl(dialogId, messageId, kind) {
    const k = kind === 'thumb' ? 'thumb' : 'full';
    return `/api/telegram/media/${encodeURIComponent(dialogId)}/${encodeURIComponent(messageId)}?kind=${k}`;
}

/**
 * GET /api/telegram/media/:dialog_id/:message_id — СЫРОЙ Response.
 *
 * Отдаётся именно `Response`, а не готовый blob: вызывающий (media.js) читает
 * тело через `body.getReader()`, чтобы показывать процент загрузки. Ошибка
 * разбирается в тот же контракт, что и в `call()` — `Error` с полями
 * `status`/`code`/`retryAfterSecs`, потому что решения принимаются по КОДУ
 * (`no_media` — рисуем заглушку и не ретраим, `flood_wait` — ждём, `503` —
 * даём кнопку повтора).
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @param {'thumb'|'full'} kind
 * @returns {Promise<Response>}
 * @throws {Error} с полями status/code/retryAfterSecs при не-2xx
 */
export async function fetchMedia(dialogId, messageId, kind) {
    const res = await apiFetch(mediaUrl(dialogId, messageId, kind));
    if (res.ok) return res;

    let body = null;
    try {
        body = await res.json();
    } catch (_) {
        // Ошибка без JSON-тела (например, ответ прокси) — соберём по статусу.
    }
    const e = (body && typeof body === 'object' && body.error) ? body.error : null;
    const err = new Error((e && e.message) || `HTTP ${res.status}`);
    err.status = res.status;
    err.code = (e && e.code) || null;
    err.retryAfterSecs = (e && typeof e.retry_after_secs === 'number')
        ? e.retry_after_secs
        : null;
    throw err;
}

export { call as _call, jsonInit as _jsonInit };
