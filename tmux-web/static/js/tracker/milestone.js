// tmux-web — плагин forge-tracker: диалог вехи Гантта (`MILESTONE ▸ gantt`).
//
// ЗАЧЕМ ОН ЕСТЬ. Раньше веха заводилась и правилась двумя `window.prompt`
// подряд: подпись, потом дата строкой `ГГГГ-ММ-ДД`. Это плохо трижды —
// дату приходилось НАБИРАТЬ (единственное место панели, где не было пикера),
// удаление пряталось за «сотри подпись», а браузерная модалка вдобавок
// намертво блокирует автоматизацию (её не видит ни CDP, ни e2e-гейт).
//
// ФОРМА ТА ЖЕ, ЧТО У ДИАЛОГА ПЕРЕНОСА (`PUSH ▸ tasks/todo`, to-todo.js):
// затемнение, карточка в терминальной рамке, титульная строка, тело и подвал
// с `CANCEL` · `OK`. Разница — в подвале слева стоит `DELETE`, и он виден
// только при ПРАВКЕ существующей вехи: у вехи два поля, отдельного
// подтверждения удаление не требует, но и случайным оно быть не должно —
// поэтому это явная кнопка, а не пустая строка.
//
// ПОЧЕМУ DOM ПРИХОДИТ СНАРУЖИ. Ровно по той же причине, что и в to-todo.js:
// `../core/dom.js` дёргает `document.getElementById` на уровне модуля, и
// импорт сделал бы файл неимпортируемым в голом node. Узлы и сервисы
// (`create`, `patch`, `remove`, `note`) приходят один раз в
// `initTrackerMilestone(deps)`, а чистая часть — `milestonePatch` — покрыта
// node-тестом.
//
// СНИМКА У ВЕХ НЕТ. Сервер на любую правку вехи шлёт кадр `reload`, панель
// перечитывает состояние, и оно тянет за собой `refreshGantt` — поэтому
// диалог не обновляет ничего сам: закрылся, и `◆` переехал во всех открытых
// окнах.

/** Голая дата: единственная форма, в которой веха хранится и правится. */
const BARE_DATE = /^\d{4}-\d{2}-\d{2}$/;

const local = {
    /**
     * Зависимости от main.js.
     * @type {null | {dom: object, create: function(object): Promise<*>,
     *   patch: function(string, object): Promise<*>,
     *   remove: function(string): Promise<*>,
     *   note: function(string): void}}
     */
    deps: null,
    /** Слушатели навешены (идемпотентность init). */
    wired: false,
    /** Правимая веха или null (создание). */
    ms: null,
    /** Запрос ушёл — второй клик по OK не должен завести вторую веху. */
    busy: false,
};

// ------------------------------------------------------------------ чистое

/**
 * Тело PATCH вехи: только изменённое.
 *
 * ЧИСТАЯ функция — как `buildPatch` карточки задачи и по той же причине: у
 * вехи два поля, и «правка одной подписи не должна трогать дату» проверяется
 * тестом, а не глазами. Пустая подпись и непригодная дата в патч не попадают
 * вовсе: у вехи, в отличие от срока задачи, СБРОСА нет — веха без даты не
 * существует, её удаляют кнопкой.
 *
 * @param {object} ms веха до правки
 * @param {{title: string, date: string}} form значения формы
 * @returns {object} тело PATCH (может быть пустым)
 */
export function milestonePatch(ms, form) {
    const old = ms || {};
    const patch = {};
    const title = String((form && form.title) || '').trim();
    if (title && title !== String(old.title || '')) patch.title = title;
    const date = String((form && form.date) || '').trim();
    if (BARE_DATE.test(date) && date !== String(old.date || '').slice(0, 10)) patch.date = date;
    return patch;
}

/**
 * Годна ли форма для СОЗДАНИЯ вехи: нужны и подпись, и дата.
 *
 * @param {{title: string, date: string}} form
 * @returns {boolean}
 */
export function milestoneReady(form) {
    const title = String((form && form.title) || '').trim();
    const date = String((form && form.date) || '').trim();
    return !!title && BARE_DATE.test(date);
}

// --------------------------------------------------------------------- DOM

function dom() {
    return (local.deps && local.deps.dom) || {};
}

function showError(message) {
    const { $trkMsError } = dom();
    if (!$trkMsError) return;
    $trkMsError.textContent = message || '';
    $trkMsError.hidden = !message;
}

/** Значения формы. */
function formValues() {
    const { $trkMsTitle, $trkMsDate } = dom();
    return {
        title: $trkMsTitle ? $trkMsTitle.value : '',
        date: $trkMsDate ? $trkMsDate.value : '',
    };
}

/** Кнопка OK гаснет, пока форма не годна: пустая веха — заведомый 400. */
function paint() {
    const { $trkMsOk, $trkMsDelete } = dom();
    if ($trkMsOk) $trkMsOk.disabled = local.busy || !milestoneReady(formValues());
    if ($trkMsDelete) {
        $trkMsDelete.hidden = !local.ms;
        $trkMsDelete.disabled = local.busy;
    }
}

/**
 * Открыть диалог.
 *
 * @param {object|null} ms правимая веха; `null` — создание новой
 * @param {string} defaultDate дата по умолчанию для новой вехи (`YYYY-MM-DD`)
 */
