//! Превентивное соблюдение лимита запросов VK — token bucket с инъекцией
//! часов.
//!
//! Подсистемы такого класса у плагина Telegram нет вовсе, и это не упущение, а
//! разница платформ: `FLOOD_WAIT` у Telegram узнаётся ПОСТ-ФАКТУМ (сервер сам
//! говорит, сколько ждать), а лимит VK — 3 запроса в секунду на
//! пользовательский токен — известен ЗАРАНЕЕ. Известный лимит дешевле
//! соблюдать, чем ловить: код 6 стоит одного запроса и одной секунды, а
//! регулярное его получение переходит в код 9 (flood control), из которого
//! выходить уже долго.
//!
//! # Почему token bucket
//!
//! - **Залп проходит мгновенно.** Открытие вкладки — это три запроса подряд
//!   (`getConversations` + `users.get` + `groups.getById`). Bucket пропускает
//!   их без задержки, а «спать 334 мс между вызовами» растянуло бы открытие
//!   вкладки на секунду впустую.
//! - **Непрерывное пополнение — ближайшая аппроксимация скользящей секунды**,
//!   которой VK меряет лимит.
//! - **Фиксированное окно даёт пограничный залп.** Три запроса в конце окна
//!   плюс три в начале следующего — шесть за 20 мс, то есть гарантированный
//!   код 6. Именно это ловит тест `throttle_spaces_calls_evenly` в
//!   `tests/vk_stub.rs`.
//!
//! # Честная оговорка про «не больше 3 в секунду»
//!
//! Token bucket с ёмкостью `C` и пополнением `r` формально допускает
//! `C + r·W` запросов в окне длины `W`: полный бак (3) плюс то, что натекло за
//! секунду (3). «Не больше трёх в любую скользящую секунду» и «залп из трёх
//! проходит мгновенно» — взаимоисключающие требования, и выбран второй
//! вариант: стартовый залп ограничен ёмкостью бака, а в УСТАНОВИВШЕМСЯ режиме
//! (бак исчерпан) интервал между вызовами равен ровно `1/rps`, то есть
//! долгоживущая сессия ровно 3 rps и не превышает. Ровно это и проверяется на
//! стенде.
//!
//! # Где живёт
//!
//! `Arc<Throttle>` внутри [`crate::api::VkClient`], ОДИН экземпляр на токен:
//! лимит — свойство токена, а не вызова, поэтому дробить его между
//! REST-хендлерами и LongPoll-воркером нельзя.
//!
//! # Что НЕ троттлится (отдельный критерий приёмки)
//!
//! - `a_check` на LongPoll-сервере — это не метод API, а ожидание на ЧУЖОМ
//!   хосте; 25-секундное удержание токена съело бы весь бюджет;
//! - `POST <upload_url>` — тоже не метод API;
//! - `GET <attachment_url>` при проксировании медиа — CDN VK, не API.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::Mutex;

/// Источник времени.
///
/// Существует ради тестов: чистое ядро [`Bucket::take`] проверяется на ручных
/// отметках времени, без единого `sleep`. Прод всегда использует
/// [`SystemClock`].
pub trait Clock: Send + Sync {
    /// Текущий момент.
    fn now(&self) -> Instant;
}

/// Боевые часы — [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// ЧИСТОЕ ЯДРО троттлера: ведро токенов с непрерывным пополнением.
///
/// Ни асинхронности, ни блокировок, ни `Instant::now()` внутри — всё время
/// приходит параметром. Благодаря этому вся арифметика лимита проверяется
/// мгновенными тестами, а не прогоном на секунды.
#[derive(Debug, Clone)]
pub struct Bucket {
    /// Сколько запросов можно сделать прямо сейчас (дробное — пополнение
    /// непрерывное).
    tokens: f64,
    /// Потолок накопления. Равен `rps`: столько запросов подряд разрешено
    /// после простоя.
    capacity: f64,
    /// Скорость пополнения, токенов в секунду.
    refill_per_sec: f64,
    /// Момент последнего пересчёта.
    last: Instant,
}

