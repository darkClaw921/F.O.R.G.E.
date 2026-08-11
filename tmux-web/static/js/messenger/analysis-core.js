// tmux-web — ОБЩЕЕ ЯДРО попапа «Анализ чата» для панелей мессенджеров.
//
// Разбор переписки локальным `claude` CLI: кнопка ✦ в титлбаре панели
// открывает попап, кнопка ANALYZE запускает разбор, поле снизу задаёт вопросы
// по той же истории, вкладка «Контекст чата» хранит пер-чатовую заметку.
//
// Панель плагина (VK, Telegram, Bitrix24) отдаёт сюда СВОИ ref'ы, СВОЙ
// транспорт и СВОИ подписи ошибок, а поведение целиком живёт здесь. Модуль
// ничего не знает ни про один конкретный мессенджер: ни имён его модулей, ни
// его оверлеев, ни его лимитера запросов.
//
// ============================================================================
// ФАБРИКА, А НЕ МОДУЛЬ С ГЛОБАЛЬНЫМ СОСТОЯНИЕМ
// ============================================================================
//
// `createAnalysisPanel` держит ВСЁ состояние в замыкании. Две панели, созданные
// на одной странице (а именно так и будет, когда рядом с VK появятся Telegram и
// Bitrix24), обязаны быть слепы друг к другу: общий `let` на уровне модуля
// означал бы, что открытие второго попапа стирает беседу первого и уводит его
// дельты в чужой узел.
//
// ============================================================================
// ДВА ВЫВОДА, А НЕ ОДИН
// ============================================================================
//
// `refs.out` принадлежит **разбору** и живёт постоянно: он заполняется
// сохранённым текстом при открытии попапа, перерисовывается по ANALYZE /
// REFRESH и стирается только кнопкой CLEAR. Ответ на вопрос уходит в СВОЮ
// строку `refs.thread`.
//
// Так и была устроена исходная жалоба: раньше контейнер был один, и `ask`
// затирал разбор, ради которого его и открывали. Отсюда правило, которое
// нельзя вернуть обратно: **`clearOut()` не вызывается при `kind === 'ask'`
// НИКОГДА** — ни на запуске, ни в `analysis_started`, ни в `analysis_done`.
// Куда льются дельты, решает `deltaTarget()`, и решает по одному признаку:
// есть ли открытая строка треда (`local.askRow`).
//
// И разбор, и тред Q&A живут на СЕРВЕРЕ (рядом с заметкой беседы), поэтому
// переживают закрытие попапа, перезагрузку страницы и видны со второго
// устройства. Кнопок очистки всё равно две, и бьют они в РАЗНЫЕ адреса:
// `api.clearAnalysis` стирает разбор и не трогает тред, `api.clearThread` —
// наоборот. Это не симметрия ради симметрии: разбор пересобирается одной
// кнопкой за минуту, а десяток заданных вопросов — нет.
//
// ============================================================================
// ТРЕД ПРИХОДИТ ИЗ GET, А НЕ КОПИТСЯ ТОЛЬКО СВОИМИ ОТВЕТАМИ
// ============================================================================
//
// `local.thread` наполняется полем `thread` ответа `api.getSavedAnalysis` при
// открытии попапа и при смене беседы, а свои свежие пары ДОПИСЫВАЮТСЯ в тот же
// список. Ровно этим чинится исходная жалоба: до доработки тред жил только в
// памяти вкладки и умирал вместе со страницей.
//
// Два лимита треда РАЗНЫЕ, и это намеренно:
// - `MAX_THREAD_TURNS` — сколько пар ПОКАЗЫВАЕМ и помним (зеркало сервера);
// - `clampThread` (`MAX_HISTORY_*`) — сколько пар ОТПРАВЛЯЕМ модели.
// Помним больше, чем отправляем: контекст модели дорог, а история переписки
// пользователя — нет.
//
// ============================================================================
// СВОРАЧИВАНИЕ И MARKDOWN
// ============================================================================
//
// 1. Сворачивается ТОЛЬКО `refs.out`. Метка «разобрано до …», статус и тред
//    лежат снаружи сворачиваемого узла — свёрнутый разбор не должен уносить с
//    экрана то, ради чего его и сворачивают. Состояние глобальное
//    (`localStorage`, ключ `<prefix>.analysis.collapsed`), а не на беседу: это
//    привычка смотреть в вопросы, а не свойство конкретного чата.
// 2. Старт разбора РАЗВОРАЧИВАЕТ блок принудительно — иначе дельты текли бы в
//    невидимое, и пользователь смотрел бы на пустой экран, считая, что сломано.
// 3. Markdown рисуется ОДИН РАЗ, по `analysis_done`: во время стрима в блоке
//    сырой текст (`pre-wrap`). Разбирать незакрытую разметку на каждой дельте —
//    это и мерцание, и лишняя работа восемь раз в секунду. Неполный ответ
//    (отмена, ошибка) так и остаётся сырым: дорисовывать за модель нечего.
//    Сохранённый разбор и ответы треда из `api.getSavedAnalysis` — уже целые,
//    поэтому рисуются Markdown'ом сразу.
//
// ============================================================================
// ЧЕТЫРЕ РЕШЕНИЯ, КОТОРЫЕ ЛЕГКО СЛОМАТЬ ОБРАТНО
// ============================================================================
//
// 1. **Разметка статична**, а ref'ы приходят снаружи готовыми узлами. В попапе
//    больше десятка интерактивных узлов, и сборка их здесь заставила бы ядро
//    ходить через querySelector в обход конвенции панели, а заодно сломала бы
//    фейковый DOM тестов и e2e-селекторы.
// 2. **Esc не перехватывается, пока открыт блокирующий оверлей панели.** Такой
//    оверлей живёт ВЫШЕ попапа и может всплыть прямо во время выгрузки истории;
//    забрав у него Esc, попап закрывал бы себя, оставляя чужое окно висеть
//    поверх. Признак ядро спрашивает функцией `isBlockingOverlayOpen` — как
//    именно панель его считает, ядру знать незачем; у кого такого оверлея нет,
//    тот отдаёт `() => false`.
// 3. **Заметка читается ЛЕНИВО и ровно один раз на беседу.** Она нужна только
//    на вкладке «Контекст», а бюджет запросов у панели общий. Флаг
//    `promptLoaded` сбрасывается при смене беседы, иначе вторая беседа
//    показала бы заметку первой.
// 4. **Ошибки разбираются по `err.code`**, а не по тексту: тексты меняются (в
//    них подмешивается формулировка мессенджера и путь до бинаря `claude`),
//    коды — контракт `routes/error.rs` плагина.
//
// ИНВАРИАНТ XSS: любые тексты (заметка, ответ модели, имя беседы) попадают в
// DOM только через textContent и createTextNode. Ни одной записи разметки
// строкой в модуле нет — это проверяется тестом ПО ИСХОДНИКУ, поэтому
// запретное свойство не упоминается здесь даже в комментарии.
//
// ============================================================================
// ЧТО ВАЖНО ЗНАТЬ ПРО СТРИМИНГ
// ============================================================================
//
// 1. **Кадры НЕ отфильтрованы сервером по беседе.** Broadcast WS панели общий —
//    иначе бейдж и тосты не работали бы из чужой вкладки. Поэтому КАЖДЫЙ кадр
//    здесь сверяется с парой (`local.dialogId`, `local.runId`): без этого вывод
//    разбора, запущенного во второй вкладке по другой беседе, затёк бы в
//    открытый попап.
// 2. **`analysis_started` приходит РАНЬШЕ ответа POST'а** (хаб шлёт кадр до
//    того, как маршрут вернёт `run_id`). Поэтому попап, у которого запуск «в
//    полёте» (`local.pending`), УСЫНОВЛЯЕТ `run_id` из кадра. Иначе первые
//    дельты отфильтровались бы как чужие и потерялись молча.
// 3. **Дельты копятся в буфере и выливаются одним кадром анимации.** Сервер их
//    уже коалесцирует (120 мс / 400 символов), но при быстрой генерации это
//    всё равно до десятка вставок в секунду, и каждая — reflow блока с
//    собственной прокруткой. `analysis_done` / `analysis_error` обязаны слить
//    остаток СИНХРОННО: последний кусок ответа теряться не имеет права.
// 4. **Автоскролл — через общую фабрику `messenger/follow-core.js`**, СВОИМ
//    инстансом на панель.
// 5. **История Q&A уезжает полем `history` в каждом следующем запросе**: сервер
//    не помнит, о чём его спрашивали в прошлый раз, — он лишь ХРАНИТ тред
//    рядом с разбором. Лимиты отправки зеркалят серверные
//    (`chat-analysis/src/prompt.rs`).

import { createFollow } from './follow-core.js';
import { renderMarkdownText } from './markdown.js';

/**
 * Глубина по умолчанию — та же, что `context::DEFAULT_DEPTH` на сервере.
 *
 * Дублируется намеренно: значение стоит и в `<option selected>` разметки, и
 * здесь как страховка на случай, если селект не резолвился (старый index.html
 * из кэша PWA).
 *
 * Это ДЕФОЛТ ЯДРА, а не единственно возможное значение: панель, у которой
 * потолок платформы другой, передаёт своё через `defaultDepth` фабрики (см.
 * `createAnalysisPanel`). Так сделан Битрикс24 — портал отдаёт историю
 * страницами по 50 сообщений при бюджете 2 rps, и 500 сообщений там означали
 * бы десять запросов и пять секунд ожидания перед первым словом модели.
 */
export const DEFAULT_DEPTH = 200;

/**
 * Потолок заметки до первого ответа сервера.
 *
 * Настоящее значение приезжает в `max_chars` вместе с текстом: счётчик обязан
 * совпадать с лимитом, по которому сервер РЕАЛЬНО режет. Константа здесь —
 * только чтобы счётчик не показывал «/undefined» до первой загрузки.
 */
export const FALLBACK_MAX_CHARS = 4000;

