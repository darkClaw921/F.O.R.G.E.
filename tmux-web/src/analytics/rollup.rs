//! Структуры-накопители аналитики: дневные бакеты, роллап сессии, сводный
//! индекс, их слияние и выборка под фильтр.
//!
//! ### Зачем дневные бакеты
//!
//! Все чарты дашборда — временные ряды по дням (или неделям) с фильтром по
//! проекту. Поэтому единица хранения — [`DayBucket`]: всё, что случилось за
//! один календарный день (в локальной зоне машины) в рамках одной сессии.
//! Слияние роллапов — это поэлементное сложение бакетов, что делает и
//! инкрементальное дочитывание файла, и объединение 1300+ сессий дешёвыми.
//!
//! ### Хранение распределений
//!
//! Медиана и перцентили паузы «читает ли код» — ключевая метрика запроса,
//! считать её по пяти отображаемым бакетам нельзя (слишком грубо). Хранить
//! же сырые сэмплы для всего архива дорого. Компромисс — [`Samples`]:
//! точный вектор значений до [`MAX_KEPT_SAMPLES`] штук на день плюс
//! логарифмическая гистограмма на 128 бинов. Пока сэмплов немного (обычный
//! случай) перцентили точны; после переполнения — оцениваются по
//! гистограмме с относительной погрешностью не хуже ~12 % на бин.
//! Гистограмма к тому же складывается при слиянии без потерь.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

use super::parse::{Record, AWAY_THRESHOLD_MS, QUICK_REPLY_MS};
use super::text::HumanText;
use crate::claude_memory::encode_project_dir;

/// Сколько точных сэмплов распределения храним на один день до перехода на
/// оценку по гистограмме.
pub const MAX_KEPT_SAMPLES: usize = 20_000;

/// Число бинов логарифмической гистограммы: 32 октавы × 4 суб-бина.
const HIST_BINS: usize = 128;
/// Суб-бинов на октаву (степень двойки).
const HIST_SUB: u32 = 4;

// ---------------------------------------------------------------------------
// Распределения
// ---------------------------------------------------------------------------

/// Логарифмическая гистограмма положительных величин.
///
/// Значения 0..3 попадают в собственные бины (точные), дальше бин — это
/// «октава + два старших бита мантиссы», то есть шаг ~19 % по значению.
/// Пустая гистограмма не занимает памяти (`bins` пустой вектор).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogHist {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bins: Vec<u32>,
}

impl LogHist {
    fn bin_index(v: u32) -> usize {
        if v < HIST_SUB {
            return v as usize;
        }
        let octave = 31 - v.leading_zeros();
        let sub = (v >> (octave - 2)) & (HIST_SUB - 1);
        ((octave * HIST_SUB + sub) as usize).min(HIST_BINS - 1)
    }

    /// Представитель бина — середина его диапазона значений.
    fn bin_value(idx: usize) -> u64 {
        if idx < HIST_SUB as usize {
            return idx as u64;
        }
        let octave = idx as u32 / HIST_SUB;
        let sub = idx as u32 % HIST_SUB;
        let lo = (u64::from(HIST_SUB + sub)) << (octave - 2);
        let hi = (u64::from(HIST_SUB + sub + 1)) << (octave - 2);
        (lo + hi) / 2
    }

    pub fn add(&mut self, v: u32) {
        if self.bins.is_empty() {
            self.bins = vec![0; HIST_BINS];
        }
        self.bins[Self::bin_index(v)] += 1;
    }

    pub fn merge(&mut self, other: &LogHist) {
        if other.bins.is_empty() {
            return;
        }
        if self.bins.is_empty() {
            self.bins = other.bins.clone();
            return;
        }
        for (a, b) in self.bins.iter_mut().zip(other.bins.iter()) {
            *a += *b;
        }
    }

    /// Оценка квантиля `q` ∈ [0, 1] по гистограмме.
    fn quantile(&self, q: f64, count: u64) -> Option<u64> {
        if self.bins.is_empty() || count == 0 {
            return None;
        }
        let target = ((count as f64 - 1.0) * q).round() as u64;
        let mut seen = 0u64;
        for (idx, n) in self.bins.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            seen += u64::from(*n);
            if seen > target {
                return Some(Self::bin_value(idx));
            }
        }
        self.bins
            .iter()
            .rposition(|n| *n > 0)
            .map(Self::bin_value)
    }
}

/// Накопитель распределения: сумма, счётчик, точные сэмплы (до лимита) и
/// логарифмическая гистограмма.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Samples {
    pub count: u64,
    pub sum: u64,
    /// Точные значения; обрезаются на [`MAX_KEPT_SAMPLES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<u32>,
    #[serde(default, skip_serializing_if = "LogHist::is_empty")]
    pub hist: LogHist,
}

impl LogHist {
    fn is_empty(&self) -> bool {
        self.bins.is_empty()
    }
}

impl Samples {
    pub fn push(&mut self, v: u32) {
        self.count += 1;
        self.sum += u64::from(v);
        self.hist.add(v);
        if self.kept.len() < MAX_KEPT_SAMPLES {
            self.kept.push(v);
        }
    }

    pub fn merge(&mut self, other: &Samples) {
        self.count += other.count;
        self.sum += other.sum;
        self.hist.merge(&other.hist);
        let room = MAX_KEPT_SAMPLES.saturating_sub(self.kept.len());
        if room > 0 {
            self.kept
                .extend(other.kept.iter().take(room).copied());
        }
    }

