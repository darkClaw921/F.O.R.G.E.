// tmux-web — Echo chat renderer (Phase 5c)
//
// Чистый DOM-renderer без зависимостей. Без сторонних библиотек —
// «лёгкий» markdown (code blocks через <pre>, inline-code через <code>,
// автолинки) и без подсветки синтаксиса. Это намеренно: vanilla bundle,
// CDN-зависимости только для xterm.js (уже подключён в index.html).

import {
    $echoMessages, $echoInput, $echoSend,
} from '../core/dom.js';
import { renderMarkdownInto } from '../core/markdown.js';

// In-memory map: messageId → элемент сообщения (для streaming append).
// Заполняется при render и updateChunk. Очищается при clearMessages.
const _messageEls = new Map();

// Стрим-буферы по run_id: накапливаем дельты до assistant_done, чтобы
// финализация имела доступ к собранному тексту (для подсветки и т.п.).
const _streamBuffers = new Map(); // run_id → { msgEl, contentEl, text }

/**
 * Полная перерисовка истории. Используется при открытии чата / переключении.
 *
 * @param {Array<{id, role, content, created_at, tokens_in, tokens_out}>} messages
 */
export function renderConversation(messages) {
    if (!$echoMessages) return;
    $echoMessages.innerHTML = '';
    _messageEls.clear();
    _streamBuffers.clear();
    if (!Array.isArray(messages)) return;
    for (const m of messages) {
        const el = buildMessageNode(m);
        $echoMessages.appendChild(el);
        if (m.id) _messageEls.set(m.id, el);
    }
    scrollToBottom();
}

/**
 * Добавить одно сообщение (для optimistic insert при отправке user_message).
 */
export function appendMessage(msg) {
    if (!$echoMessages) return;
    const el = buildMessageNode(msg);
    $echoMessages.appendChild(el);
    if (msg.id) _messageEls.set(msg.id, el);
    scrollToBottom();
    return el;
}

/**
 * Принимает стрим-дельту от сервера. `runId` идентифицирует stream —
 * по нему мы группируем чанки одного assistant-ответа. Если для runId
 * ещё нет сообщения — создаём pending-карточку (без id, добавим при
 * `finalizeMessage`).
 *
 * @param {string} runId
 * @param {'text'|'thinking'|'tool_use'} kind
 * @param {string} delta
 */
export function appendChunk(runId, kind, delta) {
    if (!$echoMessages) return;
    let buf = _streamBuffers.get(runId);
    if (!buf) {
        const msgEl = buildMessageNode({
            id: '',
            role: 'assistant',
            content: '',
            created_at: Math.floor(Date.now() / 1000),
        });
        msgEl.dataset.streaming = '1';
        msgEl.dataset.runId = runId;
        const contentEl = msgEl.querySelector('.echo-msg-content');
        $echoMessages.appendChild(msgEl);
        buf = { msgEl, contentEl, text: '', kindDivs: {}, anim: {} };
        _streamBuffers.set(runId, buf);
    }
    // Разные kinds — разные подэлементы (text, thinking, tool_use).
    if (!buf.kindDivs[kind]) {
        const k = document.createElement('div');
        k.className = `echo-msg-chunk echo-msg-chunk-${kind}`;
        if (kind === 'thinking') {
            k.dataset.label = 'thinking';
        } else if (kind === 'tool_use') {
            k.dataset.label = 'tool';
        }
        buf.contentEl.appendChild(k);
        buf.kindDivs[kind] = k;
    }
    // Кладём delta в очередь typewriter-анимации и (пере)запускаем tick.
    // Если предыдущая анимация ещё идёт — новый chunk просто допишется в
    // pending и продолжит выводиться без шва.
    if (!buf.anim[kind]) {
        buf.anim[kind] = { pending: '', running: false };
    }
    buf.anim[kind].pending += delta;
    _scheduleTypewriter(buf, kind);
}

// Сколько кадров целимся потратить на вывод текущего pending'а.
// 18 кадров ≈ 300мс @ 60fps. Чем больше pending — тем быстрее печатаем,
// чтобы догнать поток и не отставать. Минимум 1 символ за кадр.
const TYPEWRITER_TARGET_FRAMES = 18;

function _scheduleTypewriter(buf, kind) {
    const a = buf.anim[kind];
    if (a.running) return;
    a.running = true;
    const k = buf.kindDivs[kind];
    const isText = kind === 'text';
    const tick = () => {
        // Buf мог быть удалён (другой чат / finalize) — выходим тихо.
        if (!buf.kindDivs[kind]) {
            a.running = false;
            return;
        }
        const len = a.pending.length;
        if (len === 0) {
            a.running = false;
            return;
        }
        const charsThisFrame = Math.max(1, Math.ceil(len / TYPEWRITER_TARGET_FRAMES));
        const part = a.pending.slice(0, charsThisFrame);
        a.pending = a.pending.slice(charsThisFrame);
        k.appendChild(document.createTextNode(part));
        if (isText) buf.text += part;
        scrollToBottom();
        if (a.pending.length > 0) {
            requestAnimationFrame(tick);
        } else {
            a.running = false;
        }
    };
    requestAnimationFrame(tick);
}

function _flushAllPending(buf) {
    if (!buf.anim) return;
    for (const kind of Object.keys(buf.anim)) {
        const a = buf.anim[kind];
        if (!a || !a.pending) continue;
        const k = buf.kindDivs[kind];
        if (!k) continue;
        if (a.pending.length > 0) {
            k.appendChild(document.createTextNode(a.pending));
            if (kind === 'text') buf.text += a.pending;
            a.pending = '';
        }
    }
}

