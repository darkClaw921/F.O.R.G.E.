# tmux-web/static/js/vk/api.js

REST-клиент панели VK (Ф3).

Тонкие обёртки над apiFetch через messenger/http-envelope.js::createCaller. Шапка модуля содержит таблицу путей, обязанную совпадать ДОСЛОВНО с шапкой plugins/vk/src/routes/mod.rs: расхождение даёт молчаливый 404, а не ошибку сборки. Пути — /api/vk/dialogs (НЕ conversations), решение Ф1.1.

Функции: getStatus, loginToken({token}), verifyLogin, logout, submitCaptcha({sid,key}), captchaImageUrl(sid), getFilters, getDialogs, getMessages, getMessage, sendMessage, markRead, sendTyping, pressButton, mediaUrl, fetchMedia.

Правила, перенесённые из Telegram дословно: решения по err.code; отсутствие автоповторов у loginToken и pressButton; импорт в потребителях через "import * as vkApi" — закэшированная старая версия тогда даёт «undefined is not a function» в одном месте, а не SyntaxError, роняющий модуль вместе с панелью.

Новое относительно Telegram: каждый мутирующий вызов принимает необязательные {captchaSid, captchaKey} и кладёт их в тело как captcha_sid/captcha_key (имена серверные — это провод), а markCaptcha при коде captcha_required вешает на ошибку err.captcha={sid,img}; ловит его js/vk/captcha.js.

Инвариант «браузер на серверы VK не ходит»: в модуле нет ни одной ссылки на api.vk.com/vk.com/vk.ru. И картинка captcha (captchaImageUrl -> /api/vk/captcha/:sid), и медиа (mediaUrl -> /api/vk/media/...) идут через прокси бэкенда. Индекс вложения в mediaUrl обязателен: у сообщения VK до 10 вложений, и без него второе фото затирало бы первое в кэше.

ДОПОЛНЕНО В Ф12. Таблица путей в шапке модуля закрыта целиком: все роуты из
plugins/vk/src/routes/mod.rs реализованы, включая /upload (Ф8), /media/:peer/:mid/:idx
(Ф7) и /messages/:mid/button (Ф9). Соответствие обеих таблиц теперь проверяется и
по проводу — tmux-web/tests/vk_smoke.rs держит таблицу «метод × путь × тело →
статус + code», а несмонтированный роут ловится сценарием
test_routes_absent_without_the_plugin.
