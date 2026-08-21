//! Хранилище готовых расшифровок голосовых сообщений.
//!
//! По образцу `plugins/chat-analysis/src/prompts.rs`: JSON на диске, путь
//! ПАРАМЕТРОМ конструктора, а не константой внутри. Ядро не знает, где у
//! плагина каталог кэша, и знать не должно — у VK, Telegram и Bitrix он в трёх
//! разных местах.
//!
//! # Раскладка повторяет медиа-кэш
//!
//! ```text
//! <cache_dir>/media/<peer_id>/<message_id>.<index>.<kind>.bin   ← уже есть
//! <cache_dir>/voice/<peer_id>/<message_id>.<index>.json         ← это
//! ```
//!
//! Те же координаты `(peer, message_id, index)`, что и у файла, который
//! расшифровывали. Это не эстетика: чистка кэша по беседе сносит один каталог,
//! а не ходит по индексу; и человек, заглянувший в каталог руками, видит рядом
//! файл и его расшифровку.
//!
//! # ЧТЕНИЕ НЕ ПАДАЕТ НИКОГДА
//!
//! Битый JSON, обрезанный файл, версия схемы из будущего — всё это отвечает
//! «расшифровки нет», а не ошибкой. Причина простая: расшифровка — КЭШ, её
//! всегда можно посчитать заново, а отказ на чтении означал бы, что один
//! испорченный файл (питание пропало на записи) навсегда ломает кнопку у
//! конкретного сообщения. Битый файл молча перезаписывается следующим успешным
//! разбором.
//!
//! Ошибка остаётся ровно на записи: там она означает «диск переполнен» или
//! «нет прав», и об этом человеку сказать надо.
//!
//! # Запись атомарна
//!
//! Через `forge-atomic`: `tmp` → `fsync` → `rename` → `0600`. Половина файла на
//! диске выглядела бы обрезанным JSON — то есть, по правилу выше, «расшифровки
//! нет», и потеря была бы тихой. Атомарность делает такое состояние
//! невозможным, а не маловероятным.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::VoiceError;

/// Версия схемы файла расшифровки.
///
/// Читаются только файлы С ЭТОЙ версией. Файл более новой версии — это
/// расшифровка, записанная сборкой из будущего (человек откатил devforge), и
/// читать её по старой схеме значило бы показать неверный текст. Такой файл
/// считается отсутствующим и перезаписывается.
pub const SCHEMA_VERSION: u32 = 1;

/// Подкаталог хранилища внутри каталога кэша плагина.
pub const STORE_DIR: &str = "voice";

/// Каким движком получена расшифровка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// Локальный whisper.cpp на GPU.
    Local,
    /// Облако polza.ai.
    Cloud,
}

impl Engine {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

/// Координаты голосового вложения. Те же, что в ключе медиа-кэша.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VoiceKey {
    /// Беседа/диалог.
    pub peer: String,
    /// Сообщение.
    pub message_id: String,
    /// Номер вложения внутри сообщения.
    pub index: u8,
}

impl VoiceKey {
    pub fn new(peer: impl Into<String>, message_id: impl Into<String>, index: u8) -> Self {
        Self {
            peer: peer.into(),
            message_id: message_id.into(),
            index,
        }
    }

    /// Имя файла расшифровки без каталога.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.{}.json", sanitise(&self.message_id), self.index)
    }
}

/// Чужой идентификатор в имени файла.
///
/// `peer` и `message_id` приходят от мессенджера, то есть это ЧУЖОЙ ВВОД:
/// Bitrix даёт `chat123`, VK — число, Telegram — число со знаком. Всё, что не
/// буква, цифра, минус или подчёркивание, заменяется на `_` — иначе строка
/// `../../../etc/passwd` в идентификаторе увела бы запись из каталога кэша.
fn sanitise(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

/// Готовая расшифровка одного голосового сообщения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Версия схемы. Читается ПЕРВЫМ делом: файл чужой версии не разбирается.
    pub schema: u32,
    /// Текст расшифровки.
    pub text: String,
    /// Язык, как его назвал движок. Пустая строка — движок не сказал.
    #[serde(default)]
    pub language: String,
    /// Каким движком получено.
    pub engine: Engine,
    /// Длительность записи, мс. Ноль — неизвестна.
    #[serde(default)]
    pub duration_ms: i64,
    /// Когда посчитано, unix-секунды.
    #[serde(default)]
    pub created_at: i64,
}

