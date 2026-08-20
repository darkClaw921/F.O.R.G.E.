//! HTTP-поверхность плагина встреч.
//!
//! Сборка идёт в [`build_router`], который вызывается из
//! [`crate::register_routes`] ДО `.fallback(...)` и `.layer(auth)` хоста:
//! только так Bearer-auth и `csrf_guard` автоматически покрывают роуты плагина
//! в remote-mode. Регистрация после `.fallback` дала бы 404, после `.layer` —
//! дыру в авторизации.
//!
//! # ЗАМОРОЖЕННЫЙ КОНТРАКТ ИМЁН
//!
//! Таблица ниже — источник истины для `tmux-web/static/js/meet/api.js`. Обе
//! стороны обязаны совпадать дословно: расхождение даёт молчаливый 404, а не
//! ошибку сборки. Перечислены СРАЗУ все пути всех фаз, чтобы имена не
//! расползлись, когда роуты начнут добавляться по одному.
//!
//! | метод | путь | фаза | что делает |
//! |---|---|---|---|
//! | GET    | `/api/meet/status`                        | **Ф2** | состояние плагина; **200 ВСЕГДА**, 404 = плагин выключен |
//! | GET    | `/api/meet/badges`                        | **Ф2** | батч состояний для ленты (`?plugin&chat_id&min_id&max_id`) |
//! | GET    | `/ws/meet`                                | **Ф2** | поток событий (см. [`crate::ws::protocol`]) |
//! | POST   | `/api/meet/meetings`                      | Ф3 | старт записи (`mode: normal\|assisted`) → `{meeting_id, attached}` |
//! | POST   | `/api/meet/meetings/:id/stop`             | Ф3 | «встреча кончилась, обработай» — файлы сохраняются |
//! | POST   | `/api/meet/meetings/:id/cancel`           | Ф3 | «прерви и выброси» |
//! | POST   | `/api/meet/meetings/:id/confirm`          | Ф3 | «я внутри — пиши» (ассистированный вход) |
//! | POST   | `/api/meet/meetings/:id/show-window`      | Ф3 | показать окно бота человеку |
//! | GET    | `/api/meet/meetings/:id/screenshot`       | Ф3 | «что видит бот» — PNG |
//! | GET    | `/api/meet/meetings/:id`                  | **Ф6** | карточка встречи |
//! | GET    | `/api/meet/meetings/:id/transcript`       | **Ф6** | расшифровка (`?offset&limit`) |
//! | PATCH  | `/api/meet/meetings/:id/speakers`         | **Ф7** | ручное переименование участника |
//! | GET    | `/api/meet/meetings/:id/summary`          | **Ф6** | саммари (`{status, text}`; отсутствие текста — не 404) |
//! | POST   | `/api/meet/meetings/:id/summary`          | **Ф6** | пересчитать саммари = `retry?from=summarizing` |
//! | GET    | `/api/meet/meetings/:id/audio`            | **Ф7** | архивный микс; **честный Range**, 206/416/HEAD |
//! | POST   | `/api/meet/meetings/:id/retry`            | **Ф6** | повтор этапа (`?from=`) |
//! | GET    | `/api/meet/models`                        | Ф5 | список моделей whisper |
//! | POST   | `/api/meet/models/:name/download`         | Ф5 | скачать модель — **только по явному действию** |
//! | DELETE | `/api/meet/models/:name`                  | Ф5 | удалить модель с диска |
//! | GET    | `/api/meet/cloud/models`                  | **Ф16** | каталог облачных моделей (`stt`/`chat`) с ценами; ключ наружу не уходит |
//! | GET    | `/api/meet/cloud/balance`                 | **Ф16** | остаток средств в рублях и лимиты ключа |
//! | GET    | `/api/meet/config`                        | **Ф7** | настройки плагина |
//! | PUT    | `/api/meet/config`                        | **Ф7** | сохранить настройки |
//! | DELETE | `/api/meet/profile`                       | **Ф8** | сбросить постоянный профиль браузера (живая сессия аккаунта) |
//! | POST   | `/api/meet/deps/recheck`                  | Ф5 | перепроверить внешние бинари |
//! | — | `/ws/meet/ingest` | — | **НЕ СОЗДАЁТСЯ НИКОГДА** (см. ниже). Строка стоит в таблице намеренно: отсутствие роута — такое же решение, как и его наличие, и без записи его однажды «добавят обратно» |
//!
//! Прочерк в колонке метода — не опечатка: по нему строку отличает и человек,
//! и разбор таблицы в `meet_contract.test.mjs` (он собирает роуты только по
//! строкам с настоящим методом, а наличие ИМЕННО ЭТОЙ строки проверяет
//! отдельно).
//!
//! ## `/ws/meet/ingest` НЕ СОЗДАЁТСЯ. НИКОГДА.
//!
//! Звук идёт через CDP-биндинг (`Runtime.addBinding` →
//! `window.__forge_<rand32>` → `Runtime.bindingCalled`), а НЕ через наш
//! HTTP-сервер. Между инжектированным скриптом и сервером стоят четыре
//! барьера: `auth::csrf_guard` отвергает апгрейд `/ws/*` с чужим `Origin`,
//! дальше Bearer-auth, жёсткий `connect-src` в CSP у Meet и Private Network
//! Access для запроса из публичного origin в loopback. Биндинг убирает все
//! четыре разом, потому что убирает сеть.
//!
//! **Вариант «исключение в `csrf_guard` по одноразовому токену» не делать
//! никогда.** Токен физически лежит в памяти страницы, которую контролирует
//! третья сторона, — а мы ей ещё и CSP отключили ради инжекта. Скрипт сайта
//! прочитает токен и откроет WS к главному серверу, где живёт `/ws/attach`, то
//! есть терминал. Это превращает плагин записи встреч в drive-by RCE.
//!
//! # Порядок проверок ошибок — [`check_ladder`]
//!
//! ```text
//! готовность бинарей (409 *_unavailable)
//!   → слот (409 busy)
//!     → резолв встречи (404)
//!       → допустимость перехода (409 invalid_state)
//!         → разбор параметров (400)
//! ```
//!
//! Query разбирается **вручную внутри хендлера**, а не экстрактором `Query`:
//! экстрактор отработал бы ДО тела хендлера и отдал 400 раньше, чем мы успели
//! сказать «whisper не установлен» — и пользователь чинил бы не то.

