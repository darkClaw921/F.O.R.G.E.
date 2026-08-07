БАГФИКС forge-zw3d (2026-08-06): три жалобы пользователя на вкладку «Битрикс24» + одна находка живой приёмки.

## 1. Кнопки ботов не нажимались (реальная причина: ДВА неизвестных вида кнопок)

Диагноз через живой curl к test-work-2.bitrix24.ru (im.dialog.messages.get, чат «Команда заботы Битрикс24», BOT_ID=17): все живые кнопки реальных ботов портала используют поля LINK и COMMAND/COMMAND_PARAMS, а НЕ ACTION/ACTION_VALUE — единственный вид, который разбирал attach.rs::button_from_value до фикса. Итог: 100% живых кнопок приезжали action='other', enabled=false, «Портал прислал незнакомое действие кнопки» — хотя формально валидны.

Разведка через MCP b24-dev-mcp (bitrix-article-details «Работа с клавиатурами») подтвердила: у кнопки бывает ОДНО из четырёх действий — LINK, ACTION+ACTION_VALUE, COMMAND(+COMMAND_PARAMS), APP_ID(+APP_PARAMS, устаревший Открытые линии сценарий, не реализован).

Решение (attach.rs::button_from_value, порядок проверки LINK → COMMAND → ACTION):
- LINK → button_action::LINK, value = http_url(LINK) (тот же санитайзер, что у карточек). Работает КЛИЕНТСКИ, botkb.js::pressButton → window.open(url,'_blank','noopener,noreferrer'), второй рубеж — botkb.js::safeUrl.
- COMMAND → button_action::COMMAND, поля command/command_params/bot_id на ButtonDto. Требует СЕТЬ — до этой фичи у клавиатуры бота такого пути не было вовсе. Новый метод im.message.command (МЕТОД: Scope 'im', НЕ 'imbot'!) — живая проба curl'ом (MESSAGE_ID=51,BOT_ID=17,COMMAND=careButton,COMMAND_PARAMS=1) вернула result:true ОБЫЧНЫМ входящим вебхуком БЕЗ scope imbot. Это открытие меняет расклад §6 плана: не все действия чат-бота требуют imbot, только те, что требуют ЗАРЕГИСТРИРОВАННОГО бота (call/dialog), а не команду уже существующего.
  - Backend: api/methods.rs::{M_MESSAGE_COMMAND, message_command_params, B24Client::message_command}; в api/mod.rs::NO_RETRY_METHODS (не идемпотентен — как im.message.add, повтор = второе исполнение, второй голос); routes/dialogs.rs::{CommandReq, post_command} на POST /api/bitrix/messages/:mid/command (без reread — команда обычно НЕ меняет текущее сообщение, ответ бота придёт НОВЫМ сообщением через реалтайм).
  - Frontend: api.js::sendCommand; botkb.js::doCommand (сеть + собственное состояние кнопки — по образцу telegram/botkb.js::doCallback, а НЕ через колбэк в chat.js: единственное исключение из правила «модуль не импортирует сеть», см. шапку botkb.js). Кнопка синхронно disabled+pending на время запроса; при lock_after_press(BLOCK:Y) успех оставляет её disabled НАВСЕГДА (класс b24-kb-btn-off), ошибка возвращает в рабочее состояние.
- Кнопка без BOT_ID (COMMAND) приезжает выключенной: COMMAND_NEEDS_BOT_ID.

Живая приёмка (test-work-2, чат id=17): клик по «Получить консультацию» (LINK) открыл https://www.bitrix24.ru/register/cbot.php новой вкладкой; клик по кнопке «5» (COMMAND careButton/5) вернул result:true, кнопка получила b24-kb-btn-off НАВСЕГДА (BLOCK:Y у этих кнопок).

## 2. Находка ПОПУТНО (не в жалобе, но блокировала бы использование фикса №1): невидимый текст кнопки

