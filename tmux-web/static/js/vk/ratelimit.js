// tmux-web — VK plugin: клиентский планировщик запросов.
//
// НОВЫЙ модуль: у Telegram аналога нет, потому что там ограничителем был
// `flood_wait` ПО ФАКТУ. У VK лимит известен заранее — 3 запроса в секунду на
// пользовательский токен, — и его надо соблюдать превентивно.
//
// ============================================================================
// ЗАЧЕМ ОТДЕЛЬНЫЙ ПЛАНИРОВЩИК, ЕСЛИ НА БЭКЕНДЕ УЖЕ ЕСТЬ ТРОТТЛЕР
// ============================================================================
//
// Он решает ДРУГУЮ задачу. Серверный `Throttle` (plugins/vk/src/api/throttle.rs)
// отвечает за «уложиться в лимит»: он ставит запросы в очередь и выдаёт их
// равномерно. Клиентский — за то, чтобы В ЭТУ ОЧЕРЕДЬ НЕ ПОПАДАЛИ ЗАПРОСЫ,
// КОТОРЫЕ УЖЕ НЕАКТУАЛЬНЫ.
//
// Разница видна на одном сценарии: пользователь быстро прощёлкал десять бесед.
// С одним лишь серверным троттлером он честно дождётся ВСЕХ ДЕСЯТИ историй —
// три с лишним секунды, в течение которых лента будет перерисовываться чужими
// данными. С коалесcингом по ключу в полёте остаётся ровно последняя.
//
// Отсюда три механизма, и каждый закрывает свой случай:
//
//   1. `createLimiter` — token bucket с двумя приоритетами. Действие
//      пользователя («открой беседу») обгоняет фоновое («обнови список»):
//      иначе фон, поставленный в очередь секундой раньше, задерживает то, чего
//      человек ждёт прямо сейчас.
//   2. `createCoalescer` — по одному ключу живёт только ПОСЛЕДНИЙ запрос,
//      предыдущий отменяется через `AbortController`. Ключ — это «что
//      грузим» (`history:1234`), а не «куда»: две загрузки одной беседы
//      бессмысленны, двух разных — законны.
//   3. `dropStale` — генерационный счётчик. Отменённый `fetch` иногда всё же
//      успевает вернуть данные (отмена не мгновенна), и без проверки поколения
//      история покинутой беседы дорисовалась бы поверх открытой.
//
// ФАБРИКИ ЧИСТЫЕ: время и фабрика `AbortController` инжектятся, поэтому
// node-тест гоняет всю логику без браузера и без стенда — см.
// tmux-web/tests/frontend/vk_ratelimit.test.mjs. Внизу файла живут ОБЩИЕ НА
// ВКЛАДКУ экземпляры: бюджет 3 rps выдаётся на токен, то есть один на всю
// панель, и два планировщика означали бы двойной расход (ровно та же ошибка,
// что «два VkClient» на бэкенде).

import { vkState } from './state.js';

/**
 * Минимальный интервал между запросами, мс.
 *
 * 380, а не 334 (=1000/3): это запас на дрожание таймеров браузера и на то,
 * что сервер считает секунду по СВОИМ часам. Промах в меньшую сторону стоит
 * кода 6 от VK, то есть лишнего круга по сети; промах в большую — 40 мс
 * задержки, которых никто не заметит.
 */
export const MIN_INTERVAL_MS = 380;

/**
 * Ёмкость bucket'а: сколько запросов можно выпустить ЗАЛПОМ.
 *
 * Три — ровно лимит VK. Открытие вкладки честно стоит трёх запросов подряд
 * (список бесед + фильтры + история первой беседы), и растягивать их на
 * секунду означало бы показать пустую панель там, где данные уже готовы.
 */
export const BURST = 3;

/** Приоритеты очереди: действие пользователя обгоняет фоновое. */
export const PRIORITY_USER = 'user';
/** Фоновое обновление — уступает пользовательскому. */
export const PRIORITY_BG = 'bg';

/**
 * Создаёт планировщик запросов (token bucket + очередь с приоритетами).
 *
 * Пополнение НЕПРЕРЫВНОЕ (дробными токенами по времени), а не «раз в интервал
 * +1»: фиксированное окно даёт всплеск на его границе, а bucket разводит
 * запросы равномерно — именно это и отличает его от счётчика в секунду.
 *
 * @param {{minIntervalMs?: number, burst?: number,
 *   now?: function(): number, schedule?: function(function(), number): *,
 *   frozen?: function(): boolean}} [opts]
 *   `now` — источник времени (тест подставляет свой);
 *   `schedule` — планировщик отложенного вызова (по умолчанию setTimeout);
 *   `frozen` — «фоновые запросы сейчас нельзя» (поднятый оверлей captcha).
 * @returns {{acquire: function(string): Promise<void>, size: function(): number,
 *   pump: function(): void, _debug: function(): object}}
 */
