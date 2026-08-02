// Регресс-тест чистой логики медиа-карточек вкладки Telegram (Фаза 4).
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/telegram/format.js через
// `await import()`: он специально не импортирует ни DOM, ни core/*, поэтому
// работает в Node как есть. Из js/telegram/media.js проверяются ЧИСТЫЕ
// решающие функции плеера (resolveCardClick, isPlayerClick) — сам модуль
// импортируется со стабом `document` (см. ниже); всё остальное про
// DOM/fetch/objectURL по-прежнему проверяется рантайм-прогоном в браузере.
//
// Запуск: node tmux-web/tests/frontend/telegram_media.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/telegram/format.js', import.meta.url).href;
const F = await import(modUrl);

// media.js тянет core/dom.js, который на верхнем уровне зовёт
// document.getElementById для каждого ref'а. Минимальный стаб (все ref'ы —
// null) позволяет импортировать модуль в Node; ни один тест ниже не трогает
// реальный DOM — проверяются только чистые функции.
globalThis.document = {
    getElementById: () => null,
    addEventListener: () => {},
};
const mediaUrl = new URL('../../static/js/telegram/media.js', import.meta.url).href;
const M = await import(mediaUrl);

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

// ---------- formatBytes ----------
eq(F.formatBytes(0), '0 B', 'bytes: ноль');
eq(F.formatBytes(512), '512 B', 'bytes: меньше килобайта');
eq(F.formatBytes(999), '999 B', 'bytes: граница единиц');
eq(F.formatBytes(1000), '1.0 KB', 'bytes: ровно килобайт');
eq(F.formatBytes(840000), '840 KB', 'bytes: без дробной части свыше 10');
eq(F.formatBytes(1234567), '1.2 MB', 'bytes: мегабайты с одной цифрой');
eq(F.formatBytes(12345678), '12 MB', 'bytes: двузначные без дробей');
eq(F.formatBytes(null), '', 'bytes: неизвестный размер — пустая строка');
eq(F.formatBytes(undefined), '', 'bytes: undefined — пустая строка');
eq(F.formatBytes(-5), '', 'bytes: отрицательный размер не рисуем');

// ---------- durationLabel ----------
eq(F.durationLabel(0), '0:00', 'duration: ноль');
eq(F.durationLabel(7), '0:07', 'duration: секунды с ведущим нулём');
eq(F.durationLabel(65), '1:05', 'duration: минуты');
eq(F.durationLabel(3725), '1:02:05', 'duration: часы');
eq(F.durationLabel(null), '', 'duration: нет длительности');

// ---------- mediaKey ----------
eq(F.mediaKey('-1001234567890', 42, 'thumb'), '-1001234567890/42/thumb', 'key: thumb');
eq(F.mediaKey('777000', 7, 'full'), '777000/7/full', 'key: full');
eq(F.mediaKey('777000', 7, 'bogus'), '777000/7/full', 'key: неизвестный kind — это full');

// ---------- mediaFileName ----------
eq(
    F.mediaFileName({ id: 42, media: { kind: 'document', file_name: 'report.pdf' } }),
    'report.pdf',
    'name: имя из Telegram как есть'
);
eq(
    F.mediaFileName({ id: 42, media: { kind: 'photo', mime: 'image/jpeg' } }),
    'photo_42.jpg',
    'name: у фото имени нет — генерируем'
);
eq(
    F.mediaFileName({ id: 7, media: { kind: 'voice', mime: 'audio/ogg' } }),
    'voice_7.ogg',
    'name: голосовое'
);
eq(
    F.mediaFileName({ id: 9, media: { kind: 'other' } }),
    'other_9.bin',
    'name: неизвестный вид без MIME'
);
eq(
    F.mediaFileName({ id: 3, media: { kind: 'document', file_name: '   ' } }),
    'document_3.bin',
    'name: пробельное имя не считается именем'
);

// ---------- sanitizeDownloadName ----------
eq(F.sanitizeDownloadName('report.pdf'), 'report.pdf', 'save: обычное имя не портим');
eq(F.sanitizeDownloadName('../../.bashrc'), 'bashrc', 'save: путь и ведущие точки срезаны');
eq(F.sanitizeDownloadName('C:\\dir\\report 1.pdf'), 'report 1.pdf', 'save: windows-путь');
eq(F.sanitizeDownloadName('a/b/c/../evil.sh'), 'evil.sh', 'save: только последний сегмент');
eq(F.sanitizeDownloadName(''), 'download.bin', 'save: пустое имя — fallback');
eq(F.sanitizeDownloadName('...'), 'download.bin', 'save: одни точки — fallback');
eq(F.sanitizeDownloadName('a<b>c:d"e|f?g*h'), 'a_b_c_d_e_f_g_h', 'save: спецсимволы ФС');
ok(
    !F.sanitizeDownloadName('bad\u0000\u001fname.txt').includes('\u0000'),
    'save: управляющие символы вырезаны'
);

