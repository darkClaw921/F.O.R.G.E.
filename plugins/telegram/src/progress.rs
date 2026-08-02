//! Отчёты о прогрессе передачи файлов — общий механизм для приёма и отправки.
//!
//! Живёт отдельным модулем, а не внутри роута загрузки, ровно по одной
//! причине: throttle обязан быть ОДИН на оба направления. Скачивание
//! (`download_progress`, приём медиа) и отправка (`upload_progress`,
//! отправка медиа) иначе разъехались бы по частоте событий, и прогрессбар
//! в UI вёл бы себя по-разному в двух половинах одного и того же диалога.
//!
//! # Почему нужен throttle
//!
//! Чанк загрузки у grammers — 512 КиБ, но обёртка ProgressReader при
//! отправке дёргает колбэк на КАЖДЫЙ `poll_read` (это килобайты). Без
//! ограничения 100-мегабайтный файл дал бы десятки тысяч кадров в
//! broadcast-канал ёмкостью 1024 — все подписчики немедленно получили бы
//! `Lagged` и ушли в `resync`, то есть индикатор прогресса СЛОМАЛ бы
//! realtime. Отсюда [`Throttle`]: не чаще одного события в
//! [`MIN_INTERVAL`] и не чаще, чем раз в [`MIN_DELTA`] байт.
//!
//! # Почему финальное событие обязательно
//!
//! `is_final` проходит ВСЕГДА, даже если с прошлого раза не прошло ни
//! миллисекунды. Иначе последнее событие могло бы не дойти, и прогрессбар
//! навсегда замер бы на 97 % — визуально это выглядит как зависшая
//! загрузка, хотя файл уже на месте.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, ReadBuf};

use crate::state::TelegramState;
use crate::ws::protocol::TgServerMsg;

/// Минимальный интервал между событиями прогресса.
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Минимальный прирост байт между событиями прогресса (256 КиБ).
pub const MIN_DELTA: u64 = 256 * 1024;

/// Ограничитель частоты событий прогресса.
///
/// Состояние — момент и объём последнего отправленного события; решение
/// принимает [`Throttle::should_emit`]. Структура намеренно не знает ни про
/// broadcast, ни про DTO: это чистая логика, проверяемая без сети.
#[derive(Debug, Clone)]
pub struct Throttle {
    /// Когда отправляли прошлое событие.
    last_emit: Instant,
    /// Сколько байт было при прошлом событии.
    last_bytes: u64,
    /// Минимальный интервал.
    min_interval: Duration,
    /// Минимальный прирост байт.
    min_delta: u64,
    /// Было ли хоть одно событие (первое пропускаем всегда — UI обязан
    /// увидеть «загрузка началась» без ожидания 100 мс).
    started: bool,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttle {
    /// Throttle со штатными параметрами (100 мс / 256 КиБ).
    pub fn new() -> Self {
        Self::with_limits(MIN_INTERVAL, MIN_DELTA)
    }

    /// Throttle с явными параметрами — нужен тестам (и на случай, если
    /// какой-то поток захочет собственную частоту).
    pub fn with_limits(min_interval: Duration, min_delta: u64) -> Self {
        Self {
            last_emit: Instant::now(),
            last_bytes: 0,
            min_interval,
            min_delta,
            started: false,
        }
    }

    /// Пора ли отправлять событие о `current` переданных байтах.
    ///
    /// `is_final == true` ⇒ всегда `true` (см. шапку модуля). В остальных
    /// случаях достаточно ЛЮБОГО из условий: прошло `min_interval` или
    /// прибавилось `min_delta` байт — так медленная загрузка тоже двигает
    /// прогрессбар (по времени), а быстрая не заваливает канал (по байтам).
    ///
    /// Метод изменяет состояние: `true` означает «событие считается
    /// отправленным», поэтому вызывать его надо ровно один раз на решение.
    pub fn should_emit(&mut self, current: u64, is_final: bool) -> bool {
        if is_final {
            self.mark(current);
            return true;
        }
        if !self.started {
            self.mark(current);
            return true;
        }
        let by_time = self.last_emit.elapsed() >= self.min_interval;
        let by_bytes = current.saturating_sub(self.last_bytes) >= self.min_delta;
        if by_time || by_bytes {
            self.mark(current);
            return true;
        }
        false
    }

    /// Запоминает отправленное событие.
    fn mark(&mut self, current: u64) {
        self.last_emit = Instant::now();
        self.last_bytes = current;
        self.started = true;
    }

    /// Сколько байт было на прошлом отправленном событии (диагностика/тесты).
    pub fn last_bytes(&self) -> u64 {
        self.last_bytes
    }
}

/// Обёртка над `AsyncRead`, считающая прочитанные байты.
///
/// Нужна отправке файлов (Фаза 5): `upload_stream` у grammers принимает
/// поток и сам решает, когда читать, поэтому единственная точка, где можно
/// узнать прогресс, — сам ридер. Написана здесь вместе с [`Throttle`],
/// чтобы обе половины (приём/отправка) считали прогресс одинаково.
///
/// Прозрачность — контракт: [`ProgressReader`] НЕ меняет, не буферизует и
/// не переупорядочивает данные; байты доходят до читателя в точности как из
/// вложенного ридера (тест `progress_reader_is_byte_transparent`).
pub struct ProgressReader<R, F> {
    /// Вложенный ридер.
    inner: R,
    /// Сколько байт прочитано суммарно.
    read: u64,
    /// Колбэк «прочитано столько-то всего».
    on_progress: F,
}

impl<R, F> ProgressReader<R, F> {
    /// Оборачивает ридер колбэком прогресса.
    pub fn new(inner: R, on_progress: F) -> Self {
        Self {
            inner,
            read: 0,
            on_progress,
        }
    }

