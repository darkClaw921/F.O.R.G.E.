# tmux-web/src/server_config.rs

Загрузка/сохранение ~/.config/forge/server_config.json и резолвинг эффективной конфигурации сервера для devforge.

## ServerConfig
Файл JSON: { auth_token: Option<String>, bind: Option<String>, port: Option<u16> }. Все поля опциональны.

## resolve(cli_opts, file_cfg) -> EffectiveConfig

Источники в порядке приоритета: CLI > server_config.json > env > default.

**Изменено (Phase 'mobile UX'):** remote_mode определяется ТОЛЬКО CLI флагом --remote. Раньше файл мог авто-включать remote_mode если в нём были auth_token/bind — это сбивало с толку: после одного --remote запуска следующий 'cargo run' без флагов автоматически поднимался на 0.0.0.0 + bearer auth. Сейчас:
- cargo run (без флагов) → 127.0.0.1, remote_mode=false, auth=false.
- cargo run --remote → 0.0.0.0, remote_mode=true, токен из файла (или auto-gen).

Метод ServerConfig::implies_remote() удалён (был только для auto-remote).

## Финальные значения
- port: cli > file > DEFAULT_PORT (7331).
- bind: CLI > file (только при remote_mode); в legacy localhost-mode ВСЕГДА 127.0.0.1.
- auth_token: CLI > file (только при remote_mode); legacy → None.

## EffectiveConfig
{ bind: String, port: u16, auth_token: Option<String>, remote_mode: bool }.

## Зависимости
- anyhow, serde, serde_json для load/save.
- cli::state_dir для пути к файлу.
