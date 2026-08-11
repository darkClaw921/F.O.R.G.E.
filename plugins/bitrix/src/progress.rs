//! Отчёты о прогрессе передачи файлов (Р8).
//!
//! Модуль общий на ОБА направления: скачивание вложения в файловый кэш (Ф8,
//! кадр [`B24ServerMsg::DownloadProgress`]) и отправку файла в портал (Ф9,
//! кадр [`B24ServerMsg::UploadProgress`]). Отдельным модулем, а не куском
//! роута, он живёт ровно затем, чтобы ограничитель частоты был ОДИН: разъедься
//! он по двум копиям, и прогрессбар вёл бы себя по-разному в двух половинах
//! одной переписки, а объяснить эту разницу было бы нечем.
//!
//! # Почему троттлинг нужен
//!
//! Тело ответа портала читается кусками по нескольку килобайт. Без ограничения
//! файл на сотню мегабайт дал бы десятки тысяч кадров в broadcast-канал
//! ёмкостью [`crate::state::BROADCAST_CAPACITY`]: все подписчики немедленно
//! получили бы `Lagged` и ушли бы в `resync` (Р19). То есть ИНДИКАТОР
//! ПРОГРЕССА СЛОМАЛ БЫ REALTIME — самый обидный из возможных дефектов.
//!
//! Условий подавления два и они работают ПО «ИЛИ»: прошло [`MIN_INTERVAL`] ЛИБО
//! прибавилось [`MIN_DELTA`] байт. Медленную загрузку двигает время (иначе
//! полоса замирала бы на минуты), быструю сдерживают байты (иначе канал залился
//! бы за секунду).
//!
//! # Почему финальное событие проходит ВСЕГДА
//!
//! `is_final` игнорирует оба условия. Иначе последний кадр мог бы не дойти, и
//! полоса навсегда осталась бы на 97 % — визуально это «загрузка зависла», хотя
//! файл давно на месте. Ошибка дешёвая в написании и дорогая в диагностике,
//! поэтому она закрыта не дисциплиной, а веткой в [`Throttle::should_emit`].
//!
//! # Индетерминированная фаза
//!
//! Отправка файла в Bitrix24 одношаговая: `im.v2.File.upload` принимает base64
//! в теле JSON и не даёт никакой обратной связи о ходе (Р7). Процентов на этой
//! фазе не существует ВООБЩЕ, и рисовать их значило бы врать. Поэтому кадр
//! несёт `indeterminate: true`, а фронт показывает бегущую полосу.
//!
//! Отсюда двухфазность (Р8): фаза [`PHASE_CLIENT`] (браузер → devforge) знает
//! точные проценты, фаза [`PHASE_PORTAL`] (devforge → портал) — нет.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, ReadBuf};

use crate::state::B24State;
use crate::ws::protocol::B24ServerMsg;

/// Минимальный интервал между кадрами прогресса.
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Минимальный прирост байт между кадрами прогресса (256 КиБ).
pub const MIN_DELTA: u64 = 256 * 1024;

/// Фаза «браузер → devforge»: проценты точные (Р8).
pub const PHASE_CLIENT: &str = "client";

/// Фаза «devforge → портал»: процентов не существует (Р8).
pub const PHASE_PORTAL: &str = "portal";

