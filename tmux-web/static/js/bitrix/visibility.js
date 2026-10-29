// tmux-web — Bitrix24 plugin: белый список видимых диалогов.
//
// Модуль ЧИСТЫЙ: ни DOM, ни сети, ни состояния. Зависит только от
// `format.js::dialogKind`, поэтому целиком покрывается node-тестом
// tmux-web/tests/frontend/bitrix_visibility.test.mjs.
//
// ============================================================================
// ВСЕ ТОЧКИ ПРИМЕНЕНИЯ БЕЛОГО СПИСКА (перечень проверяется в Ф11)
// ============================================================================
//
// Пропуск ЛЮБОЙ из них = протечка скрытого диалога: пользователь просил не
// показывать чат, а тот всё равно звенит тостом или считается в бейдже.
//
//   1. КОЛОНКА СПИСКА          — js/bitrix/dialogs.js::visibleSource()
//                                (и всё, что от неё производно: рендер,
//                                 навигация, выбор активного диалога).
//   2. СЧЁТЧИКИ ДИРЕКТОРИЙ     — js/bitrix/dialogs.js::renderDirs()
//                                (`~/all`, `~/chats`, `~/tasks`, `~/unread`
//                                 считаются ПО ВИДИМЫМ строкам).
//   3. БЕЙДЖ НА КНОПКЕ ВКЛАДКИ — js/bitrix/main.js::recomputeUnread()
//                                При ДЕЙСТВУЮЩЕМ фильтре серверный
//                                `counters.total` игнорируется целиком:
//                                расщепить готовое число портала на видимую и
//                                скрытую части нечем, поэтому считаем по
//                                загруженным видимым строкам.
//   4. ТОСТЫ И НАКОПЛЕНИЕ      — js/bitrix/main.js::maybeToast() (Ф7)
//      НЕПРОЧИТАННОГО            через dialogShown() перед показом
//                                уведомления о новом сообщении.
//
// С Ф7.4 каждая из четырёх спрашивает настройки ТОГО ПОРТАЛА, чей кадр
// обрабатывается (точки 1–3 — активного, точка 4 — портала кадра). Пропуск
// портала так же опасен, как пропуск самой точки: кадры соседних порталов
// идут в тот же сокет, и общий фильтр протекал бы в обе стороны сразу.
//
// ============================================================================
// ЧЕМ БИТРИКС24 ОТЛИЧАЕТСЯ ОТ VK (важное ограничение)
// ============================================================================
//
// У VK тип беседы выводится ИЗ САМОГО peer_id по диапазонам, поэтому кадр о
// неизвестной беседе решается точно. Здесь так НЕЛЬЗЯ: `"chat7"` — это и
// обычный групповой чат, и ЧАТ ЗАДАЧИ, а различает их поле `entity_type`,
// которого в идентификаторе нет. Значит про диалог, отсутствующий в
// загруженном списке, тип неизвестен.
//
// Решение — консервативное, как у Telegram: неизвестный диалог при действующем
// фильтре СКРЫТ. Иначе фильтр протекал бы уведомлениями ровно от тех чатов,
// которые просили не показывать, — то есть не работал бы в единственном
// сценарии, ради которого его включают.
//
// ============================================================================
// «ВКЛЮЧЁН» И «ДЕЙСТВУЕТ» — РАЗНЫЕ ВЕЩИ
// ============================================================================
//
// Тумблер без выбора не считается фильтром: включённый режим с пустым белым
// списком означал бы пустую панель, и пользователь прочитал бы это как
// поломку, а не как собственную настройку. Поэтому `filterActive` требует и
// тумблера, и непустого выбора, а форма настроек прямо пишет «Ничего не
// выбрано — пока показаны все диалоги».
//
// Ключи настроек (хост, tmux-web/src/user_settings.rs, Ф11):
//   bitrix_filter_enabled  — bool, дефолт FALSE, читается `=== true`;
//   bitrix_visible_kinds   — виды диалогов из VISIBLE_KINDS;
//   bitrix_visible_peers   — конкретные dialog_id (строки).
//
// ============================================================================
// БЕЛЫЙ СПИСОК — У КАЖДОГО ПОРТАЛА СВОЙ (мультипортальность, Ф7.4)
// ============================================================================
//
// `bitrix_visible_peers` хранит `DIALOG_ID` КОНКРЕТНОГО портала: `chat7` у
// одного портала и `chat7` у другого — разные чаты. Общий на все порталы
// список означал бы, что галочка, поставленная в рабочем портале, прячет
// случайный чат в личном.
//
// Поэтому настройки живут в карте `bitrix_portal_prefs` (`portal_id` → запись
// из пяти полей: `filter_enabled`, `visible_kinds`, `visible_peers`,
// `voice_scope_kinds`, `voice_scope_peers`), а `normalizeFilter` принимает
// ВТОРЫМ аргументом идентификатор портала. Правило чтения одно на три стороны
// (хост `user_settings.rs`, плагин `forge_voice::prefs`, этот модуль), и
// расходиться им нельзя:
//
//   1. в карте есть запись портала        ⇒ читается она;
//   2. карта ПУСТА                        ⇒ читаются ПЛОСКИЕ ключи (состояние
//                                           до миграции: портал-мигрант обязан
//                                           увидеть свои прежние настройки);
//   3. карта непуста, записи портала нет  ⇒ ДЕФОЛТЫ (фильтра нет).
//
// Портал НЕ ПЕРЕДАН — это случай «плоские ключи», ровно как было до Ф7:
// подставлять вместо него первую попавшуюся запись нельзя, «первая» здесь
// означает случайный портал.
//
// МОДУЛЬ ОСТАЁТСЯ ЧИСТЫМ: portal_id приходит АРГУМЕНТОМ. Читать активный
// портал сам он не имеет права — тогда бы он знал про `api.js`, а тест про
// сеть.

