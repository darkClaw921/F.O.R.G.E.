//! Запуск whisper.cpp с ГАРАНТИЕЙ, что расчёт идёт на GPU.
//!
//! Модуль НЕ импортирует `axum` и `http` и не знает ни про один плагин: всё,
//! что раньше приходило из `MeetConfig`/`DepsReport`, теперь приезжает явными
//! полями [`WhisperRequest`], [`WhisperFlags`] и [`RunOptions`]. Тонкий слой,
//! собирающий их из конфига, остаётся у плагина.
//!
//! # Правило «локальные модели только на GPU» исполняется СТРУКТУРНО
//!
//! Два независимых механизма, и ни один из них не комментарий:
//!
//! 1. **`-ng` и `--no-gpu` НИКОГДА не попадают в argv.** [`build_args`] —
//!    чистая функция, и на неё есть тест, перебирающий комбинации настроек:
//!    ни при какой из них запрещённый флаг не появляется. Приём взят из теста
//!    приватности в `claude-runner`. Аргументы, пришедшие СНАРУЖИ (из конфига
//!    пользователя), проходят через [`sanitize_extra_args`], который такой флаг
//!    выбрасывает и называет вслух.
//! 2. **stderr прогона парсится на [`METAL_NEEDLE`].** Не нашли — процесс
//!    УБИВАЕТСЯ с `metal_unavailable`, а не досчитывает молча на процессоре.
//!
//! Цена нарушения измерена: те же двадцать секунд аудио считаются 14 секунд на
//! GPU и 0,7× реального времени на CPU. Разрыв по энкоду пятнадцатикратный,
//! час аудио на CPU — почти полтора часа расчёта.
//!
//! # ПОЧЕМУ ГАРД СМОТРИТ НА ТОЧНЫЙ ПРЕФИКС, А НЕ НА ПОДСТРОКУ
//!
//! Живая проба сравнила три прогона:
//!
//! | строка | GPU | `-ng` | `--help` |
//! |---|---|---|---|
//! | `ggml_metal_device_init:` | есть | **есть** | **есть** |
//! | `ggml_metal_library_init:` | есть | **есть** | **есть** |
//! | `ggml_metal_rsets_init:` | есть | **есть** | **есть** |
//! | **`ggml_metal_init:`** | **есть** | нет | нет |
//!
//! То есть ggml грузит Metal-бэкенд и печатает про устройство ВСЕГДА, даже
//! когда считать на нём не собирается. Гард вида
//! `stderr.contains("ggml_metal")` пропустил бы CPU-прогон — то есть молча
//! нарушил бы правило проекта тем единственным способом, который невозможно
//! заметить по результату: транскрипт получился бы точно такой же, только
//! через полтора часа.
//!
//! **Поле `systeminfo` из JSON для гарда не годится тоже**: у GPU- и
//! CPU-прогона оно побайтово одинаково (проверено сравнением файлов) — это
//! флаги сборки, а не использованный бэкенд.
//!
//! # `--vad` без `-vm` — не ошибка, а SIGABRT
//!
//! `whisper-cli --vad` без модели VAD завершается кодом 134 со стеком
//! `ggml_abort`. Поэтому «не собирать `--vad` без `-vm`» — инвариант
//! [`build_args`], а не проверка в рантайме: рантайму тут уже нечего
//! проверять, процесс умер.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::proc::{self, LineVerdict, ProcError};

/// Опорная строка Metal-гарда. **Точный префикс строки stderr.**
pub const METAL_NEEDLE: &str = "ggml_metal_init:";

/// Строки, которыми whisper прямо объявляет расчёт на процессоре.
///
/// Существуют ради скорости реакции: гард и без них снял бы прогон по
/// истечении окна ожидания, но эти строки печатаются сразу, и ждать после них
/// нечего.
pub const CPU_MARKERS: [&str; 2] = [
    "whisper_backend_init_gpu: no GPU found",
    "whisper_backend_init: using BLAS backend",
];

/// Флаги, которых не должно быть в argv НИКОГДА.
pub const FORBIDDEN_ARGS: [&str; 2] = ["-ng", "--no-gpu"];

/// Потолок прогона по умолчанию, секунды. Час: столько идёт расшифровка
/// часовой записи на GPU в худшем случае.
pub const DEFAULT_TIMEOUT_SECS: u64 = 3600;

/// Окно ожидания доказательства GPU по умолчанию, секунды.
///
/// Минута, а не пара секунд: на холодной машине перед `ggml_metal_init:`
/// проходит `ggml_metal_library_init`, который в первый раз занимает 12,5
/// секунды.
pub const DEFAULT_METAL_GRACE_SECS: u64 = 60;

