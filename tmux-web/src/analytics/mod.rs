//! Аналитика использования Claude Code.
//!
//! Модуль читает транскрипты Claude Code (`~/.claude/projects/**/*.jsonl`)
//! и сворачивает их в дневные бакеты метрик: сообщения человека, паузы
//! перед ответом, токены, стоимость, инструменты, компакты, субагенты.
//!
//! ### Состав
//!
//! * [`parse`] — построчный разбор одного `.jsonl` в [`rollup::SessionRollup`].
//!   Узкие serde-структуры, дешёвый префильтр по подстроке `"type":"…"`,
//!   устойчивость к битым/усечённым строкам.
//! * [`rollup`] — структуры-накопители ([`rollup::DayBucket`],
//!   [`rollup::SessionRollup`], [`rollup::AnalyticsIndex`]), их слияние и
//!   выборка под фильтр (диапазон дат, проект, дневной/недельный бакет).
//!
//! * [`scan`] — обход `~/.claude/projects/*` и инкрементальный разбор по
//!   ключу `(len, mtime, parse_offset)`: неизменившийся файл не открывается,
//!   доросший дочитывается с оффсета, усохший или переписанный собирается
//!   заново. Параллелизм — `spawn_blocking` + семафор по числу ядер,
//!   прогресс — атомарные счётчики [`scan::ScanProgress`].
//! * [`cache`] — снимок индекса в `~/.forge/analytics-cache.json` через
//!   `forge-atomic`; поле `version` при несовпадении даёт полный пересбор.
//!
//! Ядро разбора намеренно не знает ни про сеть, ни про файловую систему:
//! `parse`/`rollup` читают поток байт, `scan`/`cache` — всё остальное.
//!
//! ### Ключевые правила (выведены из живых транскриптов)
//!
//! Правила разбора — не деталь реализации, а суть метрик; отклонение от них
//! делает всю аналитику неверной. Полный список — в документации
//! [`parse::parse_session`].
//!
//! ### Этот файл: хранилище и поверхность API
//!
//! [`AnalyticsStore`] — то, что лежит в `AppState` и что видят роуты
//! `/api/analytics/*`. Он сводит воедино три вещи, каждая из которых сама по
//! себе ничего не решает:
//!
//! 1. **Индекс в памяти** ([`rollup::AnalyticsIndex`]) — по нему строятся все
//!    ответы; файлы при запросе НЕ читаются.
//! 2. **Роллапы по файлам** — сырьё для следующего инкрементального прохода и
//!    содержимое снимка кэша.
//! 3. **Состояние индексации** ([`AnalyticsState`]) — `Idle` / `Scanning` /
//!    `Ready`, плюс живой прогресс из [`scan::ScanProgress`].
//!
//! #### Почему индексация ленивая
//!
//! На диске 2.7 ГБ транскриптов. Сканировать их на старте devforge значило бы
//! подарить каждому запуску секунды дисковой работы ради вкладки, которую в
//! этом запуске могут ни разу не открыть. Поэтому:
//!
//! * [`AnalyticsStore::init_lazy`] на старте читает ТОЛЬКО снимок
//!   `~/.forge/analytics-cache.json` — каталог `~/.claude/projects` при этом
//!   не открывается ни разу;
//! * первый скан стартует из [`AnalyticsStore::ensure_scan`], то есть по
//!   первому запросу к любому роуту аналитики.
//!
//! #### Почему запрос во время скана — это 200, а не ошибка
//!
//! Фронт рисует прогресс-бар, а не сообщение об ошибке. Ответ со `state`
//! `"scanning"` и заполненным `progress` — это ВАЛИДНЫЙ ответ: серии в нём
//! пустые (или частичные, если поднялся кэш), и клиент просто поллит
//! `/api/analytics/status` до `"ready"`. 202 или 503 заставили бы каждого
//! клиента лечить состояние отдельной веткой кода — и первая же забытая ветка
//! показала бы пользователю пустой экран без объяснения.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use self::rollup::{
    AnalyticsIndex, Bucket, DayBucket, Query, SessionRollup, NIGHT_HOURS, PAUSE_BUCKET_EDGES_MS,
};
use self::scan::{ScanProgress, ScanProgressSnapshot};

pub mod cache;
pub mod parse;
pub mod rollup;
pub mod scan;
pub mod text;

/// Границы гистограммы длины сообщения человека (символов).
pub const MSG_LEN_EDGES: [u32; 4] = [100, 300, 1_000, 3_000];

/// Сколько инструментов, моделей и проектов отдаём в сериях (топ-N).
const TOP_N: usize = 20;

/// Насколько «свежим» считается готовый индекс: пока с последнего прохода
/// прошло меньше этого времени, [`AnalyticsStore::ensure_scan`] нового
/// прохода не начинает.
///
/// Без этого порога дашборд сам себя загонял бы в вечный скан: каждый
/// запрос (включая поллинг `status` раз в полсекунды) запускал бы новый
/// обход всех файлов, состояние никогда не доходило бы до `ready`, а
/// прогресс-бар не исчезал бы с экрана. Проверено живым прогоном.
const REFRESH_MIN_INTERVAL_MS: i64 = 60_000;

// ===========================================================================
// Состояние
// ===========================================================================

