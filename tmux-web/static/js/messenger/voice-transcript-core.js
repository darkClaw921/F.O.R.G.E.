// Текст расшифровки под голосовым сообщением — ОДИН модуль на три панели.
//
// Появился в Ф5 фичи голосовых. К этому моменту всё, что ниже кнопки, уже
// существует трижды и одинаково: роуты `…/voice/<peer>/<mid>/<idx>/transcribe`
// и `…/transcript`, четвёрка кадров `voice_transcribe_*` с ОДИНАКОВЫМ набором
// полей (страж — `tests/frontend/voice_frames_contract.test.mjs`) и уже готовые
// обёртки `transcribeVoice` / `getVoiceTranscript` в `api.js` каждой панели.
// Расходиться тут нечему — расходиться может только карточка, если её ветку
// написать в трёх местах. Поэтому вся логика живёт здесь, а панель отдаёт
// ровно три вещи: свой транспорт, имя своего DOM-события с кадрами и свой
// префикс классов.
//
// ============================================================================
// ЧЕТЫРЕ СОСТОЯНИЯ, И ПЯТОГО НЕТ
// ============================================================================
//
//   idle    — расшифровки нет. Видна кнопка «Расшифровать».
//   running — идёт прогон. Кнопка заблокирована, полоса процентов живёт
//             кадрами `voice_transcribe_delta`.
//   ready   — текст готов. Длинный свёрнут кнопкой «Показать полностью».
//   error   — отказ. Показывается ЧЕЛОВЕЧЕСКАЯ причина по коду и «Повторить».
//
// Отдельный флажок `fetching` — это «GET готового текста в полёте», и он
// НИЧЕГО не показывает и ничего не запрещает: чтение файла с диска занимает
// миллисекунды, а погашенная на это время кнопка — это кнопка, которая не
// нажалась. Нажатие посреди такого чтения просто выигрывает: `running`
// ставится сразу, а приехавший следом ответ применяется только к `idle`.
//
// Запуск же (`POST`) отдельного флажка не требует вовсе: состояние `running`
// выставляется ДО запроса, и второе нажатие упирается в него.
//
// ============================================================================
// СОСТОЯНИЕ ЖИВЁТ В МОДУЛЕ, А НЕ В УЗЛАХ
// ============================================================================
//
// Лента перерисовывается целиком (скролл, смена беседы, `resync` сокета), и
// узлы карточек при этом создаются заново. Если бы текст расшифровки хранился
// только в DOM, он исчезал бы при каждой перерисовке — а расшифровка стоит
// секунд GPU либо денег в облаке. Поэтому источник истины — `Map` по ключу
// `plugin/dialog/message/index`, а узлы лишь ОТОБРАЖАЮТ её. Отсюда же следует
// главное свойство: перерисовка посреди прогона не начинает второй прогон, она
// просто рисует то же самое `running` в новом узле.
//
// Один ключ может быть виден в НЕСКОЛЬКИХ узлах одновременно (одно и то же
// сообщение в ленте и в предпросмотре ответа), поэтому на ключ хранится СПИСОК
// представлений — в порядке появления.
//
// ⚠ ОТЦЕПЛЕННЫЙ УЗЕЛ — НЕ ПРИЗНАК МУСОРА. Первая версия чистила список по
// `root.isConnected === false` прямо в рендере, и это ломало карточку целиком:
// панель зовёт `attach` при СБОРКЕ карточки, а в документ и сама карточка, и
// вся строка ленты попадают позже. Свежее представление в этот момент всегда
// отцеплено — то есть выбрасывалось первым же кадром, и на экране навсегда
// оставалось «Расшифровать», сколько бы кадров ни пришло. Ни один тест на
// фейковом документе этого не видел: `isConnected` там просто нет. Поймала
// живая проба в headless Chrome.
//
// Поэтому правило другое: ПОСЛЕДНЕЕ представление ключа не выбрасывается
// никогда, а предыдущие — только когда они отцеплены (лента перерисовалась).
//
// ============================================================================
// СОТНЯ ГОЛОСОВЫХ НЕ ДАЁТ СОТНИ ЗАПРОСОВ
// ============================================================================
//
// «Готовый текст восстанавливается сам» и «лента не бомбит сервер» —
// требования встречные, и сходятся они на лени:
//
//  1. ключ, про который уже всё известно (`ready`, `running`, `error` или
//     просто уже спрошенный), не спрашивается ВТОРОЙ раз — сколько бы раз
//     карточка ни перерисовалась;
//  2. пока карточки не видно, её и не спрашивают: если в окружении есть
//     `IntersectionObserver`, запрос уходит при появлении карточки в поле
//     зрения;
//  3. там, где наблюдателя нет (старый браузер, фейковый документ тестов),
//     работает жёсткий потолок [`EAGER_FETCH_BUDGET`] запросов на пачку
//     отрисовки. Остальные карточки остаются в `idle` — то есть с рабочей
//     кнопкой, а не со сломанным видом.
//
// Автоматически расшифровка НЕ запускается никогда: GET читает уже посчитанное
// из файла на диске, POST делает только нажатие человека. За автоматический
// разбор входящих отвечает бэкенд (Ф4) и отдельный тумблер настроек.
//
// ============================================================================
// ИНВАРИАНТ XSS
// ============================================================================
//
// Тексты (расшифровка, причина отказа, имя движка) попадают в DOM ТОЛЬКО через
// textContent. Ни одной записи разметки строкой в модуле нет — это проверяется
// тестом ПО ИСХОДНИКУ, поэтому запретное свойство не упоминается здесь даже в
// комментарии. Расшифровка — это дословная речь собеседника, то есть чужой
// текст ровно в том же смысле, что и тело сообщения.