impl Transcript {
    /// Собирает запись с текущей меткой времени.
    pub fn new(text: impl Into<String>, engine: Engine) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            text: text.into(),
            language: String::new(),
            engine,
            duration_ms: 0,
            created_at: now_secs(),
        }
    }

    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    #[must_use]
    pub fn with_duration_ms(mut self, ms: i64) -> Self {
        self.duration_ms = ms;
        self
    }
}

/// Текущее время в unix-секундах.
///
/// `SystemTime`, а не `chrono`: метка нужна только для показа «расшифровано
/// тогда-то» и для будущей чистки по возрасту, а тащить в листовой крейт ещё
/// одну зависимость ради одного числа незачем.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Замок на время `tmp` → `rename` ВНУТРИ процесса.
///
/// # Зачем он нужен поверх атомарной записи
///
/// `forge_atomic::tmp_path_for` называет временный файл по PID —
/// `.<имя>.<pid>.tmp`. Между процессами этого достаточно, а вот два ПОТОКА
/// одного процесса, пишущие один и тот же путь, получают ОДНО имя временного
/// файла: первый переименовывает его, второй тут же падает на `rename` с
/// «No such file or directory», а в худшем случае дописывает в чужой tmp. Это
/// не теория — ровно так падал тест параллельной записи, пока замка не было.
///
/// Замок глобальный, а не поле стора: у каждого плагина свой экземпляр
/// [`TranscriptStore`], и защищать надо ИМЯ ВРЕМЕННОГО ФАЙЛА, которое общее для
/// всего процесса. Цена — сериализация записей транскриптов; они редкие
/// (одна на голосовое) и мелкие, а расшифровка перед этим шла секунды.
fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// JSON-стор расшифровок с корнем в переданном каталоге.
#[derive(Debug, Clone)]
pub struct TranscriptStore {
    root: PathBuf,
}

