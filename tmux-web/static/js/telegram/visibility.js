// tmux-web / Telegram — белый список видимых чатов и папок.
//
// Модуль НАМЕРЕННО чистый: ни DOM, ни сети, ни импортов состояния. Зависит
// только от `format.js` (принадлежность диалога папке), поэтому целиком
// покрывается node-тестом без фейкового документа.
//
// ── Что делает фича (forge-c2dt)
//
// Пользователь отмечает в настройках папки и/или отдельные чаты. Когда режим
// включён, во вкладке видны ТОЛЬКО они: и в колонке чатов, и в счётчиках папок,
// и в бейдже непрочитанного на ✈, и в тостах. Остальные чаты продолжают жить в
// аккаунте — devforge просто перестаёт их показывать.
//
// ── Почему фильтр «действует», а не просто «включён»
//
// Тумблер и непустой выбор — разные вещи. Включённый режим с пустым белым
// списком означал бы пустую панель, и пользователь прочитал бы это как
// поломку, а не как собственную настройку. Поэтому [`filterActive`] требует
// обоих условий, а UI показывает предупреждение, пока ничего не выбрано.
//
// ── Строгость к неизвестным чатам
//
// WS вещает по ВСЕМУ аккаунту, а `/dialogs` отдаёт только верхушку списка.
// Поэтому про чат из кадра `new_message` может быть неизвестно ничего, кроме
// id. При действующем фильтре такой чат считается СКРЫТЫМ (кроме случая, когда
// он отмечен поимённо): иначе фильтр «протекал» бы уведомлениями ровно от тех
// чатов, которые пользователь просил не показывать. Цена — чат из отмеченной
// папки, не попавший в загруженную страницу, тоже промолчит до её догрузки.

import { dialogInFolder } from './format.js';

/** Ключ пункта «все неархивные» в секции папок. */
export const DIR_INBOX = 'inbox';
/** Ключ пункта «архив». */
export const DIR_ARCHIVE = 'archive';
/** Префикс ключа пользовательской папки: `folder:<id>`. */
export const DIR_FOLDER_PREFIX = 'folder:';

/**
 * Ключ папки для белого списка.
 *
 * @param {?object} folder — DTO папки (`{id, title, ...}`)
 * @returns {?string} `folder:<id>` или null
 */
export function folderKey(folder) {
    if (!folder || typeof folder !== 'object') return null;
    if (folder.id === undefined || folder.id === null || folder.id === '') return null;
    return `${DIR_FOLDER_PREFIX}${folder.id}`;
}

/**
 * Папка по ключу из белого списка.
 *
 * @param {string} key
 * @param {Array<object>} folders — `tgState.folders`
 * @returns {?object} DTO папки; null для `inbox`/`archive` и для исчезнувшей
 */
export function folderByKey(key, folders) {
    if (typeof key !== 'string' || !key.startsWith(DIR_FOLDER_PREFIX)) return null;
    const id = key.slice(DIR_FOLDER_PREFIX.length);
    return (Array.isArray(folders) ? folders : [])
        .find((f) => f && String(f.id) === id) || null;
}

/**
 * Приводит настройки к рабочему виду фильтра.
 *
 * Принимает объект user settings (или null, если настройки ещё не приехали).
 * Отсутствие полей = фильтр выключен, что совпадает с backend-дефолтом и с
 * поведением до фичи.
 *
 * @param {?object} settings
 * @returns {{enabled: boolean, folders: Array<string>, dialogs: Array<string>,
 *   folderSet: Set<string>, dialogSet: Set<string>, active: boolean}}
 */
