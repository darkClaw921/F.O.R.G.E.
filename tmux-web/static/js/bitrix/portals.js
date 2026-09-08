// tmux-web — Bitrix24 plugin: строка вкладок порталов (мультипортальность, Ф8).
//
// Ряд под #b24-titlebar: по вкладке на каждый подключённый портал, бейдж
// непрочитанного на каждой, кнопка `+` и контекстное меню
// «переименовать / отвязать».
//
//   ┌─ FORGE://bitrix ────────── ✦ ◐ ⏻ ─┐
//   │ ▸ test-work-2  │ acme •3 │  +      │
//   ├───────────┬─────────────────────────┤
//   │ // DIRS   │ SESSION ▸ ~/all         │
//
// Строка видна ВСЕГДА, включая мобильную раскладку, где сайдбар
// #b24-contacts уезжает за левый край и открывается по ☰. Причина не
// косметическая: на узком экране вкладка портала — единственный способ
// понять, в чей чат сейчас уйдёт сообщение, и прятать её под тот же
// гамбургер значило бы прятать именно ту информацию, ради которой панели
// разведены по цвету (см. шапку css/bitrix.css).
//
// ============================================================================
// ЧТО ЗДЕСЬ ЕСТЬ И ЧЕГО ЗДЕСЬ НЕТ
// ============================================================================
//
// ЕСТЬ: состав вкладок, активный портал, память последнего открытого диалога
// на КАЖДЫЙ портал и локальные счётчики бейджей.
//
// НЕТ: reset панели и загрузка данных. Модуль не импортирует ни main.js, ни
// dialogs.js, ни chat.js — он зовёт колбэк `onSwitch(id, rememberedDialogId)`,
// который ставит main.js. Прямой импорт замкнул бы цикл (main.js импортирует
// этот модуль), а «почти цикл» в ES-модулях даёт не ошибку сборки, а
// `undefined` в половине экспортов — симптом без диагноза.
//
// ЧИСТАЯ ЛОГИКА ОТДЕЛЕНА ОТ DOM НАМЕРЕННО. Всё, что решает, КАКИМИ быть
// вкладкам (`computeTabs`, `nextActiveId`, `orderOf`, `movedOrder`,
// `portalLabel`), — функции без побочных эффектов: их зовёт node-тест, у
// которого DOM нет вовсе. Рисование живёт в `render()` ниже и в тест не
// заглядывает.
//
// ============================================================================
// РЕШЕНИЯ, КОТОРЫЕ ЛЕГКО ПЕРЕОТКРЫТЬ
// ============================================================================
//
// 1. **Пустой `title` показывается как `id`.** Это зафиксированное решение, а
//    не заглушка. Хост портала бэкенд НЕ отдаёт вовсе (он не пересекает
//    границу HTTP — шапка plugins/bitrix/src/routes/portals.rs), поэтому
//    единственное узнаваемое имя безымянной вкладки — её `id`. Строка вида
//    «портал без имени» сделала бы две безымянные вкладки неразличимыми.
// 2. **`portal_id` уходит на сервер ДОСЛОВНО.** Нормализация на бэкенде
//    регистронезависима, но своя копия её правил во фронте была бы вторым
//    парсером идентификатора.
// 3. **Пустой массив вкладок = «порталов ещё нет».** Слоты без вебхука сервер
//    фильтрует сам, а реестр пустым не бывает никогда — значит экран
//    подключения показывается по пустому списку, а не по отдельному признаку.
// 4. **Перестановка шлёт ПОЛНЫЙ список `id`, а не позицию.** Перетаскивание
//    меняет позиции сразу нескольких вкладок, и индекс заставлял бы сервер
//    угадывать, относительно какого состояния списка он посчитан.
// 5. **Отвязка спрашивает дважды**: подтверждение самой отвязки и отдельный
//    вопрос про кэш. Слить их в один вопрос нельзя: «отвязать» обратимо
//    (вебхук вставляется заново), «снести кэш» — нет.
// 6. **Реакция на `404 unknown_portal` ровно одна** — перечитать состав
//    вкладок и переключиться. Повтор запроса с тем же `id` бессмысленно: этого
//    портала в реестре больше нет.
//
// Инвариант XSS панели соблюдается дословно: только `createElement` и
// `textContent`, `innerHTML` не используется.

