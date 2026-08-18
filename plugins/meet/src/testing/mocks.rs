//! Моки внешних бинарей: ffmpeg, ffprobe, whisper.cpp, claude.
//!
//! # Почему моки, а не настоящие бинари
//!
//! Настоящий `whisper-cli` есть не на каждой машине сборки, первый его запуск
//! стоит 12,5 секунды на `ggml_metal_library_init` (измерено в Ф0), а вывод
//! меняется от версии к версии. Тест, зависящий от всего этого, ловил бы не
//! наши ошибки, а чужие обновления.
//!
//! Приём взят из `claude-runner`: мок — это shell-скрипт, созданный в
//! `TempDir` с `chmod 0755`, путь к которому подсовывается через конфиг
//! (`whisper_path`, `ffmpeg_path`, …) или через `PATH`. Ничего, кроме
//! отданного каталога, моки не трогают.
//!
//! # ТРИ ВАРИАНТА НА КАЖДЫЙ БИНАРЬ
//!
//! | вариант | что проверяет |
//! |---|---|
//! | успех | пайплайн доходит до конца и видит результат на диске |
//! | падение со stderr | **хвост stderr доезжает до пользователя** — в `meeting_events` и в кадр `error`, а не теряется в логе сервера |
//! | зависание | срабатывает таймаут и процесс УБИВАЕТСЯ, а тест не висит |
//!
//! # `exec sleep`, а не `sleep`
//!
//! Зависание изображается строкой `exec sleep N`, и это не стилистика: без
//! `exec` оболочка остаётся родителем, `kill` по её pid убивает только её, а
//! `sleep` остаётся сиротой и продолжает держать pipe stderr открытым — то
//! есть чтение вывода не заканчивается, и тест зависает ровно там, где
//! проверял, что не зависнет. `exec` заменяет оболочку процессом `sleep`,
//! поэтому убивается именно он.

use std::path::{Path, PathBuf};

/// Строка `--version`, по которой опознаётся настоящий whisper.cpp.
///
/// Проба обязана верифицировать кандидата: под именем `whisper` в PATH этой
/// машины лежит Python **openai-whisper**, у которого другой CLI, другие
/// модели и CPU по умолчанию (измерено в Ф0).
pub const WHISPER_VERSION_LINE: &str = "whisper.cpp version: 1.9.2";

/// Строки, которые ggml печатает ВСЕГДА — в том числе с `-ng` и даже на
/// `--help` (измерено в Ф0).
///
/// Существуют здесь ровно затем, чтобы негативный тест Metal-гарда был
/// честным: гард вида `contains("ggml_metal")` на них сработал бы и пропустил
/// CPU-прогон.
pub const GGML_ALWAYS_LINES: [&str; 3] = [
    "ggml_metal_device_init: GPU name:   MTL0 (Apple M1 Max)",
    "ggml_metal_library_init: using embedded metal library",
    "ggml_metal_rsets_init: reusing residency sets",
];

/// Строка, которую печатает ТОЛЬКО реальный прогон на GPU.
pub const GGML_METAL_INIT_LINE: &str = "ggml_metal_init: allocating";

/// Флаги, которые `whisper-cli --help` печатает в версии 1.9.2.
pub const WHISPER_HELP_FLAGS: [&str; 9] = [
    "-p N,     --processors N",
    "-bs N,    --beam-size N",
    "-fa,      --flash-attn",
    "-nfa,     --no-flash-attn",
    "-ojf,     --output-json-full",
    "-pp,      --print-progress",
    "-of FNAME, --output-file FNAME",
    "-ng,      --no-gpu",
    "--vad, -vm FNAME --vad-model FNAME",
];

/// Пишет исполняемый скрипт-мок и возвращает путь к нему.
///
/// Права ровно `0755`: без бита исполнения `Command::new` отвечает
/// `Permission denied`, и тест падал бы не там, где ошибка.
#[cfg(unix)]
pub fn write_mock_bin(dir: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let mut script = String::from("#!/bin/sh\n");
    script.push_str(body);
    if !script.ends_with('\n') {
        script.push('\n');
    }
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// Заглушка вне unix: моки — shell-скрипты, и на других системах тесты,
/// которым они нужны, просто не запускаются.
#[cfg(not(unix))]
pub fn write_mock_bin(_dir: &Path, _name: &str, _body: &str) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "моки внешних бинарей есть только на unix",
    ))
}

