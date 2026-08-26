// tmux-web / настройки — переиспользуемая форма выбора бесед.
//
// Экспортирует `buildPeerPicker(options)` — два списка с галочками («группы»
// площадки и «отдельные беседы»), поиск по второму списку, черновик выбора и
// две кнопки: «Снять всё» и «Убрать несуществующие».
//
// ── ЗАЧЕМ ОТДЕЛЬНЫЙ МОДУЛЬ
//
// Такую форму в проекте уже трижды написали руками — `vk-visibility-tab.js`,
// `telegram-visibility-tab.js`, `bitrix-visibility-tab.js`. Четвёртая копия
// (выбор бесед для авторазбора голосовых) была бы копией копии, а вместе с ней
// скопировался бы и главный инвариант формы — тот, который легче всего
// потерять (см. ниже про черновик). Поэтому он живёт ЗДЕСЬ, в одном месте, и
// покрыт своим тестом.
//
// ── ПОЧЕМУ ТРИ ФОРМЫ ВИДИМОСТИ НА ЭТОТ МОДУЛЬ НЕ ПЕРЕВЕДЕНЫ
//
// Сознательно. Общего у них — устройство двух списков; расходятся же они во
// всём остальном: свои классы разметки на каждую панель (`vk-visibility-item`
// против `tg-visibility-item`), свой тумблер режима, своя строка-итог с
// предупреждением о пустом выборе, свой набор пометок беседы, свои помощники
// `normalizeFilter`/`prune*` и свой payload сохранения. Перевод потребовал бы
// параметризовать всё перечисленное и переписать три работающие формы, чьи
// классы разметки прибиты гвоздями в их тестах, — ради выигрыша в полсотни
// строк. Приоритет обратный: не сломать работающее. Модуль сделан пригодным
// для них (классы задаются `classPrefix`, источники данных — функциями), и
// переезд возможен отдельной задачей, когда его будет чем оправдать.
//
// ── ИНВАРИАНТ, РАДИ КОТОРОГО ВСЁ ЭТО
//
// Черновик хранит ВСЕ выбранные id, а не только те, что удалось нарисовать.
// Список бесед приходит верхушкой (первые 200), а выбор человека вполне может
// содержать беседу, до которой руки не доходили полгода. Если бы черновик
// собирался из нарисованных галочек, первое же открытие настроек молча
// вычёркивало бы такие беседы из белого списка. Поэтому:
//   • выбранные, но не загруженные id рисуются отдельным блоком «Выбрано вне
//     загруженного списка» — иначе человек не поймёт, почему счётчик больше
//     числа галочек, и не сможет снять лишнее;
//   • чистка от несуществующих — ЯВНАЯ кнопка, и нажимает её человек;
//   • отказ загрузки списка выбор НЕ трогает: он уйдёт обратно тем же
//     составом.

/**
 * Строит форму выбора.
 *
 * @param {object} o параметры
 * @param {?Document} o.doc документ (тесты подсовывают свой)
 * @param {string} o.classPrefix префикс классов разметки (`voice-scope`)
 * @param {string} o.groupsTitle заголовок списка групп («Папки», «Типы бесед»)
 * @param {string} o.peersTitle заголовок списка бесед
 * @param {string} o.searchPlaceholder подсказка поля поиска
 * @param {string} o.searchAria доступное имя поля поиска
 * @param {Array<string>} o.initialGroups выбранные ключи групп
 * @param {Array<string>} o.initialPeers выбранные id бесед
 * @param {?function(): Promise<Array<object>>} o.loadGroups источник групп:
 *        `[{key, label, hint}]`; отсутствует — список групп не рисуется
 * @param {?function(): Promise<Array<object>>} o.loadPeers источник бесед:
 *        `[{id, title, marks}]`; отсутствует — список бесед не рисуется
 * @param {?function(): void} o.onChange зовётся после любой правки черновика
 * @param {?string} o.pruneTitle подсказка кнопки чистки
 * @returns {object} `{node, groups(), peers(), setDisabled(v), isLoaded()}`
 */