/**
 * Сколько символов текста показываем несвёрнутым.
 *
 * Пятнадцатиминутное голосовое — это лист текста, и развёрнутым он выдавливает
 * из ленты всё остальное. Порог выбран так, чтобы обычное «ага, буду в семь»
 * не сворачивалось никогда.
 */
export const COLLAPSE_CHARS = 320;

/**
 * Потолок запросов готового текста на одну пачку отрисовки — там, где нет
 * `IntersectionObserver`.
 */
export const EAGER_FETCH_BUDGET = 12;

/** Виды вложений, у которых бывает расшифровка (лексика трёх панелей). */
export const AUDIO_KINDS = ['voice', 'audio', 'voice_message', 'audio_message'];

/**
 * Человеческие причины отказа ПО КОДУ.
 *
 * Тексты кадра `voice_transcribe_error` сюда не попадают вовсе: в них хвост
 * stderr движка, пути до бинарей и обрывки конфигурации. Человеку в ленте это
 * не нужно, а показывать пути и тем более следы ключей — прямой вред. Коды —
 * контракт `forge_voice::VoiceError::code`.
 */
export const ERROR_TEXT = {
    voice_not_audio: 'Это вложение не звук — расшифровывать нечего.',
    voice_too_long: 'Запись слишком длинная для расшифровки.',
    voice_no_ffmpeg: 'Не установлен ffmpeg — звук нечем подготовить.',
    voice_no_whisper: 'Локальный движок расшифровки не установлен.',
    voice_no_model: 'Модель локального движка не загружена.',
    voice_no_key: 'Не введён ключ облака расшифровки.',
    voice_busy: 'Сейчас идёт другая расшифровка — попробуйте через минуту.',
    voice_config: 'Настройки расшифровки не читаются.',
    metal_unavailable: 'Локальному движку нужен GPU — включите облако в настройках.',
    not_authorized: 'Панель не подключена.',
    not_connected: 'Нет связи с сервером мессенджера.',
    no_media: 'Файл голосового не удалось получить.',
};

/** Причина отказа, когда код неизвестен или его нет вовсе. */
export const ERROR_FALLBACK = 'Расшифровать не удалось.';

/** Подписи движков — те же два слова, что во вкладке настроек. */
const ENGINE_LABEL = { local: 'локально', cloud: 'облако' };

