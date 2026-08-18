// ⚠ СТРАЖ ВКЛАДКИ НАСТРОЕК ВСТРЕЧ (Ф7 forge-meet, задача .3).
//
// Грузит РЕАЛЬНЫЙ модуль static/js/settings/meet-tab.js на фейковом документе.
// Сеть приходит инъекцией, поэтому ни браузера, ни сервера не нужно — и заодно
// доказано, что вкладка не ходит никуда, кроме переданных ей функций.
//
// ЗАЧЕМ ЭТОТ ТЕСТ. Вкладка держит пять обещаний, и каждое ломается тихо:
//
//   1. БЕЙДЖ УСТРОЙСТВА. `gpu_metal: false` — не «медленнее», а «расшифровки
//      не будет»: пайплайн убивает прогон, не доказавший Metal. Пропади
//      предупреждение — человек крутил бы пороги VAD, не понимая, почему
//      расшифровка падает.
//   2. ПОЛЕ ИЗ ОКРУЖЕНИЯ НЕ РЕДАКТИРУЕТСЯ. Переменная сильнее файла;
//      редактируемое поле означало бы «сохранил, а работает старое».
//   3. СОХРАНЯЮТСЯ ТОЛЬКО ИЗМЕНЁННЫЕ поля. Отправка всей формы затирала бы
//      чужие правки файла, сделанные между открытием вкладки и нажатием.
//   4. ПОСЛЕ СОХРАНЕНИЯ СКАЗАНО, ЧТО НУЖЕН ПЕРЕЗАПУСК. Молчание здесь — ложь:
//      конфиг плагина неизменяем в работающем процессе.
//   5. МОДЕЛЬ КАЧАЕТСЯ ТОЛЬКО ПО НАЖАТИЮ. Полтора гигабайта не уезжают при
//      открытии вкладки.
//
// Запуск: node tmux-web/tests/frontend/settings_meet_tab.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { makeEl, makeDoc, dispatch } from './meet_popup_dom.mjs';

let failures = 0;
let checks = 0;

function ok(cond, label, detail) {
    checks += 1;
    if (cond) return;
    failures += 1;
    console.error(`[FAIL] ${label}${detail ? `\n       ${detail}` : ''}`);
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) return;
    failures += 1;
    console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
}

// `makeDoc` возвращает конверт `{ doc, hosts, … }`, а не сам документ:
// глобальным обязан стать именно `doc`, иначе `document.createElement` не
// существует и вкладка падает на первой строке.
const DOM = makeDoc([]);
globalThis.document = DOM.doc;
globalThis.window = globalThis;

const TAB = await import(new URL('../../static/js/settings/meet-tab.js', import.meta.url).href);

// ---------------------------------------------------------------------------
// Ответы сервера — той же формы, что отдают роуты Ф7.
// ---------------------------------------------------------------------------

function field(key, kind, group, value, extra) {
    return Object.assign({
        key, kind, group, value,
        env: `FORGE_MEET_${key.toUpperCase()}`,
        choices: [],
        min: null,
        max: null,
        source: 'default',
        env_locked: false,
    }, extra || {});
}

function config(over) {
    return Object.assign({
        config_path: '/tmp/meet.toml',
        file_exists: true,
        file_parses: true,
        restart_required: true,
        readonly: {
            cache_dir: '/tmp/cache',
            db_path: '/tmp/cache/meet.db',
            models_dir: '/tmp/cache/models',
            max_concurrent_whisper: 1,
            audio_sample_rate: 16000,
            audio_codec: 'pcm16',
            audio_transport: 'cdp_binding',
        },
        fields: [
            field('bot_name', 'text', 'bot', 'F.O.R.G.E.'),
            field('window_mode', 'choice', 'bot', 'minimized', {
                choices: ['offscreen', 'minimized', 'visible', 'headless'],
            }),
            field('mute_audio', 'bool', 'bot', true),
            field('whisper_model', 'text', 'transcribe', 'large-v3-turbo-q5_0'),
            field('summary_enabled', 'bool', 'summary', true),
            field('alone_timeout_secs', 'int', 'timeouts', 60, { min: 5, max: 3600 }),
            field('vad_on_db', 'float', 'vad', 12, { min: 1, max: 60 }),
            field('max_concurrent_recordings', 'int', 'limits', 2, { min: 1, max: 8 }),
            field('retention_days', 'int', 'storage', 90, { min: 0, max: 3650 }),
        ],
    }, over || {});
}