    /// Сколько байт прочитано.
    pub fn bytes_read(&self) -> u64 {
        self.read
    }

    /// Отдаёт вложенный ридер обратно.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R, F> AsyncRead for ProgressReader<R, F>
where
    R: AsyncRead + Unpin,
    F: FnMut(u64) + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let me = self.get_mut();
        let poll = Pin::new(&mut me.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let n = buf.filled().len().saturating_sub(before) as u64;
            if n > 0 {
                me.read += n;
                (me.on_progress)(me.read);
            }
        }
        poll
    }
}

/// Отправитель событий `download_progress` по одному медиа.
///
/// Держит собственный [`Throttle`], поэтому две параллельные загрузки не
/// глушат друг друга. Знает ключ медиа целиком, чтобы фронт нашёл нужную
/// карточку по `(dialog_id, message_id, kind)`.
pub struct DownloadReporter {
    /// Куда вещать.
    state: Arc<TelegramState>,
    /// Диалог (строкой — 64-битный id не влезает в JS-число).
    dialog_id: String,
    /// Сообщение.
    message_id: i32,
    /// `thumb` | `full`.
    kind: String,
    /// Ограничитель частоты.
    throttle: Throttle,
}

impl DownloadReporter {
    /// Новый отчётчик для конкретного медиа.
    pub fn new(
        state: Arc<TelegramState>,
        dialog_id: impl Into<String>,
        message_id: i32,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            state,
            dialog_id: dialog_id.into(),
            message_id,
            kind: kind.into(),
            throttle: Throttle::new(),
        }
    }

    /// Промежуточный отчёт (проходит throttle).
    pub fn report(&mut self, received: u64, total: Option<u64>) {
        self.emit(received, total, false);
    }

    /// Финальный отчёт — уходит всегда.
    pub fn finish(&mut self, received: u64, total: Option<u64>) {
        self.emit(received, total, true);
    }

    fn emit(&mut self, received: u64, total: Option<u64>, is_final: bool) {
        if !self.throttle.should_emit(received, is_final) {
            return;
        }
        self.state
            .broadcast_event(TgServerMsg::DownloadProgress {
                dialog_id: self.dialog_id.clone(),
                message_id: self.message_id,
                kind: self.kind.clone(),
                received,
                total,
            });
    }
}

