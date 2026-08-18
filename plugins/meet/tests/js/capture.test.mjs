/*
 * ТАПЫ ЗАХВАТА: ОДИН ИСТОЧНИК — ОДНА ДОРОЖКА.
 *
 * Проверяется `../../src/bot/inject/capture.js` — тот самый файл, который
 * уезжает в страницу. Не копия и не пересказ.
 *
 * ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Живой прогон на мок-встрече дал РОВНО ВДВОЕ
 * больше дорожек, чем источников (6 вместо 3, 4 вместо 2, 2 вместо 1), и
 * поймал это только восьмисекундный сценарий: короткий прогон в 1720 мс
 * успевал закончиться раньше первого обхода приёмников и выглядел здоровым.
 * Тест, который ловит дефект лишь иногда, — не страж. Здесь браузера нет
 * вовсе, время двигает сам тест, и обход приёмников делается вызовом
 * `env.scan()`, то есть дефект ловится ВСЕГДА и за миллисекунды.
 *
 * ЧТО ИМЕННО БЫЛО СЛОМАНО. Обход `pc.getReceivers()` брал приёмники и у
 * ОТПРАВЛЯЮЩЕЙ стороны: на каждый `addTrack` заводится трансивер, у него
 * есть приёмник, а у приёмника — трек, который никогда ничего не получит.
 * Он `live`, он `audio`, и отличить его от настоящего можно только по
 * направлению трансивера.
 *
 * ЧЕМ ЗАМЕНЁН БРАУЗЕР. Ниже — минимальные подделки WebRTC, Web Audio и
 * медиаэлемента: ровно те методы, которых касается `capture.js`. Подделка
 * намеренно скупая: чем больше в ней своего поведения, тем больше шансов
 * проверить её саму вместо проверяемого кода.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const CAPTURE_SRC = readFileSync(join(HERE, "../../src/bot/inject/capture.js"), "utf8");

let idCounter = 0;
const nextId = (prefix) => `${prefix}-${++idCounter}`;

class FakeTrack {
  constructor(kind, id) {
    this.kind = kind;
    this.id = id || nextId("track");
    this.readyState = "live";
    this.muted = false;
    this.listeners = {};
  }
  addEventListener(name, fn) {
    (this.listeners[name] = this.listeners[name] || []).push(fn);
  }
  stop() {
    this.readyState = "ended";
    (this.listeners.ended || []).forEach((fn) => fn());
  }
}

class FakeStream {
  constructor(tracks) {
    this.tracks = (tracks || []).slice();
  }
  getTracks() {
    return this.tracks.slice();
  }
  getAudioTracks() {
    return this.tracks.filter((t) => t.kind === "audio");
  }
}

class FakeMediaElement {
  constructor() {
    this.attrs = {};
    this.style = {};
    this.parentNode = null;
    this.storedSrcObject = null;
  }
  setAttribute(k, v) {
    this.attrs[k] = String(v);
  }
  getAttribute(k) {
    return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null;
  }
  hasAttribute(k) {
    return Object.prototype.hasOwnProperty.call(this.attrs, k);
  }
  play() {
    return Promise.resolve();
  }
}
// Аксессор на ПРОТОТИПЕ: тап 3 патчит именно его, и подделка обязана давать
// ему `Object.getOwnPropertyDescriptor(proto, "srcObject")` с `set`.
Object.defineProperty(FakeMediaElement.prototype, "srcObject", {
  configurable: true,
  enumerable: true,
  get() {
    return this.storedSrcObject;
  },
  set(v) {
    this.storedSrcObject = v;
  }
});

class FakeAudioNode {
  constructor(ctx) {
    this.context = ctx;
    this.outputs = [];
  }
  connect(dest) {
    this.outputs.push(dest);
    return dest;
  }
  disconnect() {
    this.outputs = [];
  }
}
class FakeAudioDestinationNode extends FakeAudioNode {}
class FakeAudioWorkletNode extends FakeAudioNode {
  constructor(ctx, name, options) {
    super(ctx);
    this.processorName = name;
    this.options = options;
    this.port = { onmessage: null, postMessage() {} };
  }
}

class FakeAudioContext {
  constructor() {
    this.state = "running";
    this.currentTime = 0;
    this.destination = new FakeAudioDestinationNode(this);
    this.audioWorklet = { addModule: () => Promise.resolve() };
  }
  resume() {
    return Promise.resolve();
  }
  createMediaStreamSource() {
    return new FakeAudioNode(this);
  }
  createMediaStreamDestination() {
    const node = new FakeAudioNode(this);
    node.stream = new FakeStream([new FakeTrack("audio", nextId("mirror"))]);
    return node;
  }
}

/** Соединение с ручным управлением: тест сам решает, что и когда приходит. */
class FakePeerConnection {
  constructor() {
    this.connectionState = "connected";
    this.transceivers = [];
    this.senders = [];
    this.listeners = {};
  }
  addEventListener(name, fn) {
    (this.listeners[name] = this.listeners[name] || []).push(fn);
  }
  fire(name, ev) {
    (this.listeners[name] || []).forEach((fn) => fn(ev));
  }
  getTransceivers() {
    return this.transceivers;
  }
  getReceivers() {
    return this.transceivers.map((t) => t.receiver);
  }
  getSenders() {
    return this.senders;
  }