    // Читается тестами и будущими метриками Фазы 7; в самом бинаре пока
    // никто не спрашивает «пусто ли распределение», и на этом одном методе
    // подключать `allow(dead_code)` на весь модуль было бы дороже.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }

    /// Квантиль `q` ∈ [0, 1].
    ///
    /// Пока все сэмплы сохранены точно — считается по ним; после
    /// переполнения — оценивается по логарифмической гистограмме.
    pub fn quantile(&self, q: f64) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        if self.kept.len() as u64 == self.count {
            let mut v = self.kept.clone();
            v.sort_unstable();
            let idx = (((v.len() - 1) as f64) * q).round() as usize;
            return Some(u64::from(v[idx]));
        }
        self.hist.quantile(q, self.count)
    }

    pub fn p50(&self) -> Option<u64> {
        self.quantile(0.5)
    }

    pub fn p90(&self) -> Option<u64> {
        self.quantile(0.9)
    }

    /// Раскладывает распределение по границам `edges` (возрастающим).
    ///
    /// Возвращает `edges.len() + 1` счётчиков: `[< edges[0], edges[0]..edges[1],
    /// …, >= edges.last()]`. Пока все сэмплы сохранены точно — считает по ним;
    /// после переполнения — по логарифмической гистограмме, относя каждый бин
    /// целиком к корзине его представителя (погрешность бина ~19 % по
    /// значению, то есть заметна только у самой границы корзины).
    ///
    /// Нужна чартам-гистограммам дашборда (длина сообщения, длительность
    /// хода): гистограмма пауз хранится отдельным полем [`DayBucket`], потому
    /// что её границы зафиксированы продуктовым решением, а эти — нет.
    pub fn bucket_counts(&self, edges: &[u32]) -> Vec<u64> {
        let mut out = vec![0u64; edges.len() + 1];
        if self.count == 0 {
            return out;
        }
        let idx_of = |v: u64| -> usize {
            edges
                .iter()
                .position(|e| v < u64::from(*e))
                .unwrap_or(edges.len())
        };
        if self.kept.len() as u64 == self.count {
            for v in &self.kept {
                out[idx_of(u64::from(*v))] += 1;
            }
            return out;
        }
        for (bin, n) in self.hist.bins.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            out[idx_of(LogHist::bin_value(bin))] += u64::from(*n);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Оценка числа различных слов
// ---------------------------------------------------------------------------

/// Число регистров HyperLogLog (степень двойки). 512 регистров дают
/// стандартную ошибку ≈ 1.04/√512 ≈ 4.6 %.
const HLL_REGISTERS: usize = 512;
/// Сколько старших бит хеша уходит на номер регистра (log2 [`HLL_REGISTERS`]).
const HLL_PREFIX_BITS: u32 = 9;

/// Счётчик числа РАЗЛИЧНЫХ значений (HyperLogLog) для знаменателя TTR.
///
/// ### Почему не точное множество
///
/// TTR за день — «различных слов / всего слов», и точный ответ требует
/// хранить множество слов дня. Дневной бакет живёт не только в памяти: он
/// уезжает в снимок кэша по каждой из ~1300 сессий, и точные множества
/// раздули бы снимок на десятки мегабайт, причём тем сильнее, чем активнее
/// работал человек. HyperLogLog занимает ровно [`HLL_REGISTERS`] байт при
/// любом объёме словаря и складывается без потерь (поэлементный максимум
/// регистров), то есть слияние сессий и дней остаётся дешёвым и точным.
///
/// ### Точность
///
/// На малых словарях (до ~2.5·m слов) работает линейный счёт — там ошибка
/// доли процента. Дальше — оценка HyperLogLog с ошибкой ~4.6 %. Для TTR,
/// которая читается как тренд («стал ли я писать однообразнее»), этого
/// достаточно; выдавать её как точное число нельзя, и в документации это
/// зафиксировано.
///
/// ### Формат в снимке
///
/// Регистры сериализуются одной base64-строкой (684 символа вместо 512
/// чисел JSON-массива) и опускаются целиком, пока счётчик пуст.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Distinct {
    /// Регистры HLL; пустой вектор — ни одного значения не добавляли.
    regs: Vec<u8>,
}

impl Distinct {
    /// Учитывает значение по его 64-битному хешу
    /// ([`super::text::hash64`] — стабильному между запусками).
    pub fn add_hash(&mut self, h: u64) {
        if self.regs.is_empty() {
            self.regs = vec![0u8; HLL_REGISTERS];
        }
        let idx = (h >> (64 - HLL_PREFIX_BITS)) as usize;
        // Ранг — позиция первой единицы в оставшихся битах (1..=55).
        let rest = h << HLL_PREFIX_BITS;
        let rank = if rest == 0 {
            (64 - HLL_PREFIX_BITS + 1) as u8
        } else {
            (rest.leading_zeros() + 1) as u8
        };
        if rank > self.regs[idx] {
            self.regs[idx] = rank;
        }
    }

