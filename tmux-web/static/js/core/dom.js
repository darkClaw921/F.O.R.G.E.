// tmux-web — DOM references (Phase 0 ES Modules refactor)
//
// 1:1 копия блока `const $... = document.getElementById(...)` из IIFE
// `tmux-web/static/app.js` (строки 173-233, 674). Все ref'ы вычисляются один
// раз при первом импорте модуля.
//
// КОРРЕКТНО ТОЛЬКО при импорте после полной загрузки DOM. В Phase 1 main.js
// будет подключён через `<script type="module">` — это гарантирует implicit
// `defer`, т.е. модуль выполнится после parse HTML. Документированное
// поведение HTML-спецификации.
//
// В Phase 0 модуль ещё НЕ подключен к index.html — legacy app.js работает
// как раньше; модуль готов к импорту из main.js в Phase 1.
//
// НЕ импортирует ничего, кроме браузерного DOM. Pure leaf-module.

// ---- Layout / sidebar ----
export const $layout = document.getElementById('layout');
export const $btnSidebarToggle = document.getElementById('btn-sidebar-toggle');
export const $sidebar = document.getElementById('session-list');
export const $btnNew = document.getElementById('btn-new');
export const $btnNewPath = document.getElementById('btn-new-path');
export const $sidebarOverlay = document.getElementById('sidebar-overlay');

// ---- Terminal / window-bar / status ----
export const $terminalEl = document.getElementById('terminal');
export const $placeholder = document.getElementById('placeholder');
export const $windowBar = document.getElementById('window-bar');
export const $windowTabs = document.getElementById('window-tabs');
export const $windowNewBtn = document.getElementById('window-new');
export const $windowNewWorktreeBtn = document.getElementById('window-new-worktree');
export const $statusDot = document.getElementById('status-dot');
export const $statusText = document.getElementById('status-text');

// ---- Phase 6.A: tab-bar + Tasks UI ----
export const $tabTerminal = document.getElementById('tab-terminal');
export const $tabTasks = document.getElementById('tab-tasks');
export const $tasksStatus = document.getElementById('tasks-status');
export const $tasksEl = document.getElementById('tasks');
export const $tasksReload = document.getElementById('tasks-reload');
export const $tasksNew = document.getElementById('tasks-new');
export const $tasksMeta = document.getElementById('tasks-meta');
export const $tasksBoard = document.getElementById('tasks-board');

// ---- Gantt timeline (gantt-диаграмма под канбан-доской вкладки Tasks) ----
export const $tasksGantt = document.getElementById('tasks-gantt');
export const $ganttCanvas = document.getElementById('gantt-canvas');
export const $ganttRange = document.getElementById('gantt-range');
export const $ganttToggle = document.getElementById('gantt-toggle');

// ---- Git tab (lazygit) ----
export const $tabGit = document.getElementById('tab-git');
export const $gitEl = document.getElementById('git');
export const $gitTermEl = document.getElementById('git-term');
export const $gitPlaceholder = document.getElementById('git-placeholder');
export const $gitError = document.getElementById('git-error');
export const $gitErrorText = document.getElementById('git-error-text');
export const $gitErrorRetry = document.getElementById('git-error-retry');
export const $gitErrorClose = document.getElementById('git-error-close');
export const $gitInstallHelp = document.getElementById('git-install-help');
export const $gitInstallList = document.getElementById('git-install-list');

// ---- Docker tab (lazydocker) ----
export const $tabDocker = document.getElementById('tab-docker');
export const $dockerEl = document.getElementById('docker');
export const $dockerTermEl = document.getElementById('docker-term');
export const $dockerPlaceholder = document.getElementById('docker-placeholder');
export const $dockerError = document.getElementById('docker-error');
export const $dockerErrorText = document.getElementById('docker-error-text');
export const $dockerErrorRetry = document.getElementById('docker-error-retry');
export const $dockerErrorClose = document.getElementById('docker-error-close');
export const $dockerInstallHelp = document.getElementById('docker-install-help');
export const $dockerInstallList = document.getElementById('docker-install-list');

// ---- Telescope tab (tv) ----
export const $tabTelescope = document.getElementById('tab-telescope');
export const $telescopeEl = document.getElementById('telescope');
export const $telescopeTermEl = document.getElementById('telescope-term');
export const $telescopePlaceholder = document.getElementById('telescope-placeholder');
export const $telescopeError = document.getElementById('telescope-error');
export const $telescopeErrorText = document.getElementById('telescope-error-text');
export const $telescopeErrorRetry = document.getElementById('telescope-error-retry');
export const $telescopeErrorClose = document.getElementById('telescope-error-close');
export const $telescopeInstallHelp = document.getElementById('telescope-install-help');
export const $telescopeInstallList = document.getElementById('telescope-install-list');
export const $telescopeChannelBar = document.getElementById('telescope-channel-bar');

