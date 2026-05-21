// tmux-web — Notifications tab (global notifier-config).
//
// Заменяет project-specific notify-настройки на единый глобальный конфиг,
// читаемый/обновляемый через REST:
//   GET   /api/notifier-config  — текущий снапшот NotifierConfig
//   PATCH /api/notifier-config  — частичное обновление (PatchNotifierConfigReq)
//
// На бэкенде состояние хранится в ~/.config/forge/notifier.json
// (см. tmux-web/src/notifier_config.rs).

/**
 * Загружает текущий глобальный notifier-config с бэкенда.
 * При ошибке возвращает дефолты (template="", delay_minutes=0,
 * wait_previous=false, session=null) и пишет warning в console.
 */
export async function fetchNotifierConfig() {
    try {
        const r = await fetch('/api/notifier-config', {
            headers: { 'Accept': 'application/json' },
        });
        if (!r.ok) {
            console.warn('GET /api/notifier-config failed:', r.status);
            return { template: '', delay_minutes: 0, wait_previous: false, session: null };
        }
        return await r.json();
    } catch (e) {
        console.warn('fetchNotifierConfig failed', e);
        return { template: '', delay_minutes: 0, wait_previous: false, session: null };
    }
}

/**
 * Сохраняет частичное обновление notifier-config через PATCH.
 * Семантика `session`: пустая строка после trim сбрасывает session в None
 * на бэкенде (см. patch_notifier_config / PatchNotifierConfigReq).
 */
export async function saveNotifierConfig(patch) {
    try {
        const r = await fetch('/api/notifier-config', {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(patch),
        });
        if (!r.ok) {
            const text = await r.text();
            return { ok: false, error: text || ('HTTP ' + r.status) };
        }
        const updated = await r.json();
        return { ok: true, config: updated };
    } catch (e) {
        return { ok: false, error: e && e.message ? e.message : String(e) };
    }
}

/**
 * Строит форму глобальных notifier-настроек. Принимает текущий снапшот
 * config (или пустой объект) и опционально callback onSaved(updated).
 *
 * Поддерживаемые поля:
 *   - template          (textarea)
 *   - delay_minutes     (number, ≥0)
 *   - wait_previous     (checkbox)
 *   - session           (text; пустая строка ⇒ сброс в None)
 */
export function buildNotificationsForm(config, onSaved) {
    const cfg = config || {};
    const root = document.createElement('div');
    root.className = 'notifier-global';

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Глобальные настройки notifier. Шаблон: плейсхолдеры {id} {title} {description} {priority} {type}. ' +
        'delay_minutes=0 — отправлять сразу; wait_previous переопределяет delay ' +
        '(сообщение уходит после закрытия предыдущей задачи в той же сессии). ' +
        'session — дефолтная tmux-сессия для notify (пусто = должна быть указана в promote_todo).';
    root.appendChild(hint);

    const tplWrap = document.createElement('label');
    tplWrap.className = 'notify-field';
    tplWrap.textContent = 'Template';
    const tpl = document.createElement('textarea');
    tpl.className = 'notify-template';
    tpl.rows = 3;
    tpl.placeholder = 'task: {title}\n{description}';
    tpl.value = typeof cfg.template === 'string' ? cfg.template : '';
    tpl.title = 'Шаблон. Поддержка плейсхолдеров: {id} {title} {description} {priority} {type}.';
    tplWrap.appendChild(tpl);
    root.appendChild(tplWrap);

    const delayWrap = document.createElement('label');
    delayWrap.className = 'notify-field';
    delayWrap.textContent = 'Delay (minutes)';
    const delay = document.createElement('input');
    delay.type = 'number';
    delay.min = '0';
    delay.step = '1';
    delay.className = 'notify-delay';
    const delayVal = (typeof cfg.delay_minutes === 'number') ? cfg.delay_minutes : 0;
    delay.value = String(delayVal);
    delay.title = '0 — отправлять сразу. Игнорируется, если включён wait_previous.';
    delayWrap.appendChild(delay);
    root.appendChild(delayWrap);

    const waitWrap = document.createElement('label');
    waitWrap.className = 'modal-check notify-check';
    const wait = document.createElement('input');
    wait.type = 'checkbox';
    wait.className = 'notify-wait';
    wait.checked = !!cfg.wait_previous;
    wait.title = 'Ждать закрытия предыдущей задачи перед отправкой следующей. Переопределяет delay.';
    waitWrap.appendChild(wait);
    waitWrap.appendChild(document.createTextNode(' Wait for previous (overrides delay)'));
    root.appendChild(waitWrap);

    const sessWrap = document.createElement('label');
    sessWrap.className = 'notify-field';
    sessWrap.textContent = 'Default tmux session';
    const sess = document.createElement('input');
    sess.type = 'text';
    sess.className = 'notify-session';
    sess.placeholder = 'tmux session name (пусто = требовать body.session в promote_todo)';
    sess.value = (typeof cfg.session === 'string') ? cfg.session : '';
    sess.title = 'Дефолтная tmux-сессия для notify. Пусто = сбросить (сервер ждёт body.session в promote_todo).';
    sessWrap.appendChild(sess);
    root.appendChild(sessWrap);

    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    root.appendChild(err);

    const actions = document.createElement('div');
    actions.className = 'notify-actions';
    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    saveBtn.addEventListener('click', async () => {
        err.style.display = 'none';
        err.textContent = '';
        saveBtn.disabled = true;

        const rawDelay = parseInt(delay.value, 10);
        const safeDelay = Number.isFinite(rawDelay) && rawDelay >= 0 ? rawDelay : 0;
        const patch = {
            template: String(tpl.value || ''),
            delay_minutes: safeDelay,
            wait_previous: !!wait.checked,
            session: String(sess.value || '').trim(),
        };

        const result = await saveNotifierConfig(patch);
        saveBtn.disabled = false;
        if (result.ok) {
            if (typeof onSaved === 'function') onSaved(result.config);
        } else {
            err.style.display = '';
            err.textContent = result.error || 'Не удалось сохранить настройки.';
        }
    });
    actions.appendChild(saveBtn);
    root.appendChild(actions);

    return root;
}
