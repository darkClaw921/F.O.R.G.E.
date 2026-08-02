# scrollTargetForRow

scrollTargetForRow(log, row, pad?) -> ?number (tmux-web/static/js/telegram/follow.js) — вычисляет новый scrollTop ленты Telegram так, чтобы КОНЕЦ блока строки оказался виден. Возвращает null, если двигать не нужно.

ПАРАМЕТРЫ:
 * log — {scrollTop, scrollHeight, clientHeight}. В продакшене это сам элемент #tg-log (DOM-узел подходит по утиной типизации), в тестах — обычный объект. null ⇒ null;
 * row — {top, bottom} — координаты строки ОТНОСИТЕЛЬНО ВЕРХА ВИДИМОЙ ОБЛАСТИ ленты, то есть `rowRect.top - logRect.top` / `rowRect.bottom - logRect.top`. Считает их chat.js::rowOffset через getBoundingClientRect. null ⇒ раскладка неизвестна, цель — самый низ (maxTop);
 * pad (необязательный) — зазор под нижним краем, по умолчанию FOLLOW_SCROLL_PAD (8px).

ФОРМУЛА: `maxTop = max(0, scrollHeight - clientHeight)` (потолок прокрутки); `want = scrollTop + (row.bottom - clientHeight) + pad`; затем `want` отсекается потолком; наконец `return want > scrollTop ? want : null`.

ВВЕРХ ФУНКЦИЯ НЕ ТЯНЕТ НИКОГДА (`want <= scrollTop` ⇒ null). Это ключевое ограничение, а не оптимизация: следование обязано только ДОГОНЯТЬ уехавший вниз конец сообщения, а не возвращать пользователя к нему сверху. Без этого правила живое следование перебивало бы переход к оригиналу по цитате (jumpToMessage уезжает ВВЕРХ) и «подпрыгивало» бы на каждом кадре ResizeObserver у уже видимой строки. Оно же гарантирует, что все собственные доскроллы увеличивают scrollTop, — на чём стоит cancelledByScroll (отмена по направлению движения, а не по флагу).

ПОЧЕМУ getBoundingClientRect, А НЕ offsetTop (в вызывающем chat.js::rowOffset): у #tg-log нет своего `position`, смещения его детей считаются от #telegram, и прямой `scrollTop = row.offsetTop` промахнулся бы на высоту титульбара и шапки сессии. Та же причина указана в chat.js::scrollRowIntoView.

ЧИСЛОВЫЕ ГАРДЫ: все поля прогоняются через `Number(...) || 0`, pad — через Number.isFinite. Мусор в раскладке (NaN из фейкового DOM, отсутствующие поля) даёт корректный no-op, а не NaN в scrollTop.

ПОТРЕБИТЕЛЬ: chat.js::scrollBlockIntoView.
