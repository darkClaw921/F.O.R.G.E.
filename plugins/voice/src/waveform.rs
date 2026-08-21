//! Расчёт НАСТОЯЩЕЙ звуковой огибающей голосового сообщения.
//!
//! # Зачем модуль вообще существует
//!
//! Огибающую отдаёт ровно один мессенджер из трёх. VK кладёт её в
//! `audio_message.waveform`, Telegram — в `DocumentAttributeAudio.waveform`
//! (упакованной по 5 бит на отсчёт), а Bitrix24 не отдаёт вовсе. Пока её нет,
//! фронт рисует ДЕТЕРМИНИРОВАННЫЙ ШУМ по `(message_id, index)` — картинка
//! стабильная, но к записи отношения не имеющая: одинаково выглядят «да» и
//! полутораминутный монолог.
//!
//! Считать её на бэке можно ровно потому, что оригинал файла уже лежит на
//! диске: медиа-кэш каждого плагина кладёт его при первом показе. То есть
//! расчёт ничего не скачивает и никуда не ходит — он читает файл, который уже
//! есть.
//!
//! # Три решения
//!
//! 1. **Декодирует ffmpeg, а не свой парсер.** Голосовые приходят в ogg/opus
//!    (VK, Telegram) и в mp3/m4a (Bitrix) — писать декодер под каждый кодек
//!    ради тридцати столбиков было бы абсурдно, а ffmpeg в проекте уже
//!    обязателен: без него не работает ни запись встреч, ни облачная
//!    расшифровка.
//! 2. **8 кГц моно.** Огибающая — это громкость, а не звук: частота выше
//!    восьми килогерц не добавляет ей ни одного пикселя, зато вчетверо
//!    увеличивает объём, который надо прочитать из трубы. Минута голосового в
//!    PCM 8 кГц — 960 КБ, в 48 кГц — почти 6 МБ.
//! 3. **RMS, а не пик.** Пик по бакету даёт частокол одинаковой высоты: у речи
//!    почти в каждом окне есть выброс. RMS — это средняя энергия окна, и
//!    именно она рисует узнаваемую форму фразы с паузами.
//!
//! # Диапазон значений совпадает с VK
//!
//! [`MAX_WAVEFORM`] и потолок значения выбраны не сами по себе: посчитанная
//! огибающая ложится в то же поле `MediaDto::waveform`, что и присланная VK, и
//! рисует её тот же код фронта. Разъехавшийся диапазон означал бы, что у
//! Bitrix волна визуально ниже, чем у VK, при одинаковой записи.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::VoiceError;
use crate::proc;

/// Сколько столбиков огибающей отдаём наружу.
///
/// Двести — ровно `MAX_WAVEFORM` из `plugins/vk/src/attach.rs`: фронт уже
/// умеет массив такого размера и прореживает его сам под ширину карточки.
pub const MAX_WAVEFORM: usize = 200;

/// Потолок значения одного столбика.
///
/// Байт, как у VK. Фронт всё равно нормализует массив по его собственному
/// максимуму, но одинаковый диапазон означает, что два мессенджера с одной и
/// той же записью дадут одинаковую картинку и в DTO, и в логах.
pub const WAVEFORM_MAX_VALUE: i32 = 255;

/// Сколько бит занимает один отсчёт в упаковке Telegram.
///
/// Telegram кладёт огибающую голосового в `DocumentAttributeAudio.waveform`
/// СПЛОШНЫМ битовым потоком по пять бит на отсчёт — то есть значения `0..=31`,
/// и границы отсчётов с границами байтов не совпадают.
pub const TELEGRAM_BITS_PER_SAMPLE: usize = 5;

/// Потолок значения одного отсчёта в упаковке Telegram (`2^5 - 1`).
pub const TELEGRAM_MAX_VALUE: i32 = (1 << TELEGRAM_BITS_PER_SAMPLE) - 1;

/// Частота дискретизации, в которую декодируется вход.
pub const DECODE_SAMPLE_RATE: u32 = 8_000;

/// Потолок расчёта по умолчанию.
///
/// Полминуты: декод голосового — это доли секунды, и всё, что дольше, означает
/// зависший ffmpeg, а не длинную запись.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// argv ffmpeg: декодировать что угодно в сырой PCM на stdout.
///
/// `-v error` — иначе баннер и таблица кодеков забивают stderr; `-f s16le` с
/// выходом `-` кладёт сырые сэмплы в трубу, минуя временный файл: писать на
/// диск мегабайт ради того, чтобы тут же его прочитать, незачем.
#[must_use]
pub fn build_decode_args(input: &Path, sample_rate: u32) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        input.display().to_string(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        sample_rate.max(1).to_string(),
        "-f".into(),
        "s16le".into(),
        "-".into(),
    ]
}

