# tmux-web/static/hotkeys.js

Горячие клавиши веб-интерфейса. Классический IIFE-скрипт (НЕ ES-модуль), подключён в static/index.html как <script src='/hotkeys.js'>, выполняется раньше графа ES-модулей. Не экспортирует ничего в window.

Файл реализует ДВЕ независимые системы и содержит ДВА гейта.

## 1. Vim-навигация (всегда включена, настройкой не управляется)

vimAction() — 1/2/3 → Terminal/Tasks/Git, gt/gT → цикл вкладок (pendingG таймаут 700мс), j/k → сессия вниз/вверх, h/l → фокус сайдбар/основная панель, Enter → выбрать сфокусированную сессию, ? → справка, Esc → отмена. Guard isEditingTarget() гасит vim внутри input/textarea/contentEditable/xterm. Первой строкой vimAction стоит 'if (e.metaKey || e.ctrlKey || e.altKey) return false' — поэтому нативные Cmd-шорткаты (Cmd+B сайдбар, Cmd+C в xterm) не перехватываются.

### Гейт Telegram (добавлен в forge-nert)

Третьей строкой vimAction стоит 'if (telegramOwnsKeys()) return false'. Панель Telegram использует те же j/k/h/l/g/1/2/3/? для своих чатов и сообщений, и без гейта одно нажатие 'j' и листало бы чаты, и уводило фокус на сессию в сайдбаре. Контракт window.ForgeApp.telegramKeys.owns() появляется только после загрузки ES-модулей (этот файл выполняется раньше) — до тех пор ответ 'нет', что совпадает с поведением до фичи. owns() сам учитывает и настройку telegram_hotkeys_enabled, и положение фокуса: уйдя в сайдбар сессий, пользователь возвращает себе vim.

## 2. Cmd-hold hint mode (opt-in, ПО УМОЛЧАНИЮ ВЫКЛЮЧЕН)

Vimium-style: удержание ⌘ дольше CMD_HOLD_DELAY_MS=200 рисует на всех видимых кликабельных элементах (SELECTOR) жёлтые бейджи с буквенными кодами из HINT_ALPHABET ('asdfjkl...' — home-row first, одно- или двухбуквенные). Набор кода → hideHints() + activate(el) = focus() + click() (для input/textarea/select только focus).

Ключевые функции: showHints(), hideHints(), applyTyped() (dim/partial по префиксу), isVisible() (рекурсивно вверх: размер, viewport, hidden/display/visibility/opacity), generateCodes(), activate().

### Гейт настройки

cmdHintsEnabled() читает window.ForgeApp.state.userSettings.cmd_hints_enabled — публичный контракт js/public-api.js, где state это ТА ЖЕ живая ссылка, в которую settings/user-settings-api.js пишет userSettings, так что переключение тумблера подхватывается со следующего нажатия, без перезагрузки. Строгое '=== true' даёт нужную деградацию: пока модули не загрузились или fetch настроек упал — фича выключена, что совпадает с дефолтом.

ВАЖНО: файл раздаётся через service worker стратегией stale-while-revalidate, поэтому ЛЮБАЯ правка требует бампа CACHE_VERSION в static/sw.js — иначе первый reload после обновления выполнит старую копию.
