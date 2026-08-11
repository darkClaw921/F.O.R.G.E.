// tmux-web — Settings modal: форма «Горячие клавиши».
//
// Форма ОБЩАЯ для панелей-мессенджеров и параметризуется конфигом:
//
//   buildHotkeysForm(settings, onSaved, config)
//   config = { legend, hint, actions, groups, bindingRows, overridesFromBindings,
//              comboFromEvent, normalizeCombo, enabledKey, mapKey, invalidate }
//
// Готовые конфиги: [`TELEGRAM_HOTKEYS_CONFIG`] и [`VK_HOTKEYS_CONFIG`].
//
// ── Почему параметризация, а не вторая копия формы
//
// Для VK эта форма отличается ровно тремя вещами: источник реестра действий,
// имена двух полей настроек и текст легенды. Копия дала бы ТРЕТЬЮ точку правки
// при каждом изменении UX записи комбинаций (а он уже переживал два подхода:
// отмену по Escape и «дефолт хранится как отсутствие записи»). Это
// единственное место общего существующего кода, которое проект VK трогает
// намеренно; изменение мгновенно проверяется глазами в модалке и не касается
// ни одного модуля панели.
//
// ── Что делает форма
//
//   • тумблер `<enabledKey>` (bool, default **true**);
//   • таблица действий с текущими комбинациями и их переназначением;
//   • сброс одного действия и всех сразу.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   telegram_hotkeys_enabled / vk_hotkeys_enabled — bool, default true
//   telegram_hotkeys         / vk_hotkeys         — map «id действия» → «комбинация»
//
// ВАЖНО про дефолт тумблера: инициализация строго `!== false`, а НЕ `=== true`
// (как у соседей в interface-tab.js). У тех фич дефолт выключенный, у этих —
// включённый: копирование их идиомы показывало бы фичу выключенной всем, у
// кого настройка ещё ни разу не сохранялась.
//
// Save шлёт только свои два поля: PATCH применяет лишь Some(..)-варианты.
// Карта хоткеев заменяется ЦЕЛИКОМ (см. PatchUserSettingsReq) — иначе снять
// переопределение было бы нечем.
//
// ── Про CSS-классы `tg-hotkeys-*`
//
// Они остались историческими намеренно. С параметризацией форма стала общей, и
// её оформление (`css/settings-modal.css`) — тоже одно на обе панели.
// Переименование в нейтральное потребовало бы синхронной правки блока правил
// ради косметики; если оно всё же понадобится, менять надо ДВА места: этот
// файл и один блок в settings-modal.css.

import {
    TG_ACTIONS, TG_ACTION_GROUPS, bindingRows as tgBindingRows,
    comboFromEvent as tgComboFromEvent, normalizeCombo as tgNormalizeCombo,
    overridesFromBindings as tgOverridesFromBindings,
} from '../telegram/keys.js';
import {
    VK_ACTIONS, VK_ACTION_GROUPS, bindingRows as vkBindingRows,
    comboFromEvent as vkComboFromEvent, normalizeCombo as vkNormalizeCombo,
    overridesFromBindings as vkOverridesFromBindings,
} from '../vk/keys.js';
import {
    B24_ACTIONS, B24_ACTION_GROUPS, bindingRows as b24BindingRows,
    comboFromEvent as b24ComboFromEvent, normalizeCombo as b24NormalizeCombo,
    overridesFromBindings as b24OverridesFromBindings,
} from '../bitrix/keys.js';
import { updateUserSettings } from './user-settings-api.js';

/** Текст на кнопке в режиме записи. */
const RECORDING_LABEL = 'жмите клавишу…';

/**
 * Сбрасывает кэш разобранных привязок в панели.
 *
 * Через `window.ForgeApp.<contract>`, а не прямым импортом keynav.js:
 * настройки открываются и без включённого плагина, и статический импорт тянул
 * бы всю панель в граф модалки. Отсутствие контракта — норма (модули панели
 * могли ещё не загрузиться).
 *
 * @param {string} contract — имя контракта в window.ForgeApp
 */
