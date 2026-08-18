//! `GET` / `PUT /api/meet/config` — настройки плагина из интерфейса.
//!
//! # Что здесь можно, а чего нельзя
//!
//! Записывается ТОЛЬКО то, что перечислено в [`crate::config::EDITABLE`], —
//! белый список, а не «всё, кроме опасного». Причина в шапке самого списка и
//! стоит того, чтобы повторить: в конфиге пять путей к исполняемым файлам, и
//! роут, умеющий записать произвольный ключ, — это выполнение произвольного
//! бинаря одним `PUT`.
//!
//! # Источник каждого значения виден
//!
//! `default` / `file` / `env`. Различие не косметическое: значение из
//! переменной окружения файлом НЕ ПЕРЕЗАПИСЫВАЕТСЯ (иначе «сохранил в
//! интерфейсе, а работает старое»), поэтому такое поле приезжает с
//! `env_locked: true`, и UI рисует его только для чтения.
//!
//! # НОВЫЕ ЗНАЧЕНИЯ БЕРУТСЯ НА СЛЕДУЮЩЕМ СТАРТЕ. Это не недоделка
//!
//! [`MeetState::config`] неизменяем и раздан по всему плагину как
//! `state.config.поле` — от таймаутов автовыхода в сеансе записи до пути
//! модели в пайплайне. Сделать его горячим значило бы завести замок вокруг
//! сотни мест чтения; в награду получилась бы запись, у которой таймаут
//! автовыхода поменялся посреди встречи, и расшифровка, начатая одной моделью
//! и продолженная другой.
//!
//! Поэтому `PUT` честно сохраняет файл и отвечает `restart_required: true`.
//! Идущие записи он НЕ ТРОГАЕТ — ни одной, ни на секунду; на это есть тест,
//! сравнивающий `state.config` до и после запроса.
//!
//! # Файл правится, а не пересобирается
//!
//! Через `toml_edit`: комментарии, порядок ключей и форматирование остаются на
//! месте. Сборка файла заново из `toml::Table` стёрла бы объяснения, которые
//! человек писал руками, и заметил бы он это в лучшем случае через месяц.
//!
//! Битый TOML — единственный случай, когда `PUT` отказывается работать: молча
//! перезаписать неразбираемый файл значило бы выбросить все остальные
//! настройки пользователя разом.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::config::{self, EditableField, MeetConfig};
use crate::error::MeetError;
use crate::state::MeetState;

/// Откуда пришло текущее значение поля.
pub const SOURCE_DEFAULT: &str = "default";
pub const SOURCE_FILE: &str = "file";
pub const SOURCE_ENV: &str = "env";

/// Одно поле настроек глазами интерфейса.
#[derive(Debug, Clone, Serialize)]
pub struct FieldDto {
    #[serde(flatten)]
    pub field: &'static EditableField,
    /// Действующее значение (после файла, env и `validate_and_fix`).
    pub value: serde_json::Value,
    /// `default` | `file` | `env`.
    pub source: &'static str,
    /// Задано переменной окружения: файл его не перебьёт, UI показывает
    /// только для чтения.
    pub env_locked: bool,
}

/// Значения, которые видны, но не редактируются.
#[derive(Debug, Clone, Serialize)]
pub struct ReadonlyDto {
    pub cache_dir: String,
    pub db_path: String,
    pub models_dir: String,
    /// Всегда 1 — Metal занимается процессом целиком.
    pub max_concurrent_whisper: usize,
    pub audio_sample_rate: u32,
    pub audio_codec: &'static str,
    pub audio_transport: &'static str,
}

/// Постоянный профиль браузера глазами интерфейса.
///
/// Отдаётся ВСЕГДА, даже когда `persist_profile` выключен: каталог мог
/// остаться от прежней настройки, и «выключил галку — а сессия Google так и
/// лежит на диске» было бы ровно тем молчаливым расхождением, от которого
/// плагин уходит везде. Пока каталог есть, у человека есть и кнопка сброса.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileDto {
    /// Включён ли постоянный профиль прямо сейчас.
    pub persist: bool,
    pub path: String,
    /// Каталог существует на диске.
    pub exists: bool,
    /// Права каталога восьмеричной строкой (`700`). Пусто — не Unix либо
    /// каталога нет. UI показывает это рядом с предупреждением о живой сессии.
    pub mode: String,
    /// Предупреждение о том, ЧТО именно лежит в каталоге. Текст здесь, а не во
    /// фронте, чтобы он не разъехался с фактическим поведением.
    pub warning: &'static str,
}

