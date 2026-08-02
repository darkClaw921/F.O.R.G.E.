// tmux-web — Settings modal: раздел «Видимость чатов» вкладки Telegram.
//
// Экспортирует buildVisibilityForm(settings, onSaved) — фабрику DOM-узла с
// тумблером режима «показывать только выбранное», списком папок аккаунта и
// списком чатов (с поиском). Данные для списков тянутся из плагина при первом
// показе: GET /api/telegram/folders и GET /api/telegram/dialogs.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   telegram_filter_enabled  — bool (checkbox), default false
//   telegram_visible_folders — Vec<String>: 'inbox' | 'archive' | 'folder:<id>'
//   telegram_visible_dialogs — Vec<String>: id чатов
//
// ВАЖНО про выбранные, но не загруженные чаты. `/dialogs` отдаёт верхушку
// списка, а выбор пользователя может содержать чат, до которого руки не
// доходили полгода. Черновик хранит ВСЕ выбранные id, а не только те, что
// удалось нарисовать: иначе первое же открытие настроек молча вычёркивало бы
// такие чаты из белого списка. Явная кнопка «Убрать несуществующие» для этого
// есть — но нажимает её пользователь, а не мы за него.

import { FOLDER_LIST_LIMIT } from '../telegram/format.js';
import { getDialogs, listFolders } from '../telegram/api.js';
import {
    DIR_ARCHIVE, DIR_INBOX, filterSummary, folderKey, normalizeFilter, pruneMissingDialogs,
} from '../telegram/visibility.js';
import { updateUserSettings } from './user-settings-api.js';

/** Сколько чатов показывать в списке до фильтрации поиском. */
const CHAT_LIST_LIMIT = FOLDER_LIST_LIMIT;

/**
 * Строит форму.
 *
 * @param {?object} settings — user settings (может быть null/{})
 * @param {?function(object): void} onSaved
 * @returns {HTMLElement}
 */
