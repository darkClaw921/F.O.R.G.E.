# tmux-web/static/js/telegram/analysis.js

Тонкая обёртка попапа «Анализ чата» для панели Telegram (#tg-analysis).

ЭТО ОБЁРТКА, А НЕ РЕАЛИЗАЦИЯ. Всё поведение живёт в общем ядре js/messenger/analysis-core.js (фабрика createAnalysisPanel): вкладки, ленивый GET заметки и сохранённого разбора, тред Q&A, сворачивание, rAF-буфер дельт, фильтрация кадров по паре (dialog_id, run_id), усыновление run_id из analysis_started, перерисовка Markdown по analysis_done, разбор ошибок по err.code. Панели VK и Bitrix24 берут ровно то же ядро.

ЧТО ЗДЕСЬ СВОЕГО (и почему это не уехало в ядро):
1. Ref'ы — резолв 28 узлов #tg-analysis-* из core/dom.js.
2. Транспорт — прямые вызовы js/telegram/api.js. В ОТЛИЧИЕ ОТ VK здесь НЕТ ни callC, ни клиентского лимитера 3 rps: у Telegram их не существует, и тащить чужую механику в ядро было бы неверно.
3. isBlockingOverlayOpen: () => false — у Telegram нет captcha, то есть нет оверлея, который мог бы всплыть выше попапа и забрать Esc.
4. Имя диалога для шапки и активный диалог — из tgState.
5. Три подписи ошибок с именем мессенджера: not_authorized, not_connected, flood_wait. ВАЖНО: код ограничения скорости у Telegram — flood_wait, а у VK — rate_limited; остальные коды (claude_unavailable, invalid_state, storage_failed, invalid_input) общие и берутся из ядра.

prefix: 'tg' — из него ядро строит ключи localStorage через точки: tg.analysis.depth и tg.analysis.collapsed. С vk.* и b24.* они не пересекаются, поэтому глубина и сворачивание у трёх панелей независимы.

СОСТАВ ЭКСПОРТОВ совпадает с js/vk/analysis.js один в один (единое соглашение между мессенджерами): initAnalysis, teardownAnalysis, openAnalysis, closeAnalysis, isAnalysisOpen, setAnalysisTab, ensurePrompt, savePrompt, ensureSavedAnalysis, clearAnalysis, clearThread, setOutCollapsed, toggleOutCollapsed, startAnalysis, refreshAnalysis, askQuestion, cancelRun, onAnalysisStarted/Delta/Done/Error, _debugAnalysisState, плюс DEPTH_STORAGE_KEY, COLLAPSE_STORAGE_KEY, analysisErrorText и ре-экспорт констант ядра.

СВЯЗИ: js/telegram/main.js зовёт initAnalysis/teardownAnalysis и раскладывает 4 WS-кадра в onAnalysis*; js/telegram/keynav.js::panelVisible() учитывает #tg-analysis (P0: без гейта j/k крадут ввод у полей попапа и вопрос физически не набрать).
