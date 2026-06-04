// tmux-web — интерактивный hover-попап «Следующий шаг».
//
// Зачем: для сессии с классом .has-next-step (есть предложение в
// state.nextSteps) при наведении показывает ИНТЕРАКТИВНЫЙ попап, в котором
// можно:
//   - отредактировать текст предложения в <textarea> и «Отправить в терминал»
//     (POST /api/echo/next-steps/:session/send {text}) — доставляет актуальный
//     текст из textarea в tmux-сессию;
//   - ввести коррекцию «Что нужно было сделать» и «Сохранить»
//     (POST /api/echo/next-steps/:session/feedback {correction}) — пишет правило
//     памяти на бэкенде.
//
// Отличие от js/ui/tooltip.js: тот tooltip пассивный (исчезает при mouseout
// сразу), а этот УДЕРЖИВАЕТСЯ, пока курсор над попапом — иначе нельзя было бы
// печатать в textarea и кликать кнопки. Реализация: делегированный mouseover на
// .session-item.has-next-step ПОКАЗЫВАЕТ попап; скрытие отложено (таймер), а
// mouseenter самого попапа отменяет скрытие. Уходим только когда курсор покинул
// и сессию, и попап.
//
// После любого действия локально снимаем свечение/попап оптимистично — WS
// NextStepEvent{has_suggestion:false} затем подтвердит (echo/ws.js перефетчит
// и перерендерит сайдбар). Один singleton-попап на body, как forge-tooltip.

import { state } from '../core/state.js';
import { renderSidebar } from '../sidebar/sidebar.js';
import { sendNextStep, feedbackNextStep } from '../echo/api.js';
import { notify } from '../echo/notifications.js';

let popupEl = null;
let currentSession = null;     // имя сессии, для которой сейчас открыт попап
let hideTimer = null;          // setTimeout handle отложенного скрытия
const HIDE_DELAY_MS = 160;     // люфт на переход курсора item → попап

// Лениво создаёт singleton-элемент попапа с разметкой.
function ensurePopup() {
    if (popupEl) return popupEl;
    const el = document.createElement('div');
    el.className = 'next-step-popup';
    el.setAttribute('role', 'dialog');
    el.setAttribute('aria-hidden', 'true');
    el.innerHTML = `
        <div class="nsp-header">Следующий шаг</div>
        <textarea class="nsp-text" rows="4"
            placeholder="Предложение следующего шага…"></textarea>
        <div class="nsp-actions">
            <button type="button" class="nsp-send">Отправить в терминал</button>
        </div>
        <div class="nsp-fb-label">Что нужно было сделать</div>
        <textarea class="nsp-correction" rows="2"
            placeholder="Коррекция (запишется в правила)…"></textarea>
        <div class="nsp-actions">
            <button type="button" class="nsp-save">Сохранить</button>
        </div>
    `;
    document.body.appendChild(el);

    // Курсор над попапом — отменяем отложенное скрытие, чтобы можно было
    // печатать и нажимать кнопки.
    el.addEventListener('mouseenter', cancelHide);
    // Ушли с попапа — планируем скрытие (если не вернулись на item/попап).
    el.addEventListener('mouseleave', scheduleHide);

    el.querySelector('.nsp-send').addEventListener('click', onSend);
    el.querySelector('.nsp-save').addEventListener('click', onSave);

    popupEl = el;
    return el;
}

// Позиционирует попап справа от .session-item (sidebar слева), с зажимом в
// viewport. position: fixed.
function position(target) {
    const el = ensurePopup();
    const r = target.getBoundingClientRect();
    const pr = el.getBoundingClientRect();
    const gap = 10;
    const margin = 8;

    // Справа от элемента сайдбара; если не влезает — слева.
    let left = r.right + gap;
    if (left + pr.width > window.innerWidth - margin) {
        left = r.left - pr.width - gap;
    }
    left = Math.max(margin, Math.min(left, window.innerWidth - pr.width - margin));

    // По верхней кромке item; зажим снизу.
    let top = r.top;
    top = Math.max(margin, Math.min(top, window.innerHeight - pr.height - margin));

    el.style.left = `${Math.round(left)}px`;
    el.style.top = `${Math.round(top)}px`;
}

