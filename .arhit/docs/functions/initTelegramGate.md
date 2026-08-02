# initTelegramGate

Гейт кнопки вкладки Telegram (tmux-web/static/js/telegram/main.js).

ЧТО ДЕЛАЕТ: вызывается один раз из core/bootstrap.js при инициализации UI. Запрашивает GET /api/telegram/status. Ответ 200 с {enabled:true} -> снимает hidden с кнопки #telegram-toggle (✈ в #settings-bar рядом с 🍺) и применяет пришедшее состояние логина (applyLoginState). Любая ошибка, включая 404 'плагин выключен' -> кнопка молча остаётся скрытой, в консоль НИЧЕГО не пишется: выключенный opt-in плагин это штатная ситуация, а не сбой. Идемпотентен (флаг local.gated).

СОСЕДНИЕ ЭКСПОРТЫ МОДУЛЯ: initTelegram() — первое открытие вкладки (вызывается из tabs/tabs.js, идемпотентен, переспрашивает статус, т.к. вкладку могли открыть много позже гейта); applyLoginState(dto) — оверлей #tg-login, видимый шаг wizard'а, #tg-self-user, кнопка выхода; setUnreadBadge(n) — бейдж #tg-unread-badge (99+ при переполнении); setMode('connected'|'reconnecting'|'offline') и setLatency(ms) — статусбар; suspendTelegram()/resumeTelegram() — реакция на visibilitychange (WS у Telegram постоянный, поэтому закрывается не по смене вкладки, а по уходу страницы в фон); teardownTelegram() — beforeunload; _debugState() — снимок для e2e (по образцу echo/main.js).

СВЯЗИ: api.js (getStatus + разбор контракта ошибок в Error.status/.code/.retryAfterSecs), state.js (tgState), core/dom.js ($tg*-рефы). Все данные попадают в DOM только через textContent — innerHTML в модулях плагина не используется (XSS).
