// Регресс-тест ОБЩЕЙ формы горячих клавиш (js/settings/hotkeys-tab.js).
//
// Форма — единственное место общего существующего кода, которое проект VK
// тронул намеренно: вместо второй копии она параметризована конфигом. Отсюда
// два требования, которые и проверяет этот тест:
//   1) с телеграмным конфигом форма ведёт себя ТОЧНО как раньше (поля
//      настроек, дефолт тумблера, состав таблицы, payload сохранения);
//   2) с VK-конфигом — так же, но пишет в vk_hotkeys_enabled / vk_hotkeys.
//
// Отдельно закреплён ГЕЙТ ДЕФОЛТА: тумблер инициализируется строго `!== false`.
// Подмена на `=== true` (идиома соседних форм, где дефолт выключенный) молча
// показала бы фичу выключенной всем, у кого настройка ни разу не сохранялась.
//
// Запуск: node tmux-web/tests/frontend/settings_hotkeys_form.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

// ---------------------------------------------------------------------------
// Фейковый DOM (минимум, которым пользуется форма)
// ---------------------------------------------------------------------------

function makeText(data) {
    return { nodeType: 3, tagName: null, parentNode: null, textContent: String(data) };
}

function detach(node) {
    const p = node.parentNode;
    if (!p) return;
    const i = p.childNodes.indexOf(node);
    if (i >= 0) p.childNodes.splice(i, 1);
    node.parentNode = null;
}

function makeEl(tag) {
    const node = {
        nodeType: 1,
        tagName: String(tag || 'div').toLowerCase(),
        childNodes: [],
        parentNode: null,
        dataset: {},
        style: {},
        attributes: {},
        listeners: new Map(),
        className: '',
        title: '',
        type: '',
        checked: false,
        disabled: false,
        hidden: false,
    };

    node.classList = {
        add(...cs) {
            const set = new Set(node.className.split(/\s+/).filter(Boolean));
            cs.forEach((c) => set.add(c));
            node.className = Array.from(set).join(' ');
        },
        remove(...cs) {
            const set = new Set(node.className.split(/\s+/).filter(Boolean));
            cs.forEach((c) => set.delete(c));
            node.className = Array.from(set).join(' ');
        },
        contains(c) {
            return node.className.split(/\s+/).filter(Boolean).includes(c);
        },
    };

    Object.defineProperty(node, 'textContent', {
        get: () => node.childNodes.map((c) => c.textContent).join(''),
        set(v) {
            node.childNodes.slice().forEach(detach);
            const t = String(v == null ? '' : v);
            if (t !== '') node.appendChild(makeText(t));
        },
    });
    // Форма чистит таблицу через innerHTML = '' — другой разметки она туда не
    // кладёт, поэтому достаточно очистки.
    Object.defineProperty(node, 'innerHTML', {
        get: () => node.childNodes.map((c) => c.textContent).join(''),
        set(v) {
            node.childNodes.slice().forEach(detach);
            if (String(v) !== '') throw new Error('фейковый DOM не парсит HTML');
        },
    });

    node.appendChild = (child) => {
        detach(child);
        child.parentNode = node;
        node.childNodes.push(child);
        return child;
    };
    node.removeChild = (child) => { detach(child); return child; };
    node.remove = () => detach(node);
    node.addEventListener = (type, fn) => {
        if (!node.listeners.has(type)) node.listeners.set(type, []);
        node.listeners.get(type).push(fn);
    };
    node.removeEventListener = (type, fn) => {
        const arr = node.listeners.get(type) || [];
        const i = arr.indexOf(fn);
        if (i >= 0) arr.splice(i, 1);
    };
    node.matches = (sel) => {
        const s = String(sel || '');
        if (s.charAt(0) === '.') return node.classList.contains(s.slice(1));
        return node.tagName === s.toLowerCase();
    };
    node.querySelectorAll = (sel) => {
        const out = [];
        const walk = (n) => n.childNodes.forEach((c) => {
            if (c.nodeType !== 1) return;
            if (c.matches(sel)) out.push(c);
            walk(c);
        });
        walk(node);
        return out;
    };
    node.querySelector = (sel) => node.querySelectorAll(sel)[0] || null;
    return node;
}

