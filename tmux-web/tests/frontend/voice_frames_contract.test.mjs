// СТРАЖ ЕДИНОГО ФОРМАТА КАДРОВ РАСШИФРОВКИ (голосовые-Ф3).
//
// Текст под голосовым сообщением рисует ОДИН модуль на три мессенджера
// (голосовые-Ф5). Он подписывается на кадры `voice_transcribe_*` и читает у них
// поля по именам — а объявлены эти кадры ТРИЖДЫ, в трёх независимых enum'ах:
//
//   • plugins/vk/src/ws/protocol.rs        — VkServerMsg
//   • plugins/telegram/src/ws/protocol.rs  — TgServerMsg
//   • plugins/bitrix/src/ws/protocol.rs    — B24ServerMsg
//
// Между ними нет ни общего типа, ни компилятора, который бы их свёл: три
// плагина изолированы друг от друга по построению. Значит, расхождение — вопрос
// времени, и стоить оно будет ровно столько, сколько стоит молча неработающая
// кнопка в одной из трёх панелей: кадр приходит, поле называется иначе,
// подписчик читает `undefined` и не рисует ничего.
//
// Здесь сверяется ровно это: у всех трёх есть все четыре кадра, и НАБОР ИМЁН
// ПОЛЕЙ у одноимённых кадров совпадает. Типы полей намеренно НЕ сверяются —
// `message_id` у VK и Telegram это `i32`, у Битрикса `i64`, и в JSON оба числа.
//
// Заодно проверяется, что все четыре кадра объявлены во фронтовых
// `SERVER_FRAMES` каждой панели: кадр без записи там уходит в `console.debug` и
// теряется.
//
// Запуск: node tmux-web/tests/frontend/voice_frames_contract.test.mjs
// Код возврата 0 — расхождений нет, 1 — есть [FAIL].

import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');

let failures = 0;
let checks = 0;

function eq(name, actual, expected, hint) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) {
        console.log(`[ok]   ${name}`);
        return;
    }
    failures += 1;
    console.log(
        `[FAIL] ${name}\n       actual:   ${a}\n       expected: ${e}${hint ? `\n       ${hint}` : ''}`,
    );
}

function ok(name, cond, detail) {
    checks += 1;
    if (cond) {
        console.log(`[ok]   ${name}`);
        return;
    }
    failures += 1;
    console.log(`[FAIL] ${name}${detail ? `\n       ${detail}` : ''}`);
}

/** Четыре кадра расшифровки — в порядке жизненного цикла прогона. */
const FRAMES = [
    'VoiceTranscribeStarted',
    'VoiceTranscribeDelta',
    'VoiceTranscribeDone',
    'VoiceTranscribeError',
];

/** `VoiceTranscribeStarted` → `voice_transcribe_started`. */
const snake = (name) => name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();

/**
 * Имена полей варианта enum'а по исходнику Rust.
 *
 * Разбор ручной и намеренно тупой: вариант объявлен на четырёх пробелах, поля
 * внутри — на восьми, комментарии начинаются с `///`. Ничего умнее здесь не
 * нужно, а лишняя сообразительность разборщика однажды «починила» бы
 * расхождение вместо того, чтобы о нём сказать.
 */
function frameFields(src, variant) {
    const at = src.indexOf(`    ${variant} {`);
    if (at < 0) return null;
    const end = src.indexOf('\n    },', at);
    if (end < 0) return null;
    return src
        .slice(at, end)
        .split('\n')
        .map((l) => /^ {8}([a-z_]+):/.exec(l))
        .filter(Boolean)
        .map((m) => m[1]);
}

const plugins = [
    { name: 'vk', enum: 'VkServerMsg', rs: '../../../plugins/vk/src/ws/protocol.rs', js: '../../static/js/vk/ws.js' },
    { name: 'telegram', enum: 'TgServerMsg', rs: '../../../plugins/telegram/src/ws/protocol.rs', js: '../../static/js/telegram/ws.js' },
    { name: 'bitrix', enum: 'B24ServerMsg', rs: '../../../plugins/bitrix/src/ws/protocol.rs', js: '../../static/js/bitrix/ws.js' },
];

// ---------------------------------------------------------------------------
// 1. Все четыре кадра объявлены во всех трёх плагинах, с одинаковыми полями.
// ---------------------------------------------------------------------------

