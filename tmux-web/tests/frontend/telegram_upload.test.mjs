// Регресс-тест чистой логики ОТПРАВКИ файлов (Фаза 5).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/format.js через
// `await import()`: он не импортирует ни DOM, ни core/*, поэтому работает в
// Node как есть. Сам upload.js в Node не грузится (там document/XHR), но вся
// логика, которую можно ошибиться, вынесена в format.js и проверяется здесь:
// выбор MIME и вида вложения, имя для скриншота из буфера, подписи этапов,
// предварительная валидация размера и разбор контракта ошибок.
//
// Запуск: node tmux-web/tests/frontend/telegram_upload.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const modUrl = new URL('../../static/js/telegram/format.js', import.meta.url).href;
const F = await import(modUrl);

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

// ---------- uploadMime ----------
eq(F.uploadMime('a.jpg', 'image/jpeg'), 'image/jpeg', 'mime: браузер сказал — верим ему');
eq(F.uploadMime('a.JPG', ''), 'image/jpeg', 'mime: пустой type → по расширению');
eq(F.uploadMime('report.pdf'), 'application/pdf', 'mime: pdf по расширению');
eq(F.uploadMime('clip.MOV', undefined), 'video/quicktime', 'mime: регистр расширения не важен');
eq(F.uploadMime('noext', ''), 'application/octet-stream', 'mime: без расширения — octet-stream');
eq(F.uploadMime('a.unknownxyz', ''), 'application/octet-stream', 'mime: незнакомое расширение');
eq(F.uploadMime('a.bin', ' IMAGE/PNG '), 'image/png', 'mime: тип нормализуется');

// ---------- uploadKind ----------
eq(F.uploadKind('image/png'), 'photo', 'kind: картинка');
eq(F.uploadKind('video/mp4'), 'video', 'kind: видео');
eq(F.uploadKind('audio/ogg'), 'audio', 'kind: аудио');
eq(F.uploadKind('application/pdf'), 'document', 'kind: документ');
eq(F.uploadKind(''), 'document', 'kind: неизвестный MIME — документ');

// ---------- pastedFileName ----------
const pastedAt = new Date(2026, 6, 30, 9, 5, 7).getTime(); // 2026-07-30 09:05:07 локально
eq(F.pastedFileName('image/png', pastedAt), 'pasted_20260730_090507.png',
    'paste: имя из даты и времени');
eq(F.pastedFileName('image/jpeg', pastedAt), 'pasted_20260730_090507.jpeg', 'paste: расширение из MIME');
eq(F.pastedFileName('image/svg+xml', pastedAt), 'pasted_20260730_090507.svg', 'paste: svg+xml → svg');
eq(F.pastedFileName('', pastedAt), 'pasted_20260730_090507.png', 'paste: без MIME — png');
ok(!F.pastedFileName('image/png').includes('/'), 'paste: в имени нет разделителей пути');

// ---------- подписи этапов ----------
eq(F.uploadStageLabel('queued'), 'QUEUED', 'этап: в очереди');
eq(F.uploadStageLabel('local', 47, 100), 'LOCAL 47%', 'этап: первая нога — браузер → devforge');
eq(F.uploadStageLabel('tg', 12, 100), 'TG 12%', 'этап: вторая нога — devforge → Telegram');
eq(F.uploadStageLabel('sent'), '✓ SENT', 'этап: доставлено');
eq(F.uploadStageLabel('failed'), '!! FAILED', 'этап: провал');
// 100% появляется только когда байты реально дошли: до этого потолок 99.
eq(F.uploadStageLabel('local', 999, 1000), 'LOCAL 99%', 'этап: 99% до финала');
eq(F.uploadStageLabel('local', 1000, 1000), 'LOCAL 100%', 'этап: 100% на финале');

eq(F.uploadQueueLabel(1, 1), 'QUEUED', 'очередь: один файл — без счётчика');
eq(F.uploadQueueLabel(2, 3), 'QUEUED 2/3', 'очередь: позиция из скольких');
eq(F.uploadQueueLabel(5, 2), 'QUEUED 5/5', 'очередь: total не меньше позиции');

// ---------- предварительная валидация ----------
eq(F.uploadRejectReason({ size: 1024 }), null, 'валидация: обычный файл проходит');
eq(F.uploadRejectReason({ size: 0 }), 'файл пустой', 'валидация: пустой файл отсекается до сети');
ok(String(F.uploadRejectReason({ size: 300 }, 100)).includes('больше лимита'),
    'валидация: превышение лимита');
eq(F.uploadRejectReason({ size: 100 }, 100), null, 'валидация: ровно лимит — можно');
ok(F.uploadRejectReason({}) !== null, 'валидация: размер неизвестен — отказ');
ok(F.MAX_UPLOAD_BYTES === 2 * 1024 * 1024 * 1024, 'валидация: клиентский лимит — 2 ГиБ');