/// Подтверждает ли вывод, что расчёт пошёл на GPU.
///
/// Требует строку, НАЧИНАЮЩУЮСЯ с [`METAL_NEEDLE`]: `contains` поймал бы
/// `whisper: ggml_metal_init: …` в чужом сообщении, а `contains("ggml_metal")`
/// — всегдашние строки инициализации устройства.
pub fn metal_confirmed(stderr: &str) -> bool {
    stderr.lines().any(|l| l.trim_start().starts_with(METAL_NEEDLE))
}

/// Объявляет ли строка расчёт на процессоре.
pub fn cpu_declared(line: &str) -> bool {
    CPU_MARKERS.iter().any(|m| line.contains(m))
}

/// Флаги whisper.cpp, ФАКТИЧЕСКИ поддержанные установленной версией.
///
/// Проверяются программно разбором `--help`, а не гаданием по номеру версии:
/// формула brew, сборка из исходников и старый бинарь в `~/bin` дают три
/// разных набора, и «версия 1.9.2» ничего о них не говорит.
///
/// `--help` печатается в **stderr** — парсер обязан читать именно его.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhisperFlags {
    /// `-p N` — число процессоров. Всегда передаём `1`: параллелизм ломает
    /// таймкоды на границах чанков, а таймкоды здесь несут имена спикеров.
    pub processors: bool,
    /// `-bs N` — ширина луча.
    pub beam_size: bool,
    /// `-fa`. В 1.9.2 УЖЕ дефолт `true`, обратный флаг `-nfa`, поэтому в argv
    /// не попадает — но найденный в help ошибкой не считается.
    pub flash_attn: bool,
    /// `-ojf` — потокенные метки. Без них резать сегмент по границе смены
    /// спикера нечем.
    pub output_json_full: bool,
    /// `-pp` — прогресс в stderr.
    pub print_progress: bool,
    /// `-of` — путь вывода без расширения.
    pub output_file: bool,
    /// `--vad`. **Без `-vm` роняет процесс по SIGABRT** (exit 134), поэтому
    /// одного этого флага мало — нужна ещё модель VAD.
    pub vad: bool,
    /// `-vm` — модель VAD.
    pub vad_model: bool,
}

impl Default for WhisperFlags {
    /// Базовый набор: то, что есть в любой сборке whisper.cpp начиная с 1.5.
    ///
    /// VAD по умолчанию ВЫКЛЮЧЕН, и это не осторожность ради осторожности:
    /// `--vad` без `-vm` не ругается, а падает по SIGABRT.
    fn default() -> Self {
        Self {
            processors: true,
            beam_size: true,
            flash_attn: false,
            output_json_full: true,
            print_progress: true,
            output_file: true,
            vad: false,
            vad_model: false,
        }
    }
}

impl WhisperFlags {
    /// Разбирает вывод `whisper-cli --help`.
    ///
    /// Ищем длинные имена (`--output-json-full`), а не короткие: короткое
    /// `-p` встречается внутри других строк справки, а длинное — нет.
    pub fn parse_help(help: &str) -> Self {
        let has = |needle: &str| help.contains(needle);
        Self {
            processors: has("--processors"),
            beam_size: has("--beam-size"),
            flash_attn: has("--flash-attn"),
            output_json_full: has("--output-json-full"),
            print_progress: has("--print-progress"),
            output_file: has("--output-file"),
            vad: has("--vad"),
            vad_model: has("--vad-model"),
        }
    }
}

/// Что запускать.
#[derive(Debug, Clone)]
pub struct WhisperRequest {
    /// Путь к бинарю.
    pub bin: PathBuf,
    /// Файл модели.
    pub model: PathBuf,
    /// Вход: WAV 16 кГц моно.
    pub wav: PathBuf,
    /// Префикс вывода БЕЗ расширения — `-of` дописывает его сам.
    pub out_prefix: PathBuf,
    pub language: String,
    pub beam_size: u32,
    /// Модель VAD. `None` — `--vad` не собирается вовсе.
    pub vad_model: Option<PathBuf>,
}

impl WhisperRequest {
    /// Файл, который whisper положит при `-ojf`.
    pub fn json_path(&self) -> PathBuf {
        let mut p = self.out_prefix.clone().into_os_string();
        p.push(".json");
        PathBuf::from(p)
    }
}

