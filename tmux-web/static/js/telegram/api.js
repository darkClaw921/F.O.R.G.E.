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

// ---------------------------------------------------------------------------
// Анализ чата через Claude.
//
// Пути и формы тел БАЙТ В БАЙТ повторяют VK-шные (js/vk/api.js): попап один и
// тот же (js/messenger/analysis-core.js), и расхождение в транспорте означало
// бы «в VK работает, в Telegram нет».
//
//   GET    /api/telegram/dialogs/:id/prompt            getChatPrompt
//   PUT    /api/telegram/dialogs/:id/prompt            putChatPrompt
//   GET    /api/telegram/dialogs/:id/analysis          getSavedAnalysis
//   POST   /api/telegram/dialogs/:id/analysis          startAnalysis
//   DELETE /api/telegram/dialogs/:id/analysis          clearSavedAnalysis
//   POST   /api/telegram/dialogs/:id/analysis/cancel   cancelAnalysis
//   DELETE /api/telegram/dialogs/:id/analysis/thread   clearThread
//   POST   /api/telegram/voice/:id/:mid/:idx/transcribe transcribeVoice    (ГС-Ф3)
//   GET    /api/telegram/voice/:id/:mid/:idx/transcript getVoiceTranscript (ГС-Ф3)
//   GET    /api/telegram/voice/settings              getVoiceSettings   (ГС-Ф4)
//   PUT    /api/telegram/voice/settings              setVoiceSettings   (ГС-Ф4)
//
// Лимитера у Telegram нет вовсе (в отличие от VK) — вызовы идут прямо через
// общий `call`, разбирающий тот же конверт ошибок `{"error":{"code":…}}`.
// ---------------------------------------------------------------------------

/**
 * GET /api/telegram/dialogs/:id/prompt — заметка пользователя к чату.
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
    return call(`/api/telegram/dialogs/${encodeURIComponent(dialogId)}/prompt`);
}

/**
 * PUT /api/telegram/dialogs/:id/prompt — сохранить заметку.
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
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/prompt`,
        jsonInit('PUT', { prompt: String(prompt == null ? '' : prompt) }),
    );
}

/**
 * GET /api/telegram/dialogs/:id/analysis — последний СОХРАНЁННЫЙ разбор.
 *
 * Лежит в локальном файле, поэтому роут отвечает 200 и при мёртвой связи с
 * Telegram: попап показывает прошлый разбор даже когда выгрузить новую
 * историю нечем.
 *
 * «Разбора нет» — это `{text: '', updated_at: null, last_message_id: null}`, а
 * НЕ 404: попап открывают и до первого запуска, и ошибка в этом месте была бы
 * враньём о нормальном состоянии.
 *
 * `thread` ВСЕГДА массив: сервер отдаёт `[]` там, где вопросов не задавали, а
 * потребитель (ядро попапа) обходит его циклом и не обязан помнить, что поля
 * может не быть вовсе (старый бэкенд, кэш PWA со старым фронтом).
 * Нормализация здесь, а не у потребителя: точка входа данных одна.
 *
 * @param {string} dialogId
 * @returns {Promise<{text: string, last_message_id: ?number,
 *   messages_used: number, updated_at: ?number, max_chars: number,
 *   thread: Array<{q: string, a: string, at: ?number}>}>}
 */
export async function getSavedAnalysis(dialogId) {
    const dto = await call(`/api/telegram/dialogs/${encodeURIComponent(dialogId)}/analysis`);
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
 * DELETE /api/telegram/dialogs/:id/analysis — стереть сохранённый разбор.
 *
 * Заметку чата НЕ трогает: в попапе это две разные кнопки и в хранилище две
 * разные величины. `{cleared: false}` — не ошибка, а «стирать было нечего»
 * (две вкладки нажали подряд).
 *
 * @param {string} dialogId
 * @returns {Promise<{cleared: boolean}>}
 */
export async function clearSavedAnalysis(dialogId) {
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/analysis`,
        jsonInit('DELETE', {}),
    );
}

/**
 * DELETE /api/telegram/dialogs/:id/analysis/thread — стереть ТОЛЬКО тред.
 *
 * Отдельный путь, а не `?thread=1` у общего DELETE: так разведение кнопок
 * становится буквальным — CLEAR ✕ и CLEAR Q&A бьют в РАЗНЫЕ адреса и не могут
 * стереть чужое. Ни разбор, ни заметку чата не трогает. `{cleared: false}` —
 * не ошибка, а «стирать было нечего».
 *
 * @param {string} dialogId
 * @returns {Promise<{cleared: boolean}>}
 */
export async function clearThread(dialogId) {
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/analysis/thread`,
        jsonInit('DELETE', {}),
    );
}

