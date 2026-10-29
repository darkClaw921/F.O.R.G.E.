// tmux-web — Bitrix24 plugin: WebSocket-клиент `/ws/bitrix`.
//
// Тонкая надстройка над js/messenger/ws-core.js: ядро держит соединение,
// лестницу реконнекта, ping/pong и оценку задержки, а этот модуль знает про
// адрес, про токен и про то, куда класть результат. Копировать ядро ради
// третьей панели нечего — оно уже покрыто Node-тестом
// (tmux-web/tests/frontend/messenger_ws_core.test.mjs).
//
// Соединение ПОСТОЯННОЕ и НЕ привязано к вкладке devforge. Сервер вещает
// события без фильтрации по диалогу (plugins/bitrix/src/ws/mod.rs), потому
// что бейдж непрочитанного на кнопке Ⓑ и тосты обязаны работать, пока
// пользователь сидит в terminal/tasks. Поэтому здесь нет teardown'а при смене
// вкладки — только visibilitychange и beforeunload (см. core/bootstrap.js).
//
// Контракт кадров — plugins/bitrix/src/ws/protocol.rs. Константа
// SERVER_FRAMES ниже обязана совпадать с тестом
// `server_frame_tags_are_frozen` в том же файле.
//
// Соединение ОДНО на все порталы (мультипортальность, Ф5): в каждом кадре
// сервер шлёт `portal_id`, по которому фронт решает, рисовать событие в
// открытой ленте или поднять бейдж соседней вкладки. Служебные кадры сокета
// (`ping`, `portals`) приходят с зарезервированным `_socket`, кадры ещё не
// подключённого слота — с `_vacant`; пустым это поле не бывает никогда.
//
// РЕШЕНИЕ ПРИНИМАЕТСЯ ЗДЕСЬ, А НЕ В main.js (Ф8.4). Маршрутизация — свойство
// транспорта: кадр чужого портала не должен доходить до обработчика ленты
// ВООБЩЕ, иначе «не рисовать» пришлось бы помнить в каждом из двадцати
// обработчиков, и один забытый давал бы чужое сообщение в открытом чате.
// Чужие кадры уходят в один-единственный обработчик `foreign` (main.js);
// чистая функция решения — `frameRoute`, она же и покрывается node-тестом.

import { withWsToken } from '../core/auth.js';
import { createWsClient } from '../messenger/ws-core.js';
import { activePortal } from './api.js';
import { b24State } from './state.js';

/**
 * Зарезервированные значения `portal_id` (protocol.rs).
 *
 * Настоящий идентификатор портала подчёркиваний не содержит по построению
 * (нормализация — plugins/bitrix/src/routes/portal.rs), поэтому коллизии с
 * реальным порталом нет и быть не может.
 */
export const SOCKET_PORTAL = '_socket';
export const VACANT_PORTAL = '_vacant';

/** Лестница реконнекта, мс. Последнее значение повторяется. */
export const WS_BACKOFFS_MS = [1000, 2000, 5000, 10000];

/**
 * Ожидаемый период серверного ping'а, мс — HEARTBEAT_INTERVAL в
 * plugins/bitrix/src/ws/mod.rs. Используется только как эталон для оценки
 * задержки: расхождение с сервером ухудшит точность LAT, но ничего не сломает.
 */
export const PING_INTERVAL_MS = 15000;

/**
 * Полный набор кадров сервера — для сверки с protocol.rs глазами и для
 * диагностики: неизвестный тип не ошибка (сервер может начать присылать новое
 * событие раньше, чем обновится закешированный фронт), но в консоли о нём
 * стоит сказать.
 *
 * Часть кадров платформа не отдаёт никогда (`typing`, `presence`, `read` —
 * см. §6 плана и шапку protocol.rs). Они перечислены здесь ровно потому, что
 * обработчики под них всё равно объявляются: иначе появление такого кадра в
 * будущем потребовало бы правки фронта, а сегодня даёт шум «no handler».
 */
