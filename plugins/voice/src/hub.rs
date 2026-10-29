//! Реестр активных расшифровок.
//!
//! Сделан по образцу `forge-chat-analysis::hub` и решает те же четыре задачи:
//! дедуп, потолок параллелизма, отмена и таймаут. Ядро не знает ни одного
//! плагина — кадры уходят через [`crate::sink::VoiceSink`], а саму работу
//! (локальный whisper или облако) передают ЗАМЫКАНИЕМ в [`VoiceHub::start`].
//!
//! # ДЕДУП ПО КЛЮЧУ ВЛОЖЕНИЯ, А НЕ ПО ПОДПИСЧИКУ
//!
//! Голосовое лежит в общей беседе, и «расшифруй» на нём нажмут двое. Без
//! дедупа это два whisper'а на одном файле: вдвое дольше и вдвое дороже, а
//! результат один и тот же. Поэтому второй запрос ПОДКЛЮЧАЕТСЯ к идущему
//! run'у ([`StartOutcome::Joined`]) и получает те же кадры — они адресованы
//! ключом вложения, а не подписчиком.
//!
//! # ПОТОЛОК ОТКАЗЫВАЕТ СРАЗУ, А НЕ ЖДЁТ НА СЕМАФОРЕ
//!
//! Политика взята у `chat-analysis` дословно и повторена НАМЕРЕННО: молчаливое
//! ожидание в очереди означало бы открытую плашку без единого кадра и без
//! объяснения — человек видит «идёт расшифровка», а на деле его запрос стои́т
//! за чужим. [`crate::VoiceError::Busy`] несёт ЧИСЛО занятых слотов, чтобы
//! плагин мог сказать «идёт 3 расшифровки, попробуйте через минуту», а не
//! «занято».
//!
//! Потолок нужен не ради вежливости: локальный whisper грузит модель в память
//! GPU целиком, и три параллельных прогона большой модели — это отказ по
//! памяти, а не «втрое медленнее».
//!
//! # ТАЙМАУТ УБИВАЕТ ПРОЦЕСС, А НЕ ТОЛЬКО ЗАДАЧУ
//!
//! По истечении таймаута реестр СНАЧАЛА отменяет токен и ДОЖИДАЕТСЯ, пока
//! задание доиграет, и только потом отвечает ошибкой. Просто бросить задачу
//! было бы недостаточно: `tokio::process::Child` убивается на `Drop` только
//! если до него дошли руки, а зависший ffmpeg, оставшийся сиротой, держал бы
//! файл и слот до конца жизни devforge.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::error::VoiceError;
use crate::proc::ProcError;
use crate::sink::VoiceSink;
use crate::store::{Transcript, VoiceKey};

/// Сколько расшифровок идёт одновременно по умолчанию.
///
/// Одна. Локальный whisper держит модель в памяти GPU целиком, и второй
/// одновременный прогон большой модели — это не «вдвое медленнее», а отказ по
/// памяти. Облаку параллелизм тоже не даёт ничего: предел там по стене на
/// запрос, а не по числу соединений.
pub const DEFAULT_MAX_PARALLEL: usize = 1;

/// Потолок одной расшифровки по умолчанию.
///
/// Десять минут: голосовое длиннее пятнадцати минут не бывает ни в одном из
/// трёх мессенджеров, а всё, что дольше, означает зависший процесс.
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 600;

/// Сколько ждать, пока отменённое задание доиграет и убьёт свой процесс.
const CANCEL_GRACE: Duration = Duration::from_secs(10);

/// Чем кончился запрос на расшифровку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// Запущен новый run с этим идентификатором.
    Started(String),
    /// Расшифровка этого вложения УЖЕ идёт; внутри — её идентификатор.
    /// Второй процесс не запускался.
    Joined(String),
}

impl StartOutcome {
    /// Идентификатор run'а — новый или уже идущий.
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            Self::Started(id) | Self::Joined(id) => id,
        }
    }

    /// Запустился ли НОВЫЙ прогон.
    #[must_use]
    pub fn is_new(&self) -> bool {
        matches!(self, Self::Started(_))
    }
}

