//! Облачная расшифровка ВСТРЕЧИ: пер-трековый прогон поверх ядра.
//!
//! # Что переехало в `forge-voice`, а что осталось здесь
//!
//! Всё, что не знает про встречу, живёт в [`forge_voice::cloud::stt`]:
//! [`plan_windows`], [`plan_windows_at_silence`], [`build_window_args`],
//! [`build_form_fields`], [`parse_verbose_json`], [`shift_segments`],
//! [`SttOptions`], [`CloudSttOutcome`], [`transcribe_file`] и общее тело
//! [`run_windows`]. Тем же кодом расшифровываются голосовые сообщения в трёх
//! мессенджерах — окна по 90 секунд и разбор `verbose_json` у них ровно те же.
//!
//! Здесь остался ПЕР-ТРЕКОВЫЙ путь: он стоит на [`crate::pipeline::pertrack`],
//! то есть на дорожках, интервалах речи и подписи спикера — понятиях встречи,
//! которых у голосового сообщения нет и быть не может. Прежние пути импорта
//! (`crate::cloud::stt::plan_windows` и остальные) продолжают резолвиться через
//! обратный `pub use` ниже.
//!
//! # Нарезка обязательна, и режет её НАШ код
//!
//! У облачного STT жёсткий предел около **60 секунд по стене на запрос**:
//! две минуты аудио отвечают 200 за 35 с, три минуты — обрыв TCP на 62-й
//! секунде. Предел по СТЕНЕ, а не по размеру файла, поэтому окно меряется
//! длительностью аудио, а не байтами, и берётся с запасом
//! ([`super::MAX_STT_WINDOW_SECS`] = 90 с).
//!
//! # ОБЛАКО УМЕЕТ ИДТИ ПО ДОРОЖКАМ, И ЭТО ДЕШЕВЛЕ МИКСА
//!
//! Раньше облачный путь шёл только по миксу, а отказ обосновывался тем, что
//! «N загрузок вместо одной — это N-кратные деньги». **Это неверно**, и
//! опровержение лежит в шапке [`crate::pipeline::pertrack`]: пер-трековый путь
//! отдаёт движку не запись, а ВЫРЕЗАННУЮ РЕЧЬ. Живой замер оттуда: запись 90 с
//! с речью как на встрече — микс 90 000 мс звука против 12 319 мс на двух
//! дорожках, то есть 14 %. Платят в облаке за минуты звука, значит по дорожкам
//! там ДЕШЕВЛЕ, а не дороже. N-кратная цена возникла бы ровно в одном случае —
//! если бы все участники говорили одновременно всю встречу, и на этот случай
//! есть [`within_cloud_budget`].
//!
//! Даёт это то же, что и локально: подпись спикера становится фактом
//! (`raw/track-N.wav` — это микрофон одного человека), а перебивания
//! разбираются вместо того, чтобы склеиваться в кашу.
//!
//! # ОКНА РЕЖУТСЯ ПО ТИШИНЕ, А НЕ ПО АРИФМЕТИКЕ
//!
//! [`plan_windows`] режет запись ровно по 90 с — и реплика, попавшая на
//! границу, страдает. Измерено живьём на приёмке Ф17: на границе 180 с одной
//! встречи ПРОПАЛО 7 секунд речи, на той же границе другой — ЗАДВОИЛСЯ
//! фрагмент «базы данных». Арифметика при этом верна: окна стыкуются встык.
//! Ломается именно реплика на стыке.
//!
//! На пер-трековом пути лечение естественно: после
//! [`crate::pipeline::pertrack::write_speech_only_wav`] файл состоит из КУСКОВ
//! речи, между которыми в записи была тишина, и границы кусков — это и есть
//! безопасные границы разреза. [`plan_windows_on_cuts`] набирает окно из ЦЕЛЫХ
//! кусков и не режет кусок никогда — кроме одного случая, когда выбора нет:
//! кусок сам длиннее окна. Такой разрез осознанный, он попадает в
//! [`CutWindows::forced_cuts`] и оттуда в журнал встречи.
//!
//! Миксовому пути то же лечение делает [`plan_windows_at_silence`] уже в ядре.

use std::path::PathBuf;

use super::{CloudClient, MAX_STT_WINDOW_SECS};
use crate::pipeline::pertrack::{self, CutPlan, TrackJob, TrackTranscript};
use crate::pipeline::proc::ProcError;

