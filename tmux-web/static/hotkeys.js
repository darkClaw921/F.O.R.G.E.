(function () {
    'use strict';

    const HINT_ALPHABET = 'asdfjklewcmprtyuiopghbvnxz'.split('');
    const CMD_HOLD_DELAY_MS = 200;

    const SELECTOR = [
        'button:not([disabled])',
        '[role="button"]:not([disabled])',
        'a[href]',
        'select:not([disabled])',
        'input:not([type="hidden"]):not([disabled])',
        'textarea:not([disabled])',
        '[tabindex]:not([tabindex="-1"])',
        '.session-item',
        '.task-card',
        '.todo-card',
    ].join(',');

    const state = {
        hintsActive: false,
        cmdTimer: null,
        cmdHeld: false,
        cmdSawOther: false,
        hints: [],
        typed: '',
        pendingG: false,
        pendingGTimer: null,
    };

    let hintLayer = null;
    let helpEl = null;

    // Включены ли Cmd-подсказки (настройка cmd_hints_enabled, вкладка
    // Настройки → Интерфейс). Фича opt-in: по умолчанию ВЫКЛЮЧЕНА.
    //
    // Этот файл — классический скрипт, а не ES-модуль, поэтому импортировать
    // state напрямую нельзя. Читаем через публичный контракт window.ForgeApp
    // (js/public-api.js) — `ForgeApp.state` это та же живая ссылка, в которую
    // settings/user-settings-api.js пишет userSettings, так что переключение
    // тумблера подхватывается со следующего нажатия, без перезагрузки.
    //
    // Строгое `=== true` даёт нужную деградацию: пока модули не загрузились
    // (этот скрипт выполняется раньше них) или если fetch настроек упал —
    // фича выключена, что совпадает с дефолтом. Самостоятельный fetch тут не
    // подошёл бы: /hotkeys.js раздаётся без токена, а /api/user-settings в
    // remote-режиме требует авторизации.
    function cmdHintsEnabled() {
        const app = window.ForgeApp;
        const us = app && app.state && app.state.userSettings;
        return !!(us && us.cmd_hints_enabled === true);
    }

    function isEditingTarget(t) {
        if (!t) return false;
        const tag = (t.tagName || '').toLowerCase();
        if (tag === 'input' || tag === 'textarea' || tag === 'select') return true;
        if (t.isContentEditable) return true;
        if (t.classList && t.classList.contains('xterm-helper-textarea')) return true;
        return false;
    }

    function isVisible(el) {
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) return false;
        const vh = window.innerHeight || document.documentElement.clientHeight;
        const vw = window.innerWidth || document.documentElement.clientWidth;
        if (r.bottom < 0 || r.top > vh) return false;
        if (r.right < 0 || r.left > vw) return false;
        let p = el;
        while (p && p !== document.body) {
            if (p.hasAttribute && p.hasAttribute('hidden')) return false;
            const cs = window.getComputedStyle(p);
            if (!cs) break;
            if (cs.display === 'none' || cs.visibility === 'hidden' || parseFloat(cs.opacity) === 0) return false;
            p = p.parentElement;
        }
        return true;
    }

    function generateCodes(n) {
        if (n <= HINT_ALPHABET.length) return HINT_ALPHABET.slice(0, n);
        const codes = [];
        for (let i = 0; i < HINT_ALPHABET.length && codes.length < n; i++) {
            for (let j = 0; j < HINT_ALPHABET.length && codes.length < n; j++) {
                codes.push(HINT_ALPHABET[i] + HINT_ALPHABET[j]);
            }
        }
        return codes;
    }

    function ensureLayer() {
        if (hintLayer) return hintLayer;
        hintLayer = document.createElement('div');
        hintLayer.id = 'hotkey-hints-layer';
        document.body.appendChild(hintLayer);
        return hintLayer;
    }

    function showHints() {
        if (state.hintsActive) return;
        const all = Array.from(document.querySelectorAll(SELECTOR));
        const seen = new Set();
        const visible = [];
        for (const el of all) {
            if (seen.has(el)) continue;
            if (!isVisible(el)) continue;
            seen.add(el);
            visible.push(el);
        }
        if (visible.length === 0) return;
        const codes = generateCodes(visible.length);
        const layer = ensureLayer();
        layer.innerHTML = '';
        layer.style.display = 'block';
        state.hints = visible.map((el, i) => {
            const r = el.getBoundingClientRect();
            const tag = document.createElement('span');
            tag.className = 'hotkey-hint';
            tag.textContent = codes[i].toUpperCase();
            tag.style.left = (r.left + window.scrollX) + 'px';
            tag.style.top = (r.top + window.scrollY) + 'px';
            layer.appendChild(tag);
            return { el, code: codes[i], tag };
        });
        state.hintsActive = true;
        state.typed = '';
    }

    function hideHints() {
        if (!state.hintsActive && (!hintLayer || hintLayer.style.display === 'none')) return;
        state.hintsActive = false;
        state.typed = '';
        if (hintLayer) {
            hintLayer.innerHTML = '';
            hintLayer.style.display = 'none';
        }
        state.hints = [];
    }

    function applyTyped() {
        let matched = null;
        let anyPartial = false;
        for (const h of state.hints) {
            const startsWith = h.code.startsWith(state.typed);
            h.tag.classList.toggle('hotkey-hint-dim', !startsWith);
            h.tag.classList.toggle('hotkey-hint-partial', startsWith && h.code !== state.typed);
            if (startsWith) {
                anyPartial = true;
                if (h.code === state.typed) matched = h;
            }
        }
        if (matched) {
            const el = matched.el;
            hideHints();
            activate(el);
            return;
        }
        if (!anyPartial) hideHints();
    }

    function activate(el) {
        if (!el) return;
        const tag = (el.tagName || '').toLowerCase();
        if (typeof el.focus === 'function') {
            try { el.focus({ preventScroll: false }); } catch (_) { try { el.focus(); } catch (_) {} }
        }
        if (tag === 'input' || tag === 'textarea' || tag === 'select') return;
        try { el.click(); } catch (_) {}
    }

    function activeTabName() {
        const map = { terminal: 'tab-terminal', tasks: 'tab-tasks', git: 'tab-git' };
        for (const name of Object.keys(map)) {
            const btn = document.getElementById(map[name]);
            if (btn && btn.classList.contains('active')) return name;
        }
        return null;
    }

    function clickTab(name) {
        const id = name === 'terminal' ? 'tab-terminal' : name === 'tasks' ? 'tab-tasks' : 'tab-git';
        const btn = document.getElementById(id);
        if (btn) btn.click();
    }

    function focusSessionList(delta) {
        const list = document.getElementById('session-list');
        if (!list) return false;
        const items = Array.from(list.querySelectorAll('.session-item'));
        if (items.length === 0) return false;
        const active = document.activeElement;
        let idx = items.findIndex((it) => it === active || it.contains(active));
        if (idx < 0) {
            idx = items.findIndex((it) => it.classList.contains('active'));
        }
        let next;
        if (idx < 0) next = delta > 0 ? 0 : items.length - 1;
        else next = Math.max(0, Math.min(items.length - 1, idx + delta));
        const target = items[next];
        if (!target) return false;
        if (target.tabIndex < 0) target.tabIndex = 0;
        try { target.focus(); } catch (_) {}
        target.scrollIntoView({ block: 'nearest' });
        return true;
    }

    function focusSidebar() {
        const list = document.getElementById('session-list');
        if (!list) return false;
        const first = list.querySelector('.session-item.active') || list.querySelector('.session-item');
        if (!first) {
            const ps = document.getElementById('project-select');
            if (ps) { try { ps.focus(); } catch (_) {} }
            return true;
        }
        if (first.tabIndex < 0) first.tabIndex = 0;
        try { first.focus(); } catch (_) {}
        return true;
    }

    function focusMainPane() {
        const tab = activeTabName();
        if (tab === 'terminal') {
            const helper = document.querySelector('#terminal .xterm-helper-textarea');
            if (helper) { try { helper.focus(); } catch (_) {} return true; }
            const t = document.getElementById('terminal');
            if (t) t.focus && t.focus();
            return true;
        }
        if (tab === 'tasks') {
            const t = document.getElementById('tasks-new');
            if (t) { try { t.focus(); } catch (_) {} }
            return true;
        }
        if (tab === 'git') {
            const helper = document.querySelector('#git-term .xterm-helper-textarea');
            if (helper) { try { helper.focus(); } catch (_) {} return true; }
            const t = document.getElementById('git-term');
            if (t) t.focus && t.focus();
            return true;
        }
        return false;
    }

    function ensureHelp() {
        if (helpEl) return helpEl;
        helpEl = document.createElement('div');
        helpEl.id = 'hotkey-help';
        helpEl.innerHTML = ''
            + '<div class="hotkey-help-card">'
            + '  <h3>Горячие клавиши</h3>'
            + '  <ul>'
            + '    <li><kbd>1</kbd> / <kbd>2</kbd> / <kbd>3</kbd> — Terminal / Tasks / Git</li>'
            + '    <li><kbd>g</kbd><kbd>t</kbd> / <kbd>g</kbd><kbd>T</kbd> — следующая / предыдущая вкладка</li>'
            + '    <li><kbd>j</kbd> / <kbd>k</kbd> — сессия вниз / вверх</li>'
            + '    <li><kbd>h</kbd> / <kbd>l</kbd> — фокус: сайдбар / основная область</li>'
            + '    <li><kbd>Enter</kbd> — выбрать сфокусированную сессию</li>'
            + '    <li id="hotkey-help-cmd"></li>'
            + '    <li><kbd>?</kbd> — открыть/закрыть эту справку</li>'
            + '    <li><kbd>Esc</kbd> — отмена</li>'
            + '  </ul>'
            + '  <div class="hotkey-help-hint">Esc или ? — закрыть</div>'
            + '</div>';
        document.body.appendChild(helpEl);
        helpEl.addEventListener('click', (e) => { if (e.target === helpEl) hideHelp(); });
        return helpEl;
    }
    // Строка про ⌘-метки зависит от настройки, а карточка справки создаётся
    // один раз и переиспользуется — поэтому текст обновляем при каждом показе,
    // иначе он застынет в состоянии на момент первого открытия.
    function updateCmdHelpLine(el) {
        const li = el.querySelector('#hotkey-help-cmd');
        if (!li) return;
        li.innerHTML = cmdHintsEnabled()
            ? 'Зажать <kbd>⌘</kbd> — метки на всех элементах; введите буквы → клик'
            : 'Зажать <kbd>⌘</kbd> — метки на элементах: выключено, включить в Настройках → Интерфейс';
    }
    function toggleHelp() {
        const el = ensureHelp();
        updateCmdHelpLine(el);
        el.classList.toggle('show');
    }
    function hideHelp() {
        if (helpEl) helpEl.classList.remove('show');
    }

    function vimAction(e) {
        if (e.metaKey || e.ctrlKey || e.altKey) return false;
        if (isEditingTarget(e.target)) return false;

        const key = e.key;

        if (helpEl && helpEl.classList.contains('show')) {
            if (key === 'Escape' || key === '?') { hideHelp(); return true; }
        }

        if (key === '?') { toggleHelp(); return true; }
        if (key === 'Escape') {
            hideHelp();
            return false;
        }

        if (key === '1' || key === '2' || key === '3') {
            clickTab(key === '1' ? 'terminal' : key === '2' ? 'tasks' : 'git');
            return true;
        }

        if (state.pendingG && (key === 't' || key === 'T')) {
            state.pendingG = false;
            clearTimeout(state.pendingGTimer);
            const order = ['terminal', 'tasks', 'git'];
            const cur = order.indexOf(activeTabName());
            const base = cur < 0 ? 0 : cur;
            const next = key === 'T' ? (base - 1 + order.length) % order.length : (base + 1) % order.length;
            clickTab(order[next]);
            return true;
        }
        if (key === 'g') {
            state.pendingG = true;
            clearTimeout(state.pendingGTimer);
            state.pendingGTimer = setTimeout(() => { state.pendingG = false; }, 700);
            return true;
        }

        if (key === 'j') { focusSessionList(+1); return true; }
        if (key === 'k') { focusSessionList(-1); return true; }
        if (key === 'h') { focusSidebar(); return true; }
        if (key === 'l') { focusMainPane(); return true; }

        if (key === 'Enter') {
            const el = document.activeElement;
            if (el && el.classList && el.classList.contains('session-item')) {
                el.click();
                return true;
            }
        }

        return false;
    }

    function onKeyDown(e) {
        // Cmd-hold → hint mode (только если зажат один Meta, без других модификаторов).
        if (e.key === 'Meta' || e.key === 'OS') {
            // Гейт стоит внутри ветки Meta, а не в начале onKeyDown: vim-часть
            // (1/2/3, gt, j/k/h/l, ?) настройкой не управляется и работает
            // всегда. Выходя здесь, мы не ставим cmdHeld — значит ветка
            // «Cmd+другая клавиша» ниже не сработает, и Cmd-шорткаты уйдут в
            // vimAction, который сам отсеивает события с metaKey.
            if (!cmdHintsEnabled()) return;
            state.cmdHeld = true;
            state.cmdSawOther = false;
            if (state.cmdTimer) clearTimeout(state.cmdTimer);
            state.cmdTimer = setTimeout(() => {
                state.cmdTimer = null;
                if (state.cmdHeld && !state.cmdSawOther && !state.hintsActive) showHints();
            }, CMD_HOLD_DELAY_MS);
            return;
        }

        // Любая другая клавиша во время удержания Cmd до активации hints —
        // отменяет hint-mode (это пользовательский Cmd+shortcut: Cmd+C, Cmd+R и т.п.).
        if (state.cmdHeld && !state.hintsActive) {
            state.cmdSawOther = true;
            if (state.cmdTimer) { clearTimeout(state.cmdTimer); state.cmdTimer = null; }
            return;
        }

        if (state.hintsActive) {
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                hideHints();
                return;
            }
            if (e.key === 'Backspace') {
                e.preventDefault();
                e.stopPropagation();
                state.typed = state.typed.slice(0, -1);
                if (state.typed === '') {
                    for (const h of state.hints) {
                        h.tag.classList.remove('hotkey-hint-dim');
                        h.tag.classList.remove('hotkey-hint-partial');
                    }
                } else {
                    applyTyped();
                }
                return;
            }
            if (e.key && e.key.length === 1 && /[a-zA-Z]/.test(e.key)) {
                e.preventDefault();
                e.stopPropagation();
                state.typed += e.key.toLowerCase();
                applyTyped();
                return;
            }
            return;
        }

        if (vimAction(e)) {
            e.preventDefault();
        }
    }

    function onKeyUp(e) {
        if (e.key === 'Meta' || e.key === 'OS') {
            state.cmdHeld = false;
            if (state.cmdTimer) { clearTimeout(state.cmdTimer); state.cmdTimer = null; }
            if (state.hintsActive) hideHints();
        }
    }

    document.addEventListener('keydown', onKeyDown, true);
    document.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('blur', () => {
        state.cmdHeld = false;
        if (state.cmdTimer) { clearTimeout(state.cmdTimer); state.cmdTimer = null; }
        hideHints();
    });
    window.addEventListener('scroll', () => { if (state.hintsActive) hideHints(); }, true);
    window.addEventListener('resize', () => { if (state.hintsActive) hideHints(); });
})();
