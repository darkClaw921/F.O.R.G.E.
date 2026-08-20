// ⚠ СТРАЖ КОНТРАКТА ПЛАГИНА ВСТРЕЧ (фронтовая Ф2 forge-meet, ГЕЙТ ФФ2.4).
//
// Плагин собран из артефактов, между которыми НЕТ ни компилятора, ни общих
// типов, ни единого теста, который бы их сводил:
//
//   • plugins/meet/src/routes/mod.rs   — таблица путей в шапке;
//   • static/js/meet/api.js            — та же поверхность глазами фронта;
//   • plugins/meet/src/ws/protocol.rs  — enum MeetServerMsg (кадры WS);
//   • static/js/meet/ws.js             — карта FRAME_HANDLERS;
//   • plugins/meet/src/providers.rs    — реестр платформ (уезжает в /status);
//   • static/js/messenger/meet-core.js — встроенный реестр и разбор ссылок;
//   • plugins/meet/src/status.rs       — статусы и терминальность;
//   • static/js/messenger/meet-store.js — те же списки во фронте.
//
// Расхождение любой пары НЕ ломает сборку и НЕ пишет ни строчки в консоль:
//   — разъехавшийся путь даёт 404, который гейт читает как «плагин выключен»,
//     и кнопка встречи просто не появляется;
//   — незнакомый кадр WS уходит в console.debug и молча теряется (транспорт
//     намеренно терпим к неизвестным типам);
//   — переименованное поле реестра заставляет normalizePlatforms отбросить
//     запись ПОШТУЧНО И МОЛЧА, фронт остаётся на встроенном наборе, и весь
//     смысл серверного источника истины пропадает БЕЗ ЕДИНОГО СООБЩЕНИЯ;
//   — разошедшийся список терминальных статусов включает поллинг там, где его
//     не должно быть, и этого не видно вообще никак.
//
// На последних двух бэкенд УЖЕ ловили дважды (запись без `paths` и
// `google_meet` против `meet`), поэтому здесь сверка идёт В ОБЕ СТОРОНЫ и
// ПОЛЕ В ПОЛЕ, а не «в основном совпадает».
//
// Сравнение ТЕКСТОВОЕ по исходникам Rust: собирать бэкенд ради имени поля
// дороже, чем прочитать файл. Тот же приём у Telegram, VK и Битрикса.
//
// Запуск: node tmux-web/tests/frontend/meet_contract.test.mjs
// Код возврата 0 — расхождений нет, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');

let failures = 0;
let checks = 0;

function eq(name, actual, expected, hint) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) return;
    failures += 1;
    console.error(
        `[FAIL] ${name}\n       получено:  ${a}\n       ожидалось: ${e}${hint ? `\n       ${hint}` : ''}`,
    );
}

function ok(name, cond, detail) {
    checks += 1;
    if (cond) return;
    failures += 1;
    console.error(`[FAIL] ${name}${detail ? `\n       ${detail}` : ''}`);
}

// Заглушки браузера: ws.js тянет core/auth.js, который на верхнем уровне
// подменяет window.fetch.
globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
globalThis.location = { protocol: 'http:', host: 'h', href: 'http://h/', origin: 'http://h', hash: '', pathname: '/', search: '' };
globalThis.history = { replaceState() {} };
globalThis.window = globalThis;

const WS = await import(new URL('../../static/js/meet/ws.js', import.meta.url).href);
const API = await import(new URL('../../static/js/meet/api.js', import.meta.url).href);
const CORE = await import(new URL('../../static/js/messenger/meet-core.js', import.meta.url).href);
const STORE = await import(new URL('../../static/js/messenger/meet-store.js', import.meta.url).href);

const routesRs = read('../../../plugins/meet/src/routes/mod.rs');
const protocolRs = read('../../../plugins/meet/src/ws/protocol.rs');
const providersRs = read('../../../plugins/meet/src/providers.rs');
const statusRs = read('../../../plugins/meet/src/status.rs');
const repoRs = read('../../../plugins/meet/src/db/repo/mod.rs');

// ===========================================================================
// 1. РОУТЫ В ОБЕ СТОРОНЫ
// ===========================================================================

