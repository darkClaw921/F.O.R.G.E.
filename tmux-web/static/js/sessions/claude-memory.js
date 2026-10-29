// Кнопка 🧠: память проекта Claude и общая память Codex / его субагентов.
// Источник чтения и записи фиксируется для каждой отрисовки. Поздний ответ
// предыдущей вкладки или сохранения не меняет текущую вкладку/закрытое окно.

import { state } from '../core/state.js';
import { buildModalOverlay } from '../core/utils.js';
import { renderMarkdownInto } from '../core/markdown.js';

const providers = {
    claude: { endpoint: '/api/claude-memory', label: 'Claude', scope: 'Память Claude для текущего проекта.', empty: 'Для этого проекта пока нет сохранённой памяти Claude.' },
    codex: { endpoint: '/api/codex-memory', label: 'Codex', scope: 'Общая память Codex и память субагентов. Автоматическая память может обновляться самим Codex.', empty: 'У Codex и его субагентов пока нет сохранённой памяти.' },
};

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
            && href.split(/[?#]/, 1)[0].toLowerCase().endsWith('.md');
        if (!isRelativeMdLink) return;
        a.removeAttribute('target');
        const source = a.closest('[data-memory-file]')?.dataset.memoryFile || 'MEMORY.md';
        let targetName;
        try {
            targetName = decodeURIComponent(new URL(href, `https://memory.invalid/${source}`).pathname.slice(1));
        } catch {
            targetName = '';
        }
        const targetId = sectionsById.get(targetName);
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
 * @param {{path: string|null, provider: string, endpoint: string, isCurrent: () => boolean, onSaved: () => Promise<void>}} ctx — `onSaved`
 *   вызывается после успешного PUT (перерисовывает всю модалку заново —
 *   проще и надёжнее, чем точечно патчить DOM и карту секций/ссылок).
 */
function buildMemorySection(file, ctx) {
    const { name, content, isIndex } = file;
    const id = `memory-file-${ctx.provider}-${encodeURIComponent(name)}`;

    const section = document.createElement('div');
    section.className = 'claude-memory-section';
    section.dataset.memoryFile = name;

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
        if (!ctx.isCurrent()) return;
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
            if (!ctx.isCurrent()) return;
            saveBtn.disabled = true;
            cancelBtn.disabled = true;
            try {
                const resp = await fetch(ctx.endpoint, {
                    method: 'PUT',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ path: ctx.path, file: name, content: textarea.value }),
                });
                if (!resp.ok) {
                    const text = await resp.text().catch(() => '');
                    throw new Error(text || `HTTP ${resp.status}`);
                }
                if (ctx.isCurrent()) await ctx.onSaved();
            } catch (e) {
                if (!ctx.isCurrent()) return;
                window.alert(`Не удалось сохранить «${name}»: ${e.message || e}`);
                saveBtn.disabled = false;
                cancelBtn.disabled = false;
            }
        });
    });

    return { el: section, id };
}

