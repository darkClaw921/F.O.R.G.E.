// tmux-web — Telegram plugin: левая колонка #tg-contacts.
//
// Раскладка (дизайн hacker-green):
//
//   // DIRECTORIES        ← ~/inbox, реальные папки аккаунта, ~/archive
//   ~/inbox        (12)   ← amber-счётчик непрочитанного (без muted)
//   💼 ~/работа     (3)   ← папки из GET /api/telegram/folders, порядок Telegram
//   ~/archive
//   // ~/inbox — 12 NODES ← люди и группы АКТИВНОЙ папки
//   sofia@node
//   [ONLINE]         (3)
//   pollbot@node [BOT]    ← бот — помечен, иначе от человека не отличить
//   [OFFLINE]        (1)
//   // CHANNELS           ← каналы отдельным блоком
//   news@chan [M]         ← заглушённый — приглушён и помечен [M]
//   [CHANNEL] 87 msgs
//
// Активная папка живёт в tgState.activeDir ('inbox' | 'archive' |
// 'folder:<id>') и фильтрует оба списка через dialogsForDir (format.js).
// Клик по ~/archive дополнительно тянет свежий срез архива
// (GET /dialogs?folder=archive) и вливает его в общий список (mergeArchive);
// клик по пользовательской папке — расширенную выборку (ensureFullList), без
// неё редкие чаты (типично — боты) не доезжают до фронта вовсе.
//
// XSS: title/username приходят из Telegram и могут содержать что угодно
// (`<img onerror=…>` в имени — рабочий вектор). Поэтому в этом файле НЕТ ни
// одного innerHTML: только createElement + textContent.
//
// Точечные обновления (onOnline/onRead/bumpUnread) правят ОДИН элемент и не
// перерисовывают список: полная перерисовка сбрасывала бы позицию скролла и
// «дёргала» колонку на каждом апдейте присутствия.

import {
    $tgChannelList, $tgDialogList, $tgDirList, $tgNodesTitle, $tgStatusRight,
} from '../core/dom.js';

import { getDialogs, listFolders } from './api.js';
import {
    botMark, dialogStatusLine, dialogsForDir, dirEntries, dirUnread, displayName, displayNick,
    FOLDER_LIST_LIMIT, mergeArchive, mergeDialogs, mutedIds, mutedMark, nodesTitle,
    onlineCounterText, splitDialogs, unreadBadgeText,
} from './format.js';
import { currentVisibilityFilter, tgState } from './state.js';
import { emptyDirKeys, visibleDialogs } from './visibility.js';

const local = {
    /** Колбэк выбора диалога (ставит main.js, чтобы не тянуть chat.js сюда). */
    onSelect: null,
    /** dialog_id → { online: boolean, wasOnline: ?number } */
    presence: new Map(),
    /** dialog_id → корневой <li> для точечных правок. */
    rows: new Map(),
    /** Обработчики секции // DIRECTORIES навешены (делегирование, один раз). */
    dirsBound: false,
    /** Расширенный список под пользовательские папки уже догружен. */
    fullListLoaded: false,
    /** Догрузка расширенного списка в полёте (защита от гонки кликов). */
    fullListPending: false,
};

/**
 * Привязывает обработчик выбора диалога и клики по секции папок.
 *
 * Через колбэк, а не прямым импортом chat.js: иначе dialogs.js ⇄ chat.js
 * образовали бы цикл модулей (chat сообщает о непрочитанном обратно сюда).
 *
 * События папок — делегированием на `#tg-dir-list`: строки пересоздаются при
 * каждом renderDirs, и подписки на самих <li> пришлось бы навешивать заново;
 * заодно работают и статические ~/inbox, ~/archive из index.html до первой
 * загрузки данных.
 *
 * @param {{onSelect?: function(string): void}} opts
 */
export function initDialogs(opts) {
    if (opts && typeof opts.onSelect === 'function') local.onSelect = opts.onSelect;
    if ($tgDirList && !local.dirsBound) {
        local.dirsBound = true;
        $tgDirList.addEventListener('click', (e) => {
            const li = e.target && e.target.closest ? e.target.closest('.tg-dir') : null;
            if (li && li.dataset.dir) selectDir(li.dataset.dir);
        });
        $tgDirList.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter' && e.key !== ' ') return;
            const li = e.target && e.target.closest ? e.target.closest('.tg-dir') : null;
            if (!li || !li.dataset.dir) return;
            e.preventDefault();
            selectDir(li.dataset.dir);
        });
    }
}

