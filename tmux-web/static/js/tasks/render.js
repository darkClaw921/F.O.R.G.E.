// tmux-web — Tasks kanban render (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - TASK_COLUMNS / COLUMN_TITLES (app.js:1859-1877)
//   - renderTasks       (app.js:3175)
//   - compareIssues     (app.js:3225)
//   - renderColumn      (app.js:3235)
//   - renderTodoCard    (app.js:3393)
//   - renderCard        (app.js:3583)
//   - currentTodosProjectId (app.js:2967)
//   - setTasksStatus    — re-exported from ws/tasks-ws.js (см. ниже)

import { state } from '../core/state.js';
import { $tasksBoard, $tasksMeta } from '../core/dom.js';
import { openCreateModal, openEditModal, openTodoEditModal } from './modals.js';
import { updateTask, promoteTodo, cleanColumn } from './crud.js';

export const TASK_COLUMNS = ['todo', 'open', 'in_progress', 'blocked', 'deferred', 'draft', 'closed'];

export const COLUMN_TITLES = {
    todo: 'TODO',
    open: 'Open',
    in_progress: 'In progress',
    blocked: 'Blocked',
    deferred: 'Deferred',
    draft: 'Draft',
    closed: 'Closed',
};

export function currentTodosProjectId() {
    return state.activeProjectId || null;
}

export function renderTasks() {
    if (!$tasksBoard) return;
    const data = state.tasksData || { issues: [], total: 0 };
    const issues = Array.isArray(data.issues) ? data.issues : [];

    const byStatus = {};
    for (const col of TASK_COLUMNS) byStatus[col] = [];
    for (const issue of issues) {
        const s = String(issue.status || '').toLowerCase();
        if (s === 'todo') continue;
        if (Object.prototype.hasOwnProperty.call(byStatus, s)) {
            byStatus[s].push(issue);
        }
    }

    const todos = Array.isArray(state.todosData) ? state.todosData.slice() : [];
    todos.sort(compareIssues);
    byStatus.todo = todos;

    for (const col of TASK_COLUMNS) {
        if (col === 'todo') continue;
        byStatus[col].sort(compareIssues);
    }

    $tasksBoard.innerHTML = '';
    for (const col of TASK_COLUMNS) {
        $tasksBoard.appendChild(renderColumn(col, byStatus[col]));
    }

    if ($tasksMeta) {
        const total = (typeof data.total === 'number') ? data.total : issues.length;
        $tasksMeta.textContent = `Total: ${total} · TODO: ${todos.length}`;
    }
}

export function compareIssues(a, b) {
    const pa = (typeof a.priority === 'number') ? a.priority : 5;
    const pb = (typeof b.priority === 'number') ? b.priority : 5;
    if (pa !== pb) return pa - pb;
    const ua = a.updated_at || '';
    const ub = b.updated_at || '';
    if (ua === ub) return 0;
    return ua < ub ? 1 : -1;
}