export function createLimiter(opts) {
    const o = opts || {};
    // Тайминги читаются из объекта опций НА КАЖДОМ обращении, а не
    // защёлкиваются при создании. Это тот же приём, что `with_retry_delays` и
    // `with_ws_timings` на бэкенде: иначе интеграционный тест обязан был бы
    // ждать настоящие 380 мс на каждый запрос — то есть проверять выдержку
    // вместо поведения.
    const interval = () => (Number.isFinite(o.minIntervalMs) && o.minIntervalMs > 0
        ? o.minIntervalMs
        : MIN_INTERVAL_MS);
    const burst = () => (Number.isFinite(o.burst) && o.burst > 0 ? o.burst : BURST);
    const now = typeof o.now === 'function' ? o.now : () => Date.now();
    const schedule = typeof o.schedule === 'function'
        ? o.schedule
        : (fn, ms) => (typeof setTimeout === 'function' ? setTimeout(fn, ms) : fn());
    const frozen = typeof o.frozen === 'function' ? o.frozen : () => false;

    const state = {
        tokens: burst(),
        last: now(),
        // Две очереди, а не одна с сортировкой: приоритетов ровно два, и
        // сортировка на каждом добавлении была бы дороже самой очереди.
        user: [],
        bg: [],
        timer: null,
    };

    /** Доливает токены за прошедшее время. */
    function refill() {
        const t = now();
        const dt = Math.max(0, t - state.last);
        if (dt <= 0) return;
        state.last = t;
        state.tokens = Math.min(burst(), state.tokens + dt / interval());
    }

    /** Выпускает столько ждущих, сколько позволяет bucket. */
    function pump() {
        refill();
        while (state.tokens >= 1) {
            // Пользовательская очередь ВСЕГДА первая: фон, вставший в очередь
            // раньше, не имеет права задерживать то, что человек ждёт сейчас.
            const q = state.user.length ? state.user : (frozen() ? null : state.bg);
            if (!q || !q.length) break;
            state.tokens -= 1;
            const resolve = q.shift();
            resolve();
        }
        arm();
    }

    /**
     * Ставит таймер на момент, когда появится следующий токен.
     *
     * Ровно один таймер на планировщик: по одному на ждущего означало бы N
     * таймеров при быстром прощёлкивании бесед.
     */
    function arm() {
        const pending = state.user.length + (frozen() ? 0 : state.bg.length);
        if (!pending || state.timer !== null) return;
        const wait = Math.max(1, Math.ceil((1 - state.tokens) * interval()));
        state.timer = schedule(() => {
            state.timer = null;
            pump();
        }, wait);
    }

    return {
        /**
         * Ждёт своей очереди. Резолвится, когда запрос можно отправлять.
         *
         * @param {'user'|'bg'} [priority='user']
         * @returns {Promise<void>}
         */
        acquire(priority) {
            return new Promise((resolve) => {
                const q = priority === PRIORITY_BG ? state.bg : state.user;
                q.push(resolve);
                pump();
            });
        },
        /** Сколько запросов ждёт очереди. */
        size() {
            return state.user.length + state.bg.length;
        },
        /** Пнуть очередь вручную (после снятия «заморозки» фона). */
        pump,
        _debug() {
            return {
                tokens: Math.round(state.tokens * 100) / 100,
                user: state.user.length,
                bg: state.bg.length,
                timerArmed: state.timer !== null,
            };
        },
    };
}

/**
 * Создаёт коалесcер: по одному ключу живёт только ПОСЛЕДНИЙ запрос.
 *
 * Предыдущий отменяется через `AbortController` — не «забывается», а именно
 * отменяется: брошенный `fetch` продолжил бы занимать и соединение, и место в
 * серверной очереди 3 rps, то есть задерживал бы актуальный запрос.
 *
 * `fn` получает `signal` и обязан передать его в `fetch`. Отменённый вызов
 * отвергается ошибкой с `name === 'AbortError'` — вызывающему полагается
 * молча её проглотить: это не сбой, а наше собственное решение.
 *
 * @param {{controller?: function(): AbortController}} [opts]
 * @returns {{submit: function(string, function(AbortSignal): Promise<*>): Promise<*>,
 *   abort: function(string): boolean, abortAll: function(): number,
 *   size: function(): number, _debug: function(): object}}
 */
