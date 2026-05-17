# tmux-web/src/qr_print.rs

Модуль печати QR-баннера при старте devforge.

## Public API
pub fn print_startup_qr(bind_host, port, remote_mode, auth_token: Option<&str>).

В remote-mode с токеном URL формируется как 'http://<host>:<port>#token=<token>'. Hash безопаснее query (не идёт в access logs и Referer). Frontend на старте парсит location.hash, сохраняет в localStorage 'forge.authToken', очищает hash через history.replaceState. См. tmux-web/static/app.js bootstrapAuthToken.

## Алгоритм
1. detect_lan_ip через local_ip_address::local_ip (loopback/unspec — отбрасываем).
2. Формируем token_suffix: если remote_mode && Some(token) && !empty → '#token=<token>', иначе ''.
3. URLs (label, url):
   - LAN URL: 'http://<lan-ip>:<port>{suffix}' если LAN IP резолвится.
   - Bind URL: 'http://<bind>:<port>{suffix}' если bind не wildcard/empty и URL не дублирует LAN.
   - Fallback: 'http://127.0.0.1:<port>{suffix}' если ничего другого нет.
4. Для каждой пары → print_one_qr: header + Dense1x2 unicode QR.
5. Warning '⚠ Сервер слушает только 127.0.0.1' если loopback bind без --remote.

## Рендер
qrcode::QrCode::new(url.as_bytes()) → unicode::Dense1x2 с dark=Light/light=Dark (инверсия для тёмных терминалов), quiet_zone=true.

## Зависимости
- qrcode 0.14 (default-features=false).
- local-ip-address 0.6.
- tracing (debug/warn для fail-cases).
