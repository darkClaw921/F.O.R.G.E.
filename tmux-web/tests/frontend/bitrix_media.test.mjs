// Регресс-тест МЕДИА панели «Битрикс24» (Ф8) на фейковом DOM.
//
// Грузит РЕАЛЬНЫЕ модули `js/bitrix/{media,attachments,format,api}.js` поверх
// bitrix_dom_stub.mjs. Стаб обязан импортироваться ПЕРВЫМ: `core/dom.js`
// резолвит все `$b24*` один раз на верхнем уровне модуля.
//
// Что покрыто:
//   • НИ ОДНОГО ВНЕШНЕГО АДРЕСА: пути прокси собираются только из сегментов
//     плагина, а в разметке карточки нет `src` с чужим хостом;
//   • ЛЕНИВОСТЬ: построение карточки не делает ни одного запроса — байты
//     идут только по явному действию либо по `IntersectionObserver`;
//   • ДЕДУП: N одновременных `loadMedia` одного ключа = один `fetch`;
//   • REVOKE ПРИ СМЕНЕ ДИАЛОГА освобождает вложения и СОХРАНЯЕТ аватары —
//     они общие для всей панели и переиспользуются колонкой;
//   • `dropMessageMedia` бьёт ровно по одному сообщению;
//   • `teardownMedia` не оставляет ни одной живой ссылки;
//   • ключ карточки совпадает с серверным `media_cache::CacheKey::id`.
//
// Запуск: node tmux-web/tests/frontend/bitrix_media.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { flush, netCalls, setNet } from './bitrix_dom_stub.mjs';

// ---------------------------------------------------------------------------
// objectURL: в Node их нет вовсе, а модуль обязан звать revoke ровно столько
// раз, сколько было создано. Считаем и то, и другое.
// ---------------------------------------------------------------------------

const urls = { created: 0, revoked: [], live: new Set() };
globalThis.URL.createObjectURL = (blob) => {
    urls.created += 1;
    const u = `blob:b24/${urls.created}`;
    urls.live.add(u);
    return u;
};
globalThis.URL.revokeObjectURL = (u) => {
    urls.revoked.push(u);
    urls.live.delete(u);
};
// IntersectionObserver: в Node его нет, и без подмены модуль честно падает на
// «грузим сразу» (документированный фолбэк для очень старых браузеров). Здесь
// нужен именно НАБЛЮДАТЕЛЬ — им и доказывается ленивость: пока карточка не
// пересекла вьюпорт, запроса нет.
const observed = [];
globalThis.IntersectionObserver = class IntersectionObserver {
    constructor(cb) {
        this.cb = cb;
    }

    observe(node) {
        observed.push(node);
    }

    unobserve() {}

    disconnect() {
        observed.length = 0;
    }

    /** Ручная «прокрутка»: сообщает, что узел стал видимым. */
    fire(node) {
        this.cb([{ isIntersecting: true, target: node }]);
    }
};

globalThis.Blob = class Blob {
    constructor(parts) {
        this.size = (parts || []).reduce((n, p) => n + (p.length || p.byteLength || 0), 0);
    }
};

const F = await import(new URL('../../static/js/bitrix/format.js', import.meta.url).href);
const A = await import(new URL('../../static/js/bitrix/attachments.js', import.meta.url).href);
const api = await import(new URL('../../static/js/bitrix/api.js', import.meta.url).href);
const media = await import(new URL('../../static/js/bitrix/media.js', import.meta.url).href);
const state = await import(new URL('../../static/js/bitrix/state.js', import.meta.url).href);

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

/** Все узлы поддерева в порядке обхода. */
function walk(node, out = []) {
    if (!node) return out;
    out.push(node);
    for (const c of (node.childNodes || [])) walk(c, out);
    return out;
}

/** Ответ «байты файла» для подменённой сети. */
function bytesRes(payload, type) {
    return {
        ok: true,
        status: 200,
        headers: {
            get: (h) => {
                const k = String(h).toLowerCase();
                if (k === 'content-type') return type || 'application/octet-stream';
                if (k === 'content-length') return String(payload.length);
                return null;
            },
        },
        // Потока нет — модуль обязан честно упасть на `blob()`, а не сломаться.
        body: null,
        blob: async () => new globalThis.Blob([payload]),
    };
}

