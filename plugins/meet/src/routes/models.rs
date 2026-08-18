//! Роуты моделей whisper и перепроверки внешних бинарей.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET | `/api/meet/models` | каталог: что известно, что лежит на диске, что выбрано |
//! | POST | `/api/meet/models/:name/download` | **скачать — только по явному действию человека** |
//! | DELETE | `/api/meet/models/:name` | удалить с диска |
//! | POST | `/api/meet/deps/recheck` | перепроверить бинари без перезапуска devforge |
//!
//! # Скачивание отвечает СРАЗУ, а не по окончании
//!
//! Модель по умолчанию весит 547 МиБ, `large-v3-turbo` — полтора гигабайта.
//! Держать HTTP-запрос открытым всё это время значит спорить с таймаутами
//! прокси, браузера и обратного прокси пользователя разом. Поэтому роут
//! отвечает `202` и уходит в фоновую задачу, а прогресс идёт кадрами
//! [`MeetServerMsg::ModelProgress`] — тем же каналом, что и всё остальное в
//! плагине.
//!
//! Повторное нажатие на «скачать» во время загрузки НЕ запускает вторую:
//! два писателя в один `.part` дали бы перемешанные байты, которые прошли бы
//! проверку размера и упали на контрольной сумме — то есть час загрузки
//! впустую.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::deps::DepsReport;
use crate::error::MeetError;
use crate::models;
use crate::state::MeetState;
use crate::ws::protocol::MeetServerMsg;

/// Тело `GET /api/meet/models`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelsDto {
    /// Где плагин их ищет — чтобы человек мог положить веса руками.
    pub models_dir: String,
    /// Модель, выбранная в конфиге.
    pub selected: String,
    pub models: Vec<models::ModelStatus>,
    /// Что качается прямо сейчас.
    pub downloading: Vec<String>,
}

/// `GET /api/meet/models`.
pub async fn list(State(state): State<Arc<MeetState>>) -> Json<ModelsDto> {
    let mut downloading: Vec<String> = Vec::new();
    for m in models::MODELS {
        if state.is_downloading(m.name).await {
            downloading.push(m.name.to_string());
        }
    }
    Json(ModelsDto {
        models_dir: state.config.models_dir.display().to_string(),
        selected: state.config.whisper_model.clone(),
        models: models::catalog(&state.config.models_dir, &state.config.whisper_model),
        downloading,
    })
}

/// Ответ на запрос загрузки.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadDto {
    pub name: String,
    /// Началась ли загрузка ИМЕННО СЕЙЧАС.
    pub started: bool,
    /// Уже на диске.
    pub downloaded: bool,
    pub size_bytes: u64,
}

