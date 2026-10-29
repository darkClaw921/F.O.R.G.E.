//! Привязка дорожки к человеку — КАСКАД ИЗ ПЯТИ СТУПЕНЕЙ. **Чистые функции.**
//!
//! Модуль НЕ импортирует `axum` и `http`, не ходит в браузер и не делает
//! ввода-вывода: всё решается значениями, поэтому и проверяется значениями.
//!
//! # Зачем каскад, а не один способ
//!
//! Потому что способа, работающего везде, нет. Платформа либо говорит, чей это
//! поток (`msid`, SSRC), либо не говорит — и тогда остаётся сопоставлять
//! таймлайны. Живая проба Ф0 показала худший из вариантов: **Телемост не
//! отдаёт трек на участника вовсе** (режим `Slots`), и ступени, опирающиеся на
//! «трек = человек», там не срабатывают НИКОГДА. Это не деградация и не
//! ошибка — это свойство площадки, и код обязан молча идти дальше по каскаду.
//!
//! | ступень | чем связывает | когда работает |
//! |---|---|---|
//! | [`BindingStage::Msid`] | `msid`/`mid` дорожки ↔ ключ участника | площадка раскрывает связь |
//! | [`BindingStage::Ssrc`] | SSRC ↔ участник | то же, другим полем |
//! | [`BindingStage::Correlation`] | VAD-таймлайн дорожки ↔ DOM-индикатор говорящего | ВСЕГДА, если есть и то, и другое |
//! | [`BindingStage::Dom`] | площадка сама называет владельца потока | редко |
//! | [`BindingStage::Fallback`] | «Участник N» плюс ручное переименование | всегда |
//!
//! Пятая ступень терпима именно потому, что у нас есть пер-трековое аудио:
//! человек жмёт play на «Участнике 2», за две секунды узнаёт голос и
//! подписывает. Без пер-трекового звука она была бы тупиком.
//!
//! # Корреляция — это задача о назначении, а не «кто больше похож»
//!
//! Жадный выбор «каждой дорожке лучшего спикера» на перебивающихся репликах
//! даёт двум дорожкам ОДНОГО человека и теряет второго. Поэтому назначение
//! ищется целиком: перебором для малых наборов (их и бывают единицы) и жадно
//! с уникальностью — для больших.
//!
//! # Почему одного порога мало
//!
//! Порог 0.7 отсекает «похоже не очень». Но бывает хуже: два таймлайна
//! ОДИНАКОВО хорошо ложатся на одного и того же человека — например, площадка
//! продублировала дорожку. Порог такую пару пропустит, и имя будет назначено
//! наугад. Поэтому есть второе условие — ЗАЗОР между лучшим и вторым
//! претендентом ([`AttribConfig::min_margin`]). Не выполнилось — участник
//! остаётся «Участником N», и это правильный ответ: неверное имя в транскрипте
//! хуже отсутствующего, потому что его не видно.

use serde::{Deserialize, Serialize};

/// Ступень каскада, фактически давшая имя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStage {
    /// `msid`/`mid` дорожки совпал с ключом участника.
    Msid,
    /// SSRC совпал с участником.
    Ssrc,
    /// Корреляция VAD-таймлайна с DOM-индикатором активного спикера.
    Correlation,
    /// Площадка прямо назвала владельца потока.
    Dom,
    /// Имени нет: «Участник N» плюс ручное переименование в интерфейсе.
    Fallback,
}

impl BindingStage {
    /// Значение колонки `meeting_participants.source` и поля манифеста.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Msid => "msid",
            Self::Ssrc => "ssrc",
            Self::Correlation => "correlation",
            Self::Dom => "dom",
            Self::Fallback => "fallback",
        }
    }

    /// Насколько ступень надёжна: меньше — лучше. Нужна, чтобы в манифест
    /// уехала ЛУЧШАЯ сработавшая ступень, а не последняя.
    fn rank(self) -> u8 {
        match self {
            Self::Msid => 0,
            Self::Ssrc => 1,
            Self::Correlation => 2,
            Self::Dom => 3,
            Self::Fallback => 4,
        }
    }
}