function dispatch(node, type, ev) {
    const e = Object.assign({
        type,
        target: node,
        defaultPrevented: false,
        preventDefault() { this.defaultPrevented = true; },
        stopPropagation() {},
    }, ev || {});
    (node.listeners.get(type) || []).slice().forEach((fn) => fn(e));
    return e;
}

const $document = makeEl('#document');
$document.createElement = (tag) => makeEl(tag);
$document.createTextNode = (t) => makeText(t);
globalThis.document = $document;
globalThis.window = { ForgeApp: {} };

/** Что уехало в PATCH /api/user-settings. */
const patches = [];
globalThis.fetch = async (url, init) => {
    let body = null;
    try { body = init && init.body ? JSON.parse(init.body) : null; } catch (_) { body = null; }
    patches.push({ url: String(url), method: (init && init.method) || 'GET', body });
    return {
        ok: true,
        status: 200,
        json: async () => Object.assign({ saved: true }, body),
    };
};

// ---------------------------------------------------------------------------
// Модуль
// ---------------------------------------------------------------------------

const modUrl = new URL('../../static/js/settings/hotkeys-tab.js', import.meta.url).href;
const F = await import(modUrl);
const TG = await import(new URL('../../static/js/telegram/keys.js', import.meta.url).href);
const VK = await import(new URL('../../static/js/vk/keys.js', import.meta.url).href);
const B24 = await import(new URL('../../static/js/bitrix/keys.js', import.meta.url).href);

// ---------------------------------------------------------------------------
// Мини-фреймворк
// ---------------------------------------------------------------------------

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Тумблер формы. */
function toggleOf(fs) {
    return fs.querySelector('.tg-hotkeys-enabled');
}

/** Строки таблицы действий. */
function rowsOf(fs) {
    return fs.querySelector('.tg-hotkeys-table').querySelectorAll('.tg-hotkeys-row');
}

/** Кнопка комбинации в строке. */
function comboBtn(row) {
    return row.querySelector('.tg-hotkeys-combo');
}

/** Нажатие клавиши в режиме записи (слушатель висит на document в capture). */
function pressKey(code, key, mods) {
    return dispatch($document, 'keydown', {
        code,
        key: key === undefined ? '' : key,
        ctrlKey: !!(mods && mods.ctrl),
        altKey: !!(mods && mods.alt),
        shiftKey: !!(mods && mods.shift),
        metaKey: !!(mods && mods.meta),
    });
}

/** Кнопка Save (последняя в блоке действий). */
function saveBtn(fs) {
    const acts = fs.querySelector('.notify-actions');
    return acts.querySelectorAll('button').find((b) => b.textContent === 'Save');
}

function resetAllBtn(fs) {
    const acts = fs.querySelector('.notify-actions');
    return acts.querySelectorAll('button').find((b) => b.textContent === 'Сбросить всё');
}

// ---------------------------------------------------------------------------
// 1. Конфиги: состав и адресация полей
// ---------------------------------------------------------------------------