const reference = {};
for (const frame of FRAMES) {
    for (const plugin of plugins) {
        const src = read(plugin.rs);
        const fields = frameFields(src, frame);
        ok(
            `${plugin.name}: кадр ${snake(frame)} объявлен`,
            Array.isArray(fields) && fields.length > 0,
            'кадра нет — значит, в этой панели расшифровка молчит',
        );
        if (!fields) continue;

        // Координаты вложения обязаны быть у КАЖДОГО кадра: сервер вещает их
        // всем подписчикам без фильтрации, и подписчик обязан сам понять, его
        // ли это карточка.
        for (const must of ['run_id', 'dialog_id', 'message_id', 'index']) {
            ok(
                `${plugin.name}/${snake(frame)}: поле ${must} на месте`,
                fields.includes(must),
                `есть: ${fields.join(', ')}`,
            );
        }

        if (!reference[frame]) {
            reference[frame] = { plugin: plugin.name, fields };
            continue;
        }
        eq(
            `${snake(frame)}: набор полей совпадает с ${reference[frame].plugin}`,
            fields,
            reference[frame].fields,
            'общий фронт-модуль читает поля по именам — расхождение даёт undefined и пустую карточку',
        );
    }
}

// Смысловые поля финальных кадров — поимённо: именно их читает карточка.
ok(
    'done несёт текст, язык, движок и длительность',
    ['text', 'language', 'engine', 'duration_ms'].every((f) =>
        reference.VoiceTranscribeDone.fields.includes(f)),
    `есть: ${reference.VoiceTranscribeDone.fields.join(', ')}`,
);
ok(
    'error несёт КОД отдельно от текста',
    ['code', 'message'].every((f) => reference.VoiceTranscribeError.fields.includes(f)),
    'по коду фронт различает «нет ключа» и «нет GPU»; разбирать это из русской фразы он не должен',
);
ok(
    'started называет движок ДО отправки',
    reference.VoiceTranscribeStarted.fields.includes('engine'),
    'человек имеет право знать, ушло ли голосовое в чужое облако, до того как оно туда ушло',
);
ok(
    'delta несёт проценты',
    reference.VoiceTranscribeDelta.fields.includes('percent'),
);

// ---------------------------------------------------------------------------
// 2. Фронт каждой панели знает все четыре кадра.
// ---------------------------------------------------------------------------

for (const plugin of plugins) {
    const js = read(plugin.js);
    const marker = 'export const SERVER_FRAMES = [';
    const start = js.indexOf(marker);
    ok(`${plugin.name}: SERVER_FRAMES найдена`, start >= 0);
    if (start < 0) continue;
    const block = js.slice(start + marker.length, js.indexOf('];', start));
    const tags = [...block.matchAll(/'([a-z_]+)'/g)].map((m) => m[1]);
    eq(
        `${plugin.name}: все четыре кадра расшифровки перечислены`,
        FRAMES.map(snake).filter((t) => !tags.includes(t)),
        [],
        'кадр без записи в SERVER_FRAMES уходит в console.debug и теряется',
    );
}

// ---------------------------------------------------------------------------
// 3. Коалесцинг прогресса объявлен во всех трёх — одинаковым шагом.
// ---------------------------------------------------------------------------
//
// Без него whisper заливает broadcast сотнями кадров в секунду, а переполнение
// канала — это `Lagged`, то есть `resync` и перечитывание всего списка бесед
// ПОСРЕДИ расшифровки.

const steps = plugins.map((p) => {
    const m = /pub const VOICE_PROGRESS_STEP: u8 = (\d+);/.exec(read(p.rs));
    return m ? Number(m[1]) : null;
});
ok('шаг коалесцинга объявлен во всех трёх', steps.every((s) => s !== null), `${steps}`);
eq('шаг коалесцинга одинаков', [...new Set(steps)].length, 1, `${steps}`);
ok('шаг коалесцинга не нулевой', steps[0] > 0, 'ноль означает кадр на каждую строку stderr');

console.log(
    failures === 0
        ? `\n[OK] voice_frames_contract: ${checks} проверок`
        : `\n[FAIL] voice_frames_contract: ${failures} из ${checks}`,
);
process.exit(failures === 0 ? 0 : 1);