/**
 * Допустимые значения глубины — ровно те, что стоят `<option>`'ами в разметке.
 *
 * Список закрытый: значение приезжает из localStorage, то есть из места, куда
 * писать может кто угодно (другая версия панели, руки в devtools). Чужое число
 * молча игнорируется — сервер всё равно режет по своим потолкам, но показывать
 * в селекте пункт, которого в нём нет, незачем.
 *
 * Это список ЯДРА, а не общий на все панели: свой набор панель передаёт в
 * `depthChoices` фабрики (у Битрикс24 это `[50, 100, 200]` — портал отдаёт
 * историю страницами по 50 сообщений при бюджете 2 rps). Разбирать чужое
 * значение по списку ядра было бы ошибкой ровно того же рода, что и хардкод
 * ключа localStorage: пункт «500», которого нет в разметке панели, выглядел бы
 * как «сервер меня не послушал».
 */
export const DEPTH_CHOICES = [50, 200, 500];

/**
 * Клиентские лимиты треда — зеркало `chat-analysis/src/prompt.rs`
 * (`MAX_QUESTION_CHARS`, `MAX_HISTORY_TURNS`, `MAX_HISTORY_CHARS`).
 *
 * Дублирование сознательное: сервер обрежет и без нас, но тогда пользователь
 * не узнал бы, что его первые вопросы модель уже не видит.
 */
export const MAX_QUESTION_CHARS = 2000;
export const MAX_HISTORY_TURNS = 6;
export const MAX_HISTORY_CHARS = 8000;

/**
 * Сколько пар треда ПОКАЗЫВАЕМ и держим в памяти — зеркало серверного
 * `prompts::MAX_THREAD_TURNS`.
 *
 * Больше, чем `MAX_HISTORY_TURNS`, и это не рассогласование: сервер хранит 12
 * пар, а модели уезжает 6. Обрезать показанное по размеру промпта значило бы
 * стирать с экрана вопросы, которые на сервере целы, — пользователь решил бы,
 * что их потеряли.
 */
export const MAX_THREAD_TURNS = 12;

/**
 * Насколько близко к низу вывода считается «пользователь внизу», px.
 *
 * Не ноль: дробная высота строки и субпиксельная прокрутка дают остаток в
 * единицы пикселей даже у прокрученного до упора блока.
 */
export const BOTTOM_EPS_PX = 24;

/**
 * Ключ localStorage для выбранной глубины. Один на все вкладки — так и надо.
 *
 * Ключи строятся из префикса панели, а не хардкодятся: иначе Telegram и
 * Bitrix24 делили бы с VK одну ячейку, и выбор глубины в одной панели молча
 * менял бы её в остальных.
 *
 * @param {string} prefix
 * @returns {string}
 */
export function depthStorageKey(prefix) {
    return `${prefix}.analysis.depth`;
}

/**
 * Ключ localStorage для свёрнутого состояния разбора.
 *
 * Один на все беседы — так и задумано: это привычка смотреть в вопросы, а не
 * свойство конкретного чата. Но НЕ один на все панели (см. `depthStorageKey`).
 *
 * @param {string} prefix
 * @returns {string}
 */
export function collapseStorageKey(prefix) {
    return `${prefix}.analysis.collapsed`;
}

// ---------------------------------------------------------------------------
// Чистые помощники (без DOM и без состояния — их же переиспользуют тесты)
// ---------------------------------------------------------------------------

/**
 * Нормализует глубину по ПРОИЗВОЛЬНОМУ списку допустимых значений.
 *
 * Отдельная функция, а не параметр по умолчанию у `pickDepth`: на `pickDepth`
 * висят тесты панелей VK и Telegram, проверяющие её ОДНОАРГУМЕНТНОЙ, и
 * дописать туда второй аргумент значило бы менять подпись, на которую уже
 * кто-то смотрит. Здесь же список приходит от панели: у Битрикс24 он свой.
 *
 * Пустой/битый список — не повод отвергнуть всё подряд: тогда селект панели
 * замер бы на одном значении навсегда, поэтому падаем на список ядра.
 *
 * @param {?Array<number>} choices допустимые значения панели
 * @param {*} value
 * @returns {?number}
 */
export function pickDepthFrom(choices, value) {
    const list = Array.isArray(choices) && choices.length ? choices : DEPTH_CHOICES;
    const n = Number(value);
    return list.includes(n) ? n : null;
}

/**
 * Нормализует глубину: только значение из `DEPTH_CHOICES`, иначе `null`.
 *
 * @param {*} value
 * @returns {?number}
 */
export function pickDepth(value) {
    return pickDepthFrom(DEPTH_CHOICES, value);
}

/**
 * Длина текста в CODE POINTS — так же, как считает сервер.
 *
 * `String.length` считает единицы UTF-16, то есть emoji весит два. Счётчик,
 * расходящийся с серверным, обещал бы пользователю место, которого нет.
 *
 * @param {string} text
 * @returns {number}
 */
export function charCount(text) {
    return Array.from(String(text == null ? '' : text)).length;
}

/**
 * Обрезает историю ТЕМ ЖЕ правилом, что `prompt.rs::clamp_history`: не больше
 * `MAX_HISTORY_TURNS` пар и `MAX_HISTORY_CHARS` символов, лишнее отбрасывается
 * СО СТАРЫХ.
 *
 * Старые, а не новые: последний вопрос почти всегда уточняет предыдущий, и
 * выбросить его значит лишить модель единственного, что ей точно нужно.
 *
 * @param {Array<{q: string, a: string}>} thread
 * @returns {Array<{q: string, a: string}>}
 */
export function clampThread(thread) {
    const list = Array.isArray(thread) ? thread : [];
    const kept = [];
    let used = 0;
    for (let i = list.length - 1; i >= 0; i -= 1) {
        const turn = list[i];
        if (!turn) continue;
        if (kept.length >= MAX_HISTORY_TURNS) break;
        const cost = charCount(turn.q) + charCount(turn.a);
        if (used + cost > MAX_HISTORY_CHARS) break;
        used += cost;
        kept.push({ q: String(turn.q == null ? '' : turn.q), a: String(turn.a == null ? '' : turn.a) });
    }
    kept.reverse();
    return kept;
}

/**
 * Обрезает тред до того, что ПОКАЗЫВАЕМ: `MAX_THREAD_TURNS` последних пар.
 *
 * Не путать с `clampThread` — та готовит урезанную копию для модели. Здесь
 * ограничение только одно и по парам: сервер хранит ровно столько же, и
 * показывать больше, чем переживёт перезагрузку, было бы обманом.
 *
 * @param {Array<{q: string, a: string}>} thread
 * @returns {Array<{q: string, a: string}>}
 */
export function clampThreadForView(thread) {
    const list = Array.isArray(thread) ? thread : [];
    const kept = [];
    for (const turn of list) {
        if (!turn) continue;
        kept.push({
            q: String(turn.q == null ? '' : turn.q),
            a: String(turn.a == null ? '' : turn.a),
        });
    }
    return kept.length > MAX_THREAD_TURNS ? kept.slice(kept.length - MAX_THREAD_TURNS) : kept;
}

/**
 * Приводит заметку к тому виду, в каком её сохранит сервер.
 *
 * Правило повторяет `prompts::sanitize`: `trim` по краям, обрезка до
 * `maxChars` символов, затем `trimEnd` у среза. Обрезаем на клиенте, чтобы
 * пользователь увидел ровно то, что уедет, — молчаливая серверная обрезка
 * выглядит как потерянный текст.
 *
 * @param {string} text
 * @param {number} maxChars
 * @returns {string}
 */
export function clampPrompt(text, maxChars) {
    const trimmed = String(text == null ? '' : text).trim();
    const limit = Number.isFinite(maxChars) && maxChars > 0 ? maxChars : FALLBACK_MAX_CHARS;
    const points = Array.from(trimmed);
    if (points.length <= limit) return trimmed;
    return points.slice(0, limit).join('').replace(/\s+$/, '');
}

/**
 * Строка «разобрано до <дата>, N сообщений» — по СОХРАНЁННОМУ разбору.
 *
 * Дата собирается локалью браузера, а не форматом сервера: `updated_at` едет
 * в unix-секундах именно ради этого — единственный, кто знает часовой пояс
 * пользователя, это его браузер.
 *
 * Пустая строка (разбора нет) — не «—» и не «никогда»: узел просто молчит,
 * иначе попап до первого запуска выглядел бы сломанным.
 *
 * @param {?number} at unix-секунды
 * @param {number} used сколько сообщений было в разборе
 * @returns {string}
 */
export function updatedLabel(at, used) {
    const ts = Number(at);
    if (!Number.isFinite(ts) || ts <= 0) return '';
    let when = String(ts);
    try {
        when = new Date(ts * 1000).toLocaleString();
    } catch (_) {
        // Экзотическая локаль/среда без Intl — число секунд честнее пустоты.
    }
    const n = Number(used);
    const count = Number.isFinite(n) && n > 0 ? `, ${n} сообщений` : '';
    return `разобрано до ${when}${count}`;
}

/**
 * Подписи ошибок по умолчанию — общие для всех панелей.
 *
 * Формулировки, в которых должно стоять имя конкретного мессенджера («токен
 * VK», «связь с VK»), панель передаёт своей картой в `errorTexts`: ядро не
 * знает, чей это чат, а безымянное «сервис» пользователю ничего не объясняет.
 */
const DEFAULT_ERROR_TEXTS = {
    claude_unavailable: 'Claude CLI не найден. Установите `claude` и убедитесь, что он есть в PATH devforge.',
    invalid_state: 'По этой беседе анализ уже идёт — дождитесь окончания или остановите его.',
    not_authorized: 'Доступ к мессенджеру не подтверждён: войдите заново.',
    not_connected: 'Нет связи с мессенджером — историю беседы сейчас не выгрузить.',
    storage_failed: 'Не удалось сохранить заметку на диск: проверьте права на каталог настроек.',
    invalid_input: 'Сервер отверг запрос: проверьте вопрос и глубину.',
    rate_limited: 'Слишком часто — сервис просит подождать.',
};

