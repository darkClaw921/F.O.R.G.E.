// tmux-web — Settings modal: TODO behavior tab (Phase 2 — user-settings feature)
//
// Экспортирует buildTodoBehaviorForm(settings, onSaved) — фабрика DOM-узла
// (fieldset) с 6-ю контролами пользовательских настроек поведения TODO.
// Использует тот же стиль классов, что и notifications-tab.js
// (.notify-fieldset, .notify-field, .notify-hint, .notify-error,
// .notify-actions, .modal-check) — без необходимости править CSS.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   todo_default_plan_mode        — bool (checkbox)
//   todo_default_priority         — u8 0..4 (select)
//   todo_default_issue_type       — string enum (select)
//   todo_plan_mode_suffix         — string (textarea); пусто = use default
//   todo_confirm_delete           — bool (checkbox, initial true)
//   todo_confirm_promote_on_drag  — bool (checkbox, initial false)
//
// Save отправляет полный payload через updateUserSettings (optimistic +
// rollback внутри клиента). onSaved(updatedSettings) вызывается на успехе.

import { updateUserSettings } from './user-settings-api.js';

// Дефолты, совпадающие с backend defaults в src/user_settings.rs.
// Используются как fallback, если settings === null (например, при первой
// загрузке после ошибки fetchUserSettings).
const DEFAULT_PLAN_MODE_SUFFIX = 'Создай план для этой задачи';

const PRIORITY_OPTIONS = [
    { value: 0, label: '0 — critical' },
    { value: 1, label: '1 — high' },
    { value: 2, label: '2 — medium' },
    { value: 3, label: '3 — low' },
    { value: 4, label: '4 — backlog' },
];

const ISSUE_TYPE_OPTIONS = [
    'task',
    'bug',
    'feature',
    'epic',
    'chore',
    'docs',
    'question',
];

