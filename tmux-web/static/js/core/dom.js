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

// ---- Аналитика Claude Code (полноэкранный вью, Фаза 5) ----
//
// Селекторы вью #analytics — соседа #daily-summary внутри #main. Видимость
// переключает js/analytics/analytics.js::showAnalytics той же механикой, что
// у «Сводки дня»: вью занимает #main целиком, скрывая #home и placeholder.
//
// Все ref'ы вычисляются один раз при импорте модуля; узла нет в DOM —
// переменная null, и весь вью просто не открывается (проверка в showAnalytics).
export const $analytics = document.getElementById('analytics');
export const $analyticsBack = document.getElementById('analytics-back');
export const $analyticsRange = document.getElementById('analytics-range');
export const $analyticsProject = document.getElementById('analytics-project');
export const $analyticsRescan = document.getElementById('analytics-rescan');
export const $analyticsMeta = document.getElementById('analytics-meta');
export const $analyticsProgress = document.getElementById('analytics-progress');
export const $analyticsError = document.getElementById('analytics-error');
export const $analyticsKpi = document.getElementById('analytics-kpi');
export const $analyticsGrid = document.getElementById('analytics-grid');

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
// АНАЛИЗ ЧАТА ЧЕРЕЗ CLAUDE (js/telegram/analysis.js).
//
// Ref'ы объявлены здесь, а не резолвятся в самом модуле: разметка попапа
// статична (index.html, блок #tg-analysis), и querySelector внутри модуля
// был бы единственным исключением из конвенции панели. Имена зеркалят
// VK-шные ($vkAnalysis*) — панель одна и та же, ядро тоже общее
// (js/messenger/analysis-core.js).
export const $tgAnalysisToggle = document.getElementById('tg-analysis-toggle');
export const $tgAnalysis = document.getElementById('tg-analysis');
export const $tgAnalysisTarget = document.getElementById('tg-analysis-target');
export const $tgAnalysisClose = document.getElementById('tg-analysis-close');
export const $tgAnalysisTabRun = document.getElementById('tg-analysis-tab-run');
export const $tgAnalysisTabCtx = document.getElementById('tg-analysis-tab-ctx');
export const $tgAnalysisPaneRun = document.getElementById('tg-analysis-pane-run');
export const $tgAnalysisPaneCtx = document.getElementById('tg-analysis-pane-ctx');
export const $tgAnalysisDepth = document.getElementById('tg-analysis-depth');
export const $tgAnalysisStart = document.getElementById('tg-analysis-start');
export const $tgAnalysisRefresh = document.getElementById('tg-analysis-refresh');
export const $tgAnalysisCancel = document.getElementById('tg-analysis-cancel');
export const $tgAnalysisClear = document.getElementById('tg-analysis-clear');
export const $tgAnalysisClearThread = document.getElementById('tg-analysis-clear-thread');
export const $tgAnalysisStatus = document.getElementById('tg-analysis-status');
// «разобрано до <дата>, N сообщений» — метка СОХРАНЁННОГО разбора, а не
// текущего run'а: она переживает перезагрузку и объясняет, от чего REFRESH
// будет считать «новые сообщения».
export const $tgAnalysisUpdated = document.getElementById('tg-analysis-updated');
// Кнопка-шапка разбора: сворачивает ТОЛЬКО #tg-analysis-out. Метка, статус и
// тред остаются на экране — они лежат снаружи сворачиваемого узла.
export const $tgAnalysisOutToggle = document.getElementById('tg-analysis-out-toggle');
export const $tgAnalysisOut = document.getElementById('tg-analysis-out');
export const $tgAnalysisThread = document.getElementById('tg-analysis-thread');
export const $tgAnalysisAsk = document.getElementById('tg-analysis-ask');
export const $tgAnalysisQuestion = document.getElementById('tg-analysis-question');
export const $tgAnalysisSend = document.getElementById('tg-analysis-send');
export const $tgAnalysisError = document.getElementById('tg-analysis-error');
// Вкладка «Контекст чата»: пер-чатовая заметка пользователя.
export const $tgAnalysisPrompt = document.getElementById('tg-analysis-prompt');
export const $tgAnalysisPromptCount = document.getElementById('tg-analysis-prompt-count');
export const $tgAnalysisPromptSave = document.getElementById('tg-analysis-prompt-save');
export const $tgAnalysisPromptClear = document.getElementById('tg-analysis-prompt-clear');
export const $tgAnalysisPromptStatus = document.getElementById('tg-analysis-prompt-status');
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
// АНАЛИЗ ЧАТА ЧЕРЕЗ CLAUDE (js/vk/analysis.js).
//
// Ref'ы объявлены здесь, а не резолвятся в самом модуле: разметка попапа
// статична (index.html, блок #vk-analysis), и querySelector внутри модуля
// был бы единственным исключением из конвенции панели. Контракт «ref в
// dom.js ⇔ id в index.html» проверяется тестом vk_contract.test.mjs.
export const $vkAnalysisToggle = document.getElementById('vk-analysis-toggle');
export const $vkAnalysis = document.getElementById('vk-analysis');
export const $vkAnalysisTarget = document.getElementById('vk-analysis-target');
export const $vkAnalysisClose = document.getElementById('vk-analysis-close');
export const $vkAnalysisTabRun = document.getElementById('vk-analysis-tab-run');
export const $vkAnalysisTabCtx = document.getElementById('vk-analysis-tab-ctx');
export const $vkAnalysisPaneRun = document.getElementById('vk-analysis-pane-run');
export const $vkAnalysisPaneCtx = document.getElementById('vk-analysis-pane-ctx');
export const $vkAnalysisDepth = document.getElementById('vk-analysis-depth');
export const $vkAnalysisStart = document.getElementById('vk-analysis-start');
export const $vkAnalysisRefresh = document.getElementById('vk-analysis-refresh');
export const $vkAnalysisCancel = document.getElementById('vk-analysis-cancel');
export const $vkAnalysisClear = document.getElementById('vk-analysis-clear');
export const $vkAnalysisClearThread = document.getElementById('vk-analysis-clear-thread');
export const $vkAnalysisStatus = document.getElementById('vk-analysis-status');
// «разобрано до <дата>, N сообщений» — метка СОХРАНЁННОГО разбора, а не
// текущего run'а: она переживает перезагрузку и объясняет, от чего REFRESH
// будет считать «новые сообщения».
export const $vkAnalysisUpdated = document.getElementById('vk-analysis-updated');
// Кнопка-шапка разбора (АЧ-И2): сворачивает ТОЛЬКО #vk-analysis-out. Метка,
// статус и тред остаются на экране — они лежат снаружи сворачиваемого узла.
export const $vkAnalysisOutToggle = document.getElementById('vk-analysis-out-toggle');
export const $vkAnalysisOut = document.getElementById('vk-analysis-out');
export const $vkAnalysisThread = document.getElementById('vk-analysis-thread');
export const $vkAnalysisAsk = document.getElementById('vk-analysis-ask');
export const $vkAnalysisQuestion = document.getElementById('vk-analysis-question');
export const $vkAnalysisSend = document.getElementById('vk-analysis-send');
export const $vkAnalysisError = document.getElementById('vk-analysis-error');
// Вкладка «Контекст чата»: пер-чатовая заметка пользователя.
export const $vkAnalysisPrompt = document.getElementById('vk-analysis-prompt');
export const $vkAnalysisPromptCount = document.getElementById('vk-analysis-prompt-count');
export const $vkAnalysisPromptSave = document.getElementById('vk-analysis-prompt-save');
export const $vkAnalysisPromptClear = document.getElementById('vk-analysis-prompt-clear');
export const $vkAnalysisPromptStatus = document.getElementById('vk-analysis-prompt-status');
export const $vkDropzone = document.getElementById('vk-dropzone');
export const $vkLightbox = document.getElementById('vk-lightbox');
export const $vkLightboxImg = document.getElementById('vk-lightbox-img');
// Стрелки галереи — новые против Telegram: у сообщения VK до 10 вложений.
export const $vkLightboxPrev = document.getElementById('vk-lightbox-prev');
export const $vkLightboxNext = document.getElementById('vk-lightbox-next');
export const $vkLightboxClose = document.getElementById('vk-lightbox-close');
// Подпись под полноразмерным вложением: `имя · 1280×720 · 1.2 MB`.
export const $vkLightboxCaption = document.getElementById('vk-lightbox-caption');

