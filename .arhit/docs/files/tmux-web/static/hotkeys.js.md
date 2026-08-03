# tmux-web/static/hotkeys.js

Горячие клавиши веб-интерфейса. Классический IIFE-скрипт (НЕ ES-модуль), подключён в static/index.html как <script src='/hotkeys.js'>, выполняется раньше графа ES-модулей. Не экспортирует ничего в window.

Файл реализует ДВЕ независимые системы и содержит ДВА гейта.

## 1. Vim-навигация (всегда включена, настройкой не управляется)

vimAction() — 1/2/3 → Terminal/Tasks/Git, gt/gT → цикл вкладок (pendingG таймаут 700мс), j/k → сессия вниз/вверх, h/l → фокус сайдбар/основная панель, Enter → выбрать сфокусированную сессию, ? → справка, Esc → отмена. Guard isEditingTarget() гасит vim внутри input/textarea/contentEditable/xterm. Первой строкой vimAction стоит 'if (e.metaKey || e.ctrlKey || e.altKey) return false' — поэтому нативные Cmd-шорткаты (Cmd+B сайдбар, Cmd+C в xterm) не перехватываются.

### Гейт панелей-мессенджеров (forge-nert → расширен в Ф11 VK)

Третьей строкой vimAction стоит 'if (panelOwnsKeys()) return false'. Панели Telegram и VK используют те же j/k/h/l/g/1/2/3/? для своих чатов и сообщений, и без гейта одно нажатие 'j' и листало бы чаты, и уводило фокус на сессию в сайдбаре.

panelOwnsKeys() идёт ЦИКЛОМ по константе PANEL_KEY_CONTRACTS = ['telegramKeys', 'vkKeys'] и спрашивает window.ForgeApp[<имя>].owns(). Контракт публикуется панелью в её initKeynav() и появляется только после загрузки ES-модулей (этот файл выполняется раньше) — до тех пор ответ 'нет', что совпадает с поведением до фичи. Отсутствие контракта, отсутствие owns(), исключение внутри owns() и любой не-true ответ пропускаются: сравнение строгое, а сломанная панель не повод отнимать у пользователя навигацию.

owns() каждой панели сам учитывает свою настройку (telegram_hotkeys_enabled / vk_hotkeys_enabled, оба читаются '!== false'), видимость своего пейна и положение фокуса: уйдя в сайдбар сессий, пользователь возвращает себе vim. Одновременно ответить true панели не могут — switchTab держит непогашенным ровно один пейн.

ПОЧЕМУ СПИСОК ИМЁН, А НЕ ОБЩИЙ РЕЕСТР app.keyboardOwners, в который панели регистрируются сами: реестр потребовал бы правки работающего js/telegram/keynav.js — единственной причины её трогать, и ради нуля пользовательской выгоды. Независимого деплоя нет (hotkeys.js и обе панели собираются в один бинарь через rust-embed), а одно имя было захардкожено тут и до появления второй панели. Реестр вводить, когда появится третья.

Автоматическое покрытие: tmux-web/tests/frontend/hotkeys_panel_owner.test.mjs (файл грузится как CommonJS на фейковый DOM, наблюдаемое следствие «vim сработал» — preventDefault).

## 2. Cmd-hold hint mode (opt-in, ПО УМОЛЧАНИЮ ВЫКЛЮЧЕН)

Vimium-style: удержание ⌘ дольше CMD_HOLD_DELAY_MS=200 рисует на всех видимых кликабельных элементах (SELECTOR) жёлтые бейджи с буквенными кодами из HINT_ALPHABET ('asdfjkl...' — home-row first, одно- или двухбуквенные). Набор кода → hideHints() + activate(el) = focus() + click() (для input/textarea/select только focus).

Ключевые функции: showHints(), hideHints(), applyTyped() (dim/partial по префиксу), isVisible() (рекурсивно вверх: размер, viewport, hidden/display/visibility/opacity), generateCodes(), activate().

### Гейт настройки

cmdHintsEnabled() читает window.ForgeApp.state.userSettings.cmd_hints_enabled — публичный контракт js/public-api.js, где state это ТА ЖЕ живая ссылка, в которую settings/user-settings-api.js пишет userSettings, так что переключение тумблера подхватывается со следующего нажатия, без перезагрузки. Строгое '=== true' даёт нужную деградацию: пока модули не загрузились или fetch настроек упал — фича выключена, что совпадает с дефолтом.

ВАЖНО: файл раздаётся через service worker стратегией stale-while-revalidate, поэтому ЛЮБАЯ правка требует бампа CACHE_VERSION в static/sw.js — иначе первый reload после обновления выполнит старую копию. История бампов: v2 — гейт cmd_hints_enabled, v3 — гейт telegram, v4 — гейт vk (второй владелец клавиатуры).
