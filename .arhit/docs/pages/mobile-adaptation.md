# Mobile-адаптация tmux-web (Phase A/B/C/D)

Архитектурная сводка о мобильной адаптации web-фронтенда F.O.R.G.E. Затрагивает три области: layout (off-canvas sidebar, full-screen modals, scroll-snap kanban), quick-command bar для shell-ввода, и TUI quick-keys для lazygit/lazydocker/television.

## Цель

Сделать tmux-web эффективным на сенсорных устройствах (планшеты, телефоны):
- Layout адаптируется под узкие экраны (≤768px).
- Появляется off-canvas sidebar вместо постоянной левой панели.
- Под xterm-инстансами появляется горизонтальная панель быстрых команд и spec-клавиш.
- В TUI-вкладках (git/docker/telescope) появляется bar с TUI-управляющими клавишами (q, Esc, ?, :, /, h, j, k, l, Enter, ^C, Tab, стрелки).
- Сохраняется работа с PTY без физической клавиатуры.

## Breakpoints

- `max-width: 768px` — основной mobile breakpoint. Меняется layout (off-canvas, kanban scroll, quick-bar positioning).
- `max-width: 480px` — узкие телефоны. Дополнительно уменьшаются padding и шрифты.
- `(max-width: 768px) and (prefers-reduced-motion: reduce)` — отключает transition на off-canvas sidebar для accessibility.

Все breakpoints определены в `tmux-web/static/style.css` (~строки 2805-3015).

## Архитектурные решения

### Off-canvas sidebar через `body.sidebar-open` + `#sidebar-overlay`

- Sidebar (`aside.sidebar`) на mobile становится `position:fixed; transform:translateX(-100%)`.
- Класс `body.sidebar-open` (управляется JS) переключает `transform:translateX(0)` — sidebar выезжает справа налево.
- `#sidebar-overlay` (sibling после `</aside>`) — полупрозрачный overlay поверх контента. Click по нему или Esc-keydown закрывают sidebar.
- `restoreSidebarState()` на mobile всегда стартует с закрытым sidebar (не использует localStorage для desktop-состояния).

### `matchMedia('(max-width: 768px)')` как единый источник правды

JS-код в `app.js` создаёт один MediaQueryList `_mqlMobile` и подписывается на `change`. Listener:
- Перерисовывает sidebar (mobile↔desktop переключение).
- Применяет `applyTerminalFontSize()` (mobile=11px, desktop=13px).
- Прячет/показывает все `.tui-quick-bar` (на desktop — `hidden=true`).

Это даёт реактивное переключение UI без перезагрузки страницы (например, при повороте экрана или resize окна).

### `window.ForgeApp.sendToActivePty` как единый диспетчер

Public API, экспортированный из `app.js` (`window.ForgeApp = { sendToActivePty, state }`). Маршрутизирует raw-байты в активный PTY по `state.activeTab`:
- `terminal` → main `/ws/attach`.
- `git` → `/ws/lazygit`.
- `docker` → `/ws/lazydocker`.
- `telescope` → `/ws/telescope`.

Все mobile-features (quick-cmd, TUI quick-bar) используют этот диспетчер вместо прямого доступа к WS. Это позволяет:
- `quick-cmd.js` оставаться изолированным IIFE без знания деталей WS-протокола.
- Легко добавлять новые TUI (просто новый case в `sendToActivePty`).

### `window.QuickCmd` как изолированный IIFE-модуль

Отдельный файл `tmux-web/static/quick-cmd.js`, подключённый из `index.html` после `app.js`. Зависит только от `window.ForgeApp.sendToActivePty` и нескольких DOM-id. Если модуль не загрузился — приложение работает без него.

Публичный API: `onPtyInput`, `openEditor`, `refresh`, `refreshTuiBars`. Полное описание — в [tmux-web/static/quick-cmd.js](tmux-web/static/quick-cmd.js).

## Изменённые/добавленные файлы

| Файл | Phase | Что изменилось |
|---|---|---|
| `tmux-web/static/index.html` | A/B/C | `#sidebar-overlay`, `#quick-cmd-bar`, три `.tui-quick-bar`, `<script src="/quick-cmd.js">` |
| `tmux-web/static/style.css` | A/B/C | 3 mobile media-секции (768/480/reduced-motion) в конце файла |
| `tmux-web/static/app.js` | A/B/C | mobile-ветка sidebar, matchMedia, applyTerminalFontSize, sendToActivePty, ForgeApp export, QuickCmd hooks |
| `tmux-web/static/quick-cmd.js` | B/C | новый IIFE-модуль ~450 строк |

## localStorage-схема (quick-cmd)

- `forge.quickCmd.freq` — `Record<string, number>` — счётчики команд.
- `forge.quickCmd.pinned` — `string[]` — закреплённые (всегда показываются).
- `forge.quickCmd.hidden` — `string[]` — скрытые из bar.

Подробнее — в [tmux-web/static/quick-cmd.js](tmux-web/static/quick-cmd.js).

## Алгоритм трекинга частоты команд