// ---------------------------------------------------------------------------
// 1. Ключи и адреса: только собственные пути плагина
// ---------------------------------------------------------------------------

{
    // Ключ дословно совпадает с серверным `media_cache::CacheKey::id` и с полем
    // `key` кадра `download_progress`. Расхождение = прогресс второго файла в
    // карточке первого.
    eq(F.mediaKey('chat27', 113, 0), 'chat27/113/0', 'ключ вложения');
    eq(F.mediaKey('19', '113', '2'), '19/113/2', 'ключ терпит строки');
    eq(F.mediaKey('chat27', 113), 'chat27/113/0', 'индекс по умолчанию — ноль');
    eq(F.avatarKey(17), 'avatar/17', 'ключ аватара');
    ok(!F.avatarKey(17).startsWith('chat'), 'пространства имён не пересекаются');

    eq(api.mediaUrl('chat27', 113, 0), '/api/bitrix/media/chat27/113/0', 'путь вложения');
    eq(api.avatarUrl(17), '/api/bitrix/media/avatar/17', 'путь аватара');
    // Чужая строка в сегменте обязана быть закодирована, а не разрезать путь.
    ok(
        !api.mediaUrl('../../etc', 1, 0).includes('../'),
        'сегмент пути кодируется: ../ не должен доезжать до сервера сырым',
    );
    for (const u of [api.mediaUrl('chat27', 113, 0), api.avatarUrl(17)]) {
        ok(u.startsWith('/api/bitrix/'), `${u} — собственный путь плагина`);
        ok(!/https?:/i.test(u), `${u} не содержит внешнего адреса`);
    }
}

// ---------------------------------------------------------------------------
// 2. Форматтеры прогресса и имени файла
// ---------------------------------------------------------------------------

{
    eq(F.progressPercent(50, 100), 50, 'половина');
    eq(F.progressPercent(0, 100), 0, 'начало');
    eq(F.progressPercent(200, 100), 100, 'клампится сверху');
    eq(F.progressPercent(5, null), null, 'без общего размера процентов НЕ СУЩЕСТВУЕТ');
    eq(F.progressPercent(5, 0), null, 'нулевой размер — тоже «неизвестно»');
    ok(F.progressLabel(512, 1024).includes('50%'), 'подпись содержит проценты');
    ok(!F.progressLabel(512, null).includes('%'), 'без размера процентов в подписи нет');

    // `download` браузеры трактуют как подсказку ПУТИ — имя приходит из портала.
    eq(F.sanitizeDownloadName('../../.bashrc'), 'bashrc', 'путь вычищается');
    eq(F.sanitizeDownloadName('C:\\dir\\file.jpg'), 'file.jpg', 'windows-путь тоже');
    eq(F.sanitizeDownloadName(''), 'file', 'пустое имя не даёт пустого download');
    eq(F.sanitizeDownloadName('отчёт.txt'), 'отчёт.txt', 'кириллица сохраняется');

    ok(F.isImageKind('image'), 'картинка открывается лайтбоксом');
    ok(!F.isImageKind('file'), 'файл — нет');
    for (const k of ['audio', 'voice', 'video', 'video_note']) {
        ok(F.isPlayableKind(k), `${k} играется плеером`);
    }
    ok(!F.isPlayableKind('file'), 'документ не играется');
    eq(F.playerTag('voice'), 'audio', 'голосовое — audio');
    eq(F.playerTag('video_note'), 'video', 'кружок — video');
}

// ---------------------------------------------------------------------------
// 3. Разметка карточки: ни одного внешнего src, ни одного запроса
// ---------------------------------------------------------------------------