function invalidatePanelBindings(contract) {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const api = app && app[contract];
    if (api && typeof api.invalidate === 'function') {
        try { api.invalidate(); } catch (_) { /* кэш не критичен */ }
    }
}

/** Конфиг формы для панели Telegram. */
export const TELEGRAM_HOTKEYS_CONFIG = {
    legend: 'Горячие клавиши Telegram',
    hint: 'Клавиатурная навигация по панели Telegram: выбор чата, чтение, ответ, '
        + 'кнопки ботов — без мыши. Работает только когда вкладка Telegram '
        + 'открыта и фокус внутри неё; в поле ввода одиночные клавиши не '
        + 'перехватываются. Внутри панели шпаргалку открывает «?».',
    toggleLabel: ' Клавиатурная навигация в Telegram',
    actions: TG_ACTIONS,
    groups: TG_ACTION_GROUPS,
    bindingRows: tgBindingRows,
    overridesFromBindings: tgOverridesFromBindings,
    comboFromEvent: tgComboFromEvent,
    normalizeCombo: tgNormalizeCombo,
    enabledKey: 'telegram_hotkeys_enabled',
    mapKey: 'telegram_hotkeys',
    invalidate: () => invalidatePanelBindings('telegramKeys'),
};

/** Конфиг формы для панели VK. */
export const VK_HOTKEYS_CONFIG = {
    legend: 'Горячие клавиши VK',
    hint: 'Клавиатурная навигация по панели VK: выбор беседы, чтение, ответ, '
        + 'вложения, кнопки сообществ — без мыши. Работает только когда вкладка '
        + 'VK открыта и фокус внутри неё; в поле ввода одиночные клавиши не '
        + 'перехватываются. Внутри панели шпаргалку открывает «?».',
    toggleLabel: ' Клавиатурная навигация в VK',
    actions: VK_ACTIONS,
    groups: VK_ACTION_GROUPS,
    bindingRows: vkBindingRows,
    overridesFromBindings: vkOverridesFromBindings,
    comboFromEvent: vkComboFromEvent,
    normalizeCombo: vkNormalizeCombo,
    enabledKey: 'vk_hotkeys_enabled',
    mapKey: 'vk_hotkeys',
    invalidate: () => invalidatePanelBindings('vkKeys'),
};

/**
 * Конфиг формы для панели «Битрикс24».
 *
 * Третий конфиг — и по-прежнему НИ ОДНОЙ строки самой формы не скопировано:
 * ровно ради этого она и параметризована. Отличий снова три — реестр действий,
 * имена двух полей настроек и тексты.
 */
export const B24_HOTKEYS_CONFIG = {
    legend: 'Горячие клавиши Битрикс24',
    hint: 'Клавиатурная навигация по панели Битрикс24: выбор диалога, чтение, '
        + 'ответ и правка, вложения, действия над задачей — без мыши. Работает '
        + 'только когда вкладка Битрикс24 открыта и фокус внутри неё; в поле '
        + 'ввода одиночные клавиши не перехватываются. Внутри панели шпаргалку '
        + 'открывает «?».',
    toggleLabel: ' Клавиатурная навигация в Битрикс24',
    actions: B24_ACTIONS,
    groups: B24_ACTION_GROUPS,
    bindingRows: b24BindingRows,
    overridesFromBindings: b24OverridesFromBindings,
    comboFromEvent: b24ComboFromEvent,
    normalizeCombo: b24NormalizeCombo,
    enabledKey: 'bitrix_hotkeys_enabled',
    mapKey: 'bitrix_hotkeys',
    invalidate: () => invalidatePanelBindings('b24Keys'),
};

/**
 * Строит форму.
 *
 * @param {?object} settings — user settings (может быть null/{})
 * @param {?function(object): void} onSaved — вызывается с обновлёнными настройками
 * @param {?object} config — один из [`TELEGRAM_HOTKEYS_CONFIG`] / [`VK_HOTKEYS_CONFIG`]
 * @returns {HTMLElement}
 */
