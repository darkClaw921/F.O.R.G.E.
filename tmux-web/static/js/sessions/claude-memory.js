// tmux-web — модалка «Память Claude» для активной tmux-сессии.
//
// Кнопка 🧠 в правом верхнем углу #tab-bar. По клику запрашивает
// GET /api/claude-memory?path=<cwd активной сессии> и показывает
// содержимое ~/.claude/projects/<encoded-cwd>/memory/ (MEMORY.md + все
// связанные *.md файлы) в стандартной модалке (см. css/modals.css).
// Каждая секция (MEMORY.md и каждый связанный файл) редактируется на
// месте (✎) и сохраняется через PUT /api/claude-memory.

import { state } from '../core/state.js';
import { buildModalOverlay, escapeText } from '../core/utils.js';
import { renderMarkdownInto } from '../core/markdown.js';

function currentSessionPath() {
    const sess = (state.sessions || []).find((s) => s && s.name === state.currentSession);
    return (sess && sess.path) ? sess.path : null;
}

/**
 * Перехватывает клики по относительным `*.md`-ссылкам внутри отрендеренной
 * памяти (например `[Title](filename.md)` в MEMORY.md — это ссылка на
 * другой файл ТОЙ ЖЕ папки памяти, а не URL этого веб-сервера; без
 * перехвата браузер уходил на несуществующий `http://<host>/filename.md`).
 *
 * `sectionsById` — карта «имя файла» → `id` заголовка его секции (уже
 * отрендеренной на странице). Если секция найдена — скроллит к ней,
 * предварительно запомнив текущую позицию в `backStack`, чтобы кнопка
 * «↑ Назад» могла вернуть на неё. Если не найдена — блокирует переход и
 * помечает ссылку визуально.
 */
function wireInternalMemoryLinks(body, sectionsById, backStack, onNavigate) {
    body.querySelectorAll('a[href]').forEach((a) => {
        const href = a.getAttribute('href') || '';
        const isRelativeMdLink = !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href)
            && !href.startsWith('/')
            && !href.startsWith('#')
            && href.toLowerCase().endsWith('.md');
        if (!isRelativeMdLink) return;
        a.removeAttribute('target');
        const targetId = sectionsById.get(href);
        if (targetId) {
            a.href = `#${targetId}`;
            a.addEventListener('click', (ev) => {
                ev.preventDefault();
                const el = document.getElementById(targetId);
                if (!el) return;
                backStack.push(body.scrollTop);
                onNavigate();
                el.scrollIntoView({ behavior: 'smooth', block: 'start' });
            });
        } else {
            a.href = '#';
            a.classList.add('claude-memory-link-missing');
            a.title = 'Этот файл памяти не загружен (не найден в текущей папке памяти)';
            a.addEventListener('click', (ev) => ev.preventDefault());
        }
    });
}

/**
 * Строит одну редактируемую секцию памяти (MEMORY.md либо один связанный
 * файл). Возвращает `{el, id}` — `el` уже содержит заголовок с кнопкой ✎
 * и view/edit-переключение, `id` — id заголовка (для кросс-ссылок).
 *
 * @param {{name: string, content: string, isIndex: boolean}} file
 * @param {{path: string|null, onSaved: () => void}} ctx — `onSaved`
 *   вызывается после успешного PUT (перерисовывает всю модалку заново —
 *   проще и надёжнее, чем точечно патчить DOM и карту секций/ссылок).
 */
