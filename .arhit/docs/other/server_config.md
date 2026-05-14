# server_config

Конфигурация сервера (tmux-web/src/server_config.rs).

## Файл ~/.config/forge/server_config.json
Формат: {auth_token?, bind?, port?}. Все поля опциональны. Отсутствующий файл → Ok(None).

## API
- load() / load_from(&Path) → Result<Option<ServerConfig>>. None при отсутствии файла.
- save() / save_to(&Path, &ServerConfig) → atomic write tmp + rename.
- resolve(&RunOptions, Option<&ServerConfig>) → EffectiveConfig. Priority: CLI > file > env > default.
- finalize_token(&EffectiveConfig) / finalize_token_at — авто-генерирует 64-hex Bearer при remote_mode=true без token; сохраняет файл.
- is_localhost_bind(&str) → bool. True для 127.x, ::1, localhost. False для 0.0.0.0, fe80::1 (link-local), :: (unspecified), 192.168.*, [::1] (bracket-форма не распознаётся — известный case).
- print_public_bind_warning() — banner про HTTP-without-TLS на public bind'е.
- print_auto_token_banner() — banner про auto-generated token.

## Контракт (закреплён тестами Phase 8 .6)
- Невалидный JSON → Err с file path в message.
- 0-байтный файл → Err (JSON требует хотя бы {}).
- {} → Default-ServerConfig (все поля None).
- Лишние поля игнорируются (serde без deny_unknown_fields).
- save_to на read-only dir → Err не панику.
- is_localhost_bind:
  - '::1' — true (loopback)
  - '[::1]' — false (bracket-форма не поддерживается)
  - 'fe80::1' — false (link-local публичен)
  - '::' — false (unspecified = публичен)
- resolve в legacy-mode (remote=false) принудительно понижает bind до 127.0.0.1, даже если CLI/файл указали 0.0.0.0 — defense-in-depth.

## Auto-remote
ServerConfig::implies_remote() = true если auth_token.is_some() || bind.is_some(). resolve() активирует remote_mode при таком файле даже без --remote в CLI.

## Тесты
30 unit-тестов в server_config::tests (Phase 1 + Phase 8 .6).