eq(F.TELEGRAM_HOTKEYS_CONFIG.enabledKey, 'telegram_hotkeys_enabled', 'конфиг TG: имя тумблера');
eq(F.TELEGRAM_HOTKEYS_CONFIG.mapKey, 'telegram_hotkeys', 'конфиг TG: имя карты');
eq(F.VK_HOTKEYS_CONFIG.enabledKey, 'vk_hotkeys_enabled', 'конфиг VK: имя тумблера');
eq(F.VK_HOTKEYS_CONFIG.mapKey, 'vk_hotkeys', 'конфиг VK: имя карты');
ok(F.TELEGRAM_HOTKEYS_CONFIG.actions === TG.TG_ACTIONS, 'конфиг TG: реестр телеграмный');
ok(F.VK_HOTKEYS_CONFIG.actions === VK.VK_ACTIONS, 'конфиг VK: реестр VK');
eq(F.B24_HOTKEYS_CONFIG.enabledKey, 'bitrix_hotkeys_enabled', 'конфиг Б24: имя тумблера');
eq(F.B24_HOTKEYS_CONFIG.mapKey, 'bitrix_hotkeys', 'конфиг Б24: имя карты');
ok(F.B24_HOTKEYS_CONFIG.actions === B24.B24_ACTIONS, 'конфиг Б24: реестр Битрикс24');
// Поля настроек не пересекаются НИ У ОДНОЙ ПАРЫ: одна форма не смеет затирать
// соседнюю панель. Проверяется попарно по всем трём — с появлением третьей
// панели «одна пара» перестала быть исчерпывающей проверкой.
{
    const cfgs = [F.TELEGRAM_HOTKEYS_CONFIG, F.VK_HOTKEYS_CONFIG, F.B24_HOTKEYS_CONFIG];
    const enabledKeys = new Set(cfgs.map((c) => c.enabledKey));
    const mapKeys = new Set(cfgs.map((c) => c.mapKey));
    eq(enabledKeys.size, cfgs.length, 'конфиги: тумблеры адресуют РАЗНЫЕ поля');
    eq(mapKeys.size, cfgs.length, 'конфиги: карты адресуют РАЗНЫЕ поля');
    // И контракты панелей тоже разные — иначе сброс кэша одной панели
    // инвалидировал бы чужие привязки.
    eq(new Set(cfgs.map((c) => String(c.legend))).size, cfgs.length,
        'конфиги: легенды различимы глазом');
}

// ---------------------------------------------------------------------------
// 2. ГЕЙТ ДЕФОЛТА: тумблер включён, пока явно не выключили
// ---------------------------------------------------------------------------

for (const [name, cfg] of [
    ['Telegram', F.TELEGRAM_HOTKEYS_CONFIG],
    ['VK', F.VK_HOTKEYS_CONFIG],
    ['Битрикс24', F.B24_HOTKEYS_CONFIG],
]) {
    eq(toggleOf(F.buildHotkeysForm({}, null, cfg)).checked, true,
        `${name}: пустые настройки — тумблер ВКЛЮЧЁН`);
    eq(toggleOf(F.buildHotkeysForm(null, null, cfg)).checked, true,
        `${name}: настройки не приехали — тумблер ВКЛЮЧЁН`);
    eq(toggleOf(F.buildHotkeysForm({ [cfg.enabledKey]: false }, null, cfg)).checked, false,
        `${name}: явное false — тумблер выключён`);
    eq(toggleOf(F.buildHotkeysForm({ [cfg.enabledKey]: true }, null, cfg)).checked, true,
        `${name}: явное true — тумблер включён`);
    // Любое НЕ-false значение (мусор в файле, строка от старого клиента)
    // обязано читаться как «включено» — это и есть разница `!== false`.
    eq(toggleOf(F.buildHotkeysForm({ [cfg.enabledKey]: 'yes' }, null, cfg)).checked, true,
        `${name}: не-false значение — тумблер ВКЛЮЧЁН`);
    eq(toggleOf(F.buildHotkeysForm({ [cfg.enabledKey]: 0 }, null, cfg)).checked, true,
        `${name}: 0 — не false, тумблер ВКЛЮЧЁН`);
}

// Вызов без конфига (совместимость со старым кодом) обязан дать телеграмную
// форму, а не пустую таблицу.
{
    const fs = F.buildHotkeysForm({}, null);
    eq(rowsOf(fs).length, TG.TG_ACTIONS.length, 'без конфига: строится телеграмная форма');
    eq(fs.querySelector('legend').textContent, F.TELEGRAM_HOTKEYS_CONFIG.legend,
        'без конфига: легенда телеграмная');
}

// ---------------------------------------------------------------------------
// 3. Таблица действий
// ---------------------------------------------------------------------------

