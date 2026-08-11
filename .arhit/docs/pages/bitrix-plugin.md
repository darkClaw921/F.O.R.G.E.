# Плагин forge-bitrix — вкладка «Битрикс24»

Вкладка мессенджера портала Bitrix24 в devforge: личные диалоги, групповые
чаты и **чаты задач** через ВХОДЯЩИЙ ВЕБХУК пользователя
(`https://<портал>/rest/<user_id>/<код>/`).

Каталог `plugins/bitrix`, крейт `forge-bitrix`, обычный rlib, линкуется
статически в бинарь `devforge`. Двенадцать фаз, ~34 500 строк Rust,
~10 000 строк JS, 1 731 строка CSS. Приёмка на живом портале пройдена
(см. раздел «Живая приёмка» в конце).

Соседние страницы: `bitrix-lifecycle` (четыре точки входа и порядок в хосте),
`bitrix-realtime-model` (поток событий и фолбэк), `bitrix-tasks-model`
(задачи), `bitrix-media-model` (скачивание и кэш), `bitrix-visibility-model`
(белый список), `bitrix-serde-traps` (десять ловушек разбора),
`bitrix-config-secret`, `bitrix-login`, `bitrix-routes`, `bitrix-state`,
`bitrix-b24client`, `bitrix-b24error`, `bitrix-throttle`, `bitrix-batch`,
`bitrix-b24stub`, `bitrix-upload`.

## Как включается

Только явно, по одному из трёх источников (приоритет CLI > file > env >
default(false)):

- CLI-флаг `devforge run --bitrix` (и `devforge start --bitrix` — daemon.rs
  пробрасывает флаг в детач-процесс);
- `~/.config/forge/server_config.json`, поле `"bitrix": true`;
- переменная окружения `DEVFORGE_BITRIX=1|true|yes|on`.

Без включения роуты `/api/bitrix/*` и `/ws/bitrix` НЕ регистрируются вовсе и
отдают 404. На этом 404 держится гейт кнопки вкладки во фронте
(`js/bitrix/main.js::initB24Gate`): 404 = «плагин выключен», кнопка молча
остаётся скрытой, в консоль ничего не пишется. Инвариант закреплён двумя
тестами в `tmux-web/tests/bitrix_smoke.rs` —
`test_routes_absent_without_the_plugin` (роутер без регистрации) и
`test_the_host_registers_the_plugin_only_behind_the_flag` (условие в main.rs
проверяется по исходнику).

## Где конфиг и как он устроен

`~/.config/forge/bitrix.toml`, права `0600`, атомарная запись
(temp + fsync + rename). Путь переопределяется непустым
`FORGE_BITRIX_CONFIG_PATH` — без него `cargo test` затирал бы живой вебхук
разработчика.

**Ключ вебхука называется `webhook`** (константа
`config::WEBHOOK_KEY = "webhook"`), НЕ `webhook_url`: в плане фигурировало
второе имя, реализация остановилась на первом — «url» в TOML-ключе не
уточняет ничего, кроме длины строки. Форма файла:

```toml
webhook = "https://портал.bitrix24.ru/rest/1/код/"
# необязательные ключи (все имеют дефолты и валидируются при загрузке):
# base_url_override   = ""             # Р17: в проде только https
# cache_dir           = "~/.cache/forge/bitrix"
# poll_active_secs    = 4              # Р4, вкладка открыта
# poll_idle_secs      = 20             # Р4, WS-подписчиков нет
# recent_sync_secs    = 60             # Р4, фолбэк-синхронизация
# max_upload_bytes    = 26214400       # Р7, 25 MiB
# max_cache_bytes     = 536870912      # 512 MiB, 0 = не подрезать
# rate_limit_rps      = 2              # клиентское ведро
# burst               = 5
# connect_timeout_secs = 10
# request_timeout_secs = 30
```