/// Сырые байты `s16le` в сэмплы.
#[must_use]
pub fn decode_s16le(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Считает огибающую по сэмплам. **Чистая функция.**
///
/// Возвращает не больше `buckets` значений в диапазоне
/// `0..=`[`WAVEFORM_MAX_VALUE`]. Записи короче числа бакетов дают МЕНЬШЕ
/// столбиков (по одному на сэмпл), а не массив с делением на ноль: у
/// полусекундного «ага» столбиков честно меньше, чем у минутного монолога, и
/// дорисовывать недостающие нулями значило бы показать паузу, которой не было.
///
/// Полная тишина даёт нули, а не единицы: нормализовать нечего, и деление на
/// нулевой максимум здесь исключено проверкой, а не удачей.
#[must_use]
pub fn rms_buckets(samples: &[i16], buckets: usize) -> Vec<i32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let n = buckets.min(samples.len());
    let mut raw: Vec<f64> = Vec::with_capacity(n);
    for i in 0..n {
        // Границы считаются от индекса, а не накоплением шага: накопление
        // теряет остаток, и последний бакет получался бы короче остальных на
        // длине, не кратной числу бакетов.
        let from = i * samples.len() / n;
        let to = ((i + 1) * samples.len() / n).max(from + 1).min(samples.len());
        let slice = &samples[from..to];
        let sum: f64 = slice.iter().map(|s| {
            let v = *s as f64;
            v * v
        }).sum();
        raw.push((sum / slice.len() as f64).sqrt());
    }

    let peak = raw.iter().copied().fold(0.0f64, f64::max);
    if peak <= 0.0 {
        return vec![0; n];
    }
    raw.iter()
        .map(|v| ((v / peak) * WAVEFORM_MAX_VALUE as f64).round() as i32)
        .map(|v| v.clamp(0, WAVEFORM_MAX_VALUE))
        .collect()
}

/// Распаковывает 5-битный поток Telegram в отсчёты `0..=`[`TELEGRAM_MAX_VALUE`].
///
/// # Почему это не «поделить байты»
///
/// Отсчёт занимает пять бит, а байт — восемь, поэтому границы не совпадают: с
/// третьего отсчёта каждый следующий разрезан между двумя байтами. Биты идут
/// от МЛАДШЕГО к старшему внутри байта и байты — по порядку, то есть отсчёт
/// `i` — это биты `5i..5i+5` сплошной битовой строки.
///
/// # Хвостовые биты отсчётом не становятся
///
/// Число бит редко кратно пяти: в последнем байте почти всегда остаётся
/// огрызок в один-четыре бита. Считать его отсчётом значило бы приписать записи
/// лишний столбик со случайной высотой — поэтому длина результата считается
/// ДЕЛЕНИЕМ НАЦЕЛО, а огрызок отбрасывается.
///
/// Пустой вход даёт пустой вектор: у обычного аудиофайла (не голосового)
/// атрибута нет вовсе, и это не ошибка.
#[must_use]
pub fn unpack_5bit(packed: &[u8]) -> Vec<i32> {
    let count = packed.len() * 8 / TELEGRAM_BITS_PER_SAMPLE;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let bit = i * TELEGRAM_BITS_PER_SAMPLE;
        let mut value = 0i32;
        for b in 0..TELEGRAM_BITS_PER_SAMPLE {
            let pos = bit + b;
            let byte = packed[pos / 8];
            let is_set = (byte >> (pos % 8)) & 1 == 1;
            if is_set {
                value |= 1 << b;
            }
        }
        out.push(value);
    }
    out
}

/// Приводит чужую огибающую к общему виду: не длиннее `buckets` столбиков и в
/// диапазоне `0..=`[`WAVEFORM_MAX_VALUE`].
///
/// `src_max` — потолок ИСХОДНОЙ шкалы (у Telegram это
/// [`TELEGRAM_MAX_VALUE`]). Пересчёт нужен ровно потому, что одно и то же поле
/// `MediaDto::waveform` рисует один и тот же код фронта: оставь Telegram свою
/// пятибитную шкалу, и его волна была бы визуально такой же (фронт нормализует
/// по максимуму массива), а вот в логах и в тестах два мессенджера с одинаковой
/// записью давали бы несравнимые числа.
///
/// Лишние отсчёты не выбрасываются, а УСРЕДНЯЮТСЯ по бакетам: прореживание
/// «каждый N-й» теряет как раз пики, то есть ровно ту форму, ради которой
/// огибающая и рисуется. Границы бакетов считаются от индекса — тем же
/// способом, что в [`rms_buckets`], поэтому хвост записи не пропадает.
#[must_use]
pub fn fit(values: &[i32], src_max: i32, buckets: usize) -> Vec<i32> {
    if values.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let scale = |v: f64| -> i32 {
        if src_max <= 0 {
            return 0;
        }
        ((v / f64::from(src_max)) * f64::from(WAVEFORM_MAX_VALUE))
            .round()
            .clamp(0.0, f64::from(WAVEFORM_MAX_VALUE)) as i32
    };
    if values.len() <= buckets {
        return values.iter().map(|v| scale(f64::from(*v))).collect();
    }
    (0..buckets)
        .map(|i| {
            let from = i * values.len() / buckets;
            let to = ((i + 1) * values.len() / buckets)
                .max(from + 1)
                .min(values.len());
            let slice = &values[from..to];
            let sum: f64 = slice.iter().map(|v| f64::from(*v)).sum();
            scale(sum / slice.len() as f64)
        })
        .collect()
}

