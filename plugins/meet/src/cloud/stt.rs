//! Облачная расшифровка: `POST /audio/transcriptions` окнами по ≤ 90 секунд.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Нарезка обязательна, и режет её НАШ код
//!
//! У облачного STT жёсткий предел около **60 секунд по стене на запрос**:
//! две минуты аудио отвечают 200 за 35 с, три минуты — обрыв TCP на 62-й
//! секунде. Предел по СТЕНЕ, а не по размеру файла, поэтому окно меряется
//! длительностью аудио, а не байтами, и берётся с запасом
//! ([`super::MAX_STT_WINDOW_SECS`] = 90 с). Ждать отказа и резать после него
//! нельзя: отказ стоит минуту стены на каждое окно.
//!
//! # На облако уходит mp3, а не WAV
//!
//! Принимаются `flac, m4a, mp3, mp4, mpeg, mpga, oga, ogg, wav, webm`, но WAV
//! на 193 КБ **уронил их шлюз** с `Maximum call stack size exceeded` (наивная
//! base64-обвязка на той стороне), тогда как mp3 того же аудио прошёл. Заодно
//! это на порядок меньше трафика: час встречи в PCM 16 кГц — 115 МБ.
//!
//! # Таймкоды посегментные, пословных НЕТ
//!
//! `timestamp_granularities[]=word` **принимается без ошибки и молча
//! игнорируется** — поля `words` в ответе нет вовсе. Поэтому параметр не
//! отправляется совсем (на это есть тест
//! [`tests::the_request_never_asks_for_word_timestamps`]): отправлять параметр,
//! который не работает, — значит однажды поверить, что он работает.
//!
//! Следствие для [`crate::pipeline::speakers`]: [`WhisperSegment::tokens`] у
//! облачных сегментов ПУСТ, и ветка «уверенность ниже порога ⇒ режем сегмент по
//! границе слова» в облачном режиме обязана деградировать до пометки
//! `overlapped`, а не выдавать границы, которых никто не измерял. Разрезать
//! нечем — слов с метками нет.
//!
//! # Одно упавшее окно не теряет встречу
//!
//! Каждое окно самостоятельно. Отказ одного попадает в
//! [`CloudSttOutcome::failed`] и в журнал встречи, а остальные окна доезжают:
//! правило «`failed` только когда нет транскрипта» сильнее полноты.
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
//! Движку специфична только отправка: план нарезки, вырезка речи, обратный
//! пересчёт координат и подпись берутся из [`crate::pipeline::pertrack`] как
//! есть — облачный прогон встаёт РЯДОМ с локальным как вторая реализация
//! одного шага, а не как его копия.
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
//! кусок сам длиннее окна (человек говорит без пауз полторы минуты). Такой
//! разрез осознанный, он попадает в [`CutWindows::forced_cuts`] и оттуда в
//! журнал встречи, чтобы дыра на этом стыке не выглядела загадкой.
//!
//! Миксовому пути то же лечение делает [`plan_windows_at_silence`]: интервалы
//! речи на общей оси уже посчитаны пайплайном, и граница окна сдвигается ВНИЗ,
//! к паузе перед пределом. Вниз, а не в обе стороны: потолок
//! [`MAX_STT_WINDOW_SECS`] измерен обрывом TCP и превышению не подлежит.
//! Интервалов нет (режим `Mixed`) — остаётся арифметика: придумывать паузы
//! вместо того, чтобы их знать, было бы хуже, чем резать честно и грубо.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{status_error, CloudClient, CloudError, MAX_STT_WINDOW_SECS, STT_RESPONSE_FORMAT};
use crate::pipeline::pertrack::{self, CutPlan, TrackJob, TrackTranscript};
use crate::pipeline::proc::{self, ProcError};
use crate::pipeline::whisper::WhisperSegment;

/// Подкаталог, куда кладутся временные mp3 окон.
pub const WINDOW_DIR: &str = "cloud";

