// tmux-web — Echo orchestrator (Phase 5c)
//
// initEcho() — однократная инициализация всего UI Echo-вкладки:
//   1. Заполнить model-picker.
//   2. Привязать input + sendCb (отправка user_message через WS).
//   3. Привязать sidebar-табы (Chats / Auto / Memory).
//   4. Подгрузить список conversations и выбрать первую (или создать).
//   5. Инициализировать autonomous / memory pane'ы (lazy refresh).
//   6. Запустить stats polling.
//
// connectEchoWs(conversationId) / disconnectEchoWs() — wrap ws.js с
// зарегистрированными handlers'ами. Используется switchTab лифцикл
// (см. tui-tabs.js / tabs.js) и bootstrap'ом.

import {
    $echoConversationsList, $echoNewChat,
    $echoSidebarTabChats, $echoSidebarTabAuto, $echoSidebarTabMemory,
    $echoConversations, $echoAutonomous, $echoMemory,
    $echoModelPicker,
} from '../core/dom.js';

import {
    listConversations, createConversation, listMessages,
} from './api.js';

import {
    renderConversation, appendMessage, appendChunk,
    finalizeMessage, showThinking, clearMessages, bindInput,
} from './chat.js';

import {
    connectEchoWs as wsConnect, disconnectEchoWs as wsDisconnect,
    sendUserMessage, sendActionInvoke,
} from './ws.js';

import { initModelPicker, getSelectedModel } from './model-picker.js';
import { initAutonomousPane, refreshAutonomous } from './autonomous.js';
import { initMemoryPane, refreshMemory } from './memory.js';
import { notify, notifyFromServerMsg } from './notifications.js';
import { renderActionButtons } from './action-buttons.js';
import { initStats, stopStats, updateFromWs as updateStatsFromWs } from './stats.js';

const state = {
    initialized: false,
    activeConversationId: null,
    unbindInput: null,
    bootstrapPromise: null,
};

/** Singleton init. Безопасно вызывать многократно. */
export function initEcho() {
    if (state.initialized) return;
    state.initialized = true;

    initModelPicker((newModel) => {
        notify({ level: 'info', title: 'Модель', body: newModel, ttl: 1500 });
    });

    state.unbindInput = bindInput((text) => {
        if (!state.activeConversationId) {
            notify({
                level: 'warn',
                title: 'Нет чата',
                body: 'Создайте новый чат сначала',
            });
            return;
        }
        // Optimistic: показываем сразу user-message + thinking-индикатор.
        appendMessage({
            id: '',
            role: 'user',
            content: text,
            created_at: Math.floor(Date.now() / 1000),
        });
        // Пушим WS; run_id ещё неизвестен — он придёт в первом
        // assistant_chunk; chat.appendChunk создаст карточку под него.
        const ok = sendUserMessage({
            text,
            conversation_id: state.activeConversationId,
            model: getSelectedModel(),
        });
        if (!ok) {
            notify({
                level: 'error',
                title: 'WS disconnected',
                body: 'Сообщение не отправлено',
            });
        }
    });

    bindSidebarTabs();
    if ($echoNewChat) {
        $echoNewChat.addEventListener('click', async () => {
            await createAndOpenChat();
        });
    }

    initAutonomousPane();
    initMemoryPane();
    initStats(30000);

    // Старт фоновой подгрузки списка чатов.
    state.bootstrapPromise = refreshChatsList().then(() => {
        if (!state.activeConversationId) {
            // Если нет чатов вообще — создадим первый.
            return createAndOpenChat();
        }
    }).catch((e) => {
        console.warn('[echo-main] bootstrap failed', e);
    });
}

function bindSidebarTabs() {
    const tabs = [
        { btn: $echoSidebarTabChats, pane: $echoConversations },
        { btn: $echoSidebarTabAuto, pane: $echoAutonomous },
        { btn: $echoSidebarTabMemory, pane: $echoMemory },
    ];
    tabs.forEach(({ btn, pane }) => {
        if (!btn || !pane) return;
        btn.addEventListener('click', () => {
            tabs.forEach((t) => {
                if (t.btn) t.btn.classList.remove('active');
                if (t.pane) t.pane.hidden = true;
            });
            btn.classList.add('active');
            pane.hidden = false;
            if (pane === $echoAutonomous) refreshAutonomous();
            if (pane === $echoMemory) refreshMemory();
        });
    });
}

async function refreshChatsList() {
    if (!$echoConversationsList) return;
    let data;
    try {
        data = await listConversations(null);
    } catch (e) {
        // Сообщение ошибки может содержать произвольный текст — вставляем как
        // textContent, чтобы исключить HTML-инъекцию.
        $echoConversationsList.innerHTML = '';
        const errLi = document.createElement('li');
        errLi.className = 'echo-empty';
        errLi.textContent = `Ошибка: ${(e && e.message) || e}`;
        $echoConversationsList.appendChild(errLi);
        return;
    }
    const items = (data && data.items) || [];
    $echoConversationsList.innerHTML = '';
    if (items.length === 0) {
        $echoConversationsList.innerHTML = '<li class="echo-empty">Нет чатов</li>';
        return;
    }
    for (const c of items) {
        const li = document.createElement('li');
        li.className = 'echo-conv-item';
        li.dataset.id = c.id;
        if (c.id === state.activeConversationId) li.classList.add('active');
        const title = document.createElement('div');
        title.className = 'echo-conv-title';
        title.textContent = c.title || '(no title)';
        li.appendChild(title);
        const meta = document.createElement('div');
        meta.className = 'echo-conv-meta';
        const dt = c.updated_at ? new Date(c.updated_at * 1000).toLocaleString() : '';
        meta.textContent = `${c.model || 'sonnet'} · ${dt}`;
        li.appendChild(meta);
        li.addEventListener('click', () => openChat(c.id));
        $echoConversationsList.appendChild(li);
    }
    // Если есть выбранный — переоткроем для актуальности.
    if (!state.activeConversationId && items.length > 0) {
        openChat(items[0].id);
    }
}