export function openMilestoneDialog(ms, defaultDate) {
    const { $trkMs, $trkMsTitle, $trkMsDate, $trkMsHint, $trkMsOk } = dom();
    if (!$trkMs) return;
    local.ms = ms || null;
    local.busy = false;
    showError('');
    if ($trkMsTitle) $trkMsTitle.value = local.ms ? String(local.ms.title || '') : '';
    if ($trkMsDate) {
        $trkMsDate.value = local.ms
            ? String(local.ms.date || '').slice(0, 10)
            : String(defaultDate || '').slice(0, 10);
    }
    if ($trkMsHint) $trkMsHint.textContent = local.ms ? 'правка вехи' : 'новая веха';
    if ($trkMsOk) $trkMsOk.textContent = local.ms ? 'OK ⏎' : 'ADD ⏎';
    paint();
    $trkMs.hidden = false;
    if ($trkMsTitle) $trkMsTitle.focus();
}

/** Закрыть диалог, ничего не меняя. */
export function closeMilestoneDialog() {
    const { $trkMs } = dom();
    if (!$trkMs) return;
    $trkMs.hidden = true;
    local.ms = null;
    local.busy = false;
    showError('');
}

/** Открыт ли диалог. Нужен карточке задачи, чтобы не перехватывать Esc. */
export function isMilestoneOpen() {
    const { $trkMs } = dom();
    return !!$trkMs && !$trkMs.hidden;
}

/** OK: создать новую веху или сохранить правку. */
async function submit() {
    if (local.busy) return;
    const form = formValues();
    if (!milestoneReady(form)) return;
    const deps = local.deps;
    const title = form.title.trim();

    if (!local.ms) {
        local.busy = true;
        paint();
        try {
            await deps.create({ title, date: form.date });
        } catch (e) {
            local.busy = false;
            paint();
            // Ошибка показывается В ДИАЛОГЕ: набранное не пропадает, повтор —
            // один клик (то же правило, что у диалога переноса).
            showError(`веха не создана: ${(e && e.message) || e}`);
            return;
        }
        closeMilestoneDialog();
        deps.note(`веха «${title}» — ${form.date}`);
        return;
    }

    const patch = milestonePatch(local.ms, form);
    if (!Object.keys(patch).length) {
        closeMilestoneDialog();
        return;
    }
    local.busy = true;
    paint();
    try {
        await deps.patch(local.ms.id, patch);
    } catch (e) {
        local.busy = false;
        paint();
        showError(`правка вехи не удалась: ${(e && e.message) || e}`);
        return;
    }
    closeMilestoneDialog();
    deps.note(`веха «${title}» — ${form.date}`);
}

/** DELETE: убрать веху. Видна только при правке. */
async function remove() {
    if (local.busy || !local.ms) return;
    const deps = local.deps;
    const title = String(local.ms.title || '');
    local.busy = true;
    paint();
    try {
        await deps.remove(local.ms.id);
    } catch (e) {
        local.busy = false;
        paint();
        showError(`удаление вехи не удалось: ${(e && e.message) || e}`);
        return;
    }
    closeMilestoneDialog();
    deps.note(`веха «${title}» удалена`);
}

function onKeydown(ev) {
    if (!isMilestoneOpen()) return;
    if (ev.key === 'Escape') {
        ev.stopPropagation();
        closeMilestoneDialog();
        return;
    }
    // Enter в однострочных полях — это «готово»: диалог из двух полей не
    // должен требовать поездки мышью до кнопки.
    if (ev.key === 'Enter' && !ev.shiftKey) {
        const { $trkMs } = dom();
        if (!$trkMs || !$trkMs.contains(ev.target)) return;
        ev.preventDefault();
        ev.stopPropagation();
        submit();
    }
}

/**
 * Навесить слушатели диалога. Идемпотентна: зовётся из initTracker на каждом
 * входе на вкладку.
 *
 * @param {object} deps узлы (`dom`) и сервисы: `create`, `patch`, `remove`, `note`.
 */
export function initTrackerMilestone(deps) {
    local.deps = deps || local.deps;
    if (local.wired) return;
    local.wired = true;
    const {
        $trkMs, $trkMsClose, $trkMsCancel, $trkMsOk, $trkMsDelete, $trkMsTitle, $trkMsDate,
    } = dom();
    if ($trkMsClose) $trkMsClose.addEventListener('click', closeMilestoneDialog);
    if ($trkMsCancel) $trkMsCancel.addEventListener('click', closeMilestoneDialog);
    if ($trkMsOk) $trkMsOk.addEventListener('click', submit);
    if ($trkMsDelete) $trkMsDelete.addEventListener('click', remove);
    if ($trkMsTitle) $trkMsTitle.addEventListener('input', paint);
    if ($trkMsDate) $trkMsDate.addEventListener('input', paint);
    if ($trkMs) {
        $trkMs.addEventListener('click', (ev) => {
            if (ev.target === $trkMs) closeMilestoneDialog();
        });
    }
    document.addEventListener('keydown', onKeydown);
}

/** Снять слушатель (beforeunload / teardown панели). */
export function teardownTrackerMilestone() {
    document.removeEventListener('keydown', onKeydown);
    local.wired = false;
}
