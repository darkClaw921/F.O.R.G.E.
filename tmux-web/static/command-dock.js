/*
 * command-dock.js — нижняя панель команд для вкладки терминала (desktop).
 *
 * Что делает:
 *   1. Показывает топ самых часто набираемых команд (источник —
 *      localStorage forge.quickCmd.freq, который ведёт quick-cmd.js по
 *      stdin xterm). Клик по chip отправляет команду в активную сессию
 *      через send-keys (ForgeApp.sendToActivePty + '\r').
 *   2. Позволяет создавать собственные элементы — как команды (выполняются
 *      по клику, с Enter), так и «просто текст» (вставляется в строку без
 *      Enter). Хранятся в localStorage forge.cmdDock.items.
 *   3. Пользовательские элементы можно переупорядочивать drag-and-drop.
 *   4. Частую команду можно «закрепить» (📌) — она превращается в
 *      пользовательский элемент-команду.
 *
 * Интеграция:
 *   - sendToPty: window.ForgeApp.sendToActivePty(text)
 *   - freq: общий localStorage-ключ с quick-cmd.js (forge.quickCmd.freq)
 *   - Разметка: #cmd-dock внутри #terminal (index.html)
 *   - Подключение: <script src="/command-dock.js"> ПОСЛЕ /quick-cmd.js
 *
 * Публичный API:
 *   window.CommandDock.refresh()  — перерисовать
 */