for (const [name, cfg, actions, groups] of [
    ['Telegram', F.TELEGRAM_HOTKEYS_CONFIG, TG.TG_ACTIONS, TG.TG_ACTION_GROUPS],
    ['VK', F.VK_HOTKEYS_CONFIG, VK.VK_ACTIONS, VK.VK_ACTION_GROUPS],
    ['Битрикс24', F.B24_HOTKEYS_CONFIG, B24.B24_ACTIONS, B24.B24_ACTION_GROUPS],
]) {
    const fs = F.buildHotkeysForm({}, null, cfg);
    eq(fs.querySelector('legend').textContent, cfg.legend, `${name}: легенда своя`);
    const rows = rowsOf(fs);
    eq(rows.length, actions.length, `${name}: по строке на действие`);

    // Заголовки групп — в объявленном порядке и по одному на группу.
    const heads = fs.querySelector('.tg-hotkeys-table').querySelectorAll('.tg-hotkeys-group')
        .map((h) => h.textContent);
    eq(heads, groups, `${name}: заголовки групп в объявленном порядке`);

    // Fixed-действие показано отключённой кнопкой: Escape — выход из режима.
    const fixedIdx = actions.findIndex((a) => a.fixed);
    ok(comboBtn(rows[rows.length - 1]).disabled === true || fixedIdx >= 0,
        `${name}: fixed-действие есть в реестре`);
    const fixedRow = rows.find((r) => comboBtn(r).classList.contains('tg-hotkeys-fixed'));
    ok(fixedRow, `${name}: fixed-строка отрисована`);
    eq(comboBtn(fixedRow).disabled, true, `${name}: fixed-кнопка отключена`);

    // Кнопка сброса недоступна, пока значение дефолтное.
    ok(rows.every((r) => r.querySelector('.tg-hotkeys-reset').disabled),
        `${name}: без переопределений сброс каждой строки недоступен`);
}

// ---------------------------------------------------------------------------
// 4. Переопределения из настроек и конфликт
// ---------------------------------------------------------------------------

{
    // VK: конфликт «msg.copy занял j» обязан обезоружить nav.next и сказать об
    // этом словами, а не молча вернуть дефолт.
    const cfg = F.VK_HOTKEYS_CONFIG;
    const fs = F.buildHotkeysForm({ vk_hotkeys: { 'msg.copy': 'j' } }, null, cfg);
    const rows = rowsOf(fs);
    const byTitle = (t) => rows.find((r) => r.querySelector('.tg-hotkeys-title').textContent === t);
    const loser = byTitle(VK.findAction('nav.next').title);
    eq(comboBtn(loser).textContent, '—', 'конфликт: проигравший показан без клавиши');
    ok(loser.classList.contains('tg-hotkeys-row-conflict'), 'конфликт: строка помечена');
    ok(loser.querySelector('.tg-hotkeys-warn').textContent.includes(VK.findAction('msg.copy').title),
        'конфликт: назван победитель');
    const note = fs.querySelector('.tg-hotkeys-conflict');
    ok(note.textContent.includes('без клавиши'), 'конфликт: общее предупреждение показано');
    eq(note.style.display, '', 'конфликт: предупреждение видимо');

    // Чужая (телеграмная) карта на форму VK не влияет — панели делят файл, но
    // не настройки.
    const clean = F.buildHotkeysForm({ telegram_hotkeys: { 'msg.copy': 'j' } }, null, cfg);
    eq(clean.querySelector('.tg-hotkeys-conflict').style.display, 'none',
        'изоляция: телеграмная карта не создаёт конфликта в форме VK');
}

// ---------------------------------------------------------------------------
// 5. Запись комбинации, сброс и сохранение
// ---------------------------------------------------------------------------