{
    const before = netCalls.length;
    const msg = {
        id: 113,
        dialog_id: 'chat27',
        media: [
            {
                index: 0, file_id: 95, kind: 'file', name: 'forge_probe.txt',
                extension: 'txt', size: 500, has_thumb: false, has_full: true,
                player_url: null, waveform: [],
            },
            {
                index: 1, file_id: 96, kind: 'image', name: 'shot.png',
                size: 2048, has_thumb: true, has_full: true, width: 72, height: 61,
                player_url: null, waveform: [],
            },
            {
                index: 2, file_id: 0, kind: 'file', name: null,
                has_thumb: false, has_full: false, player_url: null, waveform: [],
            },
        ],
    };
    const box = media.renderMediaGroup(msg);
    ok(!!box, 'группа карточек собралась');
    eq(netCalls.length, before, 'ПОСТРОЕНИЕ карточек не делает ни одного запроса');

    const cards = walk(box).filter((n) => String(n.className || '').split(' ').includes('b24-media-card'));
    eq(cards.length, 3, 'по карточке на вложение');
    eq(cards[0].dataset.dialogId, 'chat27', 'диалог уехал в dataset — это адрес прокси');
    eq(cards[0].dataset.msgId, '113', 'сообщение тоже');
    eq(cards[1].dataset.index, '1', 'индекс сохранён: он часть ключа кэша');

    // Внешних адресов в разметке нет ни одного.
    for (const n of walk(box)) {
        if (typeof n.src === 'string' && n.src) {
            ok(false, `узел несёт src=${n.src} — внешних адресов в DOM быть не должно`);
        }
    }
    // Обход узлов, а не JSON.stringify: у фейкового дерева есть обратные
    // ссылки на родителя, и сериализация ушла бы в цикл.
    const strings = walk(box).flatMap((n) => [
        String(n.className || ''), String(n.src || ''), String(n.href || ''),
        String(n.title || ''), typeof n.textContent === 'string' ? n.textContent : '',
    ]);
    ok(
        !strings.some((v) => /bitrix24\.ru|cdn-ru|https?:\/\//.test(v)),
        'в дереве карточек нет внешних URL',
    );

    // ЛЕНИВОСТЬ: картинка поставлена под наблюдение, но байты не запрошены.
    ok(observed.length === 1, `под наблюдением ровно одна карточка (${observed.length})`);
    eq(netCalls.length, before, 'до появления карточки во вьюпорте запросов нет');

    // Превью-узел заводится ТОЛЬКО у картинки с файлом: у портала нет отдельного
    // маленького файла, и пустой <img> у документа был бы обещанием, которое
    // нечем исполнить.
    const thumbs = walk(box).filter((n) => String(n.className || '') === 'b24-media-thumb');
    eq(thumbs.length, 1, 'превью только у картинки');

    // У вложения без файла кнопки выключены — они бы всё равно кончились 404.
    const noFileBtns = walk(cards[2]).filter((n) => String(n.className || '').includes('b24-media-btn'));
    ok(noFileBtns.length > 0 && noFileBtns.every((b) => b.disabled === true),
        'у вложения без файла кнопки выключены');
    const okBtns = walk(cards[0]).filter((n) => String(n.className || '').includes('b24-media-btn'));
    ok(okBtns.every((b) => b.disabled === false), 'у обычного вложения кнопки активны');
}

// ---------------------------------------------------------------------------
// 4. Дедуп: десять одновременных загрузок одного ключа = один запрос
// ---------------------------------------------------------------------------

{
    // Через модуль, а не `Map.clear()`: прямая очистка карты оставила бы
    // objectURL живыми, и следующая секция считала бы утечками мусор теста.
    media.teardownMedia();
    const seen = [];
    setNet(async (url) => {
        seen.push(url);
        // Пауза даёт всем десяти вызовам застать загрузку незавершённой.
        await new Promise((r) => setTimeout(r, 5));
        return bytesRes('AAAA', 'text/plain');
    });

    const key = F.mediaKey('chat27', 113, 0);
    const path = api.mediaUrl('chat27', 113, 0);
    const results = await Promise.all(
        Array.from({ length: 10 }, () => media.loadMedia(key, path)),
    );
    eq(seen.length, 1, 'десять параллельных загрузок схлопнулись в один запрос');
    ok(results.every((u) => u === results[0]), 'все получили один и тот же objectURL');
    eq(state.b24State.mediaCache.get(key), results[0], 'результат лёг в кэш');

    // Повторный вызов после завершения берёт из кэша и в сеть не идёт.
    const again = await media.loadMedia(key, path);
    eq(seen.length, 1, 'попадание в кэш не стоит запроса');
    eq(again, results[0], 'и отдаёт тот же URL');
}

// ---------------------------------------------------------------------------
// 5. Revoke при смене диалога: вложения освобождаются, аватары — нет
// ---------------------------------------------------------------------------

{
    // Через модуль, а не `Map.clear()`: прямая очистка карты оставила бы
    // objectURL живыми, и следующая секция считала бы утечками мусор теста.
    media.teardownMedia();
    urls.revoked.length = 0;
    setNet(async () => bytesRes('BBBB', 'image/png'));

    await media.loadMedia(F.mediaKey('chat27', 113, 0), api.mediaUrl('chat27', 113, 0));
    await media.loadMedia(F.mediaKey('chat27', 113, 1), api.mediaUrl('chat27', 113, 1));
    await media.loadMedia(F.mediaKey('chat27', 115, 0), api.mediaUrl('chat27', 115, 0));
    await media.loadMedia(F.avatarKey(17), api.avatarUrl(17));
    await flush();
    eq(state.b24State.mediaCache.size, 4, 'в кэше три вложения и аватар');

    const freed = media.revokeDialogMedia();
    eq(freed, 3, 'освобождены ровно вложения');
    eq(urls.revoked.length, 3, 'revokeObjectURL позван на каждое');
    eq(state.b24State.mediaCache.size, 1, 'аватар пережил смену диалога');
    ok(state.b24State.mediaCache.has(F.avatarKey(17)),
        'аватары общие для всей панели — качать их заново на каждом клике нельзя');
}

// ---------------------------------------------------------------------------
// 6. dropMessageMedia бьёт ровно по одному сообщению
// ---------------------------------------------------------------------------

{
    // Через модуль, а не `Map.clear()`: прямая очистка карты оставила бы
    // objectURL живыми, и следующая секция считала бы утечками мусор теста.
    media.teardownMedia();
    urls.revoked.length = 0;
    setNet(async () => bytesRes('CCCC', 'image/png'));

    for (const [mid, idx] of [[113, 0], [113, 1], [1130, 0], [4, 0]]) {
        // eslint-disable-next-line no-await-in-loop
        await media.loadMedia(F.mediaKey('chat27', mid, idx), api.mediaUrl('chat27', mid, idx));
    }
    await media.loadMedia(F.avatarKey(17), api.avatarUrl(17));
    await flush();

    const dropped = media.dropMessageMedia('chat27', 113);
    eq(dropped, 2, 'оба вложения сообщения 113');
    ok(state.b24State.mediaCache.has(F.mediaKey('chat27', 1130, 0)),
        '1130 — не префикс 113: чужая запись уцелела');
    ok(state.b24State.mediaCache.has(F.mediaKey('chat27', 4, 0)), 'соседнее сообщение уцелело');
    ok(state.b24State.mediaCache.has(F.avatarKey(17)), 'аватар не при чём');
}

// ---------------------------------------------------------------------------
// 7. teardownMedia не оставляет живых ссылок
// ---------------------------------------------------------------------------

{
    urls.revoked.length = 0;
    media.teardownMedia();
    eq(state.b24State.mediaCache.size, 0, 'кэш пуст');
    eq(urls.live.size, 0, 'ни одного неотозванного objectURL');
    const snap = media._debugMediaState();
    eq(snap.cached, 0, '_debugMediaState видит пустой кэш');
    eq(snap.loading, 0, 'незавершённых загрузок нет');
    eq(snap.cards, 0, 'реестр карточек очищен');
}

// ---------------------------------------------------------------------------
// 8. Кадр прогресса от сервера
// ---------------------------------------------------------------------------

{
    // Кадр о карточке, которой нет на экране (другой диалог, страница выше),
    // обязан молча игнорироваться, а не ронять обработчик WS.
    media.onDownloadProgress({ key: 'chat999/1/0', received: 10, total: 100 });
    media.onDownloadProgress(null);
    media.onDownloadProgress({});
    ok(true, 'неизвестный ключ прогресса не роняет обработчик');

    // А кадр о живой карточке двигает её полосу.
    const box = media.renderMediaGroup({
        id: 113,
        dialog_id: 'chat27',
        media: [{ index: 0, file_id: 95, kind: 'file', name: 'a.txt', size: 100, has_full: true, waveform: [] }],
    });
    media.onDownloadProgress({ key: 'chat27/113/0', received: 50, total: 100 });
    const status = walk(box).find((n) => String(n.className || '').includes('b24-media-status'));
    ok(status && String(status.textContent).includes('50%'),
        `полоса показывает проценты портала: ${status && status.textContent}`);
    ok(status && String(status.textContent).startsWith('B24'),
        'подпись отличает ногу «портал → сервер» от «сервер → браузер»');
}

// ---------------------------------------------------------------------------

media.teardownMedia();
console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_media)`);
if (failures > 0) process.exit(1);
