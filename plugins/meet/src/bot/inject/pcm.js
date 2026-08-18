/*
 * Чистые функции звука: пересчёт частоты, PCM16, base64.
 *
 * Модуль ES: его импортирует `node --test` (tests/js/*.test.mjs) и он же
 * встраивается в worklet. Строка `export` помечена маркером и вырезается
 * сборщиком — в `AudioWorkletGlobalScope` модулей нет, и `export` там был бы
 * синтаксической ошибкой.
 *
 * ПОЧЕМУ ЭТИ ФУНКЦИИ ЖИВУТ ОТДЕЛЬНО
 *
 * Пересчёт 48 000 → 16 000 и упаковка в PCM16 — самое ошибкоопасное место
 * всего захвата: ошибка на единицу в шаге даёт медленно уезжающий таймлайн,
 * а неверное клиппирование — треск, который слышно только на живой записи.
 * Проверять это надо арифметикой в `node --test`, а не ушами через час
 * встречи.
 */

/** Ноль в PCM16. */
export const PCM16_SILENCE = 0; // FORGE_STRIP_FOR_WORKLET

/**
 * Пересчитывает моно-поток `Float32` в целевую частоту усреднением.
 *
 * Усреднение по окну, а не выбор каждого N-го сэмпла: децимация без
 * фильтрации заворачивает всё выше половины целевой частоты обратно в
 * слышимый диапазон (алиасинг), и речь превращается в металлический звон.
 * Усреднение — простейший фильтр нижних частот, и для 48→16 (целое
 * соотношение 3) он честно снимает главную часть проблемы.
 *
 * Дробное соотношение (44 100 → 16 000) обрабатывается тем же кодом:
 * границы окна считаются в дробных позициях, а `carry` переносит остаток в
 * следующий вызов, поэтому СТЫКИ КАДРОВ не дают ни щелчков, ни сдвига.
 */
export function createResampler(fromRate, toRate) { // FORGE_STRIP_FOR_WORKLET
  const ratio = fromRate / toRate;
  // Дробная позиция начала следующего выходного сэмпла во входном потоке.
  let pos = 0;
  // Хвост входа, не уместившийся в целое окно на прошлом вызове.
  let tail = new Float32Array(0);

  return function resample(input) {
    const src = tail.length ? concat(tail, input) : input;
    const out = [];
    while (pos + ratio <= src.length) {
      const from = pos;
      const to = pos + ratio;
      const i0 = Math.floor(from);
      const i1 = Math.min(src.length, Math.ceil(to));
      let sum = 0;
      let n = 0;
      for (let i = i0; i < i1; i++) {
        sum += src[i];
        n++;
      }
      out.push(n ? sum / n : 0);
      pos = to;
    }
    // Всё, что левее текущей позиции, больше не нужно; остальное переносим.
    const keep = Math.floor(pos);
    tail = src.slice(keep);
    pos -= keep;
    return Float32Array.from(out);
  };
}

function concat(a, b) {
  const out = new Float32Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

/**
 * Float32 (-1..1) → PCM16 little-endian.
 *
 * Клиппирование ОБЯЗАТЕЛЬНО: значения вне диапазона встречаются регулярно
 * (сумма нескольких источников в одном узле), а `Math.round(1.4 * 32767)`
 * без ограничения переполняет `Int16` и превращает громкий участок в
 * противоположный по знаку — то есть в треск.
 */
export function floatToPcm16(input) { // FORGE_STRIP_FOR_WORKLET
  const out = new Int16Array(input.length);
  for (let i = 0; i < input.length; i++) {
    let v = input[i];
    if (v > 1) { v = 1; } else if (v < -1) { v = -1; }
    // Асимметрия диапазона Int16 (-32768..32767) учтена намеренно: масштаб
    // 32767 для обоих знаков не даёт -32768 появиться из -1.0.
    out[i] = Math.round(v * 32767);
  }
  return out;
}

/** Среднеквадратичный уровень блока, 0..1. */
export function rms(input) { // FORGE_STRIP_FOR_WORKLET
  if (!input.length) { return 0; }
  let sum = 0;
  for (let i = 0; i < input.length; i++) {
    sum += input[i] * input[i];
  }
  return Math.sqrt(sum / input.length);
}

/**
 * Уровень в децибелах. Тишина даёт `floor` (-100 дБ), а не `-Infinity`:
 * бесконечность отравила бы адаптивный шумовой пол навсегда.
 */
export function toDb(level, floorDb) { // FORGE_STRIP_FOR_WORKLET
  const min = typeof floorDb === "number" ? floorDb : -100;
  if (!(level > 0)) { return min; }
  const db = 20 * Math.log10(level);
  return db < min ? min : db;
}

/**
 * Int16Array → base64.
 *
 * Своими руками, без `btoa(String.fromCharCode.apply(...))`: раскладывать
 * тысячи аргументов в `apply` — верный способ поймать «Maximum call stack
 * size exceeded» на длинном кадре, причём не сразу, а на первой встрече,
 * которая окажется длиннее тестовой.
 */
const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; // FORGE_STRIP_FOR_WORKLET

export function pcm16ToBase64(samples) { // FORGE_STRIP_FOR_WORKLET
  const bytes = new Uint8Array(samples.length * 2);
  for (let i = 0; i < samples.length; i++) {
    const v = samples[i] < 0 ? samples[i] + 0x10000 : samples[i];
    bytes[i * 2] = v & 0xff;
    bytes[i * 2 + 1] = (v >> 8) & 0xff;
  }
  let out = "";
  let i = 0;
  for (; i + 2 < bytes.length; i += 3) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
    out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + B64[(n >> 6) & 63] + B64[n & 63];
  }
  const rest = bytes.length - i;
  if (rest === 1) {
    const n = bytes[i] << 16;
    out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + "==";
  } else if (rest === 2) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8);
    out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + B64[(n >> 6) & 63] + "=";
  }
  return out;
}