/// Экранирует текст для вставки в одинарные кавычки shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Мок, который печатает заданное и выходит с нулём.
pub fn mock_success(
    dir: &Path,
    name: &str,
    stdout: &str,
    stderr: &str,
) -> std::io::Result<PathBuf> {
    write_mock_bin(
        dir,
        name,
        &format!(
            "printf '%s' {}\nprintf '%s' {} >&2\nexit 0\n",
            sh_quote(stdout),
            sh_quote(stderr)
        ),
    )
}

/// Мок, который пишет текст в stderr и выходит НЕНУЛЕВЫМ кодом.
///
/// Ради него всё и затевалось: тест проверяет, что хвост этого текста доехал
/// до пользователя, а не остался в логе сервера.
pub fn mock_failure(dir: &Path, name: &str, stderr: &str, code: i32) -> std::io::Result<PathBuf> {
    write_mock_bin(
        dir,
        name,
        &format!("printf '%s\\n' {} >&2\nexit {code}\n", sh_quote(stderr)),
    )
}

/// Мок, который спит дольше любого разумного таймаута.
pub fn mock_hang(dir: &Path, name: &str, secs: u64) -> std::io::Result<PathBuf> {
    write_mock_bin(dir, name, &format!("exec sleep {secs}\n"))
}

// --- ffmpeg ------------------------------------------------------------------

/// Пролог разбора argv ffmpeg: первый `-i` и последний аргумент (выход).
const FFMPEG_ARGV_PROLOGUE: &str = r#"
FIRST_IN=""
OUT=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-i" ] && [ -z "$FIRST_IN" ]; then FIRST_IN="$a"; fi
  OUT="$a"
  prev="$a"
done
"#;

/// Успешный ffmpeg: КОПИРУЕТ первый вход в выходной файл.
///
/// Копирует, а не пишет заглушку, намеренно: `work/mix.wav` дальше читают как
/// настоящий WAV (длительность, сэмплы), и мок, оставляющий четыре байта,
/// увёл бы отладку в разбор «битого» файла вместо проверяемой логики.
pub fn mock_ffmpeg_success(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    write_mock_bin(
        dir,
        name,
        &format!(
            "{FFMPEG_ARGV_PROLOGUE}\n\
             echo 'ffmpeg version 8.0 (mock)' >&2\n\
             if [ -n \"$OUT\" ]; then\n\
             \x20 if [ -n \"$FIRST_IN\" ] && [ -f \"$FIRST_IN\" ]; then cp \"$FIRST_IN\" \"$OUT\"; else printf 'mock' > \"$OUT\"; fi\n\
             fi\n\
             exit 0\n"
        ),
    )
}

/// Падающий ffmpeg: длинный вывод плюс осмысленная последняя строка.
///
/// Длинный намеренно: хвост обязан быть хвостом, а не первыми строками —
/// причина падения ffmpeg всегда в конце вывода.
pub fn mock_ffmpeg_failure(dir: &Path, name: &str, message: &str) -> std::io::Result<PathBuf> {
    write_mock_bin(
        dir,
        name,
        &format!(
            "i=1\nwhile [ $i -le 40 ]; do echo \"frame= $i fps=0.0 q=-0.0 size=0kB\" >&2; i=$((i+1)); done\n\
             printf '%s\\n' {} >&2\nexit 1\n",
            sh_quote(message)
        ),
    )
}

/// Зависающий ffmpeg.
pub fn mock_ffmpeg_hang(dir: &Path, name: &str, secs: u64) -> std::io::Result<PathBuf> {
    mock_hang(dir, name, secs)
}

/// ffprobe, отвечающий заданной длительностью в секундах.
pub fn mock_ffprobe(dir: &Path, name: &str, duration_secs: f64) -> std::io::Result<PathBuf> {
    write_mock_bin(dir, name, &format!("echo '{duration_secs}'\nexit 0\n"))
}

// --- whisper -----------------------------------------------------------------

