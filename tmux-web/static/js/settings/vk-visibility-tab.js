// tmux-web — Settings modal: раздел «Видимость бесед» вкладки VK.
//
// Экспортирует buildVkVisibilityForm(settings, onSaved) — фабрику DOM-узла с
// тумблером режима «показывать только выбранное», списком ТИПОВ бесед и
// списком отдельных бесед (с поиском). Данные для второго списка тянутся из
// плагина ОДИН РАЗ при первом показе: GET /api/vk/dialogs?limit=200.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   vk_filter_enabled — bool (checkbox), default false
//   vk_visible_kinds  — Vec<String>: 'user' | 'chat' | 'community'
//   vk_visible_peers  — Vec<String>: peer_id бесед
//
// ПОЧЕМУ ОТДЕЛЬНЫЙ ФАЙЛ, А НЕ ПАРАМЕТРИЗАЦИЯ ТЕЛЕГРАМНОЙ ФОРМЫ. У Telegram
// белый список строится из ПАПОК аккаунта с их правилами, у VK — из трёх типов
// peer'а. Общего между формами остаётся один тумблер; параметризация ради него
// потребовала бы переписать работающую форму Telegram — ровно то, чего мы
// избегаем. (У формы ХОТКЕЕВ ситуация обратная: там различий три, и её
// параметризуют — см. Ф11.)
//
// ПОЧЕМУ НИКАКОГО ПОЛЛИНГА. Пользовательский токен VK ограничен 3 запросами в
// секунду на весь клиент, и открытая модалка не имеет права съедать этот
// бюджет: список подгружается один раз, фоновым приоритетом общего
// планировщика (js/vk/ratelimit.js), и больше в сеть не ходит.
//
// ВАЖНО про выбранные, но не загруженные беседы. `/dialogs` отдаёт верхушку
// списка, а выбор пользователя может содержать беседу, до которой руки не
// доходили полгода. Черновик хранит ВСЕ выбранные id, а не только те, что
// удалось нарисовать: иначе первое же открытие настроек молча вычёркивало бы
// такие беседы из белого списка. Явная кнопка «Убрать несуществующие» для
// этого есть — но нажимает её пользователь, а не мы за него.

import { getDialogs } from '../vk/api.js';
import { peerKind } from '../vk/format.js';
import { PRIORITY_BG, vkLimiter } from '../vk/ratelimit.js';
import {
    filterSummary, kindLabel, normalizeFilter, pruneMissingPeers, VISIBLE_KINDS,
} from '../vk/visibility.js';
import { updateUserSettings } from './user-settings-api.js';

/** Сколько бесед просим у плагина для списка выбора. */
export const PEER_LIST_LIMIT = 200;

/**
 * Строит форму.
 *
 * @param {?object} settings — user settings (может быть null/{})
 * @param {?function(object): void} onSaved
 * @returns {HTMLElement}
 */
