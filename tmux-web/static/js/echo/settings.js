// tmux-web — Settings modal: Echo tab (Phase 6 — settings, hardening).
//
// Экспортирует:
//   renderEchoSettingsTab(containerEl, currentSettings) — рендерит форму
//     с полями: default_model (select+input), notifications_enabled (checkbox).
//     Read-only поля (cli_path, max_parallel_runs) пока опускаем — серверный
//     /api/echo/healthz их не выдаёт; в Phase 7 (post-release) можно
//     добавить /api/echo/config — пока показываем подсказки.
//
// Save: PATCH /api/user-settings через user-settings-api.js (optimistic +
// rollback). onSaved(updatedSettings) вызывается на успехе.
//
// Стилизация: те же CSS-классы, что и todo-tab/notifications-tab —
// .notify-fieldset, .notify-field, .notify-hint, .notify-error,
// .notify-actions, .modal-check. Без правок CSS.

import { updateUserSettings } from '../settings/user-settings-api.js';

// Несколько типичных моделей Claude — UI-помощник, не строгий enum.
// Пользователь может ввести любую через свободный input «custom…».
// На момент Phase 6 актуальный canonical alias — claude-3-5-sonnet-latest.
const MODEL_PRESETS = [
    { value: '', label: '— use plugin default —' },
    { value: 'claude-3-5-sonnet-latest', label: 'claude-3-5-sonnet-latest (default)' },
    { value: 'claude-3-5-haiku-latest', label: 'claude-3-5-haiku-latest' },
    { value: 'claude-opus-4-latest', label: 'claude-opus-4-latest' },
    { value: '__custom__', label: 'Custom…' },
];

/**
 * Строит DOM-узел формы. settings может быть null — тогда используются дефолты.
 * onSaved(updatedSettings) — callback после успешного PATCH.
 *
 * @param {object|null} settings — текущий UserSettings (или null)
 * @param {(updated: object) => void} [onSaved]
 * @returns {HTMLFieldSetElement}
 */
export function buildEchoSettingsForm(settings, onSaved) {
    const s = settings || {};

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset echo-settings-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Echo (chat assistant)';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Настройки чат-ассистента Э.Х.О: модель по умолчанию для новых '
        + 'conversation\'ов и тосты-нотификации (autonomous events и action results).';
    fs.appendChild(hint);

    // 1) Default model — select из пресетов + опциональный custom-input.
    const modelWrap = document.createElement('label');
    modelWrap.className = 'notify-field';
    modelWrap.textContent = 'Default model';
    const modelSel = document.createElement('select');
    modelSel.className = 'echo-default-model-select';
    for (const opt of MODEL_PRESETS) {
        const o = document.createElement('option');
        o.value = opt.value;
        o.textContent = opt.label;
        modelSel.appendChild(o);
    }
    const currentModel = (typeof s.echo_default_model === 'string' && s.echo_default_model) || '';
    const isPreset = MODEL_PRESETS.some((p) => p.value === currentModel);
    if (isPreset) {
        modelSel.value = currentModel;
    } else if (currentModel) {
        modelSel.value = '__custom__';
    } else {
        modelSel.value = '';
    }
    modelWrap.appendChild(modelSel);
    fs.appendChild(modelWrap);

    const customWrap = document.createElement('label');
    customWrap.className = 'notify-field';
    customWrap.textContent = 'Custom model name';
    const customInput = document.createElement('input');
    customInput.type = 'text';
    customInput.className = 'echo-default-model-custom';
    customInput.placeholder = 'e.g. claude-3-5-sonnet-20241022';
    customInput.value = isPreset ? '' : currentModel;
    customWrap.appendChild(customInput);
    fs.appendChild(customWrap);
    customWrap.style.display = modelSel.value === '__custom__' ? '' : 'none';

    modelSel.addEventListener('change', () => {
        customWrap.style.display = modelSel.value === '__custom__' ? '' : 'none';
        if (modelSel.value !== '__custom__') {
            customInput.value = '';
        }
    });

    // 2) Notifications toggle.
    const notifWrap = document.createElement('label');
    notifWrap.className = 'modal-check notify-check';
    const notif = document.createElement('input');
    notif.type = 'checkbox';
    notif.className = 'echo-notifications-enabled';
    // Дефолт true: если undefined в settings — считаем включённым.
    notif.checked = s.echo_notifications_enabled !== false;
    notif.title = 'Если выключено — Echo toast-нотификации не показываются (autonomous events / action results / errors).';
    notifWrap.appendChild(notif);
    notifWrap.appendChild(document.createTextNode(' Показывать toast-нотификации Echo'));
    fs.appendChild(notifWrap);

    // 3) Read-only info-блок (cli/max_parallel пока серверный endpoint не отдаёт).
    const infoHint = document.createElement('div');
    infoHint.className = 'notify-hint';
    infoHint.textContent =
        'CLI path, max_parallel_runs и daily token cap настраиваются через '
        + '~/.config/forge/echo.toml или переменные окружения FORGE_ECHO_*.';
    fs.appendChild(infoHint);

    // 4) Actions: save / error label.
    const actions = document.createElement('div');
    actions.className = 'notify-actions';

    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    actions.appendChild(saveBtn);

    const status = document.createElement('span');
    status.className = 'notify-error';
    status.style.marginLeft = '8px';
    actions.appendChild(status);

    fs.appendChild(actions);

    saveBtn.addEventListener('click', async () => {
        status.textContent = '';
        status.className = 'notify-error';
        saveBtn.disabled = true;

        // Собираем payload. echo_default_model: '' (sentinel) сбросит в null
        // на backend. Для preset из MODEL_PRESETS отправляем его value;
        // для custom — текст из input'а.
        let model;
        if (modelSel.value === '__custom__') {
            model = (customInput.value || '').trim();
        } else {
            model = modelSel.value || '';
        }
        const payload = {
            echo_default_model: model,
            echo_notifications_enabled: !!notif.checked,
        };

        try {
            const updated = await updateUserSettings(payload);
            status.textContent = 'Saved';
            status.className = 'notify-hint';
            if (typeof onSaved === 'function') onSaved(updated);
        } catch (e) {
            status.textContent = 'Save failed: ' + (e && e.message ? e.message : e);
            status.className = 'notify-error';
        } finally {
            saveBtn.disabled = false;
        }
    });

    return fs;
}

/**
 * Удобная обёртка для модального диспетчера: рендерит форму внутрь
 * переданного контейнера, очищая его содержимое.
 */
export function renderEchoSettingsTab(containerEl, currentSettings, onSaved) {
    if (!containerEl) return;
    containerEl.innerHTML = '';
    containerEl.appendChild(buildEchoSettingsForm(currentSettings, onSaved));
}
