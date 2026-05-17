// tmux-web — xterm.js initialization, sendResize, status, placeholder
// (Phase 1 ES Modules refactor)
//
// 1:1 копии из IIFE `tmux-web/static/app.js`:
//   - initTerminal         (app.js:248)
//   - sendResize           (app.js:460)
//   - scheduleResizeFromTerm (app.js:1661)
//   - setStatus            (app.js:1674)
//   - showPlaceholder      (app.js:1683)
//
// Зависимости:
//   - state из core/state.js (state.term/fitAddon/webLinksAddon/ws/encoder/
//     lastResizeKey)
//   - $terminalEl, $statusDot, $statusText, $placeholder из core/dom.js

import { state } from '../core/state.js';
import {
    $terminalEl,
    $statusDot,
    $statusText,
    $placeholder,
} from '../core/dom.js';

/**
 * Инициализация xterm.js Terminal.
 * @param {object|null} termTheme — xterm ITheme (результат mapTermTheme).
 *   Если null — используется fallback-палитра (offline / API недоступен).
 *   Тема обязана прийти ДО new Terminal — xterm рендерит фон сразу при
 *   open(), и присвоение options.theme после этого пересчитывает только
 *   глифы, оставляя background-canvas от старой темы до следующего
 *   полного перерисова.
 */
export function initTerminal(termTheme) {
    // Доступ к глобалам, которые подключены через CDN <script>:
    // window.Terminal, window.FitAddon, window.WebLinksAddon
    const Terminal = window.Terminal;
    const FitAddon = window.FitAddon && window.FitAddon.FitAddon;
    const WebLinksAddon = window.WebLinksAddon && window.WebLinksAddon.WebLinksAddon;

    if (!Terminal || !FitAddon || !WebLinksAddon) {
        console.error('xterm.js / addons not loaded — проверь CDN-ссылки');
        return;
    }

    // Fallback-палитра для случая, когда /api/themes/active недоступен
    // (offline, dev без backend). Совпадает с историческим hard-coded
    // объектом, чтобы поведение не регрессировало при сбоях API.
    const fallbackTheme = {
        background: '#000000',
        foreground: '#d8dee9',
        cursor: '#d8dee9',
        selectionBackground: '#3a4356',
    };

    const term = new Terminal({
        cursorBlink: true,
        fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
        fontSize: 13,
        scrollback: 5000,
        allowProposedApi: true,
        theme: termTheme || fallbackTheme,
    });

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();
    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);

    term.open($terminalEl);
    // Первичная подгонка под контейнер.
    try {
        fitAddon.fit();
    } catch (e) {
        console.warn('initial fit failed', e);
    }

    // Ввод пользователя — bytes → WS (cx2.5).
    term.onData((data) => {
        if (window.QuickCmd && typeof window.QuickCmd.onPtyInput === 'function') {
            try { window.QuickCmd.onPtyInput(data); } catch (e) { console.debug('[quick-cmd] onPtyInput failed', e); }
        }
        if (state.ws && state.ws.readyState === WebSocket.OPEN) {
            state.ws.send(state.encoder.encode(data));
        }
    });

    // Автоматический onResize от xterm (например при font-size change) —
    // тоже шлём в PTY.
    term.onResize(({ cols, rows }) => {
        sendResize(cols, rows);
    });

    state.term = term;
    state.fitAddon = fitAddon;
    state.webLinksAddon = webLinksAddon;

    // ResizeObserver на контейнер #terminal — каждый раз когда меняется размер
    // окна / sidebar / шрифт, делаем fit() + шлём resize в PTY (cx2.6).
    const ro = new ResizeObserver(() => {
        if (!state.fitAddon) return;
        try {
            state.fitAddon.fit();
        } catch (_) { /* xterm может бросить если контейнер 0×0 */ }
    });
    ro.observe($terminalEl);

    // Дополнительно слушаем window resize (страховка для старых браузеров).
    window.addEventListener('resize', () => {
        try { state.fitAddon && state.fitAddon.fit(); } catch (_) {}
    });
}

export function sendResize(cols, rows) {
    if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return;
    const key = cols + 'x' + rows;
    if (key === state.lastResizeKey) return;
    state.lastResizeKey = key;
    try {
        state.ws.send(JSON.stringify({ type: 'resize', cols, rows }));
    } catch (e) {
        console.warn('resize send failed', e);
    }
}

export function scheduleResizeFromTerm() {
    if (!state.term) return;
    const cols = state.term.cols;
    const rows = state.term.rows;
    // Сбросим lastResizeKey чтобы повторно отправить (после switch).
    state.lastResizeKey = '';
    sendResize(cols, rows);
}

export function setStatus(kind, text) {
    $statusDot.classList.remove(
        'status-connected', 'status-connecting',
        'status-disconnected', 'status-error',
    );
    $statusDot.classList.add('status-' + kind);
    $statusText.textContent = text;
}

export function showPlaceholder(visible) {
    if (visible) {
        $placeholder.classList.remove('hidden');
    } else {
        $placeholder.classList.add('hidden');
    }
}
