// tmux-web — Settings modal: «Сводка дня» tab (Daily summary feature, Phase 5)
//
// Экспортирует renderDailySummaryTab(container, { onClose }) — наполняет
// переданный контейнер DOM-узлами вкладки настроек «Сводка дня».
//
// Содержимое вкладки:
//   1) Кнопка «Сгенерировать сводку за сегодня» — запускает generateDailyReport()
//      в ФОНЕ (промис не блокирует UI, модалку можно закрыть). По завершении —
//      glass-уведомление справа (notify({ glass: true })). Флаг _bgGenerating
//      на уровне модуля защищает от параллельных запусков.
//   2) Кнопка «Открыть страницу» — закрывает settings-модалку через onClose()
//      и вызывает window.ForgeApp.showDailySummary() (точка интеграции из Phase 4).
//
// Использует общий стиль классов настроек (.notify-fieldset, .notify-hint,
// .notify-actions) и класс статуса .rs-test-status (pending/ok/error/warn),
// поэтому не требует правок CSS.

import {
    generateDailyReport,
    getDailyReportPrompts,
    saveDailyReportPrompts,
} from '../echo/api.js';
import { notify } from '../echo/notifications.js';

// Идёт ли уже фоновая генерация сводки за сегодня — защита от дублей даже
// после закрытия/повторного открытия модалки (флаг на уровне модуля).
let _bgGenerating = false;

// Наполняет контейнер вкладки. onClose — колбэк закрытия модалки (передаётся
// из modal.js). settings не используется — вкладка не хранит пользовательских
// настроек, только действия. Параметр сохранён для единообразия с другими
// renderXxxTab/buildXxxForm фабриками.
export function renderDailySummaryTab(container, { onClose } = {}) {
    container.innerHTML = '';

    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Сводка дня';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Сгенерируйте текстовую сводку того, что было сделано за сегодня, ' +
        'либо откройте отдельную страницу со сводками по дням.';
    fs.appendChild(hint);

    const actions = document.createElement('div');
    actions.className = 'notify-actions ds-actions';

    const genBtn = document.createElement('button');
    genBtn.type = 'button';
    genBtn.className = 'primary ds-generate';
    genBtn.textContent = 'Сгенерировать сводку за сегодня';
    actions.appendChild(genBtn);

    const openBtn = document.createElement('button');
    openBtn.type = 'button';
    openBtn.className = 'ds-open';
    openBtn.textContent = 'Открыть страницу';
    actions.appendChild(openBtn);

    const status = document.createElement('span');
    status.className = 'rs-test-status ds-status';
    actions.appendChild(status);

    fs.appendChild(actions);
    container.appendChild(fs);

    // Фоновая генерация: запрос уходит в фоне (промис НЕ блокирует UI —
    // модалку можно закрыть), а по завершении показывается glass-уведомление
    // справа. Кнопка тут же разблокируется; повторный запуск защищён флагом
    // уровня модуля _bgGenerating.
    genBtn.addEventListener('click', () => {
        if (_bgGenerating) {
            status.textContent = 'Уже генерируется в фоне…';
            status.className = 'rs-test-status ds-status pending';
            return;
        }
        _bgGenerating = true;
        status.textContent = 'Запущена в фоне — уведомим по готовности';
        status.className = 'rs-test-status ds-status pending';

        generateDailyReport()
            .then((report) => {
                const day = report && report.day ? report.day : '';
                notify({
                    glass: true,
                    level: 'info',
                    title: 'Сводка дня готова',
                    body: (day ? day + ' — ' : '') + 'нажмите «Открыть страницу», чтобы посмотреть.',
                    ttl: 8000,
                });
            })
            .catch((e) => {
                notify({
                    glass: true,
                    level: 'error',
                    title: 'Не удалось собрать сводку',
                    body: errMsg(e),
                    ttl: 9000,
                });
            })
            .finally(() => {
                _bgGenerating = false;
            });
    });

    openBtn.addEventListener('click', () => {
        if (typeof onClose === 'function') onClose();
        if (window.ForgeApp && typeof window.ForgeApp.showDailySummary === 'function') {
            window.ForgeApp.showDailySummary();
        }
    });

    // --- Промпты генерации -------------------------------------------------
    container.appendChild(buildPromptsFieldset());
}

