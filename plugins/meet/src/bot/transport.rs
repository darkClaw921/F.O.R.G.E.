//! Транспорт звука со страницы в плагин.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # ЗВУК ИДЁТ ЧЕРЕЗ CDP-БИНДИНГ, А НЕ ЧЕРЕЗ НАШ HTTP-СЕРВЕР
//!
//! На пути сетевого варианта стоят ЧЕТЫРЕ независимых барьера:
//!
//! 1. `auth::csrf_guard` (`tmux-web/src/auth.rs`) отвергает апгрейд `/ws/*` с
//!    чужим `Origin`, а инжектированный скрипт живёт на `telemost.yandex.ru`;
//! 2. Bearer-авторизация — токена у страницы нет;
//! 3. жёсткий `connect-src` в CSP у Google Meet режет сокет к loopback;
//! 4. Private Network Access требует preflight для запроса из публичного
//!    origin в локальную сеть.
//!
//! `Runtime.addBinding` → `window.__forge_<rand32>(payload)` →
//! `Runtime.bindingCalled` убирает все четыре РАЗОМ, потому что убирает сеть.
//!
//! ## Отклонённый вариант, который нельзя делать никогда
//!
//! «Исключение в `csrf_guard` по одноразовому токену» **отклонено
//! окончательно**. Токен физически лежит в памяти страницы, которую
//! контролирует третья сторона, — а мы этой странице ещё и CSP отключили
//! (`Page.setBypassCSP`) ради инжекта. Скрипт площадки прочитает токен и
//! откроет WS к главному серверу, где живёт `/ws/attach`, то есть терминал.
//! Плагин записи встреч превратился бы в drive-by RCE.
//!
//! То же записано комментарием рядом с `RAW_UPLOAD_PATHS` в
//! `tmux-web/src/auth.rs` и строкой в таблице роутов
//! ([`crate::routes`]). Роут `/ws/meet/ingest` **не создаётся намеренно**, и
//! на его отсутствие есть тест.
//!
//! # Почему за трейтом
//!
//! Цена биндинга — 32 КБ/с на дорожку, 43 КБ/с после base64; пять активных
//! дорожек это около 213 КБ/с по локальному сокету. Расчётно приемлемо, но
//! проверяется только живой встречей. [`AudioTransport`] и поле конфига
//! [`crate::config::AudioTransport`] существуют затем, чтобы переход на
//! бинарный loopback-сокет или на Opus был СМЕНОЙ РЕАЛИЗАЦИИ, а не
//! переделкой пайплайна.
//!
//! > Совпадение имён не случайно и не опечатка: перечисление в конфиге —
//! > ВЫБОР транспорта, трейт здесь — сам транспорт. Модуль не реэкспортирует
//! > ни то, ни другое в корень крейта, поэтому спутать их в коде нельзя.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::frame::AudioFrame;
use crate::error::MeetError;

/// Разобранное сообщение со страницы.
#[derive(Debug, Clone, PartialEq)]
pub enum PageMessage {
    /// Кадр пер-трекового PCM16.
    Audio(AudioFrame),
    /// Событие жизненного цикла: появился трек, началась речь, сработал тап.
    Event(PageEvent),
    /// Служебное сообщение обёртки инжекта: сколько кадров потеряно в
    /// кольцевом буфере страницы до появления функции биндинга.
    Meta { dropped: u64, buffered: u64 },
}

/// Событие со страницы в сыром виде.
///
/// Поля остаются `serde_json::Value`: набор событий растёт каждой фазой, и
/// жёсткая структура здесь означала бы правку Rust на каждое новое поле
/// JavaScript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageEvent {
    /// Имя события (`track_added`, `speech_start`, `capture_path`, …).
    #[serde(rename = "n")]
    pub name: String,
    /// Момент на общей оси, мс от `T0`.
    #[serde(rename = "ms", default)]
    pub at_ms: u64,
    /// Остальные поля как есть.
    #[serde(flatten)]
    pub fields: serde_json::Value,
}

impl PageEvent {
    /// Строковое поле события.
    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(serde_json::Value::as_str)
    }

    /// Числовое поле события.
    pub fn u64_field(&self, key: &str) -> Option<u64> {
        self.fields.get(key).and_then(serde_json::Value::as_u64)
    }
}

/// Ошибка разбора сообщения страницы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Не JSON.
    NotJson,
    /// Неизвестный вид сообщения (поле `k`).
    UnknownKind(String),
    /// Кадр звука без обязательного поля.
    MalformedFrame(&'static str),
    /// base64 не декодируется.
    BadBase64,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => write!(f, "сообщение страницы не JSON"),
            Self::UnknownKind(k) => write!(f, "неизвестный вид сообщения: {k}"),
            Self::MalformedFrame(field) => write!(f, "в кадре звука нет поля {field}"),
            Self::BadBase64 => write!(f, "PCM не декодируется из base64"),
        }
    }
}