Запись — только через `save_webhook_to`: read-modify-write (чужие ключи
переживают сохранение), `0600` выставляются В МОМЕНТ СОЗДАНИЯ файла (секрет ни
на мгновение не лежит с более широкими правами), атомарность, откат при любой
ошибке. При каждой загрузке `tighten_perms_if_needed` чинит `0644` на `0600`
с warn'ом: файл могли создать `echo >`, скопировать с другой машины или
достать из бэкапа.

В `user_settings.json` вебхука нет и не будет: тот файл читается фронтом и
уезжает в браузер. Там лежат только предпочтения панели —
`bitrix_hotkeys_enabled`, `bitrix_hotkeys`, `bitrix_filter_enabled`,
`bitrix_visible_kinds`, `bitrix_visible_peers`.

## Замороженные имена (контракт Р1)

| что | значение |
|---|---|
| каталог / крейт | `plugins/bitrix` / `forge-bitrix` |
| REST-префикс | `/api/bitrix/*` |
| WebSocket | `/ws/bitrix` |
| CLI-флаг | `--bitrix` |
| env | `DEVFORGE_BITRIX`, `FORGE_BITRIX_*` |
| конфиг | `~/.config/forge/bitrix.toml` (0600), ключ `webhook` |
| DOM-панель | `#bitrix` |
| DOM/CSS-префикс | `b24-` |
| JS-каталог | `static/js/bitrix/` |
| контракты во фронте | `window.ForgeApp.b24Keys` / `b24Filter` |

`bitrix`, а не `bitrix24`: цифра ничего не уточняет, зато удлиняет каждое имя
и провоцирует опечатки. Разнесение имени модуля (`bitrix`) и DOM-префикса
(`b24-`) не новое — у Telegram так же (`telegram` / `tg-`).

## Что панель умеет

**Подключение.** Вставка URL вебхука в трёх формах (полный адрес с именем
метода, без метода, с хвостовым слэшем и без), проверка `profile` → `scope` в
неизменном порядке, восемь состояний автомата
(`not_configured | checking | invalid_url | invalid_credentials |
scope_missing | throttled | disconnected | authorized`), отвязка одним
кликом. Подключение вебхука в РАБОТАЮЩЕМ процессе поднимает realtime без
рестарта devforge.

**Список.** Один список диалогов и четыре директории `~/all`, `~/chats`,
`~/tasks`, `~/unread` (Р2 вариант A). Источник — `im.recent.get` (он
единственный отдаёт чаты задач) плюс `im.recent.list` для дозагрузки при
>200 диалогов, оба на холодном старте одним `batch` вместе с
`im.counters.get` (Р3). Аватары, счётчики непрочитанного, присутствие,
закрепление (`im.recent.pin`) и скрытие (`im.recent.hide`).

**Лента.** Страница истории ≤50, прокрутка вверх с догрузкой (`LAST_ID`) и
вниз (`FIRST_ID`), нормализация порядка по возрастанию id, разделители дней,
системные строки отдельным стилем, BB-коды структурными сегментами (никакого
`innerHTML`), карточки `params.ATTACH` (MESSAGE/LINK/GRID), клавиатуры
`params.KEYBOARD` (`PUT`/`SEND` работают клиентски, `CALL`/`DIALOG`
дизейблятся с объяснением в `title`), stick-to-bottom защёлкой.

**Переписка.** Отправка с оптимистичной строкой и заменой её настоящей,
правка, удаление, «прочитано» (debounce 800 мс), ручная пометка
«непрочитано» (`MESSAGE_ID` обязателен), «печатает» (не чаще 8 с), лайк
(единственная реакция платформы), ответ (`REPLY_ID` + текстовая цитата, Р10),
счётчик символов и 400 на >20 000 (Р12), запрет ретрая отправки со строкой
«⟳ повторить» (Р9).

**Файлы.** Приём ⇪ / drag&drop / Ctrl+V, staging на диск
(`<cache_dir>/uploads/<uuid>.part`, TTL 30 мин, GC-воркер, уборка сирот на
старте), заливка одношагово через `im.v2.File.upload`, потолок 25 MiB из
конфига (Р7), двухфазный прогресс (Р8).

