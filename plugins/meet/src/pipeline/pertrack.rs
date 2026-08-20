//! Пер-трековая расшифровка с нарезкой по VAD.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Чего он стоит и зачем он есть
//!
//! Сумма времени речи по дорожкам примерно равна длительности встречи: люди
//! редко говорят одновременно дольше секунды. Поэтому расшифровка КАЖДОЙ
//! дорожки ПО ВЫРЕЗАННОЙ РЕЧИ стоит примерно столько же GPU-времени, сколько
//! расшифровка микса, — а даёт две вещи, которых микс не даёт никогда:
//!
//! * **перебивание разбирается**. В миксе два одновременно говорящих человека
//!   склеиваются в одну звуковую дорожку, и whisper выдаёт на этом месте либо
//!   кашу, либо реплику одного из двоих; по дорожкам это два независимых
//!   куска речи, и оба доходят до транскрипта;
//! * **имя спикера становится фактом**. В миксе спикер вычисляется
//!   перекрытием сегмента с интервалами речи — то есть догадкой с
//!   `confidence`. Здесь дорожка И ЕСТЬ человек, и подпись берётся напрямую.
//!
//! # ⚠ ВКЛЮЧАТЬ ЭТО СЕГОДНЯ НЕ НА ЧЕМ, И ЭТО НЕ ФИГУРА РЕЧИ
//!
//! Путь работает ТОЛЬКО в режиме [`crate::bot::mode::TrackMode::PerParticipant`]
//! — «дорожка соответствует человеку». Ни одной площадки в этом режиме у нас
//! **не подтверждено живой пробой**:
//!
//! | площадка | режим | чем измерено |
//! |---|---|---|
//! | Яндекс Телемост | `Slots` | живая проба Ф0 и живой прогон Ф4: два аудиотрека на трёх удалённых участниках, `track.muted` не менялся ни разу при смене говорящего |
//! | Google Meet, Teams, Zoom | не измерено | живых встреч не было (`forge-meet-f0-8bzq.2` невыполнима) |
//!
//! То есть на сегодня этот код исполняется только в тестах и на мок-встрече в
//! режиме `per_participant`. Он не выключен и не спрятан: детектор
//! [`crate::bot::mode::detect_track_mode`] включит его сам в ту минуту, когда
//! появится площадка, отдающая дорожку на человека. Делать вид, что путь
//! проверен на живой встрече, нельзя — поэтому здесь написано прямо, что не
//! проверен.
//!
//! # У ЭТОГО ПУТИ ДВА ДВИЖКА, И ОБЩЕГО У НИХ ПОЧТИ ВСЁ
//!
//! Движку специфична РОВНО ОДНА функция — [`transcribe_tracks`] (локальный
//! `whisper-cli`); её облачный близнец —
//! [`crate::cloud::stt::transcribe_tracks`]. Всё остальное движку безразлично
//! и используется обоими как есть: [`plan_cuts`], [`write_speech_only_wav`],
//! [`remap_segments`], [`attribute_direct`], [`merge_tracks`], [`CUT_PAD_MS`].
//! Копии этих функций в облачном модуле нет и быть не должно: обратный
//! пересчёт координат — единственная тонкая часть пути, и проверять его
//! round-trip-тестом надо в одном месте, а не в двух.
//!
//! Различается и бюджет, и только он: здесь считается ОЦЕНКА ВРЕМЕНИ с
//! постоянной ценой запуска процесса ([`within_budget`]), в облаке — ЗВУК
//! ([`crate::cloud::stt::within_cloud_budget`]), потому что там процесса нет.
//!
//! # Как устроена нарезка
//!
//! ```text
//! raw/track-N.wav          общая ось, между репликами — тишина
//!      │  интервалы речи этой дорожки (VAD из журнала записи)
//!      ▼
//! plan_cuts ─────────────► CutPlan { pieces: [(ось, смещение в файле, длина)] }
//!      │
//!      ▼
//! write_speech_only_wav ─► work/track-N-speech.wav   только речь, без пауз
//!      │
//!      ▼
//! whisper ───────────────► сегменты В КООРДИНАТАХ ВЫРЕЗАННОГО ФАЙЛА
//!      │
//!      ▼
//! remap_segments ────────► те же сегменты НА ОБЩЕЙ ОСИ
//! ```
//!
//! Обратное соответствие — единственная тонкая часть, и оно проверяется
//! round-trip-тестом: любая точка внутри куска, переведённая на ось вырезанного
//! файла и обратно, обязана вернуться в себя.
//!
//! # ПОЧЕМУ ПОЛЯ ВОКРУГ ИНТЕРВАЛА ОБЯЗАТЕЛЬНЫ
//!
//! VAD отмечает речь по энергии и режет по порогу: первый слог тише
//! остального слова и в интервал часто не попадает. Отрезав ровно по границам,
//! мы отдали бы whisper слово без начала — и получили бы «ривет» вместо
//! «привет». [`CUT_PAD_MS`] с обеих сторон стоит 0,6 секунды звука на реплику
//! и спасает первый слог.
//!
//! # ПОЧЕМУ ЕСТЬ БЮДЖЕТ, А НЕ «ВСЕГДА ПО ДОРОЖКАМ»
//!
//! Обоснование фазы («стоит примерно столько же») — это ГИПОТЕЗА О ЗВУКЕ, а не
//! свойство кода. На записи, где пять человек говорят одновременно весь час,
//! сумма речи по дорожкам впятеро больше длительности встречи, и пер-трековая
//! расшифровка стоила бы впятеро дороже. Поэтому гипотеза проверяется ПЕРЕД
//! запуском ([`within_budget`]): не влезли — идём по миксу и пишем причину в
//! журнал встречи. Так «не выросло существенно» становится инвариантом кода, а
//! не надеждой автора.
//!
//! ## ⚠ ИЗМЕРЕНО ЖИВЬЁМ: У КАЖДОГО ПРОГОНА ЕСТЬ ПОСТОЯННАЯ ЦЕНА
//!
//! Первая редакция бюджета считала только звук — и живой прогон
//! (`pipeline_live::per_track_transcription_beats_the_mix_on_crosstalk`,
//! whisper-cli 1.9.2, large-v3-turbo-q5_0, M1 Max) показал, что этого мало:
//!
//! | запись | путь | звука в whisper | время whisper | перебивание |
//! |---|---|---|---|---|
//! | 18 с, речь подряд | микс | 18 000 мс | **1 073 мс** | потеряно |
//! | 18 с, речь подряд | 2 дорожки | 12 319 мс (68 %) | **2 565 мс** | разобрано |
//! | 90 с, речь как на встрече | микс | 90 000 мс | **2 479 мс** | потеряно |
//! | 90 с, речь как на встрече | 2 дорожки | 12 319 мс (14 %) | **2 651 мс** | разобрано |
//!
//! На короткой записи звука МЕНЬШЕ на треть, а времени БОЛЬШЕ вдвое: каждый
//! запуск `whisper-cli` заново грузит модель, и эта постоянная цена
//! (~[`WHISPER_STARTUP_MS`] на процесс) больше самого расчёта. На записи
//! нормальной длины разница по времени уже в пределах 7 %, а расшифровка
//! строго лучше. Гипотеза задачи «суммарное время не вырастет» верна для
//! встреч и НЕВЕРНА для коротких кусков — это измерение, а не рассуждение.
//!
//! Поэтому бюджет считает не звук, а ОЦЕНКУ ВРЕМЕНИ: каждая дорожка стоит
//! своей вырезанной речи плюс постоянная цена запуска, пересчитанная в
//! «эквивалент звука» через измеренную скорость [`WHISPER_SPEEDUP`]. Час
//! встречи с пятью дорожками в бюджет влезает с запасом, восемнадцать секунд с
//! двумя — нет, и это правильно: там пер-трековый путь действительно дороже.
//!
//! ## Что микс теряет — дословно из того же прогона
//!
//! ```text
//! по миксу:     «Коллеги, я предлагаю перенести релиз на следующие уже прошли.»
//! по дорожкам:  «Коллеги, я предлагаю перенести релиз на следующую неделю.»  (Аня)
//!               «Подожди, тесты уже прошли.»                                  (Боря)
//! ```
//!
//! Микс не просто потерял перебившую реплику — он СКЛЕИЛ два голоса в одно
//! предложение, которого никто не говорил. Это и есть цена вопроса.