// ---- Settings bar ----
export const $projectSettings = document.getElementById('project-settings');

// ---- Claude memory button (шапка сессии tmux) ----
export const $claudeMemoryBtn = document.getElementById('claude-memory-btn');

// ---- Screensaver (заставка «таверна дворфов») ----
//
// Кнопка-переключатель в #settings-bar и полноэкранная вью #screensaver в
// #main (по образцу #daily-summary). Видимость и анимацию переключает
// js/screensaver/screensaver.js::showScreensaver / hideScreensaver.
export const $screensaverToggle = document.getElementById('screensaver-toggle');
export const $screensaver = document.getElementById('screensaver');
export const $screensaverBack = document.getElementById('screensaver-back');
export const $ssStage = document.getElementById('ss-stage');

// ---- Phase 5: origin-табы (скрыты при remote_mode=false) ----
export const $originTabs = document.getElementById('origin-tabs');

// ---- Home (главная страница: история недавних сессий) ----
export const $home = document.getElementById('home');
export const $homeCards = document.getElementById('home-cards');
export const $homeRestoreAll = document.getElementById('home-restore-all');
export const $homeRestoreSelected = document.getElementById('home-restore-selected');
export const $homeEmpty = document.getElementById('home-empty');

// ---- Daily summary (Сводка дня) ----
//
// Селекторы для вью #daily-summary. Видимость переключает
// js/daily-summary/daily-summary.js::showDailySummary. Все ref'ы вычисляются
// один раз при импорте; если узел отсутствует в DOM — переменная будет null.
export const $dailySummary = document.getElementById('daily-summary');
export const $dailySummaryBack = document.getElementById('daily-summary-back');
export const $dailySummaryPrev = document.getElementById('daily-summary-prev');
export const $dailySummaryToday = document.getElementById('daily-summary-today');
export const $dailySummaryNext = document.getElementById('daily-summary-next');
export const $dailySummaryDay = document.getElementById('daily-summary-day');
export const $dailySummaryRegen = document.getElementById('daily-summary-regen');
export const $dailySummaryStatus = document.getElementById('daily-summary-status');
export const $dailySummaryContent = document.getElementById('daily-summary-content');
export const $dailySummarySuggestions = document.getElementById('daily-summary-suggestions');
export const $dailySummaryEmpty = document.getElementById('daily-summary-empty');
export const $dailySummaryGenerate = document.getElementById('daily-summary-generate');

// ---- Echo plugin (Phase 5c) ----
//
// Селекторы для DOM-узлов Echo-вкладки. Совпадают с id из index.html.
// Все ref'ы вычисляются один раз при импорте; если узел отсутствует в DOM
// (например, fallback на старый шаблон) — переменная будет null. Frontend
// должен проверять truthy перед обращением.
export const $tabEcho = document.getElementById('tab-echo');
export const $echoEl = document.getElementById('echo');
export const $echoSidebar = document.getElementById('echo-sidebar');
export const $echoSidebarTabChats = document.getElementById('echo-sidebar-tab-chats');
export const $echoSidebarTabAuto = document.getElementById('echo-sidebar-tab-auto');
export const $echoSidebarTabMemory = document.getElementById('echo-sidebar-tab-memory');
export const $echoConversations = document.getElementById('echo-conversations');
export const $echoConversationsList = document.getElementById('echo-conversations-list');
export const $echoNewChat = document.getElementById('echo-new-chat');
export const $echoAutonomous = document.getElementById('echo-autonomous');
export const $echoAutonomousList = document.getElementById('echo-autonomous-list');
export const $echoNewAuto = document.getElementById('echo-new-auto');
export const $echoMemory = document.getElementById('echo-memory');
export const $echoMemoryList = document.getElementById('echo-memory-list');
export const $echoMemoryRegen = document.getElementById('echo-memory-regen');
export const $echoMain = document.getElementById('echo-main');
export const $echoHeader = document.getElementById('echo-header');
export const $echoModelPicker = document.getElementById('echo-model-picker');
export const $echoStatus = document.getElementById('echo-status');
export const $echoStatsCanvas = document.getElementById('echo-stats-canvas');
export const $echoStatsText = document.getElementById('echo-stats-text');
export const $echoMessages = document.getElementById('echo-messages');
export const $echoInputWrap = document.getElementById('echo-input-wrap');
export const $echoInput = document.getElementById('echo-input');
export const $echoSend = document.getElementById('echo-send');
export const $echoToasts = document.getElementById('echo-toasts');