**Медиа.** Скачивание строго через `im.v2.File.download` → `downloadUrl` →
прокси плагина, файловый кэш с LRU-подчисткой, дедуп inflight, ленивая
загрузка через `IntersectionObserver`, лайтбокс с галереей, аватары через
`/api/bitrix/media/avatar/:user_id` (Р6).

**Задачи.** Директория `~/tasks`, карточка-шапка над лентой (статус,
приоритет, дедлайн, просрочка, постановщик, исполнитель, ссылка «открыть в
Битриксе»), матрица действий `action` строго по правам портала, два
независимых семейства счётчиков, ветка «задача без чата» (Р20).

**Realtime.** `im.v2.Event.subscribe` + цикл `get{offset}` с ack, дедуп по
кольцевому буферу на 512 `message.id` (Р19), персист курсора, правило шести
часов (Р18), адаптивная каденция 2/4/20 с, фолбэк-синхронизация раз в 60 с
(правки, счётчики, присутствие, изменения списка), кадр `resync` при
`Lagged`.

**Обвязка UX.** 23 хоткея по `event.code` с уступкой vim по фокусу,
белый список диалогов в четырёх точках применения, вкладка настроек, три
режима темы с собственными токенами на `#bitrix`, off-canvas колонка при
≤768 px, восстановление вкладки в PWA, `_debugState()` для e2e.

## Чего платформа не даёт (§6 плана)

Кадры протокола объявлены, но сервером не шлются; UI деградирует честно, а
`GET /api/bitrix/status` перечисляет отсутствующие возможности в поле
`capabilities` — фронт по нему прячет или блокирует элементы, а не показывает
ошибки.

| чего нет | почему | как ведёт себя панель |
|---|---|---|
| входящее «печатает» | отправка есть, входящих событий не существует | индикатор не показывается никогда |
| realtime онлайн/оффлайн | присутствие приезжает только `recent_sync` | точка присутствия обновляется раз в 60 с |
| realtime-правки | `ONIMV2MESSAGEUPDATE` документирован, но живьём не приходит | правка видна с задержкой до 60 с, через фолбэк |
| эмодзи-реакции | у вебхука есть только `im.message.like` | вместо набора эмодзи — счётчик «♥ N» |
| `reply_to` при чтении | платформа не отражает `REPLY_ID` в ответе | `reply_to` всегда `null`, цитата дублируется текстом (Р10) |
| пересылка сообщений | метода нет | кнопки нет |
| realtime «прочитано собеседником» | только `readed_list` по запросу | галочек «прочитано» в ленте нет |
| папки/фолдеры | у портала их нет | `folder_id` всегда `null` (поле ради формы) |
| капча | вебхуку не предъявляется | состояний и роутов нет вовсе |
| волны голосовых | портал не отдаёт `waveform` | плеер без волны |
| inline-кнопки `CALL`/`DIALOG` | требуют scope `imbot` | `disabled` + объяснение в `title` |
| `user.get` | нет scope | имена только через `im.user.*` и поглощение из ответов |
| `task.commentitem.getlist` | legacy-форум мёртв, возвращает `[]` | комментарии читаются лентой чата задачи |

## Что добавляет к возможностям VK/Telegram

Задачи как сущность: чат задачи у Bitrix24 — это ОБЫЧНЫЙ IM-чат с
`entity_type="TASKS_TASK"`, а комментарии задачи и сообщения чата — один и тот
же поток. Поэтому «Задачи» здесь не вторая вкладка, а директория `~/tasks`
одного списка диалогов плюс обогащённая шапка-карточка над лентой
(`#b24-task-card`). Плюс: матрица действий по правам портала, статусы и
просрочка, системные сообщения ленты с BB-сегментами, карточки
`ATTACH.BLOCKS`, три независимых счётчика (`DIALOG`/`CHAT`/`TASKS_TASK`) и
уведомления (`NOTIFY`), «кто прочитал», права и ограничения чата
(`restrictions`/`permissions`/`text_field_enabled` — поле ввода блокируется с
внятной причиной), пин и скрытие диалога, серверный поиск, ручная пометка
«непрочитано».

