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
//   GET  /api/vk/dialogs/:id/prompt                  getChatPrompt         (АЧ-Ф1)
//   PUT  /api/vk/dialogs/:id/prompt                  putChatPrompt         (АЧ-Ф1)
//   GET  /api/vk/dialogs/:id/analysis                getSavedAnalysis      (АЧ-И1)
//   DELETE /api/vk/dialogs/:id/analysis              clearSavedAnalysis    (АЧ-И1)
//   POST /api/vk/dialogs/:id/analysis                startAnalysis         (АЧ-Ф3)
//   DELETE /api/vk/dialogs/:id/analysis/thread       clearThread           (АЧ-И2)
//   POST /api/vk/dialogs/:id/analysis/cancel         cancelAnalysis        (АЧ-Ф3)
//   POST /api/vk/voice/:peer/:mid/:idx/transcribe    transcribeVoice       (ГС-Ф3)
//   GET  /api/vk/voice/:peer/:mid/:idx/transcript    getVoiceTranscript    (ГС-Ф3)
//   GET  /api/vk/voice/settings                      getVoiceSettings      (ГС-Ф4)
//   PUT  /api/vk/voice/settings                      setVoiceSettings      (ГС-Ф4)
//
// («АЧ» — фазы фичи «Анализ чата через Claude», нумерация у неё своя; «АЧ-И» —
// её доработки: «И1» — сохраняемый и инкрементальный анализ, «И2» — тред
// вопросов на сервере. Ровно те же пометки стоят в шапке
// plugins/vk/src/routes/mod.rs.)
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

// ---------------------------------------------------------------------------
// Анализ чата через Claude (АЧ-Ф1/Ф3).
//
// Место в бюджете 3 rps: очередь js/vk/ratelimit.js берёт ВЫЗЫВАЮЩИЙ
// (js/vk/analysis.js делает `await vkLimiter.acquire(PRIORITY_USER)` перед
// каждым из этих вызовов) — ровно как chat.js и dialogs.js. Планировщик не
// импортируется сюда намеренно: api.js остаётся чистым транспортом, иначе
// один и тот же запрос занимал бы место в очереди дважды.
//
// Все четыре идут через `callC`, а не через голый `call`: код 14 у VK
// прилетает на ЛЮБОЙ метод, в том числе на выгрузку истории внутри
// `POST /analysis`, и ошибка обязана донести до captcha.js поле `err.captcha`.
// ---------------------------------------------------------------------------

/**
 * GET /api/vk/dialogs/:id/prompt — заметка пользователя к беседе.
 *
 * `max_chars` приходит С СЕРВЕРА и не зашивается в JS: счётчик символов в
 * textarea обязан совпадать с лимитом, по которому сервер реально режет, —
 * иначе пользователь допишет фразу, которая молча пропадёт.
 *
 * Отсутствие заметки — это `{prompt: '', updated_at: null}`, а НЕ 404: «нет
 * заметки» и «пустая заметка» на этом роуте одно состояние.
 *
 * @param {string} dialogId
 * @returns {Promise<{prompt: string, updated_at: ?number, max_chars: number}>}
 */
export async function getChatPrompt(dialogId) {
    return callC(`/api/vk/dialogs/${seg(dialogId)}/prompt`);
}

/**
 * PUT /api/vk/dialogs/:id/prompt — сохранить заметку.
 *
 * Пустая строка СТИРАЕТ заметку, и это единственный способ её стереть:
 * отдельного DELETE нет. Тело обязано присутствовать всегда — отсутствующий
 * или битый JSON сервер считает `400 invalid_input`, а не «стереть».
 *
 * @param {string} dialogId
 * @param {string} prompt
 * @returns {Promise<{prompt: string, updated_at: ?number, max_chars: number}>}
 */
export async function putChatPrompt(dialogId, prompt) {
    return callC(
        `/api/vk/dialogs/${seg(dialogId)}/prompt`,
        jsonInit('PUT', { prompt: String(prompt == null ? '' : prompt) }),
    );
}

/**
 * GET /api/vk/dialogs/:id/analysis — последний СОХРАНЁННЫЙ разбор беседы.
 *
 * Лежит в том же локальном файле, что и заметка, поэтому роут отвечает 200 и
 * при мёртвой сети VK: попап показывает прошлый разбор даже когда выгрузить
 * новую историю нечем.
 *
 * «Разбора нет» — это `{text: '', updated_at: null, last_message_id: null}`, а
 * НЕ 404: попап открывают и до первого запуска, и ошибка в этом месте была бы
 * враньём о нормальном состоянии.
 *
 * `last_message_id` — точка отсчёта REFRESH (до какого сообщения разобрано).
 * Фронту она нужна только как признак «есть от чего считать новые»; сам номер
 * в запрос не уезжает — его берёт сервер из своего же хранилища.
 *
 * `thread` — сохранённый тред вопросов и ответов (АЧ-И2), от старых к новым.
 * Он ВСЕГДА массив: сервер отдаёт `[]` там, где вопросов не задавали, а
 * потребитель (js/vk/analysis.js) обходит его циклом и не обязан помнить, что
 * поля может не быть вовсе (старый бэкенд, кэш PWA со старым фронтом).
 * Нормализация здесь, а не у потребителя: точка входа данных одна.
 *
 * @param {string} dialogId
 * @returns {Promise<{text: string, last_message_id: ?number,
 *   messages_used: number, updated_at: ?number, max_chars: number,
 *   thread: Array<{q: string, a: string, at: ?number}>}>}
 */
