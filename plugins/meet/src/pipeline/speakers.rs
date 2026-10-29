//! Слияние транскрипта с таймлайном спикеров. **Чистые функции.**
//!
//! Модуль НЕ импортирует `axum`, `http` и не делает ввода-вывода. Настоящие
//! баги фазы живут здесь, и проверяются они значениями, а не прогонами.
//!
//! # Источник интервалов зависит от платформы, и оба варианта РАВНОПРАВНЫ
//!
//! | режим | откуда интервалы |
//! |---|---|
//! | `PerParticipant` (трек = человек) | пер-трековый VAD: [`SpeechEvent::On`] / [`SpeechEvent::Off`] |
//! | `Slots` (Телемост) | индикатор активного спикера из DOM: [`SpeechEvent::Active`] |
//!
//! Для Телемоста DOM — **единственный** источник, а не запасной: проба Ф0
//! показала два аудиотрека на трёх удалённых участниках и `track.muted`, не
//! менявшийся ни разу при смене говорящего. Резать там по треку нечего.
//! Поэтому [`intervals_from_events`] принимает оба вида событий одним
//! потоком: ветка «трек = человек» не может быть единственной ни в одном
//! месте пайплайна.
//!
//! # Порядок шагов атрибуции
//!
//! 1. отбросить интервалы короче `min_interval_ms` (250 мс) — это дыхание и
//!    щелчки, а не речь;
//! 2. слить соседние интервалы ОДНОГО спикера с зазором меньше
//!    `merge_gap_ms` (400 мс) — пауза между словами не означает смену
//!    говорящего;
//! 3. пересечь сегмент с интервалами по миллисекундам, двумя указателями;
//! 4. `confidence = доминирующий / сумма всех перекрытий`;
//! 5. если `confidence < min_confidence` и второй претендент занимает не
//!    меньше `second_share` — РАЗРЕЗАТЬ сегмент по потокенным меткам и
//!    пометить куски `overlapped`;
//! 6. склеить соседние сегменты одного спикера в абзацы —
//!    [`merge_into_paragraphs`].
//!
//! Шестой шаг вынесен отдельной функцией намеренно: `transcript.json` обязан
//! сохранить исходную нарезку (по ней фронт ищет и подсвечивает реплики), а
//! абзацы нужны человекочитаемому `transcript.md` и промпту саммари. Склей мы
//! всё внутри [`attribute_segments`] — вернуть гранулярность было бы уже
//! неоткуда.
//!
//! # ДЕТЕРМИНИРОВАННЫЙ TIE-BREAK ОБЯЗАТЕЛЕН
//!
//! При равном перекрытии победитель выбирается по имени спикера
//! (лексикографически меньшее). Без этого правила победа зависела бы от
//! порядка обхода `HashMap`, то есть от случайного `RandomState`: тесты
//! падали бы через раз, а расшифровка одной и той же записи давала бы разные
//! файлы при повторном прогоне.

use std::collections::HashMap;

use super::whisper::{WhisperSegment, WhisperToken};

/// Событие таймлайна речи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechEvent {
    /// Пер-трековый VAD: в дорожке началась речь.
    On { speaker: String, at_ms: i64 },
    /// Пер-трековый VAD: речь кончилась (после hangover).
    Off { speaker: String, at_ms: i64 },
    /// Платформа сменила активного спикера. `None` — говорящих нет.
    ///
    /// Событие ЭКСКЛЮЗИВНОЕ: новый активный закрывает предыдущего. Ровно так
    /// ведёт себя индикатор в DOM — «говорит» подсвечен ровно один.
    Active { speaker: Option<String>, at_ms: i64 },
}

impl SpeechEvent {
    pub fn at_ms(&self) -> i64 {
        match self {
            Self::On { at_ms, .. } | Self::Off { at_ms, .. } | Self::Active { at_ms, .. } => *at_ms,
        }
    }
}

/// Интервал речи одного спикера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    pub speaker: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Interval {
    pub fn len_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// Пороги атрибуции. Все — поля конфига, а не константы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeakerConfig {
    pub min_interval_ms: i64,
    pub merge_gap_ms: i64,
    pub min_confidence: f64,
    pub second_share: f64,
    /// Позволено ли резать спорный сегмент по границе СЛОВА.
    ///
    /// # Почему это выключатель, а не «есть метки — режем»
    ///
    /// Потому что «есть метки» проверяется по непустому
    /// [`WhisperSegment::tokens`], а облачный `openai/whisper-1` кладёт в это
    /// поле НЕ МЕТКИ ВРЕМЕНИ, а числовые идентификаторы токенов модели
    /// (измерено живым запросом; отчёт — `forge-meet-polza-probe`). Сегодня
    /// облачный разбор оставляет `tokens` пустым, и ветка не срабатывает сама
    /// собой — но «не срабатывает по совпадению» и «выключено намеренно» это
    /// разные вещи: первое чинится чужой правкой парсера, после которой
    /// сегменты резались бы по ВЫДУМАННЫМ границам МОЛЧА.
    ///
    /// Выключенный флаг не отменяет диагностику: спорный сегмент всё равно
    /// помечается `overlapped`, то есть человек видит «здесь говорили вдвоём»
    /// вместо неверно разрезанного текста.
    pub allow_word_split: bool,
}