/**
 * Человеческий текст ошибки — СТРОГО по `err.code`.
 *
 * Разбор по тексту здесь был бы особенно вреден: сообщение
 * `claude_unavailable` содержит путь до бинаря, который у каждого свой.
 *
 * Неизвестный код НЕ роняет вызывающего и не показывает пустоту: остаётся
 * сообщение сервера, а если и его нет — общая формулировка.
 *
 * @param {?Error & {code?: ?string}} err
 * @param {?Object<string, string>} [texts] подписи панели поверх общих
 * @returns {string}
 */
export function analysisErrorText(err, texts) {
    const code = err && err.code;
    const table = texts ? Object.assign({}, DEFAULT_ERROR_TEXTS, texts) : DEFAULT_ERROR_TEXTS;
    if (typeof code === 'string' && Object.prototype.hasOwnProperty.call(table, code)) {
        return table[code];
    }
    return (err && err.message) ? String(err.message) : 'неизвестная ошибка';
}

// ---------------------------------------------------------------------------
// Фабрика панели
// ---------------------------------------------------------------------------

/**
 * Создаёт попап анализа поверх готовых узлов панели.
 *
 * @param {{
 *   prefix: string,
 *   refs: Object<string, ?object>,
 *   api: {
 *     getPrompt: function(string): Promise<object>,
 *     putPrompt: function(string, string): Promise<object>,
 *     getSavedAnalysis: function(string): Promise<object>,
 *     clearAnalysis: function(string): Promise<object>,
 *     clearThread: function(string): Promise<object>,
 *     start: function(string, object): Promise<object>,
 *     cancel: function(string, ?string): Promise<object>,
 *   },
 *   isBlockingOverlayOpen?: function(): boolean,
 *   onNotice?: function(string): void,
 *   dialogLabel?: function(string): string,
 *   activeDialogId?: function(): ?string,
 *   errorTexts?: Object<string, string>,
 *   depthChoices?: Array<number>,
 *   defaultDepth?: number,
 * }} cfg
 * @returns {object} методы панели
 */
