# csrf_guard

tmux-web/src/auth.rs::csrf_guard — anti-CSRF / drive-by guard, подключён ВСЕГДА (и в localhost, и в remote) внешним слоем в main.rs.

Логика:
- /ws/* — если Origin задан и не same-origin -> 403 (drive-by RCE через /ws/attach). Native-клиенты Origin не шлют — пропускаются.
- мутирующий /api/* (POST/PUT/PATCH/DELETE) — требует same-origin Origin (если задан) И Content-Type: application/json, когда у запроса есть тело. Это лишает атакующего 'простого запроса' CORS (text/plain, form-urlencoded, multipart): с JSON-Content-Type браузер обязан сделать preflight, а CORS-заголовков сервер не отдаёт вовсе.
- bodyless-мутации (DELETE без тела) Content-Type не требуют.
- GET/HEAD, статика, healthz — пропуск.

ИСКЛЮЧЕНИЕ (Фаза 5 Telegram, is_allowed_raw_body): тело без application/json пропускается ТОЛЬКО для точного пути /api/telegram/upload и ТОЛЬКО с Content-Type application/octet-stream (основной тип, параметры вроде '; charset=binary' игнорируются). Файл там передаётся сырыми байтами, и JSON физически неприменим.
Почему это не ослабляет защиту: application/octet-stream НЕ входит в список 'простых' Content-Type CORS, поэтому кросс-доменный fetch/XHR с ним браузер обязан предварить preflight'ом OPTIONS, который не пройдёт; через <form> такой Content-Type выставить нельзя в принципе. Проверка same-origin остаётся ОБЯЗАТЕЛЬНОЙ и выполняется ДО исключения.
Сравнение по ПОЛНОМУ пути, а не по префиксу /api/telegram/: под префиксное исключение автоматически попадали бы будущие мутирующие роуты плагина и молча теряли бы защиту.

Тесты (auth.rs::tests, 31 шт.): same-origin octet-stream на /api/telegram/upload -> 200; тот же запрос с чужим Origin -> 403; /api/telegram/dialogs/:id/messages с octet-stream -> 415; text/plain, multipart/form-data, x-www-form-urlencoded на upload -> 415; 'Application/Octet-Stream; charset=binary' -> 200. Плюс проверено curl'ом на живом инстансе.
