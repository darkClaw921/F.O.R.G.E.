// tmux-web/tests/frontend/pwa_push_helper.test.js
//
// PWA push helper — регресс-тесты urlBase64ToUint8Array (base64url VAPID key →
// Uint8Array для applicationServerKey pushManager.subscribe).
//
// ДВА УРОВНЯ покрытия:
//   (a) РЕАЛЬНЫЙ код: динамический import('../../static/js/pwa/push.js') —
//       push.js это ESM с `export function urlBase64ToUint8Array`. install.js
//       (его единственный импорт) не имеет top-level side-effects, atob есть в
//       Node v26 глобально → import проходит без мокинга self/window/document.
//       Это защита от дрейфа: правка push.js, ломающая контракт, валит тест.
//   (b) РЕПЛИКА один-в-один (контракт идентичен push.js::urlBase64ToUint8Array,
//       менять синхронно). Прогоняем те же кейсы на реплике, плюс сверяем, что
//       реплика и реальный код дают идентичный выход — двойная страховка.
//
// Запуск: node tmux-web/tests/frontend/pwa_push_helper.test.js
// Exit 0 — все ассерты прошли, exit 1 — хотя бы один упал.

'use strict';

const path = require('node:path');
const url = require('node:url');

// Глушим бенайн-warning MODULE_TYPELESS_PACKAGE_JSON при import ESM из CJS.
process.removeAllListeners('warning');
process.on('warning', () => {});

// =============================================================================
// Tiny assertion runner (стиль sidebar_grouping.test.js)
// =============================================================================

let passed = 0;
let failed = 0;
const failures = [];

function assert(label, cond, details) {
    if (cond) {
        passed += 1;
        console.log('  ok   ' + label);
    } else {
        failed += 1;
        failures.push({ label, details: details || '' });
        console.log('  FAIL ' + label + (details ? '  — ' + details : ''));
    }
}

function eq(label, actual, expected) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    assert(label, a === e, 'expected ' + e + ', got ' + a);
}

function group(name, fn) {
    console.log('\n[' + name + ']');
    return Promise.resolve().then(fn);
}

// =============================================================================
// РЕПЛИКА urlBase64ToUint8Array — контракт идентичен push.js (строки 365-376).
// ВАЖНО: при изменении push.js::urlBase64ToUint8Array обновить и эту функцию.
// =============================================================================

function urlBase64ToUint8ArrayReplica(base64String) {
    const padding = '='.repeat((4 - (base64String.length % 4)) % 4);
    const base64 = (base64String + padding)
        .replace(/-/g, '+')
        .replace(/_/g, '/');
    const rawData = atob(base64);
    const outputArray = new Uint8Array(rawData.length);
    for (let i = 0; i < rawData.length; ++i) {
        outputArray[i] = rawData.charCodeAt(i);
    }
    return outputArray;
}

// =============================================================================
// Хелперы для base64url
// =============================================================================

