# forge-telegram — вкладка мессенджера на личном аккаунте (обзор фичи)

Плагин devforge, дающий вкладку ✈ с перепиской по MTProto от ЛИЧНОГО аккаунта
Telegram (не bot-API). Реализован за шесть фаз; эта страница — карта фичи
целиком. Детали по частям: `telegram-routes`, `telegram-login-routes`,
`telegram-login-wizard`, `telegram-phase3-realtime`, `telegram-phase4-media`,
`telegram-media-lifecycle`, `telegram-upload-routes`, `telegram-send-media`,
`telegram-upload-frontend`, `telegram-presence-and-read`,
`telegram-phase5-media-send`.

## Opt-in: без флага ничего не меняется

`devforge run --telegram` | `--telegram` у `start` | `DEVFORGE_TELEGRAM=1` |
`server_config.json {"telegram": true}`. Приоритет: CLI > file > env >
default(false).

Без флага `register_routes` не вызывается: `/api/telegram/*` и `/ws/telegram`
дают 404, фронт держит кнопку ✈ скрытой, НИ ОДИН воркер не стартует.
Инвариант закреплён тестом `test_routes_absent_without_the_plugin`,
e2e-сценарием и проверен вручную (в логе инстанса без флага ноль строк
`forge_telegram`).

## Почему НЕТ crate telegram-host-api

У Echo есть `echo-host-api` — trait `HostApi` и DTO, чтобы плагин не зависел от
`AppState` девфорджа. Telegram'у от хоста не нужно НИЧЕГО: ни сессий tmux, ни
задач, ни путей проектов. Он получает только `Router` для монтирования роутов и
живёт на собственном `TelegramState`. Заводить пустой boundary ради симметрии
значило бы поддерживать слой, который ничего не разделяет.

Единственная точка, где хост знает о плагине предметно, — исключение
`is_allowed_raw_body` в `tmux-web/src/auth.rs::csrf_guard` для ТОЧНОГО пути
`/api/telegram/upload` с `application/octet-stream` (same-origin остаётся
обязательным).

## Состав

plugins/telegram/src/
- `config.rs`   — TelegramConfig: defaults -> ~/.config/forge/telegram.toml ->
                  env FORGE_TELEGRAM_* -> validate;
- `state.rs`    — TelegramState: broadcast, login Mutex, runtime RwLock,
                  uploads/peers/unread/media_inflight, workers +
                  CancellationToken, authorized_notify, тайминги WS;
- `session.rs`  — FileSession: своё файловое хранилище сессии + flock;
- `login.rs`    — LoginFlow (5 состояний) + LoginStateDto, токены наружу не
                  отдаются никогда;
- `runtime.rs`  — TgRuntime: подключение MTProto;
- `peers.rs`    — трёхэшелонный резолв peer'ов (кэш -> session -> скан диалогов);
- `dto.rs`      — DialogDto/MessageDto/MediaDto, все 64-битные id строками;
- `media_cache.rs`, `progress.rs`, `updates.rs`;
- `routes/`     — mod, login, dialogs, upload, media, error;
- `ws/`         — protocol.rs (типы кадров), mod.rs (цикл соединения).

tmux-web/static/js/telegram/ — state, api, ws, main, login, dialogs, chat,
media, upload, format, theme, mobile. Стили — css/telegram.css.

## Контракт ошибок REST

Единая обёртка `{"error":{"code","message","retry_after_secs?"}}`. Таблица
кодов и статусов — на странице `plugins/telegram/src/routes/error.rs`.
Фронт принимает решения по `code`, а не по статусу (409 бывает и
`not_authorized`, и `invalid_state`); переименование кода валит тест
`full_error_contract_table`.

## Таблица WS-событий (/ws/telegram)

сервер -> клиент: `login_state`, `new_message`, `message_edited`,
`message_deleted`, `typing`, `online`, `read`, `upload_progress`,
`download_progress`, `resync`, `ping`.
клиент -> сервер: `pong` (единственный).

Внешний тег `type` в snake_case. Broadcast БЕЗ фильтрации по диалогу: бейдж и
тосты обязаны работать с любой вкладки. Полнота набора закреплена тестом с
исчерпывающим `match` — новый вариант ломает сборку тестов.

