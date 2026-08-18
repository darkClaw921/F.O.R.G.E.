// tmux-web — ОБЩЕЕ ЯДРО детекта ссылок на видеовстречи для трёх панелей.
//
// Задача модуля ровно одна: по сообщению мессенджера ответить, есть ли в нём
// ссылка на встречу, и если есть — вернуть платформу, локальную метку
// встречи и канонический адрес для входа. Ни узлов, ни запросов, ни состояния
// панели здесь нет: модуль чистый и проверяется в голом Node.
//
// Линкификации в проекте нет вообще. Текст сообщения рисуется текстовыми
// узлами, а Битрикс намеренно рисует адрес как `span.b24-seg-url`, а не как
// кликабельную ссылку. Поэтому «найти ссылку» — отдельная работа, а не
// побочный эффект рендера.
//
// ============================================================================
// ГЛАВНОЕ: ЭТО МОДУЛЬ ПРО БЕЗОПАСНОСТЬ, А НЕ ПРО РЕГУЛЯРКИ
// ============================================================================
//
// Совпадение платформы ищется ТОЛЬКО в `URL.hostname` — точным равенством
// либо суффиксом, начинающимся с точки (`.zoom.us`). Подстрочный поиск по
// адресу («строка содержит telemost.yandex.ru») даёт четыре готовых обхода,
// и каждый из них закрыт отрицательным тестом в
// tests/frontend/messenger_meet_core.test.mjs:
//
//   1. поддомен-обманка   https://telemost.yandex.ru.evil.com/j/1
//   2. домен в query      https://evil.com/?r=telemost.yandex.ru
//   3. домен во фрагменте https://evil.com#telemost.yandex.ru
//   4. домен в userinfo   https://telemost.yandex.ru@evil.com/j/1
//
// Четвёртый — тот, который обычно забывают: `hostname` у него `evil.com`, а
// глазами адрес читается как ссылка Телемоста. Разбор через конструктор URL
// схлопывает все четыре в одно и то же: хост берётся из хоста.
//
// Цена ошибки здесь не «кнопка не появилась». Адрес из этого модуля уезжает на
// бэкенд, там его открывает НАСТОЯЩИЙ Chrome с отключённым CSP
// (`Page.setBypassCSP`) и нашим инжектированным скриптом внутри. Ссылка от
// постороннего человека в чате, признанная встречей по ошибке, — это чужая
// страница внутри нашего браузерного контекста.
//
// Схема проверяется СУЩЕСТВУЮЩИМ `safeHref` (messenger/markdown.js): белый
// список `http:`/`https:`, разбор через конструктор URL, относительные адреса
// запрещены. Четвёртая копия `safeUrl` в проекте намеренно не заводится — их
// уже три, и расходиться они начнут в тот день, когда одну из них поправят.
//
// ============================================================================
// НОРМАЛИЗАЦИЯ — БЕЛЫЙ СПИСОК ПАРАМЕТРОВ, А НЕ ЧЁРНЫЙ
// ============================================================================
//
// Соблазн «срезать всю query, id всё равно в пути» ломает две платформы из
// четырёх: у Zoom в `?pwd=` лежит ПАРОЛЬ встречи, у Teams в `?context=` —
// обязательная часть ссылки (арендатор и организатор). Без них бот упрётся в
// форму пароля или получит отказ, и выглядеть это будет как «Zoom не
// работает», а не как «мы отрезали параметр».
//
// Поэтому у каждой платформы свой список разрешённых параметров, а всё
// остальное отбрасывается. Мусор вроде `?ncrnd=` (его Телемост дописывает сам
// при каждом открытии) и `?authuser=` у Meet исчезает без отдельного правила:
// его просто нет в списке. Заодно это и есть ответ на вопрос «почему одна и та
// же встреча приходит двумя разными строками» — после нормализации она одна.
//
// ============================================================================
// РЕЕСТР ПЛАТФОРМ ПРИХОДИТ С БЭКЕНДА
// ============================================================================
//
// Встроенный набор — ЗАПАСНОЙ ВАРИАНТ, а не источник истины. Источник —
// `GET /api/meet/status` (`providers.rs`), и `configureMeetPlatforms()`
// подменяет им встроенный. Причина простая и уже проверенная на этом проекте:
// разметку и скрипты браузер держит в кэше PWA неделями, поэтому фронтовая
// константа «что считать ссылкой на встречу» протухнет незаметно — новая форма
// ссылки Zoom перестанет распознаваться у половины пользователей, и понять это
// по жалобе будет невозможно.
//
// ФОРМА ЗАПИСИ РЕЕСТРА — ОДНА, ПРОВОДНАЯ. Пока фронт и бэкенд писались
// параллельно, `normalizePlatforms` принимала оба написания каждого поля
// (`hosts`/`domains`, `paths`/`path_patterns`, `params`/`keep_params`). Теперь
// принимается РОВНО то, что отдаёт `Provider` в `plugins/meet/src/providers.rs`:
//
//   platform, title, hosts, host_suffixes, paths, keep_params, key_case,
//   link_regex, audio
//
// Терпимость снята намеренно. Она маскировала бы ровно ту поломку, ради
// которой реестр и отдаётся с сервера: переименуй бэкенд поле — и фронт молча
// остался бы на встроенном наборе, а узнали бы мы об этом по жалобе «кнопка не
// появляется на новых ссылках Zoom». Бэкенд на этом уже дважды поймали:
// запись без `paths` отбрасывалась ПОШТУЧНО И МОЛЧА, а идентификатор Google
// Meet расходился сам с собой (`google_meet` в JSON против `meet` в строковой
// форме). Оба раза расхождение проявлялось ТИШИНОЙ, а не ошибкой.
//
// Одно написание держится тестом `tests/frontend/meet_contract.test.mjs`,
// который сверяет встроенный набор с таблицей `PROVIDERS` бэкенда В ОБЕ
// СТОРОНЫ — поле в поле.
//
// ============================================================================
// `localKey` — ЭТО НЕ `url_key`
// ============================================================================
//
// У встречи есть ДВА разных ключа, и путать их дорого:
//
//   • `url_key` — ИДЕНТИЧНОСТЬ КОМНАТЫ. Считает его ТОЛЬКО бэкенд
//     (`providers.rs::parse_meeting_url`), по нему работает частичный
//     уникальный индекс `ux_meetings_active_url`, то есть именно он не даёт
//     боту войти в одну встречу дважды.
//   • `localKey` — строка `<платформа>:<идентификатор>` ИЗ ЭТОГО МОДУЛЯ.
//     Локальная метка: подпись строки ленты, дедуп нескольких ссылок в одном
//     сообщении, `data-meet-key` в разметке. На сервер она НЕ УЕЗЖАЕТ
//     НИКОГДА — туда уходит `url`, и бэкенд разбирает его заново.
//
// Почему не сделать их одним: правила идентичности у платформ разные и в
// запись реестра не помещаются (у Zoom вендорный поддомен в идентичность не
// входит, у Meet `lookup/<имя>` — часть идентификатора, у Teams значим регистр
// процентного кодирования). Повторить их здесь означало бы завести ВТОРУЮ
// реализацию идентичности комнаты, которая начнёт расходиться с первой в тот
// день, когда одну из них поправят. Поэтому имена разведены: строку с именем
// `localKey` никто не отправит туда, где ждут `url_key`.
//
// Отдельно записан ответ на вопрос, который за этим стоял: считать ли
// `telemost.yandex.ru` и `telemost.yandex.com` ОДНОЙ комнатой. ХОСТ ОСТАЁТСЯ
// ЧАСТЬЮ ИДЕНТИЧНОСТИ, пока живая проба не покажет обратное — у Teams это
// верно наверняка (`teams.microsoft.us` это GCC High, физически другое
// облако), а цена двух ошибок несимметрична: лишний вход бота видно сразу и
// он отменяется, а склейка двух РАЗНЫХ комнат в одну молча привязала бы
// запись к чужой встрече.
//
// ============================================================================
// ИСТОЧНИК ТЕКСТА В КАЖДОЙ ПАНЕЛИ РАЗНЫЙ
// ============================================================================
//
// Это неочевидно, и ошибка здесь выглядит как «кнопка не появляется на
// некоторых сообщениях» — то есть как случайность.
//
// - Telegram: `msg.text`. Приватная `messageText()` из telegram/chat.js НЕ
//   годится — она подмешивает `[photo]` и `[voice 7s]`, то есть кладёт в
//   детект мусор.
// - VK: `msg.text` ПЛЮС вложения-ссылки. VK регулярно выносит адрес из текста
//   в отдельное вложение, и на самом типовом сообщении со ссылкой текст
//   оказывается пустым.
// - Битрикс: обход `msg.segments`. `segmentsText()` в одиночку НЕДОСТАТОЧНА:
//   она возвращает `inner || value`, поэтому у `[URL=https://…]созвон[/URL]`
//   плоский текст даст «созвон», а адрес пропадёт.
//
// ============================================================================
// ЧЕГО МОДУЛЬ НАМЕРЕННО НЕ ДЕЛАЕТ
// ============================================================================
//
// Не восстанавливает схему у адреса, написанного руками без неё
// (`telemost.yandex.ru/j/123`). Такой текст `safeHref` отвергает как
// относительный, и это правильное поведение: догадка «наверное, тут https»
// — ровно тот класс эвристик, из-за которого модуль детекта превращается в
// модуль угадывания. Ссылки, которые вставляют из клиента встречи (а это
// подавляющее большинство), схему несут всегда.

