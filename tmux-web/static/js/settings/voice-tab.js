// tmux-web / настройки — секция «Голосовые сообщения».
//
// ОДИН модуль на три панели (VK, Telegram, Битрикс24), различия — в конфиге.
// Это прямое требование задачи фазы: «вынести в общий модуль настроек, а не
// копировать третий раз». Прецедент в этом же каталоге — `hotkeys-tab.js`
// (`buildHotkeysForm(settings, onSaved, config)` плюс три константы-конфига);
// формы видимости, наоборот, скопированы, но там расходится ВСЁ содержимое
// (папки против типов против видов), а здесь — ровно два ключа.
//
// ── Что в секции
//
// 1. Чекбокс «Автоматически разбирать новые аудиосообщения» — пишет СВОЙ ключ
//    user_settings (`vk_voice_autotranscribe` / `telegram_…` / `bitrix_…`).
//    Дефолт — ВЫКЛЮЧЕНО, и читается он строго как `=== true`: фича тратит GPU
//    либо деньги в облаке, и включаться сама она не имеет права.
// 2. Селект движка (Локально / Облако) — ОБЩИЙ для всех мессенджеров. Он живёт
//    не в user_settings, а в `voice.toml` рядом с ключом polza.ai, и читается
//    роутом `…/voice/settings` той панели, чья вкладка открыта. Отсюда подпись
//    под ним: человек обязан понимать, что переключил движок ВЕЗДЕ.
//
// ── Почему чекбокс сохраняется сразу, а не по кнопке Save
//
// Потому что он один. Кнопка Save существует в соседних формах, где черновик
// собирается из десятков галочек и «сохранить» — осмысленный момент. Здесь
// момент один и тот же: щёлкнул — включилось. Ошибка сети при этом ОТКАТЫВАЕТ
// галочку обратно (см. `updateUserSettings`, у него optimistic-обновление с
// откатом) и печатает причину рядом — молча вернувшийся в исходное положение
// чекбокс выглядел бы как поломка UI.
//
// ── Чего здесь нет
//
// Ключа polza.ai и вообще любых секретов: роут отдаёт лишь признак
// `cloud_key_set`. Если ключа нет, а выбрано облако, бэкенд присылает
// `warning`, и секция показывает его текст — это единственный способ узнать
// причину, не открывая `voice.toml`.

import { buildPeerPicker } from './peer-picker.js';
import { updateUserSettings } from './user-settings-api.js';

/**
 * Подпись блока выбора бесед — ОДНА на три панели.
 *
 * Пустой список означает «во всех беседах», и сказать это надо вслух: белый
 * список, который человек ни разу не открывал, читается как «ничего не
 * выбрано», а значит — как «ничего и не разбирается». Настройка работает
 * ровно наоборот, и молчаливое расхождение здесь стоило бы человеку
 * уверенности в том, что фича вообще включена.
 */
export const SCOPE_HINT =
    'Пока ничего не выбрано, разбираются голосовые ВО ВСЕХ беседах. '
    + 'Отметьте нужные — и авторазбор ограничится только ими. '
    + 'Беседы, скрытые белым списком видимости, не разбираются в любом случае.';

/**
 * Значения селекта движка — в порядке показа.
 *
 * Облако первым и подписано «по умолчанию», потому что это СТАНДАРТ фичи:
 * голосовое в чате живёт секунды и читается сразу, поэтому оно не должно ни
 * ждать очереди к GPU, ни требовать 574 МБ весов на машине. Локальный движок —
 * осознанный выбор человека, а не то, куда можно съехать молча: бэкенд по той
 * же причине НЕ наследует `stt_backend` из настроек встреч (см. шапку
 * `plugins/voice/src/config.rs`).
 */
export const ENGINES = [
    { key: 'cloud', label: 'Облако (polza.ai) — по умолчанию' },
    { key: 'local', label: 'Локально (whisper.cpp на GPU)' },
];

/** Подпись под селектом: настройка одна на все мессенджеры. */
export const SHARED_HINT =
    'Движок общий для всех мессенджеров: он хранится в voice.toml рядом с ключом polza.ai.';

