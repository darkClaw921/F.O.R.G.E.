//! **СВОД ИНВАРИАНТОВ ПЛАГИНА ВСТРЕЧ.** Один модуль, по которому видно, что не
//! забыто ничего.
//!
//! # Зачем свод, если каждый инвариант и так покрыт в своей фазе
//!
//! Плагин собран из четырнадцати обещаний, ни одно из которых не видно по
//! коду: «`init` не запускает процессов», «`status` не падает никогда»,
//! «слот whisper строго один», «провал саммари сохраняет транскрипт».
//! Проверки на них написаны — но лежат в девяти файлах и семи фазах, и
//! ответить на вопрос «а это ещё проверяется?» можно было только чтением всех
//! девяти. Через полгода на такой вопрос отвечают «наверное».
//!
//! Здесь список сведён в одну таблицу [`INVARIANTS`], и каждая строка
//! проверяется ТРИЖДЫ:
//!
//! 1. **тест существует** — файл на месте, функция с таким именем есть;
//! 2. **тест исполняется** — на нём нет `#[ignore]`, то есть он идёт в обычном
//!    прогоне, а не «когда-нибудь руками»;
//! 3. **тест всё ещё говорит то самое** — в теле есть опорная строка
//!    (`marker`), ради которой он написан. Без неё выпотрошенный тест —
//!    оставили имя, вынули утверждение — прошёл бы свод незамеченным.
//!
//! Свод намеренно НЕ дублирует сами проверки: копия инварианта разошлась бы с
//! оригиналом при первой же правке, и одна из двух начала бы врать. Здесь
//! проверяется НАЛИЧИЕ И ЖИВОСТЬ проверок, а не их предмет.
//!
//! # Отдельно: ГЕРМЕТИЧНОСТЬ
//!
//! Негерметичный тест не падает и ничего не сообщает — он молча открывает
//! `~/.forge/meet` живого пользователя, где лежат записи настоящих встреч:
//! чужие голоса, расшифровки, саммари. `reconcile_on_boot` на старте пройдётся
//! по этим записям и переведёт незакрытые в `failed`, а ретенция вычистит
//! старые по дисковому бюджету. Это не гипотеза, а прямое следствие дефолтных
//! путей: `MeetConfig::default()` указывает ровно туда.
//!
//! Поэтому [`every_config_in_tests_is_hermetic`] проходит по ВСЕМУ исходнику
//! крейта и требует, чтобы каждый тестовый `MeetConfig { … }` уводил
//! `config_path`, `cache_dir` и `db_path` во временный каталог. Исключения
//! возможны (конфиг, который не доходит ни до диска, ни до базы), но каждое
//! записано в [`HERMETIC_EXCEPTIONS`] с причиной — и сам список проверяется на
//! протухание.
//!
//! ## Измерено, а не выведено (18.08.2026)
//!
//! Статическая проверка отвечает на вопрос «может ли тест залезть в живой
//! каталог». На вопрос «залез ли» отвечает только замер, и он сделан: снимок
//! `~/.forge/meet` и `~/.config/forge` (имя, размер и mtime каждого файла) до
//! и после полного `cargo test -p forge-meet`.
//!
//! Результат: в `~/.forge/meet` не появилось ни `meet.db`, ни `meet.toml`, ни
//! одного каталога встречи; mtime единственного лежащего там файла (веса
//! модели, 547 МиБ) не изменился. Единственная разница во всём снимке —
//! `~/.config/forge/telegram.session`, который в это время писал РАБОТАЮЩИЙ
//! devforge пользователя, а не тесты.

use std::path::{Path, PathBuf};

/// Строка свода: обещание плагина и то, чем оно проверено.
struct Invariant {
    /// Формулировка из плана — дословно, чтобы список можно было сверить
    /// построчно, не заглядывая в код.
    claim: &'static str,
    /// Файл относительно корня крейта.
    file: &'static str,
    /// Имя тестовой функции.
    test: &'static str,
    /// Опорная строка в теле теста: без неё тест утратил предмет.
    marker: &'static str,
}