import { safeHref } from './markdown.js';

// ==========================================================================
// Ограничители
// ==========================================================================

/**
 * Потолок длины одного адреса-кандидата.
 *
 * Всё длиннее — не ссылка на встречу, а либо data-подобная простыня, либо
 * попытка нагрузить разбор. Реальный максимум среди четырёх платформ — ссылка
 * Teams с `context`, и она укладывается в ~400 символов.
 */
const MAX_URL_CHARS = 2048;

/** Потолок длины значения одного разрешённого параметра. */
const MAX_PARAM_CHARS = 1024;

/**
 * Сколько адресов-кандидатов вынимаем из одного сообщения.
 *
 * Детекту нужен первый подошедший; десятки кандидатов бывают только у
 * сообщения-простыни со списком ссылок, и разбирать их все незачем.
 */
const MAX_CANDIDATES = 32;

/** Ограничитель вложенности при обходе сегментов Битрикса (как в `renderSegments`). */
const MAX_SEGMENT_DEPTH = 8;

// ==========================================================================
// Встроенный реестр платформ
// ==========================================================================

/**
 * Запасной реестр: работает, пока не пришёл ответ `/api/meet/status`.
 *
 * `paths` — ИСХОДНИКИ регулярных выражений строками, потому что ровно в таком
 * виде их отдаёт бэкенд (JSON не умеет regexp). Каждый компилируется
 * обёрнутым в `^(?:…)$`: незаякоренный шаблон совпал бы с серединой пути, и
 * `/redirect?to=/j/123` внезапно стал бы встречей.
 *
 * Группа 1 — идентификатор комнаты. Из него собирается ключ `<платформа>:<id>`,
 * по которому одна и та же встреча, присланная в двух чатах и с разным
 * мусором в query, остаётся одной встречей.
 *
 * @type {Array<object>}
 */