function status(over) {
    return Object.assign({
        enabled: true,
        deps: Object.assign({
            probed: true,
            gpu_metal: true,
            ffmpeg: { name: 'ffmpeg', found: true, path: '/opt/ffmpeg', version: '7.1' },
            ffprobe: { name: 'ffprobe', found: true, path: '/opt/ffprobe', version: '7.1' },
            whisper: { name: 'whisper-cli', found: true, path: '/opt/whisper-cli', version: '1.9.2' },
            chrome: { name: 'chrome', found: true, path: '/Applications/Chrome', version: '151' },
        }, (over && over.deps) || {}),
    }, over || {});
}

function modelsDto(over) {
    return Object.assign({
        models_dir: '/tmp/cache/models',
        selected: 'large-v3-turbo-q5_0',
        downloading: [],
        models: [
            {
                name: 'large-v3-turbo-q5_0', file: 'ggml.bin', kind: 'transcribe',
                size_bytes: 574_000_000, on_disk_bytes: 574_000_000,
                downloaded: true, selected: true, known: true, note: '',
            },
            {
                name: 'large-v3', file: 'ggml-large.bin', kind: 'transcribe',
                size_bytes: 1_620_000_000, on_disk_bytes: null,
                downloaded: false, selected: false, known: true, note: '',
            },
        ],
    }, over || {});
}

/** Собирает набор инъекций с журналом вызовов. */
function makeApi(over) {
    const calls = [];
    const api = {
        getMeetConfig: async () => { calls.push(['getMeetConfig']); return config(); },
        getMeetStatus: async () => { calls.push(['getMeetStatus']); return status(); },
        getModels: async () => { calls.push(['getModels']); return modelsDto(); },
        putMeetConfig: async (p) => {
            calls.push(['putMeetConfig', p]);
            return { saved: Object.keys(p.values), env_locked: [], fixes: [], restart_required: true };
        },
        downloadModel: async (n) => { calls.push(['downloadModel', n]); return { started: true }; },
        recheckDeps: async () => { calls.push(['recheckDeps']); return status().deps; },
    };
    return { api: Object.assign(api, over || {}), calls };
}

/** Все узлы поддерева (обход руками: у стаба нет getElementsByTagName). */
function walk(node, out = []) {
    out.push(node);
    for (const c of node.childNodes || []) walk(c, out);
    return out;
}

function textOf(node) {
    return walk(node).map((n) => (n.nodeType === 3 ? n.textContent : '')).join('')
        + walk(node).filter((n) => n.nodeType === 1 && n.childNodes.length === 0)
            .map((n) => n.textContent || '').join(' ');
}

function findByField(refs, key) {
    return refs.fields.get(key);
}

// ===========================================================================
// 1. ЧИСТЫЕ ФУНКЦИИ: бейдж устройства и строки зависимостей
// ===========================================================================

{
    eq(TAB.deviceBadge({ probed: true, gpu_metal: true }).tone, 'ok', 'GPU — спокойный тон');
    const cpu = TAB.deviceBadge({ probed: true, gpu_metal: false });
    eq(cpu.tone, 'warn', 'CPU — ПРЕДУПРЕЖДЕНИЕ, а не «просто медленнее»');
    ok(/не запустится/i.test(cpu.text), 'текст про CPU обязан говорить, что расшифровки НЕ БУДЕТ', cpu.text);
    ok(/GPU|Metal/.test(cpu.text), 'и называть правило проекта', cpu.text);
    eq(TAB.deviceBadge({ probed: false }).tone, 'wait',
        'до проб — «проверяем», а не «ничего нет»: разница между «не нашли» и «не смотрели»');
    eq(TAB.deviceBadge(null).tone, 'wait', 'отсутствующий отчёт не выдумывает GPU');

    const missing = TAB.depLine('whisper', { found: false }, TAB.DEP_HINTS.whisper);
    eq(missing.tone, 'warn', 'отсутствующий бинарь — предупреждение');
    ok(/brew install whisper-cpp/.test(missing.text), 'и текст говорит, ЧТО ставить', missing.text);
    eq(TAB.depLine('ffmpeg', { found: null }, '').tone, 'wait', '«ещё не проверяли» отличается от «не нашли»');

    eq(TAB.formatBytes(574_000_000), '547 МБ', 'вес модели читается человеком');
    eq(TAB.formatBytes(0), '—', 'нулевой вес не притворяется числом');
}