/// Сколько токенов в ведре на старте процесса (см. [`Bucket::cold`]).
///
/// Единица, а не ноль: первый запрос ждать не должен — бюджет VK за
/// предыдущую секунду мог быть и не тронут (штатный запуск после паузы), и
/// задерживать проверку токена на треть секунды при каждом старте незачем.
/// Ограничивается ровно ЗАЛП, а не первый вызов.
pub const COLD_START_TOKENS: f64 = 1.0;

impl Bucket {
    /// Новое ведро, ПОЛНОЕ на момент `now`.
    ///
    /// Полное, а не пустое: плагин только что стартовал, бюджет запросов за
    /// предыдущую секунду не израсходован, и заставлять первый же запрос
    /// ждать было бы неправдой про лимит.
    ///
    /// `rps` меньше или равный нулю трактуется как «минимально возможный»
    /// (0.001 запроса в секунду): паниковать в троттлере из-за кривого
    /// конфига нельзя, а нулевая скорость пополнения дала бы вечное ожидание.
    /// Штатно значение уже зажато [`crate::config::VkConfig::validate`].
    ///
    /// Ёмкость — `max(rps, 1)`, а не просто `rps`. При `rps < 1` ведро
    /// ёмкостью `rps` не смогло бы вместить ЦЕЛЫЙ токен, и ни один запрос не
    /// прошёл бы никогда — вместо «медленно» получилось бы «никогда».
    pub fn new(rps: f64, now: Instant) -> Self {
        let rate = if rps.is_finite() && rps > 0.0 {
            rps
        } else {
            0.001
        };
        let capacity = rate.max(1.0);
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec: rate,
            last: now,
        }
    }

    /// Ведро для ХОЛОДНОГО СТАРТА процесса: с [`COLD_START_TOKENS`] токеном
    /// вместо полного.
    ///
    /// Ведро живёт в памяти процесса, а бюджет запросов VK — на стороне VK и
    /// привязан к токену. Перезапуск `devforge` обнуляет только первое.
    /// Полное ведро на старте означает буквально: «предыдущая секунда была
    /// пустой» — а после перезапуска это неправда, предыдущая секунда была
    /// занята запросами ПРЕДЫДУЩЕГО процесса. Дальше происходит то, от чего
    /// весь троттлер и написан: залп из трёх (`users.get` +
    /// `messages.getConversations` + `messages.getLongPollServer`) ложится
    /// поверх чужого хвоста, VK отвечает кодом 6, повторы шлют ТОТ ЖЕ запрос
    /// ещё дважды — и однотипные повторы эскалируют до кода 9 (flood
    /// control), из которого выходить уже долго.
    ///
    /// Цена — первые вызовы после запуска процесса разносятся на `1/rps`
    /// (при 3 rps это 333 мс на вызов, то есть ~0.7 с на весь стартовый
    /// залп). УСТАНОВИВШИЙСЯ режим не меняется вовсе: через секунду работы
    /// ведро полное, и залп при открытии вкладки снова проходит мгновенно.
    pub fn cold(rps: f64, now: Instant) -> Self {
        let mut bucket = Self::new(rps, now);
        bucket.tokens = bucket.tokens.min(COLD_START_TOKENS);
        bucket
    }

    /// Попытка занять один токен на момент `now`.
    ///
    /// - `Ok(())` — можно идти в сеть, токен списан;
    /// - `Err(d)` — токенов нет, надо подождать `d` (ровно столько, сколько
    ///   натекает недостающая доля).
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

    /// Сколько токенов в ведре прямо сейчас (для тестов и диагностики).
    pub fn tokens(&self) -> f64 {
        self.tokens
    }

    /// Потолок накопления.
    pub fn capacity(&self) -> f64 {
        self.capacity
    }
}

/// Счётчики ожиданий — уходят в `GET /api/vk/status` и в лог.
///
/// По ним видно, насколько плагин упирается в лимит: растущий `total_wait_ms`
/// при спокойной работе означает, что `rate_limit_rps` в конфиге завышен и
/// код 6 не за горами.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ThrottleStats {
    /// Сколько раз вызов уходил в ожидание.
    pub total_waits: u64,
    /// Суммарное время ожидания, миллисекунды.
    pub total_wait_ms: u64,
}