impl Default for SpeakerConfig {
    fn default() -> Self {
        Self {
            min_interval_ms: crate::config::DEFAULT_SPEAKER_MIN_INTERVAL_MS as i64,
            merge_gap_ms: crate::config::DEFAULT_SPEAKER_MERGE_GAP_MS as i64,
            min_confidence: crate::config::DEFAULT_SPEAKER_MIN_CONFIDENCE,
            second_share: crate::config::DEFAULT_SPEAKER_SECOND_SHARE,
            allow_word_split: true,
        }
    }
}

impl From<&crate::config::MeetConfig> for SpeakerConfig {
    fn from(c: &crate::config::MeetConfig) -> Self {
        Self {
            min_interval_ms: c.speaker_min_interval_ms as i64,
            merge_gap_ms: c.speaker_merge_gap_ms as i64,
            min_confidence: c.speaker_min_confidence,
            second_share: c.speaker_second_share,
            // Пословные метки даёт только локальный whisper.cpp с `-ojf`.
            allow_word_split: c.stt_backend.is_local(),
        }
    }
}

/// Слово транскрипта, склеенное из потокенных меток.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Сегмент с назначенным спикером.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributedSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// `None` — за этот кусок никто не «расписался»: интервалов речи здесь
    /// нет вовсе. Это законный исход, а не ошибка: whisper слышит то, что
    /// VAD и DOM пропустили.
    pub speaker: Option<String>,
    /// `доминирующий / сумма перекрытий`. `0.0` при отсутствии интервалов.
    pub confidence: f64,
    /// Кусок спорного сегмента либо сегмент, где говорили одновременно.
    pub overlapped: bool,
    pub words: Vec<Word>,
}

/// Собирает интервалы речи из потока событий.
///
/// Вход может быть каким угодно, и это не паранойя: события приезжают из
/// браузера двумя независимыми каналами (VAD в worklet и DOM-проба), а
/// журнал прерванной записи дочитывается с диска.
///
/// Разбираются:
/// * несортированный вход — сортировка устойчивая, порядок одинаковых меток
///   сохраняется;
/// * дубли `on`/`on` и `off`/`off` — лишние игнорируются;
/// * `off` без `on` — игнорируется;
/// * НЕЗАКРЫТЫЙ интервал в конце записи — закрывается `end_ms`.
pub fn intervals_from_events(events: &[SpeechEvent], end_ms: i64) -> Vec<Interval> {
    let mut sorted: Vec<&SpeechEvent> = events.iter().collect();
    // `sort_by_key` устойчива: события с одной меткой остаются в порядке
    // прихода, и «off, потом on» не превращается в «on, потом off».
    sorted.sort_by_key(|e| e.at_ms());

    let mut out: Vec<Interval> = Vec::new();
    let mut open_vad: HashMap<String, i64> = HashMap::new();
    // Активный по DOM — ровно один, поэтому не мапа.
    let mut open_dom: Option<(String, i64)> = None;

    fn close(out: &mut Vec<Interval>, speaker: String, start: i64, at: i64) {
        if at > start {
            out.push(Interval {
                speaker,
                start_ms: start,
                end_ms: at,
            });
        }
    }

    for ev in sorted {
        match ev {
            SpeechEvent::On { speaker, at_ms } => {
                // Повторный `on` при открытом интервале — не ошибка: worklet
                // мог продублировать событие на границе кванта.
                open_vad.entry(speaker.clone()).or_insert(*at_ms);
            }
            SpeechEvent::Off { speaker, at_ms } => {
                if let Some(start) = open_vad.remove(speaker) {
                    close(&mut out, speaker.clone(), start, *at_ms);
                }
            }
            SpeechEvent::Active { speaker, at_ms } => {
                if let Some((prev, start)) = open_dom.take() {
                    // Тот же самый активный пришёл повторно — интервал не
                    // рвём, иначе непрерывная реплика распалась бы на куски
                    // по числу обновлений DOM.
                    if Some(&prev) == speaker.as_ref() {
                        open_dom = Some((prev, start));
                        continue;
                    }
                    close(&mut out, prev, start, *at_ms);
                }
                if let Some(name) = speaker {
                    open_dom = Some((name.clone(), *at_ms));
                }
            }
        }
    }

    // Незакрытое в конце записи: бот вышел, а `off` не пришёл — обычное дело
    // при обрыве. Терять из-за этого последнюю реплику незачем.
    let mut leftovers: Vec<(String, i64)> = open_vad.into_iter().collect();
    leftovers.sort();
    for (speaker, start) in leftovers {
        close(&mut out, speaker, start, end_ms);
    }
    if let Some((speaker, start)) = open_dom {
        close(&mut out, speaker, start, end_ms);
    }

    sort_intervals(&mut out);
    out
}

fn sort_intervals(v: &mut [Interval]) {
    v.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| a.speaker.cmp(&b.speaker))
            .then_with(|| a.end_ms.cmp(&b.end_ms))
    });
}

/// Шаги 1–2: отбросить короткое, слить близкое.
pub fn prepare_intervals(intervals: &[Interval], cfg: &SpeakerConfig) -> Vec<Interval> {
    let mut kept: Vec<Interval> = intervals
        .iter()
        .filter(|i| i.len_ms() >= cfg.min_interval_ms)
        .cloned()
        .collect();
    sort_intervals(&mut kept);

    let mut merged: Vec<Interval> = Vec::with_capacity(kept.len());
    for interval in kept {
        // Ищем последний интервал ТОГО ЖЕ спикера: между двумя репликами
        // одного человека вполне может стоять чужая.
        let target = merged
            .iter_mut()
            .rev()
            .find(|m| m.speaker == interval.speaker);
        match target {
            Some(prev) if interval.start_ms - prev.end_ms < cfg.merge_gap_ms => {
                prev.end_ms = prev.end_ms.max(interval.end_ms);
            }
            _ => merged.push(interval),
        }
    }
    sort_intervals(&mut merged);
    merged
}