export function buildHotkeysForm(settings, onSaved, config) {
    // Конфиг по умолчанию — телеграмный: единственный вызов формы до
    // параметризации был именно телеграмным, и промолчавший про конфиг
    // вызывающий обязан получить прежнее поведение, а не пустую таблицу.
    const cfg = config || TELEGRAM_HOTKEYS_CONFIG;
    const s = settings || {};

    // Рабочая копия переопределений: правки живут здесь до нажатия Save, чтобы
    // закрытая без сохранения модалка ничего не меняла.
    const draft = new Map();
    const initial = (s[cfg.mapKey] && typeof s[cfg.mapKey] === 'object')
        ? s[cfg.mapKey]
        : {};
    for (const [id, combo] of Object.entries(initial)) {
        const canon = cfg.normalizeCombo(combo);
        if (canon) draft.set(id, canon);
    }

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = cfg.legend;
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent = cfg.hint;
    fs.appendChild(hint);

    // ── тумблер ──
    const onWrap = document.createElement('label');
    onWrap.className = 'modal-check notify-check';
    const onBox = document.createElement('input');
    onBox.type = 'checkbox';
    onBox.className = 'tg-hotkeys-enabled';
    // Строго `!== false`: дефолт этой настройки — ВКЛЮЧЕНО (см. шапку файла).
    onBox.checked = s[cfg.enabledKey] !== false;
    onBox.title = 'Выключение возвращает клавиши глобальной vim-навигации '
        + '(j/k по сессиям), переназначения при этом сохраняются.';
    onWrap.appendChild(onBox);
    onWrap.appendChild(document.createTextNode(cfg.toggleLabel || ' Клавиатурная навигация'));
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

    /**
     * Строки в порядке объявленных групп.
     *
     * Порядок берётся из `config.groups`, а не из порядка реестра: реестр может
     * перечислить действия иначе, и заголовок группы тогда появился бы дважды.
     * Действие с неизвестной группой не выбрасывается — уезжает в «Прочее».
     */
    const orderedRows = () => {
        const rows = cfg.bindingRows(mapToObject(draft));
        const known = new Set(cfg.groups || []);
        const out = [];
        for (const name of (cfg.groups || [])) {
            for (const row of rows) if (row.group === name) out.push(row);
        }
        for (const row of rows) if (!known.has(row.group)) out.push(row);
        return out;
    };

    const render = () => {
        table.innerHTML = '';
        const rows = orderedRows();
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

            // Четвёртая колонка сетки. Ячейка создаётся ВСЕГДА, даже пустая:
            // строка без примечания иначе теряла бы колонку, и клавиши
            // соседних строк переставали выравниваться по вертикали.
            const note = document.createElement('span');
            if (row.conflictWith) {
                conflicts += 1;
                line.classList.add('tg-hotkeys-row-conflict');
                note.className = 'tg-hotkeys-warn';
                const other = cfg.actions.find((a) => a.id === row.conflictWith);
                note.textContent = `занято: ${other ? other.title : row.conflictWith}`;
            } else {
                note.className = 'tg-hotkeys-hint';
                note.textContent = row.hint || '';
            }
            // Текст обрезается многоточием — полный вид остаётся в подсказке.
            if (note.textContent) note.title = note.textContent;
            line.appendChild(note);

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
            const combo = cfg.comboFromEvent(e);
            // Нажат один модификатор — ждём дальше, это ещё не комбинация.
            if (!combo) return;
            e.preventDefault();
            e.stopPropagation();
            const action = cfg.actions.find((a) => a.id === actionId);
            stopRecording();
            if (action && cfg.normalizeCombo(action.combo) === combo) {
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
            [cfg.enabledKey]: !!onBox.checked,
            // Только отличия от дефолтов: см. hotkeys-core::overridesFromBindings.
            [cfg.mapKey]: cfg.overridesFromBindings(draft),
        };

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
            // Панель кэширует разобранные привязки. Кэш и сам заметит новую
            // карту (ключ считается по содержимому), но явный сброс делает
            // контракт видимым и не зависит от его внутренностей.
            if (typeof cfg.invalidate === 'function') cfg.invalidate();
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