/// Состояние индексации, каким его видит хранилище.
///
/// `Scanning` не несёт счётчиков: они живут в [`scan::ScanProgress`] и
/// читаются атомарно без взятия лока — иначе хендлер `status` во время скана
/// вставал бы в очередь за пишущим локом и «прогресс» переставал бы
/// обновляться ровно тогда, когда он нужен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyticsState {
    /// Индекс пуст, скан не запускался.
    Idle,
    /// Скан идёт; прогресс — в [`scan::ScanProgress`].
    Scanning,
    /// Индекс готов; значение — момент последнего успешного прохода (unix-мс).
    Ready { updated_at_ms: i64 },
}

impl AnalyticsState {
    /// Строковое имя состояния для JSON (`idle` / `scanning` / `ready`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AnalyticsState::Idle => "idle",
            AnalyticsState::Scanning => "scanning",
            AnalyticsState::Ready { .. } => "ready",
        }
    }
}

/// Содержимое хранилища под локом.
#[derive(Debug, Default)]
struct Data {
    /// Готовый к запросам сводный индекс.
    index: AnalyticsIndex,
    /// Роллапы по ключу-пути — сырьё следующего инкрементального прохода.
    files: BTreeMap<String, SessionRollup>,
    /// Момент последнего успешного прохода (unix-мс); 0 — прохода не было.
    updated_at_ms: i64,
    /// Идёт ли скан прямо сейчас (владелец правды — `progress.running`,
    /// это поле нужно, чтобы отличить `Idle` от `Ready` до первого скана).
    scanning: bool,
    /// Был ли хоть один успешный проход или подъём кэша.
    ready: bool,
    /// Последняя ошибка (не роняет состояние — только диагностика).
    last_error: Option<String>,
    /// Справочные плитки из `~/.claude.json` (глобальные счётчики CLI).
    globals: Option<GlobalUsageDto>,
    /// mtime `~/.claude.json` на момент чтения — по нему решается, нужно ли
    /// перечитывать файл (он большой, и парсить его на каждый запрос незачем).
    globals_mtime_ms: i64,
}

struct Inner {
    data: RwLock<Data>,
    progress: Arc<ScanProgress>,
    /// `~/.claude/projects`; `None` — нет `$HOME`, аналитика пуста, но жива.
    projects_dir: Option<PathBuf>,
    /// `~/.forge/analytics-cache.json`; `None` — снимок не пишем и не читаем.
    cache_path: Option<PathBuf>,
    /// `~/.claude.json`; `None` — справочные плитки не показываются.
    config_path: Option<PathBuf>,
}

/// Хранилище аналитики в `AppState`.
///
/// Cheap-clonable: клон — это `Arc::clone` и ничего больше, ни индекс, ни
/// роллапы не копируются.
///
/// # Инварианты
///
/// * Ни один публичный метод не держит лок через `.await` файловой работы:
///   скан и запись кэша идут в отдельных задачах, а лок берётся только на
///   время подмены данных.
/// * Второй скан не стартует, пока идёт первый ([`scan::try_begin`] —
///   compare-exchange по атомарному флагу).
#[derive(Clone)]
pub struct AnalyticsStore {
    inner: Arc<Inner>,
}

impl AnalyticsStore {
    /// Пустое хранилище с явными путями (тесты подставляют временные
    /// каталоги; продакшен зовёт [`AnalyticsStore::with_default_paths`]).
    pub fn new(
        projects_dir: Option<PathBuf>,
        cache_path: Option<PathBuf>,
        config_path: Option<PathBuf>,
    ) -> Self {
        AnalyticsStore {
            inner: Arc::new(Inner {
                data: RwLock::new(Data::default()),
                progress: Arc::new(ScanProgress::default()),
                projects_dir,
                cache_path,
                config_path,
            }),
        }
    }

    /// Хранилище с путями по умолчанию: `~/.claude/projects` и
    /// `~/.forge/analytics-cache.json`.
    pub fn with_default_paths() -> Self {
        Self::new(
            scan::claude_projects_dir(),
            cache::default_cache_path(),
            scan::claude_config_path(),
        )
    }

    /// Перечитывает `~/.claude.json`, если файл изменился с прошлого раза.
    ///
    /// Вызывается из [`AnalyticsStore::summary`] — то есть по требованию, а не
    /// на старте devforge: файл весит сотни килобайт, и разбирать его ради
    /// вкладки, которую могут не открыть, незачем. Повторные вызовы стоят
    /// одного `stat`: пока `mtime` тот же, разбор не повторяется.
    ///
    /// Отсутствие или битый файл — не ошибка: плитки просто не появятся.
    async fn refresh_globals(&self) {
        let Some(path) = self.inner.config_path.clone() else {
            return;
        };
        let probe = path.clone();
        let Ok(mtime) = tokio::task::spawn_blocking(move || scan::file_mtime_ms(&probe)).await
        else {
            return;
        };
        {
            let data = self.inner.data.read().await;
            if data.globals_mtime_ms == mtime && (data.globals.is_some() || mtime == 0) {
                return;
            }
        }
        let Ok(raw) = tokio::task::spawn_blocking(move || scan::read_global_usage(&path)).await
        else {
            return;
        };
        let mut data = self.inner.data.write().await;
        data.globals_mtime_ms = mtime;
        data.globals = raw.map(GlobalUsageDto::from_raw);
    }

