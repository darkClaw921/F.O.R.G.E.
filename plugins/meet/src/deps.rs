//! Состояние внешних зависимостей: ffmpeg, ffprobe, whisper.cpp, Chrome и
//! доступность Metal.
//!
//! # Пробы живут в `spawn_workers`, а НЕ в `init`
//!
//! `init` синхронен, вызывается на пути старта devforge и не имеет права
//! запускать процессы: `whisper-cli --help` на ПЕРВОМ в жизни запуске после
//! установки тратит 12,5 секунды на `ggml_metal_library_init` (измерено в Ф0).
//! Столько devforge не будет ждать биндинга порта. На отсутствие процессов в
//! `init` есть тест, а не комментарий.
//!
//! # Плагин стартует ВСЕГДА
//!
//! Ни одна отсутствующая зависимость не мешает старту — она лишь сужает набор
//! доступного, и `GET /api/meet/status` честно об этом рассказывает:
//!
//! | чего нет | что перестаёт работать |
//! |---|---|
//! | ffmpeg | только архивный файл. Транскрипт есть: по проводу уже идёт PCM 16 кГц, ffmpeg с критического пути расшифровки убран |
//! | whisper или модель | 409 на старте записи плюс кнопка «Расшифровать» задним числом, когда появится |
//! | Chrome | 409 с текстом, что поставить |
//! | Metal | 409: считать на CPU запрещено правилом проекта |
//!
//! # Имя бинаря whisper.cpp неопределённо
//!
//! Формула ставила `whisper-cpp`, апстрим с 1.7.x переименовал `main` →
//! `whisper-cli`. Ф0 измерила на этой машине: фактическое имя — `whisper-cli`,
//! а `whisper-cpp` и `main` не существуют вовсе. Отсюда порядок кандидатов
//! [`WHISPER_CANDIDATES`], и отсюда же особая осторожность с последним: в
//! `PATH` под именем `whisper` часто лежит Python **openai-whisper** — другой
//! CLI, другие модели, CPU по умолчанию. Принять его за свой значило бы
//! нарушить правило «локальные модели только на GPU» МОЛЧА.
//!
//! # Metal: проба и ГАРД — РАЗНЫЕ проверки, и путать их нельзя
//!
//! | | что спрашивает | по какой строке |
//! |---|---|---|
//! | проба ([`probe_all`]) | «есть ли на машине Metal-устройство» | `ggml_metal_device_init:` в выводе `--help` |
//! | гард ([`crate::pipeline::whisper`]) | «ЭТОТ прогон реально пошёл на GPU» | `ggml_metal_init:` в stderr прогона |
//!
//! Строки разные, потому что разные вопросы. `ggml_metal_device_init:`
//! печатается ВСЕГДА, когда ggml видит устройство — в том числе при `-ng` и
//! даже на `--help`; она годится как признак наличия железа и НЕ ГОДИТСЯ как
//! доказательство, что расчёт пошёл на нём. `ggml_metal_init:` печатается
//! только при реальном выделении Metal-бэкенда, и именно её требует гард.
//!
//! Совпади эти две проверки — правило «локальные модели только на GPU»
//! исполнялось бы уговором: CPU-прогон печатает `ggml_metal_device_init:` и
//! прошёл бы гард молча. На это есть тест
//! [`the_probe_and_the_guard_look_at_different_lines`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::MeetConfig;

/// Кандидаты имени бинаря whisper.cpp, в порядке проверки.
///
/// Последний — ТОЛЬКО с верификацией вывода `--version`.
pub const WHISPER_CANDIDATES: [&str; 4] = ["whisper-cli", "whisper-cpp", "main", "whisper"];

