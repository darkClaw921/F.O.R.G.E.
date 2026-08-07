//! ДВА независимых лимита Bitrix24 в одном месте.
//!
//! Портал ограничивает работу по двум осям сразу, и это не одна настройка с
//! двумя порогами, а два разных механизма с разной реакцией:
//!
//! | | 1. Интенсивность | 2. Ресурсоёмкость (`operating`) |
//! |---|---|---|
//! | На что считается | портал + IP | **метод + вебхук** |
//! | Модель | leaky bucket: 2 req/s, ёмкость 50 | окно из 10 минутных корзин, ~420–480 с |
//! | Как выглядит превышение | HTTP 503 `QUERY_LIMIT_EXCEEDED`, а живьём — **зависание или обрыв TCP без JSON** | HTTP 429 `OPERATION_TIME_LIMIT` |
//! | Что блокируется | всё | **только этот метод**, остальные работают |
//! | Реакция | превентивный token bucket + один повтор | пауза метода до `operating_reset_at` |
//! | Источник данных | известен заранее | `time.operating` / `time.operating_reset_at` КАЖДОГО успешного ответа |
//!
//! Заголовков `X-RateLimit-*` и `Retry-After` портал НЕ присылает — других
//! источников, кроме тела ответа, не существует.
//!
//! # Почему token bucket, а не «спать 500 мс между вызовами»
//!
//! - **Залп проходит мгновенно.** Холодный старт панели — это `batch` +
//!   `im.counters.get` + добор имён; равномерная задержка растянула бы открытие
//!   вкладки на секунды впустую.
//! - **Непрерывное пополнение — ближайшая аппроксимация скользящей секунды**,
//!   которой портал меряет интенсивность.
//! - **Фиксированное окно даёт пограничный залп**: пять запросов в конце окна
//!   плюс пять в начале следующего — это десять за 20 мс, то есть
//!   гарантированный 503.
//!
//! Ёмкость нашего ведра — 5, а не портальные 50: девять десятых бюджета
//! остаются живому пользователю портала, который в это же время работает в
//! браузере. Значение в конфиге ([`crate::config::BitrixConfig::burst`]).
//!
//! # Честная оговорка про «не больше 2 в секунду»
//!
//! Token bucket с ёмкостью `C` и пополнением `r` формально допускает `C + r·W`
//! запросов в окне длины `W`. «Не больше двух в любую скользящую секунду» и
//! «залп из пяти проходит мгновенно» — взаимоисключающие требования; выбрано
//! второе, потому что портальное ведро вмещает 50, и наш залп в 5 для него
//! незаметен. В УСТАНОВИВШЕМСЯ режиме (ведро исчерпано) интервал между
//! вызовами равен ровно `1/rps`.
//!
//! # Где живёт
//!
//! `Arc<Throttle>` внутри [`crate::api::B24Client`], ОДИН экземпляр на секрет:
//! лимит — свойство пары портал+вебхук, а не отдельного вызова. Дробить его
//! между REST-хендлерами и воркером событий нельзя — портал считает общий
//! бюджет и при его превышении просто перестаёт отвечать.
//!
//! # Что НЕ учитывается в `operating`
//!
//! Внешний `batch` стоит ОДИН хит лимита интенсивности и не расходует
//! `operating` — поэтому он и объявлен главным инструментом экономии (§4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

use super::error::TimeInfo;

/// Мягкий порог `operating` на метод, после которого пишется предупреждение.
///
/// Жёсткий лимит портала — 420 с в коробке и не раскрывается в облаке;
/// предупреждение на 80 % даёт время заметить прожорливый метод до того, как
/// он встанет на десять минут.
pub const OPERATING_SOFT_LIMIT_SECS: f64 = 420.0 * 0.8;

/// Сколько [`Throttle::acquire`] согласен ЖДАТЬ снятия паузы метода.
///
/// Пауза `operating` может длиться до десяти минут — держать столько
/// HTTP-запрос пользователя нельзя. Короткие паузы (хвост окна) дешевле
/// переждать, чем показать пользователю ошибку; всё, что длиннее, честнее
/// вернуть как `429 flood_wait` с обратным отсчётом.
pub const MAX_METHOD_PAUSE_WAIT: Duration = Duration::from_secs(5);

