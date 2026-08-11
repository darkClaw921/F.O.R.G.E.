Фронт-тесты панели «Анализ чата» плагина Telegram (задачи forge-caport-tg-j1x4.16 и .17).

СОСТАВ:
- tmux-web/tests/frontend/telegram_analysis.test.mjs — 199 ассертов (VK-образец 181).
- tmux-web/tests/frontend/telegram_analysis_prompt.test.mjs — 59 ассертов (образец 52).
- tmux-web/tests/frontend/telegram_contract.test.mjs — 60 ассертов, НОВЫЙ тип теста для этого плагина (см. отдельную страницу tg-analysis-contract-test).
- tmux-web/tests/frontend/telegram_dom_stub.mjs — дополнен, ничего не заменено.

ЧТО ПОКРЫТО: вкладки; ленивый GET заметки (ровно один на чат, перечитывание при смене чата) и сохранённого разбора; тред Q&A (history в следующем запросе, clampThread, восстановление из GET); сворачивание + localStorage; фильтрация ЧУЖИХ кадров по паре (dialog_id, run_id); усыновление run_id из analysis_started; rAF-буфер дельт и синхронный слив по done; все восемь кодов ошибок; независимость CLEAR ✕ и CLEAR Q&A. Сверх VK: пути /api/telegram/… в каждом запросе и проверка, что ключи tg.analysis.* не задевают vk.analysis.*.

ЯВНО НЕПРИМЕНИМЫ (помечено комментарием в шапке обоих тестов, а не выкинуто молча): captcha (у Telegram нет блокирующего оверлея, isBlockingOverlayOpen: () => false — вместо ветки «Esc принадлежит оверлею» проверено, что Esc всегда закрывает попап) и клиентский лимитер 3 rps (его нет ни на клиенте, ни в контракте).

ЛОВУШКА, ЗАФИКСИРОВАННАЯ ТЕСТОМ: код ограничения скорости у Telegram — flood_wait, у VK — rate_limited. Тест требует, чтобы подписи были РАЗНЫМИ: одинаковая подпись означала бы, что обёртка копипастнута из VK вместе с чужим кодом.

ПРАВКИ СТАБА, которые стоит помнить: removeEventListener был ЗАГЛУШКОЙ — проверка teardown'а на нём зеленела бы на пустоте; дефолтный fetch по-прежнему ПАДАЕТ (offline), чтобы тесты не начали молча получать успехи; DocumentFragment растворяется в appendChild.

ВАЖНО ПРИ ПРОГОНАХ: telegram_analysis и vk_analysis делят ядро js/messenger/analysis-core.js. Проба на регресс (временное снятие сверки dialog_id в frameIsOurs) красит ОБА теста — не пугаться, если параллельный прогон суite'а в этот момент показал падение vk_analysis 178/181.