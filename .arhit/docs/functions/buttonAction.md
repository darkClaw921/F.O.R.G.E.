# buttonAction

tmux-web/static/js/telegram/botkb.js — классификация кнопки в ДЕЙСТВИЕ. Единственная точка,
решающая, что произойдёт по клику. Принимает и нормализованную кнопку, и сырой ReplyButtonDto.

Сигнатура: buttonAction(btn) -> {action, ...}:
- {action:'callback', data: ?string} — kind 'callback' БЕЗ requires_password;
- {action:'open-url', url} — 'url' | 'login_url' | 'user_profile', url прошёл safeUrl;
- {action:'inline-query', query, samePeer} — 'switch_inline' (samePeer=false) |
  'switch_inline_current' (samePeer=true); query подставляется в поле ввода БЕЗ отправки:
  автоматическая отправка от имени пользователя недопустима;
- {action:'send-text', text} — 'text' (обычная кнопка reply-клавиатуры: нажатие = отправка
  подписи);
- {action:'unsupported', reason} — всё остальное, с ЧЕЛОВЕЧЕСКОЙ причиной, которую фронт
  показывает тостом: request_phone/request_location/request_poll/web_view/buy/game/other,
  недопустимый или пустой url, а также callback с requires_password («кнопка требует пароль
  2FA — не поддержано в v1»: сервер шлёт password: None, Telegram ответил бы PASSWORD_MISSING).

Неподдержанная кнопка РИСУЕТСЯ (визуально неактивной), а не прячется: пропавшая кнопка выглядит
как поломка панели, а неактивная с причиной — как честное ограничение.
