//! Реестр активных анализов чата поверх [`forge_claude_runner::ClaudeRunner`].
//!
//! [`AnalysisHub`] решает три задачи, которых нет у самого раннера:
//!
//! 1. **Дедуп по беседе.** Второй запуск для того же `dialog_id` НЕ порождает
//!    второго процесса Claude — он подключается к идущему
//!    ([`StartOutcome::AlreadyRunning`]). Так и должно быть: кадры вещаются
//!    без фильтрации, поэтому вторая вкладка (или повторный клик по кнопке)
//!    уже видит тот же поток. Ответ `409` здесь был бы враньём: run
//!    существует, и клиенту нужен именно он.
//! 2. **Явный потолок параллелизма.** [`MAX_PARALLEL_ANALYSIS`] анализов по
//!    РАЗНЫМ беседам; третий получает отказ [`AnalysisError::Busy`] СРАЗУ.
//!    Семафор внутри раннера сделал бы вместо этого молчаливое ожидание — то
//!    есть открытый попап без единого кадра и без объяснения, худший из
//!    возможных исходов.
//! 3. **Коалесцинг дельт** — см. [`pump`]. Это условие корректности, а не
//!    оптимизация.
//!
//! # Кто сохраняет разбор
//!
//! Пампер, и ТОЛЬКО по успешно завершённому run'у вида `analyze`/`refresh`
//! ([`PersistCtx`]). Инвариант, ради которого всё это устроено именно так:
//! оборванный отменой, таймаутом или ошибкой текст НЕ становится сохранённым
//! разбором. Иначе следующий REFRESH получил бы огрызок в
//! `<previous_analysis>` и добросовестно повторил бы его в ответе — то есть
//! одна отменённая кнопка портила бы разбор беседы навсегда.
//!
//! `ask` не трогает сохранённый разбор вовсе: вопрос — это разговор о разборе,
//! а не новый разбор. Зато он сохраняет ПАРУ «вопрос — ответ» в тред той же
//! записи ([`PersistTarget::Turn`]) — по тем же правилам и с той же оговоркой:
//! только `ok == true`, только непустой ответ. Оборванный отменой огрызок в
//! треде вводил бы в заблуждение и человека, и следующий вопрос: тред уходит
//! модели как контекст.
//!
//! # Кто эмитит финальный кадр
//!
//! Штатное завершение (EOF стрима, ошибка CLI, таймаут) — задача-пампер, через
//! [`AnalysisSink::done`]. **Отмена — нет.** [`AnalysisHub::cancel`] abort'ит
//! таску раннера, а вместе с ней и пампера, поэтому кадр
//! `done{ok:false,reason:"cancelled"}` шлёт ПЛАГИН из своего роута отмены.
//! Признак «нас отменили» пампер видит через
//! [`tokio_util::sync::CancellationToken`] в записи run'а: без него обрыв
//! канала после abort'а он принял бы за нормальный конец и отправил бы
//! `ok:true`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_claude_runner::{ClaudeEvent, ClaudeRunner, RunRequest, Usage};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::error::AnalysisError;
use crate::prompts::PromptStore;
use crate::sink::AnalysisSink;

/// Сколько анализов по РАЗНЫМ беседам разрешено одновременно.
///
/// Два, а не четыре (как у Echo): анализ — это ручное действие пользователя,
/// который смотрит в один попап, а каждый run стоит полного окна контекста.
pub const MAX_PARALLEL_ANALYSIS: usize = 2;

/// Дефолтный таймаут одного анализа, секунды (для вызовов без конфига —
/// например, из тестов; у плагина это поле конфига).
pub const DEFAULT_ANALYSIS_TIMEOUT_SECS: u64 = 300;

/// Минимальный интервал между кадрами [`AnalysisSink::delta`], мс.
pub const DELTA_FLUSH_MS: u64 = 120;

/// Порог досрочного слива буфера дельт, code points.
///
/// Нужен ради ОТЗЫВЧИВОСТИ на быстрой генерации: 120 мс тишины при потоке в
/// сотни символов дали бы рывками по пол-абзаца. Порог возвращает ощущение
/// печатающей машинки, не возвращая шторма кадров.
pub const DELTA_FLUSH_CHARS: usize = 400;

/// Что именно просят у модели — попадает в кадр `started` как есть.
pub const KIND_ANALYZE: &str = "analyze";
/// Свободный вопрос по переписке.
pub const KIND_ASK: &str = "ask";
/// Инкрементальное обновление разбора: прошлый разбор + только новые сообщения.
pub const KIND_REFRESH: &str = "refresh";

/// Причины неуспеха в финальном кадре.
pub const REASON_CANCELLED: &str = "cancelled";
/// Run не уложился в таймаут.
pub const REASON_TIMEOUT: &str = "timeout";
/// Раннер отдал `ClaudeEvent::Error` либо `result` с `is_error`.
pub const REASON_ERROR: &str = "error";

/// Запись активного run'а.
#[derive(Debug, Clone)]
pub struct ActiveRun {
    /// Идентификатор run'а (он же — id в реестре самого раннера).
    pub run_id: String,
    /// Беседа (строкой) — ключ дедупа.
    pub dialog_id: String,
    /// `analyze` | `ask` | `refresh`.
    pub kind: String,
    /// Сколько сообщений попало в транскрипт.
    pub messages_used: u32,
    /// Часть истории отброшена потолком символов.
    pub truncated: bool,
    /// Взводится [`AnalysisHub::cancel`] ДО abort'а — по нему пампер отличает
    /// отмену от нормального конца стрима.
    cancelled: CancellationToken,
}

/// Что делать вызывающему после [`AnalysisHub::start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// Запущен новый run с этим `run_id`.
    Started(String),
    /// Для беседы уже идёт run — подключайтесь к нему.
    AlreadyRunning(String),
}

impl StartOutcome {
    /// Идентификатор run'а в обоих исходах.
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            Self::Started(id) | Self::AlreadyRunning(id) => id,
        }
    }

    /// Подключились к чужому (уже идущему) run'у?
    #[must_use]
    pub fn attached(&self) -> bool {
        matches!(self, Self::AlreadyRunning(_))
    }
}

/// Всё, что нужно для запуска: собранный промпт плюс размеры транскрипта для
/// кадра `started`.
#[derive(Debug, Clone)]
pub struct StartRequest {
    /// Беседа (строкой).
    pub dialog_id: String,
    /// `analyze` | `ask` | `refresh`.
    pub kind: String,
    /// Готовый промпт целиком — уедет в stdin процесса.
    pub prompt: String,
    /// Сколько сообщений попало в транскрипт.
    pub messages_used: u32,
    /// Часть истории отброшена потолком символов.
    pub truncated: bool,
    /// Максимальный id сообщения в транскрипте — станет
    /// [`crate::prompts::ChatAnalysis::last_message_id`] при успешном
    /// завершении. `None` ⇒ сохранять нечего (пустой транскрипт).
    pub newest_id: Option<i32>,
    /// Вопрос пользователя — только у `kind == "ask"`, он же станет `q`
    /// сохранённой пары треда. У `analyze`/`refresh` вопроса не бывает.
    pub question: Option<String>,
}

/// Реестр активных анализов.
pub struct AnalysisHub {
    runner: ClaudeRunner,
    cli_path: PathBuf,
    model: Option<String>,
    /// Хранилище заметок и разборов — ТО ЖЕ, что у состояния плагина (`Arc` на
    /// один `Mutex`), плюс путь файла. `None` в тестах, которым сохранение
    /// неинтересно: hub обязан работать и без хранилища.
    store: Option<(Arc<Mutex<PromptStore>>, PathBuf)>,
    /// `run_id` → запись. Ключ — run, а не диалог: отмена приходит с `run_id`,
    /// а дедуп по беседе — это поиск по значению (записей не больше
    /// [`MAX_PARALLEL_ANALYSIS`], линейный проход дешевле второй карты, которую
    /// пришлось бы держать согласованной).
    ///
    /// Под `Arc` — чтобы спавненная таска-пампер могла снять СВОЮ запись, не
    /// удерживая ссылку на весь hub (тот живёт внутри состояния плагина и в
    /// `Arc` сам по себе не завёрнут).
    active: Arc<Mutex<HashMap<String, ActiveRun>>>,
}