// ---------- подписи ----------
eq(F.mediaKindLabel('photo'), 'PHOTO', 'label: фото');
eq(F.mediaKindLabel('document'), 'FILE', 'label: документ показываем как FILE');
eq(F.mediaKindLabel('zzz'), 'DATA', 'label: неизвестный вид');
eq(F.mediaGlyph('video'), '▶', 'glyph: видео');
eq(F.mediaGlyph('zzz'), '◌', 'glyph: неизвестный вид');
ok(!/[\u{1F300}-\u{1FAFF}]/u.test(Object.keys({}).length + F.mediaGlyph('photo')),
    'glyph: без цветных emoji (терминальная стилистика)');

// ---------- mediaMetaLine ----------
eq(
    F.mediaMetaLine({ width: 1280, height: 720, size: 1234567 }),
    '1280×720 · 1.2 MB',
    'meta: разрешение и размер'
);
eq(
    F.mediaMetaLine({ duration_secs: 7, size: 24000 }),
    '0:07 · 24 KB',
    'meta: длительность и размер'
);
eq(F.mediaMetaLine({}), '', 'meta: нечего показать');
eq(
    F.mediaMetaLine({ width: 0, height: 0, size: 100 }),
    '100 B',
    'meta: нулевое разрешение не рисуем'
);

// ---------- прогресс ----------
eq(F.progressPercent(0, 100), 0, 'progress: старт');
eq(F.progressPercent(49, 100), 49, 'progress: середина');
eq(F.progressPercent(99.6, 100), 99, 'progress: округление вниз');
eq(F.progressPercent(100, 100), 100, 'progress: готово');
eq(F.progressPercent(150, 100), 100, 'progress: не больше 100');
eq(F.progressPercent(10, null), null, 'progress: неизвестный размер');
eq(F.progressPercent(10, 0), null, 'progress: нулевой размер — неизвестный');
ok(F.progressPercent(999999, 1000000) < 100, 'progress: 100% только при завершении');
eq(F.progressLabel(49, 100), '49%', 'label: проценты');
eq(F.progressLabel(2048, null), '2.0 KB…', 'label: без размера — принятый объём');
eq(F.progressLabel(0, null), '0 B…', 'label: самое начало без размера');

// ---------- предикаты ----------
ok(F.canShowThumb({ kind: 'photo', has_thumb: true }), 'thumb: у фото есть');
ok(!F.canShowThumb({ kind: 'photo', has_thumb: false }), 'thumb: has_thumb=false — не грузим');
ok(!F.canShowThumb({ kind: 'voice', has_thumb: true }), 'thumb: голосовому превью не нужно');
ok(!F.canShowThumb(null), 'thumb: без медиа');
ok(F.canShowThumb({ kind: 'document', has_thumb: true }), 'thumb: документ с превью');
ok(F.isPlayableKind('video') && F.isPlayableKind('voice') && F.isPlayableKind('audio'),
    'playable: video/voice/audio');
ok(!F.isPlayableKind('photo') && !F.isPlayableKind('document'), 'playable: не всё подряд');
ok(F.isImageKind('photo') && F.isImageKind('sticker'), 'image: фото и стикер');
ok(!F.isImageKind('video'), 'image: видео не в лайтбокс');

// ---------- краевые случаи: гигантские файлы и граница i32 ----------
eq(F.formatBytes(2147483648), '2.1 GB', 'bytes: 2 ГиБ (за границей i32) не переполняется');
eq(F.formatBytes(1e12), '1.0 TB', 'bytes: терабайт');
ok(F.formatBytes(Number.MAX_SAFE_INTEGER).endsWith(' TB'),
    'bytes: за пределами шкалы остаёмся в TB, а не «undefined»');
eq(F.formatBytes('123'), '123 B', 'bytes: строковый размер из JSON приводится к числу');
eq(F.formatBytes(NaN), '', 'bytes: NaN — пустая строка');