    /// Подъём индекса из снимка кэша. **Скан не запускает.**
    ///
    /// Вызывается один раз при сборке `AppState`. Читает единственный файл —
    /// `~/.forge/analytics-cache.json`; каталога транскриптов не касается, так
    /// что старт devforge остаётся ровно таким же, каким был до аналитики.
    ///
    /// Снимка нет, он битый или другой версии — не ошибка: состояние
    /// останется `Idle`, а первый запрос к API соберёт индекс с нуля.
    pub async fn init_lazy(&self) {
        let Some(path) = self.inner.cache_path.clone() else {
            return;
        };
        let started = std::time::Instant::now();
        let Ok(Some(snapshot)) = tokio::task::spawn_blocking(move || cache::load(&path)).await
        else {
            return;
        };
        let mut index = AnalyticsIndex::default();
        for rollup in snapshot.files.values() {
            index.add_session(rollup);
        }
        let sessions = index.sessions;
        let files = snapshot.files.len();
        {
            let mut data = self.inner.data.write().await;
            data.index = index;
            data.files = snapshot.files;
            data.updated_at_ms = snapshot.updated_at_ms;
            data.ready = true;
        }
        tracing::info!(
            sessions,
            files,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "analytics: индекс поднят из снимка кэша"
        );
    }

    /// Запускает скан, если он нужен и не идёт. Идемпотентно.
    ///
    /// Возвращает `true`, если скан был запущен именно этим вызовом.
    /// `full = true` — полный пересбор (кэш и накопленные роллапы
    /// отбрасываются), `false` — обычный инкрементальный проход.
    ///
    /// # Когда инкрементальный проход НЕ начинается
    ///
    /// Если индекс уже готов и последний проход был меньше
    /// [`REFRESH_MIN_INTERVAL_MS`] назад. Это не оптимизация, а условие
    /// работоспособности: каждый роут аналитики зовёт `ensure_scan`, и
    /// поллинг `status` без этого порога перезапускал бы обход бесконечно —
    /// состояние никогда не становилось бы `ready`.
    ///
    /// Сам скан живёт в отдельной задаче: хендлер не ждёт его завершения и
    /// сразу отдаёт клиенту состояние с прогрессом.
    pub async fn ensure_scan(&self, full: bool) -> bool {
        if self.inner.progress.snapshot().running {
            return false;
        }
        let Some(projects_dir) = self.inner.projects_dir.clone() else {
            return false;
        };
        let prev = {
            let mut data = self.inner.data.write().await;
            if data.scanning {
                return false;
            }
            if !full && data.ready && now_ms() - data.updated_at_ms < REFRESH_MIN_INTERVAL_MS {
                return false;
            }
            data.scanning = true;
            if full {
                data.files.clear();
                data.index = AnalyticsIndex::default();
                data.ready = false;
            }
            data.files.clone()
        };

        let store = self.clone();
        tokio::spawn(async move {
            store.run_scan(projects_dir, prev).await;
        });
        true
    }