/// Отладочная печать БЕЗ содержимого хранилища и без списка активных run'ов.
///
/// Impl ручной, а не `derive`, по двум причинам, и обе существенны:
///
/// 1. `derive` потребовал бы `Debug` от `ClaudeRunner` — то есть протянул бы
///    требование в чужой листовой крейт ради одной строки логов;
/// 2. печатать содержимое `store` НЕЛЬЗЯ. Там лежат заметки пользователя о
///    конкретных людях и тексты разборов, а состояние плагина попадает в
///    `tracing` целиком (`{state:?}`) — `derive` тихо унёс бы приватную
///    переписку в лог.
///
/// Наружу поэтому видны только безопасные факты: путь к CLI и модель.
impl std::fmt::Debug for AnalysisHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalysisHub")
            .field("cli_path", &self.cli_path)
            .field("model", &self.model)
            .field("has_store", &self.store.is_some())
            .finish_non_exhaustive()
    }
}

impl AnalysisHub {
    /// Собирает hub. **Не падает** при отсутствии бинаря — так же, как
    /// [`ClaudeRunner::new`]: плагин обязан подниматься на машине без Claude
    /// CLI, а отказ выносит роут кодом `409 claude_unavailable`.
    pub fn new(cli_path: PathBuf, model: Option<String>, timeout_secs: u64) -> Self {
        let runner = ClaudeRunner::new(cli_path.clone(), MAX_PARALLEL_ANALYSIS)
            .with_run_timeout(timeout_secs);
        Self {
            runner,
            cli_path,
            model,
            store: None,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Подключает хранилище разборов (см. [`Self::store`]).
    ///
    /// Отдельным шагом, а не параметром конструктора: hub создаётся из
    /// конфига, а мапа принадлежит состоянию плагина, которое строится позже.
    #[must_use]
    pub fn with_store(mut self, store: Arc<Mutex<PromptStore>>, path: PathBuf) -> Self {
        self.store = Some((store, path));
        self
    }

    /// Путь, по которому ищется бинарь — его называет `409 claude_unavailable`.
    #[must_use]
    pub fn cli_path(&self) -> &Path {
        &self.cli_path
    }

    /// Есть ли чем запускать анализ.
    #[must_use]
    pub fn cli_available(&self) -> bool {
        self.cli_path.exists()
    }

    /// Сколько анализов идёт прямо сейчас.
    pub async fn active_count(&self) -> usize {
        self.active.lock().await.len()
    }

    /// Идущий run по беседе, если он есть.
    ///
    /// Роут спрашивает ДО выгрузки истории: повторный клик не должен стоить
    /// полутора секунд запросов к мессенджеру ради ответа «подключайся к тому
    /// же».
    pub async fn attached_run(&self, dialog_id: &str) -> Option<ActiveRun> {
        self.active
            .lock()
            .await
            .values()
            .find(|r| r.dialog_id == dialog_id)
            .cloned()
    }

    /// Запись run'а по идентификатору.
    pub async fn run(&self, run_id: &str) -> Option<ActiveRun> {
        self.active.lock().await.get(run_id).cloned()
    }

    /// Свободен ли слот под НОВУЮ беседу.
    ///
    /// Отдельный вопрос от [`Self::start`] по той же причине, что и
    /// [`Self::attached_run`]: отказ «уже идёт N анализов» обязан прилетать до
    /// выгрузки истории, а не после неё.
    ///
    /// # Errors
    ///
    /// [`AnalysisError::Busy`] — все слоты заняты.
    pub async fn has_free_slot(&self) -> Result<(), AnalysisError> {
        let n = self.active.lock().await.len();
        if n >= MAX_PARALLEL_ANALYSIS {
            return Err(AnalysisError::Busy(n));
        }
        Ok(())
    }

    /// Запускает анализ и возвращает `run_id`.
    ///
    /// Кадр `started` уходит ЗДЕСЬ, синхронно, до возврата: так он
    /// гарантированно оказывается у приёмника раньше первой дельты.
    ///
    /// # Errors
    ///
    /// [`AnalysisError::Busy`] — заняты все [`MAX_PARALLEL_ANALYSIS`] слотов
    /// ДРУГИМИ беседами. Для той же беседы отказа не бывает: возвращается
    /// [`StartOutcome::AlreadyRunning`].
    pub async fn start(
        &self,
        sink: Arc<dyn AnalysisSink>,
        req: StartRequest,
    ) -> Result<StartOutcome, AnalysisError> {
        // Дедуп, лимит и вставка — под ОДНИМ lock'ом: иначе два одновременных
        // POST по одной беседе оба увидели бы «свободно» и запустили два
        // процесса Claude на один попап.
        let (run_id, cancelled) = {
            let mut active = self.active.lock().await;
            if let Some(run) = active.values().find(|r| r.dialog_id == req.dialog_id) {
                return Ok(StartOutcome::AlreadyRunning(run.run_id.clone()));
            }
            if active.len() >= MAX_PARALLEL_ANALYSIS {
                return Err(AnalysisError::Busy(active.len()));
            }
            let run_id = uuid::Uuid::new_v4().to_string();
            let cancelled = CancellationToken::new();
            active.insert(
                run_id.clone(),
                ActiveRun {
                    run_id: run_id.clone(),
                    dialog_id: req.dialog_id.clone(),
                    kind: req.kind.clone(),
                    messages_used: req.messages_used,
                    truncated: req.truncated,
                    cancelled: cancelled.clone(),
                },
            );
            (run_id, cancelled)
        };

        // ТОЛЬКО РАЗМЕРЫ. Ни промпта, ни транскрипта, ни вопроса пользователя в
        // логах не бывает никогда: это переписка с конкретным человеком.
        tracing::info!(
            target: "chat_analysis",
            run_id = %run_id,
            dialog_id = %req.dialog_id,
            kind = %req.kind,
            messages_used = req.messages_used,
            truncated = req.truncated,
            prompt_chars = req.prompt.chars().count(),
            "chat analysis started (prompt itself is never logged)"
        );

        sink.started(
            &run_id,
            &req.dialog_id,
            &req.kind,
            req.messages_used,
            req.truncated,
        );

        let rx = self
            .runner
            .stream(RunRequest {
                prompt: req.prompt,
                model: self.model.clone(),
                system: Some(crate::prompt::SYSTEM_PROMPT.to_string()),
                run_id: run_id.clone(),
                // ОБЯЗАТЕЛЬНО: транскрипт на десятки тысяч символов в argv
                // виден в `ps aux` любому локальному пользователю и упирается
                // в ARG_MAX.
                prompt_via_stdin: true,
            })
            .await;

        // Что именно сохранится по успеху, зависит от вида run'а: разбор —
        // только когда известно, до какого сообщения он сделан; пара треда —
        // только когда известен вопрос. Всё остальное не сохраняется вовсе.
        let target = match req.kind.as_str() {
            KIND_ANALYZE | KIND_REFRESH => req.newest_id.map(|last_message_id| {
                PersistTarget::Analysis {
                    last_message_id,
                    messages_used: req.messages_used,
                }
            }),
            KIND_ASK => req
                .question
                .clone()
                .filter(|q| !q.trim().is_empty())
                .map(|question| PersistTarget::Turn { question }),
            _ => None,
        };
        let persist = match (&self.store, target) {
            (Some((store, path)), Some(target)) => Some(PersistCtx {
                store: store.clone(),
                path: path.clone(),
                dialog_id: req.dialog_id.clone(),
                target,
            }),
            _ => None,
        };

        let active = self.active.clone();
        let pump_run_id = run_id.clone();
        let dialog_id = req.dialog_id.clone();
        tokio::spawn(async move {
            pump(
                rx,
                sink,
                PumpCtx {
                    run_id: pump_run_id.clone(),
                    dialog_id,
                    cancelled,
                    flush_every: Duration::from_millis(DELTA_FLUSH_MS),
                    flush_chars: DELTA_FLUSH_CHARS,
                    persist,
                },
            )
            .await;
            active.lock().await.remove(&pump_run_id);
        });

        Ok(StartOutcome::Started(run_id))
    }

    /// Отменяет run по идентификатору. Возвращает `dialog_id` отменённого
    /// run'а, либо `None`, если такого run'а уже нет.
    ///
    /// Неизвестный run — НЕ ошибка: гонка «пользователь нажал «стоп» ровно
    /// тогда, когда модель дописала ответ» штатна и происходит регулярно.
    pub async fn cancel(&self, run_id: &str) -> Option<String> {
        // Токен взводится ДО abort'а, иначе пампер успел бы увидеть закрытый
        // канал и отправить финальный кадр с `ok:true`.
        let entry = self.active.lock().await.remove(run_id);
        let entry = entry?;
        entry.cancelled.cancel();
        self.runner.cancel(run_id).await;
        tracing::info!(
            target: "chat_analysis",
            run_id,
            dialog_id = %entry.dialog_id,
            "chat analysis cancelled"
        );
        Some(entry.dialog_id)
    }

    /// Отменяет тот run, что идёт по беседе. Возвращает его `run_id`.
    ///
    /// Нужен фронту без сохранённого `run_id`: кнопка «стоп» после
    /// перезагрузки вкладки обязана работать, а `run_id` жил в памяти попапа.
    pub async fn cancel_dialog(&self, dialog_id: &str) -> Option<String> {
        let run_id = self.attached_run(dialog_id).await?.run_id;
        self.cancel(&run_id).await.map(|_| run_id)
    }

    /// Гасит все активные run'ы. Вызывается из teardown плагина ДО остановки
    /// его воркеров, иначе процесс `claude` пережил бы остановку devforge и
    /// продолжил бы жечь токены.
    pub async fn shutdown(&self) {
        let drained: Vec<ActiveRun> = {
            let mut active = self.active.lock().await;
            active.drain().map(|(_, v)| v).collect()
        };
        for run in &drained {
            run.cancelled.cancel();
        }
        // `ClaudeRunner::shutdown` abort'ит таски раннера; дочерние процессы
        // умирают через `kill_on_drop`.
        self.runner.shutdown().await;
        if !drained.is_empty() {
            tracing::info!(
                target: "chat_analysis",
                killed = drained.len(),
                "chat analysis shutdown — active runs aborted"
            );
        }
    }
}

/// Контекст пампера.
pub(crate) struct PumpCtx {
    /// Идентификатор run'а.
    pub run_id: String,
    /// Беседа (строкой).
    pub dialog_id: String,
    /// Взведён ⇒ нас отменили, финального кадра не шлём.
    pub cancelled: CancellationToken,
    /// Минимальный интервал между кадрами дельт.
    pub flush_every: Duration,
    /// Порог досрочного слива, code points.
    pub flush_chars: usize,
    /// Куда сохранить результат по УСПЕШНОМУ завершению. `None` ⇒ сохранять
    /// нечего или незачем (пустой транскрипт, `ask` без вопроса, hub без
    /// хранилища).
    pub persist: Option<PersistCtx>,
}

/// Всё, что нужно памперу, чтобы записать результат run'а.
pub(crate) struct PersistCtx {
    /// Мапа заметок, разборов и тредов — общая с состоянием плагина.
    pub store: Arc<Mutex<PromptStore>>,
    /// Файл хранилища.
    pub path: PathBuf,
    /// Беседа (строкой) — ключ записи.
    pub dialog_id: String,
    /// В какую половину записи уйдёт текст.
    pub target: PersistTarget,
}

/// Куда пампер кладёт полный текст успешного run'а.
///
/// Разделение по виду run'а, а не два `Option`-поля: у разбора и у пары треда
/// нет общих параметров, и `Option` на каждый параметр допускал бы состояния
/// «разбор без точки отсчёта» и «пара без вопроса», которых не бывает.
pub(crate) enum PersistTarget {
    /// Текст станет сохранённым разбором (`analyze` / `refresh`).
    Analysis {
        /// Максимальный id сообщения в транскрипте: точка отсчёта следующего
        /// REFRESH.
        last_message_id: i32,
        /// Сколько сообщений было в транскрипте.
        messages_used: u32,
    },
    /// Текст станет ответом новой пары треда (`ask`).
    Turn {
        /// Вопрос пользователя — `q` пары.
        question: String,
    },
}

/// Перекладывает события раннера в [`AnalysisSink`], КОАЛЕСЦИРУЯ текст.
///
/// # Почему это условие корректности, а не оптимизация
///
/// `claude -p --include-partial-messages` отдаёт сотни `TextDelta` в секунду.
/// Один кадр на дельту переполнил бы `broadcast`-канал плагина (у VK это 1024
/// кадра) за считанные секунды, а переполнение у `tokio::sync::broadcast` —
/// это `RecvError::Lagged`, который WS-цикл переводит в кадр `resync`. Фронт по
/// `resync` перечитывает весь список бесед — и получает новый шторм. Панель
/// уходит в цикл перезагрузок ровно тогда, когда пользователь смотрит на неё.
///
/// Поэтому текст копится в буфере и уходит кадром не чаще
/// [`PumpCtx::flush_every`] либо по накоплению [`PumpCtx::flush_chars`]
/// символов. `Thinking` не вещается вовсе (шум и удвоение трафика), `ToolUse`
/// тоже: анализ инструментами не пользуется.
pub(crate) async fn pump(
    mut rx: mpsc::Receiver<ClaudeEvent>,
    sink: Arc<dyn AnalysisSink>,
    ctx: PumpCtx,
) {
    let mut buf = String::new();
    // ОТДЕЛЬНЫЙ накопитель полного ответа: `buf` — буфер коалесцинга, он
    // флашится и обнуляется по тику, поэтому к концу run'а в нём лежит только
    // последний кусок. Копим не больше `MAX_ANALYSIS_CHARS` (столько всё равно
    // сохранится), дальше просто перестаём копить — на поток к фронту это не
    // влияет никак.
    let mut full = String::new();
    let mut full_chars = 0usize;
    let mut usage = Usage::default();
    let mut ok = true;
    let mut reason: Option<String> = None;

    let mut ticker = tokio::time::interval(ctx.flush_every);
    // Первый тик у `interval` срабатывает немедленно — съедаем его, чтобы
    // первый же кадр не ушёл до накопления хоть чего-то.
    ticker.tick().await;
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let flush = |buf: &mut String| {
        if buf.is_empty() {
            return;
        }
        sink.delta(&ctx.run_id, &ctx.dialog_id, &std::mem::take(buf));
    };

    loop {
        tokio::select! {
            // Приоритет чтения не задаём: `biased` заставил бы пампер
            // выгребать канал, пока тот не опустеет, — то есть ровно на
            // быстром потоке никогда не доходить до тика.
            ev = rx.recv() => match ev {
                Some(ClaudeEvent::TextDelta { text }) => {
                    // Счётчик, а не `full.chars().count()` на каждой дельте:
                    // их сотни в секунду, и пересчёт всего накопленного дал бы
                    // квадрат ровно на длинном ответе.
                    if full_chars < crate::prompts::MAX_ANALYSIS_CHARS {
                        full.push_str(&text);
                        full_chars += text.chars().count();
                    }
                    buf.push_str(&text);
                    if buf.chars().count() >= ctx.flush_chars {
                        flush(&mut buf);
                    }
                }
                // Молча: в v1 в попап не показываются.
                Some(ClaudeEvent::Thinking { .. } | ClaudeEvent::ToolUse { .. }) => {}
                Some(ClaudeEvent::Result { usage: u, is_error, .. }) => {
                    usage = u;
                    if is_error {
                        ok = false;
                        reason = Some(REASON_ERROR.to_string());
                    }
                }
                Some(ClaudeEvent::Error { message }) => {
                    // Текст ошибки прокидывается ЦЕЛИКОМ: к нему уже приклеен
                    // хвост stderr, и это единственный способ увидеть «claude
                    // не залогинен», не читая логи сервера.
                    ok = false;
                    reason = Some(if message.starts_with("run timed out") {
                        REASON_TIMEOUT.to_string()
                    } else {
                        REASON_ERROR.to_string()
                    });
                    flush(&mut buf);
                    sink.error(&ctx.run_id, &ctx.dialog_id, &message);
                }
                None => break,
            },
            _ = ticker.tick() => flush(&mut buf),
        }
    }

    // Хвост буфера уходит ОБЯЗАТЕЛЬНО и до финального кадра: иначе последние
    // символы ответа терялись бы ровно на коротких ответах.
    flush(&mut buf);

    if ctx.cancelled.is_cancelled() {
        // Финальный кадр шлёт роут отмены — см. шапку модуля. И РАЗБОР ПРИ
        // ЭТОМ НЕ СОХРАНЯЕТСЯ: то, что успело натечь до «стоп», — половина
        // мысли, а не разбор.
        return;
    }
    // Сохранение — строго на успехе. `ok == false` это отмена, таймаут или
    // ошибка CLI: во всех трёх случаях текст оборван, и записать его значило
    // бы отравить следующий REFRESH (см. шапку модуля).
    if ok {
        if let Some(p) = &ctx.persist {
            persist_result(p, &full).await;
        }
    }
    sink.done(
        &ctx.run_id,
        &ctx.dialog_id,
        ok,
        reason.as_deref(),
        usage.input_tokens,
        usage.output_tokens,
    );
}

/// Записывает результат run'а в хранилище: мапа и файл — под ОДНИМ lock'ом,
/// как в хендлере правки заметки.
///
/// Ошибка записи НЕ роняет run и не превращается в `done{ok:false}`: текст уже
/// целиком у пользователя на экране, а не сохранился он — досадно, но это не
/// повод объявить успешный разбор провалившимся. В лог уходят только размеры:
/// и разбор, и ответ на вопрос — пересказ чужой переписки.
async fn persist_result(p: &PersistCtx, text: &str) {
    // Пустой накопитель — это run, из которого не приехало ни одной дельты.
    // Ни разбором, ни парой треда он стать не может.
    if text.trim().is_empty() {
        return;
    }
    let mut guard = p.store.lock().await;
    let what = match &p.target {
        PersistTarget::Analysis {
            last_message_id,
            messages_used,
        } => {
            guard.set_analysis(
                &p.dialog_id,
                text,
                *last_message_id,
                *messages_used,
                crate::text::now_unix(),
            );
            "analysis"
        }
        PersistTarget::Turn { question } => {
            guard.push_turn(&p.dialog_id, question, text, crate::text::now_unix());
            "qa turn"
        }
    };
    match crate::prompts::save(&p.path, &guard) {
        Ok(()) => tracing::debug!(
            target: "chat_analysis",
            dialog_id = %p.dialog_id,
            what,
            chars = text.chars().count(),
            "chat analysis result stored (the text itself is never logged)"
        ),
        Err(e) => tracing::warn!(
            target: "chat_analysis",
            dialog_id = %p.dialog_id,
            what,
            error = %e,
            "could not persist the chat analysis result"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testing::{Frame, RecordingSink};

    /// Ёмкость `broadcast`-канала плагина (у VK — `state::BROADCAST_CAPACITY`).
    /// Здесь она нужна одной проверке: кадров на шторм дельт обязано быть
    /// СИЛЬНО меньше, иначе переполнение канала даст `Lagged` → `resync` →
    /// цикл перезагрузок панели.
    const PLUGIN_BROADCAST_CAPACITY: usize = 1024;

    /// Mock CLI, печатающий два чанка и `result`, и сразу выходящий.
    const FAST_CLI: &str = r#"#!/bin/sh
cat > /dev/null
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"раз "}}'
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"два"}}'
printf '%s\n' '{"type":"result","usage":{"input_tokens":11,"output_tokens":3}}'
"#;