  /**
   * ОТПРАВКА. Ровно то место, где родился дефект: вместе с отправителем
   * появляется трансивер, а у него — вечно молчащий приёмник.
   */
  send(track) {
    const phantom = new FakeTrack("audio", nextId("phantom"));
    phantom.muted = true;
    this.senders.push({ track });
    this.transceivers.push({ currentDirection: "sendonly", sender: { track }, receiver: { track: phantom } });
    return phantom;
  }

  /** Приём с событием `track` — обычный путь. */
  receive(track) {
    const stream = new FakeStream([track]);
    this.transceivers.push({ currentDirection: "recvonly", sender: { track: null }, receiver: { track } });
    this.fire("track", { track, streams: [stream] });
    return stream;
  }

  /** Приём БЕЗ события: SFU положил дорожку в готовый трансивер. */
  receiveSilently(track) {
    this.transceivers.push({ currentDirection: "recvonly", sender: { track: null }, receiver: { track } });
  }
}

/** Собирает окружение и выполняет в нём `capture.js`. */
function boot(overrides = {}) {
  const posted = [];
  const timers = [];
  const audios = [];

  const body = {
    children: [],
    appendChild(el) {
      this.children.push(el);
      el.parentNode = this;
    },
    removeChild(el) {
      const at = this.children.indexOf(el);
      if (at !== -1) this.children.splice(at, 1);
      el.parentNode = null;
    }
  };
  const document = {
    body,
    documentElement: body,
    createElement(tag) {
      assert.equal(tag, "audio", "скрипт создаёт только <audio>");
      const el = new FakeMediaElement();
      audios.push(el);
      return el;
    }
  };

  const localStreams = [];
  const navigator = {
    mediaDevices: {
      getUserMedia() {
        const stream = new FakeStream([new FakeTrack("audio", nextId("local"))]);
        localStreams.push(stream);
        return Promise.resolve(stream);
      }
    }
  };

  const window = {
    AudioContext: FakeAudioContext,
    RTCPeerConnection: FakePeerConnection,
    HTMLMediaElement: FakeMediaElement,
    AudioNode: FakeAudioNode,
    AudioDestinationNode: FakeAudioDestinationNode
  };

  const cfg = {
    sampleRate: 16000,
    frameMs: 20,
    receiverScanMs: 1000,
    vad: { onDb: 12, offDb: 6, confirmMs: 120, hangoverMs: 500 },
    workletName: "forge-capture",
    workletSource: "// worklet",
    ...overrides
  };

  let clock = 0;
  const fn = new Function(
    "FORGE_MEET_CFG",
    "post",
    "forgeSendAudioFrame",
    "window",
    "document",
    "navigator",
    "MediaStream",
    "AudioWorkletNode",
    "setInterval",
    "clearInterval",
    "performance",
    "URL",
    "Blob",
    CAPTURE_SRC
  );

  fn(
    cfg,
    (msg) => posted.push(msg),
    () => {},
    window,
    document,
    navigator,
    FakeStream,
    FakeAudioWorkletNode,
    (callback) => {
      timers.push({ callback, live: true });
      return timers.length - 1;
    },
    (token) => {
      if (timers[token]) timers[token].live = false;
    },
    { now: () => clock },
    { createObjectURL: () => "blob:fake", revokeObjectURL() {} },
    class FakeBlob {}
  );

  return {
    window,
    document,
    posted,
    audios,
    localStreams,
    navigator,
    advance(ms) {
      clock += ms;
    },
    /** Один обход приёмников — то, что в браузере делает setInterval. */
    scan() {
      timers.filter((t) => t.live).forEach((t) => t.callback());
    },
    events(name) {
      return posted.filter((m) => m.k === "e" && m.n === name);
    },
    tracks() {
      return posted.filter((m) => m.k === "e" && m.n === "track_added");
    },
    path() {
      const seen = posted.filter((m) => m.k === "e" && m.n === "capture_path");
      return seen.length ? seen[seen.length - 1].p : null;
    }
  };
}

