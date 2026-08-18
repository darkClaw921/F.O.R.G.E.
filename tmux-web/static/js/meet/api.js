// tmux-web — forge-meet: REST-клиент и ГЕЙТ включённости плагина.
//
// Тонкие обёртки поверх `apiFetch` (core/api.js) через общий разборщик
// конверта `messenger/http-envelope.js` — ровно как в панели VK.
// Authorization подставляет подменённый `window.fetch` из core/auth.js,
// поэтому заголовков здесь нет вовсе (и модуль грузится в голом Node:
// core/auth.js он НЕ импортирует).
//
// ============================================================================
// ПУТИ — ЗАМОРОЖЕННЫЙ КОНТРАКТ
// ============================================================================
//
// `MEET_ROUTES` обязан совпадать ДОСЛОВНО с таблицей в шапке
// `plugins/meet/src/routes/mod.rs`: расхождение даёт молчаливый 404, а не
// ошибку сборки. Сверка идёт В ОБЕ СТОРОНЫ контрактным тестом
// `tmux-web/tests/frontend/meet_contract.test.mjs` — роут без клиента и
// клиент без роута одинаково валят прогон.
//
// Все пути перечислены СРАЗУ, включая те, чьи хендлеры появятся
// в Ф3–Ф8. Так имена не расползутся, когда роуты начнут добавляться по одному:
// список имён — это и есть договор, а не побочный эффект реализации.
//
// ============================================================================
// ГЕЙТ: РОВНО ОДИН GET ЗА ЗАГРУЗКУ СТРАНИЦЫ
// ============================================================================
//
// `initMeetGate()` спрашивает `GET /api/meet/status` и мемоизирует ОБЕЩАНИЕ,
// а не результат: гейт зовут из bootstrap, а в будущем позовут и панели, и
// порядок между ними не определён — без мемоизации промиса два одновременных
// вызова дали бы два запроса.
//
//   200 → реестр платформ уезжает в `configureMeetPlatforms` (источник истины
//         про «что считать ссылкой на встречу» живёт на сервере: разметку
//         браузер держит в кэше PWA неделями), а транспорт батча — в
//         `configureMeetStore`. С этого момента `meetEnabled()` истинна.
//   404 → плагин запущен без `--meet`. `meetEnabled()` НАВСЕГДА false:
//         `configureMeetStore` не звался, значит стор не умеет ходить в сеть,
//         `attachMeetButton` выйдет первой строкой, WS не откроется
//         (`connectMeetWs` сам проверяет гейт), хоткей вернёт false.
//
// 404 — ШТАТНЫЙ ПУТЬ, а не ошибка: так запускается devforge у всех, кто про
// встречи не просил. В консоль по нему не пишется НИЧЕГО — иначе у них при
// каждой загрузке страницы был бы жёлтый warning ни о чём.
//
// Выключенный плагин не должен стоить фронту ничего: один GET, один 404, и на
// этом всё.

import { apiFetch } from '../core/api.js';
import { createCaller, jsonInit } from '../messenger/http-envelope.js';
import { configureMeetPlatforms, resetMeetPlatforms } from '../messenger/meet-core.js';
import { configureMeetStore, meetEnabled, resetMeetStore } from '../messenger/meet-store.js';

/** Единственная точка выхода модуля в сеть. */
const call = createCaller(apiFetch);

// ==========================================================================
// Таблица-контракт
// ==========================================================================

/**
 * Поверхность плагина глазами фронта.
 *
 * `path` — ШАБЛОН в записи бэкенда (`:id`, `:name`), а не собранный адрес:
 * сравнение с таблицей `routes/mod.rs` должно быть текстовым и дословным.
 * `fn` — имя клиентской функции; для `/ws/meet` она живёт в `js/meet/ws.js`,
 * потому что это не HTTP-вызов, а соединение.
 *
 * @type {Array<{method: string, path: string, fn: string, phase: string}>}
 */