/**
 * Конфиг секции для панели VK.
 *
 * `api` передаётся ССЫЛКАМИ на функции панели, а не строкой пути: адреса живут
 * в модулях `api.js` каждой панели, и контрактные тесты сверяют их с таблицей
 * роутов плагина. Собранный здесь путь эту сверку обошёл бы.
 */
export const VK_VOICE_CONFIG = {
    key: 'vk',
    enabledKey: 'vk_voice_autotranscribe',
    toggleClass: 'vk-voice-auto',
    scope: {
        kindsKey: 'vk_voice_scope_kinds',
        peersKey: 'vk_voice_scope_peers',
        groupsTitle: 'Типы бесед',
        peersTitle: 'Отдельные беседы',
        searchPlaceholder: 'Поиск по названию',
        searchAria: 'Поиск беседы',
    },
    hint:
        'Новое входящее голосовое расшифровывается само, текст появляется под волной. '
        + 'Беседы, скрытые белым списком выше, не разбираются; свои исходящие — тоже.',
};

/** Конфиг секции для панели Telegram. */
export const TELEGRAM_VOICE_CONFIG = {
    key: 'telegram',
    enabledKey: 'telegram_voice_autotranscribe',
    toggleClass: 'tg-voice-auto',
    // У Telegram «группы» — это ПАПКИ аккаунта: принадлежность чата папке из
    // одного его id не выводится, поэтому папку выбирают в форме, а плагин
    // сверяется с ней по своей карте чатов.
    scope: {
        kindsKey: 'telegram_voice_scope_folders',
        peersKey: 'telegram_voice_scope_dialogs',
        groupsTitle: 'Папки',
        peersTitle: 'Отдельные чаты',
        searchPlaceholder: 'Поиск по названию',
        searchAria: 'Поиск чата',
    },
    hint:
        'Новое входящее голосовое расшифровывается само, текст появляется под волной. '
        + 'Разбираются только голосовые: пересланная музыка и подкасты — по кнопке.',
};

/** Конфиг секции для панели Битрикс24. */
export const B24_VOICE_CONFIG = {
    key: 'bitrix',
    enabledKey: 'bitrix_voice_autotranscribe',
    toggleClass: 'b24-voice-auto',
    // Ограничение портала: чат задачи по `DIALOG_ID` неотличим от обычного
    // чата, поэтому вид `task` — такая же группа в этом списке, а отдельного
    // ключа «только чаты задач» нет и быть не может.
    scope: {
        kindsKey: 'bitrix_voice_scope_kinds',
        peersKey: 'bitrix_voice_scope_peers',
        groupsTitle: 'Виды диалогов',
        peersTitle: 'Отдельные диалоги',
        searchPlaceholder: 'Поиск по названию',
        searchAria: 'Поиск диалога',
    },
    hint:
        'Новое входящее голосовое расшифровывается само, текст появляется под волной. '
        + 'Диалоги, скрытые белым списком выше, не разбираются; свои исходящие — тоже.',
};

/**
 * Строит секцию «Голосовые сообщения».
 *
 * @param {?object} settings снимок user settings (`state.userSettings`)
 * @param {?function} onSaved зовётся с обновлёнными настройками после PATCH
 * @param {object} config один из `*_VOICE_CONFIG`
 * @param {?{getVoiceSettings: function, setVoiceSettings: function}} api
 *        сетевой слой панели; отсутствует — селект движка не рисуется
 * @param {?Document} doc документ (тесты подсовывают свой)
 * @returns {object} узел `<fieldset>`
 */
