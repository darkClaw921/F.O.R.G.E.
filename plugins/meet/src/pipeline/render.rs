//! Рендер результата: `transcript.json` и `transcript.md`.
//!
//! Модуль НЕ импортирует `axum` и `http`. Рендер — чистая функция от
//! результата [`super::speakers::attribute_segments`], поэтому и проверяется
//! golden-тестами: сравнивать целые файлы дешевле и честнее, чем перечислять
//! ожидания по одному.
//!
//! # Два формата, потому что два читателя
//!
//! | файл | кто читает | что важно |
//! |---|---|---|
//! | `transcript.json` | фронт (поиск, подсветка, переименование спикеров) | исходная нарезка на сегменты, потокенные метки, `confidence` |
//! | `transcript.md` | человек и промпт саммари | абзацы по спикерам с временными метками |
//!
//! Абзацы в `.md` — это [`super::speakers::merge_into_paragraphs`] поверх тех
//! же сегментов. Сливать их прямо в `attribute_segments` нельзя: гранулярность
//! нужна `.json`, и вернуть её после слияния было бы уже неоткуда.
//!
//! # ВРЕМЕНИ ГЕНЕРАЦИИ В ФАЙЛАХ НЕТ
//!
//! Ни `generated_at`, ни версии devforge. Требование «прогон дважды даёт
//! побитово идентичные файлы» — не эстетика: повтор этапа
//! (`retry?from=transcribing`) обязан быть отличим от «ничего не изменилось»,
//! а с меткой времени каждый прогон выглядел бы новым результатом. Дата
//! встречи в шапке есть — но это дата ВСТРЕЧИ, свойство данных, а не прогона.
//!
//! # Оба файла пишутся АТОМАРНО
//!
//! Через `forge_atomic`: недописанного `transcript.md` на диске не бывает.
//! Файл читают и фронт, и Ф6 (промпт саммари), и оба обязаны видеть либо
//! старую версию целиком, либо новую целиком.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::speakers::AttributedSegment;

/// Версия формата `transcript.json`.
pub const TRANSCRIPT_VERSION: u32 = 1;

/// Пометка перекрытия в `.md`.
///
/// Словами, а не значком: файл уезжает в промпт саммари, и «⚠» модель
/// прочитает как угодно, а «говорили одновременно» — однозначно.
pub const OVERLAP_MARK: &str = "(говорили одновременно)";

/// Движок расшифровки у записей, сделанных до появления поля `engine`.
///
/// Локальный путь был единственным на момент их создания, поэтому старое
/// значение восстанавливается однозначно, а не угадывается.
fn default_engine() -> String {
    ENGINE_LOCAL.to_string()
}

/// Значение [`TranscriptMeta::engine`] для локального whisper.cpp.
pub const ENGINE_LOCAL: &str = "whisper.cpp";

/// Значение [`TranscriptMeta::engine`] для облачной расшифровки.
pub const ENGINE_CLOUD: &str = "polza.ai";

/// Шапка расшифровки — всё, что не зависит от сегментов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptMeta {
    pub meeting_id: String,
    pub platform: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Начало встречи в unix-миллисекундах. Свойство ДАННЫХ, а не прогона.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_unix_ms: Option<i64>,
    pub duration_ms: i64,
    /// Чем расшифровано: `whisper.cpp` у локального пути, `polza.ai` у
    /// облачного.
    ///
    /// Поле существует потому, что `model` само по себе не отвечает на вопрос
    /// «кто это сделал»: и у облака, и у локального движка модель — просто
    /// строка. Пока движок не записывался, шапка расшифровки утверждала
    /// «whisper.cpp» ВСЕГДА, включая облачные прогоны, а `model` брался из
    /// `whisper_model` даже тогда, когда работало облако. То есть артефакт
    /// врал о собственном происхождении, и по нему нельзя было отличить один
    /// путь расшифровки от другого — ровно то, что требуется проверять.
    #[serde(default = "default_engine")]
    pub engine: String,
    pub model: String,
    pub language: String,
    /// Применённый множитель коррекции дрейфа. `None` — коррекции не было.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_scale: Option<f64>,
}

/// Слово в машинном формате. Имена коротки намеренно: час встречи — это
/// десятки тысяч слов, и `start_ms`/`end_ms` на каждом стоили бы сотни
/// килобайт на ровном месте.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordDto {
    pub t: String,
    pub s: i64,
    pub e: i64,
}