export function buildVisibilityForm(settings, onSaved) {
    const initial = normalizeFilter(settings);

    // Рабочая копия: правки живут здесь до Save, закрытая без сохранения
    // модалка ничего не меняет.
    const draftFolders = new Set(initial.folders);
    const draftDialogs = new Set(initial.dialogs);

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Видимость чатов';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Отметьте папки и чаты, которые нужны в devforge. Остальные исчезнут '
        + 'из колонки, из счётчиков и из уведомлений — в самом Telegram они '
        + 'останутся нетронутыми.';
    fs.appendChild(hint);

    // ── тумблер ──
    const onWrap = document.createElement('label');
    onWrap.className = 'modal-check notify-check';
    const onBox = document.createElement('input');
    onBox.type = 'checkbox';
    onBox.className = 'tg-filter-enabled';
    onBox.checked = initial.enabled;
    onBox.title = 'Выключение возвращает все чаты, выбор при этом сохраняется.';
    onWrap.appendChild(onBox);
    onWrap.appendChild(document.createTextNode(' Показывать только выбранные чаты и папки'));
    fs.appendChild(onWrap);

    const summary = document.createElement('div');
    summary.className = 'notify-hint tg-visibility-summary';
    fs.appendChild(summary);

    // ── папки ──
    const foldersTitle = document.createElement('div');
    foldersTitle.className = 'tg-visibility-group';
    foldersTitle.textContent = 'Папки';
    fs.appendChild(foldersTitle);

    const foldersBox = document.createElement('div');
    foldersBox.className = 'tg-visibility-list tg-visibility-folders';
    foldersBox.textContent = 'Загрузка…';
    fs.appendChild(foldersBox);

    // ── чаты ──
    const chatsTitle = document.createElement('div');
    chatsTitle.className = 'tg-visibility-group';
    chatsTitle.textContent = 'Отдельные чаты';
    fs.appendChild(chatsTitle);

    const search = document.createElement('input');
    search.type = 'search';
    search.className = 'tg-visibility-search';
    search.placeholder = 'Поиск по названию';
    search.setAttribute('aria-label', 'Поиск чата');
    fs.appendChild(search);

    const chatsBox = document.createElement('div');
    chatsBox.className = 'tg-visibility-list tg-visibility-chats';
    chatsBox.textContent = 'Загрузка…';
    fs.appendChild(chatsBox);

    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    const ok = document.createElement('div');
    ok.className = 'notify-hint tg-visibility-save-ok';
    ok.style.display = 'none';
    ok.textContent = 'Сохранено';
    fs.appendChild(ok);

    // Загруженные данные плагина.
    const data = { folders: [], dialogs: [], loaded: false };

    const refreshSummary = () => {
        const draft = normalizeFilter({
            telegram_filter_enabled: !!onBox.checked,
            telegram_visible_folders: Array.from(draftFolders),
            telegram_visible_dialogs: Array.from(draftDialogs),
        });
        summary.textContent = filterSummary(draft);
        // Включённый режим с пустым выбором — не ошибка, но и не то, чего ждут:
        // предупреждаем прямо здесь, а не оставляем гадать по пустой панели.
        summary.classList.toggle(
            'tg-visibility-warn',
            !!onBox.checked && draftFolders.size === 0 && draftDialogs.size === 0,
        );
    };

    onBox.addEventListener('change', refreshSummary);

    /** Одна строка списка с чекбоксом. */
    function row(key, label, checked, set, extra) {
        const wrap = document.createElement('label');
        wrap.className = 'modal-check tg-visibility-item';
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
        text.className = 'tg-visibility-label';
        text.textContent = label;
        wrap.appendChild(text);
        if (extra) {
            const badge = document.createElement('span');
            badge.className = 'tg-visibility-badge';
            badge.textContent = extra;
            wrap.appendChild(badge);
        }
        return wrap;
    }

    function renderFolders() {
        foldersBox.textContent = '';
        // ~/inbox и ~/archive — не папки Telegram, а пункты нашей навигации;
        // в белом списке они значат «все неархивные» и «весь архив».
        foldersBox.appendChild(row(DIR_INBOX, '~/inbox — все неархивные',
            draftFolders.has(DIR_INBOX), draftFolders));
        for (const f of data.folders) {
            const key = folderKey(f);
            if (!key) continue;
            const title = String(f.title || key);
            foldersBox.appendChild(row(key, (f.emoticon ? f.emoticon + ' ' : '') + title,
                draftFolders.has(key), draftFolders));
        }
        foldersBox.appendChild(row(DIR_ARCHIVE, '~/archive — архив',
            draftFolders.has(DIR_ARCHIVE), draftFolders));
    }

    function renderChats() {
        const q = String(search.value || '').trim().toLowerCase();
        chatsBox.textContent = '';
        const list = data.dialogs.filter((d) => {
            if (!q) return true;
            return String(d.title || d.id).toLowerCase().includes(q);
        });
        if (!list.length) {
            const empty = document.createElement('div');
            empty.className = 'notify-hint';
            empty.textContent = q ? 'Ничего не найдено.' : 'Список чатов пуст.';
            chatsBox.appendChild(empty);
        }
        for (const d of list.slice(0, CHAT_LIST_LIMIT)) {
            const id = String(d.id);
            const marks = [];
            if (d.archived) marks.push('архив');
            if (d.muted) marks.push('без звука');
            if (d.bot) marks.push('бот');
            chatsBox.appendChild(row(id, String(d.title || id),
                draftDialogs.has(id), draftDialogs, marks.join(' · ')));
        }
        // Выбранные чаты, которых нет в загруженной странице, обязаны быть
        // видны — иначе пользователь не поймёт, почему счётчик выбора больше
        // числа галочек, и не сможет снять лишнее.
        const shownIds = new Set(data.dialogs.map((d) => String(d.id)));
        const orphans = Array.from(draftDialogs).filter((id) => !shownIds.has(id));
        if (orphans.length && !q) {
            const note = document.createElement('div');
            note.className = 'notify-hint';
            note.textContent = `Выбрано вне загруженного списка: ${orphans.length}.`;
            chatsBox.appendChild(note);
            for (const id of orphans) {
                chatsBox.appendChild(row(id, `id ${id}`, true, draftDialogs, 'не в списке'));
            }
        }
    }

    search.addEventListener('input', () => {
        if (data.loaded) renderChats();
    });

    // ── загрузка данных плагина ──
    (async () => {
        try {
            const [foldersResp, dialogsResp] = await Promise.all([
                listFolders(),
                getDialogs(CHAT_LIST_LIMIT, 'all'),
            ]);
            data.folders = Array.isArray(foldersResp && foldersResp.folders)
                ? foldersResp.folders : [];
            data.dialogs = Array.isArray(dialogsResp && dialogsResp.dialogs)
                ? dialogsResp.dialogs : [];
            data.loaded = true;
            renderFolders();
            renderChats();
        } catch (e) {
            // Плагин выключен, нет логина, not_connected — списки нарисовать
            // нечем. Уже сохранённый выбор при этом НЕ теряется: он лежит в
            // черновике и уйдёт обратно тем же составом, если нажать Save.
            const msg = (e && e.code === 'not_authorized')
                ? 'Войдите в Telegram, чтобы выбрать чаты.'
                : `Не удалось загрузить список чатов: ${(e && e.message) || 'ошибка'}`;
            foldersBox.textContent = msg;
            chatsBox.textContent = '';
            search.disabled = true;
        }
    })();

    refreshSummary();

    // ── действия ──
    const actions = document.createElement('div');
    actions.className = 'notify-actions';

    const clearBtn = document.createElement('button');
    clearBtn.type = 'button';
    clearBtn.textContent = 'Снять всё';
    clearBtn.addEventListener('click', () => {
        draftFolders.clear();
        draftDialogs.clear();
        if (data.loaded) { renderFolders(); renderChats(); }
        refreshSummary();
    });
    actions.appendChild(clearBtn);

    const pruneBtn = document.createElement('button');
    pruneBtn.type = 'button';
    pruneBtn.textContent = 'Убрать несуществующие';
    pruneBtn.title = 'Вычистить из выбора чаты, которых нет в загруженном списке';
    pruneBtn.addEventListener('click', () => {
        if (!data.loaded) return;
        const kept = pruneMissingDialogs(Array.from(draftDialogs), data.dialogs);
        draftDialogs.clear();
        kept.forEach((id) => draftDialogs.add(id));
        renderChats();
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

        const payload = {
            telegram_filter_enabled: !!onBox.checked,
            telegram_visible_folders: Array.from(draftFolders),
            telegram_visible_dialogs: Array.from(draftDialogs),
        };

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
            // Панель кэширует разобранный фильтр по содержимому настроек, но
            // явный сброс делает контракт видимым и не зависит от внутренностей.
            invalidateTelegramFilter();
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
 * Сбрасывает кэш фильтра в панели Telegram и перерисовывает колонку.
 *
 * Через `window.ForgeApp.telegramFilter`, а не прямым импортом: настройки
 * открываются и без включённого плагина, и статический импорт тянул бы всю
 * панель в граф модалки. Отсутствие контракта — норма (вкладку ни разу не
 * открывали).
 */
function invalidateTelegramFilter() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const api = app && app.telegramFilter;
    if (api && typeof api.refresh === 'function') {
        try { api.refresh(); } catch (_) { /* перерисовка не критична */ }
    }
}
