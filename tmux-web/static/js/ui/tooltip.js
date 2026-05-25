// tmux-web — кастомный tooltip (замена нативного title).
//
// Зачем: нативный `title` появляется с задержкой браузера (~0.5–1 с), не
// стилизуется и не обновляется, пока курсор висит над элементом. Этот модуль
// даёт мгновенную стилизованную подсказку.
//
// Контракт: один глобальный <div class="forge-tooltip"> на body. Показывается
// для любого элемента с атрибутом `data-tooltip="<текст>"`. Логика навешана
// через ДЕЛЕГИРОВАНИЕ на document (mouseover/mouseout/focusin/focusout) —
// поэтому работает и для элементов, которые перерисовываются динамически
// (sidebar пересобирается каждые 3 с при polling сессий), без переподписки.
//
// Позиционирование: position: fixed, над целевым элементом по центру, с
// зажимом в границы viewport. Текст берётся из data-tooltip в момент показа,
// поэтому всегда актуальный.

let tipEl = null;

// Лениво создаёт singleton-элемент подсказки.
function ensureTip() {
    if (tipEl) return tipEl;
    tipEl = document.createElement('div');
    tipEl.className = 'forge-tooltip';
    tipEl.setAttribute('role', 'tooltip');
    tipEl.setAttribute('aria-hidden', 'true');
    document.body.appendChild(tipEl);
    return tipEl;
}

// Позиционирует подсказку над target с зажимом в viewport.
function position(target) {
    const tip = ensureTip();
    const r = target.getBoundingClientRect();
    const tr = tip.getBoundingClientRect();
    const gap = 8;
    const margin = 6;

    // По центру цели по горизонтали.
    let left = r.left + r.width / 2 - tr.width / 2;
    left = Math.max(margin, Math.min(left, window.innerWidth - tr.width - margin));

    // Над целью; если не влезает сверху — снизу.
    let top = r.top - tr.height - gap;
    let placement = 'top';
    if (top < margin) {
        top = r.bottom + gap;
        placement = 'bottom';
    }

    tip.style.left = `${Math.round(left)}px`;
    tip.style.top = `${Math.round(top)}px`;
    tip.dataset.placement = placement;
}

function show(target) {
    const text = target.getAttribute('data-tooltip');
    if (!text) return;
    const tip = ensureTip();
    tip.textContent = text;
    tip.setAttribute('aria-hidden', 'false');
    tip.classList.add('visible');
    // Позиционируем после установки текста (нужны актуальные размеры).
    position(target);
}

function hide() {
    if (!tipEl) return;
    tipEl.classList.remove('visible');
    tipEl.setAttribute('aria-hidden', 'true');
}

// Находит ближайший вверх по дереву элемент с data-tooltip.
function closestTip(node) {
    if (!node || node.nodeType !== 1) return null;
    return node.closest('[data-tooltip]');
}

let inited = false;

// Навешивает делегированные обработчики на document. Идемпотентна.
export function initTooltips() {
    if (inited) return;
    inited = true;
    ensureTip();

    document.addEventListener('mouseover', (e) => {
        const target = closestTip(e.target);
        if (target) show(target);
    });
    document.addEventListener('mouseout', (e) => {
        const target = closestTip(e.target);
        // Скрываем только если ушли с tooltip-элемента наружу (а не на потомка).
        if (target && (!e.relatedTarget || !target.contains(e.relatedTarget))) {
            hide();
        }
    });
    // Доступность с клавиатуры.
    document.addEventListener('focusin', (e) => {
        const target = closestTip(e.target);
        if (target) show(target);
    });
    document.addEventListener('focusout', () => hide());
    // Прячем при скролле/ресайзе — позиция привязана к viewport.
    window.addEventListener('scroll', hide, true);
    window.addEventListener('resize', hide);
}