/// Источник времени.
///
/// Даёт и монотонные часы (для ведра), и календарные (для `operating_reset_at`,
/// который портал присылает абсолютным unix-временем). Существует ради тестов:
/// чистое ядро проверяется на ручных отметках, без единого `sleep`.
pub trait Clock: Send + Sync {
    /// Монотонный момент — для ведра.
    fn now(&self) -> Instant;
    /// Календарное unix-время, секунды — для сравнения с `operating_reset_at`.
    fn unix(&self) -> i64;
}

/// Боевые часы.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn unix(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

// ---------------------------------------------------------------------------
// Лимит 1: интенсивность.
// ---------------------------------------------------------------------------

/// ЧИСТОЕ ЯДРО первого лимита: ведро токенов с непрерывным пополнением.
///
/// Ни асинхронности, ни блокировок, ни `Instant::now()` внутри — всё время
/// приходит параметром, поэтому арифметика лимита проверяется мгновенными
/// тестами, а не прогоном на секунды.
#[derive(Debug, Clone)]
pub struct Bucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl Bucket {
    /// Новое ведро, ПОЛНОЕ на момент `now`.
    ///
    /// Полное, а не пустое: плагин только что стартовал, бюджет предыдущей
    /// секунды не израсходован, и заставлять первый запрос ждать было бы
    /// неправдой про лимит.
    ///
    /// Нефинитные и неположительные значения не паникуют, а трактуются как
    /// «минимально возможное»: кривой конфиг не должен вешать плагин. Ёмкость
    /// поднимается минимум до одного целого токена — ведро, не вмещающее
    /// токена, не пропустило бы ни одного запроса никогда.
    pub fn new(rps: f64, capacity: f64, now: Instant) -> Self {
        let rate = if rps.is_finite() && rps > 0.0 {
            rps
        } else {
            0.001
        };
        let cap = if capacity.is_finite() && capacity > 0.0 {
            capacity.max(1.0)
        } else {
            1.0
        };
        Self {
            tokens: cap,
            capacity: cap,
            refill_per_sec: rate,
            last: now,
        }
    }

    /// Попытка занять токен на момент `now`.
    ///
    /// - `Ok(())` — можно идти в сеть, токен списан;
    /// - `Err(d)` — токенов нет, надо подождать ровно `d`.
    ///
    /// Время, идущее назад (перевод часов, ручной clock в тесте), не ломает
    /// ведро: отрицательный интервал считается нулевым, и `last` не
    /// откатывается.
    pub fn take(&mut self, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            self.last = now;
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let need = 1.0 - self.tokens;
        Err(Duration::from_secs_f64(need / self.refill_per_sec))
    }

    /// Сколько токенов в ведре прямо сейчас (диагностика и тесты).
    pub fn tokens(&self) -> f64 {
        self.tokens
    }

    /// Потолок накопления.
    pub fn capacity(&self) -> f64 {
        self.capacity
    }
}

// ---------------------------------------------------------------------------
// Лимит 2: ресурсоёмкость метода.
// ---------------------------------------------------------------------------

/// Бюджет `operating` ОДНОГО метода.
///
/// Заполняется из `time` каждого успешного ответа. Портал уже отдаёт
/// `operating` накопленным по текущему окну, поэтому здесь хранится последнее
/// присланное значение, а не наша собственная сумма: своя арифметика окна
/// разъехалась бы с портальной на первом же перезапуске процесса.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct MethodBudget {
    /// Накопленное порталом `operating`-время метода в текущем окне, секунды.
    pub operating: f64,
    /// `operating_reset_at` последнего ответа (unix-время).
    pub reset_at: i64,
    /// До какого unix-времени метод стоит на паузе. `0` ⇒ паузы нет.
    pub paused_until: i64,
}