use std::path::{Path, PathBuf};

use super::proc::ProcError;
use super::speakers::{self, AttributedSegment, Interval, SpeakerConfig};
use super::whisper::{self, RunOptions, WhisperRequest, WhisperSegment, WhisperToken};
use crate::deps::WhisperFlags;

/// Поля вокруг интервала речи, мс — с каждой стороны.
pub const CUT_PAD_MS: i64 = 300;

/// Во сколько раз ОЦЕНКА времени пер-трекового прогона может превышать оценку
/// прогона по миксу, прежде чем путь признаётся невыгодным.
///
/// Полтора, а не единица, потому что платим мы за разное: пер-трековый путь
/// разбирает перебивания и делает подпись спикера фактом. Полуторную цену за
/// это отдать не жалко, двойную — уже жалко.
pub const SPEECH_BUDGET_RATIO: f64 = 1.5;

/// Постоянная цена ОДНОГО запуска whisper, мс. **Измерено живьём.**
///
/// Столько уходит на старт процесса и загрузку модели до первого сэмпла.
/// Величина взята с запасом вниз: на холодной машине `ggml_metal_library_init`
/// в первый раз стоит 12,5 секунды (измерено в Ф0), но это разовая цена, и
/// закладывать её в каждый прогон значило бы выключить путь навсегда.
pub const WHISPER_STARTUP_MS: i64 = 800;

