# tmux-web/static/js/bitrix/login.js

Экран подключения панели «Битрикс24» — оверлей #b24-login (Ф3). Один шаг и одно поле: адрес входящего вебхука. Ни кода из SMS, ни пароля, ни captcha у портала нет вовсе.

АВТОМАТ ОТОБРАЖЕНИЯ (источник истины — поле state ответа сервера): not_configured (форма без ошибки) | checking (форма заблокирована, ошибки НЕТ) | invalid_url | invalid_credentials | scope_missing (перечисление недостающих прав im/task) | throttled (обратный отсчёт, автоповтора нет) | disconnected | authorized (оверлей скрыт, поле ОЧИЩЕНО).

ЭКСПОРТ: renderLoginState(dto) — идемпотентная отрисовка; initB24Login({onAuthorized,onLoggedOut}) — идемпотентная навеска слушателей; submitLogout(); setLoginHint/setLogoutHint (тексты приходят с бэкенда полями hint и logout_hint ответа /api/bitrix/status); startCountdown/stopCountdown/formatWait; resetLogin(); STATE_TEXT (запасные тексты состояний); _debugLogin() для e2e.

ТРИ ПРАВИЛА. 1) Разбор вставленного адреса живёт ТОЛЬКО на бэкенде — во фронте нет ни split, ни match, ни RegExp (проверяет bitrix_contract.test.mjs); второй парсер был бы вторым местом ошибиться с секретом. 2) Секрет не задерживается в DOM и не попадает в сторадж: значение #b24-webhook очищается немедленно при переходе в authorized. 3) Автоповтора нет: пока тикает обратный отсчёт throttled, ни один запрос не уходит.

ИДЕМПОТЕНТНОСТЬ обязательна: renderLoginState зовут из снапшота GET /api/bitrix/status, из WS-кадра login_state и из ответа каждого POST'а подключения. Повторный вызов не сбрасывает поле, не перезапускает отсчёт и не навешивает вторых слушателей.

Ошибки разбираются ПО КОДУ (invalid_input / flood_wait / scope_missing / not_authorized / not_connected), а не по тексту. На экране обязано быть ограничение Р15: вебхук выпускается от имени одного сотрудника портала, и в remote-режиме все, кто открыл devforge, увидят его переписку.

Связи: core/dom.js (ref'ы #b24-login*), js/bitrix/api.js (loginWebhook/logout), js/bitrix/main.js (applyLoginState делегирует сюда оверлей). Тесты: tmux-web/tests/frontend/bitrix_login.test.mjs (58 проверок на фейковом DOM). Серверная сторона — plugins/bitrix/src/{login,routes/login}.rs, страница bitrix-login.

ФАЗА 5. Переход в authorized запускает загрузку данных панели (main.js::loadPanelData), а падение из authorized — resetChat(): лента чужого портала не должна оставаться на экране.