botkb.js::buildButton красил bg_color ТОЛЬКО в borderColor, а text_color — всегда в color. У «Команда заботы Битрикс24» BG_COLOR='#fff2b7' (бледно-жёлтый), TEXT_COLOR='#000' (чёрный) — пара, которую бот подобрал под ЗАЛИВКУ (портал в официальных клиентах заполняет ею весь прямоугольник кнопки). У нас заливки не было (background: transparent из CSS), и чёрный текст на прозрачном тёмном фоне панели был НЕВИДИМ — кнопка выглядела пустым прямоугольником (хотя формально нажималась). Найдено ТОЛЬКО во время живой приёмки скриншотом. Фикс: bg_color теперь красит и backgroundColor, и borderColor.

## 3. Свои сообщения подсвечивались, должны быть на фоне чата

css/bitrix.css::.b24-line-own имел background: var(--b-active) — сплошную подложку на КАЖДОЙ своей строке. У Telegram-плагина (эталон, указанный пользователем) класс tg-line-own существует, но НИ ОДНОГО правила CSS для него нет: своё сообщение отличается ТОЛЬКО цветом ника (nick-own), фон общий для всей ленты. Правило .b24-line-own убрано целиком; класс на li остаётся (используется для e2e/семантики), просто без стиля.

## 4. Двойной клик по сообщению не отвечал (в Bitrix такого пути не было вовсе)

У Telegram есть отдельный модуль reply.js с classifyRowClick + dblclick на #tg-log. У Bitrix reply был ТОЛЬКО через кнопку «↩» (startReply(id) уже существовал с Ф6) — dblclick-слушателя на #b24-log не было ни разу.

Добавлено в chat.js (без нового модуля — reply и так живёт в chat.js как local.mode, в отличие от Telegram, где это отдельный чистый модуль):
- classifyLineDblClick(target) — чистая функция, экспортирована, тест без DOM. Ignore-список REPLY_DBLCLICK_IGNORE = ['.b24-line-media', '.b24-kb', 'button'] (у Bitrix НЕТ <a href> — все ссылки текстом, format.js::renderSegments, поэтому в отличие от Telegram ссылки в ignore-список добавлять не нужно).
- onLogDblClick(e) — читает li.dataset.msgId, зовёт startReply(id).
- startReply(id) получил дополнительный guard: || msg.system || msg.deleted (раньше проверялись только pending/failed) — у системной строки нет автора для цитаты, у удалённой текст уже подменён заглушкой, и buildActions() им кнопку «↩» тоже не предлагает — guard просто выравнивает dblclick-путь с уже существующим поведением кнопки.

## Тесты и живая приёмка

Backend: attach.rs +4 теста (LINK/COMMAND happy-path на дословных живых фикстурах портала, COMMAND без BOT_ID, невалидный LINK всё равно рисуется disabled а не пропадает), methods.rs +2 (command_params, NO_RETRY guard), api/mod.rs NO_RETRY_METHODS [2]→[3], b24_stub.rs +1 route-тест (post_command, включая 400 на пустой command и 404 на мусорный mid) + пункт 6 в transport_error_is_retried_once_and_message_add_never (im.message.command тоже НЕ ретраится). Frontend: bitrix_chat.test.mjs +2 секции (LINK/COMMAND клавиатура включая lock/error-путь; двойной клик — reply/ignore/system/button/spacer). Все 13 файлов tests/frontend/bitrix_*.test.mjs зелёные, bitrix_contract.test.mjs (новый путь /api/bitrix/messages/:mid/command в обеих таблицах) зелёный, cargo test -p forge-bitrix зелёный, clippy чист.

Живая приёмка на test-work-2.bitrix24.ru (изолированный HOME+порт, debug-сборка читает static/ с диска без пересборки — rust-embed без debug-embed feature): все 4 пункта подтверждены скриншотами и сетевыми вызовами.

Связано: bitrix-attach (ButtonDto — источник истины по действиям), bitrix-plugin, bitrix-routes.