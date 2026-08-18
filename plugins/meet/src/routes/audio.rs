//! `GET /api/meet/meetings/:id/audio` — архивный микс либо ОДНА дорожка.
//!
//! # Почему Range здесь есть, хотя нашему плееру он не нужен
//!
//! Фронт грузит запись через `fetch` + `blob:` (у элемента `<audio>` нет
//! Bearer-токена, и в remote-mode прямой `src` ушёл бы без авторизации), то
//! есть перемотку обеспечивает blob, а не сервер. Тем не менее Range
//! реализован: правильное поведение эндпоинта, отдающего медиафайл, не зависит
//! от того, кто его дёргает. `curl -r`, `ffprobe` по HTTP и любой внешний
//! плеер — законные клиенты, и «Accept-Ranges нет, качай целиком» им означает
//! 14 МБ ради двух секунд звука.
//!
//! # Дорожка — ПАРАМЕТР ЗАПРОСА, а не отдельный роут
//!
//! `?track=N` вместо `/audio/track/N`. Решение принято фронтом (Ф4) и здесь
//! исполняется дословно: таблица `MEET_ROUTES` в `static/js/meet/api.js` и
//! таблица в шапке [`crate::routes`] сверяются контрактным тестом ДОСЛОВНО, и
//! второй путь означал бы девятнадцатую строку в одной таблице и двадцатую в
//! другой — то есть красный тест ради адреса, который отличается от первого
//! выборкой файла и ничем больше.
//!
//! Обход каталога через этот параметр невозможен структурно: `N` разбирается в
//! `u32`, а имя файла собирается `format!("track-{n}.wav")`. Строка из URL в
//! путь не конкатенируется вообще — тот же приём, что в
//! `plugins/bitrix/src/media_cache.rs`.
//!
//! # Что именно отдаётся
//!
//! | запрос | файл | тип |
//! |---|---|---|
//! | без параметров | `audio_path` из базы, иначе `<dir>/audio.m4a` | `audio/mp4` |
//! | `?track=N` | `<dir>/raw/track-N.wav` | `audio/wav` |
//!
//! Архив — AAC 32 кбит/с (14 МБ на час), пер-трековый WAV — 115 МБ на час.
//! Второй существует ради прослушивания коротких кусков при переименовании
//! спикеров, и грузить его целиком в плеер незачем: ровно поэтому Range у него
//! важнее, чем у архива.
//!
//! # Несколько диапазонов в одном запросе Range ИГНОРИРУЮТСЯ
//!
//! `bytes=0-10,20-30` требует ответа `multipart/byteranges`. Отдать вместо
//! него первый диапазон со статусом 206 значило бы соврать: клиент получил бы
//! НЕ ТО, что просил, и молча склеил бы битый файл. RFC 9110 разрешает сервер,
//! который не умеет multipart, — он обязан проигнорировать заголовок и отдать
//! представление целиком (200). Так и сделано, и на это есть тест.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use tokio::io::AsyncReadExt;

use crate::db::repo::{meetings, MeetingRow};
use crate::error::{ErrorBody, MeetError};
use crate::state::MeetState;

use super::{check_ladder, Preconditions};

/// Размер куска, которым тело уходит в сокет.
///
/// Файл НЕ читается в память целиком: четырёхчасовая встреча это 56 МБ архива
/// и 1,6 ГБ пер-трековых WAV, и `Vec<u8>` на каждый запрос превратил бы
/// прослушивание дорожки в отказ в обслуживании.
const CHUNK: u64 = 64 * 1024;

/// Максимальный номер дорожки, который вообще разбирается.
///
/// Больше тысячи участников в одной встрече не бывает; всё, что выше, —
/// либо ошибка клиента, либо перебор, и ответ на него 400, а не поход в ФС.
pub const MAX_TRACK_INDEX: u32 = 1023;

// ---------------------------------------------------------------------------
// Разбор Range — чистая функция.
// ---------------------------------------------------------------------------

/// Полуинтервал байт, ВКЛЮЧАЯ оба конца (так же, как в `Content-Range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// Последний байт, включительно.
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(self) -> bool {
        false // построение гарантирует end >= start
    }
}

/// Что делать с заголовком `Range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// Заголовка нет, он не про байты или синтаксически негоден — отдаём файл
    /// целиком со статусом 200.
    Whole,
    /// Один диапазон — 206 плюс `Content-Range`.
    Single(ByteRange),
    /// Синтаксис верен, но диапазон за пределами файла — 416.
    Unsatisfiable,
}