/// Сегмент, который мок whisper положит в `<output>.json`.
#[derive(Debug, Clone)]
pub struct MockSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Потокенные метки. Пусто — мок разложит слова текста равномерно.
    ///
    /// В настоящем `-ojf` метки ПОТОКЕННЫЕ (subword: «Кол», «лег», «и»), и
    /// склейка их в слова — работа `speakers.rs`. Мок умеет отдавать и такие:
    /// достаточно перечислить куски явно.
    pub tokens: Vec<(u64, u64, String)>,
}

impl MockSegment {
    pub fn new(start_ms: u64, end_ms: u64, text: &str) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.to_string(),
            tokens: Vec::new(),
        }
    }

    /// Сегмент с явными потокенными метками.
    pub fn with_tokens(start_ms: u64, end_ms: u64, text: &str, tokens: &[(u64, u64, &str)]) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.to_string(),
            tokens: tokens
                .iter()
                .map(|(a, b, t)| (*a, *b, (*t).to_string()))
                .collect(),
        }
    }
}

/// Как ведёт себя мок whisper.
#[derive(Debug, Clone)]
pub struct WhisperMock {
    /// Ответ на `--version`. Пусто — кандидат «не наш» (так выглядит Python
    /// openai-whisper).
    pub version: String,
    /// Что печатает `--help`.
    pub help_flags: Vec<String>,
    /// Печатать ли `ggml_metal_init:` — то есть считать ли на GPU.
    pub metal_init: bool,
    /// Печатать ли всегдашние `ggml_metal_*_init:`.
    pub always_lines: bool,
    /// Печатать ли строки, прямо объявляющие расчёт на процессоре
    /// (`whisper_backend_init_gpu: no GPU found`).
    ///
    /// Выключается, чтобы проверить ВТОРОЙ путь гарда — окно ожидания. Есть
    /// сборки, которые про бэкенд не печатают ничего, и на них «убить по
    /// маркеру» не сработает вовсе.
    pub cpu_markers: bool,
    /// Что положить в `<output>.json`.
    pub segments: Vec<MockSegment>,
    /// Ответ, зависящий от ВХОДНОГО файла: `(подстрока пути, сегменты)`.
    ///
    /// Нужен пер-трековой расшифровке: она запускает whisper по разу на
    /// дорожку, и мок, отвечающий всем одинаково, не отличил бы «прогнали три
    /// файла» от «прогнали один трижды». Первое совпадение подстроки в
    /// значении `-f` побеждает; не совпало ничего — отдаются [`Self::segments`].
    pub per_input: Vec<(String, Vec<MockSegment>)>,
    /// Код выхода прогона.
    pub exit_code: i32,
    /// Текст в stderr перед выходом.
    pub stderr: String,
    /// Сколько спать вместо работы (`0` — не спать).
    pub sleep_secs: u64,
}

impl Default for WhisperMock {
    fn default() -> Self {
        Self {
            version: WHISPER_VERSION_LINE.to_string(),
            help_flags: WHISPER_HELP_FLAGS.iter().map(|s| s.to_string()).collect(),
            metal_init: true,
            always_lines: true,
            cpu_markers: true,
            segments: vec![MockSegment::new(0, 2_000, " Коллеги, начинаем.")],
            per_input: Vec::new(),
            exit_code: 0,
            stderr: String::new(),
            sleep_secs: 0,
        }
    }
}

impl WhisperMock {
    /// Прогон на CPU: всегдашние строки есть, `ggml_metal_init:` нет.
    ///
    /// **Главный негативный случай фазы.** Гард обязан признать это CPU-прогоном
    /// и убить процесс: проверка `contains("ggml_metal")` здесь сработала бы, а
    /// значит пропустила бы расчёт на процессоре — то есть молча нарушила бы
    /// правило проекта «локальные модели только на GPU».
    pub fn cpu_run() -> Self {
        Self {
            metal_init: false,
            ..Self::default()
        }
    }

    /// Тот же CPU-прогон, но ДОЛГИЙ: доказывает, что гард именно убивает
    /// процесс, а не дожидается его конца.
    pub fn cpu_run_that_keeps_going(secs: u64) -> Self {
        Self {
            metal_init: false,
            sleep_secs: secs,
            ..Self::default()
        }
    }