/** Кодирует Buffer в base64url-no-pad (как реальный VAPID public key). */
function toBase64Url(buf) {
    return Buffer.from(buf).toString('base64')
        .replace(/\+/g, '-')
        .replace(/\//g, '_')
        .replace(/=+$/, '');
}

/** Эталонный 65-байтный uncompressed EC point: 0x04 + 32 X + 32 Y. */
function makeVapidBytes() {
    const bytes = Buffer.alloc(65);
    bytes[0] = 0x04;
    for (let i = 1; i < 65; i++) {
        // Детерминированный паттерн с гарантией присутствия '-' и '_' в base64url:
        // нужны байтовые группы, дающие символы 62 ('+'→'-') и 63 ('/'→'_').
        bytes[i] = (i * 37 + 11) & 0xff;
    }
    return bytes;
}

function buffersEqual(uint8, buf) {
    return Buffer.compare(Buffer.from(uint8), Buffer.from(buf)) === 0;
}

// =============================================================================
// Tests
// =============================================================================

async function run() {
    // Загружаем РЕАЛЬНУЮ функцию из push.js.
    const pushPath = path.join(__dirname, '..', '..', 'static', 'js', 'pwa', 'push.js');
    let real;
    try {
        const mod = await import(url.pathToFileURL(pushPath).href);
        real = mod.urlBase64ToUint8Array;
    } catch (e) {
        console.error('FATAL: не удалось импортировать push.js:', e.message);
        process.exit(1);
    }

    assert('реальная urlBase64ToUint8Array импортирована', typeof real === 'function');

    const vapidBytes = makeVapidBytes();
    const vapidB64 = toBase64Url(vapidBytes);

    await group('реальный 65-байтный VAPID-ключ декодируется точно', () => {
        eq('длина base64url строки = 87 (mod4==3)', vapidB64.length, 87);
        eq('длина строки mod4 == 3', vapidB64.length % 4, 3);
        const out = real(vapidB64);
        assert('выход — Uint8Array', out instanceof Uint8Array);
        eq('длина ровно 65', out.length, 65);
        eq('out[0] === 0x04 (uncompressed point)', out[0], 0x04);
        assert('побайтно равен эталону (Buffer.compare===0)', buffersEqual(out, vapidBytes));
    });

    await group('padding: четыре остатка по модулю 4', () => {
        // mod4==0 → padding '' (двойной %4!): 'AAAA' = 4 байта нулей в кодировке.
        const m0 = real('AAAA');
        eq('mod4==0: вход "AAAA" → 3 байта', m0.length, 3);
        // Проверяем что padding для mod4==0 пуст (косвенно: "AAAA" валиден без '=').
        eq('mod4==0 даёт корректные байты [0,0,0]', Array.from(m0), [0, 0, 0]);

        // mod4==2 → два '='. 'QQ' = base64('A') = байт 0x41.
        const m2 = real('QQ');
        eq('mod4==2: "QQ" → 1 байт', m2.length, 1);
        eq('mod4==2: байт == 0x41', m2[0], 0x41);

        // mod4==3 → один '='. 'QUI' = base64('AB') = [0x41,0x42].
        const m3 = real('QUI');
        eq('mod4==3: "QUI" → 2 байта', m3.length, 2);
        eq('mod4==3: байты [0x41,0x42]', Array.from(m3), [0x41, 0x42]);

        // Реальный 87-символьный ключ (mod4==3) → 65 байт.
        eq('реальный ключ mod4==3 → 65 байт', real(vapidB64).length, 65);
    });

    await group('замена алфавита base64url → base64 (-_ → +/)', () => {
        // Подбираем байты, дающие символы '+' (62) и '/' (63) в обычном base64,
        // т.е. '-' и '_' в base64url. Байты [0xfb,0xff] → base64 "+/8=".
        const raw = Buffer.from([0xfb, 0xff, 0x00]); // base64: "+/8A", url: "-_8A"
        const b64url = toBase64Url(raw);
        assert('строка содержит "-"', b64url.includes('-'));
        assert('строка содержит "_"', b64url.includes('_'));
        const out = real(b64url);
        assert('декодировано в исходные байты (замена сработала)', buffersEqual(out, raw));
        // Без замены atob('-_8A') дал бы другие байты — сверим, что замена нужна.
        eq('out[0]==0xfb', out[0], 0xfb);
        eq('out[1]==0xff', out[1], 0xff);
    });

    await group('пустая строка на входе', () => {
        const out = real('');
        assert('Uint8Array', out instanceof Uint8Array);
        eq('длина 0', out.length, 0);
        // padding == '' т.к. (4 - 0%4) % 4 == 0
    });

    await group('инвариант длины выхода = число байт после atob', () => {
        const cases = ['QQ' /*1*/, 'QUI' /*2*/, 'QUJD' /*3*/, vapidB64 /*65*/];
        const expectedLens = [1, 2, 3, 65];
        cases.forEach((c, i) => {
            const out = real(c);
            eq('длина для "' + (c.length > 10 ? c.slice(0, 6) + '…' : c) + '" = ' + expectedLens[i],
                out.length, expectedLens[i]);
        });
    });

    await group('невалидный base64 → atob бросает (контракт try/catch в enablePush)', () => {
        // '!!!!' остаётся '!!!!' после замены (нет -/_), atob его отвергает.
        let threw = false;
        try { real('!!!!'); } catch (_) { threw = true; }
        assert('real("!!!!") бросает', threw);

        // Ключ с пробелом тоже невалиден.
        let threw2 = false;
        try { real('QQ QQ'); } catch (_) { threw2 = true; }
        assert('real со spaces бросает', threw2);
    });

    await group('чистота: повтор даёт идентичный массив, вход не мутируется', () => {
        const input = vapidB64;
        const a = real(input);
        const b = real(input);
        assert('два независимых Uint8Array', a !== b);
        assert('содержимое идентично', buffersEqual(a, b));
        eq('входная строка не изменена', input, vapidB64);
    });

    await group('реплика идентична реальному коду (cross-check)', () => {
        const cases = ['', 'QQ', 'QUI', 'QUJD', 'AAAA', vapidB64, toBase64Url(Buffer.from([0xfb, 0xff, 0x00]))];
        for (const c of cases) {
            const r = real(c);
            const rep = urlBase64ToUint8ArrayReplica(c);
            assert('реал==реплика для "' + (c.length > 10 ? c.slice(0, 6) + '…' : (c || '<empty>')) + '"',
                buffersEqual(r, rep));
        }
        // И обе бросают на одинаковом невалидном входе.
        let realThrew = false, repThrew = false;
        try { real('!!!!'); } catch (_) { realThrew = true; }
        try { urlBase64ToUint8ArrayReplica('!!!!'); } catch (_) { repThrew = true; }
        assert('обе бросают на "!!!!"', realThrew && repThrew);
    });

    // =========================================================================
    // Summary
    // =========================================================================
    console.log('\n=================================');
    console.log('  passed: ' + passed);
    console.log('  failed: ' + failed);
    console.log('=================================');

    if (failed > 0) {
        console.log('\nFailures:');
        for (const f of failures) {
            console.log('  - ' + f.label + (f.details ? ': ' + f.details : ''));
        }
        process.exit(1);
    }
    process.exit(0);
}

run().catch((err) => {
    console.error('Unexpected test harness error:', err);
    process.exit(1);
});