/// Всё, что нужно заданию: кому слать прогресс и когда остановиться.
#[derive(Clone)]
pub struct RunCtx {
    run_id: String,
    key: VoiceKey,
    cancel: CancellationToken,
    sink: Arc<dyn VoiceSink>,
}

impl RunCtx {
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn key(&self) -> &VoiceKey {
        &self.key
    }

    /// Токен отмены. Его ОБЯЗАНЫ передать в [`crate::proc`]: иначе ни отмена,
    /// ни таймаут не дойдут до внешнего процесса.
    #[must_use]
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Отправляет кадр прогресса.
    pub fn progress(&self, percent: u8) {
        self.sink.progress(&self.run_id, &self.key, percent.min(100));
    }
}

/// Один идущий прогон.
struct Active {
    run_id: String,
    cancel: CancellationToken,
}

/// Реестр расшифровок.
#[derive(Clone)]
pub struct VoiceHub {
    sink: Arc<dyn VoiceSink>,
    runs: Arc<Mutex<HashMap<VoiceKey, Active>>>,
    max_parallel: usize,
    run_timeout: Duration,
    /// Счётчик для идентификаторов run'ов.
    seq: Arc<std::sync::atomic::AtomicU64>,
}

impl VoiceHub {
    pub fn new(sink: Arc<dyn VoiceSink>, max_parallel: usize, run_timeout: Duration) -> Self {
        Self {
            sink,
            runs: Arc::new(Mutex::new(HashMap::new())),
            // Ноль слотов означал бы «расшифровка выключена навсегда», причём
            // молча: единица — минимум, при котором фича вообще существует.
            max_parallel: max_parallel.max(1),
            run_timeout: if run_timeout.is_zero() {
                Duration::from_secs(DEFAULT_RUN_TIMEOUT_SECS)
            } else {
                run_timeout
            },
            seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Сколько расшифровок идёт прямо сейчас.
    #[must_use]
    pub fn active(&self) -> usize {
        self.runs.lock().expect("реестр расшифровок отравлен").len()
    }

    /// Идентификатор идущей расшифровки этого вложения, если она идёт.
    #[must_use]
    pub fn running(&self, key: &VoiceKey) -> Option<String> {
        self.runs
            .lock()
            .expect("реестр расшифровок отравлен")
            .get(key)
            .map(|a| a.run_id.clone())
    }

    /// Запускает расшифровку вложения.
    ///
    /// `engine` уезжает в кадр `started` — человек имеет право знать, ушло ли
    /// его голосовое в чужое облако, ДО того, как оно туда ушло.
    ///
    /// `job` получает [`RunCtx`] и обязан передать его [`RunCtx::cancel`] во
    /// все запуски внешних процессов: иначе ни отмена, ни таймаут до них не
    /// дойдут.
    ///
    /// # Errors
    ///
    /// [`VoiceError::Busy`], если все слоты заняты ДРУГИМИ вложениями. Своё же
    /// вложение отказом не отвечает — оно подключается к идущему run'у.
    pub fn start<F, Fut>(
        &self,
        key: VoiceKey,
        engine: &str,
        job: F,
    ) -> Result<StartOutcome, VoiceError>
    where
        F: FnOnce(RunCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Transcript, VoiceError>> + Send + 'static,
    {
        // Дедуп и проверка потолка — ПОД ОДНИМ замком: между «посмотрел» и
        // «занял» не должно быть окна, иначе два одновременных запроса на один
        // ключ оба увидели бы пустой реестр и запустили по процессу.
        let cancel = CancellationToken::new();
        let run_id = {
            let mut runs = self.runs.lock().expect("реестр расшифровок отравлен");
            if let Some(active) = runs.get(&key) {
                return Ok(StartOutcome::Joined(active.run_id.clone()));
            }
            if runs.len() >= self.max_parallel {
                return Err(VoiceError::Busy(runs.len()));
            }
            let n = self
                .seq
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .wrapping_add(1);
            let run_id = format!("voice-{n}");
            runs.insert(
                key.clone(),
                Active {
                    run_id: run_id.clone(),
                    cancel: cancel.clone(),
                },
            );
            run_id
        };

        let ctx = RunCtx {
            run_id: run_id.clone(),
            key: key.clone(),
            cancel: cancel.clone(),
            sink: self.sink.clone(),
        };
        // Кадр `started` уходит СИНХРОННО, до возврата из `start`: иначе
        // плагин успел бы ответить на HTTP-запрос раньше, чем подписчик узнал
        // о начале, и полоса прогресса появилась бы уже на середине.
        self.sink.started(&run_id, &key, engine);

        let sink = self.sink.clone();
        let runs = self.runs.clone();
        let timeout = self.run_timeout;
        let task_run_id = run_id.clone();
        tokio::spawn(async move {
            let run_id = task_run_id;
            let fut = job(ctx);
            tokio::pin!(fut);

            let outcome: Result<Transcript, VoiceError> = tokio::select! {
                biased;
                res = &mut fut => res,
                _ = tokio::time::sleep(timeout) => {
                    // Сначала отменяем и ДОЖИДАЕМСЯ: задание обязано увидеть
                    // отмену и убить свой процесс. Бросить задачу здесь значило
                    // бы оставить сироту, держащую файл и слот.
                    cancel.cancel();
                    let _ = tokio::time::timeout(CANCEL_GRACE, &mut fut).await;
                    Err(VoiceError::Proc(ProcError::new(
                        "timeout",
                        format!("расшифровка не уложилась в {timeout:?} и снята"),
                        "",
                    )))
                }
            };

            // Слот освобождается ДО кадров: подписчик, увидевший `done`, имеет
            // право тут же запросить следующую расшифровку и не получить
            // «занято» от уже закончившегося run'а.
            runs.lock().expect("реестр расшифровок отравлен").remove(&key);

            match outcome {
                Ok(t) => sink.done(&run_id, &key, &t),
                Err(e) => sink.error(&run_id, &key, e.code(), &e.to_string()),
            }
        });

        Ok(StartOutcome::Started(run_id))
    }

    /// Отменяет расшифровку вложения. `false` — её и не было.
    ///
    /// Финальный кадр (`error` с кодом `cancelled`) шлёт сама задача, когда
    /// увидит отмену: реестр не знает, успел ли внешний процесс умереть, и
    /// объявлять run законченным раньше него было бы враньём.
    pub fn cancel(&self, key: &VoiceKey) -> bool {
        let active = self
            .runs
            .lock()
            .expect("реестр расшифровок отравлен")
            .get(key)
            .map(|a| a.cancel.clone());
        match active {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Отменяет ВСЕ идущие расшифровки — например, при остановке плагина.
    pub fn cancel_all(&self) {
        for a in self
            .runs
            .lock()
            .expect("реестр расшифровок отравлен")
            .values()
        {
            a.cancel.cancel();
        }
    }
}

impl std::fmt::Debug for VoiceHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceHub")
            .field("active", &self.active())
            .field("max_parallel", &self.max_parallel)
            .field("run_timeout", &self.run_timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Engine;

    #[derive(Default)]
    struct Recorder {
        frames: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn frames(&self) -> Vec<String> {
            self.frames.lock().unwrap().clone()
        }
        fn count(&self, prefix: &str) -> usize {
            self.frames()
                .iter()
                .filter(|f| f.starts_with(prefix))
                .count()
        }
    }

    impl VoiceSink for Recorder {
        fn started(&self, run_id: &str, key: &VoiceKey, engine: &str) {
            self.frames
                .lock()
                .unwrap()
                .push(format!("started {run_id} {} {engine}", key.message_id));
        }
        fn progress(&self, run_id: &str, _key: &VoiceKey, percent: u8) {
            self.frames
                .lock()
                .unwrap()
                .push(format!("progress {run_id} {percent}"));
        }
        fn error(&self, run_id: &str, _key: &VoiceKey, code: &str, _message: &str) {
            self.frames
                .lock()
                .unwrap()
                .push(format!("error {run_id} {code}"));
        }
        fn done(&self, run_id: &str, _key: &VoiceKey, t: &Transcript) {
            self.frames
                .lock()
                .unwrap()
                .push(format!("done {run_id} {}", t.text));
        }
    }

    fn hub(limit: usize, timeout_ms: u64) -> (Arc<Recorder>, VoiceHub) {
        let rec = Arc::new(Recorder::default());
        let hub = VoiceHub::new(rec.clone(), limit, Duration::from_millis(timeout_ms));
        (rec, hub)
    }

    /// Ждёт, пока реестр опустеет, но не дольше отведённого.
    async fn drain(hub: &VoiceHub) {
        for _ in 0..200 {
            if hub.active() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("реестр не опустел: {hub:?}");
    }

    #[tokio::test]
    async fn a_run_reports_started_progress_and_done() {
        let (rec, hub) = hub(2, 10_000);
        let key = VoiceKey::new("peer", "1", 0);
        let out = hub
            .start(key.clone(), "cloud", |ctx| async move {
                ctx.progress(10);
                ctx.progress(100);
                Ok(Transcript::new("расшифровано", Engine::Cloud))
            })
            .unwrap();
        assert!(out.is_new());
        drain(&hub).await;

        let f = rec.frames();
        assert_eq!(f[0], format!("started {} 1 cloud", out.run_id()));
        assert!(f.iter().any(|x| x.ends_with("progress voice-1 100") || x.contains("progress")));
        assert!(f.last().unwrap().contains("done"), "{f:?}");
        assert!(f.last().unwrap().contains("расшифровано"), "{f:?}");
        assert_eq!(hub.running(&key), None, "слот не освобождён");
    }

    /// **ДВА ОДНОВРЕМЕННЫХ ЗАПРОСА НА ОДИН КЛЮЧ ДАЮТ ОДИН RUN.**
    ///
    /// И это не «оптимизация»: второй whisper на том же файле — это вдвое
    /// дольше и вдвое дороже ради того же самого текста.
    #[tokio::test]
    async fn two_requests_for_one_key_share_a_single_run() {
        let (rec, hub) = hub(4, 10_000);
        let key = VoiceKey::new("peer", "7", 0);
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let counter = runs.clone();
        let first = hub
            .start(key.clone(), "local", move |_ctx| async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(Transcript::new("один текст", Engine::Local))
            })
            .unwrap();

        let counter = runs.clone();
        let second = hub
            .start(key.clone(), "local", move |_ctx| async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Transcript::new("второй процесс!", Engine::Local))
            })
            .unwrap();

        assert!(first.is_new());
        assert_eq!(second, StartOutcome::Joined(first.run_id().to_string()));
        assert_eq!(hub.active(), 1, "в реестре два run'а на одно вложение");

        drain(&hub).await;
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "запустилось два процесса на одном файле"
        );
        // Кадры пришли ОДИН раз и адресованы ключом — их видят оба подписчика.
        assert_eq!(rec.count("started"), 1, "{:?}", rec.frames());
        assert_eq!(rec.count("done"), 1, "{:?}", rec.frames());
        assert!(rec.frames().last().unwrap().contains("один текст"));
    }

    /// **ПОТОЛОК ОТКАЗЫВАЕТ СРАЗУ И НАЗЫВАЕТ ЧИСЛО ЗАНЯТЫХ СЛОТОВ.**
    #[tokio::test]
    async fn the_parallel_limit_refuses_at_once_and_names_the_count() {
        let (_rec, hub) = hub(1, 10_000);
        let busy = VoiceKey::new("peer", "1", 0);
        hub.start(busy.clone(), "local", |_c| async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            Ok(Transcript::new("долго", Engine::Local))
        })
        .unwrap();

        let started = std::time::Instant::now();
        let err = hub
            .start(VoiceKey::new("peer", "2", 0), "local", |_c| async move {
                Ok(Transcript::new("не должно случиться", Engine::Local))
            })
            .unwrap_err();
        assert_eq!(err, VoiceError::Busy(1));
        assert_eq!(err.code(), "voice_busy");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "отказ ждал на семафоре: {:?}",
            started.elapsed()
        );
        // Своё же вложение отказом не отвечает даже при занятом потолке.
        assert!(!hub.start(busy.clone(), "local", |_c| async move {
            Ok(Transcript::new("x", Engine::Local))
        })
        .unwrap()
        .is_new());

        drain(&hub).await;
        // Слот освободился — следующий запрос проходит.
        assert!(hub
            .start(VoiceKey::new("peer", "2", 0), "local", |_c| async move {
                Ok(Transcript::new("теперь можно", Engine::Local))
            })
            .unwrap()
            .is_new());
        drain(&hub).await;
    }