export function buildVkVisibilityForm(settings, onSaved) {
    const initial = normalizeFilter(settings);

    // Рабочая копия: правки живут здесь до Save, закрытая без сохранения
    // модалка ничего не меняет.
    const draftKinds = new Set(initial.kinds);
    const draftPeers = new Set(initial.peers);

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Видимость бесед';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Отметьте типы бесед и отдельные беседы, которые нужны в devforge. '
        + 'Остальные исчезнут из колонки, из счётчиков и из уведомлений — в '
        + 'самом VK они останутся нетронутыми.';
    fs.appendChild(hint);

    // ── тумблер ──
    const onWrap = document.createElement('label');
    onWrap.className = 'modal-check notify-check';
    const onBox = document.createElement('input');
    onBox.type = 'checkbox';
    onBox.className = 'vk-filter-enabled';
    // `=== true` в предикате, поэтому и здесь строго: дефолт настройки — ВЫКЛ.
    onBox.checked = initial.enabled;
    onBox.title = 'Выключение возвращает все беседы, выбор при этом сохраняется.';
    onWrap.appendChild(onBox);
    onWrap.appendChild(document.createTextNode(' Показывать только выбранные беседы'));
    fs.appendChild(onWrap);

    const summary = document.createElement('div');
    summary.className = 'notify-hint vk-visibility-summary';
    fs.appendChild(summary);

    // ── типы ──
    const kindsTitle = document.createElement('div');
    kindsTitle.className = 'vk-visibility-group';
    kindsTitle.textContent = 'Типы бесед';
    fs.appendChild(kindsTitle);

    const kindsBox = document.createElement('div');
    kindsBox.className = 'vk-visibility-list vk-visibility-kinds';
    fs.appendChild(kindsBox);

    // ── отдельные беседы ──
    const peersTitle = document.createElement('div');
    peersTitle.className = 'vk-visibility-group';
    peersTitle.textContent = 'Отдельные беседы';
    fs.appendChild(peersTitle);

    const search = document.createElement('input');
    search.type = 'search';
    search.className = 'vk-visibility-search';
    search.placeholder = 'Поиск по названию';
    search.setAttribute('aria-label', 'Поиск беседы');
    fs.appendChild(search);

    const peersBox = document.createElement('div');
    peersBox.className = 'vk-visibility-list vk-visibility-peers';
    peersBox.textContent = 'Загрузка…';
    fs.appendChild(peersBox);

    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    const ok = document.createElement('div');
    ok.className = 'notify-hint vk-visibility-save-ok';
    ok.style.display = 'none';
    ok.textContent = 'Сохранено';
    fs.appendChild(ok);

    // Загруженные данные плагина.
    const data = { dialogs: [], loaded: false };

    const refreshSummary = () => {
        const draft = normalizeFilter({
            vk_filter_enabled: !!onBox.checked,
            vk_visible_kinds: Array.from(draftKinds),
            vk_visible_peers: Array.from(draftPeers),
        });
        summary.textContent = filterSummary(draft);
        // Включённый режим с пустым выбором — не ошибка, но и не то, чего ждут:
        // предупреждаем прямо здесь, а не оставляем гадать по пустой панели.
        summary.classList.toggle(
            'vk-visibility-warn',
            !!onBox.checked && draftKinds.size === 0 && draftPeers.size === 0,
        );
    };

    onBox.addEventListener('change', refreshSummary);

    /** Одна строка списка с чекбоксом. */
    function row(key, label, checked, set, extra) {
        const wrap = document.createElement('label');
        wrap.className = 'modal-check vk-visibility-item';
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
        text.className = 'vk-visibility-label';
        text.textContent = label;
        wrap.appendChild(text);
        if (extra) {
            const badge = document.createElement('span');
            badge.className = 'vk-visibility-badge';
            badge.textContent = extra;
            wrap.appendChild(badge);
        }
        return wrap;
    }

    function renderKinds() {
        kindsBox.textContent = '';
        // Список типов — константа контракта настроек (js/vk/visibility.js), а
        // не выборка из аккаунта: сети для него не нужно, поэтому он готов
        // сразу, ещё до ответа плагина.
        for (const kind of VISIBLE_KINDS) {
            kindsBox.appendChild(row(kind.key, kind.label,
                draftKinds.has(kind.key), draftKinds, kind.hint));
        }
    }

    /** Пометки беседы: тип, глушение, бот. */
    function marksOf(d) {
        // Тип показывается ТОТ, по которому будет судить фильтр (он выводится
        // из peer_id, а не из поля DTO) — иначе подпись и поведение разошлись
        // бы на первой же беседе с неожиданным `kind`.
        const marks = [kindLabel(peerKind(d.id)).toLowerCase()];
        if (d.muted) marks.push('без звука');
        if (d.bot) marks.push('бот');
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
            empty.textContent = q ? 'Ничего не найдено.' : 'Список бесед пуст.';
            peersBox.appendChild(empty);
        }
        for (const d of list) {
            const id = String(d.id);
            peersBox.appendChild(row(id, String(d.title || id),
                draftPeers.has(id), draftPeers, marksOf(d)));
        }
        // Выбранные беседы, которых нет в загруженной странице, обязаны быть
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
    (async () => {
        try {
            // Через общий планировщик и с ФОНОВЫМ приоритетом: открытая
            // модалка не должна отодвигать запросы самой панели в бюджете
            // 3 rps. Ровно один запрос за показ формы.
            await vkLimiter.acquire(PRIORITY_BG);
            const resp = await getDialogs({ limit: PEER_LIST_LIMIT, filter: 'all' });
            data.dialogs = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
            data.loaded = true;
            renderPeers();
        } catch (e) {
            // Плагин выключен, нет логина, not_connected — список нарисовать
            // нечем. Уже сохранённый выбор при этом НЕ теряется: он лежит в
            // черновике и уйдёт обратно тем же составом, если нажать Save.
            const msg = (e && e.code === 'not_authorized')
                ? 'Войдите в VK, чтобы выбрать беседы.'
                : `Не удалось загрузить список бесед: ${(e && e.message) || 'ошибка'}`;
            peersBox.textContent = msg;
            search.disabled = true;
        }
    })();

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
    pruneBtn.title = 'Вычистить из выбора беседы, которых нет в загруженном списке';
    pruneBtn.addEventListener('click', () => {
        // Только по загруженному списку и только по нажатию: чистка «сама
        // собой» вычеркнула бы беседу, просто не попавшую в верхушку страницы.
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

        // ТОЛЬКО свои три поля: PATCH применяет Some(..)-варианты, поэтому
        // соседние настройки (и телеграмные, и todo/echo) остаются нетронутыми.
        // Списки уходят ЦЕЛИКОМ — снять галочку иначе нечем.
        const payload = {
            vk_filter_enabled: !!onBox.checked,
            vk_visible_kinds: Array.from(draftKinds),
            vk_visible_peers: Array.from(draftPeers),
        };

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
            // Панель кэширует разобранный фильтр по содержимому настроек, но
            // явный сброс делает контракт видимым и не зависит от внутренностей.
            invalidateVkFilter();
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
 * Сбрасывает кэш фильтра в панели VK и применяет новый белый список.
 *
 * Через `window.ForgeApp.vkFilter`, а не прямым импортом: настройки
 * открываются и без включённого плагина, и статический импорт тянул бы всю
 * панель в граф модалки. Отсутствие контракта — норма (плагин выключен).
 * Сам `refresh` делает три вещи: сбрасывает кэш разбора, перерисовывает
 * колонку со счётчиками и пересчитывает бейдж.
 */
function invalidateVkFilter() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const api = app && app.vkFilter;
    if (api && typeof api.refresh === 'function') {
        try { api.refresh(); } catch (_) { /* перерисовка не критична */ }
    }
}