/// Битрейт окна. Речь 16 кГц моно: 32 кбит/с — тот же выбор, что у архивного
/// AAC, и на распознавании он не сказывается.
pub const WINDOW_BITRATE: &str = "32k";

/// Одно окно расшифровки на общей оси встречи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub index: usize,
    /// Начало окна на общей оси, мс. **Это и есть смещение**, которое потом
    /// прибавляется к таймкодам ответа.
    pub start_ms: i64,
    /// Длительность окна, мс.
    pub len_ms: i64,
}

impl Window {
    /// Конец окна на общей оси, мс.
    #[must_use]
    pub fn end_ms(&self) -> i64 {
        self.start_ms + self.len_ms
    }

    /// Имя временного файла окна.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("window-{:04}.mp3", self.index)
    }
}

/// Режет запись на окна. **Чистая функция.**
///
/// Окна идут встык, без перекрытия: перекрытие удвоило бы речь на стыке, а
/// сшивать дубли по тексту — гадание. Последнее окно короче остальных.
#[must_use]
pub fn plan_windows(total_ms: i64, window_ms: i64) -> Vec<Window> {
    if total_ms <= 0 {
        return Vec::new();
    }
    // Потолок здесь, а не только в конфиге: функция чистая и вызывается из
    // тестов напрямую, а предел измерен и обсуждению не подлежит.
    let win = window_ms.clamp(1_000, MAX_STT_WINDOW_SECS as i64 * 1_000);
    let mut out = Vec::new();
    let mut start = 0i64;
    while start < total_ms {
        let len = win.min(total_ms - start);
        out.push(Window {
            index: out.len(),
            start_ms: start,
            len_ms: len,
        });
        start += len;
    }
    out
}

/// Окна вырезанного файла и места, где кусок речи пришлось разрезать.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CutWindows {
    /// Окна В КООРДИНАТАХ ВЫРЕЗАННОГО ФАЙЛА. Идут встык и покрывают его весь.
    pub windows: Vec<Window>,
    /// Точки вырезанного файла, где ОДИН кусок речи разрезан пополам.
    ///
    /// Пусто в подавляющем большинстве встреч: разрез случается только у
    /// куска, который сам длиннее окна.
    pub forced_cuts: Vec<i64>,
}

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