## Ограничение мультипользовательности (Р15)

Вебхук выпускается от имени ОДНОГО сотрудника портала. Мультипортальности и
мультипользовательности в v1 нет: в remote-mode все, кто открыл этот
devforge, увидят переписку владельца вебхука. Предупреждение об этом входит в
текст подсказки экрана подключения (`routes::WEBHOOK_HINT`), который
приходит с бэкенда, а не зашит в разметку.

## Инвариант: СЕКРЕТ НЕ ПОКИДАЕТ ПРОЦЕСС

Секрет — ВЕСЬ URL вебхука целиком: `user_id` криптографически связан с кодом,
поэтому «спрятать только код» бессмысленно. Инвариант держится не
дисциплиной, а типами и тестами:

1. `config::WebhookUrl` **не реализует `Serialize`** — DTO, WS-кадр или
   структурный лог с секретом физически не соберутся;
2. `Debug` печатает `<redacted>`, читать содержимое можно только через
   `expose()` — grep по `expose()` показывает ВСЕ такие места;
3. `WebhookUrl::fingerprint()` (`portal#uid…хвост`) допустим только в логах
   сервера;
4. `e.without_url()` вызывается на КАЖДОЙ транспортной ошибке; в лог уходят
   только имя метода и код;
5. `downloadUrl` из `im.v2.File.download` содержит код вебхука и НИКОГДА не
   отдаётся браузеру — байты идут прокси-роутом; тест
   `download_urls_never_leak_to_the_client` проверяет, что ни `downloadUrl`,
   ни `urlDownload`, ни `unifiedLink` не встречаются ни в одном DTO;
6. `GET /api/bitrix/status` не содержит ни кода, ни отпечатка, ни хоста
   портала ни в одном из восьми состояний автомата — тест
   `test_status_never_leaks_the_webhook_in_any_state` читает СЫРОЕ тело
   ответа, поэтому утечка через новое поле краснеет автоматически;
7. `no_source_line_logs_the_exposed_url` — построчный grep по `src/`;
8. аватары и вложения адресуются путями прокси (Р6), внешних URL в DOM
   панели нет вовсе.

Проверено живьём в Ф12: прогон с `RUST_LOG=trace` и грепом по коду вебхука и
по `downloadUrl` даёт ноль вхождений (см. «Живая приёмка»).

## Транспорт

Весь выход в сеть идёт через `api::B24Client` — ОДИН экземпляр на секрет, то
есть один троттлер (`B24State::api_client()` кэширует его, ключуя самим
вебхуком). Подробности — в записях `bitrix-b24client`, `bitrix-b24error`,
`bitrix-throttle`, `bitrix-batch`, `bitrix-b24stub`.

Коротко то, что определяет поведение всей вкладки:

- **Вызов**: `POST <portal>/rest/<uid>/<code>/<method>.json`,
  `Content-Type: application/json`. Вложенные структуры (`ATTACH`, `KEYBOARD`,
  `fields`, `filter`, `select`) передаются ТОЛЬКО JSON-телом — в query они
  ломаются.
- **Два независимых лимита.** Интенсивность (портал+IP, 2 req/s, ёмкость 50 —
  наше ведро 2 rps / 5) и `operating`-время НА МЕТОД (окно 10 минут, ~420–480 с,
  превышение блокирует только этот метод до `operating_reset_at`). Заголовков
  `X-RateLimit-*` и `Retry-After` портал не шлёт: единственный источник — блок
  `time` каждого успешного ответа.
- **Пустое тело при HTTP 200 — это троттлинг**, а не битый ответ: retryable
  `Transport` → `503 not_connected`, а не `Decode`. Живьём превышение
  интенсивности выглядит как ЗАВИСАНИЕ или обрыв TCP без JSON-ошибки — это
  подтвердилось и на приёмке Ф12 (см. ниже).
