# Smoke-test чек-лист: TUI-вкладки (lazygit / lazydocker / telescope)

Чек-лист для ручного тестирования вкладок Lazygit, Lazydocker (Docker) и Telescope (tv) в tmux-web после Phase 1-3.
Тестирование требует user-interaction — выполняется человеком после сборки release-бинаря.

## Подготовка

```bash
cd tmux-web
cargo run --release
# Узнать порт из логов CLI или из GET /healthz
# Открыть http://localhost:<PORT> в браузере
```

В системе должны быть установлены бинари:
- `lazygit` (https://github.com/jesseduffield/lazygit)
- `lazydocker` (https://github.com/jesseduffield/lazydocker)
- `tv` или `television` (https://github.com/alexpasmantier/television)

## 1. Lazygit-таб (регрессия — Phase 1 не должна сломать)

- [ ] Открыть вкладку Lazygit — UI инициализируется, виден интерфейс lazygit
- [ ] Переключение проекта (выбор другой сессии из sidebar) — lazygit перезапускается с новым cwd, виден git-статус нового проекта
- [ ] Ресайз окна браузера — PTY получает resize, отображение корректно подстраивается под новый размер (cols/rows)
- [ ] Закрытие/повторное открытие вкладки — корректный reconnect, нет zombie-процессов
- [ ] Error-banner при удалении `lazygit` из PATH: должна показаться плашка с install-help (`brew install jesseduffield/lazygit/lazygit` и пр.)

## 2. Lazydocker-таб (Docker — новая Phase 1+2)

- [ ] Кнопка "Docker" присутствует в sidebar/header (рядом с Lazygit/Telescope)
- [ ] Клик по кнопке — открывается вкладка Lazydocker, виден TUI lazydocker (контейнеры/images/volumes/networks)
- [ ] Ресайз окна браузера — корректный resize PTY, layout lazydocker перестраивается
- [ ] Смена активного проекта — lazydocker перезапускается с новым cwd (если для docker контекста есть смысл cwd)
- [ ] Error-banner при отсутствии `lazydocker` в PATH — показывается с install-help (`brew install lazydocker` / `go install github.com/jesseduffield/lazydocker@latest`)
- [ ] WS-маршрут: в DevTools видно подключение к `/ws/lazydocker`

## 3. Telescope-таб (tv — новая Phase 1+2)

- [ ] Кнопка "Telescope" присутствует в sidebar/header
- [ ] Клик по кнопке — открывается вкладка, виден интерфейс `tv` (television)
- [ ] Ресайз окна браузера — корректный resize PTY
- [ ] Смена активного проекта — `tv` перезапускается с новым cwd, fuzzy-поиск работает в новой директории
- [ ] Error-banner при отсутствии `tv`/`television` в PATH — показывается с install-help (`cargo install television` / `brew install television`)
- [ ] WS-маршрут: в DevTools видно подключение к `/ws/telescope`

## 4. Cross-cutting: смена активного проекта

- [ ] При смене активного проекта (выбор другой сессии в sidebar) во ВСЕХ трёх TUI (lazygit, lazydocker, telescope), которые открыты как вкладки, происходит:
  - cwd переключается на путь нового проекта
  - PTY перезапускается БЕЗ reconnect WebSocket (или с прозрачным reconnect, не требующим перезагрузки страницы)
  - старый процесс корректно убивается (нет orphan child)

## 5. Remote-режим (через /api/remote-servers)

Предусловие: запущены два сервера, один в режиме remote-host, второй в режиме remote-client, добавлен remote-server в client UI.

- [ ] Подключение к remote-серверу через UI работает (отображается список его проектов)
- [ ] Открытие Lazygit-таба на remote-сервере — WS проксируется через `/api/remote-servers/:id/ws/lazygit`, виден TUI lazygit удалённого сервера
- [ ] Открытие Lazydocker-таба на remote-сервере — WS проксируется через `/api/remote-servers/:id/ws/lazydocker`, виден lazydocker удалённого сервера
- [ ] Открытие Telescope-таба на remote-сервере — WS проксируется через `/api/remote-servers/:id/ws/telescope`, виден tv удалённого сервера
- [ ] Ресайз и control-сообщения (switch/resize) корректно пересылаются upstream

## 6. Acceptance criteria

Все пункты выше — выполнены, никаких регрессий в lazygit, оба новых таба функционируют идентично lazygit-табу по UX (resize / switch / error-banner / install-help).

## Связанные файлы

- Backend: [tmux-web/src/pty.rs](../../tmux-web/src/pty.rs), [tmux-web/src/ws.rs](../../tmux-web/src/ws.rs), [tmux-web/src/main.rs](../../tmux-web/src/main.rs)
- Remote-proxy: [tmux-web/src/remote_proxy.rs](../../tmux-web/src/remote_proxy.rs)
- Frontend: [tmux-web/static/index.html](../../tmux-web/static/index.html), [tmux-web/static/app.js](../../tmux-web/static/app.js), [tmux-web/static/style.css](../../tmux-web/static/style.css)