pub mod audio;
pub mod cloud;
pub mod config;
pub mod meetings;
pub mod models;
pub mod speakers;

use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::db::repo::MeetingRow;
use crate::deps::DepsReport;
use crate::error::{ErrorBody, MeetError};
use crate::providers::{Provider, PROVIDERS};
use crate::state::MeetState;
use crate::status::{can_transition, MeetStatus};
use crate::ws::protocol::{BadgeEntry, Limits};

impl IntoResponse for MeetError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(ErrorBody::from(&self))).into_response()
    }
}

/// Собирает роутер плагина.
pub fn build_router(state: Arc<MeetState>) -> Router {
    Router::new()
        .route("/api/meet/status", get(status))
        .route("/api/meet/badges", get(badges))
        .route("/api/meet/meetings", post(meetings::start))
        .route("/api/meet/meetings/:id/stop", post(meetings::stop))
        .route("/api/meet/meetings/:id/cancel", post(meetings::cancel))
        .route("/api/meet/meetings/:id/confirm", post(meetings::confirm))
        .route("/api/meet/meetings/:id/show-window", post(meetings::show_window))
        .route("/api/meet/meetings/:id/screenshot", get(meetings::screenshot))
        .route("/api/meet/meetings/:id", get(meetings::get))
        .route("/api/meet/meetings/:id/transcript", get(meetings::transcript))
        .route(
            "/api/meet/meetings/:id/summary",
            get(meetings::summary).post(meetings::regenerate_summary),
        )
        .route(
            "/api/meet/meetings/:id/speakers",
            axum::routing::patch(speakers::patch),
        )
        // `?track=N` — параметр, а не отдельный роут: таблица выше и
        // `MEET_ROUTES` во фронте сверяются ДОСЛОВНО, и второй адрес означал бы
        // расхождение двух таблиц ради выборки файла.
        .route("/api/meet/meetings/:id/audio", get(audio::audio))
        .route("/api/meet/meetings/:id/retry", post(meetings::retry))
        .route("/api/meet/models", get(models::list))
        .route("/api/meet/models/:name/download", post(models::download))
        .route("/api/meet/models/:name", axum::routing::delete(models::remove))
        .route("/api/meet/deps/recheck", post(models::recheck))
        // Каталог и баланс — ОТДЕЛЬНЫЕ роуты, а не поля `status`: это сетевые
        // вызовы в чужой сервис, а `status` гейтит панель плагина целиком.
        .route("/api/meet/cloud/models", get(cloud::models))
        .route("/api/meet/cloud/balance", get(cloud::balance))
        .route(
            "/api/meet/config",
            get(config::get_config).put(config::put_config),
        )
        .route(
            "/api/meet/profile",
            axum::routing::delete(config::delete_profile),
        )
        .route("/ws/meet", get(crate::ws::meet_ws))
        .with_state(state)
}

// --- GET /api/meet/status ----------------------------------------------------

/// Настройки, которые плагин показывает о себе.
#[derive(Debug, Clone, Serialize)]
pub struct StatusConfigDto {
    pub bot_name: String,
    pub window_mode: &'static str,
    pub whisper_model: String,
    pub whisper_language: String,
    pub audio_codec: &'static str,
    pub audio_transport: &'static str,
    pub audio_sample_rate: u32,
    pub summary_enabled: bool,
    pub cache_dir: String,
    /// Где считается расшифровка: `cloud` | `local`.
    pub stt_backend: &'static str,
    /// Где считается саммари: `cloud` | `local`.
    pub summary_backend: &'static str,
}

