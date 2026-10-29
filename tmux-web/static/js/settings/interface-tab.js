// tmux-web — Settings modal: вкладка «Интерфейс».
//
// Экспортирует buildInterfaceForm(settings, onSaved) — фабрика DOM-узла
// (fieldset) с тумблерами двух opt-in фич интерфейса. Использует те же классы,
// что notifications-tab.js / todo-tab.js (.notify-fieldset, .notify-hint,
// .notify-error, .notify-actions, .modal-check) — править CSS не требуется.
//
// Поля настроек (мэппинг 1:1 на backend struct в src/user_settings.rs):
//   cmd_hints_enabled        — bool (checkbox), default false
//   next_step_enabled        — bool (checkbox), default false
//   auto_approve_enabled     — bool (checkbox), default false
//   auto_approve_broad       — bool (checkbox), default false
//   auto_approve_questions   — bool (checkbox), default TRUE
//   auto_approve_blocklist   — string[] (textarea, по строке), default []
//   claude_llm_enabled       — bool (checkbox), default false
//   claude_llm_backend       — string (select claude|kimi), default "claude";
//                              облако — env-трио FORGE_LLM_CLOUD_* (приоритет)
//
// ВАЖНО про дефолты: все фичи-тумблеры выключены при нулевой конфигурации,
// поэтому их чекбоксы инициализируются строго `=== true`. Идиома `!== false`
// (как в echo/settings.js для echo_notifications_enabled) здесь НЕПРИМЕНИМА —
// у той настройки дефолт включённый, и копирование инвертировало бы наш.
// Исключение — auto_approve_questions: у него дефолт `true` (он живёт внутри
// уже opt-in авто-апрува и нужен для ВЫключения), поэтому именно там `!== false`.
//
// Save шлёт только свои поля: PATCH применяет лишь Some(..)-варианты, так
// что остальные настройки не затрагиваются (см. todo-tab.js).

import { updateUserSettings } from './user-settings-api.js';