/**
 * Загружает список диалогов (и папки аккаунта) и перерисовывает колонку.
 *
 * Папки — best-effort: их ошибка НЕ роняет список диалогов, секция просто
 * покажет прежние папки (или только ~/inbox и ~/archive).
 *
 * @returns {Promise<Array<object>>} загруженные DialogDto
 * @throws {Error} с полями code/status (см. api.js) — вызывающий решает,
 *   показывать ли ошибку: при `not_authorized` её показывать не надо, там
 *   работает wizard логина.
 */
export async function loadDialogs() {
    const resp = await getDialogs();
    tgState.dialogs = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
    // Список пересобран с нуля (логин, resync, реконнект WS) — расширенная
    // выборка вместе с ним потеряна, и следующий вход в папку обязан догрузить
    // её заново, иначе редкие чаты снова пропадут из папок.
    local.fullListLoaded = false;
    try {
        const f = await listFolders();
        tgState.folders = Array.isArray(f && f.folders) ? f.folders : [];
    } catch (_) {
        // not_connected и т.п. — папки подъедут со следующим resync
        // или WS-событием folders_changed.
    }
    renderAll();
    return tgState.dialogs;
}

/**
 * Перечитывает папки аккаунта (WS `folders_changed`) и перерисовывает колонку.
 *
 * Перерисовка ПОЛНАЯ, а не только секция // DIRECTORIES: у активной папки
 * могли измениться правила (include/exclude/флаги), и отфильтрованный список
 * диалогов обязан это отразить.
 *
 * @returns {Promise<Array<object>>} загруженные папки
 */
export async function loadFolders() {
    const resp = await listFolders();
    tgState.folders = Array.isArray(resp && resp.folders) ? resp.folders : [];
    renderAll();
    return tgState.folders;
}

/**
 * Селектор фильтрации для ключа папки ('inbox' | 'archive' | 'folder:<id>').
 *
 * `null` — папка с таким id исчезла (folders_changed её удалил): вызывающий
 * обязан откатиться на inbox, а не показывать пустой список.
 */
function dirSelectorOf(key) {
    if (key === 'archive') return { type: 'archive' };
    if (typeof key === 'string' && key.startsWith('folder:')) {
        const id = Number(key.slice('folder:'.length));
        const folder = (tgState.folders || []).find((f) => f && Number(f.id) === id);
        return folder ? { type: 'folder', folder } : null;
    }
    return { type: 'inbox' };
}

/**
 * Делает папку активной: фильтр списка + подсветка.
 *
 * Для ~/archive дополнительно тянется свежий срез архива: общая выборка
 * могла не захватить его целиком, а кэш непрочитанного по архиву сервер не
 * пересевает. Для пользовательской папки — расширенная выборка (см.
 * [`ensureFullList`]). Ошибка запроса не критична — показываем, что было.
 *
 * @param {string} dirKey
 */
export function selectDir(dirKey) {
    const key = String(dirKey || 'inbox');
    if (tgState.activeDir === key) return;
    tgState.activeDir = key;
    renderAll();
    if (key === 'archive') {
        getDialogs(undefined, 'archive')
            .then((resp) => {
                tgState.dialogs = mergeArchive(tgState.dialogs, resp && resp.dialogs);
                renderAll();
            })
            .catch(() => { /* not_connected — остаётся срез из общей выборки */ });
    } else if (key.startsWith('folder:')) {
        ensureFullList();
    }
}

/**
 * Догружает расширенный список диалогов под пользовательскую папку.
 *
 * Зачем вообще: пользовательские папки (dialogFilter) фильтруются на фронте,
 * из того, что уже загружено. Стартовые 100 диалогов — это верхушка по дате, и
 * чат с ботом, которому не писали полгода, в неё не попадает: папка «Боты»
 * выглядит пустой при живых ботах в аккаунте. Запрос с явным `?folder=all`
 * поднимает выборку до [`FOLDER_LIST_LIMIT`].
 *
 * ЛЕНИВО и ОДИН раз на загрузку списка — не на старте: расширенная выборка
 * стоит нескольких RPC к Telegram, и делать их при каждом открытии вкладки
 * (в т.ч. на каждом реконнекте WS) значило бы менять один дефект на другой —
 * задержку первой отрисовки и риск FLOOD_WAIT.
 *
 * @returns {?Promise<void>} null, если догрузка уже сделана или идёт
 */
