# DevForge Remote Mode — Overview

Документ-обзор архитектуры режима remote (Phase 1-7).

## Режимы запуска

### Legacy (default, без флагов)
- Bind: 127.0.0.1:8787 (hardcoded, не выходит наружу).
- Auth: НЕТ (middleware не подключён).
- REST /api/remote-servers: 404 (роуты не зарегистрированы).
- /healthz: { status: 'ok', remote_mode: false, version: X.Y.Z }.
- Frontend: UI origin-табов скрыт, кнопка 'Add remote' скрыта, ?server= в API/WS — 400.

### Remote-mode (devforge run --remote [--bind ADDR] [--token HEX])
- Bind: bind/0.0.0.0:port.
- Auth: Bearer-token middleware подключён ВСЕГДА (auto-gen 64-hex при отсутствии --token).
- REST /api/remote-servers/* CRUD доступен.
- /healthz: { status: 'ok', remote_mode: true, version: X.Y.Z } — публично без auth (frontend читает ДО ввода token).
- Frontend: origin-табы видны, Settings → Remote servers tab.
- Public-bind warning (Phase 7) печатается на старте если bind != 127.x.x.x.

## Конфигурация (server_config.json)
Путь: ~/.config/forge/server_config.json
Поля: { auth_token: Option<hex64>, bind: Option<String>, port: Option<u16> }.
Приоритет resolve: CLI флаги > файл > env (DEVFORGE_AUTH_TOKEN) > defaults.
Auto-gen: при remote_mode без token → finalize_token() генерит, печатает банер, atomic-save merge.

## Pairing flow

### На host-машине (где запущен devforge с --remote)
1. devforge pair --generate — генерит токен + server_config.json (bind=0.0.0.0).
   ИЛИ
   devforge run --remote — auto-gen токена при первом старте, печатает банер.
2. Скопировать токен из банера / server_config.json.

### На client-машине (где работает фронт)
1. Settings → Remote servers → Add.
2. Поля: Label (для UI), URL (http://host:8787), Token (paste).
3. Test connection — POST + GET healthz через локальный devforge.
4. Save → запись в ~/.config/forge/remote_servers.json.
ИЛИ через CLI: devforge remote add http://host:8787 --token <HEX> [--label NAME].

## Архитектура прокси

### HTTP-прокси (Phase 3)
Frontend делает fetch('/api/sessions?server=<id>').
Backend handler через try_proxy_to_remote → remote_proxy::proxy_request:
1. Достаёт RemoteServer{url, token} из state.remotes.
2. reqwest <url><path>?<query> + Authorization: Bearer <token>.
3. Hop-by-hop headers фильтруются.
4. JSON-ответ обогащается origin=<server_id> через enrich_with_origin.
5. Возвращается клиенту с тем же status+headers+body.
Логирование: warn! на network errors, trace! на non-2xx (Phase 7).

### WebSocket-прокси (Phase 4)
Frontend открывает ws('/ws/attach?session=X&server=<id>') или /ws/lazygit, /ws/tasks, /ws/todos.
Backend ws-handler → remote_proxy::proxy_websocket:
1. http(s):// → ws(s)://, connect_async с Bearer-header.
2. tokio::join! двух pumps: down_to_up + up_to_down.
3. На любой Close с любой стороны — каскадное закрытие другой.
Логирование: trace! на каждый Close + send/recv errors с server_id+path+code+reason (Phase 7).

## Aggregated view (Phase 6)
В remote_mode фронт показывает origin-табы: All, Local, <remote labels>.
- activeOrigin='all' — sidebar агрегирует local-сессии + lazy-load remote-сессий.
- activeOrigin='local' — только local.
- activeOrigin=<id> — только данный remote (lazy fetch при первом раскрытии origin-секции).
DTO везде содержит origin: 'local' | server_id. Глобальные id вида '<origin>::<id>' для уникализации между origin'ами.

## Reconnect (Phase 7)
### Health probe per-server
Per-server timer в frontend (remoteProbeState Map):
- Backoff: 2s → 4s → 8s → 16s → 32s → 60s + jitter(0..1s).
- На success: backoff reset to steady-state (4s).
- На fail: step++.
- UI badge: state.remoteOnline → renderSidebar() при смене online↔offline.

### WS reconnect
Все WS на frontend имеют backoff-reconnect:
- /ws/attach — 2s → 60s + jitter, сохраняет currentSession+origin.
- /ws/tasks — 1s → 10s + degraded polling.
- /ws/todos — 1s → 10s + degraded polling.
- /ws/lazygit — manual retry (UI banner с Retry-кнопкой).

## Безопасность
- Token хранится ТОЛЬКО локально (~/.config/forge/{server_config,remote_servers}.json).
- REST /api/remote-servers (GET) отдаёт RemoteServerView { id, label, url } — БЕЗ token.
- Bearer-auth middleware на ВСЕХ путях кроме /healthz, /, /assets/, /static/.
- 401 с WWW-Authenticate: Bearer на wrong/missing token.
- Public-bind WARNING печатается на старте если bind != 127.x.x.x (Phase 7).
- РЕКОМЕНДАЦИЯ: использовать SSH-tunnel / WireGuard / Tailscale / reverse-proxy с HTTPS для production — devforge сам TLS НЕ умеет.

## Файлы реализации
- tmux-web/src/cli.rs — argv → RunOptions/Subcommand, run_pair, run_remote.
- tmux-web/src/server_config.rs — resolve, finalize_token, print_public_bind_warning.
- tmux-web/src/auth.rs — Bearer middleware, path exclusions.
- tmux-web/src/remotes.rs — RemoteServerStore, default_remotes_path.
- tmux-web/src/remote_proxy.rs — proxy_request, proxy_websocket, enrich_with_origin, ProxyError.
- tmux-web/src/main.rs — AppState, /healthz, try_proxy_to_remote, регистрация /api/remote-servers/* (only if remote_mode).
- tmux-web/static/app.js — isRemoteMode, origin-табы, probeRemoteServer, reconnect-backoff WS, Settings → Remote servers UI.