/// Разбирает `Range: bytes=…` относительно файла длиной `len`.
///
/// Правила ровно по RFC 9110 §14.1.1:
///
/// * другая единица (`items=0-5`) — не наша, заголовок игнорируется;
/// * синтаксически негодный спек (`bytes=abc`, `bytes=10-5`) — игнорируется,
///   ответ 200. Это НЕ 400: сервер, отвечающий ошибкой на непонятный ему
///   `Range`, ломает клиентов, которые прекрасно обошлись бы целым файлом;
/// * несколько диапазонов — игнорируются целиком (см. шапку модуля);
/// * `bytes=N-` при `N >= len` и `bytes=-0` — 416.
pub fn parse_range(raw: Option<&str>, len: u64) -> RangeSpec {
    let Some(raw) = raw else {
        return RangeSpec::Whole;
    };
    let raw = raw.trim();
    let Some(spec) = raw.strip_prefix("bytes=") else {
        return RangeSpec::Whole;
    };

    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.len() != 1 {
        // Несколько диапазонов: multipart/byteranges мы не собираем, а первый
        // диапазон под видом всех — это ложь. Отдаём целиком.
        return RangeSpec::Whole;
    }
    let part = parts[0];
    let Some((first, last)) = part.split_once('-') else {
        return RangeSpec::Whole;
    };
    let (first, last) = (first.trim(), last.trim());

    // Суффиксная форма `bytes=-500` — «последние 500 байт».
    if first.is_empty() {
        let Ok(n) = last.parse::<u64>() else {
            return RangeSpec::Whole;
        };
        if n == 0 || len == 0 {
            return RangeSpec::Unsatisfiable;
        }
        let start = len.saturating_sub(n);
        return RangeSpec::Single(ByteRange {
            start,
            end: len - 1,
        });
    }

    let Ok(start) = first.parse::<u64>() else {
        return RangeSpec::Whole;
    };
    if len == 0 || start >= len {
        return RangeSpec::Unsatisfiable;
    }
    if last.is_empty() {
        return RangeSpec::Single(ByteRange {
            start,
            end: len - 1,
        });
    }
    let Ok(end) = last.parse::<u64>() else {
        return RangeSpec::Whole;
    };
    if end < start {
        // Синтаксически негодный спек — игнорируем заголовок, а не отвечаем
        // 416: 416 означал бы «такого куска в файле нет», а здесь клиент
        // попросил бессмыслицу.
        return RangeSpec::Whole;
    }
    RangeSpec::Single(ByteRange {
        start,
        end: end.min(len - 1),
    })
}

// ---------------------------------------------------------------------------
// Разбор query.
// ---------------------------------------------------------------------------

/// Разбирает `?track=N` ВРУЧНУЮ — см. «Порядок проверок» в шапке
/// [`crate::routes`]. Незнакомые параметры игнорируются.
pub fn parse_audio_query(raw: Option<&str>) -> Result<Option<u32>, MeetError> {
    let mut track: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        if k == "track" {
            track = Some(v.into_owned());
        }
    }
    let Some(track) = track.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let idx = track
        .parse::<u32>()
        .map_err(|_| MeetError::BadRequest(format!("track должен быть целым числом, а не {track:?}")))?;
    if idx > MAX_TRACK_INDEX {
        return Err(MeetError::BadRequest(format!(
            "track = {idx} больше максимума {MAX_TRACK_INDEX}"
        )));
    }
    Ok(Some(idx))
}

/// Файл и его MIME по номеру дорожки.
///
/// Путь собирается ИЗ РАЗОБРАННОГО ЧИСЛА, а не из строки запроса: `../../etc`
/// не парсится в `u32` и отбрасывается ещё в [`parse_audio_query`], до единого
/// обращения к файловой системе.
pub fn audio_file(meeting: &MeetingRow, track: Option<u32>) -> (PathBuf, &'static str) {
    let dir = PathBuf::from(&meeting.meeting_dir);
    match track {
        Some(n) => (
            dir.join(crate::ingest::recorder::RAW_DIR)
                .join(format!("track-{n}.wav")),
            "audio/wav",
        ),
        None => (
            meeting
                .audio_path
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.join(crate::pipeline::ARCHIVE_AUDIO)),
            "audio/mp4",
        ),
    }
}

// ---------------------------------------------------------------------------
// Хендлер.
// ---------------------------------------------------------------------------