(function () {
    'use strict';

    // ===== Константы =====
    const LS_FREQ = 'forge.quickCmd.freq';      // общий с quick-cmd.js
    const LS_QC_PINNED = 'forge.quickCmd.pinned'; // закреплённые в quick-cmd
    const LS_ITEMS = 'forge.cmdDock.items';     // пользовательские элементы
    const LS_COLLAPSED = 'forge.cmdDock.collapsed';
    const LS_FREQ_HIDDEN = 'forge.cmdDock.freqHidden'; // скрытые из frequent
    const LS_FREQ_CLEAN = 'forge.cmdDock.freqCleanV';  // версия выполненной очистки
    const FREQ_CLEAN_VERSION = 1;               // повышать при улучшении эвристики
    const FREQ_TOP_N = 12;
    const MOBILE_MQ = '(max-width: 768px)';

    // ===== State =====
    const state = {
        dock: null,
        pinnedEl: null,
        frequentEl: null,
        bodyEl: null,
        addBtn: null,
        collapseBtn: null,
        editorEl: null,
        mq: null,
        dragId: null,          // id перетаскиваемого элемента
        idCounter: 0,
    };

    // ===== localStorage helpers =====
    function lsGet(key, fallback) {
        try {
            const raw = localStorage.getItem(key);
            if (!raw) return fallback;
            const v = JSON.parse(raw);
            return v == null ? fallback : v;
        } catch (_) { return fallback; }
    }
    function lsSet(key, value) {
        try { localStorage.setItem(key, JSON.stringify(value)); } catch (_) {}
    }

    function getFreq() {
        const v = lsGet(LS_FREQ, {});
        return (v && typeof v === 'object' && !Array.isArray(v)) ? v : {};
    }
    function setFreq(map) { lsSet(LS_FREQ, map); }

    function getQcPinned() {
        const v = lsGet(LS_QC_PINNED, []);
        return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
    }

    // items: [{ id, label, text, kind: 'cmd' | 'text' }]
    function getItems() {
        const v = lsGet(LS_ITEMS, []);
        if (!Array.isArray(v)) return [];
        return v
            .filter((it) => it && typeof it.text === 'string' && it.text.length > 0)
            .map((it) => ({
                id: String(it.id || nextId()),
                label: typeof it.label === 'string' && it.label ? it.label : it.text,
                text: it.text,
                kind: it.kind === 'text' ? 'text' : 'cmd',
            }));
    }
    function setItems(list) { lsSet(LS_ITEMS, list); }

    function getFreqHidden() {
        const v = lsGet(LS_FREQ_HIDDEN, []);
        return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
    }
    function setFreqHidden(list) { lsSet(LS_FREQ_HIDDEN, list); }

    function isCollapsed() { return lsGet(LS_COLLAPSED, false) === true; }
    function setCollapsed(v) { lsSet(LS_COLLAPSED, !!v); }

    function nextId() {
        state.idCounter += 1;
        return 'it_' + Date.now().toString(36) + '_' + state.idCounter;
    }

    // ===== Отправка в PTY =====
    function sendToPty(text) {
        if (!text) return;
        const app = window.ForgeApp;
        if (!app || typeof app.sendToActivePty !== 'function') {
            console.warn('[command-dock] ForgeApp.sendToActivePty unavailable');
            return;
        }
        app.sendToActivePty(text);
    }

    function runItem(item) {
        if (!item || !item.text) return;
        // 'cmd' — выполнить (с Enter), 'text' — просто вставить в строку.
        sendToPty(item.kind === 'text' ? item.text : item.text + '\r');
    }

    // ===== Frequent =====
    // Эвристика «похоже на команду» — отсекает мусор, накопленный из
    // вставленного кода/логов/stack-trace (исторические записи во freq).
    // Новые вставки уже не попадают сюда (quick-cmd.js игнорирует
    // bracketed paste), но старый freq может содержать хлам.
    function looksLikeCommand(s) {
        if (typeof s !== 'string') return false;
        s = s.trim();
        if (s.length < 2) return false;                 // одиночные символы
        if (!/[A-Za-z0-9]/.test(s)) return false;       // только пунктуация: } { ; …
        if (/[;{},=]$/.test(s)) return false;           // строки кода: …; …{ …, …=
        // ключевые слова языков программирования в начале строки
        if (/^(const|let|var|return|import|export|function|class|if|for|while|else|switch|case|break|continue|await|async|new|throw|try|catch|finally|public|private|def|fn|impl|struct|enum)\b/.test(s)) return false;
        if (/^at\s+\S/.test(s) && /\(/.test(s)) return false; // stack trace: "at Foo … (…)"
        if (/:\d+:\d+\)?$/.test(s)) return false;       // …file:line:col — лог/трейс
        if (/^\S+\s+\|\s/.test(s)) return false;        // лог-префикс: "app | …"
        if (/^\s*[)\]}]/.test(s)) return false;         // начинается со скобки-закрывашки
        return true;
    }

    // Одноразовая очистка forge.quickCmd.freq от мусора (код/логи/трейсы),
    // накопленного до фикса bracketed-paste. Идемпотентна: выполняется,
    // только если сохранённая версия < FREQ_CLEAN_VERSION. Закреплённые в
    // quick-cmd команды (forge.quickCmd.pinned) и собственные элементы дока
    // не трогаем, даже если эвристика их забраковала.
    function migrateFreqOnce() {
        try {
            if (lsGet(LS_FREQ_CLEAN, 0) >= FREQ_CLEAN_VERSION) return;
            const freq = getFreq();
            const keep = new Set(getQcPinned());
            for (const it of getItems()) keep.add(it.text);
            let removed = 0;
            for (const key of Object.keys(freq)) {
                if (keep.has(key)) continue;
                if (!looksLikeCommand(key)) { delete freq[key]; removed += 1; }
            }
            if (removed > 0) setFreq(freq);
            lsSet(LS_FREQ_CLEAN, FREQ_CLEAN_VERSION);
            if (removed > 0) {
                console.info('[command-dock] freq cleanup: удалено мусорных записей =', removed);
            }
        } catch (e) {
            console.warn('[command-dock] migrateFreqOnce failed', e);
        }
    }

    function computeFrequent(excludeTexts) {
        const freq = getFreq();
        const hidden = new Set(getFreqHidden());
        const exclude = excludeTexts || new Set();
        return Object.keys(freq)
            .filter((cmd) => typeof freq[cmd] === 'number' && freq[cmd] > 0)
            .filter((cmd) => !hidden.has(cmd) && !exclude.has(cmd))
            .filter((cmd) => looksLikeCommand(cmd))
            .sort((a, b) => freq[b] - freq[a] || (a < b ? -1 : 1))
            .slice(0, FREQ_TOP_N);
    }

    // ===== DOM refs =====
    function ensureRefs() {
        if (state.dock) return true;
        state.dock = document.getElementById('cmd-dock');
        state.pinnedEl = document.getElementById('cmd-dock-pinned');
        state.frequentEl = document.getElementById('cmd-dock-frequent');
        state.bodyEl = document.getElementById('cmd-dock-body');
        state.addBtn = document.getElementById('cmd-dock-add');
        state.collapseBtn = document.getElementById('cmd-dock-collapse');
        if (!state.dock || !state.pinnedEl || !state.frequentEl
            || !state.bodyEl || !state.addBtn || !state.collapseBtn) {
            return false;
        }
        state.addBtn.addEventListener('click', (e) => {
            e.preventDefault();
            toggleEditor();
        });
        state.collapseBtn.addEventListener('click', (e) => {
            e.preventDefault();
            setCollapsed(!isCollapsed());
            applyCollapsed();
        });
        return true;
    }

    // ===== Рендер chip'ов =====
    function makeChip(opts) {
        // opts: { label, text, kind, pinned, item }
        const chip = document.createElement('div');
        chip.className = 'cmd-chip ' + (opts.kind === 'text' ? 'cmd-chip-text' : 'cmd-chip-cmd');
        chip.title = opts.text + (opts.kind === 'text' ? '  (вставить как текст)' : '  (выполнить)');

        const label = document.createElement('span');
        label.className = 'cmd-chip-label';
        label.textContent = opts.label;
        chip.appendChild(label);

        if (opts.kind === 'text') {
            const kindBadge = document.createElement('span');
            kindBadge.className = 'cmd-chip-kind';
            kindBadge.textContent = 'txt';
            chip.appendChild(kindBadge);
        }

        // Клик по chip (но не по action-кнопкам) — отправить.
        chip.addEventListener('click', (e) => {
            if (e.target.closest('.cmd-chip-act')) return;
            e.preventDefault();
            runItem({ text: opts.text, kind: opts.kind });
        });

        return chip;
    }

    function renderPinned() {
        const el = state.pinnedEl;
        el.innerHTML = '';
        const items = getItems();
        if (items.length === 0) {
            const empty = document.createElement('span');
            empty.className = 'cmd-dock-empty';
            empty.textContent = 'Нет своих команд — нажмите «+ Добавить»';
            el.appendChild(empty);
            return;
        }
        for (const item of items) {
            const chip = makeChip({
                label: item.label, text: item.text, kind: item.kind, pinned: true,
            });
            chip.classList.add('cmd-chip-pinned');
            chip.dataset.id = item.id;
            chip.setAttribute('draggable', 'true');

            // edit
            const editBtn = document.createElement('button');
            editBtn.type = 'button';
            editBtn.className = 'cmd-chip-act cmd-chip-edit';
            editBtn.textContent = '✎';
            editBtn.title = 'Редактировать';
            editBtn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                openEditor(item);
            });
            chip.appendChild(editBtn);

            // delete
            const delBtn = document.createElement('button');
            delBtn.type = 'button';
            delBtn.className = 'cmd-chip-act cmd-chip-del';
            delBtn.textContent = '×';
            delBtn.title = 'Удалить';
            delBtn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                deleteItem(item.id);
            });
            chip.appendChild(delBtn);

            attachDnd(chip);
            el.appendChild(chip);
        }
    }

    function renderFrequent() {
        const el = state.frequentEl;
        el.innerHTML = '';
        const ownTexts = new Set(getItems().map((it) => it.text));
        const freq = computeFrequent(ownTexts);
        for (const cmd of freq) {
            const chip = makeChip({ label: cmd, text: cmd, kind: 'cmd', pinned: false });

            // pin → превратить в пользовательский элемент
            const pinBtn = document.createElement('button');
            pinBtn.type = 'button';
            pinBtn.className = 'cmd-chip-act cmd-chip-pin';
            pinBtn.textContent = '📌';
            pinBtn.title = 'Закрепить';
            pinBtn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                addItem({ label: cmd, text: cmd, kind: 'cmd' });
            });
            chip.appendChild(pinBtn);

            // hide из frequent
            const hideBtn = document.createElement('button');
            hideBtn.type = 'button';
            hideBtn.className = 'cmd-chip-act cmd-chip-del';
            hideBtn.textContent = '×';
            hideBtn.title = 'Скрыть из частых';
            hideBtn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                const h = getFreqHidden();
                if (!h.includes(cmd)) { h.push(cmd); setFreqHidden(h); }
                render();
            });
            chip.appendChild(hideBtn);

            el.appendChild(chip);
        }
    }

    // ===== CRUD =====
    function addItem(data) {
        const text = (data.text || '').trim();
        if (!text) return;
        const items = getItems();
        // дедуп по text+kind
        if (items.some((it) => it.text === text && it.kind === (data.kind || 'cmd'))) {
            render();
            return;
        }
        items.push({
            id: nextId(),
            label: (data.label || text).trim() || text,
            text: text,
            kind: data.kind === 'text' ? 'text' : 'cmd',
        });
        setItems(items);
        render();
    }

    function updateItem(id, data) {
        const items = getItems();
        const idx = items.findIndex((it) => it.id === id);
        if (idx < 0) return;
        const text = (data.text || '').trim();
        if (!text) return;
        items[idx] = {
            id: id,
            label: (data.label || text).trim() || text,
            text: text,
            kind: data.kind === 'text' ? 'text' : 'cmd',
        };
        setItems(items);
        render();
    }

    function deleteItem(id) {
        setItems(getItems().filter((it) => it.id !== id));
        render();
    }

    function reorderItems(orderedIds) {
        const items = getItems();
        const byId = new Map(items.map((it) => [it.id, it]));
        const next = [];
        for (const id of orderedIds) {
            if (byId.has(id)) { next.push(byId.get(id)); byId.delete(id); }
        }
        // хвост — то, что не попало в orderedIds (на всякий случай)
        for (const it of byId.values()) next.push(it);
        setItems(next);
    }

    // ===== Drag-and-drop (пользовательские chip'ы) =====
    function attachDnd(chip) {
        chip.addEventListener('dragstart', (e) => {
            state.dragId = chip.dataset.id;
            chip.classList.add('dragging');
            try {
                e.dataTransfer.effectAllowed = 'move';
                e.dataTransfer.setData('text/plain', chip.dataset.id);
            } catch (_) {}
        });
        chip.addEventListener('dragend', () => {
            state.dragId = null;
            clearDropMarkers();
            state.pinnedEl.querySelectorAll('.cmd-chip.dragging')
                .forEach((c) => c.classList.remove('dragging'));
        });
        chip.addEventListener('dragover', (e) => {
            if (!state.dragId || state.dragId === chip.dataset.id) return;
            e.preventDefault();
            try { e.dataTransfer.dropEffect = 'move'; } catch (_) {}
            const rect = chip.getBoundingClientRect();
            const after = (e.clientX - rect.left) > rect.width / 2;
            clearDropMarkers();
            chip.classList.add(after ? 'drop-after' : 'drop-before');
        });
        chip.addEventListener('dragleave', () => {
            chip.classList.remove('drop-before', 'drop-after');
        });
        chip.addEventListener('drop', (e) => {
            e.preventDefault();
            const dragId = state.dragId;
            if (!dragId || dragId === chip.dataset.id) { clearDropMarkers(); return; }
            const rect = chip.getBoundingClientRect();
            const after = (e.clientX - rect.left) > rect.width / 2;
            clearDropMarkers();
            // собрать новый порядок id
            const ids = Array.from(state.pinnedEl.querySelectorAll('.cmd-chip-pinned'))
                .map((c) => c.dataset.id)
                .filter((id) => id !== dragId);
            const targetIdx = ids.indexOf(chip.dataset.id);
            const insertAt = after ? targetIdx + 1 : targetIdx;
            ids.splice(insertAt, 0, dragId);
            reorderItems(ids);
            render();
        });
    }

    function clearDropMarkers() {
        state.pinnedEl.querySelectorAll('.drop-before, .drop-after')
            .forEach((c) => c.classList.remove('drop-before', 'drop-after'));
    }

    // ===== Inline-редактор (добавить / править) =====
    function ensureEditor() {
        if (state.editorEl) return state.editorEl;
        const ed = document.createElement('form');
        ed.className = 'cmd-dock-editor';
        ed.hidden = true;
        ed.innerHTML =
            '<input type="text" class="cmd-dock-ed-text" placeholder="Команда или текст…" autocomplete="off">'
          + '<input type="text" class="cmd-dock-ed-label" placeholder="Подпись (необязательно)" autocomplete="off">'
          + '<select class="cmd-dock-ed-kind">'
          +   '<option value="cmd">Команда (Enter)</option>'
          +   '<option value="text">Просто текст</option>'
          + '</select>'
          + '<button type="submit" class="cmd-dock-hbtn cmd-dock-ed-save">Сохранить</button>'
          + '<button type="button" class="cmd-dock-hbtn cmd-dock-ed-cancel">Отмена</button>';
        // редактор кладём первым в body, над секциями
        state.bodyEl.insertBefore(ed, state.bodyEl.firstChild);

        const textInput = ed.querySelector('.cmd-dock-ed-text');
        const labelInput = ed.querySelector('.cmd-dock-ed-label');
        const kindSel = ed.querySelector('.cmd-dock-ed-kind');
        const cancelBtn = ed.querySelector('.cmd-dock-ed-cancel');

        ed.addEventListener('submit', (e) => {
            e.preventDefault();
            const data = {
                text: textInput.value,
                label: labelInput.value,
                kind: kindSel.value,
            };
            const editId = ed.dataset.editId || '';
            if (editId) {
                updateItem(editId, data);
            } else {
                addItem(data);
            }
            closeEditor();
        });
        cancelBtn.addEventListener('click', (e) => {
            e.preventDefault();
            closeEditor();
        });
        ed.addEventListener('keydown', (e) => {
            if (e.key === 'Escape') { e.preventDefault(); closeEditor(); }
        });

        ed._fields = { textInput, labelInput, kindSel };
        state.editorEl = ed;
        return ed;
    }

    function openEditor(item) {
        const ed = ensureEditor();
        const f = ed._fields;
        if (item) {
            ed.dataset.editId = item.id;
            f.textInput.value = item.text;
            f.labelInput.value = (item.label && item.label !== item.text) ? item.label : '';
            f.kindSel.value = item.kind === 'text' ? 'text' : 'cmd';
        } else {
            delete ed.dataset.editId;
            f.textInput.value = '';
            f.labelInput.value = '';
            f.kindSel.value = 'cmd';
        }
        ed.hidden = false;
        // развернуть, если был свёрнут
        if (isCollapsed()) { setCollapsed(false); applyCollapsed(); }
        f.textInput.focus();
    }

    function closeEditor() {
        if (state.editorEl) state.editorEl.hidden = true;
    }

    function toggleEditor() {
        const ed = ensureEditor();
        if (ed.hidden) openEditor(null);
        else closeEditor();
    }

    // ===== Видимость / layout =====
    function isMobile() {
        if (state.mq) return state.mq.matches;
        try { state.mq = window.matchMedia(MOBILE_MQ); return state.mq.matches; }
        catch (_) { return false; }
    }

    function applyCollapsed() {
        if (!state.dock) return;
        state.dock.classList.toggle('collapsed', isCollapsed());
    }

    // Док показываем только на активной вкладке терминала и не на mobile
    // (там работает .quick-cmd-bar). Поскольку док — flex-сиблинг #terminal
    // внутри #main, его появление/изменение высоты автоматически ужимает
    // #terminal (flex:1); ResizeObserver в terminal/xterm.js делает fit().
    // Никакого ручного резервирования места не требуется.
    function terminalActive() {
        const t = document.getElementById('terminal');
        return !!t && !t.hidden;
    }

    function render() {
        if (!ensureRefs()) return;
        if (isMobile() || !terminalActive()) {
            state.dock.hidden = true;
            return;
        }
        state.dock.hidden = false;
        applyCollapsed();
        renderPinned();
        renderFrequent();
    }

    function refresh() { render(); }

    // ===== Bootstrap =====
    function onMqChange() { render(); }

    function init() {
        migrateFreqOnce();
        if (!ensureRefs()) {
            setTimeout(init, 150);
            return;
        }
        try {
            state.mq = window.matchMedia(MOBILE_MQ);
            if (typeof state.mq.addEventListener === 'function') {
                state.mq.addEventListener('change', onMqChange);
            } else if (typeof state.mq.addListener === 'function') {
                state.mq.addListener(onMqChange);
            }
        } catch (_) {}
        render();
        // Периодически обновляем frequent (freq растёт по мере ввода команд).
        // Без таймеров-поллеров: обновляем при возврате фокуса на окно.
        window.addEventListener('focus', () => render());
        // Переключение вкладок меняет hidden у #terminal — синхронизируем
        // видимость дока (показываем только на вкладке терминала).
        try {
            const terminal = document.getElementById('terminal');
            if (terminal) {
                const mo = new MutationObserver(() => render());
                mo.observe(terminal, { attributes: true, attributeFilter: ['hidden'] });
            }
        } catch (_) {}
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

    // ===== Публичный API =====
    window.CommandDock = {
        refresh: refresh,
    };
})();