/// Внутренность троттлера под одним lock'ом.
///
/// Ведро и счётчики держатся вместе намеренно: иначе статистика могла бы
/// разъехаться с реальными списаниями под конкуренцией, а второй мьютекс —
/// это второй порядок захвата и потенциальный deadlock на ровном месте.
#[derive(Debug)]
struct Inner {
    bucket: Bucket,
    stats: ThrottleStats,
}

/// Асинхронная обёртка над [`Bucket`]: ждёт, пока лимит позволит запрос.
pub struct Throttle {
    inner: Mutex<Inner>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Throttle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Arc<dyn Clock>` не Debug; печатать содержимое под lock'ом из
        // `fmt` нельзя (он синхронный) — поэтому терпимая заглушка.
        f.write_str("Throttle { .. }")
    }
}

impl Throttle {
    /// Троттлер на боевых часах.
    pub fn new(rps: f64) -> Self {
        Self::with_clock(rps, Arc::new(SystemClock))
    }

    /// Троттлер на заданных часах (тесты).
    pub fn with_clock(rps: f64, clock: Arc<dyn Clock>) -> Self {
        let now = clock.now();
        Self {
            inner: Mutex::new(Inner {
                bucket: Bucket::new(rps, now),
                stats: ThrottleStats::default(),
            }),
            clock,
        }
    }

    /// Троттлер для ХОЛОДНОГО СТАРТА процесса — боевой конструктор
    /// [`crate::api::VkClient`]. Обоснование целиком в [`Bucket::cold`].
    pub fn cold(rps: f64) -> Self {
        Self::cold_with_clock(rps, Arc::new(SystemClock))
    }

    /// [`Throttle::cold`] на заданных часах (тесты).
    pub fn cold_with_clock(rps: f64, clock: Arc<dyn Clock>) -> Self {
        let now = clock.now();
        Self {
            inner: Mutex::new(Inner {
                bucket: Bucket::cold(rps, now),
                stats: ThrottleStats::default(),
            }),
            clock,
        }
    }

    /// Ждёт, пока лимит позволит следующий запрос, и списывает токен.
    ///
    /// Lock УДЕРЖИВАЕТСЯ ЧЕРЕЗ `sleep` намеренно. `tokio::sync::Mutex`
    /// справедлив (FIFO), поэтому очередь ожидающих обслуживается в порядке
    /// прихода, а не случайно; отпусти мы lock на время сна, все ожидающие
    /// проснулись бы одновременно и устроили thundering herd, из которого
    /// кто-то мог бы не выйти вовсе. Цена — сериализация вызовов, но она и
    /// есть смысл лимита «3 запроса в секунду на токен».
    pub async fn acquire(&self) {
        let mut guard = self.inner.lock().await;
        loop {
            match guard.bucket.take(self.clock.now()) {
                Ok(()) => return,
                Err(wait) => {
                    guard.stats.total_waits += 1;
                    guard.stats.total_wait_ms += wait.as_millis() as u64;
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Снимок счётчиков.
    pub async fn stats(&self) -> ThrottleStats {
        self.inner.lock().await.stats
    }

    /// Сколько токенов доступно прямо сейчас (диагностика и тесты).
    pub async fn available(&self) -> f64 {
        let mut guard = self.inner.lock().await;
        let now = self.clock.now();
        // Пересчёт без списания: `take` + возврат токена дал бы ту же цифру,
        // но с побочным эффектом на `last`.
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// Ручные часы: время двигается только явным [`TestClock::advance`].
    struct TestClock {
        base: Instant,
        offset_ns: AtomicU64,
    }

    impl TestClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset_ns: AtomicU64::new(0),
            })
        }

        fn advance(&self, d: Duration) {
            self.offset_ns
                .fetch_add(d.as_nanos() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_nanos(self.offset_ns.load(Ordering::SeqCst))
        }
    }

    // -----------------------------------------------------------------
    // ГЕЙТ ФАЗЫ: чистое ядро на инъектированных часах, БЕЗ ожидания.
    // -----------------------------------------------------------------

    #[test]
    fn bucket_take_on_injected_clock() {
        let t0 = Instant::now();
        let mut b = Bucket::new(3.0, t0);

        // Три подряд на ОДНОМ И ТОМ ЖЕ now — это залп открытия вкладки.
        for i in 0..3 {
            assert!(b.take(t0).is_ok(), "залповый запрос {i} обязан пройти");
        }

        // Четвёртый — ждать треть секунды.
        let wait = b.take(t0).expect_err("четвёртый запрос обязан ждать");
        assert!(
            wait >= Duration::from_millis(330) && wait <= Duration::from_millis(340),
            "ожидание {wait:?} должно быть около 333 мс"
        );

        // Через 334 мс токен уже натёк.
        assert!(
            b.take(t0 + Duration::from_millis(334)).is_ok(),
            "через 334 мс запрос обязан пройти"
        );

        // Бюджет за ночь НЕ копится: после долгого простоя доступно ровно
        // capacity, а не «сколько натекло».
        let long = t0 + Duration::from_secs(10);
        assert!(b.take(long).is_ok());
        assert!(b.take(long).is_ok());
        assert!(b.take(long).is_ok());
        assert!(
            b.take(long).is_err(),
            "после простоя доступно ровно capacity=3, четвёртый обязан ждать"
        );
        assert!(b.tokens() <= b.capacity() + f64::EPSILON);
    }

    /// Холодный старт процесса НЕ разрешает залп: бюджет VK привязан к
    /// токену и переживает перезапуск `devforge`, а ведро — нет.
    #[test]
    fn cold_bucket_does_not_allow_a_burst() {
        let t0 = Instant::now();
        let mut b = Bucket::cold(3.0, t0);

        // Первый вызов (проверка токена) идёт сразу — его никто не задерживает.
        assert!(b.take(t0).is_ok(), "первый запрос после старта обязан пройти");
        // А вот второго в ту же миллисекунду уже нет: залп разнесён.
        let wait = b.take(t0).expect_err("второй запрос стартового залпа обязан ждать");
        assert!(
            wait >= Duration::from_millis(330) && wait <= Duration::from_millis(340),
            "ожидание {wait:?} должно быть около 333 мс"
        );

        // Через секунду работы ведро уже полное — установившийся режим не
        // изменился, залп открытия вкладки снова проходит мгновенно.
        let warm = t0 + Duration::from_secs(1);
        for i in 0..3 {
            assert!(b.take(warm).is_ok(), "прогретый залп {i} обязан пройти");
        }
        assert!(b.take(warm).is_err(), "четвёртый — уже за лимитом");
    }

    /// Ровно на границе (333.33 мс) токена ещё нет — округление в пользу
    /// лимита, а не в пользу лишнего запроса.
    #[test]
    fn bucket_rounds_in_favour_of_the_limit() {
        let t0 = Instant::now();
        let mut b = Bucket::new(3.0, t0);
        for _ in 0..3 {
            b.take(t0).unwrap();
        }
        assert!(
            b.take(t0 + Duration::from_millis(333)).is_err(),
            "333 мс — это ещё чуть меньше трети секунды"
        );
    }

    /// Время, идущее назад, не должно ни паниковать, ни выдавать лишние
    /// токены.
    #[test]
    fn bucket_survives_time_going_backwards() {
        let t0 = Instant::now() + Duration::from_secs(60);
        let mut b = Bucket::new(3.0, t0);
        for _ in 0..3 {
            b.take(t0).unwrap();
        }
        let past = t0 - Duration::from_secs(30);
        assert!(b.take(past).is_err(), "прошлое не наливает токенов");
        // …и `last` не откатился: через 334 мс от t0 всё ещё работает.
        assert!(b.take(t0 + Duration::from_millis(334)).is_ok());
    }

    /// Кривой конфиг не должен вешать плагин намертво.
    #[test]
    fn bucket_tolerates_a_nonsensical_rps() {
        let t0 = Instant::now();
        for rps in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let mut b = Bucket::new(rps, t0);
            // Первый запрос проходит (ведро полное), второй — ждёт конечное
            // время, а не «бесконечность» и не панику.
            let _ = b.take(t0);
            if let Err(d) = b.take(t0) {
                assert!(d.as_secs() < 10 * 365 * 24 * 3600, "rps={rps}: {d:?}");
            }
        }
    }

    /// Дробный `rps` (меньше запроса в секунду) обязан означать «медленно», а
    /// НЕ «никогда»: ёмкость поднимается до одного целого токена.
    #[test]
    fn bucket_handles_fractional_rps() {
        let t0 = Instant::now();
        let mut b = Bucket::new(0.5, t0);
        assert!(b.take(t0).is_ok(), "первый запрос обязан пройти");
        let wait = b.take(t0).expect_err("второй — ждать");
        assert!(
            wait >= Duration::from_millis(1900) && wait <= Duration::from_millis(2100),
            "при 0.5 rps ждать две секунды, а не вечность: {wait:?}"
        );
        assert!(b.take(t0 + Duration::from_secs(2)).is_ok());
    }

    // -----------------------------------------------------------------
    // Асинхронная обёртка.
    // -----------------------------------------------------------------

    /// `acquire` не ждёт, пока в ведре есть токены.
    #[tokio::test]
    async fn acquire_does_not_wait_while_tokens_are_left() {
        let throttle = Throttle::new(100.0);
        let started = Instant::now();
        for _ in 0..4 {
            throttle.acquire().await;
        }
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "четыре вызова при rps=100 обязаны пройти мгновенно: {:?}",
            started.elapsed()
        );
        assert_eq!(throttle.stats().await, ThrottleStats::default());
    }

    /// …и ждёт, когда токены кончились. rps=40 подобран так, чтобы тест
    /// упирался в лимит, но не спал дольше сотни миллисекунд.
    #[tokio::test]
    async fn acquire_waits_when_the_bucket_is_empty() {
        let throttle = Throttle::new(40.0);
        for _ in 0..40 {
            throttle.acquire().await;
        }
        let started = Instant::now();
        for _ in 0..4 {
            throttle.acquire().await;
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(70),
            "четыре вызова сверх ёмкости обязаны занять ~100 мс, заняли {elapsed:?}"
        );
        let stats = throttle.stats().await;
        // Верхняя граница с запасом: реальный `sleep` может проснуться на
        // доли миллисекунды раньше, чем натечёт целый токен, и вызов уйдёт на
        // второй короткий круг. Смысл проверки — «ждал каждый», а не точный
        // счётчик.
        assert!(
            (4..=6).contains(&stats.total_waits),
            "четыре вызова сверх ёмкости обязаны ждать: {stats:?}"
        );
        assert!(stats.total_wait_ms > 0, "{stats:?}");
    }

    /// На ручных часах ожидание видно без единого реального `sleep`:
    /// `available()` показывает ровно `capacity` после долгого простоя.
    #[tokio::test]
    async fn stats_and_available_on_a_manual_clock() {
        let clock = TestClock::new();
        let throttle = Throttle::with_clock(3.0, clock.clone());

        for _ in 0..3 {
            throttle.acquire().await;
        }
        assert!(throttle.available().await < 1.0, "ведро исчерпано");

        clock.advance(Duration::from_secs(10));
        let available = throttle.available().await;
        assert!(
            (available - 3.0).abs() < 1e-9,
            "после простоя ровно capacity, а не накопленный за ночь бюджет: {available}"
        );

        // Ни одного ожидания не понадобилось.
        assert_eq!(throttle.stats().await.total_waits, 0);
    }

    /// Параллельные вызовы делят ОДИН бюджет: это и есть «один Throttle на
    /// токен». Тест сломается, если кто-то заведёт по троттлеру на вызов.
    #[tokio::test]
    async fn concurrent_callers_share_one_budget() {
        let throttle = Arc::new(Throttle::new(50.0));
        let mut set = Vec::new();
        for _ in 0..60 {
            let t = throttle.clone();
            set.push(tokio::spawn(async move { t.acquire().await }));
        }
        for h in set {
            h.await.unwrap();
        }
        let stats = throttle.stats().await;
        assert!(
            (10..=13).contains(&stats.total_waits),
            "60 вызовов при ёмкости 50 — около 10 ожиданий на ОБЩЕМ бюджете; \
             отдельный троттлер на вызов дал бы 0: {stats:?}"
        );
    }
}