/**
 * Ключ вложения.
 *
 * Плагин входит в ключ первым: две панели на одной странице легко дают
 * одинаковую тройку (беседа «1», сообщение «1», вложение 0), и без имени
 * плагина кадр Telegram дорисовал бы карточку VK.
 *
 * @param {string} plugin
 * @param {string|number} dialogId
 * @param {string|number} messageId
 * @param {number|string} index
 * @returns {string}
 */
export function transcriptKey(plugin, dialogId, messageId, index) {
    const idx = Number(index);
    return `${plugin}/${dialogId}/${messageId}/${Number.isFinite(idx) ? idx : 0}`;
}

/** Бывает ли у такого вложения расшифровка. */
export function isAudioKind(kind) {
    return AUDIO_KINDS.includes(String(kind || ''));
}

/** Надо ли сворачивать этот текст. */
export function needsCollapse(text) {
    return String(text || '').length > COLLAPSE_CHARS;
}

/** Человеческая причина отказа по коду. */
export function errorText(code) {
    const key = String(code || '');
    return Object.prototype.hasOwnProperty.call(ERROR_TEXT, key) ? ERROR_TEXT[key] : ERROR_FALLBACK;
}

/** Подпись строки состояния для `running`. */
function runningLabel(state) {
    const engine = ENGINE_LABEL[String(state.engine || '')];
    const head = engine ? `Расшифровка (${engine})` : 'Расшифровка';
    return Number.isFinite(state.percent) ? `${head}… ${state.percent} %` : `${head}…`;
}

/** Подпись строки состояния для `ready`. */
function readyLabel(state) {
    const engine = ENGINE_LABEL[String(state.engine || '')];
    const lang = String(state.language || '').trim();
    const parts = [];
    if (lang) parts.push(lang);
    if (engine) parts.push(engine);
    return parts.length ? `Расшифровано (${parts.join(', ')})` : 'Расшифровано';
}

/** Пустое состояние ключа. */
function blankState() {
    return {
        status: 'idle',
        fetching: false,
        fetched: false,
        text: '',
        percent: null,
        engine: '',
        language: '',
        durationMs: 0,
        code: '',
        runId: null,
        expanded: false,
    };
}

/**
 * Ставит/снимает класс без `classList`.
 *
 * `classList` есть не во всех фейковых документах панелей (карточки Bitrix
 * строятся на документе из десяти строк), а расходиться с ними модулю нельзя:
 * тогда одна из трёх панелей осталась бы без теста разметки.
 */
function setClass(node, cls, on) {
    const have = String(node.className || '').split(' ').filter(Boolean);
    const has = have.includes(cls);
    if (on === has) return;
    if (on) have.push(cls);
    node.className = (on ? have : have.filter((c) => c !== cls)).join(' ');
}

/** `setAttribute`, если документ его умеет. */
function attr(node, name, value) {
    if (typeof node.setAttribute === 'function') node.setAttribute(name, String(value));
}

/**
 * Фабрика карточки расшифровки для одной панели.
 *
 * Всё состояние — в замыкании: две панели на одной странице обязаны быть слепы
 * друг к другу (тот же довод, что у `createAnalysisPanel`).
 *
 * @param {object} opts
 * @param {string} opts.plugin — имя панели, первый сегмент ключа
 * @param {string} opts.classPrefix — префикс классов (`vk-voice`, `tg-voice`, …)
 * @param {string} [opts.frameEvent] — имя DOM-события с кадрами расшифровки
 * @param {object} [opts.api] — `{transcribe, transcript}`
 * @returns {object}
 */