/// Склеивает потокенные метки whisper в слова.
///
/// Метки в `-ojf` ПОТОКЕННЫЕ (subword: «Кол», «лег», «и»), а не пословные —
/// измерено в Ф0. Признак начала слова — ведущий пробел в тексте токена;
/// продолжение приклеивается к предыдущему слову вместе со своей меткой
/// конца.
pub fn merge_tokens_to_words(tokens: &[WhisperToken]) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    for t in tokens {
        let raw = &t.text;
        if raw.trim().is_empty() {
            continue;
        }
        let starts_word = raw.starts_with(' ') || out.is_empty();
        if starts_word {
            out.push(Word {
                text: raw.trim().to_string(),
                start_ms: t.start_ms,
                end_ms: t.end_ms,
            });
        } else if let Some(last) = out.last_mut() {
            last.text.push_str(raw.trim_end());
            last.end_ms = last.end_ms.max(t.end_ms);
        }
    }
    out
}

/// Перекрытие двух отрезков в миллисекундах.
fn overlap(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0)
}

/// Кто говорил в этом отрезке: перекрытия по спикерам.
///
/// Возвращает пары, отсортированные по убыванию перекрытия, а при равенстве —
/// ПО ИМЕНИ. Это и есть детерминированный tie-break.
fn overlaps_for(start: i64, end: i64, intervals: &[Interval], from: usize) -> Vec<(String, i64)> {
    let mut acc: HashMap<&str, i64> = HashMap::new();
    for i in intervals.iter().skip(from) {
        if i.start_ms >= end {
            break;
        }
        let o = overlap(start, end, i.start_ms, i.end_ms);
        if o > 0 {
            *acc.entry(i.speaker.as_str()).or_insert(0) += o;
        }
    }
    let mut v: Vec<(String, i64)> = acc.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Присваивает сегментам транскрипта спикеров. **Шаги 1–5.**
pub fn attribute_segments(
    segments: &[WhisperSegment],
    intervals: &[Interval],
    cfg: &SpeakerConfig,
) -> Vec<AttributedSegment> {
    let prepared = prepare_intervals(intervals, cfg);

    let mut ordered: Vec<&WhisperSegment> = segments.iter().collect();
    ordered.sort_by_key(|s| (s.start_ms, s.end_ms));

    let mut out: Vec<AttributedSegment> = Vec::with_capacity(ordered.len());
    // Указатель по интервалам: сегменты и интервалы отсортированы, поэтому
    // достаточно одного прохода. Квадратичный перебор на часовой встрече —
    // это 4000 сегментов на 2000 интервалов.
    let mut cursor = 0usize;

    for seg in ordered {
        while cursor < prepared.len() && prepared[cursor].end_ms <= seg.start_ms {
            cursor += 1;
        }
        let words = merge_tokens_to_words(&seg.tokens);
        let ranked = overlaps_for(seg.start_ms, seg.end_ms, &prepared, cursor);
        let total: i64 = ranked.iter().map(|(_, n)| *n).sum();

        if ranked.is_empty() || total == 0 {
            out.push(AttributedSegment {
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                text: seg.text.clone(),
                speaker: None,
                confidence: 0.0,
                overlapped: false,
                words,
            });
            continue;
        }

        let (top, top_ms) = ranked[0].clone();
        let confidence = top_ms as f64 / total as f64;
        let second_share = ranked
            .get(1)
            .map(|(_, n)| *n as f64 / total as f64)
            .unwrap_or(0.0);

        let disputed = confidence < cfg.min_confidence && second_share >= cfg.second_share;
        // РЕЗАТЬ НЕЧЕМ — ЗНАЧИТ ПОМЕЧАЕМ. Без `allow_word_split` облачный
        // сегмент разрезался бы по границам, которых никто не измерял.
        if disputed && cfg.allow_word_split && !words.is_empty() {
            let pieces = split_by_words(seg, &words, &prepared, cursor);
            if pieces.len() > 1 {
                out.extend(pieces);
                continue;
            }
        }

        out.push(AttributedSegment {
            start_ms: seg.start_ms,
            end_ms: seg.end_ms,
            text: seg.text.clone(),
            speaker: Some(top),
            confidence,
            // Спорный, но неразрезаемый сегмент (нет меток) всё равно обязан
            // быть помечен: пользователю важно знать, что здесь говорили
            // вдвоём.
            overlapped: disputed,
            words,
        });
    }
    out
}

/// Шаг 5: режет спорный сегмент по потокенным меткам.
///
/// Каждое слово относится к тому спикеру, чей интервал перекрывает его
/// СЕРЕДИНУ: граница слова размыта, а середина — нет.
fn split_by_words(
    seg: &WhisperSegment,
    words: &[Word],
    intervals: &[Interval],
    from: usize,
) -> Vec<AttributedSegment> {
    let mut pieces: Vec<AttributedSegment> = Vec::new();
    for w in words {
        let mid = w.start_ms + (w.end_ms - w.start_ms) / 2;
        let owner = overlaps_for(mid, mid + 1, intervals, from)
            .first()
            .map(|(name, _)| name.clone());

        match pieces.last_mut() {
            Some(last) if last.speaker == owner => {
                last.end_ms = last.end_ms.max(w.end_ms);
                last.text.push(' ');
                last.text.push_str(&w.text);
                last.words.push(w.clone());
            }
            _ => pieces.push(AttributedSegment {
                start_ms: w.start_ms,
                end_ms: w.end_ms,
                text: w.text.clone(),
                speaker: owner,
                confidence: 1.0,
                overlapped: true,
                words: vec![w.clone()],
            }),
        }
    }
    if pieces.len() <= 1 {
        return Vec::new();
    }
    // Границы кусков подтягиваем к границам сегмента: иначе между концом
    // сегмента и концом последнего слова осталась бы дыра в таймлайне.
    if let Some(first) = pieces.first_mut() {
        first.start_ms = first.start_ms.min(seg.start_ms);
    }
    if let Some(last) = pieces.last_mut() {
        last.end_ms = last.end_ms.max(seg.end_ms);
    }
    pieces
}

/// Шаг 6: склеивает соседние сегменты одного спикера в абзацы.
///
/// Разрезанные куски (`overlapped`) не склеиваются с обычными: они несут
/// другую информацию — «здесь говорили одновременно», — и слияние стёрло бы
/// её.
pub fn merge_into_paragraphs(segments: &[AttributedSegment]) -> Vec<AttributedSegment> {
    let mut out: Vec<AttributedSegment> = Vec::new();
    for seg in segments {
        match out.last_mut() {
            Some(last)
                if last.speaker == seg.speaker
                    && last.overlapped == seg.overlapped
                    && seg.start_ms >= last.start_ms =>
            {
                last.end_ms = last.end_ms.max(seg.end_ms);
                if !last.text.is_empty() && !seg.text.is_empty() {
                    last.text.push(' ');
                }
                last.text.push_str(&seg.text);
                last.words.extend(seg.words.iter().cloned());
                // Уверенность абзаца — самая слабая из слитых: абзац не может
                // быть надёжнее своей худшей реплики.
                last.confidence = last.confidence.min(seg.confidence);
            }
            _ => out.push(seg.clone()),
        }
    }
    out
}

/// Линейно растягивает таймкоды. Используется контролем дрейфа.
///
/// Множитель, а не сдвиг: дрейф накапливается пропорционально времени —
/// расхождение в конце часовой записи вдвое больше, чем в середине. Сдвиг
/// испортил бы начало записи ради конца.
pub fn remap_segment_times(segments: &mut [AttributedSegment], scale: f64) {
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let map = |v: i64| -> i64 { (v as f64 * scale).round() as i64 };
    for s in segments.iter_mut() {
        s.start_ms = map(s.start_ms);
        s.end_ms = map(s.end_ms);
        for w in s.words.iter_mut() {
            w.start_ms = map(w.start_ms);
            w.end_ms = map(w.end_ms);
        }
    }
}

/// Тот же пересчёт для интервалов речи: они живут на той же оси, и корректировать
/// нужно обе стороны, иначе атрибуция разъедется ровно на величину коррекции.
pub fn remap_intervals(intervals: &mut [Interval], scale: f64) {
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    for i in intervals.iter_mut() {
        i.start_ms = (i.start_ms as f64 * scale).round() as i64;
        i.end_ms = (i.end_ms as f64 * scale).round() as i64;
    }
}

/// Множитель коррекции дрейфа.
///
/// `None` — коррекция не нужна: расхождение меньше порога либо считать не от
/// чего.
pub fn drift_scale(timeline_ms: i64, actual_ms: i64, threshold_ms: i64) -> Option<f64> {
    if timeline_ms <= 0 || actual_ms <= 0 {
        return None;
    }
    if (timeline_ms - actual_ms).abs() < threshold_ms {
        return None;
    }
    Some(actual_ms as f64 / timeline_ms as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(s: &str, at: i64) -> SpeechEvent {
        SpeechEvent::On {
            speaker: s.into(),
            at_ms: at,
        }
    }
    fn off(s: &str, at: i64) -> SpeechEvent {
        SpeechEvent::Off {
            speaker: s.into(),
            at_ms: at,
        }
    }
    fn active(s: Option<&str>, at: i64) -> SpeechEvent {
        SpeechEvent::Active {
            speaker: s.map(str::to_string),
            at_ms: at,
        }
    }
    fn iv(s: &str, a: i64, b: i64) -> Interval {
        Interval {
            speaker: s.into(),
            start_ms: a,
            end_ms: b,
        }
    }

    fn seg(start: i64, end: i64, text: &str, words: &[(i64, i64, &str)]) -> WhisperSegment {
        WhisperSegment {
            start_ms: start,
            end_ms: end,
            text: text.into(),
            tokens: words
                .iter()
                .map(|(a, b, t)| WhisperToken {
                    text: (*t).to_string(),
                    start_ms: *a,
                    end_ms: *b,
                    p: 0.9,
                })
                .collect(),
        }
    }

    // --- intervals_from_events ---------------------------------------------

    #[test]
    fn vad_events_become_intervals() {
        let got = intervals_from_events(&[on("a", 100), off("a", 900)], 10_000);
        assert_eq!(got, vec![iv("a", 100, 900)]);
    }

    /// НЕЗАКРЫТЫЙ ИНТЕРВАЛ в конце записи закрывается концом записи.
    ///
    /// Так выглядит обрыв: бот вышел, `off` не пришёл. Потерять из-за этого
    /// последнюю реплику нельзя.
    #[test]
    fn an_interval_left_open_at_the_end_is_closed_by_the_recording() {
        let got = intervals_from_events(&[on("a", 100)], 5_000);
        assert_eq!(got, vec![iv("a", 100, 5_000)]);

        // И у DOM-таймлайна тоже.
        let got = intervals_from_events(&[active(Some("Коля"), 200)], 5_000);
        assert_eq!(got, vec![iv("Коля", 200, 5_000)]);
    }

    /// ДУБЛИ `on`/`on` и `off`/`off` не заводят лишних интервалов.
    #[test]
    fn duplicate_on_and_off_events_are_ignored() {
        let got = intervals_from_events(
            &[on("a", 100), on("a", 200), off("a", 900), off("a", 950)],
            10_000,
        );
        assert_eq!(
            got,
            vec![iv("a", 100, 900)],
            "дубль сдвинул или размножил интервал"
        );

        // `off` без `on` — просто ничего.
        assert!(intervals_from_events(&[off("a", 100)], 1_000).is_empty());
    }

    /// НЕСОРТИРОВАННЫЙ ВХОД даёт тот же результат, что и сортированный.
    #[test]
    fn unsorted_input_gives_the_same_intervals() {
        let sorted = vec![on("a", 0), off("a", 500), on("b", 600), off("b", 1_000)];
        let mut shuffled = sorted.clone();
        shuffled.reverse();
        assert_eq!(
            intervals_from_events(&sorted, 2_000),
            intervals_from_events(&shuffled, 2_000)
        );
    }

    /// DOM-таймлайн: новый активный ЗАКРЫВАЕТ предыдущего.
    ///
    /// Это основной путь для Телемоста — там трек не соответствует человеку,
    /// и другого источника спикеров нет.
    #[test]
    fn the_dom_timeline_is_exclusive() {
        let got = intervals_from_events(
            &[
                active(Some("Коля"), 0),
                active(Some("Оля"), 1_000),
                active(None, 2_500),
                active(Some("Коля"), 3_000),
            ],
            4_000,
        );
        assert_eq!(
            got,
            vec![
                iv("Коля", 0, 1_000),
                iv("Оля", 1_000, 2_500),
                iv("Коля", 3_000, 4_000),
            ]
        );
    }

    /// Повторное сообщение о ТОМ ЖЕ активном не рвёт реплику на куски.
    #[test]
    fn repeating_the_same_active_speaker_keeps_one_interval() {
        let got = intervals_from_events(
            &[
                active(Some("Коля"), 0),
                active(Some("Коля"), 500),
                active(Some("Коля"), 900),
                active(None, 1_200),
            ],
            2_000,
        );
        assert_eq!(got, vec![iv("Коля", 0, 1_200)]);
    }

    /// Оба источника в одном потоке: VAD и DOM не мешают друг другу.
    #[test]
    fn vad_and_dom_events_live_in_the_same_stream() {
        let got = intervals_from_events(
            &[
                on("track-0", 0),
                active(Some("Коля"), 100),
                off("track-0", 800),
                active(None, 900),
            ],
            1_000,
        );
        assert_eq!(got, vec![iv("track-0", 0, 800), iv("Коля", 100, 900)]);
    }

    #[test]
    fn zero_length_intervals_never_appear() {
        let got = intervals_from_events(&[on("a", 500), off("a", 500)], 1_000);
        assert!(got.is_empty(), "{got:?}");
        // И незакрытый интервал ровно в конце записи тоже не появляется.
        assert!(intervals_from_events(&[on("a", 1_000)], 1_000).is_empty());
    }

    // --- prepare_intervals ---------------------------------------------------

    #[test]
    fn short_intervals_are_dropped_and_close_ones_are_merged() {
        let cfg = SpeakerConfig::default();
        let got = prepare_intervals(
            &[
                iv("a", 0, 100), // 100 мс — короче 250, выбрасывается
                iv("a", 1_000, 2_000),
                iv("a", 2_300, 3_000), // зазор 300 < 400 — сливается
                iv("a", 5_000, 6_000), // зазор 2000 — отдельный
                iv("b", 2_100, 2_500),
            ],
            &cfg,
        );
        assert_eq!(
            got,
            vec![
                iv("a", 1_000, 3_000),
                iv("b", 2_100, 2_500),
                iv("a", 5_000, 6_000),
            ]
        );
    }

    /// Слияние — только для ОДНОГО спикера: чужая реплика между двумя своими
    /// не мешает, но и не сливается.
    #[test]
    fn merging_never_crosses_speakers() {
        let cfg = SpeakerConfig::default();
        let got = prepare_intervals(
            &[
                iv("a", 0, 1_000),
                iv("b", 1_100, 1_400),
                iv("a", 1_300, 2_000),
            ],
            &cfg,
        );
        assert_eq!(got.iter().filter(|i| i.speaker == "a").count(), 1);
        assert_eq!(got.iter().find(|i| i.speaker == "a").unwrap().end_ms, 2_000);
        assert!(got.iter().any(|i| i.speaker == "b"));
    }

    // --- склейка токенов -----------------------------------------------------

    /// Метки whisper ПОТОКЕННЫЕ: «Кол», «лег», «и» — одно слово.
    #[test]
    fn subword_tokens_are_glued_into_words() {
        let tokens = vec![
            WhisperToken {
                text: " Кол".into(),
                start_ms: 20,
                end_ms: 200,
                p: 0.9,
            },
            WhisperToken {
                text: "лег".into(),
                start_ms: 200,
                end_ms: 400,
                p: 0.9,
            },
            WhisperToken {
                text: "и,".into(),
                start_ms: 400,
                end_ms: 460,
                p: 0.9,
            },
            WhisperToken {
                text: " начинаем".into(),
                start_ms: 500,
                end_ms: 900,
                p: 0.9,
            },
        ];
        let words = merge_tokens_to_words(&tokens);
        assert_eq!(words.len(), 2, "{words:?}");
        assert_eq!(words[0].text, "Коллеги,");
        assert_eq!((words[0].start_ms, words[0].end_ms), (20, 460));
        assert_eq!(words[1].text, "начинаем");
        assert_eq!((words[1].start_ms, words[1].end_ms), (500, 900));
    }

    // --- attribute_segments --------------------------------------------------

    #[test]
    fn a_clean_segment_gets_its_speaker_with_full_confidence() {
        let segs = [seg(1_000, 2_000, "Привет", &[(1_000, 2_000, " Привет")])];
        let out = attribute_segments(&segs, &[iv("Коля", 500, 3_000)], &SpeakerConfig::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].speaker.as_deref(), Some("Коля"));
        assert_eq!(out[0].confidence, 1.0);
        assert!(!out[0].overlapped);
    }

    /// Никто не говорил — спикера нет, и это не ошибка.
    #[test]
    fn a_segment_with_no_intervals_has_no_speaker() {
        let segs = [seg(0, 1_000, "…", &[])];
        let out = attribute_segments(&segs, &[], &SpeakerConfig::default());
        assert_eq!(out[0].speaker, None);
        assert_eq!(out[0].confidence, 0.0);
    }

    /// **ПЕРЕКРЫТИЕ 50/50 С ДЕТЕРМИНИРОВАННЫМ TIE-BREAK.**
    ///
    /// Ровно тот случай, ради которого правило и введено: без него победитель
    /// зависел бы от порядка обхода `HashMap`, и тест падал бы через раз.
    /// Проверяется ДВАЖДЫ — с обоими порядками входа.
    #[test]
    fn an_exact_tie_is_broken_deterministically_by_name() {
        let cfg = SpeakerConfig {
            // Разрезание отключаем: здесь проверяется именно выбор
            // победителя.
            min_confidence: 0.0,
            ..SpeakerConfig::default()
        };
        let segs = [seg(0, 1_000, "спорно", &[])];

        let a_first = attribute_segments(&segs, &[iv("Аня", 0, 500), iv("Боря", 500, 1_000)], &cfg);
        let b_first = attribute_segments(&segs, &[iv("Боря", 500, 1_000), iv("Аня", 0, 500)], &cfg);
        assert_eq!(a_first[0].speaker.as_deref(), Some("Аня"));
        assert_eq!(
            b_first[0].speaker, a_first[0].speaker,
            "порядок входа решил исход"
        );
        assert!((a_first[0].confidence - 0.5).abs() < 1e-9);

        // И повторный прогон на тех же данных даёт то же самое — сто раз.
        for _ in 0..100 {
            let again =
                attribute_segments(&segs, &[iv("Боря", 500, 1_000), iv("Аня", 0, 500)], &cfg);
            assert_eq!(again, a_first);
        }
    }

    /// ПЕРЕБИВАНИЕ: спорный сегмент режется по словам, куски помечаются.
    #[test]
    fn an_interrupted_segment_is_split_along_the_word_timings() {
        let segs = [seg(
            0,
            2_000,
            "раз два три четыре",
            &[
                (0, 400, " раз"),
                (400, 900, " два"),
                (1_100, 1_500, " три"),
                (1_500, 2_000, " четыре"),
            ],
        )];
        let intervals = [iv("Аня", 0, 1_000), iv("Боря", 1_000, 2_000)];
        let out = attribute_segments(&segs, &intervals, &SpeakerConfig::default());

        assert_eq!(out.len(), 2, "{out:#?}");
        assert_eq!(out[0].speaker.as_deref(), Some("Аня"));
        assert_eq!(out[0].text, "раз два");
        assert_eq!(out[1].speaker.as_deref(), Some("Боря"));
        assert_eq!(out[1].text, "три четыре");
        assert!(
            out.iter().all(|s| s.overlapped),
            "куски обязаны быть помечены"
        );
        // Границы сегмента сохранены целиком — дыр в таймлайне нет.
        assert_eq!(out[0].start_ms, 0);
        assert_eq!(out[1].end_ms, 2_000);
    }

    /// Спорный сегмент БЕЗ пословных меток резать нечем — но пометить его
    /// обязаны: пользователю важно знать, что там говорили вдвоём.
    #[test]
    fn a_disputed_segment_without_word_timings_is_still_marked() {
        let segs = [seg(0, 1_000, "спорно", &[])];
        let out = attribute_segments(
            &segs,
            &[iv("Аня", 0, 500), iv("Боря", 400, 1_000)],
            &SpeakerConfig::default(),
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].overlapped);
        assert!(out[0].speaker.is_some());
    }

    /// **ОБЛАЧНЫЙ РЕЖИМ: РЕЗАТЬ ЗАПРЕЩЕНО, ПОМЕЧАТЬ ОБЯЗАНЫ.**
    ///
    /// Метки в сегменте есть (их подложил бы чужой парсер, приняв
    /// идентификаторы токенов модели за времена), но `allow_word_split`
    /// выключен — и сегмент остаётся ЦЕЛЫМ с пометкой `overlapped`, а не
    /// режется по границам, которых никто не измерял. Тот же вход при
    /// локальном пути даёт два куска, и разница между двумя половинами теста и
    /// есть проверяемое свойство.
    #[test]
    fn in_the_cloud_a_disputed_segment_is_marked_instead_of_being_split() {
        let segs = [seg(
            0,
            2_000,
            "раз два три четыре",
            &[
                (0, 400, " раз"),
                (400, 900, " два"),
                (1_100, 1_500, " три"),
                (1_500, 2_000, " четыре"),
            ],
        )];
        let intervals = [iv("Аня", 0, 1_000), iv("Боря", 1_000, 2_000)];

        let local = attribute_segments(&segs, &intervals, &SpeakerConfig::default());
        assert_eq!(local.len(), 2, "локальный путь режет по словам: {local:#?}");

        let cloud = SpeakerConfig {
            allow_word_split: false,
            ..SpeakerConfig::default()
        };
        let out = attribute_segments(&segs, &intervals, &cloud);
        assert_eq!(out.len(), 1, "облачный сегмент разрезан: {out:#?}");
        assert_eq!(out[0].text, "раз два три четыре", "текст обязан быть целым");
        assert_eq!(out[0].start_ms, 0);
        assert_eq!(out[0].end_ms, 2_000);
        assert!(
            out[0].overlapped,
            "спорный сегмент обязан быть помечен, даже когда резать нечем"
        );
        assert!(out[0].speaker.is_some(), "спикер-лидер всё равно назначен");
    }

    /// Переключатель стадии решает, позволено ли резать: облако — нельзя,
    /// локальный whisper — можно. Проверяется на СБОРКЕ порогов из конфига,
    /// потому что именно она разъедется первой.
    #[test]
    fn the_backend_decides_whether_words_may_be_split() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::testing::hermetic_config(dir.path());

        cfg.stt_backend = crate::config::SttBackend::Cloud;
        assert!(
            !SpeakerConfig::from(&cfg).allow_word_split,
            "облако пословных меток не даёт — резать нечем"
        );

        cfg.stt_backend = crate::config::SttBackend::Local;
        assert!(SpeakerConfig::from(&cfg).allow_word_split);
    }

    /// Второй претендент слишком мал — сегмент не режется.
    #[test]
    fn a_small_second_speaker_does_not_split_the_segment() {
        let segs = [seg(
            0,
            1_000,
            "раз два",
            &[(0, 500, " раз"), (500, 1_000, " два")],
        )];
        // 950 мс против 50 мс: доля второго 5 % — меньше 25 %.
        let out = attribute_segments(
            &segs,
            &[iv("Аня", 0, 950), iv("Боря", 950, 1_000)],
            &SpeakerConfig::default(),
        );
        assert_eq!(out.len(), 1);
        assert!(!out[0].overlapped, "{out:#?}");
        assert_eq!(out[0].speaker.as_deref(), Some("Аня"));
    }

    /// НЕСОРТИРОВАННЫЙ вход сегментов не ломает алгоритм двух указателей.
    #[test]
    fn segments_arriving_out_of_order_are_attributed_correctly() {
        let segs = [
            seg(2_000, 3_000, "второй", &[]),
            seg(0, 1_000, "первый", &[]),
        ];
        let out = attribute_segments(
            &segs,
            &[iv("Аня", 0, 1_000), iv("Боря", 2_000, 3_000)],
            &SpeakerConfig::default(),
        );
        assert_eq!(out[0].text, "первый");
        assert_eq!(out[0].speaker.as_deref(), Some("Аня"));
        assert_eq!(out[1].speaker.as_deref(), Some("Боря"));
    }

    /// ПРОГОН ДВАЖДЫ на одних данных даёт побитово одинаковый результат.
    #[test]
    fn running_twice_on_the_same_input_gives_the_identical_result() {
        let segs: Vec<WhisperSegment> = (0..50)
            .map(|i| {
                seg(
                    i * 1_000,
                    i * 1_000 + 900,
                    "текст",
                    &[
                        (i * 1_000, i * 1_000 + 450, " раз"),
                        (i * 1_000 + 450, i * 1_000 + 900, " два"),
                    ],
                )
            })
            .collect();
        let intervals: Vec<Interval> = (0..50)
            .flat_map(|i| {
                [
                    iv("Аня", i * 1_000, i * 1_000 + 500),
                    iv("Боря", i * 1_000 + 450, i * 1_000 + 900),
                ]
            })
            .collect();
        let a = attribute_segments(&segs, &intervals, &SpeakerConfig::default());
        let b = attribute_segments(&segs, &intervals, &SpeakerConfig::default());
        assert_eq!(a, b);
    }

    // --- абзацы --------------------------------------------------------------

    #[test]
    fn adjacent_segments_of_one_speaker_become_a_paragraph() {
        let segs = [
            seg(0, 1_000, "Привет", &[(0, 1_000, " Привет")]),
            seg(1_100, 2_000, "как дела", &[(1_100, 2_000, " как")]),
            seg(2_100, 3_000, "нормально", &[(2_100, 3_000, " нормально")]),
        ];
        let intervals = [iv("Аня", 0, 2_000), iv("Боря", 2_050, 3_000)];
        let attributed = attribute_segments(&segs, &intervals, &SpeakerConfig::default());
        let paragraphs = merge_into_paragraphs(&attributed);

        assert_eq!(paragraphs.len(), 2, "{paragraphs:#?}");
        assert_eq!(paragraphs[0].speaker.as_deref(), Some("Аня"));
        assert_eq!(paragraphs[0].text, "Привет как дела");
        assert_eq!(paragraphs[0].end_ms, 2_000);
        assert_eq!(paragraphs[1].speaker.as_deref(), Some("Боря"));
    }

    /// Разрезанные куски не сливаются с обычными: они несут другую
    /// информацию.
    #[test]
    fn overlapped_pieces_do_not_merge_with_ordinary_ones() {
        let a = AttributedSegment {
            start_ms: 0,
            end_ms: 1_000,
            text: "раз".into(),
            speaker: Some("Аня".into()),
            confidence: 1.0,
            overlapped: false,
            words: vec![],
        };
        let b = AttributedSegment {
            overlapped: true,
            start_ms: 1_000,
            end_ms: 2_000,
            text: "два".into(),
            ..a.clone()
        };
        assert_eq!(merge_into_paragraphs(&[a, b]).len(), 2);
    }

    // --- remap ----------------------------------------------------------------

    /// PROPERTY: round-trip идемпотентен.
    ///
    /// Растянули на `k`, вернули на `1/k` — получили исходное с точностью до
    /// округления в миллисекунду. Больше миллисекунды разойтись не имеет
    /// права: коррекция дрейфа применяется к уже привязанным сегментам, и
    /// систематическая ошибка накапливалась бы при каждом повторе этапа.
    #[test]
    fn remapping_there_and_back_is_idempotent() {
        let base: Vec<AttributedSegment> = (0..200)
            .map(|i| AttributedSegment {
                start_ms: i * 997,
                end_ms: i * 997 + 613,
                text: "x".into(),
                speaker: Some("Аня".into()),
                confidence: 1.0,
                overlapped: false,
                words: vec![Word {
                    text: "x".into(),
                    start_ms: i * 997 + 10,
                    end_ms: i * 997 + 100,
                }],
            })
            .collect();

        for scale in [1.0, 1.001, 0.999, 1.05, 0.9, 1.5, 0.5] {
            let mut segs = base.clone();
            remap_segment_times(&mut segs, scale);
            remap_segment_times(&mut segs, 1.0 / scale);
            for (a, b) in base.iter().zip(segs.iter()) {
                assert!(
                    (a.start_ms - b.start_ms).abs() <= 1,
                    "scale={scale}: {} → {}",
                    a.start_ms,
                    b.start_ms
                );
                assert!((a.end_ms - b.end_ms).abs() <= 1, "scale={scale}");
                assert!((a.words[0].start_ms - b.words[0].start_ms).abs() <= 1);
            }
        }
    }

    #[test]
    fn remapping_scales_the_whole_timeline_including_words() {
        let mut segs = vec![AttributedSegment {
            start_ms: 1_000,
            end_ms: 2_000,
            text: "x".into(),
            speaker: None,
            confidence: 0.0,
            overlapped: false,
            words: vec![Word {
                text: "x".into(),
                start_ms: 1_200,
                end_ms: 1_800,
            }],
        }];
        remap_segment_times(&mut segs, 2.0);
        assert_eq!((segs[0].start_ms, segs[0].end_ms), (2_000, 4_000));
        assert_eq!(
            (segs[0].words[0].start_ms, segs[0].words[0].end_ms),
            (2_400, 3_600)
        );

        // Бессмысленный множитель ничего не портит.
        let before = segs.clone();
        remap_segment_times(&mut segs, 0.0);
        remap_segment_times(&mut segs, f64::NAN);
        remap_segment_times(&mut segs, -1.0);
        assert_eq!(segs, before);
    }

    /// Порог дрейфа: полсекунды — не повод, две секунды — повод.
    #[test]
    fn the_drift_threshold_decides_whether_to_correct_at_all() {
        assert_eq!(
            drift_scale(3_600_000, 3_600_500, 1_000),
            None,
            "0,5 с — не дрейф"
        );
        let k = drift_scale(3_600_000, 3_602_000, 1_000).expect("2 с — дрейф");
        assert!((k - 3_602_000.0 / 3_600_000.0).abs() < 1e-12);
        assert_eq!(drift_scale(0, 1_000, 1_000), None);
        assert_eq!(drift_scale(1_000, 0, 1_000), None);
    }

    /// После коррекции последняя метка совпадает с фактической длительностью.
    #[test]
    fn after_the_correction_the_last_mark_matches_the_real_duration() {
        let timeline = 3_600_000i64;
        let actual = 3_602_500i64;
        let mut segs = vec![AttributedSegment {
            start_ms: 0,
            end_ms: timeline,
            text: "x".into(),
            speaker: None,
            confidence: 0.0,
            overlapped: false,
            words: vec![],
        }];
        let k = drift_scale(timeline, actual, 1_000).unwrap();
        remap_segment_times(&mut segs, k);
        assert_eq!(segs[0].end_ms, actual);
    }

    /// Интервалы речи корректируются тем же множителем — иначе атрибуция
    /// разъедется ровно на величину коррекции.
    #[test]
    fn intervals_are_remapped_by_the_same_factor() {
        let mut ivs = vec![iv("Аня", 1_000, 2_000)];
        remap_intervals(&mut ivs, 2.0);
        assert_eq!(ivs, vec![iv("Аня", 2_000, 4_000)]);
    }

    /// Пороги приезжают из конфига, а не из констант в коде.
    #[test]
    fn the_thresholds_come_from_the_config() {
        let cfg = crate::config::MeetConfig {
            speaker_min_interval_ms: 111,
            speaker_merge_gap_ms: 222,
            speaker_min_confidence: 0.33,
            speaker_second_share: 0.44,
            ..crate::config::MeetConfig::default()
        };
        let s = SpeakerConfig::from(&cfg);
        assert_eq!(s.min_interval_ms, 111);
        assert_eq!(s.merge_gap_ms, 222);
        assert!((s.min_confidence - 0.33).abs() < 1e-12);
        assert!((s.second_share - 0.44).abs() < 1e-12);
    }
}
