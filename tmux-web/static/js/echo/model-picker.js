// tmux-web — Echo model picker (Phase 5c)
//
// Заполняет <select id='echo-model-picker'> списком моделей и сохраняет
// выбор в localStorage под ключом 'forge.echo.model'.
//
// Список моделей — хардкод, потому что claude CLI не даёт listing-API.
// При расширении Claude SDK можно подменить getDefaultModels(), не трогая
// callers. Имя выводимое в UI должно совпадать с тем, что принимает
// `claude --model` (см. plugins/echo/src/claude/mod.rs).

import { $echoModelPicker } from '../core/dom.js';

const LS_KEY = 'forge.echo.model';

const DEFAULT_MODELS = [
    { id: 'claude-opus-4-5', label: 'Opus 4.5' },
    { id: 'claude-3-5-sonnet-latest', label: 'Sonnet 3.5 (latest)' },
    { id: 'claude-3-5-haiku-latest', label: 'Haiku 3.5 (latest)' },
];

/** Текущая выбранная модель (читается из LS, fallback — первая в списке). */
export function getSelectedModel() {
    try {
        const v = localStorage.getItem(LS_KEY);
        if (v) return v;
    } catch (_) {}
    return DEFAULT_MODELS[0].id;
}

/** Список моделей для UI. Возвращает копию массива. */
export function listModels() {
    return DEFAULT_MODELS.slice();
}

/**
 * Заполнить <select> и навесить change-handler.
 * Идемпотентен: повторный init очищает existing options и наполняет заново.
 */
export function initModelPicker(onChange) {
    if (!$echoModelPicker) return;
    $echoModelPicker.innerHTML = '';
    const current = getSelectedModel();
    for (const m of DEFAULT_MODELS) {
        const opt = document.createElement('option');
        opt.value = m.id;
        opt.textContent = m.label;
        if (m.id === current) opt.selected = true;
        $echoModelPicker.appendChild(opt);
    }
    $echoModelPicker.addEventListener('change', (ev) => {
        const v = ev.target.value;
        try { localStorage.setItem(LS_KEY, v); } catch (_) {}
        if (typeof onChange === 'function') {
            try { onChange(v); } catch (e) { console.warn('[echo] model-picker onChange threw', e); }
        }
    });
}