/** Строки таблицы `//! | метод | `путь` | фаза | …` из шапки routes/mod.rs. */
function rustRoutes(src) {
    const out = [];
    for (const line of src.split('\n')) {
        const m = /^\/\/!\s*\|\s*(GET|POST|PUT|PATCH|DELETE)\s*\|\s*`([^`]+)`\s*\|/.exec(line);
        if (m) out.push(`${m[1]} ${m[2]}`);
    }
    return out;
}

const backendRoutes = rustRoutes(routesRs);
const frontRoutes = API.MEET_ROUTES.map((r) => `${r.method} ${r.path}`);

ok('таблица путей в routes/mod.rs непуста', backendRoutes.length >= 19, `найдено ${backendRoutes.length}`);
eq('порядок и состав роутов совпадают дословно', frontRoutes, backendRoutes,
    'разъехавшийся путь даёт молчаливый 404, а не ошибку сборки');

// В обе стороны поимённо — чтобы в отчёте было видно КАКОЙ роут лишний.
for (const r of backendRoutes) {
    ok(`роут бэкенда «${r}» есть в api.js`, frontRoutes.includes(r));
}
for (const r of frontRoutes) {
    ok(`клиент «${r}» есть в таблице бэкенда`, backendRoutes.includes(r));
}

// Каждому роуту соответствует реальная экспортированная функция.
for (const r of API.MEET_ROUTES) {
    const mod = r.path === '/ws/meet' ? WS : API;
    ok(`${r.fn} экспортирована (${r.path})`, typeof mod[r.fn] === 'function');
}

// `/ws/meet/ingest` НЕ СОЗДАЁТСЯ НИКОГДА: звук идёт через CDP-биндинг, а не
// через наш HTTP-сервер. Исключение в csrf_guard по одноразовому токену
// превратило бы плагин записи встреч в drive-by RCE (токен лежит в памяти
// страницы, которой мы ещё и CSP отключили).
//
// ОТСУТСТВИЕ РОУТА — ТАКОЕ ЖЕ РЕШЕНИЕ, КАК ЕГО НАЛИЧИЕ, поэтому оно стоит в
// таблице отдельной строкой (с прочерком вместо метода, чтобы разбор выше не
// принял её за роут) и проверяется здесь с трёх сторон: строка на месте,
// роутер его не регистрирует, фронт про него не знает.
ok('строка про намеренно отсутствующий /ws/meet/ingest есть в таблице',
    /^\/\/!\s*\|\s*—\s*\|\s*`\/ws\/meet\/ingest`\s*\|/m.test(routesRs),
    'без строки в таблице отсутствие роута выглядит как забывчивость, и его однажды «вернут»');
ok('и роутер его действительно не регистрирует',
    !/\.route\(\s*"\/ws\/meet\/ingest"/.test(routesRs));
ok('во фронте нет ни одного упоминания /ws/meet/ingest',
    !read('../../static/js/meet/api.js').includes('/ws/meet/ingest')
    && !read('../../static/js/meet/ws.js').includes('/ws/meet/ingest'));

// ===========================================================================
// 2. КАДРЫ WS В ОБЕ СТОРОНЫ
// ===========================================================================

/** Имена вариантов `enum MeetServerMsg` в порядке объявления, snake_case. */
function rustFrameTags(src) {
    const head = src.indexOf('pub enum MeetServerMsg {');
    if (head < 0) throw new Error('в protocol.rs не найден `pub enum MeetServerMsg`');
    const body = src.slice(head);
    const end = body.indexOf('\n}\n');
    if (end < 0) throw new Error('не найден конец объявления MeetServerMsg');
    const lines = body.slice(0, end).split('\n');
    for (const line of lines) {
        // Явный rename на ВАРИАНТЕ сломал бы вывод тега из имени.
        if (/^ {4}#\[serde\(rename\s*=/.test(line)) {
            throw new Error(`вариант с явным serde(rename): ${line.trim()}`);
        }
    }
    ok('теги кадров выводятся из имён вариантов (rename_all = snake_case)',
        src.includes('#[serde(tag = "type", rename_all = "snake_case")]'),
        'без атрибута теги были бы CamelCase, и разбор ниже врал бы');
    return lines
        .map((l) => /^ {4}([A-Z][A-Za-z0-9]*)\s*[({,]/.exec(l))
        .filter(Boolean)
        .map((m) => m[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
}

const backendFrames = rustFrameTags(protocolRs);
const frontFrames = WS.SERVER_FRAMES;

ok('набор кадров в protocol.rs непуст', backendFrames.length >= 10, `найдено ${backendFrames.length}`);
eq('кадры совпадают по составу И ПОРЯДКУ', frontFrames, backendFrames,
    'порядок значим: он делает будущий diff читаемым — видно, добавили кадр или переставили');
for (const f of backendFrames) {
    ok(`кадр «${f}» имеет обработчик в ws.js`, Object.prototype.hasOwnProperty.call(WS.FRAME_HANDLERS, f));
}
for (const f of frontFrames) {
    ok(`обработчик «${f}» отвечает существующему кадру`, backendFrames.includes(f));
}

// SERVER_FRAMES выводится ИЗ карты, а не пишется рядом: два списка разъехались
// бы, и тест сравнивал бы список сам с собой.
ok('SERVER_FRAMES выводится из FRAME_HANDLERS',
    read('../../static/js/meet/ws.js').includes('Object.keys(FRAME_HANDLERS)'));

// --- КАДРЫ КЛИЕНТА → СЕРВЕРА ----------------------------------------------
//
// Их ровно один (`pong`), и отправляет его ядро, а не этот модуль. Проверка
// нужна не ради полноты списка, а ради того, чтобы у панели не завёлся ВТОРОЙ
// путь общения с сервером: сервер разбирает `MeetClientMsg` строго, и любой
// другой кадр он отвергает — молча для отправителя.
{
    const head = protocolRs.indexOf('pub enum MeetClientMsg {');
    ok('в protocol.rs есть MeetClientMsg', head > 0);
    const body = protocolRs.slice(head, protocolRs.indexOf('\n}\n', head));
    const clientFrames = [...body.matchAll(/^ {4}([A-Z][A-Za-z0-9]*)\s*[({,]?\s*$/gm)]
        .map((m) => m[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
    eq('клиент умеет говорить ровно одно слово', clientFrames, ['pong']);

    const wsSrc = read('../../static/js/meet/ws.js');
    const sends = [...wsSrc.matchAll(/JSON\.stringify\(\s*\{\s*type:\s*'([a-z_]+)'/g)].map((m) => m[1]);
    eq('панель не отправляет НИЧЕГО своего', sends, [],
        'ping/pong держит ядро; собственная отправка означала бы второй, непроверенный канал');
}

// Кадр очереди whisper — отдельно: он приходит, ПОКА СТАТУС ОСТАЁТСЯ
// `transcribing`, поэтому обработчик обязан не подменять им статус.
{
    ok('кадр progress несёт queued и queue_position',
        /queued: bool/.test(protocolRs) && /queue_position: Option<usize>/.test(protocolRs));
    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    STORE.linkMeeting('mq', STORE.meetKey('telegram', '1', 5), {
        id: 'mq', status: 'transcribing', summary_status: 'none', platform: 'telemost',
        started_at: 1, ended_at: 2, duration_ms: 1000,
    });
    WS.FRAME_HANDLERS.progress({ type: 'progress', meeting_id: 'mq', phase: 'transcribing', queued: true, queue_position: 2 });
    eq('очередь whisper не подменяет статус строки',
        STORE.peekMeetState(STORE.meetKey('telegram', '1', 5)).status, 'transcribing');
    eq('позиция в очереди доступна отдельно', STORE.getMeetProgress('mq').queuePosition, 2);
    STORE.resetMeetStore();
}

// Фаза прогресса — тоже перечисление на проводе.
{
    const head = protocolRs.indexOf('pub enum Phase {');
    const body = protocolRs.slice(head, protocolRs.indexOf('\n}\n', head));
    const phases = [...body.matchAll(/^ {4}([A-Z][A-Za-z]*),/gm)].map((m) => m[1].toLowerCase());
    eq('фазы пайплайна', phases, ['joining', 'recording', 'transcoding', 'transcribing', 'summarizing']);
}

// ===========================================================================
// 3. РЕЕСТР ПЛАТФОРМ — ПОЛЕ В ПОЛЕ, В ОБЕ СТОРОНЫ
// ===========================================================================

/** Все строковые литералы куска Rust (`"…"` и raw `r"…"`). */
function rustStrings(chunk) {
    return [...chunk.matchAll(/r?"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
}

/**
 * Значение поля-массива внутри блока `Provider { … }`.
 *
 * Построчный сбор, а не одна регулярка: шаблоны путей САМИ содержат `]`
 * (`[^/?#]{4,512}`), и нежадное `[\s\S]*?\]` обрезало бы их посередине.
 */
function rustArrayField(block, field) {
    const lines = block.split('\n');
    const start = lines.findIndex((l) => new RegExp(`^\\s*${field}: &\\[`).test(l));
    if (start < 0) return null;
    let collected = lines[start];
    if (/\],\s*$/.test(collected)) return rustStrings(collected);
    for (let i = start + 1; i < lines.length; i += 1) {
        collected += `\n${lines[i]}`;
        if (/^\s*\],\s*$/.test(lines[i])) break;
    }
    return rustStrings(collected);
}

