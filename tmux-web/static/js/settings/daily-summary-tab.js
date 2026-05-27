// tmux-web — Settings modal: «Сводка дня» tab (Daily summary feature, Phase 5)
//
// Экспортирует renderDailySummaryTab(container, { onClose }) — наполняет
// переданный контейнер DOM-узлами вкладки настроек «Сводка дня».
//
// Содержимое вкладки:
//   1) Кнопка «Сгенерировать сводку за сегодня» — вызывает generateDailyReport()
//      без аргумента day (= сегодня). Рядом статус-строка с классами
//      pending/ok/error (паттерн как у «Test connection» в remotes-tab.js).
//   2) Кнопка «Открыть страницу» — закрывает settings-модалку через onClose()
//      и вызывает window.ForgeApp.showDailySummary() (точка интеграции из Phase 4).
//
// Использует общий стиль классов настроек (.notify-fieldset, .notify-hint,
// .notify-actions) и класс статуса .rs-test-status (pending/ok/error/warn),
// поэтому не требует правок CSS.

import { generateDailyReport } from '../echo/api.js';

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

    genBtn.addEventListener('click', async () => {
        genBtn.disabled = true;
        status.textContent = 'Генерируется…';
        status.className = 'rs-test-status ds-status pending';
        try {
            const report = await generateDailyReport();
            const day = report && report.day ? report.day : '';
            status.textContent = 'Готово' + (day ? ' — ' + day : '');
            status.className = 'rs-test-status ds-status ok';
        } catch (e) {
            status.textContent = 'Ошибка: ' + (e && e.message ? e.message : String(e));
            status.className = 'rs-test-status ds-status error';
        } finally {
            genBtn.disabled = false;
        }
    });

    openBtn.addEventListener('click', () => {
        if (typeof onClose === 'function') onClose();
        if (window.ForgeApp && typeof window.ForgeApp.showDailySummary === 'function') {
            window.ForgeApp.showDailySummary();
        }
    });
}