// ---- Bitrix24 plugin ----
//
// Ref'ы вкладки «Битрикс24» (плагин forge-bitrix, opt-in `--bitrix`).
// Устройство ровно то же, что у телеграмной и VK-секций выше: значения
// вычисляются ОДИН РАЗ при импорте модуля, поэтому узлы обязаны быть в
// разметке к этому моменту (панель #bitrix присутствует в index.html всегда,
// даже при выключенном плагине — скрыт только `hidden`).
//
// СЛЕДСТВИЕ ДЛЯ ТЕСТОВ: стаб `document` обязан стоять ДО первого импорта
// любого модуля js/bitrix/*, иначе все ref'ы окажутся null и тест будет
// проверять пустоту. Код панели всё равно проверяет truthy у каждого узла:
// index.html может приехать из старого кэша PWA, где секции #bitrix ещё нет.
//
// Имена: id кнопки — `bitrix-toggle` (по имени плагина), всё внутри панели —
// с префиксом `b24-`. Разнесение имени модуля и DOM-префикса не новое:
// telegram/`tg-` устроен так же.
export const $bitrixToggle = document.getElementById('bitrix-toggle');
export const $b24UnreadBadge = document.getElementById('b24-unread-badge');
export const $b24 = document.getElementById('bitrix');
export const $b24Titlebar = document.getElementById('b24-titlebar');
export const $b24SelfUser = document.getElementById('b24-self-user');
export const $b24ThemeToggle = document.getElementById('b24-theme-toggle');
export const $b24Logout = document.getElementById('b24-logout');
// СТРОКА ВКЛАДОК ПОРТАЛОВ (js/bitrix/portals.js, мультипортальность).
//
// Отдельный ряд ПОД титлбаром, видимый ВСЕГДА — включая мобильную раскладку,
// где колонка #b24-contacts уезжает под ☰. На узком экране вкладка портала —
// единственный способ понять, в чей чат уйдёт сообщение.
export const $b24Portals = document.getElementById('b24-portals');
export const $b24PortalTabs = document.getElementById('b24-portal-tabs');
export const $b24PortalAdd = document.getElementById('b24-portal-add');
export const $b24PortalMenu = document.getElementById('b24-portal-menu');
export const $b24PortalRename = document.getElementById('b24-portal-rename');
export const $b24PortalRemove = document.getElementById('b24-portal-remove');
// АНАЛИЗ ЧАТА ЧЕРЕЗ CLAUDE (js/bitrix/analysis.js).
//
// Ref'ы объявлены здесь, а не резолвятся в самом модуле: разметка попапа
// статична (index.html, блок #b24-analysis), и querySelector внутри модуля
// был бы единственным исключением из конвенции панели. Имена зеркалят
// Telegram-ские ($tgAnalysis*) и VK-шные ($vkAnalysis*) — панель одна и та же,
// ядро тоже общее (js/messenger/analysis-core.js), и разные имена в трёх
// портах одной фичи мешали бы читать три main.js рядом.
export const $b24AnalysisToggle = document.getElementById('b24-analysis-toggle');
export const $b24Analysis = document.getElementById('b24-analysis');
export const $b24AnalysisTarget = document.getElementById('b24-analysis-target');
export const $b24AnalysisClose = document.getElementById('b24-analysis-close');
export const $b24AnalysisTabRun = document.getElementById('b24-analysis-tab-run');
export const $b24AnalysisTabCtx = document.getElementById('b24-analysis-tab-ctx');
export const $b24AnalysisPaneRun = document.getElementById('b24-analysis-pane-run');
export const $b24AnalysisPaneCtx = document.getElementById('b24-analysis-pane-ctx');
// Глубина выгрузки у Битрикс24 СВОЯ (50 / 100 / 200, по умолчанию 100):
// портал отдаёт историю страницами по 50 сообщений при бюджете 2 rps, и
// привычные 500 означали бы десять запросов перед первым словом модели.
export const $b24AnalysisDepth = document.getElementById('b24-analysis-depth');
export const $b24AnalysisStart = document.getElementById('b24-analysis-start');
export const $b24AnalysisRefresh = document.getElementById('b24-analysis-refresh');
export const $b24AnalysisCancel = document.getElementById('b24-analysis-cancel');
export const $b24AnalysisClear = document.getElementById('b24-analysis-clear');
export const $b24AnalysisClearThread = document.getElementById('b24-analysis-clear-thread');
export const $b24AnalysisStatus = document.getElementById('b24-analysis-status');
// «разобрано до <дата>, N сообщений» — метка СОХРАНЁННОГО разбора, а не
// текущего run'а: она переживает перезагрузку и объясняет, от чего REFRESH
// будет считать «новые сообщения».
export const $b24AnalysisUpdated = document.getElementById('b24-analysis-updated');
// Кнопка-шапка разбора: сворачивает ТОЛЬКО #b24-analysis-out. Метка, статус и
// тред остаются на экране — они лежат снаружи сворачиваемого узла.
export const $b24AnalysisOutToggle = document.getElementById('b24-analysis-out-toggle');
export const $b24AnalysisOut = document.getElementById('b24-analysis-out');
export const $b24AnalysisThread = document.getElementById('b24-analysis-thread');
export const $b24AnalysisAsk = document.getElementById('b24-analysis-ask');
export const $b24AnalysisQuestion = document.getElementById('b24-analysis-question');
export const $b24AnalysisSend = document.getElementById('b24-analysis-send');
export const $b24AnalysisError = document.getElementById('b24-analysis-error');
// Вкладка «Контекст чата»: пер-чатовая заметка пользователя.
export const $b24AnalysisPrompt = document.getElementById('b24-analysis-prompt');
export const $b24AnalysisPromptCount = document.getElementById('b24-analysis-prompt-count');
export const $b24AnalysisPromptSave = document.getElementById('b24-analysis-prompt-save');
export const $b24AnalysisPromptClear = document.getElementById('b24-analysis-prompt-clear');
export const $b24AnalysisPromptStatus = document.getElementById('b24-analysis-prompt-status');
// Колонка диалогов: сама колонка, кнопка ☰ и подложка (js/bitrix/mobile.js).
export const $b24Contacts = document.getElementById('b24-contacts');
export const $b24ContactsToggle = document.getElementById('b24-contacts-toggle');
export const $b24ContactsScrim = document.getElementById('b24-contacts-scrim');
// Четыре ЛОКАЛЬНЫЕ директории одного списка (Р2). В отличие от VK это не
// серверные фильтры: портал отдаёт один `im.recent.get`, а разделение
// чаты/задачи/непрочитанные делается по полям строки.
export const $b24DirList = document.getElementById('b24-dir-list');
export const $b24DirAllCount = document.getElementById('b24-dir-all-count');
export const $b24DirChatsCount = document.getElementById('b24-dir-chats-count');
export const $b24DirTasksCount = document.getElementById('b24-dir-tasks-count');
export const $b24DirUnreadCount = document.getElementById('b24-dir-unread-count');
export const $b24NodesTitle = document.getElementById('b24-nodes-title');
export const $b24DialogList = document.getElementById('b24-dialog-list');
export const $b24SessionHead = document.getElementById('b24-session-head');
// Шапка-карточка задачи над лентой — то, чего нет ни у Telegram, ни у VK:
// чат задачи это обычный IM-чат с entity_type="TASKS_TASK", и карточка
// показывает статус, срок и матрицу действий (Ф10).
export const $b24TaskCard = document.getElementById('b24-task-card');
export const $b24Log = document.getElementById('b24-log');
export const $b24Typing = document.getElementById('b24-typing');
// Очередь отправляемых файлов (Ф9). НАД полем ввода, а не в ленте:
// `chat.renderHistory()` пересобирает `#b24-log` целиком, и карточка отправки
// исчезала бы вместе с прогрессом от любого приехавшего сообщения.
export const $b24Uploads = document.getElementById('b24-uploads');
// Строка режима ввода (Ф6): цитата ответа либо правка. Режимы
// взаимоисключающие, поэтому узел один.
export const $b24PromptNote = document.getElementById('b24-prompt-note');
export const $b24Prompt = document.getElementById('b24-prompt');
export const $b24Input = document.getElementById('b24-input');
// Счётчик символов (Р12): портал режет текст на 20 006 символах МОЛЧА, поэтому
// предупреждать обязан фронт — до отправки.
export const $b24CharCount = document.getElementById('b24-charcount');
export const $b24Attach = document.getElementById('b24-attach');
export const $b24FileInput = document.getElementById('b24-file-input');
export const $b24Send = document.getElementById('b24-send');
export const $b24Mode = document.getElementById('b24-mode');
export const $b24StatusRight = document.getElementById('b24-status-right');
export const $b24Lat = document.getElementById('b24-lat');
// Оверлей подключения: один шаг — вставка URL входящего вебхука. Разбор
// трёх форм ввода живёт ТОЛЬКО на бэкенде (Ф3).
export const $b24Login = document.getElementById('b24-login');
export const $b24LoginStepWebhook = document.getElementById('b24-login-step-webhook');
// Заголовок и CANCEL нужны РЕЖИМУ ДОБАВЛЕНИЯ портала: экран подключения
// переиспользуется кнопкой `+` строки вкладок (js/bitrix/portals.js).
export const $b24LoginTitle = document.getElementById('b24-login-title');
export const $b24LoginCancel = document.getElementById('b24-login-cancel');
// Текст подсказки приходит с бэкенда (`hint` в /api/bitrix/status):
// интерфейс портала меняется, а разметку браузер держит в кэше PWA неделями.
export const $b24LoginHint = document.getElementById('b24-login-hint');
export const $b24Webhook = document.getElementById('b24-webhook');
export const $b24LoginWebhookSubmit = document.getElementById('b24-login-webhook-submit');
export const $b24LoginStatus = document.getElementById('b24-login-status');
export const $b24LoginError = document.getElementById('b24-login-error');
export const $b24LoginCountdown = document.getElementById('b24-login-countdown');
// Предупреждение про выход (`logout_hint` в /api/bitrix/status): отозвать
// входящий вебхук через REST невозможно, и на портале он остаётся
// действующим — иначе «вышел» будет прочитано как «отозвал».
export const $b24LogoutNote = document.getElementById('b24-logout-note');
export const $b24Dropzone = document.getElementById('b24-dropzone');
export const $b24Lightbox = document.getElementById('b24-lightbox');
export const $b24LightboxImg = document.getElementById('b24-lightbox-img');
export const $b24LightboxPrev = document.getElementById('b24-lightbox-prev');
export const $b24LightboxNext = document.getElementById('b24-lightbox-next');
export const $b24LightboxClose = document.getElementById('b24-lightbox-close');
export const $b24LightboxCaption = document.getElementById('b24-lightbox-caption');

