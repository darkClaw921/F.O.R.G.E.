// tmux-web / VK — белый список видимых бесед.
//
// Модуль НАМЕРЕННО чистый: ни DOM, ни сети, ни импортов состояния. Зависит
// только от `format.js` (тип peer'а по его id), поэтому целиком покрывается
// node-тестом без фейкового документа.
//
// ── Что делает фича
//
// Пользователь отмечает в настройках типы бесед и/или отдельные беседы. Когда
// режим включён, во вкладке видны ТОЛЬКО они: и в колонке, и в счётчиках
// серверных фильтров, и в бейдже непрочитанного на Ⓥ, и в тостах. Остальные
// беседы продолжают жить в аккаунте — devforge просто перестаёт их показывать.
//
// ── Почему фильтр «действует», а не просто «включён»
//
// Тумблер и непустой выбор — разные вещи. Включённый режим с пустым белым
// списком означал бы пустую панель, и пользователь прочитал бы это как
// поломку, а не как собственную настройку. Поэтому [`filterActive`] требует
// обоих условий, а форма настроек прямо пишет «Ничего не выбрано — пока
// показаны все беседы». Инвариант перенесён из телеграмного близнеца ДОСЛОВНО.
//
// ── Чем словарь настроек отличается от телеграмного (единственное намеренное
//    расхождение)
//
// У Telegram белый список строится по ПАПКАМ аккаунта. Папок у VK нет, а
// четыре «директории» колонки — `all|unread|important|unanswered` — это
// параметр `filter` метода `messages.getConversations`, то есть ДРУГОЙ ЗАПРОС,
// а не свойство беседы. Строить по ним постоянную настройку бессмысленно:
// «показывать только непрочитанные» означало бы, что беседа исчезает из панели
// в тот момент, когда её прочитали. Поэтому здесь белый список — это ТИПЫ
// peer'а (`user`/`chat`/`community`) плюс поимённый выбор.
//
// ── Строгость к неизвестным беседам и УЛУЧШЕНИЕ ОТНОСИТЕЛЬНО TELEGRAM
//
// WS вещает по ВСЕМУ аккаунту, а `/dialogs` отдаёт только верхушку списка.
// Поэтому про беседу из кадра `new_message` может быть неизвестно ничего,
// кроме `peer_id`. У телеграмного близнеца этого хватало только на
// консервативное решение: принадлежность чата папке из одного id не выводится,
// и неизвестный чат при действующем фильтре приходилось СКРЫВАТЬ (иначе фильтр
// протекал бы уведомлениями ровно от тех чатов, которые просили не показывать)
// — ценой того, что чат из отмеченной папки молчал до догрузки страницы.
//
// У VK этой цены нет. `peerKind` вычисляется ИЗ САМОГО `peer_id` по диапазонам
// (`< 0` — сообщество, `< 2e9` — пользователь, `>= 2e9` — беседа), без
// обращения к загруженному списку. Значит при фильтре ПО ТИПУ неизвестная
// беседа решается ТОЧНО, а не консервативно: тип известен всегда. Строгость
// сохраняется ровно там, где информации по-прежнему нет, — в поимённом режиме:
// беседа, не отмеченная вручную и не подходящая ни по одному выбранному типу,
// скрыта.
//
// ── Чего здесь нет
//
// `emptyDirKeys` (приглушение пустых пунктов навигации) НЕ переносится:
// пустоту серверного фильтра клиент не знает, не сделав запрос, а четыре
// запроса ради подсветки — это весь секундный бюджет при лимите 3 rps.

import { peerKind } from './format.js';

/**
 * Типы бесед, из которых собирается белый список, — в порядке показа в форме.
 *
 * Живут здесь, а не в форме настроек: список типов — часть контракта настроек
 * (значения ложатся в `vk_visible_kinds` и сравниваются с [`peerKind`]), и
 * держать его рядом с предикатом дешевле, чем синхронизировать два места.
 */
export const VISIBLE_KINDS = [
    { key: 'user', label: 'Личные сообщения', hint: 'переписка с людьми' },
    { key: 'chat', label: 'Беседы', hint: 'групповые чаты' },
    { key: 'community', label: 'Сообщества', hint: 'паблики и боты' },
];

/**
 * Человеческое название типа (для формы настроек и подписей).
 *
 * @param {string} kind
 * @returns {string} название либо сам ключ, если тип незнаком
 */
export function kindLabel(kind) {
    const found = VISIBLE_KINDS.find((k) => k.key === kind);
    return found ? found.label : String(kind == null ? '' : kind);
}

/**
 * Приводит настройки к рабочему виду фильтра.
 *
 * Принимает объект user settings (или null, если настройки ещё не приехали).
 * Отсутствие полей = фильтр выключен, что совпадает и с backend-дефолтом, и с
 * поведением до фичи.
 *
 * `=== true`, а не `!== false`: дефолт ЭТОЙ настройки — выключено (в отличие
 * от `vk_hotkeys_enabled`, где дефолт включён). Гейты дефолтов расходятся по
 * полям намеренно, и путать их нельзя.
 *
 * @param {?object} settings
 * @returns {{enabled: boolean, kinds: Array<string>, peers: Array<string>,
 *   kindSet: Set<string>, peerSet: Set<string>, active: boolean}}
 */