Хук `window.QuickCmd.onPtyInput(data)` вызывается из `term.onData(...)` на всех xterm-инстансах. Алгоритм:
1. Буферизует stdin до `\r` или `\n`.
2. На Backspace удаляет последний символ, на Ctrl+C обнуляет буфер, CSI-последовательности игнорирует.
3. При Enter — нормализует команду (trim), если непустая и не начинается с пробела (HISTCONTROL=ignorespace) — инкрементирует `freq[cmd]` в localStorage и вызывает `refresh()`.

Это даёт автоматическую персонализацию quick-cmd-bar без UI-настроек.

## TUI quick-bar architecture

Три `.tui-quick-bar` — для git, docker, telescope. Содержат fixed-набор TUI-управляющих клавиш (q, Esc, ?, :, /, h, j, k, l, Enter, ^C, Tab, стрелки в виде CSI `\x1b[A/B/C/D`).

### MutationObserver

Поскольку TUI-панели управляются атрибутом `hidden`, `quick-cmd.js` подписывается на изменения атрибута через `MutationObserver` на трёх контейнерах. При смене видимости активной TUI-панели — bar перерисовывается.

### Mobile-only видимость

Через `matchMedia('(max-width: 768px)')`. На desktop все `.tui-quick-bar` скрыты (`hidden=true`). На mobile — показываются вместе с активной TUI-панелью.

## Touch-target минимумы

Все интерактивные элементы соответствуют рекомендации Apple HIG: `min-width:44px; min-height:44px`. Это касается:
- Кнопок quick-cmd-bar (.quick-cmd-key, .quick-cmd-cmd, .quick-cmd-edit).
- TUI quick-bar (.tui-quick-key).
- Tab-кнопок top-bar.
- Sidebar items.

## Safe-area handling (iOS)

В `.quick-cmd-bar` и `.tui-quick-bar` добавлен `padding-bottom: env(safe-area-inset-bottom)` для учёта iOS home-bar. Это значит, что bar не закрывается жестом home gesture на iPhone X+.

## Kanban — горизонтальный scroll-snap

На mobile `#tasks-board` становится горизонтальной лентой с scroll-snap:
- `display:flex; flex-direction:row; overflow-x:auto; scroll-snap-type: x mandatory`.
- Каждая `.kanban-column` — `flex:0 0 85vw; scroll-snap-align:start`.

Это даёт UX как в Trello/Linear mobile.

## Full-screen модалки

Все модалки (settings, edit-task и т.п.) на mobile занимают полный экран:
- `.modal-content` → `width:100vw; height:100vh; max-width:none; border-radius:0`.
- Заголовок и кнопка закрытия — `position:sticky; top:0`.

## Точки расширения

### Как добавить новую TUI-секцию с quick-bar

1. **HTML (index.html)**: добавить panel `<section id="newtui" hidden>` с `.tui-placeholder`, `.tui-error`, `.tui-install-help`, `#newtui-term`. Внутри секции добавить `<div class="tui-quick-bar" id="newtui-quick-bar" hidden></div>`.
2. **app.js**:
   - Добавить `newTuiTerm` в `state`.
   - В `initTuiTabs()` создать инстанс через `createTuiTab({ name:'newtui', wsPath:'/ws/newtui', ... })`.
   - Добавить case в `sendToActivePty`: `case 'newtui': state.newTuiTerm?.ws?.send(text); break;`.
   - В `switchTab` зарегистрировать `'newtui'`.
3. **quick-cmd.js**:
   - Добавить id в массив `TUI_BAR_IDS = ['git-quick-bar', 'docker-quick-bar', 'telescope-quick-bar', 'newtui-quick-bar']`.
   - Добавить контейнер в MutationObserver-список.

### Как добавить новую команду в DEFAULTS

`tmux-web/static/quick-cmd.js`, константа `DEFAULTS`. После добавления — `arhit arch build && arhit analyze` чтобы обновить граф.

## Известные ограничения и идеи на будущее

- **Требуется ручной test в Chrome DevTools device toolbar**: автоматических CI-тестов для mobile-UX пока нет. Регрессии могут проскочить.
- **MutationObserver** наблюдает только атрибут `hidden`. Если в будущем кто-то начнёт прятать TUI через CSS `display:none` — quick-bar не обновится.
- **freq не различает shell** (zsh/bash/fish): одна общая таблица. Это может быть субоптимально если у пользователя разные алиасы.
- **Pipe/multiline команды**: трекаются как первая строка. Heredoc-команды теряют структуру в freq.
- **Не работает offline**: модуль рассчитан на active WS. Если WS оборвался — клик по quick-cmd no-op.
- **iOS PWA на splash-screen**: пока нет manifest.json для standalone-режима. Идея на будущее: добавить PWA-манифест чтобы tmux-web запускался как нативное приложение.
- **Pull-to-refresh**: не реализован. Можно добавить swipe-down для reconnect WS.
- **Keyboard show/hide events**: на iOS виртуальная клавиатура съедает viewport — quick-bar может перекрыться. Идея: подписаться на `visualViewport.resize` и корректировать `bottom`.

## Связанные файлы

- [tmux-web/static/index.html](tmux-web/static/index.html)
- [tmux-web/static/style.css](tmux-web/static/style.css)
- [tmux-web/static/app.js](tmux-web/static/app.js)
- [tmux-web/static/quick-cmd.js](tmux-web/static/quick-cmd.js)