    /// Поэлементный максимум — слияние HLL без потери точности.
    pub fn merge(&mut self, other: &Distinct) {
        if other.regs.is_empty() {
            return;
        }
        if self.regs.is_empty() {
            self.regs = other.regs.clone();
            return;
        }
        for (a, b) in self.regs.iter_mut().zip(other.regs.iter()) {
            if *b > *a {
                *a = *b;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regs.is_empty() || self.regs.iter().all(|r| *r == 0)
    }

    /// Оценка числа различных значений.
    pub fn estimate(&self) -> u64 {
        if self.regs.is_empty() {
            return 0;
        }
        let m = self.regs.len() as f64;
        // Константа смещения HyperLogLog для m = 512.
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut sum = 0.0f64;
        let mut zeros = 0usize;
        for r in &self.regs {
            sum += 2f64.powi(-(*r as i32));
            if *r == 0 {
                zeros += 1;
            }
        }
        let raw = alpha * m * m / sum;
        // Малые словари: линейный счёт точнее сырой оценки HLL.
        if raw <= 2.5 * m && zeros > 0 {
            return (m * (m / zeros as f64).ln()).round() as u64;
        }
        raw.round() as u64
    }
}

impl Serialize for Distinct {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.regs))
    }
}

impl<'de> Deserialize<'de> for Distinct {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let s = String::deserialize(d)?;
        let regs = base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .unwrap_or_default();
        // Снимок другой формы (иная длина регистров) молча считается пустым:
        // это ровно тот случай, ради которого существует версия кэша, и
        // ронять загрузку из-за него нельзя.
        Ok(Distinct {
            regs: if regs.len() == HLL_REGISTERS {
                regs
            } else {
                Vec::new()
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Дневной бакет
// ---------------------------------------------------------------------------

/// Статистика одного инструмента за день.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolStat {
    pub calls: u64,
    pub interrupted: u64,
}

impl ToolStat {
    fn merge(&mut self, other: &ToolStat) {
        self.calls += other.calls;
        self.interrupted += other.interrupted;
    }
}

/// Статистика одной модели за день.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStat {
    pub messages: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_create: u64,
    pub thinking: u64,
    /// Доля стоимости сессии, отнесённая на этот день (см.
    /// [`SessionRollup::redistribute_totals`]).
    pub cost_usd: f64,
}

impl ModelStat {
    fn merge(&mut self, other: &ModelStat) {
        self.messages += other.messages;
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.cache_read += other.cache_read;
        self.cache_create += other.cache_create;
        self.thinking += other.thinking;
        self.cost_usd += other.cost_usd;
    }
}

/// Границы гистограммы пауз (мс): `<5 c`, `5–15 c`, `15–60 c`, `1–5 мин`,
/// `>5 мин`. Именно эти пять бакетов рисует ключевой чарт дашборда.
pub const PAUSE_BUCKET_EDGES_MS: [i64; 4] = [5_000, 15_000, 60_000, 300_000];

/// Сколько первых часов суток считаются «ночью» (00:00–05:59 локального
/// времени). Правило одно на бэкенд и на фронт: тепловая карта подчёркивает
/// ровно эти столбцы, а плитка «ночная активность» считает ровно их сумму.
pub const NIGHT_HOURS: usize = 6;

/// Потолок числа различных промптов, отслеживаемых за один день.
///
/// Дневной набор промптов в живых данных — сотни записей, до потолка не
/// достаёт ни один день архива. Потолок существует только как страховка от
/// патологического файла: после него новые ключи не заводятся (счётчики уже
/// известных продолжают расти), и доля повторов оказывается заниженной, а не
/// снимок — раздутым.
pub const MAX_PROMPT_KEYS: usize = 20_000;

/// Всё, что случилось за один календарный день (локальная зона).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayBucket {
    /// `YYYY-MM-DD` в локальной зоне машины.
    pub date: String,
    pub human_messages: u64,
    pub assistant_messages: u64,
    pub sessions_started: u64,

    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_create: u64,
    pub thinking: u64,

    /// Стоимость, отнесённая на день из итогов `cost-state`.
    pub cost_usd: f64,
    pub lines_added: u64,
    pub lines_removed: u64,

    pub tool_calls: u64,
    pub interruptions: u64,
    pub compacts: u64,
    /// Записи с `isSidechain == true` (работа субагентов).
    pub subagent_records: u64,
    /// Запуски субагентов (записи `agent-name`).
    pub subagent_starts: u64,

    /// Сумма измеренных длительностей ходов ассистента.
    pub gen_ms: u64,
    /// Доля `totalAPIDuration` сессии, отнесённая на день.
    pub api_ms: u64,
    /// Доля `totalToolDuration` сессии, отнесённая на день.
    pub tool_ms: u64,
    /// «Человеко-время» — сумма пауз перед ответом (без «отошёл»).
    pub human_ms: u64,

    /// Паузы длиннее [`AWAY_THRESHOLD_MS`] — «отошёл», в перцентили не входят.
    pub away_count: u64,
    /// Ответы быстрее [`QUICK_REPLY_MS`] — «не читая».
    pub quick_replies: u64,
    /// Гистограмма пауз по [`PAUSE_BUCKET_EDGES_MS`].
    pub pause_buckets: [u64; 5],

    pub pauses: Samples,
    pub msg_lens: Samples,
    pub turn_ms: Samples,

    /// Активность человека: `[час][день недели]`, где день недели 0 = понедельник.
    #[serde(default, skip_serializing_if = "is_zero_heatmap")]
    pub hour_x_weekday: Vec<Vec<u32>>,

    // --- Разнообразие (чарт 11): признаки «автопилота» ---
    /// Всего слов в сообщениях человека за день (знаменатель TTR).
    #[serde(default)]
    pub words_total: u64,
    /// Оценка числа РАЗЛИЧНЫХ слов за день (числитель TTR).
    #[serde(default, skip_serializing_if = "Distinct::is_empty")]
    pub words_distinct: Distinct,
    /// Нормализованные промпты дня: хеш → сколько раз встретился.
    /// Повторов за день = `сумма значений − число ключей`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prompt_hashes: BTreeMap<u64, u32>,
    /// Сколько сообщений реально попало в [`DayBucket::prompt_hashes`]
    /// (после [`MAX_PROMPT_KEYS`] новые ключи не заводятся).
    #[serde(default)]
    pub prompts_counted: u64,
    /// Сообщения, начинающиеся со слэша.
    #[serde(default)]
    pub slash_commands: u64,
    /// Односложные подтверждения («ок», «продолжай») — прямой индикатор
    /// автопилота.
    #[serde(default)]
    pub one_word_acks: u64,

    pub versions: BTreeSet<String>,
    pub models: BTreeMap<String, ModelStat>,
    pub tools: BTreeMap<String, ToolStat>,
    pub agents: BTreeMap<String, u64>,
}

fn is_zero_heatmap(v: &[Vec<u32>]) -> bool {
    v.is_empty()
}

impl DayBucket {
    pub fn new(date: &str) -> Self {
        DayBucket {
            date: date.to_string(),
            ..Default::default()
        }
    }

