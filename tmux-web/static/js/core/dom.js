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
