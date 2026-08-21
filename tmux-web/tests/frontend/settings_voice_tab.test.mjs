// tmux-web / тест секции «Голосовые сообщения» в трёх вкладках настроек.
//
// Проверяются ПЯТЬ обещаний секции (ГС-Ф4):
//
//   1. дефолт чекбокса — ВЫКЛЮЧЕН (фича opt-in, читается `=== true`);
//   2. переключение шлёт PATCH /api/user-settings со СВОИМ полем панели —
//      `vk_voice_autotranscribe` / `telegram_…` / `bitrix_…`, и ни с каким
//      другим: три панели — три решения человека;
//   3. отказ PATCH ОТКАТЫВАЕТ чекбокс и показывает причину (молча вернувшаяся
//      галочка читалась бы как поломка UI);
//   4. селект движка показывает значение, пришедшее с бэкенда, и печатает его
//      `warning`, если движок сейчас нерабочий;
//   5. отказ переключения движка возвращает селект на прежнее значение.
//
// Запуск: `node tmux-web/tests/frontend/settings_voice_tab.test.mjs`.
// Код возврата 0/1. Фейковый DOM — общий стаб `meet_popup_dom.mjs`.

import { makeEl, makeText, dispatch } from './meet_popup_dom.mjs';

// ── окружение ───────────────────────────────────────────────────────────────

const $document = makeEl('#document');
// У общего стаба нет `style` и `checked` — они нужны только формам настроек,
// а не карточкам встреч, ради которых он написан. Добавляем ЗДЕСЬ, чтобы не
// трогать чужой стаб и не менять поведение соседних тестов.
$document.createElement = (tag) => {
    const node = makeEl(tag);
    node.style = {};
    node.checked = false;
    return node;
};
$document.createTextNode = (t) => makeText(t);
globalThis.document = $document;
globalThis.window = { ForgeApp: {} };

/** Что уехало в сеть: сюда пишет мок `fetch` (его зовёт user-settings-api). */
const patches = [];
/** Чем отвечает `PATCH /api/user-settings`. */
let patchFails = false;

globalThis.fetch = async (url, init) => {
    let body = null;
    try { body = init && init.body ? JSON.parse(init.body) : null; } catch (_) { body = null; }
    patches.push({ url: String(url), method: (init && init.method) || 'GET', body });
    if (patchFails) {
        return { ok: false, status: 500, text: async () => 'бэкенд не отвечает' };
    }
    return { ok: true, status: 200, json: async () => Object.assign({ saved: true }, body) };
};

const V = await import(new URL('../../static/js/settings/voice-tab.js', import.meta.url).href);

// ── микро-фреймворк ─────────────────────────────────────────────────────────

let failures = 0;
let checks = 0;
function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    } else {
        console.log(`[ok]   ${label}`);
    }
}
function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    } else {
        console.log(`[ok]   ${label}`);
    }
}
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Мок сетевого слоя панели (роуты `…/voice/settings`). */
function makeApi(initial, opts) {
    const o = opts || {};
    const calls = [];
    return {
        calls,
        api: {
            async getVoiceSettings() {
                calls.push({ method: 'GET' });
                if (o.getFails) throw new Error('роут недоступен');
                return initial;
            },
            async setVoiceSettings(engine) {
                calls.push({ method: 'PUT', engine });
                if (o.putFails) throw new Error('в voice.toml опечатка');
                return { engine, cloud_key_set: true };
            },
        },
    };
}

const toggleOf = (fs, cls) => fs.querySelector(`.${cls}`);
const selectOf = (fs) => fs.querySelector('.voice-engine-select');
const errorOf = (fs) => fs.querySelector('.notify-error');
const warningOf = (fs) => fs.querySelector('.voice-engine-warning');

// ── 1. дефолт и поле панели ─────────────────────────────────────────────────

for (const [config, cls, key] of [
    [V.VK_VOICE_CONFIG, 'vk-voice-auto', 'vk_voice_autotranscribe'],
    [V.TELEGRAM_VOICE_CONFIG, 'tg-voice-auto', 'telegram_voice_autotranscribe'],
    [V.B24_VOICE_CONFIG, 'b24-voice-auto', 'bitrix_voice_autotranscribe'],
]) {
    patches.length = 0;
    const fs = V.buildVoiceForm({}, null, config, null, $document);
    const box = toggleOf(fs, cls);
    ok(!!box, `${key}: чекбокс отрисован`);
    eq(box.checked, false, `${key}: дефолт — выключено (нулевая конфигурация)`);

    // Браузер меняет `checked` САМ, до события; стаб — нет, поэтому щелчок
    // изображается двумя строками.
    box.checked = true;
    dispatch(box, 'change');
    // Обработчик асинхронный: даём ему добежать.
    // eslint-disable-next-line no-await-in-loop
    await flush();
    // eslint-disable-next-line no-await-in-loop
    await flush();

    eq(patches.length, 1, `${key}: щелчок отправил ровно один запрос`);
    eq(patches[0].method, 'PATCH', `${key}: метод PATCH`);
    eq(patches[0].url, '/api/user-settings', `${key}: адрес user-settings`);
    eq(patches[0].body, { [key]: true }, `${key}: в теле ТОЛЬКО своё поле панели`);
}