    /// Тело фонового прохода: скан → подмена индекса → запись снимка.
    async fn run_scan(&self, projects_dir: PathBuf, prev: BTreeMap<String, SessionRollup>) {
        let progress = Arc::clone(&self.inner.progress);
        let outcome = scan::scan_incremental(projects_dir, prev, progress).await;

        let Some(outcome) = outcome else {
            // Скан уже шёл — флаг `scanning` принадлежит ему, не нам.
            let mut data = self.inner.data.write().await;
            data.scanning = false;
            return;
        };

        let updated_at_ms = now_ms();
        let files_for_cache = outcome.files.clone();
        {
            let mut data = self.inner.data.write().await;
            data.index = outcome.index;
            data.files = outcome.files;
            data.updated_at_ms = updated_at_ms;
            data.scanning = false;
            data.ready = true;
        }
        tracing::info!(
            sessions = files_for_cache.len(),
            files_read = outcome.files_read,
            bytes_read = outcome.bytes_read,
            errors = outcome.errors,
            elapsed_ms = outcome.elapsed_ms,
            "analytics: скан завершён"
        );

        let Some(path) = self.inner.cache_path.clone() else {
            return;
        };
        let snapshot = cache::CacheSnapshot::new(files_for_cache, updated_at_ms);
        let result =
            tokio::task::spawn_blocking(move || cache::save(&path, &snapshot)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let msg = format!("не удалось сохранить снимок кэша: {e:#}");
                tracing::warn!(error = %e, "analytics: снимок кэша не сохранён");
                self.inner.data.write().await.last_error = Some(msg);
            }
            Err(e) => {
                tracing::warn!(error = %e, "analytics: задача записи снимка упала");
            }
        }
    }

    /// Полный пересбор: удаляет снимок кэша и запускает скан с нуля.
    ///
    /// Повторный вызов во время идущего скана второго прохода НЕ порождает —
    /// возвращает текущее состояние как есть.
    pub async fn rescan(&self) -> StatusDto {
        if let Some(path) = self.inner.cache_path.clone() {
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await;
        }
        self.ensure_scan(true).await;
        self.status().await
    }

    /// Готовый индекс остаётся `Ready` и во время фонового обновления.
    ///
    /// `Scanning` означает ровно одно: «показывать нечего, идёт первая
    /// сборка». Если данные уже есть, откат в `Scanning` из-за
    /// освежающего прохода спрятал бы за прогресс-баром вполне рабочий
    /// дашборд. Что проход идёт, клиент видит по `progress.running`.
    fn state_of(data: &Data) -> AnalyticsState {
        if data.ready {
            AnalyticsState::Ready {
                updated_at_ms: data.updated_at_ms,
            }
        } else if data.scanning {
            AnalyticsState::Scanning
        } else {
            AnalyticsState::Idle
        }
    }

    /// Снимок состояния и прогресса для `GET /api/analytics/status`.
    pub async fn status(&self) -> StatusDto {
        let progress = self.inner.progress.snapshot();
        let data = self.inner.data.read().await;
        let state = Self::state_of(&data);
        StatusDto {
            state: state.as_str().to_string(),
            done: progress.done,
            total: progress.total,
            errors: progress.errors,
            files_read: progress.files_read,
            bytes_read: progress.bytes_read,
            updated_at: data.updated_at_ms,
            sessions: data.index.sessions,
            files: data.files.len(),
            projects: data.index.projects.len(),
            last_error: data.last_error.clone(),
        }
    }

    /// Список проектов для селектора дашборда, по убыванию последней
    /// активности.
    ///
    /// Во время скана отдаёт то, что уже известно (из кэша или из
    /// завершившегося прохода) — пустой список тоже валидный ответ.
    pub async fn projects(&self) -> Vec<ProjectDto> {
        let data = self.inner.data.read().await;
        let mut out: Vec<ProjectDto> = data
            .index
            .projects
            .values()
            .map(|p| {
                let messages = p.days.values().map(|d| d.human_messages).sum();
                let last_date = p.days.keys().next_back().cloned().unwrap_or_default();
                ProjectDto {
                    id: p.dir.clone(),
                    path: p.cwd.clone(),
                    display_name: display_name(&p.cwd, &p.dir),
                    sessions: p.sessions,
                    messages,
                    last_active_date: last_date,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.last_active_date
                .cmp(&a.last_active_date)
                .then_with(|| b.messages.cmp(&a.messages))
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    /// Сводка за период для `GET /api/analytics/summary`.
    ///
    /// Считается ЦЕЛИКОМ по индексу в памяти: ни одного обращения к диску,
    /// поэтому ответ на полном архиве укладывается в единицы миллисекунд.
    ///
    /// Если индекс ещё не готов — ответ всё равно 200: `state` = `scanning`
    /// (или `idle`), серии пустые, `progress` заполнен.
    pub async fn summary(&self, params: &SummaryParams) -> SummaryDto {
        self.refresh_globals().await;
        let progress = self.inner.progress.snapshot();
        let data = self.inner.data.read().await;
        let state = Self::state_of(&data);

        let bucket = params.bucket();
        let (from, to) = resolve_range(&data.index, params);
        let query = Query {
            from: Some(from.clone()),
            to: Some(to.clone()),
            project: params.project.clone().filter(|p| !p.is_empty()),
            bucket,
        };

        let buckets = data.index.query(&query);
        let total = data.index.total(&query);

        let (prev_from, prev_to) = previous_range(&from, &to);
        let prev_total = data.index.total(&Query {
            from: Some(prev_from.clone()),
            to: Some(prev_to.clone()),
            ..query.clone()
        });

        SummaryDto {
            state: state.as_str().to_string(),
            progress: ProgressDto::from(progress),
            updated_at: data.updated_at_ms,
            range: RangeDto {
                from,
                to,
                bucket: match bucket {
                    Bucket::Day => "day",
                    Bucket::Week => "week",
                }
                .to_string(),
                project: query.project.clone(),
                previous_from: prev_from,
                previous_to: prev_to,
            },
            kpi: KpiDto::from_bucket(&total),
            kpi_previous: KpiDto::from_bucket(&prev_total),
            series: SeriesDto::build(&buckets, &total, &data.index, &query),
            globals: data.globals.clone(),
        }
    }
}

/// Человекочитаемое имя проекта: последний сегмент `cwd`, иначе закодированное
/// имя каталога.
fn display_name(cwd: &str, dir: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => dir.to_string(),
    }
}

/// Текущее время в unix-мс.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Границы периода: явные `from`/`to` или полный диапазон индекса.
///
/// Пустой индекс даёт сегодняшний день с обеих сторон — так серии выходят
/// пустыми, но диапазон остаётся осмысленным для подписи оси.
fn resolve_range(index: &AnalyticsIndex, params: &SummaryParams) -> (String, String) {
    let days = match params.project.as_deref() {
        Some(p) if !p.is_empty() => index.projects.get(p).map(|pr| &pr.days),
        _ => Some(&index.days),
    };
    let span = days.and_then(|d| Some((d.keys().next()?.clone(), d.keys().next_back()?.clone())));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let (min, max) = span.unwrap_or_else(|| (today.clone(), today.clone()));
    let from = params
        .from
        .clone()
        .filter(|s| is_date(s))
        .unwrap_or_else(|| min.clone());
    let to = params
        .to
        .clone()
        .filter(|s| is_date(s))
        .unwrap_or_else(|| max.clone());
    if from > to {
        (to.clone(), to)
    } else {
        (from, to)
    }
}

/// Предыдущий период той же длины, вплотную перед `from`.
///
/// Именно «той же длины», а не «предыдущий календарный месяц»: плитки
/// показывают дельту к сопоставимому отрезку, и семь дней должны сравниваться
/// с семью, даже если период начался в середине недели.
fn previous_range(from: &str, to: &str) -> (String, String) {
    let (Some(a), Some(b)) = (parse_date(from), parse_date(to)) else {
        return (from.to_string(), to.to_string());
    };
    let len = (b - a).num_days() + 1;
    let prev_to = a - chrono::Duration::days(1);
    let prev_from = prev_to - chrono::Duration::days(len - 1);
    (
        prev_from.format("%Y-%m-%d").to_string(),
        prev_to.format("%Y-%m-%d").to_string(),
    )
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn is_date(s: &str) -> bool {
    parse_date(s).is_some()
}

// ===========================================================================
// Параметры запроса
// ===========================================================================

/// Query-параметры `GET /api/analytics/summary`.
///
/// Все поля опциональны и все невалидные значения молча заменяются
/// умолчаниями: дашборд не должен уметь получить 400 из-за опечатки в
/// адресной строке, а `bucket=quarter` — это `day`, а не ошибка.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SummaryParams {
    /// Начало периода, `YYYY-MM-DD` (включительно).
    pub from: Option<String>,
    /// Конец периода, `YYYY-MM-DD` (включительно).
    pub to: Option<String>,
    /// Закодированное имя каталога проекта; пусто/отсутствует — все проекты.
    pub project: Option<String>,
    /// `day` (умолчание) или `week`.
    pub bucket: Option<String>,
}

impl SummaryParams {
    fn bucket(&self) -> Bucket {
        match self.bucket.as_deref() {
            Some("week") => Bucket::Week,
            _ => Bucket::Day,
        }
    }
}

// ===========================================================================
// DTO
// ===========================================================================

/// `GET /api/analytics/status` — состояние и прогресс индексации.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDto {
    /// `idle` | `scanning` | `ready`.
    pub state: String,
    /// Обработано файлов в текущем/последнем скане.
    pub done: usize,
    /// Всего файлов в текущем/последнем скане.
    pub total: usize,
    pub errors: usize,
    pub files_read: usize,
    pub bytes_read: u64,
    /// Момент последнего успешного прохода, unix-мс (0 — прохода не было).
    pub updated_at: i64,
    pub sessions: u64,
    /// Сколько файлов лежит в индексе.
    pub files: usize,
    pub projects: usize,
    /// Последняя негромкая ошибка (например, снимок кэша не записался).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Прогресс скана внутри `summary` — чтобы фронт рисовал полосу, не делая
/// второго запроса.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressDto {
    pub done: usize,
    pub total: usize,
    pub running: bool,
}

impl From<ScanProgressSnapshot> for ProgressDto {
    fn from(s: ScanProgressSnapshot) -> Self {
        ProgressDto {
            done: s.done,
            total: s.total,
            running: s.running,
        }
    }
}

/// Элемент списка проектов для селектора.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    /// Закодированное имя каталога `~/.claude/projects/<dir>` — оно же
    /// значение параметра `project` в `summary`.
    pub id: String,
    /// Рабочий каталог (`cwd`) последней виденной сессии проекта.
    pub path: String,
    /// Последний сегмент `path` — то, что видит человек в селекторе.
    pub display_name: String,
    pub sessions: u64,
    /// Сообщений человека за всю историю проекта.
    pub messages: u64,
    /// Последний день с активностью, `YYYY-MM-DD`.
    pub last_active_date: String,
}