export function normalizeFilter(settings) {
    const s = (settings && typeof settings === 'object') ? settings : {};
    const kinds = cleanList(s.vk_visible_kinds);
    const peers = cleanList(s.vk_visible_peers);
    const enabled = s.vk_filter_enabled === true;
    return {
        enabled,
        kinds,
        peers,
        kindSet: new Set(kinds),
        peerSet: new Set(peers),
        // Пустой выбор фильтром не считается — см. шапку модуля.
        active: enabled && (kinds.length > 0 || peers.length > 0),
    };
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

/** Действует ли фильтр (включён И что-то выбрано). */
export function filterActive(filter) {
    return !!(filter && filter.active);
}

/**
 * Видна ли беседа.
 *
 * Порядок проверок: поимённый выбор сильнее всего (беседа, отмеченная вручную,
 * видна, даже если её тип не отмечен), затем типы.
 *
 * Тип берётся из [`peerKind`], а НЕ из `DialogDto.kind`, хотя DTO под рукой.
 * Это осознанно: точно тот же вызов делает [`dialogIdVisible`] для беседы, о
 * которой известен один `peer_id`, и два разных источника типа рано или поздно
 * разошлись бы — беседа осталась бы в колонке, но перестала звенеть (или
 * наоборот). Один источник = невозможность такого расхождения.
 *
 * @param {?object} d — DialogDto
 * @param {?object} filter — результат [`normalizeFilter`]
 * @returns {boolean}
 */
export function dialogVisible(d, filter) {
    if (!filterActive(filter)) return true;
    if (!d || typeof d !== 'object') return false;
    const id = String(d.id == null ? '' : d.id);
    if (!id) return false;
    if (filter.peerSet.has(id)) return true;
    return filter.kindSet.has(peerKind(id));
}

/**
 * Видимые беседы (порядок сервера сохраняется).
 *
 * @param {Array<object>} dialogs
 * @param {?object} filter
 * @returns {Array<object>}
 */
export function visibleDialogs(dialogs, filter) {
    const list = Array.isArray(dialogs) ? dialogs : [];
    if (!filterActive(filter)) return list.slice();
    return list.filter((d) => dialogVisible(d, filter));
}

/**
 * Видна ли беседа, о которой известен только `peer_id`.
 *
 * Точка входа для WS-кадров: `new_message` приходит по всему аккаунту, и
 * беседы может не быть в загруженном списке.
 *
 * ЗДЕСЬ И ЖИВЁТ УЛУЧШЕНИЕ ОТНОСИТЕЛЬНО TELEGRAM (см. шапку модуля): тип
 * выводится из самого id, поэтому решение по неизвестной беседе ТОЧНОЕ.
 * Загруженный список (`dialogs`) передаётся только ради симметрии сигнатуры с
 * телеграмным близнецом и как источник DTO — на результат он не влияет,
 * потому что [`dialogVisible`] всё равно спрашивает тип у id.
 *
 * @param {string|number} peerId
 * @param {?object} filter
 * @param {Array<object>} [dialogs] — загруженная страница (`vkState.dialogs`)
 * @returns {boolean}
 */
export function dialogIdVisible(peerId, filter, dialogs) {
    if (!filterActive(filter)) return true;
    const id = String(peerId == null ? '' : peerId);
    // Кадр без адреса скрыт: показать его было бы нечем — открыть такую
    // «беседу» по клику всё равно невозможно.
    if (!id) return false;
    if (filter.peerSet.has(id)) return true;
    if (filter.kindSet.has(peerKind(id))) return true;
    // До сюда доходят только беседы, не подошедшие ни поимённо, ни по типу.
    // Загруженное DTO ничего не добавит (тип у него тот же), но проверка
    // оставлена явной: она документирует, что список УЖЕ учтён.
    const known = (Array.isArray(dialogs) ? dialogs : [])
        .find((d) => d && String(d.id) === id);
    return known ? dialogVisible(known, filter) : false;
}

/**
 * Человекочитаемая сводка выбора — для подписи в настройках.
 *
 * @param {?object} filter
 * @returns {string}
 */
export function filterSummary(filter) {
    if (!filter || !filter.enabled) return 'Показаны все беседы.';
    const k = filter.kinds.length;
    const p = filter.peers.length;
    if (k === 0 && p === 0) {
        return 'Ничего не выбрано — пока показаны все беседы.';
    }
    const parts = [];
    if (k > 0) parts.push(`${k} ${plural(k, 'тип', 'типа', 'типов')}`);
    if (p > 0) parts.push(`${p} ${plural(p, 'беседа', 'беседы', 'бесед')}`);
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
 * Убирает из белого списка бесед записи, которых больше нет в аккаунте.
 *
 * Вызывается ТОЛЬКО из формы настроек и только по свежезагруженному списку по
 * ЯВНОЙ КОМАНДЕ пользователя: чистить на каждом рендере нельзя — `/dialogs`
 * отдаёт верхушку, и выбранная, но давно не поднимавшаяся беседа вычистилась
 * бы сама собой, молча.
 *
 * @param {Array<string>} selected
 * @param {Array<object>} dialogs — загруженный список
 * @returns {Array<string>}
 */
export function pruneMissingPeers(selected, dialogs) {
    const known = new Set((Array.isArray(dialogs) ? dialogs : [])
        .filter(Boolean)
        .map((d) => String(d.id)));
    return cleanList(selected).filter((id) => known.has(id));
}