    fn heatmap_mut(&mut self) -> &mut Vec<Vec<u32>> {
        if self.hour_x_weekday.is_empty() {
            self.hour_x_weekday = vec![vec![0u32; 7]; 24];
        }
        &mut self.hour_x_weekday
    }

    /// Поэлементное сложение бакетов. Даты обязаны совпадать (или у
    /// приёмника быть пустой — так строится агрегат за период).
    pub fn merge(&mut self, other: &DayBucket) {
        if self.date.is_empty() {
            self.date = other.date.clone();
        }
        self.human_messages += other.human_messages;
        self.assistant_messages += other.assistant_messages;
        self.sessions_started += other.sessions_started;

        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.cache_read += other.cache_read;
        self.cache_create += other.cache_create;
        self.thinking += other.thinking;

        self.cost_usd += other.cost_usd;
        self.lines_added += other.lines_added;
        self.lines_removed += other.lines_removed;

        self.tool_calls += other.tool_calls;
        self.interruptions += other.interruptions;
        self.compacts += other.compacts;
        self.subagent_records += other.subagent_records;
        self.subagent_starts += other.subagent_starts;

        self.gen_ms += other.gen_ms;
        self.api_ms += other.api_ms;
        self.tool_ms += other.tool_ms;
        self.human_ms += other.human_ms;

        self.away_count += other.away_count;
        self.quick_replies += other.quick_replies;
        for (a, b) in self.pause_buckets.iter_mut().zip(other.pause_buckets.iter()) {
            *a += *b;
        }

        self.pauses.merge(&other.pauses);
        self.msg_lens.merge(&other.msg_lens);
        self.turn_ms.merge(&other.turn_ms);

        if !other.hour_x_weekday.is_empty() {
            let dst = self.heatmap_mut();
            for (h, row) in other.hour_x_weekday.iter().enumerate() {
                for (d, v) in row.iter().enumerate() {
                    dst[h][d] += *v;
                }
            }
        }

        self.words_total += other.words_total;
        self.words_distinct.merge(&other.words_distinct);
        self.prompts_counted += other.prompts_counted;
        for (h, n) in &other.prompt_hashes {
            // Повторы двух сессий в один день обязаны сложиться, иначе доля
            // повторов считалась бы только внутри одной сессии.
            if self.prompt_hashes.len() < MAX_PROMPT_KEYS || self.prompt_hashes.contains_key(h) {
                *self.prompt_hashes.entry(*h).or_default() += *n;
            }
        }
        self.slash_commands += other.slash_commands;
        self.one_word_acks += other.one_word_acks;

        self.versions.extend(other.versions.iter().cloned());
        for (k, v) in &other.models {
            self.models.entry(k.clone()).or_default().merge(v);
        }
        for (k, v) in &other.tools {
            self.tools.entry(k.clone()).or_default().merge(v);
        }
        for (k, v) in &other.agents {
            *self.agents.entry(k.clone()).or_default() += *v;
        }
    }

    /// TTR — доля различных слов среди всех слов дня, 0..1.
    ///
    /// Числитель — ОЦЕНКА ([`Distinct`], ошибка ~4.6 % на больших словарях),
    /// поэтому значение читается как тренд, а не как точное число. Дни без
    /// слов дают 0, а не «деление на ноль».
    pub fn ttr(&self) -> f64 {
        if self.words_total == 0 {
            return 0.0;
        }
        let distinct = self.words_distinct.estimate().min(self.words_total);
        distinct as f64 / self.words_total as f64
    }

    /// Доля сообщений, чей нормализованный текст в этот день уже встречался.
    pub fn repeat_share(&self) -> f64 {
        if self.prompts_counted == 0 {
            return 0.0;
        }
        let total: u64 = self.prompt_hashes.values().map(|n| u64::from(*n)).sum();
        let distinct = self.prompt_hashes.len() as u64;
        if total <= distinct {
            return 0.0;
        }
        (total - distinct) as f64 / self.prompts_counted as f64
    }

