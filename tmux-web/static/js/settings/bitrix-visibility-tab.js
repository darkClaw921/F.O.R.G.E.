// tmux-web — Settings modal: раздел «Видимость диалогов» вкладки «Битрикс24».
//
// Экспортирует buildBitrixVisibilityForm(settings, onSaved) — фабрику DOM-узла
// с тумблером режима «показывать только выбранное», списком ВИДОВ диалогов и
// списком отдельных диалогов (с поиском). Данные для второго списка тянутся из
// плагина ОДИН РАЗ при первом показе: GET /api/bitrix/dialogs?limit=200.
//
// Поля настроек — запись ПОРТАЛА в карте `bitrix_portal_prefs`
// (мэппинг 1:1 на `BitrixPortalPrefs` в src/user_settings.rs):
//   filter_enabled  — bool (checkbox), default false, читается `=== true`
//   visible_kinds   — Vec<String>: 'user' | 'chat' | 'task'
//   visible_peers   — Vec<String>: DIALOG_ID ('chat7', '19', 'sg12')
//
// ============================================================================
// ПЕРЕКЛЮЧАТЕЛЬ ПОРТАЛА В ШАПКЕ ФОРМЫ (Ф7.5)
// ============================================================================
//
// `visible_peers` — это `DIALOG_ID` КОНКРЕТНОГО портала (`chat7` у двух
// порталов означает разные чаты), поэтому форма обязана отвечать на вопрос
// «чьи это настройки» ЯВНО, а не подразумевать активный портал панели.
//
// Отсюда три следствия, каждое из которых легко потерять:
//
//   • список диалогов тянется с ЯВНЫМ заголовком портала
//     (`getDialogs({portal})`) — иначе пикер на портале B показывал бы чаты
//     портала A, и человек отметил бы галочками чужие чаты;
//   • сохранение уходит ТОЛЬКО в запись выбранного портала
//     (`portalPrefsPatch`), а слияние делает бэкенд — записи остальных
//     порталов при этом не трогаются;
//   • смена портала перечитывает и выбор, и список: черновик предыдущего
//     портала не переносится никуда.
//
// Выбранный портал хранится в `bitrix-settings-portal.js` — он же нужен блоку
// скоупа голосовых, который живёт в общей на три панели форме.
//
// ПОЧЕМУ ОТДЕЛЬНЫЙ ФАЙЛ, А НЕ ПАРАМЕТРИЗАЦИЯ СОСЕДНЕЙ ФОРМЫ. У Telegram белый
// список строится из ПАПОК аккаунта, у VK — из трёх типов peer'а, здесь — из
// трёх видов диалога портала, и у чатов задач есть свои пометки (статус, номер
// задачи), которых нет ни у кого. Общего между тремя формами остаётся один
// тумблер; параметризация ради него потребовала бы переписать две работающие
// формы. (У формы ХОТКЕЕВ ситуация обратная: там различий три, и её
// параметризуют — см. hotkeys-tab.js.)
//
// ПОЧЕМУ НИКАКОГО ПОЛЛИНГА. Бюджет портала — 2 запроса в секунду на весь
// devforge (Р16), и открытая модалка не имеет права его съедать: список
// подгружается один раз и больше в сеть не ходит. Своего планировщика у панели
// Битрикс24 нет (в отличие от VK): троттлинг живёт на СЕРВЕРЕ, в
// `plugins/bitrix/src/config.rs`, поэтому клиенту достаточно не частить.
//
// ВАЖНОЕ ПРО ВЫБРАННЫЕ, НО НЕ ЗАГРУЖЕННЫЕ ДИАЛОГИ. `/dialogs` отдаёт верхушку
// `im.recent.get`, а выбор пользователя может содержать чат, до которого руки
// не доходили полгода. Черновик хранит ВСЕ выбранные id, а не только те, что
// удалось нарисовать: иначе первое же открытие настроек молча вычёркивало бы
// такие диалоги из белого списка. Явная кнопка «Убрать несуществующие» для
// этого есть — но нажимает её пользователь, а не мы за него.