export const BUILTIN_MEET_PLATFORMS = [
    {
        platform: 'telemost',
        title: 'Яндекс Телемост',
        // Форма пути снята с живой встречи: /j/60497752704486.
        hosts: [
            'telemost.yandex.ru',
            'telemost.yandex.com',
            'telemost.yandex.by',
            'telemost.yandex.kz',
            'telemost.360.yandex.ru',
        ],
        // Пусто НЕ по недосмотру: суффиксное сравнение с `yandex.ru` пустило
        // бы сюда любой сервис компании.
        host_suffixes: [],
        paths: ['/j/(\\d{4,32})'],
        // Пусто НЕ по недосмотру: id целиком в пути, а `ncrnd` Телемост
        // дописывает сам при каждом открытии — в адресе ему делать нечего.
        keep_params: [],
        key_case: 'preserve',
        audio: true,
    },
    {
        platform: 'meet',
        title: 'Google Meet',
        hosts: ['meet.google.com'],
        host_suffixes: [],
        // `abc-defg-hij` и именные ссылки `/lookup/<имя>`.
        paths: ['/([A-Za-z]{3}-[A-Za-z]{4}-[A-Za-z]{3})', '/lookup/([A-Za-z0-9_-]{3,64})'],
        keep_params: [],
        // Код встречи Meet регистронезависим: без приведения одна встреча,
        // присланная двумя людьми, дала бы две метки.
        key_case: 'lower',
        audio: true,
    },
    {
        platform: 'zoom',
        title: 'Zoom',
        // Личный поддомен компании (`us02web.zoom.us`) — норма, поэтому здесь
        // и нужен суффикс. Именно он и есть та проверка, ради которой
        // подстрочный поиск запрещён: суффикс сравнивается ОТ ТОЧКИ.
        hosts: ['zoom.us', 'zoomgov.com'],
        host_suffixes: ['.zoom.us', '.zoomgov.com'],
        paths: [
            '/j/(\\d{8,15})',
            '/wc/join/(\\d{8,15})',
            '/wc/(\\d{8,15})/join',
            '/s/(\\d{8,15})',
            '/w/(\\d{8,15})',
            '/my/([A-Za-z0-9._-]{1,64})',
        ],
        // ПАРОЛЬ ВСТРЕЧИ. Срезать нельзя — бот упрётся в форму ввода.
        keep_params: ['pwd'],
        key_case: 'preserve',
        audio: true,
    },
    {
        platform: 'teams',
        title: 'Microsoft Teams',
        hosts: ['teams.microsoft.com', 'teams.live.com', 'teams.microsoft.us'],
        host_suffixes: [],
        // Идентификатор треда приезжает процентно-закодированным
        // (`19%3ameeting_…%40thread.v2`) — в шаблоне это обычные символы пути.
        paths: ['/l/meetup-join/([^/?#]{4,512})(?:/\\d*)?', '/meet/(\\d{6,20})'],
        // `context` — обязательная часть ссылки (арендатор и организатор),
        // `p` — код доступа у ссылок нового вида.
        keep_params: ['context', 'p'],
        key_case: 'preserve',
        audio: true,
    },
];