/// Тело `GET /api/meet/config`.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigDto {
    pub config_path: String,
    /// Существует ли файл. `false` — плагин ни разу не настраивали, и это
    /// норма.
    pub file_exists: bool,
    /// Разбирается ли файл. `false` — `PUT` откажется его трогать.
    pub file_parses: bool,
    pub fields: Vec<FieldDto>,
    pub readonly: ReadonlyDto,
    pub profile: ProfileDto,
    /// Ни одно значение не применяется на лету — см. шапку модуля. Поле
    /// существует затем, чтобы UI не выдумывал эту фразу сам.
    pub restart_required: bool,
}

/// Ключи, ЯВНО присутствующие в файле (для различения `default` и `file`).
fn file_keys(path: &std::path::Path) -> (bool, bool, std::collections::HashSet<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (false, true, std::collections::HashSet::new());
    };
    match raw.parse::<toml::Table>() {
        Ok(t) => (true, true, t.keys().cloned().collect()),
        Err(_) => (true, false, std::collections::HashSet::new()),
    }
}

/// Собирает ответ по действующему конфигу.
pub fn describe(cfg: &MeetConfig) -> ConfigDto {
    let (file_exists, file_parses, present) = file_keys(&cfg.config_path);
    let fields = config::EDITABLE
        .iter()
        .map(|f| {
            let env_locked = config::env_locked(f.key);
            let source = if env_locked {
                SOURCE_ENV
            } else if present.contains(f.key) {
                SOURCE_FILE
            } else {
                SOURCE_DEFAULT
            };
            FieldDto {
                field: f,
                value: config::editable_value(cfg, f.key).unwrap_or(serde_json::Value::Null),
                source,
                env_locked,
            }
        })
        .collect();

    ConfigDto {
        config_path: cfg.config_path.display().to_string(),
        file_exists,
        file_parses,
        fields,
        readonly: ReadonlyDto {
            cache_dir: cfg.cache_dir.display().to_string(),
            db_path: cfg.db_path.display().to_string(),
            models_dir: cfg.models_dir.display().to_string(),
            max_concurrent_whisper: cfg.max_concurrent_whisper,
            audio_sample_rate: cfg.audio_sample_rate,
            audio_codec: cfg.audio_codec.as_str(),
            audio_transport: cfg.audio_transport.as_str(),
        },
        profile: describe_profile(cfg),
        restart_required: true,
    }
}

/// Предупреждение о содержимом постоянного профиля.
///
/// Формулировка прямая НАМЕРЕННО: в каталоге лежит действующая сессия
/// Google-аккаунта, и человек, включающий настройку, обязан понимать это без
/// чтения документации.
pub const PROFILE_WARNING: &str =
    "В каталоге профиля сохраняется живая сессия аккаунта, под которым бот входит во встречи. \
     Кто получит доступ к каталогу — получит и доступ к аккаунту. Сброс удаляет каталог целиком.";

/// Состояние постоянного профиля браузера.
pub fn describe_profile(cfg: &MeetConfig) -> ProfileDto {
    let path = crate::bot::persistent_profile_dir(&cfg.cache_dir);
    let meta = std::fs::metadata(&path).ok();
    ProfileDto {
        persist: cfg.persist_profile,
        exists: meta.as_ref().is_some_and(std::fs::Metadata::is_dir),
        mode: meta.as_ref().map(dir_mode).unwrap_or_default(),
        path: path.display().to_string(),
        warning: PROFILE_WARNING,
    }
}

#[cfg(unix)]
fn dir_mode(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:o}", meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn dir_mode(_meta: &std::fs::Metadata) -> String {
    String::new()
}

/// `GET /api/meet/config`.
pub async fn get_config(State(state): State<Arc<MeetState>>) -> Json<ConfigDto> {
    Json(describe(&state.config))
}

/// Ответ `DELETE /api/meet/profile`.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileResetResponse {
    /// Каталог существовал и был удалён.
    pub removed: bool,
    pub profile: ProfileDto,
}