import {
    $b24PortalAdd, $b24PortalMenu, $b24PortalRemove, $b24PortalRename,
    $b24PortalTabs, $b24Portals,
} from '../core/dom.js';
import * as b24Api from './api.js';
import { b24State } from './state.js';

/** Потолок длины подписи вкладки, символов (дальше — усечение с многоточием). */
export const TAB_LABEL_MAX = 18;

const local = {
    /** Слушатели навешены. */
    inited: false,
    /** Состав вкладок — PortalDto[] в порядке реестра. */
    portals: [],
    /** Активный портал (пустая строка — не выбран). */
    activeId: '',
    /**
     * Локальные счётчики бейджей: id портала → число.
     *
     * Отдельно от `PortalDto.unread`, потому что источники РАЗНЫЕ: в DTO
     * приезжает снимок для холодного старта, а живое число двигает кадр
     * `counters` своего портала — кадр `portals` на каждый счётчик сервер
     * намеренно не шлёт (иначе полный список вкладок летел бы по сокету
     * постоянно).
     */
    unread: new Map(),
    /**
     * Последний открытый диалог КАЖДОГО портала: id портала → dialog_id.
     *
     * Полного кэша состояния на портал здесь нет и не будет (см. шапку
     * js/bitrix/main.js, раздел про переключение): запоминается ровно то, без
     * чего возврат на вкладку ощущается потерей места, — открытый диалог.
     */
    lastDialog: new Map(),
    /** id портала, для которого открыто контекстное меню (null — закрыто). */
    menuFor: null,
    /** id портала, который сейчас тащат мышью (null — не тащат). */
    dragId: null,
    /** Колбэк переключения портала: (id, rememberedDialogId) => void. */
    onSwitch: null,
    /** Колбэк кнопки `+`: открыть экран подключения в режиме добавления. */
    onAdd: null,
    /** Колбэк «состав вкладок изменился» — панель решает, что перечитать. */
    onChanged: null,
    /** Колбэк короткого уведомления. */
    onNotice: null,
};

// ---------------------------------------------------------------------------
// Чистая логика — покрывается node-тестом без DOM
// ---------------------------------------------------------------------------

/**
 * Подпись вкладки.
 *
 * ПУСТОЙ `title` показывается как `id` — решение 1 шапки модуля.
 *
 * @param {object} p — PortalDto
 * @returns {string}
 */
export function portalLabel(p) {
    if (!p) return '';
    const title = (typeof p.title === 'string') ? p.title.trim() : '';
    return title || String(p.id || '');
}

/**
 * Усечение подписи под ширину вкладки.
 *
 * Своя функция, а не `format.js::truncate`: там потолок применяется к тексту
 * сообщения и меряется иначе, а здесь важно, чтобы длинное имя портала не
 * растягивало ряд — ровно то, что требует критерий приёмки Ф8.5.
 *
 * @param {string} text
 * @param {number} [max=TAB_LABEL_MAX]
 * @returns {string}
 */
export function truncateLabel(text, max = TAB_LABEL_MAX) {
    const s = String(text == null ? '' : text);
    if (s.length <= max) return s;
    return `${s.slice(0, Math.max(1, max - 1))}…`;
}

/**
 * Состояние вкладок — единственный источник для рендера.
 *
 * @param {Array<object>} portals — PortalDto[] в порядке реестра
 * @param {string} activeId
 * @param {Map<string, number>} [unread] — живые счётчики (перекрывают снимок)
 * @returns {Array<object>} `{id, label, title, unread, active, connected, state}`
 */
export function computeTabs(portals, activeId, unread) {
    const rows = Array.isArray(portals) ? portals : [];
    const live = (unread instanceof Map) ? unread : new Map();
    return rows
        .filter((p) => p && p.id)
        .map((p) => {
            const id = String(p.id);
            // Живое число ПЕРЕКРЫВАЕТ снимок: снимок отдан ради холодного
            // старта, и после первого же кадра `counters` он устарел.
            const n = live.has(id) ? live.get(id) : Number(p.unread);
            return {
                id,
                label: portalLabel(p),
                title: (typeof p.title === 'string') ? p.title : '',
                unread: Number.isFinite(n) ? Math.max(0, Math.trunc(n)) : 0,
                active: id === String(activeId || ''),
                connected: !!p.connected,
                state: (p.login && typeof p.login.state === 'string')
                    ? p.login.state
                    : 'not_configured',
            };
        });
}