/// Ограничитель частоты кадров прогресса.
///
/// Чистая логика без сети и без DTO: состояние — момент и объём последнего
/// отправленного кадра, решение принимает [`Throttle::should_emit`]. Ровно
/// поэтому правило проверяется юнит-тестами, а не наблюдением за сокетом.
#[derive(Debug, Clone)]
pub struct Throttle {
    /// Когда отправляли прошлый кадр.
    last_emit: Instant,
    /// Сколько байт было при прошлом кадре.
    last_bytes: u64,
    /// Минимальный интервал.
    min_interval: Duration,
    /// Минимальный прирост байт.
    min_delta: u64,
    /// Был ли хоть один кадр.
    ///
    /// Первый проходит всегда: UI обязан увидеть «загрузка началась», не
    /// дожидаясь ни ста миллисекунд, ни четверти мегабайта.
    started: bool,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttle {
    /// Ограничитель с боевыми параметрами (100 мс / 256 КиБ).
    pub fn new() -> Self {
        Self::with_limits(MIN_INTERVAL, MIN_DELTA)
    }

    /// Ограничитель с явными параметрами.
    ///
    /// Данные, а не константы: иначе тест «частые кадры подавляются» обязан был
    /// бы спать настоящие сто миллисекунд на каждый ассерт.
    pub fn with_limits(min_interval: Duration, min_delta: u64) -> Self {
        Self {
            last_emit: Instant::now(),
            last_bytes: 0,
            min_interval,
            min_delta,
            started: false,
        }
    }

    /// Пора ли отправлять кадр о `current` переданных байтах.
    ///
    /// `is_final == true` ⇒ ВСЕГДА `true` (см. шапку модуля). В остальных
    /// случаях достаточно любого из двух условий.
    ///
    /// Метод МЕНЯЕТ состояние: `true` означает «кадр считается отправленным»,
    /// поэтому звать его нужно ровно один раз на решение.
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

    /// Запоминает отправленный кадр.
    fn mark(&mut self, current: u64) {
        self.last_emit = Instant::now();
        self.last_bytes = current;
        self.started = true;
    }

    /// Сколько байт было на прошлом отправленном кадре (диагностика и тесты).
    pub fn last_bytes(&self) -> u64 {
        self.last_bytes
    }

    /// Был ли хоть один кадр.
    pub fn started(&self) -> bool {
        self.started
    }
}

/// Обёртка над `AsyncRead`, считающая прочитанные байты.
///
/// Нужна ОТПРАВКЕ (Ф9): staging-файл читается потоком, и единственная точка,
/// где видно «сколько уже ушло», — сам ридер. Живёт рядом с [`Throttle`], чтобы
/// обе половины считали прогресс одинаково.
///
/// Прозрачность — контракт: обёртка не меняет, не буферизует и не
/// переупорядочивает данные (тест `progress_reader_is_byte_transparent`).
pub struct ProgressReader<R, F> {
    /// Вложенный ридер.
    inner: R,
    /// Сколько байт прочитано суммарно.
    read: u64,
    /// Колбэк «прочитано столько-то ВСЕГО» (не дельта: у дельты теряется
    /// последний кусок, если колбэк вызовут не на каждом чтении).
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

/// Отправитель кадров `download_progress` по ОДНОМУ вложению (Ф8).
///
/// Собственный [`Throttle`] на отчётчик, а не общий на процесс: две
/// параллельные загрузки не должны глушить друг друга. Ключ — тот же
/// `<dialog>/<message>/<index>`, которым карточка адресуется во фронте; без
/// индекса прогресс второго файла сообщения прилетал бы в карточку первого.
pub struct DownloadReporter {
    /// Куда вещать.
    state: Arc<B24State>,
    /// Ключ карточки (`<dialog>/<message>/<index>`).
    key: String,
    /// Ограничитель частоты.
    throttle: Throttle,
}

impl DownloadReporter {
    /// Новый отчётчик по готовому ключу.
    pub fn new(state: Arc<B24State>, key: impl Into<String>) -> Self {
        Self {
            state,
            key: key.into(),
            throttle: Throttle::new(),
        }
    }

    /// Отчётчик по ключу кэша — он же адрес карточки во фронте.
    pub fn for_key(state: Arc<B24State>, key: &crate::media_cache::CacheKey) -> Self {
        Self::new(state, key.id())
    }

    /// Промежуточный отчёт (проходит троттлинг).
    pub fn report(&mut self, received: u64, total: Option<u64>) {
        self.emit(received, total, false);
    }

    /// Финальный отчёт — уходит ВСЕГДА.
    pub fn finish(&mut self, received: u64, total: Option<u64>) {
        self.emit(received, total, true);
    }

