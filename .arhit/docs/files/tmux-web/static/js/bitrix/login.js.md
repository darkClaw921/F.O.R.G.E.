# tmux-web/static/js/bitrix/login.js

Экран подключения панели «Битрикс24» — оверлей #b24-login (Ф3). Один шаг и одно поле: адрес входящего вебхука. Ни кода из SMS, ни пароля, ни captcha у портала нет вовсе.

АВТОМАТ ОТОБРАЖЕНИЯ (источник истины — поле state ответа сервера): not_configured (форма без ошибки) | checking (форма заблокирована, ошибки НЕТ) | invalid_url | invalid_credentials | scope_missing (перечисление недостающих прав im/task) | throttled (обратный отсчёт, автоповтора нет) | disconnected | authorized (оверлей скрыт, поле ОЧИЩЕНО).

ЭКСПОРТ: renderLoginState(dto) — идемпотентная отрисовка; initB24Login({onAuthorized,onLoggedOut}) — идемпотентная навеска слушателей; submitLogout(); setLoginHint/setLogoutHint (тексты приходят с бэкенда полями hint и logout_hint ответа /api/bitrix/status); startCountdown/stopCountdown/formatWait; resetLogin(); STATE_TEXT (запасные тексты состояний); _debugLogin() для e2e.

ТРИ ПРАВИЛА. 1) Разбор вставленного адреса живёт ТОЛЬКО на бэкенде — во фронте нет ни split, ни match, ни RegExp (проверяет bitrix_contract.test.mjs); второй парсер был бы вторым местом ошибиться с секретом. 2) Секрет не задерживается в DOM и не попадает в сторадж: значение #b24-webhook очищается немедленно при переходе в authorized. 3) Автоповтора нет: пока тикает обратный отсчёт throttled, ни один запрос не уходит.

ИДЕМПОТЕНТНОСТЬ обязательна: renderLoginState зовут из снапшота GET /api/bitrix/status, из WS-кадра login_state и из ответа каждого POST'а подключения. Повторный вызов не сбрасывает поле, не перезапускает отсчёт и не навешивает вторых слушателей.

Ошибки разбираются ПО КОДУ (invalid_input / flood_wait / scope_missing / not_authorized / not_connected), а не по тексту. На экране обязано быть ограничение Р15: вебхук выпускается от имени одного сотрудника портала, и в remote-режиме все, кто открыл devforge, увидят его переписку.

Связи: core/dom.js (ref'ы #b24-login*), js/bitrix/api.js (loginWebhook/logout), js/bitrix/main.js (applyLoginState делегирует сюда оверлей). Тесты: tmux-web/tests/frontend/bitrix_login.test.mjs (58 проверок на фейковом DOM). Серверная сторона — plugins/bitrix/src/{login,routes/login}.rs, страница bitrix-login.

ФАЗА 5. Переход в authorized запускает загрузку данных панели (main.js::loadPanelData), а падение из authorized — resetChat(): лента чужого портала не должна оставаться на экране.

## МУЛЬТИПОРТАЛЬНОСТЬ (Ф5/Ф8)

Экран подключения панели «Битрикс24» (оверлей #b24-login). Один шаг и одно поле: адрес входящего вебхука.

АВТОМАТ ОТОБРАЖЕНИЯ (source of truth — поле state ответа сервера): not_configured / checking / invalid_url / invalid_credentials / scope_missing / throttled / disconnected / authorized. renderLoginState(dto) ИДЕМПОТЕНТНА — её зовут из /status, из WS-кадра login_state и из ответа каждого POST'а подключения.

ТРИ ПРАВИЛА: разбор вставленного адреса живёт ТОЛЬКО на бэкенде (ни одного split/match/replace — это проверяет bitrix_contract.test.mjs); секрет не задерживается в DOM и не попадает в сторадж (поле очищается немедленно после успеха); автоповтора нет — пока тикает обратный отсчёт throttled, ни один запрос не уходит.

ДВА РЕЖИМА ОДНОГО ЭКРАНА (мультипортальность, Ф8):
- 'connect' — подключение АДРЕСУЕМОГО портала (POST /api/bitrix/login/webhook); экран открыт, пока состояние не authorized;
- 'add' — добавление ЕЩЁ ОДНОГО портала (POST /api/bitrix/portals), открывается кнопкой + строки вкладок через openAddPortal(), закрывается успехом или cancelAddPortal().
Второго экрана НЕ заводится: это единственное место, где через фронт проходит СЕКРЕТ, и вся работа с ним написана здесь один раз. В режиме 'add' renderLoginState НЕ ДЕЛАЕТ НИЧЕГО — снимки /status и кадры login_state в этот момент описывают ПЕРВЫЙ портал и закрыли бы экран прямо под руками, стерев введённое. Неудачная проверка вебхука портал не откатывает, поэтому успешный ответ POST /portals закрывает экран в ЛЮБОМ состоянии подключения.
SCREEN_TITLE — заголовки режимов ('BITRIX24 // CONNECT' / 'BITRIX24 // ADD PORTAL'), узлы #b24-login-title и #b24-login-cancel.
Колбэки initB24Login: onAuthorized, onLoggedOut, onPortalAdded(portalId), onAddCancelled.