    /// **ОТМЕНА ЗАВЕРШАЕТ RUN, ШЛЁТ `cancelled` И ОСВОБОЖДАЕТ СЛОТ.**
    #[tokio::test]
    async fn cancelling_ends_the_run_and_frees_the_slot() {
        let (rec, hub) = hub(1, 30_000);
        let key = VoiceKey::new("peer", "1", 0);
        hub.start(key.clone(), "local", |ctx| async move {
            // Ровно так ведёт себя настоящее задание: оно смотрит на токен и
            // отдаёт отмену, убив свой процесс.
            ctx.cancel().cancelled().await;
            Err(VoiceError::Cancelled)
        })
        .unwrap();

        // Даём заданию доехать до ожидания токена.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(hub.cancel(&key), "нечего было отменять");
        drain(&hub).await;

        assert_eq!(rec.count("error"), 1, "{:?}", rec.frames());
        assert!(
            rec.frames().last().unwrap().ends_with("cancelled"),
            "{:?}",
            rec.frames()
        );
        assert_eq!(hub.active(), 0);
        // Отменять то, чего нет, — не ошибка и не паника.
        assert!(!hub.cancel(&key));
    }

    /// **ТАЙМАУТ СНИМАЕТ RUN И ДОЖИДАЕТСЯ, ПОКА ЗАДАНИЕ УБЬЁТ СВОЙ ПРОЦЕСС.**
    ///
    /// Сирот быть не должно: `tokio::process::Child` умирает на `Drop`, только
    /// если до него дошли руки.
    #[tokio::test]
    async fn the_timeout_kills_the_run_and_waits_for_the_process() {
        let (rec, hub) = hub(1, 200);
        let key = VoiceKey::new("peer", "1", 0);
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = killed.clone();

        let started = std::time::Instant::now();
        hub.start(key.clone(), "local", move |ctx| async move {
            // Задание «держит процесс» и убивает его по отмене.
            ctx.cancel().cancelled().await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(VoiceError::Cancelled)
        })
        .unwrap();
        drain(&hub).await;

        assert!(
            killed.load(std::sync::atomic::Ordering::SeqCst),
            "задание не увидело отмены — процесс остался сиротой"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(
            rec.frames().last().unwrap().ends_with("timeout"),
            "код отказа обязан быть таймаутом: {:?}",
            rec.frames()
        );
        assert_eq!(hub.active(), 0, "слот не освобождён после таймаута");
    }

    /// Отказ задания доезжает до подписчика СВОИМ кодом, а не общим.
    #[tokio::test]
    async fn a_failure_keeps_its_own_code() {
        let (rec, hub) = hub(1, 10_000);
        hub.start(VoiceKey::new("peer", "1", 0), "local", |_c| async move {
            Err(VoiceError::Proc(ProcError::new(
                "metal_unavailable",
                "GPU не подтверждён",
                "",
            )))
        })
        .unwrap();
        drain(&hub).await;
        assert!(
            rec.frames().last().unwrap().ends_with("metal_unavailable"),
            "{:?}",
            rec.frames()
        );
    }

    /// Нулевые настройки не выключают фичу молча.
    #[test]
    fn zero_settings_fall_back_to_something_workable() {
        let h = VoiceHub::new(Arc::new(Recorder::default()), 0, Duration::ZERO);
        assert_eq!(h.max_parallel, 1, "ноль слотов выключил бы расшифровку молча");
        assert_eq!(h.run_timeout, Duration::from_secs(DEFAULT_RUN_TIMEOUT_SECS));
        assert!(format!("{h:?}").contains("max_parallel"));
    }

    /// Реестр отменяет всё разом при остановке плагина.
    #[tokio::test]
    async fn cancel_all_stops_every_run() {
        let (_rec, hub) = hub(4, 30_000);
        for i in 0..3 {
            hub.start(VoiceKey::new("peer", i.to_string(), 0), "local", |ctx| async move {
                ctx.cancel().cancelled().await;
                Err(VoiceError::Cancelled)
            })
            .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(hub.active(), 3);
        hub.cancel_all();
        drain(&hub).await;
    }
}