/**
 * Какой портал обязан стать активным после смены состава.
 *
 * Правила по убыванию приоритета: остаться на прежнем, если он ещё в списке;
 * иначе первый в списке; иначе пусто (вкладок нет — экран подключения).
 *
 * @param {Array<object>} portals
 * @param {string} wantedId
 * @returns {string}
 */
export function nextActiveId(portals, wantedId) {
    const rows = (Array.isArray(portals) ? portals : []).filter((p) => p && p.id);
    const want = String(wantedId || '');
    if (want && rows.some((p) => String(p.id) === want)) return want;
    return rows.length ? String(rows[0].id) : '';
}

/**
 * Порядок вкладок списком `id` — ровно то, что уезжает в PATCH.
 *
 * @param {Array<object>} portals
 * @returns {Array<string>}
 */
export function orderOf(portals) {
    return (Array.isArray(portals) ? portals : [])
        .filter((p) => p && p.id)
        .map((p) => String(p.id));
}

/**
 * Новый порядок после перетаскивания `dragged` на место `target`.
 *
 * Вставка ПЕРЕД целью, если тащили справа налево, и ПОСЛЕ — если слева
 * направо: так вкладка оказывается там, куда её отпустили, а не соседней с
 * этим местом.
 *
 * @param {Array<string>} ids
 * @param {string} dragged
 * @param {string} target
 * @returns {Array<string>} новый порядок (исходный массив не меняется)
 */
export function movedOrder(ids, dragged, target) {
    const list = (Array.isArray(ids) ? ids : []).map((x) => String(x));
    const from = list.indexOf(String(dragged));
    const to = list.indexOf(String(target));
    if (from < 0 || to < 0 || from === to) return list;
    const next = list.slice();
    next.splice(from, 1);
    next.splice(to, 0, String(dragged));
    return next;
}

// ---------------------------------------------------------------------------
// Состояние
// ---------------------------------------------------------------------------

/** Активный портал — дословно как его отдал бэкенд. */
export function activePortalId() {
    return local.activeId;
}

/** Подпись вкладки по id (для префикса тоста). Неизвестный портал ⇒ его id. */
export function portalTitle(id) {
    const key = String(id == null ? '' : id);
    const found = local.portals.find((p) => p && String(p.id) === key);
    return found ? portalLabel(found) : key;
}

/** Есть ли вообще подключённые порталы. */
export function hasPortals() {
    return local.portals.length > 0;
}

/** Снимок состава вкладок (копия — наружу отдаётся не внутренний массив). */
export function portalsList() {
    return local.portals.slice();
}

/**
 * Поставить бейджу вкладки ТОЧНОЕ число.
 *
 * Зовётся обработчиком кадра `counters`: у портала есть готовое серверное
 * число, и складывать его из приращений было бы заведомо хуже.
 *
 * @param {string} portalId
 * @param {number} count
 */
export function setPortalUnread(portalId, count) {
    const id = String(portalId == null ? '' : portalId);
    if (!id) return;
    const n = Number(count);
    local.unread.set(id, Number.isFinite(n) ? Math.max(0, Math.trunc(n)) : 0);
    render();
}

/**
 * Подвинуть бейдж вкладки на `delta`.
 *
 * Зовётся на входящее сообщение ЧУЖОГО портала: точное число приедет
 * ближайшим кадром `counters`, но ждать его значит показывать бейдж с
 * задержкой в целый цикл опроса.
 *
 * @param {string} portalId
 * @param {number} [delta=1]
 */
export function bumpPortalUnread(portalId, delta = 1) {
    const id = String(portalId == null ? '' : portalId);
    if (!id) return;
    const cur = Number(local.unread.get(id)) || 0;
    local.unread.set(id, Math.max(0, cur + (Number(delta) || 0)));
    render();
}

/**
 * Запомнить открытый диалог портала.
 *
 * @param {string} portalId
 * @param {?string} dialogId — пусто ⇒ забыть
 */
export function rememberDialog(portalId, dialogId) {
    const id = String(portalId == null ? '' : portalId);
    if (!id) return;
    const dlg = (dialogId == null || dialogId === '') ? '' : String(dialogId);
    if (dlg) local.lastDialog.set(id, dlg);
    else local.lastDialog.delete(id);
}

/** Последний открытый диалог портала (пустая строка — не открывался). */
export function rememberedDialog(portalId) {
    return local.lastDialog.get(String(portalId == null ? '' : portalId)) || '';
}

