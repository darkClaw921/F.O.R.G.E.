// Тесты пересчёта частоты, PCM16 и base64.
// Прогон: node --test plugins/meet/tests/js/
//
// Это самое ошибкоопасное место захвата. Ошибка на единицу в шаге даёт
// медленно уезжающий таймлайн (полсекунды за час — и атрибуция спикеров
// разъехалась), а забытое клиппирование превращает громкий участок в треск.
// Ни то, ни другое не видно, пока не послушаешь часовую запись.

import test from "node:test";
import assert from "node:assert/strict";

import {
  createResampler,
  floatToPcm16,
  pcm16ToBase64,
  rms,
  toDb
} from "../../src/bot/inject/pcm.js";

test("48 кГц → 16 кГц: длина выхода втрое меньше и не накапливает ошибку", () => {
  const resample = createResampler(48000, 16000);
  let total = 0;
  // Сто вызовов по 128 сэмплов — ровно так worklet и получает вход.
  for (let i = 0; i < 100; i++) {
    total += resample(new Float32Array(128)).length;
  }
  // 12 800 входных сэмплов при соотношении 3 дают 4266 выходных
  // (последний неполный кадр переносится в хвост).
  assert.ok(Math.abs(total - 12800 / 3) <= 1, `сэмплов ${total}, ожидалось около ${12800 / 3}`);
});

test("СТЫКИ КАДРОВ не теряют и не дублируют сэмплы", () => {
  // Пилообразный сигнал: по значению видно, какой именно сэмпл попал в выход.
  const make = (from, n) => Float32Array.from({ length: n }, (_, i) => (from + i) / 1000);

  const whole = createResampler(48000, 16000)(make(0, 3000));
  const piecewise = (() => {
    const r = createResampler(48000, 16000);
    const a = r(make(0, 1000));
    const b = r(make(1000, 1000));
    const c = r(make(2000, 1000));
    return Float32Array.from([...a, ...b, ...c]);
  })();

  assert.equal(piecewise.length, whole.length, "разбиение на куски изменило длину");
  for (let i = 0; i < whole.length; i++) {
    assert.ok(
      Math.abs(whole[i] - piecewise[i]) < 1e-6,
      `сэмпл ${i}: цельный проход ${whole[i]}, по кускам ${piecewise[i]}`
    );
  }
});

test("дробное соотношение 44 100 → 16 000 не уезжает на длинном проходе", () => {
  const resample = createResampler(44100, 16000);
  let total = 0;
  const chunks = 1000;
  for (let i = 0; i < chunks; i++) {
    total += resample(new Float32Array(128)).length;
  }
  const input = chunks * 128;
  const expected = input * (16000 / 44100);
  // Допуск в один сэмпл на весь проход: именно накопление ошибки и даёт
  // медленно уезжающий таймлайн.
  assert.ok(
    Math.abs(total - expected) <= 1,
    `на ${input} входных сэмплах вышло ${total}, ожидалось ${expected.toFixed(1)}`
  );
});

test("усреднение, а не децимация: постоянный сигнал остаётся постоянным", () => {
  const resample = createResampler(48000, 16000);
  const out = resample(new Float32Array(1200).fill(0.5));
  assert.ok(out.length > 0);
  for (const v of out) {
    assert.ok(Math.abs(v - 0.5) < 1e-6, `постоянный вход дал ${v}`);
  }
});

test("floatToPcm16 клиппирует и не переполняет Int16", () => {
  const out = floatToPcm16(Float32Array.from([0, 1, -1, 1.5, -1.5, 0.5]));
  assert.deepEqual(Array.from(out), [0, 32767, -32767, 32767, -32767, 16384]);
  // Переполнение здесь означало бы смену знака, то есть треск.
  for (const v of out) {
    assert.ok(v >= -32768 && v <= 32767, `значение вне Int16: ${v}`);
  }
});

test("base64: круговой ход и корректная набивка", () => {
  const decode = (s) => Buffer.from(s, "base64");

  for (const samples of [
    [],
    [0],
    [1, -1],
    [32767, -32768, 0, 12345],
    Array.from({ length: 320 }, (_, i) => (i % 2 ? 1 : -1) * (i * 97) % 32767
    )
  ]) {
    const arr = Int16Array.from(samples);
    const b64 = pcm16ToBase64(arr);
    const bytes = decode(b64);
    assert.equal(bytes.length, arr.length * 2, `длина байтов для ${arr.length} сэмплов`);
    const back = new Int16Array(bytes.buffer, bytes.byteOffset, arr.length);
    assert.deepEqual(Array.from(back), Array.from(arr), `круговой ход сломан на ${b64}`);
  }
});

test("base64 совпадает с эталонной реализацией node", () => {
  const arr = Int16Array.from({ length: 1000 }, (_, i) => ((i * 7919) % 65536) - 32768);
  const ours = pcm16ToBase64(arr);
  const theirs = Buffer.from(arr.buffer, arr.byteOffset, arr.byteLength).toString("base64");
  assert.equal(ours, theirs);
});

test("длинный кадр не роняет стек", () => {
  // Именно это ломает `btoa(String.fromCharCode.apply(null, …))`, и ломает не
  // сразу, а на первой встрече длиннее тестовой.
  const arr = new Int16Array(200000);
  const b64 = pcm16ToBase64(arr);
  assert.equal(b64.length, Math.ceil((arr.length * 2) / 3) * 4);
});

test("rms и toDb: тишина не даёт минус бесконечность", () => {
  assert.equal(rms(new Float32Array(0)), 0);
  assert.equal(rms(new Float32Array(10)), 0);
  assert.ok(Math.abs(rms(Float32Array.from([1, -1, 1, -1])) - 1) < 1e-6);

  // -Infinity отравил бы адаптивный шумовой пол навсегда.
  assert.equal(toDb(0), -100);
  assert.equal(toDb(-1), -100);
  assert.ok(Math.abs(toDb(1)) < 1e-6);
  assert.ok(Math.abs(toDb(0.5) - -6.0206) < 0.001);
  assert.equal(toDb(0.0000001, -60), -60, "пол задаётся параметром");
});