/// Границы и параметры выборки, возвращённые сервером (фронт рисует по ним
/// подписи и не гадает, что именно сервер понял под «всё время»).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeDto {
    pub from: String,
    pub to: String,
    pub bucket: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub previous_from: String,
    pub previous_to: String,
}

/// `GET /api/analytics/summary` — всё, что нужно дашборду за один запрос.
///
/// Ответ ВСЕГДА 200, даже когда индекс ещё собирается: `state` = `scanning`,
/// `progress` заполнен, серии пустые. Клиент по этому же полю решает, рисовать
/// ли прогресс-бар, и не различает «ошибку» и «ещё не готово».
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDto {
    /// `idle` | `scanning` | `ready`.
    pub state: String,
    pub progress: ProgressDto,
    /// Момент последнего успешного прохода, unix-мс.
    pub updated_at: i64,
    pub range: RangeDto,
    /// Плитки за выбранный период.
    pub kpi: KpiDto,
    /// Те же плитки за предыдущий период такой же длины — для дельт.
    pub kpi_previous: KpiDto,
    pub series: SeriesDto,
    /// Справочные плитки из `~/.claude.json`. Отсутствуют, если файла нет.
    ///
    /// Это ГЛОБАЛЬНЫЕ счётчики CLI за всё время: они не фильтруются ни
    /// периодом, ни проектом, и в UI подписаны именно так, чтобы их числа не
    /// путали с остальными метриками дашборда.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub globals: Option<GlobalUsageDto>,
}

/// Строка справочной плитки: имя и глобальный счётчик использований.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDto {
    pub name: String,
    pub usage_count: u64,
    /// Момент последнего использования, unix-мс (0 — неизвестно).
    pub last_used_at: i64,
}

/// Глобальные счётчики CLI из `~/.claude.json`.
///
/// Из транскриптов эти числа не выводятся: навык, плагин и «глобальный»
/// счётчик инструментов CLI пишет только в свой конфиг. Поэтому они и живут
/// отдельным блоком ответа, а не среди серий — период и проект к ним
/// неприменимы.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalUsageDto {
    pub skills: Vec<UsageDto>,
    pub plugins: Vec<UsageDto>,
    pub tools: Vec<UsageDto>,
}