import { dialogKind } from './format.js';

/**
 * Виды диалогов, из которых собирается белый список, — в порядке показа в
 * форме настроек.
 *
 * Их ТРИ, хотя видов диалога у портала семь: `sg`, `openline`, `copilot` и
 * `collab` сведены к «чатам» ([`filterKind`]). Раскладывать настройку по
 * редким видам значило бы требовать от пользователя знания внутренней
 * терминологии портала ради галочек, которые он поставит все сразу.
 */
export const VISIBLE_KINDS = [
    { key: 'user', label: 'Личные диалоги', hint: 'переписка с сотрудниками' },
    { key: 'chat', label: 'Чаты', hint: 'групповые чаты, группы, открытые линии' },
    { key: 'task', label: 'Чаты задач', hint: 'обсуждения задач (директория ~/tasks)' },
];

/** Человеческое название вида — для формы настроек и подписей. */
export function kindLabel(kind) {
    const found = VISIBLE_KINDS.find((k) => k.key === kind);
    return found ? found.label : String(kind == null ? '' : kind);
}

/**
 * Вид диалога В ТЕРМИНАХ БЕЛОГО СПИСКА: `user` | `chat` | `task`.
 *
 * @param {?object|string} d — DialogDto либо вид
 * @returns {string}
 */
export function filterKind(d) {
    const kind = dialogKind(d);
    if (kind === 'user' || kind === 'task') return kind;
    return 'chat';
}

/** Список строк без пустых и дублей, порядок сохраняется. */
function cleanList(raw) {
    if (!Array.isArray(raw)) return [];
    const seen = new Set();
    const out = [];
    for (const item of raw) {
        if (typeof item !== 'string' && typeof item !== 'number') continue;
        const s = String(item).trim();
        if (!s || seen.has(s)) continue;
        seen.add(s);
        out.push(s);
    }
    return out;
}

/** Имя карты пер-портальных настроек в `user_settings.json`. */
export const PORTAL_PREFS_KEY = 'bitrix_portal_prefs';

/** Простой объект (не массив, не null) — или `null`. */
function plainObject(value) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    return value;
}

/**
 * Настройки КОНКРЕТНОГО портала — правило из шапки модуля целиком.
 *
 * Возвращает запись из пяти полей в терминах КАРТЫ (`filter_enabled`,
 * `visible_kinds`, `visible_peers`, `voice_scope_kinds`, `voice_scope_peers`),
 * а не плоских ключей: одна форма на оба источника избавляет вызывающих от
 * ветвления «мигрировали или нет».
 *
 * @param {?object} settings — window.ForgeApp.state.userSettings
 * @param {?string} [portalId] — портал; пусто ⇒ плоские ключи (как до Ф7)
 * @returns {{filter_enabled: boolean, visible_kinds: Array<string>,
 *   visible_peers: Array<string>, voice_scope_kinds: Array<string>,
 *   voice_scope_peers: Array<string>}}
 */
