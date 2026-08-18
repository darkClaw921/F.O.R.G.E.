//! Прогон JS-тестов инжекта из `cargo test`.
//!
//! Чистая часть захвата — пересчёт частоты, PCM16, base64 и VAD — написана на
//! JavaScript, потому что исполняется в браузере. Проверять её надо там же,
//! где она живёт, то есть в `node --test`; но если оставить эти тесты
//! отдельной командой, их перестанут запускать в первый же занятый день.
//!
//! Поэтому `cargo test -p forge-meet` запускает их сам. Нет `node` — тест
//! **пропускается с внятным сообщением**, а не падает: node не значится в
//! зависимостях плагина, и требовать его от каждой машины сборки было бы
//! новым требованием к окружению ради одной проверки.

use std::path::Path;
use std::process::Command;

/// Есть ли `node` в PATH.
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn the_javascript_suite_passes() {
    if !node_available() {
        eprintln!("пропуск: в PATH нет node — JS-тесты инжекта не прогнаны");
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/js");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("нет каталога tests/js")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("mjs"))
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "в tests/js нет ни одного теста");

    let out = Command::new("node")
        .arg("--test")
        .args(&files)
        .current_dir(root)
        .output()
        .expect("node не запустился");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "JS-тесты инжекта упали\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    // Тест обязан РЕАЛЬНО что-то прогнать: пустой набор с кодом 0 выглядел бы
    // как успех и молча снял бы проверку с самого ошибкоопасного места.
    let passed = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("# pass ").or_else(|| l.trim().strip_prefix("ℹ pass ")))
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or(0);
    assert!(
        passed >= 25,
        "прогнано подозрительно мало JS-тестов ({passed})\n{stdout}"
    );
}

/// Файлы, которые импортируют тесты, обязаны существовать под теми же
/// именами, что встраиваются в worklet. Переименование без правки сборщика
/// иначе прошло бы молча: Rust собрал бы worklet из старого файла, а node
/// проверял бы новый.
#[test]
fn the_tested_modules_are_the_ones_that_get_embedded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["pcm.js", "vad.js", "worklet.js", "capture.js", "transport.js"] {
        assert!(
            root.join("src/bot/inject").join(name).is_file(),
            "нет src/bot/inject/{name}"
        );
    }
    for name in ["pcm.test.mjs", "vad.test.mjs", "capture.test.mjs"] {
        let src = std::fs::read_to_string(root.join("tests/js").join(name)).unwrap();
        assert!(
            src.contains("../../src/bot/inject/"),
            "{name} импортирует не тот модуль, который встраивается в worklet"
        );
    }
}