impl GlobalUsageDto {
    /// Сортирует каждую карту по убыванию счётчика и оставляет топ-[`TOP_N`].
    fn from_raw(raw: scan::GlobalUsage) -> Self {
        GlobalUsageDto {
            skills: top_usage(raw.skill_usage),
            plugins: top_usage(raw.plugin_usage),
            tools: top_usage(raw.tool_usage),
        }
    }
}

/// Топ-[`TOP_N`] счётчиков по убыванию; нулевые записи отбрасываются —
/// «плагин, которым ни разу не пользовались» в плитке «топ» только шумит.
fn top_usage(src: BTreeMap<String, scan::UsageEntry>) -> Vec<UsageDto> {
    let mut out: Vec<UsageDto> = src
        .into_iter()
        .filter(|(_, e)| e.usage_count > 0)
        .map(|(name, e)| UsageDto {
            name,
            usage_count: e.usage_count,
            last_used_at: e.last_used_at,
        })
        .collect();
    out.sort_by(|a, b| {
        b.usage_count
            .cmp(&a.usage_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(TOP_N);
    out
}

/// KPI-плитки за один период.
///
/// Тот же тип отдаётся и для предыдущего периода (`kpiPrevious`) — дельты
/// считает фронт, чтобы формат плитки менялся без правки бэкенда.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KpiDto {
    pub human_messages: u64,
    pub assistant_messages: u64,
    pub sessions: u64,
    pub messages_per_session: f64,
    pub avg_msg_len: f64,
    pub median_msg_len: u64,
    /// Медианная пауза перед ответом (мс), без пауз «отошёл».
    pub median_pause_ms: u64,
    /// p90 той же паузы.
    pub p90_pause_ms: u64,
    /// Доля ответов быстрее 10 с — «не читая», 0..1.
    pub share_under10s: f64,
    /// Пауз длиннее 30 мин («отошёл»); в медиану не входят.
    pub away_count: u64,
    /// Суммарное время генерации (мс).
    pub gen_ms: u64,
    /// Суммарное время инструментов (мс).
    pub tool_ms: u64,
    /// «Человеко-время» — сумма пауз перед ответом (мс).
    pub human_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_create: u64,
    pub tokens_thinking: u64,
    pub cost_usd: f64,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub tool_calls: u64,
    pub interruptions: u64,
    pub compacts: u64,
    pub subagents: u64,

    // --- Разнообразие (чарт 11) и ночная активность (чарт 2) ---
    /// TTR за период: оценка числа различных слов / все слова, 0..1.
    pub ttr: f64,
    /// Доля повторяющихся промптов, 0..1.
    pub repeat_share: f64,
    /// Энтропия Шеннона по вызовам инструментов, в битах.
    pub tool_entropy_bits: f64,
    /// Доля слэш-команд среди сообщений человека, 0..1.
    pub slash_share: f64,
    /// Доля односложных подтверждений («ок», «продолжай»), 0..1.
    pub ack_share: f64,
    /// Сообщения человека в часы 00:00–05:59 локального времени.
    pub night_messages: u64,
    /// Их доля от всех сообщений человека, 0..1.
    pub night_share: f64,
}

impl KpiDto {
    /// Свёртка суммарного бакета периода в плитки.
    ///
    /// Медиана и p90 берутся из [`rollup::Samples`], куда паузы «отошёл» не
    /// попадают по построению (см. `SessionRollup::on_human_message`), поэтому
    /// здесь никакой дополнительной фильтрации не нужно — и не должно быть:
    /// вторая фильтрация «на всякий случай» отрезала бы честные длинные паузы.
    pub fn from_bucket(b: &DayBucket) -> Self {
        let pauses = b.pauses.count;
        KpiDto {
            human_messages: b.human_messages,
            assistant_messages: b.assistant_messages,
            sessions: b.sessions_started,
            messages_per_session: ratio(b.human_messages as f64, b.sessions_started as f64),
            avg_msg_len: b.msg_lens.mean(),
            median_msg_len: b.msg_lens.p50().unwrap_or(0),
            median_pause_ms: b.pauses.p50().unwrap_or(0),
            p90_pause_ms: b.pauses.p90().unwrap_or(0),
            share_under10s: ratio(b.quick_replies as f64, pauses as f64),
            away_count: b.away_count,
            gen_ms: b.gen_ms,
            tool_ms: b.tool_ms,
            human_ms: b.human_ms,
            tokens_in: b.tokens_in,
            tokens_out: b.tokens_out,
            tokens_cache_read: b.cache_read,
            tokens_cache_create: b.cache_create,
            tokens_thinking: b.thinking,
            cost_usd: b.cost_usd,
            lines_added: b.lines_added,
            lines_removed: b.lines_removed,
            tool_calls: b.tool_calls,
            interruptions: b.interruptions,
            compacts: b.compacts,
            subagents: b.subagent_starts,
            ttr: b.ttr(),
            repeat_share: b.repeat_share(),
            tool_entropy_bits: b.tool_entropy_bits(),
            slash_share: b.slash_share(),
            ack_share: b.ack_share(),
            night_messages: b.night_messages(),
            night_share: ratio(b.night_messages() as f64, b.human_messages as f64),
        }
    }
}

fn ratio(a: f64, b: f64) -> f64 {
    if b <= 0.0 {
        0.0
    } else {
        a / b
    }
}

/// Строка топ-таблицы инструментов (чарт 6).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDto {
    pub name: String,
    pub calls: u64,
    pub interrupted: u64,
}

/// Строка разбивки по моделям (чарт 7).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDto {
    pub name: String,
    pub messages: u64,
    pub cost_usd: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

/// Строка топа проектов (чарт 8).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSeriesDto {
    pub id: String,
    pub display_name: String,
    pub messages: u64,
    pub cost_usd: f64,
    pub lines_added: u64,
    pub lines_removed: u64,
}