export const SERVER_FRAMES = [
    // `portals` стоит сразу за `login_state`: оба описывают не ленту, а саму
    // панель — состояние подключения и строку вкладок порталов
    // (мультипортальность, Ф5). Обработчики им пишет Ф8, здесь только
    // контракт; порядок побайтно сверяет с protocol.rs
    // bitrix_contract.test.mjs.
    'login_state', 'portals', 'dialogs_patch', 'message_new', 'message_update',
    'message_delete', 'reaction', 'typing', 'read', 'counters', 'presence',
    'task_update', 'upload_progress', 'download_progress',
    // Расшифровка голосовых (ГС-Ф3) — сразу за `download_progress`: она
    // продолжает жизненный цикл вложения, который начинается его скачиванием.
    // Порядок сверяет с protocol.rs bitrix_contract.test.mjs.
    'voice_transcribe_started', 'voice_transcribe_delta',
    'voice_transcribe_done', 'voice_transcribe_error',
    'error',
    // Анализ чата через Claude (Ф12). Четыре кадра стоят ПЕРЕД служебными
    // `resync`/`ping` — там же, где у VK и Telegram: порядок здесь не
    // косметика, его побайтно сверяет с protocol.rs bitrix_contract.test.mjs.
    'analysis_started', 'analysis_delta', 'analysis_done', 'analysis_error',
    'resync', 'ping',
];

const local = {
    /** Клиент ядра (создаётся при первом connect). */
    client: null,
    /** Карта type → обработчик (из buildHandlers в main.js). */
    handlers: null,
};

/** Адрес соединения с токеном в query (WebSocket не умеет заголовки). */
function urlFor() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return withWsToken(`${proto}://${location.host}/ws/bitrix`);
}

/** Зовёт обработчик панели, не давая его исключению порвать соединение. */
function callHandler(name, arg) {
    const h = local.handlers && local.handlers[name];
    if (typeof h !== 'function') return;
    try {
        h(arg);
    } catch (e) {
        console.warn(`[b24-ws] handler ${name} threw`, e);
    }
}

/**
 * Куда адресован кадр — ЧИСТАЯ функция, покрывается node-тестом без сокета.
 *
 * Сокет ОДИН на все порталы, и в каждом кадре сервер шлёт `portal_id`. Решений
 * ровно четыре:
 *
 *   `socket`  — служебный кадр сокета (`ping`, `portals`): портала у него нет
 *               по существу, и он идёт в обработчики как есть;
 *   `active`  — кадр открытого портала: рисуется в ленте и списке;
 *   `foreign` — кадр СОСЕДНЕЙ вкладки: в рендер НЕ идёт, а поднимает её бейдж
 *               и (для нового сообщения) даёт тост с префиксом названия;
 *   `invalid` — кадра без `portal_id` быть не может: сервер собирает конверт
 *               так, что поле обязательно (`every_frame_on_the_wire_carries_a_
 *               portal_id` в protocol.rs). Значит это ошибка протокола, и
 *               рисовать такой кадр в активный портал НЕЛЬЗЯ — он мог прийти
 *               откуда угодно.
 *
 * Пустой `activeId` (первые миллисекунды жизни панели, до ответа
 * `GET /api/bitrix/portals`) означает «портал выбирает сервер»: все кадры
 * считаются своими. Иначе панель молчала бы на старте — ровно тогда, когда
 * показывает первый экран.
 *
 * Слот `_vacant` (портал ещё не подключён) — тоже `active`, если своего
 * портала нет: его `login_state` и есть то, что рисует экран подключения.
 *
 * @param {object} msg — кадр целиком
 * @param {string} activeId — активный портал (пусто — не выбран)
 * @returns {'socket'|'active'|'foreign'|'invalid'}
 */