    /// Прогон, который НИЧЕГО не говорит про бэкенд: ни `ggml_metal_init:`,
    /// ни маркеров процессора — только всегдашние строки.
    ///
    /// Самый неприятный случай: убивать не по чему, и спасает только окно
    /// ожидания.
    pub fn silent_about_the_backend() -> Self {
        Self {
            metal_init: false,
            cpu_markers: false,
            ..Self::default()
        }
    }

    /// Не whisper.cpp: `--version` отвечает чужой строкой.
    ///
    /// Так выглядит Python openai-whisper, лежащий в PATH под именем
    /// `whisper`.
    pub fn foreign_cli() -> Self {
        Self {
            version: "20250625".to_string(),
            help_flags: vec!["--model MODEL".into(), "--output_format FORMAT".into()],
            metal_init: false,
            always_lines: false,
            ..Self::default()
        }
    }

    /// Падение с текстом в stderr.
    pub fn failing(message: &str) -> Self {
        Self {
            exit_code: 1,
            stderr: message.to_string(),
            segments: Vec::new(),
            ..Self::default()
        }
    }

    /// Зависание.
    pub fn hanging(secs: u64) -> Self {
        Self {
            sleep_secs: secs,
            segments: Vec::new(),
            ..Self::default()
        }
    }

    /// JSON, который мок положит в `<output>.json`.
    ///
    /// Форма дословно повторяет вывод `-ojf` версии 1.9.2 (см. страницу
    /// `forge-meet-phase0-whisper`): у каждого сегмента `offsets` в
    /// миллисекундах и массив `tokens` со СВОИМИ `offsets`.
    pub fn output_json(&self) -> String {
        Self::json_for(&self.segments)
    }

    /// Тот же JSON для произвольного набора сегментов.
    pub fn json_for(segments: &[MockSegment]) -> String {
        let transcription: Vec<serde_json::Value> = segments
            .iter()
            .map(|s| {
                let tokens: Vec<serde_json::Value> = if s.tokens.is_empty() {
                    derive_tokens(s)
                } else {
                    s.tokens
                        .iter()
                        .map(|(a, b, t)| token_json(t, *a, *b))
                        .collect()
                };
                serde_json::json!({
                    "timestamps": {
                        "from": timestamp(s.start_ms),
                        "to": timestamp(s.end_ms),
                    },
                    "offsets": {"from": s.start_ms, "to": s.end_ms},
                    "text": s.text,
                    "tokens": tokens,
                })
            })
            .collect();

        serde_json::to_string_pretty(&serde_json::json!({
            // Побайтово одинаково у GPU- и CPU-прогона — поэтому для гарда
            // НЕ ГОДИТСЯ (проверено сравнением файлов в Ф0).
            "systeminfo": "MTL : EMBED_LIBRARY = 1 | BF16 = 1 | ACCELERATE = 1 | NEON = 1 |",
            "model": {"type": "large"},
            "params": {"model": "ggml-large-v3-turbo-q5_0.bin", "language": "ru"},
            "result": {"language": "ru"},
            "transcription": transcription,
        }))
        .unwrap_or_default()
    }
}

/// Служебный токен whisper — такие лежат в `tokens[]` с нулевой длительностью
/// и обязаны отфильтровываться потребителем.
const SERVICE_TOKEN: &str = "[_BEG_]";

fn timestamp(ms: u64) -> String {
    let (h, m, s, milli) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1_000 % 60, ms % 1_000);
    format!("{h:02}:{m:02}:{s:02},{milli:03}")
}

fn token_json(text: &str, from: u64, to: u64) -> serde_json::Value {
    serde_json::json!({
        "text": text,
        "timestamps": {"from": timestamp(from), "to": timestamp(to)},
        "offsets": {"from": from, "to": to},
        "id": 1000,
        "p": 0.98,
    })
}

