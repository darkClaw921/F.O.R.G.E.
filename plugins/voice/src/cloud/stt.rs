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
//! это на порядок меньше трафика: час записи в PCM 16 кГц — 115 МБ.
//!
//! # Таймкоды посегментные, пословных НЕТ
//!
//! Параметр пословных меток **принимается без ошибки и молча игнорируется** —
//! поля `words` в ответе нет вовсе. Поэтому он не отправляется совсем (на это
//! есть тест): отправлять параметр, который не работает, — значит однажды
//! поверить, что он работает.
//!
//! Следствие: [`WhisperSegment::tokens`] у облачных сегментов ПУСТ, и ветка
//! «уверенность ниже порога ⇒ режем сегмент по границе слова» в облачном режиме
//! обязана деградировать до пометки, а не выдавать границы, которых никто не
//! измерял. Разрезать нечем — слов с метками нет.
//!
//! # Одно упавшее окно не теряет всю расшифровку
//!
//! Каждое окно самостоятельно. Отказ одного попадает в
//! [`CloudSttOutcome::failed`] и в журнал, а остальные окна доезжают: правило
//! «`failed` только когда нет транскрипта» сильнее полноты.
//!
//! # ОКНА РЕЖУТСЯ ПО ТИШИНЕ, А НЕ ПО АРИФМЕТИКЕ
//!
//! [`plan_windows`] режет запись ровно по 90 с — и реплика, попавшая на
//! границу, страдает. Измерено живьём: на границе 180 с одной встречи ПРОПАЛО
//! 7 секунд речи, на той же границе другой — ЗАДВОИЛСЯ фрагмент «базы данных».
//! Арифметика при этом верна: окна стыкуются встык. Ломается именно реплика на
//! стыке — модель ДОГОВАРИВАЕТ оборванную фразу по контексту, а следующее окно
//! расшифровывает её настоящий хвост.
//!
//! Лечение — [`plan_windows_at_silence`]: интервалы речи, если они известны,
//! двигают границу окна ВНИЗ, к паузе перед пределом. Вниз, а не в обе стороны:
//! потолок [`super::MAX_STT_WINDOW_SECS`] измерен обрывом TCP и превышению не
//! подлежит. Интервалов нет — остаётся арифметика: придумывать паузы вместо
//! того, чтобы их знать, было бы хуже, чем резать честно и грубо.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{status_error, CloudClient, CloudError, MAX_STT_WINDOW_SECS};
use crate::proc::{self, ProcError};
use crate::whisper::WhisperSegment;

/// Подкаталог, куда кладутся временные mp3 окон.
pub const WINDOW_DIR: &str = "cloud";

/// Битрейт окна. Речь 16 кГц моно: 32 кбит/с — тот же выбор, что у архивного
/// AAC, и на распознавании он не сказывается.
pub const WINDOW_BITRATE: &str = "32k";

/// Поле вокруг интервала речи, мс.
///
/// Живёт здесь, а не только в пер-трековом коде плагина встреч, потому что
/// нужен обоим: [`plan_windows_at_silence`] отступает на него ПЕРЕД началом
/// следующей реплики. Первый слог тише остального слова и в интервал VAD часто
/// не попадает — рез по самому началу отрезал бы его.
pub const CUT_PAD_MS: i64 = 300;

/// Одно окно расшифровки на общей оси записи.
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

/// Окна файла и места, где кусок речи пришлось разрезать.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CutWindows {
    /// Окна В КООРДИНАТАХ ФАЙЛА. Идут встык и покрывают его весь.
    pub windows: Vec<Window>,
    /// Точки файла, где ОДИН кусок речи разрезан пополам.
    ///
    /// Пусто в подавляющем большинстве записей: разрез случается только у
    /// куска, который сам длиннее окна.
    pub forced_cuts: Vec<i64>,
}

/// Режет ЗАПИСЬ на окна, двигая границы в тишину. **Чистая функция.**
///
/// Граница переносится ВНИЗ, к ближайшей паузе перед пределом окна: потолок
/// [`MAX_STT_WINDOW_SECS`] измерен, и превышать его нельзя ни на миллисекунду.
/// Точка реза берётся с полем [`CUT_PAD_MS`] ПЕРЕД началом следующей реплики.
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
                    let c = (merged[n].0 - CUT_PAD_MS).max(floor);
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