    /// Mock CLI, который висит: даёт время нажать «стоп».
    const SLOW_CLI: &str = r#"#!/bin/sh
printf '%s\n' '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"начало"}}'
sleep 30
"#;

    fn mock_cli(dir: &tempfile::TempDir, script: &str) -> PathBuf {
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn hub_with(dir: &tempfile::TempDir, script: &str) -> AnalysisHub {
        AnalysisHub::new(mock_cli(dir, script), Some("sonnet".into()), 30)
    }

    fn req(dialog_id: &str) -> StartRequest {
        StartRequest {
            dialog_id: dialog_id.to_string(),
            kind: KIND_ANALYZE.to_string(),
            prompt: "<task>разбери</task>".into(),
            messages_used: 42,
            truncated: false,
            newest_id: Some(7042),
            question: None,
        }
    }

    /// Тот же запрос, но вопросом: `kind = ask` плюс сам вопрос.
    fn ask_req(dialog_id: &str, question: &str) -> StartRequest {
        StartRequest {
            kind: KIND_ASK.to_string(),
            question: Some(question.to_string()),
            ..req(dialog_id)
        }
    }

    /// Потолок ожидания события, которое ОБЯЗАНО произойти.
    ///
    /// Щедрый намеренно. В `cargo test --workspace` этот модуль гоняется
    /// параллельно с десятками других бинарей, и спавн мок-процесса
    /// `/bin/sh` под нагрузкой занимает секунды вместо миллисекунд. Прежние
    /// 20 с иногда не укрывали именно это — тест падал не на дефекте
    /// продукта, а на таймере (см. forge-s4b2k). На зелёном прогоне высокий
    /// потолок не стоит ничего: ждём по факту, а не по часам.
    const WAIT_CAP: Duration = Duration::from_secs(60);

    /// Окно, в течение которого ЗАПРЕЩЁННАЯ запись могла бы всплыть.
    ///
    /// Единственное место, где сон уместен: отсутствие события опросом не
    /// доказывается — ждать «пока ничего не появится» можно только конечное
    /// время. Здесь сон не флакует: если фон опоздает, тест не упадёт ложно,
    /// а в худшем случае пропустит регресс, поэтому окно щедрое.
    const FORBIDDEN_WRITE_GRACE: Duration = Duration::from_millis(500);

    /// Ждёт кадр, удовлетворяющий предикату, до [`WAIT_CAP`].
    ///
    /// Отдельная функция, а не голый `sink.wait_for`: потолок обязан быть
    /// один на весь модуль, иначе он снова расползётся по числам в вызовах.
    async fn wait_frame(sink: &RecordingSink, pred: impl Fn(&Frame) -> bool) -> Option<Frame> {
        sink.wait_for(WAIT_CAP, pred).await
    }

    /// Ждёт кадр, удовлетворяющий предикату, не дольше `secs`.
    ///
    /// Остаётся ради проверок ОТСУТСТВИЯ кадра, где короткий потолок — часть
    /// смысла теста. Для «кадр обязан прийти» используй [`wait_frame`].
    async fn wait_for(
        sink: &RecordingSink,
        secs: u64,
        pred: impl Fn(&Frame) -> bool,
    ) -> Option<Frame> {
        sink.wait_for(Duration::from_secs(secs), pred).await
    }

    /// Ждёт АСИНХРОННОЕ условие до [`WAIT_CAP`], опрашивая его.
    ///
    /// Макрос, а не функция: условие содержит `.await` (поход под
    /// `store.lock().await`), и заворачивать его в замыкание, возвращающее
    /// future, значило бы бороться с временами жизни там, где боремся с
    /// флаком. Приём скопирован из `plugins/meet/tests/pipeline.rs` (фаза 7),
    /// намеренно копией: тащить зависимость между крейтами ради макроса
    /// дороже, чем повторить восемь строк.
    ///
    /// НЕ ЗАМЕНЯЙ НА `sleep`. Фоновая запись (пампер → `PromptStore` → файл)
    /// происходит не «за 100 мс», а «когда планировщик дойдёт»; под
    /// параллельным прогоном workspace это иногда сотни миллисекунд. Сон на
    /// глазок делает тест красным без единого дефекта в продукте — ровно тот
    /// баг, что чинился в forge-s4b2k.
    macro_rules! wait_until {
        ($label:expr, $cond:expr) => {{
            let started = std::time::Instant::now();
            let mut reached = false;
            while started.elapsed() < WAIT_CAP {
                if $cond {
                    reached = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                reached,
                "{}: ТАЙМАУТ ожидания ({:?}) — это не «неверное состояние», а «не дождались»",
                $label, WAIT_CAP
            );
        }};
    }

    /// ДЕДУП ПО БЕСЕДЕ: второй запуск подключается к идущему run'у и НЕ
    /// порождает второго процесса Claude.
    #[tokio::test]
    async fn a_second_start_for_the_same_dialog_attaches_to_the_running_one() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, SLOW_CLI);
        let sink = RecordingSink::new();

        let first = hub.start(sink.clone(), req("2000000001")).await.unwrap();
        assert!(matches!(first, StartOutcome::Started(_)));

        let second = hub.start(sink.clone(), req("2000000001")).await.unwrap();
        assert_eq!(second, StartOutcome::AlreadyRunning(first.run_id().to_string()));
        assert!(second.attached());
        assert_eq!(hub.active_count().await, 1, "второго процесса быть не должно");
        // И `started` вещался ровно один раз: подключение к чужому run'у своих
        // кадров не порождает.
        assert_eq!(
            sink.frames()
                .iter()
                .filter(|f| matches!(f, Frame::Started { .. }))
                .count(),
            1
        );

        // И размеры транскрипта у подключившегося берутся из ЖИВОЙ записи.
        let snap = hub.attached_run("2000000001").await.unwrap();
        assert_eq!(snap.messages_used, 42);
        assert_eq!(snap.run_id, first.run_id());

        hub.shutdown().await;
    }