/// Состояние одного внешнего бинаря.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepBinary {
    /// Как искали (`ffmpeg`, `whisper-cli`, …).
    ///
    /// `String`, а не `&'static str`: отчёт уезжает в WS-кадр, а кадры
    /// плагина обязаны разбираться обратно (контрактный тест с фронтом гоняет
    /// их в обе стороны), и `Deserialize` для заимствованной строки потребовал
    /// бы времени жизни на весь кадр.
    pub name: String,
    /// `None` — ещё не проверяли. Отличать «не нашли» от «не смотрели»
    /// обязательно: до первой пробы интерфейс не имеет права писать «ffmpeg не
    /// установлен».
    pub found: Option<bool>,
    pub path: Option<String>,
    pub version: Option<String>,
    /// Человекочитаемая причина отказа — то, что показывается пользователю
    /// вместе с 409.
    pub error: Option<String>,
}

impl DepBinary {
    /// Состояние «ещё не проверяли».
    pub fn unprobed(name: &str) -> Self {
        Self {
            name: name.to_string(),
            found: None,
            path: None,
            version: None,
            error: None,
        }
    }

    /// Готов ли компонент к использованию.
    pub fn is_ready(&self) -> bool {
        self.found == Some(true)
    }
}

/// Флаги whisper.cpp, ФАКТИЧЕСКИ поддержанные установленной версией.
///
/// Проверяются программно разбором `--help`, а не гаданием по номеру версии:
/// формула brew, сборка из исходников и старый бинарь в `~/bin` дают три
/// разных набора, и «версия 1.9.2» ничего о них не говорит.
///
/// `--help` печатается в **stderr** (измерено в Ф0) — парсер обязан читать
/// именно его.
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
    /// `--vad`. **Без `-vm` роняет процесс по SIGABRT** (exit 134, измерено в
    /// Ф0), поэтому одного этого флага мало — нужна ещё модель VAD.
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

/// Полный отчёт о внешнем окружении. Уезжает в `GET /api/meet/status` и в
/// кадр `deps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepsReport {
    /// Проходили ли пробы вообще. `false` между `init` и первым тиком
    /// `spawn_workers` — интерфейс в это время показывает «проверяем», а не
    /// «ничего нет».
    pub probed: bool,
    pub ffmpeg: DepBinary,
    pub ffprobe: DepBinary,
    pub whisper: DepBinary,
    pub chrome: DepBinary,
    /// Доступен ли Metal.
    ///
    /// Опорная строка — точный префикс `ggml_metal_init:` в НАЧАЛЕ строки
    /// stderr (измерено в Ф0). Проверка `contains("ggml_metal")` НЕ ГОДИТСЯ:
    /// `ggml_metal_device_init:` и `ggml_metal_library_init:` печатаются
    /// всегда, даже с `-ng` и даже на `--help`, поэтому такой гард пропустил
    /// бы CPU-прогон. Поле `systeminfo` из JSON не годится тоже — у GPU- и
    /// CPU-прогона оно побайтово одинаково.
    pub gpu_metal: Option<bool>,
    /// Флаги, фактически поддержанные найденным бинарём whisper.
    #[serde(default)]
    pub whisper_flags: WhisperFlags,
    /// Модели whisper, найденные в каталоге моделей.
    pub models: Vec<String>,
    /// Выбранная модель есть на диске.
    pub model_ready: bool,
}

impl Default for DepsReport {
    fn default() -> Self {
        Self {
            probed: false,
            ffmpeg: DepBinary::unprobed("ffmpeg"),
            ffprobe: DepBinary::unprobed("ffprobe"),
            whisper: DepBinary::unprobed("whisper-cli"),
            chrome: DepBinary::unprobed("chrome"),
            gpu_metal: None,
            whisper_flags: WhisperFlags::default(),
            models: Vec::new(),
            model_ready: false,
        }
    }
}

impl DepsReport {
    /// Можно ли начать ЗАПИСЬ. Нужен только браузер: расшифровку можно
    /// дозапустить потом, а вот встречу переиграть нельзя.
    pub fn can_record(&self) -> bool {
        // До первой пробы не мешаем: отказать из-за непроверенного окружения
        // хуже, чем попробовать и честно упасть с текстом ошибки.
        !self.probed || self.chrome.is_ready()
    }