export const MEET_ROUTES = [
    { method: 'GET', path: '/api/meet/status', fn: 'getMeetStatus', phase: '2' },
    { method: 'GET', path: '/api/meet/badges', fn: 'fetchBadgeRange', phase: '2' },
    { method: 'GET', path: '/ws/meet', fn: 'connectMeetWs', phase: '2' },
    { method: 'POST', path: '/api/meet/meetings', fn: 'startMeeting', phase: '3' },
    { method: 'POST', path: '/api/meet/meetings/:id/stop', fn: 'stopMeeting', phase: '3' },
    { method: 'POST', path: '/api/meet/meetings/:id/cancel', fn: 'cancelMeeting', phase: '3' },
    { method: 'POST', path: '/api/meet/meetings/:id/confirm', fn: 'confirmMeeting', phase: '3' },
    { method: 'POST', path: '/api/meet/meetings/:id/show-window', fn: 'showMeetingWindow', phase: '3' },
    { method: 'GET', path: '/api/meet/meetings/:id/screenshot', fn: 'fetchMeetingScreenshot', phase: '3' },
    { method: 'GET', path: '/api/meet/meetings/:id', fn: 'getMeeting', phase: '6' },
    { method: 'GET', path: '/api/meet/meetings/:id/transcript', fn: 'getTranscript', phase: '6' },
    { method: 'PATCH', path: '/api/meet/meetings/:id/speakers', fn: 'patchSpeakers', phase: '7' },
    { method: 'GET', path: '/api/meet/meetings/:id/summary', fn: 'getSummary', phase: '6' },
    { method: 'POST', path: '/api/meet/meetings/:id/summary', fn: 'regenerateSummary', phase: '6' },
    { method: 'GET', path: '/api/meet/meetings/:id/audio', fn: 'fetchMeetingAudio', phase: '7' },
    { method: 'POST', path: '/api/meet/meetings/:id/retry', fn: 'retryMeeting', phase: '6' },
    { method: 'GET', path: '/api/meet/models', fn: 'getModels', phase: '5' },
    { method: 'POST', path: '/api/meet/models/:name/download', fn: 'downloadModel', phase: '5' },
    { method: 'DELETE', path: '/api/meet/models/:name', fn: 'deleteModel', phase: '5' },
    { method: 'GET', path: '/api/meet/config', fn: 'getMeetConfig', phase: '7' },
    { method: 'PUT', path: '/api/meet/config', fn: 'putMeetConfig', phase: '7' },
    { method: 'DELETE', path: '/api/meet/profile', fn: 'resetMeetProfile', phase: '8' },
    { method: 'POST', path: '/api/meet/deps/recheck', fn: 'recheckDeps', phase: '5' },
];

/** Кодирование сегмента пути. Идентификатор встречи — UUID, но правило одно. */
function seg(v) {
    return encodeURIComponent(String(v));
}

// ==========================================================================
// Ф2: статус и батч ленты
// ==========================================================================

/**
 * `GET /api/meet/status` — единственный роут, который никогда не падает.
 *
 * 200 отдаётся даже когда нет ни одного внешнего бинаря: именно этим ответом
 * пользователь узнаёт, ЧЕГО не хватает. 404 означает «плагин выключен» — и
 * это единственный способ узнать про выключенный плагин, потому что без флага
 * `--meet` роутов нет вовсе.
 */
export function getMeetStatus() {
    return call('/api/meet/status');
}

/**
 * `GET /api/meet/badges` — батч состояний для ленты.
 *
 * Параметры ДИАПАЗОННЫЕ (`min_id`/`max_id`), а не список id: так задуман роут,
 * и именно поэтому N+1 на ленте физически не возникает.
 *
 * @param {{plugin: string, chat_id: string|number, min_id: number, max_id: number}} q
 */
export function fetchBadgeRange(q) {
    const p = new URLSearchParams({
        plugin: String(q.plugin),
        chat_id: String(q.chat_id),
        min_id: String(q.min_id),
        max_id: String(q.max_id),
    });
    return call(`/api/meet/badges?${p.toString()}`);
}