/// Часть `msid`, называющая УЧАСТНИКА, а не дорожку.
///
/// `msid` в SDP — это пара «поток и дорожка», и у LiveKit она читаемая:
/// `PA_<sid участника>|TR_<sid дорожки>` (измерено живьём на
/// vcc.airlabs.pro). Левая половина одинакова у ВСЕХ дорожек одного человека
/// — и микрофон, и демонстрация экрана приезжают под одним `PA_*`.
///
/// Человеческого имени в `msid` нет ни у одной известной площадки, поэтому
/// сама по себе эта функция никого не называет. Её работа другая: сказать,
/// что две дорожки принадлежат ОДНОМУ человеку, — а имя для него уже могла
/// найти корреляция по одной из них.
///
/// Разделителя нет — значит вся строка и есть ось: два трека с одинаковым
/// `msid` целиком это буквально один поток.
pub fn msid_participant(msid: &str) -> &str {
    msid.split('|').next().unwrap_or(msid).trim()
}

/// Интервал речи на общей оси, мс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Span {
    pub fn new(start_ms: i64, end_ms: i64) -> Self {
        Self { start_ms, end_ms }
    }

    pub fn len_ms(self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// Дорожка глазами пер-трекового VAD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackTimeline {
    /// Номер файла `raw/track-N.wav`.
    pub idx: usize,
    /// Идентификатор дорожки платформы.
    pub track_id: String,
    /// `msid`/`mid`, если площадка его дала.
    pub msid: Option<String>,
    /// SSRC, если он известен.
    pub ssrc: Option<u32>,
    /// Интервалы речи.
    pub speech: Vec<Span>,
}

/// Участник глазами DOM-индикатора.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpeakerTimeline {
    /// Ключ участника (атрибут плитки либо имя).
    pub key: String,
    /// Видимое имя.
    pub name: String,
    /// Интервалы, в которые площадка считала его говорящим.
    pub active: Vec<Span>,
}

/// Пороги каскада. Поля конфига, а не константы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttribConfig {
    /// Окно наблюдения, мс: столько первых миллисекунд встречи участвует в
    /// корреляции. Девяносто секунд достаточно, чтобы высказались все, и
    /// достаточно мало, чтобы не платить за час встречи.
    pub window_ms: i64,
    /// Порог принятия назначения.
    pub min_score: f64,
    /// Обязательный зазор между лучшим и вторым претендентом.
    pub min_margin: f64,
}

/// Окно корреляции по умолчанию — 90 секунд.
pub const DEFAULT_CORRELATION_WINDOW_SECS: u64 = 90;

/// Порог принятия по умолчанию.
pub const DEFAULT_CORRELATION_MIN_SCORE: f64 = 0.7;

/// Зазор до второго претендента по умолчанию.
pub const DEFAULT_CORRELATION_MIN_MARGIN: f64 = 0.1;

impl From<&crate::config::MeetConfig> for AttribConfig {
    fn from(cfg: &crate::config::MeetConfig) -> Self {
        Self {
            window_ms: (cfg.speaker_correlation_window_secs.saturating_mul(1_000)) as i64,
            min_score: cfg.speaker_correlation_min_score,
            min_margin: cfg.speaker_correlation_min_margin,
        }
    }
}

impl Default for AttribConfig {
    fn default() -> Self {
        Self {
            window_ms: (DEFAULT_CORRELATION_WINDOW_SECS * 1_000) as i64,
            min_score: DEFAULT_CORRELATION_MIN_SCORE,
            min_margin: DEFAULT_CORRELATION_MIN_MARGIN,
        }
    }
}

/// Итог привязки одной дорожки.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackBinding {
    pub idx: usize,
    pub track_id: String,
    /// Имя участника. `None` — ни одна ступень не сработала.
    pub name: Option<String>,
    /// Ключ участника, если он известен.
    pub key: Option<String>,
    pub stage: BindingStage,
    /// Уверенность: 1.0 у прямых связей, оценка совпадения у корреляции.
    pub confidence: f64,
}

impl TrackBinding {
    /// Метка для транскрипта: имя либо «Участник N».
    ///
    /// Номер берётся от ИНДЕКСА ДОРОЖКИ, а не от порядка в списке: по этому же
    /// номеру человек находит `raw/track-N.wav`, слушает голос и подписывает.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("Участник {}", self.idx + 1))
    }
}

/// Итог каскада целиком.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributionReport {
    pub bindings: Vec<TrackBinding>,
    /// ЛУЧШАЯ сработавшая ступень. Уезжает в манифест и в карточку встречи:
    /// без неё невозможно понять, доверять именам или нет.
    pub stage: BindingStage,
}

impl AttributionReport {
    /// Сколько дорожек получили настоящее имя.
    pub fn named(&self) -> usize {
        self.bindings.iter().filter(|b| b.name.is_some()).count()
    }