/// Режет ЗАПИСЬ на окна, двигая границы в тишину. **Чистая функция.**
///
/// Это лечение `forge-12532` для миксового пути: на приёмке Ф17 арифметическая
/// граница 180 с пришлась на середину реплики, и в транскрипт дважды попал
/// хвост «базы данных» — модель ДОГОВОРИЛА оборванную фразу по контексту, а
/// следующее окно расшифровало её настоящий хвост. Перекрытия окон при этом
/// не было и нет: дубль рождается достройкой, а не наложением.
///
/// Граница переносится ВНИЗ, к ближайшей паузе перед пределом окна: потолок
/// [`MAX_STT_WINDOW_SECS`] измерен, и превышать его нельзя ни на миллисекунду.
/// Точка реза берётся с полем [`pertrack::CUT_PAD_MS`] ПЕРЕД началом следующей
/// реплики — первый слог тише остального слова и в интервал VAD часто не
/// попадает, а резать по самому началу значило бы отрезать его.
///
/// Пауз в пределах окна нет вовсе (речь без перерыва длиннее окна) — режем
/// арифметически и называем это место в [`CutWindows::forced_cuts`].
#[must_use]
pub fn plan_windows_at_silence(total_ms: i64, window_ms: i64, speech: &[(i64, i64)]) -> CutWindows {
    if total_ms <= 0 {
        return CutWindows::default();
    }
    let win = window_ms.clamp(1_000, MAX_STT_WINDOW_SECS as i64 * 1_000);

    // Речь СЛИВАЕТСЯ: интервалы разных дорожек пересекаются, и пауза есть
    // только там, где молчат все.
    let mut spans: Vec<(i64, i64)> = speech.iter().copied().filter(|(s, e)| e > s).collect();
    spans.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut out = CutWindows::default();
    let mut start = 0i64;
    while start < total_ms {
        let limit = (start + win).min(total_ms);
        let end = if limit >= total_ms {
            total_ms
        } else {
            // Предел окна уже пришёлся на паузу — резать там и надо.
            match merged.iter().position(|(s, e)| limit > *s && limit < *e) {
                None => limit,
                Some(n) => {
                    // Предел попал в середину реплики: отступаем к её началу с
                    // полем, но не залезая в предыдущую реплику.
                    let floor = if n > 0 { merged[n - 1].1 } else { 0 };
                    let c = (merged[n].0 - pertrack::CUT_PAD_MS).max(floor);
                    if c > start {
                        c
                    } else {
                        // Отступать некуда: реплика длиннее окна целиком.
                        out.forced_cuts.push(limit);
                        limit
                    }
                }
            }
        };
        out.windows.push(Window {
            index: out.windows.len(),
            start_ms: start,
            len_ms: end - start,
        });
        start = end;
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

/// argv ffmpeg: вырезать окно и сжать в mp3.
///
/// `-ss` и `-t` стоят ДО `-i`: на WAV это точная и мгновенная перемотка по
/// смещению в файле, а после `-i` ffmpeg декодировал бы всё от начала записи —
/// на последнем окне часовой встречи это час работы вместо секунды.
#[must_use]
pub fn build_window_args(input: &Path, w: Window, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-ss".into(),
        format!("{:.3}", w.start_ms as f64 / 1000.0),
        "-t".into(),
        format!("{:.3}", w.len_ms as f64 / 1000.0),
        "-i".into(),
        input.display().to_string(),
        "-c:a".into(),
        "libmp3lame".into(),
        "-b:a".into(),
        WINDOW_BITRATE.into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        out.display().to_string(),
    ]
}

/// Текстовые поля формы `POST /audio/transcriptions`. **Чистая функция.**
///
/// `timestamp_granularities` здесь нет и быть не может — см. шапку модуля.
#[must_use]
pub fn build_form_fields(model: &str, language: &str) -> Vec<(String, String)> {
    let mut fields = vec![
        ("model".to_string(), model.to_string()),
        (
            "response_format".to_string(),
            STT_RESPONSE_FORMAT.to_string(),
        ),
    ];
    if !language.trim().is_empty() {
        fields.push(("language".to_string(), language.trim().to_string()));
    }
    fields
}

/// Сдвигает сегменты на общую ось встречи.
///
/// **Отдельная функция, а не `remap_segment_times`.** Тот пересчёт —
/// МНОЖИТЕЛЬ и живёт на `AttributedSegment` (коррекция дрейфа); сдвиг окна —
/// СЛОЖЕНИЕ на `WhisperSegment`, до всякой атрибуции. Подмена одного другим
/// растянула бы запись вместо переноса.
pub fn shift_segments(segments: &mut [WhisperSegment], offset_ms: i64) {
    if offset_ms == 0 {
        return;
    }
    for s in segments.iter_mut() {
        s.start_ms += offset_ms;
        s.end_ms += offset_ms;
        for t in s.tokens.iter_mut() {
            t.start_ms += offset_ms;
            t.end_ms += offset_ms;
        }
    }
}

/// Разбирает ответ `response_format=verbose_json`.
///
/// Форма OpenAI: `{"task","language","duration","text","segments":[{"start",
/// "end","text",…}]}`, время в СЕКУНДАХ дробными числами.
///
/// Поле `tokens` в этом ответе — массив ЧИСЛОВЫХ идентификаторов токенов
/// модели, а не меток времени, поэтому не читается вовсе: получить из него
/// [`crate::pipeline::whisper::WhisperToken`] невозможно.
pub fn parse_verbose_json(raw: &str) -> Result<(Vec<WhisperSegment>, Option<String>), CloudError> {
    let v: Value =
        serde_json::from_str(raw).map_err(|e| CloudError::Decode(format!("{e}: {}", head(raw))))?;
    let language = v
        .get("language")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ms = |node: &Value, key: &str| -> i64 {
        node.get(key)
            .and_then(Value::as_f64)
            .map(|s| (s * 1000.0).round() as i64)
            .unwrap_or(0)
    };

    if let Some(items) = v.get("segments").and_then(Value::as_array) {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            out.push(WhisperSegment {
                start_ms: ms(item, "start"),
                end_ms: ms(item, "end"),
                text,
                // Пословных меток облако не даёт — см. шапку модуля.
                tokens: Vec::new(),
            });
        }
        return Ok((out, language));
    }

    // Массива сегментов нет, но текст есть: так отвечают модели, у которых
    // `verbose_json` выродился в `json`. Терять расшифровку из-за этого нельзя,
    // но и врать про таймкоды тоже — сегмент растягивается на всё окно, и
    // вызывающий подставит его границы.
    let text = v
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(CloudError::Decode(format!(
            "в ответе нет ни segments, ни text: {}",
            head(raw)
        )));
    }
    Ok((
        vec![WhisperSegment {
            start_ms: 0,
            end_ms: 0,
            text,
            tokens: Vec::new(),
        }],
        language,
    ))
}