impl From<ParseError> for MeetError {
    fn from(e: ParseError) -> Self {
        MeetError::Internal(e.to_string())
    }
}

/// Разбирает одно сообщение со страницы.
///
/// Чистая функция: её вход — ровно то, что пришло в `Runtime.bindingCalled`,
/// и проверять её можно без браузера. Это важнее, чем кажется: формат кадра —
/// единственное место, где ошибка проявляется не отказом, а МОЛЧАЛИВО
/// испорченным звуком.
pub fn parse_page_message(raw: &str, sample_rate: u32) -> Result<PageMessage, ParseError> {
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|_| ParseError::NotJson)?;

    // Служебное сообщение обёртки инжекта.
    if v.get("__forge").and_then(serde_json::Value::as_str) == Some("meta") {
        return Ok(PageMessage::Meta {
            dropped: v.get("dropped").and_then(serde_json::Value::as_u64).unwrap_or(0),
            buffered: v.get("buffered").and_then(serde_json::Value::as_u64).unwrap_or(0),
        });
    }

    match v.get("k").and_then(serde_json::Value::as_str) {
        Some("a") => {
            let track_id = v
                .get("t")
                .and_then(serde_json::Value::as_str)
                .ok_or(ParseError::MalformedFrame("t"))?;
            let offset = v
                .get("o")
                .and_then(serde_json::Value::as_u64)
                .ok_or(ParseError::MalformedFrame("o"))?;
            let data = v
                .get("d")
                .and_then(serde_json::Value::as_str)
                .ok_or(ParseError::MalformedFrame("d"))?;
            let bytes = decode_base64(data).ok_or(ParseError::BadBase64)?;
            Ok(PageMessage::Audio(AudioFrame::new(
                track_id,
                offset,
                sample_rate,
                AudioFrame::samples_from_le_bytes(&bytes),
            )))
        }
        Some("e") => {
            let ev: PageEvent = serde_json::from_value(v).map_err(|_| ParseError::NotJson)?;
            Ok(PageMessage::Event(ev))
        }
        Some(other) => Err(ParseError::UnknownKind(other.to_string())),
        None => Err(ParseError::UnknownKind(String::new())),
    }
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Счётчики транспорта. Уезжают в манифест и в лог.
///
/// Потери обязаны быть ВИДНЫ: молчаливая потеря первых секунд записи —
/// худший из исходов, потому что она незаметна.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportStats {
    /// Принято кадров звука.
    pub frames: u64,
    /// Принято сэмплов.
    pub samples: u64,
    /// Сколько кадров страница потеряла ДО появления функции биндинга.
    pub dropped_by_page: u64,
    /// Сколько сообщений не разобралось.
    pub malformed: u64,
}

/// Транспорт звука со страницы.
///
/// Реализаций две: [`CdpBindingTransport`] (боевая) и [`ScriptedTransport`]
/// (тесты). Абстракция не декоративная — на ней держится обещание «переход на
/// другой транспорт это смена реализации».
#[async_trait]
pub trait AudioTransport: Send {
    /// Человекочитаемое имя. Совпадает со значением
    /// [`crate::config::AudioTransport::as_str`].
    fn name(&self) -> &'static str;

    /// Следующее сообщение. `None` — канал закрыт.
    async fn recv(&mut self) -> Option<PageMessage>;

    /// Счётчики.
    fn stats(&self) -> TransportStats;
}

/// Боевой транспорт: приёмник `Runtime.bindingCalled` из [`forge_cdp`].
pub struct CdpBindingTransport {
    rx: forge_cdp::BindingReceiver,
    sample_rate: u32,
    stats: TransportStats,
}

impl CdpBindingTransport {
    pub fn new(rx: forge_cdp::BindingReceiver, sample_rate: u32) -> Self {
        Self {
            rx,
            sample_rate,
            stats: TransportStats::default(),
        }
    }

    /// Имя биндинга — случайное на встречу. Постоянное имя было бы, во-первых,
    /// готовым признаком автоматизации для антибот-логики площадки, во-вторых,
    /// риском столкновения двух ботов в одном профиле.
    pub fn binding_name(&self) -> &str {
        self.rx.binding_name()
    }
}

