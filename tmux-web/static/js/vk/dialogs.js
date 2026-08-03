// tmux-web — VK plugin: левая колонка #vk-contacts.
//
// Раскладка (дизайн hacker-green, тот же, что у Telegram):
//
//   // FILTERS             ← четыре СЕРВЕРНЫХ фильтра VK
//   ~/all           (12)   ← amber-счётчик непрочитанного (без заглушённых)
//   ~/unread        (12)
//   ~/important            ← счётчика нет: он считался бы по случайной выборке
//   ~/unanswered
//   // ~/all — 12 NODES    ← люди и беседы
//   durov@node
//   [ONLINE]         (3)
//   project_x@chat
//   [CHAT] 4 members (1)
//   // COMMUNITIES         ← сообщества (peer_id < 0) отдельным блоком
//   test_club@club [BOT] [M]
//   [CLUB]
//
// ============================================================================
// ГЛАВНОЕ ОТЛИЧИЕ ОТ js/telegram/dialogs.js: «ДИРЕКТОРИЯ» — ЭТО ЗАПРОС
// ============================================================================
//
// У Telegram папки фильтруются НА ФРОНТЕ из одного загруженного списка. У VK
// `all|unread|important|unanswered` — это параметр `filter` метода
// `messages.getConversations`, то есть ДРУГОЙ ЗАПРОС. Отсюда три следствия,
// и каждое видно в коде:
//
//   1. переключение фильтра идёт с дебаунсом [`DIR_DEBOUNCE_MS`] и через общий
//      планировщик (лимит 3 rps), а не мгновенной фильтрацией массива;
//   2. СПИСКОВ ДВА. `vkState.dialogs` — всегда `filter=all`, это источник
//      бейджа и счётчиков; `vkState.dirDialogs` — транзиентная выборка
//      выбранного фильтра. Один список означал бы, что бейдж считает по
//      случайной странице: зашёл в «Важные» — и «непрочитано 2» вместо 47;
//   3. `emptyDirKeys` (приглушение пустых пунктов) НЕ ПЕРЕНОСИТСЯ: пустоту
//      серверного фильтра клиент не знает, не сделав запрос, а делать четыре
//      запроса ради подсветки — это весь секундный бюджет.
//
// БЕЛЫЙ СПИСОК ВИДИМОСТИ (visibility.js) применяется в ЭТОМ файле ДВАЖДЫ:
// в `visibleSource` (колонка) и в `renderDirs` (счётчики фильтров). Ещё две
// точки живут в main.js — бейдж и тосты. Пропуск любой из четырёх = протечка:
// скрытая беседа продолжила бы всплывать тостом и накручивать цифру на Ⓥ,
// ведущую в беседу, которой в списке нет.
//
// ИНВАРИАНТ ГРАФА МОДУЛЕЙ: этот файл НЕ ИМПОРТИРУЕТ chat.js. Выбор беседы
// уходит колбэком `onSelect` из `initDialogs({onSelect})`, который ставит
// main.js — единственная точка сборки.
//
// XSS: title/username приходят из VK и могут содержать что угодно
// (`<img onerror=…>` в имени — рабочий вектор). Поэтому здесь НЕТ ни одного
// innerHTML: только createElement + textContent.
//
// Точечные обновления (onOnline/onRead/bumpUnread) правят ОДИН элемент и не
// перерисовывают список: полная перерисовка сбрасывала бы позицию скролла и
// «дёргала» колонку на каждом апдейте присутствия.

import {
    $vkCommunityList, $vkDialogList, $vkDirList, $vkNodesTitle, $vkStatusRight,
} from '../core/dom.js';

import * as vkApi from './api.js';
import {
    botMark, dialogStatusLine, dirEntries, dirShowsCount, displayName, displayNick, mergeDialogs,
    mutedIds, mutedMark, nodesTitle, onlineCounterText, splitDialogs, unreadBadgeText, unreadTotal,
} from './format.js';
import {
    createGeneration, PRIORITY_BG, PRIORITY_USER, vkCoalescer, vkLimiter,
} from './ratelimit.js';
import { currentVisibilityFilter, vkState } from './state.js';
import { visibleDialogs } from './visibility.js';