// Строит форму. settings может быть null — тогда используются дефолты.
// onSaved(updatedSettings) — callback после успешного PATCH.
export function buildTodoBehaviorForm(settings, onSaved) {
    const s = settings || {};

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'TODO behavior';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Настройки поведения карточек TODO: дефолты создания, plan-mode суффикс, ' +
        'подтверждения для деструктивных действий.';
    fs.appendChild(hint);

    // 1) checkbox: todo_default_plan_mode
    const planWrap = document.createElement('label');
    planWrap.className = 'modal-check notify-check';
    const plan = document.createElement('input');
    plan.type = 'checkbox';
    plan.className = 'todo-default-plan-mode';
    plan.checked = !!s.todo_default_plan_mode;
    plan.title = 'Если включено — новые TODO будут создаваться с активным Plan Mode.';
    planWrap.appendChild(plan);
    planWrap.appendChild(document.createTextNode(' Включать Plan Mode по умолчанию для новых TODO'));
    fs.appendChild(planWrap);

    // 2) select: todo_default_priority
    const prWrap = document.createElement('label');
    prWrap.className = 'notify-field';
    prWrap.textContent = 'Default priority';
    const pr = document.createElement('select');
    pr.className = 'todo-default-priority';
    const prInitial = Number.isFinite(s.todo_default_priority) ? Number(s.todo_default_priority) : 2;
    for (const opt of PRIORITY_OPTIONS) {
        const o = document.createElement('option');
        o.value = String(opt.value);
        o.textContent = opt.label;
        if (opt.value === prInitial) o.selected = true;
        pr.appendChild(o);
    }
    pr.title = 'Приоритет, используемый при создании новой TODO-карточки.';
    prWrap.appendChild(pr);
    fs.appendChild(prWrap);

    // 3) select: todo_default_issue_type
    const itWrap = document.createElement('label');
    itWrap.className = 'notify-field';
    itWrap.textContent = 'Default issue type';
    const it = document.createElement('select');
    it.className = 'todo-default-issue-type';
    const itInitial = typeof s.todo_default_issue_type === 'string' && s.todo_default_issue_type
        ? s.todo_default_issue_type
        : 'task';
    for (const v of ISSUE_TYPE_OPTIONS) {
        const o = document.createElement('option');
        o.value = v;
        o.textContent = v;
        if (v === itInitial) o.selected = true;
        it.appendChild(o);
    }
    it.title = 'Тип issue по умолчанию для новых TODO.';
    itWrap.appendChild(it);
    fs.appendChild(itWrap);

    // 4) textarea: todo_plan_mode_suffix
    const sufWrap = document.createElement('label');
    sufWrap.className = 'notify-field';
    sufWrap.textContent = 'Plan mode suffix';
    const suf = document.createElement('textarea');
    suf.className = 'todo-plan-mode-suffix';
    suf.rows = 3;
    suf.placeholder = DEFAULT_PLAN_MODE_SUFFIX;
    suf.value = typeof s.todo_plan_mode_suffix === 'string' ? s.todo_plan_mode_suffix : '';
    suf.title = 'Текст, добавляемый к задаче при включённом Plan Mode. Пусто — использовать значение по умолчанию.';
    sufWrap.appendChild(suf);
    const sufHint = document.createElement('div');
    sufHint.className = 'notify-hint';
    sufHint.textContent = 'Если пусто — используется значение по умолчанию: «' + DEFAULT_PLAN_MODE_SUFFIX + '».';
    sufWrap.appendChild(sufHint);
    fs.appendChild(sufWrap);

    // 5) checkbox: todo_confirm_delete
    const delWrap = document.createElement('label');
    delWrap.className = 'modal-check notify-check';
    const del = document.createElement('input');
    del.type = 'checkbox';
    del.className = 'todo-confirm-delete';
    // initial true: если поле undefined, считаем true (дефолт).
    del.checked = s.todo_confirm_delete === undefined ? true : !!s.todo_confirm_delete;
    del.title = 'Запрашивать подтверждение при удалении TODO.';
    delWrap.appendChild(del);
    delWrap.appendChild(document.createTextNode(' Подтверждать удаление TODO'));
    fs.appendChild(delWrap);

    // 6) checkbox: todo_confirm_promote_on_drag
    const promWrap = document.createElement('label');
    promWrap.className = 'modal-check notify-check';
    const prom = document.createElement('input');
    prom.type = 'checkbox';
    prom.className = 'todo-confirm-promote-on-drag';
    // initial false: если undefined → false.
    prom.checked = s.todo_confirm_promote_on_drag === undefined ? false : !!s.todo_confirm_promote_on_drag;
    prom.title = 'Запрашивать подтверждение при переносе TODO в Open через drag-and-drop.';
    promWrap.appendChild(prom);
    promWrap.appendChild(document.createTextNode(' Подтверждать перенос TODO в Open при drag-and-drop'));
    fs.appendChild(promWrap);

    // Inline error block (hidden by default).
    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    // Inline success indicator (показывается на 2 сек после Save).
    const ok = document.createElement('div');
    ok.className = 'notify-hint todo-save-ok';
    ok.style.display = 'none';
    ok.textContent = 'Сохранено';
    fs.appendChild(ok);

    // Save button.
    const actions = document.createElement('div');
    actions.className = 'notify-actions';
    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    saveBtn.addEventListener('click', async () => {
        err.style.display = 'none';
        err.textContent = '';
        ok.style.display = 'none';
        saveBtn.disabled = true;

        const rawPriority = parseInt(pr.value, 10);
        const safePriority = Number.isFinite(rawPriority) && rawPriority >= 0 && rawPriority <= 4
            ? rawPriority
            : 2;

        const payload = {
            todo_default_plan_mode: !!plan.checked,
            todo_default_priority: safePriority,
            todo_default_issue_type: String(it.value || 'task'),
            todo_plan_mode_suffix: String(suf.value || ''),
            todo_confirm_delete: !!del.checked,
            todo_confirm_promote_on_drag: !!prom.checked,
        };

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
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