/// `GET`/`HEAD /api/meet/meetings/:id/audio[?track=N]`.
///
/// `HEAD` обслуживается тем же кодом: `axum::routing::get` матчит и его, а
/// тело отбрасывается явной веткой. Отдельный хендлер разъехался бы с
/// заголовками GET в первой же правке — а весь смысл `HEAD` в том, что его
/// заголовки совпадают с заголовками `GET`.
pub async fn audio(
    State(state): State<Arc<MeetState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, MeetError> {
    // Лестница: резолв встречи (404) РАНЬШЕ разбора параметров (400).
    let meeting = meetings::get(&state, &id).await.ok();
    check_ladder(Preconditions {
        meeting: meeting.as_ref(),
        params: parse_audio_query(raw.as_deref()).err(),
        ..Default::default()
    })?;
    let meeting = meeting.expect("лестница обязана была отвергнуть отсутствие встречи");
    let track = parse_audio_query(raw.as_deref())?;

    let (path, mime) = audio_file(&meeting, track);
    let meta = tokio::fs::metadata(&path).await.map_err(|_| MeetError::NotFound)?;
    if !meta.is_file() {
        return Err(MeetError::NotFound);
    }
    let len = meta.len();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let spec = parse_range(range_header.as_deref(), len);

    // 416 несёт `Content-Range: bytes */LEN` — единственный способ сказать
    // клиенту, какой диапазон вообще существует.
    if spec == RangeSpec::Unsatisfiable {
        let err = MeetError::RangeNotSatisfiable { len };
        return Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_RANGE, format!("bytes */{len}")),
            ],
            Json(ErrorBody::from(&err)),
        )
            .into_response());
    }

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio")
        .to_string();
    let mut resp_headers = HeaderMap::new();
    let mut put = |name: header::HeaderName, value: String| {
        if let Ok(v) = value.parse() {
            resp_headers.insert(name, v);
        }
    };
    put(header::CONTENT_TYPE, mime.to_string());
    put(header::ACCEPT_RANGES, "bytes".to_string());
    // Запись не меняется после `ready`, но и вечной её объявлять нельзя:
    // повтор этапа (`retry?from=transcoding`) пересобирает архив под тем же
    // адресом. Сутки — компромисс, `immutable` был бы враньём.
    put(header::CACHE_CONTROL, "private, max-age=86400".to_string());
    put(
        header::CONTENT_DISPOSITION,
        format!("inline; filename=\"{filename}\""),
    );

    let (status, start, want) = match spec {
        RangeSpec::Single(r) => {
            put(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", r.start, r.end, len),
            );
            (StatusCode::PARTIAL_CONTENT, r.start, r.len())
        }
        _ => (StatusCode::OK, 0, len),
    };
    put(header::CONTENT_LENGTH, want.to_string());

    // HEAD: те же заголовки, пустое тело. Content-Length при этом остаётся —
    // именно за ним HEAD и ходят.
    if method == Method::HEAD {
        return Ok((status, resp_headers, Body::empty()).into_response());
    }

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| MeetError::NotFound)?;
    Ok((status, resp_headers, file_body(file, start, want)).into_response())
}