// ===========================================================================
// 2. СБОРКА ВКЛАДКИ
// ===========================================================================

{
    const { api, calls } = makeApi();
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);

    eq(calls.map((c) => c[0]).sort(), ['getMeetConfig', 'getMeetStatus', 'getModels'],
        'при открытии вкладки идут ровно три запроса — и ни одного скачивания');
    ok(!calls.some((c) => c[0] === 'downloadModel'),
        'ПОЛТОРА ГИГАБАЙТА НЕ КАЧАЮТСЯ ПРИ ОТКРЫТИИ ВКЛАДКИ');

    // Разделы на месте и в объявленном порядке.
    const groups = walk(host).filter((n) => n.dataset && n.dataset.meetGroup)
        .map((n) => n.dataset.meetGroup);
    eq(groups, ['bot', 'transcribe', 'summary', 'timeouts', 'storage', 'limits', 'vad', 'readonly'],
        'разделы идут в порядке MEET_GROUPS, «для продвинутых» — в конце');

    // Пороги VAD и лимиты помечены как продвинутые.
    const advanced = walk(host).filter((n) => n.dataset && n.dataset.meetAdvanced === '1')
        .map((n) => n.dataset.meetGroup);
    eq(advanced, ['limits', 'vad'], 'VAD и лимиты — в разделе для продвинутых');

    // Поля построены нужными типами.
    eq(findByField(refs, 'bot_name').tagName, 'input', 'имя бота — текстовое поле');
    eq(findByField(refs, 'window_mode').tagName, 'select', 'режим окна — выбор из списка');
    eq(findByField(refs, 'window_mode').value, 'minimized', 'выбрано текущее значение');
    eq(findByField(refs, 'summary_enabled').checked, true, 'булево поле — галка');
    eq(findByField(refs, 'alone_timeout_secs').value, '60', 'число подставлено');
    eq(findByField(refs, 'alone_timeout_secs').min, '5', 'граница уехала в поле');

    // Бейдж устройства нарисован ПЕРВЫМ и спокоен при GPU.
    eq(refs.device.dataset.meetTone, 'ok', 'при Metal вкладка не пугает');

    // Модели: скачанная и нескачанная различимы, у второй есть кнопка.
    const rows = walk(host).filter((n) => n.dataset && n.dataset.meetModel);
    eq(rows.map((r) => r.dataset.meetModel), ['large-v3-turbo-q5_0', 'large-v3'],
        'обе модели показаны');
    ok(!refs.fields.has('model:large-v3-turbo-q5_0'),
        'у скачанной модели кнопки «скачать» нет');
    ok(refs.fields.has('model:large-v3'), 'у нескачанной — есть');
    const state = rows[1].childNodes.find((c) => c.className === 'meet-model-state');
    ok(/не скачана/.test(state.textContent) && /ГБ|МБ/.test(state.textContent),
        'честный статус модели: скачана или нет и сколько весит', state.textContent);
}

// ===========================================================================
// 3. CPU — ЖЁЛТОЕ ПРЕДУПРЕЖДЕНИЕ ПРЯМО ВО ВКЛАДКЕ
// ===========================================================================

{
    const { api } = makeApi({
        getMeetStatus: async () => status({ deps: Object.assign(status().deps, { gpu_metal: false }) }),
    });
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);
    eq(refs.device.dataset.meetTone, 'warn', 'CPU обязан быть виден предупреждением');
    eq(refs.device.className, 'notify-error', 'и оформлен как ошибка, а не как подсказка');
}

// ===========================================================================
// 4. ЗНАЧЕНИЕ ИЗ ОКРУЖЕНИЯ — ТОЛЬКО ДЛЯ ЧТЕНИЯ
// ===========================================================================