// ---------------------------------------------------------------------------
// Загрузка состава и реакция на изменения
// ---------------------------------------------------------------------------

/**
 * `GET /api/bitrix/portals` — перечитать состав вкладок.
 *
 * Ошибку глотаем молча: плагин мог быть выключен (404 — штатный путь гейта), а
 * панель без строки вкладок продолжает работать с порталом, который сервер
 * выбирает по умолчанию.
 *
 * @returns {Promise<void>}
 */
export async function loadPortals() {
    try {
        const resp = await b24Api.getPortals();
        applyPortals(resp && resp.portals);
    } catch (_) {
        // Штатный путь при выключенном плагине; строка вкладок просто пуста.
    }
}

/**
 * Кадр `portals` из WS — состав или статусы вкладок изменились.
 *
 * Именно кадром, а не перезагрузкой страницы: сервер шлёт его на добавление,
 * отвязку, переименование, перестановку и на КАЖДЫЙ переход автомата
 * подключения любого портала.
 *
 * @param {object} frame — кадр целиком
 */
export function applyPortalsFrame(frame) {
    if (!frame) return;
    applyPortals(frame.portals);
}

/**
 * Реакция на `404 unknown_portal`: заголовок назвал портал, которого в реестре
 * больше нет (его отвязали в соседней вкладке браузера).
 *
 * Реакция ровно одна — перечитать состав и переключиться на уцелевший портал.
 * Повторять исходный запрос бессмысленно: его адресат не вернётся.
 *
 * @param {Error & {code?: string}} err
 * @returns {boolean} правда, если ошибка была именно этой (вызывающий может
 *   не показывать её пользователю)
 */
export function handleUnknownPortal(err) {
    if (!err || err.code !== 'unknown_portal') return false;
    notice('портал отвязан — переключаюсь');
    void loadPortals();
    return true;
}

/**
 * Применяет новый состав: чинит активный портал, чистит память исчезнувших,
 * перерисовывает ряд.
 *
 * @param {Array<object>} list — PortalDto[]
 */
function applyPortals(list) {
    const rows = (Array.isArray(list) ? list : []).filter((p) => p && p.id);
    const hadActive = local.activeId !== '';
    local.portals = rows;

    // Память исчезнувших порталов освобождается ЗДЕСЬ: иначе отвязанный портал
    // навсегда оставлял бы за собой строку в двух картах, а `id` у нового
    // портала может совпасть со старым (пользователь подключил тот же портал
    // заново) — и он унаследовал бы чужой диалог и чужой бейдж.
    const alive = new Set(rows.map((p) => String(p.id)));
    for (const id of Array.from(local.unread.keys())) {
        if (!alive.has(id)) local.unread.delete(id);
    }
    for (const id of Array.from(local.lastDialog.keys())) {
        if (!alive.has(id)) local.lastDialog.delete(id);
    }

    const next = nextActiveId(rows, local.activeId);
    if (next !== local.activeId) {
        // ПЕРВЫЙ выбор (панель только поднялась) — не переключение: данные в
        // этот момент грузит гейт, и второй reset+reload означал бы два
        // одинаковых запроса списка подряд, то есть двойную трату бюджета
        // портала на каждую загрузку страницы.
        if (hadActive) switchPortal(next);
        else adopt(next);
    }
    render();
    if (typeof local.onChanged === 'function') {
        try {
            local.onChanged(rows.slice());
        } catch (e) {
            console.warn('[b24-portals] onChanged threw', e);
        }
    }
}

/** Тихо принять портал активным, НЕ трогая панель (первый выбор). */
function adopt(id) {
    local.activeId = String(id || '');
    b24Api.setActivePortal(local.activeId);
}

/**
 * Переключиться на портал: запомнить открытый диалог текущего и отдать
 * управление панели.
 *
 * Сам reset и перезагрузку делает `onSwitch` (js/bitrix/main.js) — см. раздел
 * «ЧЕГО ЗДЕСЬ НЕТ» в шапке модуля.
 *
 * @param {string} portalId
 */