/// **ЧЕТЫРНАДЦАТЬ ОБЕЩАНИЙ ПЛАГИНА.**
///
/// Порядок — как в плане фазы: список сверяется построчно глазами, и
/// перестановка строк сделала бы эту сверку невозможной.
const INVARIANTS: &[Invariant] = &[
    Invariant {
        claim: "init меньше 500 мс и не запускает процессов",
        file: "src/lib.rs",
        test: "init_is_fast_and_spawns_no_processes",
        marker: "500",
    },
    Invariant {
        claim: "плагин стартует без единого внешнего бинаря",
        file: "src/lib.rs",
        test: "the_plugin_starts_with_no_external_binaries_at_all",
        marker: "база обязана открыться",
    },
    Invariant {
        claim: "status — единственный роут, который никогда не падает",
        file: "../../tmux-web/tests/meet_smoke.rs",
        test: "the_status_route_never_fails_without_external_binaries",
        marker: "200",
    },
    Invariant {
        claim: "лестница ошибок: деп-проверка раньше 404, 404 раньше 400",
        file: "src/routes/mod.rs",
        test: "the_error_ladder_checks_dependencies_before_404_and_404_before_400",
        marker: "check_ladder",
    },
    Invariant {
        claim: "exhaustive-тест матрицы переходов статусов",
        file: "src/status.rs",
        test: "transition_matrix_is_exhaustive",
        marker: "EXPECTED",
    },
    Invariant {
        claim: "таблица reconcile_on_boot по строке на статус × наличие спасаемого звука",
        file: "src/reconcile.rs",
        test: "the_decision_table_is_complete",
        marker: "MeetStatus::ALL",
    },
    Invariant {
        claim: "дедуп двух одновременных стартов даёт ровно одну строку",
        file: "tests/repo.rs",
        test: "two_starts_on_one_room_produce_exactly_one_meeting",
        marker: "attached",
    },
    Invariant {
        claim: "Busy отдаётся быстрее 50 мс",
        file: "tests/routes_recording.rs",
        test: "the_third_simultaneous_start_is_refused_immediately",
        marker: "from_millis(50)",
    },
    Invariant {
        claim: "семафор whisper строго один",
        file: "src/hub/mod.rs",
        test: "the_hub_never_gets_more_than_one_whisper_slot",
        marker: "limits().whisper, 1",
    },
    Invariant {
        claim: "Metal-гард убивает CPU-прогон",
        file: "tests/pipeline.rs",
        test: "a_cpu_whisper_run_fails_the_meeting_with_metal_unavailable",
        marker: "metal_unavailable",
    },
    Invariant {
        claim: "провал саммари сохраняет транскрипт",
        file: "tests/summary.rs",
        test: "a_failing_claude_keeps_the_transcript_and_only_fails_the_summary",
        marker: "SummaryStatus::Failed",
    },
    Invariant {
        claim: "транскрипта нет в argv",
        file: "tests/summary.rs",
        test: "the_transcript_reaches_the_child_through_stdin_and_never_through_argv",
        marker: "argv",
    },
    Invariant {
        claim: "индекс бейджей сходится с БД после 200 случайных переходов",
        file: "tests/repo.rs",
        test: "the_badge_index_converges_with_the_database_after_200_random_transitions",
        marker: "200",
    },
    Invariant {
        claim: "Range, HEAD и 416 работают",
        file: "tests/routes_audio.rs",
        test: "a_range_past_the_end_gets_416_with_the_real_length",
        marker: "RANGE_NOT_SATISFIABLE",
    },
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Тело функции `fn <name>` целиком — от первой `{` до парной ей.
fn fn_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let at = src.find(&format!("fn {name}("))?;
    let start = src[at..].find('{')? + at;
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[start + 1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Атрибуты прямо перед объявлением функции.
fn attrs_before<'a>(src: &'a str, name: &str) -> &'a str {
    let Some(at) = src.find(&format!("fn {name}(")) else {
        return "";
    };
    let from = src[..at].rfind("\n\n").map(|p| p + 2).unwrap_or(0);
    &src[from..at]
}