{
    const cfg = F.VK_HOTKEYS_CONFIG;
    const saved = [];
    const fs = F.buildHotkeysForm({}, (upd) => saved.push(upd), cfg);
    const rows = rowsOf(fs);
    const row = rows.find((r) => r.querySelector('.tg-hotkeys-title').textContent
        === VK.findAction('msg.copy').title);
    const btn = comboBtn(row);
    eq(btn.textContent, 'Y', 'запись: до правки показан дефолт');

    // Начали запись — кнопка просит клавишу.
    dispatch(btn, 'click', {});
    ok(btn.textContent.includes('жмите'), 'запись: кнопка перешла в режим записи');

    // Один модификатор — ещё не комбинация, ждём дальше.
    {
        const e = pressKey('ControlLeft', 'Control', { ctrl: true });
        eq(e.defaultPrevented, false, 'запись: один модификатор не завершает запись');
    }
    // Русская раскладка: сопоставление по event.code.
    {
        const e = pressKey('KeyC', 'с', { ctrl: true });
        eq(e.defaultPrevented, true, 'запись: комбинация принята');
    }
    {
        const fresh = rowsOf(fs).find((r) => r.querySelector('.tg-hotkeys-title').textContent
            === VK.findAction('msg.copy').title);
        eq(comboBtn(fresh).textContent, 'CtrlC', 'запись: новая комбинация показана');
        eq(fresh.querySelector('.tg-hotkeys-reset').disabled, false,
            'запись: сброс строки стал доступен');
    }

    // Save шлёт ТОЛЬКО свои два поля и только отличия от дефолтов.
    const before = patches.length;
    await (async () => { dispatch(saveBtn(fs), 'click', {}); await flush(); await flush(); })();
    ok(patches.length === before + 1, 'сохранение: ушёл ровно один PATCH');
    const sent = patches[patches.length - 1];
    eq(sent.method, 'PATCH', 'сохранение: метод PATCH');
    eq(Object.keys(sent.body).sort(), ['vk_hotkeys', 'vk_hotkeys_enabled'],
        'сохранение: в теле ровно два поля VK и ни одного телеграмного');
    eq(sent.body.vk_hotkeys, { 'msg.copy': 'Ctrl+c' }, 'сохранение: только отличие от дефолта');
    eq(sent.body.vk_hotkeys_enabled, true, 'сохранение: тумблер уехал как есть');
    eq(saved.length, 1, 'сохранение: onSaved вызван');

    // Escape в режиме записи — отмена, а не назначение.
    {
        const r = rowsOf(fs).find((x) => x.querySelector('.tg-hotkeys-title').textContent
            === VK.findAction('msg.quote').title);
        const b = comboBtn(r);
        dispatch(b, 'click', {});
        const e = pressKey('Escape', 'Escape');
        eq(e.defaultPrevented, true, 'запись: Escape перехвачен');
        const fresh = rowsOf(fs).find((x) => x.querySelector('.tg-hotkeys-title').textContent
            === VK.findAction('msg.quote').title);
        eq(comboBtn(fresh).textContent, 'O', 'запись: Escape вернул прежнюю клавишу');
    }

    // Назначение РОВНО дефолта хранится как «нет переопределения»: иначе смена
    // дефолта в будущей версии не доехала бы до пользователя.
    {
        const r = rowsOf(fs).find((x) => x.querySelector('.tg-hotkeys-title').textContent
            === VK.findAction('msg.reply').title);
        dispatch(comboBtn(r), 'click', {});
        pressKey('KeyR', 'к');
        const beforeSave = patches.length;
        dispatch(saveBtn(fs), 'click', {});
        await flush(); await flush();
        eq(patches.length, beforeSave + 1, 'дефолт: сохранение прошло');
        ok(!('msg.reply' in patches[patches.length - 1].body.vk_hotkeys),
            'дефолт: назначенный вручную дефолт в карту не попал');
    }

    // «Сбросить всё» очищает черновик целиком.
    {
        dispatch(resetAllBtn(fs), 'click', {});
        const beforeSave = patches.length;
        dispatch(saveBtn(fs), 'click', {});
        await flush(); await flush();
        eq(patches.length, beforeSave + 1, 'сброс: сохранение прошло');
        eq(patches[patches.length - 1].body.vk_hotkeys, {}, 'сброс: карта ушла пустой');
    }
}

// ---------------------------------------------------------------------------
// 6. invalidate адресует СВОЮ панель
// ---------------------------------------------------------------------------

