/*
 * quick-cmd.js — Quick-command bar для mobile.
 *
 * Авто-трекинг частоты команд из stdin основного xterm и TUI-tabs.
 * Бар показывает top-N команд + spec-клавиш (Esc, Tab, Ctrl+C, стрелки).
 * Данные хранятся в localStorage (forge.quickCmd.freq/pinned/hidden).
 *
 * Публичный API:
 *   window.QuickCmd.onPtyInput(text) — вызывается из app.js term.onData
 *   window.QuickCmd.openEditor()     — открывает Edit-UI
 *   window.QuickCmd.refresh()        — перерисовать бар
 *
 * Интеграция:
 *   - sendToPty: window.ForgeApp.sendToActivePty(text) (см. app.js)
 *   - Подключение: <script src="/quick-cmd.js"> ПОСЛЕ /app.js
 */
(function () {
    'use strict';

    // ===== Константы =====
    const LS_KEY_FREQ = 'forge.quickCmd.freq';
    const LS_KEY_PINNED = 'forge.quickCmd.pinned';
    const LS_KEY_HIDDEN = 'forge.quickCmd.hidden';
    const TOP_N = 8;
    const DEFAULTS = ['ls', 'cd ..', 'git status', 'clear', 'exit'];

    // Spec-keys: метка → raw-байты для PTY.
    const SPEC_KEYS = [
        { label: 'Esc',    bytes: '\x1b'    },
        { label: 'Tab',    bytes: '\t'      },
        { label: '^C',     bytes: '\x03'    },
        { label: '↑',      bytes: '\x1b[A'  },
        { label: '↓',      bytes: '\x1b[B'  },
        { label: '←',      bytes: '\x1b[D'  },
        { label: '→',      bytes: '\x1b[C'  },
    ];

    // TUI quick-keys (Phase C): минимальный набор клавиш для git/docker/telescope
    // (lazygit, lazydocker, television). Каждая кнопка отправляет raw-байты
    // в активный PTY через ForgeApp.sendToActivePty.
    const TUI_KEYS = [
        { label: 'q',     bytes: 'q'       },
        { label: 'Esc',   bytes: '\x1b'    },
        { label: '?',     bytes: '?'       },
        { label: ':',     bytes: ':'       },
        { label: '/',     bytes: '/'       },
        { label: 'h',     bytes: 'h'       },
        { label: 'j',     bytes: 'j'       },
        { label: 'k',     bytes: 'k'       },
        { label: 'l',     bytes: 'l'       },
        { label: 'Enter', bytes: '\r'      },
        { label: '^C',    bytes: '\x03'    },
        { label: 'Tab',   bytes: '\t'      },
        { label: '↑',     bytes: '\x1b[A'  },
        { label: '↓',     bytes: '\x1b[B'  },
        { label: '←',     bytes: '\x1b[D'  },
        { label: '→',     bytes: '\x1b[C'  },
    ];

    // Соответствие имени активной вкладки → id TUI quick-bar.
    const TUI_BAR_IDS = {
        git: 'git-quick-bar',
        docker: 'docker-quick-bar',
        telescope: 'telescope-quick-bar',
    };

    // Mobile breakpoint должен совпадать с CSS @media (max-width: 768px).
    const MOBILE_MQ = '(max-width: 768px)';

    // ===== State =====
    const state = {
        // Буфер ввода пользователя до \r/\n (per active tab — но единый,
        // т.к. одновременно вводят только в один PTY).
        inputBuffer: '',
        // Режим пропуска escape-последовательности. Сохраняется между
        // вызовами onPtyInput, т.к. xterm может фрагментировать data.
        // Возможные значения:
        //   null     — обычный ввод
        //   'init'   — увидели \x1b, ждём первый байт после
        //   'csi'    — \x1b[ ... до final-byte (0x40-0x7e)
        //   'osc'    — \x1b] ... до BEL (0x07) или ST (\x1b\\)
        //   'dcs'    — \x1bP ... до ST
        //   'apc'    — \x1b_ ... до ST
        //   'pm'     — \x1b^ ... до ST
        //   'sos'    — \x1bX ... до ST
        //   'ss3'    — \x1bO ... пропустить 1 байт
        //   'esc1'   — одиночный ESC + 1 byte, пропустить
        //   'st-tail'— уже увидели \x1b в OSC-подобной — ждём \\
        escMode: null,
        // Режим вставки (bracketed paste). xterm оборачивает вставленный
        // текст в \x1b[200~ ... \x1b[201~. Всё между ними — НЕ команды
        // (это код/логи/многострочный текст), их нельзя засчитывать в freq.
        pasteMode: false,
        // Аккумулятор параметров текущей CSI-последовательности (для
        // распознавания 200~/201~).
        csiParams: '',
        // DOM-refs (заполняются после DOMContentLoaded).
        bar: null,
        keysEl: null,
        cmdsEl: null,
        editBtn: null,
        // Editor-модалка (создаётся лениво).
        editorEl: null,
        mq: null,
    };

    // ===== localStorage helpers =====
    function lsGetJSON(key, fallback) {
        try {
            const raw = localStorage.getItem(key);
            if (!raw) return fallback;
            const v = JSON.parse(raw);
            return v == null ? fallback : v;
        } catch (_) {
            return fallback;
        }
    }
    function lsSetJSON(key, value) {
        try { localStorage.setItem(key, JSON.stringify(value)); } catch (_) {}
    }

    function getFreq() {
        const v = lsGetJSON(LS_KEY_FREQ, {});
        return (v && typeof v === 'object' && !Array.isArray(v)) ? v : {};
    }
    function setFreq(map) { lsSetJSON(LS_KEY_FREQ, map); }

    function getPinned() {
        const v = lsGetJSON(LS_KEY_PINNED, []);
        return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
    }
    function setPinned(list) { lsSetJSON(LS_KEY_PINNED, list); }

    function getHidden() {
        const v = lsGetJSON(LS_KEY_HIDDEN, []);
        return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
    }
    function setHidden(list) { lsSetJSON(LS_KEY_HIDDEN, list); }

    // ===== Нормализация и валидация команд =====
    // Паттерны типичного ANSI-эха от xterm в ответ на программные запросы
    // (color queries, device attributes, cursor pos и т.п.). Если в буфере
    // остались такие хвосты после неполного skip — отбрасываем.
    const ANSI_ECHO_PATTERNS = [
        /^[\[\]?]/,           // начинается с [, ], ? — почти всегда CSI/OSC/DA остаток
        /^[Oo]/,              // SS3 хвост (например 'OP', 'OQ')
        /rgb:[0-9a-f]/i,      // OSC 10/11/12 ответы
        /^\d+(;\d+)*[a-zA-Z~]?$/, // CSI-параметры (типа '15;33R', '?64;1c')
        /\\$/,                // ST-хвост
        /;[0-9a-f]{4}\//i,    // rgb-фрагмент
    ];

    function normalize(text) {
        if (typeof text !== 'string') return '';
        const t = text.trim();
        if (t.length === 0) return '';
        // Отбрасываем команды с управляющими байтами (включая ESC).
        for (let i = 0; i < t.length; i++) {
            const code = t.charCodeAt(i);
            if (code < 0x20 && code !== 0x09) return '';
            if (code === 0x7f) return '';
        }
        // Слишком длинные строки — скорее всего paste, не команда.
        if (t.length > 200) return '';
        // Отбрасываем шум от ANSI-эха.
        for (const re of ANSI_ECHO_PATTERNS) {
            if (re.test(t)) return '';
        }
        // Минимальная санити-проверка: команда должна начинаться с
        // печатного непробельного ASCII или UTF-8.
        const first = t.charCodeAt(0);
        if (first <= 0x20) return '';
        return t;
    }

    function bumpFreq(cmd) {
        const freq = getFreq();
        freq[cmd] = (typeof freq[cmd] === 'number' ? freq[cmd] : 0) + 1;
        setFreq(freq);
    }

    // ===== Top-N выбор =====
    function computeTopCommands() {
        const freq = getFreq();
        const pinned = getPinned();
        const hidden = new Set(getHidden());

        // Базовый пул: pinned ∪ freq-keys ∪ DEFAULTS.
        const pool = new Set();
        for (const c of pinned) pool.add(c);
        for (const c of Object.keys(freq)) pool.add(c);
        for (const c of DEFAULTS) pool.add(c);

        // Hidden фильтруем (даже если pinned — pinned имеет приоритет;
        // hidden скрывает только не-pinned).
        const pinnedSet = new Set(pinned);
        const filtered = Array.from(pool).filter((c) => pinnedSet.has(c) || !hidden.has(c));

        // Сортировка: pinned первыми (в порядке pinned-массива),
        // затем по freq desc, затем DEFAULTS-порядок, иначе алфавит.
        const defaultsIdx = new Map(DEFAULTS.map((c, i) => [c, i]));
        function rank(c) {
            const pIdx = pinned.indexOf(c);
            if (pIdx >= 0) return [0, pIdx, 0, c];
            const f = typeof freq[c] === 'number' ? freq[c] : 0;
            const dIdx = defaultsIdx.has(c) ? defaultsIdx.get(c) : 999;
            return [1, -f, dIdx, c];
        }
        filtered.sort((a, b) => {
            const ra = rank(a), rb = rank(b);
            for (let i = 0; i < ra.length; i++) {
                if (ra[i] < rb[i]) return -1;
                if (ra[i] > rb[i]) return 1;
            }
            return 0;
        });
        return filtered.slice(0, TOP_N);
    }

    // ===== Отправка в PTY =====
    function sendToPty(text) {
        if (!text) return;
        const app = window.ForgeApp;
        if (!app || typeof app.sendToActivePty !== 'function') {
            console.warn('[quick-cmd] ForgeApp.sendToActivePty unavailable');
            return;
        }
        app.sendToActivePty(text);
    }

    // ===== Рендер бара =====
    function isMobile() {
        if (state.mq) return state.mq.matches;
        try {
            state.mq = window.matchMedia(MOBILE_MQ);
            return state.mq.matches;
        } catch (_) {
            return false;
        }
    }

    function ensureRefs() {
        if (state.bar) return true;
        state.bar = document.getElementById('quick-cmd-bar');
        state.keysEl = document.getElementById('quick-cmd-keys');
        state.cmdsEl = document.getElementById('quick-cmd-cmds');
        state.editBtn = document.getElementById('quick-cmd-edit');
        if (!state.bar || !state.keysEl || !state.cmdsEl || !state.editBtn) {
            return false;
        }
        state.editBtn.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            openEditor();
        });
        return true;
    }

    function renderSpecKeys() {
        if (!state.keysEl) return;
        state.keysEl.innerHTML = '';
        for (const sk of SPEC_KEYS) {
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'quick-cmd-key';
            btn.textContent = sk.label;
            btn.dataset.bytes = sk.bytes;
            btn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                sendToPty(sk.bytes);
            });
            state.keysEl.appendChild(btn);
        }
    }

    function renderCommands() {
        if (!state.cmdsEl) return;
        state.cmdsEl.innerHTML = '';
        const cmds = computeTopCommands();
        if (cmds.length === 0) {
            const empty = document.createElement('span');
            empty.className = 'quick-cmd-empty';
            empty.textContent = 'No commands yet';
            state.cmdsEl.appendChild(empty);
            return;
        }
        for (const cmd of cmds) {
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'quick-cmd-cmd';
            btn.textContent = cmd;
            btn.title = cmd;
            btn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                sendToPty(cmd + '\r');
            });
            state.cmdsEl.appendChild(btn);
        }
    }

    function refresh() {
        if (!ensureRefs()) return;
        const show = isMobile();
        if (show) {
            state.bar.hidden = false;
            renderSpecKeys();
            renderCommands();
        } else {
            state.bar.hidden = true;
        }
        // TUI quick-keys bars: их видимость не зависит от quick-cmd-bar,
        // но reacts на тот же mobile-breakpoint и активную вкладку.
        refreshTuiBars();
    }

    // ===== onPtyInput: буферизация + bump =====
    //
    // Важно: term.onData в xterm срабатывает не только на пользовательский
    // ввод, но и на ПРОГРАММНЫЕ ответы xterm на запросы программы
    // (DA, color queries OSC 10/11/12, cursor position и т.п.). Эти ответы
    // имеют формат \x1b[ ... letter (CSI) или \x1b] ... BEL/ST (OSC) или
    // \x1bP ... ST (DCS) и др. Если их не отфильтровать, фрагменты вроде
    // ']11;rgb:4c4c/4f4f/6969\x07' оседают в буфере и записываются как
    // мусорные «команды». Поэтому при встрече \x1b мы:
    //   1) сбрасываем уже накопленный буфер (он гарантированно не команда);
    //   2) переходим в режим skip до конца escape-sequence (escMode);
    //   3) escMode сохраняется между вызовами onPtyInput, т.к. xterm
    //      может фрагментировать data.
    function startEsc() {
        state.inputBuffer = '';
        state.escMode = 'init';
    }
    function endEsc() { state.escMode = null; }

    function processEscByte(code, ch) {
        switch (state.escMode) {
            case 'init':
                // Первый байт после \x1b — определяем тип последовательности.
                if (ch === '[') { state.escMode = 'csi'; state.csiParams = ''; return; }
                if (ch === ']') { state.escMode = 'osc'; return; }
                if (ch === 'P') { state.escMode = 'dcs'; return; }
                if (ch === '_') { state.escMode = 'apc'; return; }
                if (ch === '^') { state.escMode = 'pm';  return; }
                if (ch === 'X') { state.escMode = 'sos'; return; }
                if (ch === 'O') { state.escMode = 'ss3'; return; }
                if (code === 0x1b) { /* \x1b\x1b — рестарт */ state.escMode = 'init'; return; }
                // Одиночный ESC + любой другой байт — закончили.
                endEsc();
                return;
            case 'csi':
                // CSI: \x1b[ params final-byte. Final-byte = 0x40-0x7e.
                if (code >= 0x40 && code <= 0x7e) {
                    // Bracketed paste: ESC[200~ начинает вставку, ESC[201~ — конец.
                    if (ch === '~' && state.csiParams === '200') state.pasteMode = true;
                    else if (ch === '~' && state.csiParams === '201') state.pasteMode = false;
                    endEsc();
                } else {
                    state.csiParams += ch;
                }
                return;
            case 'osc':
            case 'dcs':
            case 'apc':
            case 'pm':
            case 'sos':
                // Терминатор: BEL (0x07) или ST (\x1b\\). 0x9c (8-bit ST)
                // — тоже валидно.
                if (code === 0x07 || code === 0x9c) { endEsc(); return; }
                if (code === 0x1b) { state.escMode = 'st-tail'; return; }
                return;
            case 'st-tail':
                // Ожидаем '\\' после \x1b. Если другой байт — считаем,
                // что новая sequence началась.
                if (ch === '\\') { endEsc(); return; }
                if (code === 0x1b) { return; }
                state.escMode = null;
                return;
            case 'ss3':
                // SS3: \x1bO + 1 байт — finishing byte.
                endEsc();
                return;
            default:
                endEsc();
                return;
        }
    }

    function onPtyInput(data) {
        if (typeof data !== 'string' || data.length === 0) return;
        for (let i = 0; i < data.length; i++) {
            const ch = data.charAt(i);
            const code = data.charCodeAt(i);

            // Если в skip-режиме — обрабатываем escape-байт.
            if (state.escMode) {
                processEscByte(code, ch);
                continue;
            }

            if (code === 0x1b) {
                startEsc();
                continue;
            }
            // Внутри вставки (bracketed paste) ничего не считаем командами.
            if (state.pasteMode) continue;
            if (ch === '\r' || ch === '\n') {
                const cmd = normalize(state.inputBuffer);
                state.inputBuffer = '';
                if (cmd) {
                    bumpFreq(cmd);
                    if (isMobile()) refresh();
                }
                continue;
            }
            if (code === 0x7f || code === 0x08) {
                state.inputBuffer = state.inputBuffer.slice(0, -1);
                continue;
            }
            if (code === 0x03 || code === 0x04) {
                // Ctrl+C / Ctrl+D — команда отменена.
                state.inputBuffer = '';
                continue;
            }
            if (code >= 0x20 && code < 0x7f) {
                state.inputBuffer += ch;
                continue;
            }
            if (code > 0x7f && code !== 0x9c) {
                // UTF-8 байты (multi-byte char) — копим как есть.
                state.inputBuffer += ch;
                continue;
            }
            // Прочие управляющие байты (Tab\t, BEL, и т.п.) — игнорируем,
            // чтобы не загрязнять буфер.
        }
    }

    // ===== Edit-UI (открывается из кнопки ✎) =====
    function buildKnownList() {
        const freq = getFreq();
        const pinned = new Set(getPinned());
        const set = new Set();
        for (const c of Object.keys(freq)) set.add(c);
        for (const c of pinned) set.add(c);
        for (const c of DEFAULTS) set.add(c);
        return Array.from(set).sort();
    }

    function ensureEditor() {
        if (state.editorEl) return state.editorEl;
        const overlay = document.createElement('div');
        overlay.id = 'quick-cmd-editor';
        overlay.className = 'quick-cmd-editor';
        overlay.hidden = true;
        overlay.innerHTML =
            '<div class="quick-cmd-editor-card">'
          +   '<header class="quick-cmd-editor-head">'
          +     '<h3>Quick commands</h3>'
          +     '<button type="button" class="quick-cmd-editor-close" title="Close">×</button>'
          +   '</header>'
          +   '<div class="quick-cmd-editor-add">'
          +     '<input type="text" class="quick-cmd-editor-input" placeholder="Add command…">'
          +     '<button type="button" class="quick-cmd-editor-addbtn">+ Add</button>'
          +   '</div>'
          +   '<ul class="quick-cmd-editor-list"></ul>'
          + '</div>';
        document.body.appendChild(overlay);

        const closeBtn = overlay.querySelector('.quick-cmd-editor-close');
        const addInput = overlay.querySelector('.quick-cmd-editor-input');
        const addBtn = overlay.querySelector('.quick-cmd-editor-addbtn');
        const listEl = overlay.querySelector('.quick-cmd-editor-list');

        function closeEditor() { overlay.hidden = true; }
        closeBtn.addEventListener('click', closeEditor);
        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) closeEditor();
        });

        function doAdd() {
            const cmd = normalize(addInput.value);
            if (!cmd) return;
            const freq = getFreq();
            if (typeof freq[cmd] !== 'number') freq[cmd] = 0;
            setFreq(freq);
            // Снимем hidden если был.
            const hidden = getHidden().filter((c) => c !== cmd);
            setHidden(hidden);
            addInput.value = '';
            renderList();
            refresh();
        }
        addBtn.addEventListener('click', doAdd);
        addInput.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') { e.preventDefault(); doAdd(); }
        });

        function renderList() {
            listEl.innerHTML = '';
            const all = buildKnownList();
            if (all.length === 0) {
                const li = document.createElement('li');
                li.className = 'quick-cmd-editor-empty';
                li.textContent = 'Нет команд. Добавьте через "+ Add".';
                listEl.appendChild(li);
                return;
            }
            const pinned = new Set(getPinned());
            const hidden = new Set(getHidden());
            for (const cmd of all) {
                const li = document.createElement('li');
                li.className = 'quick-cmd-editor-item';

                const label = document.createElement('span');
                label.className = 'quick-cmd-editor-cmd';
                label.textContent = cmd;
                li.appendChild(label);

                const actions = document.createElement('div');
                actions.className = 'quick-cmd-editor-actions';

                // pin/unpin
                const pinBtn = document.createElement('button');
                pinBtn.type = 'button';
                pinBtn.className = 'quick-cmd-editor-btn';
                pinBtn.textContent = pinned.has(cmd) ? '📌 unpin' : '📌 pin';
                pinBtn.addEventListener('click', () => {
                    const cur = getPinned();
                    if (cur.includes(cmd)) {
                        setPinned(cur.filter((c) => c !== cmd));
                    } else {
                        cur.push(cmd);
                        setPinned(cur);
                    }
                    renderList();
                    refresh();
                });
                actions.appendChild(pinBtn);

                // hide/show
                const hideBtn = document.createElement('button');
                hideBtn.type = 'button';
                hideBtn.className = 'quick-cmd-editor-btn';
                hideBtn.textContent = hidden.has(cmd) ? '👁 show' : '🚫 hide';
                hideBtn.addEventListener('click', () => {
                    const cur = getHidden();
                    if (cur.includes(cmd)) {
                        setHidden(cur.filter((c) => c !== cmd));
                    } else {
                        cur.push(cmd);
                        setHidden(cur);
                    }
                    renderList();
                    refresh();
                });
                actions.appendChild(hideBtn);

                // delete (из freq + pinned + hidden)
                const delBtn = document.createElement('button');
                delBtn.type = 'button';
                delBtn.className = 'quick-cmd-editor-btn quick-cmd-editor-del';
                delBtn.textContent = '🗑';
                delBtn.title = 'Delete from history';
                delBtn.addEventListener('click', () => {
                    const freq = getFreq();
                    delete freq[cmd];
                    setFreq(freq);
                    setPinned(getPinned().filter((c) => c !== cmd));
                    setHidden(getHidden().filter((c) => c !== cmd));
                    renderList();
                    refresh();
                });
                actions.appendChild(delBtn);

                li.appendChild(actions);
                listEl.appendChild(li);
            }
        }

        overlay._renderList = renderList;
        state.editorEl = overlay;
        return overlay;
    }

    function openEditor() {
        const overlay = ensureEditor();
        if (overlay._renderList) overlay._renderList();
        overlay.hidden = false;
    }

    // ===== TUI quick-keys bar (Phase C) =====
    //
    // Для каждой из трёх TUI-вкладок (git/docker/telescope) есть свой
    // <div class="tui-quick-bar"> в index.html. Бар наполняется кнопками
    // лениво при первом показе и затем переиспользуется. Видимость
    // переключается атрибутом hidden — показываем только на mobile
    // и только когда state.activeTab совпадает с целевой вкладкой.
    //
    // Источник activeTab: window.ForgeApp.state.activeTab (см. app.js).
    // Триггер обновления: MutationObserver на атрибуте hidden у #git/#docker/
    // #telescope. switchTab() переключает hidden — этого достаточно.

    function getActiveTab() {
        try {
            const app = window.ForgeApp;
            if (app && app.state && typeof app.state.activeTab === 'string') {
                return app.state.activeTab;
            }
        } catch (_) {}
        return null;
    }

    function ensureTuiBarFilled(barEl) {
        if (!barEl || barEl.dataset.filled === '1') return;
        barEl.innerHTML = '';
        for (const k of TUI_KEYS) {
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'tui-quick-key';
            btn.textContent = k.label;
            btn.dataset.bytes = k.bytes;
            btn.addEventListener('click', (e) => {
                e.preventDefault();
                e.stopPropagation();
                sendToPty(k.bytes);
            });
            barEl.appendChild(btn);
        }
        barEl.dataset.filled = '1';
    }

    function refreshTuiBars() {
        const showMobile = isMobile();
        const active = getActiveTab();
        for (const tab of Object.keys(TUI_BAR_IDS)) {
            const bar = document.getElementById(TUI_BAR_IDS[tab]);
            if (!bar) continue;
            const shouldShow = showMobile && active === tab;
            if (shouldShow) {
                ensureTuiBarFilled(bar);
                bar.hidden = false;
            } else {
                bar.hidden = true;
            }
        }
    }

    function initTuiBars() {
        // Сразу заполнить — кнопки статичные, нет смысла откладывать.
        for (const tab of Object.keys(TUI_BAR_IDS)) {
            const bar = document.getElementById(TUI_BAR_IDS[tab]);
            if (bar) ensureTuiBarFilled(bar);
        }
        // Подписаться на смену видимости TUI-вкладок (switchTab переключает hidden).
        try {
            const observer = new MutationObserver(() => refreshTuiBars());
            for (const tabId of ['git', 'docker', 'telescope']) {
                const el = document.getElementById(tabId);
                if (el) observer.observe(el, { attributes: true, attributeFilter: ['hidden'] });
            }
        } catch (_) {}
        refreshTuiBars();
    }

    // ===== Bootstrap =====
    function onMqChange() { refresh(); }

    // Миграция: чистим localStorage от мусорных команд, накопленных
    // предыдущей версией модуля (когда ESC-skip ловил не все escape
    // последовательности и фрагменты OSC-ответов вроде ']11;rgb:...'
    // попадали в freq). Применяем актуальный normalize() — всё, что не
    // проходит, удаляем.
    function migrateStorage() {
        try {
            const freq = getFreq();
            let changed = false;
            for (const key of Object.keys(freq)) {
                if (!normalize(key)) {
                    delete freq[key];
                    changed = true;
                }
            }
            if (changed) setFreq(freq);

            const pinned = getPinned();
            const cleanPinned = pinned.filter((c) => !!normalize(c));
            if (cleanPinned.length !== pinned.length) setPinned(cleanPinned);

            const hidden = getHidden();
            const cleanHidden = hidden.filter((c) => !!normalize(c));
            if (cleanHidden.length !== hidden.length) setHidden(cleanHidden);
        } catch (e) {
            console.warn('[quick-cmd] migrateStorage failed', e);
        }
    }

    function init() {
        migrateStorage();
        if (!ensureRefs()) {
            // Бар ещё не в DOM — отложить.
            setTimeout(init, 100);
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
        // Инициализация TUI quick-keys bars + наблюдатель за hidden.
        initTuiBars();
        refresh();
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

    // ===== Публичный API =====
    window.QuickCmd = {
        onPtyInput: onPtyInput,
        openEditor: openEditor,
        refresh: refresh,
        refreshTuiBars: refreshTuiBars,
    };
})();
