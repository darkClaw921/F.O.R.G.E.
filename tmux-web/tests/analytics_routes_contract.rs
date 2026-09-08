//! ⚠ СТРАЖ КОНТРАКТА API АНАЛИТИКИ (Фаза 3, задача forge-an-f3-u6023.6).
//!
//! Поверхность `/api/analytics/*` описана ДВАЖДЫ, и между этими описаниями
//! нет ни компилятора, ни общего типа:
//!
//!   * `tmux-web/src/main.rs`            — вызовы `.route("/api/analytics/…")`;
//!   * `tmux-web/static/js/analytics/api.js` — таблица `ANALYTICS_ROUTES`.
//!
//! Расхождение НЕ ломает сборку и не пишет ни строчки в лог. Оно проявляется
//! у пользователя как пустой дашборд: клиент уходит по несуществующему пути,
//! получает 404 от статик-фолбэка, и `call()` бросает `HTTP 404` в консоль,
//! которую никто не читает. Ровно на этом уже ловили плагины встреч и
//! Телеграма, поэтому сверка идёт В ОБЕ СТОРОНЫ и поимённо.
//!
//! Сравнение ТЕКСТОВОЕ по исходникам: поднимать сервер и опрашивать роутер
//! ради четырёх строк дороже, чем прочитать два файла. Тот же приём у
//! `meet_contract.test.mjs`.
//!
//! Запуск: `cargo test -p devforge analytics`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Префикс, по которому опознаются роуты аналитики с обеих сторон.
const PREFIX: &str = "/api/analytics/";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("не прочитан {}: {e}", path.display()))
}

/// Один зарегистрированный роут: метод, путь и имя хендлера.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Route {
    method: String,
    path: String,
    handler: String,
}