/** `Platform::Telemost` → `telemost` (по match-арму `as_str`). */
const platformWire = (() => {
    const head = providersRs.indexOf('pub fn as_str(self)');
    const body = providersRs.slice(head, providersRs.indexOf('\n    }\n', head));
    const map = {};
    for (const m of body.matchAll(/Self::(\w+) => "([^"]+)"/g)) map[m[1]] = m[2];
    return map;
})();

eq('идентификатор Google Meet один и тот же в обеих формах', platformWire.GoogleMeet, 'meet',
    'serde-имя и as_str обязаны совпадать: первое уезжает в JSON и WS, второе — в базу и url_key');
ok('расхождение имени закрыто явным serde(rename)', /#\[serde\(rename = "meet"\)\]/.test(providersRs));

/** Блоки `Provider { … }` из константы PROVIDERS. */
function rustProviders(src) {
    const head = src.indexOf('pub const PROVIDERS: [Provider;');
    const body = src.slice(head, src.indexOf('\n];', head));
    return body.split('    Provider {').slice(1).map((block) => {
        const variant = /platform: Platform::(\w+)/.exec(block)[1];
        return {
            platform: platformWire[variant],
            title: /title: "([^"]+)"/.exec(block)[1],
            hosts: rustArrayField(block, 'hosts') || [],
            host_suffixes: rustArrayField(block, 'host_suffixes') || [],
            paths: rustArrayField(block, 'paths') || [],
            keep_params: rustArrayField(block, 'keep_params') || [],
            key_case: /key_case: "(\w+)"/.exec(block)[1],
            audio: /audio: (true|false)/.exec(block)[1] === 'true',
        };
    });
}