export function switchPortal(portalId) {
    const id = String(portalId == null ? '' : portalId);
    if (!id || id === local.activeId) return;

    // Диалог запоминается только у портала, который ЕЩЁ в реестре.
    // Переключение бывает и вынужденным — портал отвязали в соседней вкладке
    // браузера, — и тогда запоминать его открытый диалог значит вернуть в
    // память строку, которую `applyPortals` только что оттуда убрал: новый
    // портал с тем же `id` унаследовал бы чужой диалог.
    if (local.activeId && local.portals.some((p) => String(p.id) === local.activeId)) {
        rememberDialog(local.activeId, b24State.activeDialogId);
    }
    adopt(id);
    // Бейдж вкладки, на которую перешли, обнуляется сразу: пользователь именно
    // её и открыл. Точное число приедет ближайшим `counters` этого портала.
    local.unread.set(id, 0);
    render();

    if (typeof local.onSwitch === 'function') {
        try {
            local.onSwitch(id, rememberedDialog(id));
        } catch (e) {
            console.warn('[b24-portals] onSwitch threw', e);
        }
    }
}

// ---------------------------------------------------------------------------
// Действия контекстного меню
// ---------------------------------------------------------------------------

/**
 * Переименовать вкладку.
 *
 * Пустая строка СТИРАЕТ имя — вкладка снова подписывается `id`. Отмена
 * (`null` из prompt) не делает ничего: «отменил» и «стёр» — разные намерения.
 *
 * @param {string} portalId
 */
export async function renamePortal(portalId) {
    const id = String(portalId || '');
    if (!id) return;
    const current = local.portals.find((p) => String(p.id) === id);
    const asked = (typeof window !== 'undefined' && typeof window.prompt === 'function')
        ? window.prompt(`>> имя вкладки для «${id}» (пусто — показывать id)`,
            (current && current.title) || '')
        : null;
    if (asked === null || asked === undefined) return;
    try {
        await b24Api.renamePortal(id, asked);
        // Ответ применять не обязательно: сервер шлёт кадр `portals` всем
        // подписчикам, и вторая вкладка браузера обязана увидеть то же имя.
        // Но ждать сокет ради собственного действия незачем — перечитываем.
        await loadPortals();
    } catch (err) {
        if (handleUnknownPortal(err)) return;
        notice(errText(err, 'переименовать не удалось'));
    }
}

/**
 * Отвязать портал.
 *
 * ДВА вопроса, и слить их нельзя (решение 5 шапки): отвязка обратима — вебхук
 * вставляется заново, — а снос кэша нет.
 *
 * @param {string} portalId
 */
export async function removePortal(portalId) {
    const id = String(portalId || '');
    if (!id) return;
    const label = portalTitle(id);
    if (!ask(`>> отвязать портал «${label}»? Вебхук на самом портале продолжит действовать.`)) {
        return;
    }
    const purge = ask(`>> снести и локальный кэш «${label}»? Это необратимо.`);
    try {
        await b24Api.removePortal(id, purge);
        await loadPortals();
    } catch (err) {
        if (handleUnknownPortal(err)) return;
        notice(errText(err, 'отвязать не удалось'));
    }
}

/**
 * Отправить новый порядок вкладок.
 *
 * @param {Array<string>} order — ПОЛНЫЙ список id (решение 4 шапки)
 * @param {string} moved — портал, который перетащили (адресат запроса)
 */
async function commitOrder(order, moved) {
    try {
        await b24Api.reorderPortals(moved, order);
        await loadPortals();
    } catch (err) {
        if (handleUnknownPortal(err)) return;
        notice(errText(err, 'переставить не удалось'));
        // Порядок на экране мог уже отличаться от серверного — возвращаем его
        // к тому, что реально записано.
        void loadPortals();
    }
}

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

/** Перерисовывает ряд вкладок целиком: их единицы, инкремент не нужен. */
export function render() {
    if (!$b24PortalTabs) return;
    const tabs = computeTabs(local.portals, local.activeId, local.unread);

    $b24PortalTabs.textContent = '';
    for (const t of tabs) $b24PortalTabs.appendChild(buildTab(t));

    // Ряд не прячется даже пустым: кнопка `+` — единственный вход в добавление
    // ВТОРОГО портала, и она обязана быть на месте всегда. Пустой список при
    // этом означает «порталов ещё нет», и панель поверх закрыта экраном
    // подключения.
    if ($b24Portals) $b24Portals.classList.toggle('b24-portals-empty', tabs.length === 0);
}

/**
 * Собирает вкладку. Только `createElement`/`textContent`.
 *
 * @param {object} t — строка из [`computeTabs`]
 * @returns {object} узел `li`
 */