async function renderMemory(body, backBtn, backStack, modal) {
    modal.controller?.abort();
    const controller = new AbortController();
    modal.controller = controller;
    const revision = ++modal.revision;
    const provider = modal.provider;
    const path = modal.path;
    const { endpoint, empty } = providers[provider];
    const isCurrent = () => !modal.closed && modal.revision === revision;
    body.textContent = '';
    backStack.length = 0;
    backBtn.hidden = true;

    const loading = document.createElement('p');
    loading.textContent = 'Загрузка…';
    body.appendChild(loading);

    let data;
    try {
        const qs = provider === 'claude' && path ? `?path=${encodeURIComponent(path)}` : '';
        const resp = await fetch(`${endpoint}${qs}`, { signal: controller.signal });
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        data = await resp.json();
    } catch (e) {
        if (!isCurrent() || e.name === 'AbortError') return;
        body.textContent = '';
        const err = document.createElement('p');
        err.className = 'claude-memory-error';
        err.textContent = `Не удалось загрузить память: ${e.message || String(e)}`;
        body.appendChild(err);
        return;
    }

    if (!isCurrent()) return;
    body.textContent = '';
    if (data.truncated) {
        const notice = document.createElement('p');
        notice.className = 'claude-memory-scope';
        notice.textContent = 'Показана часть памяти: достигнут лимит размера или числа файлов.';
        body.appendChild(notice);
    }
    if (!data.exists || (provider === 'codex' && !data.index_name && !(data.files || []).length)) {
        const message = document.createElement('p');
        message.textContent = empty;
        body.appendChild(message);
        return;
    }

    const sectionsById = new Map();
    const onSaved = () => renderMemory(body, backBtn, backStack, modal);
    const ctx = { path, provider, endpoint, onSaved, isCurrent };

    const location = document.createElement('p');
    location.className = 'claude-memory-location';
    location.textContent = data.dir || '';
    body.appendChild(location);

    const indexName = provider === 'claude' ? 'MEMORY.md' : data.index_name;
    const files = (data.files || []).filter((file) => file.name !== indexName);
    if (provider === 'codex') files.sort((a, b) => Number(a.name.startsWith('agent-memory/')) - Number(b.name.startsWith('agent-memory/')) || a.name.localeCompare(b.name));
    if (indexName) files.unshift({ name: indexName, content: data.index || '' });
    let currentGroup;
    for (const file of files) {
        if (provider === 'codex') {
            const group = file.name.startsWith('agent-memory/') ? 'Память субагентов' : 'Автоматическая память Codex';
            if (group !== currentGroup) {
                const heading = document.createElement('h3');
                heading.className = 'claude-memory-source';
                heading.textContent = group;
                body.appendChild(heading);
                currentGroup = group;
            }
        }
        const { el, id } = buildMemorySection({ ...file, isIndex: file.name === indexName }, ctx);
        sectionsById.set(file.name, id);
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
    heading.textContent = 'Память агентов';
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

    const modal = { provider: 'claude', path: currentSessionPath(), revision: 0, closed: false, controller: null };
    const providerTabs = document.createElement('div');
    providerTabs.className = 'claude-memory-provider-tabs';
    providerTabs.setAttribute('role', 'group');
    providerTabs.setAttribute('aria-label', 'Агент');
    const scope = document.createElement('p');
    scope.className = 'claude-memory-scope';
    for (const [key, config] of Object.entries(providers)) {
        const button = document.createElement('button');
        button.type = 'button';
        button.dataset.provider = key;
        button.textContent = config.label;
        button.setAttribute('aria-pressed', String(key === modal.provider));
        button.addEventListener('click', () => {
            if (modal.closed || modal.provider === key) return;
            modal.provider = key;
            providerTabs.querySelectorAll('button').forEach((tab) => {
                tab.setAttribute('aria-pressed', String(tab.dataset.provider === key));
            });
            scope.textContent = config.scope;
            void renderMemory(body, backBtn, backStack, modal);
        });
        providerTabs.appendChild(button);
    }
    scope.textContent = providers[modal.provider].scope;
    card.append(providerTabs, scope);

    const body = document.createElement('div');
    body.className = 'claude-memory-body';
    card.appendChild(body);

    const actions = document.createElement('div');
    actions.className = 'modal-actions';
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.textContent = 'Закрыть';
    const close = () => {
        modal.closed = true;
        modal.controller?.abort();
        overlay.remove();
    };
    closeBtn.addEventListener('click', close);
    actions.appendChild(closeBtn);
    card.appendChild(actions);

    overlay.appendChild(card);
    overlay.addEventListener('click', (ev) => {
        if (ev.target === overlay) close();
    });
    document.body.appendChild(overlay);

    backBtn.addEventListener('click', () => {
        const prev = backStack.pop();
        if (prev === undefined) return;
        body.scrollTo({ top: prev, behavior: 'smooth' });
        backBtn.hidden = backStack.length === 0;
    });

    await renderMemory(body, backBtn, backStack, modal);
}
