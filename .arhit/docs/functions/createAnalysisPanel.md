# createAnalysisPanel

Фабрика общего попапа «Анализ чата» — tmux-web/static/js/messenger/analysis-core.js (этап 1b переноса фичи в Telegram и Bitrix24).

НАЗНАЧЕНИЕ. Всё поведение попапа разбора переписки локальным claude CLI, вынесенное из js/vk/analysis.js, чтобы три панели-мессенджера не держали три копии ~1800 строк. Панель отдаёт свои узлы, свой транспорт и свои подписи ошибок; ядро не знает ни про один конкретный мессенджер.

СИГНАТУРА. createAnalysisPanel({prefix, refs, api, isBlockingOverlayOpen, onNotice, dialogLabel, activeDialogId, errorTexts}) -> {init, teardown, open, close, isOpen, debugState, onStarted, onDelta, onDone, onError, setTab, ensurePrompt, savePrompt, ensureSavedAnalysis, clearAnalysis, clearThread, setOutCollapsed, toggleOutCollapsed, startAnalysis, refreshAnalysis, askQuestion, cancelRun, errorText}.
- prefix ('vk' | 'tg' | 'b24') задаёт ключи localStorage (<prefix>.analysis.depth, <prefix>.analysis.collapsed) и имена классов разметки (<prefix>-analysis-qa, -qa-q, -qa-a, -tab-active, -out-toggle-off, -out-caret).
- refs: root, close, target, tabRun, tabCtx, paneRun, paneCtx, depth, start, refresh, cancel, clear, clearThread, status, updated, outToggle, out, thread, ask, question, send, error, prompt, promptCount, promptSave, promptClear, promptStatus. Все опциональны — отсутствующий узел просто не обслуживается.
- api: getPrompt, putPrompt, getSavedAnalysis, clearAnalysis, clearThread, start, cancel. Ядро внутри себя в сеть не ходит вовсе; лимитеры и конверт ошибок — забота панели.
- isBlockingOverlayOpen: пока оверлей панели открыт, Esc принадлежит ему (у VK это captcha; Telegram и Bitrix отдают () => false).
- dialogLabel/activeDialogId: имя беседы для шапки и активная беседа панели.
- errorTexts: подписи, в которых стоит имя мессенджера, поверх общей таблицы кодов.

СОСТОЯНИЕ — В ЗАМЫКАНИИ. Модульных let нет: два попапа на одной странице не видят беседы, открытости, сворачивания и run'а друг друга. Свой инстанс createFollow() на панель по той же причине.

ЧТО ВНУТРИ. Вкладки run/ctx; ленивый GET заметки и сохранённого разбора — ровно один на беседу (флаг ставится ДО запроса, при ошибке снимается); тред Q&A, приезжающий полем thread того же GET и дополняемый своими парами; сворачивание ТОЛЬКО блока разбора с памятью в localStorage; rAF-буфер дельт со сливом одним кадром и синхронным доливом на done/error; фильтрация кадров по паре (dialog_id, run_id); усыновление run_id из analysis_started, обгоняющего ответ POST'а; выбор цели дельт по ОТКРЫТОЙ строке треда, а не по kind; перерисовка Markdown один раз по analysis_done и только у целого ответа; разбор ошибок по err.code.

ИНВАРИАНТЫ, КОТОРЫЕ НЕЛЬЗЯ СЛОМАТЬ: clearOut() никогда не зовётся на пути вопроса; applyThread отделена от applySavedAnalysis (иначе CLEAR разбора стирал бы с экрана целый на сервере тред); в onDone для ask сырой ответ снимается ДО перерисовки Markdown; REFRESH стирает прошлый разбор перед ПЕРВОЙ дельтой, а не на старте; startRun не проверяет analysisBusy; cancelFlush помнит, откуда взялся id (rAF или timeout); только textContent/createElement.

ПОТРЕБИТЕЛИ. js/vk/analysis.js — тонкая обёртка с прежними публичными экспортами. Дальше по плану — js/telegram/analysis.js и js/bitrix/analysis.js.
ТЕСТЫ. tests/frontend/messenger_analysis_core.test.mjs (изоляция экземпляров, ключи по префиксу, подписи ошибок, teardown, мусорные кадры, инвариант по исходнику) + вся поведенческая база панели VK: vk_analysis.test.mjs (181), vk_analysis_prompt.test.mjs (52), e2e vk-ui.spec.ts.
