/*
 * ТРАНСПОРТ ЗВУКА СО СТРАНИЦЫ В RUST.
 *
 * Кадр уходит через CDP-биндинг: `window.__forge_<rand32>(payload)` →
 * `Runtime.bindingCalled`. Функция биндинга уже забрана в замыкание
 * обёрткой инжекта и удалена из `window`; здесь она видна как `post`.
 *
 * ПОЧЕМУ НЕ ЧЕРЕЗ НАШ HTTP-СЕРВЕР — ЧЕТЫРЕ БАРЬЕРА
 *
 *   1. `auth::csrf_guard` (tmux-web/src/auth.rs) отвергает апгрейд `/ws/*` с
 *      чужим `Origin`, а этот скрипт живёт на `telemost.yandex.ru`;
 *   2. Bearer-авторизация — токена у страницы нет;
 *   3. жёсткий `connect-src` в CSP у Google Meet режет сокет к localhost;
 *   4. Private Network Access — запрос из публичного origin в loopback
 *      требует preflight, которого мы не отдаём.
 *
 * Биндинг убирает все четыре РАЗОМ, потому что убирает сеть. Роут
 * `/ws/meet/ingest` не создаётся намеренно; разбор отклонённого варианта
 * «исключение по одноразовому токену» — в `routes/mod.rs` и рядом с
 * `RAW_UPLOAD_PATHS` в `tmux-web/src/auth.rs`.
 *
 * ФОРМАТ КАДРА
 *
 *   {"k":"a","t":"<id трека>","o":<смещение в сэмплах>,"n":<сэмплов>,"d":"<base64 PCM16>"}
 *
 * Ключи однобуквенные не ради красоты: при пяти дорожках уходит около
 * 213 КБ/с, и каждый лишний байт имени поля умножается на пятьдесят кадров в
 * секунду.
 */

/** Счётчики транспорта — уезжают в манифест и видны в логе. */
var forgeTransportStats = { frames: 0, samples: 0, bytes: 0, failed: 0 };

/**
 * Отправляет кадр звука.
 *
 * Ошибка отправки НЕ прерывает захват: канал мог моргнуть при
 * переподключении CDP, а потерянный кадр по устройству формата означает
 * тишину на своём месте — таймлайн не сдвигается.
 */
function forgeSendAudioFrame(entry, msg) {
  try {
    var samples = new Int16Array(msg.b);
    var payload = {
      k: "a",
      t: entry.id,
      o: msg.o,
      n: msg.n,
      d: pcm16ToBase64(samples)
    };
    forgeTransportStats.frames++;
    forgeTransportStats.samples += msg.n;
    forgeTransportStats.bytes += payload.d.length;
    post(payload);
  } catch (e) {
    forgeTransportStats.failed++;
    if (forgeTransportStats.failed === 1) {
      try {
        post({ k: "e", n: "transport_error", message: String(e && e.message ? e.message : e) });
      } catch (ignored) {}
    }
  }
}