/// Огибающая из атрибута Telegram, готовая к укладке в `MediaDto::waveform`.
///
/// Композиция [`unpack_5bit`] и [`fit`]: распаковать пятибитный поток и
/// привести его к общим потолкам — [`MAX_WAVEFORM`] столбиков в диапазоне
/// `0..=`[`WAVEFORM_MAX_VALUE`].
#[must_use]
pub fn from_telegram(packed: &[u8]) -> Vec<i32> {
    fit(&unpack_5bit(packed), TELEGRAM_MAX_VALUE, MAX_WAVEFORM)
}

/// Считает огибающую файла: ffmpeg → PCM → RMS по бакетам.
///
/// `ffmpeg` — путь к бинарю; модуль НЕ читает конфиг сам, потому что путь у
/// каждого плагина берётся из своего места, а у тестов — из мока.
///
/// Ошибки называются вслух и НИКОГДА не подменяются пустым массивом: пустой
/// массив на фронте неотличим от «огибающей нет», и отсутствие ffmpeg выглядело
/// бы как обычное голосовое без волны — то есть чинить было бы нечего.
pub async fn compute(
    input: &Path,
    ffmpeg: &Path,
    buckets: usize,
    timeout: Duration,
) -> Result<Vec<i32>, VoiceError> {
    let meta = std::fs::metadata(input)
        .map_err(|e| VoiceError::Audio(format!("{} не читается: {e}", input.display())))?;
    if meta.len() == 0 {
        return Err(VoiceError::Audio(format!(
            "{} пуст: огибающую считать не по чему",
            input.display()
        )));
    }

    let args = build_decode_args(input, DECODE_SAMPLE_RATE);
    let pcm = proc::capture_stdout_bytes(ffmpeg, &args, timeout).await?;
    let samples = decode_s16le(&pcm);
    if samples.is_empty() {
        return Err(VoiceError::Audio(format!(
            "{}: ffmpeg не выдал ни одного сэмпла (файл не аудио или битый)",
            input.display()
        )));
    }
    Ok(rms_buckets(&samples, buckets.min(MAX_WAVEFORM)))
}

/// [`compute`] с обычными параметрами: двести столбиков и полминуты потолка.
pub async fn compute_default(input: &Path, ffmpeg: &Path) -> Result<Vec<i32>, VoiceError> {
    compute(
        input,
        ffmpeg,
        MAX_WAVEFORM,
        Duration::from_secs(DEFAULT_TIMEOUT_SECS),
    )
    .await
}

// ---------------------------------------------------------------------------
// Кэш посчитанной огибающей.
// ---------------------------------------------------------------------------

/// Окончание имени файла с посчитанной огибающей.
///
/// # ПОЧЕМУ ОТДЕЛЬНЫЙ ФАЙЛ, А НЕ ПОЛЕ В `.meta.json`
///
/// Мету пишет `commit` медиа-кэша — РАНЬШЕ, чем появляется огибающая: расчёт
/// идёт после коммита и не имеет права задерживать выдачу файла человеку.
/// Дописать поле в мету означало бы чтение-правку-запись чужого файла из
/// фоновой задачи, то есть гонку с `get`, который в этот момент мету читает, и
/// риск потерять поля, которых фоновая задача не знает.
///
/// Отдельный сайдкар пишется ОДИН раз и целиком, его отсутствие означает ровно
/// «ещё не посчитали», а формат меты остаётся тем, чем был. Плюс дисковый: у
/// не-аудио вложений сайдкара нет вовсе, а не пустое поле в каждой мете.
pub const SIDECAR_SUFFIX: &str = ".wave.json";

/// Расширение файла байтов в медиа-кэше — общее у всех трёх плагинов.
pub const BIN_SUFFIX: &str = ".bin";

/// Версия схемы сайдкара.
///
/// Файл чужой версии считается отсутствующим и пересчитывается: огибающая —
/// кэш, а не источник истины, и читать её по неверной схеме значило бы
/// нарисовать не ту волну.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Содержимое сайдкара `<...>.wave.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveformCache {
    /// Версия схемы, [`CACHE_SCHEMA_VERSION`].
    #[serde(default)]
    pub schema: u32,
    /// Столбики `0..=`[`WAVEFORM_MAX_VALUE`], не длиннее [`MAX_WAVEFORM`].
    #[serde(default)]
    pub waveform: Vec<i32>,
    /// Unix-время расчёта (диагностика).
    #[serde(default)]
    pub computed_at: i64,
}

