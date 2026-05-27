// tmux-web — Daily summary (Сводка дня)
//
// Отдельная вью #daily-summary в #main: показывает markdown-сводку за день
// с навигацией по датам (← Сегодня →) и кнопкой пересоздания. Сводки
// приходят с бэкенда (Echo plugin, Phase 1-3):
//   GET  /api/echo/daily-reports?limit=N → {items:[{id,day,content,...}]}
//   GET  /api/echo/daily-reports/:day    → {id,day,content,...} | 404
//   POST /api/echo/daily-reports/generate {day?} → {id,day,content}
//
// Если за выбранный день сводки ещё нет (404) — показываем пустое состояние
// с кнопкой «Сгенерировать». Markdown рендерится через renderMarkdownInto из
// core/markdown.js (тот же рендер, что и в чате Echo).
//
// Видимость переключает showDailySummary(); по образцу home.js::showHome
// она скрывает placeholder и #home, чтобы вью не накладывались.
//
// XSS-безопасность: markdown строится через document.createElement/textContent
// (см. core/markdown.js), пользовательский контент не уходит в innerHTML.
//
// Зависимости:
//   - getDailyReport/generateDailyReport (echo/api.js) — REST-клиент.
//   - renderMarkdownInto (core/markdown.js) — рендер markdown в DOM.
//   - showPlaceholder (terminal/xterm.js) — спрятать placeholder при показе.
//   - DOM-ссылки $dailySummary* (core/dom.js).

import { getDailyReport, generateDailyReport } from '../echo/api.js';
import { renderMarkdownInto } from '../core/markdown.js';
import { showPlaceholder } from '../terminal/xterm.js';
import { fetchSessions } from '../sessions/sessions.js';
import { createTodoForPath } from '../tasks/crud.js';
import { $home } from '../core/dom.js';
import {
    $dailySummary, $dailySummaryBack, $dailySummaryPrev, $dailySummaryToday,
    $dailySummaryNext, $dailySummaryDay, $dailySummaryRegen, $dailySummaryStatus,
    $dailySummaryContent, $dailySummarySuggestions, $dailySummaryEmpty,
    $dailySummaryGenerate,
} from '../core/dom.js';

// Текущий выбранный день (YYYY-MM-DD). null до первого showDailySummary.
let _currentDay = null;
// Защита от повторной навески listener'ов на кнопки.
let _bound = false;
// Флаг «идёт сетевой запрос» — блокирует кнопки и параллельные загрузки.
let _busy = false;

/** Возвращает сегодняшнюю дату в формате YYYY-MM-DD (локальная зона). */
function todayStr() {
    const d = new Date();
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    return `${y}-${m}-${day}`;
}

/** Сдвигает дату YYYY-MM-DD на `delta` дней, возвращает YYYY-MM-DD. */
function shiftDay(dayStr, delta) {
    const [y, m, d] = dayStr.split('-').map(Number);
    const dt = new Date(y, m - 1, d);
    dt.setDate(dt.getDate() + delta);
    const yy = dt.getFullYear();
    const mm = String(dt.getMonth() + 1).padStart(2, '0');
    const dd = String(dt.getDate()).padStart(2, '0');
    return `${yy}-${mm}-${dd}`;
}

/** Показывает статус-строку (например «Генерация…»), либо скрывает её. */
function setStatus(text) {
    if (!$dailySummaryStatus) return;
    if (text) {
        $dailySummaryStatus.textContent = text;
        $dailySummaryStatus.hidden = false;
    } else {
        $dailySummaryStatus.textContent = '';
        $dailySummaryStatus.hidden = true;
    }
}

/** Блокирует/разблокирует управляющие кнопки на время сетевого запроса. */
function setBusy(busy) {
    _busy = busy;
    if ($dailySummaryRegen) $dailySummaryRegen.disabled = busy;
    if ($dailySummaryGenerate) $dailySummaryGenerate.disabled = busy;
    if ($dailySummaryPrev) $dailySummaryPrev.disabled = busy;
    if ($dailySummaryNext) $dailySummaryNext.disabled = busy;
    if ($dailySummaryToday) $dailySummaryToday.disabled = busy;
}