export async function getSavedAnalysis(dialogId) {
    const dto = await callC(`/api/vk/dialogs/${seg(dialogId)}/analysis`);
    const raw = dto && Array.isArray(dto.thread) ? dto.thread : [];
    const thread = raw
        .filter((t) => t && typeof t === 'object')
        .map((t) => ({
            q: String(t.q == null ? '' : t.q),
            a: String(t.a == null ? '' : t.a),
            at: Number.isFinite(t.at) ? Number(t.at) : null,
        }));
    return Object.assign({}, dto, { thread });
}

/**
 * DELETE /api/vk/dialogs/:id/analysis/thread — стереть ТОЛЬКО тред вопросов.
 *
 * Отдельный путь, а не `?thread=1` у общего DELETE: query-параметры в этом
 * плагине разбираются вручную (`RawQuery`), потому что `Query<T>` ломает
 * auth-лестницу маршрута. Заодно это делает разведение кнопок буквальным —
 * CLEAR ✕ и CLEAR Q&A бьют в РАЗНЫЕ адреса и не могут стереть чужое.
 *
 * Ни разбор, ни заметку беседы не трогает. `{cleared: false}` — не ошибка, а
 * «стирать было нечего» (две вкладки нажали подряд).
 *
 * @param {string} dialogId
 * @returns {Promise<{cleared: boolean}>}
 */
export async function clearThread(dialogId) {
    return callC(
        `/api/vk/dialogs/${seg(dialogId)}/analysis/thread`,
        jsonInit('DELETE', {}),
    );
}

/**
 * DELETE /api/vk/dialogs/:id/analysis — стереть сохранённый разбор.
 *
 * Заметку беседы НЕ трогает: в попапе это две разные кнопки и в хранилище две
 * разные величины. `{cleared: false}` — не ошибка, а «стирать было нечего»
 * (две вкладки нажали подряд).
 *
 * @param {string} dialogId
 * @returns {Promise<{cleared: boolean}>}
 */
export async function clearSavedAnalysis(dialogId) {
    return callC(`/api/vk/dialogs/${seg(dialogId)}/analysis`, jsonInit('DELETE', {}));
}

/**
 * POST /api/vk/dialogs/:id/analysis — запустить разбор или задать вопрос.
 *
 * Ответ приходит СРАЗУ, в момент запуска процесса: текст модели едет кадрами
 * `analysis_delta` по /ws/vk, финал — `analysis_done`. Держать HTTP открытым
 * на всю генерацию нельзя, её оборвёт любой прокси между браузером и devforge.
 *
 * `attached: true` означает, что нового процесса не запускалось — мы
 * подключились к уже идущему run'у (вторая вкладка, повторный клик).
 *
 * ТРИ вида запуска (`kind`):
 * - `analyze` — полный разбор выгруженной истории;
 * - `ask` — вопрос по той же истории (сохранённый разбор не трогает);
 * - `refresh` — дочитать только сообщения новее сохранённого разбора и
 *   вернуть ОБНОВЛЁННЫЙ ЦЕЛЬНЫЙ разбор, который заменит прошлый.
 *
 * ДВА исхода `refresh`, о которых обязан знать вызывающий:
 * - сохранённого разбора нет ⇒ `400 invalid_input` (сначала полный разбор);
 * - новых сообщений нет ⇒ `200 {run_id: null, no_new_messages: true}`. Это
 *   штатный ответ, а не ошибка: модель не запускалась, кадров по /ws/vk не
 *   будет, и ждать их нельзя — попап навсегда остался бы в «идёт разбор».
 *   Именно ради этого случая `run_id` стал nullable.
 *
 * Необязательные поля НЕ отправляются вовсе, если их нет: сервер отличает
 * отсутствующий `depth` (взять значение по умолчанию) от переданного.
 *
 * @param {string} dialogId
 * @param {{kind: 'analyze'|'ask'|'refresh', question?: string, depth?: number,
 *   history?: Array<{q: string, a: string}>}} opts
 * @returns {Promise<{run_id: ?string, attached: boolean, messages_used: number,
 *   truncated: boolean, no_new_messages: boolean}>}
 */
export async function startAnalysis(dialogId, opts) {
    const o = opts || {};
    const body = { kind: String(o.kind || 'analyze') };
    if (o.question != null && String(o.question) !== '') body.question = String(o.question);
    if (Number.isFinite(o.depth) && o.depth > 0) body.depth = Number(o.depth);
    if (Array.isArray(o.history) && o.history.length) {
        body.history = o.history.map((t) => ({
            q: String((t && t.q) || ''),
            a: String((t && t.a) || ''),
        }));
    }
    return callC(`/api/vk/dialogs/${seg(dialogId)}/analysis`, jsonInit('POST', body));
}

