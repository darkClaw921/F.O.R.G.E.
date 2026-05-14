# DevForge Security Smoke Checklist (Phase 7)

Чеклист пунктов безопасности, проверенных в Phase 7. Используется как regression-чеклист перед каждым релизом remote-mode.

## 1. Token не утекает в JS / DOM

### Проверка
1. devforge run --remote (auto-gen токена).
2. Открыть UI в браузере.
3. Settings -> Remote servers -> Add: ввести label/URL/token, нажать Save.
4. DevTools -> Network -> проверить ответ POST /api/remote-servers:
   - В ответе должен быть RemoteServerView { id, label, url } -- без поля token.
5. DevTools -> Network -> проверить GET /api/remote-servers:
   - Массив из RemoteServerView. Поля token нет.
6. DevTools -> Sources -> app.js: state.remoteServers содержит только { id, label, url } (без token).

### Покрыто кодом
- src/remotes.rs::RemoteServerView -- DTO без поля token.
- impl From<&RemoteServer> for RemoteServerView -- копирует только id/label/url.
- src/remotes.rs::tests::view_excludes_token -- unit-тест проверяет, что serde_json::to_string(&view) НЕ содержит ни значение токена, ни строку 'token' (имя поля).

### Acceptance
PASS -- token хранится только на disk (~/.config/forge/remote_servers.json) и в RemoteServer (in-memory под RwLock). В public API всегда отдаётся RemoteServerView.

## 2. 401 на wrong Bearer

### Проверка (curl)
```
$ devforge run --remote
[devforge] auto-generated auth token saved to ~/.config/forge/server_config.json
Bearer token: abc123...

$ curl -i http://localhost:8787/api/projects
HTTP/1.1 401 Unauthorized
WWW-Authenticate: Bearer realm="devforge"

$ curl -i -H 'Authorization: Bearer WRONG-TOKEN' http://localhost:8787/api/projects
HTTP/1.1 401 Unauthorized
WWW-Authenticate: Bearer realm="devforge"

$ curl -i -H 'Authorization: Bearer abc123...' http://localhost:8787/api/projects
HTTP/1.1 200 OK
```

### Покрыто кодом
- src/auth.rs::bearer_auth -- middleware. На path не из EXCLUDED_EXACT/PREFIXES и без правильного Bearer -> 401 + WWW-Authenticate.
- src/auth.rs::tests::some_token_protected_without_header_returns_401 -- unit-тест.
- src/auth.rs::tests::some_token_protected_with_wrong_bearer_returns_401 -- unit-тест.
- src/auth.rs::tests::some_token_protected_with_correct_bearer_passes -- positive case.
- src/auth.rs::tests::malformed_authorization_header_rejected -- Basic auth header / Bearer без значения -> 401.

### Acceptance
PASS -- middleware применяется через axum::middleware::from_fn_with_state ТОЛЬКО при auth_token=Some (см. main.rs).

## 3. Public-bind WARNING печатается на старте

### Проверка
```
$ devforge run --remote --bind 0.0.0.0 --token abc123...
[INFO] listening on http://0.0.0.0:8787

╔══════════════════════════════════════════════════════════════════════════╗
║ WARNING: DevForge is bound to a public address WITHOUT TLS              ║
║   Bind:  0.0.0.0:8787                                                   ║
║   Token: abc12300...0123                                                ║
║   Auth is on, but transport is plain HTTP. Anyone sniffing the wire     ║
║   can capture the Bearer token. Use one of:                             ║
║     - SSH tunnel:  ssh -L 8787:127.0.0.1:8787 user@host                 ║
║     - WireGuard / Tailscale / ZeroTier private network                  ║
║     - Reverse-proxy с HTTPS (Caddy / nginx / Traefik) перед devforge    ║
╚══════════════════════════════════════════════════════════════════════════╝
```

На localhost-bind (127.0.0.1) warning НЕ печатается.