/**
 * Пауза перед запросом при переключении фильтра, мс.
 *
 * Пользователь щёлкает по колонке стрелками и мышью быстрее, чем летит запрос;
 * без дебаунса четыре нажатия — это четыре запроса, то есть больше секунды
 * бюджета, потраченной на выборки, которых он уже не увидит.
 */
export const DIR_DEBOUNCE_MS = 300;

/** Сколько бесед просим у сервера за раз. */
export const DIALOG_PAGE = 40;

const local = {
    /** Колбэк выбора беседы (ставит main.js, чтобы не тянуть chat.js сюда). */
    onSelect: null,
    /** peer_id → { online: boolean, wasOnline: ?number } */
    presence: new Map(),
    /** peer_id → корневой <li> для точечных правок. */
    rows: new Map(),
    /** Обработчики секции // FILTERS навешены (делегирование, один раз). */
    dirsBound: false,
    /** Список фильтров с сервера (GET /api/vk/filters). */
    filters: [],
    /** Таймер дебаунса переключения фильтра. */
    dirTimer: null,
    /** Поколение выборки фильтра: ответ покинутого фильтра выбрасывается. */
    dirGen: createGeneration(),
    /** Беседы, по которым пришёл `dialog_changed` и ждёт перечитывания. */
    changedPeers: new Set(),
    /** Таймер дебаунса перечитывания после `dialog_changed`. */
    changedTimer: null,
};

/**
 * Привязывает обработчик выбора беседы и клики по секции фильтров.
 *
 * События фильтров — делегированием на `#vk-dir-list`: строки пересоздаются при
 * каждом renderDirs, и подписки на самих <li> пришлось бы навешивать заново.
 * Флаг `dirsBound` делает функцию идемпотентной — tabs.js зовёт инициализацию
 * на каждом входе на вкладку, и второй слушатель дал бы двойной запрос.
 *
 * @param {{onSelect?: function(string): void}} opts
 */
export function initDialogs(opts) {
    if (opts && typeof opts.onSelect === 'function') local.onSelect = opts.onSelect;
    if ($vkDirList && !local.dirsBound) {
        local.dirsBound = true;
        $vkDirList.addEventListener('click', (e) => {
            const li = e.target && e.target.closest ? e.target.closest('.vk-dir') : null;
            if (li && li.dataset.dir) selectDir(li.dataset.dir);
        });
        $vkDirList.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter' && e.key !== ' ') return;
            const li = e.target && e.target.closest ? e.target.closest('.vk-dir') : null;
            if (!li || !li.dataset.dir) return;
            e.preventDefault();
            selectDir(li.dataset.dir);
        });
    }
}

/**
 * Загружает список бесед (ВСЕГДА `filter=all`) и перерисовывает колонку.
 *
 * Именно `all`: этот список — источник бейджа и счётчиков, и подмена его
 * выборкой фильтра сделала бы бейдж зависящим от того, куда пользователь ткнул.
 *
 * Фильтры — best-effort: их ошибка НЕ роняет список, секция покажет встроенный
 * набор из четырёх пунктов (он всё равно константен, см. format.js::dirEntries).
 *
 * @returns {Promise<Array<object>>} загруженные DialogDto
 * @throws {Error} с полями code/status — вызывающий решает, показывать ли
 *   ошибку: при `not_authorized` работает экран логина.
 */
export async function loadDialogs() {
    await vkLimiter.acquire(PRIORITY_USER);
    const resp = await vkCoalescer.submit('dialogs:all', (signal) => vkApi.getDialogs({
        limit: DIALOG_PAGE,
        filter: 'all',
        signal,
    }));
    vkState.dialogs = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
    void loadFilters();
    renderAll();
    return vkState.dialogs;
}