    fn emit(&mut self, received: u64, total: Option<u64>, is_final: bool) {
        if !self.throttle.should_emit(received, is_final) {
            return;
        }
        self.state.broadcast_event(B24ServerMsg::DownloadProgress {
            key: self.key.clone(),
            received,
            total,
        });
    }
}

/// Отправитель кадров `upload_progress` по ОДНОЙ загрузке (Ф9, Р8).
///
/// Двухфазный: [`PHASE_CLIENT`] считает точные проценты приёма файла от
/// браузера, [`PHASE_PORTAL`] отмечает заливку в портал, где процентов не
/// существует вовсе (одношаговая base64-отправка обратной связи не даёт).
/// Финальный кадр каждой фазы проходит мимо троттлинга.
pub struct UploadReporter {
    /// Куда вещать.
    state: Arc<B24State>,
    /// Идентификатор upload-сессии — адрес карточки во фронте.
    upload_id: String,
    /// Полный размер файла.
    total: u64,
    /// Текущая фаза.
    phase: &'static str,
    /// Ограничитель частоты.
    throttle: Throttle,
}

impl UploadReporter {
    /// Новый отчётчик фазы `client`.
    pub fn new(state: Arc<B24State>, upload_id: impl Into<String>, total: u64) -> Self {
        Self {
            state,
            upload_id: upload_id.into(),
            total,
            phase: PHASE_CLIENT,
            throttle: Throttle::new(),
        }
    }

    /// Текущая фаза (диагностика и тесты).
    pub fn phase(&self) -> &'static str {
        self.phase
    }

    /// Промежуточный отчёт фазы `client`.
    pub fn report(&mut self, sent: u64) {
        self.emit(sent, false);
    }

    /// Финал фазы `client` — уходит всегда.
    pub fn finish_client(&mut self, sent: u64) {
        self.emit(sent, true);
    }

    /// Переводит отчётчик в фазу `portal` и сразу шлёт индетерминированный кадр.
    ///
    /// Троттлинг здесь сбрасывается намеренно: смена фазы — это отдельное
    /// событие интерфейса («файл принят, отправляем в портал»), и подавить его
    /// из-за того, что сто миллисекунд назад ушёл кадр приёма, значило бы
    /// оставить полосу в состоянии предыдущей фазы.
    pub fn begin_portal(&mut self) {
        self.phase = PHASE_PORTAL;
        self.throttle = Throttle::new();
        self.emit(self.total, false);
    }

    /// Финал целиком — уходит всегда, независимо от фазы.
    pub fn finish(&mut self, sent: u64) {
        self.emit(sent, true);
    }

    fn emit(&mut self, sent: u64, is_final: bool) {
        if !self.throttle.should_emit(sent, is_final) {
            return;
        }
        self.state.broadcast_event(B24ServerMsg::UploadProgress {
            upload_id: self.upload_id.clone(),
            phase: self.phase.to_string(),
            sent,
            total: self.total,
            // Процентов у заливки в портал нет вовсе — фронт рисует бегущую
            // полосу вместо числа.
            indeterminate: self.phase == PHASE_PORTAL,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BitrixConfig;

    fn state() -> (tempfile::TempDir, Arc<B24State>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        (dir, B24State::new(cfg))
    }

    /// Первый кадр проходит всегда: «загрузка началась» не должна ждать
    /// ни таймера, ни четверти мегабайта.
    #[test]
    fn the_first_frame_always_passes() {
        let mut t = Throttle::new();
        assert!(t.should_emit(0, false));
        assert!(t.started());
    }

    /// Частые мелкие кадры подавляются обоими условиями сразу.
    #[test]
    fn frequent_small_frames_are_suppressed() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), MIN_DELTA);
        assert!(t.should_emit(0, false), "первый проходит");
        for n in 1..=100u64 {
            assert!(
                !t.should_emit(n * 1024, false),
                "кадр на {n} КиБ обязан быть подавлен: ни времени, ни дельты не набралось"
            );
        }
        assert_eq!(t.last_bytes(), 0, "состояние обновляется только на кадре");
    }