import { getDialogs, getPortals } from '../bitrix/api.js';
import {
    filterSummary, filterKind, kindLabel, normalizeFilter, portalPrefsPatch,
    pruneMissingPeers, VISIBLE_KINDS,
} from '../bitrix/visibility.js';
import { setSettingsPortal, settingsPortal } from './bitrix-settings-portal.js';
import { updateUserSettings } from './user-settings-api.js';

/** Сколько диалогов просим у плагина для списка выбора. */
export const PEER_LIST_LIMIT = 200;

/**
 * Строит форму.
 *
 * @param {?object} settings — user settings (может быть null/{})
 * @param {?function(object): void} onSaved
 * @param {?function(string): void} [onPortalChange] — вкладка настроек
 *   перерисовывает СОСЕДНИЕ формы (скоуп голосовых тоже пер-портальный)
 * @returns {HTMLElement}
 */
export function buildBitrixVisibilityForm(settings, onSaved, onPortalChange) {
    /** Портал, чьи настройки правит форма прямо сейчас. */
    const portal = { id: settingsPortal() };

    // Рабочая копия: правки живут здесь до Save, закрытая без сохранения
    // модалка ничего не меняет.
    const draftKinds = new Set();
    const draftPeers = new Set();

    /** Заполняет черновик настройками ВЫБРАННОГО портала. */
    function adoptDrafts() {
        const initial = normalizeFilter(settings, portal.id);
        draftKinds.clear();
        draftPeers.clear();
        initial.kinds.forEach((k) => draftKinds.add(k));
        initial.peers.forEach((p) => draftPeers.add(p));
        return initial;
    }

    const initial = adoptDrafts();

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Видимость диалогов';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Отметьте виды диалогов и отдельные диалоги, которые нужны в devforge. '
        + 'Остальные исчезнут из колонки, из счётчиков директорий, из бейджа '
        + 'вкладки и из уведомлений — на самом портале они останутся '
        + 'нетронутыми.';
    fs.appendChild(hint);

    // ── переключатель портала ──
    //
    // Ряд рисуется ВСЕГДА, даже когда портал один: он отвечает на вопрос «чьи
    // это настройки», и при подключении второго портала ответ не должен
    // появляться задним числом.
    const portalRow = document.createElement('div');
    portalRow.className = 'b24-visibility-portal-row';
    const portalLabel = document.createElement('span');
    portalLabel.className = 'b24-visibility-group';
    portalLabel.textContent = 'Портал';
    portalRow.appendChild(portalLabel);

    const portalSelect = document.createElement('select');
    portalSelect.className = 'b24-visibility-portal';
    portalSelect.setAttribute('aria-label', 'Портал, чьи настройки видны');
    portalRow.appendChild(portalSelect);
    fs.appendChild(portalRow);

    const portalHint = document.createElement('div');
    portalHint.className = 'notify-hint';
    portalHint.textContent =
        'Выбор относится ТОЛЬКО к этому порталу: один и тот же номер чата у '
        + 'разных порталов означает разные чаты.';
    fs.appendChild(portalHint);

    // ── тумблер ──
    const onWrap = document.createElement('label');
    onWrap.className = 'modal-check notify-check';
    const onBox = document.createElement('input');
    onBox.type = 'checkbox';
    onBox.className = 'b24-filter-enabled';
    // `=== true` в предикате, поэтому и здесь строго: дефолт настройки — ВЫКЛ
    // (в отличие от соседнего тумблера хоткеев, где дефолт ВКЛ).
    onBox.checked = initial.enabled;
    onBox.title = 'Выключение возвращает все диалоги, выбор при этом сохраняется.';
    onWrap.appendChild(onBox);
    onWrap.appendChild(document.createTextNode(' Показывать только выбранные диалоги'));
    fs.appendChild(onWrap);

    const summary = document.createElement('div');
    summary.className = 'notify-hint b24-visibility-summary';
    fs.appendChild(summary);

    // ── виды ──
    const kindsTitle = document.createElement('div');
    kindsTitle.className = 'b24-visibility-group';
    kindsTitle.textContent = 'Виды диалогов';
    fs.appendChild(kindsTitle);

    const kindsBox = document.createElement('div');
    kindsBox.className = 'b24-visibility-list b24-visibility-kinds';
    fs.appendChild(kindsBox);

    // ── отдельные диалоги ──
    const peersTitle = document.createElement('div');
    peersTitle.className = 'b24-visibility-group';
    peersTitle.textContent = 'Отдельные диалоги';
    fs.appendChild(peersTitle);

    const search = document.createElement('input');
    search.type = 'search';
    search.className = 'b24-visibility-search';
    search.placeholder = 'Поиск по названию';
    search.setAttribute('aria-label', 'Поиск диалога');
    fs.appendChild(search);

    const peersBox = document.createElement('div');
    peersBox.className = 'b24-visibility-list b24-visibility-peers';
    peersBox.textContent = 'Загрузка…';
    fs.appendChild(peersBox);

    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    const ok = document.createElement('div');
    ok.className = 'notify-hint b24-visibility-save-ok';
    ok.style.display = 'none';
    ok.textContent = 'Сохранено';
    fs.appendChild(ok);

    /** Загруженные данные плагина. */
    const data = { dialogs: [], loaded: false };

    const refreshSummary = () => {
        const draft = normalizeFilter({
            bitrix_filter_enabled: !!onBox.checked,
            bitrix_visible_kinds: Array.from(draftKinds),
            bitrix_visible_peers: Array.from(draftPeers),
        });
        summary.textContent = filterSummary(draft);
        // Включённый режим с пустым выбором — не ошибка, но и не то, чего ждут:
        // предупреждаем прямо здесь, а не оставляем гадать по пустой панели.
        summary.classList.toggle(
            'b24-visibility-warn',
            !!onBox.checked && draftKinds.size === 0 && draftPeers.size === 0,
        );
    };

    onBox.addEventListener('change', refreshSummary);

    /** Одна строка списка с чекбоксом. */
    function row(key, label, checked, set, extra) {
        const wrap = document.createElement('label');
        wrap.className = 'modal-check b24-visibility-item';
        const box = document.createElement('input');
        box.type = 'checkbox';
        box.checked = checked;
        box.addEventListener('change', () => {
            if (box.checked) set.add(key);
            else set.delete(key);
            refreshSummary();
        });
        wrap.appendChild(box);
        const text = document.createElement('span');
        text.className = 'b24-visibility-label';
        text.textContent = label;
        wrap.appendChild(text);
        if (extra) {
            const badge = document.createElement('span');
            badge.className = 'b24-visibility-badge';
            badge.textContent = extra;
            wrap.appendChild(badge);
        }
        return wrap;
    }

    function renderKinds() {
        kindsBox.textContent = '';
        // Список видов — константа контракта настроек (js/bitrix/visibility.js),
        // а не выборка из портала: сети для него не нужно, поэтому он готов
        // сразу, ещё до ответа плагина.
        for (const kind of VISIBLE_KINDS) {
            kindsBox.appendChild(row(kind.key, kind.label,
                draftKinds.has(kind.key), draftKinds, kind.hint));
        }
    }

    /** Пометки диалога: вид, глушение, номер задачи. */
    function marksOf(d) {
        // Вид показывается ТОТ, по которому будет судить фильтр (`filterKind`
        // сводит семь видов портала к трём) — иначе подпись и поведение
        // разошлись бы на первой же открытой линии или группе.
        const marks = [kindLabel(filterKind(d)).toLowerCase()];
        if (d.task_id) marks.push(`задача #${d.task_id}`);
        if (d.muted) marks.push('без звука');
        return marks.join(' · ');
    }

    function renderPeers() {
        const q = String(search.value || '').trim().toLowerCase();
        peersBox.textContent = '';
        const list = data.dialogs.filter((d) => {
            if (!q) return true;
            return String(d.title || d.id).toLowerCase().includes(q);
        });
        if (!list.length) {
            const empty = document.createElement('div');
            empty.className = 'notify-hint';
            empty.textContent = q ? 'Ничего не найдено.' : 'Список диалогов пуст.';
            peersBox.appendChild(empty);
        }
        for (const d of list) {
            const id = String(d.id);
            peersBox.appendChild(row(id, String(d.title || id),
                draftPeers.has(id), draftPeers, marksOf(d)));
        }
        // Выбранные диалоги, которых нет в загруженной странице, обязаны быть
        // видны — иначе пользователь не поймёт, почему счётчик выбора больше
        // числа галочек, и не сможет снять лишнее.
        const shownIds = new Set(data.dialogs.map((d) => String(d.id)));
        const orphans = Array.from(draftPeers).filter((id) => !shownIds.has(id));
        if (orphans.length && !q) {
            const note = document.createElement('div');
            note.className = 'notify-hint';
            note.textContent = `Выбрано вне загруженного списка: ${orphans.length}.`;
            peersBox.appendChild(note);
            for (const id of orphans) {
                peersBox.appendChild(row(id, `id ${id}`, true, draftPeers, 'не в списке'));
            }
        }
    }

    search.addEventListener('input', () => {
        if (data.loaded) renderPeers();
    });

    // ── загрузка данных плагина ──
    //
    // Портал передаётся ЯВНО (`api.js` подмешивает `X-Forge-Portal`): форма
    // может быть открыта не на том портале, что панель, и умолчание «активный»
    // показало бы здесь чужие чаты.
    async function loadPeers() {
        data.loaded = false;
        peersBox.textContent = 'Загрузка…';
        search.disabled = false;
        try {
            const resp = await getDialogs({ limit: PEER_LIST_LIMIT, portal: portal.id });
            data.dialogs = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
            data.loaded = true;
            renderPeers();
        } catch (e) {
            // Плагин выключен, вебхук не привязан, портал душит — список
            // нарисовать нечем. Уже сохранённый выбор при этом НЕ теряется: он
            // лежит в черновике и уйдёт обратно тем же составом, если нажать
            // Save.
            const code = (e && e.code) || '';
            let msg;
            if (code === 'not_configured' || code === 'not_authorized') {
                msg = 'Подключите портал во вкладке «Битрикс24», чтобы выбрать диалоги.';
            } else if (code === 'throttled') {
                msg = 'Портал сейчас ограничивает запросы — откройте настройки чуть позже.';
            } else {
                msg = `Не удалось загрузить список диалогов: ${(e && e.message) || 'ошибка'}`;
            }
            peersBox.textContent = msg;
            search.disabled = true;
        }
    }

    /**
     * Рисует список порталов в переключателе.
     *
     * Пустой `title` показывается как `id` — ровно как в строке вкладок панели:
     * бэкенд хост портала наружу не отдаёт вовсе, и второго правила подписи
     * быть не должно.
     */
    function renderPortals(list) {
        portalSelect.textContent = '';
        const rows = (Array.isArray(list) ? list : []).filter((p) => p && p.id);
        if (!rows.length) {
            const opt = document.createElement('option');
            opt.value = '';
            opt.textContent = 'портал не подключён';
            portalSelect.appendChild(opt);
            portalSelect.disabled = true;
            return;
        }
        for (const p of rows) {
            const opt = document.createElement('option');
            opt.value = String(p.id);
            opt.textContent = String(p.title || p.id);
            portalSelect.appendChild(opt);
        }
        portalSelect.disabled = rows.length < 2;
        // Портал формы мог исчезнуть (его отвязали в соседней вкладке
        // браузера) — тогда правим первый уцелевший, а не пишем в никуда.
        if (!rows.some((p) => String(p.id) === portal.id)) {
            portal.id = String(rows[0].id);
            setSettingsPortal(portal.id);
            onBox.checked = adoptDrafts().enabled;
            renderKinds();
            refreshSummary();
            void loadPeers();
        }
        portalSelect.value = portal.id;
    }

    portalSelect.addEventListener('change', () => {
        const next = String(portalSelect.value || '');
        if (!next || next === portal.id) return;
        portal.id = next;
        setSettingsPortal(next);
        // Черновик предыдущего портала не переносится: в нём лежат его
        // `DIALOG_ID`, и «перенесённая» галочка означала бы случайный чат.
        // Тумблер перечитывается вместе со списками — он тоже пер-портальный,
        // и оставленный от соседа он молча включил бы фильтр не там.
        onBox.checked = adoptDrafts().enabled;
        renderKinds();
        refreshSummary();
        void loadPeers();
        // Скоуп голосовых у портала тоже свой — его форма живёт по соседству.
        if (typeof onPortalChange === 'function') onPortalChange(next);
    });

    (async () => {
        try {
            const resp = await getPortals();
            renderPortals(resp && resp.portals);
        } catch (_) {
            // Реестр недоступен (плагин выключен) — переключать нечего, но
            // остальная форма обязана работать: выбор лежит в настройках, а не
            // в ответе плагина.
            renderPortals([]);
        }
    })();

    void loadPeers();

    renderKinds();
    refreshSummary();

    // ── действия ──
    const actions = document.createElement('div');
    actions.className = 'notify-actions';

    const clearBtn = document.createElement('button');
    clearBtn.type = 'button';
    clearBtn.textContent = 'Снять всё';
    clearBtn.addEventListener('click', () => {
        draftKinds.clear();
        draftPeers.clear();
        renderKinds();
        if (data.loaded) renderPeers();
        refreshSummary();
    });
    actions.appendChild(clearBtn);

    const pruneBtn = document.createElement('button');
    pruneBtn.type = 'button';
    pruneBtn.textContent = 'Убрать несуществующие';
    pruneBtn.title = 'Вычистить из выбора диалоги, которых нет в загруженном списке';
    pruneBtn.addEventListener('click', () => {
        // Только по загруженному списку и только по нажатию: чистка «сама
        // собой» вычеркнула бы диалог, просто не попавший в верхушку страницы.
        if (!data.loaded) return;
        const kept = pruneMissingPeers(Array.from(draftPeers), data.dialogs);
        draftPeers.clear();
        kept.forEach((id) => draftPeers.add(id));
        renderPeers();
        refreshSummary();
    });
    actions.appendChild(pruneBtn);

    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    saveBtn.addEventListener('click', async () => {
        err.style.display = 'none';
        err.textContent = '';
        ok.style.display = 'none';
        saveBtn.disabled = true;

        // ТОЛЬКО свои три поля И ТОЛЬКО в записи ВЫБРАННОГО портала: PATCH
        // применяет Some(..)-варианты и сливает карту по ключу портала,
        // поэтому ни соседние настройки (телеграмные, VK, todo/echo), ни
        // записи остальных порталов не трогаются. Списки уходят ЦЕЛИКОМ —
        // снять галочку иначе нечем.
        //
        // Плоские ключи (`bitrix_visible_*`) отсюда НЕ пишутся: после миграции
        // они заморожены на своём домультипортальном значении и нужны только
        // откату на старую сборку (см. шапку `user_settings.rs`).
        const payload = portalPrefsPatch(portal.id, {
            filter_enabled: !!onBox.checked,
            visible_kinds: Array.from(draftKinds),
            visible_peers: Array.from(draftPeers),
        });

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
            // Панель кэширует разобранный фильтр по содержимому настроек, но
            // явный сброс делает контракт видимым и не зависит от внутренностей.
            invalidateB24Filter();
            if (typeof onSaved === 'function') onSaved(updated);
        } catch (e) {
            err.style.display = '';
            err.textContent = (e && e.message) ? e.message : 'Не удалось сохранить настройки.';
        } finally {
            saveBtn.disabled = false;
        }
    });
    actions.appendChild(saveBtn);
    fs.appendChild(actions);

    return fs;
}

/**
 * Сбрасывает кэш фильтра в панели «Битрикс24» и применяет новый белый список.
 *
 * Через `window.ForgeApp.b24Filter` (Р1), а не прямым импортом: настройки
 * открываются и без включённого плагина, и статический импорт тянул бы всю
 * панель в граф модалки. Отсутствие контракта — норма (плагин выключен).
 * Сам `refresh` делает четыре вещи: сбрасывает кэш разбора, перерисовывает
 * колонку, пересчитывает счётчики директорий и бейдж вкладки.
 */
function invalidateB24Filter() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const api = app && app.b24Filter;
    if (api && typeof api.refresh === 'function') {
        try { api.refresh(); } catch (_) { /* перерисовка не критична */ }
    }
}