function ensureFullList() {
    if (local.fullListLoaded || local.fullListPending) return null;
    local.fullListPending = true;
    return getDialogs(FOLDER_LIST_LIMIT, 'all')
        .then((resp) => {
            local.fullListLoaded = true;
            tgState.dialogs = mergeDialogs(tgState.dialogs, resp && resp.dialogs);
            renderAll();
        })
        .catch(() => {
            // not_connected / flood_wait: остаётся стартовая выборка. Повтор
            // произойдёт при следующем входе в папку — сам не ретраим, чтобы
            // не долбить Telegram по кругу.
        })
        .finally(() => { local.fullListPending = false; });
}

/** Полная перерисовка колонки (списки — по активной папке). */
export function renderAll() {
    local.rows.clear();
    let sel = dirSelectorOf(tgState.activeDir);
    if (!sel) {
        // Активная папка удалена на другом устройстве — откат на inbox.
        tgState.activeDir = 'inbox';
        sel = { type: 'inbox' };
    }
    // Белый список настроек и разбор по папкам — два независимых фильтра, их
    // порядок на результат не влияет (пересечение коммутативно). Белый список
    // стоит первым по смыслу: «этих чатов в devforge нет вовсе», а уже потом
    // оставшееся раскладывается по папкам. Выключенный фильтр отдаёт список
    // как есть.
    const allowed = visibleDialogs(tgState.dialogs, currentVisibilityFilter(), tgState.folders);
    const visible = dialogsForDir(allowed, sel);
    const { nodes, channels } = splitDialogs(visible);
    renderDirs();
    renderList($tgDialogList, nodes, 'tg-dialog');
    renderList($tgChannelList, channels, 'tg-channel');
    if ($tgNodesTitle) $tgNodesTitle.textContent = nodesTitle(nodes.length, activeDirName());
    // Присутствие переживает перерисовку: строки пересобираются из той же
    // карты, поэтому статусы не «мигают» после перезагрузки списка.
    renderOnlineCounter();
    highlightActive();
}

/** Имя активной папки для заголовка секции узлов (`~/работа`). */
function activeDirName() {
    const entry = dirEntries(tgState.folders).find((e) => e.key === tgState.activeDir);
    return entry ? entry.name : '~/inbox';
}

/**
 * Секция // DIRECTORIES: ~/inbox, реальные папки аккаунта в порядке Telegram,
 * ~/archive. Справа у каждого пункта — amber-счётчик непрочитанного по папке
 * (заглушённые диалоги в него не входят, как и в бейдж на кнопке ✈).
 *
 * Список короткий и меняется редко, поэтому пересобирается целиком —
 * в отличие от списков диалогов, позицию скролла тут беречь не от чего.
 * XSS: emoticon и слаг попадают в DOM только через textContent.
 */
export function renderDirs() {
    if (!$tgDirList) return;
    const filter = currentVisibilityFilter();
    // Счётчики обязаны считать по тому же срезу, что видно в колонке: иначе
    // папка показывала бы «(7)» на скрытых чатах, и цифра вела бы в никуда.
    const allowed = visibleDialogs(tgState.dialogs, filter, tgState.folders);
    const muted = mutedIds(allowed);
    const entries = dirEntries(tgState.folders);
    // Папка, из которой при действующем фильтре не видно ничего, из навигации
    // НЕ убирается — прятать структуру аккаунта мы не вправе, — но приглушается,
    // чтобы пустой список внутри не читался как сбой загрузки.
    const empty = emptyDirKeys(entries, tgState.dialogs, filter, tgState.folders);
    $tgDirList.textContent = '';
    for (const entry of entries) {
        const li = document.createElement('li');
        li.className = 'tg-dir' + (entry.key === tgState.activeDir ? ' active' : '')
            + (empty.has(entry.key) ? ' tg-dir-filtered-out' : '');
        li.dataset.dir = entry.key;
        li.tabIndex = 0;
        // Полное имя папки — подсказкой (слаг мог обрезаться/транслитерироваться).
        if (entry.folder && entry.folder.title) li.title = String(entry.folder.title);

        const name = document.createElement('span');
        name.className = 'tg-dir-name';
        name.textContent = (entry.emoticon ? entry.emoticon + ' ' : '') + entry.name;

        const sel = entry.folder ? { type: 'folder', folder: entry.folder } : { type: entry.key };
        const text = unreadBadgeText(dirUnread(allowed, sel, muted));
        const count = document.createElement('span');
        count.className = text ? 'tg-dir-count tg-dialog-unread' : 'tg-dir-count';
        count.textContent = text ? `(${text})` : '';

        li.appendChild(name);
        li.appendChild(count);
        $tgDirList.appendChild(li);
    }
}

