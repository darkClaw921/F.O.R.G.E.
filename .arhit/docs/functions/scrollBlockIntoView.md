# scrollBlockIntoView

scrollBlockIntoView(row) (tmux-web/static/js/telegram/chat.js, внутренняя) — доскролл ленты Telegram так, чтобы КОНЕЦ блока строки оказался виден. Исполнительная половина «следования»; решение «куда и надо ли» считает follow.js.

ПОСЛЕДОВАТЕЛЬНОСТЬ:
 1. гард `if (!$tgLog || local.loading) return`;
 2. `top = scrollTargetForRow($tgLog, rowOffset(row), FOLLOW_SCROLL_PAD)`; `null` ⇒ выход (двигать не нужно, либо это было бы движением ВВЕРХ, чего следование не делает никогда);
 3. `behavior = scrollBehavior(prefersReducedMotion())` — 'auto' при reduce, иначе 'smooth';
 4. `$tgLog.scrollTo({top, behavior})` под `typeof === 'function'` и в try/catch; при отсутствии или исключении — фолбэк `$tgLog.scrollTop = top`.

ГАРД local.loading — САМЫЙ ВАЖНЫЙ. Во время подгрузки истории вверх loadOlder держит ЯКОРЬ: запоминает scrollHeight, вставляет страницу старых сообщений в начало ленты и восстанавливает scrollTop с учётом прироста. Посторонняя прокрутка внутри этого окна необратимо теряет позицию чтения — пользователя выбросило бы в случайное место истории. Поэтому следование во время загрузки молчит; после неё renderHistory переставит наблюдение, и добор роста продолжится. Мутационная проверка (снятие гарда) поймана тестом telegram_scroll_edge.test.mjs, секция «подгрузка истории вверх (якорь)».

ФОЛБЭК НА scrollTop — не косметика: `Element.scrollTo` есть не во всех WebView и отсутствует в node-стендах тестов, а безусловный вызов уронил бы весь обработчик ленты.

КТО ЗОВЁТ: afterRowChanged (ветка 'follow' — сразу после появления/пересборки строки) и onFollowResize (отложенный рост).

СВЯЗАННЫЕ: rowOffset, follow.js::scrollTargetForRow, follow.js::scrollBehavior, prefersReducedMotion, FOLLOW_SCROLL_PAD.