/// Облако глазами интерфейса.
///
/// **Ключа здесь нет ни в каком виде** — только маска; на это есть отдельный
/// тест приватности (`tests/key_privacy.rs`).
#[derive(Debug, Clone, Serialize)]
pub struct CloudStatusDto {
    /// Настроено ли облако вообще (есть ключ).
    pub configured: bool,
    /// Маска ключа (`pza_RIcg••••••••Uzu4js`). Пусто — ключа нет.
    pub key_mask: String,
    /// Ключ задан переменной окружения: файлом его не перебить, и UI не
    /// предлагает править.
    pub env_locked: bool,
    pub base_url: String,
    pub stt_model: String,
    pub summary_model: String,
    /// Живость ключа и остаток лимита — из БЕСПЛАТНОГО `GET /key`.
    ///
    /// `None` означает ровно одно из двух: ключа нет (тогда `configured:
    /// false`) либо облако не ответило (тогда непусто поле `error`).
    ///
    /// Поле `limit_reset` внутри осталось СТРОКОЙ: у живого ключа там
    /// `monthly`, а не `daily`, как было записано в плане.
    pub key: Option<crate::cloud::service::KeyInfo>,
    /// Почему облако не ответило: машинный код плюс текст для человека.
    ///
    /// **ЭТО ПОЛЕ, А НЕ 500.** `status` — единственный роут, который никогда
    /// не падает; недоступный polza.ai обязан быть строчкой в ответе, а не
    /// отказом, который выключит панель плагина целиком.
    pub error: Option<CloudErrorDto>,
    /// Сколько секунд назад спрашивали. `None` — не спрашивали ни разу.
    pub checked_secs_ago: Option<u64>,
}

/// Отказ облака в ответе статуса.
#[derive(Debug, Clone, Serialize)]
pub struct CloudErrorDto {
    pub code: &'static str,
    pub message: String,
}

/// Тело `GET /api/meet/status`.
#[derive(Debug, Clone, Serialize)]
pub struct StatusDto {
    /// Всегда `true`. Признак «плагин выключен» — это 404 на самом роуте, а не
    /// поле: без флага `--meet` роутов нет вовсе, и фронт узнаёт об этом
    /// единственным способом, который работает при выключенном плагине.
    pub enabled: bool,
    /// Открыта ли база. `false` в короткое окно между `init` и `spawn_workers`.
    pub db_ready: bool,
    pub deps: DepsReport,
    pub limits: Limits,
    /// **Реестр платформ с регулярками ссылок.**
    ///
    /// Отдаётся с бэкенда, а не хранится константой во фронте, потому что
    /// разметку браузер держит в кэше PWA неделями: фронтовая константа
    /// протухла бы незаметно, а серверная приезжает с каждым ответом.
    pub providers: &'static [Provider],
    /// Встречи, занимающие слот записи прямо сейчас.
    pub active: Vec<MeetingRow>,
    pub config: StatusConfigDto,
    /// Состояние облака. Отдаётся ВСЕГДА, даже когда обе стадии локальные:
    /// «ключ настроен, но не используется» человек обязан видеть, иначе
    /// переключатель в настройках выглядел бы сломанным.
    pub cloud: CloudStatusDto,
}

/// `GET /api/meet/status` — **единственный роут, который никогда не падает.**
///
/// 200 отдаётся даже когда нет ни одного внешнего бинаря, нет базы и нет
/// моделей: именно этим ответом пользователь узнаёт, ЧЕГО не хватает. Роут,
/// который отказывает при неполном окружении, оставил бы человека без
/// единственного источника диагностики.
async fn status(State(state): State<Arc<MeetState>>) -> Json<StatusDto> {
    // Список активных встреч — приятная, но необязательная часть ответа.
    // Ошибка базы не имеет права превратить `status` в 500.
    let active = match crate::db::repo::meetings::list_by_status(&state, &MeetStatus::ACTIVE).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                error = %e,
                "forge-meet: активные встречи для status недоступны, отдаю пустой список"
            );
            Vec::new()
        }
    };

    Json(StatusDto {
        enabled: true,
        db_ready: state.db().is_some(),
        deps: state.deps_snapshot().await,
        limits: state.limits(),
        providers: &PROVIDERS,
        active,
        config: StatusConfigDto {
            bot_name: state.config.bot_name.clone(),
            window_mode: state.config.window_mode.as_str(),
            whisper_model: state.config.whisper_model.clone(),
            whisper_language: state.config.whisper_language.clone(),
            audio_codec: state.config.audio_codec.as_str(),
            audio_transport: state.config.audio_transport.as_str(),
            audio_sample_rate: state.config.audio_sample_rate,
            summary_enabled: state.config.summary_enabled,
            cache_dir: state.config.cache_dir.display().to_string(),
            stt_backend: state.config.stt_backend.as_str(),
            summary_backend: state.config.summary_backend.as_str(),
        },
        cloud: cloud_status(&state).await,
    })
}

