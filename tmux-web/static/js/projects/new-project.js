// tmux-web — New project modal (Phase 1 ES Modules refactor)
//
// 1:1 копия openNewProjectModal из IIFE `tmux-web/static/app.js` (3868).

import { buildModalOverlay } from '../core/utils.js';
import { fetchProjects } from './projects.js';

export function openNewProjectModal() {
    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card';

    card.innerHTML = `
        <h2>New project</h2>
        <label>Name<br><input type="text" id="np-name" placeholder="my-project"></label>
        <label>Path<br><input type="text" id="np-path" placeholder="/Users/me/work/my-project"></label>
        <label class="modal-check"><input type="checkbox" id="np-init" checked> Initialize (mkdir + git init + br init + scaffold)</label>
        <div class="modal-actions">
            <button type="button" id="np-cancel">Cancel</button>
            <button type="button" id="np-create" class="primary">Create</button>
        </div>
    `;
    overlay.appendChild(card);
    document.body.appendChild(overlay);

    const $name = card.querySelector('#np-name');
    const $path = card.querySelector('#np-path');
    const $init = card.querySelector('#np-init');
    const $cancel = card.querySelector('#np-cancel');
    const $create = card.querySelector('#np-create');

    $name.focus();

    const close = () => overlay.remove();
    $cancel.addEventListener('click', close);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });
    $create.addEventListener('click', async () => {
        const name = ($name.value || '').trim();
        const path = ($path.value || '').trim();
        if (!name || !path) {
            window.alert('Заполни name и path');
            return;
        }
        const url = $init.checked ? '/api/projects/init' : '/api/projects';
        try {
            const r = await fetch(url, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ name, path }),
            });
            if (!r.ok) {
                const text = await r.text();
                window.alert('Создание не удалось: ' + (text || r.status));
                return;
            }
            close();
            await fetchProjects();
        } catch (e) {
            window.alert('Ошибка запроса: ' + e.message);
        }
    });
}