/**
 * Перечитывает состав серверных фильтров.
 *
 * Ответ КОНСТАНТЕН (роут не ходит в VK), поэтому запрос делается один раз за
 * загрузку и с фоновым приоритетом: он не должен задерживать список бесед.
 * Кадра `folders_changed` у VK нет и быть не может — состав фильтров не
 * меняется.
 *
 * @returns {Promise<Array<object>>}
 */
export async function loadFilters() {
    try {
        await vkLimiter.acquire(PRIORITY_BG);
        const resp = await vkApi.getFilters();
        const list = Array.isArray(resp && resp.filters) ? resp.filters : [];
        if (list.length) {
            local.filters = list;
            renderDirs();
        }
        return list;
    } catch (_) {
        // not_connected / not_authorized: остаётся встроенный список из
        // четырёх пунктов — он совпадает с серверным по построению.
        return local.filters;
    }
}

/**
 * Делает фильтр активным.
 *
 * Подсветка переезжает СРАЗУ (это реакция на клик), а запрос уходит с
 * дебаунсом: пользователь, прощёлкивающий колонку, не должен оплачивать
 * каждый промежуточный пункт запросом из бюджета 3 rps.
 *
 * @param {string} dirKey — 'all' | 'unread' | 'important' | 'unanswered'
 */
export function selectDir(dirKey) {
    const key = String(dirKey || 'all');
    if (vkState.activeDir === key) return;
    vkState.activeDir = key;
    // Транзиентная выборка принадлежит ПРЕДЫДУЩЕМУ фильтру: показывать её под
    // новым заголовком — значит врать про содержимое.
    vkState.dirDialogs = [];
    // Поколение растёт ДО запроса: ответ покинутого фильтра будет отброшен,
    // даже если отмена не успела сработать.
    local.dirGen.bump();
    renderAll();

    if (local.dirTimer) {
        clearTimeout(local.dirTimer);
        local.dirTimer = null;
    }
    if (key === 'all') return;
    if (typeof setTimeout !== 'function') {
        void loadDirDialogs(key);
        return;
    }
    local.dirTimer = setTimeout(() => {
        local.dirTimer = null;
        void loadDirDialogs(key);
    }, DIR_DEBOUNCE_MS);
}

/**
 * Тянет выборку серверного фильтра в `vkState.dirDialogs`.
 *
 * Через коалесcер: два быстрых переключения дают ОДИН живой запрос, предыдущий
 * отменяется. Через поколение: ответ, доехавший после ухода на другой фильтр,
 * выбрасывается — иначе колонка показала бы «Важные» под заголовком
 * «Непрочитанные».
 *
 * @param {string} key
 * @returns {Promise<Array<object>>}
 */
export async function loadDirDialogs(key) {
    const gen = local.dirGen.current();
    try {
        await vkLimiter.acquire(PRIORITY_USER);
        if (local.dirGen.stale(gen)) return [];
        const resp = await vkCoalescer.submit('dialogs:dir', (signal) => vkApi.getDialogs({
            limit: DIALOG_PAGE,
            filter: key,
            signal,
        }));
        if (local.dirGen.stale(gen)) return [];
        vkState.dirDialogs = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
        renderAll();
        return vkState.dirDialogs;
    } catch (e) {
        // Отмена — наше собственное решение, а не сбой: молчим.
        if (e && (e.name === 'AbortError' || local.dirGen.stale(gen))) return [];
        // Остальное показывать в колонке негде; ошибку увидит лента/экран
        // логина. Пустая выборка честнее полустарой.
        return [];
    }
}

/**
 * Вливает догруженную страницу бесед в список `filter=all`.
 *
 * @param {Array<object>} fresh
 */
export function mergeDialogPage(fresh) {
    vkState.dialogs = mergeDialogs(vkState.dialogs, fresh);
    renderAll();
}

