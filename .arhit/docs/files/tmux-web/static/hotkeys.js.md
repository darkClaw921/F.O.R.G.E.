# tmux-web/static/hotkeys.js

Горячие клавиши веб-интерфейса. Классический IIFE-скрипт (НЕ ES-модуль), подключён в static/index.html как <script src='/hotkeys.js'>, выполняется раньше графа ES-модулей. Не экспортирует ничего в window.

Файл реализует ДВЕ независимые системы.

## 1. Vim-навигация (всегда включена, настройкой не управляется)

vimAction() — 1/2/3 → Terminal/Tasks/Git, gt/gT → цикл вкладок (pendingG таймаут 700мс), j/k → сессия вниз/вверх, h/l → фокус сайдбар/основная панель, Enter → выбрать сфокусированную сессию, ? → справка, Esc → отмена. Guard isEditingTarget() гасит vim внутри input/textarea/contentEditable/xterm. Первой строкой vimAction стоит 'if (e.metaKey || e.ctrlKey || e.altKey) return false' — поэтому нативные Cmd-шорткаты (Cmd+B сайдбар из core/bootstrap.js, Cmd+C в xterm) не перехватываются.

## 2. Cmd-hold hint mode (opt-in, ПО УМОЛЧАНИЮ ВЫКЛЮЧЕН)

Vimium-style: удержание ⌘ дольше CMD_HOLD_DELAY_MS=200 рисует на всех видимых кликабельных элементах (SELECTOR) жёлтые бейджи с буквенными кодами из HINT_ALPHABET ('asdfjkl...' — home-row first, одно- или двухбуквенные). Набор кода → hideHints() + activate(el) = focus() + click() (для input/textarea/select только focus).

Ключевые функции: showHints() (сбор видимых элементов + отрисовка), hideHints(), applyTyped() (dim/partial по префиксу), isVisible() (рекурсивно вверх: размер, viewport, hidden/display/visibility/opacity), generateCodes(), activate().

### Гейт настройки

cmdHintsEnabled() читает window.ForgeApp.state.userSettings.cmd_hints_enabled — публичный контракт js/public-api.js, где state это ТА ЖЕ живая ссылка, в которую settings/user-settings-api.js пишет userSettings. Тот же приём, что у quick-cmd.js:607 (ForgeApp.state.activeTab). Импортировать state напрямую нельзя — файл не модуль.

Строгое '=== true' даёт нужную деградацию: пока модули не загрузились или fetch настроек упал — фича выключена, что совпадает с дефолтом. Самостоятельный fetch тут не подошёл бы: /hotkeys.js раздаётся публично без токена (auth.rs), а /api/user-settings под PROTECTED_PREFIXES и в remote-режиме вернул бы 401.

Гейт стоит ВНУТРИ ветки Meta в onKeyDown, а НЕ в начале функции — иначе умерла бы и vim-часть (vimAction достигается ниже по тому же обработчику). При выключенной фиче ранний return не ставит state.cmdHeld, поэтому ветка «Cmd+другая клавиша» не срабатывает, событие уходит в vimAction, который отсеивает metaKey — нативные шорткаты живы, onKeyUp инертен.

Справка ? (updateCmdHelpLine) обновляет строку про ⌘-метки при КАЖДОМ показе: карточка кешируется в helpEl, иначе текст застыл бы в состоянии на момент первого открытия.

## Состояние и listeners

state = { hintsActive, cmdTimer, cmdHeld, cmdSawOther, hints[], typed, pendingG, pendingGTimer }. keydown/keyup на document в CAPTURE-фазе (чтобы работать поверх xterm). Хинты гасятся по: keyup Meta, Escape, полному совпадению, отсутствию частичных совпадений, window blur, scroll (capture), resize.

## Стили

css/tui-channels.css:1-33 (имя файла историческое): #hotkey-hints-layer (fixed, inset:0, z-index 99999, display переключается инлайн-стилем — класса body.cmd-held НЕТ), .hotkey-hint (#ffd54f), .hotkey-hint-partial, .hotkey-hint-dim, #hotkey-help. Цвета захардкожены, темы их не переопределяют.

## Кеширование

/hotkeys.js лежит в SHELL_ASSETS sw.js и раздаётся через stale-while-revalidate — при изменении файла ОБЯЗАТЕЛЕН бамп CACHE_VERSION в sw.js (+ синхронно константы в tests/frontend/sw.test.js), иначе первый reload выполнит старую копию.