export function portalPrefs(settings, portalId) {
    const s = plainObject(settings) || {};
    const map = plainObject(s[PORTAL_PREFS_KEY]);
    const id = String(portalId == null ? '' : portalId).trim();

    if (map && id) {
        const own = plainObject(map[id]);
        if (own) {
            return {
                filter_enabled: own.filter_enabled === true,
                visible_kinds: cleanList(own.visible_kinds),
                visible_peers: cleanList(own.visible_peers),
                voice_scope_kinds: cleanList(own.voice_scope_kinds),
                voice_scope_peers: cleanList(own.voice_scope_peers),
            };
        }
        // Карта непуста, а нашей записи в ней нет — портал подключён ПОСЛЕ
        // миграции. Дефолты, а не чужие настройки: в чужих лежат DIALOG_ID
        // другого портала, и при консервативном правиле «неизвестный диалог
        // скрыт» этот портал показал бы пустую панель.
        if (Object.keys(map).length > 0) {
            return {
                filter_enabled: false,
                visible_kinds: [],
                visible_peers: [],
                voice_scope_kinds: [],
                voice_scope_peers: [],
            };
        }
    }

    // Карта пуста или портал не передан — ПЛОСКИЕ ключи (состояние до Ф7).
    return {
        filter_enabled: s.bitrix_filter_enabled === true,
        visible_kinds: cleanList(s.bitrix_visible_kinds),
        visible_peers: cleanList(s.bitrix_visible_peers),
        voice_scope_kinds: cleanList(s.bitrix_voice_scope_kinds),
        voice_scope_peers: cleanList(s.bitrix_voice_scope_peers),
    };
}

/**
 * Тело PATCH `/api/user-settings`, меняющее настройки ОДНОГО портала.
 *
 * Здесь, а не в форме настроек: имя карты и форма записи — часть контракта с
 * хостом, и второе место с этими строками разошлось бы с первым молча. Слияние
 * (по ключу портала и по полям записи) делает бэкенд, поэтому в теле лежит
 * ровно то, что человек сейчас правил.
 *
 * @param {string} portalId
 * @param {object} fields — подмножество полей записи портала
 * @returns {object} тело PATCH
 */
export function portalPrefsPatch(portalId, fields) {
    const id = String(portalId == null ? '' : portalId).trim();
    return { [PORTAL_PREFS_KEY]: { [id]: plainObject(fields) || {} } };
}

/**
 * Приводит настройки пользователя к рабочему виду фильтра.
 *
 * Отсутствие полей = фильтр выключен, что совпадает и с дефолтом бэкенда, и с
 * поведением до появления фичи.
 *
 * `=== true`, а НЕ `!== false`: дефолт этой настройки — выключено (в отличие
 * от `bitrix_hotkeys_enabled`, где дефолт включён). Гейты дефолтов расходятся
 * по полям намеренно, и путать их нельзя.
 *
 * @param {?object} settings — window.ForgeApp.state.userSettings
 * @param {?string} [portalId] — портал, чьи настройки нужны (Ф7.4). Пусто ⇒
 *   плоские ключи, ровно как до мультипортальности.
 * @returns {{enabled: boolean, kinds: Array<string>, peers: Array<string>,
 *   kindSet: Set<string>, peerSet: Set<string>, active: boolean}}
 */
export function normalizeFilter(settings, portalId) {
    const prefs = portalPrefs(settings, portalId);
    const kinds = prefs.visible_kinds;
    const peers = prefs.visible_peers;
    const enabled = prefs.filter_enabled;
    return {
        enabled,
        kinds,
        peers,
        kindSet: new Set(kinds),
        peerSet: new Set(peers),
        active: enabled && (kinds.length > 0 || peers.length > 0),
    };
}

/** Действует ли фильтр (включён И что-то выбрано). */
export function filterActive(filter) {
    return !!(filter && filter.active);
}

/**
 * Виден ли диалог, о котором известно DTO.
 *
 * Порядок проверок: поимённый выбор сильнее всего (диалог, отмеченный вручную,
 * виден, даже если его вид не отмечен), затем виды.
 *
 * @param {?object} d — DialogDto
 * @param {?object} filter — результат normalizeFilter
 * @returns {boolean}
 */