export function createVoiceTranscript(opts) {
    const cfg = {
        plugin: String((opts && opts.plugin) || 'msg'),
        classPrefix: String((opts && opts.classPrefix) || 'msg-voice'),
        frameEvent: (opts && opts.frameEvent) || null,
        api: (opts && opts.api) || null,
        eagerBudget: EAGER_FETCH_BUDGET,
        observe: true,
    };

    const states = new Map();
    /** ключ → СПИСОК представлений в порядке появления (см. шапку модуля). */
    const views = new Map();
    /** Сколько «слепых» запросов уже потрачено в текущей пачке отрисовки. */
    let eagerSpent = 0;
    let eagerTimer = null;
    let listening = false;
    let observer = null;
    const stats = { fetches: 0, starts: 0, frames: 0 };

    const cls = (suffix) => (suffix ? `${cfg.classPrefix}-${suffix}` : cfg.classPrefix);

    function stateOf(key) {
        let st = states.get(key);
        if (!st) {
            st = blankState();
            states.set(key, st);
        }
        return st;
    }

    // -----------------------------------------------------------------------
    // Рендер
    // -----------------------------------------------------------------------

    function renderView(view) {
        const st = stateOf(view.key);
        const { root, status, progress, bar, textEl, runBtn, toggleBtn } = view;

        // Состояние — В `dataset`, а не отдельным классом: CSS панели цепляется
        // за `[data-voice-state="…"]`, и одно место истины избавляет от пары
        // «класс поставили, а снять забыли».
        root.dataset.voiceState = st.status;
        attr(root, 'aria-busy', st.status === 'running' ? 'true' : 'false');

        // --- строка состояния
        if (st.status === 'running') status.textContent = runningLabel(st);
        else if (st.status === 'ready') status.textContent = readyLabel(st);
        else if (st.status === 'error') status.textContent = errorText(st.code);
        else status.textContent = '';

        // --- полоса прогресса
        //
        // Она НЕ дубль строки состояния: `whisper` на пятнадцатиминутной
        // записи молчит минутами, и единственное, что отличает работающий
        // движок от зависшего, — ползущая полоса. Пока процентов ещё нет
        // (кадр `started` пришёл, `delta` — нет), полоса живёт нулевой
        // ширины: подменять неизвестное правдоподобным числом нельзя.
        progress.hidden = st.status !== 'running';
        bar.style.width = `${Number.isFinite(st.percent) ? st.percent : 0}%`;

        // --- сам текст
        const hasText = st.status === 'ready' && String(st.text || '') !== '';
        textEl.textContent = hasText ? st.text : '';
        textEl.hidden = !hasText;
        const collapsible = hasText && needsCollapse(st.text);
        setClass(textEl, cls('text-collapsed'), collapsible && !st.expanded);

        // --- кнопки
        toggleBtn.hidden = !collapsible;
        toggleBtn.textContent = st.expanded ? 'Свернуть' : 'Показать полностью';
        attr(toggleBtn, 'aria-expanded', st.expanded ? 'true' : 'false');

        runBtn.hidden = st.status === 'ready';
        runBtn.disabled = st.status === 'running';
        runBtn.textContent = st.status === 'error' ? 'Повторить' : 'Расшифровать';
        attr(runBtn, 'title', st.status === 'error'
            ? 'Повторить расшифровку голосового'
            : 'Расшифровать голосовое сообщение');
    }

    /**
     * Забывает отцепленный узел.
     *
     * Наблюдатель держит цель СИЛЬНОЙ ссылкой, поэтому узлы прежней ленты,
     * оставленные под наблюдением, — это утечка ровно на длину переписки.
     */
    function forget(view) {
        if (observer && typeof observer.unobserve === 'function') observer.unobserve(view.root);
    }

    function renderKey(key) {
        const list = views.get(key);
        if (!list) return;
        // Рисуем во ВСЕ представления, включая ещё не вставленные в документ:
        // именно в таком состоянии карточка живёт между `attach` и вставкой
        // строки в ленту (см. шапку модуля).
        for (const view of list) renderView(view);
    }

    // -----------------------------------------------------------------------
    // Транспорт
    // -----------------------------------------------------------------------

    function coordsOf(key) {
        const parts = String(key).split('/');
        return {
            dialogId: parts.slice(1, parts.length - 2).join('/'),
            messageId: parts[parts.length - 2],
            index: Number(parts[parts.length - 1]) || 0,
        };
    }

    /** Читает уже посчитанную расшифровку. Ничего не запускает. */
    async function fetchTranscript(key) {
        const st = stateOf(key);
        if (st.fetched || st.fetching || st.status !== 'idle') return;
        if (!cfg.api || typeof cfg.api.transcript !== 'function') return;
        st.fetched = true;
        st.fetching = true;
        stats.fetches += 1;
        renderKey(key);
        const c = coordsOf(key);
        try {
            const resp = await cfg.api.transcript(c.dialogId, c.messageId, c.index);
            const cur = stateOf(key);
            cur.fetching = false;
            // Кадр мог прийти, пока ответ был в пути: он свежее ответа.
            if (cur.status === 'idle' && resp && resp.ready === true) {
                cur.status = 'ready';
                cur.text = String(resp.text || '');
                cur.engine = String(resp.engine || '');
                cur.language = String(resp.language || '');
                cur.durationMs = Number(resp.duration_ms) || 0;
            }
        } catch (_) {
            // Молча: «готового текста нет» — обычное состояние вложения, и
            // ошибка чтения не должна превращать карточку в красную. Кнопка
            // остаётся, нажатие даст внятный отказ.
            stateOf(key).fetching = false;
        }
        renderKey(key);
    }

    /** Запускает расшифровку. Только по нажатию человека. */
    async function start(key, force) {
        const st = stateOf(key);
        if (st.status === 'running') return;
        if (!cfg.api || typeof cfg.api.transcribe !== 'function') return;
        st.fetched = true;
        // «running» ставится ДО запроса: второе нажатие по ещё не отвеченному
        // POST'у обязано быть бесплатным.
        st.status = 'running';
        st.percent = null;
        st.code = '';
        stats.starts += 1;
        renderKey(key);
        const c = coordsOf(key);
        try {
            const resp = await cfg.api.transcribe(c.dialogId, c.messageId, c.index,
                force ? { force: true } : undefined);
            const cur = stateOf(key);
            if (resp && resp.ready === true) {
                cur.status = 'ready';
                cur.text = String(resp.text || '');
                cur.engine = String(resp.engine || cur.engine || '');
                cur.percent = null;
            } else if (resp) {
                cur.runId = resp.run_id != null ? String(resp.run_id) : cur.runId;
                cur.engine = String(resp.engine || cur.engine || '');
                if (cur.status !== 'ready' && cur.status !== 'error') cur.status = 'running';
            }
        } catch (err) {
            const cur = stateOf(key);
            cur.status = 'error';
            cur.code = (err && err.code) ? String(err.code) : '';
        }
        renderKey(key);
    }

    // -----------------------------------------------------------------------
    // Кадры WS
    // -----------------------------------------------------------------------

    /**
     * Применяет кадр расшифровки.
     *
     * Кадры вещаются ВСЕМ подписчикам без фильтрации по вкладке, поэтому
     * первое, что здесь происходит, — вычисление ключа: чужой ключ не трогает
     * ни одного узла.
     *
     * @param {object} detail — `detail` DOM-события панели
     */
    function onFrame(detail) {
        if (!detail || !detail.frame) return;
        if (detail.plugin && String(detail.plugin) !== cfg.plugin) return;
        const key = transcriptKey(cfg.plugin, detail.dialogId, detail.messageId, detail.index);
        const frame = detail.frame;
        const st = stateOf(key);
        stats.frames += 1;
        st.fetched = true;
        switch (String(detail.phase)) {
            case 'started':
                st.status = 'running';
                st.runId = frame.run_id != null ? String(frame.run_id) : null;
                st.engine = String(frame.engine || '');
                st.percent = 0;
                st.code = '';
                break;
            case 'delta':
                // Проценты только РАСТУТ: кадры коалесцированы на сервере, но
                // порядок доставки после `resync` не гарантирован никем, а
                // ползущая назад полоса выглядит как поломка.
                if (st.status !== 'running') st.status = 'running';
                {
                    const pct = Math.max(0, Math.min(100, Number(frame.percent) || 0));
                    if (!Number.isFinite(st.percent) || pct > st.percent) st.percent = pct;
                }
                break;
            case 'done':
                st.status = 'ready';
                st.text = String(frame.text || '');
                st.language = String(frame.language || '');
                st.engine = String(frame.engine || st.engine || '');
                st.durationMs = Number(frame.duration_ms) || 0;
                st.percent = 100;
                st.runId = null;
                break;
            case 'error':
                st.status = 'error';
                st.code = String(frame.code || '');
                st.runId = null;
                break;
            default:
                return;
        }
        renderKey(key);
    }

    /** Подписка на DOM-событие панели — ленивая и ровно одна. */
    function listen() {
        if (listening || !cfg.frameEvent) return;
        const target = globalThis.document;
        if (!target || typeof target.addEventListener !== 'function') return;
        target.addEventListener(cfg.frameEvent, (e) => onFrame(e && e.detail));
        listening = true;
    }

    // -----------------------------------------------------------------------
    // Ленивое чтение готового текста
    // -----------------------------------------------------------------------

    function ensureObserver() {
        if (observer !== null || !cfg.observe) return observer;
        const IO = globalThis.IntersectionObserver;
        if (typeof IO !== 'function') return null;
        observer = new IO((entries) => {
            for (const entry of entries) {
                if (!entry.isIntersecting) continue;
                observer.unobserve(entry.target);
                const key = entry.target.dataset && entry.target.dataset.voiceKey;
                if (key) fetchTranscript(key);
            }
        }, { rootMargin: '200px' });
        return observer;
    }

    /**
     * Планирует чтение готового текста.
     *
     * Наблюдатель — предпочтительный путь: он спрашивает ровно про то, что
     * человек видит. Потолок — страховка для окружений без наблюдателя;
     * сбрасывается он не по таймеру ленты, а микрозадачей, то есть на каждую
     * ПАЧКУ отрисовки.
     */
    function scheduleFetch(key, root) {
        const st = stateOf(key);
        if (st.fetched || st.status !== 'idle') return;
        const io = ensureObserver();
        if (io) {
            io.observe(root);
            return;
        }
        if (eagerSpent >= cfg.eagerBudget) return;
        eagerSpent += 1;
        if (eagerTimer === null) {
            eagerTimer = setTimeout(() => { eagerTimer = null; eagerSpent = 0; }, 0);
            if (eagerTimer && typeof eagerTimer.unref === 'function') eagerTimer.unref();
        }
        fetchTranscript(key);
    }

    // -----------------------------------------------------------------------
    // Разметка
    // -----------------------------------------------------------------------

    /**
     * Блок расшифровки под карточкой вложения.
     *
     * Возвращает `null` для всего, что не звук: у фотографии расшифровки не
     * бывает, и пустой блок под ней — мусор, а не сдержанность.
     *
     * @param {object} o
     * @param {string} o.dialogId
     * @param {string|number} o.messageId
     * @param {number} [o.index]
     * @param {string} o.kind — вид вложения в лексике панели
     * @param {object} [o.doc] — документ (фейковый в тестах)
     * @returns {?object} узел или null
     */
    function attach(o) {
        const doc = (o && o.doc) || globalThis.document;
        if (!doc || !isAudioKind(o && o.kind)) return null;
        listen();

        const key = transcriptKey(cfg.plugin, o.dialogId, o.messageId, o.index);
        const root = doc.createElement('div');
        root.className = cls('');
        root.dataset.voiceKey = key;
        attr(root, 'data-voice-key', key);

        const head = doc.createElement('div');
        head.className = cls('head');
        const label = doc.createElement('span');
        label.className = cls('label');
        label.textContent = 'Расшифровка';
        const status = doc.createElement('span');
        status.className = cls('status');
        // Строка живая: прогресс и финал обязаны доехать до скринридера, но
        // `assertive` перебивал бы чтение самого сообщения.
        attr(status, 'role', 'status');
        attr(status, 'aria-live', 'polite');
        head.appendChild(label);
        head.appendChild(status);

        const progress = doc.createElement('div');
        progress.className = cls('progress');
        progress.hidden = true;
        // Полоса — картинка того же числа, что уже сказано словами в строке
        // состояния; скринридеру она даёт только шум.
        attr(progress, 'aria-hidden', 'true');
        const bar = doc.createElement('span');
        bar.className = cls('bar');
        bar.style.width = '0%';
        progress.appendChild(bar);

        const textEl = doc.createElement('div');
        textEl.className = cls('text');
        textEl.hidden = true;

        const actions = doc.createElement('div');
        actions.className = cls('actions');
        const runBtn = doc.createElement('button');
        runBtn.className = cls('run');
        attr(runBtn, 'type', 'button');
        runBtn.type = 'button';
        runBtn.dataset.action = 'voice-transcribe';
        const toggleBtn = doc.createElement('button');
        toggleBtn.className = cls('toggle');
        attr(toggleBtn, 'type', 'button');
        toggleBtn.type = 'button';
        toggleBtn.dataset.action = 'voice-toggle';
        toggleBtn.hidden = true;
        actions.appendChild(runBtn);
        actions.appendChild(toggleBtn);

        root.appendChild(head);
        root.appendChild(progress);
        root.appendChild(textEl);
        root.appendChild(actions);

        const view = { key, root, status, progress, bar, textEl, runBtn, toggleBtn };

        if (typeof runBtn.addEventListener === 'function') {
            runBtn.addEventListener('click', (e) => {
                // Карточка вложения слушает клики по себе (открыть, играть) —
                // кнопка расшифровки к ним не относится.
                if (e && typeof e.stopPropagation === 'function') e.stopPropagation();
                // `hidden` проверяется наравне с `disabled`: спрятанную кнопку
                // мышью не нажать, но программный клик (клавиатурный хоткей
                // панели, тест) до обработчика доходит.
                if (runBtn.disabled || runBtn.hidden) return;
                const st = stateOf(key);
                start(key, st.status === 'ready');
            });
        }
        if (typeof toggleBtn.addEventListener === 'function') {
            toggleBtn.addEventListener('click', (e) => {
                if (e && typeof e.stopPropagation === 'function') e.stopPropagation();
                const st = stateOf(key);
                st.expanded = !st.expanded;
                renderKey(key);
            });
        }

        const list = views.get(key) || [];
        // Прежние представления выбрасываются ЗДЕСЬ и только здесь: появление
        // нового — единственный момент, когда точно известно, что лента
        // перерисовалась. Отцепленное представление, оставшееся последним, —
        // это ещё не вставленная карточка, а не мусор.
        const live = list.filter((old) => {
            if (old.root.isConnected !== false) return true;
            forget(old);
            return false;
        });
        live.push(view);
        views.set(key, live);

        renderView(view);
        scheduleFetch(key, root);
        return root;
    }

    return {
        attach,
        onFrame,
        listen,
        /** Ручной запуск — тем же путём, что и кнопка (для хоткеев панели). */
        start: (dialogId, messageId, index, force) =>
            start(transcriptKey(cfg.plugin, dialogId, messageId, index), force === true),
        /** Снимок состояния ключа (для тестов и отладки). */
        peek: (dialogId, messageId, index) => {
            const st = states.get(transcriptKey(cfg.plugin, dialogId, messageId, index));
            return st ? { ...st } : null;
        },
        /** Переопределение транспорта и политик — панелью при старте и тестами. */
        configure: (next) => {
            if (!next) return;
            if (next.api) cfg.api = next.api;
            if (next.frameEvent) cfg.frameEvent = next.frameEvent;
            if (Number.isFinite(next.eagerBudget)) cfg.eagerBudget = next.eagerBudget;
            if (next.observe !== undefined) cfg.observe = next.observe !== false;
        },
        /** Полный сброс — смена аккаунта панели, выход, тесты. */
        reset: () => {
            states.clear();
            views.clear();
            eagerSpent = 0;
            if (observer && typeof observer.disconnect === 'function') observer.disconnect();
            observer = null;
            stats.fetches = 0;
            stats.starts = 0;
            stats.frames = 0;
        },
        stats,
    };
}