const backendProviders = rustProviders(providersRs);
const frontBuiltin = CORE.BUILTIN_MEET_PLATFORMS;

ok('в providers.rs разобраны все пять платформ', backendProviders.length === 5, `разобрано ${backendProviders.length}`);
ok('разбор Rust не пустой (иначе сверка ниже зеленела бы впустую)',
    backendProviders.every((p) => p.hosts.length > 0 && p.paths.length > 0),
    JSON.stringify(backendProviders.map((p) => [p.platform, p.hosts.length, p.paths.length])));

eq('состав и порядок платформ', frontBuiltin.map((p) => p.platform), backendProviders.map((p) => p.platform));

for (const back of backendProviders) {
    const front = frontBuiltin.find((p) => p.platform === back.platform);
    ok(`платформа «${back.platform}» есть во встроенном наборе фронта`, !!front);
    if (!front) continue;
    for (const field of ['title', 'hosts', 'host_suffixes', 'paths', 'keep_params', 'key_case', 'audio']) {
        eq(`${back.platform}.${field} совпадает с бэкендом`, front[field], back[field],
            'встроенный набор — ЗЕРКАЛО серверного реестра: он работает, пока /status не ответил, и протухнув врал бы молча');
    }
}
for (const front of frontBuiltin) {
    ok(`платформа «${front.platform}» есть в реестре бэкенда`,
        backendProviders.some((p) => p.platform === front.platform),
        'платформа, которой нет на сервере, дала бы кнопку записи на ссылке, которую бэкенд не умеет открыть');
}

// --- ОДНО НАПИСАНИЕ ПОЛЕЙ -------------------------------------------------

/** Имена полей `pub struct Provider`. */
const providerFields = (() => {
    const head = providersRs.indexOf('pub struct Provider {');
    const body = providersRs.slice(head, providersRs.indexOf('\n}\n', head));
    return [...body.matchAll(/^ {4}pub (\w+):/gm)].map((m) => m[1]);
})();

eq('поля Provider на проводе', providerFields,
    ['platform', 'title', 'hosts', 'host_suffixes', 'paths', 'keep_params', 'key_case', 'link_regex', 'audio']);

const builtinKeys = [...new Set(frontBuiltin.flatMap((p) => Object.keys(p)))];
eq('встроенная запись использует ТОЛЬКО проводные имена полей',
    builtinKeys.filter((k) => !providerFields.includes(k)), [],
    'лишнее имя означает, что фронт живёт в своей системе именования');
eq('из проводных полей фронт не использует ровно одно',
    providerFields.filter((k) => !builtinKeys.includes(k)), ['link_regex'],
    'link_regex отдаётся для подсветки и диагностики; детект работает по paths, и вторая регулярка ему не нужна');

// Нормализованная запись сохраняет ту же систему имён (плюс скомпилированные
// шаблоны, которых на проводе нет: JSON не умеет regexp).
{
    const normalized = CORE.meetPlatforms()[0];
    eq('нормализованная запись реестра', Object.keys(normalized).sort(),
        ['audio', 'host_suffixes', 'hosts', 'keep_params', 'key_case', 'matchers', 'paths', 'platform', 'title']);
}

// Живая проверка: настоящая форма ответа `/status` применяется целиком.
{
    const applied = CORE.configureMeetPlatforms({ providers: backendProviders });
    eq('реестр бэкенда применяется ЦЕЛИКОМ, без поштучных потерь', applied.length, backendProviders.length,
        'первая редакция бэкенда теряла записи молча — ровно этот случай');
    ok('и после этого ссылки пяти платформ распознаются',
        ['https://telemost.yandex.ru/j/60497752704486',
            'https://meet.google.com/abc-defg-hij',
            'https://us02web.zoom.us/j/89123456789',
            'https://teams.live.com/meet/9312345678901',
            'https://vcc.airlabs.pro/r/6b2-m7e-g9z'].every((u) => CORE.detectMeetUrl(u) !== null));
    CORE.resetMeetPlatforms();
}

// ===========================================================================
// 4. `localKey` ПРОТИВ `url_key` — РАЗВЕДЁННЫЕ ИМЕНА И РАЗВЕДЁННЫЕ РОЛИ
// ===========================================================================
//
// Ключ фронта и `url_key` бэкенда РАЗНЫЕ по построению: фронтовый не содержит
// хоста, не разворачивает `lookup/` у Meet и не сохраняет регистр процентного
// кодирования у Teams. Повторять правила идентичности во фронте нельзя — это
// была бы ВТОРАЯ реализация, расходящаяся с первой при первой же правке.
// Поэтому: идентичность считает ТОЛЬКО бэкенд, а фронтовая метка называется
// так, что её никто не отправит туда, где ждут `url_key`.

