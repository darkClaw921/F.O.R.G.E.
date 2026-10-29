# Битрикс24: мультипортальный конфиг (Фаза 1)

Файл plugins/bitrix/src/config.rs описывает N подключений массивом таблиц:

    poll_active_secs = 4            # общие настройки — один раз на машину
    cache_dir = "~/.forge/bitrix"

    [[portals]]
    id = "acme"                     # выведен из ХОСТА, секрета не содержит
    title = "Acme"                  # необязательно
    webhook = "https://acme.bitrix24.ru/rest/1/.../"

    [[portals]]
    id = "beta"
    webhook = "https://beta.bitrix24.ru/rest/9/.../"

## Два уровня

Корневые ключи — настройки МАШИНЫ; [[portals]] — список подключений. BitrixRootConfig::portal_configs() раздаёт общие настройки КОПИЕЙ в пер-портальные BitrixConfig, поэтому BitrixConfig остаётся PER-PORTAL, а сигнатура B24State::new(cfg) и весь код под ней в этой фазе не менялись. BitrixConfig::load() = BitrixRootConfig::load().primary().

Новые поля BitrixConfig: portal_id и portal_title, оба #[serde(skip)] — из ВЕРХНЕГО уровня TOML не читаются, приходят из записи [[portals]]. Рядом с прежними serde(skip)-полями config_path, prompts_path, user_settings_path; герметичность тестов через FORGE_BITRIX_CONFIG_PATH и FORGE_BITRIX_PROMPTS_PATH сохранена.

## Секрет

WebhookUrl остаётся newtype БЕЗ Serialize; читать содержимое можно только через expose(). Секрет — весь URL вебхука целиком. Единственные два места, где expose() превращает его в сериализуемое значение: save_portals_to (запись [[portals]]) и api_base(). Debug печатает <redacted>, в логи уходит только fingerprint.

## Идентификатор портала

Выводится из ХОСТА (portal_id_from_webhook), пути вебхука не содержит, поэтому его можно отдавать во фронт и писать в логи. Коллизии — суффиксом -2/-3, занятые id не переименовываются. Детерминирован: на него завязаны миграции дисковых данных (Ф6) и настроек (Ф7).

## Совместимость и миграция

Верхнеуровневый ключ webhook остаётся ЧИТАЕМЫМ: файл без [[portals]] даёт ровно один портал (migrated_from_legacy = true), поведение плагина не меняется. Чтение неразрушающее — файл байт в байт остаётся прежним. Миграция происходит при первой ЗАПИСИ: save_portals_to снимает верхнеуровневый ключ и пишет массив, поэтому секрет не остаётся в файле в двух экземплярах. Идемпотентна.

## Запись

save_portals_to — единственный путь секрета на диск, четыре инварианта (read-modify-write, 0600 в момент создания, tmp+fsync+rename, удаление tmp при любой ошибке) плюс tighten_perms_if_needed на каждой загрузке. Обёртки: save_webhook_to (однопортальная замена, используется старым POST /api/bitrix/login/webhook), add_portal_to (для POST /api/bitrix/portals, Ф4.2), remove_portal_from (для Ф2.3/Ф4.3), clear_webhook_to (logout — снимает и legacy-ключ, и массив).

## Что осталось следующим фазам

Реестр порталов и жизненный цикл — Ф2 (registry.rs); экстрактор Portal и перевод хендлеров — Ф3; HTTP CRUD порталов — Ф4.