// ---- Telegram plugin ----
//
// Ref'ы вкладки мессенджера (плагин forge-telegram, opt-in `--telegram`).
// Вычисляются один раз при импорте; когда плагин выключен, узлы всё равно
// присутствуют в разметке (панель просто hidden), но код плагина обязан
// проверять truthy — index.html может отдаваться старым шаблоном.
export const $telegramToggle = document.getElementById('telegram-toggle');
export const $tgUnreadBadge = document.getElementById('tg-unread-badge');
export const $telegram = document.getElementById('telegram');
export const $tgTitlebar = document.getElementById('tg-titlebar');
export const $tgSelfUser = document.getElementById('tg-self-user');
export const $tgThemeToggle = document.getElementById('tg-theme-toggle');
export const $tgLogout = document.getElementById('tg-logout');
export const $tgContacts = document.getElementById('tg-contacts');
// ☰ и подложка off-canvas-колонки контактов (js/telegram/mobile.js): на
// десктопе кнопка скрыта через CSS, подложка — через [hidden].
export const $tgContactsToggle = document.getElementById('tg-contacts-toggle');
export const $tgContactsScrim = document.getElementById('tg-contacts-scrim');
export const $tgDirList = document.getElementById('tg-dir-list');
// Счётчик непрочитанного у ~/inbox (amber). ~/archive в v1 — пункт-заглушка,
// но ref заведён сразу: его заполняет та же renderDirs.
export const $tgDirInboxCount = document.getElementById('tg-dir-inbox-count');
export const $tgDirArchiveCount = document.getElementById('tg-dir-archive-count');
export const $tgNodesTitle = document.getElementById('tg-nodes-title');
export const $tgDialogList = document.getElementById('tg-dialog-list');
export const $tgChannelList = document.getElementById('tg-channel-list');
export const $tgSessionHead = document.getElementById('tg-session-head');
export const $tgLog = document.getElementById('tg-log');
export const $tgTyping = document.getElementById('tg-typing');
export const $tgPrompt = document.getElementById('tg-prompt');
export const $tgInput = document.getElementById('tg-input');
export const $tgAttach = document.getElementById('tg-attach');
// Скрытый input[type=file]: по нему кликает кнопка ⇪ (js/telegram/upload.js).
export const $tgFileInput = document.getElementById('tg-file-input');
export const $tgSend = document.getElementById('tg-send');
export const $tgMode = document.getElementById('tg-mode');
// Правая ячейка статусбара: счётчик контактов в сети (js/telegram/dialogs.js).
export const $tgStatusRight = document.getElementById('tg-status-right');
export const $tgLat = document.getElementById('tg-lat');
export const $tgLogin = document.getElementById('tg-login');
export const $tgLoginStepCreds = document.getElementById('tg-login-step-creds');
export const $tgLoginStepCode = document.getElementById('tg-login-step-code');
export const $tgLoginStepPassword = document.getElementById('tg-login-step-password');
export const $tgLoginError = document.getElementById('tg-login-error');
export const $tgLoginCountdown = document.getElementById('tg-login-countdown');
// Поля и кнопки 3-этапного login-wizard'а (js/telegram/login.js).
export const $tgLoginStatus = document.getElementById('tg-login-status');
export const $tgApiId = document.getElementById('tg-api-id');
export const $tgApiHash = document.getElementById('tg-api-hash');
export const $tgPhone = document.getElementById('tg-phone');
export const $tgLoginCredsSubmit = document.getElementById('tg-login-creds-submit');
export const $tgCode = document.getElementById('tg-code');
export const $tgLoginCodeSubmit = document.getElementById('tg-login-code-submit');
export const $tgLoginCodeBack = document.getElementById('tg-login-code-back');
export const $tgPassword = document.getElementById('tg-password');
export const $tgLoginPasswordSubmit = document.getElementById('tg-login-password-submit');
export const $tgLoginPasswordBack = document.getElementById('tg-login-password-back');
export const $tgLoginPhone = document.getElementById('tg-login-phone');
export const $tgLoginHintText = document.getElementById('tg-login-hint-text');
export const $tgDropzone = document.getElementById('tg-dropzone');
export const $tgLightbox = document.getElementById('tg-lightbox');
export const $tgLightboxImg = document.getElementById('tg-lightbox-img');
export const $tgLightboxClose = document.getElementById('tg-lightbox-close');
// Подпись под полноразмерным медиа в лайтбоксе: `имя · 1280×720 · 1.2 MB`.
export const $tgLightboxCaption = document.getElementById('tg-lightbox-caption');