    /// Можно ли расшифровывать. Metal обязателен: правило «локальные модели
    /// только на GPU» исполняется структурно, а не уговорами.
    pub fn can_transcribe(&self) -> bool {
        self.whisper.is_ready() && self.model_ready && self.gpu_metal == Some(true)
    }

    /// Код ошибки для лестницы 409, если запись начать нельзя.
    pub fn record_blocker(&self) -> Option<&'static str> {
        (!self.can_record()).then_some("chrome_unavailable")
    }

    /// Код ошибки для лестницы 409, если расшифровать нельзя ЛОКАЛЬНО.
    pub fn transcribe_blocker(&self) -> Option<&'static str> {
        if !self.whisper.is_ready() {
            Some("whisper_unavailable")
        } else if !self.model_ready {
            Some("model_unavailable")
        } else if self.gpu_metal != Some(true) {
            Some("metal_unavailable")
        } else {
            None
        }
    }

    /// Что мешает расшифровать ВЫБРАННЫМ путём.
    ///
    /// # Деградация работает в ОБЕ стороны
    ///
    /// В облачном режиме отсутствие `whisper-cli`, весов на диске (1,6 ГБ) и
    /// Metal перестаёт быть препятствием: ни одно из трёх облаку не нужно, и
    /// отказ по ним был бы отказом по требованию, которого никто не
    /// предъявлял. В локальном режиме ровно так же ничему не мешает отсутствие
    /// ключа polza.ai.
    ///
    /// Единственная внешняя зависимость облачного пути — `ffmpeg`: запись
    /// режется на окна и сжимается в mp3 НАШЕЙ стороной (сырой WAV роняет их
    /// шлюз с `Maximum call stack size exceeded`, а предел сервиса — около 60
    /// секунд ПО СТЕНЕ на запрос).
    pub fn transcribe_blocker_for(
        &self,
        backend: crate::config::SttBackend,
        cloud_configured: bool,
    ) -> Option<&'static str> {
        if backend.is_local() {
            return self.transcribe_blocker();
        }
        if !cloud_configured {
            return Some("cloud_not_configured");
        }
        // До первой пробы не мешаем — по той же причине, что и у записи:
        // «ещё не смотрели» не равно «ничего нет».
        if self.probed && !self.ffmpeg.is_ready() {
            return Some("ffmpeg_unavailable");
        }
        None
    }
}

// --- пробы --------------------------------------------------------------------

/// Строка, по которой опознаётся НАСТОЯЩИЙ whisper.cpp в ответе `--version`.
pub const WHISPER_VERSION_NEEDLE: &str = "whisper.cpp";

/// Признак Metal-УСТРОЙСТВА в выводе ggml.
///
/// Именно устройства, а не прогона на нём: строка печатается всегда, когда
/// ggml видит GPU (в том числе с `-ng`). Разбор — в шапке модуля.
pub const GGML_DEVICE_NEEDLE: &str = "ggml_metal_device_init:";

/// Ищет исполняемый файл в `PATH`.
///
/// Свой обход, а не крейт `which`: нужен один вызов, а новый узел в дереве
/// зависимостей стоит дороже пятнадцати строк.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Результат запуска пробы: код, stdout и stderr одной строкой.
#[derive(Debug, Clone, Default)]
pub struct ProbeOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl ProbeOutput {
    /// stdout и stderr вместе.
    ///
    /// Разные бинари печатают одно и то же в разные потоки: `whisper-cli
    /// --help` уходит в stderr, `ffmpeg -version` — в stdout. Искать в обоих
    /// дешевле, чем помнить, кто куда пишет.
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Запускает бинарь и ждёт его не дольше таймаута.
///
/// Таймаут ОБЯЗАН быть щедрым: первый в жизни запуск `whisper-cli` после
/// установки тратит 12,5 секунды на `ggml_metal_library_init` (измерено в Ф0),
/// и пятисекундная проба дала бы на свежей машине ложное «whisper
/// недоступен» — то есть отправила бы пользователя чинить то, что не сломано.
pub async fn run_probe(bin: &Path, args: &[&str], timeout: Duration) -> Option<ProbeOutput> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Some(ProbeOutput {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }),
        // Таймаут: `kill_on_drop` снимает процесс вместе с future.
        _ => None,
    }
}