export function renderColumn(status, items) {
    const col = document.createElement('div');
    col.className = 'kanban-col';
    col.dataset.status = status;

    const header = document.createElement('div');
    header.className = 'kanban-col-header';
    header.dataset.status = status;
    const title = document.createElement('span');
    title.textContent = COLUMN_TITLES[status] || status;

    const right = document.createElement('span');
    right.className = 'col-meta';
    const count = document.createElement('span');
    count.className = 'col-count';
    count.textContent = String(items.length);
    right.appendChild(count);

    if (status !== 'closed') {
        const addBtn = document.createElement('button');
        addBtn.type = 'button';
        addBtn.className = 'col-add';
        addBtn.textContent = '+';
        addBtn.title = `Создать задачу со статусом ${COLUMN_TITLES[status] || status}`;
        addBtn.addEventListener('click', (ev) => {
            ev.stopPropagation();
            openCreateModal({ status });
        });
        right.appendChild(addBtn);
    }

    if (items.length > 0) {
        const cleanBtn = document.createElement('button');
        cleanBtn.type = 'button';
        cleanBtn.className = 'col-clean';
        cleanBtn.textContent = 'clean';
        const verb = (status === 'closed')
            ? 'удалить'
            : (status === 'todo' ? 'удалить TODO' : 'закрыть');
        cleanBtn.title = `Массово ${verb} все задачи колонки ${COLUMN_TITLES[status] || status}`;
        cleanBtn.addEventListener('click', async (ev) => {
            ev.stopPropagation();
            const colTitle = COLUMN_TITLES[status] || status;
            const action = (status === 'closed')
                ? `физически удалить ${items.length} задач(и) из «${colTitle}»`
                : (status === 'todo'
                    ? `удалить ${items.length} TODO из «${colTitle}»`
                    : `закрыть ${items.length} задач(и) в «${colTitle}»`);
            if (!window.confirm(`Точно ${action}? Действие необратимо.`)) return;
            cleanBtn.disabled = true;
            const ids = items.map((it) => it && it.id).filter(Boolean);
            const res = await cleanColumn(status, ids);
            cleanBtn.disabled = false;
            if (res && res.fail > 0) {
                window.alert(`Очистка завершена с ошибками: ok=${res.ok}, fail=${res.fail}`);
            }
        });
        right.appendChild(cleanBtn);
    }

    header.appendChild(title);
    header.appendChild(right);

    const body = document.createElement('div');
    body.className = 'kanban-col-body';
    body.dataset.status = status;

    const isLegitTarget = (raw) => {
        if (!raw) return false;
        const isTodo = raw.startsWith('todo:');
        if (isTodo) {
            return body.dataset.status === 'open';
        }
        return body.dataset.status !== 'todo';
    };

    body.addEventListener('dragover', (ev) => {
        if (body.dataset.status === 'todo') {
            if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'none';
            return;
        }
        ev.preventDefault();
        if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'move';
        body.classList.add('drop-target');
    });
    body.addEventListener('dragenter', (ev) => {
        if (body.dataset.status === 'todo') return;
        ev.preventDefault();
        body.classList.add('drop-target');
    });
    body.addEventListener('dragleave', (ev) => {
        const rel = ev.relatedTarget;
        if (rel && body.contains(rel)) return;
        body.classList.remove('drop-target');
    });
    body.addEventListener('drop', (ev) => {
        ev.preventDefault();
        body.classList.remove('drop-target');
        const raw = ev.dataTransfer ? ev.dataTransfer.getData('text/plain') : '';
        if (!raw) return;

        const targetStatus = body.dataset.status || status;
        const isTodoPayload = raw.startsWith('todo:');

        if (isTodoPayload) {
            const todoId = raw.slice('todo:'.length);
            if (!todoId) return;
            if (targetStatus !== 'open') {
                return;
            }
            const needConfirm = !!(state.userSettings && state.userSettings.todo_confirm_promote_on_drag === true);
            if (needConfirm && !window.confirm('Promote TODO в bd-задачу?')) return;
            promoteTodo(todoId);
            return;
        }

        if (targetStatus === 'todo') {
            return;
        }
        const id = raw;
        const issue = state.tasksData && Array.isArray(state.tasksData.issues)
            ? state.tasksData.issues.find((it) => it && it.id === id)
            : null;
        if (!issue) return;
        const currentStatus = String(issue.status || '').toLowerCase();
        if (currentStatus === targetStatus) return;
        updateTask(id, { status: targetStatus });
    });
    void isLegitTarget;

    if (status === 'todo') {
        for (const todo of items) {
            body.appendChild(renderTodoCard(todo));
        }
    } else {
        for (const issue of items) {
            body.appendChild(renderCard(issue));
        }
    }

    col.appendChild(header);
    col.appendChild(body);
    return col;
}

