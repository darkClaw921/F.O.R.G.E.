// tmux-web — Settings modal: вкладка «Интерфейс».
//
// Экспортирует buildInterfaceForm(settings, onSaved) — фабрика DOM-узла
// (fieldset) с тумблерами двух opt-in фич интерфейса. Использует те же классы,
// что notifications-tab.js / todo-tab.js (.notify-fieldset, .notify-hint,
// .notify-error, .notify-actions, .modal-check) — править CSS не требуется.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   cmd_hints_enabled  — bool (checkbox), default false
//   next_step_enabled  — bool (checkbox), default false
//
// ВАЖНО про дефолты: обе фичи выключены при нулевой конфигурации, поэтому
// чекбоксы инициализируются строго `=== true`. Идиома `!== false` (как в
// echo/settings.js для echo_notifications_enabled) здесь НЕПРИМЕНИМА — у той
// настройки дефолт включённый, и копирование инвертировало бы наш.
//
// Save шлёт только свои два поля: PATCH применяет лишь Some(..)-варианты, так
// что остальные настройки не затрагиваются (см. todo-tab.js).

import { updateUserSettings } from './user-settings-api.js';

// Строит форму. settings может быть null/{} — тогда обе фичи считаются
// выключенными (совпадает с backend-дефолтом).
// onSaved(updatedSettings) — callback после успешного PATCH.
export function buildInterfaceForm(settings, onSaved) {
    const s = settings || {};

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Интерфейс';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Дополнительные возможности интерфейса. По умолчанию выключены — ' +
        'включайте те, которыми пользуетесь.';
    fs.appendChild(hint);

    // 1) checkbox: cmd_hints_enabled
    const cmdWrap = document.createElement('label');
    cmdWrap.className = 'modal-check notify-check';
    const cmd = document.createElement('input');
    cmd.type = 'checkbox';
    cmd.className = 'cmd-hints-enabled';
    cmd.checked = s.cmd_hints_enabled === true;
    cmd.title =
        'Удержание ⌘ дольше 200 мс показывает на кнопках и ссылках бейджи с ' +
        'буквенными кодами: набрать код — сработает клик. Обычные ⌘-шорткаты ' +
        '(⌘C, ⌘B и т.п.) продолжают работать.';
    cmdWrap.appendChild(cmd);
    cmdWrap.appendChild(document.createTextNode(' Подсказки хоткеев при удержании ⌘'));
    fs.appendChild(cmdWrap);

    const cmdHint = document.createElement('div');
    cmdHint.className = 'notify-hint';
    cmdHint.textContent =
        'Удержать Command → на кликабельных элементах появятся буквенные ' +
        'коды; набрать код — элемент нажмётся.';
    fs.appendChild(cmdHint);

    // 2) checkbox: next_step_enabled
    const stepWrap = document.createElement('label');
    stepWrap.className = 'modal-check notify-check';
    const step = document.createElement('input');
    step.type = 'checkbox';
    step.className = 'next-step-enabled';
    step.checked = s.next_step_enabled === true;
    step.title =
        'Когда Claude в сессии затихает на 10+ секунд, Echo генерирует через ' +
        'Claude CLI предложение следующего шага. Карточка сессии светится ' +
        'голубым, по наведению открывается попап с текстом и кнопкой ' +
        '«Отправить в терминал». Выключено — подсказки не генерируются вовсе.';
    stepWrap.appendChild(step);
    stepWrap.appendChild(document.createTextNode(' Подсказки «Следующий шаг» (свечение сессий)'));
    fs.appendChild(stepWrap);

    const stepHint = document.createElement('div');
    stepHint.className = 'notify-hint';
    stepHint.textContent =
        'Затихшая сессия светится голубым, когда готова подсказка что делать ' +
        'дальше. Генерация идёт через Claude CLI и расходует токены — ' +
        'выключенная настройка полностью останавливает воркер.';
    fs.appendChild(stepHint);

    // Inline error block (hidden by default).
    const err = document.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    // Inline success indicator (показывается на 2 сек после Save).
    const ok = document.createElement('div');
    ok.className = 'notify-hint interface-save-ok';
    ok.style.display = 'none';
    ok.textContent = 'Сохранено';
    fs.appendChild(ok);

    // Save button.
    const actions = document.createElement('div');
    actions.className = 'notify-actions';
    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary';
    saveBtn.textContent = 'Save';
    saveBtn.addEventListener('click', async () => {
        err.style.display = 'none';
        err.textContent = '';
        ok.style.display = 'none';
        saveBtn.disabled = true;

        const payload = {
            cmd_hints_enabled: !!cmd.checked,
            next_step_enabled: !!step.checked,
        };

        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
            if (typeof onSaved === 'function') onSaved(updated);
        } catch (e) {
            err.style.display = '';
            err.textContent = (e && e.message) ? e.message : 'Не удалось сохранить настройки.';
        } finally {
            saveBtn.disabled = false;
        }
    });
    actions.appendChild(saveBtn);
    fs.appendChild(actions);

    return fs;
}
