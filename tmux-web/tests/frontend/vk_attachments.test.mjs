// Регресс-тест нормализации вложений панели VK (js/vk/attachments.js).
//
// Модуль новый — аналога в js/telegram/* нет: у Telegram вложение одно на
// сообщение, у VK их до десяти плюс рекурсивные пересылки.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ:
//
//   1. ИНВАРИАНТ url === null. Прямая ссылка VK, попавшая в <img src>, — это
//      и протухшая картинка через сутки, и запрос браузера пользователя на
//      серверы VK с его cookie. Тест проверяет ВСЕ поля результата на
//      отсутствие http-адресов, кроме playerUrl (он ведёт на страницу-плеер и
//      предназначен ровно для открытия во вкладке);
//   2. ИНДЕКСЫ НЕ СДВИГАЮТСЯ. index входит в ключ медиа-кэша, и «выбросим
//      неизвестное вложение» показало бы пользователю не тот файл;
//   3. ГЛУБИНА ПЕРЕСЫЛОК ОГРАНИЧЕНА. У VK цепочка не ограничена ничем, и
//      наивная рекурсия рисует экран из одного сообщения.
//
// Запуск: node tmux-web/tests/frontend/vk_attachments.test.mjs

const A = await import(new URL('../../static/js/vk/attachments.js', import.meta.url).href);

let failures = 0;
let checks = 0;

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

/** MediaDto в том виде, в каком его отдаёт бэкенд. */
function media(index, kind, extra) {
    return Object.assign({
        index,
        kind,
        mime: null,
        size: null,
        file_name: null,
        title: null,
        width: null,
        height: null,
        duration_secs: null,
        has_thumb: false,
        has_full: false,
        player_url: null,
        link_url: null,
        waveform: [],
    }, extra || {});
}

// ---------------------------------------------------------------------------
// Таблица видов.
// ---------------------------------------------------------------------------

const kindTable = [
    ['photo', 'photo'],
    ['video', 'video'],
    ['audio', 'audio'],
    ['voice', 'voice'],
    ['document', 'document'],
    ['sticker', 'sticker'],
    ['graffiti', 'graffiti'],
    ['other', 'other'],
    // Сырые имена VK — приезжают в кадрах WS и в превью пересланных.
    ['audio_message', 'voice'],
    ['doc', 'document'],
    ['wall', 'other'],
    ['wall_reply', 'other'],
    ['link', 'other'],
    ['market', 'other'],
    ['market_album', 'other'],
    ['poll', 'other'],
    ['gift', 'other'],
    ['call', 'other'],
    ['story', 'other'],
    ['podcast', 'other'],
    // Из будущего — карточка обязана нарисоваться, а не исчезнуть.
    ['app_action', 'unsupported'],
    ['hologram', 'unsupported'],
    ['', 'unsupported'],
];
for (const [raw, expected] of kindTable) {
    eq(A.attachmentKind(raw), expected, `attachmentKind('${raw}')`);
    eq(A.attachmentKind({ kind: raw }), expected, `attachmentKind({kind:'${raw}'})`);
}
eq(A.attachmentKind(null), 'unsupported', 'attachmentKind(null)');
eq(A.attachmentKind({ type: 'doc' }), 'document', 'сырое поле type тоже читается');

// ---------------------------------------------------------------------------
// Нормализация массива.
// ---------------------------------------------------------------------------

const msg = {
    id: 7001,
    dialog_id: '1234',
    text: '',
    attachments: [
        media(0, 'photo', { width: 1920, height: 1080, has_thumb: true, has_full: true }),
        media(1, 'unsupported'),
        media(2, 'voice', { duration_secs: 7, has_full: true, mime: 'audio/ogg' }),
        media(3, 'video', {
            title: 'Ролик',
            duration_secs: 95,
            has_thumb: true,
            has_full: false,
            player_url: 'https://vk.com/video_ext.php?oid=1',
        }),
        media(4, 'document', { file_name: 'смета.pdf', size: 4096, has_full: true }),
        // Ссылка: превью ЕСТЬ (регресс живого дефекта), файла нет, адрес свой.
        media(5, 'other', {
            title: 'Войти в Битрикс24',
            width: 1200,
            height: 630,
            has_thumb: true,
            has_full: false,
            link_url: 'https://psph.bitrix24.ru/crm/contact/details/27307/',
        }),
    ],
};