/**
 * Рендерит markdown-контент сводки в #daily-summary-content. Минималистичный
 * Notion-стиль: чистая типографика без карточек/чипов — оформление целиком
 * в css/daily-summary.css (классы .echo-md-*).
 */
function renderContent(content) {
    if (!$dailySummaryContent) return;
    $dailySummaryContent.innerHTML = '';
    renderMarkdownInto($dailySummaryContent, content || '');
    $dailySummaryContent.hidden = false;
    if ($dailySummaryEmpty) $dailySummaryEmpty.hidden = true;
}

/** Прячет и очищает блок «Предлагаемые задачи». */
function hideSuggestions() {
    if (!$dailySummarySuggestions) return;
    $dailySummarySuggestions.innerHTML = '';
    $dailySummarySuggestions.hidden = true;
}

/**
 * Рендерит блок «Предлагаемые задачи по проектам» под markdown-сводкой.
 *
 * @param {Array<{project_path:string, project_name:string,
 *   tasks:Array<{title:string, description?:string, priority?:number}>}>} suggestions
 *
 * Каждый проект — отдельная группа: подзаголовок (project_name), карточки задач
 * (визуально как TODO-карточки `.kanban-card`) и кнопка «Добавить выбранные в
 * TODO». Клик по карточке тоглит выбор (класс `selected`); кнопка создаёт TODO
 * через createTodoForPath(project_path, ...) для каждой выбранной задачи.
 * Успешные карточки помечаются `added` и становятся некликабельными.
 *
 * XSS-безопасность: весь пользовательский контент уходит через textContent,
 * innerHTML используется только для очистки контейнера ('').
 */