// ---- VK plugin ----
//
// Ref'ы вкладки мессенджера ВКонтакте (плагин forge-vk, opt-in `--vk`).
// Устройство ровно то же, что у телеграмной секции выше: значения
// вычисляются ОДИН РАЗ при импорте модуля, поэтому узлы обязаны быть в
// разметке к этому моменту (панель #vk присутствует в index.html всегда,
// даже при выключенном плагине — скрыт только `hidden`).
//
// СЛЕДСТВИЕ ДЛЯ ТЕСТОВ: стаб `document` обязан стоять ДО первого импорта
// любого модуля js/vk/*, иначе все ref'ы окажутся null и тест будет
// проверять пустоту. Код панели всё равно проверяет truthy у каждого узла:
// index.html может приехать из старого кэша PWA, где секции #vk ещё нет.
export const $vkToggle = document.getElementById('vk-toggle');
export const $vkUnreadBadge = document.getElementById('vk-unread-badge');
export const $vk = document.getElementById('vk');
export const $vkTitlebar = document.getElementById('vk-titlebar');
export const $vkSelfUser = document.getElementById('vk-self-user');
export const $vkThemeToggle = document.getElementById('vk-theme-toggle');
export const $vkLogout = document.getElementById('vk-logout');
// Колонка бесед: сама колонка, кнопка ☰ и подложка (js/vk/mobile.js).
export const $vkContacts = document.getElementById('vk-contacts');
export const $vkContactsToggle = document.getElementById('vk-contacts-toggle');
export const $vkContactsScrim = document.getElementById('vk-contacts-scrim');
// Четыре СЕРВЕРНЫХ фильтра (аналог папок Telegram, но переключение = запрос).
export const $vkDirList = document.getElementById('vk-dir-list');
export const $vkDirAllCount = document.getElementById('vk-dir-all-count');
export const $vkDirUnreadCount = document.getElementById('vk-dir-unread-count');
export const $vkNodesTitle = document.getElementById('vk-nodes-title');
export const $vkDialogList = document.getElementById('vk-dialog-list');
// Сообщества (peer_id < 0) — аналог секции CHANNELS у Telegram.
export const $vkCommunityList = document.getElementById('vk-community-list');
export const $vkSessionHead = document.getElementById('vk-session-head');
export const $vkLog = document.getElementById('vk-log');
export const $vkTyping = document.getElementById('vk-typing');
export const $vkPrompt = document.getElementById('vk-prompt');
export const $vkInput = document.getElementById('vk-input');
export const $vkAttach = document.getElementById('vk-attach');
export const $vkFileInput = document.getElementById('vk-file-input');
export const $vkSend = document.getElementById('vk-send');
export const $vkMode = document.getElementById('vk-mode');
// Правая ячейка статусбара: счётчик собеседников в сети (js/vk/dialogs.js).
export const $vkStatusRight = document.getElementById('vk-status-right');
export const $vkLat = document.getElementById('vk-lat');
// Оверлей логина: один шаг (вставка токена), без wizard'а.
export const $vkLogin = document.getElementById('vk-login');
export const $vkLoginStepToken = document.getElementById('vk-login-step-token');
// Текст подсказки приходит с бэкенда: правила выдачи scope `messages` у VK
// меняются, и хардкод в разметке устарел бы между релизами.
export const $vkLoginHint = document.getElementById('vk-login-hint');
export const $vkToken = document.getElementById('vk-token');
export const $vkLoginTokenSubmit = document.getElementById('vk-login-token-submit');
export const $vkLoginStatus = document.getElementById('vk-login-status');
export const $vkLoginError = document.getElementById('vk-login-error');
export const $vkLoginCountdown = document.getElementById('vk-login-countdown');
// CAPTCHA (VK код 14) — ОТДЕЛЬНЫЙ оверлей, а не шаг логина: код прилетает на
// любой метод, в том числе посреди уже идущей переписки.
export const $vkCaptcha = document.getElementById('vk-captcha');
export const $vkCaptchaImg = document.getElementById('vk-captcha-img');
export const $vkCaptchaKey = document.getElementById('vk-captcha-key');
export const $vkCaptchaSubmit = document.getElementById('vk-captcha-submit');
export const $vkCaptchaCancel = document.getElementById('vk-captcha-cancel');
export const $vkCaptchaError = document.getElementById('vk-captcha-error');
export const $vkDropzone = document.getElementById('vk-dropzone');
export const $vkLightbox = document.getElementById('vk-lightbox');
export const $vkLightboxImg = document.getElementById('vk-lightbox-img');
// Стрелки галереи — новые против Telegram: у сообщения VK до 10 вложений.
export const $vkLightboxPrev = document.getElementById('vk-lightbox-prev');
export const $vkLightboxNext = document.getElementById('vk-lightbox-next');
export const $vkLightboxClose = document.getElementById('vk-lightbox-close');
// Подпись под полноразмерным вложением: `имя · 1280×720 · 1.2 MB`.
export const $vkLightboxCaption = document.getElementById('vk-lightbox-caption');