/// `POST /api/meet/models/:name/download` — **единственная точка, откуда
/// начинается загрузка модели**.
///
/// Автоматических путей нет ни одного: на это есть тест, обходящий исходники
/// (`models::tests::nothing_but_the_route_starts_a_download`). Причина в
/// шапке `models.rs` — докачка полутора гигабайт в момент «встреча начинается
/// через десять секунд» это худший из сценариев.
pub async fn download(
    State(state): State<Arc<MeetState>>,
    Path(name): Path<String>,
) -> Result<Response, MeetError> {
    let spec = models::spec(&name).ok_or_else(|| {
        MeetError::BadRequest(format!(
            "модель {name:?} не из каталога; известные: {}",
            models::MODELS
                .iter()
                .map(|m| m.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    if models::is_installed(&state.config.models_dir, &name) {
        return Ok((
            StatusCode::OK,
            Json(DownloadDto {
                name,
                started: false,
                downloaded: true,
                size_bytes: spec.size_bytes,
            }),
        )
            .into_response());
    }

    if !state.begin_download(&name).await {
        return Ok((
            StatusCode::ACCEPTED,
            Json(DownloadDto {
                name,
                started: false,
                downloaded: false,
                size_bytes: spec.size_bytes,
            }),
        )
            .into_response());
    }

    let worker = state.clone();
    let job_name = name.clone();
    let handle = tokio::spawn(async move {
        run_download(worker, job_name).await;
    });
    state.register_worker(handle).await;

    Ok((
        StatusCode::ACCEPTED,
        Json(DownloadDto {
            name,
            started: true,
            downloaded: false,
            size_bytes: spec.size_bytes,
        }),
    )
        .into_response())
}

/// Фоновая загрузка с прогрессом в WS.
async fn run_download(state: Arc<MeetState>, name: String) {
    let source = state.model_source();
    let dir = state.config.models_dir.clone();

    // Прогресс режется по времени, а не по каждому чанку: 574 МиБ по 8 КиБ
    // это семьдесят тысяч кадров, из которых пользователю видны единицы.
    let last = std::sync::Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1));
    let sender = state.clone();
    let progress_name = name.clone();
    let progress = move |done: u64, total: Option<u64>| {
        let mut guard = last.lock().unwrap_or_else(|e| e.into_inner());
        if guard.elapsed() < std::time::Duration::from_millis(500) && Some(done) != total {
            return;
        }
        *guard = std::time::Instant::now();
        sender.broadcast_event(MeetServerMsg::ModelProgress {
            name: progress_name.clone(),
            done,
            total,
            status: "downloading".into(),
            message: None,
        });
    };

    let result = models::download_model(&dir, &name, source.as_ref(), &progress).await;
    state.end_download(&name).await;

    match result {
        Ok(path) => {
            tracing::info!(
                target: "forge_meet",
                model = %name,
                path = %path.display(),
                "forge-meet: модель скачана"
            );
            state.broadcast_event(MeetServerMsg::ModelProgress {
                name: name.clone(),
                done: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                total: models::spec(&name).map(|s| s.size_bytes),
                status: "ready".into(),
                message: None,
            });
            // Кнопка «Расшифровать» обязана ожить сама: модель появилась —
            // значит `model_ready` изменился, и статус об этом уже знает.
            crate::deps::refresh(&state).await;
        }
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                model = %name,
                error = %e,
                "forge-meet: модель не скачалась"
            );
            state.broadcast_event(MeetServerMsg::ModelProgress {
                name,
                done: 0,
                total: None,
                status: "failed".into(),
                message: Some(e.message()),
            });
        }
    }
}

/// `DELETE /api/meet/models/:name`.
pub async fn remove(
    State(state): State<Arc<MeetState>>,
    Path(name): Path<String>,
) -> Result<Json<DownloadDto>, MeetError> {
    models::delete(&state.config.models_dir, &name)?;
    let size = models::spec(&name).map(|s| s.size_bytes).unwrap_or(0);
    crate::deps::refresh(&state).await;
    Ok(Json(DownloadDto {
        name,
        started: false,
        downloaded: false,
        size_bytes: size,
    }))
}

/// Тело `POST /api/meet/deps/recheck`.
#[derive(Debug, Clone, Serialize)]
pub struct RecheckDto {
    pub deps: DepsReport,
}

/// `POST /api/meet/deps/recheck` — перепроверка без перезапуска devforge.
///
/// Нужна ровно тогда, когда человек поставил недостающее (`brew install
/// whisper-cpp`) и не хочет ронять сервер посреди чужой записи.
pub async fn recheck(State(state): State<Arc<MeetState>>) -> Json<RecheckDto> {
    Json(RecheckDto {
        deps: crate::deps::refresh(&state).await,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeetConfig;
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use tower::ServiceExt;

    fn stand(dir: &std::path::Path) -> Arc<MeetState> {
        let cfg = MeetConfig {
            config_path: dir.join("meet.toml"),
            cache_dir: dir.join("cache"),
            db_path: dir.join("cache/meet.db"),
            models_dir: dir.join("models"),
            // Внешних бинарей нет: пути указаны явно и ведут в пустоту.
            ffmpeg_path: Some(dir.join("no-ffmpeg")),
            ffprobe_path: Some(dir.join("no-ffprobe")),
            whisper_path: Some(dir.join("bin/whisper-cli")),
            chrome_path: Some(dir.join("no-chrome")),
            ..MeetConfig::default()
        };
        MeetState::new(cfg)
    }

    async fn call(app: &Router, method: &str, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (code, json)
    }

    #[tokio::test]
    async fn the_catalog_shows_what_is_on_disk_and_what_is_selected() {
        let dir = tempfile::tempdir().unwrap();
        let state = stand(dir.path());
        std::fs::create_dir_all(&state.config.models_dir).unwrap();
        std::fs::write(state.config.models_dir.join("ggml-base.bin"), b"lmgg1234").unwrap();
        let app = crate::routes::build_router(state);

        let (code, body) = call(&app, "GET", "/api/meet/models").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["selected"], crate::config::DEFAULT_WHISPER_MODEL);
        let models = body["models"].as_array().unwrap();
        let base = models.iter().find(|m| m["name"] == "base").unwrap();
        assert_eq!(base["downloaded"], true);
        let turbo = models
            .iter()
            .find(|m| m["name"] == crate::config::DEFAULT_WHISPER_MODEL)
            .unwrap();
        assert_eq!(turbo["downloaded"], false);
        assert_eq!(turbo["selected"], true);
        assert!(body["downloading"].as_array().unwrap().is_empty());
    }

    /// Неизвестное имя отвергается ДО всякой сети.
    #[tokio::test]
    async fn downloading_an_unknown_model_is_a_400() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::routes::build_router(stand(dir.path()));
        let (code, body) = call(&app, "POST", "/api/meet/models/нет-такой/download").await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "bad_request");
    }

    /// Уже скачанная модель не качается повторно.
    #[tokio::test]
    async fn downloading_an_installed_model_answers_without_starting_anything() {
        let dir = tempfile::tempdir().unwrap();
        let state = stand(dir.path());
        std::fs::create_dir_all(&state.config.models_dir).unwrap();
        std::fs::write(state.config.models_dir.join("ggml-base.bin"), b"lmgg1234").unwrap();
        let app = crate::routes::build_router(state);
        let (code, body) = call(&app, "POST", "/api/meet/models/base/download").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["started"], false);
        assert_eq!(body["downloaded"], true);
    }

    /// Загрузка идёт в фоне и рассказывает о себе кадрами WS — включая
    /// НЕУДАЧУ.
    ///
    /// Оборванная загрузка здесь не случайность, а проверяемый случай: файл
    /// правильной магии, но неправильной длины обязан дать кадр `failed` с
    /// причиной и не оставить в каталоге ничего, похожего на модель. Полный
    /// успешный путь (включая SHA-256) проверяет
    /// `models::tests::a_finished_download_lands_under_the_final_name_after_a_full_check`
    /// — сюда его не затащить, не подделав контрольную сумму настоящей
    /// модели, то есть не сняв проверку.
    #[tokio::test]
    async fn a_download_streams_progress_and_reports_its_failure() {
        struct Truncated;
        #[async_trait::async_trait]
        impl models::ModelSource for Truncated {
            async fn fetch(
                &self,
                _url: &str,
                dest: &std::path::Path,
                progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
            ) -> Result<(), String> {
                progress(0, Some(8));
                std::fs::write(dest, b"lmgg1234").map_err(|e| e.to_string())?;
                progress(8, Some(8));
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let state = stand(dir.path());
        state.set_model_source(Arc::new(Truncated));
        let mut rx = state.subscribe();
        let app = crate::routes::build_router(state.clone());

        let (code, body) = call(&app, "POST", "/api/meet/models/base/download").await;
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(body["started"], true, "загрузка не началась: {body}");

        let mut progress_frames = 0usize;
        let mut failure: Option<String> = None;
        for _ in 0..50 {
            match tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await {
                Ok(Ok(MeetServerMsg::ModelProgress { status, name, message, .. })) => {
                    assert_eq!(name, "base");
                    match status.as_str() {
                        "downloading" => progress_frames += 1,
                        "failed" => {
                            failure = message;
                            break;
                        }
                        other => panic!("неожиданный статус {other}"),
                    }
                }
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        assert!(progress_frames > 0, "прогресс не доехал до фронта");
        let msg = failure.expect("кадр о провале загрузки не пришёл");
        assert!(msg.contains("размер"), "{msg}");
        assert!(
            !models::is_installed(&state.config.models_dir, "base"),
            "оборванная загрузка выглядит готовой моделью"
        );
    }

    /// **`deps/recheck` обновляет статус БЕЗ перезапуска сервера.**
    ///
    /// Сценарий дословный: человек увидел «whisper не установлен», поставил
    /// его и нажал «перепроверить». Мок появляется на диске между двумя
    /// пробами.
    #[tokio::test]
    #[cfg(unix)]
    async fn recheck_picks_up_a_binary_that_appeared_after_the_first_probe() {
        use crate::testing::mocks::{mock_whisper, WhisperMock};
        let _procs = crate::testing::process_count_guard();
        let dir = tempfile::tempdir().unwrap();
        let state = stand(dir.path());
        let app = crate::routes::build_router(state.clone());

        // Первая проба: whisper нет.
        let (code, body) = call(&app, "POST", "/api/meet/deps/recheck").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["deps"]["probed"], true);
        assert_eq!(body["deps"]["whisper"]["found"], false);

        // Ставим «whisper» и модель.
        mock_whisper(&dir.path().join("bin"), "whisper-cli", &WhisperMock::default()).unwrap();
        std::fs::create_dir_all(&state.config.models_dir).unwrap();
        std::fs::write(
            state.config.models_dir.join(models::spec(crate::config::DEFAULT_WHISPER_MODEL).unwrap().file),
            b"lmgg1234",
        )
        .unwrap();

        let (code, body) = call(&app, "POST", "/api/meet/deps/recheck").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["deps"]["whisper"]["found"], true, "{body}");
        assert_eq!(body["deps"]["model_ready"], true);
        assert_eq!(body["deps"]["gpu_metal"], true);

        // И то же самое видно в /status — перезапускать devforge не пришлось.
        let (code, status) = call(&app, "GET", "/api/meet/status").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(status["deps"]["whisper"]["found"], true);
        assert_eq!(status["deps"]["whisper_flags"]["output_json_full"], true);
    }

    #[tokio::test]
    async fn deleting_a_model_removes_it_and_refreshes_the_status() {
        let dir = tempfile::tempdir().unwrap();
        let state = stand(dir.path());
        std::fs::create_dir_all(&state.config.models_dir).unwrap();
        std::fs::write(state.config.models_dir.join("ggml-base.bin"), b"lmgg1234").unwrap();
        let app = crate::routes::build_router(state.clone());

        let (code, _) = call(&app, "DELETE", "/api/meet/models/base").await;
        assert_eq!(code, StatusCode::OK);
        assert!(!models::is_installed(&state.config.models_dir, "base"));

        let (code, body) = call(&app, "DELETE", "/api/meet/models/base").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
    }
}
