# ApiError

plugins/telegram/src/routes/error.rs — единый тип ошибки HTTP-слоя плагина Telegram.

Формат наружу (ApiErrorEnvelope + IntoResponse): {"error":{"code","message","retry_after_secs"?}}.
Таблица кодов живёт в шапке модуля, а сторожевой тест full_error_contract_table фиксирует ПОЛНЫЙ
отсортированный список кодов — переименование кода валит тест, а не ломает вкладку молча.

КОДЫ: 409 not_authorized / invalid_state; 404 unknown_dialog / no_media / unknown_message;
429 flood_wait (+ стандартный заголовок Retry-After; авто-ретрай ЗАПРЕЩЁН — FLOOD_WAIT на логине
измеряется часами, UI показывает countdown); 503 not_connected; 400 invalid_input;
502 telegram_error; 504 bot_timeout.

ДОБАВЛЕНЫ ПОД D2 (кнопки ботов):
- unknown_message(message) -> 404 — сообщения нет (удалено / чужой msg_id / :mid не разбирается);
- telegram_error(message) -> 502 — Telegram отказал, и это НЕ лечится ни вводом пользователя, ни
  реконнектом (в отличие от 503 not_connected);
- bot_timeout(message) -> 504 — бот не ответил на нажатие кнопки; message содержит «бот не
  ответил», фронт показывает его как есть.

Разница 502 vs 503 сделана намеренно: она подсказывает фронту, стоит ли предлагать повтор.

impl From<LoginError> for ApiError — единственный маппинг ошибок логина в HTTP, чтобы контракт
нельзя было разъехать между четырьмя роутами.
