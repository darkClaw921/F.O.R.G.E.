// tmux-web — Notifications form + saveProjectSettings
// (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - buildNotificationsForm  (app.js:5391)
//   - saveProjectSettings     (app.js:5530)

import { state } from '../core/state.js';

export function buildNotificationsForm(project, onSaved) {
    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Notifications';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Шаблон: плейсхолдеры {id} {title} {description} {priority} {type}. ' +
        'delay_minutes=0 — отправлять сразу; wait_previous переопределяет delay ' +
        '(сообщение уходит после закрытия предыдущей задачи в той же сессии).';
    fs.appendChild(hint);

    const tplWrap = document.createElement('label');
    tplWrap.className = 'notify-field';
    tplWrap.textContent = 'Template';
    const tpl = document.createElement('textarea');
    tpl.className = 'notify-template';
    tpl.rows = 3;
    tpl.placeholder = 'task: {title}\n{description}';
    tpl.value = (project && typeof project.notify_template === 'string')
        ? project.notify_template
        : '';
    tpl.title = 'Шаблон. Поддержка плейсхолдеров: {id} {title} {description} {priority} {type}.';
    tplWrap.appendChild(tpl);
    fs.appendChild(tplWrap);

    const delayWrap = document.createElement('label');
    delayWrap.className = 'notify-field';
    delayWrap.textContent = 'Delay (minutes)';
    const delay = document.createElement('input');
    delay.type = 'number';
    delay.min = '0';
    delay.step = '1';
    delay.className = 'notify-delay';
    const delayVal = (project && typeof project.notify_delay_minutes === 'number')
        ? project.notify_delay_minutes
        : 0;
    delay.value = String(delayVal);
    delay.title = '0 — отправлять сразу. Игнорируется, если включён wait_previous.';
    delayWrap.appendChild(delay);
    fs.appendChild(delayWrap);

    const waitWrap = document.createElement('label');
    waitWrap.className = 'modal-check notify-check';
    const wait = document.createElement('input');
    wait.type = 'checkbox';
    wait.className = 'notify-wait';
    wait.checked = !!(project && project.notify_wait_previous);
    wait.title = 'Ждать закрытия предыдущей задачи перед отправкой следующей. Переопределяет delay.';
    waitWrap.appendChild(wait);
    const waitText = document.createTextNode(' Wait for previous (overrides delay)');
    waitWrap.appendChild(waitText);
    fs.appendChild(waitWrap);

    const sessWrap = document.createElement('label');
    sessWrap.className = 'notify-field';
    sessWrap.textContent = 'Session override';
    const sess = document.createElement('input');
    sess.type = 'text';
    sess.className = 'notify-session';
    sess.placeholder = 'override: tmux session name (пусто = текущая сессия проекта)';
    sess.value = (project && typeof project.notify_session === 'string')
        ? project.notify_session
        : '';
    sess.title = 'Если задано — все нотификации этого проекта пойдут в указанную сессию.';
    sessWrap.appendChild(sess);
    fs.appendChild(sessWrap);

    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

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
        const rawSess = String(sess.value || '').trim();
        const payload = {
            notify_template: String(tpl.value || ''),
            notify_delay_minutes: safeDelay,
            notify_wait_previous: !!wait.checked,
            notify_session: rawSess === '' ? null : rawSess,
        };

        const result = await saveProjectSettings(project.id, payload);
        saveBtn.disabled = false;
        if (result.ok) {
            if (typeof onSaved === 'function') onSaved();
        } else {
            err.style.display = '';
            err.textContent = result.error || 'Не удалось сохранить настройки.';
        }
    });
    actions.appendChild(saveBtn);
    fs.appendChild(actions);

    return fs;
}

export async function saveProjectSettings(projectId, payload) {
    if (!projectId) {
        return { ok: false, error: 'no project id' };
    }

    const idx = Array.isArray(state.projects)
        ? state.projects.findIndex((p) => p && p.id === projectId)
        : -1;
    const prev = (idx >= 0) ? state.projects[idx] : null;
    if (idx >= 0 && prev) {
        state.projects[idx] = Object.assign({}, prev, {
            notify_template: payload.notify_template,
            notify_delay_minutes: payload.notify_delay_minutes,
            notify_wait_previous: payload.notify_wait_previous,
            notify_session: payload.notify_session,
        });
    }

    try {
        const r = await fetch('/api/projects/' + encodeURIComponent(projectId) + '/settings', {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
        });
        if (!r.ok) {
            if (idx >= 0 && prev) {
                state.projects[idx] = prev;
            }
            const text = await r.text();
            return { ok: false, error: text || ('HTTP ' + r.status) };
        }
        const updated = await r.json();
        if (idx >= 0) {
            state.projects[idx] = updated;
        } else if (Array.isArray(state.projects)) {
            state.projects.push(updated);
        }
        return { ok: true, project: updated };
    } catch (e) {
        if (idx >= 0 && prev) {
            state.projects[idx] = prev;
        }
        return { ok: false, error: e && e.message ? e.message : String(e) };
    }
}
