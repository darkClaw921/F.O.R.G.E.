// tmux-web — Settings modal: вкладка «Горячие клавиши» (Telegram).
//
// Экспортирует buildHotkeysForm(settings, onSaved) — фабрика DOM-узла с:
//   • тумблером telegram_hotkeys_enabled (bool, default **true**);
//   • таблицей действий с текущими комбинациями и их переназначением;
//   • сбросом одного действия и всех сразу.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   telegram_hotkeys_enabled — bool, default true
//   telegram_hotkeys         — map «id действия» → «комбинация», default пустая
//
// ВАЖНО про дефолт тумблера: инициализация строго `!== false`, а НЕ `=== true`
// (как у соседей в interface-tab.js). У тех фич дефолт выключенный, у этой —
// включённый: копирование их идиомы показывало бы фичу выключенной всем, у
// кого настройка ещё ни разу не сохранялась.
//
// Save шлёт только свои два поля: PATCH применяет лишь Some(..)-варианты.
// Карта хоткеев заменяется ЦЕЛИКОМ (см. PatchUserSettingsReq) — иначе снять
// переопределение было бы нечем.

import {
    TG_ACTIONS, bindingRows, comboFromEvent, formatCombo, normalizeCombo,
    overridesFromBindings,
} from '../telegram/keys.js';
import { updateUserSettings } from './user-settings-api.js';

/** Текст на кнопке в режиме записи. */
const RECORDING_LABEL = 'жмите клавишу…';

/**
 * Строит форму.
 *
 * @param {?object} settings — user settings (может быть null/{})
 * @param {?function(object): void} onSaved — вызывается с обновлёнными настройками
 * @returns {HTMLElement}
 */