/// Во сколько раз whisper на Metal быстрее реального времени. **Измерено.**
///
/// 13,5× на M1 Max с `large-v3-turbo-q5_0` (Ф5, подтверждено в Ф11). Нужно
/// только для того, чтобы пересчитать постоянную цену запуска в «эквивалент
/// звука» и сравнивать одно с другим в одних единицах.
pub const WHISPER_SPEEDUP: f64 = 13.5;

/// Суффикс файла вырезанной речи внутри `work/`.
pub const CUT_SUFFIX: &str = "-speech.wav";

/// Кусок вырезанной речи: где он был на общей оси и где лежит в файле.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPiece {
    /// Начало куска на ОБЩЕЙ оси записи, мс.
    pub axis_start_ms: i64,
    /// Начало куска в ВЫРЕЗАННОМ файле, мс.
    pub cut_start_ms: i64,
    /// Длина куска, мс. Одна и та же в обеих системах координат.
    pub len_ms: i64,
}

impl CutPiece {
    pub fn axis_end_ms(self) -> i64 {
        self.axis_start_ms + self.len_ms
    }

    pub fn cut_end_ms(self) -> i64 {
        self.cut_start_ms + self.len_ms
    }
}

/// План нарезки одной дорожки.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CutPlan {
    /// Куски в порядке возрастания. Не пересекаются и не соприкасаются.
    pub pieces: Vec<CutPiece>,
}