/**
 * Беседы, которые сейчас показывает колонка.
 *
 * Два независимых сита, и порядок между ними на результат не влияет:
 * серверный фильтр решает, ИЗ КАКОГО списка берём (`all` — постоянный,
 * остальные — транзиентная выборка запроса), а белый список настроек —
 * какие беседы devforge вообще показывает. Белый список применяется здесь, в
 * одной точке на обе ветки: иначе беседа, скрытая в `~/all`, всплывала бы в
 * «Важных».
 */
function visibleSource() {
    const source = vkState.activeDir === 'all' ? vkState.dialogs : vkState.dirDialogs;
    return visibleDialogs(source, currentVisibilityFilter());
}

/** Полная перерисовка колонки. */
export function renderAll() {
    local.rows.clear();
    const { nodes, communities } = splitDialogs(visibleSource());
    renderDirs();
    renderList($vkDialogList, nodes, 'vk-dialog');
    renderList($vkCommunityList, communities, 'vk-community');
    if ($vkNodesTitle) $vkNodesTitle.textContent = nodesTitle(nodes.length, activeDirName());
    // Присутствие переживает перерисовку: строки пересобираются из той же
    // карты, поэтому статусы не «мигают» после перезагрузки списка.
    renderOnlineCounter();
    highlightActive();
}

/** Имя активного фильтра для заголовка секции узлов (`~/unread`). */
function activeDirName() {
    const entry = dirEntries(local.filters).find((e) => e.key === vkState.activeDir);
    return entry ? entry.name : '~/all';
}

/**
 * Секция // FILTERS: четыре серверных фильтра.
 *
 * Счётчик показывается только у `~/all` и `~/unread` (см.
 * format.js::dirShowsCount) и ВСЕГДА считается по `vkState.dialogs`, то есть по
 * выборке `filter=all`. Считать его по транзиентному списку означало бы, что
 * число меняется от того, куда пользователь ткнул.
 *
 * Белый список применяется и здесь — это ВТОРАЯ из четырёх точек. Счётчик
 * обязан считать по тому же срезу, что видно в колонке: иначе `~/all` показал
 * бы «(7)» на скрытых беседах, и цифра вела бы в никуда.
 *
 * Приглушения пустых пунктов (телеграмный `emptyDirKeys`) здесь нет и быть не
 * может: `unread`/`important`/`unanswered` считаются на СЕРВЕРЕ, и их пустоту
 * клиент не знает, не сделав запрос.
 *
 * Список короткий и константный, поэтому пересобирается целиком — беречь
 * позицию скролла тут не от чего.
 */
export function renderDirs() {
    if (!$vkDirList) return;
    const all = visibleDialogs(vkState.dialogs, currentVisibilityFilter());
    const muted = mutedIds(all);
    const total = unreadTotal(all, muted);
    $vkDirList.textContent = '';
    for (const entry of dirEntries(local.filters)) {
        const li = document.createElement('li');
        li.className = 'vk-dir' + (entry.key === vkState.activeDir ? ' active' : '');
        li.dataset.dir = entry.key;
        li.tabIndex = 0;
        // Русское название фильтра — подсказкой: в колонке стоит слаг.
        li.title = entry.title;

        const name = document.createElement('span');
        name.className = 'vk-dir-name';
        name.textContent = entry.name;

        const text = dirShowsCount(entry.key) ? unreadBadgeText(total) : '';
        const count = document.createElement('span');
        count.className = text ? 'vk-dir-count vk-dialog-unread' : 'vk-dir-count';
        count.textContent = text ? `(${text})` : '';

        li.appendChild(name);
        li.appendChild(count);
        $vkDirList.appendChild(li);
    }
}

/** Рисует один список (узлы или сообщества) в переданный <ul>. */
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
 * Подсказка строки: `Павел Дуров (@durov)`, без ника — только имя.
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
 * amber-счётчик непрочитанного. Ссылки на подэлементы кладутся в `_vk` на
 * самом узле: точечные апдейтеры находят их без повторных querySelector'ов по
 * колонке.
 */
