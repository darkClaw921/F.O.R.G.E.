// Тесты детекта речи. Прогон: node --test plugins/meet/tests/js/
//
// VAD — то место, где ошибка не видна глазами и не ловится компилятором:
// сдвинутая на 120 мс граница речи выглядит как «атрибуция иногда врёт», и
// искать её потом приходится по часовым записям. Поэтому логика вынесена в
// чистый модуль и проверяется арифметикой.

import test from "node:test";
import assert from "node:assert/strict";

import {
  VAD_DEFAULTS,
  createVadDetector,
  intervalsFromEvents
} from "../../src/bot/inject/vad.js";

const FRAME_MS = 20;

/** Скармливает детектору ровный уровень в течение `ms`, собирая переходы. */
function feed(vad, levelDb, ms, startMs) {
  const events = [];
  let t = startMs;
  for (let left = ms; left > 0; left -= FRAME_MS) {
    t += FRAME_MS;
    const e = vad.push(levelDb, t, FRAME_MS);
    if (e) { events.push(e); }
  }
  return { events, endMs: t };
}

test("тишина не рождает речи", () => {
  const vad = createVadDetector();
  const { events } = feed(vad, -70, 2000, 0);
  assert.deepEqual(events, []);
  assert.equal(vad.isSpeaking(), false);
});

test("речь объявляется только после подтверждения", () => {
  const vad = createVadDetector({ confirmMs: 120 });
  // Дадим полу устояться на тишине.
  feed(vad, -70, 500, 0);
  const floor = vad.floor();

  // Короткий щелчок в 60 мс — короче подтверждения.
  const short = feed(vad, floor + 30, 60, 500);
  assert.deepEqual(short.events, [], "щелчок не имеет права стать репликой");
  assert.equal(vad.isSpeaking(), false);
});

test("МОМЕНТ НАЧАЛА СДВИНУТ НАЗАД ровно на длину подтверждения", () => {
  const confirmMs = 120;
  const vad = createVadDetector({ confirmMs });
  feed(vad, -70, 500, 0);
  const floor = vad.floor();

  // Речь начинается в 500 мс и держится.
  const { events } = feed(vad, floor + 30, 400, 500);
  assert.equal(events.length, 1, "ожидался ровно один переход");
  assert.equal(events[0].on, true);

  // Подтверждение набралось на 500+120 = 620 мс, и ровно эти 120 мс
  // вычитаются: без сдвига ВСЕ интервалы речи съезжали бы вправо.
  assert.equal(events[0].at, 500, `переход обязан указывать на 500, а не на ${events[0].at}`);
});

test("речь кончается только после hangover, и конец тоже сдвинут назад", () => {
  const hangoverMs = 500;
  const vad = createVadDetector({ hangoverMs });
  feed(vad, -70, 500, 0);
  const floor = vad.floor();
  feed(vad, floor + 30, 1000, 500);
  assert.equal(vad.isSpeaking(), true);

  // Пауза между словами в 300 мс короче hangover — реплика не рвётся.
  const gap = feed(vad, floor, 300, 1500);
  assert.deepEqual(gap.events, [], "пауза между словами разорвала реплику");
  assert.equal(vad.isSpeaking(), true);

  // Продолжаем говорить, затем замолкаем надолго.
  feed(vad, floor + 30, 200, 1800);
  const { events } = feed(vad, floor, 800, 2000);
  assert.equal(events.length, 1);
  assert.equal(events[0].on, false);
  assert.equal(events[0].at, 2000, `конец обязан указывать на 2000, а не на ${events[0].at}`);
});

test("гистерезис: выключение требует упасть НИЖЕ порога включения", () => {
  const vad = createVadDetector({ onDb: 12, offDb: 6 });
  feed(vad, -70, 500, 0);
  const floor = vad.floor();
  feed(vad, floor + 20, 400, 500);
  assert.equal(vad.isSpeaking(), true);

  // Уровень между порогами выключения и включения: речь продолжается.
  const between = feed(vad, floor + 9, 1000, 900);
  assert.deepEqual(between.events, [], "уровень между порогами не имеет права гасить речь");
  assert.equal(vad.isSpeaking(), true);
});

test("шумовой пол адаптируется — и НЕ ползёт во время речи", () => {
  const vad = createVadDetector();
  // Постоянный шум -40 дБ: пол обязан к нему подняться.
  feed(vad, -40, 3000, 0);
  const floor = vad.floor();
  assert.ok(Math.abs(floor - -40) < 1, `пол не нашёл шум: ${floor}`);

  // Громкая речь поверх шума.
  feed(vad, -10, 2000, 3000);
  assert.equal(vad.isSpeaking(), true);
  assert.equal(
    vad.floor(),
    floor,
    `пол уполз за речью: ${floor} → ${vad.floor()} — за сотню реплик встречи это десятки децибел`
  );

  // И после речи адаптация возобновляется: иначе пол застыл бы навсегда.
  // -35 дБ выше пола, но ниже порога включения (пол + 12 дБ), то есть это
  // подросший шум, а не речь — пол обязан за ним пойти.
  feed(vad, -35, 2000, 5000);
  assert.ok(vad.floor() > floor, `пол не оттаял после речи: ${vad.floor()}`);
  assert.ok(vad.floor() <= -35 + 1e-9, `пол перепрыгнул уровень шума: ${vad.floor()}`);
});