// ==========================================================================
// Ф3: жизненный цикл записи
// ==========================================================================

/**
 * `POST /api/meet/meetings` — старт записи.
 *
 * На сервер уезжает КАНОНИЧЕСКИЙ АДРЕС (`url`) и непрозрачная тройка-источник.
 * Идентичность комнаты (`url_key`) считает бэкенд, разбирая этот адрес заново;
 * фронт её не вычисляет и не отправляет — см. `messenger/meet-core.js`.
 *
 * @param {{url: string, plugin: string, chat_id: string|number,
 *          message_id: number, mode?: string, title?: string}} payload
 */
export function startMeeting(payload) {
    return call('/api/meet/meetings', jsonInit('POST', payload));
}

/** `POST /api/meet/meetings/:id/stop` — «встреча кончилась, обработай». */
export function stopMeeting(id) {
    return call(`/api/meet/meetings/${seg(id)}/stop`, jsonInit('POST', {}));
}

/** `POST /api/meet/meetings/:id/cancel` — «прерви и выброси». */
export function cancelMeeting(id) {
    return call(`/api/meet/meetings/${seg(id)}/cancel`, jsonInit('POST', {}));
}

/**
 * `POST /api/meet/meetings/:id/confirm` — «я внутри, пиши».
 *
 * Ассистированный вход: капчу, логин и лобби мы не обходим — окно бота
 * показывается человеку, он доводит вход руками и подтверждает этой кнопкой.
 * Ответ такой же, как у `stop`/`cancel`: `{meeting_id, status}`.
 */
export function confirmMeeting(id) {
    return call(`/api/meet/meetings/${seg(id)}/confirm`, jsonInit('POST', {}));
}

/** `POST /api/meet/meetings/:id/show-window` — показать окно бота человеку. */
export function showMeetingWindow(id) {
    return call(`/api/meet/meetings/${seg(id)}/show-window`, jsonInit('POST', {}));
}

/**
 * `GET /api/meet/meetings/:id/screenshot` — «что видит бот», PNG.
 *
 * Через `fetch`+`blob:`, а не через `<img src>`: у тега нет Bearer-токена, и в
 * remote-mode картинка просто не загрузилась бы. Тот же приём, что у медиа в
 * Telegram/VK/Битриксе.
 *
 * @returns {Promise<Blob>}
 */
export async function fetchMeetingScreenshot(id) {
    return fetchBinary(`/api/meet/meetings/${seg(id)}/screenshot`);
}

// ==========================================================================
// Ф5–Ф7: карточка, расшифровка, саммари, звук
// ==========================================================================

/** `GET /api/meet/meetings/:id` — карточка встречи. */
export function getMeeting(id) {
    return call(`/api/meet/meetings/${seg(id)}`);
}

/**
 * `GET /api/meet/meetings/:id/transcript` — расшифровка страницами.
 *
 * Виртуализации в попапе нет (час встречи это 600–1200 реплик, лента рутинно
 * держит 500+ строк), поэтому пагинация здесь — не оптимизация, а способ не
 * положить вкладку на трёхчасовой записи.
 */
export function getTranscript(id, opts) {
    const o = opts || {};
    const p = new URLSearchParams();
    if (Number.isFinite(o.offset)) p.set('offset', String(o.offset));
    if (Number.isFinite(o.limit)) p.set('limit', String(o.limit));
    const qs = p.toString();
    return call(`/api/meet/meetings/${seg(id)}/transcript${qs ? `?${qs}` : ''}`);
}

/** `PATCH /api/meet/meetings/:id/speakers` — ручное переименование участника. */
export function patchSpeakers(id, payload) {
    return call(`/api/meet/meetings/${seg(id)}/speakers`, jsonInit('PATCH', payload));
}

/**
 * `GET /api/meet/meetings/:id/summary` — саммари встречи.
 *
 * Отсутствие текста — НЕ 404: ответ приходит с `status` (`none` / `pending` /
 * `ready` / `failed`) и `text: null`. «Саммари выключено» и «саммари не
 * получилось» карточка показывает по-разному, и различить их можно только по
 * статусу.
 */