fn head(raw: &str) -> String {
    raw.chars().take(200).collect()
}

// --- прогон -------------------------------------------------------------------

/// Что запускать.
#[derive(Debug, Clone)]
pub struct SttOptions {
    /// Бинарь ffmpeg — им нарезаются и сжимаются окна.
    pub ffmpeg: PathBuf,
    pub ffmpeg_timeout: Duration,
    /// Окно, мс. Больше [`MAX_STT_WINDOW_SECS`] обрезается [`plan_windows`].
    pub window_ms: i64,
    /// Интервалы речи НА ОБЩЕЙ ОСИ записи, по которым границы окон уезжают в
    /// тишину ([`plan_windows_at_silence`]).
    ///
    /// Пусто — режем арифметически, как резали до `forge-12532`. Пустым это
    /// поле бывает законно: в режиме `Mixed` пер-трекового VAD нет, а
    /// придумывать паузы вместо того, чтобы их знать, — худший из исходов.
    /// Пер-трековому прогону поле не нужно: там границы дают куски речи.
    pub speech: Vec<(i64, i64)>,
    pub model: String,
    pub language: String,
    /// Потолок ОДНОГО окна.
    pub request_timeout: Duration,
    pub cancel: CancellationToken,
}

/// Окно, которое не доехало.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedWindow {
    pub index: usize,
    pub start_ms: i64,
    pub code: &'static str,
    pub message: String,
}

/// Итог облачной расшифровки.
#[derive(Debug, Clone, Default)]
pub struct CloudSttOutcome {
    /// Сегменты ВСЕХ доехавших окон, уже на общей оси и по возрастанию времени.
    pub segments: Vec<WhisperSegment>,
    /// Сколько окон планировалось.
    pub windows: usize,
    /// Окна, которые не доехали. Пусто — расшифровка полная.
    pub failed: Vec<FailedWindow>,
    /// Границы окон, которые пришлось поставить внутрь непрерывной речи, мс.
    pub forced_cuts: Vec<i64>,
    /// Язык, как его назвало облако.
    pub language: Option<String>,
}

impl CloudSttOutcome {
    /// Полная ли расшифровка.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.failed.is_empty()
    }

    /// Текст предупреждения для журнала встречи. `None` — предупреждать не о
    /// чем.
    #[must_use]
    pub fn gap_note(&self) -> Option<String> {
        if self.failed.is_empty() {
            return None;
        }
        let list: Vec<String> = self
            .failed
            .iter()
            .map(|f| format!("{:.0} с ({})", f.start_ms as f64 / 1000.0, f.code))
            .collect();
        Some(format!(
            "облако не расшифровало {} из {} окон, пропуски начинаются на: {}",
            self.failed.len(),
            self.windows,
            list.join(", ")
        ))
    }

    /// Текст предупреждения о границах окон, поставленных внутрь речи.
    #[must_use]
    pub fn forced_note(&self) -> Option<String> {
        if self.forced_cuts.is_empty() {
            return None;
        }
        let list: Vec<String> = self
            .forced_cuts
            .iter()
            .map(|c| format!("{:.0} с", *c as f64 / 1000.0))
            .collect();
        Some(format!(
            "речь без пауз длиннее окна расшифровки — разрезана: {}",
            list.join(", ")
        ))
    }
}

