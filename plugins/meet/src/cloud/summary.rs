//! Облачное саммари: `POST /chat/completions` со стримом SSE.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Стриминг обязателен, и дельты идут ТЕМ ЖЕ путём
//!
//! Экранный вывод саммари живёт на кадрах `Progress { phase: Summarizing,
//! done }`, которые шлёт [`crate::summary::summarize`] по колбэку `on_delta`,
//! пропущенному через [`crate::summary::DeltaCoalescer`]. Облачный путь встаёт
//! ровно на это место: он принимает такой же колбэк и гоняет его через ТОТ ЖЕ
//! коалесцер. Фронт не переделывается — он вообще не узнаёт, кто считал.
//!
//! Свой коалесцер здесь был бы тихой поломкой: частота кадров на фронте
//! изменилась бы, и «оно как-то дёргается» никто бы не связал с облаком.
//!
//! # Промпт ОДИН на оба пути
//!
//! Берётся готовый [`crate::summary::BuiltPrompt`] и системная часть
//! [`crate::summary::SYSTEM_PROMPT`] — те же самые, что уходят в Claude CLI.
//! Второй промпт «для облака» разошёлся бы с первым на первой же правке, и
//! саммари двух встреч выглядели бы по-разному в зависимости от того, был ли в
//! тот день включён GPU.
//!
//! Системная часть уходит ОТДЕЛЬНЫМ сообщением с ролью `system`, а не
//! приклеивается к расшифровке: склеенная с данными, она подделывалась бы
//! содержимым разговора.
//!
//! # Обрыв посреди стрима не теряет написанное
//!
//! Ответ модели приходит по кусочкам, и обрыв на середине — это не «саммари
//! не получилось», а «саммари получилось неполным». Уже полученный текст
//! возвращается, причина обрыва едет рядом в
//! [`CloudSummary::interrupted`] — в журнал встречи и в кадр `error`. Правило
//! «`failed` только когда нет транскрипта» при этом не трогается вовсе: саммари
//! статус встречи не роняет никогда.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{status_error, CloudClient, CloudError};
use crate::pipeline::proc::ProcError;
use crate::summary::{BuiltPrompt, DeltaCoalescer, SYSTEM_PROMPT};

/// Кадр потока SSE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// Тело `data:` — как есть, без разбора.
    Data(String),
    /// `data: [DONE]` — поток закончен штатно.
    Done,
}

/// Сборщик кадров SSE из кусков байтового потока.
///
/// Существует потому, что граница TCP-чтения НЕ СОВПАДАЕТ с границей кадра:
/// `data: {"choices":[{"del` вполне может приехать одним куском, а хвост —
/// следующим. Наивный разбор «каждый кусок это кадр» теряет такие кадры молча,
/// то есть выбрасывает куски саммари.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    /// Хвост незавершённого UTF-8-символа с прошлого куска.
    ///
    /// Граница TCP-чтения режет и СИМВОЛ тоже: «Я» это два байта, и они
    /// приезжают в разных кусках. `from_utf8_lossy` на такой половине
    /// подставил бы U+FFFD НАВСЕГДА — в тексте саммари остались бы ромбики.
    pending: Vec<u8>,
}

impl SseParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавляет кусок БАЙТ, доклеивая хвост символа с прошлого раза.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);
        match std::str::from_utf8(&bytes) {
            Ok(s) => {
                let s = s.to_string();
                self.push(&s)
            }
            Err(e) => {
                let good = &bytes[..e.valid_up_to()];
                let rest = &bytes[e.valid_up_to()..];
                let s = std::str::from_utf8(good).unwrap_or_default().to_string();
                match e.error_len() {
                    // Символ обрезан концом куска — ждём продолжения.
                    None => self.pending = rest.to_vec(),
                    // Настоящий мусор в потоке: пропускаем байт и идём дальше,
                    // иначе один битый байт остановил бы весь ответ.
                    Some(n) => self.pending = rest[n.min(rest.len())..].to_vec(),
                }
                self.push(&s)
            }
        }
    }

    /// Добавляет кусок и отдаёт все кадры, которые в нём завершились.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        // Кадр заканчивается переводом строки; недописанный хвост остаётся в
        // буфере до следующего куска.
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf[..pos].trim_end_matches('\r').to_string();
            self.buf.drain(..=pos);
            if let Some(ev) = parse_line(&line) {
                out.push(ev);
            }
        }
        out
    }

    /// Хвост, оставшийся без перевода строки. Сервер имеет право не дописать
    /// последний `\n`, и терять из-за этого последний кадр нельзя.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let line = std::mem::take(&mut self.buf);
        parse_line(line.trim())
    }
}