export function buildHotkeysForm(settings, onSaved) {
    const s = settings || {};

    // Рабочая копия переопределений: правки живут здесь до нажатия Save, чтобы
    // закрытая без сохранения модалка ничего не меняла.
    const draft = new Map();
    const initial = (s.telegram_hotkeys && typeof s.telegram_hotkeys === 'object')
        ? s.telegram_hotkeys
        : {};
    for (const [id, combo] of Object.entries(initial)) {
        const canon = normalizeCombo(combo);
        if (canon) draft.set(id, canon);
    }

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Горячие клавиши Telegram';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Клавиатурная навигация по панели Telegram: выбор чата, чтение, ответ, '
        + 'кнопки ботов — без мыши. Работает только когда вкладка Telegram '
        + 'открыта и фокус внутри неё; в поле ввода одиночные клавиши не '
        + 'перехватываются. Внутри панели шпаргалку открывает «?».';
    fs.appendChild(hint);

    // ── тумблер ──
    const onWrap = document.createElement('label');
    onWrap.className = 'modal-check notify-check';
    const onBox = document.createElement('input');
    onBox.type = 'checkbox';
    onBox.className = 'tg-hotkeys-enabled';
    onBox.checked = s.telegram_hotkeys_enabled !== false;
    onBox.title = 'Выключение возвращает клавиши глобальной vim-навигации '
        + '(j/k по сессиям), переназначения при этом сохраняются.';
    onWrap.appendChild(onBox);
    onWrap.appendChild(document.createTextNode(' Клавиатурная навигация в Telegram'));
    fs.appendChild(onWrap);

    // ── таблица действий ──
    const table = document.createElement('div');
    table.className = 'tg-hotkeys-table';
    fs.appendChild(table);

    const conflictNote = document.createElement('div');
    conflictNote.className = 'notify-error tg-hotkeys-conflict';
    conflictNote.style.display = 'none';
    fs.appendChild(conflictNote);

    // Запись комбинации: одновременно активна не более одной строки.
    const recording = { id: null, stop: null };

    const stopRecording = () => {
        if (typeof recording.stop === 'function') recording.stop();
        recording.id = null;
        recording.stop = null;
    };

    const render = () => {
        table.innerHTML = '';
        const rows = bindingRows(mapToObject(draft));
        let lastGroup = null;
        let conflicts = 0;

        for (const row of rows) {
            if (row.group !== lastGroup) {
                lastGroup = row.group;
                const head = document.createElement('div');
                head.className = 'tg-hotkeys-group';
                head.textContent = row.group;
                table.appendChild(head);
            }

            const line = document.createElement('div');
            line.className = 'tg-hotkeys-row';

            const label = document.createElement('span');
            label.className = 'tg-hotkeys-title';
            label.textContent = row.title;
            line.appendChild(label);

            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'tg-hotkeys-combo';
            btn.textContent = row.combo ? row.label : '—';
            if (row.fixed) {
                // Escape — единственный выход из режима; отдать его под
                // переназначение значит позволить запереть себя внутри.
                btn.disabled = true;
                btn.title = row.hint || 'Не переназначается';
                btn.classList.add('tg-hotkeys-fixed');
            } else {
                btn.title = 'Нажмите, затем введите новую комбинацию';
                btn.addEventListener('click', () => beginRecording(row.id, btn));
            }
            line.appendChild(btn);

            const reset = document.createElement('button');
            reset.type = 'button';
            reset.className = 'tg-hotkeys-reset';
            reset.textContent = '⟲';
            reset.title = 'Вернуть клавишу по умолчанию';
            reset.disabled = row.fixed || row.isDefault;
            reset.addEventListener('click', () => {
                stopRecording();
                draft.delete(row.id);
                render();
            });
            line.appendChild(reset);

            if (row.conflictWith) {
                conflicts += 1;
                line.classList.add('tg-hotkeys-row-conflict');
                const warn = document.createElement('span');
                warn.className = 'tg-hotkeys-warn';
                const other = TG_ACTIONS.find((a) => a.id === row.conflictWith);
                warn.textContent = `занято: ${other ? other.title : row.conflictWith}`;
                line.appendChild(warn);
            } else if (row.hint) {
                const h = document.createElement('span');
                h.className = 'tg-hotkeys-hint';
                h.textContent = row.hint;
                line.appendChild(h);
            }

            table.appendChild(line);
        }

        if (conflicts > 0) {
            conflictNote.style.display = '';
            conflictNote.textContent = conflicts === 1
                ? 'Одно действие осталось без клавиши: комбинация занята другим.'
                : `Действий без клавиши: ${conflicts} — их комбинации заняты другими.`;
        } else {
            conflictNote.style.display = 'none';
            conflictNote.textContent = '';
        }
    };

    /**
     * Ловит следующее нажатие и записывает его в черновик.
     *
     * Слушатель ставится в capture на document и снимается при первом же
     * событии: оставленный висеть, он перехватывал бы клавиши всей страницы.
     */
    function beginRecording(actionId, btn) {
        stopRecording();
        recording.id = actionId;
        btn.classList.add('tg-hotkeys-recording');
        btn.textContent = RECORDING_LABEL;

        const onKey = (e) => {
            // Escape — отмена записи, а не назначение: иначе первая же ошибка
            // отняла бы у пользователя выход.
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                stopRecording();
                render();
                return;
            }
            const combo = comboFromEvent(e);
            // Нажат один модификатор — ждём дальше, это ещё не комбинация.
            if (!combo) return;
            e.preventDefault();
            e.stopPropagation();
            const action = TG_ACTIONS.find((a) => a.id === actionId);
            stopRecording();
            if (action && normalizeCombo(action.combo) === combo) {
                // Назначили ровно дефолт — храним как «без переопределения»,
                // чтобы смена дефолта в будущей версии доехала до пользователя.
                draft.delete(actionId);
            } else {
                draft.set(actionId, combo);
            }
            render();
        };

        document.addEventListener('keydown', onKey, true);
        recording.stop = () => {
            document.removeEventListener('keydown', onKey, true);
            btn.classList.remove('tg-hotkeys-recording');
        };
    }

    render();

    // ── действия ──
    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    const ok = document.createElement('div');
    ok.className = 'notify-hint tg-hotkeys-save-ok';
    ok.style.display = 'none';
    ok.textContent = 'Сохранено';
    fs.appendChild(ok);

    const actions = document.createElement('div');
    actions.className = 'notify-actions';

    const resetAll = document.createElement('button');
    resetAll.type = 'button';
    resetAll.textContent = 'Сбросить всё';
    resetAll.addEventListener('click', () => {
        stopRecording();
        draft.clear();
        render();
    });
    actions.appendChild(resetAll);

    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    saveBtn.addEventListener('click', async () => {
        stopRecording();
        err.style.display = 'none';
        err.textContent = '';
        ok.style.display = 'none';
        saveBtn.disabled = true;

        const payload = {
            telegram_hotkeys_enabled: !!onBox.checked,
            // Только отличия от дефолтов: см. keys.js::overridesFromBindings.
            telegram_hotkeys: overridesFromBindings(draft),
        };

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
            // Панель Telegram кэширует разобранные привязки. Кэш и сам
            // заметит новую карту (ключ считается по содержимому), но явный
            // сброс делает контракт видимым и не зависит от его внутренностей.
            invalidateTelegramBindings();
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

/** Map → plain-объект (JSON-совместимый). */
function mapToObject(map) {
    const out = {};
    for (const [k, v] of map.entries()) out[k] = v;
    return out;
}

/**
 * Сбрасывает кэш привязок в панели Telegram.
 *
 * Через `window.ForgeApp.telegramKeys`, а не прямым импортом keynav.js:
 * настройки открываются и без включённого плагина Telegram, и статический
 * импорт тянул бы всю панель в граф модалки. Отсутствие контракта — норма.
 */
function invalidateTelegramBindings() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const api = app && app.telegramKeys;
    if (api && typeof api.invalidate === 'function') {
        try { api.invalidate(); } catch (_) { /* кэш не критичен */ }
    }
}
