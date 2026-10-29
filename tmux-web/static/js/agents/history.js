/** Historical agent metadata: costs stay separate and parallel time stays explicit. */
import {barChart, lineChart} from '../analytics/chart.js';
import {el, clear, count, cost, clock, duration, tokenTotal, shortPath, providerName, number} from './core.js';

function textDate(ms) {
    if (number(ms) === null) return 'Нет данных';
    const date = new Date(ms);
    return Number.isFinite(date.getTime()) ? date.toLocaleString('ru-RU', {dateStyle:'short', timeStyle:'short'}) : 'Нет данных';
}

function taskState(task) {
    if (task.completed === true) return 'Завершена';
    if (task.completed === false) return 'Прервана';
    return ({working:'Работает', waiting:'Ожидает', unknown:'Не завершена'})[task.state] || 'Не завершена';
}

function table(doc, headings, label) {
    const region = el(doc, 'div', 'agent-history-scroll');
    region.setAttribute('role', 'region'); region.setAttribute('aria-label', label); region.tabIndex = 0;
    const node = el(doc, 'table', 'agent-history-table');
    const head = el(doc, 'thead'); const row = el(doc, 'tr');
    for (const heading of headings) {const cell = el(doc, 'th', '', heading); cell.setAttribute('scope', 'col'); row.appendChild(cell);}
    head.appendChild(row); node.appendChild(head);
    const body = el(doc, 'tbody'); node.appendChild(body); region.appendChild(node);
    return {region, body};
}

function cell(doc, row, value, cls = '') {
    const node = el(doc, 'td', cls, value); row.appendChild(node); return node;
}

function metric(doc, host, label, format) {
    const wrap = el(doc, 'div', 'agent-history-metric');
    wrap.appendChild(el(doc, 'dt', '', label)); const value = el(doc, 'dd'); wrap.appendChild(value); host.appendChild(wrap);
    return (next) => {value.textContent = format(next);};
}

/** Full breakdown is available on demand; controls survive every data update. */
function breakdown(doc, title, noun) {
    const root = el(doc, 'section', 'agent-history-breakdown'); root.appendChild(el(doc, 'h3', '', title));
    const empty = el(doc, 'p', 'agent-muted', `Данных по ${noun} за выбранный период нет.`); root.appendChild(empty);
    const grid = table(doc, [title, 'Запросы', 'Токены', 'Оценка API', 'Сообщено'], title); root.appendChild(grid.region);
    const more = el(doc, 'button', 'agent-button'); more.type = 'button'; root.appendChild(more);
    let rows = [], expanded = false, previous = '';
    function render() {
        clear(grid.body); empty.hidden = rows.length > 0; grid.region.hidden = rows.length === 0;
        const shown = expanded ? rows : rows.slice(0, 12);
        for (const share of shown) {
            const row = el(doc, 'tr'); const totals = share.totals || {};
            const name = cell(doc, row, share.name || share.id || 'Неизвестно', 'agent-history-name');
            name.title = share.id || share.name || '';
            if (share.provider) name.appendChild(el(doc, 'small', 'agent-muted', providerName(share.provider)));
            cell(doc, row, count(totals.requests)); cell(doc, row, count(tokenTotal(totals.tokens)));
            cell(doc, row, cost(totals.estimatedCostUsd)); cell(doc, row, cost(totals.reportedCostUsd));
            if (number(totals.unpricedRequests) > 0) row.title = `${count(totals.unpricedRequests)} запросов без стоимости; оценка неполная.`;
            grid.body.appendChild(row);
        }
        more.hidden = rows.length <= 12;
        more.textContent = expanded ? 'Показать первые 12' : `Показать все (${count(rows.length)})`;
        more.setAttribute('aria-expanded', String(expanded));
    }
    more.addEventListener('click', () => {expanded = !expanded; render();});
    return {root, update(next) {const list = Array.isArray(next) ? next : []; const signature = JSON.stringify(list); if (signature !== previous) {rows = list; previous = signature; render();}}};
}

/**
 * Render an AgentSummary and a paginated list of AgentTask records.
 * Query ownership and network polling remain in the parent view controller.
 */