/// Тело ответа: файл кусками по [`CHUNK`], начиная со смещения `start`.
fn file_body(file: tokio::fs::File, start: u64, want: u64) -> Body {
    let state = (file, start, want, false);
    let stream = futures_util::stream::unfold(state, |(mut file, pos, left, seeked)| async move {
        if left == 0 {
            return None;
        }
        if !seeked && pos > 0 {
            use tokio::io::AsyncSeekExt;
            if let Err(e) = file.seek(std::io::SeekFrom::Start(pos)).await {
                return Some((Err(e), (file, pos, 0, true)));
            }
        }
        let want = left.min(CHUNK) as usize;
        let mut buf = vec![0u8; want];
        match file.read(&mut buf).await {
            // Файл усох между `metadata` и чтением — обрываем поток, а не
            // дописываем нули: усечённый ответ честнее выдуманной тишины.
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((
                    Ok(Bytes::from(buf)),
                    (file, pos + n as u64, left - n as u64, true),
                ))
            }
            Err(e) => Some((Err(e), (file, pos, 0, true))),
        }
    });
    Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- разбор Range -------------------------------------------------------

    #[test]
    fn a_plain_range_is_inclusive_on_both_ends() {
        assert_eq!(
            parse_range(Some("bytes=0-99"), 1000),
            RangeSpec::Single(ByteRange { start: 0, end: 99 })
        );
        assert_eq!(parse_range(Some("bytes=0-99"), 1000).len_of(), 100);
        assert_eq!(
            parse_range(Some("bytes=500-"), 1000),
            RangeSpec::Single(ByteRange { start: 500, end: 999 })
        );
        // Хвост за концом файла подрезается, а не отвергается.
        assert_eq!(
            parse_range(Some("bytes=900-5000"), 1000),
            RangeSpec::Single(ByteRange { start: 900, end: 999 })
        );
    }

    #[test]
    fn the_suffix_form_asks_for_the_tail() {
        assert_eq!(
            parse_range(Some("bytes=-100"), 1000),
            RangeSpec::Single(ByteRange { start: 900, end: 999 })
        );
        // Суффикс длиннее файла — весь файл, но всё ещё 206.
        assert_eq!(
            parse_range(Some("bytes=-5000"), 1000),
            RangeSpec::Single(ByteRange { start: 0, end: 999 })
        );
        // `bytes=-0` неудовлетворим по RFC.
        assert_eq!(parse_range(Some("bytes=-0"), 1000), RangeSpec::Unsatisfiable);
    }

    #[test]
    fn a_range_past_the_end_is_unsatisfiable() {
        assert_eq!(parse_range(Some("bytes=1000-"), 1000), RangeSpec::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=1000-1100"), 1000), RangeSpec::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-10"), 0), RangeSpec::Unsatisfiable);
    }

    /// Непонятный заголовок ИГНОРИРУЕТСЯ, а не превращается в ошибку: сервер,
    /// отвечающий 400 на чужой `Range`, ломает клиента, которого целый файл
    /// устроил бы полностью.
    #[test]
    fn a_broken_or_foreign_range_is_ignored_not_rejected() {
        for raw in [
            "items=0-5",   // другая единица
            "bytes=abc",   // не число
            "bytes=x-y",   // не числа
            "bytes=10-5",  // конец раньше начала
            "bytes=",      // пусто
            "0-10",        // без единицы
        ] {
            assert_eq!(parse_range(Some(raw), 1000), RangeSpec::Whole, "{raw}");
        }
        assert_eq!(parse_range(None, 1000), RangeSpec::Whole);
    }

    /// НЕСКОЛЬКО ДИАПАЗОНОВ: либо multipart, либо честный целый файл. Отдать
    /// первый диапазон со статусом 206 значило бы вернуть НЕ ТО, что просили.
    #[test]
    fn multiple_ranges_are_served_whole_never_partially() {
        assert_eq!(parse_range(Some("bytes=0-10,20-30"), 1000), RangeSpec::Whole);
        assert_eq!(
            parse_range(Some("bytes=0-10, 20-30, 40-50"), 1000),
            RangeSpec::Whole
        );
    }

    // --- query --------------------------------------------------------------

    #[test]
    fn the_track_parameter_is_parsed_by_hand() {
        assert_eq!(parse_audio_query(None).unwrap(), None);
        assert_eq!(parse_audio_query(Some("")).unwrap(), None);
        assert_eq!(parse_audio_query(Some("track=2")).unwrap(), Some(2));
        // Незнакомые параметры игнорируются — новый фронт может дописать
        // версию клиента.
        assert_eq!(parse_audio_query(Some("v=3&track=0&x=1")).unwrap(), Some(0));
    }

    /// ОБХОД КАТАЛОГА невозможен структурно: строка из URL не доходит до пути.
    #[test]
    fn a_path_traversal_attempt_never_reaches_the_filesystem() {
        for raw in [
            "track=../../etc/passwd",
            "track=-1",
            "track=%2e%2e%2f%2e%2e%2fetc",
            "track=1.5",
            "track=99999",
        ] {
            let err = parse_audio_query(Some(raw)).unwrap_err();
            assert!(matches!(err, MeetError::BadRequest(_)), "{raw}: {err}");
        }
    }

    #[test]
    fn the_file_choice_follows_the_track_parameter() {
        let mut row = crate::testing::sample_meeting_row("m1");
        row.meeting_dir = "/tmp/meet/m1".into();
        row.audio_path = None;

        let (path, mime) = audio_file(&row, None);
        assert!(path.ends_with("audio.m4a"), "{path:?}");
        assert_eq!(mime, "audio/mp4");

        let (path, mime) = audio_file(&row, Some(3));
        assert!(path.ends_with("raw/track-3.wav"), "{path:?}");
        assert_eq!(mime, "audio/wav");

        // Явный путь из базы сильнее умолчания: архив мог быть перенесён.
        row.audio_path = Some("/vol/archive/m1.m4a".into());
        assert_eq!(
            audio_file(&row, None).0,
            PathBuf::from("/vol/archive/m1.m4a")
        );
        // …но на выбор ДОРОЖКИ он не влияет.
        assert!(audio_file(&row, Some(0)).0.ends_with("raw/track-0.wav"));
    }

    impl RangeSpec {
        /// Длина куска — только для тестов, чтобы не писать арифметику в
        /// каждом `assert`.
        fn len_of(self) -> u64 {
            match self {
                RangeSpec::Single(r) => r.len(),
                _ => 0,
            }
        }
    }
}