function renderSuggestions(suggestions) {
    if (!$dailySummarySuggestions) return;
    if (!Array.isArray(suggestions) || suggestions.length === 0) {
        hideSuggestions();
        return;
    }
    $dailySummarySuggestions.innerHTML = '';
    $dailySummarySuggestions.hidden = false;

    const heading = document.createElement('div');
    heading.className = 'daily-summary-suggestions-title';
    heading.textContent = 'Предлагаемые задачи';
    $dailySummarySuggestions.appendChild(heading);

    const hint = document.createElement('div');
    hint.className = 'daily-summary-suggestions-hint';
    hint.textContent = 'Выберите карточки и добавьте их в TODO проекта.';
    $dailySummarySuggestions.appendChild(hint);

    for (const group of suggestions) {
        if (!group || !Array.isArray(group.tasks) || group.tasks.length === 0) continue;

        const groupEl = document.createElement('div');
        groupEl.className = 'daily-summary-suggestions-group';

        const projTitle = document.createElement('div');
        projTitle.className = 'daily-summary-suggestions-project';
        projTitle.textContent = group.project_name || group.project_path || 'Проект';
        groupEl.appendChild(projTitle);

        const cardsEl = document.createElement('div');
        cardsEl.className = 'daily-summary-suggestions-cards';
        groupEl.appendChild(cardsEl);

        const addBtn = document.createElement('button');
        addBtn.type = 'button';
        addBtn.className = 'daily-summary-generate daily-summary-suggestions-add';
        addBtn.textContent = 'Добавить выбранные в TODO';
        addBtn.disabled = true;

        // Выбор задач в этой группе: Set индексов задач.
        const selected = new Set();
        // Карты index → {card, task} для тех, что ещё не добавлены.
        const cardByIndex = new Map();

        const refreshBtn = () => { addBtn.disabled = selected.size === 0; };

        group.tasks.forEach((task, idx) => {
            if (!task) return;
            const prio = (typeof task.priority === 'number') ? task.priority : 3;

            // data-status="todo" → фирменный TODO-вид (фиолетовый бордер,
            // стиль .desc/.p-pill из tasks.css) — карточка читается как TODO.
            const card = document.createElement('div');
            card.className = 'kanban-card suggestion-card';
            card.setAttribute('data-priority', String(prio));
            card.setAttribute('data-status', 'todo');
            card.setAttribute('role', 'button');
            card.setAttribute('aria-pressed', 'false');

            // Явный чекбокс-индикатор слева — однозначная аффорданс выбора.
            const checkEl = document.createElement('span');
            checkEl.className = 'suggestion-check';
            checkEl.setAttribute('aria-hidden', 'true');

            const bodyEl = document.createElement('div');
            bodyEl.className = 'suggestion-body';

            const titleEl = document.createElement('div');
            titleEl.className = 'title';
            titleEl.textContent = task.title || '(без названия)';
            bodyEl.appendChild(titleEl);

            if (task.description) {
                const descEl = document.createElement('div');
                // Без обрезки: пользователю нужно прочитать всё описание задачи,
                // чтобы решить, добавлять ли её. Перенос — в CSS (.suggestion-card .desc).
                descEl.className = 'desc';
                descEl.textContent = String(task.description);
                bodyEl.appendChild(descEl);
            }

            const metaRow = document.createElement('div');
            metaRow.className = 'meta-row';
            const pPill = document.createElement('span');
            pPill.className = 'p-pill';
            pPill.textContent = 'P' + prio;
            metaRow.appendChild(pPill);
            const typeTag = document.createElement('span');
            typeTag.className = 'type-tag';
            typeTag.textContent = 'todo';
            metaRow.appendChild(typeTag);
            bodyEl.appendChild(metaRow);

            card.appendChild(checkEl);
            card.appendChild(bodyEl);

            cardByIndex.set(idx, { card, task });

            card.addEventListener('click', () => {
                if (card.classList.contains('added')) return;
                if (selected.has(idx)) {
                    selected.delete(idx);
                    card.classList.remove('selected');
                    card.setAttribute('aria-pressed', 'false');
                } else {
                    selected.add(idx);
                    card.classList.add('selected');
                    card.setAttribute('aria-pressed', 'true');
                }
                refreshBtn();
            });

            cardsEl.appendChild(card);
        });

        addBtn.addEventListener('click', async () => {
            if (selected.size === 0 || addBtn.disabled) return;
            addBtn.disabled = true;
            const indices = Array.from(selected);
            const errors = [];

            for (const idx of indices) {
                const entry = cardByIndex.get(idx);
                if (!entry) continue;
                try {
                    await createTodoForPath(
                        group.project_path,
                        entry.task.title || '',
                        entry.task.description || '',
                    );
                    selected.delete(idx);
                    entry.card.classList.remove('selected');
                    entry.card.classList.add('added');
                    entry.card.setAttribute('aria-pressed', 'false');
                    const mark = document.createElement('span');
                    mark.className = 'daily-summary-suggestions-added-mark';
                    mark.textContent = '✓ добавлено';
                    (entry.card.querySelector('.suggestion-body') || entry.card)
                        .appendChild(mark);
                    cardByIndex.delete(idx);
                } catch (e) {
                    errors.push((entry.task.title || '(задача)') + ': '
                        + (e && e.message ? e.message : 'неизвестно'));
                }
            }

            refreshBtn();
            if (errors.length > 0) {
                window.alert('Не удалось добавить:\n' + errors.join('\n'));
            }
        });

        groupEl.appendChild(addBtn);
        $dailySummarySuggestions.appendChild(groupEl);
    }
}

/** Показывает пустое состояние (нет сводки → кнопка «Сгенерировать»). */
function showEmpty() {
    if ($dailySummaryContent) {
        $dailySummaryContent.innerHTML = '';
        $dailySummaryContent.hidden = true;
    }
    hideSuggestions();
    if ($dailySummaryEmpty) $dailySummaryEmpty.hidden = false;
}

/** Человекочитаемая дата: «27 мая 2026, вторник» (локаль ru-RU). */
function humanDate(dayStr) {
    const [y, m, d] = dayStr.split('-').map(Number);
    const dt = new Date(y, m - 1, d);
    try {
        const s = dt.toLocaleDateString('ru-RU', {
            day: 'numeric', month: 'long', year: 'numeric', weekday: 'long',
        });
        // toLocaleDateString отдаёт «вторник, 27 мая 2026 г.» — переставим
        // день недели в конец для более «заголовочного» вида.
        const m2 = s.match(/^([^,]+),\s*(.+)$/);
        return m2 ? `${m2[2]}, ${m2[1]}` : s;
    } catch {
        return dayStr;
    }
}