/// Облачный блок ответа `status`.
///
/// Собирается по тем же правилам, что и список активных встреч выше: любая
/// неудача превращается в ПОЛЕ, а не в отказ роута. Сетевого вызова здесь
/// может не быть вовсе — [`MeetState::cloud_probe`] кэширует ответ на минуту и
/// молчит, когда ключа нет.
async fn cloud_status(state: &Arc<MeetState>) -> CloudStatusDto {
    let cfg = &state.config;
    let probe = state.cloud_probe().await;
    CloudStatusDto {
        configured: cfg.cloud_configured(),
        // МАСКА, А НЕ КЛЮЧ.
        key_mask: cfg.polza_api_key.mask(),
        env_locked: crate::config::env_locked("polza_api_key"),
        base_url: cfg.cloud_base_url.clone(),
        stt_model: cfg.cloud_stt_model.clone(),
        summary_model: cfg.cloud_summary_model.clone(),
        key: probe.as_ref().and_then(|p| p.key.clone()),
        error: probe.as_ref().and_then(|p| {
            p.error.as_ref().map(|(code, message)| CloudErrorDto {
                code,
                message: message.clone(),
            })
        }),
        checked_secs_ago: probe.as_ref().map(|p| p.at.elapsed().as_secs()),
    }
}

// --- GET /api/meet/badges ----------------------------------------------------

/// Тело `GET /api/meet/badges`.
#[derive(Debug, Clone, Serialize)]
pub struct BadgesDto {
    pub plugin: String,
    pub chat_id: String,
    pub min_id: i64,
    pub max_id: i64,
    /// Только те сообщения, у которых встреча ЕСТЬ. Отсутствие записи означает
    /// «можно записать»: статуса `detected` в базе нет, иначе таблица росла бы
    /// от простого скролла ленты.
    pub badges: Vec<BadgeEntry>,
}

/// Разобранные параметры батча.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeQuery {
    pub plugin: String,
    pub chat_id: String,
    pub min_id: i64,
    pub max_id: i64,
}

/// Сколько сообщений максимум обслуживает один батч.
///
/// Страница ленты — 50 строк, тысяча даёт двадцатикратный запас и при этом не
/// даёт превратить запрос в выгрузку всей переписки.
pub const MAX_BADGE_RANGE: i64 = 1000;

/// Ручной разбор query. Не экстрактор — см. «Порядок проверок» в шапке модуля.
pub fn parse_badge_query(raw: Option<&str>) -> Result<BadgeQuery, MeetError> {
    let raw = raw.unwrap_or("");
    let mut plugin = None;
    let mut chat_id = None;
    let mut min_id = None;
    let mut max_id = None;
    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
        match k.as_ref() {
            "plugin" => plugin = Some(v.into_owned()),
            "chat_id" => chat_id = Some(v.into_owned()),
            "min_id" => min_id = Some(v.into_owned()),
            "max_id" => max_id = Some(v.into_owned()),
            // Незнакомые параметры игнорируются: новый фронт может дописать
            // версию клиента, а старый сервер не имеет права отвечать на это
            // ошибкой.
            _ => {}
        }
    }

    let bad = |m: &str| MeetError::BadRequest(m.to_string());
    let plugin = plugin.filter(|s| !s.is_empty()).ok_or_else(|| bad("нужен параметр plugin"))?;
    let chat_id = chat_id.filter(|s| !s.is_empty()).ok_or_else(|| bad("нужен параметр chat_id"))?;
    let parse_id = |name: &str, v: Option<String>| -> Result<i64, MeetError> {
        v.ok_or_else(|| MeetError::BadRequest(format!("нужен параметр {name}")))?
            .parse::<i64>()
            .map_err(|_| MeetError::BadRequest(format!("{name} должен быть целым числом")))
    };
    let min_id = parse_id("min_id", min_id)?;
    let max_id = parse_id("max_id", max_id)?;

    if max_id < min_id {
        return Err(bad("max_id меньше min_id"));
    }
    if max_id - min_id + 1 > MAX_BADGE_RANGE {
        return Err(MeetError::BadRequest(format!(
            "диапазон шире {MAX_BADGE_RANGE} сообщений"
        )));
    }
    Ok(BadgeQuery {
        plugin,
        chat_id,
        min_id,
        max_id,
    })
}

/// `GET /api/meet/badges?plugin=&chat_id=&min_id=&max_id=`.
///
/// **Обслуживается тёплым индексом в памяти, без единого запроса к SQLite.**
/// Батч дёргается на каждом рендере ленты; поход в базу означал бы запрос за
/// каждый скролл. На это есть тест: роут отвечает 200 у состояния, к которому
/// база вообще не привязана.
async fn badges(
    State(state): State<Arc<MeetState>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<BadgesDto>, MeetError> {
    let q = parse_badge_query(raw.as_deref())?;
    let found = state
        .badges_in_range(&q.plugin, &q.chat_id, q.min_id, q.max_id)
        .await;
    Ok(Json(BadgesDto {
        plugin: q.plugin,
        chat_id: q.chat_id,
        min_id: q.min_id,
        max_id: q.max_id,
        badges: found
            .into_iter()
            .map(|(key, badge)| BadgeEntry {
                chat_id: key.chat_id,
                message_id: key.message_id,
                badge,
            })
            .collect(),
    }))
}

// --- лестница ошибок ---------------------------------------------------------

/// Предусловия действия над встречей, в порядке проверки.
#[derive(Debug, Default)]
pub struct Preconditions<'a> {
    /// Код отсутствующей зависимости (`whisper_unavailable`) либо `None`.
    pub dep_blocker: Option<&'static str>,
    /// Заняты ли все слоты.
    pub busy: bool,
    /// Найденная встреча. `None` — резолв не удался.
    pub meeting: Option<&'a MeetingRow>,
    /// Запрошенный переход, если действие его требует.
    pub transition: Option<MeetStatus>,
    /// Итог ручного разбора параметров.
    pub params: Option<MeetError>,
}