/// `DELETE /api/meet/profile` — сбросить постоянный профиль браузера.
///
/// # Почему это отдельный роут, а не поле в `PUT /config`
///
/// Потому что удаление живой сессии аккаунта — ДЕЙСТВИЕ, а не настройка.
/// Спрятанное в общий `PUT`, оно случайно уезжало бы вместе с правкой
/// соседнего поля, а «я поменял имя бота и разлогинился» — худший вид
/// сюрприза.
///
/// # Идущие записи
///
/// Удалять профиль, пока им пользуется живой Chrome, — верный способ получить
/// половину сессии на диске и половину в памяти. Поэтому при непустом списке
/// сеансов роут отказывает, а не «удаляет по возможности».
pub async fn delete_profile(
    State(state): State<Arc<MeetState>>,
) -> Result<Json<ProfileResetResponse>, MeetError> {
    if !state.sessions.is_empty().await {
        // `Busy` — тот же 409, что у занятых слотов записи, и та же причина:
        // ресурс занят прямо сейчас, повторите позже.
        return Err(MeetError::Busy);
    }
    let path = crate::bot::persistent_profile_dir(&state.config.cache_dir);
    let existed = path.is_dir();
    if existed {
        std::fs::remove_dir_all(&path)
            .map_err(|e| MeetError::Internal(format!("профиль не удалился: {e}")))?;
        tracing::info!(
            target: "forge_meet",
            path = %path.display(),
            "forge-meet: постоянный профиль браузера сброшен"
        );
    }
    Ok(Json(ProfileResetResponse {
        removed: existed,
        profile: describe_profile(&state.config),
    }))
}

/// Тело `PUT /api/meet/config`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct PutRequest {
    /// `{"values": {"bot_name": "…"}}`.
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// Ответ `PUT /api/meet/config`.
#[derive(Debug, Clone, Serialize)]
pub struct PutResponse {
    /// Ключи, реально записанные в файл.
    pub saved: Vec<String>,
    /// Ключи, пропущенные из-за переменной окружения.
    pub env_locked: Vec<String>,
    /// Что поправил [`MeetConfig::validate_and_fix`] — теми же словами, что
    /// уходят в лог при старте.
    pub fixes: Vec<String>,
    /// Всегда `true`, пока конфиг в состоянии неизменяем (см. шапку модуля).
    pub restart_required: bool,
    /// Новое состояние настроек — чтобы UI не делал второй запрос.
    pub config: ConfigDto,
}

/// Разбирает тело ВРУЧНУЮ — см. «Порядок проверок» в шапке [`crate::routes`].
///
/// Неизвестный ключ — ОШИБКА, а не тихий пропуск: «сохранил, а не
/// применилось» ищут часами, и отличить это от «поле не редактируется» иначе
/// невозможно.
pub fn parse_put(body: &[u8]) -> Result<PutRequest, MeetError> {
    let req: PutRequest = serde_json::from_slice(body)
        .map_err(|e| MeetError::BadRequest(format!("тело не разбирается: {e}")))?;
    if req.values.is_empty() {
        return Err(MeetError::BadRequest(
            "нужен непустой объект values".to_string(),
        ));
    }
    for key in req.values.keys() {
        if config::editable(key).is_none() {
            return Err(MeetError::BadRequest(format!(
                "поле {key:?} не редактируется из интерфейса"
            )));
        }
    }
    Ok(req)
}

/// `PUT /api/meet/config` — сохранить настройки.
pub async fn put_config(
    State(state): State<Arc<MeetState>>,
    body: Bytes,
) -> Result<Json<PutResponse>, MeetError> {
    let req = parse_put(&body)?;
    let path = state.config.config_path.clone();

    // Файл на диске — источник истины для правки: конфиг в состоянии уже
    // перекрыт env, и сохранять его обратно значило бы вписать в файл то, чего
    // человек туда не клал.
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(MeetError::Internal(format!(
                "конфиг {} не читается: {e}",
                path.display()
            )))
        }
    };
    let mut doc = raw.parse::<toml_edit::DocumentMut>().map_err(|e| {
        MeetError::BadRequest(format!(
            "файл {} не разбирается ({e}); почините его руками — перезапись стёрла бы \
             остальные настройки",
            path.display()
        ))
    })?;

    // Проверяем ВСЮ пачку на конфиге из файла (без env): типы, перечисления и
    // границы. Ни один ключ не пишется, пока не проверены все — иначе
    // половина настроек сохранилась бы, а половина нет.
    let mut candidate = MeetConfig::from_file(&path);
    let mut saved = Vec::new();
    let mut env_locked = Vec::new();
    for (key, value) in &req.values {
        if config::env_locked(key) {
            env_locked.push(key.clone());
            continue;
        }
        candidate
            .apply_editable(key, value)
            .map_err(MeetError::BadRequest)?;
        saved.push(key.clone());
    }
    let fixes = candidate.validate_and_fix();

    for key in &saved {
        // В файл уходит ПОЧИНЕННОЕ значение, а не присланное: иначе на диске
        // осталось бы `alone_timeout_secs = 0`, при каждом старте молча
        // превращалось бы в 5, и человек читал бы в файле одно, а видел
        // другое.
        let fixed = config::editable_value(&candidate, key)
            .ok_or_else(|| MeetError::Internal(format!("поле {key} не читается обратно")))?;
        doc[key.as_str()] = json_to_toml(&fixed)?;
    }

    if !saved.is_empty() {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    MeetError::Internal(format!("каталог конфига не создался: {e}"))
                })?;
            }
        }
        // Атомарно: недописанного `meet.toml` на диске не бывает. Тот же
        // `forge_atomic`, что у транскрипта и манифеста.
        forge_atomic::write_bytes_atomic(&path, doc.to_string().as_bytes())
            .map_err(|e| MeetError::Internal(format!("конфиг не сохранился: {e}")))?;
    }

    tracing::info!(
        target: "forge_meet",
        keys = ?saved,
        env_locked = ?env_locked,
        path = %path.display(),
        "forge-meet: настройки сохранены, применятся на следующем старте"
    );

    Ok(Json(PutResponse {
        saved,
        env_locked,
        fixes,
        restart_required: true,
        // Описание собирается по КОНФИГУ СОСТОЯНИЯ, а не по кандидату:
        // действующие значения не изменились, и врать про это нельзя. Разницу
        // «в файле уже новое, в работе ещё старое» показывает
        // `restart_required`.
        config: describe(&state.config),
    }))
}