/**
 * Действующий реестр. Заменяется целиком через `configureMeetPlatforms`.
 * @type {Array<object>}
 */
let platforms = normalizePlatforms(BUILTIN_MEET_PLATFORMS);

/**
 * Подменить реестр платформ ответом `GET /api/meet/status`.
 *
 * Мусорный или пустой список НЕ обнуляет детект, а возвращает встроенный
 * набор: «бэкенд ответил чем-то странным» не должно означать «кнопка встречи
 * исчезла везде». Записи без хостов или без шаблонов пути отбрасываются
 * поштучно — одна сломанная платформа не уносит остальные три.
 *
 * Принимается либо сам массив `providers`, либо тело ответа целиком (поле
 * `providers`). Второго имени обёртки нет: `platforms` из первой редакции
 * убрано вместе с остальной терпимостью к написанию.
 *
 * @param {Array<object>|{providers?: Array<object>}} list
 * @returns {Array<object>} применённый реестр
 */
export function configureMeetPlatforms(list) {
    const raw = Array.isArray(list)
        ? list
        : (list && typeof list === 'object' ? list.providers : null);
    const next = normalizePlatforms(raw);
    platforms = next.length > 0 ? next : normalizePlatforms(BUILTIN_MEET_PLATFORMS);
    return platforms;
}

/** Вернуть встроенный реестр обратно (тесты и выключение плагина). */
export function resetMeetPlatforms() {
    platforms = normalizePlatforms(BUILTIN_MEET_PLATFORMS);
    return platforms;
}

/**
 * Действующий реестр — только для чтения (копия массива).
 * @returns {Array<object>}
 */
export function meetPlatforms() {
    return platforms.slice();
}

/**
 * Приведение записей реестра к одной внутренней форме плюс КОМПИЛЯЦИЯ
 * шаблонов пути.
 *
 * Компиляция здесь, а не в горячем пути детекта: `new RegExp` на каждое
 * сообщение ленты — это лишняя работа на каждом кадре прокрутки. Битый шаблон
 * (бэкенд собран другой версией) не роняет вызов, а выбрасывается: пусть
 * распознаётся меньше форм ссылок, чем падает вся панель.
 *
 * @param {Array<object>} list
 * @returns {Array<object>}
 */
function normalizePlatforms(list) {
    if (!Array.isArray(list)) return [];
    const out = [];
    for (const item of list) {
        if (!item || typeof item !== 'object') continue;
        // ТОЛЬКО `platform`. `id`/`key` не принимаются: одно написание — это и
        // есть смысл контракта, а «на всякий случай ещё и так» вернуло бы
        // молчаливую подмену реестра встроенным набором.
        const id = strOr(item.platform, '');
        if (!id) continue;
        const hosts = strList(item.hosts).map(normalizeHost).filter(Boolean);
        const suffixes = strList(item.host_suffixes)
            .map(normalizeHost)
            // Суффикс ОБЯЗАН начинаться с точки: без неё `zoom.us` совпал бы с
            // `notzoom.us`, то есть вернулся бы ровно тот подстрочный поиск,
            // от которого модуль и уходит.
            .filter((s) => s.length > 1 && s.startsWith('.'));
        const patterns = strList(item.paths);
        if (hosts.length === 0 && suffixes.length === 0) continue;
        const compiled = [];
        for (const src of patterns) {
            // Свои якоря бэкенда снимаются перед обёрткой: `^(?:^/j/(\d+)$)/?$`
            // формально скомпилируется, но перестанет принимать завершающий
            // слэш — то есть сломается ровно на той форме, ради которой
            // обёртка и нужна.
            const bare = src.replace(/^\^/, '').replace(/\$$/, '');
            try {
                compiled.push(new RegExp(`^(?:${bare})/?$`));
            } catch (_e) {
                // Шаблон не компилируется — молча мимо.
            }
        }
        if (compiled.length === 0) continue;
        out.push({
            platform: id,
            title: strOr(item.title, id),
            hosts,
            host_suffixes: suffixes,
            paths: patterns,
            // Скомпилированные шаблоны — единственное поле, которого нет на
            // проводе: JSON не умеет regexp.
            matchers: compiled,
            keep_params: strList(item.keep_params),
            key_case: item.key_case === 'lower' ? 'lower' : 'preserve',
            // `audio: false` (кандидат — Zoom с его WASM-пайплайном) означает
            // «ссылку узнаём, кнопку не рисуем». Умолчание — true: платформа,
            // про звук которой сервер ничего не сказал, считается рабочей.
            audio: item.audio !== false,
        });
    }
    return out;
}

// ==========================================================================
// Детект одного адреса
// ==========================================================================

