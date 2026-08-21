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

import { updateUserSettings } from './user-settings-api.js';

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
    hint:
        'Новое входящее голосовое расшифровывается само, текст появляется под волной. '
        + 'Беседы, скрытые белым списком выше, не разбираются; свои исходящие — тоже.',
};

/** Конфиг секции для панели Telegram. */
export const TELEGRAM_VOICE_CONFIG = {
    key: 'telegram',
    enabledKey: 'telegram_voice_autotranscribe',
    toggleClass: 'tg-voice-auto',
    hint:
        'Новое входящее голосовое расшифровывается само, текст появляется под волной. '
        + 'Разбираются только голосовые: пересланная музыка и подкасты — по кнопке.',
};

/** Конфиг секции для панели Битрикс24. */
export const B24_VOICE_CONFIG = {
    key: 'bitrix',
    enabledKey: 'bitrix_voice_autotranscribe',
    toggleClass: 'b24-voice-auto',
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

    box.addEventListener('change', async () => {
        const desired = !!box.checked;
        box.disabled = true;
        try {
            const updated = await updateUserSettings({ [cfg.enabledKey]: desired });
            showOk();
            if (typeof onSaved === 'function') onSaved(updated);
        } catch (e) {
            // Откат: `updateUserSettings` уже вернул кеш на место, а мы
            // возвращаем сам чекбокс — иначе UI утверждал бы, что фича
            // включена, а бэкенд об этом не знал бы.
            box.checked = !desired;
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