impl CutPlan {
    /// Сколько всего звука в вырезанном файле, мс.
    pub fn speech_ms(&self) -> i64 {
        self.pieces.iter().map(|p| p.len_ms).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Кусок, которому принадлежит НАЧАЛО отрезка в вырезанном файле.
    ///
    /// На стыке (`cut_ms` равен концу одного куска и началу следующего)
    /// побеждает СЛЕДУЮЩИЙ: начало отрезка принадлежит тому, что начинается.
    fn piece_of_start(&self, cut_ms: i64) -> Option<&CutPiece> {
        self.pieces
            .iter()
            .find(|p| cut_ms >= p.cut_start_ms && cut_ms < p.cut_end_ms())
            .or_else(|| self.pieces.last())
    }

    /// Кусок, которому принадлежит КОНЕЦ отрезка в вырезанном файле.
    ///
    /// На стыке побеждает ПРЕДЫДУЩИЙ — по той же логике, зеркально.
    fn piece_of_end(&self, cut_ms: i64) -> Option<&CutPiece> {
        self.pieces
            .iter()
            .find(|p| cut_ms > p.cut_start_ms && cut_ms <= p.cut_end_ms())
            .or_else(|| self.pieces.first())
    }

    /// Начало отрезка из вырезанного файла — на общую ось.
    pub fn to_axis_start(&self, cut_ms: i64) -> i64 {
        match self.piece_of_start(cut_ms.max(0)) {
            Some(p) => (p.axis_start_ms + (cut_ms.max(0) - p.cut_start_ms))
                .clamp(p.axis_start_ms, p.axis_end_ms()),
            // Плана нет — вырезки не было, координаты совпадают.
            None => cut_ms,
        }
    }

    /// Конец отрезка из вырезанного файла — на общую ось.
    pub fn to_axis_end(&self, cut_ms: i64) -> i64 {
        match self.piece_of_end(cut_ms.max(0)) {
            Some(p) => (p.axis_start_ms + (cut_ms.max(0) - p.cut_start_ms))
                .clamp(p.axis_start_ms, p.axis_end_ms()),
            None => cut_ms,
        }
    }

    /// Точка общей оси — в координаты вырезанного файла.
    ///
    /// `None` означает «эта миллисекунда в файл не попала»: она пришлась на
    /// вырезанную тишину. Обратного перевода у неё нет и быть не может.
    pub fn to_cut(&self, axis_ms: i64) -> Option<i64> {
        self.pieces
            .iter()
            .find(|p| axis_ms >= p.axis_start_ms && axis_ms < p.axis_end_ms())
            .map(|p| p.cut_start_ms + (axis_ms - p.axis_start_ms))
    }
}

/// Строит план нарезки по интервалам речи одной дорожки.
///
/// Что делает:
/// 1. расширяет каждый интервал на `pad_ms` в обе стороны (см. шапку модуля);
/// 2. подрезает по границам записи `[0, total_ms]`;
/// 3. сливает пересекающиеся и соприкасающиеся куски — иначе поля соседних
///    реплик наложились бы, и один и тот же звук попал бы в файл дважды;
/// 4. раскладывает по вырезанному файлу подряд, без зазоров.
pub fn plan_cuts(speech: &[(i64, i64)], pad_ms: i64, total_ms: i64) -> CutPlan {
    let pad = pad_ms.max(0);
    let mut spans: Vec<(i64, i64)> = speech
        .iter()
        .map(|(s, e)| ((s - pad).max(0), if total_ms > 0 { (e + pad).min(total_ms) } else { e + pad }))
        .filter(|(s, e)| e > s)
        .collect();
    spans.sort();

    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        match merged.last_mut() {
            // `>=`, а не `>`: соприкасающиеся куски тоже сливаются, иначе в
            // плане появился бы кусок нулевой длины между ними.
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }

    let mut pieces = Vec::with_capacity(merged.len());
    let mut cursor = 0i64;
    for (s, e) in merged {
        let len = e - s;
        pieces.push(CutPiece {
            axis_start_ms: s,
            cut_start_ms: cursor,
            len_ms: len,
        });
        cursor += len;
    }
    CutPlan { pieces }
}

/// «Эквивалент звука» постоянной цены запуска: сколько миллисекунд аудио
/// whisper успел бы посчитать за время загрузки модели.
///
/// Существует затем, чтобы сравнивать звук со звуком, а не складывать
/// миллисекунды записи с миллисекундами процессорного времени.
pub fn startup_equivalent_ms() -> i64 {
    (WHISPER_STARTUP_MS as f64 * WHISPER_SPEEDUP).round() as i64
}

/// Влезает ли пер-трековая расшифровка в бюджет времени.
///
/// `speech_ms` — сумма вырезанной речи по всем дорожкам, `tracks` — сколько
/// РАЗ придётся запустить whisper, `mix_ms` — длительность встречи (столько
/// звука уедет в whisper на миксовом пути).
///
/// Оценка обеих сторон в одних единицах — «сколько звука эквивалентно»:
///
/// ```text
/// по дорожкам:  Σ речи + число_дорожек × эквивалент_запуска
/// по миксу:     длительность встречи        (запуск один, он в обеих частях)
/// ```
///
/// Пять молчаливых слушателей почти ничего не стоят: у них нет речи, а запуск
/// на пустом плане не делается вовсе.
pub fn within_budget(speech_ms: i64, tracks: usize, mix_ms: i64) -> bool {
    if mix_ms <= 0 || tracks == 0 {
        return false;
    }
    let per_track = speech_ms + tracks as i64 * startup_equivalent_ms();
    (per_track as f64) <= mix_ms as f64 * SPEECH_BUDGET_RATIO
}

/// Пишет файл, в котором остался только звук из плана.
///
/// Читает дорожку целиком: по проводу идёт PCM16 моно 16 кГц, час записи —
/// 115 МБ, и это ровно то, что уже делает [`super::ffmpeg::mix_tracks_in_memory`]
/// сразу со всеми дорожками. ffmpeg здесь не нужен и не должен быть нужен:
/// вырезание кусков — это копирование сэмплов, а не обработка сигнала.
///
/// **Соответствие «миллисекунда → сэмпл» точное при частоте, кратной 1000**
/// (16 кГц — это ровно 16 сэмплов на миллисекунду). При некратной частоте
/// ошибка не превышает одного сэмпла на кусок, что на два порядка меньше
/// разрешения меток whisper (10 мс).
pub fn write_speech_only_wav(
    src: &Path,
    dst: &Path,
    sample_rate: u32,
    plan: &CutPlan,
) -> std::io::Result<u64> {
    use crate::ingest::wav::{read_samples, WavWriter};

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let samples = read_samples(src)?;
    let per_ms = sample_rate as i64 / 1000;
    let idx = |ms: i64| -> usize { (ms.max(0) * per_ms).max(0) as usize };

    let mut out: Vec<i16> = Vec::with_capacity(idx(plan.speech_ms()));
    for p in &plan.pieces {
        let from = idx(p.axis_start_ms).min(samples.len());
        let to = idx(p.axis_end_ms()).min(samples.len());
        if to > from {
            out.extend_from_slice(&samples[from..to]);
        }
    }

    let mut w = WavWriter::create(dst, sample_rate)?;
    w.write_frame(0, &out)?;
    w.finish()
}

/// Переносит сегменты вырезанного файла на общую ось.
///
/// # СЕГМЕНТ, ПЕРЕСЁКШИЙ СТЫК, РАЗРЕЗАЕТСЯ
///
/// В вырезанном файле две реплики, между которыми была минута тишины, лежат
/// вплотную. whisper вправе объединить их в один сегмент — и, перенесённый
/// целиком, такой сегмент занял бы на общей оси всю минуту паузы: клик по
/// реплике в плеере попал бы в тишину, а атрибуция соседних дорожек
/// перекрылась бы с ним без всякой причины.
///
/// Поэтому пересёкший стык сегмент режется по потокенным меткам (`-ojf` даёт
/// их всегда). Если меток нет — сегмент отдаётся куску с наибольшим
/// перекрытием и подрезается по его границам: соврать про длительность хуже,
/// чем соврать про принадлежность одного из двух кусков.
pub fn remap_segments(segments: &[WhisperSegment], plan: &CutPlan) -> Vec<WhisperSegment> {
    if plan.is_empty() {
        return segments.to_vec();
    }
    let mut out = Vec::with_capacity(segments.len());
    for seg in segments {
        let start = seg.start_ms.max(0);
        let end = seg.end_ms.max(start);
        let touched: Vec<&CutPiece> = plan
            .pieces
            .iter()
            .filter(|p| end.min(p.cut_end_ms()) > start.max(p.cut_start_ms))
            .collect();

        match touched.len() {
            0 => {
                // Сегмент целиком за пределами плана — такое даёт whisper на
                // хвостовой тишине. Кладём его в ближайшую точку оси, но не
                // выбрасываем: текст есть текст.
                out.push(shift_segment(seg, plan, start, end));
            }
            1 => out.push(shift_segment(seg, plan, start, end)),
            _ if seg.tokens.is_empty() => {
                let best = touched
                    .iter()
                    .max_by_key(|p| end.min(p.cut_end_ms()) - start.max(p.cut_start_ms))
                    .expect("непустой список");
                let s = start.max(best.cut_start_ms);
                let e = end.min(best.cut_end_ms());
                out.push(shift_segment(seg, plan, s, e));
            }
            _ => out.extend(split_by_tokens(seg, plan, &touched)),
        }
    }
    out.sort_by_key(|s| (s.start_ms, s.end_ms));
    out
}

/// Переносит сегмент целиком, не разрезая.
fn shift_segment(seg: &WhisperSegment, plan: &CutPlan, start: i64, end: i64) -> WhisperSegment {
    WhisperSegment {
        start_ms: plan.to_axis_start(start),
        end_ms: plan.to_axis_end(end.max(start)),
        text: seg.text.clone(),
        tokens: seg
            .tokens
            .iter()
            .map(|t| WhisperToken {
                text: t.text.clone(),
                start_ms: plan.to_axis_start(t.start_ms),
                end_ms: plan.to_axis_end(t.end_ms.max(t.start_ms)),
                p: t.p,
            })
            .collect(),
    }
}

/// Режет сегмент по стыкам, раскладывая токены по кускам.
fn split_by_tokens(
    seg: &WhisperSegment,
    plan: &CutPlan,
    touched: &[&CutPiece],
) -> Vec<WhisperSegment> {
    let mut out = Vec::new();
    for piece in touched {
        let tokens: Vec<WhisperToken> = seg
            .tokens
            .iter()
            .filter(|t| t.start_ms >= piece.cut_start_ms && t.start_ms < piece.cut_end_ms())
            .map(|t| WhisperToken {
                text: t.text.clone(),
                start_ms: plan.to_axis_start(t.start_ms),
                // Токен, вылезший за конец куска, подрезается по нему: иначе
                // хвост слова оказался бы в следующем куске, до которого на
                // общей оси может быть минута.
                end_ms: plan.to_axis_end(t.end_ms.max(t.start_ms).min(piece.cut_end_ms())),
                p: t.p,
            })
            .collect();
        if tokens.is_empty() {
            continue;
        }
        let text = tokens
            .iter()
            .map(|t| t.text.as_str())
            .collect::<String>()
            .trim()
            .to_string();
        out.push(WhisperSegment {
            start_ms: tokens.first().map(|t| t.start_ms).unwrap_or(0),
            end_ms: tokens.iter().map(|t| t.end_ms).max().unwrap_or(0),
            text,
            tokens,
        });
    }
    if out.is_empty() {
        // Ни один токен не попал ни в один кусок — вернуть сегмент целиком
        // всё равно честнее, чем потерять текст.
        return vec![shift_segment(seg, plan, seg.start_ms, seg.end_ms)];
    }
    out
}

/// Приписывает сегменты дорожки её человеку. **Без догадок.**
///
/// В миксе спикер вычисляется перекрытием и несёт `confidence`. Здесь
/// принадлежность известна: файл `raw/track-N.wav` — это микрофон одного
/// человека, и всё, что в нём звучит, сказал он. Поэтому `confidence = 1.0`, и
/// это не «округлили вверх», а разница между фактом и оценкой.
///
/// `overlapped` при этом остаётся содержательным: он означает «в это же время
/// говорил кто-то ещё» и ставится по интервалам ДРУГИХ дорожек. Ровно это и
/// есть перебивание, ради которого весь путь затевался.
pub fn attribute_direct(
    label: &str,
    segments: &[WhisperSegment],
    others: &[Interval],
    cfg: &SpeakerConfig,
) -> Vec<AttributedSegment> {
    segments
        .iter()
        .map(|seg| {
            let overlapped = others
                .iter()
                .filter(|i| i.speaker != label)
                .any(|i| {
                    let common = seg.end_ms.min(i.end_ms) - seg.start_ms.max(i.start_ms);
                    common >= cfg.min_interval_ms.max(1)
                });
            AttributedSegment {
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                text: seg.text.clone(),
                speaker: Some(label.to_string()),
                confidence: 1.0,
                overlapped,
                words: speakers::merge_tokens_to_words(&seg.tokens),
            }
        })
        .collect()
}

/// Одна дорожка на вход пер-трековой расшифровки.
#[derive(Debug, Clone)]
pub struct TrackJob {
    /// Номер дорожки — он же имя файла `raw/track-N.wav`.
    pub idx: usize,
    /// Подпись спикера: имя из каскада либо «Участник N».
    pub label: String,
    /// Исходный файл дорожки НА ОБЩЕЙ ОСИ.
    pub wav: PathBuf,
    /// Интервалы речи этой дорожки, уже с поправкой на дрейф.
    pub speech: Vec<(i64, i64)>,
}

/// Что вернула расшифровка одной дорожки.
#[derive(Debug, Clone)]
pub struct TrackTranscript {
    pub idx: usize,
    pub label: String,
    /// Сегменты УЖЕ на общей оси.
    pub segments: Vec<WhisperSegment>,
    /// Сколько миллисекунд звука ушло в whisper по этой дорожке.
    pub cut_ms: i64,
}

/// Общие параметры прогона по дорожкам.
pub struct PerTrackRequest<'a> {
    pub bin: PathBuf,
    pub model: PathBuf,
    /// Каталог транзитных файлов — туда лягут `track-N-speech.wav`.
    pub work_dir: PathBuf,
    pub language: String,
    pub sample_rate: u32,
    /// Длительность записи, мс — по ней подрезаются поля.
    pub total_ms: i64,
    pub pad_ms: i64,
    pub flags: &'a WhisperFlags,
    pub opts: RunOptions,
}