export function getSummary(id) {
    return call(`/api/meet/meetings/${seg(id)}/summary`);
}

/**
 * `POST /api/meet/meetings/:id/summary` — пересчитать саммари поверх готовой
 * расшифровки.
 *
 * Ровно то же, что `retryMeeting(id, 'summarizing')`: у операции два имени,
 * потому что пересчёт саммари — самостоятельное действие в карточке, а не
 * частный случай повтора этапа. Кнопка «ПЕРЕСЧИТАТЬ САММАРИ» ходит через
 * `retryMeeting`, этот вызов оставлен как явное имя для внешних клиентов.
 */
export function regenerateSummary(id) {
    return call(`/api/meet/meetings/${seg(id)}/summary`, jsonInit('POST', {}));
}

/**
 * `GET /api/meet/meetings/:id/audio` — архивный микс либо один трек.
 *
 * Тоже через blob: у `<audio>` нет Bearer-токена. Роут при этом отвечает
 * честным Range (206/416/HEAD) — правильное поведение эндпоинта не зависит от
 * того, кто его дёргает.
 *
 * `track` — НЕОБЯЗАТЕЛЬНЫЙ параметр запроса, а не отдельный роут: адрес тот же,
 * меняется только выборка файла на сервере (`raw/track-N.wav` вместо
 * `audio.m4a`). Им пользуется вкладка участников («послушать Участника 2») —
 * без пер-трекового звука ручное именование спикеров было бы тупиком.
 * Реализовано в Ф7; номер дорожки сервер разбирает в целое и собирает путь сам,
 * поэтому обход каталога через этот параметр невозможен.
 *
 * @param {string} id
 * @param {{track?: number}} [opts]
 * @returns {Promise<Blob>}
 */
export async function fetchMeetingAudio(id, opts) {
    const track = opts && Number.isFinite(opts.track) ? Number(opts.track) : null;
    const qs = track === null ? '' : `?track=${encodeURIComponent(String(track))}`;
    return fetchBinary(`/api/meet/meetings/${seg(id)}/audio${qs}`);
}

/** `POST /api/meet/meetings/:id/retry?from=` — повтор этапа. */
export function retryMeeting(id, from) {
    const qs = from ? `?from=${encodeURIComponent(from)}` : '';
    return call(`/api/meet/meetings/${seg(id)}/retry${qs}`, jsonInit('POST', {}));
}

// ==========================================================================
// Ф5/Ф7: модели, настройки, зависимости
// ==========================================================================

/** `GET /api/meet/models` — список моделей whisper. */
export function getModels() {
    return call('/api/meet/models');
}

/**
 * `POST /api/meet/models/:name/download` — скачать модель.
 *
 * ТОЛЬКО по явному действию пользователя: полтора гигабайта не качаются
 * «на всякий случай» при открытии вкладки.
 */
export function downloadModel(name) {
    return call(`/api/meet/models/${seg(name)}/download`, jsonInit('POST', {}));
}

/**
 * `DELETE /api/meet/models/:name` — удалить модель с диска.
 *
 * Полтора гигабайта на диске — заметная величина, и убрать их пользователь
 * должен уметь оттуда же, откуда качал, а не руками из `~/.forge/meet/models`.
 */
export function deleteModel(name) {
    return call(`/api/meet/models/${seg(name)}`, jsonInit('DELETE', {}));
}

/** `GET /api/meet/config` — настройки плагина. */
export function getMeetConfig() {
    return call('/api/meet/config');
}

/** `PUT /api/meet/config` — сохранить настройки. */
export function putMeetConfig(payload) {
    return call('/api/meet/config', jsonInit('PUT', payload));
}