/// Возвращает ПЕРВУЮ сработавшую ошибку в зафиксированном порядке.
///
/// Порядок — не стиль. Проверь мы параметры раньше зависимостей, пользователь
/// с неустановленным whisper получал бы 400 «плохой параметр» и чинил бы не
/// то. Проверь мы резолв встречи раньше слота — неподключённый клиент мог бы
/// прощупывать существование чужих записей.
pub fn check_ladder(p: Preconditions<'_>) -> Result<(), MeetError> {
    if let Some(code) = p.dep_blocker {
        return Err(MeetError::DepUnavailable(code));
    }
    if p.busy {
        return Err(MeetError::Busy);
    }
    let Some(meeting) = p.meeting else {
        return Err(MeetError::NotFound);
    };
    if let Some(to) = p.transition {
        if !can_transition(meeting.status, to) {
            return Err(MeetError::InvalidState {
                from: meeting.status.as_str(),
                to: to.as_str(),
            });
        }
    }
    if let Some(e) = p.params {
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::SourceKey;
    use crate::providers::Platform;
    use crate::status::SummaryStatus;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn state_without_db() -> Arc<MeetState> {
        // Каталоги во временной папке: тест не имеет права смотреть в
        // ~/.forge/meet, где лежат записи реальных встреч.
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::MeetConfig {
            config_path: dir.path().join("meet.toml"),
            cache_dir: dir.path().join("cache"),
            db_path: dir.path().join("cache/meet.db"),
            models_dir: dir.path().join("cache/models"),
            ..crate::config::MeetConfig::default()
        };
        std::mem::forget(dir); // каталог живёт до конца процесса теста
        MeetState::new(cfg)
    }

    fn row(id: &str, status: MeetStatus) -> MeetingRow {
        MeetingRow {
            id: id.into(),
            url_key: format!("telemost:h/{id}"),
            canonical_url: "https://telemost.yandex.ru/j/1".into(),
            platform: Platform::Telemost,
            room_id: "1".into(),
            title: None,
            status,
            status_reason: None,
            summary_status: SummaryStatus::None,
            mode: "normal".into(),
            bot_name: None,
            created_at: 1,
            updated_at: 1,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            meeting_dir: "/tmp".into(),
            audio_path: None,
            transcript_path: None,
            summary_path: None,
            track_mode: None,
            capture_path: None,
        }
    }

    async fn fetch(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    /// ГЛАВНЫЙ инвариант роута: 200 при полностью отсутствующем окружении —
    /// нет бинарей, нет моделей, нет даже базы.
    #[tokio::test]
    async fn status_answers_200_with_nothing_installed() {
        let state = state_without_db();
        let app = build_router(state);
        let (code, body) = fetch(&app, "/api/meet/status").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["enabled"], true);
        assert_eq!(body["db_ready"], false, "база ещё не открыта — и это не мешает");
        assert_eq!(body["deps"]["probed"], false);
        assert_eq!(body["active"].as_array().unwrap().len(), 0);
    }

    /// Облако НЕ НАСТРОЕНО — честное поле, а не 500 и не поход в сеть.
    ///
    /// Заодно это проверка герметичности: у конфига по умолчанию ключа нет,
    /// значит `status` не имеет права никуда ходить, и тест не зависит от
    /// доступности интернета на машине сборки.
    #[tokio::test]
    async fn status_says_the_cloud_is_not_configured_without_touching_the_network() {
        let app = build_router(state_without_db());
        let started = std::time::Instant::now();
        let (code, body) = fetch(&app, "/api/meet/status").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["cloud"]["configured"], false);
        assert_eq!(body["cloud"]["key_mask"], "");
        assert!(body["cloud"]["key"].is_null());
        assert!(body["cloud"]["error"].is_null(), "{}", body["cloud"]);
        assert!(
            body["cloud"]["checked_secs_ago"].is_null(),
            "без ключа спрашивать нечего и незачем"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "роут ходил в сеть при ненастроенном облаке"
        );

        // Переключатели видны ВСЕГДА и по умолчанию оба облачные.
        assert_eq!(body["config"]["stt_backend"], "cloud");
        assert_eq!(body["config"]["summary_backend"], "cloud");
        assert_eq!(body["cloud"]["stt_model"], crate::cloud::DEFAULT_STT_MODEL);
        assert_eq!(
            body["cloud"]["summary_model"],
            crate::cloud::DEFAULT_SUMMARY_MODEL
        );
    }

    /// **НЕДОСТУПНЫЙ POLZA.AI — ЭТО ПОЛЕ, А НЕ 500.**
    ///
    /// Ключ настоящий по форме, а адрес ведёт в закрытый порт: соединение
    /// отвергается сразу. Роут обязан ответить 200, положить причину в
    /// `cloud.error` и оставить ВСЕ ОСТАЛЬНЫЕ поля целыми — по ним человек и
    /// узнаёт, что именно сломано.
    #[tokio::test]
    async fn an_unreachable_cloud_is_a_field_not_a_broken_route() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::MeetConfig {
            config_path: dir.path().join("meet.toml"),
            cache_dir: dir.path().join("cache"),
            db_path: dir.path().join("cache/meet.db"),
            models_dir: dir.path().join("cache/models"),
            // Порт 1 на loopback закрыт: отказ приходит немедленно, и тест не
            // платит за него ни секундой ожидания.
            cloud_base_url: "http://127.0.0.1:1/api/v1".to_string(),
            polza_api_key: crate::cloud::SecretKey::new("pza_DEADdeadDEADdead_unreachable"),
            ..crate::config::MeetConfig::default()
        };
        let app = build_router(MeetState::new(cfg));
        let (code, body) = fetch(&app, "/api/meet/status").await;

        assert_eq!(code, StatusCode::OK, "роут упал из-за чужого сервиса");
        assert_eq!(body["cloud"]["configured"], true);
        let err = &body["cloud"]["error"];
        assert!(!err.is_null(), "отказ облака потерялся: {}", body["cloud"]);
        assert!(
            err["code"].as_str().unwrap().starts_with("cloud_"),
            "код отказа не облачный: {err}"
        );
        assert!(body["cloud"]["key"].is_null());

        // ОСТАЛЬНОЙ ОТВЕТ ЦЕЛ — ради него роут и существует.
        assert_eq!(body["enabled"], true);
        assert_eq!(body["deps"]["probed"], false);
        assert!(body["providers"].as_array().unwrap().len() >= 4);
        assert_eq!(body["limits"]["whisper"], 1);

        // И маска вместо ключа.
        let mask = body["cloud"]["key_mask"].as_str().unwrap();
        assert!(mask.contains('•'), "{mask}");
        assert!(!serde_json::to_string(&body)
            .unwrap()
            .contains("unreachable"));
    }

    /// Реестр платформ приезжает с бэкенда: разметку браузер держит в кэше PWA
    /// неделями, и фронтовая константа протухла бы незаметно.
    #[tokio::test]
    async fn status_carries_the_platform_registry_with_link_regexes() {
        let app = build_router(state_without_db());
        let (_, body) = fetch(&app, "/api/meet/status").await;
        let providers = body["providers"].as_array().unwrap();
        assert_eq!(providers.len(), crate::providers::PROVIDERS.len());
        assert!(
            providers
                .iter()
                .any(|p| p["platform"] == "vcc" && p["title"] == "AirLabs L.I.N.K."),
            "новая платформа обязана доезжать до фронта — иначе она детектится, \
             но в списке поддерживаемых её нет"
        );
        for p in providers {
            assert!(!p["link_regex"].as_str().unwrap().is_empty());
            assert!(!p["hosts"].as_array().unwrap().is_empty());
        }
        assert_eq!(body["limits"]["whisper"], 1, "лимит whisper поднять нельзя");
    }

    /// Батч обязан обслуживаться тёплым индексом. Доказательство сильнее
    /// счётчика запросов: у состояния НЕТ базы вовсе, и если бы роут в неё
    /// ходил, ответ был бы 503.
    #[tokio::test]
    async fn badges_are_served_from_memory_without_touching_sqlite() {
        let state = state_without_db();
        assert!(state.db().is_none(), "у состояния не должно быть базы");
        state
            .publish_badge(
                &row("m1", MeetStatus::Recording),
                &[SourceKey::new("telegram", "7", 120)],
            )
            .await;
        let app = build_router(state);

        let (code, body) = fetch(
            &app,
            "/api/meet/badges?plugin=telegram&chat_id=7&min_id=100&max_id=200",
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let badges = body["badges"].as_array().unwrap();
        assert_eq!(badges.len(), 1, "{body}");
        assert_eq!(badges[0]["message_id"], 120);
        assert_eq!(badges[0]["status"], "recording");
        assert_eq!(badges[0]["meeting_id"], "m1");
    }

    /// В ответе только те сообщения, у которых встреча есть.
    #[tokio::test]
    async fn badges_never_return_placeholders() {
        let state = state_without_db();
        state
            .publish_badge(
                &row("m1", MeetStatus::Ready),
                &[SourceKey::new("telegram", "7", 120)],
            )
            .await;
        let app = build_router(state);
        let (_, body) = fetch(
            &app,
            "/api/meet/badges?plugin=telegram&chat_id=7&min_id=1&max_id=100",
        )
        .await;
        assert_eq!(body["badges"].as_array().unwrap().len(), 0, "{body}");
    }

    #[tokio::test]
    async fn badges_reject_broken_parameters_with_400() {
        let app = build_router(state_without_db());
        for uri in [
            "/api/meet/badges",
            "/api/meet/badges?plugin=telegram",
            "/api/meet/badges?plugin=telegram&chat_id=7",
            "/api/meet/badges?plugin=telegram&chat_id=7&min_id=x&max_id=2",
            "/api/meet/badges?plugin=telegram&chat_id=7&min_id=5&max_id=1",
            "/api/meet/badges?plugin=&chat_id=7&min_id=1&max_id=2",
            "/api/meet/badges?plugin=telegram&chat_id=7&min_id=1&max_id=100000",
        ] {
            let (code, body) = fetch(&app, uri).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "bad_request", "{uri}");
        }
    }

    #[test]
    fn unknown_query_parameters_are_ignored_not_rejected() {
        // Новый фронт может дописать версию клиента в URL; старый сервер не
        // имеет права отвечать на это ошибкой.
        let q = parse_badge_query(Some(
            "plugin=telegram&chat_id=7&min_id=1&max_id=50&v=2&tab=7",
        ))
        .unwrap();
        assert_eq!(q.plugin, "telegram");
        assert_eq!(q.max_id, 50);
    }

    /// ЛЕСТНИЦА ОШИБОК: каждая ступень срабатывает раньше следующей.
    #[test]
    fn the_error_ladder_checks_dependencies_before_404_and_404_before_400() {
        let meeting = row("m1", MeetStatus::Ready);
        let all_broken = || Preconditions {
            dep_blocker: Some("whisper_unavailable"),
            busy: true,
            meeting: None,
            transition: Some(MeetStatus::Recording),
            params: Some(MeetError::BadRequest("плохой параметр".into())),
        };

        // 1. Зависимости — раньше всего.
        assert_eq!(
            check_ladder(all_broken()).unwrap_err(),
            MeetError::DepUnavailable("whisper_unavailable")
        );

        // 2. Слот — раньше резолва.
        let p = Preconditions {
            dep_blocker: None,
            ..all_broken()
        };
        assert_eq!(check_ladder(p).unwrap_err(), MeetError::Busy);

        // 3. Резолв (404) — раньше разбора параметров (400).
        let p = Preconditions {
            dep_blocker: None,
            busy: false,
            ..all_broken()
        };
        assert_eq!(check_ladder(p).unwrap_err(), MeetError::NotFound);

        // 4. Допустимость перехода — раньше параметров.
        let p = Preconditions {
            dep_blocker: None,
            busy: false,
            meeting: Some(&meeting),
            transition: Some(MeetStatus::Recording),
            params: Some(MeetError::BadRequest("плохой параметр".into())),
        };
        assert_eq!(
            check_ladder(p).unwrap_err(),
            MeetError::InvalidState {
                from: "ready",
                to: "recording"
            }
        );

        // 5. И только теперь — параметры.
        let p = Preconditions {
            dep_blocker: None,
            busy: false,
            meeting: Some(&meeting),
            transition: None,
            params: Some(MeetError::BadRequest("плохой параметр".into())),
        };
        assert_eq!(
            check_ladder(p).unwrap_err(),
            MeetError::BadRequest("плохой параметр".into())
        );

        // Всё в порядке — Ok.
        let p = Preconditions {
            meeting: Some(&meeting),
            ..Default::default()
        };
        assert!(check_ladder(p).is_ok());
    }

    /// Строки таблицы-контракта из шапки: `(метод, путь)`.
    fn table_rows() -> Vec<(String, String)> {
        let src = include_str!("mod.rs");
        let header = src.split("use std::sync::Arc;").next().unwrap();
        let mut out = Vec::new();
        for line in header.lines() {
            let line = line.trim_start_matches("//!").trim();
            if !line.starts_with('|') {
                continue;
            }
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // `| метод | `путь` | фаза | что делает |` → пустая, метод, путь, …
            let (Some(method), Some(path)) = (cells.get(1), cells.get(2)) else {
                continue;
            };
            if !matches!(*method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                continue;
            }
            let path = path.trim_matches('`');
            out.push((method.to_string(), path.to_string()));
        }
        out
    }

    /// Реально зарегистрированное: `путь → множество методов` из тела
    /// [`build_router`].
    fn registered_routes() -> std::collections::BTreeMap<String, Vec<String>> {
        let src = include_str!("mod.rs");
        let build = src
            .split("pub fn build_router")
            .nth(1)
            .and_then(|s| s.split("// --- GET /api/meet/status").next())
            .expect("не нашёлся build_router");
        let mut out: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for chunk in build.split(".route(").skip(1) {
            let path = chunk
                .split('"')
                .nth(1)
                .expect("у .route первым аргументом обязан быть строковый путь");
            let entry = out.entry(path.to_string()).or_default();
            for (needle, verb) in [
                ("get(", "GET"),
                ("post(", "POST"),
                ("put(", "PUT"),
                ("patch(", "PATCH"),
                ("delete(", "DELETE"),
            ] {
                if chunk.contains(needle) && !entry.iter().any(|m| m == verb) {
                    entry.push(verb.to_string());
                }
            }
        }
        out
    }

    /// **ТАБЛИЦА-КОНТРАКТ И РОУТЕР СВЕРЯЮТСЯ В ОБЕ СТОРОНЫ.**
    ///
    /// Одностороннюю проверку («каждый зарегистрированный путь есть в
    /// таблице») этот файл уже носил — и она пропустила Ф6 целиком: `GET /:id`
    /// и `GET /:id/transcript` стояли в таблице, фронт их звал, а `.route(…)`
    /// для них не было ни одной строки. Наружу это выглядело как «карточка
    /// встречи не открывается», и ни один тест не покраснел.
    ///
    /// Поэтому здесь равенство множеств: путь в таблице без роута — молчаливый
    /// 404 у фронта, роут без строки в таблице — поверхность, о которой не
    /// знает ни фронт, ни контрактный тест `meet_contract.test.mjs`.
    #[test]
    fn the_contract_table_and_the_router_agree_in_both_directions() {
        let table = table_rows();
        // Планка поднимается вместе с поверхностью: Ф16 добавила каталог
        // облачных моделей и баланс. Разобранная не полностью таблица
        // превратила бы сверку ниже в проверку куска.
        assert!(table.len() >= 25, "таблица-контракт разобрана не полностью: {table:?}");
        let registered = registered_routes();
        assert!(!registered.is_empty(), "в build_router не нашлось ни одного пути");

        for (method, path) in &table {
            let methods = registered.get(path).unwrap_or_else(|| {
                panic!("путь {path} стоит в таблице-контракте, но в build_router его НЕТ — фронт получит молчаливый 404")
            });
            assert!(
                methods.contains(method),
                "{method} {path} объявлен в таблице, а зарегистрированы только {methods:?}"
            );
        }
        for (path, methods) in &registered {
            for method in methods {
                assert!(
                    table.iter().any(|(m, p)| m == method && p == path),
                    "{method} {path} зарегистрирован, но в таблице-контракте его нет"
                );
            }
        }

        let src = include_str!("mod.rs");
        let header = src.split("use std::sync::Arc;").next().unwrap();
        assert!(
            header.contains("/ws/meet/ingest` НЕ СОЗДАЁТСЯ"),
            "запрет на ingest-сокет обязан стоять в таблице явной строкой"
        );
    }

    /// Страж стража: разбор таблицы и роутера обязан быть НЕ ПУСТЫМ и
    /// различать методы. Пустой разбор превратил бы тест выше в проверку
    /// «пусто равно пусту».
    #[test]
    fn the_two_way_check_actually_parses_something() {
        let table = table_rows();
        assert!(table.contains(&("GET".to_string(), "/api/meet/status".to_string())));
        assert!(table.contains(&("PATCH".to_string(), "/api/meet/meetings/:id/speakers".to_string())));
        assert!(table.contains(&("PUT".to_string(), "/api/meet/config".to_string())));
        assert!(table.contains(&("GET".to_string(), "/api/meet/cloud/models".to_string())));
        assert!(table.contains(&("GET".to_string(), "/api/meet/cloud/balance".to_string())));

        let registered = registered_routes();
        assert_eq!(
            registered.get("/api/meet/config").map(Vec::as_slice),
            Some(["GET".to_string(), "PUT".to_string()].as_slice()),
            "у /api/meet/config обязаны быть оба метода: {registered:?}"
        );
        assert_eq!(
            registered.get("/api/meet/meetings/:id/speakers").map(Vec::as_slice),
            Some(["PATCH".to_string()].as_slice())
        );
    }

    /// **Сокет приёма звука не существует. И не появится.**
    ///
    /// Тест стоит здесь не ради полноты. Роут вида `/ws/meet/ingest`
    /// напрашивается сам собой всякий раз, когда кто-то в следующий раз
    /// столкнётся с задачей «доставить кадры из страницы в плагин»; и
    /// добавить его — работа на десять минут, после которой токен встречи
    /// оказывается в памяти чужой страницы с отключённым CSP, а через главный
    /// сокет становится достижим `/ws/attach`, то есть терминал.
    ///
    /// Разбор — в шапке модуля, в `bot/transport.rs` и рядом с
    /// `RAW_UPLOAD_PATHS` в `tmux-web/src/auth.rs`.
    #[tokio::test]
    async fn the_ingest_socket_is_never_registered() {
        let app = build_router(state_without_db());
        for uri in [
            "/ws/meet/ingest",
            "/api/meet/ingest",
            "/ws/meet/audio",
        ] {
            let (code, _) = fetch(&app, uri).await;
            assert_eq!(
                code,
                StatusCode::NOT_FOUND,
                "{uri} зарегистрирован — звук обязан идти ТОЛЬКО через CDP-биндинг"
            );
        }

        // И в исходнике роутера этого пути нет ни в каком виде.
        let src = include_str!("mod.rs");
        let build = src
            .split("pub fn build_router")
            .nth(1)
            .and_then(|s| s.split("// --- GET /api/meet/status").next())
            .expect("не нашёлся build_router");
        assert!(
            !build.contains("ingest"),
            "в build_router появилось слово ingest: {build}"
        );
    }

    /// Роутер плагина не имеет права затирать роуты хоста.
    #[tokio::test]
    async fn register_routes_merges_into_the_host_router() {
        // Json, а не строка: помощник `fetch` разбирает тело как JSON.
        let host = Router::new().route("/healthz", get(|| async { Json("ok") }));
        let app = crate::register_routes(host, state_without_db());
        let (code, _) = fetch(&app, "/healthz").await;
        assert_eq!(code, StatusCode::OK);
        let (code, _) = fetch(&app, "/api/meet/status").await;
        assert_eq!(code, StatusCode::OK);
    }
}
