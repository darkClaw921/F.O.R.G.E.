# Тумблеры Cmd-подсказок и «Следующего шага» (вкладка Настройки → Интерфейс)

Две ранее всегда-включённые фичи стали opt-in и по умолчанию ВЫКЛЮЧЕНЫ. Настройки: cmd_hints_enabled и next_step_enabled в UserSettings (~/.forge/user_settings.json, GET/PATCH /api/user-settings).

## Что именно выключается

**cmd_hints_enabled** — Cmd-hold hint mode в static/hotkeys.js: удержание ⌘ >200мс рисует буквенные бейджи на кликабельных элементах. Vim-часть того же файла (1/2/3, gt, j/k, ?) настройкой НЕ управляется и работает всегда.

**next_step_enabled** — фича «Следующий шаг» ЦЕЛИКОМ, а не только косметика: воркер Echo (plugins/echo/src/next_step) перестаёт дёргать Claude CLI, голубое свечение .has-next-step гаснет, hover-попап не открывается. Выбор пользователя: фича дорогая (LLM-вызов на каждый эпизод затихания), выключенной она не должна стоить ничего.

## ВАЖНО: намеренный слом инварианта tw-z6l

user_settings.rs документировал инвариант «нулевая конфигурация = поведение побитово как до фичи user-settings». Для всех todo_* и echo_* полей он в силе. Эти два поля его НАМЕРЕННО нарушают — по прямому требованию пользователя. Дефолты false это не забытая default-функция: НЕ «чините» их обратно на true. Зафиксировано тестом user_settings::tests::test_interface_flags_default_off.

## Терминология: голубое ≠ оранжевое

Частая путаница. Это ДВА разных, взаимоисключающих индикатора:
- .needs-attention — ОРАНЖЕВЫЙ (--warn #d29922, sidebar.css:104), источник SessionDto.needs_attention из attention.rs. Claude показал permission/plan/question prompt и ждёт ответа. Настройкой НЕ управляется.
- .has-next-step — ГОЛУБОЙ пульсирующий (#4fd0e7, sidebar.css:216-247), источник state.nextSteps из /api/echo/next-steps. Claude ЗАКОНЧИЛ, сессия затихла на 10+ с и Echo сгенерировал подсказку. Сессии с needs_attention=true из источника свечения ИСКЛЮЧЕНЫ (attention.rs::idle_snapshot).
«Свечение при подсказках» = второе.

## Архитектура гейтов

### Backend — trait-метод, а не ложь в idle_sessions

HostApi::next_step_enabled() -> bool (sync, без Result — читает флаг из памяти, прецедент auth_token). Default-impl возвращает true = поведение до флага, поэтому три существующих stub'а (next_step/mod.rs, routes/next_step.rs, tests/echo_smoke.rs) компилируются нетронутыми. Реальный гейт даёт только EchoHostAdapter (echo_host.rs), читающий UserSettingsStore.

Рассматривалась альтернатива «вернуть Ok(vec![]) из idle_sessions при выключенной фиче» — отвергнута: контракт idle_sessions обещает «затихшие сессии», и возврат пустого вектора из-за настройки UI сделал бы метод лжецом. Сегодня вызывающая сторона одна, но trait эксклюзивности не обещает — следующий консьюмер унаследовал бы невидимый гейт под чужим именем.

next_step::tick_once спрашивает флаг КАЖДЫЙ тик (значение меняется в рантайме) и при false вызывает reset_stale_episodes(state, processed, &HashSet::new()) + return. Пустой idle_names помечает все живые предложения stale → чистит processed + next_steps + шлёт has_suggestion=false, гася свечение у тех, кто светился на момент выключения. Корректно благодаря инварианту processed ⊇ keys(next_steps). В steady-state бесплатно: со второго выключенного тика processed пуст и reset_stale_episodes выходит на stale.is_empty(). Воркер продолжает тикать → включение подхватывается за ≤2с без рестарта.

### Frontend — ленивое чтение, без шины событий

Шины событий для настроек в проекте нет; консьюмеры читают state.userSettings в точке использования (прецедент isEchoNotificationsEnabled в echo/notifications.js). Поэтому «применять» настройку после Save не нужно — updateUserSettings уже перезаписал state.userSettings.

- hotkeys.js — классический IIFE, читает через window.ForgeApp.state.userSettings (js/public-api.js публикует ТУ ЖЕ живую ссылку на state). Никаких window.__forgeFlag и функций синхронизации не потребовалось. Гейт внутри ветки Meta, строгое '=== true'.
- sessions/sessions.js::fetchNextSteps — приватный предикат isNextStepEnabled(); при выключенной фиче ставит state.nextSteps = {} и выходит БЕЗ HTTP-запроса. Один гейт гасит и свечение, и попап, т.к. оба консьюмера (sessions.js:124, next-step-popup.js:96) читают только state.nextSteps. Предикат намеренно приватный, а не импорт из settings/interface-tab.js — иначе инверсия слоёв.

Известная шероховатость: bootstrap.js зовёт fetchSessions() ДО fetchUserSettings(), поэтому самый первый fetchNextSteps видит userSettings === null и гейтится. У пользователя с ВКЛЮЧЁННОЙ фичей свечение появится на тик позже (3с) или сразу по WS NextStepEvent. Переупорядочивать bootstrap не стали.

## Service worker

/hotkeys.js лежит в SHELL_ASSETS и раздаётся через stale-while-revalidate, поэтому потребовался бамп CACHE_VERSION 'forge-pwa-v1' → 'forge-pwa-v2' (sw.js) + синхронная правка констант в tests/frontend/sw.test.js. Без бампа первый reload после обновления выполнил бы старую копию hotkeys.js и тумблер молча не сработал бы, самоизлечившись лишь со второго reload.

interface-tab.js в SHELL_ASSETS НЕ добавляли: sw.test.js проверяет ровно 13 ресурсов в addAll, а файл под /js/ подтягивается runtime-SWR. /api/user-settings и /api/echo/next-steps не в DATA_ALLOWLIST → не кешируются.

## Файлы

- tmux-web/src/user_settings.rs — поля, Default, PatchUserSettingsReq, patch, тесты
- plugins/echo-host-api/src/lib.rs — HostApi::next_step_enabled (default true)
- tmux-web/src/echo_host.rs — impl, читает UserSettingsStore
- plugins/echo/src/next_step/mod.rs — гейт в tick_once + тесты disabled_feature_does_not_generate / disabling_feature_clears_live_suggestion
- tmux-web/static/js/settings/interface-tab.js — форма (новый)
- tmux-web/static/js/settings/modal.js — вкладка (реестра табов нет: кнопка, панель, ветка showTab, whitelist initialTab)
- tmux-web/static/hotkeys.js — гейт + справка
- tmux-web/static/js/sessions/sessions.js — гейт fetchNextSteps
- tmux-web/static/sw.js + tests/frontend/sw.test.js — бамп версии

CSS не правился: переиспользованы .notify-fieldset / .modal-check и др.