/**
 * `DELETE /api/meet/profile` — сбросить постоянный профиль браузера.
 *
 * В каталоге профиля лежит ЖИВАЯ СЕССИЯ аккаунта, под которым бот входит во
 * встречи, поэтому у сброса отдельный адрес, а не поле в `PUT /config`:
 * удаление чужих учётных данных — действие, и уехать заодно с правкой имени
 * бота оно не должно.
 *
 * 409 `busy` — идёт запись: профиль сейчас занят живым Chrome.
 */
export function resetMeetProfile() {
    return call('/api/meet/profile', { method: 'DELETE' });
}

/** `POST /api/meet/deps/recheck` — перепроверить внешние бинари. */
export function recheckDeps() {
    return call('/api/meet/deps/recheck', jsonInit('POST', {}));
}

/**
 * Общий путь для двоичных ответов (PNG, аудио).
 *
 * Конверт ошибок разбирается тем же `parseErrorEnvelope`, что и у JSON: сервер
 * отвечает ошибкой в JSON даже на роуте, который в успехе отдаёт байты.
 */
async function fetchBinary(path) {
    const res = await apiFetch(path);
    if (!res.ok) {
        let body = null;
        try { body = await res.json(); } catch (_) { /* не наш конверт */ }
        const err = new Error((body && body.error && body.error.message) || `HTTP ${res.status}`);
        err.status = res.status;
        err.code = (body && body.error && body.error.code) || null;
        throw err;
    }
    return res.blob();
}

// ==========================================================================
// Гейт
// ==========================================================================

const gate = {
    /** Мемоизированное ОБЕЩАНИЕ, а не результат: два вызова = один запрос. */
    promise: null,
    /** Тело ответа `/api/meet/status` либо null. */
    status: null,
    /** Сколько раз реально ушёл GET (проверяется тестом). */
    requests: 0,
};

/**
 * Спросить сервер один раз за загрузку страницы и включить всё остальное.
 *
 * Идемпотентен: повторный вызов отдаёт то же обещание и НЕ делает второго
 * запроса.
 *
 * @returns {Promise<?object>} тело `/api/meet/status` при 200, иначе null
 */
export function initMeetGate() {
    if (gate.promise) return gate.promise;
    gate.promise = (async () => {
        let status;
        try {
            gate.requests += 1;
            status = await getMeetStatus();
        } catch (e) {
            // 404 — штатный путь (devforge без `--meet`), молчим совсем.
            // Прочее (500, обрыв сети) — в debug: это диагностика для нас, а
            // не сообщение пользователю, которому кнопка встречи просто не
            // нужна прямо сейчас.
            if (!e || e.status !== 404) console.debug('[meet] status unavailable', e);
            gate.status = null;
            return null;
        }
        if (!status || status.enabled !== true) {
            gate.status = null;
            return null;
        }
        gate.status = status;
        // Реестр платформ — с бэкенда. Мусорный ответ не обнуляет детект:
        // `configureMeetPlatforms` откатится на встроенный набор.
        try {
            configureMeetPlatforms(status.providers);
        } catch (e) {
            console.debug('[meet] provider registry rejected', e);
        }
        // ИНЪЕКЦИЯ ТРАНСПОРТА. Единственное место, где стор узнаёт, как ходить
        // в сеть, — и оно достижимо только по 200.
        configureMeetStore({ fetchStates: fetchBadgeRange });
        return status;
    })();
    return gate.promise;
}

/** Последний ответ `/api/meet/status` (null, пока гейт не прошёл). */
export function meetStatusSnapshot() {
    return gate.status;
}

/** Сколько раз гейт реально сходил на сервер. Только для тестов и e2e. */
export function meetGateRequests() {
    return gate.requests;
}

/**
 * Полный откат гейта — ТОЛЬКО для тестов.
 *
 * В браузере этого пути нет: «плагин выключен» обязано быть решением на всю
 * загрузку страницы, иначе появился бы способ включить кнопки задним числом.
 */
export function resetMeetGate() {
    gate.promise = null;
    gate.status = null;
    gate.requests = 0;
    resetMeetStore();
    resetMeetPlatforms();
}

export { meetEnabled };
