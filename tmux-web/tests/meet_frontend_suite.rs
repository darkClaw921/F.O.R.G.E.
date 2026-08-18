//! Прогон ФРОНТОВЫХ тестов плагина встреч из `cargo test`.
//!
//! # Зачем
//!
//! Контрактные тесты (`meet_contract`, `meet_css_layer`, `meet_api_gate` и
//! соседи) сверяют Rust с JavaScript: таблицу роутов с `MEET_ROUTES`, варианты
//! `MeetServerMsg` с картой обработчиков, `MeetStatus::ALL` с таблицами кнопок,
//! белый список настроек с подписями формы. Расхождение любой пары НЕ ломает
//! сборку и не пишет ни строчки в консоль — оно проявляется у пользователя как
//! «кнопка не появилась» или «настройка называется английским словом».
//!
//! Пока эти тесты запускались только руками из README, договор держался на
//! памяти того, кто правит код. Здесь они входят в обычный `cargo test`.
//!
//! # Почему в `tmux-web`, а не в `plugins/meet`
//!
//! Проверяемые файлы лежат по обе стороны границы: `plugins/meet/src/**` и
//! `tmux-web/static/js/**`. Крейт плагина про фронт хоста знать не обязан —
//! наоборот, хост знает про оба. Поэтому запуск живёт здесь, рядом с
//! `meet_smoke.rs`.
//!
//! # Нет `node` — ПРОПУСК, а не падение
//!
//! Node не значится в зависимостях devforge, и требовать его от каждой машины
//! сборки ради этих проверок было бы новым требованием к окружению. Тот же
//! компромисс, что в `plugins/meet/tests/js_suite.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Все фронтовые тесты плагина встреч.
///
/// Список собирается ПО КАТАЛОГУ, а не перечисляется руками: перечисленный
/// список протухает молча — новый тест просто не попадает в прогон, и об этом
/// никто не узнаёт. Отбор по имени файла: `meet_*`, `messenger_meet_*` и
/// вкладка настроек.
fn meet_frontend_tests(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("нет каталога tests/frontend")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            name.ends_with(".test.mjs")
                && (name.starts_with("meet_")
                    || name.starts_with("messenger_meet_")
                    || name == "settings_meet_tab.test.mjs")
        })
        .collect();
    files.sort();
    files
}

/// **КОНТРАКТНЫЕ ТЕСТЫ ПЛАГИНА ВСТРЕЧ ЗЕЛЁНЫЕ.**
///
/// Каждый файл — отдельный процесс `node <файл>`: они так и написаны (свой
/// счётчик проверок, код возврата 0/1), и `node --test` им не нужен. Тесты
/// герметичны: ни один не поднимает сервер и не ходит в сеть — `meet_contract`
/// даже проверяет это отдельно (`_debugMeetWs().created === false`,
/// `meetGateRequests() === 0`).
#[test]
fn the_meet_frontend_contract_tests_pass() {
    if !node_available() {
        eprintln!("пропуск: в PATH нет node — фронтовые тесты встреч не прогнаны");
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/frontend");
    let files = meet_frontend_tests(&dir);

    // Пустой набор с кодом 0 выглядел бы как успех и снял бы проверку молча —
    // ровно тот случай, ради которого весь этот файл и написан.
    assert!(
        files.len() >= 10,
        "фронтовых тестов встреч найдено подозрительно мало: {} ({:?})",
        files.len(),
        files
    );

    let mut failed: Vec<String> = Vec::new();
    for file in &files {
        let out = Command::new("node")
            .arg(file)
            .current_dir(root)
            .output()
            .expect("node не запустился");
        if !out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            failed.push(format!(
                "--- {} ---\n{stdout}\n{stderr}",
                file.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "фронтовые тесты встреч упали ({} из {}):\n{}",
        failed.len(),
        files.len(),
        failed.join("\n")
    );
}

/// Список тестов не имеет права молча потерять контрактные файлы.
///
/// Отбор по маске удобен ровно до того дня, когда кто-нибудь переименует
/// `meet_contract.test.mjs`. Четыре поверхности договора перечислены поимённо:
/// роуты и кадры, слой токенов, гейт плагина и вкладка настроек.
#[test]
fn the_four_contract_surfaces_are_in_the_list() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let names: Vec<String> = meet_frontend_tests(&root.join("tests/frontend"))
        .iter()
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
        .collect();
    for required in [
        "meet_contract.test.mjs",
        "meet_css_layer.test.mjs",
        "meet_api_gate.test.mjs",
        "settings_meet_tab.test.mjs",
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "{required} не попал в прогон; найдено: {names:?}"
        );
    }
}