// Строит fieldset «Промпты генерации»: два textarea (report_prompt,
// suggest_prompt) + кнопка «Сохранить промпты» + по кнопке «↺ дефолт» рядом с
// каждым textarea. Значения загружаются асинхронно через getDailyReportPrompts;
// дефолты сохраняются в замыкании и подставляются кнопками сброса (которые
// дополнительно шлют пустую строку соответствующего поля для сброса оверрайда).
function buildPromptsFieldset() {
    const fs = document.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = document.createElement('legend');
    legend.textContent = 'Промпты генерации';
    fs.appendChild(legend);

    const hint = document.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent =
        'Кастомные промпты для генерации сводки и предложений задач. ' +
        'Пустое поле = используется дефолтный промпт. Кнопка «↺ дефолт» ' +
        'сбрасывает поле к встроенному значению.';
    fs.appendChild(hint);

    // Дефолты, заполняются после загрузки; используются кнопками сброса.
    const defaults = { report: '', suggest: '' };

    const reportTa = makePromptTextarea('Промпт сводки');
    const suggestTa = makePromptTextarea('Промпт предложений задач');

    fs.appendChild(reportTa.field);
    fs.appendChild(suggestTa.field);

    const actions = document.createElement('div');
    actions.className = 'notify-actions ds-prompts-actions';

    const saveBtn = document.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary ds-save-prompts';
    saveBtn.textContent = 'Сохранить промпты';
    actions.appendChild(saveBtn);

    const status = document.createElement('span');
    status.className = 'rs-test-status ds-prompts-status';
    actions.appendChild(status);

    fs.appendChild(actions);

    // Применяет состояние (ответ API) к textarea и дефолтам.
    function applyState(state) {
        if (!state) return;
        reportTa.textarea.value = state.report_prompt || '';
        suggestTa.textarea.value = state.suggest_prompt || '';
        defaults.report = state.report_prompt_default || '';
        defaults.suggest = state.suggest_prompt_default || '';
    }

    function setStatus(text, cls) {
        status.textContent = text;
        status.className = 'rs-test-status ds-prompts-status' + (cls ? ' ' + cls : '');
    }

    // Загрузка текущих значений.
    setStatus('Загрузка…', 'pending');
    getDailyReportPrompts()
        .then((state) => {
            applyState(state);
            setStatus('', '');
        })
        .catch((e) => {
            setStatus('Ошибка загрузки: ' + errMsg(e), 'error');
        });

    saveBtn.addEventListener('click', async () => {
        saveBtn.disabled = true;
        setStatus('Сохранение…', 'pending');
        try {
            const state = await saveDailyReportPrompts({
                report_prompt: reportTa.textarea.value,
                suggest_prompt: suggestTa.textarea.value,
            });
            applyState(state);
            setStatus('Сохранено', 'ok');
        } catch (e) {
            setStatus('Ошибка: ' + errMsg(e), 'error');
        } finally {
            saveBtn.disabled = false;
        }
    });

    // Кнопка «↺ дефолт» подставляет дефолт в textarea и сбрасывает оверрайд
    // (пустая строка) только для своего поля.
    reportTa.resetBtn.addEventListener('click', () =>
        resetField('report', reportTa.textarea));
    suggestTa.resetBtn.addEventListener('click', () =>
        resetField('suggest', suggestTa.textarea));

    async function resetField(key, textarea) {
        const otherKey = key === 'report' ? 'suggest' : 'report';
        const otherTa = key === 'report' ? suggestTa.textarea : reportTa.textarea;
        textarea.value = defaults[key] || '';
        saveBtn.disabled = true;
        setStatus('Сброс…', 'pending');
        try {
            // Пустая строка для сбрасываемого поля; текущее значение второго
            // поля сохраняем без изменений.
            const body = {};
            body[key + '_prompt'] = '';
            body[otherKey + '_prompt'] = otherTa.value;
            const state = await saveDailyReportPrompts(body);
            applyState(state);
            setStatus('Сброшено к дефолту', 'ok');
        } catch (e) {
            setStatus('Ошибка: ' + errMsg(e), 'error');
        } finally {
            saveBtn.disabled = false;
        }
    }

    return fs;
}

// Создаёт обёртку .notify-field с лейблом, кнопкой «↺ дефолт» и textarea.
// Возвращает { field, textarea, resetBtn }.
function makePromptTextarea(labelText) {
    const field = document.createElement('label');
    field.className = 'notify-field ds-prompt-field';

    const head = document.createElement('div');
    head.className = 'ds-prompt-head';

    const span = document.createElement('span');
    span.textContent = labelText;
    head.appendChild(span);

    const resetBtn = document.createElement('button');
    resetBtn.type = 'button';
    resetBtn.className = 'ds-prompt-reset';
    resetBtn.textContent = '↺ дефолт';
    head.appendChild(resetBtn);

    field.appendChild(head);

    const textarea = document.createElement('textarea');
    textarea.className = 'ds-prompt-textarea';
    textarea.rows = 8;
    textarea.spellcheck = false;
    field.appendChild(textarea);

    return { field, textarea, resetBtn };
}

function errMsg(e) {
    return e && e.message ? e.message : String(e);
}
