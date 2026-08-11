# prompts_path

plugins/vk/src/config.rs — путь файла пер-чатовых заметок плагина VK.

default_prompts_path() = $HOME/.forge/vk-chat-prompts.json (рядом с кэшем медиа, а не рядом с vk.toml: это пользовательские данные вкладки, а не настройки, и переносить их вместе с файлом, где лежит секрет, незачем). prompts_path() — непустой FORGE_VK_PROMPTS_PATH побеждает, иначе дефолт.

VkConfig.prompts_path объявлен как #[serde(skip, default = "prompts_path")]: из TOML не читается (путь к файлу внутри самого файла сбивал бы с толку), хранится ОТДЕЛЬНЫМ полем, а не вызовом prompts_path() в момент записи — std::env process-global, и тест, подменивший переменную, гонялся бы с соседним, который в этот момент сохраняет заметку.

БЕЗ этого override любой прогон cargo test, дошедший до сохранения, затирал бы живой ~/.forge/vk-chat-prompts.json разработчика — ровно та же ошибка, что уже чинили через FORGE_VK_CONFIG_PATH.

apply_env применяет FORGE_VK_PROMPTS_PATH (пустая строка игнорируется). validate_and_fix нормализует значение: expand_tilde, пустое → дефолт с warn, относительное → абсолютное относительно текущего каталога (иначе devforge из репозитория и из systemd писал бы РАЗНЫЕ файлы).