test("порог считается ОТ ПОЛА, а не от абсолютной величины", () => {
  // Две дорожки с разным уровнем шума обязаны дать одинаковый результат.
  for (const noiseDb of [-70, -50, -30]) {
    const vad = createVadDetector();
    feed(vad, noiseDb, 3000, 0);
    const quiet = feed(vad, vad.floor() + 3, 1000, 3000);
    assert.deepEqual(quiet.events, [], `шум ${noiseDb}: +3 дБ над полом — это не речь`);
    const loud = feed(vad, vad.floor() + 20, 1000, 4000);
    assert.equal(loud.events.length, 1, `шум ${noiseDb}: +20 дБ над полом — это речь`);
  }
});

test("незакрытый интервал закрывается концом записи", () => {
  const vad = createVadDetector();
  feed(vad, -70, 500, 0);
  feed(vad, vad.floor() + 30, 1000, 500);
  assert.equal(vad.isSpeaking(), true);

  const last = vad.finish(1500);
  assert.deepEqual(last, { on: false, at: 1500 });
  assert.equal(vad.isSpeaking(), false);
  assert.equal(vad.finish(1600), null, "повторное закрытие не выдумывает интервал");
});

test("intervalsFromEvents: дубли on, незакрытый хвост, нулевые интервалы", () => {
  assert.deepEqual(
    intervalsFromEvents([{ on: true, at: 100 }, { on: false, at: 500 }], 1000),
    [{ start_ms: 100, end_ms: 500 }]
  );
  // Дубль `on` — последствие переподключения дорожки, первый выигрывает.
  assert.deepEqual(
    intervalsFromEvents(
      [{ on: true, at: 100 }, { on: true, at: 200 }, { on: false, at: 500 }],
      1000
    ),
    [{ start_ms: 100, end_ms: 500 }]
  );
  // Незакрытый хвост закрывается концом записи.
  assert.deepEqual(
    intervalsFromEvents([{ on: true, at: 800 }], 1000),
    [{ start_ms: 800, end_ms: 1000 }]
  );
  // Нулевой интервал не попадает в результат.
  assert.deepEqual(
    intervalsFromEvents([{ on: true, at: 100 }, { on: false, at: 100 }], 1000),
    []
  );
  // Закрытие без открытия игнорируется.
  assert.deepEqual(intervalsFromEvents([{ on: false, at: 100 }], 1000), []);
});

test("значения по умолчанию совпадают с планом", () => {
  assert.equal(VAD_DEFAULTS.onDb, 12);
  assert.equal(VAD_DEFAULTS.offDb, 6);
  assert.equal(VAD_DEFAULTS.confirmMs, 120);
  assert.equal(VAD_DEFAULTS.hangoverMs, 500);
  assert.ok(VAD_DEFAULTS.offDb < VAD_DEFAULTS.onDb, "без гистерезиса граница дребезжит");
});

test("полный проход по расписанию мок-встречи попадает в допуск", async () => {
  const { readFile } = await import("node:fs/promises");
  const expected = JSON.parse(
    await readFile(new URL("../fixtures/mock-meeting/expected.json", import.meta.url), "utf8")
  );
  const mode = expected.modes["per-track"];
  const tolStart = expected.tolerance_ms.speech_start;
  const tolEnd = expected.tolerance_ms.speech_end;

  for (const track of mode.tracks) {
    const vad = createVadDetector();
    const events = [];
    // Прогоняем всю встречу кадрами по 20 мс: речь по расписанию, тишина вне.
    for (let t = 0; t < expected.duration_ms; t += FRAME_MS) {
      const mid = t + FRAME_MS / 2;
      const speaking = track.speech.some((s) => mid >= s.start_ms && mid < s.end_ms);
      const e = vad.push(speaking ? -12 : -60, t + FRAME_MS, FRAME_MS);
      if (e) { events.push(e); }
    }
    const last = vad.finish(expected.duration_ms);
    if (last) { events.push(last); }

    const got = intervalsFromEvents(events, expected.duration_ms);
    assert.equal(
      got.length,
      track.speech.length,
      `дорожка ${track.index}: интервалов ${got.length}, ожидалось ${track.speech.length}`
    );
    got.forEach((iv, i) => {
      const want = track.speech[i];
      assert.ok(
        Math.abs(iv.start_ms - want.start_ms) <= tolStart,
        `дорожка ${track.index}, интервал ${i}: начало ${iv.start_ms} против эталонных ${want.start_ms}`
      );
      assert.ok(
        Math.abs(iv.end_ms - want.end_ms) <= tolEnd,
        `дорожка ${track.index}, интервал ${i}: конец ${iv.end_ms} против эталонных ${want.end_ms}`
      );
    });
  }
});