/** Полноценная петля, как на мок-встрече: N отправок и N приёмов. */
function loopback(env, count) {
  const pcSend = new env.window.RTCPeerConnection();
  const pcRecv = new env.window.RTCPeerConnection();
  const remote = [];
  for (let i = 0; i < count; i += 1) {
    pcSend.send(new FakeTrack("audio", nextId("outgoing")));
    const track = new FakeTrack("audio", nextId("remote"));
    remote.push(track);
    pcRecv.receive(track);
  }
  return { pcSend, pcRecv, remote };
}

test("подделка окружения действительно ставит все три тапа", () => {
  const env = boot();
  const installed = env.events("taps_installed");
  assert.equal(installed.length, 1);
  assert.deepEqual(
    { rtc: installed[0].rtc, element: installed[0].element, webaudio: installed[0].webaudio },
    { rtc: true, element: true, webaudio: true }
  );
  // Патч обязан быть настоящим: конструктор подменён, но `instanceof` цел.
  assert.notEqual(env.window.RTCPeerConnection, FakePeerConnection);
  const pc = new env.window.RTCPeerConnection();
  assert.ok(pc instanceof FakePeerConnection, "патч сломал instanceof площадки");
});

test("молчащие приёмники отправляющей стороны НЕ становятся дорожками", () => {
  const env = boot();
  const { remote } = loopback(env, 3);
  assert.equal(env.tracks().length, 3, "события `track` дали не три дорожки");

  // Обход приёмников — то, что на живом прогоне удваивало запись.
  env.advance(2000);
  env.scan();
  env.advance(2000);
  env.scan();

  const ids = env.tracks().map((e) => e.t);
  assert.equal(
    ids.length,
    3,
    `дорожек ${ids.length}, а источников три: обход приёмников снова берёт фантомы (${ids.join(", ")})`
  );
  assert.deepEqual(ids.sort(), remote.map((t) => t.id).sort());
});

test("дорожка, добавленная БЕЗ события track, всё-таки подхватывается обходом", () => {
  const env = boot();
  const { pcRecv } = loopback(env, 1);
  assert.equal(env.tracks().length, 1);

  const late = new FakeTrack("audio", nextId("late"));
  pcRecv.receiveSilently(late);
  env.advance(2000);
  env.scan();

  assert.equal(env.tracks().length, 2, "обход приёмников перестал делать свою работу");
  assert.ok(env.tracks().some((e) => e.t === late.id));
});

test("один и тот же трек из двух тапов даёт РОВНО ОДНУ дорожку", () => {
  const env = boot();
  const track = new FakeTrack("audio", "общий-трек");
  const pc = new env.window.RTCPeerConnection();
  const stream = pc.receive(track);

  // Площадка вешает тот же поток на свой <audio> — тап 3.
  const el = env.document.createElement("audio");
  el.srcObject = stream;
  // И ещё раз, уже другой обёрткой потока: обёртки разные, трек один.
  const another = env.document.createElement("audio");
  another.srcObject = new FakeStream([track]);
  env.advance(2000);
  env.scan();

  assert.equal(env.tracks().length, 1, "дедуп идёт не по MediaStreamTrack.id");
  assert.equal(env.tracks()[0].p, "rtcpeerconnection");
  assert.equal(env.path(), "rtcpeerconnection");
});

test("путь дорожки поднимается до приоритетного тапа, а дорожка остаётся одна", () => {
  const env = boot();
  const track = new FakeTrack("audio", "сначала-элемент");
  const el = env.document.createElement("audio");
  el.srcObject = new FakeStream([track]);
  assert.equal(env.tracks().length, 1);
  assert.equal(env.tracks()[0].p, "media_element");

  const pc = new env.window.RTCPeerConnection();
  pc.receive(track);

  assert.equal(env.tracks().length, 1, "тот же трек из старшего тапа завёл вторую дорожку");
  assert.equal(env.path(), "rtcpeerconnection", "путь не поднялся до фактического");
  assert.equal(env.events("track_ended").length, 0, "дорожку сняли, хотя это тот же самый трек");
});