/// Символ, который может входить в идентификатор Rust.
fn ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Разбирает `main.rs` и достаёт все `.route("/api/analytics/…", …)`.
///
/// Читает вызов ЦЕЛИКОМ по балансу скобок, а не построчно: многострочная
/// регистрация (как у `/api/user-settings` по соседству) — обычное дело в
/// этом файле, и построчный разбор молча потерял бы её.
fn rust_routes(src: &str) -> Vec<Route> {
    const MARK: &str = ".route(";
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel) = src[cursor..].find(MARK) {
        let start = cursor + rel + MARK.len();
        let mut depth = 1usize;
        let mut end = start;
        while end < bytes.len() && depth > 0 {
            match bytes[end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        let chunk = &src[start..end.saturating_sub(1)];
        cursor = end;

        let Some(path) = first_string_literal(chunk) else {
            continue;
        };
        if !path.starts_with(PREFIX) {
            continue;
        }
        for (method, handler) in methods_of(chunk) {
            out.push(Route {
                method,
                path: path.clone(),
                handler,
            });
        }
    }
    out.sort();
    out
}

/// Первый строковый литерал в куске исходника (это и есть путь роута).
fn first_string_literal(chunk: &str) -> Option<String> {
    let open = chunk.find('"')?;
    let rest = &chunk[open + 1..];
    let close = rest.find('"')?;
    Some(rest[..close].to_string())
}

/// Пары «метод → хендлер» из хвоста вызова `.route(...)`.
///
/// Ищет `get(имя)`, `post(имя)` и соседей ровно на границе идентификатора:
/// без этой проверки `get(get_analytics_status)` дал бы ложное второе
/// совпадение внутри имени хендлера.
fn methods_of(chunk: &str) -> Vec<(String, String)> {
    let bytes = chunk.as_bytes();
    let mut out = Vec::new();
    for verb in ["get", "post", "put", "patch", "delete"] {
        let needle = format!("{verb}(");
        let mut cursor = 0usize;
        while let Some(rel) = chunk[cursor..].find(&needle) {
            let at = cursor + rel;
            cursor = at + needle.len();
            let boundary_ok = at == 0 || !ident_char(bytes[at - 1]);
            if !boundary_ok {
                continue;
            }
            let handler: String = chunk[cursor..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if handler.is_empty() {
                continue;
            }
            out.push((verb.to_ascii_uppercase(), handler));
        }
    }
    out
}

/// Разбирает таблицу `ANALYTICS_ROUTES` из `api.js`.
///
/// Формат строки закреплён здесь намеренно: таблица — контракт, и её нельзя
/// переписать «покрасивее» так, чтобы страж перестал её видеть, — тест это
/// заметит по опустевшему списку.
fn js_routes(src: &str) -> Vec<(String, String, String)> {
    let start = src
        .find("export const ANALYTICS_ROUTES")
        .expect("в api.js нет таблицы ANALYTICS_ROUTES");
    let body_start = src[start..].find('[').expect("таблица без '['") + start;
    let body_end = src[body_start..].find("];").expect("таблица без ']'") + body_start;
    let body = &src[body_start..body_end];

    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let method = js_field(line, "method").expect("в строке таблицы нет method");
        let path = js_field(line, "path").expect("в строке таблицы нет path");
        let func = js_field(line, "fn").expect("в строке таблицы нет fn");
        out.push((method, path, func));
    }
    out.sort();
    out
}

/// Значение поля `key: '…'` из одной строки таблицы.
fn js_field(line: &str, key: &str) -> Option<String> {
    let at = line.find(&format!("{key}:"))?;
    let rest = &line[at + key.len() + 1..];
    let open = rest.find('\'')?;
    let tail = &rest[open + 1..];
    let close = tail.find('\'')?;
    Some(tail[..close].to_string())
}

// ===========================================================================
// Тесты
// ===========================================================================

/// **РОУТЫ АНАЛИТИКИ СОВПАДАЮТ В ОБЕ СТОРОНЫ.**
///
/// Лишний путь в JS — молчаливый 404 у пользователя. Лишний путь в Rust —
/// мёртвый эндпоинт, про который фронт не знает. Оба случая проверяются
/// поимённо, чтобы в отчёте было видно КАКОЙ роут разъехался.
#[test]
fn the_analytics_routes_match_in_both_directions() {
    let main_rs = read("src/main.rs");
    let api_js = read("static/js/analytics/api.js");

    let backend = rust_routes(&main_rs);
    let frontend = js_routes(&api_js);

    // Пол в четыре роута — защита от сломавшегося разбора: пустые списки с
    // обеих сторон совпали бы между собой и сняли бы проверку молча. Верхней
    // границы нет намеренно — новый роут не должен требовать правки числа
    // здесь, его поймает сверка составов ниже.
    assert!(
        backend.len() >= 4,
        "в main.rs найдено {} роутов /api/analytics/*, ожидалось не меньше 4: {backend:#?}",
        backend.len()
    );
    assert!(
        frontend.len() >= 4,
        "в ANALYTICS_ROUTES найдено {} записей, ожидалось не меньше 4: {frontend:#?}",
        frontend.len()
    );

    let back: BTreeSet<String> = backend
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    let front: BTreeSet<String> = frontend
        .iter()
        .map(|(m, p, _)| format!("{m} {p}"))
        .collect();

    for r in &back {
        assert!(
            front.contains(r),
            "роут бэкенда «{r}» отсутствует в ANALYTICS_ROUTES: фронт про него не узнает"
        );
    }
    for r in &front {
        assert!(
            back.contains(r),
            "клиент зовёт «{r}», но такого роута нет в main.rs: будет молчаливый 404"
        );
    }
    assert_eq!(back, front, "составы роутов разошлись");
}

/// Каждому роуту соответствует реальный хендлер в `main.rs` и реальная
/// экспортированная функция в `api.js`.
///
/// Имя в таблице, за которым ничего нет, — та же молчаливая дыра, что и
/// разъехавшийся путь: `import { … }` вернёт `undefined`, и вызов упадёт
/// только в момент клика.
#[test]
fn the_analytics_route_handlers_and_clients_exist() {
    let main_rs = read("src/main.rs");
    let api_js = read("static/js/analytics/api.js");

    for route in rust_routes(&main_rs) {
        let decl = format!("async fn {}(", route.handler);
        assert!(
            main_rs.contains(&decl),
            "хендлер {} роута {} {} не объявлен в main.rs",
            route.handler,
            route.method,
            route.path
        );
    }

    for (method, path, func) in js_routes(&api_js) {
        let decl = format!("export function {func}(");
        assert!(
            api_js.contains(&decl),
            "функция {func} роута {method} {path} не экспортирована из api.js"
        );
    }
}

/// Клиент ходит в сеть ТОЛЬКО через `apiFetch`.
///
/// Прямой `fetch(...)` пролез бы мимо подмены из `core/auth.js`: в
/// remote-режиме он не получил бы ни bearer-токена, ни CSRF-заголовка и
/// вернул бы 401 — а на дашборде это выглядит как «аналитика не работает».
#[test]
fn the_analytics_client_never_calls_fetch_directly() {
    let api_js = read("static/js/analytics/api.js");
    // Комментарии пропускаем: в этом самом файле слово `fetch(` стоит в
    // объяснении, ПОЧЕМУ его нельзя звать, и запрещать себе объяснение —
    // верный способ получить страж, который проще выключить, чем понять.
    for (n, line) in api_js.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }
        let bytes = line.as_bytes();
        let mut cursor = 0usize;
        while let Some(rel) = line[cursor..].find("fetch(") {
            let at = cursor + rel;
            cursor = at + "fetch(".len();
            let bare = at == 0 || !ident_char(bytes[at - 1]);
            assert!(
                !bare,
                "api.js зовёт fetch() напрямую (строка {}) — в remote-режиме это 401:\n{line}",
                n + 1
            );
        }
    }
    assert!(
        api_js.contains("import { apiFetch }"),
        "api.js обязан импортировать apiFetch"
    );
}

/// Файл клиента лежит там, где его ждёт `rust-embed` (`static/js/analytics/`).
///
/// Промах каталогом даёт файл, который есть в репозитории и которого нет в
/// бинаре: `static_embed.rs` вшивает именно `static/`.
#[test]
fn the_analytics_client_is_inside_the_embedded_static_dir() {
    let path = manifest_dir().join("static/js/analytics/api.js");
    assert!(
        Path::new(&path).is_file(),
        "нет {}: клиент не попадёт в бинарь",
        path.display()
    );
}