/// Отправитель событий `upload_progress` по одной upload-сессии.
///
/// Пишется вместе с [`DownloadReporter`], потому что throttle и контракт
/// событий у направлений общие; использоваться начнёт в отправке медиа.
pub struct UploadReporter {
    /// Куда вещать.
    state: Arc<TelegramState>,
    /// Идентификатор upload-сессии.
    upload_id: String,
    /// Полный размер файла (при отправке он известен заранее — в отличие от
    /// приёма, где Telegram может не сообщить размер).
    total: u64,
    /// Ограничитель частоты.
    throttle: Throttle,
}

impl UploadReporter {
    /// Новый отчётчик для upload-сессии.
    pub fn new(state: Arc<TelegramState>, upload_id: impl Into<String>, total: u64) -> Self {
        Self {
            state,
            upload_id: upload_id.into(),
            total,
            throttle: Throttle::new(),
        }
    }

    /// Промежуточный отчёт (проходит throttle).
    pub fn report(&mut self, sent: u64) {
        self.emit(sent, false);
    }

    /// Финальный отчёт — уходит всегда.
    pub fn finish(&mut self, sent: u64) {
        self.emit(sent, true);
    }

    fn emit(&mut self, sent: u64, is_final: bool) {
        if !self.throttle.should_emit(sent, is_final) {
            return;
        }
        self.state.broadcast_event(TgServerMsg::UploadProgress {
            upload_id: self.upload_id.clone(),
            sent,
            total: self.total,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;
    use tokio::io::AsyncReadExt;

    fn state() -> Arc<TelegramState> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            ..TelegramConfig::default()
        };
        // Файлы в тесте не создаются, каталог нужен только как значение пути.
        std::mem::forget(dir);
        TelegramState::new(cfg)
    }

    /// Основной критерий: 10 000 мелких приращений за один «тик» времени
    /// дают считаные события, а не 10 000.
    #[test]
    fn tiny_increments_are_throttled() {
        let mut t = Throttle::new();
        let mut emitted = 0;
        let mut bytes = 0u64;
        for _ in 0..10_000 {
            bytes += 64; // всего 640 КиБ ⇒ по байтам это ~2 порога
            if t.should_emit(bytes, false) {
                emitted += 1;
            }
        }
        // Первое событие + пороги по 256 КиБ. Даже если тест шёл дольше
        // 100 мс, событий не может быть много.
        assert!(
            (1..=12).contains(&emitted),
            "ожидались единицы событий, получено {emitted}"
        );
    }

    /// Порог по байтам работает независимо от времени.
    #[test]
    fn byte_threshold_emits_without_waiting() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), MIN_DELTA);
        assert!(t.should_emit(0, false), "первое событие проходит всегда");
        assert!(!t.should_emit(MIN_DELTA - 1, false));
        assert!(t.should_emit(MIN_DELTA, false));
        assert!(!t.should_emit(MIN_DELTA + 10, false));
        assert!(t.should_emit(MIN_DELTA * 2, false));
    }

    /// Порог по времени работает независимо от объёма.
    #[test]
    fn time_threshold_emits_without_bytes() {
        let mut t = Throttle::with_limits(Duration::from_millis(1), u64::MAX);
        assert!(t.should_emit(1, false));
        assert!(!t.should_emit(2, false));
        std::thread::sleep(Duration::from_millis(3));
        assert!(t.should_emit(3, false), "по времени событие обязано пройти");
    }

    /// Финал проходит всегда, даже сразу после другого события.
    #[test]
    fn final_event_always_passes() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), u64::MAX);
        assert!(t.should_emit(10, false));
        assert!(!t.should_emit(11, false));
        assert!(t.should_emit(11, true), "финал не должен глотаться");
        // И даже дважды подряд: повторный финал (ретрай) тоже уходит.
        assert!(t.should_emit(11, true));
    }

    /// Прогрессбар не должен «ехать назад»: throttle не мешает, но и не
    /// ломается на убывающем счётчике (теоретический ретрай чанка).
    #[test]
    fn decreasing_counter_does_not_panic() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), MIN_DELTA);
        assert!(t.should_emit(MIN_DELTA * 4, false));
        assert!(!t.should_emit(10, false), "saturating_sub, а не паника");
    }

    #[tokio::test]
    async fn progress_reader_is_byte_transparent() {
        // Данные с рваными границами, чтобы poll_read вызвался много раз.
        let data: Vec<u8> = (0..(70 * 1024)).map(|i| (i % 251) as u8).collect();
        let counter = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let sink = counter.clone();

        let mut reader = ProgressReader::new(std::io::Cursor::new(data.clone()), move |n| {
            sink.lock().unwrap().push(n);
        });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();

        assert_eq!(out, data, "ридер обязан быть байт-в-байт прозрачным");
        assert_eq!(reader.bytes_read(), data.len() as u64);
        let seen = counter.lock().unwrap().clone();
        assert!(!seen.is_empty(), "колбэк ни разу не вызвался");
        assert_eq!(*seen.last().unwrap(), data.len() as u64);
        // Счётчик монотонно растёт.
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    }

    /// Пустой ридер: колбэк не зовётся вообще (нечего сообщать).
    #[tokio::test]
    async fn progress_reader_skips_callback_on_empty_reads() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        let mut reader = ProgressReader::new(std::io::Cursor::new(Vec::new()), move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        assert!(out.is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn download_reporter_broadcasts_first_and_final() {
        let st = state();
        let mut rx = st.subscribe();
        let mut rep = DownloadReporter::new(st.clone(), "-100999", 7, "full");

        rep.report(0, Some(1000));
        // Это событие throttle съест (ни времени, ни байт).
        rep.report(10, Some(1000));
        rep.finish(1000, Some(1000));

        let mut frames = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            frames.push(ev);
        }
        assert_eq!(frames.len(), 2, "ожидались первое и финальное: {frames:?}");
        match &frames[0] {
            TgServerMsg::DownloadProgress {
                dialog_id,
                message_id,
                kind,
                received,
                total,
            } => {
                assert_eq!(dialog_id, "-100999");
                assert_eq!(*message_id, 7);
                assert_eq!(kind, "full");
                assert_eq!(*received, 0);
                assert_eq!(*total, Some(1000));
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
        match &frames[1] {
            TgServerMsg::DownloadProgress { received, total, .. } => {
                assert_eq!(*received, 1000);
                assert_eq!(*total, Some(1000));
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    /// Неизвестный размер (Telegram не прислал) уходит как `total: null` —
    /// фронт по нему рисует индетерминированный индикатор.
    #[tokio::test]
    async fn download_reporter_allows_unknown_total() {
        let st = state();
        let mut rx = st.subscribe();
        let mut rep = DownloadReporter::new(st.clone(), "777000", 1, "thumb");
        rep.finish(512, None);
        match rx.try_recv().unwrap() {
            TgServerMsg::DownloadProgress { total, kind, .. } => {
                assert_eq!(total, None);
                assert_eq!(kind, "thumb");
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    #[tokio::test]
    async fn upload_reporter_broadcasts_upload_progress() {
        let st = state();
        let mut rx = st.subscribe();
        let mut rep = UploadReporter::new(st.clone(), "u-1", 2048);
        rep.report(0);
        rep.finish(2048);

        let mut frames = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            frames.push(ev);
        }
        assert_eq!(frames.len(), 2);
        match &frames[1] {
            TgServerMsg::UploadProgress {
                upload_id,
                sent,
                total,
            } => {
                assert_eq!(upload_id, "u-1");
                assert_eq!(*sent, 2048);
                assert_eq!(*total, 2048);
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    /// Отсутствие подписчиков — норма (вкладку не открывали): отчётчик не
    /// должен ни падать, ни как-то менять поведение.
    #[tokio::test]
    async fn reporters_work_without_subscribers() {
        let st = state();
        DownloadReporter::new(st.clone(), "1", 1, "full").finish(1, Some(1));
        UploadReporter::new(st, "u", 1).finish(1);
    }

    /// Откат счётчика (ретрай чанка) не должен замораживать прогрессбар:
    /// порог по байтам после отката недостижим ещё долго (last_bytes велик),
    /// но порог по времени обязан продолжать пропускать события, а после
    /// нового события база для байтового порога — свежая, а не старая.
    #[test]
    fn throttle_recovers_by_time_after_counter_rollback() {
        let mut t = Throttle::with_limits(Duration::from_millis(50), MIN_DELTA);
        assert!(t.should_emit(MIN_DELTA * 4, false), "первое событие");
        assert!(!t.should_emit(10, false), "сразу после отката — тишина");

        std::thread::sleep(Duration::from_millis(60));
        assert!(
            t.should_emit(20, false),
            "по времени событие обязано пройти даже после отката"
        );
        assert_eq!(t.last_bytes(), 20, "база пересчитана на новое значение");
        // И байтовый порог теперь отсчитывается от 20, а не от старого 1 МиБ.
        assert!(t.should_emit(20 + MIN_DELTA, false));
    }

    /// Ридер, отдающий данные кусками по 8 байт и падающий после конца —
    /// имитация обрыва сети посреди передачи.
    struct BrokenReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl AsyncRead for BrokenReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let me = self.get_mut();
            if me.pos < me.data.len() {
                let n = (me.data.len() - me.pos).min(8).min(buf.remaining());
                buf.put_slice(&me.data[me.pos..me.pos + n]);
                me.pos += n;
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "network dropped",
                )))
            }
        }
    }

    /// Обрыв сети после частичных данных: ошибка доходит до читателя как
    /// есть, счётчик хранит фактически прочитанное, а колбэк НЕ зовётся на
    /// ошибочном poll (иначе UI получил бы «прогресс» из ниоткуда).
    #[tokio::test]
    async fn progress_reader_propagates_error_after_partial_data() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let sink = seen.clone();
        let mut reader = ProgressReader::new(
            BrokenReader {
                data: vec![7u8; 16],
                pos: 0,
            },
            move |n| sink.lock().unwrap().push(n),
        );

        let mut buf = [0u8; 64];
        assert_eq!(reader.read(&mut buf).await.unwrap(), 8);
        assert_eq!(reader.read(&mut buf).await.unwrap(), 8);
        let err = reader.read(&mut buf).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);

        assert_eq!(reader.bytes_read(), 16, "ошибка не должна портить счётчик");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![8, 16],
            "колбэк не зовётся на ошибочном poll"
        );
    }

    /// Telegram иногда сообщает неверный размер: total=0 и received>total
    /// уходят на фронт как есть, без клампа и без паники — решение о том,
    /// как это рисовать, принимает UI.
    #[tokio::test]
    async fn download_reporter_passes_degenerate_sizes_verbatim() {
        let st = state();
        let mut rx = st.subscribe();
        let mut rep = DownloadReporter::new(st.clone(), "777000", 3, "full");
        rep.finish(5, Some(0));

        match rx.try_recv().unwrap() {
            TgServerMsg::DownloadProgress { received, total, .. } => {
                assert_eq!(received, 5, "received > total проходит без клампа");
                assert_eq!(total, Some(0), "нулевой total не подменяется");
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    /// Две параллельные загрузки: у каждого репортёра СВОЙ throttle, поэтому
    /// первый кадр второй загрузки не съедается лимитом первой (регрессия
    /// «общий/статический throttle» ломала бы карточку второй загрузки —
    /// она не узнала бы, что загрузка началась).
    #[tokio::test]
    async fn parallel_reporters_have_independent_throttles() {
        let st = state();
        let mut rx = st.subscribe();
        let mut a = DownloadReporter::new(st.clone(), "111", 1, "full");
        let mut b = DownloadReporter::new(st.clone(), "222", 2, "thumb");
        a.report(0, Some(100));
        b.report(0, Some(100));

        let mut dialogs = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let TgServerMsg::DownloadProgress { dialog_id, .. } = ev {
                dialogs.push(dialog_id);
            }
        }
        assert_eq!(
            dialogs,
            vec!["111".to_string(), "222".to_string()],
            "оба первых кадра обязаны дойти"
        );
    }
}