    /// Наибольшая уверенность среди названных.
    pub fn best_confidence(&self) -> f64 {
        self.bindings
            .iter()
            .filter(|b| b.name.is_some())
            .map(|b| b.confidence)
            .fold(0.0, f64::max)
    }

    /// Имя дорожки по её индексу.
    pub fn name_of(&self, idx: usize) -> Option<&str> {
        self.bindings
            .iter()
            .find(|b| b.idx == idx)
            .and_then(|b| b.name.as_deref())
    }
}

/// Каскад целиком: сверху вниз, до первой сработавшей ступени НА КАЖДУЮ
/// дорожку.
///
/// Ступени пробуются в порядке надёжности, и уже назначенная дорожка ниже по
/// каскаду не переназначается: ступень, сработавшая позже и хуже, не имеет
/// права стереть найденное имя.
pub fn attribute(
    tracks: &[TrackTimeline],
    speakers: &[SpeakerTimeline],
    dom_owners: &[(String, String)],
    cfg: &AttribConfig,
) -> AttributionReport {
    let mut bindings: Vec<TrackBinding> = tracks
        .iter()
        .map(|t| TrackBinding {
            idx: t.idx,
            track_id: t.track_id.clone(),
            name: None,
            key: None,
            stage: BindingStage::Fallback,
            confidence: 0.0,
        })
        .collect();

    // Ступень 1: msid/mid ↔ ключ участника.
    for (b, t) in bindings.iter_mut().zip(tracks.iter()) {
        if let Some(msid) = t.msid.as_deref() {
            if let Some(s) = speakers.iter().find(|s| s.key == msid || s.name == msid) {
                b.name = Some(s.name.clone());
                b.key = Some(s.key.clone());
                b.stage = BindingStage::Msid;
                b.confidence = 1.0;
            }
        }
    }

    // Ступень 2: SSRC ↔ участник. Ключ участника в этой ступени — строковая
    // запись SSRC: платформа, отдающая SSRC, обычно называет его так же.
    for (b, t) in bindings.iter_mut().zip(tracks.iter()) {
        if b.name.is_some() {
            continue;
        }
        if let Some(ssrc) = t.ssrc {
            let needle = ssrc.to_string();
            if let Some(s) = speakers.iter().find(|s| s.key == needle) {
                b.name = Some(s.name.clone());
                b.key = Some(s.key.clone());
                b.stage = BindingStage::Ssrc;
                b.confidence = 1.0;
            }
        }
    }

    // Ступень 3: КОРРЕЛЯЦИЯ. Основной практический путь, когда msid непрозрачен.
    let pending: Vec<usize> = bindings
        .iter()
        .enumerate()
        .filter(|(_, b)| b.name.is_none())
        .map(|(i, _)| i)
        .collect();
    if !pending.is_empty() && !speakers.is_empty() {
        let taken: Vec<&str> = bindings
            .iter()
            .filter_map(|b| b.key.as_deref())
            .collect();
        let free: Vec<&SpeakerTimeline> = speakers
            .iter()
            .filter(|s| !taken.contains(&s.key.as_str()))
            .collect();
        if !free.is_empty() {
            let subset: Vec<&TrackTimeline> = pending.iter().map(|i| &tracks[*i]).collect();
            let scores = score_matrix(&subset, &free, cfg.window_ms);
            let assignment = best_assignment(&scores);
            for (row, col) in assignment.iter().enumerate() {
                let Some(col) = *col else { continue };
                let score = scores[row][col];
                if !accepted(&scores, row, col, cfg) {
                    continue;
                }
                let b = &mut bindings[pending[row]];
                b.name = Some(free[col].name.clone());
                b.key = Some(free[col].key.clone());
                b.stage = BindingStage::Correlation;
                b.confidence = score;
            }
        }
    }

    // Ступень 4: площадка сама назвала владельца потока.
    for b in bindings.iter_mut() {
        if b.name.is_some() {
            continue;
        }
        if let Some((_, name)) = dom_owners.iter().find(|(id, _)| *id == b.track_id) {
            b.name = Some(name.clone());
            b.stage = BindingStage::Dom;
            b.confidence = 1.0;
        }
    }

    // РАСПРОСТРАНЕНИЕ ПО ОСИ УЧАСТНИКА. Не отдельная ступень каскада, а его
    // замыкание: имя, найденное ЛЮБОЙ ступенью для одной дорожки, обязано
    // достаться и остальным дорожкам ТОГО ЖЕ человека.
    //
    // Зачем. У площадки, раскрывающей `msid` (LiveKit), человек вправе иметь
    // больше одной дорожки — микрофон и демонстрация экрана. Корреляция
    // назовёт ту из них, по которой он говорил; вторая осталась бы
    // «Участником N» при полностью известной связи, а в транскрипте это
    // выглядело бы как лишний собеседник, взявшийся ниоткуда.
    //
    // Донор ищется среди УЖЕ названных, поэтому порядок обхода не меняет
    // результата, а цикла не возникает: получатель имени сам донором в этом
    // же проходе не становится.
    let donors: Vec<(String, String, Option<String>, f64)> = bindings
        .iter()
        .zip(tracks.iter())
        .filter_map(|(b, t)| {
            let name = b.name.clone()?;
            let axis = msid_participant(t.msid.as_deref()?);
            (!axis.is_empty()).then(|| (axis.to_string(), name, b.key.clone(), b.confidence))
        })
        .collect();
    for (b, t) in bindings.iter_mut().zip(tracks.iter()) {
        if b.name.is_some() {
            continue;
        }
        let Some(axis) = t.msid.as_deref().map(msid_participant) else {
            continue;
        };
        if axis.is_empty() {
            continue;
        }
        if let Some((_, name, key, confidence)) = donors.iter().find(|(a, ..)| a == axis) {
            b.name = Some(name.clone());
            b.key = key.clone();
            // Ступень именно `Msid`: связь «эти дорожки — один человек» дал
            // именно он, и в манифесте это обязано быть видно.
            b.stage = BindingStage::Msid;
            b.confidence = *confidence;
        }
    }

    // Ступень 5 — она же дефолт каждой записи: «Участник N».
    let stage = bindings
        .iter()
        .filter(|b| b.name.is_some())
        .map(|b| b.stage)
        .min_by_key(|s| s.rank())
        .unwrap_or(BindingStage::Fallback);

    AttributionReport { bindings, stage }
}

