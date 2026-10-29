// tmux-web — Settings modal: вкладка «Аналитика» (Фаза 6 дашборда аналитики).
//
// Экспортирует `renderAnalyticsTab(host, deps)` — компактную сводку за
// последние 30 дней внутри модалки настроек: шесть плиток, спарклайн сообщений
// по дням и две кнопки — «Открыть в новой вкладке» и «Пересобрать индекс».
//
// ============================================================================
// ЧТО ЗДЕСЬ ВАЖНО
// ============================================================================
//
// 1. **ВКЛАДКА — ВИТРИНА, А НЕ ВТОРОЙ ДАШБОРД.** Всё, что требует площади
//    (тринадцать чартов, селектор проекта, периоды), живёт в полноэкранном вью
//    (`js/analytics/analytics.js`). Здесь ровно столько, сколько читается за
//    пару секунд, плюс дверь в полный дашборд. Вторая копия сетки карточек в
//    модалке шириной в 720px была бы нечитаемой и потребовала бы правки в двух
//    местах при каждом изменении метрик.
//
// 2. **ИНДЕКСАЦИЯ ЛЕНИВАЯ, И ЭТО ВИДНО.** Транскрипты не читаются на старте
//    devforge: первый же запрос к `/api/analytics/*` запускает скан, и до его
//    конца сервер честно отвечает 200 с `state: "scanning"`. Поэтому вкладка
//    обязана уметь показать прогресс-бар вместо цифр — иначе первое открытие
//    выглядит как «аналитика сломана и показывает нули».
//
// 3. **`ready` ПРИ ИДУЩЕМ ДОБОРЕ — ЭТО ДАННЫЕ.** Индекс поднят из кэша, а в
//    фоне дочитываются хвосты дописанных файлов. Подменять готовые цифры
//    прогресс-баром значило бы прятать рабочую сводку ради технической
//    подробности, поэтому такой случай рисует данные и лишь помечает, что они
//    сейчас обновятся. Развилка вынесена в `viewMode()` и общая с вью.
//
// 4. **СЕТЬ ТОЛЬКО ИНЪЕКЦИЕЙ.** Ни одного `fetch` и ни одного импорта
//    `analytics/api.js`: три функции приходят в `deps` из `settings/modal.js`.
//    Так вкладка проверяется в голом Node, без браузера и сервера, и так же
//    доказано, что она не ходит никуда, кроме трёх известных роутов.
//    Спарклайн — тоже дep (`sparkline`), по умолчанию `lineChart` из кита:
//    кит рисует через `createElementNS`, которого у крошечного фейкового
//    документа тестов нет.
//
// 5. **РАЗМЕТКА УЗЛАМИ, БЕЗ `innerHTML`.** Значения приходят с сервера (в том
//    числе тексты ошибок), и строковая сборка была бы точкой внедрения.
//    Классы — общие: `notify-fieldset`/`notify-hint`/`notify-actions` от
//    соседних вкладок и `analytics-tile*`/`analytics-progress*` из
//    `css/analytics.css`, поэтому своего CSS вкладка почти не требует.
//
// Все пять обещаний проверяются `tests/frontend/settings_analytics_tab.test.mjs`.

import {
    formatCompact, formatCount, formatMs, formatPercent, formatUsd, lineChart,
} from '../analytics/chart.js';
import {
    computeDelta, isEmptySummary, progressPercent, rangeToQuery, viewMode,
} from '../analytics/view-core.js';

/** Период компактной сводки — месяц, как умолчание полноэкранного вью. */
export const TAB_RANGE = '30';

/** Шаг опроса `status`, мс. Тот же, что во вью: полоса не должна дёргаться. */
const POLL_MS = 1500;

/**
 * Адрес дашборда для новой вкладки.
 *
 * Относительный, а не `location.origin + …`: в remote-режиме страница отдаётся
 * с того же origin, и абсолютная сборка только добавила бы способ ошибиться.
 */
export const ANALYTICS_URL = '/?view=analytics';

