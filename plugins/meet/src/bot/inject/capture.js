/*
 * ТРИ НЕЗАВИСИМЫХ ТАПА ЗАХВАТА ЗВУКА.
 *
 * Скрипт выполняется РАНЬШЕ кода страницы (Page.addScriptToEvaluateOnNewDocument)
 * и внутри той же функции, где живёт `post` — глобалов не заводит.
 *
 * ПОЧЕМУ ТАПОВ ТРИ, А НЕ ОДИН
 *
 * Одного патча `ontrack` не хватает, и это не перестраховка:
 *
 *   • Zoom ВООБЩЕ НЕ ОТДАЁТ МЕДИА ЧЕРЕЗ WebRTC. Он гоняет звук по
 *     ненадёжным DataChannel, декодирует его в WASM и играет через Web Audio.
 *     Для него единственный путь — тап 2 (AudioNode.prototype.connect).
 *   • Часть клиентов вешает поток прямо на <audio>/<video>, минуя наши глаза,
 *     — для них тап 3 (HTMLMediaElement.prototype.srcObject).
 *   • Треки появляются и БЕЗ события `track` (SFU добавляет их в готовый
 *     transceiver) — поэтому у тапа 1 есть ещё и обход getReceivers().
 *
 * Приоритет источника: 1 → 3 → 2. Фактически сработавший путь уезжает в
 * манифест полем `capture_path`: без него, разбирая молчащую запись, нельзя
 * отличить «тап не сработал» от «во встрече все молчали».
 *
 * ПРИОРИТЕТ, А НЕ СЛОЖЕНИЕ — ЭТО НЕ ФОРМАЛЬНОСТЬ
 *
 * Тапы смотрят на ОДИН И ТОТ ЖЕ звук с трёх сторон, поэтому работать
 * аддитивно они не имеют права: тап 2 зеркалит вывод графа в СВОЙ
 * `MediaStreamAudioDestinationNode`, то есть порождает НОВЫЙ
 * `MediaStreamTrack` с новым идентификатором. Дедуп по `id` такое зеркало
 * поймать не может в принципе — оно и есть другой трек, просто с тем же
 * звуком внутри. Отсюда правило: менее приоритетный тап не заводит дорожку,
 * пока работает более приоритетный, а если приоритетный тап включается
 * позже (Zoom сначала играет через Web Audio, потом открывает WebRTC), то
 * дорожки младшего пути СНИМАЮТСЯ событием `track_ended` с причиной
 * `superseded`.
 *
 * ЧТО ЗАХВАТЫВАТЬ НЕЛЬЗЯ
 *
 * Своё же исходящее. У передающей стороны на каждый `addTrack` заводится
 * трансивер, а у него — приёмник с ЖИВЫМ, но вечно молчащим треком. Обход
 * `getReceivers()` без проверки направления берёт эти пустышки и удваивает
 * запись: столько же дорожек, сколько настоящих, только тишина. Ровно это и
 * случилось на мок-встрече (задача .15): 6 дорожек вместо 3, 4 вместо 2,
 * 2 вместо 1 — то есть на каждый локальный `addTrack` по фантому. Поэтому
 * обход идёт по ТРАНСИВЕРАМ и берёт только `recvonly`/`sendrecv`, а
 * идентификаторы своих отправляемых дорожек и дорожек `getUserMedia`
 * запоминаются и не захватываются никогда.
 *
 * ЧЕГО ЗДЕСЬ ДЕЛАТЬ НЕЛЬЗЯ
 *
 * Ломать страницу. Мы гость на чужом сайте: `instanceof RTCPeerConnection` в
 * коде площадки обязан продолжать работать, обработчик `ontrack` площадки —
 * не перетираться, исходное ребро аудиографа — оставаться на месте. Каждая
 * из этих трёх вещей проверяется отдельным тестом на мок-странице.
 */