export function dialogVisible(d, filter) {
    if (!filterActive(filter)) return true;
    if (!d || typeof d !== 'object') return false;
    const id = String(d.id == null ? '' : d.id);
    if (!id) return false;
    if (filter.peerSet.has(id)) return true;
    return filter.kindSet.has(filterKind(d));
}

/**
 * Видимые диалоги (порядок сохраняется).
 *
 * @param {Array<object>} dialogs
 * @param {?object} filter
 * @returns {Array<object>} НОВЫЙ массив
 */
export function visibleDialogs(dialogs, filter) {
    const list = Array.isArray(dialogs) ? dialogs : [];
    if (!filterActive(filter)) return list.slice();
    return list.filter((d) => dialogVisible(d, filter));
}

/**
 * Виден ли диалог, о котором известен только `dialog_id`.
 *
 * Точка входа для WS-кадров: `message_new` приходит по ВСЕМУ порталу, а
 * `/dialogs` отдаёт только загруженную страницу.
 *
 * ЗДЕСЬ ЖИВЁТ КОНСЕРВАТИВНОЕ РЕШЕНИЕ (см. шапку модуля): если диалога нет в
 * загруженном списке, его вид неизвестен, и при действующем фильтре он
 * СКРЫТ — даже если по виду он мог бы подойти.
 *
 * @param {string|number} dialogId
 * @param {?object} filter
 * @param {Array<object>} [dialogs] — загруженный список (b24State.dialogs)
 * @returns {boolean}
 */
export function dialogIdVisible(dialogId, filter, dialogs) {
    if (!filterActive(filter)) return true;
    const id = String(dialogId == null ? '' : dialogId);
    // Кадр без адреса скрыт: открыть такой «диалог» по клику всё равно нельзя.
    if (!id) return false;
    if (filter.peerSet.has(id)) return true;
    const known = (Array.isArray(dialogs) ? dialogs : [])
        .find((d) => d && String(d.id) === id);
    return known ? dialogVisible(known, filter) : false;
}

/**
 * Сумма непрочитанного по ВИДИМЫМ диалогам.
 *
 * Точка 3 из перечня в шапке: при действующем фильтре серверный `total`
 * непригоден, и бейдж считается по загруженным строкам.
 *
 * @param {Array<object>} dialogs
 * @param {?object} filter
 * @returns {number}
 */
export function visibleUnread(dialogs, filter) {
    return visibleDialogs(dialogs, filter)
        .reduce((sum, d) => sum + (Number(d && d.unread_count) || 0), 0);
}

/**
 * Человекочитаемая сводка выбора — для подписи в настройках.
 *
 * @param {?object} filter
 * @returns {string}
 */
export function filterSummary(filter) {
    if (!filter || !filter.enabled) return 'Показаны все диалоги.';
    const k = filter.kinds.length;
    const p = filter.peers.length;
    if (k === 0 && p === 0) return 'Ничего не выбрано — пока показаны все диалоги.';
    const parts = [];
    if (k > 0) parts.push(`${k} ${plural(k, 'вид', 'вида', 'видов')}`);
    if (p > 0) parts.push(`${p} ${plural(p, 'диалог', 'диалога', 'диалогов')}`);
    return `Показаны только: ${parts.join(' и ')}.`;
}

/** Русское склонение по числу. */
function plural(n, one, few, many) {
    const mod10 = n % 10;
    const mod100 = n % 100;
    if (mod10 === 1 && mod100 !== 11) return one;
    if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return few;
    return many;
}

/**
 * Убирает из белого списка диалоги, которых больше нет.
 *
 * Вызывается ТОЛЬКО из формы настроек и только по ЯВНОЙ команде пользователя:
 * чистить на каждом рендере нельзя — `/dialogs` отдаёт страницу, и выбранный,
 * но давно не поднимавшийся диалог вычистился бы сам собой, молча.
 *
 * @param {Array<string>} selected
 * @param {Array<object>} dialogs
 * @returns {Array<string>}
 */
export function pruneMissingPeers(selected, dialogs) {
    const known = new Set((Array.isArray(dialogs) ? dialogs : [])
        .filter(Boolean)
        .map((d) => String(d.id)));
    return cleanList(selected).filter((id) => known.has(id));
}