/// Принимается ли назначение `row → col`.
///
/// Два условия, и оба обязательны: порог (достаточно похоже) и зазор до
/// второго претендента (похоже ОДНОЗНАЧНО). Второе спасает от случая, когда
/// две дорожки одинаково хорошо ложатся на одного человека.
fn accepted(scores: &[Vec<f64>], row: usize, col: usize, cfg: &AttribConfig) -> bool {
    let best = scores[row][col];
    if best < cfg.min_score {
        return false;
    }
    let second_in_row = scores[row]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != col)
        .map(|(_, v)| *v)
        .fold(0.0, f64::max);
    let second_in_col = scores
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != row)
        .map(|(_, r)| r[col])
        .fold(0.0, f64::max);
    best - second_in_row.max(second_in_col) >= cfg.min_margin
}

/// Матрица совпадения таймлайнов.
fn score_matrix(tracks: &[&TrackTimeline], speakers: &[&SpeakerTimeline], window_ms: i64) -> Vec<Vec<f64>> {
    tracks
        .iter()
        .map(|t| {
            let a = clip(&t.speech, window_ms);
            speakers
                .iter()
                .map(|s| overlap_score(&a, &clip(&s.active, window_ms)))
                .collect()
        })
        .collect()
}

/// Обрезает интервалы окном наблюдения и склеивает пересекающиеся.
fn clip(spans: &[Span], window_ms: i64) -> Vec<Span> {
    let mut out: Vec<Span> = spans
        .iter()
        .filter_map(|s| {
            let start = s.start_ms.max(0);
            let end = s.end_ms.min(window_ms);
            (end > start).then_some(Span::new(start, end))
        })
        .collect();
    out.sort_by_key(|s| (s.start_ms, s.end_ms));
    let mut merged: Vec<Span> = Vec::with_capacity(out.len());
    for s in out {
        match merged.last_mut() {
            Some(prev) if s.start_ms <= prev.end_ms => {
                prev.end_ms = prev.end_ms.max(s.end_ms);
            }
            _ => merged.push(s),
        }
    }
    merged
}