impl CloudClient {
    /// Отправляет одно окно и возвращает его сегменты В СОБСТВЕННОЙ оси окна.
    pub async fn transcribe_bytes(
        &self,
        audio: Vec<u8>,
        file_name: &str,
        model: &str,
        language: &str,
        timeout: Duration,
    ) -> Result<(Vec<WhisperSegment>, Option<String>), CloudError> {
        let part = reqwest::multipart::Part::bytes(audio)
            .file_name(file_name.to_string())
            .mime_str("audio/mpeg")
            .map_err(CloudError::from_reqwest)?;
        let mut form = reqwest::multipart::Form::new().part("file", part);
        for (k, v) in build_form_fields(model, language) {
            form = form.text(k, v);
        }

        let resp = self
            .post("audio/transcriptions")
            .timeout(timeout)
            .multipart(form)
            .send()
            .await
            .map_err(CloudError::from_reqwest)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(CloudError::from_reqwest)?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &body));
        }
        parse_verbose_json(&body)
    }
}

/// Расшифровывает файл целиком: нарезка → mp3 → облако → общая ось.
///
/// `on_progress` получает проценты по числу законченных окон — тот же
/// контракт, что у локального whisper, поэтому кадр `Progress` на фронте не
/// меняется.
///
/// Ошибкой считается только случай, когда не доехало НИ ОДНО окно: одно
/// упавшее окно — это дыра в расшифровке, а не потерянная встреча.
pub async fn transcribe_file(
    client: &CloudClient,
    wav: &Path,
    total_ms: i64,
    work_dir: &Path,
    opts: &SttOptions,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<CloudSttOutcome, ProcError> {
    // Границы окон уезжают в тишину, если интервалы речи известны: реплика,
    // разрезанная посередине, задваивается достройкой модели (`forge-12532`).
    let planned = if opts.speech.is_empty() {
        CutWindows {
            windows: plan_windows(total_ms, opts.window_ms),
            forced_cuts: Vec::new(),
        }
    } else {
        plan_windows_at_silence(total_ms, opts.window_ms, &opts.speech)
    };
    let windows = planned.windows;
    if windows.is_empty() {
        return Err(ProcError::new(
            "no_audio",
            "расшифровывать нечего: длительность записи нулевая",
            "",
        ));
    }

    let (mut outcome, first_error) = run_windows(
        client,
        wav,
        &windows,
        &work_dir.join(WINDOW_DIR),
        opts,
        &mut |done, total| {
            if let Some(cb) = on_progress {
                cb(((done as f64 / total as f64) * 100.0).round().min(100.0) as u8);
            }
        },
    )
    .await?;

    if outcome.segments.is_empty() {
        // Ни одного слова — вот теперь это отказ расшифровки.
        return Err(first_error.unwrap_or_else(|| {
            ProcError::new("cloud_empty", "облако не вернуло ни одного сегмента", "")
        }));
    }
    outcome.forced_cuts = planned.forced_cuts;
    // Окна идут по порядку, но ответ окна может перекрыть границу; сортировка
    // дешевле, чем разбираться в этом на стыке.
    outcome.segments.sort_by_key(|s| s.start_ms);
    Ok(outcome)
}

/// Гоняет ГОТОВЫЙ список окон одного файла. Общее тело обоих путей.
///
/// Сегменты возвращаются на оси ТОГО ФАЙЛА, который прислали: миксу это сразу
/// общая ось встречи, вырезанному речевому файлу — его собственная, и её потом
/// пересчитывает [`pertrack::remap_segments`].
///
/// `Err` здесь — только отмена и несоздавшийся каталог. Отказ окна не ошибка:
/// он ложится в `failed`, а `Option<ProcError>` несёт ПЕРВЫЙ отказ, чтобы
/// вызывающий мог назвать причину, если не доехало вообще ничего.
async fn run_windows(
    client: &CloudClient,
    wav: &Path,
    windows: &[Window],
    dir: &Path,
    opts: &SttOptions,
    on_done: &mut (dyn FnMut(usize, usize) + Send),
) -> Result<(CloudSttOutcome, Option<ProcError>), ProcError> {
    std::fs::create_dir_all(dir).map_err(|e| {
        ProcError::new(
            "cloud_workdir",
            format!("не создаётся каталог окон {}: {e}", dir.display()),
            "",
        )
    })?;

    let mut outcome = CloudSttOutcome {
        windows: windows.len(),
        ..CloudSttOutcome::default()
    };
    let mut first_error: Option<ProcError> = None;

    for (n, w) in windows.iter().enumerate() {
        if opts.cancel.is_cancelled() {
            return Err(ProcError::new("cancelled", "расшифровка отменена", ""));
        }
        let mp3 = dir.join(w.file_name());
        let step = window_once(client, wav, *w, &mp3, opts).await;
        // Временный файл не переживает окно: час встречи это 40 окон, и
        // оставленные mp3 лежали бы в каталоге встречи навсегда.
        let _ = std::fs::remove_file(&mp3);

        match step {
            Ok((mut segs, lang)) => {
                if outcome.language.is_none() {
                    outcome.language = lang;
                }
                // Вырожденный ответ без таймкодов растягивается на окно: иначе
                // весь его текст лёг бы в нулевую точку встречи.
                if segs.len() == 1 && segs[0].start_ms == 0 && segs[0].end_ms == 0 {
                    segs[0].end_ms = w.len_ms;
                }
                shift_segments(&mut segs, w.start_ms);
                outcome.segments.extend(segs);
            }
            Err(err) => {
                if err.code == "cancelled" {
                    return Err(err);
                }
                tracing::warn!(
                    target: "forge_meet",
                    window = w.index,
                    start_ms = w.start_ms,
                    code = err.code,
                    "forge-meet: окно облачной расшифровки не доехало: {}",
                    err.message
                );
                outcome.failed.push(FailedWindow {
                    index: w.index,
                    start_ms: w.start_ms,
                    code: err.code,
                    message: err.full_message(),
                });
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
        on_done(n + 1, windows.len());
    }
    Ok((outcome, first_error))
}

/// Одно окно: нарезка, отправка, разбор.
async fn window_once(
    client: &CloudClient,
    wav: &Path,
    w: Window,
    mp3: &Path,
    opts: &SttOptions,
) -> Result<(Vec<WhisperSegment>, Option<String>), ProcError> {
    let args = build_window_args(wav, w, mp3);
    proc::run(&opts.ffmpeg, &args, opts.ffmpeg_timeout, &opts.cancel)
        .await
        .map_err(|e| {
            if e.code == "cancelled" {
                e
            } else {
                ProcError::new("cloud_encode_failed", e.message.clone(), e.stderr_tail.clone())
            }
        })?;

    let bytes = std::fs::read(mp3).map_err(|e| {
        ProcError::new(
            "cloud_encode_failed",
            format!("окно {} не читается с диска: {e}", w.index),
            "",
        )
    })?;
    if bytes.is_empty() {
        return Err(ProcError::new(
            "cloud_encode_failed",
            format!("окно {} получилось пустым", w.index),
            "",
        ));
    }

    let name = w.file_name();
    let send = client.transcribe_bytes(
        bytes,
        &name,
        &opts.model,
        &opts.language,
        opts.request_timeout,
    );
    tokio::select! {
        biased;
        _ = opts.cancel.cancelled() => Err(ProcError::new("cancelled", "расшифровка отменена", "")),
        res = send => res.map_err(ProcError::from),
    }
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
                assert_eq!(
                    f.iter().find(|(k, _)| k == "response_format").map(|(_, v)| v.as_str()),
                    Some("verbose_json"),
                    "без verbose_json таймкодов не будет вовсе"
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