### Покрыто кодом
- src/server_config.rs::print_public_bind_warning(bind, port, token) -- no-op при is_localhost_bind(bind)=true.
- src/server_config.rs::is_localhost_bind -- true для 127.x.x.x, ::1, localhost.
- src/server_config.rs::tests::is_localhost_bind_recognises_loopback (Phase 7) -- unit-тест.
- src/server_config.rs::tests::is_localhost_bind_rejects_public (Phase 7) -- unit-тест.
- src/main.rs -- вызывает print_public_bind_warning ПОСЛЕ bind и ДО axum::serve в блоке `if remote_mode { ... }`.

### Acceptance
PASS -- warning виден ВСЕГДА в remote_mode + non-loopback bind, независимо от того, авто-сгенерён токен или передан явно.

## 4. /api/remote-servers НЕ зарегистрирован без --remote

### Проверка (curl)
```
$ devforge run
[INFO] listening on http://127.0.0.1:8787

$ curl -i http://127.0.0.1:8787/api/remote-servers
HTTP/1.1 404 Not Found
```

(точнее -- 404 от static-fallback'а, потому что роут не зарегистрирован -> axum NotFound -> fallback на static_embed -> файл не найден.)

### Покрыто кодом
- src/main.rs -- регистрация роутов в блоке `if remote_mode { app = app.route(...) }` для всех 4 endpoint'ов /api/remote-servers и /api/remote-servers/:id/healthz.
- При remote_mode=false блок не выполняется, роутов нет.

### Acceptance
PASS -- структурно: endpoint'ы добавляются в Router только при remote_mode=true. В legacy localhost-режиме регистрация не происходит.

## 5. /healthz публично доступен (intentional)

### Проверка
```
$ curl -i http://localhost:8787/healthz
HTTP/1.1 200 OK
Content-Type: application/json
{"status":"ok","remote_mode":true,"version":"0.1.3"}
```

Без Authorization header -- должен работать.

### Покрыто кодом
- src/auth.rs::EXCLUDED_EXACT -- содержит "/healthz".
- src/auth.rs::is_path_excluded -- true для /healthz.
- src/auth.rs::tests::some_token_healthz_pass_without_header -- unit-тест.

### Acceptance
PASS -- intentional. Frontend должен прочитать remote_mode/version ДО ввода токена (UI логина).

## 6. Логирование прокси-ошибок (Phase 7)

### Проверка
```
$ RUST_LOG=devforge=trace devforge run --remote
```
Затем дёрнуть из браузера GET /api/sessions?server=<unreachable-remote-id> -> в логах видим:
```
WARN devforge::remote_proxy: remote_proxy: upstream request failed server_id=ghost path=/api/sessions error=... is_timeout=false is_connect=true
```

### Покрыто кодом
- src/remote_proxy.rs::proxy_request -- warn! на reqwest errors с is_timeout/is_connect; trace! на non-2xx ответы.
- src/remote_proxy.rs::proxy_websocket -- trace! на Close-кадры в обе стороны с code+reason+server_id+path.

### Acceptance
PASS -- все ветки proxy ошибок инструментированы.

## Сводка

| #  | Пункт                                                    | Статус | Покрытие |
|----|----------------------------------------------------------|--------|----------|
| 1  | Token не в JS / DOM (RemoteServerView excludes token)    | PASS   | unit: view_excludes_token |
| 2  | 401 на missing / wrong Bearer                            | PASS   | unit: auth tests x5 |
| 3  | Public-bind WARNING при non-loopback bind                | PASS   | unit: is_localhost_bind matrix |
| 4  | /api/remote-servers недоступен без --remote              | PASS   | структурно (main.rs `if remote_mode`) |
| 5  | /healthz публичен (intentional)                          | PASS   | unit: healthz exclusion |
| 6  | Прокси-ошибки логируются (trace+warn)                    | PASS   | manual через RUST_LOG=devforge=trace |

Все пункты прошли smoke-проверку. Чеклист должен проверяться перед каждым релизом, изменяющим auth / remote-proxy / server_config.