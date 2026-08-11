Фронтовая часть фичи «Анализ чата через Claude» для панели Битрикс24 (br forge-caport-b24-7epr.12/.14/.15/.16).

ЧТО ЭТО. Порт уже готовой панели анализа (эталоны: js/telegram/analysis.js и js/vk/analysis.js). Всё поведение живёт в общем ядре js/messenger/analysis-core.js; модуль панели — тонкая обёртка над фабрикой createAnalysisPanel.

ФАЙЛЫ.
- tmux-web/static/index.html — кнопка #b24-analysis-toggle (✦) в титлбаре рядом с #b24-theme-toggle и статичный блок #b24-analysis рядом с #b24-dropzone/#b24-lightbox (зеркало #tg-analysis, префикс классов b24-).
- tmux-web/static/js/core/dom.js — блок $b24Analysis* (28 ref'ов + $b24AnalysisToggle) рядом с остальными $b24*.
- tmux-web/static/css/bitrix.css — секция «попап Анализ чата» + отрисованный Markdown (.md-*) внутри .b24-analysis-out/.b24-analysis-thread; добавлены .b24-login-btn-ghost и .b24-login-btn:disabled. Все цвета — только токены --b-* (проверяется bitrix_theme_contrast.test.mjs), z-index 6 (между #b24-login=5 и dropzone/lightbox=7).
- tmux-web/static/js/bitrix/api.js — группа вызовов анализа: getChatPrompt, putChatPrompt, getSavedAnalysis, clearSavedAnalysis, clearThread, startAnalysis, cancelAnalysis; пути в PATHS (dialogPrompt, dialogAnalysis, dialogAnalysisCancel, dialogAnalysisThread) — шаблоны с :id, подставляет withSeg.
- tmux-web/static/js/bitrix/analysis.js — обёртка над ядром: prefix 'b24', isBlockingOverlayOpen -> false (captcha у портала нет), dialogLabel через findDialog+displayName, activeDialogId из b24State, errorTexts (not_authorized/not_connected/flood_wait/scope_missing).
- tmux-web/static/js/bitrix/main.js — 4 обработчика кадров в buildHandlers, initAnalysis ПЕРВЫМ в initB24 (порядок Esc), клик по ✦, teardownAnalysis в teardownB24, analysis в _debugState.
- tmux-web/static/js/bitrix/ws.js — SERVER_FRAMES: analysis_started/delta/done/error вставлены перед resync/ping.
- tmux-web/static/js/bitrix/keynav.js — panelVisible() возвращает false при открытом #b24-analysis, иначе vim-навигация крала бы j/k у поля вопроса.

ГЛУБИНА ВЫГРУЗКИ У ПОРТАЛА СВОЯ: DEPTH_CHOICES = [50, 100, 200], DEFAULT_DEPTH = 100 (у VK и Telegram [50,200,500]/200). Причина платформенная: im.dialog.messages.get отдаёт максимум 50 сообщений за вызов при бюджете 2 rps, поэтому 500 сообщений = 10 запросов и 5 секунд ожидания перед первым словом модели. Бэкенд клампит по тому же списку.

РАСШИРЕНИЕ ЯДРА (назад-совместимое): createAnalysisPanel принимает необязательные depthChoices/defaultDepth; добавлен экспорт pickDepthFrom(choices, value); pickDepth(value) оставлен одноаргументным (на нём висят тесты VK/Telegram) и реализован через pickDepthFrom(DEPTH_CHOICES, value). Внутри фабрики все обращения к глубине идут через локальные DEPTHS/BASE_DEPTH/pickOwnDepth. VK и Telegram не изменены ни на строку.

ХРАНИЛИЩЕ: ключи localStorage b24.analysis.depth и b24.analysis.collapsed (строятся depthStorageKey/collapseStorageKey из префикса — общий с соседями ключ менял бы глубину сразу во всех панелях).

ОГРАНИЧЕНИЯ И ИНВАРИАНТЫ: только textContent/createElement, никакого innerHTML; Markdown рисует js/messenger/markdown.js (внутри ядра); кадры WS не отфильтрованы сервером по чату — фильтрация по паре (dialog_id, run_id) живёт в ядре; селекторы CSS не задевают #tg-*/#vk-*.