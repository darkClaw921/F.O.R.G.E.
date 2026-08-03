# tmux-web/tests/frontend/vk_dom_stub.mjs

Фейковый DOM для node-тестов панели VK (tmux-web/tests/frontend/vk_dom_stub.mjs).

НЕ ТЕСТ, а вспомогательный модуль: импортируется из vk_login.test.mjs и vk_captcha.test.mjs ПЕРВЫМ, до любого модуля js/vk/*. Причина: core/dom.js резолвит все ref'ы ОДИН раз на верхнем уровне модуля, и стаб, поставленный позже, оставил бы каждый $vk* равным null — тест проверял бы пустоту и зеленел по случайности.

Реализует ровно тот срез DOM, которым пользуются login.js и captcha.js: id-поиск, hidden/value/disabled, textContent через настоящие текстовые узлы, src как атрибут (removeAttribute обязан убирать оба), classList, addEventListener на узлах и на документе, focus() со счётчиком. Всплытие событий не эмулируется намеренно: оба модуля вешают слушателей на конкретные узлы и на document и решают всё по e.target, поэтому достаточно доставки одному слушателю — так тест проверяет логику модуля, а не реализацию capture/bubble в стабе.

Плюс стабы окружения: location, localStorage, history, window, а также globalThis.fetch с журналом netCalls и подменяемым обработчиком setNet(fn) и хелперы jsonRes(status, obj) и flush().