/// Путь сайдкара по пути файла байтов.
///
/// `<...>.bin` → `<...>.wave.json`; файл без `.bin` получает суффикс просто
/// дописыванием. Сайдкар лежит В ТОМ ЖЕ каталоге, что байты и мета: чистка
/// кэша по сообщению сносит каталог/префикс целиком и забрать его с собой
/// обязана, не зная про огибающие вовсе.
#[must_use]
pub fn sidecar_for(bin: &Path) -> PathBuf {
    let name = bin
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = name.strip_suffix(BIN_SUFFIX).unwrap_or(&name);
    bin.with_file_name(format!("{stem}{SIDECAR_SUFFIX}"))
}

/// Читает посчитанную огибающую. **Никогда не падает.**
///
/// Нет файла, битый JSON, чужая версия схемы, пустой массив — всё это
/// «огибающей нет»: фронт нарисует заглушку, а следующий расчёт перезапишет
/// файл. Отказ на чтении означал бы, что один оборванный файл навсегда лишает
/// сообщение волны.
#[must_use]
pub fn read_cached(bin: &Path) -> Option<Vec<i32>> {
    let path = sidecar_for(bin);
    let raw = std::fs::read(&path).ok()?;
    let parsed: WaveformCache = serde_json::from_slice(&raw).ok()?;
    if parsed.schema != CACHE_SCHEMA_VERSION || parsed.waveform.is_empty() {
        return None;
    }
    Some(parsed.waveform)
}

/// Есть ли уже посчитанная огибающая.
#[must_use]
pub fn is_cached(bin: &Path) -> bool {
    read_cached(bin).is_some()
}

/// Пишет огибающую в сайдкар АТОМАРНО.
///
/// # Errors
///
/// Ошибки записи (нет прав, диск переполнен). Пустая огибающая записью не
/// считается: пустой файл неотличим от «не посчитали», и записывать его значило
/// бы навсегда запомнить неудачу.
pub fn write_cached(bin: &Path, waveform: &[i32]) -> Result<(), VoiceError> {
    if waveform.is_empty() {
        return Ok(());
    }
    let record = WaveformCache {
        schema: CACHE_SCHEMA_VERSION,
        waveform: waveform.to_vec(),
        computed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|e| VoiceError::Storage(format!("огибающая не сериализуется: {e}")))?;
    let _guard = write_lock();
    forge_atomic::write_bytes_atomic(&sidecar_for(bin), &bytes)
        .map_err(|e| VoiceError::Storage(format!("{e}")))
}