/// Раскладывает слова сегмента равномерно и добавляет служебный токен —
/// такой же, как в настоящем выводе.
fn derive_tokens(s: &MockSegment) -> Vec<serde_json::Value> {
    let words: Vec<&str> = s.text.split_whitespace().collect();
    let mut out = vec![token_json(SERVICE_TOKEN, s.start_ms, s.start_ms)];
    if words.is_empty() {
        return out;
    }
    let span = s.end_ms.saturating_sub(s.start_ms).max(1);
    let step = span / words.len() as u64;
    for (i, w) in words.iter().enumerate() {
        let from = s.start_ms + step * i as u64;
        let to = if i + 1 == words.len() {
            s.end_ms
        } else {
            from + step
        };
        // Ведущий пробел — признак начала слова в выводе whisper.
        out.push(token_json(&format!(" {w}"), from, to));
    }
    out
}

/// Пишет мок whisper.cpp.
///
/// Скрипт понимает `--version`, `--help` и обычный прогон, потому что все три
/// вызова делает один и тот же код: проба `deps.rs` спрашивает версию и флаги,
/// пайплайн запускает расшифровку. Разными файлами это разъехалось бы.
pub fn mock_whisper(dir: &Path, name: &str, m: &WhisperMock) -> std::io::Result<PathBuf> {
    let mut body = String::new();

    // Разбор argv: нам нужны префикс вывода (`-of`) и ВХОДНОЙ ФАЙЛ (`-f`).
    // Вход нужен пер-трековому пути: по нему мок отвечает разным текстом на
    // разные дорожки — иначе тест не отличил бы три прогона от одного.
    body.push_str(
        r#"
OF=""
FIN=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-of" ]; then OF="$a"; fi
  if [ "$prev" = "-f" ]; then FIN="$a"; fi
  prev="$a"
done
"#,
    );

    // Всегдашние строки печатаются ДО всего, включая --help и --version:
    // ровно так ведёт себя настоящий бинарь.
    if m.always_lines {
        for line in GGML_ALWAYS_LINES {
            body.push_str(&format!("echo {} >&2\n", sh_quote(line)));
        }
    }

    body.push_str("case \"$1\" in\n");
    if m.version.is_empty() {
        body.push_str("  --version) exit 1 ;;\n");
    } else {
        body.push_str(&format!(
            "  --version) echo {}; exit 0 ;;\n",
            sh_quote(&m.version)
        ));
    }
    body.push_str("  --help|-h)\n");
    for flag in &m.help_flags {
        body.push_str(&format!("    echo {} >&2\n", sh_quote(flag)));
    }
    body.push_str("    exit 0 ;;\n");
    body.push_str("esac\n");

    if m.metal_init {
        body.push_str(&format!("echo {} >&2\n", sh_quote(GGML_METAL_INIT_LINE)));
        body.push_str("echo 'whisper_backend_init_gpu: using MTL0 backend' >&2\n");
    } else if m.cpu_markers {
        body.push_str("echo 'whisper_backend_init_gpu: no GPU found' >&2\n");
        body.push_str("echo 'whisper_backend_init: using BLAS backend' >&2\n");
    }

    if m.sleep_secs > 0 {
        body.push_str(&format!("exec sleep {}\n", m.sleep_secs));
    }

    if !m.stderr.is_empty() {
        body.push_str(&format!("printf '%s\\n' {} >&2\n", sh_quote(&m.stderr)));
    }

    if m.exit_code == 0 && (!m.segments.is_empty() || !m.per_input.is_empty()) {
        body.push_str("if [ -n \"$OF\" ]; then\n");
        if m.per_input.is_empty() {
            body.push_str("cat > \"$OF.json\" <<'FORGE_MOCK_JSON'\n");
            body.push_str(&m.output_json());
            body.push_str("\nFORGE_MOCK_JSON\n");
        } else {
            body.push_str("case \"$FIN\" in\n");
            for (needle, segments) in &m.per_input {
                body.push_str(&format!("  *{needle}*)\ncat > \"$OF.json\" <<'FORGE_MOCK_JSON'\n"));
                body.push_str(&WhisperMock::json_for(segments));
                body.push_str("\nFORGE_MOCK_JSON\n    ;;\n");
            }
            body.push_str("  *)\ncat > \"$OF.json\" <<'FORGE_MOCK_JSON'\n");
            body.push_str(&m.output_json());
            body.push_str("\nFORGE_MOCK_JSON\n    ;;\nesac\n");
        }
        body.push_str("fi\n");
    }

    body.push_str(&format!("exit {}\n", m.exit_code));
    write_mock_bin(dir, name, &body)
}