export function normalizeFilter(settings) {
    const s = (settings && typeof settings === 'object') ? settings : {};
    const folders = cleanList(s.telegram_visible_folders);
    const dialogs = cleanList(s.telegram_visible_dialogs);
    const enabled = s.telegram_filter_enabled === true;
    return {
        enabled,
        folders,
        dialogs,
        folderSet: new Set(folders),
        dialogSet: new Set(dialogs),
        // Пустой выбор фильтром не считается — см. шапку модуля.
        active: enabled && (folders.length > 0 || dialogs.length > 0),
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
 * Виден ли диалог.
 *
 * Порядок проверок: поимённый выбор сильнее всего (чат, отмеченный вручную,
 * виден, даже если его папка не отмечена), затем папки. `inbox` в белом списке
 * означает «все неархивные», `archive` — «весь архив».
 *
 * @param {?object} d — DialogDto
 * @param {?object} filter — результат [`normalizeFilter`]
 * @param {Array<object>} [folders] — `tgState.folders`
 * @returns {boolean}
 */
export function dialogVisible(d, filter, folders) {
    if (!filterActive(filter)) return true;
    if (!d || typeof d !== 'object') return false;
    if (filter.dialogSet.has(String(d.id))) return true;
    for (const key of filter.folderSet) {
        if (key === DIR_INBOX) {
            if (!d.archived) return true;
            continue;
        }
        if (key === DIR_ARCHIVE) {
            if (d.archived) return true;
            continue;
        }
        const folder = folderByKey(key, folders);
        // Папка удалена на другом устройстве (или ещё не загружена) — просто
        // не даёт совпадений. Выбрасывать её из настроек здесь нельзя: она
        // может вернуться со следующим `folders_changed`.
        if (folder && dialogInFolder(d, folder)) return true;
    }
    return false;
}

/**
 * Видимые диалоги (порядок сервера сохраняется).
 *
 * @param {Array<object>} dialogs
 * @param {?object} filter
 * @param {Array<object>} [folders]
 * @returns {Array<object>}
 */
export function visibleDialogs(dialogs, filter, folders) {
    const list = Array.isArray(dialogs) ? dialogs : [];
    if (!filterActive(filter)) return list.slice();
    return list.filter((d) => dialogVisible(d, filter, folders));
}

/**
 * Виден ли чат, о котором известен только id.
 *
 * Точка входа для WS-кадров: `new_message` приходит по всему аккаунту, и
 * диалога может не быть в загруженном списке. Если он там есть — решение
 * принимается по полному DTO, иначе действует строгое правило из шапки модуля:
 * неизвестный чат при действующем фильтре скрыт, если не отмечен поимённо.
 *
 * @param {string|number} dialogId
 * @param {?object} filter
 * @param {Array<object>} [dialogs] — загруженная страница (`tgState.dialogs`)
 * @param {Array<object>} [folders]
 * @returns {boolean}
 */
export function dialogIdVisible(dialogId, filter, dialogs, folders) {
    if (!filterActive(filter)) return true;
    const id = String(dialogId == null ? '' : dialogId);
    if (!id) return false;
    if (filter.dialogSet.has(id)) return true;
    const known = (Array.isArray(dialogs) ? dialogs : [])
        .find((d) => d && String(d.id) === id);
    if (known) return dialogVisible(known, filter, folders);
    return false;
}

/**
 * Ключи папок, которые имеет смысл показывать в секции // DIRECTORIES.
 *
 * Папка, из которой при действующем фильтре не видно ни одного чата, остаётся
 * в списке: скрывать пункт навигации из-за того, что он сейчас пуст, — значит
 * прятать от пользователя структуру его же аккаунта. Функция нужна для другого:
 * пометить такие пункты, чтобы UI мог их приглушить.
 *
 * @param {Array<object>} entries — результат `format.js::dirEntries`
 * @param {Array<object>} dialogs
 * @param {?object} filter
 * @param {Array<object>} [folders]
 * @returns {Set<string>} ключи пунктов, где нет ни одного видимого чата
 */
export function emptyDirKeys(entries, dialogs, filter, folders) {
    const out = new Set();
    if (!filterActive(filter)) return out;
    const list = Array.isArray(entries) ? entries : [];
    const visible = visibleDialogs(dialogs, filter, folders);
    for (const entry of list) {
        if (!entry || !entry.key) continue;
        const has = visible.some((d) => {
            if (entry.key === DIR_ARCHIVE) return !!d.archived;
            if (entry.folder) return dialogInFolder(d, entry.folder);
            return !d.archived;
        });
        if (!has) out.add(entry.key);
    }
    return out;
}

/**
 * Человекочитаемая сводка выбора — для подписи в настройках.
 *
 * @param {?object} filter
 * @returns {string}
 */
export function filterSummary(filter) {
    if (!filter || !filter.enabled) return 'Показаны все чаты.';
    const f = filter.folders.length;
    const d = filter.dialogs.length;
    if (f === 0 && d === 0) {
        return 'Ничего не выбрано — пока показаны все чаты.';
    }
    const parts = [];
    if (f > 0) parts.push(`${f} ${plural(f, 'папка', 'папки', 'папок')}`);
    if (d > 0) parts.push(`${d} ${plural(d, 'чат', 'чата', 'чатов')}`);
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
 * Убирает из белого списка чатов записи, которых больше нет в аккаунте.
 *
 * Вызывается ТОЛЬКО из формы настроек и только по свежезагруженному полному
 * списку: чистить на каждом рендере нельзя — страница `/dialogs` отдаёт
 * верхушку, и выбранный, но давно не поднимавшийся чат вычистился бы сам собой.
 *
 * @param {Array<string>} selected
 * @param {Array<object>} dialogs — полный загруженный список
 * @returns {Array<string>}
 */
export function pruneMissingDialogs(selected, dialogs) {
    const known = new Set((Array.isArray(dialogs) ? dialogs : [])
        .filter(Boolean)
        .map((d) => String(d.id)));
    return cleanList(selected).filter((id) => known.has(id));
}