async function openChat(conversationId) {
    if (state.activeConversationId === conversationId) return;
    state.activeConversationId = conversationId;
    // Подсветка active-item.
    if ($echoConversationsList) {
        $echoConversationsList.querySelectorAll('.echo-conv-item').forEach((el) => {
            el.classList.toggle('active', el.dataset.id === conversationId);
        });
    }
    clearMessages();
    try {
        const data = await listMessages(conversationId, { limit: 200 });
        // Гонка: пока ждали ответ listMessages, пользователь мог переключиться
        // на другой чат. Без этой проверки сообщения старого чата отрендерились
        // бы в активный (чужой) — отбрасываем устаревший ответ.
        if (state.activeConversationId !== conversationId) return;
        renderConversation((data && data.items) || []);
    } catch (e) {
        // Та же гонка для пути ошибки — не показываем тост о чужом чате.
        if (state.activeConversationId !== conversationId) return;
        notify({ level: 'error', title: 'Load failed', body: e.message });
    }
    // Подключим WS к этой conversation.
    connectEchoWs(conversationId);
}

async function createAndOpenChat() {
    try {
        const model = $echoModelPicker ? $echoModelPicker.value : getSelectedModel();
        const created = await createConversation({
            title: 'Новый чат',
            projectId: null,
            model,
        });
        await refreshChatsList();
        await openChat(created.id);
    } catch (e) {
        notify({ level: 'error', title: 'Create failed', body: e.message });
    }
}

// -------- WS lifecycle exports --------

/** Подключить WS к указанной conversation; регистрирует handlers. */
export function connectEchoWs(conversationId) {
    if (!conversationId) return;
    wsConnect(conversationId, buildHandlers());
}

export function disconnectEchoWs() {
    wsDisconnect();
}

/** Cleanup для beforeunload — снимает поллинги, WS. */
export function teardownEcho() {
    wsDisconnect();
    stopStats();
    if (state.unbindInput) state.unbindInput();
    state.initialized = false;
}

// -------- WS handlers --------

function buildHandlers() {
    return {
        assistant_chunk: (msg) => {
            appendChunk(msg.run_id, msg.kind, msg.delta);
        },
        assistant_done: (msg) => {
            finalizeMessage(msg.run_id, msg.message_id, msg.usage);
        },
        action_buttons: (msg) => {
            const msgEl = document.querySelector(
                `.echo-msg[data-message-id="${cssEscape(msg.message_id)}"]`,
            );
            if (!msgEl) return;
            renderActionButtons(msgEl, msg.actions || [], (descriptor) => {
                sendActionInvoke(descriptor.id, descriptor.params || {});
            });
        },
        notification: (msg) => {
            notifyFromServerMsg(msg);
        },
        autonomous_task_event: (msg) => {
            const level = msg.status === 'error' ? 'error'
                : msg.status === 'success' ? 'info' : 'info';
            notify({
                level,
                title: `Auto: ${msg.status}`,
                body: msg.message_preview || msg.task_id,
                ttl: 3500,
            });
        },
        stats_update: (msg) => {
            updateStatsFromWs(msg);
        },
        error: (msg) => {
            notify({ level: 'error', title: msg.code || 'Error', body: msg.message || '' });
        },
        resync: () => {
            // Сервер сообщил, что WS-подписчик отстал и пропустил часть
            // realtime-чанков (broadcast Lagged). Перечитываем переписку через
            // REST, чтобы восстановить целостность вместо показа оборванного
            // ответа.
            const cid = state.activeConversationId;
            if (!cid) return;
            listMessages(cid, { limit: 200 })
                .then((data) => {
                    if (state.activeConversationId !== cid) return;
                    clearMessages();
                    renderConversation((data && data.items) || []);
                })
                .catch((e) => {
                    notify({ level: 'error', title: 'Resync failed', body: e.message });
                });
        },
        open: () => {
            // Можно подсветить статус-индикатор.
        },
        close: () => {
            // На close мы автоматически реконнектимся — не паникуем.
        },
    };
}

// Минимальный CSS.escape polyfill для старых браузеров.
function cssEscape(s) {
    if (typeof CSS !== 'undefined' && CSS.escape) return CSS.escape(s);
    return String(s).replace(/[^a-zA-Z0-9_-]/g, (c) => '\\' + c);
}

/** Тестовый exporter — текущее состояние. */
export function _debugState() {
    return {
        activeConversationId: state.activeConversationId,
        initialized: state.initialized,
    };
}