function buildTab(t) {
    const li = document.createElement('li');
    li.className = 'b24-portal-tab';
    li.classList.toggle('active', t.active);
    li.classList.toggle('b24-portal-off', !t.connected);
    li.dataset.portalId = t.id;
    li.draggable = true;
    li.tabIndex = 0;
    // Полное имя и состояние — в title: подпись усечена, а «почему вкладка
    // тусклая» иначе не узнать вовсе.
    li.title = `${portalLabel({ id: t.id, title: t.title })} · ${t.id} · ${t.state}`;

    const dot = document.createElement('span');
    dot.className = 'b24-portal-dot';
    dot.setAttribute('aria-hidden', 'true');
    li.appendChild(dot);

    const name = document.createElement('span');
    name.className = 'b24-portal-name';
    name.textContent = truncateLabel(t.label);
    li.appendChild(name);

    if (t.unread > 0) {
        const badge = document.createElement('span');
        badge.className = 'b24-portal-badge';
        badge.textContent = t.unread > 99 ? '99+' : String(t.unread);
        li.appendChild(badge);
    }
    return li;
}

// ---------------------------------------------------------------------------
// Контекстное меню
// ---------------------------------------------------------------------------

/**
 * Открыть меню у вкладки.
 *
 * @param {string} portalId
 * @param {number} x — координата курсора в пикселях окна
 * @param {number} y
 */
function openMenu(portalId, x, y) {
    if (!$b24PortalMenu) return;
    local.menuFor = String(portalId || '');
    $b24PortalMenu.hidden = false;
    // Позиционирование от угла панели, а не от документа: панель может быть
    // не на весь экран (remote-режим показывает её в сетке).
    const host = $b24Portals || $b24PortalMenu.parentNode;
    const box = (host && typeof host.getBoundingClientRect === 'function')
        ? host.getBoundingClientRect()
        : { left: 0, top: 0 };
    $b24PortalMenu.style.left = `${Math.max(0, x - box.left)}px`;
    $b24PortalMenu.style.top = `${Math.max(0, y - box.top)}px`;
}

/** Закрыть меню. */
export function closeMenu() {
    local.menuFor = null;
    if ($b24PortalMenu) $b24PortalMenu.hidden = true;
}

// ---------------------------------------------------------------------------
// Инициализация
// ---------------------------------------------------------------------------

/**
 * Навешивает слушатели ряда. ИДЕМПОТЕНТНА: панель открывают много раз, а
 * второй набор слушателей означал бы два PATCH'а на одно перетаскивание.
 *
 * @param {{onSwitch?: function, onAdd?: function, onChanged?: function,
 *   onNotice?: function}} [handlers]
 */