/**
 * Состав плиток вкладки — семь штук, в порядке важности.
 *
 * Первые две — `hero`: медианная пауза перед ответом и доля ответов быстрее
 * десяти секунд. Это тот самый вопрос «читаю ли я код, прежде чем согласиться»,
 * ради которого дашборд и затевался, и в компактной сводке он обязан стоять
 * первым, а не теряться между стоимостью и токенами.
 *
 * `better` задаёт, какое направление считать хорошим: цвет дельты берётся от
 * него, а НЕ от знака. Рост стоимости не «зелёный», а падение медианной паузы
 * — не «отлично», это ровно сигнал автопилота.
 */
export const TAB_TILES = [
    {
        key: 'medianPauseMs',
        label: 'Медианная пауза перед ответом',
        format: formatMs,
        better: 'up',
        hero: true,
        note: 'паузы дольше 30 мин исключены',
    },
    {
        key: 'shareUnder10s',
        label: 'Ответов быстрее 10 с — «не читая»',
        format: formatPercent,
        better: 'down',
        hero: true,
    },
    {
        key: 'ackShare',
        label: 'Односложных подтверждений',
        format: formatPercent,
        better: 'down',
        hero: true,
        note: '«ок», «продолжай» — согласие без формулировки',
    },
    { key: 'humanMessages', label: 'Сообщений человека', format: formatCount, better: null },
    { key: 'sessions', label: 'Сессий', format: formatCount, better: null },
    { key: 'costUsd', label: 'Стоимость', format: formatUsd, better: null },
    {
        key: 'tokensOut',
        label: 'Токены in / out',
        better: null,
        format: (v, kpi) => `${formatCompact(kpi.tokensIn)} / ${formatCompact(v)}`,
    },
];

// ---------------------------------------------------------------------------
// Мелкие DOM-помощники (узлами, без innerHTML)
// ---------------------------------------------------------------------------

function el(doc, tag, className, textContent) {
    const n = doc.createElement(tag);
    if (className) n.className = className;
    if (textContent !== undefined && textContent !== null) n.textContent = String(textContent);
    return n;
}

function clearNode(n) {
    if (!n) return;
    while (n.firstChild) n.removeChild(n.firstChild);
}

function errMsg(e) {
    return e && e.message ? e.message : String(e);
}

/** Момент последнего обновления индекса человеческим текстом. */
function formatUpdatedAt(ms) {
    const n = Number(ms) || 0;
    if (n <= 0) return 'индекс ещё не собирался';
    try {
        return `индекс обновлён ${new Date(n).toLocaleString('ru-RU')}`;
    } catch (_) {
        return 'индекс обновлён';
    }
}

/**
 * Одна плитка со значением и (если есть с чем сравнивать) дельтой.
 *
 * Дельты может не быть вовсе: предыдущий месяц пуст у нового проекта, и
 * «+∞%» вместо цифры — не информация, а мусор (см. `computeDelta`).
 */
function renderTile(doc, spec, kpi, prev) {
    const tile = el(doc, 'div', `analytics-tile${spec.hero ? ' analytics-tile-hero' : ''}`);
    tile.appendChild(el(doc, 'div', 'analytics-tile-label', spec.label));
    const raw = Number(kpi[spec.key]) || 0;
    tile.appendChild(el(doc, 'div', 'analytics-tile-value', spec.format(raw, kpi)));

    const delta = computeDelta(raw, Number(prev[spec.key]) || 0, spec.better);
    if (delta) {
        const cls = delta.tone === 'good' ? ' is-good' : delta.tone === 'bad' ? ' is-bad' : '';
        tile.appendChild(el(doc, 'div', `analytics-tile-delta${cls}`, `${delta.text} к прошлому месяцу`));
    }
    if (spec.note) tile.appendChild(el(doc, 'div', 'analytics-tile-note', spec.note));
    return tile;
}

// ---------------------------------------------------------------------------
// Вкладка
// ---------------------------------------------------------------------------