export function buildVoiceForm(settings, onSaved, config, api, doc) {
    const d = doc || document;
    const s = (settings && typeof settings === 'object') ? settings : {};
    const cfg = config || VK_VOICE_CONFIG;

    const fs = d.createElement('fieldset');
    fs.className = 'notify-fieldset';

    const legend = d.createElement('legend');
    legend.textContent = 'Голосовые сообщения';
    fs.appendChild(legend);

    const hint = d.createElement('div');
    hint.className = 'notify-hint';
    hint.textContent = cfg.hint || '';
    fs.appendChild(hint);

    // ── чекбокс авторазбора ──
    const wrap = d.createElement('label');
    wrap.className = 'modal-check notify-check';
    const box = d.createElement('input');
    box.type = 'checkbox';
    box.className = cfg.toggleClass;
    // `=== true`, а не `!== false`: дефолт этой настройки — ВЫКЛЮЧЕНО.
    box.checked = s[cfg.enabledKey] === true;
    wrap.appendChild(box);
    wrap.appendChild(d.createTextNode(' Автоматически разбирать новые аудиосообщения'));
    fs.appendChild(wrap);

    const err = d.createElement('div');
    err.className = 'notify-error';
    err.style.display = 'none';
    fs.appendChild(err);

    const ok = d.createElement('div');
    ok.className = 'notify-hint voice-save-ok';
    ok.textContent = 'Сохранено';
    ok.style.display = 'none';
    fs.appendChild(ok);

    const showError = (e) => {
        err.style.display = '';
        err.textContent = (e && e.message) ? e.message : 'Не удалось сохранить настройку.';
    };
    const showOk = () => {
        err.style.display = 'none';
        ok.style.display = '';
        // Таймер только косметический: если окружение его не даёт (тест), это
        // ничего не ломает — надпись просто останется.
        if (typeof setTimeout === 'function') {
            setTimeout(() => { ok.style.display = 'none'; }, 2000);
        }
    };

    // ── выбор бесед для авторазбора ──
    //
    // Отдельный белый список, НЕ связанный с видимостью: «не показывай мне
    // этот чат» и «не трать GPU на этот чат» — разные просьбы. Пустой список
    // означает «во всех беседах» (см. SCOPE_HINT), тумблера у него нет.
    const scope = cfg.scope ? buildScope(d, cfg, api, s, onSaved) : null;
    if (scope) {
        fs.appendChild(scope.node);
        // Недоступен, а не спрятан: исчезнувший блок читался бы как «настройки
        // нет», а выключенный честно показывает и выбор, и то, что он сейчас
        // ни на что не влияет.
        scope.setEnabled(box.checked === true);
    }

    box.addEventListener('change', async () => {
        const desired = !!box.checked;
        box.disabled = true;
        // Доступность блока меняется СРАЗУ, ещё до ответа бэкенда: иначе
        // человек, включивший тумблер, видел бы недоступный выбор и решил, что
        // надо переоткрыть модалку. Отказ PATCH вернёт всё обратно вместе с
        // самим чекбоксом.
        if (scope) scope.setEnabled(desired);
        try {
            const updated = await updateUserSettings({ [cfg.enabledKey]: desired });
            showOk();
            if (typeof onSaved === 'function') onSaved(updated);
        } catch (e) {
            // Откат: `updateUserSettings` уже вернул кеш на место, а мы
            // возвращаем сам чекбокс — иначе UI утверждал бы, что фича
            // включена, а бэкенд об этом не знал бы.
            box.checked = !desired;
            if (scope) scope.setEnabled(!desired);
            showError(e);
        } finally {
            box.disabled = false;
        }
    });

    // ── селект движка ──
    if (!api || typeof api.getVoiceSettings !== 'function') return fs;

    const engineTitle = d.createElement('div');
    engineTitle.className = 'vk-visibility-group voice-engine-group';
    engineTitle.textContent = 'Движок расшифровки';
    fs.appendChild(engineTitle);

    const select = d.createElement('select');
    select.className = 'voice-engine-select';
    for (const item of ENGINES) {
        const opt = d.createElement('option');
        opt.value = item.key;
        opt.textContent = item.label;
        select.appendChild(opt);
    }
    // До ответа бэкенда выбирать нечего: показывать «Локально» на портале, где
    // на самом деле облако, — хуже, чем показать «загружается».
    select.disabled = true;
    fs.appendChild(select);

    const shared = d.createElement('div');
    shared.className = 'notify-hint voice-engine-shared';
    shared.textContent = SHARED_HINT;
    fs.appendChild(shared);

    const warn = d.createElement('div');
    warn.className = 'notify-hint voice-engine-warning';
    warn.style.display = 'none';
    fs.appendChild(warn);

    const applyState = (resp) => {
        const engine = resp && typeof resp.engine === 'string' ? resp.engine : '';
        if (ENGINES.some((e) => e.key === engine)) select.value = engine;
        const w = resp && resp.warning;
        if (w && w.message) {
            warn.style.display = '';
            warn.textContent = w.message;
        } else {
            warn.style.display = 'none';
            warn.textContent = '';
        }
    };

    let current = '';
    (async () => {
        try {
            const resp = await api.getVoiceSettings();
            applyState(resp);
            current = select.value;
            select.disabled = false;
        } catch (e) {
            // Роут недоступен (плагин не запущен, нет сети) — селект остаётся
            // выключенным, а причина видна рядом. Молча пустой селект человек
            // прочитал бы как «настройки нет».
            showError(e);
        }
    })();

    select.addEventListener('change', async () => {
        const desired = select.value;
        const previous = current;
        select.disabled = true;
        try {
            const resp = await api.setVoiceSettings(desired);
            applyState(resp);
            current = select.value;
            showOk();
        } catch (e) {
            // Откат: движок остался прежним, и селект обязан это показывать.
            if (previous) select.value = previous;
            showError(e);
        } finally {
            select.disabled = false;
        }
    });

    return fs;
}