// ---------- краевые случаи: границы durationLabel ----------
eq(F.durationLabel(3599), '59:59', 'duration: 59:59 ещё без часов');
eq(F.durationLabel(3600), '1:00:00', 'duration: ровно час переключает формат');
eq(F.durationLabel(59.9), '0:59', 'duration: дробные секунды режутся вниз, а не 0:60');
eq(F.durationLabel(-1), '', 'duration: отрицательная длительность не рисуется');
eq(F.durationLabel(Infinity), '', 'duration: Infinity — пустая строка, а не «Infinity:NaN»');

// ---------- краевые случаи: fallback'и имени файла ----------
eq(F.mediaFileName(null), 'file_0.bin', 'name: null-сообщение не роняет карточку');
eq(F.mediaFileName({ id: 9, media: { kind: 'other', mime: 'image/png' } }), 'other_9.png',
    'name: известный MIME важнее вида при генерации расширения');
eq(F.mediaFileName({ id: 4, media: { kind: 'document', file_name: 42 } }), 'document_4.bin',
    'name: нестроковый file_name из битого JSON игнорируется');

// ---------- краевые случаи: sanitizeDownloadName и юникод ----------
eq(F.sanitizeDownloadName('отчёт за июль.pdf'), 'отчёт за июль.pdf', 'save: кириллица не портится');
eq(F.sanitizeDownloadName('photo \u{1F389}.jpg'), 'photo \u{1F389}.jpg', 'save: emoji в имени легальны');
eq(F.sanitizeDownloadName(null), 'download.bin', 'save: null вместо имени — fallback');
eq(F.sanitizeDownloadName('   '), 'download.bin', 'save: имя из одних пробелов — fallback');
eq(F.sanitizeDownloadName('..\\..\\evil.exe'), 'evil.exe', 'save: windows-обход пути срезается');
ok(!/[\u0000-\u001f\u007f]/.test(F.sanitizeDownloadName('a\u0001b\u001fc\u007fd.txt')),
    'save: весь диапазон C0 и DEL вычищены');

// ---------- краевые случаи: нулевые и отрицательные метаданные ----------
eq(F.mediaMetaLine({ duration_secs: 0 }), '0:00', 'meta: голосовое нулевой длины показывает 0:00');
eq(F.mediaMetaLine({ width: -1, height: 100, size: 10 }), '10 B', 'meta: отрицательная ширина не рисуется');

// ---------- краевые случаи: мусор в событиях прогресса ----------
eq(F.progressPercent(-1, 100), null, 'progress: отрицательный received → null');
eq(F.progressPercent(NaN, 100), null, 'progress: NaN received → null');

// ---------- плеер: resolveCardClick (пересоздавать ли плеер) ----------
eq(M.resolveCardClick('7/10', null, false), 'start',
    'player: активного плеера нет — старт нового');
eq(M.resolveCardClick('7/10', '7/10', false), 'toggle',
    'player: повторный клик по той же карточке — play/pause, НЕ пересоздание');
eq(M.resolveCardClick('7/11', '7/10', false), 'start',
    'player: клик по другой карточке — пересоздание (предыдущий гасится)');
eq(M.resolveCardClick('8/10', '7/10', false), 'start',
    'player: тот же message_id в другом диалоге — другая карточка');
eq(M.resolveCardClick('7/10', '7/10', true), 'ignore',
    'player: клик из controls (пауза/перемотка) карточка игнорирует');
eq(M.resolveCardClick('7/11', '7/10', true), 'ignore',
    'player: клик из controls игнорируется независимо от карточки');
eq(M.resolveCardClick('7/10', undefined, false), 'start',
    'player: undefined вместо activeKey — как отсутствие плеера');

// ---------- плеер: isPlayerClick (клик пришёл из плеера?) ----------
const insidePlayer = { closest: (sel) => (sel === '.tg-media-player' ? {} : null) };
const outsidePlayer = { closest: () => null };
ok(M.isPlayerClick(insidePlayer), 'player: target внутри .tg-media-player распознан');
ok(!M.isPlayerClick(outsidePlayer), 'player: target вне плеера — обычный клик по карточке');
ok(!M.isPlayerClick(null), 'player: null target не роняет обработчик');
ok(!M.isPlayerClick({}), 'player: target без closest (текстовый узел) — не плеер');

// ---------- итог ----------
if (failures) {
    console.error(`\n${failures} из ${checks} проверок провалилось`);
    process.exit(1);
}
console.log(`telegram_media: ${checks}/${checks} проверок прошли`);