// ---------- контракт ошибок ----------
const flood = F.parseApiError(429, '{"error":{"code":"flood_wait","message":"wait","retry_after_secs":42}}');
eq(flood.code, 'flood_wait', 'ошибка: код читается');
eq(flood.retryAfterSecs, 42, 'ошибка: retry_after_secs читается');
eq(flood.message, 'wait', 'ошибка: сообщение читается');
const tooLarge = F.parseApiError(413, '{"error":{"code":"too_large","message":"too big"}}');
eq(tooLarge.code, 'too_large', 'ошибка: 413 приходит с кодом too_large');
eq(tooLarge.retryAfterSecs, null, 'ошибка: без retry_after_secs — null');
const garbage = F.parseApiError(502, '<html>bad gateway</html>');
eq(garbage.code, null, 'ошибка: не-JSON тело не ломает разбор');
eq(garbage.message, 'HTTP 502', 'ошибка: fallback на статус');
eq(F.parseApiError(0, '').message, 'HTTP 0', 'ошибка: пустое тело');

// ---------- краевые случаи: обманчивые имена файлов ----------
eq(F.uploadMime('.gitignore', ''), 'application/octet-stream', 'mime: dotfile — «расширение» не считается');
eq(F.uploadMime('a.tar.gz', ''), 'application/octet-stream', 'mime: двойное расширение смотрит на последний сегмент');
eq(F.uploadMime('file.', ''), 'application/octet-stream', 'mime: хвостовая точка — расширения нет');
eq(F.uploadMime('', ''), 'application/octet-stream', 'mime: пустое имя не роняет выбор типа');

// ---------- краевые случаи: полночь и злонамеренный MIME в pastedFileName ----------
eq(F.pastedFileName('image/png', new Date(2027, 0, 1, 0, 0, 0).getTime()),
    'pasted_20270101_000000.png', 'paste: полночь Нового года — все нули на месте');
eq(F.pastedFileName('image/../../etc', new Date(2027, 0, 1, 0, 0, 0).getTime()),
    'pasted_20270101_000000.png', 'paste: path traversal в MIME сводится к безопасному png');
eq(F.pastedFileName('image/x-fo!o', new Date(2027, 0, 1, 0, 0, 0).getTime()),
    'pasted_20270101_000000.xfoo', 'paste: спецсимволы в подтипе MIME вычищаются');

// ---------- краевые случаи: подписи этапов и очереди ----------
eq(F.uploadStageLabel('bogus'), '', 'этап: незнакомое состояние — пустая подпись, а не мусор');
eq(F.uploadStageLabel('local', 2048, null), 'LOCAL 2.0 KB…', 'этап: без Content-Length показываем объём');
eq(F.uploadQueueLabel(0, 0), 'QUEUED', 'очередь: нулевые значения не рисуют «0/0»');
eq(F.uploadQueueLabel(-1, 5), 'QUEUED 1/5', 'очередь: отрицательная позиция прижимается к 1');

// ---------- краевые случаи: точная граница лимита ----------
eq(F.uploadRejectReason({ size: F.MAX_UPLOAD_BYTES }), null, 'валидация: ровно 2 ГиБ проходит');
ok(F.uploadRejectReason({ size: F.MAX_UPLOAD_BYTES + 1 }) !== null, 'валидация: 2 ГиБ + 1 байт — отказ');
eq(F.uploadRejectReason({ size: -5 }), 'файл пустой', 'валидация: отрицательный размер = пустой');
eq(F.uploadRejectReason({ size: 1 }, 0), null, 'валидация: нулевой лимит откатывается к MAX_UPLOAD_BYTES');

// ---------- краевые случаи: обрывы и битые контракты ошибок ----------
const cut = F.parseApiError(429, '{"error":{"code":"flood_w');
eq(cut.code, null, 'ошибка: JSON, обрезанный на полпути, не роняет разбор');
eq(cut.message, 'HTTP 429', 'ошибка: после обрыва остаётся статус');
eq(F.parseApiError(400, '{"error":"peer_id_invalid"}').code, null,
    'ошибка: error-строка вместо объекта не читается как код');
eq(F.parseApiError(429, '{"error":{"code":"flood_wait","message":"w","retry_after_secs":"42"}}').retryAfterSecs,
    null, 'ошибка: строковый retry_after_secs отвергается — таймер не примет строку');
eq(F.parseApiError(429, '{"error":{"code":"flood_wait","message":"w","retry_after_secs":0}}').retryAfterSecs,
    0, 'ошибка: retry_after_secs=0 — валидный ноль, а не null');
eq(F.parseApiError(500, '{"error":{"code":"x","message":""}}').message, 'HTTP 500',
    'ошибка: пустое сообщение подменяется статусом');
eq(F.parseApiError(null, 'null').status, 0, 'ошибка: null-статус нормализуется в 0');

console.log(`\n${checks - failures}/${checks} ассертов прошло (${path.basename(here)}/telegram_upload)`);
if (failures > 0) {
    console.error(`${failures} ассертов упало`);
    process.exit(1);
}
