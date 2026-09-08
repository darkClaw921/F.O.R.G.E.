// tmux-web / тест формы «Видимость диалогов» вкладки «Битрикс24»
// (js/settings/bitrix-visibility-tab.js) — ПЕР-ПОРТАЛЬНАЯ часть (Ф7.5).
//
// Почему это отдельный тест, а не строчка в bitrix_visibility.test.mjs: там
// живут ЧИСТЫЕ предикаты и четыре точки применения, а здесь — форма, то есть
// DOM и сеть. Смешение уронило бы чистоту первого файла, который специально
// обходится без стаба браузера для самого модуля видимости.
//
// Что закреплено:
//   • переключатель портала в шапке; пустой `title` показывается как `id`
//     (бэкенд хост портала наружу не отдаёт — см. routes/portals.rs);
//   • список диалогов тянется с ЯВНЫМ заголовком X-Forge-Portal, а не с
//     активным порталом панели: форма может править не тот портал, что открыт;
//   • сохранение уходит ТОЛЬКО в запись выбранного портала
//     (`bitrix_portal_prefs`), плоские ключи не трогаются вовсе — они нужны
//     откату на старую сборку;
//   • смена портала перечитывает и выбор, и список: черновик соседнего портала
//     не переносится (в нём его `DIALOG_ID`, то есть чужие чаты).
//
// Запуск: node tmux-web/tests/frontend/settings_bitrix_visibility_form.test.mjs

import { makeEl, makeText, dispatch } from './meet_popup_dom.mjs';

const $document = makeEl('#document');
$document.createElement = (tag) => {
    const node = makeEl(tag);
    node.style = {};
    node.checked = false;
    // У общего стаба нет `style`, `checked` и `classList.toggle` — они нужны
    // только формам настроек, а не карточкам встреч, ради которых он написан.
    // Дописываем ЗДЕСЬ, чтобы не менять поведение соседних тестов (тот же
    // приём, что в settings_voice_tab.test.mjs).
    node.classList.toggle = (cls, force) => {
        const on = (force === undefined) ? !node.classList.contains(cls) : force === true;
        if (on) node.classList.add(cls);
        else node.classList.remove(cls);
        return on;
    };
    return node;
};
$document.createTextNode = (t) => makeText(t);
globalThis.document = $document;
globalThis.window = { ForgeApp: {}, location: { origin: 'http://localhost' } };

/** Что уехало в сеть: адрес, метод, заголовки, тело. */
const requests = [];
/** Ответ на GET /api/bitrix/dialogs — зависит от заголовка портала. */
const DIALOGS = {
    acme: [{ id: '19', title: 'Иванов', kind: 'user' }, { id: 'chat7', title: 'Задача', kind: 'task' }],
    beta: [{ id: 'chat3', title: 'Общий', kind: 'chat' }],
};

globalThis.fetch = async (url, init) => {
    const headers = (init && init.headers) || {};
    let body = null;
    try { body = init && init.body ? JSON.parse(init.body) : null; } catch (_) { body = null; }
    const rec = {
        url: String(url),
        method: (init && init.method) || 'GET',
        portal: headers['X-Forge-Portal'] || '',
        body,
    };
    requests.push(rec);
    const json = () => {
        if (rec.url.includes('/api/bitrix/portals')) {
            return {
                portals: [
                    { id: 'acme', title: 'Acme', connected: true },
                    { id: 'beta', title: '', connected: true },
                ],
            };
        }
        if (rec.url.includes('/api/bitrix/dialogs')) {
            return { dialogs: DIALOGS[rec.portal] || [] };
        }
        return Object.assign({ saved: true }, body);
    };
    return {
        ok: true,
        status: 200,
        headers: { get: () => 'application/json' },
        json: async () => json(),
        text: async () => JSON.stringify(json()),
    };
};

const API = await import(new URL('../../static/js/bitrix/api.js', import.meta.url).href);
const PORTAL = await import(
    new URL('../../static/js/settings/bitrix-settings-portal.js', import.meta.url).href
);
const FORM = await import(
    new URL('../../static/js/settings/bitrix-visibility-tab.js', import.meta.url).href
);

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
const flush = () => new Promise((r) => { setTimeout(r, 0); });

/** Настройки: у `acme` свой белый список, у `beta` — свой. */
const SETTINGS = {
    bitrix_filter_enabled: true,
    bitrix_visible_kinds: ['user'],
    bitrix_visible_peers: ['19'],
    bitrix_portal_prefs: {
        acme: { filter_enabled: true, visible_kinds: ['user'], visible_peers: ['19'] },
        beta: { filter_enabled: false, visible_kinds: ['chat'], visible_peers: [] },
    },
};

/** Отмеченные строки одного списка: стаб не умеет вложенных селекторов. */
const checkedLabels = (fs, sel) => (fs.querySelector(sel)
    ? fs.querySelector(sel).querySelectorAll('.b24-visibility-item') : [])
    .filter((row) => {
        const box = row.querySelector('input');
        return box && box.checked;
    })
    .map((row) => {
        const label = row.querySelector('.b24-visibility-label');
        return label ? label.textContent : '';
    });