/// Расшифровывает дорожки ПО ОЧЕРЕДИ и возвращает сегменты на общей оси.
///
/// **По очереди — не деталь реализации.** Metal занимается процессом целиком;
/// два прогона дерутся за GPU и суммарно идут медленнее одного. Вызывающий
/// держит тот же единственный слот очереди whisper на весь набор дорожек: с
/// точки зрения остальных встреч это ОДИН прогон, просто из нескольких файлов.
///
/// `on_progress` получает долю ВСЕГО набора, а не отдельной дорожки: человек
/// смотрит на одну полосу, и она не имеет права откатываться к нулю на каждой
/// дорожке.
pub async fn transcribe_tracks(
    jobs: &[TrackJob],
    req: &PerTrackRequest<'_>,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<Vec<TrackTranscript>, ProcError> {
    let mut out = Vec::with_capacity(jobs.len());
    let total = jobs.len().max(1) as u32;

    for (n, job) in jobs.iter().enumerate() {
        let plan = plan_cuts(&job.speech, req.pad_ms, req.total_ms);
        if plan.is_empty() {
            // Дорожка молчала всю встречу — слушатель. Запускать на ней
            // whisper значило бы платить за тишину.
            out.push(TrackTranscript {
                idx: job.idx,
                label: job.label.clone(),
                segments: Vec::new(),
                cut_ms: 0,
            });
            continue;
        }

        let cut_wav = req
            .work_dir
            .join(format!("track-{}{}", job.idx, CUT_SUFFIX));
        write_speech_only_wav(&job.wav, &cut_wav, req.sample_rate, &plan).map_err(|e| {
            ProcError::new(
                "cut_failed",
                format!("дорожка {} не нарезалась: {e}", job.idx),
                "",
            )
        })?;

        let wreq = WhisperRequest {
            bin: req.bin.clone(),
            model: req.model.clone(),
            wav: cut_wav.clone(),
            out_prefix: req.work_dir.join(format!("track-{}", job.idx)),
            language: req.language.clone(),
            beam_size: 5,
            vad_model: None,
        };

        let done_before = n as u32;
        let step = move |p: u8| {
            if let Some(f) = on_progress {
                let overall = (done_before * 100 + u32::from(p)) / total;
                f(overall.min(100) as u8);
            }
        };
        let outcome = whisper::run(&wreq, req.flags, &req.opts, Some(&step)).await?;

        // Транзитный файл дорожки убираем сразу: держать на диске ещё одну
        // копию звука всей встречи до конца пайплайна незачем.
        let _ = std::fs::remove_file(&cut_wav);

        out.push(TrackTranscript {
            idx: job.idx,
            label: job.label.clone(),
            segments: remap_segments(&outcome.segments, &plan),
            cut_ms: plan.speech_ms(),
        });
    }
    Ok(out)
}