/** Рисует один список (узлы или каналы) в переданный <ul>. */
function renderList(root, dialogs, cls) {
    if (!root) return;
    root.textContent = '';
    for (const d of dialogs) {
        const li = buildRow(d, cls);
        root.appendChild(li);
        local.rows.set(String(d.id), li);
    }
}

/**
 * Подсказка строки: `Иван Петров (@sofia)`, без ника — только имя.
 *
 * Смысл подсказки после того, как имя стало видно всегда: показать то, что
 * СТРОКА могла потерять. Имя обрезается ellipsis'ом в узкой колонке, ник при
 * нехватке места ужимается первым — подсказка возвращает оба целиком. Пустая
 * или дублирующая подсказка была бы хуже отсутствующей: пользователь наводит
 * мышь и получает то же, что уже прочитал.
 *
 * @param {object} d — DialogDto
 * @returns {string}
 */
function tooltipText(d) {
    const name = displayName(d);
    const nick = displayNick(d);
    return nick ? `${name} (${nick})` : name;
}

/**
 * Двухстрочный элемент списка.
 *
 * Строка 1 — имя + приглушённый @ник + метки, строка 2 — статус +
 * amber-счётчик непрочитанного. Ссылки на подэлементы кладутся в `_tg` на
 * самом узле: точечные апдейтеры находят их без повторных querySelector'ов по
 * всей колонке.
 */
function buildRow(d, cls) {
    const li = document.createElement('li');
    // Заглушённый диалог приглушается целиком (класс tg-muted) и помечается
    // терминальной меткой [M] — 🔇 в hacker-green дизайне выглядел бы чужим.
    li.className = cls + (d.muted ? ' tg-muted' : '');
    li.dataset.dialogId = String(d.id);
    li.tabIndex = 0;
    // title — тоже textContent-путь (атрибут не парсится как HTML).
    //
    // Раньше подсказка несла ЕДИНСТВЕННОЕ место, где имя было видно целиком:
    // в строке стоял слаг. Теперь имя в строке, и подсказка, повторяющая его
    // слово в слово, была бы шумом — она несёт «Имя (@ник)», то есть ровно то,
    // чего в строке может не хватить: полное имя при обрезке ellipsis'ом плюс
    // ник, который при нехватке места ужимается первым.
    li.title = tooltipText(d);

    const name = document.createElement('span');
    name.className = 'tg-dialog-name';
    const nameText = document.createElement('span');
    nameText.className = 'tg-dialog-title';
    nameText.textContent = displayName(d);
    // @ник — второстепенная деталь: серым и следом за именем. Узла нет вовсе,
    // когда ника нет (у большинства контактов Telegram его и не бывает):
    // пустой span получил бы от CSS отступ и оставил бы в строке дыру.
    const nickText = displayNick(d);
    let nick = null;
    if (nickText) {
        nick = document.createElement('span');
        nick.className = 'tg-dialog-nick';
        nick.textContent = nickText;
    }
    // Бот и человек — оба kind === 'user' и выглядят в колонке одинаково.
    // Метка [BOT] показывает ту же классификацию, по которой папка «Боты»
    // отбирает диалоги (format.js::dialogCategory), — иначе содержимое папки
    // читалось бы как случайное. Отступ даёт текстовый узел, а не CSS: класс
    // tg-dialog-bot появится в telegram.css позже, а метка обязана быть
    // читаемой уже сейчас.
    const bot = document.createElement('span');
    bot.className = 'tg-dialog-bot';
    bot.textContent = botMark(!!d.bot);
    const mark = document.createElement('span');
    mark.className = 'tg-muted-mark';
    mark.textContent = mutedMark(!!d.muted);
    name.appendChild(nameText);
    if (nick) name.appendChild(nick);
    if (d.bot) name.appendChild(document.createTextNode(' '));
    name.appendChild(bot);
    name.appendChild(mark);

    const status = document.createElement('span');
    status.className = 'tg-dialog-status';

    const statusText = document.createElement('span');
    statusText.textContent = dialogStatusLine(d, local.presence.get(String(d.id)));

    const unread = document.createElement('span');
    unread.className = 'tg-dialog-unread';
    setUnreadText(unread, d.unread_count);

    status.appendChild(statusText);
    status.appendChild(document.createTextNode(' '));
    status.appendChild(unread);

    li.appendChild(name);
    li.appendChild(status);
    li._tg = { statusText, unread, mark, bot, dialog: d };

    li.addEventListener('click', () => selectDialog(String(d.id)));
    li.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            selectDialog(String(d.id));
        }
    });
    return li;
}