{
    const hit = CORE.detectMeetUrl('https://telemost.yandex.ru/j/60497752704486');
    eq('результат детекта несёт localKey', Object.keys(hit).sort(),
        ['audio', 'id', 'localKey', 'platform', 'raw', 'title', 'url']);
    ok('и НЕ несёт поля с именем url_key', !('url_key' in hit) && !('urlKey' in hit));

    const apiSrc = read('../../static/js/meet/api.js');
    const storeSrc = read('../../static/js/messenger/meet-store.js');
    const coreSrc = read('../../static/js/messenger/meet-core.js');
    const strip = (s) => s.split('\n').filter((l) => !l.trimStart().startsWith('//') && !l.trimStart().startsWith('*')).join('\n');

    ok('api.js не отправляет localKey ни в одном запросе', !/localKey/.test(strip(apiSrc)),
        'на сервер уезжает канонический url, а идентичность комнаты бэкенд считает по нему сам');
    ok('api.js вообще не оперирует url_key', !/url_key/.test(strip(apiSrc)));
    ok('стор не оперирует url_key', !/url_key/.test(strip(storeSrc)));
    ok('meet-core не строит url_key', !/url_key/.test(strip(coreSrc)));
    ok('ключ кэша строится из тройки, а не из ссылки',
        /plugin\}\$\{MEET_KEY_SEP\}\$\{chatId\}\$\{MEET_KEY_SEP\}\$\{messageId/.test(storeSrc));

    // Бэкенд, наоборот, считает идентичность и включает в неё хост у трёх
    // платформ из четырёх. Ответ на вопрос «зеркала Телемоста — одна комната
    // или разные» ЗАПИСАН и не является догадкой: хост остаётся частью
    // идентичности, пока живая проба не покажет обратное.
    ok('бэкенд по-прежнему считает url_key сам', /pub fn parse_meeting_url/.test(providersRs));
    ok('и включает хост в идентичность у всех, кроме Zoom',
        /Platform::Zoom => String::new\(\)/.test(providersRs) && /_ => host\.clone\(\)/.test(providersRs));
    ok('решение про зеркала записано в шапке meet-core.js',
        /ХОСТ ОСТАЁТСЯ\n\/\/ ЧАСТЬЮ ИДЕНТИЧНОСТИ/.test(coreSrc),
        'вопрос «telemost.yandex.ru и .com — одна комната?» не должен остаться догадкой');
}

// ===========================================================================
// 5. СТАТУСЫ И ТЕРМИНАЛЬНОСТЬ
// ===========================================================================