/// JSON → TOML. Ограничен теми типами, которые бывают у редактируемых полей.
fn json_to_toml(v: &serde_json::Value) -> Result<toml_edit::Item, MeetError> {
    use toml_edit::{value, Item};
    let item: Item = match v {
        serde_json::Value::String(s) => value(s.as_str()),
        serde_json::Value::Bool(b) => value(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                value(i)
            } else if let Some(f) = n.as_f64() {
                value(f)
            } else {
                return Err(MeetError::Internal(format!("число {n} не представимо в TOML")));
            }
        }
        other => {
            return Err(MeetError::Internal(format!(
                "значение {other} не представимо в TOML"
            )))
        }
    };
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_parsed_by_hand_and_names_unknown_keys() {
        let ok = parse_put(r#"{"values":{"bot_name":"Секретарь"}}"#.as_bytes()).unwrap();
        assert_eq!(ok.values.len(), 1);

        // Пустой объект — ошибка: «сохранить ничего» это опечатка клиента.
        assert!(matches!(
            parse_put(br#"{"values":{}}"#).unwrap_err(),
            MeetError::BadRequest(_)
        ));
        assert!(matches!(
            parse_put("не json".as_bytes()).unwrap_err(),
            MeetError::BadRequest(_)
        ));

        // Ключ вне белого списка НАЗЫВАЕТ СЕБЯ.
        let err = parse_put(br#"{"values":{"claude_cli_path":"/bin/sh"}}"#).unwrap_err();
        assert!(
            format!("{err}").contains("claude_cli_path"),
            "ошибка обязана называть поле: {err}"
        );
    }

    #[test]
    fn json_values_become_toml_values() {
        assert_eq!(
            json_to_toml(&serde_json::json!("x")).unwrap().to_string().trim(),
            "\"x\""
        );
        assert_eq!(
            json_to_toml(&serde_json::json!(true)).unwrap().to_string().trim(),
            "true"
        );
        assert_eq!(
            json_to_toml(&serde_json::json!(7)).unwrap().to_string().trim(),
            "7"
        );
        assert!(json_to_toml(&serde_json::json!([1, 2])).is_err());
        assert!(json_to_toml(&serde_json::json!(null)).is_err());
    }

    #[test]
    fn describe_marks_where_every_value_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, "bot_name = \"Из файла\"\n").unwrap();
        let cfg = MeetConfig {
            config_path: path,
            ..crate::testing::hermetic_config(dir.path())
        };
        let cfg = MeetConfig {
            bot_name: "Из файла".into(),
            ..cfg
        };

        let dto = describe(&cfg);
        assert!(dto.file_exists && dto.file_parses);
        assert!(dto.restart_required, "конфиг в состоянии неизменяем");
        let by_key = |k: &str| dto.fields.iter().find(|f| f.field.key == k).unwrap().clone();
        assert_eq!(by_key("bot_name").source, SOURCE_FILE);
        assert_eq!(by_key("bot_name").value, serde_json::json!("Из файла"));
        assert_eq!(by_key("retention_days").source, SOURCE_DEFAULT);
        assert_eq!(dto.fields.len(), config::EDITABLE.len());
    }

    #[test]
    fn a_broken_file_is_reported_not_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.toml");
        std::fs::write(&path, "= = =").unwrap();
        let cfg = MeetConfig {
            config_path: path,
            ..crate::testing::hermetic_config(dir.path())
        };
        let dto = describe(&cfg);
        assert!(dto.file_exists);
        assert!(!dto.file_parses, "битый файл обязан быть виден в ответе");
    }
}