/**
 * Разобрать строку как ссылку на встречу.
 *
 * Порядок проверок принципиален и читается сверху вниз как лестница доверия:
 *
 *   1. длина                — ограничитель раньше любого разбора;
 *   2. `safeHref`           — схема (`http:`/`https:`), общая с рендером
 *                             Markdown; `javascript:` и относительные умирают
 *                             здесь;
 *   3. `hostname`           — точное равенство или суффикс от точки;
 *   4. шаблон пути          — заякоренный, с группой-идентификатором;
 *   5. нормализация         — белый список параметров, без userinfo и якоря.
 *
 * @param {*} raw — строка-кандидат (из текста, вложения или сегмента)
 * @returns {?{platform: string, title: string, id: string, localKey: string,
 *             url: string, raw: string, audio: boolean}}
 *   `url` — КАНОНИЧЕСКИЙ АДРЕС, и это единственное поле, которое уезжает на
 *   сервер; `raw` — исходная строка как её написал человек; `localKey` —
 *   `<платформа>:<идентификатор>`, локальная метка, НЕ `url_key` (см. шапку).
 */
export function detectMeetUrl(raw) {
    if (typeof raw !== 'string') return null;
    const text = raw.trim();
    if (text === '' || text.length > MAX_URL_CHARS) return null;

    // Схема — общим `safeHref`, а не своей проверкой. Он же отсекает
    // относительные адреса и синтаксический мусор.
    const href = safeHref(text);
    if (!href) return null;

    let url;
    try {
        url = new URL(href);
    } catch (_e) {
        return null;
    }

    // ЛАУНЧЕР TEAMS: настоящий адрес встречи лежит в параметре `url`, а путь
    // `/dl/launcher/launcher.html` комнаты не содержит вовсе. Разворот идёт ДО
    // канонизации: белый список параметров выбросил бы `url` — то есть саму
    // встречу, — и на сервер уехала бы ссылка на страницу «скачайте
    // приложение». Тот же разворот делает `providers.rs`; разойдись они, фронт
    // и бэкенд опознавали бы РАЗНЫЕ ссылки, и кнопка появлялась бы там, где
    // сервер записывать отказывается.
    const inner = launcherTarget(url);
    if (inner) {
        try {
            url = new URL(inner);
        } catch (_e) {
            return null;
        }
    }

    const host = normalizeHost(url.hostname);
    if (!host) return null;

    for (const p of platforms) {
        if (!hostMatches(host, p)) continue;
        const hit = matchPath(url.pathname, p);
        if (!hit) continue;
        const canonical = canonicalUrl(url, host, p);
        if (!canonical) continue;
        return {
            platform: p.platform,
            title: p.title,
            id: hit,
            localKey: `${p.platform}:${hit}`,
            url: canonical,
            raw: text,
            audio: p.audio,
        };
    }
    return null;
}

/** Есть ли в строке ссылка на встречу (без сборки результата). */
export function isMeetUrl(raw) {
    return detectMeetUrl(raw) !== null;
}

/** Путь ссылки-лаунчера Teams. Сравнивается целиком, без учёта регистра. */
const TEAMS_LAUNCHER_PATH = '/dl/launcher/launcher.html';

/**
 * Настоящий адрес встречи внутри ссылки-лаунчера Teams.
 *
 * Teams рассылает `…/dl/launcher/launcher.html?url=%2F_%23%2Fl%2Fmeetup-join%2F…`,
 * где внутренний адрес бывает и абсолютным, и относительным, а путь встречи
 * иногда лежит ПОСЛЕ РЕШЁТКИ (`/_#/l/meetup-join/…`) — в такой форме `URL`
 * считает его якорем, и путь остался бы `/_`.
 *
 * Хост берётся только у самого лаунчера, а хост внутренней ссылки обязан снова
 * оказаться хостом Teams: параметр `url` приходит из того же чужого сообщения,
 * что и всё остальное, и доверия ему ровно столько же.
 *
 * @param {URL} url
 * @returns {?string} абсолютный адрес встречи либо `null`
 */
function launcherTarget(url) {
    if (String(url.pathname).toLowerCase() !== TEAMS_LAUNCHER_PATH) return null;
    const host = normalizeHost(url.hostname);
    if (!host || !isTeamsHost(host)) return null;

    const raw = url.searchParams.get('url');
    if (!raw || !raw.trim()) return null;

    let inner;
    try {
        inner = new URL(raw.trim(), `https://${host}`);
    } catch (_e) {
        return null;
    }
    if (inner.protocol !== 'http:' && inner.protocol !== 'https:') return null;
    if (!isTeamsHost(normalizeHost(inner.hostname))) return null;

    if (inner.hash && inner.hash.startsWith('#/')) {
        try {
            return new URL(inner.hash.slice(1), inner.origin).href;
        } catch (_e) {
            return null;
        }
    }
    return inner.href;
}