// ----------------------------------------------------------------------------
// Ref'ы вкладки «Мои задачи» (плагин forge-tracker, opt-in `--tracker`).
//
// Разметка #tracker присутствует в index.html ВСЕГДА, поэтому ref'ы резолвятся
// один раз при импорте этого модуля — так же, как у Telegram, VK и Битрикса.
// Каждый ref обязан проверяться на null у места использования: index.html
// может приехать из старого кэша PWA, где секции #tracker ещё нет.
//
// Имена: id кнопки — `tracker-toggle` (по имени плагина), всё внутри панели —
// с префиксом `trk-`. Разнесение имени модуля и DOM-префикса не новое: у
// Битрикса `bitrix-toggle` + `b24-*`.
export const $trackerToggle = document.getElementById('tracker-toggle');
export const $trkRunningBadge = document.getElementById('trk-running-badge');
export const $tracker = document.getElementById('tracker');
export const $trkTitlebar = document.getElementById('trk-titlebar');
// «● N TIMERS · GLOBAL STORE» — состояние стора в титлбаре. GLOBAL не
// украшение: задачи трекера не привязаны к корню проекта, файл один
// (~/.config/forge/tracker.json).
export const $trkStore = document.getElementById('trk-store');
export const $trkToday = document.getElementById('trk-today');
export const $trkThemeToggle = document.getElementById('trk-theme-toggle');
export const $trkReload = document.getElementById('trk-reload');
// Левая колонка: четыре виртуальные директории (фильтры выборки, НЕ каталоги
// на диске и не серверные запросы), списки пользователя и облако тегов.
export const $trkDirs = document.getElementById('trk-dirs');
export const $trkListsTitle = document.getElementById('trk-lists-title');
export const $trkLists = document.getElementById('trk-lists');
export const $trkListAdd = document.getElementById('trk-list-add');
export const $trkTags = document.getElementById('trk-tags');
// Основная область: строка режимов, блок идущих таймеров (фаза 5) и лента.
export const $trkSessionLine = document.getElementById('trk-session-line');
export const $trkModes = document.getElementById('trk-modes');
export const $trkGanttNav = document.getElementById('trk-gantt-nav');
// Управление периодом сводки: [◀] [today] [▶], day/week/month и выгрузка CSV.
// Видно только в режиме report — как и #trk-gantt-nav в режиме gantt.
export const $trkReportNav = document.getElementById('trk-report-nav');
export const $trkRunning = document.getElementById('trk-running');
export const $trkContent = document.getElementById('trk-content');
// Нижняя строка ввода и превью разбора.
export const $trkPrompt = document.getElementById('trk-prompt');
export const $trkPromptInput = document.getElementById('trk-prompt-input');
export const $trkPromptPreview = document.getElementById('trk-prompt-preview');
export const $trkCreate = document.getElementById('trk-create');
// Статус-бар: индикатор TRACKING и строка сумм за день и неделю.
export const $trkConn = document.getElementById('trk-conn');
export const $trkStatusInfo = document.getElementById('trk-status-info');
// Карточка задачи (js/tracker/modals.js).
export const $trkModal = document.getElementById('trk-modal');
export const $trkModalNode = document.getElementById('trk-modal-node');
export const $trkModalBody = document.getElementById('trk-modal-body');
export const $trkModalError = document.getElementById('trk-modal-error');
export const $trkModalClose = document.getElementById('trk-modal-close');
export const $trkModalDelete = document.getElementById('trk-modal-delete');
export const $trkModalDone = document.getElementById('trk-modal-done');
export const $trkModalSave = document.getElementById('trk-modal-save');
export const $trkModalPush = document.getElementById('trk-modal-push');
// Диалог переноса задачи в TODO вкладки Tasks (js/tracker/to-todo.js).
// Отдельный слой ПОВЕРХ карточки: перенос вызывается и из ленты, и из доски, и
// из открытой карточки, а карточку при этом закрывать нельзя — человек может
// отменить перенос и вернуться к недописанной правке.
export const $trkPush = document.getElementById('trk-push');
export const $trkPushNode = document.getElementById('trk-push-node');
export const $trkPushRoots = document.getElementById('trk-push-roots');
export const $trkPushError = document.getElementById('trk-push-error');
export const $trkPushClose = document.getElementById('trk-push-close');
export const $trkPushCancel = document.getElementById('trk-push-cancel');
export const $trkPushOk = document.getElementById('trk-push-ok');
// Диалог вехи Гантта (js/tracker/milestone.js) — заменил два window.prompt:
// подпись, дата пикером и явная кнопка удаления.
export const $trkMs = document.getElementById('trk-ms');
export const $trkMsTitle = document.getElementById('trk-ms-title');
export const $trkMsDate = document.getElementById('trk-ms-date');
export const $trkMsHint = document.getElementById('trk-ms-hint');
export const $trkMsError = document.getElementById('trk-ms-error');
export const $trkMsClose = document.getElementById('trk-ms-close');
export const $trkMsCancel = document.getElementById('trk-ms-cancel');
export const $trkMsDelete = document.getElementById('trk-ms-delete');
export const $trkMsOk = document.getElementById('trk-ms-ok');
