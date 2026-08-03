//! Отчёты о прогрессе передачи файлов.
//!
//! Живёт отдельным модулем, а не внутри роута медиа, ровно по одной причине:
//! throttle обязан быть ОДИН на оба направления. Скачивание
//! (`download_progress`, приём вложений — Ф7) и отправка (`upload_progress`,
//! 3-шаговый конвейер VK — Ф8) иначе разъехались бы по частоте событий, и
//! прогрессбар в UI вёл бы себя по-разному в двух половинах одной переписки.
//!
//! # Почему нужен throttle
//!
//! Чтение потока с CDN идёт кусками по нескольку килобайт. Без ограничения
//! стомегабайтный файл дал бы десятки тысяч кадров в broadcast-канал ёмкостью
//! [`crate::state::BROADCAST_CAPACITY`] — все подписчики немедленно получили бы
//! `Lagged` и ушли в `resync`, то есть индикатор прогресса СЛОМАЛ бы realtime.
//! Отсюда [`Throttle`]: не чаще одного события в [`MIN_INTERVAL`] и не чаще,
//! чем раз в [`MIN_DELTA`] байт.
//!
//! # Почему финальное событие обязательно
//!
//! `is_final` проходит ВСЕГДА, даже если с прошлого раза не прошло ни
//! миллисекунды. Иначе последнее событие могло бы не дойти, и прогрессбар
//! навсегда замер бы на 97 % — визуально это выглядит как зависшая загрузка,
//! хотя файл уже на месте.
//!
//! # Чем отличается от телеграмного близнеца
//!
//! Кадр `download_progress` несёт ДОПОЛНИТЕЛЬНОЕ поле `index` — порядковый
//! номер вложения в сообщении. У Telegram вложение одно, у VK их до десяти, и
//! без индекса прогресс второго фото прилетал бы в карточку первого.
//!
//! Кадр `upload_progress` формой не отличается вовсе, но приходит В ДРУГОЙ
//! МОМЕНТ: у Telegram — во время `POST /api/telegram/upload`, у VK — во время
//! `POST /api/vk/dialogs/:id/messages`. Причина структурная: upload-сервер VK
//! выдаётся под КОНКРЕТНЫЙ `peer_id`, а на шаге `/upload` адресат ещё
//! неизвестен, поэтому весь трёхшаговый конвейер живёт внутри отправки
//! сообщения (см. [`crate::routes::upload`]). Для фронта это значит одно:
//! слушать `upload_progress` надо и ПОСЛЕ нажатия «отправить».

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, ReadBuf};