/// Мера совпадения двух таймлайнов: пересечение к объединению.
///
/// Именно IoU, а не доля пересечения: доля пересечения даёт единицу для
/// дорожки, говорящей полсекунды внутри длинной реплики соседа, — то есть
/// уверенно назначает чужое имя тому, кто просто кашлянул.
fn overlap_score(a: &[Span], b: &[Span]) -> f64 {
    let total_a: i64 = a.iter().map(|s| s.len_ms()).sum();
    let total_b: i64 = b.iter().map(|s| s.len_ms()).sum();
    if total_a == 0 || total_b == 0 {
        return 0.0;
    }
    let mut inter = 0i64;
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let start = a[i].start_ms.max(b[j].start_ms);
        let end = a[i].end_ms.min(b[j].end_ms);
        if end > start {
            inter += end - start;
        }
        if a[i].end_ms < b[j].end_ms {
            i += 1;
        } else {
            j += 1;
        }
    }
    let union = total_a + total_b - inter;
    if union <= 0 {
        return 0.0;
    }
    inter as f64 / union as f64
}

/// Сколько дорожек ещё считаются перебором.
///
/// Семь дорожек это 5040 перестановок — доли миллисекунды. Дальше растёт
/// факториально, и на больших наборах включается жадный выбор: он хуже
/// теоретически, но на восьми говорящих одновременно разница уже не
/// принципиальна.
const EXHAUSTIVE_LIMIT: usize = 7;

/// Лучшее назначение «дорожка → спикер».
///
/// Возвращает для каждой строки номер столбца либо `None`.
fn best_assignment(scores: &[Vec<f64>]) -> Vec<Option<usize>> {
    let rows = scores.len();
    let cols = scores.first().map_or(0, Vec::len);
    if rows == 0 || cols == 0 {
        return vec![None; rows];
    }
    if rows <= EXHAUSTIVE_LIMIT && cols <= EXHAUSTIVE_LIMIT {
        let mut best: Option<(f64, Vec<Option<usize>>)> = None;
        let mut current = vec![None; rows];
        let mut used = vec![false; cols];
        search(scores, 0, &mut used, &mut current, 0.0, &mut best);
        return best.map(|(_, v)| v).unwrap_or_else(|| vec![None; rows]);
    }
    greedy_assignment(scores)
}

fn search(
    scores: &[Vec<f64>],
    row: usize,
    used: &mut Vec<bool>,
    current: &mut Vec<Option<usize>>,
    total: f64,
    best: &mut Option<(f64, Vec<Option<usize>>)>,
) {
    if row == scores.len() {
        // Строгое «больше»: при равенстве побеждает вариант, найденный
        // раньше, то есть лексикографически меньший. Без этого правила
        // результат зависел бы от порядка обхода, а он обязан быть
        // воспроизводимым.
        if best.as_ref().is_none_or(|(b, _)| total > *b) {
            *best = Some((total, current.clone()));
        }
        return;
    }
    // Вариант «этой дорожке никого»: без него набор с числом дорожек больше
    // числа спикеров не имел бы ни одного полного назначения.
    current[row] = None;
    search(scores, row + 1, used, current, total, best);
    for col in 0..used.len() {
        if used[col] || scores[row][col] <= 0.0 {
            continue;
        }
        used[col] = true;
        current[row] = Some(col);
        search(scores, row + 1, used, current, total + scores[row][col], best);
        used[col] = false;
        current[row] = None;
    }
}