impl MethodBudget {
    /// Сколько ещё ждать снятия паузы на момент `now`.
    pub fn pause_left(&self, now: i64) -> Option<Duration> {
        if self.paused_until <= now {
            return None;
        }
        Some(Duration::from_secs((self.paused_until - now) as u64))
    }
}

/// Счётчики ожиданий — уходят в диагностику и в лог.
///
/// По ним видно, во что именно упирается плагин: растущий `bucket_wait_ms` при
/// спокойной работе означает завышенный `rate_limit_rps`, а ненулевой
/// `operating_pauses` — прожорливый метод, который пора звать через `batch`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ThrottleStats {
    /// Сколько раз вызов ждал токена интенсивности.
    pub bucket_waits: u64,
    /// Суммарное ожидание токена, миллисекунды.
    pub bucket_wait_ms: u64,
    /// Сколько раз вызов ждал снятия паузы метода.
    pub operating_waits: u64,
    /// Суммарное ожидание паузы метода, миллисекунды.
    pub operating_wait_ms: u64,
    /// Сколько раз метод ставился на паузу по `OPERATION_TIME_LIMIT`.
    pub operating_pauses: u64,
}

/// Ведро и счётчики под одним lock'ом.
///
/// Держатся вместе намеренно: иначе статистика могла бы разъехаться с
/// реальными списаниями под конкуренцией.
#[derive(Debug)]
struct Inner {
    bucket: Bucket,
    stats: ThrottleStats,
}

/// Оба лимита портала.
///
/// # Порядок захвата
///
/// [`Throttle::acquire`] сначала ждёт снятия паузы МЕТОДА и только потом берёт
/// токен интенсивности. Это не косметика: пауза `operating` длится минутами, и
/// ожидание её под общим lock'ом остановило бы ВСЕ методы — ровно то, чего
/// платформа не делает и чего не должны делать мы. Два мьютекса никогда не
/// удерживаются одновременно, поэтому порядка блокировок нет и взаимной
/// блокировки быть не может.
pub struct Throttle {
    inner: Mutex<Inner>,
    methods: Mutex<HashMap<String, MethodBudget>>,
    clock: Arc<dyn Clock>,
    max_pause_wait: Duration,
}

impl std::fmt::Debug for Throttle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Содержимое под lock'ом из синхронного `fmt` не прочитать.
        f.write_str("Throttle { .. }")
    }
}

impl Throttle {
    /// Троттлер на боевых часах.
    pub fn new(rps: f64, capacity: f64) -> Self {
        Self::with_clock(rps, capacity, Arc::new(SystemClock))
    }

    /// Троттлер на заданных часах (тесты).
    pub fn with_clock(rps: f64, capacity: f64, clock: Arc<dyn Clock>) -> Self {
        let now = clock.now();
        Self {
            inner: Mutex::new(Inner {
                bucket: Bucket::new(rps, capacity, now),
                stats: ThrottleStats::default(),
            }),
            methods: Mutex::new(HashMap::new()),
            clock,
            max_pause_wait: MAX_METHOD_PAUSE_WAIT,
        }
    }

    /// Меняет потолок ожидания паузы метода.
    ///
    /// Данные, а не константа: тесту нужно уметь и «переждать», и «получить
    /// отказ немедленно», не спя при этом минутами.
    #[must_use]
    pub fn with_max_pause_wait(mut self, wait: Duration) -> Self {
        self.max_pause_wait = wait;
        self
    }

