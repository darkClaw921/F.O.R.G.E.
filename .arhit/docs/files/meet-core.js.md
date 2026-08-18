# meet-core.js

Общее ядро детекта ссылок на видеовстречи для трёх панелей (tmux-web/static/js/messenger/meet-core.js). Чистый модуль: ни DOM, ни сети, ни таймеров. Создан фронтовой Ф1 forge-meet, переработан фронтовой Ф2.

БЕЗОПАСНОСТЬ. Совпадение платформы ищется ТОЛЬКО в URL.hostname — точным равенством либо суффиксом, начинающимся с точки. Подстрочный поиск даёт четыре обхода (поддомен-обманка telemost.yandex.ru.evil.com, домен в query, домен во фрагменте, домен в userinfo telemost.yandex.ru@evil.com), каждый закрыт отрицательным тестом. Цена ошибки не 'кнопка не появилась': адрес отсюда уезжает на бэкенд, и его открывает настоящий Chrome с отключённым CSP и нашим инжектированным скриптом внутри. Схема проверяется существующим safeHref из markdown.js — четвёртой копии safeUrl в проекте нет.

НОРМАЛИЗАЦИЯ — белый список параметров, а не чёрный: у Zoom в pwd пароль встречи, у Teams в context арендатор и организатор. Срез всей query сломал бы вход на двух платформах из четырёх.

РЕЕСТР ПЛАТФОРМ ПРИХОДИТ С БЭКЕНДА (GET /api/meet/status). Встроенный BUILTIN_MEET_PLATFORMS — запасной вариант, а не источник истины: разметку браузер держит в кэше PWA неделями. Ф2 ЗАМОРОЗИЛА ФОРМУ ЗАПИСИ: принимается ровно проводное написание Provider — platform, title, hosts, host_suffixes, paths, keep_params, key_case, link_regex, audio; обёртка одна (providers). Терпимость к id/domains/path_patterns/params/keyCase и обёртке platforms снята: она маскировала бы молчаливый откат на встроенный набор при переименовании поля на бэкенде. Встроенный набор — ЗЕРКАЛО серверного реестра, равенство держится тестом meet_contract.test.mjs поле в поле.

localKey — ЭТО НЕ url_key. detectMeetUrl возвращает {platform, title, id, localKey, url, raw, audio}. localKey ('<платформа>:<идентификатор>') — локальная метка: подпись строки ленты, дедуп нескольких ссылок в сообщении, data-meet-key в разметке. На сервер уезжает ТОЛЬКО url (канонический адрес), а идентичность комнаты (url_key) считает бэкенд, разбирая его заново. Имена разведены намеренно: правила идентичности у платформ разные и в запись реестра не помещаются, вторая реализация начала бы расходиться с первой. Решение по вопросу 'зеркала Телемоста — одна комната или разные' записано в шапке: хост остаётся частью идентичности, пока живая проба не покажет обратное (задача forge-87hum).

ИСТОЧНИК ТЕКСТА В КАЖДОЙ ПАНЕЛИ РАЗНЫЙ: Telegram — msg.text (приватная messageText() не годится, подмешивает [photo]); VK — msg.text ПЛЮС вложения (адрес лежит в link_url нормализованного MediaDto при kind='other', а не в kind='link'); Битрикс — обход msg.segments (segmentsText() в одиночку теряет адрес у [URL=…]созвон[/URL], а у [URL]адрес[/URL] value===null и адрес лежит текстом внутри).

Экспорты: BUILTIN_MEET_PLATFORMS, configureMeetPlatforms, resetMeetPlatforms, meetPlatforms, detectMeetUrl, isMeetUrl, collectUrlsFromText, collectUrlsFromSegments, MEET_PANELS, messageUrlCandidates, detectMeetInMessage, detectAllMeetsInMessage.

Тесты: tmux-web/tests/frontend/messenger_meet_core.test.mjs (205 проверок) и meet_contract.test.mjs.