/// Собирает argv. **Чистая функция — ядро инвариантов модуля.**
///
/// Что и почему:
///
/// | флаг | зачем |
/// |---|---|
/// | `-m`, `-f`, `-of` | модель, вход, префикс вывода |
/// | `-l` | язык |
/// | `-p 1` | НЕ ПАРАЛЛЕЛИТЬ: параллелизм ломает таймкоды на границах чанков, а таймкоды здесь несут имена спикеров |
/// | `-bs 5` | ширина луча |
/// | `-ojf` | ПОТОКЕННЫЕ метки. Посегментные по 5–30 с регулярно перекрывают смену спикера; потокенные позволяют разрезать сегмент ровно на границе |
/// | `-pp` | прогресс в stderr |
///
/// `-fa` НЕ передаётся: в 1.9.2 он уже дефолт `true`, а обратный флаг —
/// `-nfa`. Найденный пробой `-fa` при этом ошибкой не считается, просто не
/// используется.
pub fn build_args(req: &WhisperRequest, flags: &WhisperFlags) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".into(),
        req.model.display().to_string(),
        "-f".into(),
        req.wav.display().to_string(),
    ];

    if !req.language.trim().is_empty() {
        args.push("-l".into());
        args.push(req.language.trim().to_string());
    }
    if flags.processors {
        // Ровно единица. Значение совпадает с дефолтом whisper, но дефолт —
        // не контракт: в чужой сборке он может быть другим, а таймкоды здесь
        // несут имена спикеров.
        args.push("-p".into());
        args.push("1".into());
    }
    if flags.beam_size {
        args.push("-bs".into());
        args.push(req.beam_size.max(1).to_string());
    }
    if flags.output_json_full {
        args.push("-ojf".into());
    }
    if flags.print_progress {
        args.push("-pp".into());
    }

    // VAD — ТОЛЬКО парой. Один `--vad` роняет процесс по SIGABRT, и никакая
    // проверка в рантайме этого не поймает: процесса уже нет.
    if let Some(vad) = &req.vad_model {
        if flags.vad && flags.vad_model {
            args.push("--vad".into());
            args.push("-vm".into());
            args.push(vad.display().to_string());
        }
    }

    if flags.output_file {
        args.push("-of".into());
        args.push(req.out_prefix.display().to_string());
    }
    args
}

/// Просеивает аргументы, пришедшие СНАРУЖИ (из конфига пользователя).
///
/// [`build_args`] не может собрать `-ng` по построению — но дополнительные
/// аргументы приходят строкой из файла настроек, и там написать можно что
/// угодно. Запрещённый флаг здесь ВЫБРАСЫВАЕТСЯ и называется вслух в журнале:
/// молча проглотить его значило бы вернуть ровно ту дыру, ради закрытия которой
/// существует [`FORBIDDEN_ARGS`], а отказать целиком — сломать запуск из-за
/// одной лишней строки в конфиге.
///
/// Выбрасывается не только точное совпадение: `-ngl`, `-ng=1` и любой другой
/// аргумент, начинающийся с запрещённого флага, тоже не проходит.
#[must_use]
pub fn sanitize_extra_args(extra: &[String]) -> Vec<String> {
    extra
        .iter()
        .filter(|a| {
            let bad = FORBIDDEN_ARGS
                .iter()
                .any(|f| a.as_str() == *f || a.starts_with(f));
            if bad {
                tracing::warn!(
                    target: "forge_voice",
                    arg = %a,
                    "forge-voice: запрещённый флаг отброшен — локальные модели \
                     считаются только на GPU"
                );
            }
            !bad
        })
        .cloned()
        .collect()
}

/// Настройки прогона.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Потолок всего прогона.
    pub timeout: Duration,
    /// Сколько ждать доказательства GPU, прежде чем убить процесс.
    ///
    /// Не совпадает с таймаутом всего прогона: расшифровка часа аудио идёт
    /// минуты, а `ggml_metal_init:` печатается в первые секунды — правда, на
    /// холодной машине перед ней успевает пройти `ggml_metal_library_init`,
    /// который в первый раз занимает 12,5 секунды.
    pub metal_grace: Duration,
    pub cancel: CancellationToken,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            metal_grace: Duration::from_secs(DEFAULT_METAL_GRACE_SECS),
            cancel: CancellationToken::new(),
        }
    }
}

/// Токен вывода whisper с потокенной меткой.
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperToken {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub p: f64,
}

/// Сегмент вывода whisper.
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// Потокенные (subword) метки — «Кол», «лег», «и». Склейка в слова —
    /// работа вызывающего.
    pub tokens: Vec<WhisperToken>,
}

/// Итог прогона.
#[derive(Debug, Clone)]
pub struct WhisperOutcome {
    pub segments: Vec<WhisperSegment>,
    /// Хвост stderr — уезжает в журнал даже при успехе.
    pub stderr_tail: String,
    /// Подтверждён ли Metal. Всегда `true`: иначе прогон не дошёл бы сюда.
    pub metal: bool,
}