/// Замок на время `tmp` → `rename` ВНУТРИ процесса.
///
/// Тот же и по той же причине, что в [`crate::store`]: `forge_atomic`
/// называет временный файл по PID, поэтому два ПОТОКА одного процесса,
/// пишущие один путь, получают одно имя `tmp` — второй падает на `rename` либо,
/// хуже, дописывает в чужой временный файл. Одновременный расчёт двух
/// вложений одного сообщения — ровно этот случай.
fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Огибающая файла из медиа-кэша: из сайдкара, а если его нет — считает и
/// кладёт сайдкар рядом.
///
/// **Идемпотентна.** Повторный вызов на том же файле ffmpeg не запускает: это
/// и есть смысл кэша, потому что повторных скачиваний одного вложения бывает
/// сколько угодно, а форма записи не меняется никогда.
///
/// # Errors
///
/// Отсутствующий ffmpeg, битый или не-аудио файл, таймаут — все с внятным
/// кодом. Вызывающий вправе их проглотить: «огибающей нет» — рабочее состояние
/// ленты, а не поломка.
pub async fn ensure(bin: &Path, ffmpeg: &Path) -> Result<Vec<i32>, VoiceError> {
    if let Some(cached) = read_cached(bin) {
        return Ok(cached);
    }
    let waveform = compute_default(bin, ffmpeg).await?;
    write_cached(bin, &waveform)?;
    Ok(waveform)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Синтетический PCM с известной формой: три громких участка и тишина
    /// между ними. Огибающая обязана повторить эту форму.
    #[test]
    fn the_buckets_follow_the_shape_of_the_signal() {
        // 4000 сэмплов: [громко][тихо][громко][тишина].
        let mut s: Vec<i16> = Vec::new();
        s.extend(std::iter::repeat(20_000i16).take(1000));
        s.extend(std::iter::repeat(2_000i16).take(1000));
        s.extend(std::iter::repeat(20_000i16).take(1000));
        s.extend(std::iter::repeat(0i16).take(1000));

        let w = rms_buckets(&s, 4);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0], WAVEFORM_MAX_VALUE, "самый громкий бакет — потолок");
        assert_eq!(w[2], WAVEFORM_MAX_VALUE);
        assert_eq!(w[3], 0, "тишина обязана быть нулём");
        // Тихий участок ровно в десять раз тише громкого — и в огибающей тоже.
        assert!((w[1] - WAVEFORM_MAX_VALUE / 10).abs() <= 1, "{w:?}");
    }

    /// **RMS, А НЕ ПИК.** Один выброс в тихом окне не имеет права поднять
    /// столбик до потолка: у речи выброс есть почти в каждом окне, и по пику
    /// вся волна выродилась бы в частокол.
    #[test]
    fn a_single_spike_does_not_lift_a_quiet_bucket() {
        let mut s: Vec<i16> = std::iter::repeat(30_000i16).take(1000).collect();
        s.extend(std::iter::repeat(0i16).take(999));
        s.push(30_000); // одинокий выброс в тишине
        let w = rms_buckets(&s, 2);
        assert_eq!(w[0], WAVEFORM_MAX_VALUE);
        assert!(w[1] < 20, "пик подменил среднюю энергию: {w:?}");
    }

    /// Запись короче числа бакетов не паникует и не делит на ноль.
    #[test]
    fn a_recording_shorter_than_the_buckets_is_not_a_panic() {
        let w = rms_buckets(&[1_000, -1_000, 500], MAX_WAVEFORM);
        assert_eq!(w.len(), 3, "по столбику на сэмпл: {w:?}");
        assert!(w.iter().all(|v| (0..=WAVEFORM_MAX_VALUE).contains(v)));

        assert!(rms_buckets(&[], 200).is_empty());
        assert!(rms_buckets(&[1, 2, 3], 0).is_empty());
        assert_eq!(rms_buckets(&[42], 200), vec![WAVEFORM_MAX_VALUE]);
    }

    /// Полная тишина — нули, а не деление на нулевой максимум.
    #[test]
    fn silence_is_zeroes_and_never_a_division_by_zero() {
        let w = rms_buckets(&vec![0i16; 8_000], 40);
        assert_eq!(w.len(), 40);
        assert!(w.iter().all(|v| *v == 0), "{w:?}");
    }

    /// Бакеты покрывают запись ЦЕЛИКОМ и не теряют хвост на некратной длине.
    #[test]
    fn the_buckets_cover_the_whole_recording() {
        // 1001 сэмпл на 10 бакетов: последний обязан дотянуться до конца.
        let mut s: Vec<i16> = vec![0; 1000];
        s.push(30_000);
        let w = rms_buckets(&s, 10);
        assert_eq!(w.len(), 10);
        assert_eq!(w[9], WAVEFORM_MAX_VALUE, "хвост записи потерян: {w:?}");
        assert!(w[..9].iter().all(|v| *v == 0));
    }

    /// Результат ВСЕГДА укладывается в потолок и в диапазон VK.
    #[test]
    fn the_result_always_fits_the_vk_range() {
        let s: Vec<i16> = (0..50_000).map(|i| ((i * 37) % 65_535) as i16).collect();
        let w = rms_buckets(&s, MAX_WAVEFORM);
        assert!(w.len() <= MAX_WAVEFORM);
        assert!(w.iter().all(|v| (0..=WAVEFORM_MAX_VALUE).contains(v)), "{w:?}");
    }

    /// Эталон упаковки Telegram: пять бит на отсчёт, младшими битами вперёд.
    ///
    /// `0b11111_00001_00000` в битовой строке — это отсчёты `[0, 1, 31]`, а в
    /// байтах (младшие биты первыми) — `0x20, 0x7c`.
    #[test]
    fn the_five_bit_stream_unpacks_to_the_reference_samples() {
        assert_eq!(unpack_5bit(&[0x20, 0x7c]), vec![0, 1, 31]);

        // Все единицы: восемь байт — двенадцать отсчётов по потолку.
        let all = unpack_5bit(&[0xff; 5]);
        assert_eq!(all.len(), 8, "40 бит — ровно восемь отсчётов: {all:?}");
        assert!(all.iter().all(|v| *v == TELEGRAM_MAX_VALUE), "{all:?}");

        // Значения НИКОГДА не выходят за пять бит, чем бы ни был вход.
        let noisy: Vec<u8> = (0u8..=255).collect();
        assert!(unpack_5bit(&noisy)
            .iter()
            .all(|v| (0..=TELEGRAM_MAX_VALUE).contains(v)));
    }

    /// **ХВОСТОВЫЕ БИТЫ НЕ ДАЮТ ЛИШНЕГО ОТСЧЁТА.**
    ///
    /// Один байт — это восемь бит, то есть один отсчёт и огрызок в три бита.
    /// Огрызок обязан быть отброшен, иначе у каждой записи появлялся бы
    /// хвостовой столбик случайной высоты.
    #[test]
    fn the_trailing_bits_never_become_a_sample() {
        assert_eq!(unpack_5bit(&[0xff]).len(), 1, "8 бит — один отсчёт");
        assert_eq!(unpack_5bit(&[0xff, 0xff]).len(), 3, "16 бит — три отсчёта");
        assert_eq!(unpack_5bit(&[0xff; 3]).len(), 4, "24 бита — четыре отсчёта");
        // Меньше пяти бит — отсчётов нет вовсе, а не паника на пустом срезе.
        assert!(unpack_5bit(&[]).is_empty());
    }

    /// Пустой атрибут (обычный аудиофайл, а не голосовое) — пустой вектор.
    #[test]
    fn an_absent_attribute_is_an_empty_waveform_not_a_panic() {
        assert!(from_telegram(&[]).is_empty());
        assert!(fit(&[], TELEGRAM_MAX_VALUE, MAX_WAVEFORM).is_empty());
        assert!(fit(&[1, 2, 3], TELEGRAM_MAX_VALUE, 0).is_empty());
        // Испорченный потолок шкалы не делит на ноль.
        assert_eq!(fit(&[7, 7], 0, 4), vec![0, 0]);
    }

    /// Пятибитная шкала переезжает в ту же шкалу, что у VK и у расчёта на бэке.
    #[test]
    fn the_telegram_scale_becomes_the_shared_one() {
        // 31 — потолок Telegram, значит и потолок общей шкалы.
        assert_eq!(fit(&[0, 31], TELEGRAM_MAX_VALUE, 4), vec![0, WAVEFORM_MAX_VALUE]);
        // Середина остаётся серединой.
        let mid = fit(&[16], TELEGRAM_MAX_VALUE, 4)[0];
        assert!((mid - WAVEFORM_MAX_VALUE / 2).abs() <= 6, "{mid}");
        assert!(from_telegram(&[0xff; 32])
            .iter()
            .all(|v| (0..=WAVEFORM_MAX_VALUE).contains(v)));
    }

    /// **ДЛИНА ВСЕГДА ≤ 200 — И ПИКИ ПРИ ЭТОМ НЕ ТЕРЯЮТСЯ.**
    ///
    /// Прореживание «каждый N-й» выбросило бы как раз выбросы; усреднение по
    /// бакетам оставляет форму фразы.
    #[test]
    fn a_long_waveform_is_averaged_down_to_the_cap() {
        // 800 отсчётов: 400 нулей, потом 400 потолков.
        let mut src: Vec<i32> = vec![0; 400];
        src.extend(std::iter::repeat(TELEGRAM_MAX_VALUE).take(400));
        let w = fit(&src, TELEGRAM_MAX_VALUE, MAX_WAVEFORM);
        assert_eq!(w.len(), MAX_WAVEFORM);
        assert!(w[..100].iter().all(|v| *v == 0), "{w:?}");
        assert!(w[100..].iter().all(|v| *v == WAVEFORM_MAX_VALUE), "{w:?}");

        // Одинокий пик в конце длинной записи доживает до результата.
        let mut spiky: Vec<i32> = vec![0; 4_000];
        *spiky.last_mut().unwrap() = TELEGRAM_MAX_VALUE;
        let w = fit(&spiky, TELEGRAM_MAX_VALUE, MAX_WAVEFORM);
        assert_eq!(w.len(), MAX_WAVEFORM);
        assert!(*w.last().unwrap() > 0, "хвост записи потерян: {w:?}");

        // Реальная упаковка любой длины тоже укладывается в потолок.
        assert!(from_telegram(&vec![0xa5; 1_000]).len() <= MAX_WAVEFORM);
        // Короткая запись отдаётся как есть — фронт растянет её сам.
        assert_eq!(from_telegram(&[0x20, 0x7c]).len(), 3);
    }

    #[test]
    fn the_decode_args_ask_for_raw_mono_pcm_on_stdout() {
        let args = build_decode_args(Path::new("/tmp/voice.ogg"), DECODE_SAMPLE_RATE);
        let joined = args.join(" ");
        assert!(joined.contains("-ac 1"), "{joined}");
        assert!(joined.contains("-ar 8000"), "{joined}");
        assert!(joined.contains("-f s16le"), "{joined}");
        assert!(joined.ends_with(" -"), "вывод обязан идти в трубу: {joined}");
        // Ноль в частоте не имеет права доехать до ffmpeg.
        assert!(build_decode_args(Path::new("/tmp/a"), 0).contains(&"1".to_string()));
    }

    #[test]
    fn raw_bytes_become_samples() {
        let bytes = [0x00, 0x01, 0xff, 0xff, 0x2a];
        // Нечётный хвост отбрасывается: половина сэмпла — не сэмпл.
        assert_eq!(decode_s16le(&bytes), vec![256i16, -1]);
        assert!(decode_s16le(&[]).is_empty());
    }

    #[tokio::test]
    async fn an_empty_file_is_a_named_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("empty.ogg");
        std::fs::write(&f, b"").unwrap();
        let err = compute_default(&f, Path::new("/bin/echo")).await.unwrap_err();
        assert_eq!(err.code(), "voice_audio", "{err}");
        assert!(err.to_string().contains("пуст"), "{err}");

        // Файла нет вовсе — тоже ошибка с именем файла, а не паника.
        let err = compute_default(&dir.path().join("нет"), Path::new("/bin/echo"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "voice_audio", "{err}");
    }

    /// **ОТСУТСТВИЕ FFMPEG — ВНЯТНАЯ ОШИБКА, А НЕ МОЛЧАЛИВЫЙ ПУСТОЙ МАССИВ.**
    ///
    /// Пустой массив на фронте неотличим от «огибающей нет», и ненайденный
    /// бинарь выглядел бы обычным голосовым без волны.
    #[tokio::test]
    async fn a_missing_ffmpeg_is_a_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("voice.ogg");
        std::fs::write(&f, b"OggS____").unwrap();
        let err = compute_default(&f, &dir.path().join("нет-такого-ffmpeg"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "spawn_failed", "{err}");
    }

    /// Живой прогон на моке: ffmpeg отдаёт PCM в трубу, огибающая считается.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_run_reads_the_pcm_from_the_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("voice.ogg");
        std::fs::write(&f, b"OggS____").unwrap();

        // Мок печатает 400 сэмплов: 200 громких и 200 тихих. `printf` с
        // восьмеричным экранированием — самый переносимый способ выдать
        // двоичные данные из shell.
        let bin = dir.path().join("ffmpeg-mock");
        std::fs::write(
            &bin,
            "#!/bin/sh\n\
             i=0; while [ $i -lt 200 ]; do printf '\\000\\120'; i=$((i+1)); done\n\
             i=0; while [ $i -lt 200 ]; do printf '\\000\\010'; i=$((i+1)); done\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let w = compute(&f, &bin, 4, Duration::from_secs(10)).await.unwrap();
        assert_eq!(w.len(), 4);
        assert_eq!(w[0], WAVEFORM_MAX_VALUE, "{w:?}");
        assert_eq!(w[1], WAVEFORM_MAX_VALUE, "{w:?}");
        assert!(w[2] < 100 && w[2] > 0, "тихая половина: {w:?}");
        assert_eq!(w[2], w[3], "{w:?}");
    }

    /// Сайдкар лежит РЯДОМ с байтами и называется по их имени.
    #[test]
    fn the_sidecar_sits_next_to_the_bytes() {
        assert_eq!(
            sidecar_for(Path::new("/c/media/chat27/7001.1.file.bin")),
            PathBuf::from("/c/media/chat27/7001.1.file.wave.json")
        );
        // Имя без `.bin` (телеграмный `thumb`, чужой кэш) не теряет суффикса.
        assert_eq!(
            sidecar_for(Path::new("/c/media/5/12.full")),
            PathBuf::from("/c/media/5/12.full.wave.json")
        );
        // Ключ сайдкара строго следует за ключом вложения: два индекса одного
        // сообщения дают два разных файла.
        assert_ne!(
            sidecar_for(Path::new("/c/7001.0.file.bin")),
            sidecar_for(Path::new("/c/7001.1.file.bin"))
        );
    }

    /// Круг «записали — прочитали», и ни один испорченный файл не роняет чтение.
    #[test]
    fn a_broken_sidecar_reads_as_no_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("7001.0.file.bin");
        assert!(read_cached(&bin).is_none(), "пустой каталог — огибающей нет");
        assert!(!is_cached(&bin));

        write_cached(&bin, &[1, 200, 255]).unwrap();
        assert_eq!(read_cached(&bin), Some(vec![1, 200, 255]));
        assert!(is_cached(&bin));

        // Пустая огибающая не записывается: пустой файл неотличим от «не
        // считали», и запомнить неудачу навсегда было бы хуже пересчёта.
        let empty = dir.path().join("нет.bin");
        write_cached(&empty, &[]).unwrap();
        assert!(!sidecar_for(&empty).exists());

        // Битый JSON и чужая схема — «огибающей нет», а не паника.
        std::fs::write(sidecar_for(&bin), b"{ truncated").unwrap();
        assert!(read_cached(&bin).is_none());
        std::fs::write(
            sidecar_for(&bin),
            br#"{"schema":99,"waveform":[1,2],"computed_at":0}"#,
        )
        .unwrap();
        assert!(read_cached(&bin).is_none());
        // Пустой массив в файле тоже считается отсутствием.
        std::fs::write(
            sidecar_for(&bin),
            br#"{"schema":1,"waveform":[],"computed_at":0}"#,
        )
        .unwrap();
        assert!(read_cached(&bin).is_none());
    }

    /// **ПОВТОРНЫЙ ВЫЗОВ НЕ ЗАПУСКАЕТ FFMPEG.** Кэш ради этого и существует.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_ensure_never_runs_ffmpeg_again() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("7001.0.file.bin");
        std::fs::write(&bin, b"OggS____").unwrap();

        // Мок отмечает каждый свой запуск строкой в журнале.
        let log = dir.path().join("runs.log");
        let ffmpeg = dir.path().join("ffmpeg-counting");
        std::fs::write(
            &ffmpeg,
            format!(
                "#!/bin/sh\necho run >> {}\ni=0; while [ $i -lt 200 ]; do printf '\\000\\120'; i=$((i+1)); done\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let first = ensure(&bin, &ffmpeg).await.unwrap();
        assert!(!first.is_empty());
        let second = ensure(&bin, &ffmpeg).await.unwrap();
        assert_eq!(first, second, "вторая огибающая обязана быть той же");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            1,
            "ffmpeg запущен повторно — кэш не сработал"
        );
    }

    /// Одновременный расчёт двух вложений не оставляет половины файла.
    #[tokio::test]
    async fn two_concurrent_writes_never_leave_half_a_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("7001.0.file.bin");
        let b = dir.path().join("7001.1.file.bin");
        let big: Vec<i32> = (0..MAX_WAVEFORM).map(|i| (i % 256) as i32).collect();

        let mut tasks = Vec::new();
        for _ in 0..8 {
            for path in [a.clone(), b.clone()] {
                let w = big.clone();
                tasks.push(tokio::task::spawn_blocking(move || {
                    write_cached(&path, &w).unwrap();
                }));
            }
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(read_cached(&a).as_deref(), Some(big.as_slice()));
        assert_eq!(read_cached(&b).as_deref(), Some(big.as_slice()));
        // Ни одного брошенного временного файла в каталоге.
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(left.is_empty(), "остались временные файлы: {left:?}");
    }

    /// ЖИВАЯ ПРОБА на НАСТОЯЩЕМ ffmpeg и настоящем ogg/opus.
    ///
    /// Моки выше доказывают арифметику, но не доказывают, что ffmpeg вообще
    /// открывает контейнер голосового: у opus своя дорожка, и `-f s16le` на
    /// нём мог бы упереться во что угодно. Здесь файл СОЗДАЁТСЯ тем же
    /// ffmpeg'ом (полторы секунды тона и полсекунды тишины), а потом читается
    /// боевым путём — форма обязана повторить вход.
    ///
    /// `#[ignore]`, потому что требует ffmpeg в PATH:
    /// `cargo test -p forge-voice -- --ignored the_real_ffmpeg`.
    #[ignore = "требует настоящий ffmpeg в PATH"]
    #[tokio::test]
    async fn the_real_ffmpeg_reads_a_real_opus_voice_message() {
        let ffmpeg = crate::tools::resolve_ffmpeg(None).expect("ffmpeg не найден");
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("voice.ogg");

        // Полторы секунды тона плюс полсекунды тишины — ровно тот контейнер,
        // в котором приходят голосовые VK и Telegram.
        let out = std::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1.5",
                "-f",
                "lavfi",
                "-t",
                "0.5",
                "-i",
                "anullsrc=r=48000:cl=mono",
                "-filter_complex",
                "[0:a][1:a]concat=n=2:v=0:a=1",
                "-c:a",
                "libopus",
                "-y",
            ])
            .arg(&voice)
            .output()
            .expect("ffmpeg не запустился");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let w = compute(&voice, &ffmpeg, 40, Duration::from_secs(30))
            .await
            .expect("огибающая не посчиталась");
        assert_eq!(w.len(), 40, "{w:?}");
        assert!(w.iter().all(|v| (0..=WAVEFORM_MAX_VALUE).contains(v)), "{w:?}");
        // Первые три четверти — звук, последняя — тишина.
        let loud: i32 = w[..25].iter().sum::<i32>() / 25;
        let quiet: i32 = w[32..].iter().sum::<i32>() / 8;
        assert!(loud > 100, "звучащая часть вышла тихой: {w:?}");
        assert!(quiet < loud / 4, "тишина не отличилась от звука: {w:?}");

        // Кэш поверх боевого пути: сайдкар пишется и читается.
        let cached = ensure(&voice, &ffmpeg).await.unwrap();
        assert!(!cached.is_empty());
        assert_eq!(read_cached(&voice).unwrap(), cached);
    }

    /// ffmpeg отработал, но звука не нашёл — это ошибка, а не пустая волна.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_without_audio_is_a_named_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("photo.jpg");
        std::fs::write(&f, b"\xff\xd8\xff").unwrap();
        let bin = dir.path().join("ffmpeg-silent");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = compute(&f, &bin, 40, Duration::from_secs(10)).await.unwrap_err();
        assert_eq!(err.code(), "voice_audio", "{err}");
        assert!(err.to_string().contains("сэмпл"), "{err}");
    }
}
