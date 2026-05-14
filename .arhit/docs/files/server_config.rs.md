# server_config.rs

Модуль server_config.rs — конфигурация сервера devforge (Phase 1).

## Назначение
Сводит параметры запуска (CLI флаги, env-переменные, ~/.config/forge/server_config.json) в одну эффективную конфигурацию. Также отвечает за:
- Авто-генерацию 64-hex Bearer-токена при --remote без явного токена.
- Atomic-save server_config.json при auto-gen.
- Печать big-warning банеров (auto-token + public-bind без TLS).

## Структуры
- ServerConfig { auth_token, bind, port } — JSON-форма файла.
- EffectiveConfig { bind, port, auth_token, remote_mode } — итог resolve().

## Ключевые функции
- default_server_config_path() → ~/.config/forge/server_config.json (через cli::state_dir()).
- load() / load_from(path) → Result<Option<ServerConfig>> — атомарная загрузка. None если файла нет.
- save(cfg) / save_to(path, cfg) — atomic tempfile+rename.
- resolve(run_opts, file_cfg) → EffectiveConfig — приоритет: CLI > файл > env > defaults. remote_mode: bool из run_opts.remote.
- is_localhost_bind(bind) → bool — true для 127.x.x.x, ::1, localhost. Используется для классификации public-bind в print_public_bind_warning.
- generate_token_64hex() → String — два uuid::Uuid::new_v4 в hex (128 бит). Безопасный CSPRNG.
- finalize_token(eff) → Option<String> — авто-генерит токен при remote_mode без явного auth_token. Сохраняет в server_config.json (с merge'ом существующих полей). Возвращает None при remote_mode=false.
- print_auto_token_banner(token, bind, port) — приватная, печатает банер с авто-сгенерённым токеном на старте.
- print_public_bind_warning(bind, port, token) (Phase 7) — печатает большой warning при remote_mode + non-loopback bind. Показывает truncated token (8…4 hex) для диагностики, без полного значения; на None токен — печатает 'UNSAFE'. На localhost-bind — no-op.

## Бизнес-логика
- finalize_token идемпотентен: если auth_token уже задан в effective — возвращает его как есть.
- Auto-gen всегда обновляет файл (merge с существующими bind/port).
- print_public_bind_warning вызывается из main.rs после bind и до axum::serve, ВСЕГДА в remote_mode на не-loopback bind (даже если токен передан через CLI).

## Unit-тесты (10+ шт.)
- resolve_priority_cli_over_file_over_env.
- finalize_token_noop_when_non_remote.
- finalize_token_preserves_existing.
- finalize_token_autogen_writes_file.
- finalize_token_autogen_merges_existing_file.
- is_localhost_bind_recognises_loopback (Phase 7).
- is_localhost_bind_rejects_public (Phase 7).
- save/load roundtrip, atomic-write-on-temp.

## Зависит от
- cli::state_dir() — путь к каталогу конфига.
- uuid — генерация токена.
- anyhow, serde, serde_json — стандартный стек.
