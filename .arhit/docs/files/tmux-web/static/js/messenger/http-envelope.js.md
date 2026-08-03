# tmux-web/static/js/messenger/http-envelope.js

Разбор конверта ошибок REST для панелей мессенджеров (создано в Ф3 VK).

Вынесено из js/telegram/api.js (call/jsonInit); telegram/api.js не редактируется.

Экспорт: createCaller(apiFetch) -> call(path, init); jsonInit(method, payload); parseErrorEnvelope(status, body) -> Error с полями .status/.code/.retryAfterSecs/.body.

Контракт обоих плагинов одинаков: {"error":{"code":...,"message":...,"retry_after_secs":...}} плюс необязательные поля VK (captcha_sid, captcha_img, redirect_uri) — форма VK является НАДМНОЖЕСТВОМ телеграмной, поэтому один разборщик обслуживает обе панели.

ГЛАВНЫЙ ИНВАРИАНТ: решения принимаются по .code, а НЕ по тексту ошибки. Тексты меняются (в них подмешивается дословная формулировка VK), коды — контракт, зафиксированный таблицей в шапке plugins/vk/src/routes/error.rs.

Краевые случаи: тело не-JSON (страница 502 от прокси) даёт code=null и текст как есть; пустое тело — фолбэк 'HTTP <status>'; 204 без тела на успешном ответе возвращает null, а не бросает. apiFetch приходит аргументом, а не импортом, чтобы модуль грузился в Node и чтобы тест мог подсунуть свой транспорт.