/**
 * POST /api/telegram/dialogs/:id/analysis — запустить разбор или задать вопрос.
 *
 * Ответ приходит СРАЗУ, в момент запуска процесса: текст модели едет кадрами
 * `analysis_delta` по /ws/telegram, финал — `analysis_done`. Держать HTTP
 * открытым на всю генерацию нельзя, её оборвёт любой прокси между браузером и
 * devforge.
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
 *   штатный ответ, а не ошибка: модель не запускалась, кадров не будет, и
 *   ждать их нельзя — попап навсегда остался бы в «идёт разбор».
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
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/analysis`,
        jsonInit('POST', body),
    );
}

/**
 * POST /api/telegram/dialogs/:id/analysis/cancel — остановить идущий разбор.
 *
 * `runId` необязателен: без него сервер гасит тот run, что идёт по этому чату.
 * Именно так кнопка «стоп» продолжает работать после перезагрузки вкладки,
 * потерявшей `run_id`.
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
    return call(
        `/api/telegram/dialogs/${encodeURIComponent(dialogId)}/analysis/cancel`,
        jsonInit('POST', body),
    );
}

export { call as _call, jsonInit as _jsonInit };

// ---------------------------------------------------------------------------
// Голосовые сообщения (ГС-Ф3).
//
// Пути и формы ответов БАЙТ В БАЙТ повторяют VK-шные (js/vk/api.js): текст под
// голосовым рисует один и тот же модуль, и расхождение в транспорте означало
// бы «в VK работает, в Telegram нет».
//
// Индекс вложения у Telegram ВСЕГДА 0 — у сообщения ровно одно вложение. В
// адресе он есть ради одинаковости с VK, где вложений до десяти.
// ---------------------------------------------------------------------------

/**
 * POST /api/telegram/voice/:id/:mid/:idx/transcribe — запустить расшифровку.
 *
 * Ответ приходит СРАЗУ: сама расшифровка идёт секунды или минуты, а прогресс
 * едет кадрами `voice_transcribe_*` по /ws/telegram.
 *
 * ТРИ исхода: `{ready: true, text}` — уже посчитано (движок не запускался);
 * `{run_id, attached: false}` — прогон запущен; `{run_id, attached: true}` —
 * расшифровка этого вложения уже идёт (вторая вкладка, повторный клик).
 *
 * Решать по `err.code`, а не по тексту: `voice_not_audio`, `voice_too_long`,
 * `voice_no_ffmpeg`, `voice_no_whisper`, `voice_no_model`, `voice_no_key`,
 * `voice_busy`, `metal_unavailable`.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} [index] всегда 0
 * @param {{force?: boolean}} [opts] `force` — пересчитать, даже если готово
 * @returns {Promise<{run_id: ?string, attached: boolean, ready: boolean, engine: string, text?: string}>}
 */
export async function transcribeVoice(dialogId, messageId, index, opts) {
    const idx = Number(index || 0);
    // `?force=1` дописывается ОТДЕЛЬНО от шаблона пути: контрактный тест
    // вычитывает адреса прямо из кода, и подстановка внутри литерала
    // превратила бы путь в `…/transcribe:`.
    const q = opts && opts.force ? '?force=1' : '';
    return call(
        `/api/telegram/voice/${encodeURIComponent(dialogId)}/${encodeURIComponent(messageId)}/${idx}/transcribe` + q,
        jsonInit('POST', {}),
    );
}

/**
 * GET /api/telegram/voice/:id/:mid/:idx/transcript — готовая расшифровка.
 *
 * Расшифровки нет — это `{ready: false}` с пустым текстом, а НЕ ошибка. Ни
 * одного вызова Telegram: ответ читается из локального файла, поэтому работает
 * и при мёртвом соединении.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} [index] всегда 0
 * @returns {Promise<{ready: boolean, text: string, language: string, engine: string, duration_ms: number, created_at: ?number}>}
 */
export async function getVoiceTranscript(dialogId, messageId, index) {
    const idx = Number(index || 0);
    return call(
        `/api/telegram/voice/${encodeURIComponent(dialogId)}/${encodeURIComponent(messageId)}/${idx}/transcript`,
    );
}

/**
 * GET /api/telegram/voice/settings — каким движком расшифровываются голосовые.
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
    return call('/api/telegram/voice/settings');
}

/**
 * PUT /api/telegram/voice/settings — переключить движок расшифровки.
 *
 * Выбор СОХРАНЯЕТСЯ, даже если движок сейчас нерабочий: в ответе тогда придёт
 * `warning`. Отказ приходит только на чужое слово (`400 invalid_input`).
 *
 * @param {string} engine `local` | `cloud`
 * @returns {Promise<{engine: string, cloud_key_set: boolean, warning?: {code: string, message: string}}>}
 */
export async function setVoiceSettings(engine) {
    return call('/api/telegram/voice/settings', jsonInit('PUT', { engine: String(engine || '') }));
}