{
    const cfg = config();
    cfg.fields[0] = field('bot_name', 'text', 'bot', 'Из окружения', {
        source: 'env', env_locked: true,
    });
    const { api, calls } = makeApi({ getMeetConfig: async () => cfg });
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);

    const input = findByField(refs, 'bot_name');
    eq(input.disabled, true, 'поле из окружения заблокировано');
    const wrap = walk(host).find((n) => n.dataset && n.dataset.meetField === 'bot_name');
    ok(/FORGE_MEET_BOT_NAME/.test(textOf(wrap)),
        'и рядом сказано, КАКАЯ переменная его держит', textOf(wrap));

    // Даже если значение поменять силой, оно не уедет на сервер.
    input.value = 'Подмена';
    findByField(refs, 'retention_days').value = '30';
    dispatch(refs.save, 'click');
    await new Promise((r) => setTimeout(r, 0));
    const put = calls.find((c) => c[0] === 'putMeetConfig');
    eq(put[1], { values: { retention_days: 30 } },
        'заблокированное поле не попадает в запрос ни при каких условиях');
}

// ===========================================================================
// 5. СОХРАНЯЮТСЯ ТОЛЬКО ИЗМЕНЁННЫЕ ПОЛЯ, И ПРО ПЕРЕЗАПУСК СКАЗАНО ПРЯМО
// ===========================================================================

{
    const { api, calls } = makeApi();
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);

    // Ничего не трогали — сохранять нечего, и запрос не уходит.
    dispatch(refs.save, 'click');
    await new Promise((r) => setTimeout(r, 0));
    ok(!calls.some((c) => c[0] === 'putMeetConfig'),
        'форма без правок не шлёт запроса вовсе');
    ok(/не изменилось/.test(refs.note.textContent), refs.note.textContent);

    // Меняем два поля из девяти.
    findByField(refs, 'bot_name').value = 'Секретарь';
    findByField(refs, 'summary_enabled').checked = false;
    dispatch(refs.save, 'click');
    await new Promise((r) => setTimeout(r, 0));

    const put = calls.find((c) => c[0] === 'putMeetConfig');
    eq(put[1], { values: { bot_name: 'Секретарь', summary_enabled: false } },
        'уезжают ТОЛЬКО изменённые поля — иначе форма затирала бы чужие правки файла');
    ok(/следующем запуске/.test(refs.note.textContent),
        'после сохранения обязано быть сказано, что нужен перезапуск', refs.note.textContent);
}

// ===========================================================================
// 6. МОДЕЛЬ КАЧАЕТСЯ ТОЛЬКО ПО ЯВНОМУ НАЖАТИЮ
// ===========================================================================

{
    const { api, calls } = makeApi();
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);
    const btn = refs.fields.get('model:large-v3');

    eq(calls.filter((c) => c[0] === 'downloadModel').length, 0, 'до нажатия — ноль загрузок');
    dispatch(btn, 'click');
    await new Promise((r) => setTimeout(r, 0));
    eq(calls.filter((c) => c[0] === 'downloadModel').map((c) => c[1]), ['large-v3'],
        'нажатие даёт ровно одну загрузку названной модели');
    eq(btn.disabled, true, 'повторное нажатие невозможно, пока идёт загрузка');
}

// ===========================================================================
// 7. ПЕРЕПРОВЕРКА ЗАВИСИМОСТЕЙ
// ===========================================================================

{
    const bad = status({ deps: Object.assign(status().deps, {
        gpu_metal: false,
        whisper: { name: 'whisper-cli', found: false, path: null, version: null },
    }) });
    let current = bad;
    const { api, calls } = makeApi({
        getMeetStatus: async () => current,
        // Подмена ОБЯЗАНА журналировать сама: `makeApi` заменяет функцию
        // целиком, и молчаливая подмена превратила бы счётчик вызовов ниже в
        // проверку пустоты.
        recheckDeps: async () => { calls.push(['recheckDeps']); current = status(); return current.deps; },
    });
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);

    ok(/brew install whisper-cpp/.test(textOf(refs.deps)),
        'пока whisper не найден — сказано, что ставить', textOf(refs.deps));
    eq(refs.device.dataset.meetTone, 'warn', 'и устройство предупреждает');

    dispatch(refs.recheck, 'click');
    await new Promise((r) => setTimeout(r, 0));
    eq(calls.filter((c) => c[0] === 'recheckDeps').length, 1, 'перепроверка — один запрос');
    ok(!/brew install whisper-cpp/.test(textOf(refs.deps)),
        'после успешной перепроверки подсказка ушла', textOf(refs.deps));
    eq(refs.device.dataset.meetTone, 'ok', 'и бейдж устройства обновился');
}

// ===========================================================================
// 8. ОШИБКА ЗАГРУЗКИ НЕ ПОКАЗЫВАЕТ ФОРМУ С УМОЛЧАНИЯМИ
// ===========================================================================