- **Ретрай максимум один**, строго по таблице `retry_for`. `im.message.add` и
  `im.v2.File.upload` не ретраятся НИКОГДА (Р9) — у них нет дедуп-идентификатора,
  повтор создаст дубль. Запрет живёт в транспорте и не обходится вложением в
  `batch`.
- **`batch`** стоит один хит лимита интенсивности, не расходует `operating` и
  режется по 50 автоматически: 51-я команда молча не выполняется, общей ошибки
  портал не даёт.
- **Таймауты обязательны** (`connect_timeout_secs`, `request_timeout_secs`):
  без них зависший вызов не породил бы ошибки, которую можно повторить.
- **`base_url_override`** (правило Р17): в проде обязателен `https`;
  `http://127.0.0.1` разрешён только при непустом `FORGE_BITRIX_CONFIG_PATH`.
  Ради него и существует локальный стенд вместо wiremock.

Таблица «ошибка портала → HTTP-контракт панели» живёт в `bitrix-b24error` и
воспроизведена в `src/api/error.rs`.

## Десять ловушек разбора

Каждая роняла бы десериализацию целиком. Подробности и тесты — на странице
`bitrix-serde-traps`, тестовый файл `plugins/bitrix/tests/serde_traps.rs`
(13 тестов на живых фикстурах портала).

1. PHP-массивы: пустое значение приезжает `[]`, заполненное — `{}`.
2. Полиморфный `recent[].id`: строка у групп, ЧИСЛО у личных.
3. `files[].image`: `false` ИЛИ объект.
4. «Шатающиеся» поля пользователя: `birthday` — `""` или `false`,
   `last_activity_date` — datetime или `false`, `work_position` — `null` или
   строка.
5. Payload событий camelCase, методы snake_case; в `message` продублированы
   обе нотации.
6. `role` в разном регистре: `im.recent.get` даёт `"OWNER"`,
   `im.dialog.get` — `"owner"`.
7. `im.recent.list` отдаёт `total: -1` — пользоваться им нельзя.
8. `LIMIT` не гарантирует ровно N строк.
9. `im.recent.list` НЕ отдаёт чаты задач — нужны оба источника списка.
10. Сообщение молча режется на 20 006 символов, а `URL_PREVIEW:"Y"`
    переписывает текст.

Плюс одиннадцатая, найденная по ходу: список лайков (`params.LIKE`) приезжает
то строками, то числами.

## HTTP- и WS-поверхность (замороженный контракт)

| метод | путь | что делает |
|---|---|---|
| GET | `/api/bitrix/status` | состояние плагина и подключения; 200 всегда, секрета нет |
| GET | `/ws/bitrix` | поток кадров (heartbeat 15 с, idle 60 с, подписка ДО снапшота) |
| POST | `/api/bitrix/login/webhook` | принять URL вебхука (тело `{"webhook": "..."}`) |
| POST | `/api/bitrix/logout` | стереть вебхук ЛОКАЛЬНО (в портал не ходим) |
| GET | `/api/bitrix/filters` | директории `all` / `chats` / `tasks` / `unread` |
| GET | `/api/bitrix/dialogs` | список диалогов (`?limit&offset&filter`) |
| GET | `/api/bitrix/dialogs/:id/messages` | страница истории (`?limit&before_id&after_id`) |
| GET | `/api/bitrix/messages/:mid` | одно сообщение (`?dialog_id`) |
| POST | `/api/bitrix/dialogs/:id/messages` | отправить сообщение (+ `upload_id` для файла) |
| PATCH | `/api/bitrix/messages/:mid` | правка |
| DELETE | `/api/bitrix/messages/:mid` | удаление |
| POST | `/api/bitrix/dialogs/:id/read` | отметить прочитанным |
| POST | `/api/bitrix/dialogs/:id/unread` | пометить непрочитанным (`message_id` обязателен) |
| POST | `/api/bitrix/dialogs/:id/typing` | «печатает…» |
| POST | `/api/bitrix/messages/:mid/like` | переключить лайк |
| POST | `/api/bitrix/dialogs/:id/pin` | закрепить/открепить |
| POST | `/api/bitrix/dialogs/:id/hide` | скрыть диалог |
| GET | `/api/bitrix/tasks` | список задач (`?filter&start&responsible&group`) |
| GET | `/api/bitrix/tasks/counters` | счётчики задач + непрочитанное чатов задач |
| GET | `/api/bitrix/tasks/:id` | карточка задачи (БЕЗ `select`) |
| POST | `/api/bitrix/tasks/:id/:action` | действие по матрице `action` |
| GET | `/api/bitrix/media/file/:dialog/:mid/:index` | байты вложения через прокси |
| GET | `/api/bitrix/media/:dialog/:mid/:index` | синоним (форма соседних панелей) |
| GET | `/api/bitrix/media/avatar/:user_id` | аватар через прокси |
| POST | `/api/bitrix/upload` | приём файла (raw body + `X-Forge-File-Name`) |