/**
 * Блок «В каких чатах разбирать автоматически».
 *
 * Собран на общей форме выбора (`peer-picker.js`) — той же, устройство которой
 * трижды написано руками в формах видимости. Здесь она параметризуется
 * словарём площадки: у VK это ТИПЫ бесед, у Telegram — ПАПКИ аккаунта, у
 * Битрикс24 — ВИДЫ диалогов.
 *
 * Источники данных приходят СНАРУЖИ (`api.listScopeGroups` /
 * `api.listScopePeers`), а не собираются здесь: адреса и планировщики живут в
 * модулях панелей, и у VK список обязан идти ФОНОВЫМ приоритетом общего
 * лимитера (3 rps на весь клиент). Собранный внутри этого модуля запрос обошёл
 * бы и то, и другое.
 *
 * Сохранение — своей кнопкой, а не по каждой галочке: выбор собирается из
 * десятков отметок, и «сохранить» здесь осмысленный момент (в отличие от
 * чекбокса авторазбора, где щелчок и есть решение).
 *
 * @param {Document} d документ
 * @param {object} cfg конфиг панели (`*_VOICE_CONFIG`)
 * @param {?object} api сетевой слой панели
 * @param {object} s снимок user settings
 * @param {?function} onSaved обратный вызов с обновлёнными настройками
 * @returns {{node: object, setEnabled: function(boolean): void}}
 */