// Нужны ТЕЛАМ существующих тестов, которые переезд не переписывал.
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use std::path::Path;

pub use forge_voice::cloud::stt::*;

/// Режет вырезанный речевой файл на окна ПО ГРАНИЦАМ КУСКОВ. **Чистая функция.**
///
/// Окно набирается как группа целых кусков суммарной длиной не больше
/// `window_ms`. Куски в [`CutPlan`] лежат в файле подряд и без зазоров, поэтому
/// окна тоже идут встык и покрывают файл целиком — терять звук здесь нельзя.
///
/// # Единственный случай, когда кусок всё-таки режется
///
/// Кусок длиннее окна (непрерывная речь дольше 90 секунд) физически не влезает
/// в запрос: предел облака — по СТЕНЕ, и обойти его нечем. Такой кусок режется
/// арифметически, но, в отличие от [`plan_windows`], это ЕДИНСТВЕННЫЙ разрез
/// внутри речи за весь прогон, он известен поимённо и уезжает в
/// [`CutWindows::forced_cuts`].
#[must_use]
pub fn plan_windows_on_cuts(plan: &CutPlan, window_ms: i64) -> CutWindows {
    let win = window_ms.clamp(1_000, MAX_STT_WINDOW_SECS as i64 * 1_000);
    let mut out = CutWindows::default();
    // Открытое окно: начало в координатах файла и текущий конец.
    let mut open: Option<(i64, i64)> = None;

    let push = |out: &mut CutWindows, start: i64, end: i64| {
        if end > start {
            out.windows.push(Window {
                index: out.windows.len(),
                start_ms: start,
                len_ms: end - start,
            });
        }
    };

    for piece in &plan.pieces {
        if piece.len_ms > win {
            // Кусок не влезает даже в пустое окно — закрываем открытое и режем
            // его сам, помечая каждый разрез.
            if let Some((s, e)) = open.take() {
                push(&mut out, s, e);
            }
            let mut cursor = piece.cut_start_ms;
            while cursor < piece.cut_end_ms() {
                let end = (cursor + win).min(piece.cut_end_ms());
                push(&mut out, cursor, end);
                if end < piece.cut_end_ms() {
                    out.forced_cuts.push(end);
                }
                cursor = end;
            }
            continue;
        }
        match open {
            // Кусок влезает в открытое окно — окно растёт до его конца.
            Some((s, _)) if piece.cut_end_ms() - s <= win => {
                open = Some((s, piece.cut_end_ms()));
            }
            // Не влезает: окно закрывается ДО куска, а не внутри него.
            Some((s, e)) => {
                push(&mut out, s, e);
                open = Some((piece.cut_start_ms, piece.cut_end_ms()));
            }
            None => open = Some((piece.cut_start_ms, piece.cut_end_ms())),
        }
    }
    if let Some((s, e)) = open.take() {
        push(&mut out, s, e);
    }
    out
}

/// Во сколько раз сумма речи по дорожкам может превышать длительность микса,
/// прежде чем ОБЛАЧНЫЙ пер-трековый путь признаётся невыгодным.
///
/// Полтора, а не единица, по той же причине, что и у локального
/// [`crate::pipeline::pertrack::SPEECH_BUDGET_RATIO`]: пер-трек разбирает
/// перебивания и делает подпись спикера фактом. Полуторную цену за это отдать
/// не жалко, двойную — уже жалко. `openai/whisper-1` стоит 0,432 ₽ за минуту
/// звука, то есть предел означает «не больше полутора цен микса».
pub const CLOUD_SPEECH_BUDGET_RATIO: f64 = 1.5;

/// Выгоден ли пер-трековый путь В ОБЛАКЕ. **Считает звук, а не время.**
///
/// # Почему нельзя взять локальный `within_budget`
///
/// Тот считает ОЦЕНКУ ВРЕМЕНИ и включает постоянную цену запуска процесса
/// (`WHISPER_STARTUP_MS`: `whisper-cli` заново грузит модель на каждой
/// дорожке). Живой замер из шапки [`crate::pipeline::pertrack`]: 18 секунд
/// записи, две дорожки — звука на треть меньше, а времени вдвое больше. В
/// облаке процесса нет и модель не грузится: цена линейна по минутам звука, а
/// стена ограничена окном в 90 секунд на запрос. Постоянной цены запуска
/// здесь просто не существует, и закладывать её значило бы выключить путь на
/// коротких встречах без единой причины.
///
/// `speech_ms` — сумма ВЫРЕЗАННОЙ речи по всем дорожкам (столько звука уедет в
/// облако), `mix_ms` — длительность встречи (столько уехало бы по миксу).
/// Замер оттуда же: 90 с встречи → 12,3 с речи на двух дорожках, 14 % от
/// микса, — типичная встреча влезает с огромным запасом. Не влезает ровно
/// один случай: люди говорят одновременно всю встречу.
#[must_use]
pub fn within_cloud_budget(speech_ms: i64, mix_ms: i64) -> bool {
    if mix_ms <= 0 || speech_ms < 0 {
        return false;
    }
    speech_ms as f64 <= mix_ms as f64 * CLOUD_SPEECH_BUDGET_RATIO
}