Порядок проверок во ВСЕХ data-роутах: авторизация (409) → соединение (503) →
резолв объекта (404) → разбор параметров (400). Query разбирается ВРУЧНУЮ
внутри хендлера, тело — через `Option<Json<T>>`: экстракторы axum отработали
бы ДО тела хендлера и отдали 400 раньше проверки авторизации, позволив
неподключённому клиенту прощупывать валидность идентификаторов портала.

## Отношение к соседним плагинам

Устроен по образцу `forge-vk` и `forge-telegram`: те же четыре точки входа,
тот же порядок регистрации в `main.rs`, тот же контракт ошибок и тот же общий
фронтовый слой `static/js/messenger/*` (ws-core, theme-core, http-envelope,
follow-core, hotkeys-core) — он ПЕРЕИСПОЛЬЗУЕТСЯ, а не копируется.

Отдельного крейта `bitrix-host-api` НЕТ: плагину от хоста нужен только
`Router`. Пустой слой ради симметрии разделял бы нечего.

Чужие плагины не редактируются ни в одной фазе. Изоляция стилей проверяется
тестом `tmux-web/tests/frontend/bitrix_contract.test.mjs`: в `css/bitrix.css`
не должно быть ни одного селектора `.tg-`, `.vk-`, `#telegram`, `#vk`.

Акцент палитры ОРАНЖЕВЫЙ (`#FF9F43`), а не брендовый синий Битрикс24:
брендовый оттенок лежит ровно в зоне VK-акцента. Три неразличимые панели
опасны функционально — сообщение уйдёт не в тот чат. Контраст и различимость
акцентов проверяет `bitrix_theme_contrast.test.mjs`.

## Состав крейта (финальный)

