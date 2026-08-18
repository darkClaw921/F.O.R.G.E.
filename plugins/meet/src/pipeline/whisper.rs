//! Запуск whisper.cpp с ГАРАНТИЕЙ, что расчёт идёт на GPU.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Правило «локальные модели только на GPU» исполняется СТРУКТУРНО
//!
//! Два независимых механизма, и ни один из них не комментарий:
//!
//! 1. **`-ng` и `--no-gpu` НИКОГДА не попадают в argv.** [`build_args`] —
//!    чистая функция, и на неё есть тест, перебирающий комбинации настроек:
//!    ни при какой из них запрещённый флаг не появляется. Приём взят из теста
//!    приватности в `claude-runner`.
//! 2. **stderr прогона парсится на [`METAL_NEEDLE`].** Не нашли — процесс
//!    УБИВАЕТСЯ с `metal_unavailable`, а не досчитывает молча на процессоре.
//!
//! Цена нарушения измерена в Ф0: те же двадцать секунд аудио считаются 14
//! секунд на GPU и 0,7× реального времени на CPU. Разрыв по энкоду
//! пятнадцатикратный, час аудио на CPU — почти полтора часа расчёта.
//!
//! # ПОЧЕМУ ГАРД СМОТРИТ НА ТОЧНЫЙ ПРЕФИКС, А НЕ НА ПОДСТРОКУ
//!
//! Проба Ф0 сравнила три прогона:
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
//! `ggml_abort` (измерено в Ф0). Поэтому «не собирать `--vad` без `-vm`» —
//! инвариант [`build_args`], а не проверка в рантайме: рантайму тут уже
//! нечего проверять, процесс умер.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::proc::{self, LineVerdict, ProcError};
use crate::deps::WhisperFlags;

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

/// Что запускать.
#[derive(Debug, Clone)]
pub struct WhisperRequest {
    /// Путь к бинарю (из проб `deps.rs`).
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

/// Собирает argv. **Чистая функция — ядро инвариантов фазы.**
///
/// Что и почему:
///
/// | флаг | зачем |
/// |---|---|
/// | `-m`, `-f`, `-of` | модель, вход, префикс вывода |
/// | `-l` | язык из конфига |
/// | `-p 1` | НЕ ПАРАЛЛЕЛИТЬ: параллелизм ломает таймкоды на границах чанков, а таймкоды здесь несут имена спикеров |
/// | `-bs 5` | ширина луча |
/// | `-ojf` | ПОТОКЕННЫЕ метки. Посегментные по 5–30 с регулярно перекрывают смену спикера; потокенные позволяют разрезать сегмент ровно на границе |
/// | `-pp` | прогресс в stderr |
///
/// `-fa` НЕ передаётся: в 1.9.2 он уже дефолт `true`, а обратный флаг —
/// `-nfa` (измерено в Ф0). Найденный пробой `-fa` при этом ошибкой не
/// считается, просто не используется.
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
            timeout: Duration::from_secs(crate::config::DEFAULT_WHISPER_TIMEOUT_SECS),
            metal_grace: Duration::from_secs(crate::config::DEFAULT_METAL_GRACE_SECS),
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
    /// работа [`crate::pipeline::speakers`].
    pub tokens: Vec<WhisperToken>,
}

/// Итог прогона.
#[derive(Debug, Clone)]
pub struct WhisperOutcome {
    pub segments: Vec<WhisperSegment>,
    /// Хвост stderr — уезжает в журнал встречи даже при успехе.
    pub stderr_tail: String,
    /// Подтверждён ли Metal. Всегда `true`: иначе прогон не дошёл бы сюда.
    pub metal: bool,
}

