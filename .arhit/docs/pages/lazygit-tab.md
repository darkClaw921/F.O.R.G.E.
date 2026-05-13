# lazygit-tab — Git-вкладка через lazygit в PTY+WebSocket

## Цель

Заменить custom git UI (раннее — собственный JSON-API с git status / diff / commit / log) на интеграцию **lazygit** (TUI) внутри браузерной вкладки. Lazygit запускается на сервере в выделенном PTY, его I/O проксируется по WebSocket в инстанс xterm.js на фронтенде. Пользователь работает с git в полноценном TUI, идентичном локальному запуску `lazygit` в терминале, не покидая F.O.R.G.E.

**Почему lazygit, а не свой UI:**
- Бесплатно получаем staging hunks, interactive rebase, stash, log, diff viewer, cherry-pick, branch ops — годы разработки чужого TUI.
- Меньше кода в нашем репо, меньше багов с edge-кейсами git (rebase, conflicts, submodules).
- Symmetric flow с tmux-аттачем — один и тот же примитив (PTY ↔ WS ↔ xterm).

## Архитектура

### Backend компоненты (`tmux-web/src/`)

#### `pty.rs::spawn_lazygit(cwd, cols, rows) -> Result<PtyHandle>`
Спавнит `lazygit` в новом PTY размера `cols × rows` с рабочим каталогом `cwd`.

- **`CommandBuilder::new("lazygit")`** — без аргументов; lazygit сам найдёт ближайший `.git` от `cwd`.
- **ENV:**
  - `TERM=xterm-256color` — обязательно, иначе TUI отрисуется без цветов / с поломанными box-drawing символами.
  - Унаследованный `HOME` — чтобы lazygit подхватил `/Users/igorgerasimov/.config/lazygit/config.yml`.
- **Error handling:** если `lazygit` не найден в PATH, `spawn_command` возвращает `Err`, обёрнутую человеко-читаемым сообщением "lazygit not found in PATH, install via `brew install lazygit` (macOS) or your distro's package manager". Это сообщение ловится в WS-handler'е и форвардится клиенту как control-frame `{"type":"error","message":"..."}`.

Симметричен `spawn_tmux_attach`, но отдельный entry point: разные жизненные циклы (lazygit умирает при `q`, tmux-аттач переживает реконнекты).

#### `ws.rs::lazygit_attach(ws, q: LazygitQuery)`
HTTP→WS upgrade handler.

- **Query-параметры (`LazygitQuery`):** `cwd: String` (абсолютный путь к проекту), `cols: u16`, `rows: u16`.
- **Маршрут:** `GET /ws/lazygit` зарегистрирован в `main.rs`.
- После upgrade передаёт сокет в `handle_lazygit_socket`.

#### `ws.rs::handle_lazygit_socket(socket, q)`
Основной обработчик соединения.

- Создаёт первый PTY через `spawn_lazygit(&cwd, cols, rows)`.
- При ошибке spawn — шлёт клиенту `Text` frame `{"type":"error","message":"spawn failed: ..."}` и закрывает WS.
- При успехе разворачивает две задачи:
  - **PTY→WS reader:** читает stdout PTY, форвардит chunks как `Message::Binary`.
  - **WS→PTY writer:** обрабатывает входящие фреймы:
    - `Binary` → байты в PTY stdin (keystrokes пользователя).
    - `Text` → парсится как `LazygitControl` JSON (см. ниже).
- При закрытии WS / EOF PTY оба таска корректно останавливаются, дочерний процесс `lazygit` убивается.

#### `ws.rs::LazygitControl` (control protocol)
JSON-сообщения от клиента в `Text` frame:

```json
{"type":"resize","cols":120,"rows":40}
{"type":"switch_cwd","cwd":"/new/project/path","cols":120,"rows":40}
```

- **`resize`** — меняет размер текущего PTY (`set_pty_size`), что вызывает SIGWINCH в lazygit, и TUI перерисуется.
- **`switch_cwd`** — убивает текущий процесс lazygit, спавнит новый в указанном `cwd`. Используется при переключении активного проекта в F.O.R.G.E. — клиенту не нужно переподключать WS, handler делает rotate PTY на месте. Симметрия с tmux отсутствует (там используется attach к именованной сессии), потому что lazygit оперирует *путём к репозиторию*, а не *именем*.

### Frontend компоненты (`tmux-web/static/app.js`)

#### `state.gitTerm` объект
Глобальное состояние git-вкладки:

```js
{
  term:    null,   // xterm.Terminal
  fit:     null,   // FitAddon
  ws:      null,   // WebSocket к /ws/lazygit
  mounted: false,  // term.open() вызван
  cwd:     null,   // текущий cwd, для которого открыт ws
}
```

#### `initGitTerm()`
Создаёт `xterm.Terminal` с настроенной темой, FitAddon, монтирует в DOM-элемент `#git-term`. Подписывается на:
- **`onData(data)`** — шлёт keystrokes как `Uint8Array` в `ws` (Binary frame).
- **`onResize({cols, rows})`** — шлёт control-frame `{type:'resize', cols, rows}` в `ws` (Text frame).
- **`window.resize`** — вызывает `fit.fit()`, что триггерит `onResize`.

#### `openGitTab(cwd)`
Открывает (или переподключает) WS `/ws/lazygit` для активного проекта. Если `gitTerm` ещё не смонтирован — монтирует. Если `cwd` уже совпадает с открытым — no-op. Иначе:
- Если есть активный WS — шлёт `{type:'switch_cwd', cwd, cols, rows}` (rotate без реконнекта).
- Если WS закрыт / нет — открывает новый `/ws/lazygit?cwd=...&cols=...&rows=...`.