// Включённая настройка показывается включённой.
{
    const fs = V.buildVoiceForm(
        { vk_voice_autotranscribe: true }, null, V.VK_VOICE_CONFIG, null, $document,
    );
    eq(toggleOf(fs, 'vk-voice-auto').checked, true, 'сохранённое «включено» видно в чекбоксе');
}

// Строка, а не булево, включением НЕ считается (гейт `=== true`).
{
    const fs = V.buildVoiceForm(
        { vk_voice_autotranscribe: 'true' }, null, V.VK_VOICE_CONFIG, null, $document,
    );
    eq(toggleOf(fs, 'vk-voice-auto').checked, false, 'строка "true" не включает фичу');
}

// ── 2. откат чекбокса при отказе PATCH ──────────────────────────────────────

{
    patches.length = 0;
    patchFails = true;
    const fs = V.buildVoiceForm({}, null, V.VK_VOICE_CONFIG, null, $document);
    const box = toggleOf(fs, 'vk-voice-auto');
    box.checked = true;
    dispatch(box, 'change');
    await flush();
    await flush();

    eq(box.checked, false, 'отказ PATCH вернул чекбокс в исходное положение');
    eq(errorOf(fs).style.display, '', 'причина отказа показана');
    ok(String(errorOf(fs).textContent).length > 0, 'у отказа есть текст');
    eq(box.disabled, false, 'чекбокс снова доступен');
    patchFails = false;
}

// ── 3. селект движка: значение с бэкенда ────────────────────────────────────

{
    const { api, calls } = makeApi({ engine: 'local', cloud_key_set: false });
    const fs = V.buildVoiceForm({}, null, V.VK_VOICE_CONFIG, api, $document);
    await flush();
    await flush();

    const sel = selectOf(fs);
    ok(!!sel, 'селект движка отрисован');
    eq(calls.length, 1, 'значение движка спрошено у бэкенда один раз');
    eq(sel.value, 'local', 'селект показывает текущий движок с бэкенда');
    eq(sel.disabled, false, 'после ответа селект доступен');
    eq(warningOf(fs).style.display, 'none', 'исправной настройке предупреждение не нужно');
    ok(
        String(fs.textContent || '').includes('общий для всех мессенджеров'),
        'подпись про общую настройку на месте',
    );
}

// Предупреждение бэкенда показывается человеку.
{
    const { api } = makeApi({
        engine: 'cloud',
        cloud_key_set: false,
        warning: { code: 'voice_no_key', message: 'ключ polza.ai не задан' },
    });
    const fs = V.buildVoiceForm({}, null, V.B24_VOICE_CONFIG, api, $document);
    await flush();
    await flush();
    eq(warningOf(fs).style.display, '', 'предупреждение показано');
    eq(warningOf(fs).textContent, 'ключ polza.ai не задан', 'текст предупреждения — от бэкенда');
}

// Переключение движка уходит на бэкенд.
{
    const { api, calls } = makeApi({ engine: 'cloud', cloud_key_set: true });
    const fs = V.buildVoiceForm({}, null, V.TELEGRAM_VOICE_CONFIG, api, $document);
    await flush();
    await flush();

    const sel = selectOf(fs);
    sel.value = 'local';
    dispatch(sel, 'change');
    await flush();
    await flush();

    eq(calls.length, 2, 'GET при открытии и PUT при переключении');
    eq(calls[1], { method: 'PUT', engine: 'local' }, 'на бэкенд ушёл выбранный движок');
    eq(sel.value, 'local', 'селект показывает новое значение');
}

// Отказ переключения возвращает прежнее значение.
{
    const { api } = makeApi({ engine: 'cloud', cloud_key_set: true }, { putFails: true });
    const fs = V.buildVoiceForm({}, null, V.VK_VOICE_CONFIG, api, $document);
    await flush();
    await flush();

    const sel = selectOf(fs);
    sel.value = 'local';
    dispatch(sel, 'change');
    await flush();
    await flush();

    eq(sel.value, 'cloud', 'отказ PUT вернул селект на прежний движок');
    eq(errorOf(fs).style.display, '', 'причина отказа показана');
    eq(sel.disabled, false, 'селект снова доступен');
}

// Недоступный роут не рисует пустой селект молча.
{
    const { api } = makeApi({}, { getFails: true });
    const fs = V.buildVoiceForm({}, null, V.VK_VOICE_CONFIG, api, $document);
    await flush();
    await flush();
    eq(selectOf(fs).disabled, true, 'без ответа бэкенда движок переключать нечем');
    eq(errorOf(fs).style.display, '', 'причина недоступности показана');
}

// Без сетевого слоя секция всё равно строится — только с чекбоксом.
{
    const fs = V.buildVoiceForm({}, null, V.VK_VOICE_CONFIG, null, $document);
    ok(!!toggleOf(fs, 'vk-voice-auto'), 'чекбокс есть и без сетевого слоя');
    eq(selectOf(fs), null, 'селекта движка без сетевого слоя нет');
}

// ── итог ────────────────────────────────────────────────────────────────────

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED (frontend/settings_voice_tab)`);
    process.exit(1);
}
console.log(`\n${checks}/${checks} checks passed (frontend/settings_voice_tab)`);