const list = A.normalizeAttachments(msg);
eq(list.length, 6, 'все вложения доехали');
eq(list.map((a) => a.index), [0, 1, 2, 3, 4, 5], 'ПОРЯДОК И ИНДЕКСЫ сохранены');
eq(list.map((a) => a.kind), ['photo', 'unsupported', 'voice', 'video', 'document', 'other'],
    'неизвестный вид НЕ выброшен — иначе индексы сдвинулись бы');
eq(list[0].width, 1920, 'геометрия');
eq(list[2].durationSecs, 7, 'длительность голосового');
eq(list[4].fileName, 'смета.pdf', 'имя документа');
eq(list[4].size, 4096, 'размер');

// Воспроизведение и превью.
eq(list[2].playable, true, 'голосовое с файлом воспроизводится');
eq(list[3].playable, false, 'видео у VK не воспроизводится: файла во вложении нет');
eq(list[3].playerUrl, 'https://vk.com/video_ext.php?oid=1', 'страница-плеер доезжает');
eq(list[0].thumbable, true, 'у фото есть превью');
eq(list[2].thumbable, false, 'у голосового превью не бывает');
eq(list[5].thumbable, true,
    'РЕГРЕСС: у ссылки превью бывает — модель обязана это донести');
eq(list[5].playable, false, 'ссылка не воспроизводится');
eq(list[5].linkUrl, 'https://psph.bitrix24.ru/crm/contact/details/27307/',
    'адрес ссылки доезжает до модели');
eq(list[0].linkUrl, null, 'у фото адреса ссылки нет');

// ГЛАВНЫЙ ИНВАРИАНТ: адресов ФАЙЛОВ VK в модели нет.
//
// `url` — поле-ловушка, всегда null: рендер, который попробует им
// воспользоваться, сломается заметно. Исключены из проверки ровно два поля, и
// ни одно не адресует CDN вложений: `playerUrl` — страница-плеер (файла у
// video не существует), `linkUrl` — адрес, который прислал собеседник.
for (const a of list) {
    eq(a.url, null, `url всегда null (index ${a.index})`);
}
const dump = JSON.stringify(list.map((a) => {
    const { raw, playerUrl, linkUrl, ...rest } = a;
    return rest;
}));
ok(!/https?:\/\//.test(dump),
    `в модели нет http-адресов, кроме playerUrl и linkUrl: ${dump}`);

// Ограничение длины выдачи не трогает индексы.
const capped = A.normalizeAttachments(msg, { max: 2 });
eq(capped.length, 2, 'max ограничивает длину');
eq(capped.map((a) => a.index), [0, 1], 'индексы остаются исходными');

// Пустое и вырожденное.
eq(A.normalizeAttachments(null), [], 'null не роняет');
eq(A.normalizeAttachments({}), [], 'сообщение без вложений');
eq(A.normalizeAttachments({ attachments: [null, undefined, 1] }).length, 0, 'мусор отбрасывается');
// Старый кадр WS: одно поле media без массива.
eq(A.normalizeAttachments({ media: media(0, 'photo') }).length, 1, 'одиночное media поддержано');

eq(A.hasRenderableMedia(msg), true, 'есть что рисовать');
eq(A.hasRenderableMedia({ text: 'привет' }), false, 'чистый текст');
eq(A.hasRenderableMedia({ attachments: [media(0, 'other')] }), true,
    'опрос/ссылка тоже рисуются карточкой — иначе сообщение выглядело бы пустым');

// ---------------------------------------------------------------------------
// Первое вложение и суффикс строки.
// ---------------------------------------------------------------------------

eq(A.primaryAttachment(list).index, 0, 'в свёрнутой строке — первое вложение');
eq(A.primaryAttachment([]), null, 'нечего показывать');
eq(A.primaryAttachment(null), null, 'null не роняет');

eq(A.attachmentsSummary([]), '', 'без вложений суффикса нет');
eq(A.attachmentsSummary(list.slice(0, 1)), '', 'одно вложение — суффикс не нужен');
eq(A.attachmentsSummary(list.slice(0, 2)), '▣? 2 вложения', 'два вложения');
eq(A.attachmentsSummary(list), '▣?∿▶▤◌ 6 вложений', 'шесть вложений');
const ten = Array.from({ length: 10 }, (_, i) => ({ index: i, kind: 'photo' }));
const summary = A.attachmentsSummary(ten);
eq(summary, '▣▣▣▣▣▣ 10 вложений', 'глифов не больше MAX_INLINE_CARDS, счёт — полный');
eq(A.MAX_INLINE_CARDS, 6, 'потолок карточек в строке');
eq(A.attachmentsSummary(Array.from({ length: 21 }, (_, i) => ({ index: i, kind: 'photo' })))
    .endsWith('21 вложение'), true, 'склонение: 21 вложение');