{
    const hits = [];
    globalThis.window.ForgeApp = {
        telegramKeys: { invalidate: () => hits.push('telegram') },
        vkKeys: { invalidate: () => hits.push('vk') },
    };
    const fs = F.buildHotkeysForm({}, null, F.VK_HOTKEYS_CONFIG);
    dispatch(saveBtn(fs), 'click', {});
    await flush(); await flush();
    eq(hits, ['vk'], 'invalidate: форма VK сбросила кэш ТОЛЬКО панели VK');

    hits.length = 0;
    const fs2 = F.buildHotkeysForm({}, null, F.TELEGRAM_HOTKEYS_CONFIG);
    dispatch(saveBtn(fs2), 'click', {});
    await flush(); await flush();
    eq(hits, ['telegram'], 'invalidate: форма Telegram сбросила кэш ТОЛЬКО панели Telegram');

    // Панель не загружена (плагин выключен) — сохранение обязано пройти без
    // исключения: отсутствие контракта это норма, а не ошибка.
    globalThis.window.ForgeApp = {};
    const fs3 = F.buildHotkeysForm({}, null, F.VK_HOTKEYS_CONFIG);
    const before = patches.length;
    dispatch(saveBtn(fs3), 'click', {});
    await flush(); await flush();
    eq(patches.length, before + 1, 'invalidate: без контракта панели сохранение всё равно прошло');
}

// ---------------------------------------------------------------------------
// 7. Телеграмная форма после параметризации пишет ровно свои поля
// ---------------------------------------------------------------------------

{
    const fs = F.buildHotkeysForm(
        { telegram_hotkeys: { 'msg.copy': 'Ctrl+c' }, vk_hotkeys: { 'msg.copy': 'Ctrl+x' } },
        null,
        F.TELEGRAM_HOTKEYS_CONFIG,
    );
    const before = patches.length;
    dispatch(saveBtn(fs), 'click', {});
    await flush(); await flush();
    const sent = patches[patches.length - 1];
    eq(patches.length, before + 1, 'Telegram: ушёл один PATCH');
    eq(Object.keys(sent.body).sort(), ['telegram_hotkeys', 'telegram_hotkeys_enabled'],
        'Telegram: в теле ровно два телеграмных поля');
    eq(sent.body.telegram_hotkeys, { 'msg.copy': 'Ctrl+c' },
        'Telegram: своя карта сохранена, чужая не тронута');
}

// ---------------------------------------------------------------------------
// 8. Третья панель (Битрикс24): своя карта, свой invalidate, чужие не задеты
// ---------------------------------------------------------------------------

{
    const hits = [];
    globalThis.window.ForgeApp = {
        telegramKeys: { invalidate: () => hits.push('telegram') },
        vkKeys: { invalidate: () => hits.push('vk') },
        b24Keys: { invalidate: () => hits.push('b24') },
    };
    const fs = F.buildHotkeysForm(
        {
            telegram_hotkeys: { 'msg.copy': 'Ctrl+c' },
            vk_hotkeys: { 'msg.copy': 'Ctrl+x' },
            bitrix_hotkeys: { 'msg.like': 'Ctrl+l' },
        },
        null,
        F.B24_HOTKEYS_CONFIG,
    );
    const before = patches.length;
    dispatch(saveBtn(fs), 'click', {});
    await flush(); await flush();
    const sent = patches[patches.length - 1];
    eq(patches.length, before + 1, 'Битрикс24: ушёл один PATCH');
    eq(Object.keys(sent.body).sort(), ['bitrix_hotkeys', 'bitrix_hotkeys_enabled'],
        'Битрикс24: в теле ровно два своих поля');
    eq(sent.body.bitrix_hotkeys, { 'msg.like': 'Ctrl+l' },
        'Битрикс24: своя карта сохранена, чужие не тронуты');
    eq(hits, ['b24'], 'invalidate: форма Битрикс24 сбросила кэш ТОЛЬКО своей панели');
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/settings_hotkeys_form)`);
if (failures > 0) process.exit(1);