export function renderTodoCard(todo) {
    const card = document.createElement('div');
    card.className = 'kanban-card';
    card.dataset.id = todo.id || '';
    card.dataset.status = 'todo';
    const prio = (typeof todo.priority === 'number') ? todo.priority : 5;
    card.dataset.priority = String(prio);

    let dragMoved = false;
    card.draggable = true;
    card.addEventListener('dragstart', (ev) => {
        dragMoved = true;
        if (ev.dataTransfer) {
            try {
                ev.dataTransfer.setData('text/plain', 'todo:' + (todo.id || ''));
            } catch (_) {}
            ev.dataTransfer.effectAllowed = 'move';
        }
        card.classList.add('dragging');
    });
    card.addEventListener('dragend', () => {
        card.classList.remove('dragging');
        document.querySelectorAll('.kanban-col-body.drop-target')
            .forEach((el) => el.classList.remove('drop-target'));
        setTimeout(() => { dragMoved = false; }, 0);
    });

    card.addEventListener('click', (ev) => {
        if (dragMoved) return;
        const t = ev.target;
        if (t && t.classList && t.classList.contains('promote-btn')) return;
        openTodoEditModal(todo);
    });

    const titleEl = document.createElement('div');
    titleEl.className = 'title';
    titleEl.textContent = todo.title || '(untitled)';
    card.appendChild(titleEl);

    const descRaw = String(todo.description || '').trim();
    if (descRaw) {
        const descEl = document.createElement('div');
        descEl.className = 'desc';
        descEl.textContent = descRaw.length > 140 ? descRaw.slice(0, 140) + '…' : descRaw;
        card.appendChild(descEl);
    }

    const meta = document.createElement('div');
    meta.className = 'meta-row';

    const pill = document.createElement('span');
    pill.className = 'p-pill';
    pill.textContent = (prio <= 4) ? `P${prio}` : 'P?';
    meta.appendChild(pill);

    if (todo.issue_type) {
        const t = document.createElement('span');
        t.className = 'type-tag';
        t.textContent = todo.issue_type;
        meta.appendChild(t);
    }

    if (todo.plan_mode) {
        const pm = document.createElement('span');
        pm.className = 'plan-mode-badge';
        pm.title = 'Plan mode: при promote добавится «Создай план для этой задачи»';
        pm.textContent = '◆ plan';
        meta.appendChild(pm);
    }

    const promoteBtn = document.createElement('button');
    promoteBtn.type = 'button';
    promoteBtn.className = 'promote-btn';
    promoteBtn.textContent = '▲ promote';
    promoteBtn.title = 'Превратить в bd-задачу + уведомление в tmux-сессию';
    promoteBtn.addEventListener('click', (ev) => {
        ev.stopPropagation();
        promoteTodo(todo.id);
    });
    meta.appendChild(promoteBtn);

    card.appendChild(meta);

    const labels = Array.isArray(todo.labels) ? todo.labels : [];
    if (labels.length > 0) {
        const lblBox = document.createElement('div');
        lblBox.className = 'labels';
        const visible = labels.slice(0, 3);
        for (const l of visible) {
            const tag = document.createElement('span');
            tag.className = 'label-tag';
            tag.textContent = l;
            lblBox.appendChild(tag);
        }
        if (labels.length > 3) {
            const more = document.createElement('span');
            more.className = 'label-tag';
            more.textContent = '+' + (labels.length - 3);
            lblBox.appendChild(more);
        }
        card.appendChild(lblBox);
    }

    return card;
}

export function renderCard(issue) {
    const card = document.createElement('div');
    card.className = 'kanban-card';
    card.dataset.id = issue.id || '';
    card.dataset.status = String(issue.status || '').toLowerCase();
    const prio = (typeof issue.priority === 'number') ? issue.priority : 5;
    card.dataset.priority = String(prio);

    let dragMoved = false;
    card.draggable = true;
    card.addEventListener('dragstart', (ev) => {
        dragMoved = true;
        if (ev.dataTransfer) {
            try {
                ev.dataTransfer.setData('text/plain', issue.id || '');
            } catch (_) {}
            ev.dataTransfer.effectAllowed = 'move';
        }
        card.classList.add('dragging');
    });
    card.addEventListener('dragend', () => {
        card.classList.remove('dragging');
        document.querySelectorAll('.kanban-col-body.drop-target')
            .forEach((el) => el.classList.remove('drop-target'));
        setTimeout(() => { dragMoved = false; }, 0);
    });

    card.addEventListener('click', () => {
        if (dragMoved) return;
        openEditModal(issue);
    });

    const idEl = document.createElement('div');
    idEl.className = 'id';
    idEl.textContent = issue.id || '';
    card.appendChild(idEl);

    const titleEl = document.createElement('div');
    titleEl.className = 'title';
    titleEl.textContent = issue.title || '(untitled)';
    card.appendChild(titleEl);

    const meta = document.createElement('div');
    meta.className = 'meta-row';

    const pill = document.createElement('span');
    pill.className = 'p-pill';
    pill.textContent = (prio <= 4) ? `P${prio}` : 'P?';
    meta.appendChild(pill);

    if (issue.issue_type) {
        const t = document.createElement('span');
        t.className = 'type-tag';
        t.textContent = issue.issue_type;
        meta.appendChild(t);
    }
    card.appendChild(meta);

    const labels = Array.isArray(issue.labels) ? issue.labels : [];
    if (labels.length > 0) {
        const lblBox = document.createElement('div');
        lblBox.className = 'labels';
        const visible = labels.slice(0, 3);
        for (const l of visible) {
            const tag = document.createElement('span');
            tag.className = 'label-tag';
            tag.textContent = l;
            lblBox.appendChild(tag);
        }
        if (labels.length > 3) {
            const more = document.createElement('span');
            more.className = 'label-tag';
            more.textContent = '+' + (labels.length - 3);
            lblBox.appendChild(more);
        }
        card.appendChild(lblBox);
    }

    return card;
}
