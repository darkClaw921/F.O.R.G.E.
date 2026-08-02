# rowOffset

rowOffset(row) -> ?{top: number, bottom: number} (tmux-web/static/js/telegram/chat.js, внутренняя) — координаты строки ленты ОТНОСИТЕЛЬНО ВЕРХА ВИДИМОЙ ОБЛАСТИ #tg-log.

РЕАЛИЗАЦИЯ: `r = row.getBoundingClientRect(); log = $tgLog.getBoundingClientRect(); return {top: r.top - log.top, bottom: r.bottom - log.top}`. Возвращает null, если нет строки, нет $tgLog или у любого из них отсутствует getBoundingClientRect (фейковый DOM, узел уехал) — null дальше трактуется scrollTargetForRow как «раскладка неизвестна, целься в самый низ».

ПОЧЕМУ getBoundingClientRect, А НЕ offsetTop. У #tg-log нет собственного `position`, поэтому смещения его детей считаются от #telegram, и `scrollTop = row.offsetTop` промахнулся бы на высоту титульбара и шапки сессии. Та же причина и та же формула, что в chat.js::scrollRowIntoView (переход к оригиналу по цитате) — оба места ошибаются одинаково или не ошибаются вовсе.

ЕДИНИЦЫ. Результат — координаты В СИСТЕМЕ ВИДИМОЙ ОБЛАСТИ, а не в системе прокручиваемого содержимого: `bottom` большее clientHeight означает «конец блока ниже кромки, надо доскроллить на разницу». Именно на этом стоит формула scrollTargetForRow: `want = scrollTop + (bottom - clientHeight) + pad`. Смешивать эти системы нельзя — отсюда явная оговорка в JSDoc обеих функций.

ПОТРЕБИТЕЛЬ: scrollBlockIntoView (единственный).