(function () {
  "use strict";

  var CFG = FORGE_MEET_CFG;

  // Пути захвата в порядке приоритета. Строки замороженные: они уезжают в
  // `manifest.json` и в карточку встречи.
  var PATH_RTC = "rtcpeerconnection";
  var PATH_ELEMENT = "media_element";
  var PATH_WEBAUDIO = "webaudio";
  var PATH_RANK = {};
  PATH_RANK[PATH_RTC] = 3;
  PATH_RANK[PATH_ELEMENT] = 2;
  PATH_RANK[PATH_WEBAUDIO] = 1;

  // --- общее состояние ------------------------------------------------------

  var state = {
    // T0 — НАЧАЛО ЕДИНСТВЕННОЙ ОСИ ВРЕМЕНИ. Ставится при первом захваченном
    // треке и больше не меняется. Все смещения считаются от него через
    // performance.now(): он монотонен, не прыгает при переводе часов и при
    // синхронизации NTP.
    t0: null,
    tracks: {},        // id трека → состояние
    nextIdx: 0,        // ИНДЕКС НЕ ПЕРЕИСПОЛЬЗУЕТСЯ после ended
    capturePath: null,
    ctx: null,
    workletReady: null,
    pcs: [],
    localIds: {},      // id СВОИХ дорожек: микрофон бота и всё, что мы шлём
    suppressed: {},    // о каком треке уже сообщили «тап подавлен»
    mirrored: null,    // WeakSet узлов, уже зазеркаленных тапом 2
    sinks: []          // скрытые <audio>, держащие потоки живыми
  };

  function nowMs() {
    return performance.now();
  }

  /** Миллисекунды от T0. До первого трека оси нет — тогда ноль. */
  function axisMs() {
    return state.t0 === null ? 0 : Math.max(0, Math.round(nowMs() - state.t0));
  }

  function emit(name, extra) {
    var msg = { k: "e", n: name, ms: axisMs() };
    if (extra) {
      for (var key in extra) {
        if (Object.prototype.hasOwnProperty.call(extra, key)) {
          msg[key] = extra[key];
        }
      }
    }
    post(msg);
  }

  function fail(where, e) {
    // Ошибка одного тапа НЕ ИМЕЕТ ПРАВА уронить остальные: у нас их три
    // именно затем, чтобы отказ одного не оставлял встречу без звука.
    try {
      emit("tap_error", { where: String(where), message: String(e && e.message ? e.message : e) });
    } catch (ignored) {}
  }

  /**
   * Объявляет путь захвата. Побеждает САМЫЙ ПРИОРИТЕТНЫЙ из сработавших:
   * страница может отдать один и тот же звук и через WebRTC, и через
   * <audio>, и запись в манифест «webaudio» ввела бы в заблуждение того, кто
   * будет разбирать проблему через сутки.
   *
   * Смена пути на более приоритетный СНИМАЕТ дорожки младшего пути: они и
   * есть тот же звук, взятый худшим способом.
   */
  function notePath(path) {
    if (state.capturePath === path) {
      return;
    }
    if (state.capturePath && PATH_RANK[state.capturePath] >= PATH_RANK[path]) {
      return;
    }
    var previous = state.capturePath;
    state.capturePath = path;
    emit("capture_path", { p: path });
    if (previous) {
      retireTracksBelow(path);
    }
  }

  /**
   * Снимает дорожки, заведённые менее приоритетным тапом.
   *
   * Случай из жизни — Zoom: звук сначала идёт только через Web Audio, а
   * через несколько секунд площадка открывает WebRTC. Оставь мы обе дорожки,
   * в записи было бы по два экземпляра каждой реплики, и свести их нельзя:
   * это РАЗНЫЕ дорожки с одинаковым содержимым.
   */
  function retireTracksBelow(path) {
    var rank = PATH_RANK[path];
    for (var id in state.tracks) {
      if (!Object.prototype.hasOwnProperty.call(state.tracks, id)) { continue; }
      var entry = state.tracks[id];
      if (!entry.ended && PATH_RANK[entry.path] < rank) {
        endTrack(entry, "superseded");
      }
    }
  }

  /** Помечает дорожку как СВОЮ: захватывать её нельзя ни одним тапом. */
  function noteLocalTrack(track) {
    if (track && track.id) {
      state.localIds[track.id] = true;
    }
  }

  function noteLocalStream(stream) {
    try {
      if (!stream || !stream.getTracks) { return; }
      var ts = stream.getTracks();
      for (var i = 0; i < ts.length; i++) {
        noteLocalTrack(ts[i]);
      }
    } catch (e) {
      fail("noteLocalStream", e);
    }
  }

  // --- аудиоконтекст --------------------------------------------------------

  function audioContext() {
    if (state.ctx) {
      return state.ctx;
    }
    var Ctor = window.AudioContext || window.webkitAudioContext;
    // Своя частота не задаётся: пересчёт в 16 кГц делает worklet, а
    // навязанная контексту частота заставила бы Chrome ресемплить ВЕСЬ граф
    // страницы — то есть менять звук встречи, а не только наш отвод.
    state.ctx = new Ctor();
    if (state.ctx.state === "suspended" && state.ctx.resume) {
      // `--autoplay-policy=no-user-gesture-required` уже стоит, но
      // подстраховка дешёвая: нажать «первый жест пользователя» на странице
      // встречи некому.
      try { state.ctx.resume(); } catch (e) { fail("ctx.resume", e); }
    }
    return state.ctx;
  }

  /**
   * ОБХОД ИЗВЕСТНОГО БАГА: `MediaStreamAudioSourceNode`, построенный от
   * УДАЛЁННОГО трека, может молчать, если этот поток нигде не
   * воспроизводится. Лечится скрытым <audio muted> с этим же потоком:
   * элемент заставляет конвейер декодирования работать, а `muted` не даёт
   * звуку уйти в колонки пользователя.
   */
  function keepAlive(stream) {
    try {
      var el = document.createElement("audio");
      el.autoplay = true;
      el.muted = true;
      el.volume = 0;
      el.setAttribute("data-forge-sink", "1");
      el.srcObject = stream;
      // Элемент вне документа у части версий не запускает конвейер, поэтому
      // он реально добавляется — но невидимым и без места на странице.
      el.style.display = "none";
      (document.body || document.documentElement).appendChild(el);
      var p = el.play();
      if (p && p.catch) { p.catch(function () {}); }
      state.sinks.push(el);
      return el;
    } catch (e) {
      fail("keepAlive", e);
      return null;
    }
  }

  /** Убирает скрытый приёмник закончившейся дорожки. */
  function dropSink(el) {
    try {
      if (!el) { return; }
      var at = state.sinks.indexOf(el);
      if (at !== -1) { state.sinks.splice(at, 1); }
      el.srcObject = null;
      if (el.parentNode) { el.parentNode.removeChild(el); }
    } catch (e) {
      fail("dropSink", e);
    }
  }

  // --- жизненный цикл треков ------------------------------------------------

  /**
   * Берёт трек в работу.
   *
   * ИНДЕКС НЕ ПЕРЕИСПОЛЬЗУЕТСЯ. Переиспользуй мы номер после `ended`, звук
   * двух РАЗНЫХ людей склеился бы в один `raw/track-N.wav`, и никакая
   * последующая атрибуция этого уже не разобрала бы.
   *
   * ТОЖДЕСТВО ИСТОЧНИКА — ЭТО `MediaStreamTrack.id`, А НЕ ОБЪЕКТ-ОБЁРТКА.
   * Один и тот же трек приходит и событием `track`, и обходом трансиверов, и
   * присваиванием `srcObject`; `WeakSet` по объекту потока здесь бесполезен —
   * потоки в разных тапах разные, а трек внутри один.
   */
  function takeTrack(track, stream, path) {
    if (!track || track.kind !== "audio") {
      return;
    }
    // Свой же исходящий звук: микрофон бота и всё, что мы шлём наружу.
    if (state.localIds[track.id]) {
      return;
    }
    var known = state.tracks[track.id];
    if (known) {
      // Тот же источник из другого тапа. Дорожка одна; поднимаем только
      // МЕТКУ пути, если этот тап приоритетнее.
      if (!known.ended && PATH_RANK[path] > PATH_RANK[known.path]) {
        known.path = path;
        notePath(path);
      }
      return;
    }
    // НОВЫЙ источник от МЕНЕЕ приоритетного тапа, пока работает более
    // приоритетный, — это дубль того же звука. Единственный случай, когда
    // такое бывает по делу, — площадка, отдающая звук двумя способами сразу.
    if (state.capturePath && PATH_RANK[path] < PATH_RANK[state.capturePath]) {
      if (!state.suppressed[track.id]) {
        state.suppressed[track.id] = true;
        emit("tap_suppressed", { t: track.id, p: path, w: state.capturePath });
      }
      return;
    }
    if (state.t0 === null) {
      state.t0 = nowMs();
      emit("recording_started", { unix: Date.now() });
    }

    var idx = state.nextIdx++;
    var entry = {
      id: track.id,
      idx: idx,
      startOffsetMs: axisMs(),
      path: path,
      ended: false,
      node: null
    };
    state.tracks[track.id] = entry;
    notePath(path);
    emit("track_added", { t: track.id, idx: idx, off: entry.startOffsetMs, p: path });

    var ms = stream && stream.getAudioTracks ? stream : new MediaStream([track]);
    entry.sink = keepAlive(ms);
    attachWorklet(entry, track, ms);

    track.addEventListener("ended", function () {
      endTrack(entry);
    });
  }

  function endTrack(entry, why) {
    if (!entry || entry.ended) {
      return;
    }
    entry.ended = true;
    emit("track_ended", { t: entry.id, idx: entry.idx, why: why || "ended" });
    try {
      if (entry.node) { entry.node.disconnect(); }
      if (entry.source) { entry.source.disconnect(); }
      dropSink(entry.sink);
      entry.sink = null;
    } catch (e) {
      fail("endTrack", e);
    }
  }

  // --- ТАП 1: RTCPeerConnection ---------------------------------------------

  function watchPeerConnection(pc) {
    if (!pc || state.pcs.indexOf(pc) !== -1) {
      return;
    }
    state.pcs.push(pc);

    // addEventListener, а НЕ pc.ontrack: присваивание перетёрло бы обработчик
    // площадки, и встреча осталась бы без картинки и звука для человека.
    pc.addEventListener("track", function (e) {
      try {
        takeTrack(e.track, e.streams && e.streams[0], PATH_RTC);
      } catch (err) {
        fail("ontrack", err);
      }
    });

    // Обход получателей: SFU умеет добавлять дорожку в уже существующий
    // transceiver, и события `track` при этом не бывает.
    var timer = setInterval(function () {
      try {
        noteLocalSenders(pc);
        var rs = receivingTracks(pc);
        for (var i = 0; i < rs.length; i++) {
          takeTrack(rs[i], null, PATH_RTC);
        }
        if (pc.connectionState === "closed") {
          clearInterval(timer);
        }
      } catch (err) {
        fail("getReceivers", err);
      }
    }, CFG.receiverScanMs);
  }

  /** Запоминает всё, что это соединение ОТПРАВЛЯЕТ, как своё. */
  function noteLocalSenders(pc) {
    try {
      if (!pc.getSenders) { return; }
      var ss = pc.getSenders();
      for (var i = 0; i < ss.length; i++) {
        noteLocalTrack(ss[i].track);
      }
    } catch (e) {
      fail("getSenders", e);
    }
  }

  /**
   * Аудиодорожки, которые соединение ДЕЙСТВИТЕЛЬНО принимает.
   *
   * ПОЧЕМУ НЕ `getReceivers()` НАПРЯМУЮ. На каждый `addTrack` заводится
   * трансивер, и у него есть свой приёмник с треком, который никогда ничего
   * не получит. `readyState` у такого трека — `live`, `kind` — `audio`,
   * отличить его от настоящего можно ТОЛЬКО по направлению трансивера.
   * Обход без этой проверки удваивал запись: на каждую настоящую дорожку
   * приходил молчащий фантом (задача .15).
   *
   * `currentDirection` — фактическое направление после согласования; до него
   * оно `null`, и это правильно: дорожки, о которых уже известно, приходят
   * событием `track` ровно в момент применения удалённого описания.
   */
  function receivingTracks(pc) {
    var out = [];
    if (pc.getTransceivers) {
      var trs = pc.getTransceivers();
      for (var i = 0; i < trs.length; i++) {
        var dir = trs[i].currentDirection;
        if (dir !== "recvonly" && dir !== "sendrecv") { continue; }
        var t = trs[i].receiver && trs[i].receiver.track;
        if (t && t.kind === "audio" && t.readyState === "live") {
          out.push(t);
        }
      }
      return out;
    }
    // Запасной путь для реализаций без трансиверов: приёмник, который ничего
    // не получал, держит трек в состоянии `muted`. Признак слабее
    // направления — молчащий участник тоже бывает `muted`, — но пустую
    // выборку он всё же лучше.
    if (!pc.getReceivers) { return out; }
    var rs = pc.getReceivers();
    for (var k = 0; k < rs.length; k++) {
      var rt = rs[k].track;
      if (rt && rt.kind === "audio" && rt.readyState === "live" && !rt.muted) {
        out.push(rt);
      }
    }
    return out;
  }

  function installPeerConnectionTap() {
    var Native = window.RTCPeerConnection || window.webkitRTCPeerConnection;
    if (!Native) {
      return;
    }

    function ForgePeerConnection() {
      // Настоящий объект, а не наследник: `instanceof` в коде площадки
      // обязан продолжать работать, и половина клиентов проверяет именно его.
      var args = Array.prototype.slice.call(arguments);
      var pc = new (Function.prototype.bind.apply(Native, [null].concat(args)))();
      try {
        watchPeerConnection(pc);
      } catch (e) {
        fail("watchPeerConnection", e);
      }
      return pc;
    }

    // ОБА звена цепочки прототипов:
    //   .prototype    — чтобы `pc instanceof RTCPeerConnection` было истиной;
    //   .__proto__    — чтобы статика (`RTCPeerConnection.generateCertificate`)
    //                   осталась на месте.
    ForgePeerConnection.prototype = Native.prototype;
    try {
      Object.setPrototypeOf(ForgePeerConnection, Native);
    } catch (e) {
      fail("setPrototypeOf", e);
    }

    try {
      window.RTCPeerConnection = ForgePeerConnection;
      if (window.webkitRTCPeerConnection) {
        window.webkitRTCPeerConnection = ForgePeerConnection;
      }
    } catch (e) {
      fail("installPeerConnectionTap", e);
    }
  }

  // --- СВОИ ДОРОЖКИ: getUserMedia и getDisplayMedia --------------------------

  /**
   * Запоминает всё, что страница получила от `getUserMedia`, как СВОЁ.
   *
   * Не тап, а его предохранитель. Площадка показывает собственное видео в
   * `<video>` через `srcObject`, и тап 3 без этого списка записал бы
   * микрофон самого бота отдельной дорожкой. Микрофон выключен, поэтому
   * дорожка вышла бы молчащей — то есть лишний `track-N.wav` тишины в каждой
   * записи и лишний «участник» в карточке встречи.
   *
   * Обёртка не меняет ни аргументы, ни результат: возвращается то же самое
   * обещание. Отказ страницы обрабатывать НЕ надо — он её дело.
   */
  function installLocalTrackWatch() {
    try {
      var md = navigator && navigator.mediaDevices;
      if (!md) { return; }
      ["getUserMedia", "getDisplayMedia"].forEach(function (name) {
        var native = md[name];
        if (typeof native !== "function") { return; }
        md[name] = function () {
          var p = native.apply(md, arguments);
          try {
            if (p && p.then) {
              p.then(noteLocalStream, function () {});
            }
          } catch (e) {
            fail(name, e);
          }
          return p;
        };
      });
    } catch (e) {
      fail("installLocalTrackWatch", e);
    }
  }

  // --- ТАП 3: HTMLMediaElement.prototype.srcObject ---------------------------

  function installMediaElementTap() {
    try {
      var proto = window.HTMLMediaElement && window.HTMLMediaElement.prototype;
      if (!proto) { return; }
      var desc = Object.getOwnPropertyDescriptor(proto, "srcObject");
      if (!desc || !desc.set) { return; }

      Object.defineProperty(proto, "srcObject", {
        configurable: true,
        enumerable: desc.enumerable,
        get: desc.get,
        set: function (value) {
          try {
            // Свои же скрытые приёмники пропускаем: иначе тап поймал бы
            // собственный поток и завёл дубль дорожки.
            if (!this.hasAttribute || !this.hasAttribute("data-forge-sink")) {
              if (value && value.getAudioTracks) {
                var ts = value.getAudioTracks();
                for (var i = 0; i < ts.length; i++) {
                  takeTrack(ts[i], value, PATH_ELEMENT);
                }
              }
            }
          } catch (e) {
            fail("srcObject", e);
          }
          return desc.set.call(this, value);
        }
      });
    } catch (e) {
      fail("installMediaElementTap", e);
    }
  }

  // --- ТАП 2: AudioNode.prototype.connect -----------------------------------

  /**
   * Единственный путь для Zoom: он не отдаёт медиа через WebRTC вовсе.
   *
   * Всё, что подключается к `AudioDestinationNode` страницы, ДОПОЛНИТЕЛЬНО
   * зеркалится в наш `MediaStreamAudioDestinationNode`. Исходное ребро графа
   * не трогаем — звук встречи для человека обязан остаться целым; мы только
   * добавляем второе ребро.
   *
   * Защита от петли — `WeakSet` уже обработанных узлов: наше собственное
   * `connect` иначе снова попало бы в патч и ушло в бесконечную рекурсию.
   */
  function installWebAudioTap() {
    try {
      if (!window.AudioNode || !window.AudioNode.prototype.connect) { return; }
      state.mirrored = new WeakSet();
      var nativeConnect = window.AudioNode.prototype.connect;

      window.AudioNode.prototype.connect = function (destination) {
        var result = nativeConnect.apply(this, arguments);
        try {
          var isOutput =
            destination &&
            window.AudioDestinationNode &&
            destination instanceof window.AudioDestinationNode;
          // Зеркало — САМЫЙ МЛАДШИЙ путь и единственный, который порождает
          // НОВЫЙ трек вместо того, чтобы взять существующий. Пока работает
          // старший тап, зеркалить нечего: получилась бы вторая копия уже
          // записанного звука, которую никакой дедуп по `id` не поймает.
          if (
            isOutput &&
            !state.mirrored.has(this) &&
            !(state.capturePath && PATH_RANK[state.capturePath] > PATH_RANK[PATH_WEBAUDIO])
          ) {
            state.mirrored.add(this);
            var ctx = destination.context;
            var mirror = ctx.createMediaStreamDestination();
            // ВАЖНО: зовём НАТИВНЫЙ connect — патченый снова прошёл бы сюда.
            nativeConnect.call(this, mirror);
            var tracks = mirror.stream.getAudioTracks();
            for (var i = 0; i < tracks.length; i++) {
              takeTrack(tracks[i], mirror.stream, PATH_WEBAUDIO);
            }
          }
        } catch (e) {
          fail("audioNodeConnect", e);
        }
        return result;
      };
    } catch (e) {
      fail("installWebAudioTap", e);
    }
  }

  // --- отвод трека в worklet ------------------------------------------------

  function ensureWorklet(ctx) {
    if (state.workletReady) {
      return state.workletReady;
    }
    if (!ctx.audioWorklet) {
      state.workletReady = Promise.reject(new Error("нет audioWorklet"));
      return state.workletReady;
    }
    // Blob-URL, а не файл: страница чужая, и класть на неё свои ресурсы
    // некуда. CSP снят `Page.setBypassCSP`, поэтому blob: загружается.
    var url = URL.createObjectURL(new Blob([CFG.workletSource], { type: "text/javascript" }));
    state.workletReady = ctx.audioWorklet.addModule(url).then(function () {
      URL.revokeObjectURL(url);
    });
    return state.workletReady;
  }

  function attachWorklet(entry, track, stream) {
    var ctx = audioContext();
    ensureWorklet(ctx).then(function () {
      try {
        var source = ctx.createMediaStreamSource(stream);
        var node = new AudioWorkletNode(ctx, CFG.workletName, {
          numberOfInputs: 1,
          numberOfOutputs: 0,
          processorOptions: {
            trackId: entry.id,
            idx: entry.idx,
            startOffsetMs: entry.startOffsetMs,
            targetRate: CFG.sampleRate,
            frameMs: CFG.frameMs,
            vad: CFG.vad
          }
        });
        node.port.onmessage = function (e) {
          onWorkletMessage(entry, e.data);
        };
        source.connect(node);
        entry.source = source;
        entry.node = node;
      } catch (e) {
        fail("attachWorklet", e);
      }
    }, function (e) {
      fail("ensureWorklet", e);
    });
  }

  function onWorkletMessage(entry, msg) {
    try {
      if (!msg) { return; }
      if (msg.k === "a") {
        sendAudio(entry, msg);
      } else if (msg.k === "vad") {
        emit(msg.on ? "speech_start" : "speech_end", { t: entry.id, at: msg.at });
      }
    } catch (e) {
      fail("onWorkletMessage", e);
    }
  }

  // Заглушка транспорта: настоящая кодировка кадра живёт в transport.js и
  // подставляется сборщиком. Здесь — только вызов, чтобы порядок файлов не
  // определял порядок задач.
  function sendAudio(entry, msg) {
    forgeSendAudioFrame(entry, msg);
  }

  // --- запуск ---------------------------------------------------------------

  // Порядок установки соответствует приоритету путей, но фактически важен
  // только один момент: все три тапа обязаны встать ДО того, как страница
  // выполнит хоть строчку своего кода. Ради этого весь скрипт и едет через
  // Page.addScriptToEvaluateOnNewDocument.
  //
  // Список своих дорожек ставится ПЕРВЫМ: площадка зовёт `getUserMedia`
  // раньше, чем открывает соединение, и опоздай мы — свой же микрофон успел
  // бы уехать в захват через `srcObject` превью.
  installLocalTrackWatch();
  installPeerConnectionTap();
  installMediaElementTap();
  installWebAudioTap();

  emit("taps_installed", {
    rtc: !!window.RTCPeerConnection,
    element: !!(window.HTMLMediaElement && window.HTMLMediaElement.prototype),
    webaudio: !!(window.AudioNode && window.AudioNode.prototype.connect)
  });
})();