/** Хост принадлежит Teams по ДЕЙСТВУЮЩЕМУ реестру, а не по своему списку. */
function isTeamsHost(host) {
    if (!host) return false;
    const teams = platforms.find((p) => p.platform === 'teams');
    return !!teams && hostMatches(host, teams);
}

/**
 * Хост принадлежит платформе?
 *
 * ЕДИНСТВЕННОЕ место, где решается «наша это встреча или чужой сайт».
 * Равенство — полное; суффикс — обязательно с точкой в начале, поэтому
 * `telemost.yandex.ru.evil.com` не совпадёт ни с одним хостом (у него
 * последний ярлык `com`), а `us02web.zoom.us` совпадёт с `.zoom.us`.
 *
 * @param {string} host — уже нормализованный хост
 * @param {object} p
 * @returns {boolean}
 */
function hostMatches(host, p) {
    for (const h of p.hosts) {
        if (host === h) return true;
    }
    for (const s of p.host_suffixes) {
        if (host.length > s.length && host.endsWith(s)) return true;
    }
    return false;
}

/**
 * Путь против шаблонов платформы; результат — идентификатор комнаты.
 *
 * Процентное кодирование раскрывается: `19%3ameeting_X%40thread.v2` и
 * `19%3Ameeting_X%40thread.v2` — один и тот же тред Teams, и ключ у них обязан
 * быть один. Нераскрываемая последовательность остаётся как есть.
 *
 * @param {string} pathname
 * @param {object} p
 * @returns {?string}
 */
function matchPath(pathname, p) {
    for (const re of p.matchers) {
        const m = re.exec(pathname);
        if (!m) continue;
        let id = m[1] != null && m[1] !== '' ? m[1] : pathname;
        try {
            id = decodeURIComponent(id);
        } catch (_e) {
            // Битая процентная последовательность — берём как есть.
        }
        if (p.key_case === 'lower') id = id.toLowerCase();
        return id;
    }
    return null;
}

/**
 * Канонический адрес встречи.
 *
 * Что уходит: `username`/`password` (в ссылке на встречу их не бывает, зато
 * бывают в подделке), якорь, все параметры вне белого списка, завершающий слэш
 * пути и завершающая точка хоста.
 *
 * Что остаётся: схема, хост с портом, путь, разрешённые параметры В ПОРЯДКЕ
 * ОБЪЯВЛЕНИЯ в реестре — благодаря этому две записи одной встречи с разным
 * порядком query дают побайтово одинаковый адрес.
 *
 * @param {URL} url
 * @param {string} host — нормализованный хост
 * @param {object} p
 * @returns {?string}
 */
function canonicalUrl(url, host, p) {
    let out;
    try {
        out = new URL(url.href);
    } catch (_e) {
        return null;
    }
    out.username = '';
    out.password = '';
    out.hash = '';
    out.hostname = host;
    if (out.pathname.length > 1 && out.pathname.endsWith('/')) {
        out.pathname = out.pathname.slice(0, -1);
    }
    const keep = new URLSearchParams();
    for (const name of p.keep_params) {
        for (const value of url.searchParams.getAll(name)) {
            if (typeof value === 'string' && value.length <= MAX_PARAM_CHARS) keep.append(name, value);
        }
    }
    const query = keep.toString();
    out.search = query ? `?${query}` : '';
    const href = out.href;
    return href.length <= MAX_URL_CHARS ? href : null;
}

/**
 * Хост к сравнимому виду: нижний регистр и БЕЗ завершающей точки.
 *
 * Завершающая точка — не мелочь: `telemost.yandex.ru.` это валидное полное имя,
 * которое конструктор URL сохраняет как есть, и без обрезки точное равенство
 * промахнулось бы. Обратная сторона (`evil.com.` вдруг совпадёт) невозможна:
 * обрезка убирает только последний символ, ярлыки не меняются.
 *
 * @param {*} raw
 * @returns {string}
 */
function normalizeHost(raw) {
    if (typeof raw !== 'string') return '';
    let h = raw.trim().toLowerCase();
    while (h.endsWith('.')) h = h.slice(0, -1);
    return h;
}

// ==========================================================================
// Кандидаты из свободного текста
// ==========================================================================

/**
 * Регулярка ищет ТОЛЬКО кандидатов, а не решает, встреча ли это.
 *
 * Решение принимает `detectMeetUrl` по хосту. Поэтому здесь допустима широкая
 * форма: «`http(s)://` и всё до пробела или разметочной скобки». Пропустить
 * лишнее не опасно, пропустить нужное — опасно.
 */