    /// Кадр `started` уходит СИНХРОННО в `start` и несёт размеры транскрипта:
    /// попап рисует счётчик до первой дельты.
    #[tokio::test]
    async fn the_started_frame_precedes_every_delta() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, FAST_CLI);
        let sink = RecordingSink::new();

        let run_id = hub
            .start(sink.clone(), req("11"))
            .await
            .unwrap()
            .run_id()
            .to_string();
        match sink.frames().first().cloned() {
            Some(Frame::Started {
                run_id: id,
                dialog_id,
                kind,
                messages_used,
                truncated,
            }) => {
                assert_eq!(id, run_id);
                assert_eq!(dialog_id, "11");
                assert_eq!(kind, KIND_ANALYZE);
                assert_eq!(messages_used, 42);
                assert!(!truncated);
            }
            other => panic!("первым обязан быть started: {other:?}"),
        }
        assert!(
            wait_frame(&sink, Frame::is_done).await.is_some(),
            "финальный кадр не пришёл"
        );
        hub.shutdown().await;
    }

    /// ГЛОБАЛЬНЫЙ ЛИМИТ: третья беседа получает ЯВНЫЙ отказ, а не молчаливое
    /// ожидание на семафоре (пустой попап без объяснений — худший исход).
    #[tokio::test]
    async fn a_third_dialog_is_refused_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, SLOW_CLI);
        let sink = RecordingSink::new();

        hub.start(sink.clone(), req("1")).await.unwrap();
        hub.start(sink.clone(), req("2")).await.unwrap();
        assert_eq!(
            hub.has_free_slot().await,
            Err(AnalysisError::Busy(MAX_PARALLEL_ANALYSIS))
        );

        let started = std::time::Instant::now();
        let third = hub.start(sink.clone(), req("3")).await;
        assert_eq!(third, Err(AnalysisError::Busy(MAX_PARALLEL_ANALYSIS)));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "отказ обязан быть мгновенным, а не ожиданием: {:?}",
            started.elapsed()
        );

        hub.shutdown().await;
    }

    /// Слот освобождается при ЛЮБОМ завершении — здесь при нормальном.
    #[tokio::test]
    async fn a_finished_run_frees_its_slot_and_reports_usage() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, FAST_CLI);
        let sink = RecordingSink::new();

        hub.start(sink.clone(), req("77")).await.unwrap();
        let done = wait_frame(&sink, Frame::is_done)
            .await
            .expect("финальный кадр не пришёл");
        match done {
            Frame::Done {
                ok,
                reason,
                input_tokens,
                output_tokens,
                ..
            } => {
                assert!(ok, "штатный run обязан быть ok");
                assert!(reason.is_none());
                assert_eq!((input_tokens, output_tokens), (11, 3));
            }
            other => panic!("{other:?}"),
        }

        // Запись снимается таской ПОСЛЕ кадра, поэтому опрос до условия, а не
        // сон и не цикл на глазок: под параллельным прогоном workspace
        // планировщик доходит до фоновой таски когда угодно, и «50 раз по
        // 20 мс» — такой же таймер, только длиннее.
        wait_until!("слот освободился", hub.active_count().await == 0);
        // И та же беседа снова запускается.
        assert!(matches!(
            hub.start(sink, req("77")).await.unwrap(),
            StartOutcome::Started(_)
        ));
        hub.shutdown().await;
    }

    /// ОТМЕНА: слот свободен, а финальный кадр пампер НЕ шлёт — его отправит
    /// роут отмены (иначе фронт получил бы два финальных кадра, один из них с
    /// враньём `ok:true`).
    #[tokio::test]
    async fn cancel_frees_the_slot_and_emits_no_done_frame() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, SLOW_CLI);
        let sink = RecordingSink::new();

        let run_id = hub
            .start(sink.clone(), req("500"))
            .await
            .unwrap()
            .run_id()
            .to_string();
        // Дожидаемся первой дельты — процесс точно живой.
        assert!(
            wait_frame(&sink, Frame::is_delta).await.is_some(),
            "дельта от mock CLI не пришла"
        );

        assert_eq!(hub.cancel(&run_id).await.as_deref(), Some("500"));
        assert_eq!(hub.active_count().await, 0);

        // За разумное время финального кадра быть не должно.
        assert!(
            wait_for(&sink, 2, Frame::is_done).await.is_none(),
            "после отмены пампер не имеет права слать финальный кадр"
        );
    }

    /// Гонка «нажал стоп ровно когда модель дописала» — штатна.
    #[tokio::test]
    async fn cancelling_an_unknown_run_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, FAST_CLI);
        assert!(hub.cancel("нет такого").await.is_none());
        assert!(hub.cancel_dialog("2000000001").await.is_none());
    }

    /// `cancel_dialog` нужен вкладке, потерявшей `run_id` при перезагрузке.
    #[tokio::test]
    async fn cancel_by_dialog_finds_the_running_id() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, SLOW_CLI);
        let sink = RecordingSink::new();
        let run_id = hub
            .start(sink, req("42"))
            .await
            .unwrap()
            .run_id()
            .to_string();
        assert_eq!(hub.cancel_dialog("42").await.as_deref(), Some(run_id.as_str()));
        assert_eq!(hub.active_count().await, 0);
    }

    /// shutdown гасит всё: без него процесс claude пережил бы devforge.
    #[tokio::test]
    async fn shutdown_drops_every_active_run() {
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, SLOW_CLI);
        let sink = RecordingSink::new();
        hub.start(sink.clone(), req("1")).await.unwrap();
        hub.start(sink, req("2")).await.unwrap();
        assert_eq!(hub.active_count().await, 2);
        hub.shutdown().await;
        assert_eq!(hub.active_count().await, 0);
        // Идемпотентность: второй вызов ничего не ломает.
        hub.shutdown().await;
    }

    /// КОАЛЕСЦИНГ — условие корректности (риск `Lagged` → `resync` → цикл
    /// перезагрузок панели). Тест гоняет ПАМПЕР напрямую, без CLI: важна
    /// арифметика кадров, а не процесс.
    #[tokio::test]
    async fn the_pump_coalesces_a_storm_of_deltas() {
        const DELTAS: usize = 1_000;
        let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
        let sink = RecordingSink::new();

        let pump_task = tokio::spawn(pump(
            ev_rx,
            sink.clone(),
            PumpCtx {
                run_id: "r".into(),
                dialog_id: "d".into(),
                cancelled: CancellationToken::new(),
                flush_every: Duration::from_millis(DELTA_FLUSH_MS),
                flush_chars: DELTA_FLUSH_CHARS,
                persist: None,
            },
        ));

        for _ in 0..DELTAS {
            ev_tx
                .send(ClaudeEvent::TextDelta { text: "я".into() })
                .await
                .unwrap();
        }
        ev_tx
            .send(ClaudeEvent::Result {
                usage: Usage::default(),
                is_error: false,
                raw_json: serde_json::Value::Null,
            })
            .await
            .unwrap();
        drop(ev_tx);
        pump_task.await.unwrap();

        let frames = sink.delta_frames();
        assert!(
            sink.frames().iter().any(Frame::is_done),
            "финальный кадр обязателен"
        );
        // Ни один символ не потерян и порядок сохранён.
        assert_eq!(sink.text(), "я".repeat(DELTAS));
        // Порог 400 символов даёт минимум ceil(1000/400) = 3 кадра; тиков по
        // 120 мс на таком потоке добавится единицы. Двузначное число кадров на
        // тысячу дельт — это уже не шторм, а именно коалесцинг.
        assert!(
            (1..=32).contains(&frames),
            "{frames} кадров на {DELTAS} дельт — коалесцинг не сработал"
        );
        assert!(
            frames < PLUGIN_BROADCAST_CAPACITY,
            "переполнение broadcast означало бы Lagged → resync"
        );
    }

    /// Хвост буфера обязан уйти ДО финального кадра: иначе короткий ответ,
    /// уместившийся в один буфер, терялся бы целиком.
    #[tokio::test]
    async fn the_pump_flushes_the_tail_before_done() {
        let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
        let sink = RecordingSink::new();
        let pump_task = tokio::spawn(pump(
            ev_rx,
            sink.clone(),
            PumpCtx {
                run_id: "r".into(),
                dialog_id: "d".into(),
                cancelled: CancellationToken::new(),
                flush_every: Duration::from_secs(3600),
                flush_chars: DELTA_FLUSH_CHARS,
                persist: None,
            },
        ));
        ev_tx
            .send(ClaudeEvent::TextDelta {
                text: "короткий ответ".into(),
            })
            .await
            .unwrap();
        drop(ev_tx);
        pump_task.await.unwrap();

        let frames = sink.frames();
        assert!(
            matches!(&frames[0], Frame::Delta { text, .. } if text == "короткий ответ"),
            "{frames:?}"
        );
        assert!(frames[1].is_done(), "{frames:?}");
        assert_eq!(frames.len(), 2, "{frames:?}");
    }

    /// `Thinking` и `ToolUse` не вещаются ни в каком виде.
    #[tokio::test]
    async fn thinking_never_reaches_the_socket() {
        let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
        let sink = RecordingSink::new();
        let pump_task = tokio::spawn(pump(
            ev_rx,
            sink.clone(),
            PumpCtx {
                run_id: "r".into(),
                dialog_id: "d".into(),
                cancelled: CancellationToken::new(),
                flush_every: Duration::from_millis(20),
                flush_chars: DELTA_FLUSH_CHARS,
                persist: None,
            },
        ));
        ev_tx
            .send(ClaudeEvent::Thinking {
                text: "РАЗМЫШЛЕНИЕ-КАНАРЕЙКА".into(),
            })
            .await
            .unwrap();
        ev_tx
            .send(ClaudeEvent::ToolUse {
                name: "Read".into(),
                input: serde_json::json!({"file":"/etc/passwd"}),
            })
            .await
            .unwrap();
        drop(ev_tx);
        pump_task.await.unwrap();

        let frames = sink.frames();
        let dump = format!("{frames:?}");
        assert!(!dump.contains("РАЗМЫШЛЕНИЕ-КАНАРЕЙКА"), "{dump}");
        assert!(!dump.contains("/etc/passwd"), "{dump}");
        assert_eq!(frames.len(), 1, "только финальный кадр: {frames:?}");
        assert!(frames[0].is_done());
    }

    /// Ошибка раннера доезжает ЦЕЛИКОМ (в ней хвост stderr — единственный
    /// способ увидеть «claude не залогинен»), а таймаут отличается от прочих
    /// отказов по `reason`.
    #[tokio::test]
    async fn runner_errors_are_forwarded_verbatim_and_classified() {
        for (message, want_reason) in [
            (
                "claude run produced no output; stderr: Invalid API key",
                REASON_ERROR,
            ),
            ("run timed out after 30s", REASON_TIMEOUT),
        ] {
            let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
            let sink = RecordingSink::new();
            let pump_task = tokio::spawn(pump(
                ev_rx,
                sink.clone(),
                PumpCtx {
                    run_id: "r".into(),
                    dialog_id: "d".into(),
                    cancelled: CancellationToken::new(),
                    flush_every: Duration::from_millis(20),
                    flush_chars: DELTA_FLUSH_CHARS,
                    persist: None,
                },
            ));
            ev_tx
                .send(ClaudeEvent::Error {
                    message: message.to_string(),
                })
                .await
                .unwrap();
            drop(ev_tx);
            pump_task.await.unwrap();

            let frames = sink.frames();
            match &frames[0] {
                Frame::Error { message: m, .. } => assert_eq!(m, message),
                other => panic!("{other:?}"),
            }
            match &frames[1] {
                Frame::Done { ok, reason, .. } => {
                    assert!(!ok);
                    assert_eq!(reason.as_deref(), Some(want_reason));
                }
                other => panic!("{other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // СОХРАНЕНИЕ РАЗБОРА.
    //
    // Один инвариант на все тесты ниже: в хранилище попадает только ПОЛНЫЙ
    // ответ успешного `analyze`/`refresh`. Всё остальное — вопрос, отмена,
    // таймаут, ошибка CLI — оставляет прошлый разбор ровно таким, каким он был.
    // -----------------------------------------------------------------------

    /// Хранилище на диске + hub, который в него пишет.
    fn hub_with_store(
        dir: &tempfile::TempDir,
        script: &str,
    ) -> (AnalysisHub, Arc<Mutex<PromptStore>>, PathBuf) {
        let path = dir.path().join("prompts.json");
        let store = Arc::new(Mutex::new(PromptStore::default()));
        let hub = hub_with(dir, script).with_store(store.clone(), path.clone());
        (hub, store, path)
    }

    /// Успешный разбор доезжает до ФАЙЛА целиком и с точкой отсчёта.
    #[tokio::test]
    async fn a_successful_analyze_is_stored_with_its_last_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, store, path) = hub_with_store(&dir, FAST_CLI);
        let sink = RecordingSink::new();
        // Заметка уже есть — она обязана пережить запись разбора.
        store.lock().await.set("2000000001", "это заказчик", 5);

        hub.start(sink.clone(), req("2000000001")).await.unwrap();
        assert!(
            wait_frame(&sink, Frame::is_done).await.is_some(),
            "финальный кадр не пришёл"
        );

        // Опрос, а не сон и не «сразу после кадра»: порядок «сохранили →
        // отдали финальный кадр» — деталь пампера, на которую тест не обязан
        // закладываться. Ждём сам факт записи.
        wait_until!(
            "разбор сохранён",
            store.lock().await.analysis("2000000001").is_some()
        );
        let saved = store.lock().await.analysis("2000000001").cloned().unwrap();
        assert_eq!(saved.text, "раз два", "сохранён ПОЛНЫЙ ответ, а не хвост буфера");
        assert_eq!(saved.last_message_id, 7042, "точка отсчёта следующего REFRESH");
        assert_eq!(saved.messages_used, 42);
        assert!(saved.updated_at > 0);
        assert_eq!(
            store.lock().await.text("2000000001"),
            "это заказчик",
            "заметка не тронута"
        );

        // И это на диске, а не только в памяти.
        let from_disk = crate::prompts::load(&path);
        assert_eq!(from_disk.analysis("2000000001").unwrap().text, "раз два");
        hub.shutdown().await;
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ ДОРАБОТКИ: отмена не имеет права оставить огрызок
    /// «прошлым разбором» — ни поверх старого, ни на пустом месте.
    #[tokio::test]
    async fn a_cancelled_run_leaves_the_stored_analysis_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, store, _path) = hub_with_store(&dir, SLOW_CLI);
        let sink = RecordingSink::new();
        store
            .lock()
            .await
            .set_analysis("500", "ПРОШЛЫЙ РАЗБОР", 100, 7, 1_800_000_000);

        let run_id = hub
            .start(sink.clone(), req("500"))
            .await
            .unwrap()
            .run_id()
            .to_string();
        // Дельта пришла — то есть текст, который мог бы «сохраниться», уже есть.
        assert!(wait_frame(&sink, Frame::is_delta).await.is_some());
        hub.cancel(&run_id).await;
        // ЕДИНСТВЕННЫЙ случай, где сон правомерен: проверяется ОТСУТСТВИЕ
        // записи, а отсутствие опросом не доказывается — «ждать, пока ничего
        // не появится» можно только конечное время. Флак отсюда не растёт: при
        // опоздании фона тест не покраснеет ложно.
        tokio::time::sleep(FORBIDDEN_WRITE_GRACE).await;

        let saved = store.lock().await.analysis("500").cloned().unwrap();
        assert_eq!(saved.text, "ПРОШЛЫЙ РАЗБОР", "отмена переписала разбор");
        assert_eq!(saved.last_message_id, 100);

        // И там, где разбора не было, он не появляется.
        let sink2 = RecordingSink::new();
        let run_id = hub
            .start(sink2.clone(), req("501"))
            .await
            .unwrap()
            .run_id()
            .to_string();
        assert!(wait_frame(&sink2, Frame::is_delta).await.is_some());
        hub.cancel(&run_id).await;
        // Та же проверка отсутствия — см. комментарий выше.
        tokio::time::sleep(FORBIDDEN_WRITE_GRACE).await;
        assert!(store.lock().await.analysis("501").is_none());
        hub.shutdown().await;
    }

    /// Вопрос по переписке сохранённый разбор не трогает вовсе.
    #[tokio::test]
    async fn an_ask_run_never_touches_the_stored_analysis() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, store, _path) = hub_with_store(&dir, FAST_CLI);
        let sink = RecordingSink::new();
        store.lock().await.set_analysis("77", "ПРОШЛЫЙ", 100, 7, 10);

        hub.start(sink.clone(), ask_req("77", "а что со сроками?"))
            .await
            .unwrap();
        assert!(wait_frame(&sink, Frame::is_done).await.is_some());
        // Опрос до факта записи, а не сон: пампер сохраняет пару в фоне, и
        // «100 мс после финального кадра» — это ставка на планировщик, а не
        // проверка. Под параллельным прогоном workspace ставка проигрывает, и
        // тест краснеет без единого дефекта в продукте (forge-s4b2k).
        wait_until!("ответ доехал до треда", !store.lock().await.thread("77").is_empty());

        let saved = store.lock().await.analysis("77").cloned().unwrap();
        assert_eq!(saved.text, "ПРОШЛЫЙ", "ответ на вопрос стал разбором");
        assert_eq!(saved.last_message_id, 100);
        hub.shutdown().await;
    }

    /// Успешный `ask` кладёт РОВНО ОДНУ пару в тред: вопрос — из запроса,
    /// ответ — полный текст модели, и всё это доезжает до файла.
    #[tokio::test]
    async fn a_successful_ask_appends_one_turn_to_the_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, store, path) = hub_with_store(&dir, FAST_CLI);
        let sink = RecordingSink::new();
        store.lock().await.set("88", "это заказчик", 5);
        store.lock().await.set_analysis("88", "ПРОШЛЫЙ", 100, 7, 6);

        hub.start(sink.clone(), ask_req("88", "  а что со сроками? "))
            .await
            .unwrap();
        assert!(wait_frame(&sink, Frame::is_done).await.is_some());
        // Опрос до появления пары, а не сон: запись в `PromptStore` и в файл
        // делает фоновая таска пампера. Сон на глазок здесь и был причиной
        // флака forge-s4b2k — не возвращай его, «проще» он только на пустой
        // машине.
        wait_until!("пара доехала до треда", store.lock().await.thread("88").len() == 1);

        let guard = store.lock().await;
        let thread = guard.thread("88");
        assert_eq!(thread.len(), 1, "ровно одна пара");
        assert_eq!(thread[0].q, "а что со сроками?");
        assert_eq!(thread[0].a, "раз два", "сохранён ПОЛНЫЙ ответ");
        assert!(thread[0].at > 0);
        assert_eq!(guard.text("88"), "это заказчик", "заметка не тронута");
        assert_eq!(guard.analysis("88").unwrap().text, "ПРОШЛЫЙ", "разбор не тронут");
        drop(guard);

        // И это на диске, а не только в памяти.
        let from_disk = crate::prompts::load(&path);
        assert_eq!(from_disk.thread("88")[0].a, "раз два");
        hub.shutdown().await;
    }

    /// Отменённый `ask` пары не оставляет: половина ответа в треде — это
    /// половина ответа и человеку, и следующему вопросу.
    #[tokio::test]
    async fn a_cancelled_ask_leaves_no_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, store, _path) = hub_with_store(&dir, SLOW_CLI);
        let sink = RecordingSink::new();

        let run_id = hub
            .start(sink.clone(), ask_req("99", "успею нажать стоп?"))
            .await
            .unwrap()
            .run_id()
            .to_string();
        assert!(
            wait_frame(&sink, Frame::is_delta).await.is_some(),
            "текст, который мог бы «сохраниться», уже натёк"
        );
        hub.cancel(&run_id).await;
        // Проверяется ОТСУТСТВИЕ пары — здесь опрос бессмыслен (см.
        // `FORBIDDEN_WRITE_GRACE`): ждём окно, в которое запрещённая запись
        // успела бы всплыть.
        tokio::time::sleep(FORBIDDEN_WRITE_GRACE).await;

        assert!(store.lock().await.thread("99").is_empty());
        hub.shutdown().await;
    }

    /// `ok == false` (ошибка либо таймаут) — тот же запрет. Гоняется на
    /// пампере напрямую: важна ветка, а не способ в неё попасть.
    #[tokio::test]
    async fn a_failed_ask_stores_no_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        let store = Arc::new(Mutex::new(PromptStore::default()));

        let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
        let task = tokio::spawn(pump(
            ev_rx,
            RecordingSink::new(),
            PumpCtx {
                run_id: "r".into(),
                dialog_id: "d".into(),
                cancelled: CancellationToken::new(),
                flush_every: Duration::from_millis(20),
                flush_chars: DELTA_FLUSH_CHARS,
                persist: Some(PersistCtx {
                    store: store.clone(),
                    path: path.clone(),
                    dialog_id: "d".into(),
                    target: PersistTarget::Turn {
                        question: "вопрос".into(),
                    },
                }),
            },
        ));
        ev_tx
            .send(ClaudeEvent::TextDelta {
                text: "половина мысли".into(),
            })
            .await
            .unwrap();
        ev_tx
            .send(ClaudeEvent::Error {
                message: "run timed out after 30s".into(),
            })
            .await
            .unwrap();
        drop(ev_tx);
        task.await.unwrap();

        assert!(store.lock().await.thread("d").is_empty(), "огрызок попал в тред");
        assert!(!path.exists(), "файл не переписывался вовсе");
    }

    /// Разбор и обновление в тред не пишут: тред — это ТОЛЬКО вопросы.
    #[tokio::test]
    async fn analyze_and_refresh_never_touch_the_thread() {
        for kind in [KIND_ANALYZE, KIND_REFRESH] {
            let dir = tempfile::tempdir().unwrap();
            let (hub, store, _path) = hub_with_store(&dir, FAST_CLI);
            let sink = RecordingSink::new();
            store.lock().await.push_turn("7", "старый вопрос", "старый ответ", 5);

            let r = StartRequest {
                kind: kind.to_string(),
                ..req("7")
            };
            hub.start(sink.clone(), r).await.unwrap();
            assert!(wait_frame(&sink, Frame::is_done).await.is_some(), "{kind}");
            // Опрос до появления разбора, а не сон: разбор пишет фоновая
            // таска пампера, и её момент задаёт планировщик, а не таймер. Как
            // только разбор на месте — видно и то, что тред при этом не
            // тронут, ради чего тест и написан.
            wait_until!(
                format!("{kind}: разбор сохранён"),
                store.lock().await.analysis("7").is_some()
            );

            let guard = store.lock().await;
            assert_eq!(guard.thread("7").len(), 1, "{kind}: тред вырос");
            assert_eq!(guard.thread("7")[0].a, "старый ответ", "{kind}");
            assert_eq!(guard.analysis("7").unwrap().text, "раз два", "{kind}");
            drop(guard);
            hub.shutdown().await;
        }
    }

    /// Ошибка и таймаут — тот же запрет, что у отмены. Гоняется на пампере
    /// напрямую: важна ветка `ok == false`, а не способ её получить.
    #[tokio::test]
    async fn a_failed_run_stores_nothing() {
        for message in ["claude run produced no output", "run timed out after 30s"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("p.json");
            let store = Arc::new(Mutex::new(PromptStore::default()));
            store.lock().await.set_analysis("d", "ПРОШЛЫЙ", 9, 1, 10);

            let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
            let task = tokio::spawn(pump(
                ev_rx,
                RecordingSink::new(),
                PumpCtx {
                    run_id: "r".into(),
                    dialog_id: "d".into(),
                    cancelled: CancellationToken::new(),
                    flush_every: Duration::from_millis(20),
                    flush_chars: DELTA_FLUSH_CHARS,
                    persist: Some(PersistCtx {
                        store: store.clone(),
                        path: path.clone(),
                        dialog_id: "d".into(),
                        target: PersistTarget::Analysis {
                            last_message_id: 42,
                            messages_used: 3,
                        },
                    }),
                },
            ));
            ev_tx
                .send(ClaudeEvent::TextDelta {
                    text: "половина мысли".into(),
                })
                .await
                .unwrap();
            ev_tx
                .send(ClaudeEvent::Error {
                    message: message.to_string(),
                })
                .await
                .unwrap();
            drop(ev_tx);
            task.await.unwrap();

            assert_eq!(
                store.lock().await.analysis("d").unwrap().text,
                "ПРОШЛЫЙ",
                "{message}: оборванный текст стал разбором"
            );
            assert!(!path.exists(), "{message}: файл не переписывался вовсе");
        }
    }

    /// Ответ длиннее потолка сохраняется ОБРЕЗАННЫМ, а на фронт при этом
    /// уходит целиком: экран и хранилище живут по разным лимитам.
    #[tokio::test]
    async fn an_overlong_answer_is_stored_truncated_but_streamed_whole() {
        const LIMIT: usize = crate::prompts::MAX_ANALYSIS_CHARS;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        let store = Arc::new(Mutex::new(PromptStore::default()));
        let sink = RecordingSink::new();

        let (ev_tx, ev_rx) = mpsc::channel::<ClaudeEvent>(8);
        let task = tokio::spawn(pump(
            ev_rx,
            sink.clone(),
            PumpCtx {
                run_id: "r".into(),
                dialog_id: "d".into(),
                cancelled: CancellationToken::new(),
                flush_every: Duration::from_millis(20),
                flush_chars: DELTA_FLUSH_CHARS,
                persist: Some(PersistCtx {
                    store: store.clone(),
                    path,
                    dialog_id: "d".into(),
                    target: PersistTarget::Analysis {
                        last_message_id: 42,
                        messages_used: 3,
                    },
                }),
            },
        ));
        // Кириллица: обрезка по code points, а не по байтам.
        for _ in 0..(LIMIT / 100 + 5) {
            ev_tx
                .send(ClaudeEvent::TextDelta {
                    text: "я".repeat(100),
                })
                .await
                .unwrap();
        }
        drop(ev_tx);
        task.await.unwrap();

        let saved = store.lock().await.analysis("d").cloned().unwrap();
        assert_eq!(saved.text.chars().count(), LIMIT, "хранилище режет по потолку");
        assert_eq!(
            sink.text().chars().count(),
            (LIMIT / 100 + 5) * 100,
            "на фронт ответ уходит целиком"
        );
    }

    /// ИНВАРИАНТ УТЕЧКИ: промпт (а в нём — транскрипт чужой переписки) не
    /// попадает в логи ни целиком, ни куском. Логируются только РАЗМЕРЫ.
    #[tokio::test]
    async fn the_prompt_never_reaches_the_logs() {
        const CANARY: &str = "КАНАРЕЙКА-ТРАНСКРИПТА-42";
        let dir = tempfile::tempdir().unwrap();
        let hub = hub_with(&dir, FAST_CLI);
        let sink = RecordingSink::new();

        let mark = crate::testlog::mark();
        let mut r = req("2000000001");
        r.prompt = format!("<transcript>{CANARY}</transcript>");
        hub.start(sink.clone(), r).await.unwrap();
        let _ = wait_frame(&sink, Frame::is_done).await;
        hub.shutdown().await;

        let logged = crate::testlog::since(mark);
        assert!(!logged.contains(CANARY), "промпт утёк в логи:\n{logged}");
        assert!(!logged.contains("transcript"), "структура промпта в логах:\n{logged}");
        assert!(
            logged.contains("prompt_chars"),
            "длина промпта — то единственное, что логируется:\n{logged}"
        );
    }
}