/// Сегмент в машинном формате.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentDto {
    pub idx: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub confidence: f64,
    pub overlapped: bool,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<WordDto>,
}

/// Тело `transcript.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptDto {
    pub version: u32,
    #[serde(flatten)]
    pub meta: TranscriptMeta,
    /// Имена спикеров в порядке первого появления.
    pub speakers: Vec<String>,
    pub segments: Vec<SegmentDto>,
}

/// Имена спикеров в порядке ПЕРВОГО ПОЯВЛЕНИЯ.
///
/// Порядок появления, а не алфавит: в шапке `.md` он читается как состав
/// встречи, а алфавитный список ничего не говорит о том, кто её вёл.
pub fn speaker_order(segments: &[AttributedSegment]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in segments {
        if let Some(name) = &s.speaker {
            if !out.iter().any(|n| n == name) {
                out.push(name.clone());
            }
        }
    }
    out
}

/// Собирает машинное представление.
pub fn build_dto(meta: &TranscriptMeta, segments: &[AttributedSegment]) -> TranscriptDto {
    TranscriptDto {
        version: TRANSCRIPT_VERSION,
        meta: meta.clone(),
        speakers: speaker_order(segments),
        segments: segments
            .iter()
            .enumerate()
            .map(|(idx, s)| SegmentDto {
                idx,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                speaker: s.speaker.clone(),
                // Три знака: больше — шум, меньше — не отличить 0,59 от 0,61,
                // а именно там стоит порог разрезания.
                confidence: (s.confidence * 1000.0).round() / 1000.0,
                overlapped: s.overlapped,
                text: s.text.clone(),
                words: s
                    .words
                    .iter()
                    .map(|w| WordDto {
                        t: w.text.clone(),
                        s: w.start_ms,
                        e: w.end_ms,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Рендерит `transcript.json`.
pub fn render_json(meta: &TranscriptMeta, segments: &[AttributedSegment]) -> String {
    let dto = build_dto(meta, segments);
    // `to_string_pretty`, а не компактный: файл читают глазами при разборе
    // жалоб на атрибуцию, и `git diff` по нему тоже должен быть осмысленным.
    serde_json::to_string_pretty(&dto).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// `[чч:мм:сс]` от начала встречи.
pub fn timestamp(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    format!("{:02}:{:02}:{:02}", total / 3600, total % 3600 / 60, total % 60)
}

/// Длительность словами для шапки.
pub fn human_duration(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h} ч {m} мин {s} с")
    } else if m > 0 {
        format!("{m} мин {s} с")
    } else {
        format!("{s} с")
    }
}

/// Имя спикера для показа. `None` превращается в честное «неизвестно», а не
/// в пустое место: пустая строка в `.md` выглядела бы как ошибка рендера.
fn display_speaker(name: Option<&String>) -> &str {
    name.map(String::as_str).unwrap_or("Неизвестный участник")
}

/// Рендерит `transcript.md` — абзацами по спикерам.
///
/// На вход подаются УЖЕ склеенные абзацы
/// ([`super::speakers::merge_into_paragraphs`]): рендер ничего не решает про
/// группировку, он только печатает.
pub fn render_markdown(meta: &TranscriptMeta, paragraphs: &[AttributedSegment]) -> String {
    let mut out = String::new();
    out.push_str("# Расшифровка встречи\n\n");

    if let Some(title) = &meta.title {
        out.push_str(&format!("- Тема: {title}\n"));
    }
    out.push_str(&format!("- Платформа: {}\n", meta.platform));
    out.push_str(&format!("- Ссылка: {}\n", meta.url));
    out.push_str(&format!(
        "- Длительность: {}\n",
        human_duration(meta.duration_ms)
    ));
    out.push_str(&format!(
        "- Расшифровка: {}, модель {}, язык {}\n",
        meta.engine, meta.model, meta.language
    ));

    let speakers = speaker_order(paragraphs);
    if speakers.is_empty() {
        out.push_str("- Участники: не определены\n");
    } else {
        out.push_str(&format!("- Участники: {}\n", speakers.join(", ")));
    }
    if let Some(k) = meta.drift_scale {
        out.push_str(&format!(
            "- Таймлайн скорректирован по фактическому аудио (коэффициент {k:.6})\n"
        ));
    }
    out.push('\n');

    if paragraphs.is_empty() {
        out.push_str("_Речь не распознана._\n");
        return out;
    }

    for p in paragraphs {
        let mark = if p.overlapped {
            format!(" {OVERLAP_MARK}")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "**[{}] {}**{}\n\n",
            timestamp(p.start_ms),
            display_speaker(p.speaker.as_ref()),
            mark
        ));
        out.push_str(p.text.trim());
        out.push_str("\n\n");
    }
    out
}

/// Читает `transcript.json` обратно. `None` — файла нет или он не разбирается.
///
/// Существует ради ПЕРЕИМЕНОВАНИЯ СПИКЕРОВ (Ф7): пересобирать расшифровку из
/// базы нельзя — в базе нет потокенных меток (`words`), и первый же ручной
/// правкой имени файл потерял бы их навсегда. Файл — полное представление,
/// база — проекция для ленты.
pub fn read_transcript(meeting_dir: &Path) -> Option<TranscriptDto> {
    let raw = std::fs::read_to_string(meeting_dir.join(super::TRANSCRIPT_JSON)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Обратное преобразование машинного формата в то, из чего он собран.
///
/// Пара к [`build_dto`]; на их взаимную обратимость есть тест. Нужна там же,
/// где [`read_transcript`]: чтобы переписать имена и отрендерить оба файла
/// заново, ничего не потеряв.
pub fn from_dto(dto: &TranscriptDto) -> Vec<AttributedSegment> {
    dto.segments
        .iter()
        .map(|s| AttributedSegment {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            text: s.text.clone(),
            speaker: s.speaker.clone(),
            confidence: s.confidence,
            overlapped: s.overlapped,
            words: s
                .words
                .iter()
                .map(|w| super::speakers::Word {
                    text: w.t.clone(),
                    start_ms: w.s,
                    end_ms: w.e,
                })
                .collect(),
        })
        .collect()
}

/// Пишет оба файла АТОМАРНО и отдаёт их пути.
pub fn write_transcript(
    meeting_dir: &Path,
    meta: &TranscriptMeta,
    segments: &[AttributedSegment],
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let json_path = meeting_dir.join(super::TRANSCRIPT_JSON);
    let md_path = meeting_dir.join(super::TRANSCRIPT_MD);

    let json = render_json(meta, segments);
    let paragraphs = super::speakers::merge_into_paragraphs(segments);
    let md = render_markdown(meta, &paragraphs);

    forge_atomic::write_bytes_atomic(&json_path, json.as_bytes())?;
    forge_atomic::write_bytes_atomic(&md_path, md.as_bytes())?;
    Ok((json_path, md_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::speakers::{merge_into_paragraphs, Word};

    fn meta() -> TranscriptMeta {
        TranscriptMeta {
            meeting_id: "m1".into(),
            platform: "telemost".into(),
            url: "https://telemost.yandex.ru/j/1".into(),
            title: Some("Планёрка".into()),
            started_at_unix_ms: Some(1_700_000_000_000),
            duration_ms: 3_723_000,
            engine: "whisper.cpp".into(),
            model: "large-v3-turbo-q5_0".into(),
            language: "ru".into(),
            drift_scale: None,
        }
    }

    fn seg(
        start: i64,
        end: i64,
        speaker: Option<&str>,
        text: &str,
        overlapped: bool,
    ) -> AttributedSegment {
        AttributedSegment {
            start_ms: start,
            end_ms: end,
            text: text.into(),
            speaker: speaker.map(str::to_string),
            confidence: if speaker.is_some() { 1.0 } else { 0.0 },
            overlapped,
            words: vec![Word {
                text: text.split_whitespace().next().unwrap_or("").to_string(),
                start_ms: start,
                end_ms: end,
            }],
        }
    }

    fn sample() -> Vec<AttributedSegment> {
        vec![
            seg(1_000, 4_000, Some("Аня"), "Коллеги, начинаем.", false),
            seg(4_200, 6_000, Some("Аня"), "Первый вопрос — релиз.", false),
            seg(6_500, 9_000, Some("Боря"), "Готов к пятнице.", false),
            seg(9_100, 10_000, Some("Аня"), "Точно?", true),
            seg(10_500, 11_000, None, "…", false),
        ]
    }

    /// GOLDEN: машинный формат целиком.
    #[test]
    fn the_json_format_is_exactly_this() {
        let json = render_json(&meta(), &sample());
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["version"], 1);
        assert_eq!(v["meeting_id"], "m1");
        assert_eq!(v["platform"], "telemost");
        assert_eq!(v["title"], "Планёрка");
        assert_eq!(v["duration_ms"], 3_723_000);
        assert_eq!(v["model"], "large-v3-turbo-q5_0");
        assert_eq!(v["language"], "ru");
        assert_eq!(
            v["speakers"],
            serde_json::json!(["Аня", "Боря"]),
            "порядок первого появления"
        );

        let segs = v["segments"].as_array().unwrap();
        assert_eq!(segs.len(), 5);
        assert_eq!(segs[0]["idx"], 0);
        assert_eq!(segs[0]["start_ms"], 1_000);
        assert_eq!(segs[0]["speaker"], "Аня");
        assert_eq!(segs[0]["confidence"], 1.0);
        assert_eq!(segs[0]["overlapped"], false);
        assert_eq!(segs[0]["text"], "Коллеги, начинаем.");
        assert_eq!(segs[0]["words"][0]["t"], "Коллеги,");
        assert_eq!(segs[0]["words"][0]["s"], 1_000);

        // Сегмент без спикера не выдумывает имя.
        assert!(segs[4].get("speaker").is_none(), "{}", segs[4]);
        // И коррекции дрейфа не было — поля нет вовсе.
        assert!(v.get("drift_scale").is_none());

        // Никакого времени генерации: файл обязан зависеть только от данных.
        assert!(!json.contains("generated_at"), "{json}");
    }

    /// ПЕРЕКРЫТИЕ видно в ОБОИХ форматах.
    #[test]
    fn overlapped_segments_are_visible_in_both_formats() {
        let json: serde_json::Value = serde_json::from_str(&render_json(&meta(), &sample())).unwrap();
        let overlapped: Vec<&serde_json::Value> = json["segments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["overlapped"] == true)
            .collect();
        assert_eq!(overlapped.len(), 1);
        assert_eq!(overlapped[0]["text"], "Точно?");

        let md = render_markdown(&meta(), &merge_into_paragraphs(&sample()));
        assert!(md.contains(OVERLAP_MARK), "{md}");
        // Пометка стоит именно у своей реплики.
        let line = md
            .lines()
            .find(|l| l.contains("Точно") || l.contains(OVERLAP_MARK))
            .unwrap_or_default();
        assert!(line.contains(OVERLAP_MARK) || md.contains(&format!("{OVERLAP_MARK}\n")));
    }

    /// GOLDEN: человекочитаемый формат целиком.
    #[test]
    fn the_markdown_format_is_exactly_this() {
        let md = render_markdown(&meta(), &merge_into_paragraphs(&sample()));
        let expected = "\
# Расшифровка встречи

- Тема: Планёрка
- Платформа: telemost
- Ссылка: https://telemost.yandex.ru/j/1
- Длительность: 1 ч 2 мин 3 с
- Расшифровка: whisper.cpp, модель large-v3-turbo-q5_0, язык ru
- Участники: Аня, Боря

**[00:00:01] Аня**

Коллеги, начинаем. Первый вопрос — релиз.

**[00:00:06] Боря**

Готов к пятнице.

**[00:00:09] Аня** (говорили одновременно)

Точно?

**[00:00:10] Неизвестный участник**

…

";
        assert_eq!(md, expected, "\n--- получено ---\n{md}");
    }

    /// Пустая расшифровка — не ошибка и не пустой файл.
    #[test]
    fn an_empty_transcript_renders_honestly() {
        let md = render_markdown(&meta(), &[]);
        assert!(md.contains("Речь не распознана"));
        assert!(md.contains("Участники: не определены"));
        let json: serde_json::Value = serde_json::from_str(&render_json(&meta(), &[])).unwrap();
        assert_eq!(json["segments"].as_array().unwrap().len(), 0);
        assert_eq!(json["speakers"].as_array().unwrap().len(), 0);
    }

    /// Применённая коррекция дрейфа видна в обоих файлах: иначе через месяц
    /// невозможно понять, почему таймкоды не совпадают с журналом событий.
    #[test]
    fn an_applied_drift_correction_is_recorded_in_both_files() {
        let m = TranscriptMeta {
            drift_scale: Some(1.000694),
            ..meta()
        };
        let json: serde_json::Value = serde_json::from_str(&render_json(&m, &sample())).unwrap();
        assert!((json["drift_scale"].as_f64().unwrap() - 1.000694).abs() < 1e-9);
        let md = render_markdown(&m, &merge_into_paragraphs(&sample()));
        assert!(md.contains("Таймлайн скорректирован"), "{md}");
    }

    #[test]
    fn timestamps_and_durations_are_formatted_for_humans() {
        assert_eq!(timestamp(0), "00:00:00");
        assert_eq!(timestamp(3_723_000), "01:02:03");
        assert_eq!(timestamp(-5), "00:00:00");
        assert_eq!(human_duration(3_723_000), "1 ч 2 мин 3 с");
        assert_eq!(human_duration(63_000), "1 мин 3 с");
        assert_eq!(human_duration(5_000), "5 с");
    }

    /// **ПРОГОН ДВАЖДЫ ДАЁТ ПОБИТОВО ИДЕНТИЧНЫЕ ФАЙЛЫ.**
    #[test]
    fn writing_twice_produces_byte_identical_files() {
        let dir = tempfile::tempdir().unwrap();
        let (j1, m1) = write_transcript(dir.path(), &meta(), &sample()).unwrap();
        let json_a = std::fs::read(&j1).unwrap();
        let md_a = std::fs::read(&m1).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        let (j2, m2) = write_transcript(dir.path(), &meta(), &sample()).unwrap();
        assert_eq!(j1, j2);
        assert_eq!(m1, m2);
        assert_eq!(std::fs::read(&j2).unwrap(), json_a, "JSON зависит не только от данных");
        assert_eq!(std::fs::read(&m2).unwrap(), md_a, "MD зависит не только от данных");
    }

    /// Машинный формат ОБРАТИМ: `build_dto` → `from_dto` возвращает то же
    /// самое, включая потокенные метки.
    ///
    /// На этом стоит переименование спикеров: оно перечитывает файл, правит
    /// имена и рендерит оба формата заново. Потеряй `from_dto` слова — первая
    /// же ручная правка имени стёрла бы их из `transcript.json` навсегда, и
    /// заметили бы это только при следующей попытке резать сегмент по границе
    /// смены спикера.
    #[test]
    fn the_machine_format_round_trips_through_from_dto() {
        let dir = tempfile::tempdir().unwrap();
        write_transcript(dir.path(), &meta(), &sample()).unwrap();

        let back = read_transcript(dir.path()).expect("файл обязан читаться обратно");
        assert_eq!(back.meta, meta());
        let segments = from_dto(&back);
        assert_eq!(segments, sample(), "обратное преобразование потеряло данные");
        assert!(
            segments.iter().all(|s| !s.words.is_empty()),
            "потокенные метки обязаны пережить круг"
        );

        // И повторная запись из восстановленного даёт ПОБИТОВО тот же файл.
        let again = tempfile::tempdir().unwrap();
        write_transcript(again.path(), &back.meta, &segments).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(super::super::TRANSCRIPT_JSON)).unwrap(),
            std::fs::read(again.path().join(super::super::TRANSCRIPT_JSON)).unwrap()
        );
        assert_eq!(
            std::fs::read(dir.path().join(super::super::TRANSCRIPT_MD)).unwrap(),
            std::fs::read(again.path().join(super::super::TRANSCRIPT_MD)).unwrap()
        );
    }

    #[test]
    fn a_missing_or_broken_transcript_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_transcript(dir.path()).is_none());
        std::fs::write(dir.path().join(super::super::TRANSCRIPT_JSON), "{ не json").unwrap();
        assert!(read_transcript(dir.path()).is_none());
    }

    /// Файлы пишутся атомарно: временных остатков в каталоге не бывает.
    #[test]
    fn both_files_are_written_atomically_and_leave_no_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        write_transcript(dir.path(), &meta(), &sample()).unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["transcript.json", "transcript.md"], "{names:?}");

        // И обратный разбор JSON работает: его читает фронт.
        let raw = std::fs::read_to_string(dir.path().join("transcript.json")).unwrap();
        let back: TranscriptDto = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.version, TRANSCRIPT_VERSION);
        assert_eq!(back.meta.meeting_id, "m1");
        assert_eq!(back.segments.len(), 5);
    }
}