/// Проба простого бинаря вида ffmpeg/ffprobe: нашли — спросили версию.
pub async fn probe_simple(
    name: &str,
    explicit: Option<&Path>,
    timeout: Duration,
) -> DepBinary {
    let mut dep = DepBinary::unprobed(name);
    let found = match explicit {
        Some(p) => p.is_file().then(|| p.to_path_buf()),
        None => find_in_path(name),
    };
    let Some(path) = found else {
        dep.found = Some(false);
        dep.error = Some(format!("{name} не найден ни в PATH, ни в конфиге"));
        return dep;
    };
    dep.path = Some(path.display().to_string());
    match run_probe(&path, &["-version"], timeout).await {
        Some(out) => {
            dep.found = Some(true);
            dep.version = out
                .combined()
                .lines()
                .find(|l| l.contains("version"))
                .map(|l| l.trim().to_string());
        }
        None => {
            dep.found = Some(false);
            dep.error = Some(format!("{name} есть на диске, но не отвечает на -version"));
        }
    }
    dep
}

/// Проба whisper.cpp: перебор кандидатов С ВЕРИФИКАЦИЕЙ.
///
/// Возвращает состояние бинаря, его флаги и признак Metal-устройства.
///
/// Порядок кандидатов ([`WHISPER_CANDIDATES`]) и обязательность верификации —
/// прямое следствие пробы Ф0: в `PATH` этой машины под именем `whisper` лежит
/// Python **openai-whisper**, у которого другой CLI, другие модели и CPU по
/// умолчанию. Прими мы его за свой — пайплайн ушёл бы на CPU МОЛЧА, то есть
/// нарушил бы правило проекта тем самым способом, который невозможно
/// заметить.
pub async fn probe_whisper(
    explicit: Option<&Path>,
    timeout: Duration,
) -> (DepBinary, WhisperFlags, Option<bool>) {
    let mut dep = DepBinary::unprobed("whisper-cli");
    let mut tried: Vec<String> = Vec::new();

    let candidates: Vec<(String, PathBuf, bool)> = match explicit {
        // Явный путь из конфига: верифицируем строго — пользователь мог
        // прописать openai-whisper руками с тем же успехом.
        Some(p) => vec![(p.display().to_string(), p.to_path_buf(), true)],
        None => WHISPER_CANDIDATES
            .iter()
            .enumerate()
            .filter_map(|(i, name)| {
                find_in_path(name).map(|p| {
                    // Последний кандидат (`whisper`) — только со строгой
                    // проверкой `--version`.
                    (name.to_string(), p, i + 1 == WHISPER_CANDIDATES.len())
                })
            })
            .collect(),
    };

    for (name, path, strict) in candidates {
        tried.push(name.clone());
        let version = run_probe(&path, &["--version"], timeout).await;
        let version_line = version.as_ref().and_then(|o| {
            o.combined()
                .lines()
                .find(|l| l.contains(WHISPER_VERSION_NEEDLE))
                .map(|l| l.trim().to_string())
        });

        let help = run_probe(&path, &["--help"], timeout).await;
        let help_text = help.as_ref().map(ProbeOutput::combined).unwrap_or_default();
        let flags = WhisperFlags::parse_help(&help_text);

        // Верификация. Строгая — только по `--version`; мягкая допускает
        // старый бинарь без `--version`, но требует опознаваемой справки.
        let accepted = if strict {
            version_line.is_some()
        } else {
            version_line.is_some() || flags.output_json_full
        };
        if !accepted {
            continue;
        }

        dep.name = name;
        dep.found = Some(true);
        dep.path = Some(path.display().to_string());
        dep.version = version_line;
        let metal = help_text
            .lines()
            .any(|l| l.trim_start().starts_with(GGML_DEVICE_NEEDLE));
        return (dep, flags, Some(metal));
    }

    dep.found = Some(false);
    dep.error = Some(if tried.is_empty() {
        format!(
            "whisper.cpp не найден; искали: {}",
            WHISPER_CANDIDATES.join(", ")
        )
    } else {
        format!(
            "найдено под именами {}, но ни один не оказался whisper.cpp \
             (под именем whisper в PATH часто лежит Python openai-whisper)",
            tried.join(", ")
        )
    });
    (dep, WhisperFlags::default(), None)
}

