# BitrixRootConfig

plugins/bitrix/src/config.rs — разобранный bitrix.toml ЦЕЛИКОМ: общие настройки + список порталов.

Зачем отдельный тип: в файле два уровня. Корневые ключи — настройки МАШИНЫ (poll_active_secs, poll_idle_secs, recent_sync_secs, max_upload_bytes, max_cache_bytes, rate_limit_rps, burst, таймауты, cache_dir, base_url_override, claude_cli_path, analysis_*), а [[portals]] — список подключений. Разводить их по типам нужно, чтобы общие настройки не дублировались в каждой записи портала.

Поля:
- shared: BitrixConfig — общие настройки; поле shared.webhook здесь означает legacy-ключ верхнего уровня (или значение FORGE_BITRIX_WEBHOOK).
- portals: Vec<PortalConfig> — порталы в порядке файла, с уже присвоенными уникальными id.
- migrated_from_legacy: bool — список синтезирован из верхнеуровневого webhook.

Методы:
- load() — config_path() → load_from_file → apply_env → validate_and_fix. Непустой FORGE_BITRIX_WEBHOOK ПОБЕЖДАЕТ файл целиком и даёт ровно один портал (стенд и живые порталы не смешиваются).
- load_from_file(path) — tighten_perms_if_needed → чтение → ОДИН разбор в toml::Table → типизированный BitrixConfig для общих настроек + portals_from_table для [[portals]]. Ни env, ни validate_and_fix здесь не применяются.
- portal_configs() -> Vec<BitrixConfig> — общие настройки КОПИЕЙ в каждый пер-портальный BitrixConfig плюс webhook/portal_id/portal_title портала. Копия, а не ссылка: Ф6 разводит cache_dir по подкаталогам, и пер-портальной структуре нужно своё значение поля.
- primary() -> BitrixConfig — конфигурация первого портала; порталов нет ⇒ общие настройки с ПУСТЫМ вебхуком (not_configured). На ней держится совместимость: BitrixConfig::load() = BitrixRootConfig::load().primary(), поэтому сигнатура B24State::new(cfg) и весь код под ней о мультипортальности не знают.
- portal_ids() -> Vec<String> — идентификаторы, секрета не содержат.

Legacy: файл без [[portals]], но с верхнеуровневым webhook, читается как ОДИН портал. Чтение неразрушающее — файл переписывается только при первой ЗАПИСИ (save_portals_to снимает верхнеуровневый ключ).