/**
 * Завершить streaming-сообщение: проставить реальный messageId, отрендерить
 * markdown для text-чанков, показать usage stats.
 */
export function finalizeMessage(runId, messageId, usage) {
    const buf = _streamBuffers.get(runId);
    if (!buf) return;
    // Дочистим всё что не успело допечататься в typewriter — сразу же,
    // без анимации (она к этому моменту уже почти всегда закончилась,
    // но если был очень большой финальный chunk — flush'им остаток).
    _flushAllPending(buf);
    buf.msgEl.removeAttribute('data-streaming');
    if (messageId) {
        buf.msgEl.dataset.messageId = messageId;
        _messageEls.set(messageId, buf.msgEl);
    }
    // Перерендерим text-часть с markdown.
    const textDiv = buf.kindDivs.text;
    if (textDiv && buf.text) {
        textDiv.innerHTML = '';
        renderMarkdownInto(textDiv, buf.text);
    }
    // Footer с usage.
    if (usage) {
        const f = document.createElement('div');
        f.className = 'echo-msg-usage';
        const inT = usage.input_tokens || 0;
        const outT = usage.output_tokens || 0;
        const cacheR = usage.cache_read_input_tokens || 0;
        const cacheC = usage.cache_creation_input_tokens || 0;
        f.textContent = `↓ ${inT} ↑ ${outT}${cacheR ? ` cache ${cacheR}` : ''}${cacheC ? ` create ${cacheC}` : ''}`;
        buf.msgEl.appendChild(f);
    }
    _streamBuffers.delete(runId);
    scrollToBottom();
}

/**
 * Показать индикатор «thinking» — пока стрим не пришёл первым чанком.
 * Вызывается после отправки user_message, удаляется при appendChunk/finalizeMessage.
 */
export function showThinking(runId) {
    if (!$echoMessages) return;
    if (_streamBuffers.has(runId)) return;
    const msgEl = buildMessageNode({
        id: '',
        role: 'assistant',
        content: '',
        created_at: Math.floor(Date.now() / 1000),
    });
    msgEl.dataset.streaming = '1';
    msgEl.dataset.runId = runId;
    msgEl.dataset.thinking = '1';
    const contentEl = msgEl.querySelector('.echo-msg-content');
    const ind = document.createElement('div');
    ind.className = 'echo-msg-thinking-dots';
    ind.innerHTML = '<span></span><span></span><span></span>';
    contentEl.appendChild(ind);
    $echoMessages.appendChild(msgEl);
    _streamBuffers.set(runId, { msgEl, contentEl, text: '', kindDivs: {} });
    scrollToBottom();
}

/**
 * Привязать input + кнопку отправки. `sendCb(text)` вызывается при Enter
 * (без Shift). Возвращает функцию для cleanup.
 */
export function bindInput(sendCb) {
    if (!$echoInput) return () => {};
    const onKeyDown = (ev) => {
        if (ev.key === 'Enter' && !ev.shiftKey && !ev.ctrlKey && !ev.metaKey) {
            ev.preventDefault();
            submit();
        }
    };
    const onClick = () => submit();
    function submit() {
        const text = ($echoInput.value || '').trim();
        if (!text) return;
        $echoInput.value = '';
        autoResize();
        try { sendCb(text); } catch (e) { console.warn('[echo-chat] sendCb threw', e); }
    }
    function autoResize() {
        if (!$echoInput) return;
        $echoInput.style.height = 'auto';
        $echoInput.style.height = Math.min(200, $echoInput.scrollHeight) + 'px';
    }
    const onInput = () => autoResize();
    $echoInput.addEventListener('keydown', onKeyDown);
    $echoInput.addEventListener('input', onInput);
    if ($echoSend) $echoSend.addEventListener('click', onClick);
    autoResize();
    return () => {
        $echoInput.removeEventListener('keydown', onKeyDown);
        $echoInput.removeEventListener('input', onInput);
        if ($echoSend) $echoSend.removeEventListener('click', onClick);
    };
}

/** Полностью очистить рендер (используется при переключении чата). */
export function clearMessages() {
    if ($echoMessages) $echoMessages.innerHTML = '';
    _messageEls.clear();
    _streamBuffers.clear();
}

// -------- helpers --------

function buildMessageNode(msg) {
    const role = msg.role || 'assistant';
    const el = document.createElement('div');
    el.className = `echo-msg echo-msg-${role}`;
    if (msg.id) el.dataset.messageId = msg.id;
    const head = document.createElement('div');
    head.className = 'echo-msg-head';
    head.textContent = role === 'user' ? 'Вы' : 'Э.Х.О';
    el.appendChild(head);
    const content = document.createElement('div');
    content.className = 'echo-msg-content';
    if (msg.content) {
        renderMarkdownInto(content, msg.content);
    }
    el.appendChild(content);
    if (msg.tokens_in || msg.tokens_out) {
        const f = document.createElement('div');
        f.className = 'echo-msg-usage';
        const inT = msg.tokens_in || 0;
        const outT = msg.tokens_out || 0;
        f.textContent = `↓ ${inT} ↑ ${outT}`;
        el.appendChild(f);
    }
    return el;
}

function scrollToBottom() {
    if (!$echoMessages) return;
    // requestAnimationFrame чтобы DOM успел layout.
    requestAnimationFrame(() => {
        $echoMessages.scrollTop = $echoMessages.scrollHeight;
    });
}

// markdown-рендер вынесен в core/markdown.js (renderMarkdownInto).
