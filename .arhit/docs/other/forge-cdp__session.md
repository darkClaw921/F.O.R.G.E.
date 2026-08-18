# forge-cdp::session

`plugins/forge-cdp/src/session.rs` — flat-сессии, `CdpBrowser`/`CdpSession`, насос автоаттача.

## Flat-режим

`Target.attachToTarget{flatten: true}`: все цели — страница, её OOPIF-фреймы, воркеры — говорят по ОДНОМУ сокету, а различаются полем `sessionId`. Альтернатива (вложенные сессии через `Target.sendMessageToTarget`) требует упаковки команд в строку внутри команды и давно устарела.

`CdpSession` дёшево клонируется (Arc на транспорт + две строки), даёт `call`, `subscribe`, `enable_domains`, `set_auto_attach`, `run_if_waiting_for_debugger`. `CdpBrowser` — браузерный уровень: `create_target`, `attach`, `open_target` (create+attach+enable), `close_target`, `grant_permissions`, `spawn_auto_attach`.

`enable_domains` включает Page (навигация и жизненный цикл документа), Runtime (evaluate и СОБЫТИЕ `bindingCalled` — без `Runtime.enable` биндинг не доедет вообще) и Log (сообщения консоли: единственный способ увидеть, что наш инжект упал внутри чужого сайта). Ошибка `Log.enable` не роняет старт — у воркеров этого домена нет.

## Почему автоаттач обязателен

Teams и часть Google Meet живут в OOPIF: интерфейс встречи — вложенный документ другого источника, в отдельном процессе рендерера. Без `Target.setAutoAttach` мы к нему просто не подключимся, и инжект туда не попадёт — притом что на самой странице всё будет выглядеть исправно.

## Ловушка waitForDebuggerOnStart

С `waitForDebuggerOnStart: true` новая цель СТОИТ и ждёт нас — ровно то, что нужно, чтобы успеть поставить инжект до выполнения кода фрейма. Но отпустить её обязаны мы: забытый `Runtime.runIfWaitingForDebugger` вешает фрейм НАМЕРТВО. Поэтому в `handle_attached_target` освобождение стоит в хвосте, выполняемом при ЛЮБОМ исходе настройки, включая её провал и провал `Page.enable`: лучше фрейм без инжекта, чем замерший фрейм. Это закреплено тестом `target_is_released_even_when_setup_fails`.

Порядок внутри дочерней цели: enable_domains → setup (инжект) → `Target.setAutoAttach` → `runIfWaitingForDebugger`. Автоаттач ДО освобождения, иначе вложенный в OOPIF ещё один OOPIF успеет убежать.

## AutoAttachConfig.ignore_sessions

Главная сессия обязана быть в списке игнора: её настраивает вызывающий код, и повторный инжект дал бы ДВЕ копии скрипта в документе. Насос дополнительно дедуплицирует по sessionId (повторное `attachedToTarget` о той же сессии обрабатывается один раз). Отставание насоса (`Lagged`) логируется предупреждением: пропущенное событие означает замерший фрейм, это диагностируемая беда, а не мелочь.

Насос обязан подписаться на события ДО вызова `Target.setAutoAttach` на главной сессии — иначе первые фреймы прилетят в пустоту и останутся висеть навсегда.

## trait SessionSetup

`async fn setup(&self, session: &CdpSession)` — точка, куда `forge-cdp::inject` вставляет установку биндинга и скрипта нового документа для каждого OOPIF-фрейма. Ошибка логируется, но НЕ отменяет освобождение цели.