export function createAnalysisPanel(cfg) {
    const conf = cfg || {};
    const prefix = String(conf.prefix || 'msg');
    const refs = conf.refs || {};
    const api = conf.api || {};
    const errorTexts = conf.errorTexts || null;
    const overlayOpen = typeof conf.isBlockingOverlayOpen === 'function'
        ? conf.isBlockingOverlayOpen
        : () => false;
    const dialogLabel = typeof conf.dialogLabel === 'function'
        ? conf.dialogLabel
        : (id) => String(id == null ? '' : id);
    const activeDialogId = typeof conf.activeDialogId === 'function'
        ? conf.activeDialogId
        : () => null;

    const DEPTH_KEY = depthStorageKey(prefix);
    const COLLAPSE_KEY = collapseStorageKey(prefix);

    /**
     * Глубина выгрузки — ПАРАМЕТР ПАНЕЛИ, а не константа ядра.
     *
     * Потолки у мессенджеров разные и определяются не вкусом, а платформой:
     * Битрикс24 отдаёт историю страницами по 50 сообщений при бюджете 2 rps,
     * поэтому его список — `[50, 100, 200]` со значением 100 по умолчанию, а у
     * VK и Telegram остаётся прежний `[50, 200, 500]` / 200.
     *
     * Оба параметра НЕобязательны и по умолчанию равны значениям ядра: панели,
     * написанные до этой развилки, не меняются ни на строку.
     *
     * Мусор в конфиге (не массив, пустой массив, значение вне списка) не
     * роняет панель и не отключает селект — падаем на значения ядра и на
     * первый пункт списка соответственно: попап без глубины бесполезен целиком,
     * а с чужой глубиной — только слегка неудобен.
     */
    const depthChoices = (Array.isArray(conf.depthChoices)
        ? conf.depthChoices.map(Number).filter((n) => Number.isFinite(n) && n > 0)
        : []);
    const DEPTHS = depthChoices.length ? depthChoices : DEPTH_CHOICES;
    const FALLBACK_DEPTH = DEPTHS.includes(DEFAULT_DEPTH) ? DEFAULT_DEPTH : DEPTHS[0];
    const CONF_DEPTH = pickDepthFrom(DEPTHS, conf.defaultDepth);
    const BASE_DEPTH = CONF_DEPTH === null ? FALLBACK_DEPTH : CONF_DEPTH;

    /** Нормализация глубины по списку ЭТОЙ панели (см. `DEPTHS` выше). */
    function pickOwnDepth(value) {
        return pickDepthFrom(DEPTHS, value);
    }

    /** Локальное состояние попапа. Наружу отдаётся снимком `debugState`. */
    const local = {
        /** Обработчики уже навешены (init идемпотентен). */
        bound: false,
        /** Открыт ли попап прямо сейчас. */
        open: false,
        /** Беседа, к которой относится содержимое попапа. */
        dialogId: null,
        /** Идущий run. */
        runId: null,
        /** `analyze` | `ask` | `refresh` — что именно запущено. */
        kind: null,
        /** POST запуска отправлен, `run_id` ещё не известен (см. п.2 в шапке). */
        pending: false,
        /** Вопрос идущего `ask`-run'а — его половина будущей пары треда. */
        question: '',
        /** Активная вкладка: `run` | `ctx`. */
        tab: 'run',
        /** Накопленные дельты до ближайшего кадра анимации. */
        buf: '',
        /** id запрошенного кадра анимации. */
        rafId: null,
        /** `rafId` получен из setTimeout, а не из requestAnimationFrame. */
        rafTimer: false,
        /**
         * Тред Q&A: `[{q, a}]`, от старых к новым.
         *
         * Наполняется полем `thread` ответа `api.getSavedAnalysis` (сервер
         * хранит тред рядом с разбором) и дополняется своими свежими парами.
         * Показывается ЦЕЛИКОМ; урезанная копия для модели считается отдельно
         * (`clampThread`).
         */
        thread: [],
        /**
         * Узел ответа ОТКРЫТОЙ строки треда — приёмник дельт идущего `ask`.
         *
         * Не индекс и не id: строка создаётся сразу при отправке вопроса (чтобы
         * пользователь видел, что вопрос принят), а дельты приходят к ней ещё
         * несколько секунд. Ссылка переживает любые вставки соседних строк, а
         * номер — нет.
         */
        askRow: null,
        /** Сохранённый разбор этой беседы уже запрошен с сервера. */
        analysisLoaded: false,
        /** На сервере есть сохранённый разбор — от этого зависит REFRESH. */
        hasAnalysis: false,
        /** Когда сохранён разбор (unix, секунды) — для строки «разобрано до …». */
        savedAt: null,
        /** Сколько сообщений было в сохранённом разборе. */
        savedUsed: 0,
        /** Сколько сообщений выгрузил ИДУЩИЙ run (из `analysis_started`). */
        runUsed: 0,
        /** Идёт GET/DELETE сохранённого разбора — кнопки ждут. */
        analysisBusy: false,
        /** Идёт DELETE треда — кнопка CLEAR Q&A ждёт ответа сервера. */
        threadBusy: false,
        /** Разбор свёрнут (виден только заголовок). Глобально, не на беседу. */
        collapsed: false,
        /**
         * REFRESH запущен: прошлый разбор в выводе ждёт замены ПЕРВОЙ дельтой.
         *
         * Не «стереть сразу»: run может упасть на запуске `claude`, и тогда
         * пользователь остался бы с пустым экраном при живом разборе на сервере.
         */
        replaceOut: false,
        /** Прошлый `scrollTop` вывода — по нему видно, что листали ВВЕРХ. */
        outScrollTop: 0,
        /** Выбранная глубина выгрузки. */
        depth: BASE_DEPTH,
        /** Заметка этой беседы уже прочитана с сервера. */
        promptLoaded: false,
        /** Текст заметки правили после последнего сохранения. */
        promptDirty: false,
        /** Потолок символов заметки — приезжает с сервера. */
        maxChars: FALLBACK_MAX_CHARS,
        /** Идёт запрос заметки (GET/PUT) — вторая кнопка ждёт. */
        promptBusy: false,
        /** Короткие сообщения наружу (тост панели). */
        onNotice: typeof conf.onNotice === 'function' ? conf.onNotice : null,
        /** Ссылка на слушатель Esc — снимается в teardown. */
        escHandler: null,
    };

    /**
     * «Следование» вывода за растущим ответом — СВОЙ инстанс общей фабрики.
     *
     * Инстанс, а не общий синглтон: тот же модуль обслуживает ленту чата, и
     * общее состояние означало бы, что доскролл попапа гасит следование ленты
     * (и наоборот) — ключи у них пересекаются, это один и тот же `dialogId`.
     */
    const follow = createFollow();

    /** Текст ошибки с подписями этой панели. */
    function errText(err) {
        return analysisErrorText(err, errorTexts);
    }

    // -----------------------------------------------------------------------
    // Хранилище
    // -----------------------------------------------------------------------

    /**
     * Читает запомненную глубину.
     *
     * localStorage может быть недоступен (Safari private mode, отключённые
     * куки) — тогда просто работает значение по умолчанию.
     *
     * @returns {?number}
     */
    function storedDepth() {
        try {
            const store = globalThis.localStorage;
            return store ? pickOwnDepth(store.getItem(DEPTH_KEY)) : null;
        } catch (_) {
            return null;
        }
    }

    /** Запоминает глубину (молча, если хранилище недоступно). */
    function rememberDepth(depth) {
        try {
            const store = globalThis.localStorage;
            if (store) store.setItem(DEPTH_KEY, String(depth));
        } catch (_) { /* приватный режим — не беда */ }
    }

    /**
     * Читает запомненное состояние сворачивания.
     *
     * Той же осторожностью, что и глубина: хранилище может бросать на ЧТЕНИИ
     * (Safari в приватном режиме, отключённые куки), и падение здесь оставило
     * бы попап без единого обработчика — модуль не догрузился бы вовсе.
     *
     * @returns {boolean}
     */
    function storedCollapsed() {
        try {
            const store = globalThis.localStorage;
            return store ? store.getItem(COLLAPSE_KEY) === '1' : false;
        } catch (_) {
            return false;
        }
    }

    /** Запоминает состояние сворачивания (молча, если хранилище недоступно). */
    function rememberCollapsed(on) {
        try {
            const store = globalThis.localStorage;
            if (store) store.setItem(COLLAPSE_KEY, on ? '1' : '0');
        } catch (_) { /* приватный режим — не беда */ }
    }

    // -----------------------------------------------------------------------
    // Мелкие операции над DOM
    // -----------------------------------------------------------------------

    function showError(message) {
        if (!refs.error) return;
        refs.error.hidden = false;
        refs.error.textContent = '>> ERR: ' + String(message || 'unknown error');
    }

    function clearError() {
        if (!refs.error) return;
        refs.error.hidden = true;
        refs.error.textContent = '';
    }

    function setStatus(text) {
        if (refs.status) refs.status.textContent = String(text == null ? '' : text);
    }

    function setPromptStatus(text) {
        if (refs.promptStatus) {
            refs.promptStatus.textContent = String(text == null ? '' : text);
        }
    }

    /**
     * Заменяет содержимое узла деревом Markdown.
     *
     * Разметка СТРОИТСЯ узлами (`js/messenger/markdown.js` отдаёт готовый
     * DocumentFragment из `createElement`/`createTextNode`) — записи разметки
     * строкой здесь нет и быть не может: текст порождён перепиской, то есть это
     * недоверенный ввод. Ссылки модуль пропускает только со схемами http/https,
     * остальные оставляет обычным текстом.
     *
     * Пустой текст — просто пустой узел: `parseMarkdown('')` дал бы пустой
     * фрагмент, но лишний проход по парсеру на каждом закрытом run'е незачем.
     *
     * @param {?object} node
     * @param {string} text
     */
    function renderMarkdownInto(node, text) {
        if (!node) return;
        const src = String(text == null ? '' : text);
        node.textContent = '';
        if (src === '') return;
        node.appendChild(renderMarkdownText(src, document));
    }

    /** Короткое сообщение наружу (тост панели), если потребитель его дал. */
    function notice(text) {
        if (typeof local.onNotice !== 'function') return;
        try { local.onNotice(text); } catch (_) { /* уведомление — не критично */ }
    }

    /** Перерисовывает строку «разобрано до …» из текущего снимка. */
    function renderUpdated() {
        if (!refs.updated) return;
        refs.updated.textContent = updatedLabel(local.savedAt, local.savedUsed);
    }

    /** Перерисовывает счётчик символов заметки. */
    function renderPromptCount() {
        if (!refs.promptCount) return;
        const used = charCount(refs.prompt ? refs.prompt.value : '');
        refs.promptCount.textContent = `${used} / ${local.maxChars}`;
    }

    // -----------------------------------------------------------------------
    // Сворачивание блока разбора
    // -----------------------------------------------------------------------

    /**
     * Показывает текущее состояние сворачивания в DOM.
     *
     * Прячется РОВНО один узел — `refs.out`. Метка «разобрано до …», строка
     * статуса и тред лежат снаружи него, поэтому остаются на экране: ради них
     * разбор и сворачивают.
     *
     * Каретку ставит код, а не CSS-псевдоэлемент: состояние обязано читаться и с
     * отключёнными стилями, и в тексте страницы, который снимает e2e.
     */
    function applyCollapsed() {
        if (refs.out) refs.out.hidden = local.collapsed;
        if (!refs.outToggle) return;
        refs.outToggle.setAttribute('aria-expanded', local.collapsed ? 'false' : 'true');
        refs.outToggle.classList.toggle(`${prefix}-analysis-out-toggle-off`, local.collapsed);
        const caret = refs.outToggle.querySelector
            ? refs.outToggle.querySelector(`.${prefix}-analysis-out-caret`)
            : null;
        if (caret) caret.textContent = local.collapsed ? '▸' : '▾';
    }

    /**
     * Ставит состояние сворачивания и запоминает его.
     *
     * @param {boolean} on свернуть?
     */
    function setOutCollapsed(on) {
        local.collapsed = !!on;
        rememberCollapsed(local.collapsed);
        applyCollapsed();
    }

    /** Клик по шапке разбора. */
    function toggleOutCollapsed() {
        setOutCollapsed(!local.collapsed);
    }

    /**
     * Разворачивает разбор перед стартом run'а.
     *
     * Обязательный шаг, а не удобство: дельты льются в `refs.out`, и свёрнутый
     * блок означал бы текст, текущий в невидимое, — пользователь смотрел бы на
     * пустой экран и считал, что сломано. Состояние ЗАПОМИНАЕТСЯ: он увидит
     * разбор развёрнутым и после перезагрузки.
     */
    function expandOutForRun() {
        if (!local.collapsed) return;
        setOutCollapsed(false);
    }

    /**
     * Приводит кнопки в состояние, соответствующее текущему run'у.
     *
     * ANALYZE и ASK гаснут на время разбора, STOP показывается только тогда:
     * второй запуск по той же беседе сервер всё равно отвергнет `invalid_state`,
     * и честнее не дать нажать, чем показать ошибку.
     *
     * `pending` учитывается наравне с `runId`: между нажатием и ответом POST'а
     * проходит целая выгрузка истории — за это время двойной клик успевает дать
     * второй запрос. Сервер его тоже отобьёт (дедуп по беседе под одним
     * lock'ом), но кнопка обязана гаснуть СРАЗУ.
     *
     * REFRESH заблокирован, пока сохранённого разбора нет: обновлять нечего, и
     * сервер такой запрос отвергает `400 invalid_input`. Показать ошибку вместо
     * погашенной кнопки значило бы обвинить пользователя в том, что он не мог
     * знать.
     *
     * CLEAR анализа тоже гаснет на время run'а, и это не косметика: удачный run
     * СОХРАНЯЕТ разбор в финале (хаб), и удаление на середине дало бы
     * «воскресший» текст через секунду после нажатия.
     */
    function syncButtons() {
        const running = local.runId !== null || local.pending;
        if (refs.start) refs.start.disabled = running;
        if (refs.send) refs.send.disabled = running;
        if (refs.cancel) refs.cancel.hidden = !running;
        const stale = running || local.analysisBusy;
        if (refs.refresh) refs.refresh.disabled = stale || !local.hasAnalysis;
        if (refs.clear) refs.clear.disabled = stale || !local.hasAnalysis;
        // CLEAR Q&A ходит в сеть (свой DELETE), поэтому гаснет и на время
        // собственного запроса: два DELETE подряд второй раз ответят
        // `{cleared:false}`, но пользователь увидел бы «мигающую» кнопку.
        if (refs.clearThread) refs.clearThread.disabled = running || local.threadBusy;
        const busy = local.promptBusy;
        if (refs.promptSave) refs.promptSave.disabled = busy;
        if (refs.promptClear) refs.promptClear.disabled = busy;
    }

    // -----------------------------------------------------------------------
    // Вкладки
    // -----------------------------------------------------------------------

    /**
     * Переключает вкладку попапа.
     *
     * Первый вход на «Контекст» тянет заметку с сервера (ленивость — см.
     * решение 3 в шапке).
     *
     * @param {'run'|'ctx'} tab
     */
    function setTab(tab) {
        const next = tab === 'ctx' ? 'ctx' : 'run';
        local.tab = next;
        if (refs.paneRun) refs.paneRun.hidden = next !== 'run';
        if (refs.paneCtx) refs.paneCtx.hidden = next !== 'ctx';
        if (refs.tabRun) {
            refs.tabRun.setAttribute('aria-selected', next === 'run' ? 'true' : 'false');
            refs.tabRun.classList.toggle(`${prefix}-analysis-tab-active`, next === 'run');
        }
        if (refs.tabCtx) {
            refs.tabCtx.setAttribute('aria-selected', next === 'ctx' ? 'true' : 'false');
            refs.tabCtx.classList.toggle(`${prefix}-analysis-tab-active`, next === 'ctx');
        }
        if (next === 'ctx') void ensurePrompt();
    }

    // -----------------------------------------------------------------------
    // Вкладка «Контекст чата»
    // -----------------------------------------------------------------------

    /**
     * Читает заметку беседы, если она ещё не прочитана.
     *
     * РОВНО ОДИН GET на беседу: флаг ставится ДО запроса, поэтому и повторное
     * переключение вкладки, и параллельный вход второй кнопкой дают один запрос.
     * При ошибке флаг снимается — иначе беседа осталась бы без заметки навсегда.
     *
     * @returns {Promise<void>}
     */
    async function ensurePrompt() {
        if (local.promptLoaded) return;
        if (!local.dialogId) return;
        const dialogId = local.dialogId;
        local.promptLoaded = true;
        setPromptStatus('загрузка…');
        try {
            const dto = await api.getPrompt(dialogId);
            // Пока ждали очереди, пользователь мог переключить беседу: чужую
            // заметку в поле подставлять нельзя — он бы её отредактировал и
            // сохранил не туда.
            if (local.dialogId !== dialogId) return;
            applyPromptDto(dto);
            setPromptStatus(dto && dto.updated_at ? 'сохранено ранее' : 'заметки нет');
        } catch (err) {
            if (local.dialogId === dialogId) local.promptLoaded = false;
            setPromptStatus('');
            showError(errText(err));
        }
    }

    /** Кладёт ответ роута заметки в поле и счётчик. */
    function applyPromptDto(dto) {
        const text = (dto && typeof dto.prompt === 'string') ? dto.prompt : '';
        if (dto && Number.isFinite(dto.max_chars) && dto.max_chars > 0) {
            local.maxChars = dto.max_chars;
        }
        if (refs.prompt) refs.prompt.value = text;
        local.promptDirty = false;
        renderPromptCount();
    }

    /**
     * Сохраняет заметку.
     *
     * Текст режется тем же правилом, что и на сервере (`clampPrompt`), и
     * подставляется обратно в поле: пользователь обязан видеть то, что уехало.
     *
     * @param {?string} [override] текст вместо содержимого поля (CLEAR шлёт '')
     * @returns {Promise<boolean>} удалось ли сохранить
     */
    async function savePrompt(override) {
        if (!local.dialogId) return false;
        if (local.promptBusy) return false;
        const dialogId = local.dialogId;
        const raw = override != null
            ? String(override)
            : (refs.prompt ? refs.prompt.value : '');
        const text = clampPrompt(raw, local.maxChars);
        local.promptBusy = true;
        syncButtons();
        setPromptStatus('сохранение…');
        clearError();
        try {
            const dto = await api.putPrompt(dialogId, text);
            if (local.dialogId === dialogId) {
                applyPromptDto(dto);
                // Заметка на сервере теперь совпадает с полем: перечитывать её
                // при следующем входе на вкладку незачем.
                local.promptLoaded = true;
                setPromptStatus(text ? 'сохранено' : 'заметка стёрта');
            }
            return true;
        } catch (err) {
            setPromptStatus('');
            showError(errText(err));
            notice('Заметка не сохранена: ' + errText(err));
            return false;
        } finally {
            local.promptBusy = false;
            syncButtons();
        }
    }

    /** CLEAR: пустая строка — единственный способ стереть заметку. */
    async function clearPromptText() {
        if (refs.prompt) refs.prompt.value = '';
        renderPromptCount();
        await savePrompt('');
    }

    // -----------------------------------------------------------------------
    // Сохранённый разбор: подгрузка, обновление метки, очистка
    // -----------------------------------------------------------------------

    /**
     * Кладёт ответ «сохранённый разбор» в вывод и в метку.
     *
     * Пустой текст — штатное «разбора ещё не было»: вывод чистится, REFRESH и
     * CLEAR гаснут. Именно поэтому dto с пустым `text` не игнорируется, а
     * применяется: иначе после DELETE во второй вкладке первая продолжала бы
     * показывать стёртый текст как живой.
     *
     * @param {?object} dto
     */
    function applySavedAnalysis(dto) {
        const text = (dto && typeof dto.text === 'string') ? dto.text : '';
        const at = dto && Number.isFinite(dto.updated_at) ? Number(dto.updated_at) : null;
        const used = dto && Number.isFinite(dto.messages_used) ? Number(dto.messages_used) : 0;
        local.hasAnalysis = text !== '';
        local.savedAt = local.hasAnalysis ? at : null;
        local.savedUsed = local.hasAnalysis ? used : 0;
        clearOut();
        if (local.hasAnalysis && refs.out) {
            // Сохранённый разбор — ЦЕЛЬНЫЙ текст, а не поток: разметка в нём
            // закрыта, и рисовать его сырым, чтобы через мгновение перерисовать,
            // незачем. Стадия сырого текста существует только ради стрима.
            renderMarkdownInto(refs.out, text);
        }
        renderUpdated();
        syncButtons();
    }

    /**
     * Кладёт тред из ответа «сохранённый разбор» в состояние и в DOM.
     *
     * Вызывается ТОЛЬКО из пути подгрузки: очистка разбора тоже зовёт
     * `applySavedAnalysis`, и если бы тред жил внутри неё, кнопка CLEAR ✕
     * стирала бы с экрана вопросы, которые на сервере целы.
     *
     * @param {*} list поле `thread` ответа (уже нормализовано транспортом)
     */
    function applyThread(list) {
        local.thread = clampThreadForView(list);
        renderThread();
    }

    /**
     * Читает сохранённый разбор, если он ещё не читался.
     *
     * РОВНО ОДИН GET на беседу — тем же приёмом, что заметка (`ensurePrompt`):
     * флаг ставится ДО запроса, при ошибке снимается. Бюджет запросов панели
     * общий, а попап открывают и закрывают десятками нажатий.
     *
     * Идущий run — стоп-сигнал: пока в вывод льются дельты, класть туда прошлый
     * текст нельзя, он оказался бы вперемешку с новым.
     *
     * @returns {Promise<void>}
     */
    async function ensureSavedAnalysis() {
        if (local.analysisLoaded) return;
        if (!local.dialogId) return;
        if (local.runId !== null || local.pending) return;
        const dialogId = local.dialogId;
        local.analysisLoaded = true;
        local.analysisBusy = true;
        syncButtons();
        try {
            const dto = await api.getSavedAnalysis(dialogId);
            // Пока ждали очереди, беседу могли переключить — чужой разбор в этом
            // попапе показывать нельзя.
            if (local.dialogId !== dialogId) return;
            if (local.runId !== null || local.pending) return;
            applySavedAnalysis(dto);
            // Тред приезжает ТЕМ ЖЕ ответом: отдельного запроса под него нет, и
            // это единственная причина, по которой история вопросов переживает
            // перезагрузку страницы.
            applyThread(dto && dto.thread);
        } catch (err) {
            if (local.dialogId === dialogId) local.analysisLoaded = false;
            // Попап остаётся рабочим: ANALYZE и вопросы не зависят от прошлого
            // разбора, поэтому это строка ошибки, а не отказ открыться.
            showError(errText(err));
        } finally {
            if (local.dialogId === dialogId) {
                local.analysisBusy = false;
                syncButtons();
            }
        }
    }

    /**
     * CLEAR ✕ — стереть сохранённый разбор НА СЕРВЕРЕ.
     *
     * Ровно один DELETE: локальное состояние правится только после успеха, иначе
     * вкладка показывала бы пустоту, а перезагрузка возвращала бы текст.
     *
     * @returns {Promise<boolean>} стёрли ли
     */
    async function clearAnalysis() {
        if (!local.dialogId) return false;
        if (local.analysisBusy) return false;
        if (local.runId !== null || local.pending) return false;
        const dialogId = local.dialogId;
        local.analysisBusy = true;
        syncButtons();
        clearError();
        setStatus('стираю разбор…');
        try {
            await api.clearAnalysis(dialogId);
            if (local.dialogId !== dialogId) return true;
            applySavedAnalysis(null);
            // Разбор на сервере теперь заведомо пуст — перечитывать нечего.
            local.analysisLoaded = true;
            setStatus('разбор стёрт');
            return true;
        } catch (err) {
            setStatus('');
            showError(errText(err));
            notice('Разбор не стёрт: ' + errText(err));
            return false;
        } finally {
            if (local.dialogId === dialogId) {
                local.analysisBusy = false;
                syncButtons();
            }
        }
    }

    /**
     * CLEAR Q&A — стереть тред вопросов НА СЕРВЕРЕ.
     *
     * Тред хранится рядом с разбором, поэтому локальной очистки мало: стёртые
     * «с глаз» пары вернулись бы первой же перезагрузкой. Запрос идёт по СВОЕМУ
     * адресу — общий DELETE разбора треда не касается.
     *
     * Порядок строгий: сначала успех сервера, потом чистка экрана. Обратный
     * порядок при упавшем запросе показал бы пустоту вместо целого треда, и
     * пользователь узнал бы правду только при следующем открытии попапа.
     *
     * Разбор в выводе не трогается ни в одной ветке: это вторая кнопка.
     *
     * @returns {Promise<boolean>} стёрли ли
     */
    async function clearThreadTurns() {
        if (!local.dialogId) return false;
        if (local.threadBusy) return false;
        if (local.runId !== null || local.pending) return false;
        const dialogId = local.dialogId;
        local.threadBusy = true;
        syncButtons();
        clearError();
        try {
            await api.clearThread(dialogId);
            // Пока ждали очереди, беседу могли переключить: чужой тред с экрана
            // стирать нельзя — стёрли-то на сервере не его.
            if (local.dialogId !== dialogId) return true;
            local.thread = [];
            local.askRow = null;
            if (refs.thread) {
                refs.thread.textContent = '';
                refs.thread.scrollTop = 0;
            }
            return true;
        } catch (err) {
            showError(errText(err));
            notice('Вопросы не стёрты: ' + errText(err));
            return false;
        } finally {
            if (local.dialogId === dialogId) {
                local.threadBusy = false;
                syncButtons();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Открытие и закрытие
    // -----------------------------------------------------------------------

    /** Открыт ли попап прямо сейчас. Читается гейтом `panelVisible` в keynav. */
    function isOpen() {
        return local.open;
    }

    /**
     * Сбрасывает всё, что относится к КОНКРЕТНОЙ беседе.
     *
     * Вызывается при смене беседы: заметка, тред и идущий run принадлежат ей, а
     * не попапу. Не путать с `teardown` — тот снимает слушатели.
     */
    function resetForDialog() {
        local.runId = null;
        local.kind = null;
        local.pending = false;
        local.question = '';
        local.buf = '';
        local.thread = [];
        local.askRow = null;
        local.outScrollTop = 0;
        follow.clearFollow();
        local.promptLoaded = false;
        local.promptDirty = false;
        // Сохранённый разбор принадлежит беседе ровно так же, как заметка: без
        // сброса вторая беседа показала бы разбор первой и предложила бы его
        // «обновить».
        local.analysisLoaded = false;
        local.hasAnalysis = false;
        local.savedAt = null;
        local.savedUsed = 0;
        local.runUsed = 0;
        cancelFlush();
        if (refs.out) {
            refs.out.textContent = '';
            refs.out.scrollTop = 0;
        }
        if (refs.thread) refs.thread.textContent = '';
        if (refs.prompt) refs.prompt.value = '';
        if (refs.question) refs.question.value = '';
        setStatus('');
        setPromptStatus('');
        clearError();
        renderUpdated();
        renderPromptCount();
    }

    /**
     * Открывает попап.
     *
     * Без аргумента берётся активная беседа панели: кнопка ✦ в титлбаре не
     * знает про список, а открывать анализ «ни по чему» бессмысленно.
     *
     * @param {?string} [dialogId]
     */
    function open(dialogId) {
        const fromPanel = activeDialogId();
        const id = dialogId != null && String(dialogId) !== ''
            ? String(dialogId)
            : (fromPanel != null ? String(fromPanel) : null);
        if (!id) {
            notice('Сначала откройте беседу — анализировать нечего.');
            return;
        }
        if (local.dialogId !== id) {
            local.dialogId = id;
            resetForDialog();
        }
        local.open = true;
        if (refs.root) refs.root.hidden = false;
        if (refs.target) refs.target.textContent = dialogLabel(id);
        // Селект — источник истины для показанного значения, но чужое значение в
        // нём (кэш старой разметки) на глубину запроса не влияет.
        const picked = pickOwnDepth(refs.depth ? refs.depth.value : null);
        if (picked !== null) local.depth = picked;
        else if (refs.depth) refs.depth.value = String(local.depth);
        syncButtons();
        setTab(local.tab);
        // Прошлый разбор — первое, что пользователь ждёт увидеть в открытом
        // попапе; он лежит на сервере и переживает перезагрузку страницы.
        void ensureSavedAnalysis();
    }

    /**
     * Закрывает попап.
     *
     * Идущий run НЕ отменяется: закрытие окна — это «уберите с глаз», а не
     * «стоп». Кадры продолжают приходить, и повторное открытие показывает
     * готовый ответ. Для остановки есть кнопка STOP.
     */
    function close() {
        if (!local.open) return;
        local.open = false;
        if (refs.root) refs.root.hidden = true;
        clearError();
    }

    // -----------------------------------------------------------------------
    // Вывод модели: буфер, слив кадром анимации, автоскролл
    // -----------------------------------------------------------------------

    /** Прокручен ли вывод к нижнему краю (с допуском `BOTTOM_EPS_PX`). */
    function outAtBottom() {
        const node = refs.out;
        if (!node) return true;
        const top = Number(node.scrollTop) || 0;
        const height = Number(node.scrollHeight) || 0;
        const view = Number(node.clientHeight) || 0;
        return height - top - view <= BOTTOM_EPS_PX;
    }

    /**
     * Доскроллить вывод, если пользователь этого ждёт.
     *
     * Решение принимает общая фабрика: `follow` — «пользователь запустил разбор
     * и не уходил вверх», `stick` — «он и так у нижнего края». В обоих случаях
     * цель считает `scrollTargetForRow(node, null)` — самый низ; вверх она не
     * тянет никогда.
     */
    function autoscrollOut() {
        const node = refs.out;
        if (!node) return;
        const now = Date.now();
        const decision = follow.scrollDecision({
            atBottom: outAtBottom(),
            following: follow.isFollowing(local.dialogId, now),
        });
        if (decision === 'none') return;
        const want = follow.scrollTargetForRow(node, null);
        if (want !== null) node.scrollTop = want;
        local.outScrollTop = Number(node.scrollTop) || 0;
        // Продлеваем окно следования: разбор идёт дольше, чем FOLLOW_TTL_MS, и
        // без продления следование гасло бы на середине ответа.
        if (decision === 'follow') follow.noteFollow(local.dialogId, 0, now);
    }

    /**
     * Куда лить дельты идущего run'а.
     *
     * Признак ровно один — открытая строка треда: её заводит только `ask`, и
     * заводит СИНХРОННО, до первого запроса в сеть. Смотреть здесь на
     * `local.kind` было бы хуже: `kind` может приехать кадром
     * `analysis_started`, то есть уже после первых дельт, и часть ответа успела
     * бы уехать не туда.
     *
     * @returns {?object} узел-приёмник
     */
    function deltaTarget() {
        if (local.askRow) return local.askRow.answer;
        return refs.out;
    }

    /** Выливает накопленные дельты в DOM. Пустой буфер — не работа. */
    function flushBuffer() {
        local.rafId = null;
        const chunk = local.buf;
        local.buf = '';
        if (chunk === '') return;
        const target = deltaTarget();
        if (target === refs.out && local.replaceOut) {
            // Первая дельта REFRESH'а: прошлый разбор уступает место новому — но
            // только теперь, когда есть чем его заменить.
            local.replaceOut = false;
            if (refs.out) {
                refs.out.textContent = '';
                refs.out.scrollTop = 0;
                local.outScrollTop = 0;
            }
        }
        if (target) {
            // Текстовый узел, а не `textContent +=`: конкатенация переклеивала бы
            // весь ответ на каждой дельте (квадрат по длине), и это ЕДИНСТВЕННЫЙ
            // способ дописать текст, не записывая разметку строкой.
            target.appendChild(document.createTextNode(chunk));
        }
        if (local.askRow) scrollThreadToBottom();
        else autoscrollOut();
    }

    /** Просит кадр анимации, если он ещё не запрошен. */
    function scheduleFlush() {
        if (local.rafId !== null) return;
        if (typeof requestAnimationFrame === 'function') {
            local.rafTimer = false;
            local.rafId = requestAnimationFrame(() => flushBuffer());
            return;
        }
        // Среда без rAF (headless-тесты, старый WebView) — тот же кадр, только
        // таймером: терять дельты из-за отсутствия API нельзя.
        local.rafTimer = true;
        local.rafId = setTimeout(() => flushBuffer(), 16);
    }

    /**
     * Отменяет запрошенный кадр, НЕ теряя содержимое буфера.
     *
     * Чем именно отменять, решает `rafTimer`: идентификаторы rAF и таймеров
     * живут в РАЗНЫХ пространствах, и `clearTimeout(rafId)` погасил бы чужой
     * таймер с тем же числом.
     */
    function cancelFlush() {
        if (local.rafId === null) return;
        if (local.rafTimer) clearTimeout(local.rafId);
        else if (typeof cancelAnimationFrame === 'function') cancelAnimationFrame(local.rafId);
        local.rafId = null;
    }

    /** Немедленно (синхронно) дописывает остаток буфера — финал run'а. */
    function flushNow() {
        cancelFlush();
        flushBuffer();
    }

    /**
     * Стирает вывод РАЗБОРА и всё, что к нему относится.
     *
     * Вызывается ровно из трёх мест: ANALYZE (пользователь просит разобрать
     * заново), очистка сохранённого разбора и смена беседы. Из пути вопроса —
     * НИКОГДА: ровно эта строчка и была исходной жалобой.
     */
    function clearOut() {
        cancelFlush();
        local.buf = '';
        local.replaceOut = false;
        if (refs.out) {
            refs.out.textContent = '';
            refs.out.scrollTop = 0;
        }
        local.outScrollTop = 0;
    }

    /** Доскроллить тред к последней строке. */
    function scrollThreadToBottom() {
        if (!refs.thread) return;
        const view = Number(refs.thread.clientHeight) || 0;
        const height = Number(refs.thread.scrollHeight) || 0;
        refs.thread.scrollTop = Math.max(0, height - view);
    }

    /**
     * Открывает НОВУЮ строку треда под вопрос и делает её приёмником дельт.
     *
     * Строка появляется СРАЗУ при отправке вопроса, ещё пустой половиной
     * ответа: иначе между нажатием ASK и первой дельтой (выгрузка истории —
     * секунды) на экране не менялось бы ничего, кроме строки статуса.
     *
     * Узлы собираются createElement + textContent: в ответе модели встречается
     * что угодно, включая разметку из самой переписки.
     *
     * ОДНА функция и на свежие пары, и на приехавшие с сервера: второй путь
     * отрисовки означал бы, что сохранённый вопрос выглядит не так, как тот же
     * вопрос минуту назад.
     *
     * @param {string} q
     * @returns {?{box: object, answer: object}}
     */
    function openThreadRow(q) {
        if (!refs.thread) return null;
        const box = document.createElement('div');
        box.className = `${prefix}-analysis-qa`;
        const qq = document.createElement('div');
        qq.className = `${prefix}-analysis-qa-q`;
        // Вопрос остаётся ТЕКСТОМ, даже когда ответ станет Markdown'ом: его
        // писал человек в однострочном поле, разметки он туда не закладывал, а
        // звёздочки и решётки в живом вопросе встречаются чаще, чем разметка.
        qq.textContent = '> ' + String(q == null ? '' : q);
        const aa = document.createElement('div');
        aa.className = `${prefix}-analysis-qa-a`;
        box.appendChild(qq);
        box.appendChild(aa);
        refs.thread.appendChild(box);
        scrollThreadToBottom();
        return { box, answer: aa };
    }

    /**
     * Перерисовывает тред целиком из `local.thread`.
     *
     * Целиком, а не «дописать недостающее»: сравнивать DOM со списком пришлось
     * бы по содержимому (у пар нет id), и первая же расхожая пара дала бы дубль.
     * Строк здесь десяток-другой — перерисовка дешевле сверки.
     *
     * Ответы — уже завершённые тексты, поэтому рисуются Markdown'ом сразу, без
     * стадии сырого текста (она нужна только стриму).
     */
    function renderThread() {
        if (!refs.thread) return;
        refs.thread.textContent = '';
        for (const turn of local.thread) {
            const row = openThreadRow(turn.q);
            if (row) renderMarkdownInto(row.answer, turn.a);
        }
        scrollThreadToBottom();
    }

    // -----------------------------------------------------------------------
    // Обработчики кадров WS
    // -----------------------------------------------------------------------
    //
    // Все четыре зовутся из раскладки кадров панели и обязаны переживать мусор:
    // кадр приходит из сети, а `null`/кадр без полей уронил бы обработчик, а с
    // ним — всю раскладку кадров.

    /**
     * Кадр относится к тому, что показано в попапе?
     *
     * ДВЕ проверки, и обе обязательны: беседа (broadcast не фильтрован сервером)
     * и run (по одной беседе можно успеть запустить разбор дважды — второй раз
     * после отмены первого, и хвост старого run'а дописался бы в новый вывод).
     *
     * @param {*} evt
     * @returns {boolean}
     */
    function frameIsOurs(evt) {
        if (!evt || typeof evt !== 'object') return false;
        if (local.dialogId === null) return false;
        if (String(evt.dialog_id) !== String(local.dialogId)) return false;
        if (local.runId === null) return false;
        return String(evt.run_id) === String(local.runId);
    }

    /** `analysis_started` — разбор начался: сервер сообщил объём выгрузки. */
    function onStarted(evt) {
        if (!evt || typeof evt !== 'object') return;
        if (local.dialogId === null) return;
        if (String(evt.dialog_id) !== String(local.dialogId)) return;
        if (local.runId === null) {
            // Усыновление (см. п.2 в шапке): кадр обгоняет ответ POST'а, и без
            // этого первые дельты отфильтровались бы как чужие.
            if (!local.pending) return;
            if (evt.run_id == null) return;
            local.runId = String(evt.run_id);
        } else if (String(evt.run_id) !== String(local.runId)) {
            return;
        }
        if (evt.kind === 'ask' || evt.kind === 'analyze' || evt.kind === 'refresh') {
            local.kind = evt.kind;
        }
        const used = Number(evt.messages_used);
        local.runUsed = Number.isFinite(used) ? used : 0;
        const parts = [`${local.runUsed} сообщений`];
        if (evt.truncated === true) parts.push('усечено');
        parts.push(local.kind === 'ask' ? 'думаю над ответом…' : 'идёт разбор…');
        setStatus(parts.join(' · '));
        // ВОПРОС НЕ СТИРАЕТ РАЗБОР. Прошлый вывод затирается только тем, что его
        // и рисует, — полным разбором и его обновлением.
        //
        // И даже там разница есть: ANALYZE чистит СРАЗУ (пользователь попросил
        // разобрать заново, старый текст ему больше не нужен), а REFRESH —
        // только перед первой дельтой. Модель может не запуститься вовсе, и
        // терять из-за этого разбор, который на сервере цел, было бы обидно
        // вдвойне.
        if (local.kind === 'analyze') clearOut();
        else if (local.kind === 'refresh') local.replaceOut = true;
        clearError();
        // Пользователь только что сам нажал кнопку — за результатом своего
        // нажатия он должен уехать, даже если лента вывода была прокручена.
        follow.noteFollow(local.dialogId, 0, Date.now());
        syncButtons();
    }

    /** `analysis_delta` — кусок ответа. Копится в буфере до кадра анимации. */
    function onDelta(evt) {
        if (!frameIsOurs(evt)) return;
        const text = evt.text;
        if (typeof text !== 'string' || text === '') return;
        local.buf += text;
        scheduleFlush();
    }

    /**
     * `analysis_done` — run закончился (успехом, отменой или таймаутом).
     *
     * Буфер сливается СИНХРОННО: запрошенный кадр анимации мог ещё не
     * наступить, а последний кусок ответа — как раз тот, ради которого всё и
     * запускалось.
     */
    function onDone(evt) {
        if (!frameIsOurs(evt)) return;
        flushNow();
        const ok = evt.ok !== false;
        const reason = evt.reason == null ? null : String(evt.reason);
        if (reason === 'cancelled') setStatus('остановлено');
        else if (reason === 'timeout') setStatus('превышено время ожидания');
        else if (!ok) setStatus('разбор не завершён');
        else setStatus(doneStatus(evt));
        // Разметку рисуем ТОЛЬКО у целого ответа. Оборванный (отмена, таймаут,
        // ошибка) остаётся сырым текстом: у него незакрыты таблицы и списки, и
        // «дорисовать» их значило бы показать структуру, которой модель не
        // писала.
        const whole = ok && reason === null;
        if (local.kind === 'ask') {
            // Пара уезжает в историю следующего запроса: сервер не помнит, о чём
            // его спрашивали, — он лишь ХРАНИТ тред. В DOM пара уже есть: строка
            // треда открыта с момента отправки вопроса и всё это время принимала
            // дельты, поэтому дописывать сюда нечего.
            //
            // Сырой текст снимается ДО перерисовки Markdown'ом: после неё
            // `textContent` вернул бы разметку без символов разметки, и в
            // историю модели уехал бы ответ, которого она не давала.
            const answer = local.askRow ? String(local.askRow.answer.textContent || '') : '';
            if (local.question || answer) {
                local.thread.push({ q: local.question, a: answer });
                // Показанное режется ТОЛЬКО по числу пар (как на сервере).
                // `clampThread` здесь была бы ошибкой: она считает бюджет
                // промпта и стирала бы с экрана вопросы, которые сервер хранит.
                local.thread = clampThreadForView(local.thread);
            }
            if (whole && local.askRow) renderMarkdownInto(local.askRow.answer, answer);
        } else if (ok) {
            // Удачные analyze/refresh сервер СОХРАНИЛ (хаб) — метка обязана это
            // показать, иначе REFRESH выглядел бы не сработавшим.
            local.hasAnalysis = true;
            local.savedAt = Math.floor(Date.now() / 1000);
            local.savedUsed = local.runUsed;
            local.analysisLoaded = true;
            renderUpdated();
            // Перерисовка идёт ПОСЛЕ `flushNow()` выше: буфер дельт уже слит, и
            // `textContent` блока — тот самый цельный сырой ответ, который
            // сервер положил себе в хранилище. Отдельного накопителя ради этого
            // не заводим: он был бы второй копией того же текста.
            if (whole && refs.out) {
                renderMarkdownInto(refs.out, String(refs.out.textContent || ''));
            }
        }
        endRun();
    }

    /**
     * Общий финал run'а: попап снова принадлежит пользователю.
     *
     * `askRow` отпускается ЗДЕСЬ, а не в обработчиках: пока ссылка жива, дельты
     * следующего разбора ушли бы в строку прошлого вопроса.
     */
    function endRun() {
        local.runId = null;
        local.kind = null;
        local.pending = false;
        local.question = '';
        local.askRow = null;
        local.replaceOut = false;
        follow.clearFollow();
        syncButtons();
    }

    /** Строка статуса удачного финала — размер счёта токенов, если он пришёл. */
    function doneStatus(evt) {
        const inTok = Number(evt.input_tokens);
        const outTok = Number(evt.output_tokens);
        if (Number.isFinite(inTok) && Number.isFinite(outTok) && (inTok > 0 || outTok > 0)) {
            return `готово · токены ${inTok} → ${outTok}`;
        }
        return 'готово';
    }

    /**
     * `analysis_error` — процесс не запустился или упал.
     *
     * Уже полученный текст НЕ стирается: частичный разбор полезнее пустого
     * экрана, а причина видна отдельной строкой.
     */
    function onError(evt) {
        if (!frameIsOurs(evt)) return;
        flushNow();
        showError(evt.message ? String(evt.message) : 'разбор прерван');
        setStatus('');
        endRun();
    }

    // -----------------------------------------------------------------------
    // Запуск, вопрос, отмена
    // -----------------------------------------------------------------------

    /**
     * Отправляет запуск разбора и берёт run под управление попапа.
     *
     * `attached: true` в ответе — НЕ ошибка: мы подключились к уже идущему run'у
     * (второй клик, вторая вкладка), и его кадры пойдут в этот попап так же.
     *
     * `no_new_messages: true` (только у `refresh`) — тоже не ошибка и не run:
     * `run_id` в этом ответе `null`, модель НЕ запускалась, кадров не будет.
     * Попытка их дождаться оставила бы попап в «идёт разбор…» навсегда.
     *
     * @param {'analyze'|'ask'|'refresh'} kind
     * @param {string} [question]
     * @returns {Promise<boolean>} ушёл ли запрос
     */
    async function startRun(kind, question) {
        if (!local.dialogId) return false;
        if (local.runId !== null || local.pending) return false;
        // `analysisBusy` здесь НЕ проверяется намеренно. Подгрузка сохранённого
        // разбора идёт первые доли секунды после открытия попапа, и ровно в это
        // время пользователь жмёт ANALYZE. Отказ был бы молчаливым — кнопка-то
        // доступна. Гонки нет: `pending` ставится синхронно ниже, а
        // `ensureSavedAnalysis` перечитывает его ПОСЛЕ await и в вывод не лезет.
        const dialogId = local.dialogId;
        const q = String(question == null ? '' : question).trim();
        if (kind === 'ask' && q === '') return false;
        // Кнопка уже погашена `syncButtons`, но клик по ней приходит и из
        // клавиатуры, и из e2e — обновлять нечего, значит и запроса быть не
        // должно.
        if (kind === 'refresh' && !local.hasAnalysis) return false;
        local.pending = true;
        local.kind = kind;
        local.question = kind === 'ask' ? q.slice(0, MAX_QUESTION_CHARS) : '';
        clearError();
        if (kind === 'ask') {
            setStatus('вопрос отправлен…');
            // Строка треда заводится ДО сети: она и подтверждение приёма
            // вопроса, и приёмник дельт (см. `deltaTarget`).
            local.askRow = openThreadRow(local.question);
        } else if (kind === 'refresh') {
            setStatus('ищу новые сообщения…');
            expandOutForRun();
        } else {
            setStatus('выгружаю переписку…');
            clearOut();
            expandOutForRun();
        }
        syncButtons();
        try {
            const dto = await api.start(dialogId, {
                kind,
                question: kind === 'ask' ? local.question : undefined,
                depth: local.depth,
                history: kind === 'ask' ? clampThread(local.thread) : undefined,
            });
            // Пока ждали очереди и ответа, пользователь мог сменить беседу:
            // чужой run в этом попапе не наш.
            if (local.dialogId !== dialogId) return false;
            if (dto && dto.no_new_messages === true) {
                // Разбор на экране и на сервере остаётся ровно таким, каким был.
                setStatus('новых сообщений нет');
                local.kind = null;
                local.replaceOut = false;
                return true;
            }
            // `analysis_started` мог обогнать ответ и уже усыновить run_id —
            // тогда перезаписывать нечего (и нечем: id тот же).
            if (local.runId === null && dto && dto.run_id) local.runId = String(dto.run_id);
            if (kind === 'ask' && refs.question) refs.question.value = '';
            return true;
        } catch (err) {
            local.kind = null;
            local.question = '';
            local.replaceOut = false;
            // Вопрос не уехал — пустая строка треда осталась бы висеть немым
            // упрёком; убираем её вместе со ссылкой.
            if (local.askRow) {
                if (local.askRow.box && local.askRow.box.remove) local.askRow.box.remove();
                local.askRow = null;
            }
            setStatus('');
            showError(startErrorText(kind, err));
            return false;
        } finally {
            if (local.dialogId === dialogId) {
                local.pending = false;
                syncButtons();
            }
        }
    }

    /**
     * Текст ошибки запуска — с поправкой на REFRESH.
     *
     * `invalid_input` у него значит ровно одно: сохранённого разбора на сервере
     * уже нет (стёрли во второй вкладке). Общая формулировка «проверьте вопрос
     * и глубину» отправила бы пользователя чинить то, что не сломано.
     *
     * @param {string} kind
     * @param {?Error & {code?: ?string}} err
     * @returns {string}
     */
    function startErrorText(kind, err) {
        if (kind === 'refresh' && err && err.code === 'invalid_input') {
            return 'Обновлять нечего: сохранённого разбора нет. Запустите полный разбор.';
        }
        return errText(err);
    }

    /** ANALYZE: разбор всей выгруженной переписки. */
    async function startAnalysis() {
        return startRun('analyze');
    }

    /**
     * REFRESH: дочитать только новое и заменить разбор обновлённым цельным.
     *
     * Модель получает прошлый разбор и сообщения новее него, а возвращает не
     * список изменений, а ЦЕЛЬНУЮ картину — она и заменяет прошлую.
     */
    async function refreshAnalysis() {
        return startRun('refresh');
    }

    /** ASK: вопрос по той же переписке. Пустой вопрос запроса не порождает. */
    async function askQuestion(text) {
        const raw = text != null ? text : (refs.question ? refs.question.value : '');
        return startRun('ask', raw);
    }

    /**
     * STOP: гасит идущий run.
     *
     * Ошибка ответа НЕ показывается: «run не найден» — штатный исход гонки
     * «нажали стоп ровно тогда, когда модель дописала ответ», и обе стороны
     * этой гонки для пользователя выглядят одинаково — генерация прекратилась.
     *
     * @returns {Promise<void>}
     */
    async function cancelRun() {
        if (!local.dialogId) return;
        if (local.runId === null && !local.pending) return;
        const dialogId = local.dialogId;
        const runId = local.runId;
        setStatus('останавливаю…');
        try {
            await api.cancel(dialogId, runId);
        } catch (_) {
            // Молчим намеренно (см. док выше).
        } finally {
            if (local.dialogId === dialogId) {
                flushNow();
                setStatus('остановлено');
                endRun();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Жизненный цикл
    // -----------------------------------------------------------------------

    /**
     * Навешивает обработчики попапа. Идемпотентна.
     *
     * Слушатель Esc висит на document, а не на попапе: фокус может быть в поле
     * ввода сообщения или на кнопке титлбара, а Esc обязан закрывать окно
     * всегда, пока оно открыто. Регистрировать его нужно ДО лайтбокса панели —
     * тогда одно нажатие закрывает попап, а не картинку под ним.
     *
     * @param {{onNotice?: function(string): void}} [opts]
     */
    function init(opts) {
        if (opts && typeof opts.onNotice === 'function') local.onNotice = opts.onNotice;
        if (local.bound) return;
        local.bound = true;

        if (refs.close) refs.close.addEventListener('click', () => close());
        if (refs.tabRun) refs.tabRun.addEventListener('click', () => setTab('run'));
        if (refs.tabCtx) refs.tabCtx.addEventListener('click', () => setTab('ctx'));

        // Глубина: запомненное значение важнее того, что стоит `selected` в
        // разметке, — пользователь выбирал его сам и ждёт увидеть снова.
        const saved = storedDepth();
        if (saved !== null) local.depth = saved;
        if (refs.depth) {
            refs.depth.value = String(local.depth);
            refs.depth.addEventListener('change', () => {
                const picked = pickOwnDepth(refs.depth.value);
                if (picked === null) {
                    // Значение вне списка игнорируем и возвращаем видимое
                    // обратно: иначе селект показывал бы одно, а запрос уходил с
                    // другим.
                    refs.depth.value = String(local.depth);
                    return;
                }
                local.depth = picked;
                rememberDepth(picked);
            });
        }

        if (refs.start) {
            refs.start.addEventListener('click', () => { void startAnalysis(); });
        }
        if (refs.refresh) {
            refs.refresh.addEventListener('click', () => { void refreshAnalysis(); });
        }
        if (refs.cancel) {
            refs.cancel.addEventListener('click', () => { void cancelRun(); });
        }
        if (refs.clear) {
            refs.clear.addEventListener('click', () => { void clearAnalysis(); });
        }
        if (refs.clearThread) {
            refs.clearThread.addEventListener('click', () => { void clearThreadTurns(); });
        }
        if (refs.outToggle) {
            refs.outToggle.addEventListener('click', () => toggleOutCollapsed());
        }

        // Форма вопроса: submit гасится всегда. Без preventDefault браузер
        // перезагрузил бы страницу devforge целиком — вместе с открытой панелью.
        if (refs.ask) {
            refs.ask.addEventListener('submit', (e) => {
                if (e && typeof e.preventDefault === 'function') e.preventDefault();
                void askQuestion();
            });
        }
        if (refs.send) {
            // Кнопка ASK — `type=submit`, но в фейковом DOM и в мобильных
            // клавиатурах submit формы порождается не всегда: клик обязан
            // работать сам по себе. Повторного запроса это не даёт — второй вход
            // отсекает `local.pending`.
            refs.send.addEventListener('click', (e) => {
                if (e && typeof e.preventDefault === 'function') e.preventDefault();
                void askQuestion();
            });
        }
        if (refs.question) {
            refs.question.addEventListener('keydown', (e) => {
                if (!e || e.key !== 'Enter') return;
                if (!e.ctrlKey && !e.metaKey) return;
                if (typeof e.preventDefault === 'function') e.preventDefault();
                void askQuestion();
            });
        }

        if (refs.out) {
            // Прокрутка ВВЕРХ сверх порога = «не тяни меня вниз». Направление, а
            // не флаг «это был я»: собственный доскролл порождает событие scroll
            // асинхронно, и синхронный флаг успел бы сброситься.
            refs.out.addEventListener('scroll', () => {
                const next = Number(refs.out.scrollTop) || 0;
                if (follow.cancelledByScroll(local.outScrollTop, next)) follow.clearFollow();
                else if (outAtBottom() && local.runId !== null) {
                    // Вернулся к нижнему краю — следование возобновляется.
                    follow.noteFollow(local.dialogId, 0, Date.now());
                }
                local.outScrollTop = next;
            });
        }

        if (refs.prompt) {
            refs.prompt.addEventListener('input', () => {
                local.promptDirty = true;
                renderPromptCount();
            });
            refs.prompt.addEventListener('keydown', (e) => {
                if (!e || e.key !== 'Enter') return;
                if (!e.ctrlKey && !e.metaKey) return;
                if (typeof e.preventDefault === 'function') e.preventDefault();
                void savePrompt();
            });
        }
        if (refs.promptSave) {
            refs.promptSave.addEventListener('click', () => { void savePrompt(); });
        }
        if (refs.promptClear) {
            refs.promptClear.addEventListener('click', () => { void clearPromptText(); });
        }

        if (refs.root) {
            refs.root.addEventListener('click', (e) => {
                // Клик по подложке (а не по коробке) закрывает окно.
                if (e && e.target === refs.root) close();
            });
        }

        local.escHandler = (e) => {
            if (!local.open) return;
            if (!e || e.key !== 'Escape') return;
            // Блокирующий оверлей панели живёт ВЫШЕ попапа: пока он на экране,
            // Esc принадлежит ему.
            if (overlayOpen()) return;
            if (typeof e.preventDefault === 'function') e.preventDefault();
            // Ниже по цепочке слушают лайтбокс и полоса ответа — одно нажатие
            // Esc обязано закрыть ровно одно окно.
            if (typeof e.stopPropagation === 'function') e.stopPropagation();
            close();
        };
        document.addEventListener('keydown', local.escHandler);

        // Свёрнутость применяется ДО первого показа попапа: примени её позже —
        // блок успел бы моргнуть развёрнутым на кадр.
        local.collapsed = storedCollapsed();
        applyCollapsed();

        renderUpdated();
        renderPromptCount();
        syncButtons();
    }

    /**
     * Снимает слушатели и очищает состояние (beforeunload, закрытие вкладки).
     *
     * Слушатель Esc на document переживает выгрузку страницы в bfcache и стрелял
     * бы по мёртвому DOM — поэтому снимается явно, а не «сам отвалится».
     */
    function teardown() {
        if (local.escHandler) {
            document.removeEventListener('keydown', local.escHandler);
            local.escHandler = null;
        }
        close();
        local.dialogId = null;
        resetForDialog();
        local.tab = 'run';
        local.depth = BASE_DEPTH;
        local.promptBusy = false;
        local.analysisBusy = false;
        local.threadBusy = false;
        local.bound = false;
        local.onNotice = null;
    }

    /** Снимок состояния для e2e и `_debugState()` панели. */
    function debugState() {
        return {
            bound: local.bound,
            open: local.open,
            dialogId: local.dialogId,
            runId: local.runId,
            kind: local.kind,
            pending: local.pending,
            tab: local.tab,
            bufLen: local.buf.length,
            rafPending: local.rafId !== null,
            thread: local.thread.length,
            threadBusy: local.threadBusy,
            collapsed: local.collapsed,
            // Открытая строка треда — признак «ответ на вопрос стримится ТУДА»;
            // e2e проверяет по нему, что разбор в это время не трогается.
            askRow: local.askRow !== null,
            hasAnalysis: local.hasAnalysis,
            analysisLoaded: local.analysisLoaded,
            analysisBusy: local.analysisBusy,
            savedAt: local.savedAt,
            savedUsed: local.savedUsed,
            following: follow.isFollowing(local.dialogId, Date.now()),
            depth: local.depth,
            promptLoaded: local.promptLoaded,
            promptDirty: local.promptDirty,
            promptBusy: local.promptBusy,
            maxChars: local.maxChars,
            hidden: refs.root ? !!refs.root.hidden : null,
        };
    }

    return {
        init,
        teardown,
        open,
        close,
        isOpen,
        debugState,
        // Кадры WS — раскладку по типам держит панель.
        onStarted,
        onDelta,
        onDone,
        onError,
        // Действия попапа: панель переэкспортирует их своими именами, а тесты
        // панели зовут их напрямую.
        setTab,
        ensurePrompt,
        savePrompt,
        ensureSavedAnalysis,
        clearAnalysis,
        clearThread: clearThreadTurns,
        setOutCollapsed,
        toggleOutCollapsed,
        startAnalysis,
        refreshAnalysis,
        askQuestion,
        cancelRun,
        errorText: errText,
    };
}