#[async_trait]
impl AudioTransport for CdpBindingTransport {
    fn name(&self) -> &'static str {
        crate::config::AudioTransport::CdpBinding.as_str()
    }

    async fn recv(&mut self) -> Option<PageMessage> {
        loop {
            let (_session, msg) = self.rx.recv().await.ok()?;
            match msg {
                forge_cdp::BindingMessage::Meta { dropped, buffered } => {
                    self.stats.dropped_by_page = dropped;
                    return Some(PageMessage::Meta { dropped, buffered });
                }
                forge_cdp::BindingMessage::Payload(raw) => {
                    match parse_page_message(&raw, self.sample_rate) {
                        Ok(PageMessage::Audio(frame)) => {
                            self.stats.frames += 1;
                            self.stats.samples += frame.len() as u64;
                            return Some(PageMessage::Audio(frame));
                        }
                        Ok(other) => return Some(other),
                        Err(e) => {
                            // Одно битое сообщение НЕ повод рвать запись:
                            // кадр по устройству формата означает тишину на
                            // своём месте, а таймлайн не сдвигается.
                            self.stats.malformed += 1;
                            if self.stats.malformed <= 3 {
                                tracing::warn!(
                                    target: "forge_meet",
                                    error = %e,
                                    "forge-meet: сообщение страницы не разобралось"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn stats(&self) -> TransportStats {
        self.stats
    }
}

/// Транспорт-заглушка: отдаёт заранее заданные сообщения.
///
/// Публичен намеренно — им пользуются интеграционные тесты, а это отдельный
/// крейт, видящий только публичное API.
pub struct ScriptedTransport {
    queue: std::collections::VecDeque<PageMessage>,
    stats: TransportStats,
}

impl ScriptedTransport {
    pub fn new(messages: Vec<PageMessage>) -> Self {
        Self {
            queue: messages.into(),
            stats: TransportStats::default(),
        }
    }

    /// Собирает транспорт из сырых строк — ровно тех, что пришли бы из
    /// биндинга. Так проверяется и разбор, и приём одним тестом.
    pub fn from_raw(raw: &[&str], sample_rate: u32) -> Self {
        Self::new(
            raw.iter()
                .filter_map(|s| parse_page_message(s, sample_rate).ok())
                .collect(),
        )
    }
}

#[async_trait]
impl AudioTransport for ScriptedTransport {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn recv(&mut self) -> Option<PageMessage> {
        let msg = self.queue.pop_front()?;
        if let PageMessage::Audio(f) = &msg {
            self.stats.frames += 1;
            self.stats.samples += f.len() as u64;
        }
        Some(msg)
    }

    fn stats(&self) -> TransportStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_json(track: &str, offset: u64, samples: &[i16]) -> String {
        use base64::Engine as _;
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let d = base64::engine::general_purpose::STANDARD.encode(&bytes);
        format!(r#"{{"k":"a","t":"{track}","o":{offset},"n":{},"d":"{d}"}}"#, samples.len())
    }

    /// Кадр несёт РОВНО три вещи: идентификатор дорожки, сэмпловое смещение и
    /// PCM16 в base64. Всё остальное — служебное.
    #[test]
    fn an_audio_frame_carries_track_offset_and_pcm() {
        let raw = frame_json("track-a", 16_000, &[0, 1, -1, 32767]);
        let msg = parse_page_message(&raw, 16_000).unwrap();
        match msg {
            PageMessage::Audio(f) => {
                assert_eq!(f.track_id, "track-a");
                assert_eq!(f.offset_samples, 16_000);
                assert_eq!(f.sample_rate, 16_000);
                assert_eq!(f.pcm, vec![0, 1, -1, 32767]);
                assert_eq!(f.start_ms(), 1_000, "смещение переводится в мс однозначно");
                assert_eq!(f.byte_offset(), 32_000);
            }
            other => panic!("ожидался кадр звука, получено {other:?}"),
        }
    }

    #[test]
    fn events_keep_their_extra_fields() {
        let msg = parse_page_message(
            r#"{"k":"e","n":"track_added","ms":1500,"t":"track-a","idx":2,"off":1500}"#,
            16_000,
        )
        .unwrap();
        match msg {
            PageMessage::Event(e) => {
                assert_eq!(e.name, "track_added");
                assert_eq!(e.at_ms, 1_500);
                assert_eq!(e.str_field("t"), Some("track-a"));
                assert_eq!(e.u64_field("idx"), Some(2));
            }
            other => panic!("ожидалось событие, получено {other:?}"),
        }
    }

    /// Служебное сообщение обёртки инжекта: счётчик потерь обязан доезжать.
    /// Молчаливая потеря первых секунд записи незаметна — и потому худшая.
    #[test]
    fn the_page_reports_its_own_losses() {
        let msg = parse_page_message(r#"{"__forge":"meta","dropped":7,"buffered":3}"#, 16_000).unwrap();
        assert_eq!(msg, PageMessage::Meta { dropped: 7, buffered: 3 });
    }

    #[test]
    fn broken_messages_are_named_not_swallowed() {
        assert_eq!(parse_page_message("не json", 16_000), Err(ParseError::NotJson));
        assert_eq!(
            parse_page_message(r#"{"k":"z"}"#, 16_000),
            Err(ParseError::UnknownKind("z".into()))
        );
        assert_eq!(
            parse_page_message(r#"{"k":"a","o":0,"d":"AA=="}"#, 16_000),
            Err(ParseError::MalformedFrame("t"))
        );
        assert_eq!(
            parse_page_message(r#"{"k":"a","t":"x","d":"AA=="}"#, 16_000),
            Err(ParseError::MalformedFrame("o"))
        );
        assert_eq!(
            parse_page_message(r#"{"k":"a","t":"x","o":0,"d":"не base64!!"}"#, 16_000),
            Err(ParseError::BadBase64)
        );
    }

    /// Кадры, пришедшие НЕ ПО ПОРЯДКУ, разбираются одинаково: адрес кадра —
    /// его собственное смещение, а не место в потоке.
    #[test]
    fn frames_do_not_depend_on_arrival_order() {
        let a = frame_json("t", 0, &[1, 2]);
        let b = frame_json("t", 320, &[3, 4]);
        let straight = [a.as_str(), b.as_str()];
        let shuffled = [b.as_str(), a.as_str()];

        let mut first: Vec<AudioFrame> = straight
            .iter()
            .map(|s| match parse_page_message(s, 16_000).unwrap() {
                PageMessage::Audio(f) => f,
                _ => unreachable!(),
            })
            .collect();
        let mut second: Vec<AudioFrame> = shuffled
            .iter()
            .map(|s| match parse_page_message(s, 16_000).unwrap() {
                PageMessage::Audio(f) => f,
                _ => unreachable!(),
            })
            .collect();
        first.sort_by_key(|f| f.offset_samples);
        second.sort_by_key(|f| f.offset_samples);
        assert_eq!(first, second);
    }

    /// ТРЕЙТ РЕАЛЬНО АБСТРАГИРУЕТ: пайплайн работает с `dyn AudioTransport`,
    /// не зная, биндинг это или заглушка.
    #[tokio::test]
    async fn the_trait_lets_a_stub_stand_in_for_the_binding() {
        async fn drain(t: &mut dyn AudioTransport) -> (usize, TransportStats) {
            let mut n = 0;
            while let Some(msg) = t.recv().await {
                if matches!(msg, PageMessage::Audio(_)) {
                    n += 1;
                }
            }
            (n, t.stats())
        }

        let raw: Vec<String> = (0..5)
            .map(|i| frame_json("t", i * 320, &[100; 320]))
            .collect();
        let refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let mut stub = ScriptedTransport::from_raw(&refs, 16_000);
        assert_eq!(stub.name(), "scripted");

        let (frames, stats) = drain(&mut stub).await;
        assert_eq!(frames, 5);
        assert_eq!(stats.frames, 5);
        assert_eq!(stats.samples, 5 * 320);
    }

    /// Имя транспорта совпадает со значением конфига: расхождение означало бы,
    /// что в манифесте написан один транспорт, а работает другой.
    #[test]
    fn the_transport_name_matches_the_config_value() {
        assert_eq!(
            crate::config::AudioTransport::CdpBinding.as_str(),
            "cdp_binding"
        );
        assert_eq!(
            crate::config::AudioTransport::LoopbackSocket.as_str(),
            "loopback_socket"
        );
    }

    /// Пятиминутная запись при 20-мс кадрах — 15 000 кадров на дорожку.
    /// Проверяем, что счётчики не теряют ни одного и разбор не деградирует.
    #[tokio::test]
    async fn five_minutes_of_frames_arrive_without_losses() {
        let frames_per_track = 5 * 60 * 1000 / 20;
        let mut msgs = Vec::with_capacity(frames_per_track);
        for i in 0..frames_per_track {
            let raw = frame_json("t", (i as u64) * 320, &[0; 320]);
            msgs.push(parse_page_message(&raw, 16_000).unwrap());
        }
        let mut t = ScriptedTransport::new(msgs);
        let mut got = 0u64;
        let mut last_offset = None;
        while let Some(PageMessage::Audio(f)) = t.recv().await {
            if let Some(prev) = last_offset {
                assert_eq!(
                    f.offset_samples,
                    prev + 320,
                    "разрыв в сэмпловых смещениях — кадр потерян"
                );
            }
            last_offset = Some(f.offset_samples);
            got += 1;
        }
        assert_eq!(got, frames_per_track as u64);
        assert_eq!(t.stats().frames, frames_per_track as u64);
        assert_eq!(t.stats().malformed, 0);
    }
}
