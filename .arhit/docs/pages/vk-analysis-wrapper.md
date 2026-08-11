js/vk/analysis.js после этапа 1b — тонкая обёртка над messenger/analysis-core.js.

БЫЛО 1820 строк поведения, СТАЛО 313 строк проводки. Публичные экспорты не изменились ни одним именем, поэтому js/vk/main.js, keynav и тесты панели не правились вовсе.

ЧТО ОСТАЛОСЬ В ОБЁРТКЕ (и почему именно это):
1. Ref'ы #vk-analysis-* из core/dom.js — контракт «ref в dom.js ⇔ id в index.html» стережёт vk_contract.test.mjs.
2. Транспорт: объект api, где перед КАЖДЫМ вызовом vkApi стоит await vkLimiter.acquire(PRIORITY_USER). Клиентские 3 rps — VK-специфика (у Telegram лимитера нет вовсе), и порядок «сначала слот очереди, потом запрос» ровно тот же, что был до выноса.
3. isBlockingOverlayOpen: isCaptchaOpen — оверлей captcha живёт выше попапа (z-index 7 против 6) и забирает Esc.
4. dialogLabel: имя беседы из vkState.dialogs через displayName; activeDialogId: vkState.activeDialogId.
5. VK_ERROR_TEXTS — три подписи, в которых стоит имя мессенджера (not_authorized, not_connected, rate_limited). Остальные коды берутся из общей таблицы ядра. analysisErrorText(err) остаётся экспортом модуля и просто подставляет эту карту.

ПЕРЕЭКСПОРТЫ. Константы и чистые помощники (DEFAULT_DEPTH, FALLBACK_MAX_CHARS, DEPTH_CHOICES, MAX_QUESTION_CHARS, MAX_HISTORY_TURNS, MAX_HISTORY_CHARS, MAX_THREAD_TURNS, BOTTOM_EPS_PX, pickDepth, charCount, clampThread, clampThreadForView, clampPrompt, updatedLabel) идут из ядра через export ... from. DEPTH_STORAGE_KEY и COLLAPSE_STORAGE_KEY собираются из префикса 'vk' и дают прежние строки vk.analysis.depth и vk.analysis.collapsed — запомненные значения пользователей переживают обновление.

ПРИЁМКА ЭТАПА. Весь фронт-набор (74 файла, в том числе vk_analysis 181, vk_analysis_prompt 52, vk_contract 35, messenger_markdown 251) зелёный БЕЗ единой правки тестов; playwright vk-ui.spec.ts 14/14; живая приёмка на изолированном инстансе (свой HOME, порт 17441, мок api.vk.com и мок claude CLI) — 38 проверок: стрим сырым текстом, таблица и ссылка после done, вопрос не трогает разбор, REFRESH заменяет разбор целиком, сворачивание переживает перезагрузку, обе очистки независимы, отмена оставляет частичный ответ сырым, заметка сохраняется и читается лениво.