/**
 * Рисует вкладку «Аналитика» в переданный узел.
 *
 * @param {Element} host — контейнер панели (`#ps-analytics-content`)
 * @param {Object} [deps] — инъекция; в браузере приходит из `settings/modal.js`
 * @param {Function} deps.getAnalyticsSummary — `GET /api/analytics/summary`
 * @param {Function} deps.getAnalyticsStatus — `GET /api/analytics/status`
 * @param {Function} deps.rescanAnalytics — `POST /api/analytics/rescan`
 * @param {Function} [deps.onClose] — закрыть модалку (фолбэк, если вкладку
 *   открыть не удалось и дашборд показывается в этой же странице)
 * @param {Function} [deps.openWindow] — открыть новую вкладку браузера;
 *   по умолчанию `window.open`
 * @param {Function} [deps.showAnalytics] — фолбэк-показ вью в текущей странице,
 *   если браузер заблокировал новую вкладку; по умолчанию
 *   `window.ForgeApp.showAnalytics`
 * @param {Function} [deps.sparkline] — рисовальщик спарклайна (по умолчанию
 *   `lineChart` из кита)
 * @param {Document} [deps.document] — документ (по умолчанию глобальный)
 * @param {Function} [deps.setTimeout] / [deps.clearTimeout] — таймеры поллинга
 * @param {Date} [deps.now] — «сегодня» для границ периода (ради тестов)
 * @returns {Promise<Object>} ссылки на узлы и `reload()` — для тестов
 */