function buildRow(d, cls) {
    const li = document.createElement('li');
    // Заглушённая беседа приглушается целиком и помечается терминальной [M] —
    // 🔇 в hacker-green дизайне выглядел бы чужим.
    li.className = cls + (d.muted ? ' vk-muted' : '');
    li.dataset.dialogId = String(d.id);
    li.tabIndex = 0;
    // title — тоже textContent-путь (атрибут не парсится как HTML).
    //
    // Раньше подсказка несла ЕДИНСТВЕННОЕ место, где имя было видно целиком:
    // в строке стоял слаг. Теперь имя в строке, и подсказка несёт «Имя (@ник)»
    // — ровно то, чего в строке может не хватить.
    li.title = tooltipText(d);

    const name = document.createElement('span');
    name.className = 'vk-dialog-name';
    const nameText = document.createElement('span');
    nameText.className = 'vk-dialog-title';
    nameText.textContent = displayName(d);
    // @ник (screen_name) — второстепенная деталь: серым и следом за именем.
    // Узла нет вовсе, когда ника нет (у обычных бесед его не бывает): пустой
    // span получил бы от CSS отступ и оставил бы в строке дыру.
    const nickText = displayNick(d);
    let nick = null;
    if (nickText) {
        nick = document.createElement('span');
        nick.className = 'vk-dialog-nick';
        nick.textContent = nickText;
    }
    // У VK нет флага «бот»: сообщество и есть бот с точки зрения UI, и метка
    // показывает ту же классификацию, по которой строка попала в секцию
    // // COMMUNITIES.
    const bot = document.createElement('span');
    bot.className = 'vk-dialog-bot';
    bot.textContent = botMark(!!d.bot);
    const mark = document.createElement('span');
    mark.className = 'vk-muted-mark';
    mark.textContent = mutedMark(!!d.muted);
    name.appendChild(nameText);
    if (nick) name.appendChild(nick);
    if (d.bot) name.appendChild(document.createTextNode(' '));
    name.appendChild(bot);
    name.appendChild(mark);

    const status = document.createElement('span');
    status.className = 'vk-dialog-status';

    const statusText = document.createElement('span');
    statusText.textContent = dialogStatusLine(d, local.presence.get(String(d.id)));

    const unread = document.createElement('span');
    unread.className = 'vk-dialog-unread';
    setUnreadText(unread, d.unread_count);

    status.appendChild(statusText);
    status.appendChild(document.createTextNode(' '));
    status.appendChild(unread);

    li.appendChild(name);
    li.appendChild(status);
    li._vk = { statusText, unread, mark, bot, dialog: d };

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

/**
 * Беседа по id из ЛЮБОГО загруженного списка.
 *
 * Сначала `filter=all` (он полнее), затем транзиентная выборка: беседа,
 * найденная только фильтром «Важные», обязана открываться так же.
 */
export function findDialog(dialogId) {
    const id = String(dialogId);
    const pick = (list) => (list || []).find((d) => String(d.id) === id) || null;
    return pick(vkState.dialogs) || pick(vkState.dirDialogs);
}

/**
 * Делает беседу активной: подсветка, состояние, колбэк открытия.
 *
 * Повторный клик по уже активной беседе — no-op: он бы перезагружал историю и
 * терял позицию чтения.
 */
export function selectDialog(dialogId) {
    const id = String(dialogId);
    if (vkState.activeDialogId === id) return;
    vkState.activeDialogId = id;
    highlightActive();
    if (typeof local.onSelect === 'function') local.onSelect(id);
}

/** Снимает выделение и активную беседу (после logout/resync). */
export function clearActive() {
    vkState.activeDialogId = null;
    highlightActive();
}

/** Ровно один активный элемент в обеих колонках. */
export function highlightActive() {
    const active = vkState.activeDialogId ? String(vkState.activeDialogId) : null;
    for (const [id, li] of local.rows) {
        li.classList.toggle('active', id === active);
    }
}

/**
 * Обновляет счётчик непрочитанного у одной беседы.
 *
 * Патчится ОБА списка: беседа могла попасть в транзиентную выборку, и
 * расхождение между ними выглядело бы как «счётчик не обновился».
 */
export function setUnread(dialogId, count) {
    const id = String(dialogId);
    const n = Math.max(0, Number(count) || 0);
    for (const list of [vkState.dialogs, vkState.dirDialogs]) {
        const d = (list || []).find((x) => String(x.id) === id);
        if (d) d.unread_count = n;
    }
    const li = local.rows.get(id);
    if (li && li._vk) setUnreadText(li._vk.unread, n);
    renderDirs();
}

/**
 * Инкремент непрочитанного (пришло входящее в неактивную беседу).
 *
 * @returns {number} новое значение
 */
export function bumpUnread(dialogId) {
    const d = findDialog(dialogId);
    const next = ((d && Number(d.unread_count)) || 0) + 1;
    setUnread(dialogId, next);
    return next;
}

/**
 * Глушение беседы переключили (с любого устройства).
 *
 * У VK глушение только per-peer (`push_settings`), глобальных категорийных
 * дефолтов, как в Telegram, не существует — поэтому здесь нет ни
 * `notifyDefaults`, ни ветки их пересчёта.
 *
 * @param {{dialog_id: string, muted: boolean}} evt
 * @returns {boolean} нашлась ли беседа
 */
export function onMuted(evt) {
    if (!evt || !evt.dialog_id) return false;
    const id = String(evt.dialog_id);
    const muted = !!evt.muted;
    const d = findDialog(id);
    if (d) d.muted = muted;
    const li = local.rows.get(id);
    if (li && li._vk) {
        li.classList.toggle('vk-muted', muted);
        li._vk.mark.textContent = mutedMark(muted);
    }
    renderDirs();
    return !!d;
}

/**
 * WS `online` — точечное обновление статуса.
 *
 * В личных беседах `peer_id === user_id`, поэтому событие адресуется прямо в
 * строку списка. У бесед и сообществ присутствия не бывает — там свой статус.
 *
 * ТОЧЕЧНО — это принципиально: полная перерисовка колонки сбрасывала бы позицию
 * скролла, а статусы меняются постоянно и без участия пользователя.
 *
 * Событие про беседу, которой нет в колонке, не выбрасывается: присутствие
 * запоминается и всплывёт, когда беседа появится.
 *
 * @param {{user_id: string, online: boolean, was_online: ?number}} evt
 * @returns {boolean} нашлась ли строка
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
    if (!li || !li._vk || !d) return false;
    li._vk.statusText.textContent = dialogStatusLine(d, local.presence.get(id));
    return true;
}

/**
 * Известное присутствие беседы (или `null`).
 *
 * Наружу отдаётся копия, а не сама карта: шапку сессии рисует chat.js, но
 * менять присутствие вправе только этот модуль.
 */
export function presenceOf(dialogId) {
    const p = local.presence.get(String(dialogId));
    return p ? { online: p.online, wasOnline: p.wasOnline } : null;
}

/**
 * Счётчик «сколько собеседников в сети» в правой ячейке статусбара.
 *
 * Считаются только те, по кому статус ИЗВЕСТЕН: «0 ONLINE» на свежей вкладке
 * означало бы не «никого нет», а «VK ещё ничего не присылал».
 */
export function renderOnlineCounter() {
    if (!$vkStatusRight) return '';
    let online = 0;
    let known = 0;
    for (const [id, p] of local.presence) {
        // Присутствие бывает только у людей. Событие с id беседы или
        // сообщества (чужой клиент, будущий тип события) не должно
        // превращаться в «+1 в сети».
        const d = findDialog(id);
        if (d && String(d.kind) !== 'user') continue;
        known += 1;
        if (p && p.online) online += 1;
    }
    const text = onlineCounterText(online, known);
    $vkStatusRight.textContent = text;
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
 * `direction: "out"` (прочитали нас) на колонку не влияет.
 */
export function onRead(evt) {
    if (!evt || evt.direction !== 'in' || !evt.dialog_id) return;
    setUnread(String(evt.dialog_id), 0);
}

/**
 * WS `new_message` — обновить превью/счётчик в колонке.
 *
 * Счётчик поднимается только для чужого сообщения в НЕактивной беседе: своё
 * исходящее непрочитанным не бывает, а активная беседа тут же помечается
 * прочитанной.
 *
 * @param {object} msg — MessageDto
 * @returns {boolean} поднялся ли счётчик
 */
export function onNewMessage(msg) {
    if (!msg || !msg.dialog_id) return false;
    const id = String(msg.dialog_id);
    const d = findDialog(id);
    if (d) d.last_message = msg;
    if (msg.out || vkState.activeDialogId === id) return false;
    bumpUnread(id);
    return true;
}

/**
 * WS `dialog_changed` — у беседы поменялись название, состав или фото
 * (события LongPoll 51/52).
 *
 * ЧЕСТНОЕ ОГРАНИЧЕНИЕ, которое надо знать. Кадр несёт `dialog_id` затем, чтобы
 * фронт решил, стоит ли вообще перечитывать: про беседу вне загруженной
 * страницы (и вне открытой ленты) знать нечего, и запрос ради неё был бы
 * потрачен впустую при бюджете 3 rps. Но роута «одна беседа» у нас нет — есть
 * только страница `GET /api/vk/dialogs`, поэтому перечитывается она. Событий
 * 51/52 в живой беседе приходит пачками (переименовали, сменили фото, вошёл
 * участник), и без склейки это была бы очередь запросов на каждое из них —
 * отсюда дебаунс и объединение всех накопившихся id в одно чтение.
 *
 * @param {{dialog_id: string}} evt
 * @returns {boolean} запланировано ли перечитывание
 */
export function onDialogChanged(evt) {
    if (!evt || !evt.dialog_id) return false;
    const id = String(evt.dialog_id);
    // Беседа не на виду — обновлять нечего: она приедет свежей, когда список
    // перечитают по любой другой причине.
    if (!findDialog(id) && String(vkState.activeDialogId) !== id) return false;

    local.changedPeers.add(id);
    if (local.changedTimer) return true;
    local.changedTimer = setTimeout(() => {
        local.changedTimer = null;
        local.changedPeers.clear();
        loadDialogs().catch(() => {
            // not_connected / flood_wait — строка обновится со следующим
            // resync или открытием вкладки.
        });
    }, DIR_DEBOUNCE_MS);
    return true;
}

/** Сбрасывает колонку целиком (logout, смена аккаунта). */
export function resetDialogs() {
    if (local.dirTimer) {
        clearTimeout(local.dirTimer);
        local.dirTimer = null;
    }
    if (local.changedTimer) {
        clearTimeout(local.changedTimer);
        local.changedTimer = null;
    }
    local.changedPeers.clear();
    local.dirGen.bump();
    local.rows.clear();
    local.presence.clear();
    if ($vkDialogList) $vkDialogList.textContent = '';
    if ($vkCommunityList) $vkCommunityList.textContent = '';
    if ($vkStatusRight) $vkStatusRight.textContent = '';
    renderDirs();
    if ($vkNodesTitle) $vkNodesTitle.textContent = nodesTitle(0, '~/all');
}

/** Снимок для e2e/диагностики. */
export function _debugDialogsState() {
    let online = 0;
    for (const p of local.presence.values()) {
        if (p && p.online) online += 1;
    }
    return {
        dialogs: (vkState.dialogs || []).length,
        dirDialogs: (vkState.dirDialogs || []).length,
        filters: local.filters.length,
        activeDir: vkState.activeDir,
        rows: local.rows.size,
        presence: local.presence.size,
        online,
        dirPending: local.dirTimer !== null,
        changedPending: local.changedTimer !== null,
        changedPeers: local.changedPeers.size,
        activeDialogId: vkState.activeDialogId,
    };
}