fn greedy_assignment(scores: &[Vec<f64>]) -> Vec<Option<usize>> {
    let rows = scores.len();
    let cols = scores[0].len();
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (r, row) in scores.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            if *v > 0.0 {
                pairs.push((*v, r, c));
            }
        }
    }
    // Детерминированный порядок: по убыванию оценки, при равенстве — по
    // номерам. Иначе одна и та же запись давала бы разные имена при повторном
    // прогоне.
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut out = vec![None; rows];
    let mut used = vec![false; cols];
    for (_, r, c) in pairs {
        if out[r].is_none() && !used[c] {
            out[r] = Some(c);
            used[c] = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(idx: usize, spans: &[(i64, i64)]) -> TrackTimeline {
        TrackTimeline {
            idx,
            track_id: format!("track-{idx}"),
            msid: None,
            ssrc: None,
            speech: spans.iter().map(|(a, b)| Span::new(*a, *b)).collect(),
        }
    }

    fn speaker(name: &str, spans: &[(i64, i64)]) -> SpeakerTimeline {
        SpeakerTimeline {
            key: format!("id:{name}"),
            name: name.to_string(),
            active: spans.iter().map(|(a, b)| Span::new(*a, *b)).collect(),
        }
    }

    /// ОПОРНЫЙ СЛУЧАЙ: двое говорят, перебивая друг друга, и каждый получает
    /// СВОЁ имя.
    #[test]
    fn interrupting_speakers_still_get_their_own_names() {
        let tracks = [
            track(0, &[(500, 2_000), (3_000, 6_000)]),
            track(1, &[(1_500, 3_500), (5_000, 7_000)]),
        ];
        // DOM-индикатор эксклюзивен: он показывает одного, поэтому его
        // интервалы короче и рвутся на перебиваниях. Похожесть от этого падает,
        // но остаётся однозначной — ровно на это и рассчитан зазор.
        let speakers = [
            speaker("Алиса", &[(520, 1_900), (3_050, 5_900)]),
            speaker("Борис", &[(1_520, 3_400), (5_050, 6_950)]),
        ];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.stage, BindingStage::Correlation);
        assert_eq!(report.name_of(0), Some("Алиса"));
        assert_eq!(report.name_of(1), Some("Борис"));
        assert_eq!(report.named(), 2);
        assert!(report.best_confidence() >= 0.7, "{:?}", report.bindings);
    }

    /// НЕОДНОЗНАЧНОСТЬ НЕ НАЗНАЧАЕТ ИМЯ НАУГАД.
    ///
    /// Две дорожки с одинаковым таймлайном (площадка продублировала поток)
    /// одинаково хорошо ложатся на одного человека. Порог такую пару
    /// пропустил бы — спасает только зазор до второго претендента.
    #[test]
    fn an_ambiguous_case_leaves_the_participant_numbered() {
        let tracks = [track(0, &[(0, 5_000)]), track(1, &[(0, 5_000)])];
        let speakers = [speaker("Алиса", &[(0, 5_000)])];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.named(), 0, "имя назначено наугад: {:?}", report.bindings);
        assert_eq!(report.stage, BindingStage::Fallback);
        assert_eq!(report.bindings[0].label(), "Участник 1");
        assert_eq!(report.bindings[1].label(), "Участник 2");
    }

    /// Уверенность ниже порога — отказ от назначения.
    #[test]
    fn a_weak_match_is_refused() {
        // Дорожка говорит десять секунд, человек по DOM — полсекунды внутри.
        let tracks = [track(0, &[(0, 10_000)])];
        let speakers = [speaker("Алиса", &[(4_000, 4_500)])];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.named(), 0, "{:?}", report.bindings);

        // А с ослабленным порогом — принимается: порог это поле, а не догма.
        let loose = AttribConfig {
            min_score: 0.04,
            min_margin: 0.0,
            ..AttribConfig::default()
        };
        let report = attribute(&tracks, &speakers, &[], &loose);
        assert_eq!(report.name_of(0), Some("Алиса"));
    }

    /// Окно наблюдения ограничивает корреляцию: всё, что позже, не участвует.
    #[test]
    fn the_correlation_window_bounds_the_comparison() {
        let tracks = [track(0, &[(100_000, 120_000)])];
        let speakers = [speaker("Алиса", &[(100_000, 120_000)])];
        let cfg = AttribConfig::default();
        assert_eq!(cfg.window_ms, 90_000, "окно 90 секунд — величина плана");
        let report = attribute(&tracks, &speakers, &[], &cfg);
        assert_eq!(report.named(), 0, "речь за окном не имеет права назначать имена");

        let wide = AttribConfig {
            window_ms: 200_000,
            ..cfg
        };
        assert_eq!(attribute(&tracks, &speakers, &[], &wide).name_of(0), Some("Алиса"));
    }

    /// Прямая связь `msid` побеждает корреляцию и даёт уверенность 1.0.
    #[test]
    fn a_direct_msid_link_wins_over_correlation() {
        // Таймлайн дорожки 0 совпадает с Алисой, но площадка прямо сказала,
        // что это Виктор, — и прямое указание сильнее любой похожести.
        let mut tracks = [track(0, &[(0, 1_000)]), track(1, &[(2_000, 3_000)])];
        tracks[0].msid = Some("id:Виктор".to_string());
        let speakers = [
            speaker("Алиса", &[(0, 1_000)]),
            speaker("Борис", &[(2_000, 3_000)]),
            speaker("Виктор", &[(4_000, 5_000)]),
        ];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.name_of(0), Some("Виктор"), "msid обязан победить таймлайн");
        assert_eq!(report.bindings[0].stage, BindingStage::Msid);
        assert_eq!(report.bindings[0].confidence, 1.0);
        // Занятого спикера корреляция второй раз не раздаёт, а свободного
        // назначает по таймлайну.
        assert_eq!(report.name_of(1), Some("Борис"));
        assert_eq!(report.bindings[1].stage, BindingStage::Correlation);
        assert_eq!(report.stage, BindingStage::Msid, "в манифест уезжает ЛУЧШАЯ ступень");
    }

    /// ОСЬ УЧАСТНИКА ВЫДЕЛЯЕТСЯ ИЗ `msid` LiveKit.
    ///
    /// Формат снят живьём на vcc.airlabs.pro: `PA_<участник>|TR_<дорожка>`.
    #[test]
    fn the_participant_half_of_a_livekit_msid_is_the_axis() {
        assert_eq!(
            msid_participant("PA_PEpGftC6osXZ|TR_AMYPfGyxhgvJQb"),
            "PA_PEpGftC6osXZ"
        );
        // Две дорожки одного человека отличаются только правой половиной.
        assert_eq!(
            msid_participant("PA_PEpGftC6osXZ|TR_вторая"),
            msid_participant("PA_PEpGftC6osXZ|TR_AMYPfGyxhgvJQb")
        );
        // Разделителя нет — осью становится вся строка: одинаковый `msid`
        // целиком это буквально один и тот же поток.
        assert_eq!(msid_participant("2e362d2c"), "2e362d2c");
        assert_eq!(msid_participant(""), "");
    }

    /// ВТОРАЯ ДОРОЖКА ЧЕЛОВЕКА ПОЛУЧАЕТ ЕГО ИМЯ, а не номер.
    ///
    /// Реальный случай площадки на LiveKit: человек включает демонстрацию
    /// экрана, и у него становится две дорожки под одним `PA_*`. Корреляция
    /// называет ту, по которой он говорил; без замыкания по оси вторая
    /// осталась бы «Участником N» — лишним собеседником в транскрипте при
    /// полностью известной связи.
    #[test]
    fn a_second_track_of_the_same_person_inherits_the_name() {
        let mut tracks = [
            track(0, &[(0, 5_000)]),
            track(1, &[]),
            track(2, &[(6_000, 9_000)]),
        ];
        tracks[0].msid = Some("PA_alice|TR_mic".to_string());
        tracks[1].msid = Some("PA_alice|TR_screen".to_string());
        tracks[2].msid = Some("PA_bob|TR_mic".to_string());
        let speakers = [
            speaker("Алиса", &[(0, 5_000)]),
            speaker("Борис", &[(6_000, 9_000)]),
        ];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.name_of(0), Some("Алиса"));
        assert_eq!(
            report.name_of(1),
            Some("Алиса"),
            "молчавшая дорожка того же человека осталась безымянной: {:?}",
            report.bindings
        );
        assert_eq!(report.bindings[1].stage, BindingStage::Msid, "связь дал msid");
        assert_eq!(report.name_of(2), Some("Борис"));
        assert_eq!(report.stage, BindingStage::Msid, "в манифест уезжает ЛУЧШАЯ ступень");
    }

    /// ЧУЖАЯ ОСЬ ИМЕНИ НЕ ОТДАЁТ.
    ///
    /// Страж от обратной ошибки: замыкание не имеет права раздать имя всем
    /// подряд, у кого `msid` вообще есть. Разные `PA_*` — разные люди, и
    /// безымянная дорожка обязана остаться «Участником N».
    #[test]
    fn a_foreign_axis_never_borrows_a_name() {
        let mut tracks = [track(0, &[(0, 5_000)]), track(1, &[])];
        tracks[0].msid = Some("PA_alice|TR_mic".to_string());
        tracks[1].msid = Some("PA_bob|TR_mic".to_string());
        let speakers = [speaker("Алиса", &[(0, 5_000)])];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.name_of(0), Some("Алиса"));
        assert_eq!(report.name_of(1), None, "имя утекло на чужую ось");
        assert_eq!(report.bindings[1].label(), "Участник 2");
    }

    /// Площадка БЕЗ `msid` ведёт себя ровно как прежде: замыкание по оси на
    /// пустых значениях не срабатывает вовсе.
    ///
    /// Это Телемост (`msid` не связан с человеком) и любая площадка, где
    /// связь не раскрыта: два безымянных трека не имеют права слипнуться.
    #[test]
    fn without_msid_nothing_is_propagated() {
        let tracks = [track(0, &[(0, 5_000)]), track(1, &[])];
        let speakers = [speaker("Алиса", &[(0, 5_000)])];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.name_of(0), Some("Алиса"));
        assert_eq!(report.name_of(1), None);
        assert_eq!(report.bindings[1].stage, BindingStage::Fallback);
    }

    /// SSRC — вторая ступень, работает так же прямо.
    #[test]
    fn ssrc_binds_directly_too() {
        let mut tracks = [track(0, &[])];
        tracks[0].ssrc = Some(42);
        let speakers = [SpeakerTimeline {
            key: "42".to_string(),
            name: "Виктор".to_string(),
            active: vec![],
        }];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.name_of(0), Some("Виктор"));
        assert_eq!(report.bindings[0].stage, BindingStage::Ssrc);
    }

    /// Четвёртая ступень: площадка прямо назвала владельца потока.
    #[test]
    fn the_platform_may_name_the_owner_itself() {
        let tracks = [track(3, &[])];
        let owners = [("track-3".to_string(), "Галина".to_string())];
        let report = attribute(&tracks, &[], &owners, &AttribConfig::default());
        assert_eq!(report.name_of(3), Some("Галина"));
        assert_eq!(report.bindings[0].stage, BindingStage::Dom);
    }

    /// РЕЖИМ SLOTS (Телемост): DOM есть, а дорожка не соответствует человеку —
    /// каскад обязан молча дойти до «Участника N», а не выдумать имя.
    ///
    /// Проба Ф0: три говорящих участника, две дорожки, `track.muted` не
    /// менялся ни разу. Здесь это выглядит как одна дорожка, в которой
    /// говорят все, и три коротких DOM-интервала.
    #[test]
    fn the_slots_mode_of_telemost_degrades_to_numbered_participants() {
        let tracks = [track(0, &[(0, 20_000)])];
        let speakers = [
            speaker("Максим К.", &[(0, 6_000)]),
            speaker("Игорь Г.", &[(6_000, 13_000)]),
            speaker("Алексей П.", &[(13_000, 20_000)]),
        ];
        let report = attribute(&tracks, &speakers, &[], &AttribConfig::default());
        assert_eq!(report.named(), 0, "в режиме Slots имён из дорожек не бывает: {:?}", report.bindings);
        assert_eq!(report.bindings[0].label(), "Участник 1");
    }

    /// Пустые входы — не паника и не «неизвестно», а честный `fallback`.
    #[test]
    fn nothing_at_all_still_gives_an_answer() {
        let report = attribute(&[], &[], &[], &AttribConfig::default());
        assert_eq!(report.stage, BindingStage::Fallback);
        assert_eq!(report.named(), 0);

        let tracks = [track(0, &[(0, 1_000)])];
        let report = attribute(&tracks, &[], &[], &AttribConfig::default());
        assert_eq!(report.bindings[0].label(), "Участник 1");
    }

    /// Назначение ищется ЦЕЛИКОМ, а не по жадному «каждому лучшего».
    ///
    /// Жадность здесь отдала бы обеим дорожкам Алису (её оценка выше в обеих
    /// строках) и потеряла бы Бориса.
    #[test]
    fn the_assignment_is_solved_as_a_whole_not_greedily() {
        let scores = vec![vec![0.9, 0.8], vec![0.85, 0.2]];
        let assignment = best_assignment(&scores);
        assert_eq!(assignment, vec![Some(1), Some(0)], "жадность потеряла второго спикера");
    }

    /// Мера совпадения — IoU, а не доля пересечения.
    #[test]
    fn the_score_is_iou_not_containment() {
        let long = [Span::new(0, 10_000)];
        let short = [Span::new(4_000, 4_500)];
        let score = overlap_score(&long, &short);
        assert!(score < 0.06, "короткий кашель не имеет права выглядеть совпадением: {score}");
        assert_eq!(overlap_score(&long, &long), 1.0);
        assert_eq!(overlap_score(&long, &[]), 0.0);
    }

    /// Строки ступеней заморожены: они уезжают в колонку `source` и в манифест.
    #[test]
    fn the_stage_names_are_frozen() {
        for (stage, want) in [
            (BindingStage::Msid, "msid"),
            (BindingStage::Ssrc, "ssrc"),
            (BindingStage::Correlation, "correlation"),
            (BindingStage::Dom, "dom"),
            (BindingStage::Fallback, "fallback"),
        ] {
            assert_eq!(stage.as_str(), want);
            let json = serde_json::to_string(&stage).unwrap();
            assert_eq!(json, format!("\"{want}\""), "serde разошёлся с as_str");
        }
    }
}