export function frameRoute(msg, activeId) {
    const pid = (msg && typeof msg.portal_id === 'string') ? msg.portal_id : '';
    if (!pid) return 'invalid';
    if (pid === SOCKET_PORTAL) return 'socket';
    const active = String(activeId || '');
    if (!active) return 'active';
    if (pid === VACANT_PORTAL) return 'active';
    return pid === active ? 'active' : 'foreign';
}

/** Раскладывает кадр по обработчикам панели. */
function onFrame(msg) {
    const route = frameRoute(msg, activePortal());
    if (route === 'invalid') {
        // Не молча: кадр без адреса — расхождение фронта и сервера, и молчание
        // здесь дало бы «панель иногда не обновляется» без единого следа.
        console.warn('[b24-ws] кадр без portal_id — не рендерим', msg && msg.type, msg);
        return;
    }
    if (route === 'foreign') {
        // ЕДИНСТВЕННЫЙ вход кадров соседних вкладок. В ленту и список они не
        // попадают ни при каких условиях: их данные принадлежат ДРУГОМУ
        // порталу, и одинаковый `dialog_id` у двух порталов означает разные
        // чаты.
        callHandler('foreign', msg);
        return;
    }
    if (msg.type === 'ping') {
        // Ответ `pong` уже отправлен ядром; панели интересен лишь факт
        // живого соединения.
        callHandler('ping', msg);
        return;
    }
    if (msg.type === 'resync') {
        // Клиент отстал (broadcast Lagged) либо курсор событий признан
        // устаревшим (Р18) — состояние перечитывается по REST целиком,
        // поэтому у resync отдельный колбэк, а не общий обработчик.
        callHandler('resync', msg);
        return;
    }
    if (typeof (local.handlers && local.handlers[msg.type]) === 'function') {
        callHandler(msg.type, msg);
        return;
    }
    console.debug('[b24-ws] no handler for', msg.type, msg);
}

function ensureClient() {
    if (local.client) return local.client;
    local.client = createWsClient({
        urlFor,
        backoffs: WS_BACKOFFS_MS,
        pingIntervalMs: PING_INTERVAL_MS,
        label: 'b24-ws',
        onFrame,
        onLatency: (lat) => {
            b24State.lat = lat;
            callHandler('lat', lat);
        },
        onOpen: () => {
            b24State.ws = local.client ? local.client.socket() : null;
            callHandler('open');
        },
        onClose: (info) => {
            b24State.ws = null;
            callHandler('close', info);
        },
    });
    return local.client;
}

/**
 * Открыть (или переиспользовать) соединение.
 *
 * Идемпотентен: при живом сокете только обновляет handlers. Это важно, потому
 * что connect зовут из нескольких точек (гейт при загрузке страницы,
 * applyLoginState после успешного подключения, возврат вкладки из фона), и
 * порядок между ними не определён.
 *
 * @param {object} handlers — { [frameType]: fn(msg), open?, close?, resync?, lat?, ping? }
 */
export function connectB24Ws(handlers) {
    local.handlers = handlers || local.handlers || {};
    const client = ensureClient();
    client.connect();
    b24State.ws = client.socket();
}

/**
 * Закрыть соединение без реконнекта.
 *
 * Вызывается на visibilitychange:hidden (скрытую вкладку держать сокетом
 * незачем — бейджа всё равно не видно; заодно сервер уводит опрос портала в
 * idle-каденс, Р4) и на beforeunload.
 */
export function disconnectB24Ws() {
    if (local.client) local.client.disconnect();
    b24State.ws = null;
    b24State.lat = null;
}

/** Открыто ли соединение прямо сейчас. */
export function isB24WsOpen() {
    return !!local.client && local.client.isOpen();
}

/** Снимок состояния для e2e/диагностики. */
export function _debugWsState() {
    const base = local.client ? local.client._debug() : {
        readyState: null,
        backoffStep: 0,
        closedByUs: false,
        reconnectPending: false,
        opened: 0,
    };
    return { ...base, lat: b24State.lat, handlers: Object.keys(local.handlers || {}).length };
}