/// **СВОД: у каждого обещания есть живая проверка.**
#[test]
fn every_invariant_of_the_plugin_has_a_live_test() {
    let root = crate_root();
    let mut problems: Vec<String> = Vec::new();

    for inv in INVARIANTS {
        let path = root.join(inv.file);
        let Ok(src) = std::fs::read_to_string(&path) else {
            problems.push(format!("«{}»: нет файла {}", inv.claim, inv.file));
            continue;
        };
        let Some(body) = fn_body(&src, inv.test) else {
            problems.push(format!(
                "«{}»: в {} нет функции {}",
                inv.claim, inv.file, inv.test
            ));
            continue;
        };
        let attrs = attrs_before(&src, inv.test);
        if !attrs.contains("#[test]") && !attrs.contains("#[tokio::test") {
            problems.push(format!(
                "«{}»: {} перестал быть тестом (атрибуты: {})",
                inv.claim,
                inv.test,
                attrs.trim().replace('\n', " ")
            ));
        }
        if attrs.contains("#[ignore") {
            problems.push(format!(
                "«{}»: {} помечен #[ignore] — в обычном прогоне его нет",
                inv.claim, inv.test
            ));
        }
        if !body.contains(inv.marker) {
            problems.push(format!(
                "«{}»: в теле {} больше нет опорной строки {:?} — тест мог остаться именем без утверждения",
                inv.claim, inv.test, inv.marker
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "свод инвариантов разошёлся с тестами:\n  {}",
        problems.join("\n  ")
    );
    assert_eq!(
        INVARIANTS.len(),
        14,
        "список инвариантов изменился — сверьте его с планом фазы построчно"
    );
}

// ===========================================================================
// ГЕРМЕТИЧНОСТЬ
// ===========================================================================

/// Исключения: конфиги, которым временный каталог не нужен, и почему.
///
/// Ключ — `файл:подстрока`, по которой исключение опознаётся. Причина — не
/// украшение: она и есть то, что читает следующий, увидев здесь новую строку.
const HERMETIC_EXCEPTIONS: &[(&str, &str, &str)] = &[
    (
        "src/hub/mod.rs",
        "max_concurrent_whisper: 4",
        "конфиг уходит только в MeetHub::new — ни диска, ни базы он не видит",
    ),
    (
        "src/hub/mod.rs",
        "max_concurrent_recordings: recordings",
        "то же: лимиты хаба, ввода-вывода нет",
    ),
    (
        "src/pipeline/speakers.rs",
        "speaker_min_interval_ms: 111",
        "конфиг уходит только в SpeakerConfig::from — модуль чистый, ввода-вывода \
         в нём нет вовсе",
    ),
    (
        "src/pipeline/whisper.rs",
        "assert!(vad_model_path(&cfg).is_none()",
        "смотрится только models_dir, и он во временном каталоге; ни база, ни файл \
         конфига не открываются",
    ),
    (
        "tests/chrome_bot.rs",
        "let factory = ChromeBotFactory::new(cfg)",
        "фабрика бота читает только пробы платформ; каталог кэша при этом всё \
         равно уведён во временный",
    ),
    (
        "src/config.rs",
        "hermetic_paths(dir.path())",
        "база уже герметична: hermetic_paths уводит все три пути во временный каталог",
    ),
    (
        "src/routes/config.rs",
        "crate::testing::hermetic_config(dir.path())",
        "то же самое: база — hermetic_config",
    ),
    (
        "tests/routes_config.rs",
        "MeetConfig::from_file(&config_path)",
        "конфиг читается из файла, который лежит во временном каталоге, а три пути \
         подставляются рядом явно",
    ),
];

/// Окрестность литерала — по ГРАНИЦАМ СИМВОЛОВ, а не байт.
///
/// В исходниках комментарии на русском, и слепая нарезка по байтам панует
/// посреди кириллической буквы — прямо в проверке герметичности, то есть в
/// самом неудачном месте.
fn slice_around(src: &str, pos: usize, len: usize) -> &str {
    let mut from = pos.saturating_sub(400);
    while from > 0 && !src.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (pos + len + 400).min(src.len());
    while to < src.len() && !src.is_char_boundary(to) {
        to += 1;
    }
    &src[from..to.max(from)]
}

/// Все файлы крейта, где может встретиться тестовый код.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs")
                // Сам сканер в выборку не входит: он ЦИТИРУЕТ разбираемые
                // конструкции в комментариях и в собственном коде, и без этого
                // исключения проверял бы себя.
                && p.file_name().and_then(|n| n.to_str()) != Some("invariants.rs")
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out);
    walk(&root.join("tests"), &mut out);
    out.sort();
    out
}

/// Литералы `MeetConfig { … }` вместе с позицией.
fn config_literals(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut from = 0usize;
    while let Some(at) = src[from..].find("MeetConfig {") {
        let open = from + at + "MeetConfig ".len();
        let mut depth = 0usize;
        let mut end = open;
        for (i, b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((open, src[open + 1..end].to_string()));
        from = end.max(open + 1);
    }
    out
}

/// Внутри тестового кода ли эта позиция.
///
/// Для файлов в `tests/` — всегда. Для `src/**` — только после `mod tests {`.
fn in_test_code(path: &Path, src: &str, pos: usize) -> bool {
    if path.components().any(|c| c.as_os_str() == "tests") {
        return true;
    }
    match src.find("mod tests {") {
        Some(at) => pos > at,
        None => false,
    }
}

/// **ГЕРМЕТИЧНОСТЬ: ни один тест не смотрит в каталог живых записей.**
#[test]
fn every_config_in_tests_is_hermetic() {
    let root = crate_root();
    const NEEDED: [&str; 3] = ["config_path", "cache_dir", "db_path"];
    /// Базы `..X`, которые уже увели пути во временный каталог.
    const HERMETIC_BASES: [&str; 5] = [
        "hermetic_config(",
        "hermetic_paths(",
        "test_config(",
        "cfg",
        "base",
    ];

    let mut leaks: Vec<String> = Vec::new();
    let mut used_exceptions: Vec<usize> = Vec::new();

    for path in rust_sources(&root) {
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        for (pos, body) in config_literals(&src) {
            if !in_test_code(&path, &src, pos) {
                continue;
            }
            let has_all = NEEDED
                .iter()
                .all(|k| body.contains(&format!("{k}:")));
            let base_ok = body
                .split("..")
                .nth(1)
                .map(|b| HERMETIC_BASES.iter().any(|h| b.trim_start().starts_with(h)))
                .unwrap_or(false);
            if has_all || base_ok {
                continue;
            }
            // Исключение ищется В ОКРУГЕ литерала, а не по всему файлу: иначе
            // одна законная строка накрыла бы собой весь файл.
            let around = slice_around(&src, pos, body.len());
            match HERMETIC_EXCEPTIONS
                .iter()
                .position(|(f, needle, _)| *f == rel && around.contains(needle))
            {
                Some(i) => used_exceptions.push(i),
                None => {
                    let line = src[..pos].matches('\n').count() + 1;
                    leaks.push(format!(
                        "{rel}:{line} — MeetConfig без {:?} и без герметичной базы",
                        NEEDED
                            .iter()
                            .filter(|k| !body.contains(&format!("{k}:")))
                            .collect::<Vec<_>>()
                    ));
                }
            }
        }
    }

    assert!(
        leaks.is_empty(),
        "тест смотрит в каталог живых записей пользователя:\n  {}\n\
         Лечение: forge_meet::testing::hermetic_config(dir.path()) либо три пути явно.",
        leaks.join("\n  ")
    );

    // Список исключений тоже протухает: снятое исключение обязано исчезнуть,
    // иначе оно однажды прикроет настоящую утечку.
    let stale: Vec<&str> = HERMETIC_EXCEPTIONS
        .iter()
        .enumerate()
        .filter(|(i, _)| !used_exceptions.contains(i))
        .map(|(_, (_, needle, _))| *needle)
        .collect();
    assert!(
        stale.is_empty(),
        "исключения герметичности больше ничего не прикрывают: {stale:?}"
    );
}

/// База в тестах — либо в памяти, либо во временном каталоге.
///
/// `Db::open(<живой путь>)` открыл бы `~/.forge/meet/meet.db` — ту самую базу,
/// в которой лежат встречи пользователя. Проверка отдельная от конфигов:
/// путь к базе можно передать и мимо `MeetConfig`.
#[test]
fn every_database_in_tests_is_in_memory_or_in_a_tempdir() {
    let root = crate_root();
    let mut leaks = Vec::new();
    for path in rust_sources(&root) {
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        for (i, line) in src.lines().enumerate() {
            if !line.contains("Db::open(") {
                continue;
            }
            let pos = src.find(line).unwrap_or(0);
            if !in_test_code(&path, &src, pos) {
                continue;
            }
            let ok = ["dir", "tmp", "root", "temp"]
                .iter()
                .any(|m| line.contains(m));
            if !ok {
                leaks.push(format!(
                    "{}:{} — {}",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "тест открывает базу вне временного каталога:\n  {}",
        leaks.join("\n  ")
    );
}

/// Сам помощник герметичности обязан уводить ВСЕ пути, а не два из трёх.
///
/// Проверка выглядит избыточной ровно до того дня, когда в `MeetConfig`
/// появится четвёртый путь, а `hermetic_config` про него забудет: тогда все
/// тесты крейта разом начнут писать в живой каталог, и ни один не покраснеет.
#[test]
fn the_hermetic_helper_covers_every_path_of_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = forge_meet::testing::hermetic_config(dir.path());
    let temp = dir.path();
    for (name, path) in [
        ("config_path", &cfg.config_path),
        ("cache_dir", &cfg.cache_dir),
        ("db_path", &cfg.db_path),
        ("models_dir", &cfg.models_dir),
        ("claude_cli_path", &cfg.claude_cli_path),
    ] {
        assert!(
            path.starts_with(temp),
            "{name} = {} остался вне временного каталога",
            path.display()
        );
    }
    // И ни один из них не совпадает с боевым.
    let real = forge_meet::config::MeetConfig::default();
    assert_ne!(cfg.cache_dir, real.cache_dir);
    assert_ne!(cfg.db_path, real.db_path);
    assert_ne!(cfg.config_path, real.config_path);
}