    /// Ждёт, пока ОБА лимита позволят вызов `method`, и списывает токен.
    ///
    /// # Errors
    ///
    /// `Err(remaining)` — метод стоит на паузе по `OPERATION_TIME_LIMIT`
    /// дольше, чем [`Throttle::with_max_pause_wait`]. Вызывающий обязан
    /// вернуть `429 flood_wait` и НЕ ходить в сеть: запрос всё равно получил
    /// бы отказ, а портал засчитал бы его в тот же лимит.
    pub async fn acquire(&self, method: &str) -> Result<(), Duration> {
        // 1. Пауза МЕТОДА — вне общего lock'а, иначе встали бы все методы.
        loop {
            let left = {
                let guard = self.methods.lock().await;
                guard
                    .get(method)
                    .and_then(|b| b.pause_left(self.clock.unix()))
            };
            let Some(left) = left else { break };
            if left > self.max_pause_wait {
                return Err(left);
            }
            {
                let mut inner = self.inner.lock().await;
                inner.stats.operating_waits += 1;
                inner.stats.operating_wait_ms += left.as_millis() as u64;
            }
            tokio::time::sleep(left).await;
        }

        // 2. Общий лимит интенсивности.
        //
        // Lock УДЕРЖИВАЕТСЯ ЧЕРЕЗ `sleep` намеренно: `tokio::sync::Mutex`
        // справедлив (FIFO), поэтому очередь ожидающих обслуживается в
        // порядке прихода. Отпусти мы lock на время сна — все ожидающие
        // проснулись бы одновременно и устроили залп, ради предотвращения
        // которого ведро и существует.
        let mut guard = self.inner.lock().await;
        loop {
            match guard.bucket.take(self.clock.now()) {
                Ok(()) => return Ok(()),
                Err(wait) => {
                    guard.stats.bucket_waits += 1;
                    guard.stats.bucket_wait_ms += wait.as_millis() as u64;
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Запоминает `time` успешного ответа метода.
    ///
    /// Это ЕДИНСТВЕННЫЙ источник данных о втором лимите: заголовков портал не
    /// шлёт. Заодно снимается пауза, если окно уже сдвинулось.
    pub async fn note_success(&self, method: &str, time: &TimeInfo) {
        let now = self.clock.unix();
        let mut guard = self.methods.lock().await;
        let entry = guard.entry(method.to_string()).or_default();
        entry.operating = time.operating;
        if time.operating_reset_at > 0 {
            entry.reset_at = time.operating_reset_at;
        }
        if entry.paused_until <= now {
            entry.paused_until = 0;
        }
        if time.operating >= OPERATING_SOFT_LIMIT_SECS {
            // В лог уходят имя метода и число секунд — ни URL, ни параметров.
            tracing::warn!(
                target: "forge_bitrix",
                method,
                operating = time.operating,
                "forge-bitrix: the method is close to the portal operating-time limit; batch it or call it less often"
            );
        }
    }

    /// Ставит метод на паузу после `OPERATION_TIME_LIMIT`.
    ///
    /// `reset_at` — момент из тела ошибки, если портал его приложил. Иначе
    /// берётся `operating_reset_at` последнего УСПЕШНОГО ответа этого метода
    /// (он точный, а не угаданный), и лишь затем — [`fallback`].
    ///
    /// Возвращает момент, до которого метод заблокирован, — вызывающий кладёт
    /// его в ошибку, чтобы UI показал честный обратный отсчёт.
    ///
    /// [`fallback`]: super::error::RETRY_AFTER_OPERATING_FALLBACK
    pub async fn note_operating_limit(&self, method: &str, reset_at: Option<i64>) -> i64 {
        let now = self.clock.unix();
        let mut guard = self.methods.lock().await;
        let entry = guard.entry(method.to_string()).or_default();
        let until = match reset_at {
            Some(at) if at > now => at,
            _ if entry.reset_at > now => entry.reset_at,
            _ => now + super::error::RETRY_AFTER_OPERATING_FALLBACK as i64,
        };
        entry.paused_until = until;
        drop(guard);
        self.inner.lock().await.stats.operating_pauses += 1;
        tracing::warn!(
            target: "forge_bitrix",
            method,
            pause_secs = until - now,
            "forge-bitrix: the portal paused this method by its operating-time limit"
        );
        until
    }

    /// Бюджет метода (диагностика и тесты).
    pub async fn budget(&self, method: &str) -> Option<MethodBudget> {
        self.methods.lock().await.get(method).copied()
    }

    /// Стоит ли метод на паузе прямо сейчас и сколько её осталось.
    pub async fn pause_left(&self, method: &str) -> Option<Duration> {
        let now = self.clock.unix();
        self.methods
            .lock()
            .await
            .get(method)
            .and_then(|b| b.pause_left(now))
    }

    /// Снимок счётчиков.
    pub async fn stats(&self) -> ThrottleStats {
        self.inner.lock().await.stats
    }

    /// Сколько токенов интенсивности доступно прямо сейчас.
    pub async fn available(&self) -> f64 {
        let mut guard = self.inner.lock().await;
        let now = self.clock.now();
        // Пересчёт без списания: `take` дал бы ту же цифру, но с побочным
        // эффектом на `last`.
        let elapsed = now.saturating_duration_since(guard.bucket.last).as_secs_f64();
        let refill = guard.bucket.refill_per_sec;
        let cap = guard.bucket.capacity;
        if elapsed > 0.0 {
            guard.bucket.tokens = (guard.bucket.tokens + elapsed * refill).min(cap);
            guard.bucket.last = now;
        }
        guard.bucket.tokens
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

    use super::*;

    /// Ручные часы: время двигается только явным [`TestClock::advance`].
    struct TestClock {
        base: Instant,
        offset_ns: AtomicU64,
        unix: AtomicI64,
    }

    impl TestClock {
        fn new(unix: i64) -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ns: AtomicU64::new(0),
                unix: AtomicI64::new(unix),
            })
        }

        fn advance(&self, d: Duration) {
            self.offset_ns
                .fetch_add(d.as_nanos() as u64, Ordering::SeqCst);
            self.unix.fetch_add(d.as_secs() as i64, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_nanos(self.offset_ns.load(Ordering::SeqCst))
        }
        fn unix(&self) -> i64 {
            self.unix.load(Ordering::SeqCst)
        }
    }

    fn time_with(operating: f64, reset_at: i64) -> TimeInfo {
        TimeInfo {
            operating,
            operating_reset_at: reset_at,
            ..TimeInfo::default()
        }
    }

    // -----------------------------------------------------------------
    // Лимит 1: ведро на инъектированных часах, БЕЗ ожидания.
    // -----------------------------------------------------------------

    /// Боевая настройка: rps=2, ёмкость 5 — залп из пяти, потом ровно по
    /// полсекунды.
    #[test]
    fn bucket_lets_the_burst_through_then_spaces_calls_evenly() {
        let t0 = Instant::now();
        let mut b = Bucket::new(2.0, 5.0, t0);

        for i in 0..5 {
            assert!(b.take(t0).is_ok(), "залповый запрос {i} обязан пройти");
        }
        let wait = b.take(t0).expect_err("шестой запрос обязан ждать");
        assert!(
            wait >= Duration::from_millis(495) && wait <= Duration::from_millis(505),
            "при 2 rps ждать полсекунды, получено {wait:?}"
        );
        assert!(b.take(t0 + Duration::from_millis(500)).is_ok());

        // Бюджет за ночь НЕ копится: после простоя доступно ровно capacity.
        let long = t0 + Duration::from_secs(600);
        for _ in 0..5 {
            assert!(b.take(long).is_ok());
        }
        assert!(
            b.take(long).is_err(),
            "после простоя доступно ровно capacity=5"
        );
        assert!(b.tokens() <= b.capacity() + f64::EPSILON);
    }

    #[test]
    fn bucket_survives_time_going_backwards_and_a_nonsensical_config() {
        let t0 = Instant::now() + Duration::from_secs(60);
        let mut b = Bucket::new(2.0, 5.0, t0);
        for _ in 0..5 {
            b.take(t0).unwrap();
        }
        assert!(
            b.take(t0 - Duration::from_secs(30)).is_err(),
            "прошлое не наливает токенов"
        );
        assert!(b.take(t0 + Duration::from_millis(500)).is_ok());

        for (rps, cap) in [
            (0.0, 0.0),
            (-5.0, -1.0),
            (f64::NAN, f64::NAN),
            (f64::INFINITY, f64::INFINITY),
        ] {
            let mut b = Bucket::new(rps, cap, t0);
            let _ = b.take(t0);
            if let Err(d) = b.take(t0) {
                assert!(d.as_secs() < 10 * 365 * 24 * 3600, "rps={rps}: {d:?}");
            }
        }
    }

    // -----------------------------------------------------------------
    // Лимит 2: пауза метода.
    // -----------------------------------------------------------------

    /// Пауза ставится ТОЛЬКО на свой метод и снимается ровно в
    /// `operating_reset_at`.
    #[tokio::test]
    async fn the_operating_pause_is_per_method_and_ends_at_reset_at() {
        let clock = TestClock::new(1_000_000);
        let throttle = Throttle::with_clock(100.0, 100.0, clock.clone());

        let until = throttle
            .note_operating_limit("im.recent.get", Some(1_000_030))
            .await;
        assert_eq!(until, 1_000_030);

        assert_eq!(
            throttle.pause_left("im.recent.get").await,
            Some(Duration::from_secs(30))
        );
        assert!(
            throttle.pause_left("im.counters.get").await.is_none(),
            "пауза одного метода не смеет задевать другой"
        );

        clock.advance(Duration::from_secs(29));
        assert_eq!(
            throttle.pause_left("im.recent.get").await,
            Some(Duration::from_secs(1))
        );
        clock.advance(Duration::from_secs(1));
        assert!(
            throttle.pause_left("im.recent.get").await.is_none(),
            "ровно в reset_at метод обязан ожить"
        );
    }

    /// Момент разблокировки берётся из ошибки, из последнего успешного ответа
    /// или из фолбэка — именно в этом порядке.
    #[tokio::test]
    async fn the_pause_end_falls_back_from_the_error_to_the_last_success() {
        let clock = TestClock::new(1_000_000);
        let throttle = Throttle::with_clock(100.0, 100.0, clock.clone());

        // 1. Момент есть в теле ошибки — он и берётся.
        assert_eq!(
            throttle.note_operating_limit("a", Some(1_000_100)).await,
            1_000_100
        );

        // 2. Момента нет, но был успешный ответ с `operating_reset_at`.
        throttle.note_success("b", &time_with(12.5, 1_000_200)).await;
        assert_eq!(throttle.note_operating_limit("b", None).await, 1_000_200);

        // 3. Ни того, ни другого — минутная корзина окна.
        assert_eq!(
            throttle.note_operating_limit("c", None).await,
            1_000_000 + super::super::error::RETRY_AFTER_OPERATING_FALLBACK as i64
        );

        // 4. Просроченный момент из ошибки не «разблокирует» метод в прошлом.
        let until = throttle.note_operating_limit("d", Some(999_000)).await;
        assert!(until > 1_000_000, "{until}");
    }

    /// Бюджет наполняется из `time` каждого успешного ответа.
    #[tokio::test]
    async fn the_budget_is_filled_from_the_time_block() {
        let clock = TestClock::new(1_000_000);
        let throttle = Throttle::with_clock(100.0, 100.0, clock.clone());
        assert!(throttle.budget("im.recent.get").await.is_none());

        throttle
            .note_success("im.recent.get", &time_with(1.25, 1_000_600))
            .await;
        let b = throttle.budget("im.recent.get").await.unwrap();
        assert!((b.operating - 1.25).abs() < 1e-9);
        assert_eq!(b.reset_at, 1_000_600);
        assert_eq!(b.paused_until, 0);

        // Портал сам накапливает `operating` по окну — берётся его значение.
        throttle
            .note_success("im.recent.get", &time_with(2.50, 1_000_660))
            .await;
        let b = throttle.budget("im.recent.get").await.unwrap();
        assert!((b.operating - 2.50).abs() < 1e-9);
        assert_eq!(b.reset_at, 1_000_660);

        // Успешный ответ после истёкшей паузы снимает её.
        throttle.note_operating_limit("im.recent.get", Some(1_000_010)).await;
        clock.advance(Duration::from_secs(20));
        throttle
            .note_success("im.recent.get", &time_with(0.1, 1_000_700))
            .await;
        assert_eq!(throttle.budget("im.recent.get").await.unwrap().paused_until, 0);
    }

    /// Короткую паузу `acquire` пережидает, длинную — возвращает вызывающему,
    /// чтобы тот отдал `429` вместо заведомо отказного запроса.
    #[tokio::test]
    async fn acquire_waits_out_a_short_pause_and_refuses_a_long_one() {
        let throttle =
            Throttle::new(100.0, 100.0).with_max_pause_wait(Duration::from_millis(1));
        let now = chrono::Utc::now().timestamp();

        throttle.note_operating_limit("slow", Some(now + 300)).await;
        let left = throttle
            .acquire("slow")
            .await
            .expect_err("длинная пауза обязана вернуть отказ, а не спать пять минут");
        assert!(left >= Duration::from_secs(299), "{left:?}");

        // Другой метод не задет.
        throttle.acquire("fast").await.unwrap();

        let stats = throttle.stats().await;
        assert_eq!(stats.operating_pauses, 1);
        assert_eq!(stats.operating_waits, 0, "ожидания не было — был отказ");
    }

    // -----------------------------------------------------------------
    // Асинхронная обёртка.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn acquire_is_free_while_tokens_are_left() {
        let throttle = Throttle::new(100.0, 100.0);
        let started = Instant::now();
        for _ in 0..5 {
            throttle.acquire("im.counters.get").await.unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "пять вызовов при rps=100 обязаны пройти мгновенно: {:?}",
            started.elapsed()
        );
        assert_eq!(throttle.stats().await, ThrottleStats::default());
    }

    /// …и ждёт, когда токены кончились. rps=40 подобран так, чтобы тест
    /// упирался в лимит, но не спал дольше сотни миллисекунд.
    #[tokio::test]
    async fn acquire_waits_when_the_bucket_is_empty() {
        let throttle = Throttle::new(40.0, 40.0);
        for _ in 0..40 {
            throttle.acquire("m").await.unwrap();
        }
        let started = Instant::now();
        for _ in 0..4 {
            throttle.acquire("m").await.unwrap();
        }
        assert!(
            started.elapsed() >= Duration::from_millis(70),
            "четыре вызова сверх ёмкости обязаны занять ~100 мс, заняли {:?}",
            started.elapsed()
        );
        let stats = throttle.stats().await;
        assert!((4..=6).contains(&stats.bucket_waits), "{stats:?}");
        assert!(stats.bucket_wait_ms > 0, "{stats:?}");
    }

    /// РАЗНЫЕ методы делят ОДИН бюджет интенсивности: он считается на пару
    /// портал+IP, а не на метод. Тест сломается, если кто-то заведёт по ведру
    /// на метод.
    #[tokio::test]
    async fn different_methods_share_one_intensity_budget() {
        let throttle = Arc::new(Throttle::new(50.0, 50.0));
        let mut handles = Vec::new();
        for i in 0..60 {
            let t = throttle.clone();
            let method = format!("method.{}", i % 6);
            handles.push(tokio::spawn(async move { t.acquire(&method).await }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let stats = throttle.stats().await;
        assert!(
            (8..=13).contains(&stats.bucket_waits),
            "60 вызовов при ёмкости 50 — около 10 ожиданий на ОБЩЕМ бюджете; \
             отдельное ведро на метод дало бы 0: {stats:?}"
        );
    }

    #[tokio::test]
    async fn available_shows_capacity_after_an_idle_period() {
        let clock = TestClock::new(1_000_000);
        let throttle = Throttle::with_clock(2.0, 5.0, clock.clone());
        for _ in 0..5 {
            throttle.acquire("m").await.unwrap();
        }
        assert!(throttle.available().await < 1.0, "ведро исчерпано");

        clock.advance(Duration::from_secs(600));
        let available = throttle.available().await;
        assert!(
            (available - 5.0).abs() < 1e-9,
            "после простоя ровно capacity, а не накопленный за ночь бюджет: {available}"
        );
        assert_eq!(throttle.stats().await.bucket_waits, 0);
    }
}