// Рисует журнал авто-апрувов в контейнер: время, сессия, окно, что нажато и
// сама команда. Записи приходят с сервера уже отсортированными (новые первыми).
// Пустой массив — не ошибка: это «пока ничего не подтверждалось».
function renderHistory(host, records) {
    host.textContent = '';
    if (!Array.isArray(records) || records.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'notify-hint';
        empty.textContent =
            'Пока пусто. Здесь появятся запросы, подтверждённые автоматически; ' +
            'журнал очищается при перезапуске сервера.';
        host.appendChild(empty);
        return;
    }

    const table = document.createElement('table');
    table.className = 'auto-approve-history-table';

    const head = document.createElement('tr');
    ['Время', 'Сессия', 'Окно', 'Выбор', 'Команда'].forEach((title) => {
        const th = document.createElement('th');
        th.textContent = title;
        head.appendChild(th);
    });
    table.appendChild(head);

    records.forEach((rec) => {
        const tr = document.createElement('tr');
        // Окно показываем ИМЕНЕМ (window_name из tmux) — по нему человек
        // узнаёт своё окно. Индекс win.pane остаётся точным адресом нажатия и
        // уходит в tooltip; если имени нет (старый tmux не отдал), показываем
        // индекс, а при пустом индексе — «активное» (Claude не опознан по
        // имени процесса, смотрели активную панель сессии).
        const windowCell = rec.window || rec.pane || 'активное';
        const cells = [
            rec.ts ? new Date(rec.ts * 1000).toLocaleString() : '',
            rec.session || '',
            windowCell,
            rec.choice ? (rec.choice + (rec.attempt > 1 ? ' + Enter' : '')) : '',
            rec.subject || '',
        ];
        cells.forEach((value, i) => {
            const td = document.createElement('td');
            td.textContent = value;
            if (i === 2 && rec.pane) td.title = 'панель ' + rec.pane;
            if (i === 4) td.title = value; // длинная команда — полностью в tooltip
            tr.appendChild(td);
        });
        table.appendChild(tr);
    });

    host.appendChild(table);
}

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

    // 3) checkbox: auto_approve_enabled + зависимые от него настройки.
    // Слева от чекбоксов — кнопка «История»: журнал того, что воркер уже
    // подтвердил (сессия, окно, команда). Раскрывается под группой.
    const apRow = document.createElement('div');
    apRow.className = 'auto-approve-row';

    const histBtn = document.createElement('button');
    histBtn.type = 'button';
    histBtn.className = 'auto-approve-history-btn';
    histBtn.textContent = 'История';
    histBtn.title =
        'Что авто-апрув уже подтвердил: время, сессия, окно и сама команда. ' +
        'Журнал живёт в памяти сервера и очищается при его перезапуске.';
    apRow.appendChild(histBtn);

    const apWrap = document.createElement('label');
    apWrap.className = 'modal-check notify-check';
    const ap = document.createElement('input');
    ap.type = 'checkbox';
    ap.className = 'auto-approve-enabled';
    ap.checked = s.auto_approve_enabled === true;
    ap.title =
        'Фоновый воркер раз в 2 секунды осматривает панели сессий и сам ' +
        'подтверждает запросы Claude Code (разрешение на команду, Claude in ' +
        'Chrome, вопросы). Утверждение плана и команды из стоп-листа не ' +
        'подтверждаются никогда.';
    apWrap.appendChild(ap);
    apWrap.appendChild(document.createTextNode(' Авто-апрув запросов Claude Code'));
    apRow.appendChild(apWrap);
    fs.appendChild(apRow);

    // Панель истории — скрыта, пока не нажали кнопку. Данные грузятся при
    // каждом открытии: журнал пополняется воркером в фоне, кэшировать нечего.
    const histPanel = document.createElement('div');
    histPanel.className = 'auto-approve-history';
    histPanel.style.display = 'none';
    fs.appendChild(histPanel);

    histBtn.addEventListener('click', async () => {
        if (histPanel.style.display !== 'none') {
            histPanel.style.display = 'none';
            return;
        }
        histPanel.style.display = '';
        histPanel.textContent = 'Загрузка…';
        try {
            const r = await fetch('/api/auto-approve/history', {
                headers: { 'Accept': 'application/json' },
            });
            if (!r.ok) throw new Error('HTTP ' + r.status);
            renderHistory(histPanel, await r.json());
        } catch (e) {
            histPanel.textContent = 'Не удалось загрузить историю: ' +
                ((e && e.message) ? e.message : 'ошибка сети');
        }
    });

    const apHint = document.createElement('div');
    apHint.className = 'notify-hint';
    apHint.textContent =
        'Сессия не ждёт человека на «Do you want to proceed?». План ' +
        '(ExitPlanMode) и команды со стоп-словами всегда остаются вам — ' +
        'карточка сессии продолжает светиться.';
    fs.appendChild(apHint);

    const broadWrap = document.createElement('label');
    broadWrap.className = 'modal-check notify-check';
    const broad = document.createElement('input');
    broad.type = 'checkbox';
    broad.className = 'auto-approve-broad';
    broad.checked = s.auto_approve_broad === true;
    broad.title =
        'Выбирать «Yes, and don\'t ask again» / «Allow all actions for this ' +
        'session», когда такой пункт есть. Там, где второй пункт — отказ ' +
        '(Yes/No, Allow/Deny), всегда жмётся первый.';
    broadWrap.appendChild(broad);
    broadWrap.appendChild(document.createTextNode(' Расширенно: «не спрашивать снова»'));
    fs.appendChild(broadWrap);

    const qWrap = document.createElement('label');
    qWrap.className = 'modal-check notify-check';
    const questions = document.createElement('input');
    questions.type = 'checkbox';
    questions.className = 'auto-approve-questions';
    // Дефолт этой настройки — включено (см. шапку файла).
    questions.checked = s.auto_approve_questions !== false;
    questions.title =
        'Отвечать первым вариантом на вопросы AskUserQuestion. Выключите, ' +
        'если хотите выбирать вариант сами, а подтверждения команд оставить ' +
        'автоматическими.';
    qWrap.appendChild(questions);
    qWrap.appendChild(document.createTextNode(' Отвечать на вопросы (первый вариант)'));
    fs.appendChild(qWrap);

    const blockWrap = document.createElement('label');
    blockWrap.className = 'notify-field';
    blockWrap.textContent = 'Стоп-слова авто-апрува (по одному в строке)';
    const blocklist = document.createElement('textarea');
    blocklist.className = 'auto-approve-blocklist';
    blocklist.rows = 3;
    blocklist.spellcheck = false;
    blocklist.placeholder = 'release.sh\nterraform apply';
    blocklist.value = Array.isArray(s.auto_approve_blocklist)
        ? s.auto_approve_blocklist.join('\n')
        : '';
    blocklist.title =
        'Если подстрока встречается в панели с запросом — авто-апрув его ' +
        'пропускает. Дополняет встроенный список (rm -rf, git push --force, ' +
        'drop table и т.п.), а не заменяет его.';
    blockWrap.appendChild(blocklist);
    fs.appendChild(blockWrap);

    // LLM-эскалация блоклиста — единственное место tmux-web, которое само
    // спавнит `claude -p`. Запуски Claude CLI в проекте по умолчанию
    // выключены глобально: без этого тумблера (или переменной
    // FORGE_CLAUDE_RUNNER_ENABLED=1) блоклист-совпадения остаются человеку.
    const llmWrap = document.createElement('label');
    llmWrap.className = 'modal-check notify-check';
    const llm = document.createElement('input');
    llm.type = 'checkbox';
    llm.className = 'claude-llm-enabled';
    // Дефолт — выключено (см. шапку файла): инициализация строго `=== true`.
    llm.checked = s.claude_llm_enabled === true;
    llm.title =
        'Разрешить авто-апруву спрашивать Claude CLI (claude -p), можно ли ' +
        'подтвердить команду из стоп-листа. Запуски Claude CLI по умолчанию ' +
        'выключены и расходуют токены; без включения совпадения по блоклисту ' +
        'всегда остаются вам. Для всех запусков разом есть переменная ' +
        'FORGE_CLAUDE_RUNNER_ENABLED=1.';
    llmWrap.appendChild(llm);
    llmWrap.appendChild(
        document.createTextNode(' LLM-эскалация блоклиста (claude -p / kimi -p)'),
    );
    fs.appendChild(llmWrap);

    // Движок вердикта: claude (дефолт) или kimi. Облако выбирается не здесь,
    // а env-трио FORGE_LLM_CLOUD_* — заданный ключ облака побеждает этот
    // выбор (см. VerdictBackend::resolve на бэке).
    const llmBackendRow = document.createElement('div');
    llmBackendRow.className = 'notify-field';
    const llmBackendLabel = document.createElement('label');
    llmBackendLabel.textContent = 'Движок вердикта';
    const llmBackend = document.createElement('select');
    llmBackend.className = 'claude-llm-backend';
    for (const [value, text] of [
        ['claude', 'Claude CLI (claude -p)'],
        ['kimi', 'Kimi CLI (kimi --print)'],
    ]) {
        const opt = document.createElement('option');
        opt.value = value;
        opt.textContent = text;
        llmBackend.appendChild(opt);
    }
    llmBackend.value = s.claude_llm_backend === 'kimi' ? 'kimi' : 'claude';
    llmBackend.title =
        'Какой локальный CLI спрашивает вердикт для команд из стоп-листа. ' +
        'Облачный провайдер включается переменными FORGE_LLM_CLOUD_BASE_URL, ' +
        'FORGE_LLM_CLOUD_API_KEY и FORGE_LLM_CLOUD_MODEL — заданный ключ ' +
        'облака имеет приоритет над этим выбором.';
    llmBackendRow.appendChild(llmBackendLabel);
    llmBackendRow.appendChild(llmBackend);
    fs.appendChild(llmBackendRow);

    // Зависимые контролы неактивны, пока выключен главный тумблер: они не
    // имеют самостоятельного смысла и включённый вид вводил бы в заблуждение.
    const syncAutoApprove = () => {
        const on = !!ap.checked;
        broad.disabled = !on;
        questions.disabled = !on;
        blocklist.disabled = !on;
        llm.disabled = !on;
        broadWrap.style.opacity = on ? '' : '0.5';
        qWrap.style.opacity = on ? '' : '0.5';
        blockWrap.style.opacity = on ? '' : '0.5';
        llmWrap.style.opacity = on ? '' : '0.5';
    };
    ap.addEventListener('change', syncAutoApprove);
    syncAutoApprove();

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
            auto_approve_enabled: !!ap.checked,
            auto_approve_broad: !!broad.checked,
            auto_approve_questions: !!questions.checked,
            claude_llm_enabled: !!llm.checked,
            claude_llm_backend: llmBackend.value,
            // Полная замена списка (backend не мержит) — пустые строки
            // отбрасываем здесь, санитайзер на бэке делает то же самое.
            auto_approve_blocklist: blocklist.value
                .split('\n')
                .map((line) => line.trim())
                .filter((line) => line.length > 0),
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