| файл | назначение |
|---|---|
| `src/lib.rs` | четыре точки входа + реэкспорты |
| `src/config.rs` | `BitrixConfig` + секрет-newtype `WebhookUrl`, файл 0600 |
| `src/state.rs` | `B24State`, автомат `LoginFlow`, `ConnHealth`, курсор, uploads, реестр воркеров |
| `src/login.rs` | автомат подключения, `trait LoginBackend`, `LoginStateDto` |
| `src/api/mod.rs` | `B24Client`: URL, таймауты, один ретрай, Р9 |
| `src/api/error.rs` | конверт, таблица §4, `retry_for`, `without_url` |
| `src/api/throttle.rs` | оба лимита портала |
| `src/api/batch.rs` | пачка ≤50, urlencode, демультиплексор |
| `src/api/methods.rs` | типизированные обёртки методов портала |
| `src/api/schema.rs` | сырые `Deserialize`-структуры без `Serialize` |
| `src/bbcode.rs` | разбор BB-кодов → plain + структурные сегменты (Р11) |
| `src/dto.rs` | проводные DTO и чистая проекция raw→DTO через `trait PeerNames` |
| `src/attach.rs` | `params.FILE_ID` ↔ `files[]`, `params.ATTACH`, `params.KEYBOARD` |
| `src/peers.rs` | кэш пользователей с TTL и потолком, добор через `batch` |
| `src/tasks.rs` | `TaskDto`, статусы, приоритеты, матрица `action`, индекс `task_id ↔ chat_id` |
| `src/actions.rs` | троттлинг действий панели (read/typing) |
| `src/events.rs` | realtime: `subscribe` → цикл `get{offset}` с ack, дедуп, backoff, персист |
| `src/recent_sync.rs` | фолбэк-поллер `im.recent.get{LAST_SYNC_DATE}` + `im.counters.get` |
| `src/media_cache.rs` | файловый кэш байтов + LRU-sweeper |
| `src/progress.rs` | троттлинг прогресса (min-interval + min-delta, финал всегда) |
| `src/routes/mod.rs` | `build_router`, `GET /status`, замороженная таблица путей |
| `src/routes/error.rs` | конверт ошибок и таблица кодов |
| `src/routes/login.rs` | приём/проверка/сброс вебхука |
| `src/routes/dialogs.rs` | директории, список, история, отправка/правка/удаление, read/unread/typing/like/pin/hide |
| `src/routes/tasks.rs` | список, карточка, действия, счётчики |
| `src/routes/media.rs` | прокси байтов вложений и аватаров |
| `src/routes/upload.rs` | raw-upload, staging, GC |
| `src/ws/mod.rs` | `/ws/bitrix`: heartbeat, idle, Lagged → resync |
| `src/ws/protocol.rs` | кадры сервер→клиент + `pong` клиент→сервер |
| `src/testlog.rs` | `#[cfg(test)]` захват tracing — страж «секрет не в логе» |
| `tests/b24_stub.rs` | локальный стенд портала на axum |
| `tests/serde_traps.rs` | десять ловушек разбора на живых фикстурах |
| `tests/dialogs_ladder.rs` | лестница проверок 409 → 503 → 404 → 400 |
| `tests/media.rs`, `tests/media_leak.rs` | прокси, кэш, «downloadUrl не утекает» |
| `tests/tasks.rs`, `tests/tasks_traps.rs` | задачи и три ловушки платформы |
| `tests/upload.rs` | порядок проверок загрузки, staging, GC |

Фронт: `static/js/bitrix/` — 18 модулей (`api, attachments, botkb, chat,
dialogs, format, keynav, keys, login, main, media, mobile, state, tasks,
theme, upload, visibility, ws`), `static/css/bitrix.css` (1 731 строка),
14 node-тестов `tmux-web/tests/frontend/bitrix_*.test.mjs`, Playwright-спека
`e2e/tests/ui/bitrix-ui.spec.ts`.

## Точки интеграции в хост

`Cargo.toml` (members) → `plugins/bitrix/Cargo.toml` → `tmux-web/Cargo.toml`
(path-dep) → `cli.rs` (`--bitrix`, help, запрет для stop/status) →
`server_config.rs` (`FileConfig.bitrix`, `EffectiveConfig.bitrix_enabled`,
`ENV_BITRIX`) → `daemon.rs` (проброс флага) → `main.rs` (регистрация ДО
`.fallback` и `.layer(auth)`, shutdown после VK) → `auth.rs`
(`RAW_UPLOAD_PATHS += "/api/bitrix/upload"`) → `user_settings.rs` (пять
ключей панели) → `.arhit/config.json` (`plugins/bitrix/src` в `sourcePaths`).

Фронт: `index.html` (кнопка `#bitrix-toggle` + бейдж `#b24-unread-badge` +
панель `#bitrix`) → `css/bitrix.css` + `@import` в `style.css` →
`js/core/dom.js` (рефы `$b24*`) → `js/bitrix/*` → `js/core/bootstrap.js` →
`js/tabs/tabs.js` → `js/settings/{modal,bitrix-visibility-tab,hotkeys-tab}.js`
→ `css/settings-modal.css` → `hotkeys.js` (реестр `keyboardOwners` вместо
списка контрактов) → `js/pwa/bootstrap.js` → `sw.js`
(`CACHE_VERSION = 'forge-pwa-v10'`).