export function initB24Portals(handlers = {}) {
    local.onSwitch = typeof handlers.onSwitch === 'function' ? handlers.onSwitch : local.onSwitch;
    local.onAdd = typeof handlers.onAdd === 'function' ? handlers.onAdd : local.onAdd;
    local.onChanged = typeof handlers.onChanged === 'function' ? handlers.onChanged : local.onChanged;
    local.onNotice = typeof handlers.onNotice === 'function' ? handlers.onNotice : local.onNotice;

    if (local.inited) return;
    local.inited = true;

    if ($b24PortalTabs) {
        // Делегирование: вкладки перерисовываются целиком, и слушатель на
        // каждой пришлось бы вешать заново после каждого рендера.
        $b24PortalTabs.addEventListener('click', (e) => {
            const li = tabOf(e.target);
            if (!li) return;
            closeMenu();
            switchPortal(li.dataset.portalId);
        });
        $b24PortalTabs.addEventListener('keydown', (e) => {
            if (e.key !== 'Enter' && e.key !== ' ') return;
            const li = tabOf(e.target);
            if (!li) return;
            e.preventDefault();
            switchPortal(li.dataset.portalId);
        });
        $b24PortalTabs.addEventListener('contextmenu', (e) => {
            const li = tabOf(e.target);
            if (!li) return;
            e.preventDefault();
            openMenu(li.dataset.portalId, e.clientX, e.clientY);
        });

        // ---- перетаскивание ----
        $b24PortalTabs.addEventListener('dragstart', (e) => {
            const li = tabOf(e.target);
            if (!li) return;
            local.dragId = li.dataset.portalId;
            if (e.dataTransfer) {
                e.dataTransfer.effectAllowed = 'move';
                // Некоторые браузеры не начинают перетаскивание без данных.
                try { e.dataTransfer.setData('text/plain', local.dragId); } catch (_) { /* ok */ }
            }
        });
        $b24PortalTabs.addEventListener('dragover', (e) => {
            if (!local.dragId) return;
            e.preventDefault();
            if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
        });
        $b24PortalTabs.addEventListener('drop', (e) => {
            const li = tabOf(e.target);
            const dragged = local.dragId;
            local.dragId = null;
            if (!li || !dragged) return;
            e.preventDefault();
            const target = li.dataset.portalId;
            if (target === dragged) return;
            const next = movedOrder(orderOf(local.portals), dragged, target);
            // Ряд перерисовывается СРАЗУ, не дожидаясь ответа: перетаскивание
            // обязано ощущаться мгновенным, а расхождение с сервером лечит
            // `loadPortals()` в `commitOrder`.
            local.portals = next
                .map((id) => local.portals.find((p) => String(p.id) === id))
                .filter(Boolean);
            render();
            void commitOrder(next, dragged);
        });
        $b24PortalTabs.addEventListener('dragend', () => {
            local.dragId = null;
        });
    }

    if ($b24PortalAdd) {
        $b24PortalAdd.addEventListener('click', () => {
            closeMenu();
            if (typeof local.onAdd === 'function') local.onAdd();
        });
    }
    if ($b24PortalRename) {
        $b24PortalRename.addEventListener('click', () => {
            const id = local.menuFor;
            closeMenu();
            if (id) void renamePortal(id);
        });
    }
    if ($b24PortalRemove) {
        $b24PortalRemove.addEventListener('click', () => {
            const id = local.menuFor;
            closeMenu();
            if (id) void removePortal(id);
        });
    }

    // Меню закрывается кликом мимо и по Escape. Слушатели стоят на document —
    // клик по любой другой части панели обязан его убрать.
    document.addEventListener('click', (e) => {
        if (local.menuFor === null) return;
        if ($b24PortalMenu && $b24PortalMenu.contains(e.target)) return;
        closeMenu();
    });
    document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape' || local.menuFor === null) return;
        // Esc НЕ останавливается: меню — самый мелкий слой панели, и гасить им
        // общий Esc-стек (js/messenger/esc-stack.js) значило бы ломать
        // закрытие лайтбокса и попапа анализа.
        closeMenu();
    });
}

/**
 * Сброс модуля — teardown вкладки.
 *
 * Память последнего диалога и бейджи принадлежали ПРОШЛОЙ странице: после
 * перезагрузки состав приедет заново.
 */
export function resetPortals() {
    closeMenu();
    local.portals = [];
    local.activeId = '';
    local.unread.clear();
    local.lastDialog.clear();
    local.dragId = null;
    b24Api.setActivePortal('');
    if ($b24PortalTabs) $b24PortalTabs.textContent = '';
}

/** Снимок состояния для e2e/диагностики. Секрета здесь нет по построению. */
export function _debugPortalsState() {
    return {
        inited: local.inited,
        activeId: local.activeId,
        tabs: computeTabs(local.portals, local.activeId, local.unread),
        remembered: local.lastDialog.size,
        menuOpen: local.menuFor !== null,
        dragging: local.dragId !== null,
    };
}

// ---------------------------------------------------------------------------
// Мелочи
// ---------------------------------------------------------------------------

/** Ближайшая вкладка над узлом события (делегирование). */
function tabOf(target) {
    let node = target;
    while (node && node !== $b24PortalTabs) {
        if (node.dataset && node.dataset.portalId) return node;
        node = node.parentNode;
    }
    return null;
}

/** Вопрос пользователю. Отсутствие `confirm` (тесты) — это «нет». */
function ask(text) {
    if (typeof window === 'undefined' || typeof window.confirm !== 'function') return false;
    return !!window.confirm(text);
}

/** Короткое уведомление наружу (канал общий с тостами панели). */
function notice(text) {
    if (typeof local.onNotice === 'function') local.onNotice(String(text || ''));
}

/**
 * Текст ошибки для уведомления.
 *
 * Решения принимаются по `err.code` (см. шапку api.js), а этот текст — только
 * для показа человеку; на него ничего не завязано.
 */
function errText(err, fallback) {
    return (err && err.message) ? String(err.message) : fallback;
}