export function renderAnalyticsTab(host, deps = {}) {
    const doc = deps.document || (typeof document !== 'undefined' ? document : null);
    if (!host || !doc) throw new Error('analytics-tab: нет контейнера или документа');

    const setT = deps.setTimeout || ((fn, ms) => setTimeout(fn, ms));
    const clearT = deps.clearTimeout || ((id) => clearTimeout(id));
    const spark = typeof deps.sparkline === 'function' ? deps.sparkline : lineChart;

    clearNode(host);

    const agents = el(doc, 'fieldset', 'notify-fieldset analytics-agents-showcase');
    agents.appendChild(el(doc, 'legend', null, 'Агенты'));
    const agentCount = el(doc, 'div', 'notify-hint', 'Текущая работа агентов ещё не загружена.');
    const agentMeta = el(doc, 'div', 'notify-hint', 'Codex, Claude, OpenCode и GitHub Copilot.');
    agents.appendChild(agentCount);
    agents.appendChild(agentMeta);
    const agentActions = el(doc, 'div', 'notify-actions');
    const agentsOpenBtn = el(doc, 'button', 'primary', 'Открыть статистику агентов');
    agentsOpenBtn.type = 'button';
    agentActions.appendChild(agentsOpenBtn);
    agents.appendChild(agentActions);
    host.appendChild(agents);

    const fs = el(doc, 'fieldset', 'notify-fieldset');
    fs.appendChild(el(doc, 'legend', null, 'Работа с Claude'));
    fs.appendChild(el(
        doc,
        'div',
        'notify-hint',
        'Сводка за последние 30 дней по всем проектам: как часто вы отвечаете, '
        + 'сколько времени уходит на генерацию и во что это обходится. '
        + 'Полный дашборд с чартами, периодами и выбором проекта — по кнопке ниже.',
    ));

    // --- Прогресс первого скана (скрыт, пока индекс готов).
    const progress = el(doc, 'div', 'analytics-progress');
    progress.hidden = true;
    const progressLabel = el(doc, 'div', 'analytics-progress-label', 'Индексация транскриптов…');
    const track = el(doc, 'div', 'analytics-progress-track');
    const fill = el(doc, 'div', 'analytics-progress-fill');
    track.appendChild(fill);
    const progressHint = el(doc, 'div', 'analytics-progress-hint', '');
    progress.appendChild(progressLabel);
    progress.appendChild(track);
    progress.appendChild(progressHint);
    fs.appendChild(progress);

    // --- Плитки и спарклайн.
    const kpiBox = el(doc, 'div', 'analytics-kpi analytics-tab-kpi');
    fs.appendChild(kpiBox);

    const sparkBox = el(doc, 'div', 'analytics-tab-spark');
    sparkBox.hidden = true;
    fs.appendChild(sparkBox);

    const meta = el(doc, 'div', 'notify-hint analytics-tab-meta', '');
    fs.appendChild(meta);

    // --- Кнопки.
    const actions = el(doc, 'div', 'notify-actions analytics-tab-actions');
    const openBtn = el(doc, 'button', 'primary analytics-tab-open', 'Открыть в новой вкладке');
    openBtn.type = 'button';
    actions.appendChild(openBtn);

    const rescanBtn = el(doc, 'button', 'analytics-tab-rescan', 'Пересобрать индекс');
    rescanBtn.type = 'button';
    actions.appendChild(rescanBtn);

    const status = el(doc, 'span', 'rs-test-status analytics-tab-status', '');
    actions.appendChild(status);
    fs.appendChild(actions);

    host.appendChild(fs);

    // -----------------------------------------------------------------------
    // Состояние
    // -----------------------------------------------------------------------

    let pollId = null;
    let agentPollId = null;
    let agentLoading = false;
    let disposed = false;
    let lastSummary = null;

    function setStatus(text, cls) {
        status.textContent = text || '';
        status.className = 'rs-test-status analytics-tab-status' + (cls ? ' ' + cls : '');
    }

    function showProgress(p) {
        progress.hidden = false;
        kpiBox.hidden = true;
        sparkBox.hidden = true;
        const pct = progressPercent(p);
        if (fill.style) fill.style.width = `${pct}%`;
        const done = Number(p && p.done) || 0;
        const total = Number(p && p.total) || 0;
        progressLabel.textContent = 'Индексация транскриптов…';
        progressHint.textContent = total > 0
            ? `${done} из ${total} файлов (${pct}%)`
            : 'считаем, сколько файлов предстоит прочитать';
    }

    function hideProgress() {
        progress.hidden = true;
        kpiBox.hidden = false;
    }

    /** Рисует плитки, спарклайн и подпись под ними по ответу `summary`. */
    function paint(summary) {
        lastSummary = summary;
        hideProgress();

        const kpi = summary.kpi || {};
        const prev = summary.kpiPrevious || {};
        clearNode(kpiBox);

        if (isEmptySummary(summary)) {
            // Пустой месяц — штатная ситуация (человек не работал), и объяснить
            // её надо словами, а не шестью плитками с нулями.
            sparkBox.hidden = true;
            kpiBox.appendChild(el(
                doc,
                'div',
                'notify-hint analytics-tab-empty',
                'За последние 30 дней активности Claude Code не найдено.',
            ));
        } else {
            for (const spec of TAB_TILES) kpiBox.appendChild(renderTile(doc, spec, kpi, prev));
            paintSpark(summary);
        }

        const s = viewMode(summary);
        meta.textContent = s.refreshing
            ? 'индекс дочитывает свежие файлы — цифры сейчас обновятся'
            : formatUpdatedAt(summary.updatedAt);
    }

    /**
     * Спарклайн сообщений по дням.
     *
     * Компактный режим: без легенды и подписей оси X — тридцать дат в полосе
     * высотой 90px превратились бы в серую кашу; форма кривой здесь и есть всё
     * сообщение, точные числа человек смотрит в полноэкранном вью.
     */
    function paintSpark(summary) {
        const series = (summary && summary.series) || {};
        const values = Array.isArray(series.humanMessages) ? series.humanMessages : [];
        if (values.length === 0) {
            sparkBox.hidden = true;
            return;
        }
        sparkBox.hidden = false;
        clearNode(sparkBox);
        spark(sparkBox, {
            labels: series.buckets || [],
            series: [{ label: 'сообщения', values }],
        }, {
            width: 640,
            height: 90,
            legend: false,
            padding: { top: 6, right: 8, bottom: 6, left: 34 },
            formatX: () => '',
            formatValue: formatCount,
            formatY: formatCompact,
            yTicks: 2,
            title: 'Сообщения по дням за 30 дней',
            emptyText: 'сообщений за период нет',
            doc,
        });
    }

    function showError(message) {
        progress.hidden = true;
        kpiBox.hidden = false;
        clearNode(kpiBox);
        sparkBox.hidden = true;
        kpiBox.appendChild(el(
            doc,
            'div',
            'notify-error analytics-tab-error',
            `Не удалось получить аналитику: ${message}`,
        ));
    }

    // -----------------------------------------------------------------------
    // Сеть
    // -----------------------------------------------------------------------

    /**
     * Один запрос `summary` за последние 30 дней и отрисовка результата.
     *
     * Развилка «прогресс или данные» — общая с полноэкранным вью (`viewMode`):
     * повторить её здесь второй формулой значило бы получить вкладку и вью,
     * расходящиеся в трактовке одного и того же ответа.
     */
    async function load() {
        if (disposed) return null;
        const fn = deps.getAnalyticsSummary;
        if (typeof fn !== 'function') return null;
        let summary;
        try {
            summary = await fn(Object.assign(
                { bucket: 'day' },
                rangeToQuery(TAB_RANGE, deps.now instanceof Date ? deps.now : new Date()),
            ));
        } catch (e) {
            if (!disposed) showError(errMsg(e));
            return null;
        }
        if (disposed) return null;
        const mode = viewMode(summary || {});
        if (mode.mode === 'progress') {
            showProgress((summary && summary.progress) || {});
        } else {
            paint(summary || {});
        }
        if (mode.polling) startPolling();
        return summary;
    }

    /**
     * Опрос `status` до конца скана.
     *
     * Таймер живёт только пока вкладка на экране: `dispose()` его гасит, а
     * модалка удаляется целиком, поэтому висящих запросов после закрытия не
     * остаётся.
     */
    function startPolling() {
        if (pollId !== null || disposed) return;
        const fn = deps.getAnalyticsStatus;
        if (typeof fn !== 'function') return;
        pollId = setT(async () => {
            pollId = null;
            if (disposed) return;
            let st;
            try {
                st = await fn();
            } catch (_) {
                // Разовая сетевая ошибка во время скана — не повод гасить
                // вкладку: следующий тик попробует снова.
                startPolling();
                return;
            }
            if (disposed) return;
            if (st && st.state === 'scanning') {
                const shown = viewMode(lastSummary || {});
                if (shown.mode === 'progress') showProgress(st);
                else meta.textContent = 'индекс дочитывает свежие файлы — цифры сейчас обновятся';
                startPolling();
                return;
            }
            rescanBtn.disabled = false;
            setStatus('', '');
            await load();
        }, POLL_MS);
    }

    function stopPolling() {
        if (pollId !== null) {
            clearT(pollId);
            pollId = null;
        }
    }

    async function loadAgents() {
        if (disposed || doc.hidden || agentLoading || host.isConnected === false || typeof deps.getAgentLive !== 'function') return;
        agentLoading = true;
        try {
            const live = await deps.getAgentLive();
            if (disposed || doc.hidden) return;
            const n = live && live.workingCount;
            agentCount.textContent = typeof n === 'number' && Number.isFinite(n)
                ? `Сейчас работают: ${formatCount(n)}`
                : 'Нет данных о текущей работе агентов.';
            agentMeta.textContent = 'В полном режиме доступны время задач, история, токены, модели и проекты.';
        } catch (_) {
            if (!disposed && !doc.hidden) agentMeta.textContent = 'Не удалось обновить текущую работу. Можно открыть полную статистику.';
        } finally {
            agentLoading = false;
            if (!disposed && !doc.hidden) agentPollId = setT(() => { agentPollId = null; loadAgents(); }, 2000);
        }
    }

    function agentsVisibilityChanged() {
        if (doc.hidden) {
            if (agentPollId !== null) clearT(agentPollId);
            agentPollId = null;
        } else if (!disposed && agentPollId === null) {
            loadAgents();
        }
    }
    doc.addEventListener?.('visibilitychange', agentsVisibilityChanged);

    agentsOpenBtn.addEventListener('click', () => {
        if (typeof deps.onClose === 'function') deps.onClose();
        const show = typeof deps.showAnalytics === 'function'
            ? deps.showAnalytics
            : typeof window !== 'undefined' && window.ForgeApp && window.ForgeApp.showAnalytics;
        if (typeof show === 'function') show(undefined, 'agents');
    });

    // -----------------------------------------------------------------------
    // Кнопки
    // -----------------------------------------------------------------------

    // «Открыть в новой вкладке»: дашборд — длинная страница, которую смотрят
    // рядом с работой, а не вместо неё. Поэтому новая вкладка браузера, а не
    // подмена текущего экрана: терминал с идущей сессией остаётся на месте.
    //
    // Модалку при удачном открытии НЕ закрываем — пользователь ничего не терял
    // на этой странице, а закрытие выглядело бы так, будто настройки «слетели».
    //
    // Блокировщик всплывающих окон возвращает `null`. Это не редкость, поэтому
    // есть фолбэк на прежнее поведение (закрыть модалку и показать вью здесь) —
    // иначе кнопка молча не делала бы ничего.
    //
    // БЕЗ `noopener`: с ним `window.open` по спецификации возвращает `null`
    // ВСЕГДА, и отличить успех от блокировки становится нечем — фолбэк срабатывал
    // поверх успешно открытой вкладки, подменяя заодно и текущий экран. Вкладка
    // нашего же origin, скрывать от неё `opener` не от кого.
    openBtn.addEventListener('click', () => {
        const open = typeof deps.openWindow === 'function'
            ? deps.openWindow
            : (typeof window !== 'undefined' && typeof window.open === 'function'
                ? (url, target) => window.open(url, target)
                : null);
        if (open) {
            let win = null;
            try {
                win = open(`${ANALYTICS_URL}&analyticsMode=claude`, '_blank');
            } catch (e) {
                win = null;
            }
            if (win) return;
        }
        if (typeof deps.onClose === 'function') deps.onClose();
        if (typeof deps.showAnalytics === 'function') {
            deps.showAnalytics(undefined, 'claude');
            return;
        }
        const app = typeof window !== 'undefined' ? window.ForgeApp : null;
        if (app && typeof app.showAnalytics === 'function') app.showAnalytics(undefined, 'claude');
    });

    // «Пересобрать индекс»: сервер отвечает СРАЗУ статусом `scanning`, сам
    // пересбор идёт в фоне — поэтому дальше поллинг, а не ожидание ответа.
    rescanBtn.addEventListener('click', async () => {
        const fn = deps.rescanAnalytics;
        if (typeof fn !== 'function') return;
        rescanBtn.disabled = true;
        setStatus('Пересбор запущен…', 'pending');
        try {
            const st = await fn();
            if (disposed) return;
            showProgress(st || {});
            startPolling();
        } catch (e) {
            if (disposed) return;
            rescanBtn.disabled = false;
            setStatus('Ошибка: ' + errMsg(e), 'error');
        }
    });

    const refs = {
        host,
        fieldset: fs,
        kpi: kpiBox,
        spark: sparkBox,
        progress,
        progressFill: fill,
        progressHint,
        meta,
        openBtn,
        rescanBtn,
        status,
        agentCount,
        agentMeta,
        agentsOpenBtn,
        reload: load,
        /** Снять таймеры — модалка закрылась. */
        dispose() {
            disposed = true;
            stopPolling();
            if (agentPollId !== null) clearT(agentPollId);
            agentPollId = null;
            doc.removeEventListener?.('visibilitychange', agentsVisibilityChanged);
        },
    };

    // Первый запрос уходит сразу, но узлы уже на экране: вкладка не должна
    // ждать сети, чтобы показать кнопки.
    refs.ready = load();
    refs.agentsReady = loadAgents();
    return refs;
}
