Фаза 4 фичи «Анализ чата через Claude» (панель VK): фронт — разметка, попап, вкладка контекста.

ЧТО ДОБАВЛЕНО.
1. tmux-web/static/index.html — статичный блок #vk-analysis между #vk-captcha и #vk-dropzone (23 узла: шапка с именем беседы, две вкладки, панель запуска с селектом глубины 50/200/500 и кнопками ANALYZE/STOP, вывод, тред Q&A, форма вопроса, строка ошибки; панель контекста с textarea заметки, счётчиком, SAVE/CLEAR и строкой состояния). Кнопка входа #vk-analysis-toggle (✦) — в титлбаре перед #vk-theme-toggle.
2. tmux-web/static/js/core/dom.js — блок ref'ов $vkAnalysis* сразу после captcha-блока, по одному на каждый id.
3. tmux-web/static/js/vk/api.js — getChatPrompt / putChatPrompt / startAnalysis / cancelAnalysis через callC + jsonInit + seg(); четыре строки путей дописаны в таблицу шапки дословно по plugins/vk/src/routes/mod.rs (GET и PUT /api/vk/dialogs/:id/prompt, POST /api/vk/dialogs/:id/analysis и .../analysis/cancel).
4. tmux-web/static/js/vk/analysis.js — новый модуль (см. его собственную запись документации).
5. tmux-web/static/js/vk/main.js — initAnalysis({onNotice}) ДО vkMedia.initMedia() (порядок регистрации Esc-слушателей: попап обязан забирать Esc раньше лайтбокса), клик на $vkAnalysisToggle, teardownAnalysis() в teardownVk(), поле analysis в _debugState().
6. tmux-web/static/js/vk/keynav.js — panelVisible() учитывает #vk-analysis. P0 внутри фазы: без этого j/k листали бы список бесед вместо набора текста в textarea (следствие читает static/hotkeys.js).
7. tmux-web/static/css/vk.css — блок стилей попапа в конце файла.
8. Тесты: новый tmux-web/tests/frontend/vk_analysis_prompt.test.mjs, расширены vk_dom_stub.mjs (узлы попапа, select/textarea) и vk_keynav.test.mjs (гейт клавиатуры при открытом попапе).

ПОРЯДОК СЛОЁВ (z-index) — не косметика: 5 логин < 6 анализ < 7 captcha/лайтбокс/dropzone. Код 14 у VK прилетает на ЛЮБОЙ метод, в том числе на выгрузку истории внутри запуска анализа, поэтому captcha обязана перекрывать попап и оставаться кликабельной; опустить попап ниже логина тоже нельзя — он оказался бы под невидимым оверлеем входа.

ЧЕГО В ЭТОЙ ФАЗЕ НЕТ: стриминга. Кнопка ANALYZE пока не рисует текст — это фаза 5 (кадры analysis_* в ws.js, склейка дельт, тред Q&A, отмена).

ПРИЁМКА. Все node-тесты tmux-web/tests/frontend/vk_*.test.mjs зелёные; живая проверка в Chromium (Playwright, мок статуса и роута заметки): попап открывается, вкладки переключаются, заметка читается один раз на беседу, сохраняется и перечитывается, буквы j/k печатаются в поле, Esc закрывает окно, исключений в консоли нет.