export function createAgentHistory(host, {onPageChange = () => {}, doc = document} = {}) {
    clear(host);
    const root = el(doc, 'div', 'agent-history'); host.appendChild(root);
    root.appendChild(el(doc, 'h2', '', 'История работы'));
    const metrics = el(doc, 'dl', 'agent-history-metrics'); root.appendChild(metrics);
    const setters = {
        tasks: metric(doc, metrics, 'Начато задач', count),
        requests: metric(doc, metrics, 'Запросы модели', count),
        tokens: metric(doc, metrics, 'Токены', (v) => count(tokenTotal(v))),
        agentTimeMs: metric(doc, metrics, 'Сумма времени агентов', duration),
        wallTimeMs: metric(doc, metrics, 'Время с учётом параллельной работы', duration),
        estimatedCostUsd: metric(doc, metrics, 'Оценка по API-тарифам', cost),
        reportedCostUsd: metric(doc, metrics, 'Стоимость от провайдера', cost),
    };
    const outcome = el(doc, 'p', 'agent-muted'); root.appendChild(outcome);
    const coverage = el(doc, 'p', 'agent-muted agent-history-coverage'); root.appendChild(coverage);
    const pricing = el(doc, 'p', 'agent-muted'); root.appendChild(pricing);
    const charts = el(doc, 'div', 'agent-history-charts'); root.appendChild(charts);
    function chartBox(title, wide = false) {
        const box = el(doc, 'section', `agent-history-chart${wide ? ' agent-history-chart-wide' : ''}`);
        box.appendChild(el(doc, 'h3', '', title)); const canvas = el(doc, 'div', 'agent-history-chart-canvas'); box.appendChild(canvas);
        const note = el(doc, 'p', 'agent-muted'); box.appendChild(note); charts.appendChild(box); return {canvas, note};
    }
    const tokensChart = chartBox('Токены по дням');
    const costChart = chartBox('Доступная оценка API по дням');
    const timeChart = chartBox('Время работы по дням', true);
    const plots = [
        barChart(tokensChart.canvas, {labels:[],values:[]}, {doc,height:210,title:'Токены по дням',formatValue:count,formatY:count,emptyText:'Нет данных о токенах.'}),
        barChart(costChart.canvas, {labels:[],values:[]}, {doc,height:210,title:'Доступная оценка API по дням',formatValue:cost,formatY:cost,emptyText:'Нет данных для оценки стоимости.'}),
        lineChart(timeChart.canvas, {labels:[],series:[]}, {doc,height:210,title:'Сумма времени агентов и время с учётом параллельной работы по дням',formatValue:duration,formatY:duration,emptyText:'Нет данных о времени работы.'}),
    ];
    const groups = el(doc, 'div', 'agent-history-breakdowns'); root.appendChild(groups);
    const models = breakdown(doc, 'Модели', 'моделям'); const projects = breakdown(doc, 'Проекты', 'проектам');
    groups.appendChild(models.root); groups.appendChild(projects.root);
    const history = el(doc, 'section', 'agent-history-sessions'); root.appendChild(history);
    history.appendChild(el(doc, 'h3', '', 'Задачи и сессии'));
    const sessionNote = el(doc, 'p', 'agent-muted', 'Токены Copilot могут появиться только после завершения всей CLI-сессии. «Нет данных» не означает нулевой расход.'); history.appendChild(sessionNote);
    const taskGrid = table(doc, ['Начало', 'Агент', 'Проект и модель', 'Состояние', 'Время', 'Токены', 'Оценка API', 'Сообщено'], 'История задач агентов'); history.appendChild(taskGrid.region);
    const noTasks = el(doc, 'p', 'agent-muted', 'За выбранный период задач нет.'); history.appendChild(noTasks);
    const pagination = el(doc, 'div', 'agent-history-pagination');
    const prev = el(doc, 'button', 'agent-button', 'Предыдущие'); const next = el(doc, 'button', 'agent-button', 'Следующие');
    prev.type = 'button'; next.type = 'button'; const page = el(doc, 'span', 'agent-muted'); page.setAttribute('aria-live', 'polite');
    pagination.appendChild(prev); pagination.appendChild(page); pagination.appendChild(next); history.appendChild(pagination);
    let currentOffset = 0, currentLimit = 50, destroyed = false, summarySignature = '', sessionsSignature = '';
    prev.addEventListener('click', () => {if (!prev.disabled) onPageChange(Math.max(0,currentOffset-currentLimit));});
    next.addEventListener('click', () => {if (!next.disabled) onPageChange(currentOffset+currentLimit);});

    function summaryView(summary) {
        const totals = summary.totals || {};
        for (const [key, set] of Object.entries(setters)) set(totals[key]);
        outcome.textContent = `Завершено: ${count(totals.completed)}. Прервано: ${count(totals.aborted)}. Незавершённых задач: ${count(totals.incompleteTasks)}.`;
        const notes = ['Время агентов суммирует задачи; параллельное время объединяет их пересечения. Две стоимости показаны отдельно и не складываются.'];
        if (number(totals.unpricedRequests) > 0) notes.push(`Частичная оценка: у ${count(totals.unpricedRequests)} запросов стоимость неизвестна.`);
        if (number(totals.unknownTokenRequests) > 0) notes.push(`У ${count(totals.unknownTokenRequests)} запросов нет данных о токенах.`);
        if (number(totals.incompleteTasks) > 0) notes.push('Время незавершённых задач может быть неполным.');
        coverage.textContent = notes.join(' ');
        const rates = summary.pricing || {};
        pricing.textContent = rates.date ? `API-оценка рассчитана по каталогу от ${rates.date}. Это оценка использования, а не счёт подписки.` : 'Дата каталога тарифов неизвестна; неизвестные модели не считаются бесплатными.';
        const series = Array.isArray(summary.series) ? summary.series : [];
        charts.hidden = series.length === 0;
        const knownTokens = series.filter(row => tokenTotal(row.totals && row.totals.tokens) !== null);
        plots[0].update({labels:knownTokens.map(row=>row.start),values:knownTokens.map(row=>tokenTotal(row.totals.tokens)),label:'Токены'});
        tokensChart.note.textContent = series.length > knownTokens.length ? `Дней без данных о токенах: ${count(series.length-knownTokens.length)}. Они не показаны как нулевые.` : 'Включены вход, выход и кэш. Токены рассуждений уже входят в выход.';
        const knownCost = series.filter(row => number(row.totals && row.totals.estimatedCostUsd) !== null);
        plots[1].update({labels:knownCost.map(row=>row.start),values:knownCost.map(row=>row.totals.estimatedCostUsd),label:'Оценка API'});
        const partialDays = knownCost.filter(row => number(row.totals.unpricedRequests) > 0).length;
        costChart.note.textContent = `Дней без оценки: ${count(series.length-knownCost.length)}. Дней с частичной оценкой: ${count(partialDays)}. Суммы от провайдера приведены отдельно.`;
        const knownTime = series.filter(row => number(row.totals && row.totals.agentTimeMs) !== null && number(row.totals.wallTimeMs) !== null);
        plots[2].update({labels:knownTime.map(row=>row.start),series:[{label:'Сумма времени агентов',values:knownTime.map(row=>row.totals.agentTimeMs)},{label:'С учётом параллельной работы',values:knownTime.map(row=>row.totals.wallTimeMs)}]});
        timeChart.note.textContent = 'Обе линии используют одну шкалу времени. Дневные границы определяет сервер.';
        models.update(summary.models); projects.update(summary.projects);
    }

    function sessionsView(rows) {
        clear(taskGrid.body); noTasks.hidden = rows.length > 0; taskGrid.region.hidden = rows.length === 0;
        for (const task of rows) {
            const row = el(doc, 'tr');
            cell(doc,row,textDate(task.startedAtMs)); cell(doc,row,providerName(task.provider));
            const project = cell(doc,row,shortPath(task.project),'agent-history-name'); project.title = task.project || '';
            project.appendChild(el(doc,'small','agent-muted',task.model || 'Неизвестная модель'));
            if (task.nativeId) project.appendChild(el(doc,'small','agent-history-id',String(task.nativeId).slice(0,12)));
            cell(doc,row,taskState(task));
            const time = number(task.durationMs) === null ? task.elapsedMs : task.durationMs;
            const timeCell = cell(doc,row,clock(time));
            if (task.completed === null || task.completed === undefined) timeCell.title = 'Время от начала по имеющимся данным; завершение не подтверждено.';
            const tokenCell = cell(doc,row,count(tokenTotal(task.tokens)));
            if (task.provider === 'copilot' && !task.tokens) tokenCell.title = 'Copilot сообщает итоговые токены после завершения CLI-сессии.';
            const estimate = cell(doc,row,cost(task.estimatedCostUsd));
            if (number(task.unpricedRequests) > 0) estimate.appendChild(el(doc,'small','agent-muted','Частичная оценка'));
            cell(doc,row,cost(task.reportedCostUsd)); taskGrid.body.appendChild(row);
        }
    }

    return {
        update(summary = {}, sessions = [], {offset = 0, limit = 50} = {}) {
            if (destroyed) return;
            currentOffset = Math.max(0,number(offset) || 0); currentLimit = Math.max(1,number(limit) || 50);
            const rows = Array.isArray(sessions) ? sessions : [];
            const data = summary && typeof summary === 'object' ? summary : {};
            const summaryKey = JSON.stringify(data);
            if (summaryKey !== summarySignature) {summarySignature = summaryKey; summaryView(data);}
            const sessionsKey = JSON.stringify(rows);
            if (sessionsKey !== sessionsSignature) {sessionsSignature = sessionsKey; sessionsView(rows);}
            prev.disabled = currentOffset === 0; next.disabled = rows.length < currentLimit;
            page.textContent = rows.length ? `Задачи ${count(currentOffset+1)}–${count(currentOffset+rows.length)}` : currentOffset ? `После ${count(currentOffset)} задач данных больше нет.` : 'Нет задач';
        },
        redraw() {if (!destroyed) for (const plot of plots) plot.redraw();},
        destroy() {destroyed = true; clear(host);},
    };
}