export function buildPeerPicker(o) {
    const opts = o || {};
    const d = opts.doc || document;
    const prefix = opts.classPrefix || 'peer-picker';

    // Рабочая копия: правки живут здесь до сохранения, закрытая без него
    // модалка ничего не меняет.
    const draftGroups = new Set(toIds(opts.initialGroups));
    const draftPeers = new Set(toIds(opts.initialPeers));

    const root = d.createElement('div');
    root.className = `${prefix}-picker`;

    // Живые галочки двух списков — их перебирает `setDisabled`. Два массива, а
    // не один: списки перерисовываются по отдельности, и общий копил бы ссылки
    // на узлы, которых в дереве давно нет.
    const groupInputs = [];
    const peerInputs = [];
    let disabled = false;
    // Поиск бывает выключен и по своей причине — списка бесед нет вовсе.
    // Тогда включение тумблера не имеет права его «оживить».
    let searchBroken = false;

    const notify = () => {
        if (typeof opts.onChange === 'function') opts.onChange();
    };

    /** Одна строка списка с галочкой. */
    function row(key, label, checked, set, extra, bucket) {
        const wrap = d.createElement('label');
        wrap.className = `modal-check ${prefix}-item`;
        const box = d.createElement('input');
        box.type = 'checkbox';
        box.checked = checked;
        box.disabled = disabled;
        box.addEventListener('change', () => {
            if (box.checked) set.add(key);
            else set.delete(key);
            notify();
        });
        if (bucket) bucket.push(box);
        wrap.appendChild(box);
        const text = d.createElement('span');
        text.className = `${prefix}-label`;
        text.textContent = label;
        wrap.appendChild(text);
        if (extra) {
            const badge = d.createElement('span');
            badge.className = `${prefix}-badge`;
            badge.textContent = extra;
            wrap.appendChild(badge);
        }
        return wrap;
    }

    // Загруженные данные площадки.
    const data = { groups: [], peers: [], loaded: false };

    // ── список групп ──
    let groupsBox = null;
    if (typeof opts.loadGroups === 'function') {
        const title = d.createElement('div');
        title.className = `${prefix}-group`;
        title.textContent = opts.groupsTitle || 'Группы';
        root.appendChild(title);
        groupsBox = d.createElement('div');
        groupsBox.className = `${prefix}-list ${prefix}-groups`;
        groupsBox.textContent = 'Загрузка…';
        root.appendChild(groupsBox);
    }

    // ── список бесед ──
    const peersTitle = d.createElement('div');
    peersTitle.className = `${prefix}-group`;
    peersTitle.textContent = opts.peersTitle || 'Отдельные беседы';
    root.appendChild(peersTitle);

    const search = d.createElement('input');
    search.type = 'search';
    search.className = `${prefix}-search`;
    search.placeholder = opts.searchPlaceholder || 'Поиск по названию';
    search.setAttribute('aria-label', opts.searchAria || 'Поиск беседы');
    root.appendChild(search);

    const peersBox = d.createElement('div');
    peersBox.className = `${prefix}-list ${prefix}-peers`;
    peersBox.textContent = 'Загрузка…';
    root.appendChild(peersBox);

    function renderGroups() {
        if (!groupsBox) return;
        groupsBox.textContent = '';
        groupInputs.length = 0;
        if (!data.groups.length) {
            const empty = d.createElement('div');
            empty.className = 'notify-hint';
            empty.textContent = 'Групп нет.';
            groupsBox.appendChild(empty);
            return;
        }
        for (const g of data.groups) {
            const key = String(g && g.key != null ? g.key : '');
            if (!key) continue;
            groupsBox.appendChild(row(
                key, String(g.label || key), draftGroups.has(key), draftGroups, g.hint,
                groupInputs,
            ));
        }
    }

    function renderPeers() {
        const q = String(search.value || '').trim().toLowerCase();
        peersBox.textContent = '';
        peerInputs.length = 0;
        const list = data.peers.filter((p) => {
            if (!q) return true;
            return String(p.title || p.id).toLowerCase().includes(q);
        });
        if (!list.length) {
            const empty = d.createElement('div');
            empty.className = 'notify-hint';
            empty.textContent = q ? 'Ничего не найдено.' : 'Список бесед пуст.';
            peersBox.appendChild(empty);
        }
        for (const p of list) {
            const id = String(p.id);
            peersBox.appendChild(row(
                id, String(p.title || id), draftPeers.has(id), draftPeers, p.marks, peerInputs,
            ));
        }
        // Выбранные беседы, которых нет в загруженной странице, обязаны быть
        // ВИДНЫ и остаться выбранными — это и есть инвариант из шапки.
        const shown = new Set(data.peers.map((p) => String(p.id)));
        const orphans = Array.from(draftPeers).filter((id) => !shown.has(id));
        if (orphans.length && !q) {
            const note = d.createElement('div');
            note.className = 'notify-hint';
            note.textContent = `Выбрано вне загруженного списка: ${orphans.length}.`;
            peersBox.appendChild(note);
            for (const id of orphans) {
                peersBox.appendChild(
                    row(id, `id ${id}`, true, draftPeers, 'не в списке', peerInputs),
                );
            }
        }
    }

    search.addEventListener('input', () => {
        if (data.loaded) renderPeers();
    });

    // ── загрузка данных площадки ──
    (async () => {
        try {
            const [groups, peers] = await Promise.all([
                typeof opts.loadGroups === 'function' ? opts.loadGroups() : [],
                typeof opts.loadPeers === 'function' ? opts.loadPeers() : [],
            ]);
            data.groups = Array.isArray(groups) ? groups : [];
            data.peers = Array.isArray(peers) ? peers : [];
            data.loaded = true;
            renderGroups();
            renderPeers();
        } catch (e) {
            // Площадка не отвечает (плагин выключен, нет логина) — рисовать
            // списки нечем. Уже сохранённый выбор при этом НЕ теряется: он
            // лежит в черновике и уйдёт обратно тем же составом.
            const msg = (e && e.code === 'not_authorized')
                ? 'Войдите в мессенджер, чтобы выбрать беседы.'
                : `Не удалось загрузить список бесед: ${(e && e.message) || 'ошибка'}`;
            if (groupsBox) groupsBox.textContent = msg;
            else peersBox.textContent = msg;
            if (groupsBox) peersBox.textContent = '';
            searchBroken = true;
            search.disabled = true;
        }
    })();

    // ── кнопки ──
    const actions = d.createElement('div');
    actions.className = `notify-actions ${prefix}-actions`;

    const clearBtn = d.createElement('button');
    clearBtn.type = 'button';
    clearBtn.className = `${prefix}-clear`;
    clearBtn.textContent = 'Снять всё';
    clearBtn.addEventListener('click', () => {
        if (disabled) return;
        draftGroups.clear();
        draftPeers.clear();
        renderGroups();
        renderPeers();
        notify();
    });
    actions.appendChild(clearBtn);

    const pruneBtn = d.createElement('button');
    pruneBtn.type = 'button';
    pruneBtn.className = `${prefix}-prune`;
    pruneBtn.textContent = 'Убрать несуществующие';
    pruneBtn.title = opts.pruneTitle
        || 'Вычистить из выбора беседы, которых нет в загруженном списке';
    pruneBtn.addEventListener('click', () => {
        // Только по загруженному списку и только по нажатию: чистка «сама
        // собой» вычеркнула бы беседу, просто не попавшую в верхушку страницы.
        if (disabled || !data.loaded) return;
        const shown = new Set(data.peers.map((p) => String(p.id)));
        Array.from(draftPeers)
            .filter((id) => !shown.has(id))
            .forEach((id) => draftPeers.delete(id));
        renderPeers();
        notify();
    });
    actions.appendChild(pruneBtn);
    root.appendChild(actions);

    renderGroups();
    renderPeers();

    return {
        /** Узел формы. */
        node: root,
        /** Выбранные ключи групп, в порядке отметки. */
        groups: () => Array.from(draftGroups),
        /** Выбранные id бесед, включая НЕ загруженные. */
        peers: () => Array.from(draftPeers),
        /** Загружены ли списки площадки. */
        isLoaded: () => data.loaded,
        /**
         * Делает форму недоступной целиком.
         *
         * Именно недоступной, а не спрятанной: исчезнувший при снятии галочки
         * блок читался бы как «настройка пропала», а выключенный честно
         * показывает и сам выбор, и то, что он сейчас ни на что не влияет.
         */
        setDisabled(v) {
            disabled = !!v;
            for (const box of groupInputs) box.disabled = disabled;
            for (const box of peerInputs) box.disabled = disabled;
            search.disabled = disabled || searchBroken;
            clearBtn.disabled = disabled;
            pruneBtn.disabled = disabled;
        },
    };
}

/** Список строк без пустых и дублей, порядок сохраняется. */
function toIds(raw) {
    if (!Array.isArray(raw)) return [];
    const out = [];
    for (const item of raw) {
        if (typeof item !== 'string' && typeof item !== 'number') continue;
        const s = String(item).trim();
        if (s && !out.includes(s)) out.push(s);
    }
    return out;
}