// --- прогон по дорожкам --------------------------------------------------------

/// Общие параметры облачного пер-трекового прогона.
///
/// Близнец [`crate::pipeline::pertrack::PerTrackRequest`], но без всего, что
/// относится к внешнему процессу: бинаря, модели и флагов whisper здесь нет и
/// быть не может.
#[derive(Debug, Clone)]
pub struct CloudTrackRequest {
    /// Каталог транзитных файлов — туда лягут `track-N-speech.wav` и окна.
    pub work_dir: PathBuf,
    /// Частота дискретизации дорожек — по ней вырезается речь.
    pub sample_rate: u32,
    /// Длительность записи, мс: по ней подрезаются поля вокруг интервалов.
    pub total_ms: i64,
    /// Поля вокруг интервала речи, мс. Обычно
    /// [`crate::pipeline::pertrack::CUT_PAD_MS`].
    pub pad_ms: i64,
}

/// Окно, не доехавшее по КОНКРЕТНОЙ дорожке.
///
/// Дыра без имени дорожки бесполезна: человек видит в транскрипте молчание и
/// не знает, чьё оно.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackGap {
    pub track_idx: usize,
    pub label: String,
    /// Начало дыры НА ОБЩЕЙ ОСИ встречи, мс — не в координатах вырезанного
    /// файла: их человеку сопоставить не с чем.
    pub axis_start_ms: i64,
    pub code: &'static str,
    pub message: String,
}

/// Место, где непрерывная речь длиннее окна была разрезана.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedCut {
    pub track_idx: usize,
    pub label: String,
    /// Точка разреза НА ОБЩЕЙ ОСИ встречи, мс.
    pub axis_ms: i64,
}

/// Итог облачной расшифровки по дорожкам.
#[derive(Debug, Clone, Default)]
pub struct CloudPerTrackOutcome {
    /// Дорожки с сегментами УЖЕ на общей оси встречи.
    pub tracks: Vec<TrackTranscript>,
    /// Сколько окон планировалось по всем дорожкам вместе.
    pub windows: usize,
    /// Окна, которые не доехали, с именем дорожки.
    pub failed: Vec<TrackGap>,
    /// Разрезы внутри непрерывной речи — см. [`plan_windows_on_cuts`].
    pub forced_cuts: Vec<ForcedCut>,
    /// Язык, как его назвало облако.
    pub language: Option<String>,
}

impl CloudPerTrackOutcome {
    /// Сколько миллисекунд звука уехало в облако по всем дорожкам.
    #[must_use]
    pub fn speech_ms(&self) -> i64 {
        self.tracks.iter().map(|t| t.cut_ms).sum()
    }

    /// Полная ли расшифровка.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.failed.is_empty()
    }

    /// Есть ли хоть один сегмент. Пусто — расшифровки не получилось.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.iter().all(|t| t.segments.is_empty())
    }

    /// Текст предупреждения о дырах для журнала встречи.
    #[must_use]
    pub fn gap_note(&self) -> Option<String> {
        if self.failed.is_empty() {
            return None;
        }
        let list: Vec<String> = self
            .failed
            .iter()
            .map(|f| {
                format!(
                    "{} на {:.0} с ({})",
                    f.label,
                    f.axis_start_ms as f64 / 1000.0,
                    f.code
                )
            })
            .collect();
        Some(format!(
            "облако не расшифровало {} из {} окон, пропуски: {}",
            self.failed.len(),
            self.windows,
            list.join(", ")
        ))
    }

    /// Текст предупреждения о вынужденных разрезах речи.
    #[must_use]
    pub fn forced_note(&self) -> Option<String> {
        if self.forced_cuts.is_empty() {
            return None;
        }
        let list: Vec<String> = self
            .forced_cuts
            .iter()
            .map(|c| format!("{} на {:.0} с", c.label, c.axis_ms as f64 / 1000.0))
            .collect();
        // Длина окна здесь НЕ НАЗЫВАЕТСЯ числом: она настраиваемая, и
        // прибитый гвоздями потолок 90 с однажды соврал бы человеку.
        Some(format!(
            "речь без пауз длиннее окна расшифровки — разрезана: {}",
            list.join(", ")
        ))
    }
}