// --- claude ------------------------------------------------------------------

/// Мок Claude CLI, отвечающий потоком `stream-json`.
///
/// Формат тот же, что у `forge-claude-runner`: строки NDJSON, среди них
/// `content_block_delta` с текстом и завершающий `result`.
pub fn mock_claude_success(dir: &Path, name: &str, text: &str) -> std::io::Result<PathBuf> {
    let mut body = String::new();
    // Промпт приходит через stdin и обязан быть вычитан: незакрытый pipe у
    // 80 КБ транскрипта означал бы `EPIPE` на стороне вызывающего.
    body.push_str("cat > /dev/null\n");
    for chunk in text.split_inclusive(' ') {
        let line = serde_json::json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": chunk},
        })
        .to_string();
        body.push_str(&format!("printf '%s\\n' {}\n", sh_quote(&line)));
    }
    let result = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": text,
    })
    .to_string();
    body.push_str(&format!("printf '%s\\n' {}\nexit 0\n", sh_quote(&result)));
    write_mock_bin(dir, name, &body)
}

/// Падающий Claude CLI.
pub fn mock_claude_failure(dir: &Path, name: &str, message: &str) -> std::io::Result<PathBuf> {
    write_mock_bin(
        dir,
        name,
        &format!(
            "cat > /dev/null\nprintf '%s\\n' {} >&2\nexit 2\n",
            sh_quote(message)
        ),
    )
}