{
    const { api, calls } = makeApi({
        getMeetConfig: async () => { throw new Error('HTTP 503'); },
    });
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);
    ok(refs.error, 'при провале загрузки показывается ошибка, а не форма');
    ok(!refs.save, 'кнопки «Сохранить» нет — она затёрла бы конфиг умолчаниями');
    ok(/503/.test(textOf(host)), 'причина названа', textOf(host));
    ok(!calls.some((c) => c[0] === 'putMeetConfig'), 'и ничего не сохраняется');
}

// ===========================================================================
// 9. БИТЫЙ ФАЙЛ КОНФИГА БЛОКИРУЕТ СОХРАНЕНИЕ
// ===========================================================================

{
    const { api } = makeApi({
        getMeetConfig: async () => config({ file_parses: false }),
    });
    const host = makeEl('div');
    const refs = await TAB.buildMeetSettings(host, api);
    eq(refs.save.disabled, true,
        'сохранение поверх неразбираемого файла стёрло бы остальные настройки');
    ok(/не разбирается/.test(textOf(host)), 'и причина сказана', textOf(host));
}

// ===========================================================================
// 10. ⚠ ВКЛАДКА РЕАЛЬНО ЗАРЕГИСТРИРОВАНА В МОДАЛКЕ
// ===========================================================================
//
// Форма может быть безупречной и при этом недостижимой: `settings/modal.js`
// хардкодит каждую вкладку в ШЕСТИ местах (кнопка, панель, селектор контента,
// объект состояния, ветка в showTab, whitelist initialTab), и файл сам об этом
// предупреждает в своей шапке. Забудь одно из шести — вкладка не откроется, а
// ни один тест выше этого не заметит: он работает с модулем напрямую.
//
// Проверка текстовая по исходнику: поднимать модалку целиком значит тянуть
// два десятка её импортов ради шести строк.

{
    const { readFileSync } = await import('node:fs');
    const modal = readFileSync(new URL('../../static/js/settings/modal.js', import.meta.url), 'utf8');

    ok(/import \{ buildMeetSettings \} from '\.\/meet-tab\.js'/.test(modal),
        '1/6: модалка импортирует форму вкладки');
    ok(/data-tab="meet"/.test(modal), '2/6: кнопка вкладки');
    ok(/data-panel="meet"/.test(modal) && /id="ps-panel-meet"/.test(modal), '3/6: панель вкладки');
    ok(/querySelector\('#ps-meet-content'\)/.test(modal), '4/6: селектор контента');
    ok(/meetTabState\s*=\s*\{/.test(modal), '5/6: объект состояния (ленивый рендер)');
    ok(/name === 'meet'/.test(modal) && /renderMeetPanel\(\)/.test(modal), '6/6: ветка в showTab');
    ok(/initialTab === 'meet'/.test(modal), '6/6+: whitelist initialTab');

    // КНОПКА УСЛОВНАЯ — по образцу remoteTabBtn. Рисовать вкладку настроек
    // плагина, которого нет в запуске, значит показывать настройки, которые
    // никуда не применятся.
    ok(/const meetTabBtn = meetEnabled\(\)/.test(modal),
        'кнопка вкладки рисуется условно, через гейт meetEnabled()');
    ok(/const remoteTabBtn = isRemoteMode\(\)/.test(modal),
        'образец (remoteTabBtn) на месте — иначе проверка выше сравнивала бы с несуществующим');
    ok(/initialTab === 'meet' && meetEnabled\(\)/.test(modal),
        'и через whitelist вкладку тоже нельзя открыть при выключенном плагине');

    // Форма ходит в сеть ТОЛЬКО инъекцией: своих fetch у неё нет.
    const tabSrc = readFileSync(new URL('../../static/js/settings/meet-tab.js', import.meta.url), 'utf8');
    const code = tabSrc.split('\n').filter((l) => !l.trimStart().startsWith('//')).join('\n');
    ok(!/\bfetch\s*\(/.test(code), 'вкладка не зовёт fetch напрямую');
    ok(!/innerHTML/.test(code), 'разметка строится узлами: значения приходят с сервера');
}

// ===========================================================================

if (failures === 0) console.log(`[OK] settings_meet_tab: ${checks} проверок`);
else console.error(`[FAIL] settings_meet_tab: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
