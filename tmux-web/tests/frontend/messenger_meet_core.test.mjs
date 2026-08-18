// ⚠ СТРАЖ ДЕТЕКТА ССЫЛОК НА ВСТРЕЧИ (фронтовая Ф1 плагина forge-meet).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/messenger/meet-core.js через
// `await import()`. Модуль намеренно не трогает ни DOM, ни сеть, поэтому в
// голом Node работает как есть — стенд, сервер и браузер не нужны.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Адрес, признанный здесь встречей, уезжает на
// бэкенд, и там его открывает НАСТОЯЩИЙ Chrome с отключённым CSP и нашим
// инжектированным скриптом внутри. То есть ложное срабатывание детекта — это
// чужая страница, затащенная в наш браузерный контекст по ссылке от
// постороннего человека в чате. Секция 2 (отрицательные кейсы) и секция 6
// (grep по исходнику) — не «покрытие», а СТРАЖИ: они обязаны краснеть, если
// кто-то заменит сравнение хостов подстрочным поиском или притащит в модуль
// DOM.
//
// Семь секций:
//   1. Положительные формы ссылок четырёх платформ (~25 адресов).
//   2. ОТРИЦАТЕЛЬНЫЕ КЕЙСЫ — поимённо каждый вектор обхода.
//   3. Нормализация: белый список параметров, стабильность ключа.
//   4. Реестр платформ снаружи (ответ GET /api/meet/status).
//   5. Три источника текста: Telegram, VK, Битрикс.
//   6. Исходник модуля: ни DOM, ни сети, ни четвёртой копии safeUrl.
//   7. Устойчивость: мусор, длинный вход, глубокое дерево, таймаут.
//
// Запуск: node tmux-web/tests/frontend/messenger_meet_core.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const modPath = new URL('../../static/js/messenger/meet-core.js', import.meta.url);
const M = await import(modPath.href);
const FMT = await import(new URL('../../static/js/bitrix/format.js', import.meta.url).href);

let failures = 0;
let checks = 0;
const started = Date.now();

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

/** Положительный кейс: адрес обязан дать платформу и ключ. */
function hit(url, platform, key, label) {
    const r = M.detectMeetUrl(url);
    checks += 1;
    if (!r) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       детект вернул null: ${url}`);
        return null;
    }
    if (r.platform !== platform || r.localKey !== key) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${platform} / ${key}\n       получено:  ${r.platform} / ${r.localKey}`);
    }
    return r;
}

