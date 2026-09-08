//! Снимок индекса аналитики в `~/.forge/analytics-cache.json`.
//!
//! ### Зачем
//!
//! Первый полный проход по архиву транскриптов (~1341 файл, ~2.7 ГБ) стоит
//! десятки секунд. Без снимка каждый перезапуск devforge платил бы эту цену
//! заново, и обещание «вкладка открывается мгновенно» не выполнялось бы.
//! Снимок хранит ровно то, что нужно для инкрементальности: роллап каждого
//! файла вместе с его `(file_len, file_mtime_ms, parse_offset)`.
//!
//! ### Атомарность
//!
//! Запись идёт через общий крейт [`forge_atomic`] (`temp` + `fsync` +
//! `rename` + `0600`) — тот же, которым пишутся остальные файлы `~/.forge`.
//! Своей реализации `tmp+rename` здесь нет намеренно: пятая копия этой
//! последовательности неизбежно разъехалась бы с оригиналом по правам.
//! Практический смысл — прерывание процесса посреди сохранения не оставляет
//! полуфайла: на диске либо прежний снимок, либо новый целиком.
//!
//! ### Версия схемы
//!
//! [`CACHE_VERSION`] — константа формы данных, а не «версия продукта».
//! Любое изменение полей роллапа или правил разбора обязано её поднимать:
//! иначе старый снимок, собранный по прежним правилам, тихо смешался бы с
//! новыми числами. При несовпадении версии (а также при битом или
//! недочитанном JSON) снимок молча игнорируется — это не ошибка, а сигнал
//! «нужен полный пересбор».
//!
//! ### Размер снимка
//!
//! `Samples` ядра хранит до `MAX_KEPT_SAMPLES` (20 000) точных значений на
//! день, чтобы считать точные перцентили. В снимок такие «раздутые» наборы
//! не попадают: при превышении [`CACHE_MAX_KEPT`] точные сэмплы отбрасываются
//! целиком, и перцентили после загрузки считаются по логарифмической
//! гистограмме (она сохраняется всегда и складывается без потерь). Отбрасывать
//! можно только ЦЕЛИКОМ: `Samples::quantile` переходит на гистограмму именно
//! по признаку `kept.len() != count`, и усечённый наполовину набор дал бы
//! неверный ответ вместо приближённого.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::rollup::{Samples, SessionRollup};

/// Версия схемы снимка. Несовпадение → снимок игнорируется, полный пересбор.
pub const CACHE_VERSION: u32 = 3;

/// Порог, после которого точные сэмплы распределения в снимок не пишутся.
pub const CACHE_MAX_KEPT: usize = 4_096;

/// Имя файла снимка внутри `~/.forge`.
pub const CACHE_FILE_NAME: &str = "analytics-cache.json";

/// Снимок индекса: роллапы по ключу-пути файла.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CacheSnapshot {
    pub version: u32,
    /// Момент сохранения, unix-мс — его показывает состояние `Ready`.
    #[serde(default)]
    pub updated_at_ms: i64,
    /// Ключ — абсолютный путь `.jsonl` (`SessionFile::key`).
    #[serde(default)]
    pub files: BTreeMap<String, SessionRollup>,
}

impl CacheSnapshot {
    /// Новый снимок текущей версии.
    pub fn new(files: BTreeMap<String, SessionRollup>, updated_at_ms: i64) -> Self {
        CacheSnapshot {
            version: CACHE_VERSION,
            updated_at_ms,
            files,
        }
    }
}

/// `~/.forge/analytics-cache.json`.
///
/// `None`, если `$HOME` не задан — тогда аналитика работает без снимка
/// (каждый запуск пересобирает индекс), но не падает.
pub fn default_cache_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".forge").join(CACHE_FILE_NAME))
}

/// Загружает снимок.
///
/// Возвращает `None` в любом «неудобном» случае: файла нет, он не читается,
/// JSON битый или недочитанный, версия схемы не совпала. Все эти случаи
/// означают одно и то же — полный пересбор; паники не бывает ни в одном.
pub fn load(path: &Path) -> Option<CacheSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    let snapshot: CacheSnapshot = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "analytics: снимок кэша не разобран, будет полный пересбор"
            );
            return None;
        }
    };
    if snapshot.version != CACHE_VERSION {
        tracing::info!(
            found = snapshot.version,
            expected = CACHE_VERSION,
            "analytics: версия снимка кэша не совпала, полный пересбор"
        );
        return None;
    }
    Some(snapshot)
}

/// Атомарно сохраняет снимок.
///
/// Перед сериализацией с копии срезаются раздутые наборы точных сэмплов
/// ([`trim_for_cache`]) — исходный индекс в памяти не трогается, точность
/// живого процесса не страдает.
///
/// # Errors
///
/// Ошибка сериализации или атомарной записи (создание `~/.forge`, `fsync`,
/// `rename`, смена прав).
pub fn save(path: &Path, snapshot: &CacheSnapshot) -> anyhow::Result<()> {
    let mut trimmed = snapshot.clone();
    trimmed.version = CACHE_VERSION;
    trim_for_cache(&mut trimmed.files);
    let bytes = serde_json::to_vec(&trimmed)?;
    forge_atomic::write_bytes_atomic(path, &bytes)
}

/// Отбрасывает точные сэмплы там, где их слишком много для снимка.
pub fn trim_for_cache(files: &mut BTreeMap<String, SessionRollup>) {
    for rollup in files.values_mut() {
        for day in rollup.days.values_mut() {
            trim_samples(&mut day.pauses);
            trim_samples(&mut day.msg_lens);
            trim_samples(&mut day.turn_ms);
        }
    }
}

/// Точные сэмплы либо сохраняются целиком, либо отбрасываются целиком —
/// см. раздел «Размер снимка» в документации модуля.
fn trim_samples(samples: &mut Samples) {
    if samples.kept.len() > CACHE_MAX_KEPT {
        samples.kept.clear();
        samples.kept.shrink_to_fit();
    }
}
