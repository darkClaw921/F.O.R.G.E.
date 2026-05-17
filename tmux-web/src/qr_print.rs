//! QR-баннер при старте сервера.
//!
//! Печатает в stdout QR-коды со ссылками на запущенный tmux-web, чтобы
//! пользователь мог открыть mobile UI на телефоне камерой.
//!
//! ## URL'ы в баннере
//!
//! - **LAN URL** (`http://<lan-ip>:<port>`) — IP машины в локальной сети,
//!   получается через [`local_ip_address::local_ip`]. Печатается всегда,
//!   когда удалось резолвить непустой не-loopback адрес. Это URL для
//!   подключения с телефона в той же Wi-Fi.
//! - **Bind URL** (`http://<bind>:<port>`) — печатается только когда
//!   `bind_host` отличается от `127.0.0.1`, `localhost`, `0.0.0.0`, `::`
//!   и от резолвленного LAN-IP (чтобы не дублировать QR).
//! - Если `bind_host` = `127.0.0.1` или `localhost` (default без `--remote`)
//!   — печатается warning, что для подключения с телефона нужно
//!   запустить с `--remote`.
//!
//! ## Рендер
//!
//! Используется [`qrcode::render::unicode::Dense1x2`] — каждая строка
//! терминала кодирует 2 ряда QR через символы ▀▄█ (half-blocks). На
//! среднем терминале (≥80×24) QR версии 3–5 (URL до ~60 байт) занимает
//! ~22×11 строк, считывается камерой телефона без масштабирования.
//!
//! ## API
//!
//! [`print_startup_qr`] — единственная публичная функция. Безопасна для
//! вызова при любом значении `bind_host` (даже `0.0.0.0` или пустая
//! строка); внутри есть fallback на loopback URL чтобы баннер не падал.

use std::net::IpAddr;

/// Печатает в stdout QR-коды и подсказки для подключения mobile-клиента.
///
/// `auth_token`: если remote-mode включён и токен передан — он добавляется
/// в URL как hash-fragment (`#token=<token>`). Frontend на старте парсит
/// hash, сохраняет токен в localStorage и использует его для всех
/// `fetch`/`WebSocket`. Hash безопаснее query (не уходит в access logs и
/// Referer).
///
/// Не возвращает ошибок — все проблемы (не резолвится LAN IP,
/// QR-encoding failure) логируются через `tracing::warn` и не прерывают
/// запуск сервера.
pub fn print_startup_qr(
    bind_host: &str,
    port: u16,
    remote_mode: bool,
    auth_token: Option<&str>,
) {
    let token_suffix = match (remote_mode, auth_token) {
        (true, Some(t)) if !t.is_empty() => format!("#token={t}"),
        _ => String::new(),
    };

    let lan = detect_lan_ip();
    let bind_url = bind_url(bind_host, port).map(|u| format!("{u}{token_suffix}"));
    let lan_url = lan.map(|ip| format!("http://{ip}:{port}{token_suffix}"));

    let mut shown: Vec<(String, String)> = Vec::new();

    // 1) LAN URL — приоритетный для телефона.
    if let Some(ref url) = lan_url {
        shown.push((label_for_lan(remote_mode, bind_host), url.clone()));
    }

    // 2) Bind URL — печатаем только если он отличается от LAN URL и не
    //    тривиальный (loopback/wildcard).
    if let Some(b) = bind_url {
        let dup = lan_url.as_ref().map(|s| s == &b).unwrap_or(false);
        if !dup {
            shown.push(("Bind address".to_string(), b));
        }
    }

    // 3) Fallback: если ничего не накопилось — показать loopback URL.
    if shown.is_empty() {
        shown.push((
            "Local URL".to_string(),
            format!("http://127.0.0.1:{port}{token_suffix}"),
        ));
    }

    println!();
    println!("📱 Открыть на телефоне / Open on phone:");
    println!();

    for (label, url) in &shown {
        print_one_qr(label, url);
    }

    // Warning, если default-bind 127.0.0.1 без --remote — телефон в той же
    // Wi-Fi не подключится к LAN URL даже если в нём указан правильный IP.
    if !remote_mode && is_loopback_bind(bind_host) {
        println!(
            "⚠  Сервер слушает только 127.0.0.1. Чтобы подключиться с телефона,"
        );
        println!(
            "   перезапустите с флагом --remote (или укажите --bind 0.0.0.0)."
        );
        println!();
    }
}

fn label_for_lan(remote_mode: bool, bind_host: &str) -> String {
    if !remote_mode && is_loopback_bind(bind_host) {
        "LAN URL (требуется --remote)".to_string()
    } else {
        "LAN URL".to_string()
    }
}

fn is_loopback_bind(bind_host: &str) -> bool {
    let h = bind_host.trim().to_ascii_lowercase();
    h == "127.0.0.1" || h == "localhost" || h == "::1"
}

fn is_wildcard_bind(bind_host: &str) -> bool {
    let h = bind_host.trim();
    h == "0.0.0.0" || h == "::" || h.is_empty()
}

fn bind_url(bind_host: &str, port: u16) -> Option<String> {
    if is_wildcard_bind(bind_host) {
        // На wildcard конкретного URL нет — для телефона показываем LAN URL.
        return None;
    }
    Some(format!("http://{bind_host}:{port}"))
}

fn detect_lan_ip() -> Option<IpAddr> {
    match local_ip_address::local_ip() {
        Ok(ip) => {
            if ip.is_loopback() || ip.is_unspecified() {
                None
            } else {
                Some(ip)
            }
        }
        Err(e) => {
            tracing::debug!(error=?e, "failed to detect LAN IP for QR banner");
            None
        }
    }
}

fn print_one_qr(label: &str, url: &str) {
    println!("  {label}:  {url}");
    match qrcode::QrCode::new(url.as_bytes()) {
        Ok(code) => {
            let rendered = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .dark_color(qrcode::render::unicode::Dense1x2::Light)
                .light_color(qrcode::render::unicode::Dense1x2::Dark)
                .quiet_zone(true)
                .build();
            // Инвертируем dark/light: в большинстве тёмных терминалов
            // светлые модули должны рисоваться "пробелом" чёрного фона,
            // тёмные — символом. Dense1x2::{Light,Dark} здесь — это
            // unicode-символы, а не цвета: Light=▀/█ для светлого
            // фона, Dark — обратное. Стандартная пара для тёмных
            // терминалов: dark_color=Light, light_color=Dark.
            for line in rendered.lines() {
                println!("    {line}");
            }
        }
        Err(e) => {
            tracing::warn!(error=?e, %url, "failed to encode QR for url");
            println!("    [QR encoding failed — open URL manually]");
        }
    }
    println!();
}
