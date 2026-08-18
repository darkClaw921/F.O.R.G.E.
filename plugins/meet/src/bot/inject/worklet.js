/*
 * AudioWorkletProcessor захвата: пересчёт в 16 кГц, PCM16, VAD.
 *
 * Сборщик кладёт СЮДА, перед этим файлом, содержимое `pcm.js` и `vad.js` со
 * снятым словом `export`: в `AudioWorkletGlobalScope` нет ни модулей, ни
 * `import`, ни `URL`, ни `fetch` — только `sampleRate`, `currentTime`,
 * `currentFrame` и `registerProcessor`.
 *
 * ПОЧЕМУ ВЕСЬ РАСЧЁТ ЗДЕСЬ, А НЕ В ГЛАВНОМ ПОТОКЕ
 *
 * Processor видит КАЖДЫЙ сэмпл — он и так обязан их пройти, чтобы отдать
 * PCM16. Считать здесь же и уровень для VAD стоит одного умножения на сэмпл,
 * зато детект получает сэмпл-точный тайминг и не зависит от троттлинга
 * таймеров occluded-окна. Главный поток остаётся почтальоном: base64 и
 * отправка.
 *
 * ВРЕМЯ СЧИТАЕТСЯ СЭМПЛАМИ
 *
 * `currentTime` в worklet есть, но он привязан к старту КОНТЕКСТА, а ось
 * записи начинается с первого захваченного трека. Поэтому смещение кадра
 * ведётся счётчиком отданных сэмплов, а `startOffsetMs` (когда дорожка
 * появилась на общей оси) приходит из главного потока при создании узла.
 * Дальше арифметика целочисленная — и абсолютная: кадр несёт СВОЁ смещение,
 * а не «следующий после предыдущего».
 */

class ForgeCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const opts = (options && options.processorOptions) || {};
    this.trackId = opts.trackId || "";
    this.idx = opts.idx || 0;
    this.targetRate = opts.targetRate || 16000;
    this.frameMs = opts.frameMs || 20;
    // Сколько сэмплов ЦЕЛЕВОЙ частоты в одном кадре по проводу.
    this.frameSamples = Math.max(1, Math.round((this.targetRate * this.frameMs) / 1000));
    // Абсолютное смещение дорожки на общей оси, в сэмплах целевой частоты.
    // Renegotiation даёт дорожку, начинающуюся позже нуля, и это же значение
    // потом становится `adelay` в ffmpeg.
    this.baseSamples = Math.round((this.targetRate * (opts.startOffsetMs || 0)) / 1000);
    this.emitted = 0;

    this.resample = createResampler(sampleRate, this.targetRate);
    this.vad = createVadDetector(opts.vad || {});
    this.pending = [];
    this.closed = false;

    this.port.onmessage = (e) => {
      if (e.data && e.data.k === "close") {
        this.flush(true);
        const last = this.vad.finish(this.axisMs());
        if (last) { this.port.postMessage({ k: "vad", on: false, at: last.at }); }
        this.closed = true;
      }
    };
  }

  /** Текущий момент на общей оси, мс: смещение дорожки плюс отданное. */
  axisMs() {
    return Math.round(((this.baseSamples + this.emitted) * 1000) / this.targetRate);
  }

  /** Отдаёт накопленное кадрами фиксированной длины. */
  flush(force) {
    while (this.pending.length >= this.frameSamples || (force && this.pending.length > 0)) {
      const take = Math.min(this.frameSamples, this.pending.length);
      const chunk = Float32Array.from(this.pending.splice(0, take));

      // Уровень считается ДО квантования: на тихой речи разница между
      // float и int16 уже заметна, а порог VAD задан в децибелах.
      const levelDb = toDb(rms(chunk));
      const dtMs = (take * 1000) / this.targetRate;
      // Момент КОНЦА блока: сдвиг назад внутри детекта отсчитывается от него.
      const endMs = Math.round(((this.baseSamples + this.emitted + take) * 1000) / this.targetRate);
      const transition = this.vad.push(levelDb, endMs, dtMs);
      if (transition) {
        this.port.postMessage({ k: "vad", on: transition.on, at: transition.at });
      }

      const pcm = floatToPcm16(chunk);
      // АБСОЛЮТНОЕ сэмпловое смещение — единственный адрес кадра. Потеряйся
      // он по дороге, на его месте останется тишина, а таймлайн не сдвинется.
      this.port.postMessage(
        {
          k: "a",
          o: this.baseSamples + this.emitted,
          n: take,
          db: Math.round(levelDb),
          b: pcm.buffer
        },
        [pcm.buffer]
      );
      this.emitted += take;
    }
  }

  process(inputs) {
    if (this.closed) {
      return false;
    }
    const input = inputs[0];
    if (!input || !input.length) {
      // Дорожка ещё не подключена либо уже отвалилась: держим процессор
      // живым, но ничего не выдумываем. Тишину за неё дорисует писатель WAV
      // по сэмпловому смещению.
      return true;
    }

    // Моно: берём первый канал. Микс каналов здесь не нужен — удалённая
    // дорожка встречи моно почти всегда, а лишнее суммирование удвоило бы
    // работу на горячем пути.
    const mono = input[0];
    if (mono && mono.length) {
      const down = this.resample(mono);
      for (let i = 0; i < down.length; i++) {
        this.pending.push(down[i]);
      }
      this.flush(false);
    }
    return true;
  }
}

registerProcessor(FORGE_WORKLET_NAME, ForgeCaptureProcessor);