`/api/bitrix/*` в `DATA_ALLOWLIST` сервис-воркера НЕ добавлен намеренно:
переписка не кэшируется на диск браузера ни в каком виде. Для Битрикс24 это
правило строже вдвойне — там же лежат чаты задач, то есть внутренняя
переписка компании.

## Что осталось за рамками v1

- **Р13** — отдельная директория `// NOTIFY` для уведомлений портала. В
  счётчик статусбара уведомления входят (`im.counters.get.TYPE.NOTIFY`), но
  своей директории у них нет.
- **Р14** — управление чатами: создание, переименование, приглашение и
  исключение участников, выход из чата. Реализованы только `mute`, `pin`
  (`im.recent.pin`) и `hide` (`im.recent.hide`).
- **Р5** — мультипортальность: поле вебхука ОДНО, повторное сохранение
  перезаписывает его.
- **Р15** — мультипользовательность: вебхук = один сотрудник портала.
- Серверный поиск (`im.search.chat.list` / `im.search.user.list`) —
  инфраструктура есть, отдельного UI поиска в v1 нет.

## Живая приёмка (Ф12)

Изолированный прогон `HOME=$(mktemp -d) devforge run --bitrix --port 7420`
против реального портала `test-work-2.bitrix24.ru`, весь сценарий подряд БЕЗ
перезапуска инстанса, на СВЕЖЕСОЗДАННЫХ сущностях (Р22 — чат 33 с 60 строками
истории и задача 7 с чатом 35): подключение вебхука → список чатов и задач →
директории → открытие диалога → прокрутка истории вверх → отправка →
правка → удаление → отправка файла → скачивание вложения → лайк → смена
статуса задачи → приход сообщения по realtime без перезагрузки → тост и
бейдж при неактивной вкладке → бейдж от серверных счётчиков → белый список →
хоткеи → аудит DOM → logout. **21 шаг из 21 зелёный.**

Гейт того же дня: `cargo test --workspace` — 2334 теста, три прогона подряд
без единого падения; `cargo clippy --workspace --all-targets` — НОЛЬ
предупреждений в `plugins/bitrix` (13 предупреждений во всём воркспейсе, все
в файлах, которых проект не касался); `cargo tree -i` по `openssl-sys`,
`openssl`, `native-tls`, `libsql` — «did not match any packages»; в `Cargo.lock`
за весь проект добавился РОВНО ОДИН пакет (`forge-bitrix`), внешних
зависимостей ноль, поэтому и новых дублей в `cargo tree -d` быть не может;
61 фронт-тест на Node и 13 сценариев `bitrix-ui.spec.ts` — зелёные;
`git diff` по `plugins/vk`, `plugins/telegram`, `static/js/vk`,
`static/js/telegram` — пуст.

Замеренные величины: задержка realtime от отправки в портал до строки в
ленте — 0,5–4 с; догрузка страницы истории по прокрутке вверх — мгновенная;
смена статуса задачи отражается в карточке и добавляет системное сообщение в
ленту чата задачи.

Подтверждено живьём и то, что описано в §4 плана как теория: при превышении
лимита интенсивности портал НЕ отвечает JSON-ошибкой, а зависает или рвёт
TCP. На приёмке это ловилось и клиентом плагина (`elapsed_ms=10003`,
`error_code="transport"`, один повтор, состояние `disconnected` с кнопкой
«повторить»), и внешним `curl` (exit 28). Именно ради этого класса отказов в
конфиге обязателен `connect_timeout_secs`.

Аудит секрета: полный прогон под `RUST_LOG=trace`, грепы по коду вебхука, по
`downloadUrl`/`urlDownload`/`unifiedLink`, по хосту портала в DOM панели — ноль
вхождений везде, кроме одной строки `INFO` с отпечатком
(`test-work-2.bitrix24.ru#1…3d23j4`), который и является штатным способом
опознать вебхук в логе.