function rustConstList(src, name) {
    const head = src.indexOf(name);
    const body = src.slice(head, src.indexOf('];', head));
    return [...body.matchAll(/MeetStatus::(\w+),/g)]
        .map((m) => m[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
}

const allStatuses = rustConstList(statusRs, 'pub const ALL: [MeetStatus; 13]');
const activeStatuses = rustConstList(statusRs, 'pub const ACTIVE: [MeetStatus; 6]');
// Терминальные читаются из `matches!(self, Self::A | Self::B | …)` — сколько бы
// их ни было. Прежняя редакция ждала РОВНО ТРЁХ альтернатив и, появись
// четвёртая, молча упала бы на `m[1]` вместо того, чтобы назвать расхождение.
const terminalStatuses = (() => {
    const body = statusRs.slice(statusRs.indexOf('pub fn is_terminal'));
    const m = /matches!\(\s*self,\s*([^)]+)\)/.exec(body);
    if (!m) throw new Error('в status.rs не разобрался is_terminal');
    return m[1]
        .split('|')
        .map((x) => x.trim().replace(/^Self::/, ''))
        .filter(Boolean)
        .map((x) => x.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase());
})();

eq('тринадцать статусов разобраны', allStatuses.length, 13);
ok('разбор is_terminal не выродился', terminalStatuses.length >= 3, JSON.stringify(terminalStatuses));
eq('терминальные статусы совпадают с is_terminal', STORE.TERMINAL_STATUSES, terminalStatuses,
    'разойдись эти списки — фронт начал бы перезапрашивать то, что уже никогда не изменится');
eq('активные статусы совпадают с MeetStatus::ACTIVE', STORE.ACTIVE_STATUSES, activeStatuses,
    'по этому списку горит индикатор и тикает таймер записи');
for (const s of [...STORE.TERMINAL_STATUSES, ...STORE.ACTIVE_STATUSES]) {
    ok(`статус «${s}» существует на бэкенде`, allStatuses.includes(s));
}
ok('псевдостатуса none на бэкенде НЕТ — и это намеренно',
    !allStatuses.includes('none'),
    'статуса detected в базе нет: иначе таблица росла бы от простого скролла ленты. Отсутствие бейджа И ЕСТЬ «можно записать»');
eq('фронтовое имя этого псевдостатуса', STORE.STATUS_NONE, 'none');

// ---------------------------------------------------------------------------
// 5a. У КАЖДОГО СТАТУСА ЕСТЬ ВИД КНОПКИ
// ---------------------------------------------------------------------------
//
// Добавленный на бэкенде статус обязан сломать ЭТОТ тест, а не нарисоваться на
// кнопке видом «можно записать» — то есть соврать пользователю. Таблица видов
// живёт во фронте (meet-row.js), список статусов — в Rust, и единственный
// способ не дать им разойтись молча — сверить их напрямую.
{
    const ROW = await import(new URL('../../static/js/messenger/meet-row.js', import.meta.url).href);
    const views = Object.keys(ROW.MEET_VIEWS);
    for (const s of allStatuses) {
        ok(`у статуса «${s}» есть глиф и подпись на кнопке`,
            Object.prototype.hasOwnProperty.call(ROW.MEET_VIEWS, s),
            'без записи в MEET_VIEWS статус нарисовался бы видом «можно записать»');
        if (ROW.MEET_VIEWS[s]) {
            ok(`статус «${s}» закреплён на строке (виден без наведения)`,
                ROW.MEET_VIEWS[s].pinned === true);
        }
    }
    eq('лишних видов во фронте нет', views.filter((v) => v !== 'none' && !allStatuses.includes(v)), [],
        'вид без статуса — мёртвая ветка: её никто не увидит и никто не заметит, что она сломалась');
    eq('«можно записать» НЕ закреплено', ROW.MEET_VIEWS[STORE.STATUS_NONE].pinned, false,
        'иначе кнопка мозолила бы глаз на каждой ссылке в переписке');
}

// ---------------------------------------------------------------------------
// 5b. У КАЖДОГО СТАТУСА ЕСТЬ НАБОР ДЕЙСТВИЙ В КАРТОЧКЕ
// ---------------------------------------------------------------------------
//
// Пропуск здесь не падает и не пишет в консоль: `ACTS_BY_STATUS[status] ||
// ACTS_BY_STATUS.none` подставляет набор псевдостатуса, то есть предлагает
// «ЗАПИСАТЬ» там, где записывать нечего. Ровно так `pruned` (Ф7) и прожил фазу
// без своей строки — этот тест написан после того, как это нашлось.
{
    const POPUP = await import(new URL('../../static/js/messenger/meet-popup.js', import.meta.url).href);
    const acts = POPUP.ACTS_BY_STATUS;
    ok('таблица действий экспортирована', !!acts && typeof acts === 'object');
    for (const s of allStatuses) {
        ok(`у статуса «${s}» есть СВОЙ набор действий`,
            Object.prototype.hasOwnProperty.call(acts, s),
            'без своей строки статус молча получает кнопки псевдостатуса none');
    }
    eq('лишних наборов действий нет',
        Object.keys(acts).filter((k) => k !== 'none' && !allStatuses.includes(k)), [],
        'набор без статуса — мёртвая ветка');

    // Имена действий — не свободный текст: каждое обязано быть известно
    // обработчику, иначе кнопка нарисуется и не сделает ничего.
    const popupSrc = read('../../static/js/messenger/meet-popup.js');
    const known = new Set(
        [...popupSrc.matchAll(/name === '([a-z]+)'/g)].map((m) => m[1]),
    );
    known.add('record');
    for (const name of new Set(Object.values(acts).flat())) {
        ok(`действие «${name}» обрабатывается в runAct`, known.has(name),
            'кнопка есть, обработчика нет — нажатие не делает ничего и не сообщает об этом');
    }
}

// ---------------------------------------------------------------------------
// 5c. СТАТУС САММАРИ И ЭТАПЫ ПОВТОРА
// ---------------------------------------------------------------------------

{
    const summaryStatuses = (() => {
        const head = statusRs.indexOf('pub enum SummaryStatus {');
        const body = statusRs.slice(head, statusRs.indexOf('\n}\n', head));
        return [...body.matchAll(/^ {4}([A-Z][A-Za-z]*),/gm)].map((m) => m[1].toLowerCase());
    })();
    eq('статусы саммари', summaryStatuses, ['none', 'pending', 'ready', 'failed']);

    // Фронт сравнивает `summaryStatus` с литералами. Литерал, которого нет в
    // enum'е, — это ветка, которая не выполнится никогда: «ПЕРЕСЧИТАТЬ
    // САММАРИ» не появится, и понять почему будет неоткуда.
    const rowSrc = read('../../static/js/messenger/meet-row.js');
    const popupSrc = read('../../static/js/messenger/meet-popup.js');
    const literals = new Set([
        ...[...rowSrc.matchAll(/SUMMARY_FAILED\s*=\s*'([a-z]+)'/g)].map((m) => m[1]),
        ...[...popupSrc.matchAll(/summaryStatus === '([a-z]+)'/g)].map((m) => m[1]),
    ]);
    ok('фронт вообще сравнивает статус саммари', literals.size > 0);
    for (const v of literals) {
        ok(`литерал статуса саммари «${v}» существует на бэкенде`, summaryStatuses.includes(v));
    }

    // Этапы повтора (`retry?from=`) — перечисление `pipeline::run::Stage`.
    const runRs = read('../../../plugins/meet/src/pipeline/run.rs');
    const stages = (() => {
        const head = runRs.indexOf('pub enum Stage {');
        const body = runRs.slice(head, runRs.indexOf('\n}\n', head));
        return [...body.matchAll(/^ {4}([A-Z][A-Za-z]*),/gm)].map((m) => m[1].toLowerCase());
    })();
    eq('этапы повтора', stages, ['transcoding', 'transcribing', 'summarizing']);
    const asked = [...popupSrc.matchAll(/runAct\('retry',\s*'([a-z]+)'\)/g)].map((m) => m[1]);
    ok('карточка просит повтор хотя бы одного этапа', asked.length > 0);
    for (const st of asked) {
        ok(`этап повтора «${st}» существует на бэкенде`, stages.includes(st),
            'неизвестный `from` даёт 400, и выглядит это как «кнопка не работает»');
    }
}

// ---------------------------------------------------------------------------
// 5d. НАСТРОЙКИ: БЕЛЫЙ СПИСОК БЭКЕНДА ↔ ФОРМА ВО ФРОНТЕ
// ---------------------------------------------------------------------------
//
// Форма рисуется ПО ОТВЕТУ сервера, поэтому новое поле появится в ней само —
// но БЕЗ русской подписи (ключ подписывается собой) и, если у него новый
// раздел, вообще без места в форме. Ни то, ни другое не падает.
{
    const TAB = await import(new URL('../../static/js/settings/meet-tab.js', import.meta.url).href);
    const configRs = read('../../../plugins/meet/src/config.rs');
    const head = configRs.indexOf('pub const EDITABLE: &[EditableField] = &[');
    const body = configRs.slice(head, configRs.indexOf('\n];', head));
    const fields = [...body.matchAll(/key: "(\w+)"[\s\S]*?group: "(\w+)"/g)]
        .map((m) => ({ key: m[1], group: m[2] }));

    ok('белый список полей разобран', fields.length >= 20, `разобрано ${fields.length}`);
    for (const f of fields) {
        ok(`поле «${f.key}» подписано по-русски`,
            Object.prototype.hasOwnProperty.call(TAB.MEET_LABELS, f.key),
            'без подписи пользователь видит имя ключа из TOML');
        ok(`раздел «${f.group}» есть в форме`,
            TAB.MEET_GROUPS.some((g) => g.key === f.group),
            'поле с неизвестным разделом не попадёт ни в один блок формы');
    }
    eq('мёртвых подписей нет',
        Object.keys(TAB.MEET_LABELS).filter((k) => !fields.some((f) => f.key === k)), [],
        'подпись без поля переживёт удаление настройки и будет вводить в заблуждение');
    eq('пустых разделов нет',
        TAB.MEET_GROUPS.map((g) => g.key).filter((k) => k !== 'readonly' && !fields.some((f) => f.group === k)),
        [],
        'раздел без единого поля нарисуется пустой рамкой');
}

// ---------------------------------------------------------------------------
// 5e. ОТВЕТ `/api/meet/status` ↔ ТО, ЧТО ЧИТАЕТ ГЕЙТ
// ---------------------------------------------------------------------------
//
// Гейт — единственная точка, где фронт узнаёт про плагин вообще. Поле,
// переименованное на сервере, не даёт ни ошибки, ни лога: `status.providers`
// станет `undefined`, реестр молча останется встроенным, а `status.enabled !==
// true` вообще выключит кнопку встречи на всей странице.
{
    const dtoFields = (() => {
        const head = routesRs.indexOf('pub struct StatusDto {');
        const body = routesRs.slice(head, routesRs.indexOf('\n}\n', head));
        return [...body.matchAll(/^ {4}pub (\w+):/gm)].map((m) => m[1]);
    })();
    eq('поля ответа /api/meet/status', dtoFields,
        // `cloud` — блок Ф15: настроен ли ключ (МАСКОЙ), живость ключа, остаток
        // лимита и выбранные модели. Отдаётся всегда, в том числе когда обе
        // стадии локальные.
        ['enabled', 'db_ready', 'deps', 'limits', 'providers', 'active', 'config', 'cloud']);

    const apiSrc = read('../../static/js/meet/api.js');
    for (const f of ['enabled', 'providers']) {
        ok(`гейт читает поле «${f}»`, new RegExp(`status\\.${f}\\b`).test(apiSrc),
            'поле контракта, от которого зависит включение плагина во фронте');
    }
    // И наоборот: гейт не читает того, чего сервер не отдаёт.
    const readByGate = [...apiSrc.matchAll(/status\.(\w+)/g)].map((m) => m[1]);
    for (const f of new Set(readByGate)) {
        ok(`гейт читает существующее поле «${f}»`, dtoFields.includes(f),
            'обращение к несуществующему полю даёт undefined и ни одной ошибки');
    }
}

// ===========================================================================
// 6. БЕЙДЖ НА ПРОВОДЕ ↔ РАЗБОР ВО ФРОНТЕ
// ===========================================================================

const badgeFields = (() => {
    const head = repoRs.indexOf('pub struct MeetingBadge {');
    const body = repoRs.slice(head, repoRs.indexOf('\n}\n', head));
    return [...body.matchAll(/^ {4}pub (\w+):/gm)].map((m) => m[1]);
})();

eq('поля MeetingBadge', badgeFields,
    ['meeting_id', 'status', 'summary_status', 'platform', 'started_at', 'ended_at', 'duration_ms']);

{
    const storeSrc = read('../../static/js/messenger/meet-store.js');
    for (const f of badgeFields) {
        ok(`стор читает поле бейджа «${f}»`, storeSrc.includes(`src.${f}`));
    }
    // BadgeEntry сплющивает бейдж и добавляет адрес строки; `plugin` живёт в
    // конверте ответа, а не в записи.
    ok('BadgeEntry плоский и несёт chat_id + message_id',
        /pub struct BadgeEntry \{[\s\S]*?chat_id: String,[\s\S]*?message_id: i64,[\s\S]*?serde\(flatten\)/.test(protocolRs));
}

// --- ⚠ ЕДИНИЦЫ ВРЕМЕНИ НА ПРОВОДЕ РАЗНЫЕ ----------------------------------
//
// `started_at`/`ended_at` — unix-СЕКУНДЫ, `duration_ms` — МИЛЛИСЕКУНДЫ.
// Сложение секунд с Date.now() дало бы таймер записи на полвека.

{
    ok('бэкенд пишет метки времени в СЕКУНДАХ', /pub\(crate\) fn now\(\) -> i64 \{\s*\n\s*chrono::Utc::now\(\)\.timestamp\(\)/.test(repoRs),
        'timestamp() — секунды; timestamp_millis() был бы другим контрактом');
    ok('а duration_ms бэкенд считает в миллисекундах',
        read('../../../plugins/meet/src/db/repo/meetings.rs').includes('(e - s) * 1000'));

    STORE.resetMeetStore();
    STORE.configureMeetStore({ fetchStates: async () => ({ badges: [] }) });
    STORE.linkMeeting('m-u', STORE.meetKey('telegram', '1', 1), {
        id: 'm-u', status: 'recording', summary_status: 'none', platform: 'telemost',
        started_at: 1700000000, ended_at: null, duration_ms: null,
    });
    const st = STORE.peekMeetState(STORE.meetKey('telegram', '1', 1));
    eq('секунды провода переведены в миллисекунды на границе стора', st.startedAtMs, 1700000000000,
        'единица зашита В ИМЯ поля именно для того, чтобы её нельзя было перепутать молча');
    eq('и таймер считается уже в одной единице с Date.now()',
        STORE.elapsedMs(st.startedAtMs, 1700000060000), 60000);
    ok('поля без единицы в имени в состоянии нет', !('startedAt' in st) && !('endedAt' in st));
    STORE.resetMeetStore();
}

// ===========================================================================
// 7. ПОТОЛОК ДИАПАЗОНА БАТЧА
// ===========================================================================

{
    const m = /pub const MAX_BADGE_RANGE: i64 = (\d+);/.exec(routesRs);
    ok('MAX_BADGE_RANGE найден в routes/mod.rs', !!m);
    eq('потолок диапазона совпадает', STORE.MAX_BADGE_RANGE, Number(m[1]),
        'запрос шире серверного потолка даёт 400, и выглядит это как «кнопки не появились»');
    ok('запрос батча несёт ровно четыре параметра, которые разбирает сервер',
        ['plugin', 'chat_id', 'min_id', 'max_id'].every((p) => routesRs.includes(`"${p}" =>`))
        && ['plugin', 'chat_id', 'min_id', 'max_id'].every((p) => read('../../static/js/meet/api.js').includes(`${p}:`)));
}

// ===========================================================================
// 8. ТЕСТ НЕ ТРЕБУЕТ СЕРВЕРА И НЕ ХОДИТ В СЕТЬ
// ===========================================================================

ok('ни один модуль не был подключён к настоящему сокету', WS._debugMeetWs().created === false);
ok('гейт не ходил на сервер', API.meetGateRequests() === 0);

// ===========================================================================

if (failures === 0) console.log(`[OK] meet_contract: ${checks} проверок`);
else console.error(`[FAIL] meet_contract: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