/// Все временные ряды дашборда.
///
/// Одно поле — один чарт (номера по плану). Длина каждого массива по дням
/// равна длине [`SeriesDto::buckets`]: фронт индексирует их параллельно и не
/// сопоставляет по датам.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesDto {
    /// Подписи корзин: `YYYY-MM-DD` (для `week` — понедельник недели).
    pub buckets: Vec<String>,

    // Чарт 1 — сообщения по дням + новые сессии.
    pub human_messages: Vec<u64>,
    pub sessions: Vec<u64>,

    // Чарт 3 — пауза перед ответом.
    /// Гистограмма пауз за весь период: `<5 c`, `5–15 c`, `15–60 c`,
    /// `1–5 мин`, `>5 мин`.
    pub pause_buckets: Vec<u64>,
    /// Границы гистограммы (мс) — чтобы подписи оси жили на сервере.
    pub pause_bucket_edges_ms: Vec<i64>,
    pub pause_p50_ms: Vec<u64>,
    pub pause_p90_ms: Vec<u64>,
    pub quick_replies: Vec<u64>,

    // Чарт 4 — токены и стоимость по дням.
    pub tokens_in: Vec<u64>,
    pub tokens_out: Vec<u64>,
    pub cache_read: Vec<u64>,
    pub cache_create: Vec<u64>,
    pub cost_usd: Vec<f64>,

    // Чарт 5 — куда уходит время.
    pub gen_ms: Vec<u64>,
    pub tool_ms: Vec<u64>,
    pub human_ms: Vec<u64>,

    // Чарт 12 — доля thinking в выводе.
    pub thinking: Vec<u64>,

    // Чарт 10 — длительность хода ассистента.
    pub turn_p50_ms: Vec<u64>,
    pub turn_p90_ms: Vec<u64>,

    // Чарт 9 — длина сообщений.
    /// Гистограмма длин за период по [`MSG_LEN_EDGES`].
    pub msg_len_buckets: Vec<u64>,
    pub msg_len_edges: Vec<u32>,
    pub msg_len_avg: Vec<f64>,

    // Чарт 11 — разнообразие: признаки автопилота по дням.
    pub ttr: Vec<f64>,
    pub repeat_share: Vec<f64>,
    pub tool_entropy_bits: Vec<f64>,
    pub unique_projects: Vec<u64>,
    pub slash_share: Vec<f64>,
    pub ack_share: Vec<f64>,

    // Чарт 2 — тепловая карта «час × день недели» за весь период.
    /// Матрица 24×7: `[час][день недели]`, день недели 0 = понедельник,
    /// час — в ЛОКАЛЬНОЙ зоне машины (в транскриптах таймстемпы UTC).
    pub hour_x_weekday: Vec<Vec<u32>>,
    /// Ночные сообщения по дням (часы 00:00–05:59 локального времени).
    pub night_messages: Vec<u64>,
    /// Сколько первых часов суток считаются ночью — чтобы фронт подчёркивал
    /// ровно те же столбцы, что суммирует бэкенд.
    pub night_hours: u32,

    // Чарт 13 — версии CLI по дням.
    /// Для каждой корзины — версии Claude Code, писавшие записи в этот день
    /// (пустой массив = данных о версии нет, день рисуется нейтральным).
    pub versions: Vec<Vec<String>>,

    // Чарты 6, 7, 8 — топы за период.
    pub tools: Vec<ToolDto>,
    pub models: Vec<ModelDto>,
    pub projects: Vec<ProjectSeriesDto>,
}

impl SeriesDto {
    fn build(
        buckets: &[DayBucket],
        total: &DayBucket,
        index: &AnalyticsIndex,
        query: &Query,
    ) -> Self {
        let n = buckets.len();
        let mut s = SeriesDto {
            buckets: Vec::with_capacity(n),
            pause_bucket_edges_ms: PAUSE_BUCKET_EDGES_MS.to_vec(),
            pause_buckets: total.pause_buckets.to_vec(),
            msg_len_buckets: total.msg_lens.bucket_counts(&MSG_LEN_EDGES),
            msg_len_edges: MSG_LEN_EDGES.to_vec(),
            hour_x_weekday: heatmap_matrix(total),
            night_hours: NIGHT_HOURS as u32,
            ..Default::default()
        };
        let unique_projects = unique_projects_per_bucket(index, query, &s_bucket_keys(buckets));
        for b in buckets {
            s.buckets.push(b.date.clone());
            s.human_messages.push(b.human_messages);
            s.sessions.push(b.sessions_started);
            s.pause_p50_ms.push(b.pauses.p50().unwrap_or(0));
            s.pause_p90_ms.push(b.pauses.p90().unwrap_or(0));
            s.quick_replies.push(b.quick_replies);
            s.tokens_in.push(b.tokens_in);
            s.tokens_out.push(b.tokens_out);
            s.cache_read.push(b.cache_read);
            s.cache_create.push(b.cache_create);
            s.cost_usd.push(b.cost_usd);
            s.gen_ms.push(b.gen_ms);
            s.tool_ms.push(b.tool_ms);
            s.human_ms.push(b.human_ms);
            s.thinking.push(b.thinking);
            s.turn_p50_ms.push(b.turn_ms.p50().unwrap_or(0));
            s.turn_p90_ms.push(b.turn_ms.p90().unwrap_or(0));
            s.msg_len_avg.push(b.msg_lens.mean());
            s.ttr.push(b.ttr());
            s.repeat_share.push(b.repeat_share());
            s.tool_entropy_bits.push(b.tool_entropy_bits());
            s.slash_share.push(b.slash_share());
            s.ack_share.push(b.ack_share());
            s.night_messages.push(b.night_messages());
            s.versions.push(b.versions.iter().cloned().collect());
        }
        s.unique_projects = unique_projects;

        s.tools = top_tools(total);
        s.models = top_models(total);
        s.projects = top_projects(index, query);
        s
    }
}