function setUnreadText(el, count) {
    const text = unreadBadgeText(count);
    el.textContent = text ? `(${text})` : '';
}

/** Диалог по id из загруженного списка. */
export function findDialog(dialogId) {
    const id = String(dialogId);
    return (tgState.dialogs || []).find((d) => String(d.id) === id) || null;
}

/**
 * Делает диалог активным: подсветка, состояние, колбэк открытия.
 *
 * Повторный клик по уже активному диалогу — no-op: он бы перезагружал
 * историю и терял позицию чтения.
 */
export function selectDialog(dialogId) {
    const id = String(dialogId);
    if (tgState.activeDialogId === id) return;
    tgState.activeDialogId = id;
    highlightActive();
    if (typeof local.onSelect === 'function') local.onSelect(id);
}

/** Снимает выделение и активный диалог (после logout/resync). */
export function clearActive() {
    tgState.activeDialogId = null;
    highlightActive();
}

/** Ровно один активный элемент в обеих колонках. */
export function highlightActive() {
    const active = tgState.activeDialogId ? String(tgState.activeDialogId) : null;
    for (const [id, li] of local.rows) {
        li.classList.toggle('active', id === active);
    }
}

/**
 * Обновляет счётчик непрочитанного у одного диалога.
 *
 * @param {string} dialogId
 * @param {number} count
 */
export function setUnread(dialogId, count) {
    const d = findDialog(dialogId);
    if (d) d.unread_count = Math.max(0, Number(count) || 0);
    const li = local.rows.get(String(dialogId));
    if (li && li._tg) setUnreadText(li._tg.unread, d ? d.unread_count : count);
    renderDirs();
}

/**
 * Инкремент непрочитанного (пришло входящее в неактивный диалог).
 *
 * @param {string} dialogId
 * @returns {number} новое значение
 */
export function bumpUnread(dialogId) {
    const d = findDialog(dialogId);
    const next = ((d && Number(d.unread_count)) || 0) + 1;
    setUnread(dialogId, next);
    return next;
}

/**
 * WS `notify_settings` — mute диалога переключили (с любого устройства).
 *
 * Точечно: патчится поле muted в DialogDto, метка [M] и класс строки — без
 * полной перерисовки списка (позиция скролла важнее). Счётчики папок
 * пересчитываются: непрочитанное заглушённого из них выпадает (или
 * возвращается). Пересчёт бейджа на кнопке ✈ — забота вызывающего (main.js),
 * у него живёт Set заглушённых.
 *
 * @param {{dialog_id: string, muted: boolean}} evt
 * @returns {boolean} нашёлся ли диалог в списке
 */
export function onNotifySettings(evt) {
    if (!evt || !evt.dialog_id) return false;
    const id = String(evt.dialog_id);
    const muted = !!evt.muted;
    const d = findDialog(id);
    if (d) d.muted = muted;
    const li = local.rows.get(id);
    if (li && li._tg) {
        li.classList.toggle('tg-muted', muted);
        li._tg.mark.textContent = mutedMark(muted);
    }
    renderDirs();
    return !!d;
}

/**
 * WS `online` — точечное обновление статуса.
 *
 * В личных чатах dialog_id === user_id, поэтому событие адресуется прямо в
 * строку списка. Для групп/каналов online-статус участника в колонке не
 * показывается — там свой статус ([GROUP]/[CHANNEL]).
 *
 * ТОЧЕЧНО — это принципиально: полная перерисовка колонки сбрасывала бы
 * позицию скролла, а статусы присутствия меняются постоянно и без участия
 * пользователя. Меняется ровно один текстовый узел.
 *
 * Событие про диалог, которого нет в списке (контакт без переписки, участник
 * группы), не выбрасывается: присутствие запоминается и всплывёт, когда
 * диалог появится в колонке.
 *
 * @param {{user_id: string, online: boolean, was_online: ?number}} evt
 * @returns {boolean} нашлась ли строка в колонке
 */