    /// Энтропия Шеннона распределения вызовов инструментов за день (в битах).
    ///
    /// Ноль означает «весь день одним инструментом», `log2(N)` — равномерное
    /// распределение по N инструментам. Метрика симметрична TTR: показывает,
    /// сузился ли репертуар работы.
    pub fn tool_entropy_bits(&self) -> f64 {
        let total: u64 = self.tools.values().map(|t| t.calls).sum();
        if total == 0 {
            return 0.0;
        }
        let mut h = 0.0f64;
        for st in self.tools.values() {
            if st.calls == 0 {
                continue;
            }
            let p = st.calls as f64 / total as f64;
            h -= p * p.log2();
        }
        // -0.0 у единственного инструмента портит и сравнение, и вывод.
        if h <= 0.0 {
            0.0
        } else {
            h
        }
    }

    /// Доля слэш-команд среди сообщений человека, 0..1.
    pub fn slash_share(&self) -> f64 {
        if self.human_messages == 0 {
            return 0.0;
        }
        self.slash_commands as f64 / self.human_messages as f64
    }

    /// Доля односложных подтверждений среди сообщений человека, 0..1.
    pub fn ack_share(&self) -> f64 {
        if self.human_messages == 0 {
            return 0.0;
        }
        self.one_word_acks as f64 / self.human_messages as f64
    }

    /// Сообщения человека в ночные часы (00:00–05:59 локального времени).
    pub fn night_messages(&self) -> u64 {
        if self.hour_x_weekday.is_empty() {
            return 0;
        }
        self.hour_x_weekday
            .iter()
            .take(NIGHT_HOURS)
            .flat_map(|row| row.iter())
            .map(|v| u64::from(*v))
            .sum()
    }

    /// Индекс бакета гистограммы пауз для паузы `ms`.
    pub fn pause_bucket_index(ms: i64) -> usize {
        PAUSE_BUCKET_EDGES_MS
            .iter()
            .position(|edge| ms < *edge)
            .unwrap_or(PAUSE_BUCKET_EDGES_MS.len())
    }
}

// ---------------------------------------------------------------------------
// Итоги сессии из cost-state
// ---------------------------------------------------------------------------

/// Итоги сессии из ПОСЛЕДНЕЙ записи `cost-state`.
///
/// Записи `cost-state` кумулятивны: каждая следующая содержит итог с начала
/// сессии, поэтому суммировать их запрещено — только перезаписывать.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionTotals {
    /// Была ли в файле хоть одна запись `cost-state`.
    pub present: bool,
    pub total_cost_usd: f64,
    pub total_api_duration_ms: u64,
    pub total_tool_duration_ms: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
    /// Unix-мс старта сессии.
    pub start_time_ms: Option<i64>,
    pub model_usage: BTreeMap<String, super::parse::ModelUsage>,
}

// ---------------------------------------------------------------------------
// Состояние автомата ходов
// ---------------------------------------------------------------------------

/// Состояние конечного автомата ходов.
///
/// Живёт в [`SessionRollup`], а не в локальных переменных разбора, чтобы
/// пережить границу инкрементального дочитывания: ход, разрезанный
/// оффсетом пополам, закрывается корректно при следующем проходе.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnState {
    /// Таймстемп последнего настоящего сообщения человека.
    pub last_human_ts: Option<i64>,
    /// Таймстемп последней `assistant`-записи (fallback-конец хода).
    pub last_assistant_ts: Option<i64>,
    /// Конец хода из записи `system`/`turn_duration`, ещё не «съеденный»
    /// следующим сообщением человека.
    pub pending_end_ts: Option<i64>,
    /// Ход открыт: человек написал, `turn_duration` ещё не пришёл.
    pub turn_open: bool,
}

// ---------------------------------------------------------------------------
// Роллап сессии
// ---------------------------------------------------------------------------

/// Итог разбора одного файла-транскрипта.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionRollup {
    pub session_id: String,
    /// Рабочий каталог сессии (источник истины о «проекте»).
    pub cwd: Option<String>,
    /// Закодированное имя каталога `~/.claude/projects/<dir>` — тот же
    /// энкодинг, что у Claude Code (`claude_memory::encode_project_dir`).
    pub project_dir: Option<String>,
    pub git_branch: Option<String>,
    /// Заголовок сессии из записи `ai-title` (записей `summary` в файлах нет).
    pub ai_title: Option<String>,
    pub cli_versions: BTreeSet<String>,

    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,

    pub days: BTreeMap<String, DayBucket>,
    pub totals: SessionTotals,

    pub parsed_lines: u64,
    /// Строки, которые не удалось разобрать (битые/усечённые).
    pub skipped_lines: u64,
    /// Байтовый оффсет конца последней ПОЛНОЙ строки — точка дочитывания.
    pub parse_offset: u64,
    /// Размер файла на момент разбора (для инкрементальности Фазы 2).
    pub file_len: u64,
    /// mtime файла в мс (для инкрементальности Фазы 2).
    pub file_mtime_ms: i64,

    pub turn_state: TurnState,
}

impl SessionRollup {
    pub fn new(session_id: &str) -> Self {
        SessionRollup {
            session_id: session_id.to_string(),
            ..Default::default()
        }
    }

    /// Запоминает cwd и вычисляет закодированное имя каталога проекта.
    pub fn set_cwd(&mut self, cwd: &str) {
        self.project_dir = Some(encode_project_dir(std::path::Path::new(cwd)));
        self.cwd = Some(cwd.to_string());
    }

    /// Расширяет временные границы сессии.
    pub fn observe_ts(&mut self, ts: i64) {
        self.start_ts = Some(self.start_ts.map_or(ts, |v| v.min(ts)));
        self.end_ts = Some(self.end_ts.map_or(ts, |v| v.max(ts)));
    }