export function createCoalescer(opts) {
    const make = (opts && typeof opts.controller === 'function')
        ? opts.controller
        : () => (typeof AbortController === 'function' ? new AbortController() : null);
    /** ключ → AbortController запроса в полёте. */
    const inflight = new Map();

    function abort(key) {
        const k = String(key);
        const c = inflight.get(k);
        if (!c) return false;
        inflight.delete(k);
        try {
            c.abort();
        } catch (_) {
            // Уже отменён или окружение без AbortController — не наша беда.
        }
        return true;
    }

    return {
        submit(key, fn) {
            const k = String(key);
            // Отмена ПЕРЕД созданием нового контроллера: иначе новый успел бы
            // лечь в карту, и abort() убил бы как раз актуальный запрос.
            abort(k);
            const ctrl = make();
            if (ctrl) inflight.set(k, ctrl);
            const signal = ctrl ? ctrl.signal : undefined;
            // `fn` вызывается СИНХРОННО, а не через микрозадачу: между
            // регистрацией контроллера и стартом запроса не должно быть щели,
            // в которую успеет вклиниться следующий submit того же ключа.
            let p;
            try {
                p = Promise.resolve(fn(signal));
            } catch (e) {
                p = Promise.reject(e);
            }
            return p.finally(() => {
                // Снимаем ТОЛЬКО свой контроллер: пока мы летели, ключ мог
                // достаться более свежему запросу, и его отменять нельзя.
                if (inflight.get(k) === ctrl) inflight.delete(k);
            });
        },
        abort,
        abortAll() {
            const n = inflight.size;
            for (const k of [...inflight.keys()]) abort(k);
            return n;
        },
        size() {
            return inflight.size;
        },
        _debug() {
            return { keys: [...inflight.keys()] };
        },
    };
}

/**
 * Устарел ли ответ: его поколение не совпадает с текущим.
 *
 * Нужен там, где отмена не спасает. `AbortController` отменяет ЗАПРОС, но
 * ответ, уже прочитанный из сети, всё равно доедет до `.then()` — а
 * пользователь к этому моменту открыл другую беседу. Проверка поколения в
 * начале обработчика — единственный дешёвый способ это поймать.
 *
 * @param {number} gen — поколение, с которым запрос стартовал
 * @param {number} current — поколение сейчас
 * @returns {boolean} true ⇒ ответ выбросить
 */
export function dropStale(gen, current) {
    const g = Number(gen);
    const c = Number(current);
    if (!Number.isFinite(g) || !Number.isFinite(c)) return true;
    return g !== c;
}

/**
 * Счётчик поколений: `bump()` перед новой загрузкой, `current()` в обработчике.
 *
 * Отдельным объектом, а не голым числом в состоянии панели, ровно затем, чтобы
 * его можно было проверить тестом и переиспользовать в media.js (Ф7).
 *
 * @returns {{bump: function(): number, current: function(): number,
 *   stale: function(number): boolean}}
 */
export function createGeneration() {
    let gen = 0;
    return {
        bump() {
            gen += 1;
            return gen;
        },
        current() {
            return gen;
        },
        stale(g) {
            return dropStale(g, gen);
        },
    };
}

// ---------------------------------------------------------------------------
// Общие на вкладку экземпляры.
// ---------------------------------------------------------------------------

/**
 * Планировщик панели — ОДИН на вкладку.
 *
 * Лимит 3 rps VK выдаётся на ТОКЕН, а не на соединение: второй планировщик
 * означал бы второй бюджет, то есть гарантированный код 6. Это та же ошибка,
 * от которой на бэкенде защищает «один VkClient на токен».
 *
 * Фоновые запросы заморожены, пока открыт оверлей captcha: любой запрос,
 * выпущенный в этот момент, получит от VK НОВУЮ captcha — и пользователь будет
 * вводить символы по кругу, не понимая, почему их всё время просят снова.
 *
 * Тайминги вынесены в [`vkLimits`] и читаются на каждом обращении: иначе
 * тесты панели ждали бы настоящие 380 мс на каждый запрос.
 */
export const vkLimits = {
    minIntervalMs: MIN_INTERVAL_MS,
    burst: BURST,
    frozen: () => !!vkState.captcha,
};

export const vkLimiter = createLimiter(vkLimits);

/**
 * Коалесcер панели — тоже один: ключи запросов («dialogs», «history:1234»)
 * глобальны для вкладки, и раздельные карты полёта не отменяли бы друг друга.
 */
export const vkCoalescer = createCoalescer();
