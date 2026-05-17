// tmux-web — Echo action buttons (Phase 5c)
//
// Рендерит ActionDescriptor[] под assistant-message и обрабатывает клики:
//   - prompt-action  → вызывает invokeCb сразу (фронт сам решит как —
//                       обычно sendActionInvoke / sendUserMessage).
//   - system-action  → показывает confirmation modal (Cancel / Run),
//                       и только при подтверждении вызывает invokeCb.
//
// API:
//   renderActionButtons(messageEl, descriptors, invokeCb)
//   showConfirmationModal({ label, name, params }) -> Promise<boolean>

const PROMPT_KINDS = new Set(['prompt']);

/**
 * Найти или создать контейнер кнопок под сообщением.
 */
function getButtonsContainer(messageEl) {
    let c = messageEl.querySelector('.echo-action-buttons');
    if (c) {
        c.innerHTML = '';
        return c;
    }
    c = document.createElement('div');
    c.className = 'echo-action-buttons';
    messageEl.appendChild(c);
    return c;
}

/**
 * Отрендерить кнопки.
 *
 * @param {HTMLElement} messageEl
 * @param {Array<{id, label, kind, params}>} descriptors
 * @param {(action) => void} invokeCb — вызывается после (для system —
 *   после confirmation) с тем же descriptor'ом.
 */
export function renderActionButtons(messageEl, descriptors, invokeCb) {
    if (!messageEl || !Array.isArray(descriptors) || descriptors.length === 0) return;
    const container = getButtonsContainer(messageEl);
    for (const d of descriptors) {
        container.appendChild(buildButton(d, invokeCb));
    }
}

function buildButton(descriptor, invokeCb) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'echo-action-btn';
    btn.dataset.actionId = descriptor.id;
    btn.dataset.kind = descriptor.kind;
    const isSystem = !PROMPT_KINDS.has(descriptor.kind);
    if (isSystem) {
        btn.classList.add('echo-action-btn-system');
        const warn = document.createElement('span');
        warn.className = 'echo-action-warn';
        warn.textContent = '⚠';
        warn.title = 'System action — требует подтверждения';
        btn.appendChild(warn);
    }
    const label = document.createElement('span');
    label.className = 'echo-action-label';
    label.textContent = descriptor.label || descriptor.id;
    btn.appendChild(label);

    btn.addEventListener('click', async () => {
        if (isSystem) {
            const ok = await showConfirmationModal(descriptor);
            if (!ok) return;
        }
        try { invokeCb(descriptor); } catch (e) { console.warn('[echo-actions] invoke threw', e); }
    });
    return btn;
}

/**
 * Confirmation modal для system-actions.
 *
 * @param {{label, kind, params}} descriptor
 * @returns {Promise<boolean>}
 */
export function showConfirmationModal(descriptor) {
    return new Promise((resolve) => {
        const overlay = document.createElement('div');
        overlay.className = 'echo-modal-overlay echo-confirm-overlay';
        const modal = document.createElement('div');
        modal.className = 'echo-modal echo-confirm-modal';
        overlay.appendChild(modal);

        const h = document.createElement('h3');
        h.textContent = 'Confirm system action';
        modal.appendChild(h);

        const lbl = document.createElement('p');
        lbl.className = 'echo-confirm-label';
        lbl.textContent = descriptor.label || descriptor.id;
        modal.appendChild(lbl);

        const meta = document.createElement('div');
        meta.className = 'echo-confirm-meta';
        meta.textContent = `kind: ${descriptor.kind}`;
        modal.appendChild(meta);

        const pre = document.createElement('pre');
        pre.className = 'echo-confirm-params';
        try {
            pre.textContent = JSON.stringify(descriptor.params || {}, null, 2);
        } catch (e) {
            pre.textContent = String(descriptor.params || '');
        }
        modal.appendChild(pre);

        const buttons = document.createElement('div');
        buttons.className = 'echo-modal-buttons';
        const cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.textContent = 'Cancel';
        const run = document.createElement('button');
        run.type = 'button';
        run.textContent = 'Run';
        run.className = 'primary';
        buttons.appendChild(cancel);
        buttons.appendChild(run);
        modal.appendChild(buttons);

        function done(ok) {
            overlay.removeEventListener('click', onOverlay);
            document.removeEventListener('keydown', onKey);
            try { overlay.remove(); } catch (_) {}
            resolve(ok);
        }
        function onOverlay(ev) {
            if (ev.target === overlay) done(false);
        }
        function onKey(ev) {
            if (ev.key === 'Escape') done(false);
            if (ev.key === 'Enter') done(true);
        }
        cancel.addEventListener('click', () => done(false));
        run.addEventListener('click', () => done(true));
        overlay.addEventListener('click', onOverlay);
        document.addEventListener('keydown', onKey);

        document.body.appendChild(overlay);
        // Сразу фокус на Cancel — безопасный дефолт.
        cancel.focus();
    });
}