const URL_TOKEN_RE = /https?:\/\/[^\s<>"'`]+/gi;

/** Хвостовая пунктуация, приклеенная к адресу концом фразы. */
const TRAILING_PUNCT = new Set(['.', ',', ';', ':', '!', '?', '»', '"', "'", '“', '”', '…', '*', '_', '~']);

/** Парные хвосты: срезаются, только если открывающего в адресе не было. */
const TRAILING_PAIRS = { ')': '(', ']': '[', '}': '{' };

/**
 * Адреса из свободного текста в порядке появления.
 *
 * Отсечение хвостовой пунктуации обязательно: «созвон https://telemost.yandex.ru/j/1.»
 * без него даёт путь `/j/1.`, шаблон не совпадает, кнопка не появляется — и
 * выглядит это как «иногда не работает». Скобка срезается только непарная:
 * у Teams `context` бывает в скобках, и внутри адреса они законны.
 *
 * @param {*} text
 * @returns {string[]}
 */
export function collectUrlsFromText(text) {
    if (typeof text !== 'string' || text === '') return [];
    const out = [];
    URL_TOKEN_RE.lastIndex = 0;
    let m;
    while ((m = URL_TOKEN_RE.exec(text)) !== null && out.length < MAX_CANDIDATES) {
        // Слово, приклеенное слева (`видеоhttps://…`), — не ссылка: человек
        // видит его как текст, и кликабельным оно нигде не станет.
        const before = m.index > 0 ? text[m.index - 1] : '';
        if (before && /[\p{L}\p{N}]/u.test(before)) continue;
        const token = trimUrlTail(m[0]);
        if (token) out.push(token);
    }
    return out;
}

/**
 * Срезать хвост, не принадлежащий адресу.
 * @param {string} token
 * @returns {string}
 */
function trimUrlTail(token) {
    let s = token;
    while (s.length > 0) {
        const last = s[s.length - 1];
        if (TRAILING_PUNCT.has(last)) {
            s = s.slice(0, -1);
            continue;
        }
        const open = TRAILING_PAIRS[last];
        if (open && !s.includes(open)) {
            s = s.slice(0, -1);
            continue;
        }
        break;
    }
    return s;
}

// ==========================================================================
// Кандидаты из сегментов Битрикса
// ==========================================================================

/**
 * Адреса из дерева BB-сегментов Битрикса — в порядке обхода.
 *
 * ПОЧЕМУ НЕ `segmentsText()`. Она возвращает `inner || value`, то есть у
 * `[URL=https://telemost.yandex.ru/j/1]созвон[/URL]` отдаст «созвон», а адрес
 * потеряется совсем. Здесь наоборот: у сегмента `url` берётся сначала `value`,
 * а потом содержимое — потому что вторая живая форма портала прямо
 * противоположна первой. Когда `URL_PREVIEW: "Y"` оборачивает голую ссылку,
 * получается `[URL]https://…[/URL]`: атрибута НЕТ, адрес лежит текстом внутри
 * (см. bbcode.rs::a_bare_url_wrapped_by_the_portal_keeps_its_address).
 * Обе формы встречаются в одном чате.
 *
 * Текстовые сегменты тоже просматриваются: с выключенным превью портал не
 * оборачивает ссылку ничем, и она приезжает обычным текстом.
 *
 * Ограничитель глубины — как у `renderSegments`: дерево приходит от портала.
 *
 * @param {Array<object>} segments
 * @param {number} [depth]
 * @param {string[]} [acc]
 * @returns {string[]}
 */
export function collectUrlsFromSegments(segments, depth = 0, acc = []) {
    if (!Array.isArray(segments) || depth > MAX_SEGMENT_DEPTH) return acc;
    for (const seg of segments) {
        if (acc.length >= MAX_CANDIDATES) break;
        if (!seg || typeof seg !== 'object') continue;
        if (seg.kind === 'text') {
            for (const u of collectUrlsFromText(seg.text)) acc.push(u);
            continue;
        }
        if (seg.kind === 'url' && typeof seg.value === 'string' && seg.value.trim() !== '') {
            acc.push(seg.value.trim());
        }
        collectUrlsFromSegments(seg.children, depth + 1, acc);
    }
    return acc;
}

// ==========================================================================
// Кандидаты из вложений VK
// ==========================================================================

/**
 * Адрес вложения-ссылки.
 *
 * Две формы, и обе живые:
 * - нормализованный `MediaDto` бэкенда — вид у него `other` (см.
 *   `attach.rs`: сырой тип `link` схлопывается в `other`), а адрес лежит в
 *   `link_url`. ЭТО ОСНОВНАЯ ФОРМА, и она не совпадает с буквой плана, где
 *   написано `kind === 'link'`;
 * - сырое вложение VK (`type: 'link'`, адрес в `link.url`) — так приезжают
 *   кадры WS, собранные предыдущей версией плагина.
 *
 * @param {*} att
 * @returns {?string}
 */
function attachmentUrl(att) {
    if (!att || typeof att !== 'object') return null;
    const direct = strOr(att.link_url, strOr(att.linkUrl, ''));
    if (direct) return direct;
    const kind = String(att.kind || att.type || '');
    if (kind !== 'link') return null;
    if (att.link && typeof att.link === 'object') {
        const inner = strOr(att.link.url, '');
        if (inner) return inner;
    }
    return strOr(att.url, '') || null;
}

/**
 * Вложения сообщения в любой из поддерживаемых форм.
 *
 * Повторяет `attachmentSource` из vk/attachments.js намеренно: тянуть сюда
 * импорт панели VK ради трёх строк значило бы, что общий слой мессенджеров
 * зависит от конкретного плагина.
 *
 * @param {object} msg
 * @returns {Array<object>}
 */
function attachmentSource(msg) {
    if (!msg || typeof msg !== 'object') return [];
    if (Array.isArray(msg.attachments) && msg.attachments.length > 0) return msg.attachments;
    return msg.media ? [msg.media] : [];
}

// ==========================================================================
// Сообщение целиком
// ==========================================================================

/** Панели, у которых источник текста описан поимённо. */
export const MEET_PANELS = ['telegram', 'vk', 'bitrix'];

/**
 * Все адреса-кандидаты сообщения — в том порядке, в каком их видит человек.
 *
 * @param {object} msg — сообщение панели как есть (DTO ленты)
 * @param {string} panel — `telegram` | `vk` | `bitrix`; иное — режим «всё
 *   сразу», чтобы новая панель работала до того, как её здесь пропишут
 * @returns {string[]}
 */
export function messageUrlCandidates(msg, panel) {
    if (!msg || typeof msg !== 'object') return [];
    const kind = String(panel || '').toLowerCase();
    const out = [];

    if (kind === 'bitrix') {
        // Сегменты — источник истины; `msg.text` у Битрикса это запасное поле
        // кадров, где дерево не собрано (см. bitrix/chat.js: `segmentsText(...) || msg.text`).
        const segs = collectUrlsFromSegments(msg.segments);
        if (segs.length > 0) return segs.slice(0, MAX_CANDIDATES);
        return collectUrlsFromText(msg.text).slice(0, MAX_CANDIDATES);
    }

    if (kind === 'telegram') {
        // ТОЛЬКО `msg.text`. Подписи `[photo]` из `messageText()` сюда не
        // попадают, потому что `messageText()` здесь не вызывается.
        return collectUrlsFromText(msg.text).slice(0, MAX_CANDIDATES);
    }

    for (const u of collectUrlsFromText(msg.text)) out.push(u);
    for (const att of attachmentSource(msg)) {
        if (out.length >= MAX_CANDIDATES) break;
        const u = attachmentUrl(att);
        if (u) out.push(u);
    }
    if (kind !== 'vk') {
        // Неизвестная панель: пробуем и сегменты тоже.
        for (const u of collectUrlsFromSegments(msg.segments)) {
            if (out.length >= MAX_CANDIDATES) break;
            out.push(u);
        }
    }
    return out.slice(0, MAX_CANDIDATES);
}

/**
 * Первая встреча в сообщении.
 *
 * ПЕРВАЯ, а не «лучшая»: сообщение с двумя ссылками — редкость, а выбор между
 * ними по какому-нибудь приоритету платформ был бы для человека
 * непредсказуемым. Порядок обхода — порядок в сообщении: текст, затем
 * вложения (VK), либо порядок сегментов (Битрикс).
 *
 * @param {object} msg
 * @param {string} panel
 * @returns {?{platform: string, title: string, id: string, localKey: string,
 *             url: string, raw: string, audio: boolean}}
 */
export function detectMeetInMessage(msg, panel) {
    for (const candidate of messageUrlCandidates(msg, panel)) {
        const hit = detectMeetUrl(candidate);
        if (hit) return hit;
    }
    return null;
}

/**
 * Все встречи сообщения без дублей по ключу — для попапа и отладки.
 *
 * @param {object} msg
 * @param {string} panel
 * @returns {Array<object>}
 */
export function detectAllMeetsInMessage(msg, panel) {
    const seen = new Set();
    const out = [];
    for (const candidate of messageUrlCandidates(msg, panel)) {
        const hit = detectMeetUrl(candidate);
        if (hit && !seen.has(hit.localKey)) {
            seen.add(hit.localKey);
            out.push(hit);
        }
    }
    return out;
}

// ==========================================================================
// Мелочи
// ==========================================================================

/** Непустая строка или запасное значение. */
function strOr(v, fallback) {
    if (typeof v !== 'string') return fallback;
    const t = v.trim();
    return t !== '' ? t : fallback;
}

/** Массив непустых строк из чего угодно. */
function strList(v) {
    if (!Array.isArray(v)) return [];
    const out = [];
    for (const item of v) {
        if (typeof item === 'string' && item.trim() !== '') out.push(item.trim());
    }
    return out;
}