/** Обновляет надпись с текущей датой в шапке. */
function renderDayLabel() {
    if (!$dailySummaryDay) return;
    const today = todayStr();
    const human = _currentDay ? humanDate(_currentDay) : '';
    if (_currentDay === today) {
        $dailySummaryDay.textContent = `${human} · сегодня`;
    } else {
        $dailySummaryDay.textContent = human;
    }
    // Запрет навигации в будущее: следующий день недоступен, если уже сегодня.
    if ($dailySummaryNext) $dailySummaryNext.disabled = _currentDay >= today;
}

/**
 * Загружает сводку за `_currentDay` и рендерит. 404 → пустое состояние.
 * Прочие ошибки показываются в статус-строке.
 */
async function loadCurrent() {
    if (!_currentDay) return;
    renderDayLabel();
    setStatus('Загрузка…');
    try {
        const report = await getDailyReport(_currentDay);
        setStatus('');
        renderContent(report && report.content);
        renderSuggestions(report && report.suggestions);
    } catch (e) {
        if (e && e.status === 404) {
            setStatus('');
            showEmpty();
        } else {
            setStatus('Ошибка загрузки: ' + (e && e.message ? e.message : 'неизвестно'));
            showEmpty();
        }
    } finally {
        renderDayLabel();
    }
}

/**
 * Генерирует (или пересоздаёт) сводку за `_currentDay`, затем рендерит её.
 * На время запроса блокирует кнопки и показывает статус.
 */
async function generateCurrent() {
    if (!_currentDay || _busy) return;
    setBusy(true);
    setStatus('Генерация сводки…');
    try {
        const report = await generateDailyReport(_currentDay);
        setStatus('');
        renderContent(report && report.content);
        renderSuggestions(report && report.suggestions);
    } catch (e) {
        setStatus('Не удалось сгенерировать: ' + (e && e.message ? e.message : 'неизвестно'));
    } finally {
        setBusy(false);
        renderDayLabel();
    }
}

/** Навешивает обработчики на кнопки навигации/генерации (один раз). */
function bindControls() {
    if (_bound) return;
    _bound = true;

    if ($dailySummaryPrev) {
        $dailySummaryPrev.addEventListener('click', () => {
            if (_busy || !_currentDay) return;
            _currentDay = shiftDay(_currentDay, -1);
            loadCurrent();
        });
    }
    if ($dailySummaryNext) {
        $dailySummaryNext.addEventListener('click', () => {
            if (_busy || !_currentDay) return;
            if (_currentDay >= todayStr()) return;
            _currentDay = shiftDay(_currentDay, 1);
            loadCurrent();
        });
    }
    if ($dailySummaryToday) {
        $dailySummaryToday.addEventListener('click', () => {
            if (_busy) return;
            _currentDay = todayStr();
            loadCurrent();
        });
    }
    if ($dailySummaryRegen) {
        $dailySummaryRegen.addEventListener('click', () => generateCurrent());
    }
    if ($dailySummaryGenerate) {
        $dailySummaryGenerate.addEventListener('click', () => generateCurrent());
    }
    if ($dailySummaryBack) {
        $dailySummaryBack.addEventListener('click', () => closeDailySummary());
    }
}

/**
 * Закрывает вью «Сводка дня» и возвращает к основному экрану. Скрывает
 * #daily-summary и вызывает fetchSessions(), который сам решает, показать
 * #home (нет активных сессий) или активную сессию/placeholder.
 */
function closeDailySummary() {
    hideSuggestions();
    hideDailySummary();
    fetchSessions();
}

/**
 * Показывает вью «Сводка дня» и загружает отчёт за `day` (по умолчанию —
 * сегодня). Скрывает placeholder и #home, чтобы вью не накладывались
 * (по образцу home.js::showHome).
 *
 * @param {string} [day] — дата YYYY-MM-DD; по умолчанию сегодня
 */
export function showDailySummary(day) {
    if (!$dailySummary) return;
    bindControls();
    _currentDay = day || todayStr();
    $dailySummary.style.display = 'flex';
    if ($home) $home.style.display = 'none';
    showPlaceholder(false);
    loadCurrent();
}

/** Скрывает вью «Сводка дня». */
export function hideDailySummary() {
    if (!$dailySummary) return;
    $dailySummary.style.display = 'none';
}