/// Служебные токены whisper: `[_BEG_]`, `[_TT_123]` и подобные.
///
/// Публична потому, что склейка токенов в слова живёт у вызывающего, а
/// служебные токены в ней участвовать не имеют права.
pub fn is_service_token(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("[_") && t.ends_with(']')
}

/// Разбирает `<output>.json`, оставленный `-ojf`.
pub fn parse_output_json(raw: &str) -> Result<Vec<WhisperSegment>, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("вывод whisper не разбирается: {e}"))?;
    let items = v
        .get("transcription")
        .and_then(|t| t.as_array())
        .ok_or_else(|| "в выводе whisper нет массива transcription".to_string())?;

    let ms = |node: &serde_json::Value, key: &str| -> i64 {
        node.get("offsets")
            .and_then(|o| o.get(key))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    };

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let text = item
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let tokens: Vec<WhisperToken> = item
            .get("tokens")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        let text = t.get("text").and_then(serde_json::Value::as_str)?;
                        // Служебные токены в массиве лежат с нулевой
                        // длительностью и словами не являются.
                        if is_service_token(text) {
                            return None;
                        }
                        Some(WhisperToken {
                            text: text.to_string(),
                            start_ms: ms(t, "from"),
                            end_ms: ms(t, "to"),
                            p: t.get("p").and_then(serde_json::Value::as_f64).unwrap_or(1.0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Пустые сегменты whisper выдаёт на тишине; в транскрипте им не место.
        if text.is_empty() && tokens.is_empty() {
            continue;
        }
        out.push(WhisperSegment {
            start_ms: ms(item, "from"),
            end_ms: ms(item, "to"),
            text,
            tokens,
        });
    }
    Ok(out)
}

/// Разбирает строку прогресса `-pp`: `whisper_print_progress_callback:
/// progress =  40%`.
pub fn parse_progress(line: &str) -> Option<u8> {
    let rest = line.split("progress =").nth(1)?;
    let digits: String = rest
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u8>().ok().map(|p| p.min(100))
}

/// Запускает whisper и возвращает разобранный вывод.
///
/// Гард встроен в чтение stderr: как только whisper объявляет расчёт на
/// процессоре — или как только истекает окно ожидания без `ggml_metal_init:`
/// — процесс убивается, и функция отвечает `metal_unavailable`.
pub async fn run(
    req: &WhisperRequest,
    flags: &WhisperFlags,
    opts: &RunOptions,
    on_progress: Option<&(dyn Fn(u8) + Send + Sync)>,
) -> Result<WhisperOutcome, ProcError> {
    let args = build_args(req, flags);
    debug_assert!(
        !args.iter().any(|a| FORBIDDEN_ARGS.contains(&a.as_str())),
        "в argv whisper попал запрещённый флаг: {args:?}"
    );

    // Старый вывод убираем: `-ojf` пишет файл, и остаток прошлого прогона
    // выглядел бы результатом текущего, если тот упадёт до записи.
    let json_path = req.json_path();
    let _ = std::fs::remove_file(&json_path);
    if let Some(parent) = json_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let metal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher_metal = metal.clone();
    let watch = move |line: &str| -> LineVerdict {
        if line.trim_start().starts_with(METAL_NEEDLE) {
            watcher_metal.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(p) = parse_progress(line) {
            if let Some(cb) = on_progress {
                cb(p);
            }
        }
        if cpu_declared(line) && !watcher_metal.load(std::sync::atomic::Ordering::SeqCst) {
            return LineVerdict::Kill(
                "metal_unavailable",
                format!(
                    "whisper объявил расчёт на процессоре ({}), прогон снят: \
                     локальные модели считаются только на GPU",
                    line.trim()
                ),
            );
        }
        LineVerdict::Continue
    };

    // Окно ожидания живёт отдельной задачей: она гасит прогон, если
    // `ggml_metal_init:` так и не появилась — например, у сборки, которая
    // вообще ничего про Metal не печатает.
    let grace_cancel = opts.cancel.child_token();
    let watchdog_token = grace_cancel.clone();
    let watchdog_metal = metal.clone();
    let grace = opts.metal_grace;
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog_flag = timed_out.clone();
    let watchdog = tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        if !watchdog_metal.load(std::sync::atomic::Ordering::SeqCst) {
            watchdog_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            watchdog_token.cancel();
        }
    });

    let result = proc::run_watched(&req.bin, &args, opts.timeout, &grace_cancel, watch).await;
    watchdog.abort();

    let metal_seen = metal.load(std::sync::atomic::Ordering::SeqCst);
    let stderr_tail = match &result {
        Ok(t) => t.clone(),
        Err(e) => e.stderr_tail.clone(),
    };

    if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ProcError::new(
            "metal_unavailable",
            format!(
                "whisper не подтвердил Metal за {grace:?} (нет строки {METAL_NEEDLE}), \
                 прогон снят: считать на процессоре запрещено правилом проекта"
            ),
            stderr_tail,
        ));
    }
    result?;

    // Процесс мог закончиться сам, ни разу не сказав про Metal, — быстрый
    // CPU-прогон или чужая сборка. Транскрипт при этом даже есть, и именно
    // поэтому проверка обязана стоять ЗДЕСЬ, а не только по таймеру.
    if !metal_seen {
        let _ = std::fs::remove_file(&json_path);
        return Err(ProcError::new(
            "metal_unavailable",
            format!(
                "whisper закончил без строки {METAL_NEEDLE}: расчёт шёл не на GPU, \
                 результат отброшен"
            ),
            stderr_tail,
        ));
    }

    let raw = std::fs::read_to_string(&json_path).map_err(|e| {
        ProcError::new(
            "no_output",
            format!("whisper не оставил {}: {e}", json_path.display()),
            stderr_tail.clone(),
        )
    })?;
    let segments = parse_output_json(&raw)
        .map_err(|e| ProcError::new("bad_output", e, stderr_tail.clone()))?;

    Ok(WhisperOutcome {
        segments,
        stderr_tail,
        metal: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn req(dir: &Path) -> WhisperRequest {
        WhisperRequest {
            bin: dir.join("whisper-cli"),
            model: dir.join("ggml-large-v3-turbo-q5_0.bin"),
            wav: dir.join("mix.wav"),
            out_prefix: dir.join("work/whisper"),
            language: "ru".into(),
            beam_size: 5,
            vad_model: None,
        }
    }

    fn all_flag_combinations() -> impl Iterator<Item = WhisperFlags> {
        (0u16..256).map(|bits| WhisperFlags {
            processors: bits & 1 != 0,
            beam_size: bits & 2 != 0,
            flash_attn: bits & 4 != 0,
            output_json_full: bits & 8 != 0,
            print_progress: bits & 16 != 0,
            output_file: bits & 32 != 0,
            vad: bits & 64 != 0,
            vad_model: bits & 128 != 0,
        })
    }

    /// **ИНВАРИАНТ КРЕЙТА: `-ng` не появляется НИ ПРИ КАКИХ настройках.**
    ///
    /// Перебор всех комбинаций флагов и всех осмысленных значений запроса.
    /// Правило проекта «локальные модели только на GPU» держится этим тестом,
    /// а не дисциплиной автора.
    #[test]
    fn no_gpu_flags_ever_reach_the_argv() {
        let dir = Path::new("/tmp/forge-voice-args");
        let mut checked = 0usize;
        for flags in all_flag_combinations() {
            for lang in ["ru", "en", "", "  "] {
                for vad in [None, Some(dir.join("silero.bin"))] {
                    for bs in [0u32, 1, 5, 99] {
                        let r = WhisperRequest {
                            language: lang.into(),
                            beam_size: bs,
                            vad_model: vad.clone(),
                            ..req(dir)
                        };
                        let args = build_args(&r, &flags);
                        for bad in FORBIDDEN_ARGS {
                            assert!(
                                !args.iter().any(|a| a == bad),
                                "{bad} попал в argv при flags={flags:?}: {args:?}"
                            );
                        }
                        // И ни одного аргумента, начинающегося с запрещённого.
                        assert!(
                            !args.iter().any(|a| FORBIDDEN_ARGS.iter().any(|f| a.starts_with(f))),
                            "{args:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked >= 4096, "перебор оказался слишком узким: {checked}");
    }

    /// **ЗАПРЕЩЁННЫЙ ФЛАГ СНАРУЖИ ОТБРАСЫВАЕТСЯ, А НЕ ПРОХОДИТ В ARGV.**
    ///
    /// Дополнительные аргументы приходят строкой из файла настроек — там можно
    /// написать что угодно, в том числе `-ng`. Отказать целиком нельзя (одна
    /// лишняя строка в конфиге сломала бы запуск), проглотить молча — тем
    /// более.
    #[test]
    fn forbidden_flags_from_the_config_are_dropped() {
        let extra: Vec<String> = ["-t", "8", "-ng", "--no-gpu", "-ngl", "-ng=1", "--best-of", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let kept = sanitize_extra_args(&extra);
        assert_eq!(kept, vec!["-t", "8", "--best-of", "3"]);
        for bad in FORBIDDEN_ARGS {
            assert!(!kept.iter().any(|a| a.starts_with(bad)), "{kept:?}");
        }
        // Ничего запрещённого нет — список не меняется вовсе.
        let clean = vec!["-t".to_string(), "4".to_string()];
        assert_eq!(sanitize_extra_args(&clean), clean);
        assert!(sanitize_extra_args(&[]).is_empty());
    }

    /// **`--vad` НИКОГДА не собирается без `-vm`.**
    ///
    /// Не «ругается», а падает: exit 134, SIGABRT, стек `ggml_abort`.
    #[test]
    fn vad_is_never_built_without_its_model() {
        let dir = Path::new("/tmp/forge-voice-args");
        for flags in all_flag_combinations() {
            for vad in [None, Some(dir.join("silero.bin"))] {
                let r = WhisperRequest {
                    vad_model: vad.clone(),
                    ..req(dir)
                };
                let args = build_args(&r, &flags);
                if args.iter().any(|a| a == "--vad") {
                    let vm = args.iter().position(|a| a == "-vm");
                    assert!(vm.is_some(), "--vad без -vm: {args:?}");
                    let path = args.get(vm.unwrap() + 1).cloned().unwrap_or_default();
                    assert!(path.ends_with("silero.bin"), "-vm без пути: {args:?}");
                }
            }
        }

        // И положительный случай: модель есть, флаги есть — пара собралась.
        let flags = WhisperFlags {
            vad: true,
            vad_model: true,
            ..WhisperFlags::default()
        };
        let r = WhisperRequest {
            vad_model: Some(dir.join("silero.bin")),
            ..req(dir)
        };
        let args = build_args(&r, &flags);
        assert!(args.contains(&"--vad".to_string()) && args.contains(&"-vm".to_string()));
    }

    /// Базовая команда: то, что реально отработало на живой пробе.
    #[test]
    fn the_base_command_matches_the_probed_one() {
        let dir = Path::new("/tmp/forge-voice-args");
        let args = build_args(&req(dir), &WhisperFlags::default());
        let joined = args.join(" ");
        assert!(joined.contains("-m /tmp/forge-voice-args/ggml-large-v3-turbo-q5_0.bin"));
        assert!(joined.contains("-f /tmp/forge-voice-args/mix.wav"));
        assert!(joined.contains("-l ru"));
        assert!(joined.contains("-p 1"), "параллелизм ломает таймкоды: {joined}");
        assert!(joined.contains("-bs 5"));
        assert!(joined.contains("-ojf"), "нужны ПОТОКЕННЫЕ метки: {joined}");
        assert!(joined.contains("-pp"));
        assert!(joined.ends_with("-of /tmp/forge-voice-args/work/whisper"));
        assert!(!args.iter().any(|a| a == "-fa"), "{joined}");
        assert!(!args.iter().any(|a| a == "-oj"), "полу-JSON не нужен: {joined}");
    }

    /// Отсутствующий у сборки флаг не передаётся.
    #[test]
    fn flags_absent_from_the_help_are_not_passed() {
        let dir = Path::new("/tmp/forge-voice-args");
        let flags = WhisperFlags {
            output_json_full: false,
            print_progress: false,
            beam_size: false,
            ..WhisperFlags::default()
        };
        let args = build_args(&req(dir), &flags);
        assert!(!args.iter().any(|a| a == "-ojf"));
        assert!(!args.iter().any(|a| a == "-pp"));
        assert!(!args.iter().any(|a| a == "-bs"));
        assert!(args.iter().any(|a| a == "-p"), "процессоры остались");
    }

    #[test]
    fn the_help_is_parsed_into_flags() {
        let help = "-p N, --processors N\n-bs N, --beam-size N\n-ojf, --output-json-full\n\
                    -pp, --print-progress\n-of FNAME, --output-file FNAME";
        let f = WhisperFlags::parse_help(help);
        assert!(f.processors && f.beam_size && f.output_json_full && f.print_progress);
        assert!(f.output_file);
        assert!(!f.vad && !f.vad_model, "VAD в этой сборке нет: {f:?}");
        assert_eq!(WhisperFlags::parse_help(""), WhisperFlags {
            processors: false,
            beam_size: false,
            flash_attn: false,
            output_json_full: false,
            print_progress: false,
            output_file: false,
            vad: false,
            vad_model: false,
        });
    }

    /// Гард НЕ ВЕРИТ строкам, которые печатаются всегда.
    #[test]
    fn the_guard_is_not_fooled_by_the_lines_that_always_print() {
        let cpu = ALWAYS_LINES.join("\n");
        assert!(cpu.contains("ggml_metal"), "проверка теста: подстрока в выводе ЕСТЬ");
        assert!(!metal_confirmed(&cpu), "наивный гард пропустил бы CPU-прогон");

        let gpu = format!("{cpu}\nggml_metal_init: allocating\n");
        assert!(metal_confirmed(&gpu));

        // Префикс именно в начале строки: упоминание в чужом сообщении не
        // считается.
        assert!(!metal_confirmed("warning: ggml_metal_init: не вызывался"));
        assert!(metal_confirmed("   ggml_metal_init: allocating"));
        assert!(!metal_confirmed(""));
    }

    #[test]
    fn cpu_markers_are_recognised() {
        assert!(cpu_declared("whisper_backend_init_gpu: no GPU found"));
        assert!(cpu_declared("whisper_backend_init: using BLAS backend"));
        assert!(!cpu_declared("whisper_backend_init_gpu: using MTL0 backend"));
    }

    #[test]
    fn progress_lines_are_parsed() {
        assert_eq!(
            parse_progress("whisper_print_progress_callback: progress =  40%"),
            Some(40)
        );
        assert_eq!(parse_progress("progress = 100%"), Some(100));
        assert_eq!(parse_progress("ничего похожего"), None);
    }

    #[test]
    fn the_output_json_is_parsed_with_subword_tokens() {
        let raw = r#"{"transcription":[{
            "offsets":{"from":0,"to":3160},
            "text":" Коллеги, начинаем.",
            "tokens":[
              {"text":"[_BEG_]","offsets":{"from":0,"to":0},"p":0.9},
              {"text":" Кол","offsets":{"from":20,"to":200},"p":0.9},
              {"text":"лег","offsets":{"from":200,"to":400},"p":0.9},
              {"text":"и","offsets":{"from":400,"to":460},"p":0.9}
            ]}]}"#;
        let segs = parse_output_json(raw).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[0].end_ms, 3_160);
        assert_eq!(segs[0].text, "Коллеги, начинаем.");
        assert_eq!(segs[0].tokens.len(), 3, "метки потокенные, а не пословные");
        assert_eq!(segs[0].tokens[0].text, " Кол");
        assert_eq!(segs[0].tokens[2].end_ms, 460);
        assert!(is_service_token("[_BEG_]") && is_service_token(" [_TT_50] "));
        assert!(!is_service_token(" Кол"));
    }

    #[test]
    fn broken_output_is_a_named_error() {
        assert!(parse_output_json("не json").is_err());
        assert!(parse_output_json("{}").unwrap_err().contains("transcription"));
        assert!(parse_output_json(r#"{"transcription":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn the_json_path_follows_the_output_prefix() {
        let dir = Path::new("/tmp/m");
        assert_eq!(req(dir).json_path(), dir.join("work/whisper.json"));
    }

    /// Строки, которые ggml печатает ВСЕГДА — в том числе на `-ng` и на
    /// `--help`. Ровно на них ловится наивный гард.
    const ALWAYS_LINES: [&str; 3] = [
        "ggml_metal_device_init: GPU name:   MTL0 (Apple M1 Max)",
        "ggml_metal_library_init: using embedded metal library",
        "ggml_metal_rsets_init: reusing residency sets",
    ];

    // --- живые прогоны на моках -------------------------------------------------

    #[cfg(unix)]
    mod runs {
        use super::*;

        const OUTPUT_JSON: &str =
            r#"{"transcription":[{"offsets":{"from":0,"to":1000},"text":" Готово.","tokens":[]}]}"#;

        /// Пишет исполняемый shell-скрипт.
        fn mock_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// Мок whisper: печатает заданные строки stderr, при необходимости
        /// оставляет JSON и спит.
        fn stand(dir: &Path, name: &str, stderr: &[&str], write_json: bool, sleep: u64) -> WhisperRequest {
            let mut body = String::new();
            for l in stderr {
                body.push_str(&format!("echo '{}' >&2\n", l.replace('\'', "")));
            }
            if write_json {
                // Префикс вывода приходит после `-of` — мок его не разбирает, а
                // берёт известное место: проверяется гард, а не argv.
                body.push_str(&format!(
                    "mkdir -p '{}' && cat > '{}' <<'EOF'\n{OUTPUT_JSON}\nEOF\n",
                    dir.join("work").display(),
                    dir.join("work/whisper.json").display()
                ));
            }
            if sleep > 0 {
                body.push_str(&format!("exec sleep {sleep}\n"));
            }
            let bin = mock_bin(dir, name, &body);
            std::fs::write(dir.join("mix.wav"), b"RIFF").unwrap();
            std::fs::write(dir.join("model.bin"), b"lmgg").unwrap();
            WhisperRequest {
                bin,
                model: dir.join("model.bin"),
                wav: dir.join("mix.wav"),
                out_prefix: dir.join("work/whisper"),
                language: "ru".into(),
                beam_size: 5,
                vad_model: None,
            }
        }

        fn opts(grace_ms: u64, timeout_ms: u64) -> RunOptions {
            RunOptions {
                timeout: Duration::from_millis(timeout_ms),
                metal_grace: Duration::from_millis(grace_ms),
                cancel: CancellationToken::new(),
            }
        }

        #[tokio::test]
        async fn a_gpu_run_returns_parsed_segments() {
            let dir = tempfile::tempdir().unwrap();
            let mut lines: Vec<&str> = ALWAYS_LINES.to_vec();
            lines.push("ggml_metal_init: allocating");
            let req = stand(dir.path(), "whisper-ok", &lines, true, 0);
            let out = run(&req, &WhisperFlags::default(), &opts(5_000, 30_000), None)
                .await
                .unwrap();
            assert!(out.metal);
            assert_eq!(out.segments.len(), 1);
            assert_eq!(out.segments[0].text, "Готово.");
        }

        /// **ГЛАВНЫЙ ТЕСТ ГАРДА.** Прогон без `ggml_metal_init:` УБИВАЕТСЯ.
        ///
        /// Мок печатает три всегдашние строки — те самые, на которых наивный
        /// гард `contains("ggml_metal")` сработал бы, — и продолжает работать
        /// минуту. Гард обязан снять его за окно ожидания, а не досчитать.
        #[tokio::test]
        async fn a_run_that_never_says_ggml_metal_init_is_killed() {
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), "whisper-cpu", &ALWAYS_LINES, false, 60);

            let started = std::time::Instant::now();
            let err = run(&req, &WhisperFlags::default(), &opts(3_000, 60_000), None)
                .await
                .unwrap_err();

            assert_eq!(err.code, "metal_unavailable", "{err}");
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "процесс не убит, гард дождался его конца: {:?}",
                started.elapsed()
            );
            // Всегдашние строки в хвосте есть — гард их ВИДЕЛ и не поверил.
            assert!(
                err.stderr_tail.contains("ggml_metal_device_init:"),
                "{}",
                err.stderr_tail
            );
        }

        /// **НЕГАТИВНЫЙ ТЕСТ НА ЛОЖНОЕ СРАБАТЫВАНИЕ.**
        ///
        /// Мок БЫСТРО заканчивается с нулём и оставляет полноценный JSON. Окно
        /// ожидания тут не спасает — прогон закончился раньше него. Гард обязан
        /// признать это CPU-прогоном по факту отсутствия строки и выбросить
        /// результат.
        #[tokio::test]
        async fn a_fast_cpu_run_that_produced_output_is_still_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), "whisper-fast", &ALWAYS_LINES, true, 0);
            let err = run(&req, &WhisperFlags::default(), &opts(30_000, 30_000), None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "metal_unavailable", "{err}");
            assert!(
                !req.json_path().exists(),
                "транскрипт CPU-прогона остался на диске"
            );
        }

        /// Прогон, прямо объявивший процессор, снимается НЕМЕДЛЕННО, не
        /// дожидаясь окна ожидания.
        #[tokio::test]
        async fn a_run_that_declares_the_cpu_is_killed_at_once() {
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), "whisper-blas", &[CPU_MARKERS[1]], false, 60);

            let started = std::time::Instant::now();
            // Окно ожидания огромное: если бы гард полагался только на него,
            // тест шёл бы полминуты.
            let err = run(&req, &WhisperFlags::default(), &opts(30_000, 60_000), None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "metal_unavailable", "{err}");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "маркер процессора не сработал: {:?}",
                started.elapsed()
            );
            assert!(err.message.contains("процессоре"), "{}", err.message);
        }

        /// Прогресс `-pp` доезжает до подписчика.
        #[tokio::test]
        async fn progress_reaches_the_caller() {
            let dir = tempfile::tempdir().unwrap();
            let req = stand(
                dir.path(),
                "whisper-pp",
                &[
                    "ggml_metal_init: allocating",
                    "whisper_print_progress_callback: progress =  40%",
                ],
                true,
                0,
            );
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = seen.clone();
            let cb = move |p: u8| sink.lock().unwrap().push(p);
            run(&req, &WhisperFlags::default(), &opts(5_000, 30_000), Some(&cb))
                .await
                .unwrap();
            assert_eq!(seen.lock().unwrap().as_slice(), &[40]);
        }
    }
}