/// Ключи корзин ряда — в том же порядке, в каком идут сами корзины.
fn s_bucket_keys(buckets: &[DayBucket]) -> Vec<String> {
    buckets.iter().map(|b| b.date.clone()).collect()
}

/// Матрица «час × день недели» за период.
///
/// Всегда 24×7, даже когда данных нет: чарт рисует ровную карту нулей, а не
/// «пусто», и человек видит, что метрика существует, просто активности не
/// было. Часы — в локальной зоне машины (см. `SessionRollup::on_human_message`).
fn heatmap_matrix(total: &DayBucket) -> Vec<Vec<u32>> {
    if total.hour_x_weekday.is_empty() {
        return vec![vec![0u32; 7]; 24];
    }
    total.hour_x_weekday.clone()
}

/// Число различных проектов, в которых человек писал, по корзинам ряда.
///
/// Считается по проектным дням, а не по суммарному бакету: бакет уже сложен
/// по всем проектам, и «из скольких проектов он сложен» в нём не осталось.
/// Порядок и длина результата повторяют `keys` — фронт индексирует ряды
/// параллельно.
fn unique_projects_per_bucket(
    index: &AnalyticsIndex,
    query: &Query,
    keys: &[String],
) -> Vec<u64> {
    let mut per_key: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for project in index.projects.values() {
        if let Some(sel) = query.project.as_deref() {
            if project.dir != sel {
                continue;
            }
        }
        for (date, day) in &project.days {
            if day.human_messages == 0 {
                continue;
            }
            if let Some(from) = query.from.as_deref() {
                if date.as_str() < from {
                    continue;
                }
            }
            if let Some(to) = query.to.as_deref() {
                if date.as_str() > to {
                    continue;
                }
            }
            let key = rollup::bucket_key(date, query.bucket);
            if let Some(slot) = keys.iter().find(|k| **k == key) {
                per_key
                    .entry(slot.as_str())
                    .or_default()
                    .insert(project.dir.as_str());
            }
        }
    }
    keys.iter()
        .map(|k| per_key.get(k.as_str()).map(|s| s.len() as u64).unwrap_or(0))
        .collect()
}

/// Топ инструментов периода по числу вызовов.
fn top_tools(total: &DayBucket) -> Vec<ToolDto> {
    let mut out: Vec<ToolDto> = total
        .tools
        .iter()
        .map(|(name, st)| ToolDto {
            name: name.clone(),
            calls: st.calls,
            interrupted: st.interrupted,
        })
        .collect();
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));
    out.truncate(TOP_N);
    out
}

/// Разбивка периода по моделям, по убыванию стоимости.
fn top_models(total: &DayBucket) -> Vec<ModelDto> {
    let mut out: Vec<ModelDto> = total
        .models
        .iter()
        .map(|(name, st)| ModelDto {
            name: name.clone(),
            messages: st.messages,
            cost_usd: st.cost_usd,
            tokens_in: st.tokens_in,
            tokens_out: st.tokens_out,
        })
        .collect();
    out.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.messages.cmp(&a.messages))
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(TOP_N);
    out
}

/// Топ проектов за период.
///
/// Считается по КАЖДОМУ проекту отдельным запросом к индексу, а не по
/// суммарному бакету: бакет уже сложен по всем проектам и разложить его
/// обратно нельзя. Фильтр `project` при этом уважается — если выбран один
/// проект, в топе будет он один, и это ровно то, что показывает дашборд.
fn top_projects(index: &AnalyticsIndex, query: &Query) -> Vec<ProjectSeriesDto> {
    let mut out: Vec<ProjectSeriesDto> = index
        .projects
        .values()
        .filter(|p| match query.project.as_deref() {
            Some(sel) => p.dir == sel,
            None => true,
        })
        .map(|p| {
            let total = index.total(&Query {
                project: Some(p.dir.clone()),
                bucket: Bucket::Day,
                from: query.from.clone(),
                to: query.to.clone(),
            });
            ProjectSeriesDto {
                id: p.dir.clone(),
                display_name: display_name(&p.cwd, &p.dir),
                messages: total.human_messages,
                cost_usd: total.cost_usd,
                lines_added: total.lines_added,
                lines_removed: total.lines_removed,
            }
        })
        .filter(|p| p.messages > 0 || p.cost_usd > 0.0)
        .collect();
    out.sort_by(|a, b| {
        b.messages
            .cmp(&a.messages)
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    out.truncate(TOP_N);
    out
}