/// Разбирает одну строку потока.
fn parse_line(line: &str) -> Option<SseEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // Строка-комментарий (`: ping`) — штатный keep-alive, а не данные.
    if line.starts_with(':') {
        return None;
    }
    let payload = line.strip_prefix("data:")?.trim();
    if payload == "[DONE]" {
        return Some(SseEvent::Done);
    }
    if payload.is_empty() {
        return None;
    }
    Some(SseEvent::Data(payload.to_string()))
}

/// Текстовая дельта из кадра OpenAI-вида
/// `{"choices":[{"delta":{"content":"…"}}]}`.
#[must_use]
pub fn delta_of(frame: &Value) -> Option<&str> {
    frame
        .get("choices")?
        .as_array()?
        .first()?
        .get("delta")?
        .get("content")?
        .as_str()
}

/// Кто на самом деле исполнил запрос. Полезно в журнале: агрегатор может
/// увести запрос к другому провайдеру, и «почему сегодня медленно» отвечается
/// именно этим полем.
#[must_use]
pub fn provider_of(frame: &Value) -> Option<String> {
    frame
        .get("provider")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Тело запроса. **Чистая функция** — на неё смотрит тест инвариантов.
#[must_use]
pub fn build_chat_body(model: &str, system: &str, user: &str) -> Value {
    json!({
        "model": model,
        "stream": true,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    })
}

/// Настройки облачного саммари.
#[derive(Debug, Clone)]
pub struct ChatOptions {
    /// Модель. **Поле, а не константа**: каталог отдаёт Claude, GPT-5.x,
    /// DeepSeek и YandexGPT, и выбор принадлежит пользователю.
    pub model: String,
    /// Потолок паузы МЕЖДУ кадрами, а не всего ответа: длинное саммари идёт
    /// минуты, и общий таймаут резал бы его тем чаще, чем лучше оно получалось.
    pub idle_timeout: Duration,
    pub cancel: CancellationToken,
}

/// Итог облачного саммари.
#[derive(Debug, Clone, Default)]
pub struct CloudSummary {
    pub text: String,
    /// Реальный исполнитель запроса.
    pub provider: Option<String>,
    /// Чем оборвался поток. `Some` вместе с непустым `text` означает «саммари
    /// неполное».
    pub interrupted: Option<ProcError>,
    /// Сколько кадров пришло — по нему видно, был ли стрим вообще.
    pub frames: usize,
}

/// Гоняет `chat/completions` в режиме стрима.
///
/// `on_delta` зовётся с общим числом накопленных code points — тот же контракт,
/// что у локального пути, и через тот же [`DeltaCoalescer`].
pub async fn stream_chat(
    client: &CloudClient,
    system: &str,
    user: &str,
    opts: &ChatOptions,
    on_delta: Option<&(dyn Fn(usize) + Send + Sync)>,
) -> Result<CloudSummary, ProcError> {
    let body = build_chat_body(&opts.model, system, user);
    let started = Instant::now();

    let send = client.post("chat/completions").json(&body).send();
    // Потолок стоит и на ЗАГОЛОВКАХ тоже: молчащий шлюз держал бы соединение
    // открытым сколько угодно, а у клиента общего таймаута нет намеренно.
    let resp = tokio::select! {
        biased;
        _ = opts.cancel.cancelled() => return Err(cancelled()),
        r = tokio::time::timeout(opts.idle_timeout, send) => match r {
            Err(_) => return Err(ProcError::from(CloudError::Timeout(format!(
                "облако не начало отвечать за {} с",
                opts.idle_timeout.as_secs()
            )))),
            Ok(r) => r.map_err(CloudError::from_reqwest).map_err(ProcError::from)?,
        },
    };

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        return Err(status_error(status, &text).into());
    }

    let mut out = CloudSummary::default();
    let mut parser = SseParser::new();
    let mut coalescer = DeltaCoalescer::with_defaults(Instant::now());
    let mut stream = resp.bytes_stream();

    loop {
        let next = tokio::select! {
            biased;
            _ = opts.cancel.cancelled() => return Err(cancelled()),
            n = tokio::time::timeout(opts.idle_timeout, stream.next()) => n,
        };
        let chunk = match next {
            // Пауза между кадрами больше потолка: соединение живо, но ответа
            // нет — считаем поток мёртвым и отдаём написанное.
            Err(_) => {
                out.interrupted = Some(ProcError::new(
                    "cloud_timeout",
                    format!(
                        "облако молчало дольше {} с посреди ответа",
                        opts.idle_timeout.as_secs()
                    ),
                    "",
                ));
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                out.interrupted = Some(ProcError::from(CloudError::from_reqwest(e)));
                break;
            }
            Ok(Some(Ok(bytes))) => bytes,
        };

        // Куски склеиваются как БАЙТЫ: граница чтения режет и кадр, и символ.
        let events = parser.push_bytes(&chunk);
        let mut done = false;
        for ev in events {
            match ev {
                SseEvent::Done => {
                    done = true;
                }
                SseEvent::Data(raw) => {
                    out.frames += 1;
                    let Ok(frame) = serde_json::from_str::<Value>(&raw) else {
                        // Мусорный кадр — не повод потерять весь ответ.
                        continue;
                    };
                    if out.provider.is_none() {
                        out.provider = provider_of(&frame);
                    }
                    if let Some(delta) = delta_of(&frame) {
                        out.text.push_str(delta);
                        if let (Some(total), Some(cb)) =
                            (coalescer.push(delta, Instant::now()), on_delta)
                        {
                            cb(total);
                        }
                    }
                }
            }
        }
        if done {
            break;
        }
    }

    if let Some(ev) = parser.finish() {
        if let SseEvent::Data(raw) = ev {
            if let Ok(frame) = serde_json::from_str::<Value>(&raw) {
                out.frames += 1;
                if let Some(delta) = delta_of(&frame) {
                    out.text.push_str(delta);
                }
            }
        }
    }
    if let (Some(total), Some(cb)) = (coalescer.finish(), on_delta) {
        cb(total);
    }

    out.text = out.text.trim().to_string();
    if out.text.is_empty() {
        return Err(out.interrupted.unwrap_or_else(|| {
            ProcError::new("summary_empty", "облако вернуло пустой ответ", "")
        }));
    }
    tracing::info!(
        target: "forge_meet",
        provider = out.provider.as_deref().unwrap_or("-"),
        model = %opts.model,
        chars = out.text.chars().count(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "forge-meet: облачное саммари собрано"
    );
    Ok(out)
}

fn cancelled() -> ProcError {
    ProcError::new("cancelled", "саммари отменено", "")
}

/// Облачный аналог [`crate::summary::generate`].
///
/// Сигнатура повторена намеренно: Ф15 ставит выбор между локальным и облачным
/// путём одной развилкой, и обе стороны обязаны выглядеть одинаково.
///
/// Обрыв посреди ответа возвращает уже написанное: полтора экрана саммари
/// полезнее, чем «саммари не получилось».
pub async fn generate(
    client: &CloudClient,
    built: &BuiltPrompt,
    opts: &ChatOptions,
    on_delta: Option<&(dyn Fn(usize) + Send + Sync)>,
) -> Result<String, ProcError> {
    // Промпт и системная часть — те же, что у локального пути.
    let out = stream_chat(client, SYSTEM_PROMPT, &built.text, opts, on_delta).await?;
    if let Some(err) = &out.interrupted {
        tracing::warn!(
            target: "forge_meet",
            code = err.code,
            chars = out.text.chars().count(),
            "forge-meet: облачное саммари оборвалось, написанное сохранено: {}",
            err.message
        );
    }
    Ok(out.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(content: &str) -> String {
        format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": {"content": content}}]})
        )
    }

    /// **Кадр, разорванный на середине, не теряется.**
    #[test]
    fn a_frame_split_across_chunks_is_reassembled() {
        let whole = frame("Коллеги");
        let (a, b) = whole.split_at(whole.len() / 2);
        let mut p = SseParser::new();
        assert!(p.push(a).is_empty(), "полкадра — ещё не кадр");
        let evs = p.push(b);
        assert_eq!(evs.len(), 1);
        let SseEvent::Data(raw) = &evs[0] else {
            panic!("{evs:?}")
        };
        assert_eq!(
            delta_of(&serde_json::from_str(raw).unwrap()),
            Some("Коллеги")
        );
    }

    #[test]
    fn keepalives_and_done_are_recognised() {
        let mut p = SseParser::new();
        let evs = p.push(": ping\n\n");
        assert!(evs.is_empty(), "keep-alive — не данные: {evs:?}");
        let evs = p.push(&format!("{}{}", frame("а"), "data: [DONE]\n\n"));
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1], SseEvent::Done);

        // Кадр без завершающего перевода строки доезжает финальным сливом.
        let mut p = SseParser::new();
        assert!(p.push("data: {\"choices\":[{\"delta\":{\"content\":\"хвост\"}}]}").is_empty());
        let SseEvent::Data(raw) = p.finish().unwrap() else {
            panic!()
        };
        assert_eq!(delta_of(&serde_json::from_str(&raw).unwrap()), Some("хвост"));
        assert!(p.finish().is_none());
    }

    #[test]
    fn the_provider_is_read_from_the_frame() {
        let f: Value = json!({"provider": "google-vertex/global", "choices": []});
        assert_eq!(provider_of(&f).as_deref(), Some("google-vertex/global"));
        assert_eq!(provider_of(&json!({})), None);
        assert_eq!(delta_of(&json!({"choices": [{}]})), None);
    }

    /// Тело запроса: стрим включён, системная часть ОТДЕЛЬНЫМ сообщением.
    #[test]
    fn the_request_streams_and_keeps_the_system_prompt_separate() {
        let b = build_chat_body("google/gemini-3.5-flash", "система", "<transcript>x</transcript>");
        assert_eq!(b["stream"], json!(true), "без стрима экран молчал бы до конца");
        assert_eq!(b["model"], json!("google/gemini-3.5-flash"));
        assert_eq!(b["messages"][0]["role"], json!("system"));
        assert_eq!(b["messages"][1]["role"], json!("user"));
        assert!(
            !b["messages"][1]["content"].as_str().unwrap().contains("система"),
            "системная часть склеена с данными — её подделает расшифровка"
        );
    }

    /// **ПРОМПТ ОДИН НА ОБА ПУТИ.**
    ///
    /// Проверка по исходнику: собственная системная инструкция здесь появилась
    /// бы как строковый литерал, и разошлась бы с локальной на первой правке.
    #[test]
    fn the_cloud_path_reuses_the_local_prompt() {
        let src = include_str!("summary.rs");
        let head = src.split("mod tests").next().unwrap();
        assert!(
            head.contains("use crate::summary::{BuiltPrompt, DeltaCoalescer, SYSTEM_PROMPT}"),
            "облако перестало брать промпт и коалесцер у локального пути"
        );
        assert!(
            !head.contains("SYSTEM_PROMPT: &str ="),
            "в облачном пути завёлся второй системный промпт"
        );
        assert!(
            head.contains("DeltaCoalescer::with_defaults"),
            "коалесцер обязан быть ТОТ ЖЕ: свой изменил бы частоту кадров на фронте"
        );
    }

    /// Мусорный кадр посреди потока не роняет ответ.
    #[test]
    fn a_garbage_frame_does_not_kill_the_stream() {
        let mut p = SseParser::new();
        let evs = p.push(&format!("data: не json\n{}", frame("текст")));
        assert_eq!(evs.len(), 2);
        let ok: Vec<&str> = evs
            .iter()
            .filter_map(|e| match e {
                SseEvent::Data(raw) => serde_json::from_str::<Value>(raw)
                    .ok()
                    .and_then(|v| delta_of(&v).map(str::to_string)),
                SseEvent::Done => None,
            })
            .map(|_| "ok")
            .collect();
        assert_eq!(ok.len(), 1, "разобрался ровно один кадр");
    }

    /// Кириллица, разрезанная границей куска ПО СЕРЕДИНЕ СИМВОЛА, склеивается
    /// без потерь.
    #[test]
    fn a_multibyte_character_split_between_chunks_survives() {
        let whole = frame("ЯЁЖ");
        let bytes = whole.as_bytes();
        // Ищем разрез внутри многобайтового символа.
        let cut = bytes
            .iter()
            .position(|b| *b == 0xD0 || *b == 0xD1)
            .expect("в кадре нет кириллицы")
            + 1;
        let mut p = SseParser::new();
        let mut evs = p.push_bytes(&bytes[..cut]);
        evs.extend(p.push_bytes(&bytes[cut..]));
        let data: Vec<String> = evs
            .into_iter()
            .filter_map(|e| match e {
                SseEvent::Data(raw) => Some(raw),
                SseEvent::Done => None,
            })
            .collect();
        assert_eq!(data.len(), 1, "{data:?}");
        let v: Value = serde_json::from_str(&data[0]).unwrap();
        assert_eq!(delta_of(&v), Some("ЯЁЖ"), "символ рассыпался на границе куска");
    }
}