    /// Достаточно ЛЮБОГО из условий: набежала дельта — кадр проходит, даже если
    /// интервал не истёк.
    #[test]
    fn a_large_delta_passes_before_the_interval() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), 1024);
        assert!(t.should_emit(0, false));
        assert!(!t.should_emit(1023, false), "дельта не набралась");
        assert!(t.should_emit(1024, false), "дельта набралась ровно");
        assert_eq!(t.last_bytes(), 1024);
    }

    /// Второе условие — время: медленная загрузка тоже двигает полосу.
    #[test]
    fn an_elapsed_interval_passes_without_any_delta() {
        let mut t = Throttle::with_limits(Duration::from_millis(0), u64::MAX);
        assert!(t.should_emit(0, false));
        assert!(t.should_emit(0, false), "нулевой интервал истёк всегда");
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ: финальный кадр проходит мимо обоих условий, иначе
    /// полоса навсегда замерла бы на 97 %.
    #[test]
    fn the_final_frame_bypasses_the_throttle() {
        let mut t = Throttle::with_limits(Duration::from_secs(3600), u64::MAX);
        assert!(t.should_emit(0, false));
        assert!(!t.should_emit(1, false));
        assert!(t.should_emit(2, true), "финал обязан пройти");
        assert_eq!(t.last_bytes(), 2);
        // И повторный финал тоже: воркер может честно позвать finish дважды.
        assert!(t.should_emit(2, true));
    }

    /// Отчётчик скачивания шлёт кадр с ключом карточки.
    #[tokio::test]
    async fn the_download_reporter_broadcasts_the_card_key() {
        let (_d, st) = state();
        let mut rx = st.subscribe();
        let mut r = DownloadReporter::new(st.clone(), "chat27/113/0");
        r.report(0, Some(500));
        r.finish(500, Some(500));

        let first = rx.try_recv().unwrap();
        assert_eq!(
            first,
            B24ServerMsg::DownloadProgress {
                key: "chat27/113/0".into(),
                received: 0,
                total: Some(500),
            }
        );
        let last = rx.try_recv().unwrap();
        assert_eq!(
            last,
            B24ServerMsg::DownloadProgress {
                key: "chat27/113/0".into(),
                received: 500,
                total: Some(500),
            }
        );
        assert!(rx.try_recv().is_err(), "лишних кадров быть не должно");
    }

    /// Неизвестный размер уезжает как `total: null`, а не как ноль: ноль фронт
    /// прочитал бы как «файл пустой».
    #[tokio::test]
    async fn an_unknown_total_is_null_not_zero() {
        let (_d, st) = state();
        let mut rx = st.subscribe();
        DownloadReporter::new(st.clone(), "chat27/113/1").report(10, None);
        match rx.try_recv().unwrap() {
            B24ServerMsg::DownloadProgress { total, .. } => assert_eq!(total, None),
            other => panic!("не тот кадр: {other:?}"),
        }
    }

    /// Отправка двухфазная, и вторая фаза индетерминированна (Р8).
    #[tokio::test]
    async fn the_upload_reporter_switches_to_an_indeterminate_portal_phase() {
        let (_d, st) = state();
        let mut rx = st.subscribe();
        let mut r = UploadReporter::new(st.clone(), "u-1", 4096);
        assert_eq!(r.phase(), PHASE_CLIENT);
        r.report(0);
        r.finish_client(4096);
        r.begin_portal();
        assert_eq!(r.phase(), PHASE_PORTAL);
        r.finish(4096);

        let frames: Vec<B24ServerMsg> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(frames.len(), 4, "две фазы по два кадра: {frames:?}");
        for (i, frame) in frames.iter().enumerate() {
            let B24ServerMsg::UploadProgress {
                phase,
                indeterminate,
                total,
                ..
            } = frame
            else {
                panic!("не тот кадр: {frame:?}");
            };
            assert_eq!(*total, 4096);
            if i < 2 {
                assert_eq!(phase, PHASE_CLIENT);
                assert!(!indeterminate, "у приёма от браузера проценты точные");
            } else {
                assert_eq!(phase, PHASE_PORTAL);
                assert!(
                    indeterminate,
                    "одношаговая base64-заливка процентов не даёт (Р7/Р8)"
                );
            }
        }
    }

    /// Обёртка ридера обязана быть байт-прозрачной и считать ИТОГ, а не дельту.
    #[tokio::test]
    async fn progress_reader_is_byte_transparent() {
        use tokio::io::AsyncReadExt;

        let payload: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let sink = seen.clone();
        let mut reader = ProgressReader::new(std::io::Cursor::new(payload.clone()), move |n| {
            sink.lock().unwrap().push(n);
        });

        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, payload, "байты обязаны дойти без изменений");
        assert_eq!(reader.bytes_read(), payload.len() as u64);

        let seen = seen.lock().unwrap().clone();
        assert!(!seen.is_empty());
        assert_eq!(*seen.last().unwrap(), payload.len() as u64);
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "колбэк получает НАРАСТАЮЩИЙ итог: {seen:?}"
        );
    }
}