## Схема кэша медиа

`<cache_dir>/media/<dialog_id>/<message_id>.<kind>.{bin,meta.json}`.
Атомарная запись: staging + rename, причём МЕТА пишется ПОСЛЕ байтов и служит
признаком целостности (мета без bin -> запись считается неполной и в кэш не
попадает). `CacheKey::parse` — единственный путь строк из URL в файловую
систему (защита от traversal). LRU-подчистка с 90% гистерезисом, staging TTL
1 час, `protected` из `media_inflight`.

## Двухэтапная отправка файла

`POST /upload` только кладёт тело в staging и отдаёт `upload_id`; заливка в
Telegram (`upload_stream`) происходит внутри `POST /dialogs/:id/messages`.
Причина: фронт узнаёт `upload_id` ТОЛЬКО из ответа `/upload`, поэтому события
`upload_progress`, отправленные раньше ответа, не с чем сопоставить — прогресс
LOCAL -> TG не работал бы. Побочная выгода: невыбранный к отправке файл вообще
не уходит в сеть.

## Тема панели и шрифт

Панель НЕ наследует токены devforge — у неё своя палитра и свой тумблер ◐
(`js/telegram/theme.js`, режимы auto/dark/light, ключ localStorage
`forge.telegramTheme`). В режиме `auto` светлота выводится из яркости
CSS-переменной `--bg` приложения: глобального dark/light-флага в devforge нет.

Шрифт JetBrains Mono ЗАВЕНДОРЕН (`static/vendor/fonts/jetbrains-mono/`,
Regular 92 КБ + Bold 95 КБ + OFL.txt, лицензия SIL OFL 1.1). Решение Фазы 6:
терминальная сетка дизайна держится на метриках именно этого шрифта, а CDN
запрещён — devforge обязан работать офлайн и не ходить в сеть за ассетами,
тем более со страницы с перепиской. `src` начинается с `local(...)`, поэтому
у кого шрифт установлен, тот ничего не скачивает.

## Принятые риски и ограничения

1. **Размер бинаря.** Фича добавляет ~8.3 МиБ к release-сборке (18.4 -> 27.1 МБ).
   Основная часть — `grammers-tl-types` (сгенерированный код всей TL-схемы),
   187 КиБ из них — завендоренный шрифт. Ожидалось меньше; второго SQLite при
   этом НЕТ (см. следующий пункт).
2. **Хранилище сессии.** Штатный `sqlite-storage`/libsql у grammers ПАНИКУЕТ в
   devforge (конфликт символов sqlite3_* с rusqlite, который уже линкует Echo),
   поэтому написан свой `session.rs::FileSession`. Побочный эффект — в графе
   зависимостей нет ни libsql, ни второго SQLite.
3. **Одна сессия — один процесс.** На `<session_path>.lock` берётся flock;
   второй экземпляр devforge с тем же путём честно сообщает об этом и уходит в
   `disconnected`, вместо того чтобы испортить сессию.
4. **FLOOD_WAIT никогда не ретраится автоматически.** На логине блокировка
   измеряется часами, повторы её продлевают. UI показывает обратный отсчёт.
5. **Токен логина живёт только в памяти** (`LoginFlow`), наружу не отдаётся ни
   в DTO, ни в `Debug` (редактируется). Креды пишутся в
   `~/.config/forge/telegram.toml` атомарно и с правами 0600.
6. **Инвалидация сессии.** `AUTH_KEY_UNREGISTERED` из любого состояния переводит
   в `disconnected` с причиной и рассылает `login_state` всем вкладкам.
7. **Гейт чистого крипто.** `cargo tree -i openssl-sys` (а также openssl,
   native-tls, libsql) обязан быть ПУСТ: MTProto у grammers — чистое
   Rust-крипто, появление openssl означало бы транзитивную протечку.

## Проверки

`cargo test --workspace` (в т.ч. 225 тестов плагина и 14 telegram_smoke),
Node-тесты чистой логики фронта (telegram_format/media/upload),
`e2e/tests/ui/telegram-ui.spec.ts`, `cargo tree -i openssl-sys` (пусто),
`cargo tree -d` (дублей grammers нет).