function buildMemorySection(file, ctx) {
    const { name, content, isIndex } = file;
    const id = `claude-mem-file-${name.replace(/[^a-zA-Z0-9_-]/g, '-')}`;

    const section = document.createElement('div');
    section.className = 'claude-memory-section';

    const head = document.createElement('div');
    head.className = 'claude-memory-section-head';
    const title = document.createElement('h4');
    title.className = 'echo-md-h claude-memory-section-title';
    title.id = id;
    title.textContent = isIndex ? `${name} (индекс)` : name;
    head.appendChild(title);
    const editBtn = document.createElement('button');
    editBtn.type = 'button';
    editBtn.className = 'claude-memory-edit-btn';
    editBtn.textContent = '✎';
    editBtn.title = 'Редактировать';
    head.appendChild(editBtn);
    section.appendChild(head);

    const viewEl = document.createElement('div');
    viewEl.className = 'claude-memory-section-body echo-md';
    if (content.trim()) {
        renderMarkdownInto(viewEl, content);
    } else {
        const empty = document.createElement('p');
        empty.className = 'claude-memory-section-empty';
        empty.textContent = '(пусто)';
        viewEl.appendChild(empty);
    }
    section.appendChild(viewEl);

    editBtn.addEventListener('click', () => {
        const textarea = document.createElement('textarea');
        textarea.className = 'claude-memory-edit-textarea';
        textarea.value = content;
        section.replaceChild(textarea, viewEl);
        textarea.focus();

        const editActions = document.createElement('div');
        editActions.className = 'claude-memory-edit-actions';
        const saveBtn = document.createElement('button');
        saveBtn.type = 'button';
        saveBtn.className = 'claude-memory-edit-save';
        saveBtn.textContent = 'Сохранить';
        const cancelBtn = document.createElement('button');
        cancelBtn.type = 'button';
        cancelBtn.className = 'claude-memory-edit-cancel';
        cancelBtn.textContent = 'Отмена';
        editActions.appendChild(saveBtn);
        editActions.appendChild(cancelBtn);
        section.appendChild(editActions);
        editBtn.hidden = true;

        const exitEdit = () => {
            section.replaceChild(viewEl, textarea);
            section.removeChild(editActions);
            editBtn.hidden = false;
        };
        cancelBtn.addEventListener('click', exitEdit);

        saveBtn.addEventListener('click', async () => {
            saveBtn.disabled = true;
            cancelBtn.disabled = true;
            try {
                const resp = await fetch('/api/claude-memory', {
                    method: 'PUT',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ path: ctx.path, file: name, content: textarea.value }),
                });
                if (!resp.ok) {
                    const text = await resp.text().catch(() => '');
                    throw new Error(text || `HTTP ${resp.status}`);
                }
                await ctx.onSaved();
            } catch (e) {
                window.alert(`Не удалось сохранить «${name}»: ${e.message || e}`);
                saveBtn.disabled = false;
                cancelBtn.disabled = false;
            }
        });
    });

    return { el: section, id };
}

async function renderMemory(body, backBtn, backStack, path) {
    body.textContent = '';
    backStack.length = 0;
    backBtn.hidden = true;

    const loading = document.createElement('p');
    loading.textContent = 'Загрузка…';
    body.appendChild(loading);

    let data;
    try {
        const qs = path ? `?path=${encodeURIComponent(path)}` : '';
        const resp = await fetch(`/api/claude-memory${qs}`);
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        data = await resp.json();
    } catch (e) {
        body.textContent = '';
        const err = document.createElement('p');
        err.className = 'claude-memory-error';
        err.textContent = `Не удалось загрузить память: ${escapeText(e.message || String(e))}`;
        body.appendChild(err);
        return;
    }

    body.textContent = '';
    if (!data.exists) {
        body.textContent = 'Для этого проекта пока нет сохранённой памяти Claude.';
        return;
    }

    const sectionsById = new Map();
    const onSaved = () => renderMemory(body, backBtn, backStack, path);
    const ctx = { path, onSaved };

    const { el: indexEl, id: indexId } = buildMemorySection(
        { name: 'MEMORY.md', content: data.index || '', isIndex: true },
        ctx,
    );
    sectionsById.set('MEMORY.md', indexId);
    body.appendChild(indexEl);

    for (const f of (data.files || [])) {
        const { el, id } = buildMemorySection({ name: f.name, content: f.content, isIndex: false }, ctx);
        sectionsById.set(f.name, id);
        body.appendChild(el);
    }

    wireInternalMemoryLinks(body, sectionsById, backStack, () => {
        backBtn.hidden = false;
    });
}

export async function openClaudeMemoryModal() {
    const overlay = buildModalOverlay();
    const card = document.createElement('div');
    card.className = 'modal-card claude-memory-modal';

    const header = document.createElement('div');
    header.className = 'claude-memory-header';
    const heading = document.createElement('h2');
    heading.textContent = 'Память Claude';
    header.appendChild(heading);

    // Кнопка «↑ Назад» — возвращает к позиции скролла до перехода по
    // внутренней ссылке (см. wireInternalMemoryLinks). Скрыта, пока стек
    // переходов пуст.
    const backStack = [];
    const backBtn = document.createElement('button');
    backBtn.type = 'button';
    backBtn.className = 'claude-memory-back-btn';
    backBtn.textContent = '↑ Назад';
    backBtn.hidden = true;
    header.appendChild(backBtn);
    card.appendChild(header);

    const body = document.createElement('div');
    body.className = 'claude-memory-body';
    card.appendChild(body);

    const actions = document.createElement('div');
    actions.className = 'modal-actions';
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.textContent = 'Закрыть';
    closeBtn.addEventListener('click', () => overlay.remove());
    actions.appendChild(closeBtn);
    card.appendChild(actions);

    overlay.appendChild(card);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) overlay.remove();
    });
    document.body.appendChild(overlay);

    backBtn.addEventListener('click', () => {
        const prev = backStack.pop();
        if (prev === undefined) return;
        body.scrollTo({ top: prev, behavior: 'smooth' });
        backBtn.hidden = backStack.length === 0;
    });

    const path = currentSessionPath();
    await renderMemory(body, backBtn, backStack, path);
}