/// Расшифровывает дорожки ПО ОЧЕРЕДИ и возвращает сегменты на общей оси.
///
/// Цепочка одной дорожки — та же, что у локального пути, и ровно теми же
/// функциями:
///
/// ```text
/// raw/track-N.wav → plan_cuts → write_speech_only_wav → work/track-N-speech.wav
///   → plan_windows_on_cuts → окна mp3 → облако → remap_segments → общая ось
/// ```
///
/// **По очереди — не деталь реализации.** Параллельные загрузки означали бы N
/// одновременных запросов к чужому сервису от ОДНОЙ встречи; предел там по
/// стене, и очередь дешевле отказа.
///
/// Молчавшая всю встречу дорожка (слушатель) не отправляется вовсе: платить за
/// тишину незачем.
///
/// `on_progress` получает долю ВСЕГО набора, а не отдельной дорожки: человек
/// смотрит на одну полосу, и она не имеет права откатываться к нулю.
///
/// Ошибкой считается только случай, когда не доехало НИ ОДНО окно НИ ОДНОЙ
/// дорожки: правило «`failed` только когда нет транскрипта» здесь то же, что и
/// на миксовом пути.
pub async fn transcribe_tracks(
    client: &CloudClient,
    jobs: &[TrackJob],
    req: &CloudTrackRequest,
    opts: &SttOptions,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<CloudPerTrackOutcome, ProcError> {
    // Планы считаются ЗАРАНЕЕ и все сразу: без этого доля выполненного
    // считалась бы от числа дорожек, и полоса стояла бы на месте всю длинную
    // дорожку, а потом прыгала.
    let plans: Vec<(usize, CutPlan, CutWindows)> = jobs
        .iter()
        .enumerate()
        .map(|(n, job)| {
            let plan = pertrack::plan_cuts(&job.speech, req.pad_ms, req.total_ms);
            let windows = plan_windows_on_cuts(&plan, opts.window_ms);
            (n, plan, windows)
        })
        .collect();
    let total_windows: usize = plans.iter().map(|(_, _, w)| w.windows.len()).sum();
    if total_windows == 0 {
        return Err(ProcError::new(
            "no_audio",
            "расшифровывать нечего: речи на дорожках нет",
            "",
        ));
    }

    let mut outcome = CloudPerTrackOutcome {
        windows: total_windows,
        ..CloudPerTrackOutcome::default()
    };
    let mut first_error: Option<ProcError> = None;
    let mut done_windows = 0usize;

    for (n, plan, cut_windows) in &plans {
        let job = &jobs[*n];
        if plan.is_empty() {
            outcome.tracks.push(TrackTranscript {
                idx: job.idx,
                label: job.label.clone(),
                segments: Vec::new(),
                cut_ms: 0,
            });
            continue;
        }

        let cut_wav = req
            .work_dir
            .join(format!("track-{}{}", job.idx, pertrack::CUT_SUFFIX));
        pertrack::write_speech_only_wav(&job.wav, &cut_wav, req.sample_rate, plan).map_err(
            |e| {
                ProcError::new(
                    "cut_failed",
                    format!("дорожка {} не нарезалась: {e}", job.idx),
                    "",
                )
            },
        )?;

        let base = done_windows;
        let step = run_windows(
            client,
            &cut_wav,
            &cut_windows.windows,
            &req.work_dir.join(WINDOW_DIR).join(format!("track-{}", job.idx)),
            opts,
            &mut |done, _| {
                if let Some(cb) = on_progress {
                    let overall = (base + done) as f64 / total_windows as f64;
                    cb((overall * 100.0).round().min(100.0) as u8);
                }
            },
        )
        .await;

        // Транзитный файл дорожки убираем сразу: держать на диске ещё одну
        // копию звука всей встречи до конца пайплайна незачем.
        let _ = std::fs::remove_file(&cut_wav);
        let (mut track_outcome, track_error) = step?;
        done_windows += cut_windows.windows.len();

        if outcome.language.is_none() {
            outcome.language = track_outcome.language.take();
        }
        if first_error.is_none() {
            first_error = track_error;
        }
        for f in &track_outcome.failed {
            outcome.failed.push(TrackGap {
                track_idx: job.idx,
                label: job.label.clone(),
                axis_start_ms: plan.to_axis_start(f.start_ms),
                code: f.code,
                message: f.message.clone(),
            });
        }
        for cut in &cut_windows.forced_cuts {
            outcome.forced_cuts.push(ForcedCut {
                track_idx: job.idx,
                label: job.label.clone(),
                axis_ms: plan.to_axis_start(*cut),
            });
        }

        track_outcome.segments.sort_by_key(|s| s.start_ms);
        outcome.tracks.push(TrackTranscript {
            idx: job.idx,
            label: job.label.clone(),
            // Координаты вырезанного файла — обратно на общую ось встречи.
            segments: pertrack::remap_segments(&track_outcome.segments, plan),
            cut_ms: plan.speech_ms(),
        });
    }

    if outcome.is_empty() {
        // Ни одного слова ни по одной дорожке — вот теперь это отказ.
        return Err(first_error.unwrap_or_else(|| {
            ProcError::new("cloud_empty", "облако не вернуло ни одного сегмента", "")
        }));
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tile_the_recording_without_gaps_or_overlap() {
        // Час встречи окнами по 90 секунд.
        let ws = plan_windows(3_600_000, 90_000);
        assert_eq!(ws.len(), 40);
        assert_eq!(ws[0].start_ms, 0);
        assert_eq!(ws[0].len_ms, 90_000);
        // Стык встык: конец окна — начало следующего.
        for pair in ws.windows(2) {
            assert_eq!(pair[0].end_ms(), pair[1].start_ms, "{pair:?}");
        }
        assert_eq!(ws.last().unwrap().end_ms(), 3_600_000, "хвост потерян");

        // Хвост короче окна.
        let tail = plan_windows(100_000, 90_000);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1].len_ms, 10_000);
        // Запись короче окна — одно окно во всю длину.
        assert_eq!(
            plan_windows(5_000, 90_000),
            vec![Window { index: 0, start_ms: 0, len_ms: 5_000 }]
        );
        assert!(plan_windows(0, 90_000).is_empty());
        assert!(plan_windows(-1, 90_000).is_empty());
    }

    /// **Окно НЕ МОЖЕТ стать длиннее измеренного предела.**
    ///
    /// Настройка «поставлю окно побольше, чтобы запросов было меньше» кончилась
    /// бы обрывом TCP на 62-й секунде у КАЖДОГО окна, то есть пустой
    /// расшифровкой при живом облаке.
    #[test]
    fn the_window_can_never_exceed_the_measured_ceiling() {
        for asked in [90_001i64, 120_000, 600_000, i64::MAX / 2] {
            let ws = plan_windows(600_000, asked);
            assert!(
                ws.iter().all(|w| w.len_ms <= MAX_STT_WINDOW_SECS as i64 * 1_000),
                "окно {asked} не обрезано: {ws:?}"
            );
        }
        // И снизу: нулевое окно дало бы бесконечный цикл.
        let ws = plan_windows(3_000, 0);
        assert_eq!(ws.len(), 3);
        assert!(ws.iter().all(|w| w.len_ms == 1_000));
    }

    /// **ОКНО ЗАКАНЧИВАЕТСЯ ДО КУСКА РЕЧИ, А НЕ ВНУТРИ НЕГО.**
    ///
    /// Ровно этим и лечится `forge-12532`: арифметическая граница 90 с
    /// пришлась бы на середину реплики, и на приёмке Ф17 это дало живьём и
    /// пропажу 7 секунд речи, и задвоение фрагмента.
    #[test]
    fn a_window_never_ends_inside_a_piece_of_speech() {
        // Куски по 40 с подряд: арифметика оборвала бы третий на 90-й секунде.
        let plan = pertrack::plan_cuts(&[(0, 40_000), (60_000, 100_000), (200_000, 240_000)], 0, 300_000);
        assert_eq!(plan.speech_ms(), 120_000);
        let cw = plan_windows_on_cuts(&plan, 90_000);

        assert!(cw.forced_cuts.is_empty(), "резать было не нужно: {cw:?}");
        assert_eq!(
            cw.windows,
            vec![
                Window { index: 0, start_ms: 0, len_ms: 80_000 },
                Window { index: 1, start_ms: 80_000, len_ms: 40_000 },
            ],
            "окно обязано кончиться на границе куска"
        );
        // Границы окон совпадают с границами кусков — то есть попадают в
        // тишину исходной записи.
        let edges: Vec<i64> = plan.pieces.iter().map(|p| p.cut_start_ms).collect();
        for w in &cw.windows {
            assert!(edges.contains(&w.start_ms), "окно {w:?} начинается в середине реплики");
        }
        // И ничего не потеряно: окна покрывают файл целиком и идут встык.
        assert_eq!(cw.windows.last().unwrap().end_ms(), plan.speech_ms());
        for pair in cw.windows.windows(2) {
            assert_eq!(pair[0].end_ms(), pair[1].start_ms, "{pair:?}");
        }
    }

    /// **РЕЧЬ БЕЗ ПАУЗ ДЛИННЕЕ ОКНА ВСЁ-ТАКИ РЕЖЕТСЯ — И ЭТО ПОМЕЧАЕТСЯ.**
    ///
    /// Человек, говорящий полторы минуты подряд, в запрос не влезает: предел
    /// облака по СТЕНЕ, обойти его нечем. Но такой разрез ровно один, он
    /// известен поимённо и уезжает в журнал встречи.
    #[test]
    fn speech_longer_than_a_window_is_cut_but_never_silently() {
        let plan = pertrack::plan_cuts(&[(0, 200_000), (300_000, 310_000)], 0, 400_000);
        let cw = plan_windows_on_cuts(&plan, 90_000);

        assert_eq!(
            cw.windows.iter().map(|w| (w.start_ms, w.len_ms)).collect::<Vec<_>>(),
            vec![(0, 90_000), (90_000, 90_000), (180_000, 20_000), (200_000, 10_000)]
        );
        assert_eq!(
            cw.forced_cuts,
            vec![90_000, 180_000],
            "разрез внутри речи обязан быть назван"
        );
        // Последний кусок начался ровно там, где кончился длинный: конец
        // длинного куска разрезом НЕ считается.
        assert_eq!(plan.pieces[1].cut_start_ms, 200_000);

        // Куски ровно в окно не режутся вовсе.
        let plan = pertrack::plan_cuts(&[(0, 90_000)], 0, 90_000);
        let cw = plan_windows_on_cuts(&plan, 90_000);
        assert_eq!(cw.windows.len(), 1);
        assert!(cw.forced_cuts.is_empty());

        // Пустой план — пустой список окон, а не окно нулевой длины.
        assert_eq!(plan_windows_on_cuts(&CutPlan::default(), 90_000), CutWindows::default());
    }

    /// **ГРАНИЦА ОКНА МИКСА УЕЗЖАЕТ В ТИШИНУ.** Это и есть `forge-12532`.
    ///
    /// Живой случай: реплика шла 169–183 с, арифметическая граница 180 с
    /// разрезала её, и хвост «базы данных» попал в транскрипт дважды.
    #[test]
    fn a_mix_window_moves_its_edge_into_the_silence() {
        // Речь как на той встрече: реплика пересекает 180-ю секунду.
        let speech = [
            (5_000i64, 60_000i64),
            (70_000, 120_000),
            (130_000, 169_000),
            (169_000, 183_200),
            (190_000, 240_000),
        ];
        let cw = plan_windows_at_silence(300_000, 90_000, &speech);

        assert!(cw.forced_cuts.is_empty(), "резать речь было не нужно: {cw:?}");
        // Ни одна граница не лежит ВНУТРИ реплики — вот весь смысл.
        for w in &cw.windows {
            for (s, e) in speech {
                assert!(
                    w.end_ms() <= s || w.end_ms() >= e,
                    "граница {} разрезала реплику {s}-{e}",
                    w.end_ms()
                );
            }
        }
        // Окна по-прежнему встык, покрывают запись целиком и не превышают
        // измеренный потолок.
        assert_eq!(cw.windows[0].start_ms, 0);
        assert_eq!(cw.windows.last().unwrap().end_ms(), 300_000);
        for pair in cw.windows.windows(2) {
            assert_eq!(pair[0].end_ms(), pair[1].start_ms, "{pair:?}");
        }
        assert!(cw.windows.iter().all(|w| w.len_ms <= 90_000), "{cw:?}");
        // Граница взята с полем ПЕРЕД началом следующей реплики: первый слог
        // тише остального слова и в интервал VAD часто не попадает.
        assert!(
            cw.windows.iter().any(|w| w.end_ms() == 70_000 - pertrack::CUT_PAD_MS),
            "{cw:?}"
        );

        // Речи нет вовсе — арифметика, как и была.
        assert_eq!(
            plan_windows_at_silence(100_000, 90_000, &[]).windows,
            plan_windows(100_000, 90_000)
        );
        // Речь без единой паузы длиннее окна: режем и НАЗЫВАЕМ место.
        let cw = plan_windows_at_silence(200_000, 90_000, &[(0, 200_000)]);
        assert_eq!(cw.forced_cuts, vec![90_000, 180_000]);
        assert_eq!(cw.windows.len(), 3);
        assert!(plan_windows_at_silence(0, 90_000, &speech).windows.is_empty());
    }

    /// **ОБЛАЧНЫЙ БЮДЖЕТ СЧИТАЕТ ЗВУК, А НЕ ВРЕМЯ ПРОЦЕССА.**
    ///
    /// Локальный `within_budget` на тех же числах отказывает: он добавляет
    /// постоянную цену запуска whisper на каждую дорожку, которой в облаке
    /// не существует.
    #[test]
    fn the_cloud_budget_counts_audio_and_not_process_startup() {
        // Живой замер из шапки pertrack: 90 с встречи, 12,3 с речи на двух
        // дорожках — 14 % от микса.
        assert!(within_cloud_budget(12_319, 90_000));
        // ...и локальный бюджет на этих же числах ОТКАЗАЛ БЫ на короткой
        // записи, потому что считает запуск процесса.
        assert!(!crate::pipeline::pertrack::within_budget(12_319, 2, 18_000));
        assert!(
            within_cloud_budget(12_319, 18_000),
            "в облаке процесса нет: 12,3 с звука дешевле 18 с микса"
        );

        // Ровно предел — влезает, чуть больше — нет.
        assert!(within_cloud_budget(150_000, 100_000));
        assert!(!within_cloud_budget(150_001, 100_000));
        // Все говорят одновременно всю встречу — единственный случай отказа.
        assert!(!within_cloud_budget(5 * 3_600_000, 3_600_000));
        // Тишина стоит ноль, а нулевой микс — не встреча.
        assert!(within_cloud_budget(0, 60_000));
        assert!(!within_cloud_budget(0, 0));
    }

    /// **НА ОБЛАКО УХОДИТ MP3, А НЕ WAV.**
    ///
    /// WAV на 193 КБ уронил их шлюз с `Maximum call stack size exceeded`.
    #[test]
    fn the_window_is_encoded_to_mp3_with_a_fast_seek() {
        let w = Window { index: 7, start_ms: 630_000, len_ms: 90_000 };
        let out = PathBuf::from("/m/work/cloud/window-0007.mp3");
        let args = build_window_args(Path::new("/m/work/mix.wav"), w, &out);
        let joined = args.join(" ");
        assert!(joined.contains("-c:a libmp3lame"), "{joined}");
        assert!(joined.contains("-b:a 32k"), "{joined}");
        assert!(joined.contains("-ac 1"), "{joined}");
        assert!(joined.ends_with("window-0007.mp3"), "{joined}");
        assert!(!joined.contains("pcm_s16le"), "сырой PCM роняет их шлюз: {joined}");

        // `-ss` и `-t` ДО `-i`: иначе последнее окно часовой встречи
        // декодировало бы час.
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let t = args.iter().position(|a| a == "-t").unwrap();
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < i && t < i, "{joined}");
        assert_eq!(args[ss + 1], "630.000");
        assert_eq!(args[t + 1], "90.000");
        assert_eq!(w.file_name(), "window-0007.mp3");
    }

    /// **ЗАПРОС НЕ ПРОСИТ ПОСЛОВНЫХ МЕТОК.**
    ///
    /// `timestamp_granularities[]=word` принимается без ошибки и молча
    /// игнорируется: поля `words` в ответе нет. Параметр, который «вроде бы
    /// работает», однажды был бы принят за работающий, и ветка разрезания
    /// сегмента по словам поехала бы на несуществующих данных.
    #[test]
    fn the_request_never_asks_for_word_timestamps() {
        for model in ["openai/whisper-1", "openai/whisper-large-v3-turbo"] {
            for lang in ["ru", "en", "", "   "] {
                let f = build_form_fields(model, lang);
                let keys: Vec<&str> = f.iter().map(|(k, _)| k.as_str()).collect();
                assert!(
                    !keys.iter().any(|k| k.contains("timestamp")),
                    "в форме появились метки: {keys:?}"
                );
                assert!(keys.contains(&"model"));
                // Встречи ходят ТОЛЬКО в whisper-1: им нужны таймкоды, а живут
                // они лишь в `verbose_json`. Дешёвая turbo, которой по умолчанию
                // расшифровывают голосовые в чатах, этот формат не принимает
                // вовсе (шлюз отвечает BAD_REQUEST), поэтому формат выводится из
                // модели — `cloud::stt_response_format`, — а не задан константой.
                let want = if model == "openai/whisper-1" { "verbose_json" } else { "json" };
                assert_eq!(
                    f.iter().find(|(k, _)| k == "response_format").map(|(_, v)| v.as_str()),
                    Some(want),
                    "{model}: неверный response_format"
                );
                assert_eq!(keys.contains(&"language"), !lang.trim().is_empty());
            }
        }

        // И по исходнику: параметра нет нигде, кроме объяснений в комментариях.
        let src = include_str!("stt.rs");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let head = code.split("mod tests").next().unwrap();
        assert!(
            !head.contains("timestamp_granularities"),
            "параметр вернулся в код"
        );
    }

    fn body(segments: &[(f64, f64, &str)]) -> String {
        let segs: Vec<Value> = segments
            .iter()
            .enumerate()
            .map(|(i, (s, e, t))| {
                serde_json::json!({
                    "id": i, "seek": 0, "start": s, "end": e, "text": t,
                    // Именно так это приходит: ЧИСЛОВЫЕ идентификаторы токенов,
                    // а не метки времени.
                    "tokens": [50364, 1770, 13],
                    "temperature": 0.0, "avg_logprob": -0.31,
                    "compression_ratio": 1.2, "no_speech_prob": 0.01
                })
            })
            .collect();
        serde_json::json!({
            "task": "transcribe", "language": "russian", "duration": 90.0,
            "text": segments.iter().map(|s| s.2).collect::<Vec<_>>().join(" "),
            "segments": segs
        })
        .to_string()
    }

    #[test]
    fn verbose_json_becomes_segments_in_milliseconds() {
        let (segs, lang) =
            parse_verbose_json(&body(&[(0.0, 3.16, " Коллеги, начинаем."), (3.4, 7.02, " Да.")]))
                .unwrap();
        assert_eq!(lang.as_deref(), Some("russian"));
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[0].end_ms, 3_160);
        assert_eq!(segs[0].text, "Коллеги, начинаем.");
        assert_eq!(segs[1].start_ms, 3_400);
        assert!(
            segs[0].tokens.is_empty(),
            "числовые идентификаторы токенов не имеют права стать метками времени"
        );
    }

    #[test]
    fn an_answer_without_segments_still_keeps_its_text() {
        let (segs, _) = parse_verbose_json(r#"{"text":"Всем привет."}"#).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Всем привет.");
        assert_eq!(segs[0].end_ms, 0, "границы подставит вызывающий");

        assert!(parse_verbose_json("не json").is_err());
        assert_eq!(
            parse_verbose_json(r#"{"text":"  "}"#).unwrap_err().code(),
            "cloud_bad_response"
        );
        // Пустой сегмент (тишина) в расшифровку не идёт.
        let (segs, _) = parse_verbose_json(&body(&[(0.0, 1.0, "   "), (1.0, 2.0, "Да")])).unwrap();
        assert_eq!(segs.len(), 1);
    }

    /// Сдвиг окна — СЛОЖЕНИЕ, и он не имеет права быть множителем.
    #[test]
    fn shifting_moves_segments_onto_the_shared_axis() {
        let (mut segs, _) = parse_verbose_json(&body(&[(0.0, 3.0, "а"), (10.0, 12.5, "б")])).unwrap();
        shift_segments(&mut segs, 90_000);
        assert_eq!(segs[0].start_ms, 90_000);
        assert_eq!(segs[0].end_ms, 93_000);
        assert_eq!(segs[1].start_ms, 100_000);
        assert_eq!(
            segs[1].end_ms - segs[1].start_ms,
            2_500,
            "сдвиг растянул сегмент: это множитель, а не сложение"
        );
        shift_segments(&mut segs, 0);
        assert_eq!(segs[0].start_ms, 90_000);
    }
}