/// Проба Chrome. Процессов не запускает: браузер стоит секунды, а знать надо
/// лишь, есть ли он на диске.
pub fn probe_chrome(explicit: Option<&Path>) -> DepBinary {
    let mut dep = DepBinary::unprobed("chrome");
    let found = match explicit {
        Some(p) => p.is_file().then(|| p.to_path_buf()).ok_or_else(|| {
            format!("chrome_path указывает на {}, но файла там нет", p.display())
        }),
        None => forge_cdp::find_chrome().map_err(|e| e.to_string()),
    };
    match found {
        Ok(path) => {
            dep.found = Some(true);
            dep.path = Some(path.display().to_string());
        }
        Err(e) => {
            dep.found = Some(false);
            dep.error = Some(e);
        }
    }
    dep
}

/// Полная проба окружения.
///
/// **Живёт в `spawn_workers`, а не в `init`** — см. шапку модуля. Возвращает
/// готовый отчёт; кто его положит в состояние и разошлёт кадром `deps`,
/// решает вызывающий.
pub async fn probe_all(cfg: &MeetConfig) -> DepsReport {
    let timeout = Duration::from_secs(cfg.deps_probe_timeout_secs.max(1));

    let ffmpeg = probe_simple("ffmpeg", cfg.ffmpeg_path.as_deref(), timeout).await;
    let ffprobe = probe_simple("ffprobe", cfg.ffprobe_path.as_deref(), timeout).await;
    let (whisper, whisper_flags, gpu_metal) =
        probe_whisper(cfg.whisper_path.as_deref(), timeout).await;
    let chrome = probe_chrome(cfg.chrome_path.as_deref());

    let models = crate::models::installed(&cfg.models_dir);
    let model_ready = crate::models::is_installed(&cfg.models_dir, &cfg.whisper_model);

    DepsReport {
        probed: true,
        ffmpeg,
        ffprobe,
        whisper,
        chrome,
        gpu_metal,
        whisper_flags,
        models,
        model_ready,
    }
}