/// Сводит результаты дорожек в один транскрипт.
///
/// Сортировка по началу реплики, при равенстве — по подписи: без
/// детерминированного tie-break две дорожки с одинаковой меткой времени
/// давали бы разный файл при повторном прогоне одной и той же записи.
pub fn merge_tracks(
    tracks: &[TrackTranscript],
    intervals: &[Interval],
    cfg: &SpeakerConfig,
) -> Vec<AttributedSegment> {
    let mut all: Vec<AttributedSegment> = Vec::new();
    for t in tracks {
        all.extend(attribute_direct(&t.label, &t.segments, intervals, cfg));
    }
    all.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| a.speaker.cmp(&b.speaker))
            .then_with(|| a.end_ms.cmp(&b.end_ms))
    });
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: i64, end: i64, text: &str) -> WhisperSegment {
        WhisperSegment {
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
            tokens: Vec::new(),
        }
    }

    fn tok(text: &str, start: i64, end: i64) -> WhisperToken {
        WhisperToken {
            text: text.to_string(),
            start_ms: start,
            end_ms: end,
            p: 0.9,
        }
    }

    // --- план нарезки ---------------------------------------------------

    #[test]
    fn the_plan_pads_clamps_and_merges() {
        let plan = plan_cuts(&[(1_000, 2_000), (2_400, 3_000)], 300, 10_000);
        assert_eq!(
            plan.pieces,
            vec![CutPiece {
                axis_start_ms: 700,
                cut_start_ms: 0,
                len_ms: 2_600,
            }],
            "поля наложились — куски обязаны слиться в один"
        );

        let plan = plan_cuts(&[(0, 500), (9_800, 12_000)], 300, 10_000);
        assert_eq!(
            plan.pieces,
            vec![
                CutPiece { axis_start_ms: 0, cut_start_ms: 0, len_ms: 800 },
                CutPiece { axis_start_ms: 9_500, cut_start_ms: 800, len_ms: 500 },
            ],
            "поля подрезаются границами записи"
        );
    }

    /// **ROUND-TRIP.** Точка внутри куска, переведённая туда и обратно,
    /// обязана вернуться в себя. Это и есть «соответствие сохраняется».
    #[test]
    fn every_point_of_speech_survives_the_round_trip() {
        let plan = plan_cuts(&[(1_000, 2_000), (5_000, 5_500), (8_000, 9_000)], 200, 12_000);
        let mut checked = 0;
        for axis in 0..12_000 {
            if let Some(cut) = plan.to_cut(axis) {
                assert_eq!(plan.to_axis_start(cut), axis, "точка {axis} не вернулась");
                checked += 1;
            }
        }
        assert_eq!(
            checked,
            plan.speech_ms(),
            "проверено меньше точек, чем в плане речи — тест выродился"
        );
        // Тишина обратного перевода не имеет — и это тоже часть контракта.
        assert_eq!(plan.to_cut(3_000), None);
    }

    #[test]
    fn the_plan_of_a_silent_track_is_empty() {
        assert!(plan_cuts(&[], 300, 10_000).is_empty());
        assert!(plan_cuts(&[(500, 500)], 0, 10_000).is_empty(), "интервал нулевой длины");
    }

    // --- бюджет ------------------------------------------------------------

    #[test]
    fn the_budget_lets_a_real_meeting_through_and_stops_a_shouting_match() {
        // Час встречи, пятеро говорят суммарно 70 % времени — обычная планёрка.
        assert!(within_budget(2_520_000, 5, 3_600_000), "нормальная встреча не влезла");
        // Тот же час, но говорят все и одновременно: речи вчетверо больше.
        assert!(!within_budget(14_400_000, 5, 3_600_000), "непрерывный ор влез в бюджет");
        assert!(!within_budget(1_000, 1, 0), "длительность неизвестна");
        assert!(!within_budget(0, 0, 60_000), "дорожек нет — считать нечего");
    }

    /// **ЖИВОЕ ИЗМЕРЕНИЕ, ЗАКРЕПЛЁННОЕ ТЕСТОМ.**
    ///
    /// Восемнадцать секунд записи, две дорожки, 12 319 мс речи: живой прогон
    /// показал 2 565 мс против 1 073 мс у микса — то есть путь ДОРОЖЕ вдвое.
    /// Бюджет обязан это отвергать; считай он один только звук (12 319 мс
    /// против 18 000), он бы согласился, и пользователь ждал бы дольше ради
    /// красивого объяснения.
    #[test]
    fn the_budget_rejects_the_short_recording_that_measurement_proved_slower() {
        assert!(
            !within_budget(12_319, 2, 18_000),
            "короткая запись прошла бюджет: постоянная цена запуска не учтена"
        );
        // Та же встреча длиной в полторы минуты — уже выгодна: постоянная
        // цена та же, а сэкономленного звука кратно больше.
        assert!(within_budget(12_319, 2, 90_000));
    }

    // --- перенос на ось ----------------------------------------------------

    #[test]
    fn a_segment_inside_one_piece_moves_to_its_place_on_the_axis() {
        let plan = plan_cuts(&[(10_000, 12_000)], 0, 60_000);
        let got = remap_segments(&[seg(200, 900, "привет")], &plan);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].start_ms, got[0].end_ms), (10_200, 10_900));
    }

    /// Сегмент, склеенный whisper через стык, обязан РАЗРЕЗАТЬСЯ: иначе он
    /// растянулся бы на всю вырезанную паузу.
    #[test]
    fn a_segment_across_a_splice_is_cut_in_two() {
        let plan = plan_cuts(&[(0, 1_000), (30_000, 31_000)], 0, 60_000);
        let s = WhisperSegment {
            start_ms: 500,
            end_ms: 1_500,
            text: "конец начало".into(),
            tokens: vec![tok(" конец", 500, 900), tok(" начало", 1_100, 1_500)],
        };
        let got = remap_segments(&[s], &plan);
        assert_eq!(got.len(), 2, "стык не разрезан: {got:?}");
        assert_eq!((got[0].start_ms, got[0].end_ms), (500, 900));
        assert_eq!(got[0].text, "конец");
        assert_eq!((got[1].start_ms, got[1].end_ms), (30_100, 30_500));
        assert_eq!(got[1].text, "начало");
    }

    /// Тот же стык, но потокенных меток нет: сегмент уходит к большему куску
    /// и подрезается по нему — растянуться на паузу он не имеет права.
    #[test]
    fn a_tokenless_segment_across_a_splice_is_clamped_not_stretched() {
        let plan = plan_cuts(&[(0, 1_000), (30_000, 31_000)], 0, 60_000);
        let got = remap_segments(&[seg(800, 1_800, "склеилось")], &plan);
        assert_eq!(got.len(), 1);
        let width = got[0].end_ms - got[0].start_ms;
        assert!(
            width <= 1_000,
            "сегмент растянулся на вырезанную паузу: {:?}",
            got[0]
        );
    }

    #[test]
    fn without_a_plan_the_segments_are_untouched() {
        let plan = CutPlan::default();
        let got = remap_segments(&[seg(0, 100, "как есть")], &plan);
        assert_eq!(got[0].start_ms, 0);
        assert_eq!(got[0].end_ms, 100);
    }

    // --- атрибуция ---------------------------------------------------------

    #[test]
    fn the_speaker_of_a_track_is_a_fact_and_overlap_is_still_reported() {
        let cfg = SpeakerConfig::default();
        let mine = vec![seg(1_000, 3_000, "моя реплика")];
        let others = vec![
            Interval { speaker: "Аня".into(), start_ms: 0, end_ms: 5_000 },
            Interval { speaker: "Боря".into(), start_ms: 2_000, end_ms: 4_000 },
        ];
        let got = attribute_direct("Аня", &mine, &others, &cfg);
        assert_eq!(got[0].speaker.as_deref(), Some("Аня"));
        assert_eq!(got[0].confidence, 1.0, "подпись дорожки — факт, а не оценка");
        assert!(got[0].overlapped, "Боря говорил в это же время");

        let alone = attribute_direct(
            "Аня",
            &mine,
            &[Interval { speaker: "Аня".into(), start_ms: 0, end_ms: 5_000 }],
            &cfg,
        );
        assert!(!alone[0].overlapped, "перебивать было некому");
    }

    /// Перебивание: две дорожки говорят одновременно, и ОБЕ реплики доходят до
    /// транскрипта. Именно этого микс сделать не может.
    #[test]
    fn an_interruption_keeps_both_voices() {
        let cfg = SpeakerConfig::default();
        let tracks = vec![
            TrackTranscript {
                idx: 0,
                label: "Аня".into(),
                segments: vec![seg(1_000, 4_000, "я думаю что нам нужно")],
                cut_ms: 3_000,
            },
            TrackTranscript {
                idx: 1,
                label: "Боря".into(),
                segments: vec![seg(2_500, 3_500, "подожди")],
                cut_ms: 1_000,
            },
        ];
        let intervals = vec![
            Interval { speaker: "Аня".into(), start_ms: 1_000, end_ms: 4_000 },
            Interval { speaker: "Боря".into(), start_ms: 2_500, end_ms: 3_500 },
        ];
        let merged = merge_tracks(&tracks, &intervals, &cfg);
        assert_eq!(merged.len(), 2, "одна из реплик потерялась: {merged:?}");
        assert_eq!(merged[0].speaker.as_deref(), Some("Аня"));
        assert_eq!(merged[1].speaker.as_deref(), Some("Боря"));
        assert!(
            merged.iter().all(|s| s.overlapped),
            "перебивание не отмечено ни у кого"
        );
    }

    // --- нарезка файла -----------------------------------------------------

    #[test]
    fn the_cut_file_holds_only_the_speech() {
        use crate::ingest::wav::{read_samples, WavWriter};

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("track-0.wav");
        // Десять секунд: с 2-й по 3-ю уровень 1000, с 6-й по 7-ю — 2000,
        // остальное тишина.
        let mut samples = vec![0i16; 16_000 * 10];
        samples[16_000 * 2..16_000 * 3].fill(1_000);
        samples[16_000 * 6..16_000 * 7].fill(2_000);
        let mut w = WavWriter::create(&src, 16_000).unwrap();
        w.write_frame(0, &samples).unwrap();
        w.finish().unwrap();

        let plan = plan_cuts(&[(2_000, 3_000), (6_000, 7_000)], 0, 10_000);
        let dst = dir.path().join("work/track-0-speech.wav");
        write_speech_only_wav(&src, &dst, 16_000, &plan).unwrap();

        let cut = read_samples(&dst).unwrap();
        assert_eq!(cut.len(), 16_000 * 2, "в файле обязаны быть ровно две секунды");
        assert!(cut[..16_000].iter().all(|s| *s == 1_000));
        assert!(cut[16_000..].iter().all(|s| *s == 2_000));
        assert_eq!(
            cut.iter().filter(|s| **s == 0).count(),
            0,
            "в вырезанный файл попала тишина"
        );
    }
}