export function onOnline(evt) {
    if (!evt || !evt.user_id) return false;
    const id = String(evt.user_id);
    local.presence.set(id, {
        online: !!evt.online,
        wasOnline: Number.isFinite(evt.was_online) ? evt.was_online : null,
    });
    renderOnlineCounter();
    const li = local.rows.get(id);
    const d = findDialog(id);
    if (!li || !li._tg || !d) return false;
    li._tg.statusText.textContent = dialogStatusLine(d, local.presence.get(id));
    return true;
}

/**
 * Известное присутствие диалога (или `null`).
 *
 * Наружу отдаётся копия карты, а не сама карта: шапка сессии
 * (`chat.updateHead`) читает статус, но менять его вправе только этот модуль.
 *
 * @param {string} dialogId
 * @returns {?{online: boolean, wasOnline: ?number}}
 */
export function presenceOf(dialogId) {
    const p = local.presence.get(String(dialogId));
    return p ? { online: p.online, wasOnline: p.wasOnline } : null;
}

/**
 * Счётчик «сколько контактов в сети» в правой ячейке статусбара.
 *
 * Считаются только те, по кому статус ИЗВЕСТЕН: Telegram присылает
 * `UserStatus` не по всем контактам и только при изменении, поэтому «0
 * ONLINE» на свежей вкладке означало бы не «никого нет», а «мы ещё ничего не
 * знаем» (см. `onlineCounterText`).
 */
export function renderOnlineCounter() {
    if (!$tgStatusRight) return '';
    let online = 0;
    let known = 0;
    for (const [id, p] of local.presence) {
        // Присутствие бывает только у людей. Групп и каналов Telegram в
        // UserStatus не присылает, но событие с их id (чужой клиент, будущий
        // тип апдейта) не должно превращаться в «+1 в сети».
        const d = findDialog(id);
        if (d && (d.kind === 'group' || d.kind === 'channel')) continue;
        known += 1;
        if (p && p.online) online += 1;
    }
    const text = onlineCounterText(online, known);
    $tgStatusRight.textContent = text;
    return text;
}

/**
 * Забывает всё присутствие (logout, смена аккаунта).
 *
 * Статусы прошлого аккаунта после входа в другой — прямая дезинформация.
 */
export function clearPresence() {
    local.presence.clear();
    renderOnlineCounter();
}

/**
 * WS `read` — прочитано.
 *
 * `direction: "in"` (прочитали мы, возможно с телефона) обнуляет счётчик.
 * `direction: "out"` (прочитали нас) на колонку не влияет — это про галочки
 * в ленте.
 *
 * @param {{dialog_id: string, max_id: number, direction: string}} evt
 */
export function onRead(evt) {
    if (!evt || evt.direction !== 'in' || !evt.dialog_id) return;
    setUnread(String(evt.dialog_id), 0);
}

/**
 * WS `new_message` — обновить превью/счётчик в колонке.
 *
 * Счётчик поднимается только для чужого сообщения в НЕактивном диалоге:
 * своё исходящее непрочитанным не бывает, а активный диалог тут же
 * помечается прочитанным.
 *
 * @param {object} msg — MessageDto
 * @returns {boolean} поднялся ли счётчик
 */
export function onNewMessage(msg) {
    if (!msg || !msg.dialog_id) return false;
    const id = String(msg.dialog_id);
    const d = findDialog(id);
    if (d) d.last_message = msg;
    const isActive = tgState.activeDialogId === id;
    if (msg.out || isActive) {
        // Статус-строка группы/канала показывает «msgs» из last_message.id —
        // обновляем её, чтобы счётчик не отставал.
        const li = local.rows.get(id);
        if (li && li._tg && d) {
            li._tg.statusText.textContent = dialogStatusLine(d, local.presence.get(id));
        }
        return false;
    }
    bumpUnread(id);
    return true;
}

/** Снимок для e2e/диагностики. */
export function _debugDialogsState() {
    let online = 0;
    for (const p of local.presence.values()) {
        if (p && p.online) online += 1;
    }
    return {
        dialogs: (tgState.dialogs || []).length,
        folders: (tgState.folders || []).length,
        activeDir: tgState.activeDir,
        rows: local.rows.size,
        fullListLoaded: local.fullListLoaded,
        presence: local.presence.size,
        online,
        activeDialogId: tgState.activeDialogId,
    };
}