// ---------------------------------------------------------------------------
// Пересланные: ПЛОСКАЯ форма от сервера.
// ---------------------------------------------------------------------------

const withFwd = {
    id: 7002,
    fwd: [
        { depth: 0, sender: { first_name: 'Аня' }, date: 1, text: 'первое', omitted: 0 },
        { depth: 1, sender: { first_name: 'Боря' }, date: 2, text: 'вложенное', omitted: 0 },
        // Уровень, который UI уже не рисует (сервер отдал на один глубже).
        { depth: 2, sender: { first_name: 'Вера' }, date: 3, text: 'слишком глубоко', omitted: 0 },
        { depth: 0, sender: { first_name: 'Гена' }, date: 4, text: 'последнее', omitted: 7 },
    ],
};
const flat = A.flattenForwards(withFwd);
eq(flat.rows.map((r) => r.depth), [0, 1, 0], 'глубже MAX_FWD_DEPTH не рисуем');
eq(flat.rows.map((r) => r.msg.text), ['первое', 'вложенное', 'последнее'], 'порядок сохранён');
eq(flat.truncatedCount, 8, 'скрытые = серверный остаток (7) + отброшенный уровень (1)');
eq(A.forwardsTail(flat.truncatedCount), '…ещё 8 пересланных', 'подпись хвоста');
eq(A.forwardsTail(0), '', 'скрывать нечего — хвоста нет');
eq(A.forwardsTail(1), '…ещё 1 пересланное', 'склонение единицы');

// Явная глубина.
eq(A.flattenForwards(withFwd, 1).rows.map((r) => r.depth), [0, 0], 'maxDepth=1');
eq(A.flattenForwards(withFwd, 3).rows.length, 4, 'maxDepth=3 показывает всё');

// ---------------------------------------------------------------------------
// Пересланные: СЫРАЯ рекурсивная форма (кадр WS, вложенность внутри reply).
// ---------------------------------------------------------------------------

function tree(depth, width) {
    if (depth === 0) return [];
    return Array.from({ length: width }, (_, i) => ({
        text: `d${depth}-${i}`,
        fwd_messages: tree(depth - 1, width),
    }));
}
const deep = A.flattenForwards({ fwd_messages: tree(5, 3) });
ok(deep.rows.every((r) => r.depth < A.MAX_FWD_DEPTH), 'рекурсия обрезана по глубине');
eq(deep.rows.length, 3 + 9, 'два уровня по три ветви');
ok(deep.truncatedCount > 0, 'отброшенное посчитано');
// Самое важное: функция ЗАВЕРШАЕТСЯ на глубоком дереве.
ok(Number.isFinite(deep.rows.length), 'разбор не уходит в бесконечную рекурсию');

eq(A.flattenForwards({}).rows, [], 'сообщение без пересылок');
eq(A.flattenForwards(null).truncatedCount, 0, 'null не роняет');

// ---------------------------------------------------------------------------
// Превью пересланного.
// ---------------------------------------------------------------------------

eq(A.forwardPreview({ sender: { first_name: 'Аня' }, text: 'привет' }), 'Аня: привет', 'текст');
eq(A.forwardPreview({ sender: { screen_name: 'anya' }, text: 'привет' }), 'anya: привет',
    'screen_name важнее имени');
eq(A.forwardPreview({ sender: { first_name: 'Аня' }, text: '', media_kind: 'photo' }),
    'Аня: ▣ photo', 'ПУСТОЙ ТЕКСТ ПРИ ВЛОЖЕНИИ — превью не пустеет');
eq(A.forwardPreview({ text: '', media_kind: 'audio_message' }), '∿ voice',
    'сырой вид вложения нормализуется');
eq(A.forwardPreview({}), '—', 'ни текста, ни вложения');
ok(A.forwardPreview({ sender: { first_name: 'Аня' }, text: 'я'.repeat(500) }).length
    < 200, 'длинный текст обрезается');

// ---------------------------------------------------------------------------

console.log(`vk_attachments: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