/** Строки-чекбоксы одного списка. */
const boxesOf = (fs, sel) => (fs.querySelector(sel)
    ? fs.querySelector(sel).querySelectorAll('.b24-visibility-item') : [])
    .map((row) => row.querySelector('input'))
    .filter(Boolean);

// ---------------------------------------------------------------------------
// Панель открыта на `acme`; форма показывает его настройки
// ---------------------------------------------------------------------------

API.setActivePortal('acme');
PORTAL.setSettingsPortal('');
requests.length = 0;

let switched = 0;
const fs = FORM.buildBitrixVisibilityForm(SETTINGS, null, () => { switched += 1; });
await flush();
await flush();

{
    const select = fs.querySelector('.b24-visibility-portal');
    ok(select !== null, 'переключатель портала есть в шапке формы');
    eq(select.querySelectorAll('option').map((o) => o.textContent), ['Acme', 'beta'],
        'пустой title показывается как id — второго правила подписи нет');
    eq(select.value, 'acme', 'по умолчанию правится активный портал панели');

    const list = requests.filter((r) => r.url.includes('/api/bitrix/dialogs'));
    eq(list.length, 1, 'список диалогов запрошен один раз — поллинга нет');
    eq(list[0].portal, 'acme', 'запрос списка адресован ЯВНО выбранному порталу');

    eq(checkedLabels(fs, '.b24-visibility-kinds'), ['Личные диалоги'],
        'виды взяты из записи портала acme');
    eq(checkedLabels(fs, '.b24-visibility-peers'), ['Иванов'],
        'поимённый выбор — тоже его');
    ok(fs.querySelector('.b24-filter-enabled').checked, 'тумблер взят из записи портала');
}

// ---------------------------------------------------------------------------
// Сохранение уходит ТОЛЬКО в запись выбранного портала
// ---------------------------------------------------------------------------

{
    requests.length = 0;
    boxesOf(fs, '.b24-visibility-peers').forEach((box) => {
        if (box.checked) { box.checked = false; dispatch(box, 'change'); }
    });
    dispatch(fs.querySelectorAll('button').find((b) => b.textContent === 'Save'), 'click');
    await flush();
    await flush();

    const patch = requests.find((r) => r.method === 'PATCH');
    ok(patch !== undefined, 'сохранение ушло PATCH-запросом');
    eq(patch.body, {
        bitrix_portal_prefs: {
            acme: { filter_enabled: true, visible_kinds: ['user'], visible_peers: [] },
        },
    }, 'тело адресовано записи ВЫБРАННОГО портала и содержит только три поля');
    // Плоские ключи не трогаются: они заморожены на домультипортальном
    // значении и нужны откату на старую сборку.
    ok(!('bitrix_visible_peers' in patch.body), 'плоские ключи форма не переписывает');
    ok(!('beta' in patch.body.bitrix_portal_prefs), 'запись соседнего портала не упомянута');
}

// ---------------------------------------------------------------------------
// Смена портала: другой выбор, другой список, соседние формы предупреждены
// ---------------------------------------------------------------------------

{
    requests.length = 0;
    const select = fs.querySelector('.b24-visibility-portal');
    select.value = 'beta';
    dispatch(select, 'change');
    await flush();
    await flush();

    eq(PORTAL.settingsPortal(), 'beta', 'выбор запомнен для соседних форм');
    eq(switched, 1, 'вкладка настроек предупреждена — скоуп голосовых тоже портальный');

    const list = requests.filter((r) => r.url.includes('/api/bitrix/dialogs'));
    eq(list.length, 1, 'список перечитан ровно один раз');
    eq(list[0].portal, 'beta', 'и уже у нового портала');

    eq(checkedLabels(fs, '.b24-visibility-kinds'), ['Чаты'],
        'виды перечитаны из записи портала beta');
    eq(checkedLabels(fs, '.b24-visibility-peers'), [],
        'черновик соседнего портала НЕ перенесён: его id — чужие чаты');
    ok(!fs.querySelector('.b24-filter-enabled').checked,
        'тумблер тоже пер-портальный (у beta он выключен)');
}

// ---------------------------------------------------------------------------
// Портала в реестре нет вовсе — форма работает, а не падает
// ---------------------------------------------------------------------------

{
    const previous = globalThis.fetch;
    globalThis.fetch = async () => { throw new Error('плагин выключен'); };
    PORTAL.setSettingsPortal('');
    API.setActivePortal('');
    const orphan = FORM.buildBitrixVisibilityForm({}, null);
    await flush();
    await flush();
    const select = orphan.querySelector('.b24-visibility-portal');
    ok(select.disabled === true, 'без реестра переключать нечего');
    ok(String(orphan.textContent).includes('Подключите портал')
        || String(orphan.textContent).includes('Не удалось загрузить'),
    'причина пустого списка названа человеку');
    globalThis.fetch = previous;
}

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED (frontend/settings_bitrix_visibility_form)`);
    process.exit(1);
}
console.log(`\n${checks}/${checks} checks passed (frontend/settings_bitrix_visibility_form)`);