/// Перепроверяет окружение, кладёт отчёт в состояние и вещает кадр `deps`.
///
/// Зовётся из `spawn_workers` (первая проба), из `POST /api/meet/deps/recheck`
/// и после скачивания модели. Последнее не косметика: человек нажал
/// «скачать», дождался — и кнопка «Расшифровать» обязана ожить сама, без
/// перезапуска devforge.
pub async fn refresh(state: &std::sync::Arc<crate::state::MeetState>) -> DepsReport {
    let report = probe_all(&state.config).await;
    *state.deps.write().await = report.clone();
    state.broadcast_event(crate::ws::protocol::MeetServerMsg::Deps {
        deps: report.clone(),
    });
    tracing::info!(
        target: "forge_meet",
        ffmpeg = report.ffmpeg.is_ready(),
        ffprobe = report.ffprobe.is_ready(),
        whisper = report.whisper.is_ready(),
        whisper_path = report.whisper.path.as_deref().unwrap_or("-"),
        chrome = report.chrome.is_ready(),
        gpu_metal = ?report.gpu_metal,
        model_ready = report.model_ready,
        models = report.models.len(),
        "forge-meet: пробы внешних бинарей"
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unprobed_report_says_unknown_rather_than_missing() {
        let d = DepsReport::default();
        assert!(!d.probed);
        assert_eq!(d.ffmpeg.found, None, "«не смотрели» ≠ «не нашли»");
        assert!(!d.ffmpeg.is_ready());
        // И не мешает записи: отказать из-за непроверенного окружения хуже,
        // чем попробовать.
        assert!(d.can_record());
        assert_eq!(d.record_blocker(), None);
    }

    #[test]
    fn transcription_requires_metal_explicitly() {
        let mut d = DepsReport {
            probed: true,
            ..Default::default()
        };
        d.whisper.found = Some(true);
        d.model_ready = true;

        // Metal не подтверждён — расшифровка запрещена, и код говорит именно
        // об этом, а не про отсутствие бинаря.
        d.gpu_metal = None;
        assert!(!d.can_transcribe());
        assert_eq!(d.transcribe_blocker(), Some("metal_unavailable"));

        d.gpu_metal = Some(false);
        assert_eq!(d.transcribe_blocker(), Some("metal_unavailable"));

        d.gpu_metal = Some(true);
        assert!(d.can_transcribe());
        assert_eq!(d.transcribe_blocker(), None);
    }

    #[test]
    fn the_transcription_blocker_names_the_first_missing_piece() {
        let mut d = DepsReport {
            probed: true,
            ..Default::default()
        };
        assert_eq!(d.transcribe_blocker(), Some("whisper_unavailable"));
        d.whisper.found = Some(true);
        assert_eq!(d.transcribe_blocker(), Some("model_unavailable"));
        d.model_ready = true;
        assert_eq!(d.transcribe_blocker(), Some("metal_unavailable"));
    }

    /// Порядок кандидатов зафиксирован пробой Ф0, и последний из них опасен:
    /// под именем `whisper` в PATH часто лежит Python openai-whisper.
    #[test]
    fn the_whisper_candidate_order_is_frozen() {
        assert_eq!(
            WHISPER_CANDIDATES,
            ["whisper-cli", "whisper-cpp", "main", "whisper"]
        );
    }

    #[test]
    fn the_report_serialises_for_the_status_route() {
        let json = serde_json::to_string(&DepsReport::default()).unwrap();
        assert!(json.contains("\"probed\":false"));
        assert!(json.contains("gpu_metal"));
        assert!(json.contains("\"found\":null"));
    }

    // --- разбор `--help` -------------------------------------------------------

    /// Флаги берутся из справки, а не из номера версии.
    ///
    /// Отсутствующий флаг обязан ВЫКЛЮЧИТЬСЯ: передай мы `-ojf` сборке, которая
    /// его не знает, whisper упал бы на разборе аргументов — то есть встреча
    /// осталась бы без расшифровки из-за флага.
    #[test]
    fn missing_flags_are_switched_off_and_present_ones_are_used() {
        let full = crate::testing::mocks::WHISPER_HELP_FLAGS.join("\n");
        let f = WhisperFlags::parse_help(&full);
        assert!(f.processors && f.beam_size && f.output_json_full && f.print_progress);
        assert!(f.output_file && f.flash_attn && f.vad && f.vad_model);

        // Старая сборка без потокенного JSON.
        let old = full.replace("-ojf,     --output-json-full", "-oj,      --output-json");
        let f = WhisperFlags::parse_help(&old);
        assert!(!f.output_json_full, "отсутствующий флаг обязан выключиться");
        assert!(f.processors, "остальные при этом не теряются");

        // Совсем чужая справка (Python openai-whisper).
        let alien = WhisperFlags::parse_help("--model MODEL\n--output_format FORMAT");
        assert!(!alien.output_json_full && !alien.processors && !alien.vad);
    }

    /// `--vad` и `-vm` — разные флаги, и одного мало.
    ///
    /// Без модели VAD процесс не ругается, а ПАДАЕТ: exit 134, SIGABRT
    /// (измерено в Ф0). Поэтому наличие `--vad` в справке само по себе ничего
    /// не разрешает.
    #[test]
    fn vad_and_its_model_are_separate_flags() {
        let f = WhisperFlags::parse_help("  --vad  включить VAD\n");
        assert!(f.vad);
        assert!(!f.vad_model, "модель VAD — отдельный флаг");
        assert!(!WhisperFlags::default().vad, "по умолчанию VAD выключен");
    }

    // --- пробы -----------------------------------------------------------------

    fn probe_timeout() -> Duration {
        Duration::from_secs(10)
    }

    /// Отсутствующий бинарь описывается ЧЕСТНО: `found = Some(false)` и
    /// текстом, а не `None` («не смотрели») и не паникой.
    #[tokio::test]
    async fn a_missing_binary_is_reported_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let dep = probe_simple("ffmpeg", Some(&dir.path().join("nope")), probe_timeout()).await;
        assert_eq!(dep.found, Some(false));
        assert!(!dep.is_ready());
        assert!(dep.error.is_some(), "молчаливое «нет» бесполезно пользователю");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_present_binary_reports_its_version() {
        let _guard = crate::testing::process_count_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = crate::testing::mocks::mock_success(
            dir.path(),
            "ffmpeg",
            "ffmpeg version 8.0 Copyright (c)\n",
            "",
        )
        .unwrap();
        let dep = probe_simple("ffmpeg", Some(&path), probe_timeout()).await;
        assert_eq!(dep.found, Some(true));
        assert_eq!(dep.version.as_deref(), Some("ffmpeg version 8.0 Copyright (c)"));
    }

    /// **Кандидат `whisper` без верификации увёл бы пайплайн на CPU.**
    ///
    /// На машине разработчика в `PATH` лежит Python openai-whisper под этим
    /// именем. Проба обязана его отвергнуть — иначе правило «локальные модели
    /// только на GPU» нарушалось бы молча, ещё до всякого гарда.
    #[tokio::test]
    #[cfg(unix)]
    async fn the_python_whisper_in_path_is_rejected() {
        use crate::testing::mocks::{mock_whisper, WhisperMock};
        let _guard = crate::testing::process_count_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = mock_whisper(dir.path(), "whisper", &WhisperMock::foreign_cli()).unwrap();

        let (dep, flags, metal) = probe_whisper(Some(&path), probe_timeout()).await;
        assert_eq!(dep.found, Some(false), "чужой CLI принят за whisper.cpp");
        assert!(dep.error.unwrap().contains("openai-whisper"));
        // Флаги чужого CLI НЕ подхватываются: отвергнутый кандидат оставляет
        // отчёт ровно таким, каким он был бы без whisper вовсе.
        assert_eq!(flags, WhisperFlags::default());
        assert_eq!(metal, None, "у отвергнутого кандидата про Metal ничего не известно");
    }

    /// ПОРЯДОК КАНДИДАТОВ: `whisper-cli` выигрывает у `whisper`, даже когда
    /// оба лежат в `PATH`.
    ///
    /// Ровно эта ситуация и есть на машине разработчика: brew поставил
    /// `whisper-cli`, а Python — `whisper`. Переставь их местами в списке — и
    /// пайплайн ушёл бы на CPU-реализацию с другим CLI.
    #[tokio::test]
    #[cfg(unix)]
    async fn the_candidate_order_prefers_whisper_cli_over_plain_whisper() {
        use crate::testing::mocks::{mock_whisper, WhisperMock};
        let _env = crate::testing::env_guard();
        let _procs = crate::testing::process_count_guard();
        let dir = tempfile::tempdir().unwrap();
        mock_whisper(dir.path(), "whisper", &WhisperMock::foreign_cli()).unwrap();
        mock_whisper(dir.path(), "whisper-cli", &WhisperMock::default()).unwrap();

        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let (dep, _flags, metal) = probe_whisper(None, probe_timeout()).await;
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert_eq!(dep.found, Some(true), "{:?}", dep.error);
        assert_eq!(dep.name, "whisper-cli", "выбран не тот кандидат");
        assert_eq!(metal, Some(true));
    }

    /// А если в `PATH` ТОЛЬКО Python-whisper — плагин честно говорит «нет
    /// whisper.cpp», а не берёт что попало.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_path_with_only_the_python_whisper_reports_nothing_found() {
        use crate::testing::mocks::{mock_whisper, WhisperMock};
        let _env = crate::testing::env_guard();
        let _procs = crate::testing::process_count_guard();
        let dir = tempfile::tempdir().unwrap();
        mock_whisper(dir.path(), "whisper", &WhisperMock::foreign_cli()).unwrap();

        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let (dep, _flags, _metal) = probe_whisper(None, probe_timeout()).await;
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        assert_eq!(dep.found, Some(false));
        let report = DepsReport {
            probed: true,
            whisper: dep,
            ..Default::default()
        };
        assert_eq!(
            report.transcribe_blocker(),
            Some("whisper_unavailable"),
            "лестница 409 обязана назвать именно whisper"
        );
    }

    /// Настоящий whisper.cpp опознаётся, флаги и Metal приезжают из справки.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_real_whisper_cli_is_recognised_with_its_flags() {
        use crate::testing::mocks::{mock_whisper, WhisperMock};
        let _guard = crate::testing::process_count_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = mock_whisper(dir.path(), "whisper-cli", &WhisperMock::default()).unwrap();

        let (dep, flags, metal) = probe_whisper(Some(&path), probe_timeout()).await;
        assert_eq!(dep.found, Some(true), "{:?}", dep.error);
        assert_eq!(dep.version.as_deref(), Some("whisper.cpp version: 1.9.2"));
        assert!(flags.output_json_full && flags.processors);
        assert_eq!(metal, Some(true), "устройство Metal видно в справке");
    }

    /// **Проба и гард смотрят на РАЗНЫЕ строки.**
    ///
    /// Совпади они — правило «только GPU» держалось бы на уговоре: CPU-прогон
    /// печатает `ggml_metal_device_init:` и прошёл бы гард молча. Тест ловит
    /// ровно это: одна и та же строка даёт «устройство есть» пробе и «GPU не
    /// подтверждён» гарду.
    #[test]
    fn the_probe_and_the_guard_look_at_different_lines() {
        let always = crate::testing::mocks::GGML_ALWAYS_LINES.join("\n");
        assert!(
            always.lines().any(|l| l.trim_start().starts_with(GGML_DEVICE_NEEDLE)),
            "проба обязана видеть устройство"
        );
        assert!(
            !crate::pipeline::whisper::metal_confirmed(&always),
            "гард обязан НЕ поверить всегдашним строкам"
        );
        assert!(crate::pipeline::whisper::metal_confirmed(
            crate::testing::mocks::GGML_METAL_INIT_LINE
        ));
        assert_ne!(GGML_DEVICE_NEEDLE, crate::pipeline::whisper::METAL_NEEDLE);
    }

    /// Полная проба пустого окружения: плагин стартует, отчёт честен.
    #[tokio::test]
    async fn probing_an_empty_environment_says_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MeetConfig {
            config_path: dir.path().join("meet.toml"),
            cache_dir: dir.path().join("cache"),
            db_path: dir.path().join("cache/meet.db"),
            models_dir: dir.path().join("cache/models"),
            ffmpeg_path: Some(dir.path().join("no-ffmpeg")),
            ffprobe_path: Some(dir.path().join("no-ffprobe")),
            whisper_path: Some(dir.path().join("no-whisper")),
            chrome_path: Some(dir.path().join("no-chrome")),
            ..MeetConfig::default()
        };
        let report = probe_all(&cfg).await;
        assert!(report.probed);
        for dep in [&report.ffmpeg, &report.ffprobe, &report.whisper, &report.chrome] {
            assert_eq!(dep.found, Some(false), "{}", dep.name);
        }
        assert!(report.models.is_empty());
        assert!(!report.model_ready);
        assert!(!report.can_transcribe());
        assert_eq!(report.record_blocker(), Some("chrome_unavailable"));
    }
}