use crate::state::VkState;
use crate::ws::protocol::VkServerMsg;

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
        if is_final || !self.started {
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
/// Нужна ОТПРАВКЕ файлов: тело multipart-запроса уезжает в `reqwest` потоком,
/// и единственная точка, где можно узнать, сколько байт уже ушло, — сам ридер.
/// Живёт рядом с [`Throttle`], чтобы обе половины (приём и отправка) считали
/// прогресс одинаково.
///
/// Прозрачность — контракт: [`ProgressReader`] НЕ меняет, не буферизует и не
/// переупорядочивает данные; байты доходят до читателя в точности как из
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

/// Отправитель событий `upload_progress` по одной ЗАГРУЗКЕ.
///
/// Адрес карточки во фронте — это `upload_id`, выданный `POST /api/vk/upload`.
/// Своего throttle хватает на загрузку: параллельных файлов в одном сообщении
/// может быть до пяти (альбом), и общий ограничитель съедал бы кадры соседей.
///
/// `total` известен заранее (размер staging-файла), в отличие от приёма, где
/// VK не всегда сообщает длину.
pub struct UploadReporter {
    /// Куда вещать.
    state: Arc<VkState>,
    /// Идентификатор загрузки.
    upload_id: String,
    /// Полный размер файла.
    total: u64,
    /// Ограничитель частоты.
    throttle: Throttle,
}

impl UploadReporter {
    /// Новый отчётчик для одной загрузки.
    pub fn new(state: Arc<VkState>, upload_id: impl Into<String>, total: u64) -> Self {
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
        self.state.broadcast_event(VkServerMsg::UploadProgress {
            upload_id: self.upload_id.clone(),
            sent,
            total: self.total,
        });
    }
}

/// Отправитель событий `download_progress` по одному ВЛОЖЕНИЮ.
///
/// Держит собственный [`Throttle`], поэтому две параллельные загрузки не
/// глушат друг друга. Знает ключ медиа целиком — включая ИНДЕКС вложения,
/// без которого фронт не отличил бы второе фото сообщения от первого.
pub struct DownloadReporter {
    /// Куда вещать.
    state: Arc<VkState>,
    /// Беседа (строкой — `peer_id` не влезает в точное JS-число).
    dialog_id: String,
    /// Сообщение.
    message_id: i32,
    /// Порядковый номер вложения в сообщении.
    index: u8,
    /// `thumb` | `full`.
    kind: String,
    /// Ограничитель частоты.
    throttle: Throttle,
}

impl DownloadReporter {
    /// Новый отчётчик для конкретного вложения.
    pub fn new(
        state: Arc<VkState>,
        dialog_id: impl Into<String>,
        message_id: i32,
        index: u8,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            state,
            dialog_id: dialog_id.into(),
            message_id,
            index,
            kind: kind.into(),
            throttle: Throttle::new(),
        }
    }

    /// Отчётчик по готовому ключу кэша — он и есть адрес карточки во фронте.
    pub fn for_key(state: Arc<VkState>, key: &crate::media_cache::CacheKey) -> Self {
        Self::new(
            state,
            key.peer_id_string(),
            key.message_id,
            key.index,
            key.kind.as_str(),
        )
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
        self.state.broadcast_event(VkServerMsg::DownloadProgress {
            dialog_id: self.dialog_id.clone(),
            message_id: self.message_id,
            index: self.index,
            kind: self.kind.clone(),
            received,
            total,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VkConfig;
    use crate::media_cache::{CacheKey, MediaKind};

    fn state() -> Arc<VkState> {
        VkState::new(VkConfig::default())
    }

    fn key(index: u8) -> CacheKey {
        CacheKey {
            peer_id: 2_000_000_001,
            message_id: 7,
            index,
            kind: MediaKind::Full,
        }
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
        assert!(
            emitted < 100,
            "throttle пропустил {emitted} событий вместо считаных"
        );
        assert!(emitted >= 1, "первое событие обязано пройти");
    }

    /// Первое событие проходит всегда: UI обязан увидеть «началось».
    #[test]
    fn the_first_event_always_passes() {
        let mut t = Throttle::new();
        assert!(t.should_emit(0, false));
    }

    /// Финальное событие проходит, даже если ничего не изменилось.
    #[test]
    fn the_final_event_ignores_the_throttle() {
        let mut t = Throttle::new();
        assert!(t.should_emit(0, false));
        assert!(!t.should_emit(1, false), "мелкий прирост — не событие");
        assert!(t.should_emit(1, true), "финал обязан пройти всегда");
    }

    /// Порог по байтам работает без ожидания времени.
    #[test]
    fn the_byte_threshold_alone_is_enough() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), 1024);
        assert!(t.should_emit(0, false));
        assert!(!t.should_emit(1023, false));
        assert!(t.should_emit(1024, false));
        assert_eq!(t.last_bytes(), 1024);
    }

    /// Кадр несёт ИНДЕКС вложения: без него прогресс второго фото прилетел бы
    /// в карточку первого.
    #[tokio::test]
    async fn the_frame_carries_the_attachment_index() {
        let st = state();
        let mut rx = st.subscribe();
        let mut r = DownloadReporter::for_key(st.clone(), &key(3));
        r.finish(2048, Some(2048));

        match rx.try_recv().unwrap() {
            VkServerMsg::DownloadProgress {
                dialog_id,
                message_id,
                index,
                kind,
                received,
                total,
            } => {
                assert_eq!(dialog_id, "2000000001");
                assert_eq!(message_id, 7);
                assert_eq!(index, 3);
                assert_eq!(kind, "full");
                assert_eq!(received, 2048);
                assert_eq!(total, Some(2048));
            }
            other => panic!("ожидался download_progress, пришло {other:?}"),
        }
    }

    /// Ридер обязан быть байт-в-байт прозрачным: он стоит в теле реального
    /// multipart-запроса, и любая перестановка байт испортила бы файл у
    /// собеседника.
    #[tokio::test]
    async fn progress_reader_is_byte_transparent() {
        use tokio::io::AsyncReadExt;

        let data: Vec<u8> = (0..(70 * 1024)).map(|i| (i % 251) as u8).collect();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let sink = seen.clone();

        let mut reader = ProgressReader::new(std::io::Cursor::new(data.clone()), move |n| {
            sink.lock().unwrap().push(n);
        });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();

        assert_eq!(out, data, "ридер обязан быть байт-в-байт прозрачным");
        assert_eq!(reader.bytes_read(), data.len() as u64);
        let calls = seen.lock().unwrap().clone();
        assert!(!calls.is_empty(), "колбэк ни разу не вызвался");
        assert_eq!(*calls.last().unwrap(), data.len() as u64);
        assert!(calls.windows(2).all(|w| w[0] < w[1]), "счётчик не растёт: {calls:?}");
    }

    /// Пустой файл: колбэк не зовётся вовсе — сообщать нечего, а деление на
    /// ноль в процентах делает фронт.
    #[tokio::test]
    async fn progress_reader_stays_silent_on_an_empty_file() {
        use tokio::io::AsyncReadExt;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        let mut reader = ProgressReader::new(std::io::Cursor::new(Vec::new()), move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        assert!(out.is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Ридер, отдающий данные кусками по 8 байт и падающий после конца —
    /// имитация обрыва сети посреди отправки.
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

    /// Обрыв после частичных данных: ошибка доходит до читателя как есть,
    /// счётчик хранит фактически прочитанное, а колбэк НЕ зовётся на ошибочном
    /// poll — иначе UI получил бы «прогресс» из ниоткуда.
    #[tokio::test]
    async fn progress_reader_propagates_errors_without_faking_progress() {
        use tokio::io::AsyncReadExt;

        let seen = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
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
        assert_eq!(*seen.lock().unwrap(), vec![8, 16]);
    }

    /// Кадр `upload_progress` адресуется `upload_id` и несёт полный размер:
    /// форма ровно телеграмная, потому что фронт у обеих панелей один и тот же
    /// по структуре.
    #[tokio::test]
    async fn upload_reporter_broadcasts_the_documented_frame() {
        let st = state();
        let mut rx = st.subscribe();
        let mut rep = UploadReporter::new(st.clone(), "u-1", 2048);
        rep.report(0);
        // Это событие throttle съест (ни времени, ни байт).
        rep.report(10);
        rep.finish(2048);

        let mut frames = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            frames.push(ev);
        }
        assert_eq!(frames.len(), 2, "ожидались первое и финальное: {frames:?}");
        match &frames[1] {
            VkServerMsg::UploadProgress {
                upload_id,
                sent,
                total,
            } => {
                assert_eq!(upload_id, "u-1");
                assert_eq!(*sent, 2048, "финальное событие обязано быть полным");
                assert_eq!(*total, 2048);
            }
            other => panic!("неожиданный кадр: {other:?}"),
        }
    }

    /// Альбом: у каждой загрузки свой throttle, поэтому первый кадр второго
    /// файла не съедается лимитом первого — иначе карточка второго фото не
    /// узнала бы, что отправка началась.
    #[tokio::test]
    async fn upload_reporters_of_an_album_are_independent() {
        let st = state();
        let mut rx = st.subscribe();
        let mut a = UploadReporter::new(st.clone(), "u-a", 100);
        let mut b = UploadReporter::new(st.clone(), "u-b", 100);
        a.report(0);
        b.report(0);

        let mut ids = Vec::new();
        while let Ok(VkServerMsg::UploadProgress { upload_id, .. }) = rx.try_recv() {
            ids.push(upload_id);
        }
        assert_eq!(ids, vec!["u-a".to_string(), "u-b".to_string()]);
    }

    /// Отсутствие подписчиков — норма (вкладку не открывали): отчётчики не
    /// имеют права ни падать, ни менять поведение.
    #[tokio::test]
    async fn reporters_work_without_subscribers() {
        let st = state();
        DownloadReporter::for_key(st.clone(), &key(0)).finish(1, Some(1));
        UploadReporter::new(st, "u", 1).finish(1);
    }

    /// Две загрузки разных вложений одного сообщения не глушат друг друга:
    /// throttle у каждой свой.
    #[tokio::test]
    async fn reporters_of_different_attachments_are_independent() {
        let st = state();
        let mut rx = st.subscribe();
        let mut a = DownloadReporter::for_key(st.clone(), &key(0));
        let mut b = DownloadReporter::for_key(st.clone(), &key(1));
        a.report(0, None);
        b.report(0, None);

        let mut seen = Vec::new();
        while let Ok(VkServerMsg::DownloadProgress { index, .. }) = rx.try_recv() {
            seen.push(index);
        }
        assert_eq!(seen, vec![0, 1]);
    }
}