/** Отрицательный кейс: адрес НЕ ИМЕЕТ ПРАВА стать встречей. */
function miss(url, label) {
    const r = M.detectMeetUrl(url);
    checks += 1;
    if (r !== null) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ОБХОД: ${url}\n       распознано как: ${JSON.stringify(r)}`);
    }
}

// ==========================================================================
// 1. Положительные формы ссылок четырёх платформ
// ==========================================================================

// Телемост. Обе ссылки сняты с ЖИВОЙ встречи (фаза 0), форма пути /j/<цифры>.
hit('https://telemost.yandex.ru/j/95149281991644', 'telemost', 'telemost:95149281991644', 'Телемост: живая ссылка №1');
hit('https://telemost.yandex.ru/j/60497752704486', 'telemost', 'telemost:60497752704486', 'Телемост: живая ссылка №2');
hit('http://telemost.yandex.ru/j/60497752704486', 'telemost', 'telemost:60497752704486', 'Телемост: http тоже ссылка');
hit('https://telemost.yandex.com/j/12345678901234', 'telemost', 'telemost:12345678901234', 'Телемост: домен .com');
hit('https://telemost.yandex.by/j/12345678901234', 'telemost', 'telemost:12345678901234', 'Телемост: домен .by');
hit('https://telemost.yandex.kz/j/12345678901234', 'telemost', 'telemost:12345678901234', 'Телемост: домен .kz');
hit('https://telemost.yandex.ru/j/60497752704486/', 'telemost', 'telemost:60497752704486', 'Телемост: завершающий слэш');
hit('https://TELEMOST.YANDEX.RU/j/60497752704486', 'telemost', 'telemost:60497752704486', 'Телемост: ВЕРХНИЙ регистр хоста');
hit('https://telemost.yandex.ru./j/60497752704486', 'telemost', 'telemost:60497752704486', 'Телемост: завершающая точка в имени хоста');

// Google Meet.
hit('https://meet.google.com/abc-defg-hij', 'meet', 'meet:abc-defg-hij', 'Meet: код встречи');
hit('https://meet.google.com/abc-defg-hij?authuser=0&hs=122', 'meet', 'meet:abc-defg-hij', 'Meet: мусорные параметры');
hit('https://meet.google.com/ABC-DEFG-HIJ', 'meet', 'meet:abc-defg-hij', 'Meet: код в верхнем регистре даёт ТОТ ЖЕ ключ');
hit('https://meet.google.com/lookup/ab3cd4ef5g', 'meet', 'meet:ab3cd4ef5g', 'Meet: именная ссылка /lookup/');

// Zoom.
hit('https://zoom.us/j/89123456789', 'zoom', 'zoom:89123456789', 'Zoom: корневой домен');
hit('https://us02web.zoom.us/j/89123456789', 'zoom', 'zoom:89123456789', 'Zoom: поддомен площадки');
hit('https://company-name.zoom.us/j/89123456789', 'zoom', 'zoom:89123456789', 'Zoom: корпоративный поддомен');
hit('https://us02web.zoom.us/j/89123456789?pwd=aBcD1234.xyz', 'zoom', 'zoom:89123456789', 'Zoom: ссылка с паролем');
hit('https://zoom.us/wc/join/89123456789', 'zoom', 'zoom:89123456789', 'Zoom: веб-клиент /wc/join/');
hit('https://zoom.us/wc/89123456789/join', 'zoom', 'zoom:89123456789', 'Zoom: веб-клиент /wc/<id>/join');
hit('https://zoom.us/s/89123456789', 'zoom', 'zoom:89123456789', 'Zoom: ссылка организатора /s/');
hit('https://zoom.us/my/igor.room', 'zoom', 'zoom:igor.room', 'Zoom: личная комната /my/');
hit('https://www2.zoomgov.com/j/16123456789', 'zoom', 'zoom:16123456789', 'Zoom: государственный контур zoomgov');

// Microsoft Teams.
hit(
    'https://teams.microsoft.com/l/meetup-join/19%3ameeting_NGRiZmM%40thread.v2/0?context=%7B%22Tid%22%3A%22t1%22%2C%22Oid%22%3A%22o1%22%7D',
    'teams', 'teams:19:meeting_NGRiZmM@thread.v2', 'Teams: полная ссылка meetup-join',
);
hit(
    'https://teams.microsoft.com/l/meetup-join/19%3Ameeting_NGRiZmM%40thread.v2/0?context=%7B%22Tid%22%3A%22t1%22%7D',
    'teams', 'teams:19:meeting_NGRiZmM@thread.v2', 'Teams: %3A и %3a дают ОДИН ключ',
);
hit('https://teams.live.com/meet/9312345678901?p=SeCrEtPass', 'teams', 'teams:9312345678901', 'Teams: личная ссылка teams.live.com');
hit('https://teams.microsoft.us/l/meetup-join/19%3ameeting_X%40thread.v2/0', 'teams', 'teams:19:meeting_X@thread.v2', 'Teams: государственный контур .us');

{
    const r = M.detectMeetUrl('https://telemost.yandex.ru/j/95149281991644?ncrnd=8812');
    ok(r !== null, 'Телемост: ncrnd не мешает детекту');
    eq(r.url, 'https://telemost.yandex.ru/j/95149281991644', 'Телемост: ncrnd срезан из канонического адреса');
    eq(r.raw, 'https://telemost.yandex.ru/j/95149281991644?ncrnd=8812', 'исходный адрес сохранён как есть');
    eq(r.id, '95149281991644', 'идентификатор комнаты отделён от ключа');
    eq(r.title, 'Яндекс Телемост', 'подпись платформы пришла из реестра');
    ok(M.isMeetUrl('https://telemost.yandex.ru/j/95149281991644'), 'isMeetUrl — короткая форма');
}

// ==========================================================================
// 2. ⚠ ОТРИЦАТЕЛЬНЫЕ КЕЙСЫ — ГЛАВНАЯ ЧАСТЬ ФАЙЛА
// ==========================================================================
//
// Первые четыре — ровно те обходы, ради которых в модуле запрещён подстрочный
// поиск по адресу. Каждый из них выглядит для человека как ссылка на встречу.

miss('https://telemost.yandex.ru.evil.com/j/1', 'ОБХОД 1: поддомен-обманка telemost.yandex.ru.evil.com');
miss('https://evil.com/?r=telemost.yandex.ru', 'ОБХОД 2: домен в query');
miss('https://evil.com/j/12345?next=https://telemost.yandex.ru/j/1', 'ОБХОД 2б: домен в query с правдоподобным путём');
miss('https://evil.com#telemost.yandex.ru', 'ОБХОД 3: домен во фрагменте');
miss('https://evil.com/j/12345#https://telemost.yandex.ru/j/1', 'ОБХОД 3б: домен во фрагменте с правдоподобным путём');
miss('https://telemost.yandex.ru@evil.com/j/1', 'ОБХОД 4: домен в userinfo (hostname = evil.com)');
miss('https://meet.google.com@evil.com/abc-defg-hij', 'ОБХОД 4б: userinfo с доменом Meet');

// Схема — забота safeHref. Все формы одного и того же `javascript:`.
miss('javascript:alert(1)//telemost.yandex.ru/j/1', 'СХЕМА: javascript:');
miss(' JaVaScRiPt:alert(1)//telemost.yandex.ru/j/1', 'СХЕМА: javascript: вразнобой с ведущим пробелом');
miss('java\nscript:alert(1)//telemost.yandex.ru/j/1', 'СХЕМА: javascript: с переводом строки внутри');
miss('data:text/html,<script>1</script>//telemost.yandex.ru/j/1', 'СХЕМА: data:');
miss('file:///telemost.yandex.ru/j/1', 'СХЕМА: file:');
// Путь и хост здесь НАСТОЯЩИЕ, отличается только схема: если белый список
// схем однажды выпадет, покраснеют именно эти две строки. У `javascript:` и
// `data:` хоста нет вовсе, поэтому они одни такой страж не образуют.
miss('vk://telemost.yandex.ru/j/60497752704486', 'СХЕМА: чужой deep-link с настоящим хостом и путём');
miss('ftp://telemost.yandex.ru/j/60497752704486', 'СХЕМА: ftp с настоящим хостом и путём');
miss('telemost.yandex.ru/j/1234', 'СХЕМА: относительный адрес без схемы');
miss('//telemost.yandex.ru/j/1234', 'СХЕМА: протокол-относительный адрес');

// Хост похож, но не тот.
miss('https://notzoom.us/j/89123456789', 'ХОСТ: notzoom.us не совпадает с суффиксом .zoom.us');
miss('https://zoom.us.evil.com/j/89123456789', 'ХОСТ: zoom.us.evil.com');
miss('https://telemost.yandex.ru.attacker.net/j/1', 'ХОСТ: длинная поддоменная обманка');
miss('https://meet.google.com.evil.com/abc-defg-hij', 'ХОСТ: meet.google.com.evil.com');
miss('https://teams.microsoft.com.evil.com/l/meetup-join/19%3ax%40thread.v2/0', 'ХОСТ: teams.microsoft.com.evil.com');
miss('https://evil-meet.google.com/abc-defg-hij', 'ХОСТ: meet.google.com без суффиксного правила не растягивается');
miss('https://xn--telemost-yandex.ru/j/1', 'ХОСТ: punycode-похожий домен');

// Правильный хост, но путь не встреча.
miss('https://telemost.yandex.ru/', 'ПУТЬ: корень Телемоста');
miss('https://telemost.yandex.ru/about', 'ПУТЬ: страница Телемоста');
miss('https://telemost.yandex.ru/j/', 'ПУТЬ: /j/ без идентификатора');
miss('https://telemost.yandex.ru/j/abcdef', 'ПУТЬ: /j/ с нецифровым идентификатором');
miss('https://telemost.yandex.ru/redirect/j/12345678', 'ПУТЬ: незаякоренный шаблон совпал бы здесь');
miss('https://meet.google.com/', 'ПУТЬ: корень Meet');
miss('https://meet.google.com/landing', 'ПУТЬ: страница Meet');
miss('https://zoom.us/download', 'ПУТЬ: страница загрузки Zoom');
miss('https://zoom.us/j/123', 'ПУТЬ: слишком короткий идентификатор Zoom');
miss('https://teams.microsoft.com/', 'ПУТЬ: корень Teams');

// Мусор и края.
miss('', 'КРАЙ: пустая строка');
miss(null, 'КРАЙ: null');
miss(undefined, 'КРАЙ: undefined');
miss(42, 'КРАЙ: число');
miss({ url: 'https://telemost.yandex.ru/j/1234' }, 'КРАЙ: объект вместо строки');
miss('https://', 'КРАЙ: одна схема');
miss(`https://telemost.yandex.ru/j/1234?x=${'a'.repeat(4096)}`, 'КРАЙ: адрес длиннее потолка');

// ==========================================================================
// 3. Нормализация: белый список параметров и стабильность ключа
// ==========================================================================

{
    // ПАРОЛЬ ZOOM. Срез всей query сломал бы вход — здесь это закреплено.
    const r = M.detectMeetUrl('https://us02web.zoom.us/j/89123456789?pwd=SeCrEt123&uname=%D0%98&omn=1');
    eq(r.url, 'https://us02web.zoom.us/j/89123456789?pwd=SeCrEt123', 'Zoom: pwd сохранён, остальное срезано');
}
{
    // CONTEXT TEAMS. Обязательная часть ссылки.
    const r = M.detectMeetUrl(
        'https://teams.microsoft.com/l/meetup-join/19%3ameeting_A%40thread.v2/0?context=%7B%22Tid%22%3A%22t%22%7D&anon=true&launchAgent=join_launcher',
    );
    ok(r.url.includes('context='), 'Teams: context сохранён');
    ok(!r.url.includes('anon='), 'Teams: лишние параметры срезаны');
    ok(!r.url.includes('launchAgent'), 'Teams: launchAgent срезан');
    ok(decodeURIComponent(r.url).includes('{"Tid":"t"}'), 'Teams: содержимое context не искажено');
}
{
    const r = M.detectMeetUrl('https://teams.live.com/meet/9312345678901?p=PaSs&foo=1');
    eq(r.url, 'https://teams.live.com/meet/9312345678901?p=PaSs', 'Teams: код доступа p сохранён');
}
{
    // Один и тот же ключ у разных записей одной встречи — это и есть дедуп.
    const forms = [
        'https://telemost.yandex.ru/j/60497752704486',
        'https://telemost.yandex.ru/j/60497752704486/',
        'https://telemost.yandex.ru/j/60497752704486?ncrnd=1',
        'https://telemost.yandex.ru/j/60497752704486?ncrnd=2#top',
        'https://TELEMOST.yandex.ru/j/60497752704486',
        'https://telemost.yandex.ru./j/60497752704486',
        'https://user@telemost.yandex.ru/j/60497752704486',
    ];
    const keys = new Set(forms.map((f) => M.detectMeetUrl(f).localKey));
    const urls = new Set(forms.map((f) => M.detectMeetUrl(f).url));
    eq(keys.size, 1, 'семь записей одной встречи дают ОДИН ключ');
    eq(urls.size, 1, 'семь записей одной встречи дают ОДИН канонический адрес');
    eq([...urls][0], 'https://telemost.yandex.ru/j/60497752704486', 'канонический адрес без якоря, userinfo и мусора');
}
{
    // Порядок параметров во входе не должен менять канонический адрес.
    const a = M.detectMeetUrl('https://zoom.us/j/89123456789?pwd=x&extra=1');
    const b = M.detectMeetUrl('https://zoom.us/j/89123456789?extra=1&pwd=x');
    eq(a.url, b.url, 'порядок query не влияет на канонический адрес');
}
{
    const r = M.detectMeetUrl('https://telemost.yandex.ru:8443/j/60497752704486');
    eq(r.url, 'https://telemost.yandex.ru:8443/j/60497752704486', 'нестандартный порт сохраняется');
}
{
    // Хвостовая пунктуация фразы не должна попадать в адрес.
    const c = M.collectUrlsFromText('созвон https://telemost.yandex.ru/j/60497752704486. Начали!');
    eq(c, ['https://telemost.yandex.ru/j/60497752704486'], 'точка в конце фразы срезана');
    const c2 = M.collectUrlsFromText('(ссылка: https://zoom.us/j/89123456789?pwd=a)');
    eq(c2, ['https://zoom.us/j/89123456789?pwd=a'], 'непарная закрывающая скобка срезана');
    const c3 = M.collectUrlsFromText('см. https://meet.google.com/abc-defg-hij, потом https://zoom.us/j/89123456789');
    eq(c3.length, 2, 'два адреса в одной строке');
    const c4 = M.collectUrlsFromText('видеоhttps://telemost.yandex.ru/j/1234');
    eq(c4, [], 'адрес, приклеенный к слову, не кандидат');
}

// ==========================================================================
// 4. Реестр платформ приходит снаружи (GET /api/meet/status)
// ==========================================================================

{
    const applied = M.configureMeetPlatforms([
        {
            platform: 'jitsi',
            title: 'Jitsi',
            hosts: ['meet.jit.si'],
            host_suffixes: [],
            paths: ['/([A-Za-z0-9]{4,64})'],
            keep_params: ['jwt'],
            key_case: 'preserve',
            audio: true,
        },
    ]);
    eq(applied.length, 1, 'реестр подменён целиком');
    const r = M.detectMeetUrl('https://meet.jit.si/ForgeStandup?jwt=abc&foo=1');
    eq(r && r.localKey, 'jitsi:ForgeStandup', 'новая платформа из реестра распознаётся');
    eq(r.url, 'https://meet.jit.si/ForgeStandup?jwt=abc', 'белый список параметров пришёл вместе с платформой');
    eq(r.title, 'Jitsi', 'подпись берётся из поля title, как на проводе');
    eq(M.detectMeetUrl('https://telemost.yandex.ru/j/60497752704486'), null, 'встроенный набор ЗАМЕНЁН, а не дополнен');
    M.resetMeetPlatforms();
    ok(M.detectMeetUrl('https://telemost.yandex.ru/j/60497752704486') !== null, 'сброс возвращает встроенный набор');
}
{
    // Ответ бэкенда целиком (тело `/api/meet/status`), а не голый массив.
    M.configureMeetPlatforms({ providers: [{ platform: 'x', hosts: ['x.example'], paths: ['/r/(\\d+)'] }] });
    eq(M.detectMeetUrl('https://x.example/r/42').localKey, 'x:42', 'реестр принят из поля providers');
    M.resetMeetPlatforms();
}
{
    // ОДНО НАПИСАНИЕ ПОЛЕЙ. Каждая строка ниже — форма, которая РАНЬШЕ
    // принималась «на всякий случай». Теперь она обязана быть отвергнута,
    // иначе переименование поля на бэкенде однажды пройдёт молча и оставит
    // фронт на встроенном наборе — без единого сообщения.
    const wrongSpellings = [
        { label: 'id вместо platform', rec: { id: 'w', hosts: ['w.example'], paths: ['/r/(\\d+)'] } },
        { label: 'key вместо platform', rec: { key: 'w', hosts: ['w.example'], paths: ['/r/(\\d+)'] } },
        { label: 'domains вместо hosts', rec: { platform: 'w', domains: ['w.example'], paths: ['/r/(\\d+)'] } },
        { label: 'path_patterns вместо paths', rec: { platform: 'w', hosts: ['w.example'], path_patterns: ['/r/(\\d+)'] } },
        { label: 'patterns вместо paths', rec: { platform: 'w', hosts: ['w.example'], patterns: ['/r/(\\d+)'] } },
    ];
    for (const { label, rec } of wrongSpellings) {
        // Рядом ставим ЗАВЕДОМО ЗДОРОВУЮ запись: без неё реестр стал бы
        // пустым и откатился на встроенный, и проверка зеленела бы по
        // случайной причине, а не потому, что запись отвергнута.
        M.configureMeetPlatforms([rec, { platform: 'ok', hosts: ['ok.example'], paths: ['/r/(\\d+)'] }]);
        eq(M.meetPlatforms().map((p) => p.platform), ['ok'], `${label}: запись отвергнута`);
        eq(M.detectMeetUrl('https://w.example/r/42'), null, `${label}: ссылка не распознаётся`);
        M.resetMeetPlatforms();
    }
    // Второе имя обёртки тоже убрано: раньше принималось `{platforms: […]}`.
    M.configureMeetPlatforms({ platforms: [{ platform: 'x', hosts: ['x.example'], paths: ['/r/(\\d+)'] }] });
    eq(M.detectMeetUrl('https://x.example/r/42'), null, 'обёртка platforms больше не принимается — только providers');
    ok(M.detectMeetUrl('https://telemost.yandex.ru/j/60497752704486') !== null, 'и такой ответ откатывается на встроенный набор');
    M.resetMeetPlatforms();
}
{
    // Поля, которых на проводе нет, игнорируются молча — но и не заменяют
    // проводные: camelCase-написание НЕ включает платформу.
    M.configureMeetPlatforms([
        { platform: 'c', hosts: ['c.example'], paths: ['/j/(\\d+)'], keyCase: 'lower', hostSuffixes: ['.c.example'], params: ['pwd'] },
    ]);
    const p = M.meetPlatforms()[0];
    eq(p.key_case, 'preserve', 'keyCase (camelCase) не действует — регистр остаётся исходным');
    eq(p.host_suffixes, [], 'hostSuffixes (camelCase) не действует — суффиксов нет');
    eq(p.keep_params, [], 'params (camelCase) не действует — белый список пуст');
    eq(M.detectMeetUrl('https://c.example/j/7?pwd=x').url, 'https://c.example/j/7', 'параметр из непринятого написания срезан');
    M.resetMeetPlatforms();
}
{
    // audio: false — «ссылку узнаём, кнопку не рисуем» (кандидат — Zoom).
    M.configureMeetPlatforms([{ platform: 'mute', hosts: ['m.example'], paths: ['/j/(\\d+)'], audio: false }]);
    eq(M.detectMeetUrl('https://m.example/j/7').audio, false, 'признак audio доезжает до результата детекта');
    M.resetMeetPlatforms();
    eq(M.detectMeetUrl('https://telemost.yandex.ru/j/60497752704486').audio, true, 'по умолчанию платформа считается звучащей');
}
{
    // Мусор НЕ обнуляет детект: «бэкенд ответил странно» не равно «кнопка
    // встречи исчезла везде».
    for (const junk of [null, undefined, [], 'нет', 42, {}, [null, 'x', 7]]) {
        M.configureMeetPlatforms(junk);
        ok(M.detectMeetUrl('https://telemost.yandex.ru/j/60497752704486') !== null,
            `мусорный реестр ${JSON.stringify(junk)} откатывается на встроенный`);
    }
    M.resetMeetPlatforms();
}
{
    // Одна битая запись не уносит остальные.
    M.configureMeetPlatforms([
        { platform: 'broken', hosts: ['b.example'], paths: ['/([unclosed'] },
        { platform: 'nohost', paths: ['/(\\d+)'] },
        { platform: 'nopath', hosts: ['c.example'] },
        { platform: 'good', hosts: ['g.example'], paths: ['/g/(\\d+)'] },
    ]);
    eq(M.meetPlatforms().length, 1, 'битые записи отброшены поштучно');
    eq(M.detectMeetUrl('https://g.example/g/7').localKey, 'good:7', 'здоровая запись работает');
    M.resetMeetPlatforms();
}
{
    // Суффикс без ведущей точки — это возвращённый подстрочный поиск, и
    // отбрасывается он ДАЖЕ КОГДА пришёл с бэкенда. Валидный хост в той же
    // записи оставлен нарочно: иначе запись выпала бы целиком, реестр стал бы
    // пустым и откатился на встроенный, и проверка ничего бы не доказала.
    M.configureMeetPlatforms([{ platform: 'z', hosts: ['zoom.example'], host_suffixes: ['zoom.us', '.ok.example'], paths: ['/j/(\\d+)'] }]);
    eq(M.meetPlatforms()[0].host_suffixes, ['.ok.example'], 'суффикс без ведущей точки выброшен, суффикс с точкой оставлен');
    eq(M.detectMeetUrl('https://notzoom.us/j/12345'), null, 'notzoom.us не совпал (подстрочный поиск не вернулся)');
    eq(M.detectMeetUrl('https://us02web.zoom.us/j/12345'), null, 'выброшенный суффикс не работает вовсе');
    ok(M.detectMeetUrl('https://a.ok.example/j/12345') !== null, 'корректный суффикс работает');
    ok(M.detectMeetUrl('https://zoom.example/j/12345') !== null, 'точный хост записи работает');
    M.resetMeetPlatforms();
}
{
    // Заякоренный шаблон с бэкенда не должен ломать завершающий слэш.
    M.configureMeetPlatforms([{ platform: 'a', hosts: ['a.example'], paths: ['^/j/(\\d+)$'] }]);
    ok(M.detectMeetUrl('https://a.example/j/12') !== null, 'заякоренный шаблон работает');
    ok(M.detectMeetUrl('https://a.example/j/12/') !== null, 'заякоренный шаблон принимает завершающий слэш');
    M.resetMeetPlatforms();
}
{
    eq(M.meetPlatforms().map((p) => p.platform), ['telemost', 'meet', 'zoom', 'teams'], 'встроенный реестр — четыре платформы');
    const copy = M.meetPlatforms();
    copy.length = 0;
    eq(M.meetPlatforms().length, 4, 'meetPlatforms отдаёт копию, а не внутренний массив');
    eq(M.MEET_PANELS, ['telegram', 'vk', 'bitrix'], 'список панелей закреплён');
}

// ==========================================================================
// 5. Три источника текста
// ==========================================================================

// --- Telegram: только msg.text -------------------------------------------
{
    const msg = {
        id: 7,
        text: 'встречаемся тут https://telemost.yandex.ru/j/95149281991644',
        media: { kind: 'photo', file_name: 'plan.png' },
    };
    const r = M.detectMeetInMessage(msg, 'telegram');
    eq(r && r.localKey, 'telemost:95149281991644', 'Telegram: сообщение с фото и ссылкой даёт ссылку');
    const cands = M.messageUrlCandidates(msg, 'telegram');
    ok(cands.every((c) => !c.includes('[')), 'Telegram: подпись [photo] в кандидаты НЕ попадает');
    eq(cands, ['https://telemost.yandex.ru/j/95149281991644'], 'Telegram: ровно один кандидат — сам адрес');
}
{
    // Голосовое без текста: подпись [voice 7s] существует только в рендере.
    const msg = { id: 8, text: '', media: { kind: 'voice', duration_secs: 7 } };
    eq(M.messageUrlCandidates(msg, 'telegram'), [], 'Telegram: медиа без текста не даёт кандидатов');
    eq(M.detectMeetInMessage(msg, 'telegram'), null, 'Telegram: медиа без текста — не встреча');
}
{
    const msg = { id: 9, text: 'плановая встреча', media: null };
    eq(M.detectMeetInMessage(msg, 'telegram'), null, 'Telegram: текст без ссылки — не встреча');
}

// --- VK: msg.text ПЛЮС вложение-ссылка -----------------------------------
{
    // ОСНОВНАЯ живая форма: бэкенд (attach.rs) схлопывает сырой тип `link` в
    // `other`, а адрес кладёт в link_url.
    const msg = {
        id: 10,
        text: '',
        attachments: [{ index: 0, kind: 'other', title: 'Яндекс Телемост', link_url: 'https://telemost.yandex.ru/j/60497752704486' }],
    };
    const r = M.detectMeetInMessage(msg, 'vk');
    eq(r && r.localKey, 'telemost:60497752704486', 'VK: ссылка ТОЛЬКО во вложении — детектится');
}
{
    // Сырое вложение из кадра WS предыдущей версии плагина.
    const msg = { id: 11, text: '', attachments: [{ type: 'link', link: { url: 'https://us02web.zoom.us/j/89123456789?pwd=Zz' } }] };
    const r = M.detectMeetInMessage(msg, 'vk');
    eq(r && r.localKey, 'zoom:89123456789', 'VK: сырое вложение type=link с адресом в link.url');
    eq(r.url, 'https://us02web.zoom.us/j/89123456789?pwd=Zz', 'VK: пароль Zoom из вложения сохранён');
}
{
    // Одиночное поле media — тоже кадр WS старой версии.
    const msg = { id: 12, text: '', media: { kind: 'other', link_url: 'https://meet.google.com/abc-defg-hij' } };
    eq(M.detectMeetInMessage(msg, 'vk').localKey, 'meet:abc-defg-hij', 'VK: одиночное поле media');
}
{
    const msg = {
        id: 13,
        text: 'подключайтесь https://telemost.yandex.ru/j/11112222333344',
        attachments: [{ index: 0, kind: 'other', link_url: 'https://meet.google.com/abc-defg-hij' }],
    };
    eq(M.detectMeetInMessage(msg, 'vk').localKey, 'telemost:11112222333344', 'VK: текст читается РАНЬШЕ вложений');
    eq(M.detectAllMeetsInMessage(msg, 'vk').map((x) => x.localKey),
        ['telemost:11112222333344', 'meet:abc-defg-hij'], 'VK: обе встречи в порядке появления');
}
{
    // Вложение-фото с превью не должно давать ложную встречу.
    const msg = { id: 14, text: '', attachments: [{ index: 0, kind: 'photo', has_thumb: true }] };
    eq(M.detectMeetInMessage(msg, 'vk'), null, 'VK: фото — не встреча');
}
{
    // Вложение-ссылка на посторонний сайт.
    const msg = { id: 15, text: '', attachments: [{ index: 0, kind: 'other', link_url: 'https://habr.com/ru/post/1' }] };
    eq(M.detectMeetInMessage(msg, 'vk'), null, 'VK: посторонняя ссылка во вложении — не встреча');
}

// --- Битрикс: обход segments ---------------------------------------------
{
    // Живая форма [URL=адрес]созвон[/URL]: адрес в value, текст внутри.
    const segments = [
        { kind: 'text', text: 'Всем привет, ', value: null, children: [] },
        {
            kind: 'url',
            value: 'https://telemost.yandex.ru/j/11112222333344',
            children: [{ kind: 'text', text: 'созвон', value: null, children: [] }],
        },
    ];
    const r = M.detectMeetInMessage({ id: 16, segments }, 'bitrix');
    eq(r && r.localKey, 'telemost:11112222333344', 'Битрикс: [URL=адрес]созвон[/URL] отдаёт АДРЕС');
    eq(r.raw, 'https://telemost.yandex.ru/j/11112222333344', 'Битрикс: в raw именно адрес, а не подпись');
    // Доказательство, что segmentsText() в одиночку здесь бессильна.
    eq(FMT.segmentsText(segments), 'Всем привет, созвон', 'segmentsText теряет адрес — потому и нужна своя сборка');
    ok(!FMT.segmentsText(segments).includes('telemost'), 'плоский текст сегментов НЕ содержит адреса');
}
{
    // Вторая живая форма: URL_PREVIEW="Y" оборачивает голую ссылку, атрибута
    // нет, адрес лежит текстом внутри (bbcode.rs).
    const segments = [{ kind: 'url', value: null, children: [{ kind: 'text', text: 'https://meet.google.com/xyz-abcd-pqr', value: null, children: [] }] }];
    eq(M.detectMeetInMessage({ id: 17, segments }, 'bitrix').localKey, 'meet:xyz-abcd-pqr', 'Битрикс: [URL]адрес[/URL] с value=null');
}
{
    // Превью выключено — ссылка приезжает обычным текстом.
    const segments = [{ kind: 'text', text: 'жми https://zoom.us/j/89123456789?pwd=q', value: null, children: [] }];
    const r = M.detectMeetInMessage({ id: 18, segments }, 'bitrix');
    eq(r && r.localKey, 'zoom:89123456789', 'Битрикс: голая ссылка в текстовом сегменте');
    eq(r.url, 'https://zoom.us/j/89123456789?pwd=q', 'Битрикс: пароль Zoom из текста сохранён');
}
{
    // Ссылка внутри цитаты и жирного — обход рекурсивный.
    const segments = [{
        kind: 'quote',
        value: null,
        children: [{ kind: 'b', value: null, children: [{ kind: 'url', value: 'https://telemost.yandex.ru/j/44443333222211', children: [] }] }],
    }];
    eq(M.detectMeetInMessage({ id: 19, segments }, 'bitrix').localKey, 'telemost:44443333222211', 'Битрикс: вложенная в цитату ссылка');
}
{
    // Запасное поле msg.text у кадров без дерева сегментов.
    eq(M.detectMeetInMessage({ id: 20, segments: [], text: 'https://telemost.yandex.ru/j/55556666777788' }, 'bitrix').localKey,
        'telemost:55556666777788', 'Битрикс: msg.text как запасной источник');
}
{
    const segments = [{ kind: 'user', value: '15', children: [{ kind: 'text', text: 'Максим', value: null, children: [] }] }];
    eq(M.detectMeetInMessage({ id: 21, segments }, 'bitrix'), null, 'Битрикс: упоминание пользователя — не встреча');
}
{
    // Несколько ссылок: берётся первая по порядку.
    const segments = [
        { kind: 'url', value: 'https://zoom.us/j/89123456789', children: [] },
        { kind: 'url', value: 'https://telemost.yandex.ru/j/60497752704486', children: [] },
    ];
    eq(M.detectMeetInMessage({ id: 22, segments }, 'bitrix').localKey, 'zoom:89123456789', 'Битрикс: первая ссылка по порядку');
    eq(M.detectAllMeetsInMessage({ id: 22, segments }, 'bitrix').length, 2, 'Битрикс: обе встречи доступны отдельно');
}
{
    // Дубль одной встречи в двух формах — один ключ.
    const segments = [
        { kind: 'url', value: 'https://telemost.yandex.ru/j/60497752704486?ncrnd=1', children: [] },
        { kind: 'text', text: 'она же https://telemost.yandex.ru/j/60497752704486', value: null, children: [] },
    ];
    eq(M.detectAllMeetsInMessage({ id: 23, segments }, 'bitrix').length, 1, 'дубли встречи схлопываются по ключу');
}

// --- Общие края источников ------------------------------------------------
{
    eq(M.messageUrlCandidates(null, 'vk'), [], 'сообщения нет — кандидатов нет');
    eq(M.messageUrlCandidates('строка', 'vk'), [], 'сообщение не объект — кандидатов нет');
    eq(M.detectMeetInMessage({}, 'telegram'), null, 'пустое сообщение — не встреча');
    eq(M.detectMeetInMessage({ text: 'https://telemost.yandex.ru/j/60497752704486' }, 'telegram').localKey,
        'telemost:60497752704486', 'панель telegram');
    // Неизвестная панель работает по всем источникам сразу.
    eq(M.detectMeetInMessage({ segments: [{ kind: 'url', value: 'https://zoom.us/j/89123456789', children: [] }] }, 'newpanel').localKey,
        'zoom:89123456789', 'неизвестная панель читает и сегменты тоже');
    eq(M.detectMeetInMessage({ text: 'https://telemost.yandex.ru/j/60497752704486' }, undefined).localKey,
        'telemost:60497752704486', 'панель не указана — текст всё равно читается');
}

// ==========================================================================
// 6. ⚠ СТРАЖ ЧИСТОТЫ МОДУЛЯ
// ==========================================================================

{
    const src = readFileSync(modPath, 'utf8');
    // Ни DOM, ни сети: иначе модуль перестанет тестироваться в голом Node и
    // потянет за собой стенд.
    for (const banned of ['document', 'window', 'fetch', 'localStorage', 'sessionStorage', 'XMLHttpRequest', 'navigator']) {
        const re = new RegExp(`(^|[^.\\w])${banned}\\b`);
        ok(!re.test(src), `в исходнике нет обращения к ${banned}`);
    }
    for (const banned of ['innerHTML', 'outerHTML', 'insertAdjacentHTML']) {
        ok(!src.includes(banned), `в исходнике нет ${banned}`);
    }
    // Схема проверяется ОБЩИМ safeHref, а не четвёртой копией safeUrl.
    ok(/import\s*\{[^}]*safeHref[^}]*\}\s*from\s*'\.\/markdown\.js'/.test(src), 'safeHref импортируется из messenger/markdown.js');
    ok(!/function\s+safeUrl\b/.test(src), 'своей копии safeUrl в модуле нет');
    ok(!/function\s+safeHref\b/.test(src), 'своей копии safeHref в модуле нет');
    // Матчинг платформы — только по hostname. Подстрочный поиск по всему
    // адресу вернул бы все четыре обхода разом.
    ok(!/\bhref\.includes\(/.test(src), 'нет подстрочного поиска по href');
    ok(!/\braw\.includes\(/.test(src), 'нет подстрочного поиска по исходной строке');
    ok(src.includes('url.hostname'), 'хост берётся из URL.hostname');
    // Плагин выключен по умолчанию: модуль не смеет ничего запускать сам.
    ok(!/setInterval|setTimeout|requestAnimationFrame/.test(src), 'модуль не заводит таймеров');
}

// ==========================================================================
// 7. Устойчивость
// ==========================================================================

{
    for (const junk of [null, undefined, 42, {}, [], true, Symbol.iterator]) {
        ok(M.collectUrlsFromText(junk).length === 0, `collectUrlsFromText терпит ${String(junk)}`);
        ok(M.collectUrlsFromSegments(junk).length === 0, `collectUrlsFromSegments терпит ${String(junk)}`);
    }
    ok(M.collectUrlsFromSegments([null, 'x', 7, {}, { kind: 'url' }]).length === 0, 'мусорные сегменты не роняют обход');
}
{
    // Глубокое дерево сегментов: ограничитель, а не переполнение стека.
    let deep = [{ kind: 'url', value: 'https://telemost.yandex.ru/j/60497752704486', children: [] }];
    for (let i = 0; i < 200; i += 1) deep = [{ kind: 'quote', value: null, children: deep }];
    ok(Array.isArray(M.collectUrlsFromSegments(deep)), 'дерево в 200 уровней не роняет обход');
    eq(M.collectUrlsFromSegments(deep).length, 0, 'глубже ограничителя ссылка не ищется');
}
{
    // Сообщение-простыня: кандидатов не больше потолка.
    const many = Array.from({ length: 400 }, (_, i) => `https://telemost.yandex.ru/j/6049775270${String(1000 + i)}`).join(' ');
    const c = M.collectUrlsFromText(many);
    ok(c.length <= 32, `кандидаты ограничены потолком (получено ${c.length})`);
    ok(M.detectMeetInMessage({ text: many }, 'telegram') !== null, 'первая встреча простыни всё равно находится');
}
{
    const garbage = 'https://\n://\nhttp:// https:///// ht tps://a.b <https://> "https://"';
    ok(Array.isArray(M.collectUrlsFromText(garbage)), 'мусорный текст не роняет разбор');
    for (const u of M.collectUrlsFromText(garbage)) ok(M.detectMeetUrl(u) === null, `мусор ${u} не встреча`);
}
{
    const long = `текст ${'я'.repeat(50000)} https://telemost.yandex.ru/j/60497752704486`;
    eq(M.detectMeetInMessage({ text: long }, 'telegram').localKey, 'telemost:60497752704486', 'длинный текст разбирается');
}
{
    const mixed = 'Созвон 🚀 https://telemost.yandex.ru/j/60497752704486 — начали!';
    eq(M.detectMeetInMessage({ text: mixed }, 'telegram').localKey, 'telemost:60497752704486', 'эмодзи и тире рядом с адресом');
}

// ==========================================================================
// 8. ЛАУНЧЕР TEAMS: встреча спрятана в параметре `url`
// ==========================================================================
//
// Рассылки Teams часто ведут не на встречу, а на страницу-лаунчер. Не разверни
// её фронт — белый список параметров выбросил бы `url` вместе со встречей, и
// на сервер уехала бы ссылка на страницу «скачайте приложение». Тот же разворот
// делает `providers.rs`: разойдись они, кнопка появлялась бы там, где сервер
// записывать отказывается (или наоборот — не появлялась бы там, где может).
{
    const direct = M.detectMeetUrl(
        'https://teams.microsoft.com/l/meetup-join/19%3ameeting_ZjQ%40thread.v2/0'
        + '?context=%7B%22Tid%22%3A%22t%22%7D',
    );
    ok(direct !== null, 'прямая ссылка Teams разбирается');

    const wrapped = M.detectMeetUrl(
        'https://teams.microsoft.com/dl/launcher/launcher.html'
        + '?url=https%3A%2F%2Fteams.microsoft.com%2Fl%2Fmeetup-join%2F19%253ameeting_ZjQ%2540thread.v2%2F0'
        + '%3Fcontext%3D%257B%2522Tid%2522%253A%2522t%2522%257D&type=meetup-join&deeplinkId=z',
    );
    ok(wrapped !== null, 'ссылка-лаунчер опознаётся как встреча');
    eq(wrapped && wrapped.localKey, direct.localKey, 'лаунчер и прямая ссылка — одна комната');
    ok(wrapped && wrapped.url.includes('meetup-join'), 'канонический адрес ведёт на встречу, а не на лаунчер');
    ok(wrapped && !wrapped.url.includes('launcher'), 'лаунчер не остался в каноническом адресе');

    // ОТНОСИТЕЛЬНЫЙ адрес с путём во фрагменте — самая частая форма рассылки.
    const hashed = M.detectMeetUrl(
        'https://teams.microsoft.com/dl/launcher/launcher.html'
        + '?url=%2F_%23%2Fl%2Fmeetup-join%2F19%253ameeting_ZjQ%2540thread.v2%2F0'
        + '%3Fcontext%3D%257B%2522Tid%2522%253A%2522t%2522%257D&type=meetup-join',
    );
    eq(hashed && hashed.localKey, direct.localKey, 'путь во фрагменте лаунчера разворачивается');

    // СТРАЖ: лаунчер не становится дырой. Параметр `url` приезжает из того же
    // чужого сообщения, и адрес наружу Teams встречей не является.
    ok(M.detectMeetUrl(
        'https://teams.microsoft.com/dl/launcher/launcher.html?url=https%3A%2F%2Fevil.com%2Fx',
    ) === null, 'лаунчер с чужим хостом внутри не встреча');
    // ГЛАВНЫЙ СЛУЧАЙ, РАДИ КОТОРОГО ПРОВЕРКА ХОСТА ВНУТРИ И СУЩЕСТВУЕТ: тут
    // внутренний адрес — ЗАКОННАЯ встреча, только чужой площадки. Без проверки
    // ссылка `teams.microsoft.com/…` опозналась бы как встреча Google Meet, и
    // бот пошёл бы не туда, куда ведёт видимый человеку адрес. Прочие случаи
    // (`evil.com`) отсеиваются и дальше по пути, а этот — нет.
    ok(M.detectMeetUrl(
        'https://teams.microsoft.com/dl/launcher/launcher.html'
        + '?url=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij',
    ) === null, 'лаунчер Teams не разворачивается во встречу чужой площадки');
    ok(M.detectMeetUrl(
        'https://teams.microsoft.com/dl/launcher/launcher.html?url=javascript%3Aalert(1)',
    ) === null, 'опасная схема внутри лаунчера отвергается');
    ok(M.detectMeetUrl('https://teams.microsoft.com/dl/launcher/launcher.html') === null,
        'лаунчер без параметра — просто страница');
    ok(M.detectMeetUrl(
        'https://meet.google.com/dl/launcher/launcher.html'
        + '?url=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij',
    ) === null, 'тот же путь у чужой платформы встречей не становится');
}

// ==========================================================================
// 9. ПЛОЩАДКА БЕЗ ЗВУКА: флаг доезжает до находки
// ==========================================================================
//
// `audio: false` в реестре означает измеренный факт «звук отсюда мы забрать не
// умеем», а не «пока не проверяли». Флаг обязан доехать до `attachMeetButton`,
// который по нему кнопку не рисует: обещание записи, которая гарантированно
// выйдет пустой, хуже молчания.
{
    const registry = [{
        platform: 'zoom',
        title: 'Zoom',
        hosts: ['zoom.us'],
        host_suffixes: ['.zoom.us'],
        paths: ['/j/(\\d{8,15})'],
        keep_params: ['pwd'],
        key_case: 'preserve',
        audio: false,
    }];
    M.configureMeetPlatforms(registry);
    const hit = M.detectMeetUrl('https://us02web.zoom.us/j/83123456789?pwd=x');
    ok(hit !== null, 'ссылка по-прежнему опознаётся');
    eq(hit && hit.audio, false, 'флаг «звука нет» доехал до находки');
    M.resetMeetPlatforms();
    const back = M.detectMeetUrl('https://us02web.zoom.us/j/83123456789?pwd=x');
    eq(back && back.audio, true, 'сброс реестра возвращает встроенное значение');
}

const elapsed = Date.now() - started;
ok(elapsed < 5000, `весь файл укладывается в 5 с (фактически ${elapsed} мс)`);

// ==========================================================================

if (failures === 0) console.log(`[OK] messenger_meet_core: ${checks} проверок`);
else console.error(`[FAIL] messenger_meet_core: ${failures} из ${checks}`);
process.exit(failures ? 1 : 0);