function buildScope(d, cfg, api, s, onSaved) {
    const sc = cfg.scope;
    const a = api || {};

    const box = d.createElement('div');
    box.className = 'voice-scope';

    const title = d.createElement('div');
    title.className = 'vk-visibility-group voice-scope-title';
    title.textContent = 'В каких чатах разбирать автоматически';
    box.appendChild(title);

    const hint = d.createElement('div');
    hint.className = 'notify-hint voice-scope-hint';
    hint.textContent = SCOPE_HINT;
    box.appendChild(hint);

    // ОТКУДА БЕРЁТСЯ СОХРАНЁННЫЙ ВЫБОР. По умолчанию — из двух плоских ключей
    // панели. Площадка, у которой настройки ПЕР-ПОРТАЛЬНЫЕ (Битрикс24: у
    // каждого портала свои `DIALOG_ID`), подставляет свой `scopeInitial` —
    // прочитать её карту прямо здесь было бы можно только протащив портал
    // сквозь общий на три панели модуль, который про порталы не знает вовсе.
    const initial = (typeof a.scopeInitial === 'function')
        ? (a.scopeInitial(s) || {})
        : { groups: s[sc.kindsKey], peers: s[sc.peersKey] };

    const picker = buildPeerPicker({
        doc: d,
        classPrefix: 'voice-scope',
        groupsTitle: sc.groupsTitle,
        peersTitle: sc.peersTitle,
        searchPlaceholder: sc.searchPlaceholder,
        searchAria: sc.searchAria,
        initialGroups: Array.isArray(initial.groups) ? initial.groups : [],
        initialPeers: Array.isArray(initial.peers) ? initial.peers : [],
        loadGroups: typeof a.listScopeGroups === 'function'
            ? () => a.listScopeGroups() : null,
        loadPeers: typeof a.listScopePeers === 'function'
            ? () => a.listScopePeers() : null,
        onChange: () => { summary.textContent = summaryText(); },
    });
    box.appendChild(picker.node);

    const summary = d.createElement('div');
    summary.className = 'notify-hint voice-scope-summary';
    box.appendChild(summary);

    const err = d.createElement('div');
    err.className = 'notify-error voice-scope-error';
    err.style.display = 'none';
    box.appendChild(err);

    const ok = d.createElement('div');
    ok.className = 'notify-hint voice-scope-save-ok';
    ok.textContent = 'Сохранено';
    ok.style.display = 'none';
    box.appendChild(ok);

    function summaryText() {
        const groups = picker.groups().length;
        const peers = picker.peers().length;
        if (!groups && !peers) return 'Выбора нет — разбираются все беседы.';
        const parts = [];
        if (groups) parts.push(`групп: ${groups}`);
        if (peers) parts.push(`бесед: ${peers}`);
        return `Разбираем только выбранное (${parts.join(', ')}).`;
    }
    summary.textContent = summaryText();

    const actions = d.createElement('div');
    actions.className = 'notify-actions voice-scope-actions';
    const saveBtn = d.createElement('button');
    saveBtn.type = 'button';
    saveBtn.className = 'primary voice-scope-save';
    saveBtn.textContent = 'Сохранить выбор';
    saveBtn.addEventListener('click', async () => {
        err.style.display = 'none';
        err.textContent = '';
        ok.style.display = 'none';
        saveBtn.disabled = true;
        // ТОЛЬКО два своих поля: PATCH применяет Some(..)-варианты, поэтому
        // соседние настройки (включая белый список ВИДИМОСТИ) не трогаются.
        // Списки уходят ЦЕЛИКОМ — снять галочку иначе нечем; уходят и
        // выбранные, но не загруженные id, иначе форма молча вычёркивала бы
        // беседы, до которых не дошёл список.
        //
        // Пер-портальная площадка собирает тело сама (`scopePayload`): её два
        // поля лежат не в корне настроек, а в записи ВЫБРАННОГО портала.
        const payload = (typeof a.scopePayload === 'function')
            ? a.scopePayload(picker.groups(), picker.peers())
            : {
                [sc.kindsKey]: picker.groups(),
                [sc.peersKey]: picker.peers(),
            };
        try {
            const updated = await updateUserSettings(payload);
            ok.style.display = '';
            if (typeof setTimeout === 'function') {
                setTimeout(() => { ok.style.display = 'none'; }, 2000);
            }
            if (typeof onSaved === 'function') onSaved(updated);
        } catch (e) {
            err.style.display = '';
            err.textContent = (e && e.message) ? e.message : 'Не удалось сохранить выбор.';
        } finally {
            saveBtn.disabled = false;
        }
    });
    actions.appendChild(saveBtn);
    box.appendChild(actions);

    return {
        node: box,
        setEnabled(v) {
            const on = v === true;
            picker.setDisabled(!on);
            saveBtn.disabled = !on;
            // add/remove, а не `toggle(cls, force)`: второй аргумент toggle
            // поддержан не везде, а состояние здесь строго двоичное.
            if (on) box.classList.remove('voice-scope-off');
            else box.classList.add('voice-scope-off');
        },
    };
}