    /// Ключ дня (`YYYY-MM-DD`, локальная зона) для таймстемпа.
    pub fn day_key(ts: i64) -> String {
        local_dt(ts).format("%Y-%m-%d").to_string()
    }

    pub fn day_mut(&mut self, ts: i64) -> &mut DayBucket {
        let key = Self::day_key(ts);
        self.days
            .entry(key.clone())
            .or_insert_with(|| DayBucket::new(&key))
    }

    /// Настоящее сообщение человека: закрывает предыдущий ход, считает
    /// паузу и учитывает само сообщение.
    ///
    /// `text` — признаки сообщения ([`super::text::classify`]); сам текст в
    /// роллап не попадает и в снимок кэша не уезжает, остаются только
    /// счётчики и 64-битные хеши.
    pub fn on_human_message(&mut self, ts: i64, text: &HumanText) {
        self.close_turn_before(ts);

        let (hour, weekday) = {
            let dt = local_dt(ts);
            (
                dt.hour() as usize,
                dt.weekday().num_days_from_monday() as usize,
            )
        };
        let day = self.day_mut(ts);
        day.human_messages += 1;
        day.msg_lens.push(text.len.min(u32::MAX as usize) as u32);
        day.heatmap_mut()[hour][weekday] += 1;

        day.words_total += text.word_count;
        for h in &text.word_hashes {
            day.words_distinct.add_hash(*h);
        }
        if day.prompt_hashes.len() < MAX_PROMPT_KEYS
            || day.prompt_hashes.contains_key(&text.prompt_hash)
        {
            *day.prompt_hashes.entry(text.prompt_hash).or_default() += 1;
            day.prompts_counted += 1;
        }
        if text.is_slash {
            day.slash_commands += 1;
        }
        if text.is_ack {
            day.one_word_acks += 1;
        }

        self.turn_state.last_human_ts = Some(ts);
        self.turn_state.last_assistant_ts = None;
        self.turn_state.pending_end_ts = None;
        self.turn_state.turn_open = true;
    }

    /// Закрывает ход, предшествующий сообщению человека в момент `ts`.
    ///
    /// Конец хода — `pending_end_ts` (из `system`/`turn_duration`), а если
    /// такой записи не было — таймстемп последней `assistant`-записи
    /// (fallback). Пауза = `ts − конец хода`.
    fn close_turn_before(&mut self, ts: i64) {
        let from_record = self.turn_state.pending_end_ts;
        let end = match from_record {
            Some(e) => Some(e),
            None if self.turn_state.turn_open => self.turn_state.last_assistant_ts,
            None => None,
        };
        let Some(end) = end else { return };

        // Длительность хода: если конец пришёл по fallback, она ещё не
        // записана (для `turn_duration` её записал `on_turn_end`).
        if from_record.is_none() {
            if let Some(h) = self.turn_state.last_human_ts {
                self.record_turn_duration(end, end - h);
            }
        }
        self.record_pause(end, ts - end);
        self.turn_state.turn_open = false;
        self.turn_state.pending_end_ts = None;
    }

    fn record_turn_duration(&mut self, end_ts: i64, dur_ms: i64) {
        if dur_ms < 0 {
            return;
        }
        let day = self.day_mut(end_ts);
        day.gen_ms += dur_ms as u64;
        day.turn_ms.push(dur_ms.min(u32::MAX as i64) as u32);
    }

    /// Пауза относится к дню, в который ход ЗАВЕРШИЛСЯ, — именно тогда
    /// человек начал читать результат.
    fn record_pause(&mut self, end_ts: i64, pause_ms: i64) {
        if pause_ms < 0 {
            return;
        }
        let day = self.day_mut(end_ts);
        if pause_ms > AWAY_THRESHOLD_MS {
            day.away_count += 1;
            return;
        }
        day.human_ms += pause_ms as u64;
        day.pauses.push(pause_ms.min(u32::MAX as i64) as u32);
        day.pause_buckets[DayBucket::pause_bucket_index(pause_ms)] += 1;
        if pause_ms < QUICK_REPLY_MS {
            day.quick_replies += 1;
        }
    }

    /// Запись `system`/`turn_duration` — канонический конец хода.
    pub fn on_turn_end(&mut self, ts: i64) {
        if self.turn_state.turn_open {
            if let Some(h) = self.turn_state.last_human_ts {
                self.record_turn_duration(ts, ts - h);
            }
        }
        self.turn_state.pending_end_ts = Some(ts);
        self.turn_state.turn_open = false;
    }

