# forge_cdp::inject::BindingReceiver

Приёмник данных из страницы через Runtime.addBinding: страница зовёт window.__forge_<rand32>(payload), браузер шлёт Runtime.bindingCalled, приёмник отдаёт кадры вызывающему. Подписываться НАДО ДО навигации: подписка после prepare_and_navigate теряет первые кадры. Именно этот канал заменяет сеть в плагине встреч — HTTP-роут для звука не создаётся никогда.