/// argv ffmpeg: вырезать окно и сжать в mp3.
///
/// `-ss` и `-t` стоят ДО `-i`: на WAV это точная и мгновенная перемотка по
/// смещению в файле, а после `-i` ffmpeg декодировал бы всё от начала записи —
/// на последнем окне часовой записи это час работы вместо секунды.
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
/// Пословных меток здесь нет и быть не может — см. шапку модуля.
///
/// `response_format` выводится ИЗ МОДЕЛИ ([`super::stt_response_format`]):
/// модели без таймкодов не игнорируют `verbose_json`, а отвергают запрос
/// целиком, поэтому спросить его «на всякий случай» нельзя.
#[must_use]
pub fn build_form_fields(model: &str, language: &str) -> Vec<(String, String)> {
    let mut fields = vec![
        ("model".to_string(), model.to_string()),
        (
            "response_format".to_string(),
            super::stt_response_format(model).to_string(),
        ),
    ];
    if !language.trim().is_empty() {
        fields.push(("language".to_string(), language.trim().to_string()));
    }
    fields
}

/// Сдвигает сегменты на общую ось записи.
///
/// **Отдельная функция, а не пересчёт координат.** Тот пересчёт — МНОЖИТЕЛЬ
/// (коррекция дрейфа); сдвиг окна — СЛОЖЕНИЕ на [`WhisperSegment`], до всякой
/// атрибуции. Подмена одного другим растянула бы запись вместо переноса.
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
/// [`crate::whisper::WhisperToken`] невозможно.
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
    /// Пусто — режем арифметически. Пустым это поле бывает законно: интервалов
    /// речи может не быть вовсе, а придумывать паузы вместо того, чтобы их
    /// знать, — худший из исходов.
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

    /// Весь текст расшифровки одной строкой, в порядке времени.
    ///
    /// Голосовому сообщению нужен именно он: показывать человеку таймкоды
    /// пятисекундной реплики незачем, а сегменты уже отсортированы.
    #[must_use]
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Текст предупреждения для журнала. `None` — предупреждать не о чем.
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
/// контракт, что у локального whisper, поэтому кадр прогресса на фронте не
/// меняется.
///
/// Ошибкой считается только случай, когда не доехало НИ ОДНО окно: одно
/// упавшее окно — это дыра в расшифровке, а не потерянный результат.
pub async fn transcribe_file(
    client: &CloudClient,
    wav: &Path,
    total_ms: i64,
    work_dir: &Path,
    opts: &SttOptions,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<CloudSttOutcome, ProcError> {
    // Границы окон уезжают в тишину, если интервалы речи известны: реплика,
    // разрезанная посередине, задваивается достройкой модели.
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

/// Гоняет ГОТОВЫЙ список окон одного файла. Общее тело всех облачных путей.
///
/// Сегменты возвращаются на оси ТОГО ФАЙЛА, который прислали: миксу это сразу
/// общая ось, вырезанному речевому файлу — его собственная, и её потом
/// пересчитывает вызывающий.
///
/// `Err` здесь — только отмена и несоздавшийся каталог. Отказ окна не ошибка:
/// он ложится в `failed`, а `Option<ProcError>` несёт ПЕРВЫЙ отказ, чтобы
/// вызывающий мог назвать причину, если не доехало вообще ничего.
pub async fn run_windows(
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
        // Временный файл не переживает окно: час записи это 40 окон, и
        // оставленные mp3 лежали бы в каталоге навсегда.
        let _ = std::fs::remove_file(&mp3);

        match step {
            Ok((mut segs, lang)) => {
                if outcome.language.is_none() {
                    outcome.language = lang;
                }
                // Вырожденный ответ без таймкодов растягивается на окно: иначе
                // весь его текст лёг бы в нулевую точку записи.
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
                    target: "forge_voice",
                    window = w.index,
                    start_ms = w.start_ms,
                    code = err.code,
                    "forge-voice: окно облачной расшифровки не доехало: {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tile_the_recording_without_gaps_or_overlap() {
        let ws = plan_windows(3_600_000, 90_000);
        assert_eq!(ws.len(), 40);
        for pair in ws.windows(2) {
            assert_eq!(pair[0].end_ms(), pair[1].start_ms, "{pair:?}");
        }
        assert_eq!(ws.last().unwrap().end_ms(), 3_600_000, "хвост потерян");
        assert!(plan_windows(0, 90_000).is_empty());
    }

    /// **Окно НЕ МОЖЕТ стать длиннее измеренного предела.**
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
        assert_eq!(plan_windows(3_000, 0).len(), 3);
    }

    /// **ГРАНИЦА ОКНА УЕЗЖАЕТ В ТИШИНУ.**
    #[test]
    fn a_window_moves_its_edge_into_the_silence() {
        let speech = [
            (5_000i64, 60_000i64),
            (70_000, 120_000),
            (130_000, 169_000),
            (169_000, 183_200),
            (190_000, 240_000),
        ];
        let cw = plan_windows_at_silence(300_000, 90_000, &speech);
        assert!(cw.forced_cuts.is_empty(), "резать речь было не нужно: {cw:?}");
        for w in &cw.windows {
            for (s, e) in speech {
                assert!(
                    w.end_ms() <= s || w.end_ms() >= e,
                    "граница {} разрезала реплику {s}-{e}",
                    w.end_ms()
                );
            }
        }
        assert_eq!(cw.windows.last().unwrap().end_ms(), 300_000);
        assert!(cw.windows.iter().all(|w| w.len_ms <= 90_000), "{cw:?}");
        // Речи нет вовсе — арифметика, как и была.
        assert_eq!(
            plan_windows_at_silence(100_000, 90_000, &[]).windows,
            plan_windows(100_000, 90_000)
        );
        // Речь без единой паузы длиннее окна: режем и НАЗЫВАЕМ место.
        let cw = plan_windows_at_silence(200_000, 90_000, &[(0, 200_000)]);
        assert_eq!(cw.forced_cuts, vec![90_000, 180_000]);
    }

    /// **НА ОБЛАКО УХОДИТ MP3, А НЕ WAV**, и перемотка идёт ДО `-i`.
    #[test]
    fn the_window_is_encoded_to_mp3_with_a_fast_seek() {
        let w = Window { index: 7, start_ms: 630_000, len_ms: 90_000 };
        let out = PathBuf::from("/m/work/cloud/window-0007.mp3");
        let args = build_window_args(Path::new("/m/work/mix.wav"), w, &out);
        let joined = args.join(" ");
        assert!(joined.contains("-c:a libmp3lame"), "{joined}");
        assert!(!joined.contains("pcm_s16le"), "сырой PCM роняет их шлюз: {joined}");
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < i, "{joined}");
        assert_eq!(w.file_name(), "window-0007.mp3");
    }

    /// **ЗАПРОС НЕ ПРОСИТ ПОСЛОВНЫХ МЕТОК.**
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
                // Формат просится ПО МОДЕЛИ: у whisper-1 таймкоды есть и живут
                // только в `verbose_json`, а у дешёвой turbo его нет вовсе —
                // шлюз отвечает на него BAD_REQUEST, и расшифровка теряется
                // целиком (живая проба записана в `super::stt_response_format`).
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
                    "tokens": [50364, 1770, 13],
                    "temperature": 0.0, "avg_logprob": -0.31
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
        assert_eq!(segs[0].end_ms, 3_160);
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
    }

    /// Сдвиг окна — СЛОЖЕНИЕ, и он не имеет права быть множителем.
    #[test]
    fn shifting_moves_segments_onto_the_shared_axis() {
        let (mut segs, _) = parse_verbose_json(&body(&[(0.0, 3.0, "а"), (10.0, 12.5, "б")])).unwrap();
        shift_segments(&mut segs, 90_000);
        assert_eq!(segs[0].start_ms, 90_000);
        assert_eq!(
            segs[1].end_ms - segs[1].start_ms,
            2_500,
            "сдвиг растянул сегмент: это множитель, а не сложение"
        );
    }

    /// Голосовому сообщению нужен ТЕКСТ, а не таймкоды.
    #[test]
    fn the_outcome_joins_its_segments_into_one_text() {
        let (segments, _) = parse_verbose_json(&body(&[(0.0, 1.0, " Привет."), (1.0, 2.0, " Как дела?")])).unwrap();
        let out = CloudSttOutcome {
            windows: 1,
            segments,
            ..CloudSttOutcome::default()
        };
        assert_eq!(out.text(), "Привет. Как дела?");
        assert!(out.complete());
        assert_eq!(CloudSttOutcome::default().text(), "");
    }
}