    /// Учитывает запись ассистента: токены, модель, вызовы инструментов.
    pub fn on_assistant(&mut self, ts: i64, rec: &Record) {
        let model = rec
            .message
            .as_ref()
            .and_then(|m| m.model.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let usage = rec.message.as_ref().and_then(|m| m.usage.as_ref());
        let tin = usage.and_then(|u| u.input_tokens).unwrap_or(0);
        let tout = usage.and_then(|u| u.output_tokens).unwrap_or(0);
        let cread = usage.and_then(|u| u.cache_read_input_tokens).unwrap_or(0);
        let ccreate = usage
            .and_then(|u| u.cache_creation_input_tokens)
            .unwrap_or(0);
        let thinking = usage
            .and_then(|u| u.output_tokens_details.as_ref())
            .and_then(|d| d.thinking_tokens)
            .unwrap_or(0);

        let tools: Vec<String> = match rec.message.as_ref().and_then(|m| m.content.as_ref()) {
            Some(super::parse::Content::Blocks(blocks)) => blocks
                .iter()
                .filter(|b| b.kind == "tool_use")
                .map(|b| b.name.clone().unwrap_or_else(|| "unknown".to_string()))
                .collect(),
            _ => Vec::new(),
        };

        let sidechain = rec.is_sidechain;
        let day = self.day_mut(ts);
        day.assistant_messages += 1;
        day.tokens_in += tin;
        day.tokens_out += tout;
        day.cache_read += cread;
        day.cache_create += ccreate;
        day.thinking += thinking;
        if sidechain {
            day.subagent_records += 1;
        }

        let m = day.models.entry(model).or_default();
        m.messages += 1;
        m.tokens_in += tin;
        m.tokens_out += tout;
        m.cache_read += cread;
        m.cache_create += ccreate;
        m.thinking += thinking;

        for name in tools {
            day.tool_calls += 1;
            day.tools.entry(name).or_default().calls += 1;
        }

        if !sidechain {
            self.turn_state.last_assistant_ts = Some(ts);
        }
    }

    /// Прерванный пользователем вызов инструмента.
    pub fn on_interruption(&mut self, ts: i64) {
        self.day_mut(ts).interruptions += 1;
    }

    /// Запуск субагента (запись `agent-name`; таймстемпа у неё нет, поэтому
    /// относим к последнему известному дню сессии).
    pub fn on_subagent_start(&mut self, name: &str) {
        let Some(ts) = self.end_ts.or(self.start_ts) else {
            return;
        };
        let day = self.day_mut(ts);
        day.subagent_starts += 1;
        *day.agents.entry(name.to_string()).or_default() += 1;
    }

    /// Закрывает последний открытый ход по fallback (последний `assistant`).
    ///
    /// Вызывается ТОЛЬКО когда сессия заведомо закончена (файл больше не
    /// растёт): во время инкрементального разбора закрывать ход нельзя —
    /// продолжение может прийти следующей порцией.
    // Точка расширения инкрементального разбора: сейчас незакрытый ход
    // доживает до следующего прохода в `turn_state`, и звать этот метод
    // некому. Удалять его нельзя — на него ссылается контракт `scan.rs`.
    #[allow(dead_code)]
    pub fn close_open_turn(&mut self) {
        if !self.turn_state.turn_open {
            return;
        }
        let (Some(end), Some(h)) = (
            self.turn_state.last_assistant_ts,
            self.turn_state.last_human_ts,
        ) else {
            return;
        };
        self.record_turn_duration(end, end - h);
        self.turn_state.turn_open = false;
    }

    /// Раскладывает итоги сессии (`cost-state`) по дневным бакетам.
    ///
    /// `cost-state` даёт итог на всю сессию без разбивки по дням, а чартам
    /// нужен дневной ряд. Распределяем пропорционально числу записей
    /// ассистента в дне (единственный доступный прокси нагрузки); если
    /// записей нет — всё уходит на первый день сессии.
    ///
    /// Метод идемпотентен: сначала обнуляет распределённые поля, поэтому
    /// его безопасно вызывать после каждого дочитывания.
    pub fn redistribute_totals(&mut self) {
        for d in self.days.values_mut() {
            d.cost_usd = 0.0;
            d.lines_added = 0;
            d.lines_removed = 0;
            d.api_ms = 0;
            d.tool_ms = 0;
            d.sessions_started = 0;
            for m in d.models.values_mut() {
                m.cost_usd = 0.0;
            }
        }
        let Some(first_day) = self.days.keys().next().cloned() else {
            return;
        };
        if let Some(d) = self.days.get_mut(&first_day) {
            d.sessions_started = 1;
        }
        if !self.totals.present {
            return;
        }

        let keys: Vec<String> = self.days.keys().cloned().collect();
        let weights: Vec<u64> = keys
            .iter()
            .map(|k| self.days[k].assistant_messages)
            .collect();
        let total_w: u64 = weights.iter().sum();

        let cost = self.totals.total_cost_usd;
        let api = self.totals.total_api_duration_ms;
        let tool = self.totals.total_tool_duration_ms;
        let added = self.totals.lines_added;
        let removed = self.totals.lines_removed;

        if total_w == 0 {
            let d = self.days.get_mut(&first_day).expect("first day exists");
            d.cost_usd = cost;
            d.api_ms = api;
            d.tool_ms = tool;
            d.lines_added = added;
            d.lines_removed = removed;
        } else {
            let mut rest = (cost, api, tool, added, removed);
            for (i, k) in keys.iter().enumerate() {
                let last = i + 1 == keys.len();
                let w = weights[i];
                let share = w as f64 / total_w as f64;
                let d = self.days.get_mut(k).expect("day exists");
                if last {
                    d.cost_usd = rest.0;
                    d.api_ms = rest.1;
                    d.tool_ms = rest.2;
                    d.lines_added = rest.3;
                    d.lines_removed = rest.4;
                } else {
                    let c = cost * share;
                    let a = (api as f64 * share) as u64;
                    let t = (tool as f64 * share) as u64;
                    let la = (added as f64 * share) as u64;
                    let lr = (removed as f64 * share) as u64;
                    d.cost_usd = c;
                    d.api_ms = a;
                    d.tool_ms = t;
                    d.lines_added = la;
                    d.lines_removed = lr;
                    rest.0 -= c;
                    rest.1 -= a;
                    rest.2 -= t;
                    rest.3 -= la;
                    rest.4 -= lr;
                }
            }
        }

        // Стоимость по моделям — пропорционально числу сообщений модели в дне.
        let models: Vec<(String, f64)> = self
            .totals
            .model_usage
            .iter()
            .map(|(m, u)| (m.clone(), u.cost_usd))
            .collect();
        for (model, model_cost) in models {
            let total_msgs: u64 = keys
                .iter()
                .map(|k| {
                    self.days[k]
                        .models
                        .get(&model)
                        .map(|m| m.messages)
                        .unwrap_or(0)
                })
                .sum();
            if total_msgs == 0 {
                if let Some(d) = self.days.get_mut(&first_day) {
                    d.models.entry(model.clone()).or_default().cost_usd = model_cost;
                }
                continue;
            }
            for k in &keys {
                let d = self.days.get_mut(k).expect("day exists");
                if let Some(ms) = d.models.get_mut(&model) {
                    ms.cost_usd = model_cost * (ms.messages as f64 / total_msgs as f64);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Сводный индекс
// ---------------------------------------------------------------------------

/// Роллап одного проекта (каталога `~/.claude/projects/<dir>`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectRollup {
    /// Закодированное имя каталога.
    pub dir: String,
    /// Последний виденный cwd — человекочитаемое имя проекта.
    pub cwd: String,
    pub sessions: u64,
    pub days: BTreeMap<String, DayBucket>,
}

/// Сводный индекс по всем разобранным сессиям.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalyticsIndex {
    pub days: BTreeMap<String, DayBucket>,
    pub projects: BTreeMap<String, ProjectRollup>,
    pub sessions: u64,
}

impl AnalyticsIndex {
    /// Вливает роллап одной сессии в общий индекс и в её проект.
    pub fn add_session(&mut self, rollup: &SessionRollup) {
        self.sessions += 1;
        let dir = rollup
            .project_dir
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let project = self.projects.entry(dir.clone()).or_insert_with(|| ProjectRollup {
            dir: dir.clone(),
            cwd: rollup.cwd.clone().unwrap_or_default(),
            ..Default::default()
        });
        if project.cwd.is_empty() {
            project.cwd = rollup.cwd.clone().unwrap_or_default();
        }
        project.sessions += 1;
        for (date, bucket) in &rollup.days {
            project
                .days
                .entry(date.clone())
                .or_insert_with(|| DayBucket::new(date))
                .merge(bucket);
            self.days
                .entry(date.clone())
                .or_insert_with(|| DayBucket::new(date))
                .merge(bucket);
        }
    }

    /// Выборка дневных бакетов под фильтр.
    ///
    /// Ничего не пересчитывает по файлам — работает по уже собранным
    /// [`DayBucket`].
    pub fn query(&self, q: &Query) -> Vec<DayBucket> {
        let source = match q.project.as_deref() {
            Some(dir) => match self.projects.get(dir) {
                Some(p) => &p.days,
                None => return Vec::new(),
            },
            None => &self.days,
        };
        let mut out: BTreeMap<String, DayBucket> = BTreeMap::new();
        for (date, bucket) in source {
            if let Some(from) = q.from.as_deref() {
                if date.as_str() < from {
                    continue;
                }
            }
            if let Some(to) = q.to.as_deref() {
                if date.as_str() > to {
                    continue;
                }
            }
            let key = bucket_key(date, q.bucket);
            out.entry(key.clone())
                .or_insert_with(|| DayBucket::new(&key))
                .merge(bucket);
        }
        out.into_values().collect()
    }

    /// Суммарный бакет за период (KPI-плитки).
    pub fn total(&self, q: &Query) -> DayBucket {
        let mut acc = DayBucket::default();
        for b in self.query(&Query {
            bucket: Bucket::Day,
            ..q.clone()
        }) {
            acc.merge(&b);
        }
        acc.date.clear();
        acc
    }
}

/// Гранулярность временного ряда.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bucket {
    #[default]
    Day,
    Week,
}

/// Фильтр выборки: диапазон дат (включительно, `YYYY-MM-DD`), проект
/// (закодированное имя каталога) и гранулярность.
#[derive(Debug, Default, Clone)]
pub struct Query {
    pub from: Option<String>,
    pub to: Option<String>,
    pub project: Option<String>,
    pub bucket: Bucket,
}

/// Ключ корзины ряда для даты: сама дата или понедельник её недели.
///
/// Публичная, потому что серии, которые считаются НЕ по суммарному бакету
/// (уникальные проекты в день), обязаны раскладываться по тем же корзинам,
/// что и [`AnalyticsIndex::query`] — иначе ряды разъедутся по длине.
pub fn bucket_key(date: &str, bucket: Bucket) -> String {
    match bucket {
        Bucket::Day => date.to_string(),
        Bucket::Week => week_key(date),
    }
}

/// Ключ недели — дата понедельника этой недели.
fn week_key(date: &str) -> String {
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => {
            let shift = d.weekday().num_days_from_monday() as i64;
            (d - chrono::Duration::days(shift))
                .format("%Y-%m-%d")
                .to_string()
        }
        Err(_) => date.to_string(),
    }
}

/// Unix-мс → локальное время машины.
fn local_dt(ts: i64) -> chrono::DateTime<chrono::Local> {
    chrono::Local
        .timestamp_millis_opt(ts)
        .single()
        .unwrap_or_else(chrono::Local::now)
}
