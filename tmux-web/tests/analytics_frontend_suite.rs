//! Прогон ФРОНТОВЫХ тестов аналитики из `cargo test`.
//!
//! # Зачем
//!
//! Кит графиков (`static/js/analytics/chart.js`) ломается молча: атрибут
//! `width="NaN"` или `height="-12"` браузер игнорирует без единой строчки в
//! консоли, и пользователь видит пустую карточку вместо данных. Ровно это
//! ловит `tests/frontend/analytics_chart.test.mjs` — но лишь до тех пор, пока
//! его кто-то запускает. Запуск руками из README держится на памяти
//! разработчика, поэтому проверка входит в обычный `cargo test`.
//!
//! Тот же принцип, что у `meet_frontend_suite.rs`, только для аналитики:
//! проверяемые файлы лежат по обе стороны границы (`src/analytics/**` и
//! `static/js/analytics/**`), и оба конца знает хост.
//!
//! # Нет `node` — ПРОПУСК, а не падение
//!
//! Node не значится в зависимостях devforge, и требовать его от каждой машины
//! сборки ради этих проверок было бы новым требованием к окружению. Тот же
//! компромисс, что в `meet_frontend_suite.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Все фронтовые тесты аналитики.
///
/// Список собирается ПО КАТАЛОГУ, а не перечисляется руками: перечисленный
/// список протухает молча — новый `.test.mjs` просто не попадает в прогон, и
/// об этом никто не узнаёт. Отбор по имени файла: `analytics_*` плюс вкладка
/// настроек `settings_analytics_tab.test.mjs` (Фаза 6), которая по имени
/// принадлежит семейству настроек, а по смыслу — аналитике.
fn analytics_frontend_tests(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("нет каталога tests/frontend")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            name.ends_with(".test.mjs")
                && (name.starts_with("analytics_") || name == "settings_analytics_tab.test.mjs")
        })
        .collect();
    files.sort();
    files
}

/// **ФРОНТОВЫЕ ТЕСТЫ АНАЛИТИКИ ЗЕЛЁНЫЕ.**
///
/// Каждый файл — отдельный процесс `node <файл>`: они так и написаны (свой
/// счётчик проверок, код возврата 0/1), и `node --test` им не нужен. Тесты
/// герметичны: ни один не поднимает сервер и не ходит в сеть — кит графиков
/// рисует на собственном крошечном фейковом DOM.
#[test]
fn the_analytics_frontend_tests_pass() {
    if !node_available() {
        eprintln!("пропуск: в PATH нет node — фронтовые тесты аналитики не прогнаны");
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("tests/frontend");
    let files = analytics_frontend_tests(&dir);

    // Пустой набор с кодом 0 выглядел бы как успех и снял бы проверку молча —
    // ровно тот случай, ради которого весь этот файл и написан.
    assert!(
        !files.is_empty(),
        "фронтовых тестов аналитики не найдено в {dir:?}"
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
        "фронтовые тесты аналитики упали ({} из {}):\n{}",
        failed.len(),
        files.len(),
        failed.join("\n")
    );
}

/// Кит графиков не имеет права выпасть из прогона.
///
/// Отбор по маске удобен ровно до того дня, когда кто-нибудь переименует
/// `analytics_chart.test.mjs`. Проверка геометрии SVG — единственное, что
/// стоит между пользователем и молча пустой карточкой, поэтому файл назван
/// поимённо.
#[test]
fn the_analytics_chart_test_is_in_the_list() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let names: Vec<String> = analytics_frontend_tests(&root.join("tests/frontend"))
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "analytics_chart.test.mjs"),
        "analytics_chart.test.mjs не попал в прогон; найдено: {names:?}"
    );
}