// Показывает попап для сессии: заполняет textarea текстом предложения.
function show(target, sessionName) {
    const suggestion = state.nextSteps && state.nextSteps[sessionName];
    if (!suggestion) return;
    const el = ensurePopup();
    currentSession = sessionName;

    // Если открываем попап для ДРУГОЙ сессии — обновляем поля. Если для той же
    // (например, повторный mouseover) — не затираем то, что пользователь уже
    // печатает.
    if (el.dataset.session !== sessionName) {
        el.dataset.session = sessionName;
        el.querySelector('.nsp-text').value = suggestion.content || '';
        el.querySelector('.nsp-correction').value = '';
    }

    el.setAttribute('aria-hidden', 'false');
    el.classList.add('visible');
    cancelHide();
    // Подгоняем высоту textarea предложения под содержимое (в пределах CSS
    // max-height) — чтобы длинное предложение было видно целиком, а не в окне
    // из 4 строк. Делаем после .visible (нужен ненулевой scrollHeight).
    autoSizeText(el.querySelector('.nsp-text'));
    // Позиционируем после показа (нужны актуальные размеры).
    position(target);
}

// Растит textarea под её содержимое; верхняя граница задаётся CSS max-height
// (при превышении внутри textarea появляется собственный скролл).
function autoSizeText(ta) {
    if (!ta) return;
    ta.style.height = 'auto';
    ta.style.height = `${ta.scrollHeight}px`;
}

function hide() {
    cancelHide();
    if (!popupEl) return;
    popupEl.classList.remove('visible');
    popupEl.setAttribute('aria-hidden', 'true');
    popupEl.dataset.session = '';
    currentSession = null;
}

function cancelHide() {
    if (hideTimer) {
        clearTimeout(hideTimer);
        hideTimer = null;
    }
}

function scheduleHide() {
    cancelHide();
    hideTimer = setTimeout(hide, HIDE_DELAY_MS);
}

// Оптимистично убирает предложение из state и перерисовывает сайдбар, затем
// прячет попап. WS-событие потом подтвердит (или восстановит при ошибке через
// очередной poll).
function clearLocal(sessionName) {
    if (state.nextSteps && state.nextSteps[sessionName]) {
        delete state.nextSteps[sessionName];
    }
    hide();
    renderSidebar();
}

async function onSend() {
    const el = ensurePopup();
    const sessionName = currentSession;
    if (!sessionName) return;
    const text = el.querySelector('.nsp-text').value;
    clearLocal(sessionName);
    try {
        await sendNextStep(sessionName, text);
    } catch (e) {
        notify({ level: 'error', title: 'Отправка не удалась', body: e.message || String(e) });
    }
}

async function onSave() {
    const el = ensurePopup();
    const sessionName = currentSession;
    if (!sessionName) return;
    const correction = el.querySelector('.nsp-correction').value.trim();
    if (!correction) {
        notify({ level: 'warn', title: 'Пустая коррекция', body: 'Введите, что нужно было сделать' });
        return;
    }
    clearLocal(sessionName);
    try {
        await feedbackNextStep(sessionName, correction);
    } catch (e) {
        notify({ level: 'error', title: 'Сохранение не удалось', body: e.message || String(e) });
    }
}

let inited = false;

// Навешивает делегированные обработчики на document. Идемпотентна.
// Делегирование переживает пересборку сайдбара каждые 3с (poll/WS) без
// переподписки — как в js/ui/tooltip.js.
export function initNextStepPopup() {
    if (inited) return;
    inited = true;
    ensurePopup();

    document.addEventListener('mouseover', (e) => {
        if (e.target.nodeType !== 1) return;
        const item = e.target.closest('.session-item.has-next-step');
        if (item && item.dataset.session) {
            show(item, item.dataset.session);
        }
    });

    document.addEventListener('mouseout', (e) => {
        if (e.target.nodeType !== 1) return;
        const item = e.target.closest('.session-item.has-next-step');
        if (!item) return;
        // Ушли с item: если перешли на попап или его потомка — не скрываем
        // (mouseenter попапа отменит scheduleHide в любом случае).
        const to = e.relatedTarget;
        if (to && popupEl && (to === popupEl || popupEl.contains(to))) return;
        scheduleHide();
    });

    // Прячем при скролле/ресайзе — позиция привязана к viewport. НО: скролл
    // ВНУТРИ самого попапа (textarea предложения / тело попапа) не должен его
    // закрывать — иначе нельзя прочитать длинный текст прокруткой. Поэтому
    // игнорируем scroll-события, исходящие из попапа (capture-фаза → e.target
    // = реально скроллящийся элемент).
    window.addEventListener('scroll', (e) => {
        if (popupEl && e.target && e.target.nodeType === 1 && popupEl.contains(e.target)) return;
        hide();
    }, true);
    window.addEventListener('resize', hide);
}