/**
 * POST /api/vk/dialogs/:id/analysis/cancel — остановить идущий разбор.
 *
 * `runId` необязателен: без него сервер гасит тот run, что идёт по этой
 * беседе. Именно так кнопка «стоп» продолжает работать после перезагрузки
 * вкладки, потерявшей `run_id`.
 *
 * `{cancelled: false}` — не ошибка: гасить было нечего (run уже закончился).
 *
 * @param {string} dialogId
 * @param {?string} [runId]
 * @returns {Promise<{cancelled: boolean}>}
 */
export async function cancelAnalysis(dialogId, runId) {
    const body = {};
    if (runId) body.run_id = String(runId);
    return callC(`/api/vk/dialogs/${seg(dialogId)}/analysis/cancel`, jsonInit('POST', body));
}

// ============================================================================
// Голосовые сообщения (ГС-Ф3)
// ============================================================================

/**
 * POST /api/vk/voice/:peer/:mid/:idx/transcribe — запустить расшифровку.
 *
 * Ответ приходит СРАЗУ, как у `startAnalysis`: сама расшифровка идёт секунды
 * (минутное голосовое) или минуты (пятнадцатиминутное), а прогресс едет
 * кадрами `voice_transcribe_*` по /ws/vk.
 *
 * ТРИ исхода, о которых обязан знать вызывающий:
 * - `{ready: true, text}` — расшифровка уже была посчитана; движок не
 *   запускался, кадров по WS не будет;
 * - `{run_id, attached: false}` — прогон запущен, ждём кадры;
 * - `{run_id, attached: true}` — расшифровка этого вложения УЖЕ идёт (вторая
 *   вкладка, повторный клик), нового прогона не запускалось.
 *
 * Коды отказов — свои у каждой причины, и решать по ним, а не по тексту:
 * `voice_not_audio`, `voice_too_long`, `voice_no_ffmpeg`, `voice_no_whisper`,
 * `voice_no_model`, `voice_no_key`, `voice_busy`, `metal_unavailable`.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} index номер вложения в сообщении
 * @param {{force?: boolean}} [opts] `force` — пересчитать, даже если готово
 * @returns {Promise<{run_id: ?string, attached: boolean, ready: boolean, engine: string, text?: string}>}
 */
export async function transcribeVoice(dialogId, messageId, index, opts) {
    // `?force=1` дописывается ОТДЕЛЬНО от шаблона пути — так же, как в
    // js/telegram/api.js: разборщики контракта читают адреса из кода, и
    // подстановка внутри литерала превратила бы путь в `…/transcribe:`.
    const q = opts && opts.force ? '?force=1' : '';
    return callC(
        `/api/vk/voice/${seg(dialogId)}/${seg(messageId)}/${seg(index)}/transcribe` + q,
        jsonInit('POST', {}),
    );
}

/**
 * GET /api/vk/voice/:peer/:mid/:idx/transcript — готовая расшифровка.
 *
 * Расшифровки нет — это `{ready: false}` с пустым текстом, а НЕ ошибка: «ещё
 * не расшифровывали» — нормальное состояние вложения. Ни одного вызова VK и
 * ни одного запуска движка: ответ читается из локального файла, поэтому
 * работает и при мёртвой сети.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} index
 * @returns {Promise<{ready: boolean, text: string, language: string, engine: string, duration_ms: number, created_at: ?number}>}
 */
export async function getVoiceTranscript(dialogId, messageId, index) {
    return callC(
        `/api/vk/voice/${seg(dialogId)}/${seg(messageId)}/${seg(index)}/transcript`,
    );
}

/**
 * GET /api/vk/voice/settings — каким движком расшифровываются голосовые.
 *
 * Настройка ОБЩАЯ для всех мессенджеров: она живёт в `voice.toml` рядом с
 * ключом polza.ai, а не в `user_settings.json`. Поэтому три панели читают её
 * своими роутами, но видят одно и то же значение.
 *
 * Ключ облака НИКОГДА не приходит — только признак `cloud_key_set`.
 * `warning` (если он есть) объясняет, чего выбранному движку не хватает:
 * `voice_no_key`, `voice_no_model`, `voice_no_whisper`, `voice_no_ffmpeg`.
 *
 * Логина в мессенджере роут НЕ требует: движок настраивают до входа.
 *
 * @returns {Promise<{engine: string, cloud_key_set: boolean, warning?: {code: string, message: string}}>}
 */
export async function getVoiceSettings() {
    return callC('/api/vk/voice/settings');
}

/**
 * PUT /api/vk/voice/settings — переключить движок расшифровки.
 *
 * Выбор СОХРАНЯЕТСЯ, даже если движок сейчас нерабочий: в ответе тогда придёт
 * `warning`. Отказ приходит только на чужое слово (`400 invalid_input`).
 *
 * @param {string} engine `local` | `cloud`
 * @returns {Promise<{engine: string, cloud_key_set: boolean, warning?: {code: string, message: string}}>}
 */
export async function setVoiceSettings(engine) {
    return callC('/api/vk/voice/settings', jsonInit('PUT', { engine: String(engine || '') }));
}