impl TranscriptStore {
    /// `cache_dir` — каталог кэша плагина; стор живёт в его подкаталоге
    /// [`STORE_DIR`]. Путь НЕ хардкодится внутри: у трёх мессенджеров он разный.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: cache_dir.into().join(STORE_DIR),
        }
    }

    /// Корень стора.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Путь файла расшифровки.
    #[must_use]
    pub fn path(&self, key: &VoiceKey) -> PathBuf {
        self.root.join(sanitise(&key.peer)).join(key.file_name())
    }

    /// Читает расшифровку. `None` — её нет, либо файл непригоден.
    ///
    /// Непригоден = битый JSON, обрезанный файл, чужая версия схемы. Все три
    /// случая неотличимы от «ещё не считали», и это НАМЕРЕННО: см. шапку
    /// модуля.
    #[must_use]
    pub fn get(&self, key: &VoiceKey) -> Option<Transcript> {
        let path = self.path(key);
        let raw = std::fs::read_to_string(&path).ok()?;
        let parsed: Transcript = match serde_json::from_str(&raw) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(
                    target: "forge_voice",
                    path = %path.display(),
                    "forge-voice: файл расшифровки не разбирается, считаем что её нет: {e}"
                );
                return None;
            }
        };
        if parsed.schema != SCHEMA_VERSION {
            tracing::debug!(
                target: "forge_voice",
                path = %path.display(),
                schema = parsed.schema,
                "forge-voice: чужая версия схемы расшифровки, считаем что её нет"
            );
            return None;
        }
        Some(parsed)
    }

    /// Есть ли пригодная расшифровка.
    #[must_use]
    pub fn has(&self, key: &VoiceKey) -> bool {
        self.get(key).is_some()
    }

    /// Пишет расшифровку АТОМАРНО, перекрывая прежнюю.
    ///
    /// # Errors
    ///
    /// Диск переполнен, нет прав, каталог не создаётся. Здесь ошибка настоящая:
    /// в отличие от чтения, молчать о ней нельзя — пользователь ждёт, что
    /// расшифровка сохранилась.
    pub fn put(&self, key: &VoiceKey, transcript: &Transcript) -> Result<(), VoiceError> {
        let mut record = transcript.clone();
        // Версию проставляем САМИ: вызывающий не обязан её помнить, а запись со
        // случайным нулём в этом поле не прочиталась бы уже никогда.
        record.schema = SCHEMA_VERSION;
        if record.created_at == 0 {
            record.created_at = now_secs();
        }
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|e| VoiceError::Storage(format!("расшифровка не сериализуется: {e}")))?;
        let _guard = write_lock();
        forge_atomic::write_bytes_atomic(&self.path(key), &bytes)
            .map_err(|e| VoiceError::Storage(format!("{e}")))
    }

    /// Удаляет расшифровку. Отсутствие файла ошибкой не считается.
    pub fn remove(&self, key: &VoiceKey) -> Result<(), VoiceError> {
        match std::fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(VoiceError::Storage(format!("{e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, TranscriptStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = TranscriptStore::new(dir.path());
        (dir, s)
    }

    #[test]
    fn a_transcript_survives_a_round_trip() {
        let (_d, s) = store();
        let key = VoiceKey::new("2000000042", "12345", 0);
        assert!(s.get(&key).is_none(), "пустой стор ничего не знает");

        let t = Transcript::new("Привет, это голосовое.", Engine::Cloud)
            .with_language("ru")
            .with_duration_ms(4_200);
        s.put(&key, &t).unwrap();

        let back = s.get(&key).unwrap();
        assert_eq!(back.text, "Привет, это голосовое.");
        assert_eq!(back.engine, Engine::Cloud);
        assert_eq!(back.language, "ru");
        assert_eq!(back.duration_ms, 4_200);
        assert_eq!(back.schema, SCHEMA_VERSION);
        assert!(back.created_at > 0, "метка времени не проставлена");
        assert!(s.has(&key));

        // Перезапись перекрывает прежнюю расшифровку целиком.
        s.put(&key, &Transcript::new("Другой текст.", Engine::Local)).unwrap();
        let back = s.get(&key).unwrap();
        assert_eq!(back.text, "Другой текст.");
        assert_eq!(back.engine, Engine::Local);

        s.remove(&key).unwrap();
        assert!(s.get(&key).is_none());
        // Удаление того, чего нет, — не ошибка.
        s.remove(&key).unwrap();
    }

    /// Разные вложения одного сообщения не перетирают друг друга.
    #[test]
    fn every_attachment_has_its_own_file() {
        let (_d, s) = store();
        let a = VoiceKey::new("peer", "77", 0);
        let b = VoiceKey::new("peer", "77", 1);
        let c = VoiceKey::new("другая", "77", 0);
        s.put(&a, &Transcript::new("первое", Engine::Local)).unwrap();
        s.put(&b, &Transcript::new("второе", Engine::Local)).unwrap();
        s.put(&c, &Transcript::new("третье", Engine::Local)).unwrap();
        assert_eq!(s.get(&a).unwrap().text, "первое");
        assert_eq!(s.get(&b).unwrap().text, "второе");
        assert_eq!(s.get(&c).unwrap().text, "третье");
        assert_ne!(s.path(&a), s.path(&b));
        assert_ne!(s.path(&a), s.path(&c));
    }

    /// **БИТЫЙ И ОБРЕЗАННЫЙ ФАЙЛ — ЭТО «РАСШИФРОВКИ НЕТ», А НЕ ОШИБКА.**
    #[test]
    fn a_broken_file_reads_as_no_transcript_and_is_overwritten() {
        let (_d, s) = store();
        let key = VoiceKey::new("peer", "9", 0);
        let path = s.path(&key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        for bad in [
            "не json вовсе",
            "",
            // Обрезанный на середине: ровно то, что оставила бы неатомарная
            // запись при пропаже питания.
            r#"{"schema":1,"text":"Привет, это гол"#,
            "[]",
            "null",
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(s.get(&key).is_none(), "битый файл прочитался: {bad:?}");
            assert!(!s.has(&key));
        }

        // И следующий успешный разбор перезаписывает его без единой жалобы.
        s.put(&key, &Transcript::new("готово", Engine::Cloud)).unwrap();
        assert_eq!(s.get(&key).unwrap().text, "готово");
    }

    /// **ЧУЖАЯ ВЕРСИЯ СХЕМЫ ЧИТАЕТСЯ КАК «НЕТ», БЕЗ ПОТЕРИ ДАННЫХ.**
    ///
    /// Файл из будущего разбирать по старой схеме нельзя: получился бы неверный
    /// текст, а не отказ. До перезаписи он остаётся на диске целым.
    #[test]
    fn an_unknown_schema_version_reads_as_no_transcript() {
        let (_d, s) = store();
        let key = VoiceKey::new("peer", "9", 0);
        let path = s.path(&key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let future = serde_json::json!({
            "schema": SCHEMA_VERSION + 1,
            "text": "текст из будущего",
            "engine": "cloud",
            "language": "ru"
        });
        std::fs::write(&path, future.to_string()).unwrap();
        assert!(s.get(&key).is_none());
        // Данные на месте: чтение ничего не стёрло.
        assert!(std::fs::read_to_string(&path).unwrap().contains("будущего"));

        // Нулевая версия (файл, записанный до появления поля) — тоже «нет».
        std::fs::write(&path, r#"{"schema":0,"text":"старьё","engine":"local"}"#).unwrap();
        assert!(s.get(&key).is_none());
    }

    /// **ПАРАЛЛЕЛЬНАЯ ЗАПИСЬ НЕ ОСТАВЛЯЕТ ПОЛУФАЙЛА.**
    ///
    /// Побеждает одна из записей целиком — какая именно, неважно; важно, что
    /// файл на диске всегда разбирается.
    #[test]
    fn two_concurrent_writes_never_leave_half_a_file() {
        let (_d, s) = store();
        let key = VoiceKey::new("peer", "1", 0);
        let long = "а".repeat(200_000);

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = s.clone();
                let key = key.clone();
                let text = if i % 2 == 0 {
                    long.clone()
                } else {
                    "коротко".to_string()
                };
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        s.put(&key, &Transcript::new(text.clone(), Engine::Local)).unwrap();
                        // Читаем прямо во время чужих записей: обрезанного
                        // файла не должно увидеть ни одно чтение.
                        assert!(s.get(&key).is_some(), "чтение поймало полуфайл");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let final_text = s.get(&key).unwrap().text;
        assert!(final_text == long || final_text == "коротко", "смешались две записи");
    }

    /// **ЧУЖОЙ ИДЕНТИФИКАТОР НЕ УВОДИТ ЗАПИСЬ ИЗ КАТАЛОГА.**
    ///
    /// `peer` и `message_id` приходят от мессенджера. `../` в них — это не
    /// теория: идентификатор беседы Bitrix — строка, и её форму задаёт портал.
    #[test]
    fn a_hostile_identifier_stays_inside_the_store() {
        let (_d, s) = store();
        let key = VoiceKey::new("../../etc", "../../passwd", 0);
        let path = s.path(&key);
        assert!(path.starts_with(s.root()), "запись ушла из стора: {path:?}");
        assert!(!path.to_string_lossy().contains(".."), "{path:?}");
        s.put(&key, &Transcript::new("ага", Engine::Local)).unwrap();
        assert_eq!(s.get(&key).unwrap().text, "ага");

        // Пустой идентификатор тоже даёт валидное имя, а не пустой путь.
        let empty = VoiceKey::new("", "", 3);
        assert!(s.path(&empty).starts_with(s.root()));
        assert_eq!(empty.file_name(), "_.3.json");
    }

    /// Путь стора нигде внутри не хардкодится: два стора рядом не мешают друг
    /// другу.
    #[test]
    fn the_store_root_comes_from_the_caller() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let sa = TranscriptStore::new(a.path());
        let sb = TranscriptStore::new(b.path());
        assert_eq!(sa.root(), a.path().join(STORE_DIR));
        assert_ne!(sa.root(), sb.root());

        let key = VoiceKey::new("p", "1", 0);
        sa.put(&key, &Transcript::new("в первом", Engine::Local)).unwrap();
        assert!(sb.get(&key).is_none(), "стор увидел чужой файл");
    }

    #[test]
    fn the_engine_serialises_as_a_plain_word() {
        let json = serde_json::to_string(&Transcript::new("x", Engine::Cloud)).unwrap();
        assert!(json.contains(r#""engine":"cloud""#), "{json}");
        assert_eq!(Engine::Local.as_str(), "local");
        assert_eq!(Engine::Cloud.as_str(), "cloud");
    }
}