/// Зависающий Claude CLI.
pub fn mock_claude_hang(dir: &Path, name: &str, secs: u64) -> std::io::Result<PathBuf> {
    write_mock_bin(dir, name, &format!("cat > /dev/null\nexec sleep {secs}\n"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    /// Замок обязателен: запуск мока — это дочерний процесс, а инвариант
    /// «`init` не запускает процессов» считает их у всего тестового процесса.
    fn run(path: &Path, args: &[&str]) -> (i32, String, String) {
        let _guard = crate::testing::process_count_guard();
        let out = Command::new(path).args(args).output().expect("мок не запустился");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Мок обязан быть ИСПОЛНЯЕМЫМ: без бита `x` тест падал бы на
    /// `Permission denied`, и искали бы его не там.
    #[test]
    fn a_mock_is_written_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = mock_success(dir.path(), "hello", "привет", "").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "мок не исполняемый: {mode:o}");
        let (code, stdout, _) = run(&path, &[]);
        assert_eq!(code, 0);
        assert_eq!(stdout, "привет");
    }

    /// Падение обязано быть видно ИМЕННО в stderr и ИМЕННО ненулевым кодом.
    #[test]
    fn a_failing_mock_writes_to_stderr_and_exits_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock_failure(dir.path(), "boom", "Invalid data found", 3).unwrap();
        let (code, stdout, stderr) = run(&path, &[]);
        assert_eq!(code, 3);
        assert!(stdout.is_empty());
        assert!(stderr.contains("Invalid data found"), "{stderr}");
    }

    /// Зависание обязано быть НАСТОЯЩИМ: скрипт не выходит сам.
    ///
    /// И `exec` обязателен: без него `kill` по оболочке оставил бы `sleep`
    /// сиротой, держащим pipe — то есть тест таймаута висел бы вечно.
    #[test]
    fn a_hanging_mock_replaces_itself_with_sleep() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock_hang(dir.path(), "hang", 30).unwrap();
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(src.contains("exec sleep 30"), "{src}");

        let _guard = crate::testing::process_count_guard();
        let mut child = Command::new(&path).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(child.try_wait().unwrap().is_none(), "мок вышел сам — это не зависание");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// ffmpeg-мок кладёт выход туда, куда просили, и берёт содержимое входа.
    #[test]
    fn the_ffmpeg_mock_produces_the_requested_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.wav");
        std::fs::write(&input, b"RIFF....WAVEfake").unwrap();
        let out = dir.path().join("mix.wav");
        let path = mock_ffmpeg_success(dir.path(), "ffmpeg").unwrap();

        let (code, _, stderr) = run(
            &path,
            &["-i", input.to_str().unwrap(), "-c:a", "pcm_s16le", out.to_str().unwrap()],
        );
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(std::fs::read(&out).unwrap(), b"RIFF....WAVEfake");
    }

    /// У падающего ffmpeg причина стоит В КОНЦЕ вывода — потому хвост и нужен.
    #[test]
    fn the_ffmpeg_failure_puts_the_reason_at_the_very_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock_ffmpeg_failure(dir.path(), "ffmpeg", "raw/track-0.wav: Invalid data").unwrap();
        let (code, _, stderr) = run(&path, &["-i", "x", "y"]);
        assert_eq!(code, 1);
        let last = stderr.lines().last().unwrap();
        assert_eq!(last, "raw/track-0.wav: Invalid data");
        assert!(stderr.lines().count() > 20, "вывод обязан быть длинным");
    }

    /// Мок whisper отвечает на все три вызова, которые делает плагин.
    #[test]
    fn the_whisper_mock_answers_version_help_and_a_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock_whisper(dir.path(), "whisper-cli", &WhisperMock::default()).unwrap();

        let (code, stdout, _) = run(&path, &["--version"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("whisper.cpp version"), "{stdout}");

        // `--help` печатается в STDERR, не в stdout (измерено в Ф0).
        let (code, stdout, stderr) = run(&path, &["--help"]);
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "help ушёл в stdout: {stdout}");
        assert!(stderr.contains("--output-json-full"), "{stderr}");

        let out = dir.path().join("res");
        let (code, _, stderr) = run(&path, &["-m", "model.bin", "-f", "a.wav", "-of", out.to_str().unwrap()]);
        assert_eq!(code, 0);
        assert!(stderr.contains(GGML_METAL_INIT_LINE), "{stderr}");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap()).unwrap();
        assert_eq!(json["transcription"][0]["offsets"]["to"], 2_000);
        assert!(json["transcription"][0]["tokens"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[_"), "служебный токен обязан быть в выводе");
    }

    /// CPU-прогон печатает всегдашние строки — И НИ ОДНОЙ `ggml_metal_init:`.
    /// Ровно на этой разнице стоит Metal-гард.
    #[test]
    fn the_cpu_mock_prints_the_always_lines_but_never_ggml_metal_init() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock_whisper(dir.path(), "whisper-cli", &WhisperMock::cpu_run()).unwrap();
        let (code, _, stderr) = run(&path, &["-m", "m", "-f", "a.wav"]);
        assert_eq!(code, 0);
        for line in GGML_ALWAYS_LINES {
            assert!(stderr.contains(line), "нет всегдашней строки: {stderr}");
        }
        assert!(stderr.contains("ggml_metal"), "наивный гард обязан сработать — иначе тест ничего не доказывает");
        assert!(
            !stderr.contains("ggml_metal_init:"),
            "CPU-прогон не печатает ggml_metal_init: {stderr}"
        );
    }

    /// Чужой CLI под именем `whisper` опознаётся по `--version`.
    #[test]
    fn the_foreign_cli_mock_answers_version_with_something_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock_whisper(dir.path(), "whisper", &WhisperMock::foreign_cli()).unwrap();
        let (_, stdout, _) = run(&path, &["--version"]);
        assert!(!stdout.contains("whisper.cpp"), "{stdout}");
    }

    /// Claude-мок обязан вычитать stdin: промпт идёт именно туда.
    #[test]
    fn the_claude_mock_drains_stdin_and_streams_json() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = mock_claude_success(dir.path(), "claude", "краткое резюме").unwrap();
        let _guard = crate::testing::process_count_guard();
        let mut child = Command::new(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all("огромный транскрипт".as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("content_block_delta"), "{stdout}");
        assert!(stdout.lines().last().unwrap().contains("\"result\""), "{stdout}");
    }

    /// Кавычки и апострофы в тексте не ломают скрипт.
    #[test]
    fn quoting_survives_apostrophes_and_double_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let nasty = "он сказал: \"не надо\", а я 'подумал'";
        let path = mock_failure(dir.path(), "q", nasty, 1).unwrap();
        let (_, _, stderr) = run(&path, &[]);
        assert!(stderr.contains(nasty), "{stderr}");
    }
}
