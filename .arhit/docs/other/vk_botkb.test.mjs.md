# vk_botkb.test.mjs

tmux-web/tests/frontend/vk_botkb.test.mjs — node-тест клавиатур сообществ VK (122 проверки, Ф9). Запуск: node tmux-web/tests/frontend/vk_botkb.test.mjs.

Грузит РЕАЛЬНЫЕ модули js/vk/botkb.js и js/vk/chat.js поверх фейкового DOM (vk_dom_stub.mjs импортируется ПЕРВЫМ — core/dom.js резолвит ref'ы один раз при импорте).

Шесть разделов:
1. чистая логика — cleanButtonText (bidi-спуфинг, эмодзи, обрезка), normalizeButton (координаты из DTO, кнопка без подписи), normalizeMarkup (матрица, потолки 32x12, hide отбрасывается);
2. таблица видов кнопок + белый список схем URL (10 враждебных адресов) + инвариант «supported:true не делает вид исполнимым»;
3. инлайн-клавиатура в строке ленты: text/open_link активны, callback disabled с title, клик по отключённой не порождает запросов;
4. нажатие text: POST /api/vk/dialogs/:id/messages/:mid/button с телом ровно {row, col} (без payload) и «следование» ленты, включённое ДО запроса (проверяется синхронно, до await);
5. open_link: window.open с noopener, ноль сетевых вызовов;
6. нижняя клавиатура полосой над #vk-prompt: показ, one_time -> сворачивание после нажатия, возврат кнопкой, смена беседы, пересчёт по истории (latestReplyMarkup), resetChat.

МУТАЦИОННЫЕ МИШЕНИ (проверено): callback помечен supported -> раздел 3; payload вместо координат -> раздел 4; снят белый список схем -> раздел 2; onButtonPress после запроса -> раздел 4.

Связанные: js/vk/botkb.js, vk-button-support, vk_dom_stub.mjs.