/// Служебные токены whisper: `[_BEG_]`, `[_TT_123]` и подобные.
fn is_service_token(text: &str) -> bool {
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

/// Путь к модели распознавания из конфига.
pub fn model_path(cfg: &crate::config::MeetConfig) -> PathBuf {
    crate::models::model_path(&cfg.models_dir, &cfg.whisper_model)
}

/// Путь к модели VAD, если она лежит на диске.
///
/// `None` означает «`--vad` собирать нельзя» — и это единственный способ
/// получить `--vad` в argv вообще.
pub fn vad_model_path(cfg: &crate::config::MeetConfig) -> Option<PathBuf> {
    let spec = crate::models::vad_spec();
    let p = cfg.models_dir.join(spec.file);
    p.is_file().then_some(p)
}

/// Существует ли путь к бинарю whisper по отчёту проб.
pub fn binary_from(report: &crate::deps::DepsReport) -> Option<PathBuf> {
    report.whisper.path.as_ref().map(PathBuf::from)
}

/// Каталог, где живёт вывод whisper внутри каталога встречи.
pub fn out_prefix(meeting_dir: &Path) -> PathBuf {
    meeting_dir.join(super::WORK_DIR).join("whisper")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeetConfig;

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

    /// **ИНВАРИАНТ ФАЗЫ: `-ng` не появляется НИ ПРИ КАКИХ настройках.**
    ///
    /// Перебор всех комбинаций флагов и всех осмысленных значений запроса —
    /// по образцу теста приватности в `claude-runner`. Правило проекта
    /// «локальные модели только на GPU» держится этим тестом, а не
    /// дисциплиной автора.
    #[test]
    fn no_gpu_flags_ever_reach_the_argv() {
        let dir = std::path::Path::new("/tmp/forge-meet-args");
        let mut checked = 0usize;
        for bits in 0u16..256 {
            let flags = WhisperFlags {
                processors: bits & 1 != 0,
                beam_size: bits & 2 != 0,
                flash_attn: bits & 4 != 0,
                output_json_full: bits & 8 != 0,
                print_progress: bits & 16 != 0,
                output_file: bits & 32 != 0,
                vad: bits & 64 != 0,
                vad_model: bits & 128 != 0,
            };
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
                        // И ни одного аргумента, начинающегося с -ng.
                        assert!(!args.iter().any(|a| a.starts_with("-ng")), "{args:?}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked >= 4096, "перебор оказался слишком узким: {checked}");
    }

    /// **`--vad` НИКОГДА не собирается без `-vm`.**
    ///
    /// Не «ругается», а падает: exit 134, SIGABRT, стек `ggml_abort`.
    #[test]
    fn vad_is_never_built_without_its_model() {
        let dir = std::path::Path::new("/tmp/forge-meet-args");
        for bits in 0u16..256 {
            let flags = WhisperFlags {
                processors: bits & 1 != 0,
                beam_size: bits & 2 != 0,
                flash_attn: bits & 4 != 0,
                output_json_full: bits & 8 != 0,
                print_progress: bits & 16 != 0,
                output_file: bits & 32 != 0,
                vad: bits & 64 != 0,
                vad_model: bits & 128 != 0,
            };
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

    /// Базовая команда: то, что реально отработало в пробе Ф0.
    #[test]
    fn the_base_command_matches_the_probed_one() {
        let dir = std::path::Path::new("/tmp/forge-meet-args");
        let args = build_args(&req(dir), &WhisperFlags::default());
        let joined = args.join(" ");
        assert!(joined.contains("-m /tmp/forge-meet-args/ggml-large-v3-turbo-q5_0.bin"));
        assert!(joined.contains("-f /tmp/forge-meet-args/mix.wav"));
        assert!(joined.contains("-l ru"));
        assert!(joined.contains("-p 1"), "параллелизм ломает таймкоды: {joined}");
        assert!(joined.contains("-bs 5"));
        assert!(joined.contains("-ojf"), "нужны ПОТОКЕННЫЕ метки: {joined}");
        assert!(joined.contains("-pp"));
        assert!(joined.ends_with("-of /tmp/forge-meet-args/work/whisper"));
        // `-fa` в 1.9.2 уже дефолт: передавать его бессмысленно.
        assert!(!args.iter().any(|a| a == "-fa"), "{joined}");
        assert!(!args.iter().any(|a| a == "-oj"), "полу-JSON не нужен: {joined}");
    }

    /// Отсутствующий у сборки флаг не передаётся.
    #[test]
    fn flags_absent_from_the_help_are_not_passed() {
        let dir = std::path::Path::new("/tmp/forge-meet-args");
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

    /// Гард НЕ ВЕРИТ строкам, которые печатаются всегда.
    ///
    /// Тест дословно повторяет наблюдение Ф0: при `-ng` в выводе есть три
    /// строки `ggml_metal_*_init:` и нет `ggml_metal_init:`.
    #[test]
    fn the_guard_is_not_fooled_by_the_lines_that_always_print() {
        let cpu = crate::testing::mocks::GGML_ALWAYS_LINES.join("\n");
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
        let mock = crate::testing::mocks::WhisperMock {
            segments: vec![crate::testing::mocks::MockSegment::with_tokens(
                0,
                3_160,
                " Коллеги, начинаем.",
                &[(20, 200, " Кол"), (200, 400, "лег"), (400, 460, "и")],
            )],
            ..Default::default()
        };
        let segs = parse_output_json(&mock.output_json()).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[0].end_ms, 3_160);
        assert_eq!(segs[0].text, "Коллеги, начинаем.");
        assert_eq!(segs[0].tokens.len(), 3, "метки потокенные, а не пословные");
        assert_eq!(segs[0].tokens[0].text, " Кол");
        assert_eq!(segs[0].tokens[2].end_ms, 460);
    }

    /// Служебные токены (`[_BEG_]`) в словах не участвуют.
    #[test]
    fn service_tokens_are_dropped() {
        let mock = crate::testing::mocks::WhisperMock::default();
        let segs = parse_output_json(&mock.output_json()).unwrap();
        assert!(
            segs[0].tokens.iter().all(|t| !t.text.starts_with("[_")),
            "{:?}",
            segs[0].tokens
        );
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
        let dir = std::path::Path::new("/tmp/m");
        assert_eq!(req(dir).json_path(), dir.join("work/whisper.json"));
    }

    // --- живые прогоны на моках -------------------------------------------------

    #[cfg(unix)]
    mod runs {
        use super::*;
        use crate::testing::mocks::{mock_whisper, WhisperMock};

        fn stand(dir: &Path, mock: &WhisperMock) -> WhisperRequest {
            let bin = mock_whisper(dir, "whisper-cli", mock).unwrap();
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
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), &WhisperMock::default());
            let out = run(&req, &WhisperFlags::default(), &opts(5_000, 30_000), None)
                .await
                .unwrap();
            assert!(out.metal);
            assert_eq!(out.segments.len(), 1);
            assert_eq!(out.segments[0].text, "Коллеги, начинаем.");
            assert!(out.stderr_tail.contains("ggml_metal_init:"));
        }

        /// **ГЛАВНЫЙ ТЕСТ ФАЗЫ.** Прогон без `ggml_metal_init:` УБИВАЕТСЯ.
        ///
        /// Мок печатает три всегдашние строки — те самые, на которых наивный
        /// гард `contains("ggml_metal")` сработал бы, — и продолжает работать
        /// минуту. Гард обязан снять его за окно ожидания, а не досчитать.
        #[tokio::test]
        async fn a_run_that_never_says_ggml_metal_init_is_killed() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let mock = WhisperMock {
                sleep_secs: 60,
                ..WhisperMock::silent_about_the_backend()
            };
            let req = stand(dir.path(), &mock);

            // Окно ожидания три секунды, а не двести миллисекунд: под
            // параллельным прогоном тестов процесс успевает родиться позже,
            // чем истечёт слишком короткое окно, и гард снимал бы его ДО
            // первой строки — тест проверял бы не то. Доказательством
            // убийства служит сравнение с шестьюдесятью секундами сна.
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
            // И всегдашние строки в хвосте есть — то есть гард их ВИДЕЛ и не
            // поверил.
            assert!(
                err.stderr_tail.contains("ggml_metal_device_init:"),
                "{}",
                err.stderr_tail
            );
            assert!(!req.json_path().exists(), "результат CPU-прогона не отброшен");
        }

        /// **НЕГАТИВНЫЙ ТЕСТ НА ЛОЖНОЕ СРАБАТЫВАНИЕ.**
        ///
        /// Мок печатает всегдашние строки, БЫСТРО заканчивается с нулём и
        /// оставляет полноценный JSON. Окно ожидания тут не спасает — прогон
        /// закончился раньше него. Гард обязан признать это CPU-прогоном по
        /// факту отсутствия строки и выбросить результат.
        #[tokio::test]
        async fn a_fast_cpu_run_that_produced_output_is_still_rejected() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), &WhisperMock::silent_about_the_backend());

            // Окно ожидания заведомо больше времени прогона: спасти может
            // только проверка после выхода.
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
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), &WhisperMock::cpu_run_that_keeps_going(60));

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

        /// ХВОСТ STDERR ПРИ ПАДЕНИИ ДОХОДИТ ДО ВЫЗЫВАЮЩЕГО.
        #[tokio::test]
        async fn a_failing_run_carries_its_stderr_tail() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(
                dir.path(),
                &WhisperMock::failing("error: failed to load model from 'model.bin'"),
            );
            let err = run(&req, &WhisperFlags::default(), &opts(5_000, 30_000), None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "exit_failed");
            assert!(
                err.stderr_tail.contains("failed to load model"),
                "{}",
                err.stderr_tail
            );
            assert!(err.full_message().contains("failed to load model"));
        }

        /// Зависший whisper убивается по таймауту.
        #[tokio::test]
        async fn a_hanging_run_is_killed_by_the_timeout() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), &WhisperMock::hanging(60));

            let started = std::time::Instant::now();
            // Окно ожидания больше таймаута: снять прогон обязан именно
            // таймаут, и код ошибки должен быть его.
            let err = run(&req, &WhisperFlags::default(), &opts(30_000, 300), None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "timeout", "{err}");
            assert!(started.elapsed() < Duration::from_secs(10));
        }

        /// Отмена встречи снимает идущий прогон.
        #[tokio::test]
        async fn cancelling_stops_the_run() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), &WhisperMock::hanging(60));
            let o = RunOptions {
                timeout: Duration::from_secs(60),
                metal_grace: Duration::from_secs(30),
                cancel: CancellationToken::new(),
            };
            let token = o.cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                token.cancel();
            });
            let started = std::time::Instant::now();
            let err = run(&req, &WhisperFlags::default(), &o, None).await.unwrap_err();
            assert_eq!(err.code, "cancelled", "{err}");
            assert!(started.elapsed() < Duration::from_secs(10));
        }

        /// Прогресс `-pp` доезжает до подписчика.
        #[tokio::test]
        async fn progress_reaches_the_caller() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let mock = WhisperMock {
                stderr: "whisper_print_progress_callback: progress =  40%".into(),
                ..WhisperMock::default()
            };
            let req = stand(dir.path(), &mock);
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = seen.clone();
            let cb = move |p: u8| sink.lock().unwrap().push(p);
            run(
                &req,
                &WhisperFlags::default(),
                &opts(5_000, 30_000),
                Some(&cb),
            )
            .await
            .unwrap();
            assert_eq!(seen.lock().unwrap().as_slice(), &[40]);
        }

        /// Прошлый вывод не выдаётся за новый: `-ojf` пишет файл, и остаток
        /// предыдущего прогона иначе выглядел бы результатом текущего.
        #[tokio::test]
        async fn a_stale_output_file_is_removed_before_the_run() {
            let _g = crate::testing::process_count_guard();
            let dir = tempfile::tempdir().unwrap();
            let req = stand(dir.path(), &WhisperMock::failing("boom"));
            std::fs::create_dir_all(dir.path().join("work")).unwrap();
            std::fs::write(req.json_path(), r#"{"transcription":[]}"#).unwrap();

            let err = run(&req, &WhisperFlags::default(), &opts(5_000, 30_000), None)
                .await
                .unwrap_err();
            assert_eq!(err.code, "exit_failed");
            assert!(
                !req.json_path().exists(),
                "старый вывод остался и выглядит результатом упавшего прогона"
            );
        }
    }

    #[test]
    fn the_vad_model_is_only_offered_when_it_is_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MeetConfig {
            models_dir: dir.path().to_path_buf(),
            ..MeetConfig::default()
        };
        assert!(vad_model_path(&cfg).is_none(), "модели VAD нет — и флага не будет");
        std::fs::write(dir.path().join(crate::models::vad_spec().file), b"lmgg").unwrap();
        assert!(vad_model_path(&cfg).is_some());
    }
}