#### `closeGitTab()`
Вызывается при переключении с git-вкладки на другую (Tmux/Tasks/etc.). Закрывает WS, скрывает `#git-term`. Сам `xterm.Terminal` не уничтожается — переиспользуется при следующем открытии (быстрый ре-аттач).

#### Обработка ошибок на клиенте
- Text frame `{type:'error', message:'...'}` от сервера → парсится, показывается баннер в `#git-term` через `term.write`.
- Спец-кейс: если `message` содержит "lazygit" + ("not found" | "no such file") — баннер заменяется на установочную подсказку с ссылкой на https://github.com/jesseduffield/lazygit.
- WS `onclose` / `onerror` — показывается "connection lost" banner с кнопкой retry.
- Non-JSON Text frame — логируется в `console.warn`, игнорируется.

## Поток данных

```
[пользователь нажимает клавишу]
        ↓
xterm.onData(data: string)
        ↓
ws.send(new TextEncoder().encode(data))   ← Binary frame
        ↓
WS /ws/lazygit на сервере
        ↓
handle_lazygit_socket: msg = Binary(bytes)
        ↓
PtyHandle::writer.write_all(&bytes)
        ↓
[ядро доставляет байты в stdin lazygit]
        ↓
lazygit обрабатывает keystroke, перерисовывает TUI
        ↓
байты из stdout lazygit
        ↓
PtyHandle::reader.read(buf)
        ↓
ws.send(Message::Binary(buf))
        ↓
ws.onmessage(event: Blob)
        ↓
event.arrayBuffer() → Uint8Array → term.write(uint8)
        ↓
[пользователь видит обновлённый TUI]
```

Для resize / switch_cwd используется отдельный Text-канал поверх того же WS — control plane не смешивается с data plane.

## switch_cwd flow

Когда пользователь переключает активный проект в UI (нажал на другой проект в списке):

1. Frontend: `onProjectChange(newCwd)` → `openGitTab(newCwd)`.
2. `openGitTab` видит, что WS открыт и `state.gitTerm.cwd !== newCwd`.
3. Шлёт `ws.send(JSON.stringify({type:'switch_cwd', cwd:newCwd, cols, rows}))`.
4. Backend `handle_lazygit_socket`: парсит `LazygitControl::SwitchCwd`.
5. Убивает текущий child процесс lazygit, ждёт reaper.
6. Вызывает `spawn_lazygit(&new_cwd, cols, rows)` → новый PtyHandle.
7. Перезапускает PTY→WS reader на новом stdout.
8. Lazygit стартует в новом репо, его initial draw улетает в клиента → xterm перерисовывает TUI.
9. Frontend сохраняет `state.gitTerm.cwd = newCwd`.

Не требуется переоткрывать WebSocket — экономия одного round-trip и сохранение клиентских listeners.

## Error handling — полный list

| Условие | Где ловится | Реакция |
|---------|-------------|---------|
| `lazygit` нет в PATH | `spawn_lazygit` | Err → Text frame `error` → клиент рисует installation hint |
| `cwd` не существует | `spawn_lazygit` (portable-pty) | Err → Text frame `error` → клиент рисует error banner |
| WS закрыт клиентом | `handle_lazygit_socket` writer loop | kill PTY → корректное завершение |
| lazygit завершился (`q`) | PTY reader получает EOF | закрывается WS со стороны сервера |
| Bad JSON в Text frame | `serde_json::from_str` в handler | log warn, ignore (не убивает соединение) |
| WS connection lost | `ws.onclose` на клиенте | "connection lost" banner + retry button |
| `set_pty_size` failed | log warn, не fatal | TUI продолжит работать в старом размере |

## Зависимости

### Внешние утилиты
- **`lazygit`** (https://github.com/jesseduffield/lazygit) — обязательно в `PATH`. Установка:
  - macOS: `brew install lazygit`
  - Arch: `pacman -S lazygit`
  - Debian/Ubuntu: см. README репозитория lazygit (нет в стандартных репах apt)

### Rust crates (backend)
- `portable-pty` — кросс-платформенный PTY (используется и для tmux, и для lazygit).
- `axum` (с `ws` feature) — WebSocket upgrade и framing.
- `tokio` — async runtime, JoinHandle для reader/writer tasks.
- `serde` + `serde_json` — десериализация `LazygitControl`.

### JS зависимости (frontend, через CDN/локально)
- `xterm.js` (Terminal, addons).
- `xterm-addon-fit` (FitAddon — авто-расчёт cols/rows по контейнеру).

## Архитектурные решения

### Почему отдельный handler от `tmux_attach`
- **Разные жизненные циклы:** tmux-сессия переживает разрывы WS (можно реконнектиться к той же сессии). Lazygit умирает при `q` — нет смысла переподключаться.
- **Разные параметры:** tmux идентифицируется по имени сессии, lazygit — по пути к репо.
- **Разная семантика `switch`:** tmux переключает сессию (`attach -t new_name`), lazygit убивается и стартует заново.
- Поделить общий код в `handle_lazygit_socket` / `handle_socket` можно, но overlap небольшой (~30 строк) и абстракция сделала бы код менее читаемым.

### Почему JSON в Text, а не отдельный binary control protocol
- Простота: `JSON.stringify` / `serde_json` уже есть в обоих местах.
- Read-friendly logs: контрол-фреймы видны в DevTools / tracing без декодеров.
- Перформанс не критичен — control-фреймы это единицы в секунду (ресайз окна), data-фреймы — десятки/сотни кБ/с в Binary.

### Почему frontend не уничтожает `xterm.Terminal` при `closeGitTab`
- Cold start xterm + FitAddon на низкоконечных машинах = ~100–200 мс.
- Открытие git-таба должно быть мгновенным (`hidden=false` + reattach WS).
- Очистка экрана делается через `term.reset()` при switch_cwd, а не пересоздание инстанции.