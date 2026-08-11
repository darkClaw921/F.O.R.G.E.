# $vkAnalysisOutToggle

js/core/dom.js: ref на кнопку-шапку #vk-analysis-out-toggle попапа анализа (доработка АЧ-И2).

Настоящая <button> с aria-expanded и aria-controls='vk-analysis-out', внутри каретка <span class='vk-analysis-out-caret'>▾/▸</span> и подпись РАЗБОР. Клик сворачивает РОВНО один узел — #vk-analysis-out; строка «разобрано до …» (#vk-analysis-updated), статус (#vk-analysis-status) и тред (#vk-analysis-thread) лежат снаружи сворачиваемого блока и остаются на экране: ради них разбор и сворачивают.

Кнопка, а не div с onclick, — чтобы фокус и Enter/Space работали без единой строки кода. Логика в js/vk/analysis.js (applyCollapsed/setOutCollapsed/toggleOutCollapsed), состояние в localStorage под ключом vk.analysis.collapsed, глобально на все беседы. Стили .vk-analysis-out-toggle / -caret / -toggle-off — в css/vk.css, только токены --v-*.

Как и все ref'ы блока $vk*, резолвится ОДИН раз при импорте core/dom.js: опечатка в id дала бы null навсегда и молча неработающую кнопку. Пара «id в index.html ↔ ref в dom.js» сторожится tests/frontend/vk_contract.test.mjs.