test("зеркало Web Audio НЕ заводится, пока звук берётся из WebRTC", () => {
  const env = boot();
  loopback(env, 2);
  assert.equal(env.tracks().length, 2);

  // Страница играет тот же звук через граф Web Audio.
  const ctx = new FakeAudioContext();
  const source = new FakeAudioNode(ctx);
  source.connect(ctx.destination);

  assert.equal(env.tracks().length, 2, "зеркало Web Audio продублировало уже записанный звук");
  assert.equal(env.path(), "rtcpeerconnection");
  // Исходное ребро графа обязано остаться: мы гость на чужой странице.
  assert.ok(source.outputs.includes(ctx.destination), "тап оборвал звук страницы");
});

test("без WebRTC зеркало Web Audio работает — это единственный путь Zoom", () => {
  const env = boot();
  const ctx = new FakeAudioContext();
  const source = new FakeAudioNode(ctx);
  source.connect(ctx.destination);

  assert.equal(env.tracks().length, 1, "звук, идущий только через Web Audio, не захвачен");
  assert.equal(env.tracks()[0].p, "webaudio");
  assert.equal(env.path(), "webaudio");
});

test("появившийся WebRTC вытесняет зеркало Web Audio, а не складывается с ним", () => {
  const env = boot();
  const ctx = new FakeAudioContext();
  const source = new FakeAudioNode(ctx);
  source.connect(ctx.destination);
  const mirror = env.tracks()[0].t;

  env.advance(3000);
  const pc = new env.window.RTCPeerConnection();
  pc.receive(new FakeTrack("audio", nextId("remote")));

  assert.equal(env.path(), "rtcpeerconnection");
  const ended = env.events("track_ended");
  assert.equal(ended.length, 1, "зеркальная дорожка осталась жить рядом с настоящей");
  assert.equal(ended[0].t, mirror);
  assert.equal(ended[0].why, "superseded", "снятие дубля обязано быть отличимо от конца дорожки");
});

test("младший тап не заводит НОВУЮ дорожку, пока работает старший", () => {
  const env = boot();
  loopback(env, 1);
  const el = env.document.createElement("audio");
  el.srcObject = new FakeStream([new FakeTrack("audio", "звук-из-элемента")]);

  assert.equal(env.tracks().length, 1);
  const suppressed = env.events("tap_suppressed");
  assert.equal(suppressed.length, 1, "подавление обязано быть видно в журнале, а не молчать");
  assert.equal(suppressed[0].p, "media_element");
  assert.equal(suppressed[0].w, "rtcpeerconnection");
});

test("свой микрофон из getUserMedia не захватывается ни одним тапом", async () => {
  const env = boot();
  const stream = await env.navigator.mediaDevices.getUserMedia({ audio: true });
  // Площадка показывает превью самой себя — ровно так, как делает Телемост.
  const preview = env.document.createElement("audio");
  preview.srcObject = stream;
  assert.equal(env.tracks().length, 0, "в запись попал микрофон самого бота");

  // И то же самое, но замеченное через отправителей соединения.
  const pc = new env.window.RTCPeerConnection();
  const outgoing = new FakeTrack("audio", nextId("outgoing"));
  pc.send(outgoing);
  env.advance(2000);
  env.scan();
  const late = env.document.createElement("audio");
  late.srcObject = new FakeStream([outgoing]);
  assert.equal(env.tracks().length, 0, "в запись попала своя же исходящая дорожка");
});

test("скрытые приёмники самого захвата не порождают дорожек и убираются в конце", () => {
  const env = boot();
  const track = new FakeTrack("audio", nextId("remote"));
  const pc = new env.window.RTCPeerConnection();
  pc.receive(track);

  const sinks = env.audios.filter((el) => el.getAttribute("data-forge-sink") === "1");
  assert.equal(sinks.length, 1, "поток обязан играть в скрытом <audio>, иначе он молчит");
  assert.equal(env.tracks().length, 1, "тап 3 поймал собственный скрытый приёмник");
  assert.ok(env.document.body.children.includes(sinks[0]));

  track.stop();
  assert.equal(env.events("track_ended").length, 1);
  assert.equal(env.events("track_ended")[0].why, "ended");
  assert.ok(
    !env.document.body.children.includes(sinks[0]),
    "скрытый приёмник остался на странице после конца дорожки"
  );
});

test("ось времени начинается с ПЕРВОЙ дорожки и больше не сдвигается", () => {
  const env = boot();
  env.advance(5000);
  const pc = new env.window.RTCPeerConnection();
  pc.receive(new FakeTrack("audio", nextId("remote")));
  assert.equal(env.tracks()[0].off, 0, "первая дорожка обязана стоять в нуле оси");

  env.advance(1500);
  pc.receive(new FakeTrack("audio", nextId("remote")));
  assert.equal(env.tracks()[1].off, 1500, "смещение второй дорожки считается от T0");
});
