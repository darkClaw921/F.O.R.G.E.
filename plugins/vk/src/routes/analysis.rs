//! Роуты анализа чата: пер-чатовый контекст и запуск разбора через Claude.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET | `/api/vk/dialogs/:id/prompt` | заметка пользователя к беседе |
//! | PUT | `/api/vk/dialogs/:id/prompt` | сохранить заметку (пустая строка — стереть) |
//! | GET | `/api/vk/dialogs/:id/analysis` | ПОСЛЕДНИЙ сохранённый разбор + тред вопросов |
//! | DELETE | `/api/vk/dialogs/:id/analysis` | стереть сохранённый разбор (тред и заметка целы) |
//! | POST | `/api/vk/dialogs/:id/analysis` | запустить анализ, обновление либо вопрос |
//! | DELETE | `/api/vk/dialogs/:id/analysis/thread` | стереть тред вопросов (разбор и заметка целы) |
//! | POST | `/api/vk/dialogs/:id/analysis/cancel` | остановить идущий анализ |
//!
//! # Почему у треда ОТДЕЛЬНЫЙ путь, а не `?thread=1`
//!
//! Query-параметры в этом плагине разбираются вручную из `RawQuery` — типовой
//! экстрактор `Query<T>` отрабатывает ДО тела хендлера и тем самым отвечает
//! `400` раньше проверки авторизации, то есть ломает лестницу (та же причина,
//! по которой тело здесь `Option<Json<T>>`). Отдельный путь не требует ни
//! парсинга, ни исключений в auth-ladder: это просто ещё один маршрут на той
//! же ступени, и сторожевые тесты [`super`] перебирают его наравне с
//! остальными.
//!
//! # ОДИН ПУТЬ, ДВЕ ЛЕСТНИЦЫ
//!
//! У `/analysis` разные методы стоят на РАЗНЫХ лестницах, и это не
//! непоследовательность: `GET`/`DELETE` работают с локальным файлом (как
//! `/prompt`), `POST` начинается с выгрузки истории из VK. Требовать
//! соединения, чтобы ПОКАЗАТЬ уже сделанный разбор, значило бы прятать его
//! ровно тогда, когда сеть отвалилась, — то есть когда он полезнее всего.
//!
//! ```text
//!   /prompt, GET|DELETE /analysis  POST /analysis, /analysis/cancel
//!   DELETE /analysis/thread
//!   ───────────────────────────    ────────────────────────────────
//!   авторизация (409)              авторизация (409)
//!        ▼                              ▼
//!                                  соединение (503)   ← require_ready
//!        ▼                              ▼
//!   резолв peer (404)              резолв peer (404)
//!        ▼                              ▼
//!   разбор тела (400)              разбор тела (400)
//! ```
//!
//! У `/prompt` ступени «соединение» ([`super::dialogs::require_connected`])
//! НЕТ: заметка лежит в локальном файле, и `503 not_connected` на чтение
//! локального файла был бы враньём — вкладка перестала бы открывать контекст
//! ровно тогда, когда он нужнее всего (VK недоступен, пользователь читает
//! историю из кэша). Та же логика, что у `GET /api/vk/filters`.
//!
//! У `/analysis` ступень «соединение» ОБЯЗАТЕЛЬНА и стоит первой из сетевых:
//! анализ начинается с выгрузки истории через `messages.getHistory`, то есть
//! без VK не существует вовсе. Запускать процесс `claude`, чтобы через
//! полторы секунды выяснить, что транскрипт пустой, — это сожжённые токены и
//! бессмысленный ответ модели.
//!
//! # Порядок отказов у `POST /analysis` зафиксирован
//!
//! `require_ready` → `resolve_peer` → тело и `kind` → **уже идущий run по этой
//! беседе** → наличие Claude CLI → свободный слот → выгрузка истории → старт.
//!
//! # `kind: "refresh"` и ответ «новых сообщений нет»
//!
//! Обновление разбора требует ДВУХ вещей: сохранённого разбора (иначе обновлять
//! нечего — `400 invalid_input`) и хотя бы одного сообщения новее него. Второго
//! может не оказаться: пользователь нажал «обновить» дважды подряд. Это НЕ
//! ошибка, а штатный ответ `200 {run_id: null, no_new_messages: true}` — модель
//! при этом не запускается вовсе. Отвечать здесь ошибкой значило бы окрашивать
//! в красное совершенно нормальное «всё уже разобрано», а запускать модель на
//! пустом транскрипте — жечь токены ради ответа «изменений нет» и, что хуже,
//! ЗАМЕНЯТЬ им сохранённый разбор.
//!
//! Три последних проверки стоят ДО выгрузки истории не для красоты: выгрузка —
//! это до ~1.7 с запросов при бюджете 3 rps. Отказ «уже идёт два анализа»
//! после полутора секунд ожидания выглядит как поломка, а повторный клик по
//! кнопке не имеет права стоить второго обхода истории.
//!
//! # Ответ уходит СРАЗУ, остальное — кадрами WS
//!
//! `POST /analysis` возвращает `{run_id, attached, messages_used, truncated}`
//! в тот момент, когда процесс только запущен. Текст модели приезжает кадрами
//! `analysis_delta` (см. [`crate::ws::protocol`]), финал — `analysis_done`.
//! Держать HTTP-соединение открытым на всю генерацию нельзя: она длится
//! минуты, а любой прокси между браузером и devforge оборвёт её раньше.
//!
//! Авторизация при этом ОБЯЗАТЕЛЬНА: заметки — это записи пользователя о
//! конкретных людях, и отдавать их кому попало нельзя даже в одиночном
//! режиме.
//!
//! # Почему тело — `Option<Json<T>>`, а не `Json<T>`
//!
//! Экстрактор `Json<T>` отрабатывает ДО тела хендлера, то есть отверг бы
//! битый JSON раньше проверки авторизации, — и превратил бы роут в оракул
//! валидности идентификаторов (тест
//! `hostile_dialog_ids_hit_auth_before_any_parsing` в [`super`] краснеет
//! именно на этом). `Option<Json<T>>` превращает неразобранное тело в `None`,
//! которое хендлер обрабатывает уже ПОСЛЕ лестницы.
//!
//! Отсутствующее и битое тело — это `400 invalid_input`, а НЕ «пустая
//! заметка». Стирание требует явного `{"prompt": ""}`: молча удалять текст
//! из-за оборванного запроса недопустимо.
//!
//! # Ключ хранилища — КАНОНИЧЕСКИЙ id
//!
//! В файл кладётся `peer_id.to_string()` после [`resolve_peer`], а не сырой
//! сегмент URL. Иначе `/dialogs/1234/prompt` и `/dialogs/%201234%20/prompt`
//! (оба валидны для [`crate::dto::parse_dialog_id`]) завели бы ДВЕ разные
//! заметки на одну беседу.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::analysis::context::{self, DEFAULT_DEPTH};
use crate::analysis::prompt::{self, QaTurn, Task};
// `QaTurn` в этом файле уже занят парой ПРОМПТА (`analysis::prompt`), поэтому
// хранимая пара приезжает под другим именем: одно написание на два разных типа
// стоило бы читателю лишней остановки.
use crate::analysis::prompts::{
    ChatAnalysis, QaTurn as StoredTurn, MAX_ANALYSIS_CHARS, MAX_PROMPT_CHARS,
};
use crate::analysis::{
    AnalysisError, StartRequest, KIND_ANALYZE, KIND_ASK, KIND_REFRESH, REASON_CANCELLED,
};
use crate::dto;
use crate::routes::dialogs::{require_authorized, require_ready, resolve_peer};
use crate::routes::error::ApiError;
use crate::state::VkState;
use crate::ws::protocol::VkServerMsg;

/// Ответ обоих роутов.
///
/// `updated_at` — `Option`, потому что «заметки нет» и «заметка пустая» это
/// одно состояние, и фронту нужен признак, показывать ли строку «сохранено».
/// `max_chars` приезжает С СЕРВЕРА, а не зашит в JS: счётчик символов в
/// textarea обязан совпадать с тем лимитом, по которому сервер реально режет,
/// иначе пользователь допишет фразу, которая молча пропадёт.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptResp {
    /// Текст заметки; пустая строка — заметки нет.
    pub prompt: String,
    /// Когда сохранена (unix), либо `null`.
    pub updated_at: Option<i64>,
    /// Потолок длины в СИМВОЛАХ ([`MAX_PROMPT_CHARS`]).
    pub max_chars: usize,
}

/// Тело `PUT`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptReq {
    /// Новый текст заметки. Пустая строка (или строка из одних пробелов) —
    /// стереть заметку.
    #[serde(default)]
    pub prompt: String,
}

/// Тело `POST /analysis`.
///
/// Все поля кроме `kind` необязательны: кнопка «Анализ» шлёт `{"kind":
/// "analyze"}` и ничего больше.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisReq {
    /// `analyze` — структурированный разбор, `ask` — свободный вопрос.
    #[serde(default)]
    pub kind: String,
    /// Вопрос пользователя. ОБЯЗАТЕЛЕН при `kind = "ask"`.
    #[serde(default)]
    pub question: Option<String>,
    /// Сколько последних сообщений выгружать; клампится
    /// [`context::clamp_depth`], по умолчанию [`DEFAULT_DEPTH`].
    #[serde(default)]
    pub depth: Option<i64>,
    /// Предыдущие пары треда Q&A, от старых к новым.
    #[serde(default)]
    pub history: Option<Vec<QaTurnDto>>,
}

/// Пара «вопрос — ответ» треда Q&A на проводе.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QaTurnDto {
    /// Вопрос пользователя.
    #[serde(default)]
    pub q: String,
    /// Ответ модели.
    #[serde(default)]
    pub a: String,
}

/// Ответ `GET /analysis` — последний сохранённый разбор.
///
/// Форма «пусто» — это не 404 и не `null`-тело, а та же структура с пустым
/// текстом: попап открывается ДО того, как пользователь хоть раз запускал
/// разбор, и ошибка в этом месте была бы враньём о нормальном состоянии.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAnalysisResp {
    /// Текст разбора; пустая строка — разбора нет.
    pub text: String,
    /// До какого сообщения разобрано (точка отсчёта REFRESH), либо `null`.
    pub last_message_id: Option<i32>,
    /// Сколько сообщений было в транскрипте разбора.
    pub messages_used: u32,
    /// Когда сохранён (unix), либо `null`.
    pub updated_at: Option<i64>,
    /// Потолок длины в СИМВОЛАХ ([`MAX_ANALYSIS_CHARS`]) — как и у заметки,
    /// приезжает с сервера, чтобы фронт не хранил свою копию константы.
    pub max_chars: usize,
    /// Сохранённый тред вопросов, от СТАРЫХ к новым.
    ///
    /// Всегда массив, пусть и пустой (а не отсутствие поля, как в файле):
    /// фронту проще безусловно перебрать его при открытии попапа, чем на
    /// каждом обращении проверять наличие ключа.
    #[serde(default)]
    pub thread: Vec<SavedTurnResp>,
}

/// Пара треда в ответе `GET /analysis`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTurnResp {
    /// Вопрос пользователя.
    pub q: String,
    /// Ответ модели.
    pub a: String,
    /// Когда пара сохранена (unix).
    pub at: i64,
}

/// Ответ `DELETE /analysis/thread`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearThreadResp {
    /// Был ли тред, который стёрли. Пустой тред — НЕ ошибка: кнопку можно
    /// нажать дважды, и из двух вкладок сразу.
    pub cleared: bool,
}

/// Ответ `DELETE /analysis`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearAnalysisResp {
    /// Был ли разбор, который стёрли. Отсутствие разбора — НЕ ошибка: две
    /// вкладки могут нажать «очистить» подряд.
    pub cleared: bool,
}

/// Ответ `POST /analysis`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisResp {
    /// Идентификатор run'а — по нему фронт фильтрует кадры и отменяет.
    ///
    /// `null` в единственном случае: `refresh`, которому нечего обновлять
    /// (см. `no_new_messages`). Модель тогда не запускалась, и ждать кадров
    /// не нужно.
    pub run_id: Option<String>,
    /// `true` ⇒ подключились к УЖЕ идущему run'у (вторая вкладка, повторный
    /// клик). Нового процесса при этом не запускалось.
    pub attached: bool,
    /// Сколько сообщений попало в транскрипт.
    pub messages_used: u32,
    /// Часть истории отброшена потолком символов.
    pub truncated: bool,
    /// `true` ⇒ обновлять было нечего: сообщений новее сохранённого разбора
    /// нет, модель не запускалась, сохранённый разбор не тронут.
    #[serde(default)]
    pub no_new_messages: bool,
}

/// Тело `POST /analysis/cancel`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelReq {
    /// Какой run гасить. `None` ⇒ тот, что идёт по этой беседе — так кнопка
    /// «стоп» работает и после перезагрузки вкладки, потерявшей `run_id`.
    #[serde(default)]
    pub run_id: Option<String>,
}

/// Ответ `POST /analysis/cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelResp {
    /// Был ли найден и остановлен run.
    pub cancelled: bool,
}

/// Роуты анализа.
pub fn router() -> Router<Arc<VkState>> {
    // Имя параметра `:id` — то же, что у соседних роутов `/api/vk/dialogs/*`:
    // axum паникует при сборке, если один сегмент назван по-разному.
    Router::new()
        .route("/api/vk/dialogs/:id/prompt", get(get_prompt).put(put_prompt))
        .route(
            "/api/vk/dialogs/:id/analysis",
            post(post_analysis).get(get_analysis).delete(delete_analysis),
        )
        .route(
            "/api/vk/dialogs/:id/analysis/cancel",
            post(post_analysis_cancel),
        )
        // Тот же уровень лестницы, что у `GET|DELETE /analysis`: локальный
        // файл, без `require_ready`.
        .route(
            "/api/vk/dialogs/:id/analysis/thread",
            axum::routing::delete(delete_thread),
        )
}

fn resp(prompt: Option<crate::analysis::prompts::ChatPrompt>) -> Json<PromptResp> {
    let (prompt, updated_at) = match prompt {
        Some(c) => (c.prompt, Some(c.updated_at)),
        None => (String::new(), None),
    };
    Json(PromptResp {
        prompt,
        updated_at,
        max_chars: MAX_PROMPT_CHARS,
    })
}

/// `GET /api/vk/dialogs/:id/prompt` — заметка пользователя к беседе.
///
/// Ни одного вызова VK: ответ целиком собирается из локального файла,
/// прочитанного при старте. Поэтому роут отвечает `200` и при мёртвой сети —
/// см. шапку модуля.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`.
pub async fn get_prompt(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
) -> Result<Json<PromptResp>, ApiError> {
    require_authorized(&state).await?;
    let peer = resolve_peer(&id)?;
    Ok(resp(state.chat_prompt(&peer.to_string()).await))
}

/// `PUT /api/vk/dialogs/:id/prompt` — сохранить заметку.
///
/// Текст проходит [`crate::analysis::prompts::sanitize`] (нормализация
/// переводов строк, вырезание управляющих символов, обрезка до
/// [`MAX_PROMPT_CHARS`] по code points), после чего файл переписывается
/// атомарно с правами `0600`. В ответ уходит то, что РЕАЛЬНО сохранено, — а
/// не то, что прислал клиент: обрезанный хвост обязан исчезнуть в textarea
/// сразу, а не при следующем открытии попапа.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `400 invalid_input` (нет тела
/// или оно не разобралось), `500 storage_failed` (не удалось записать файл).
pub async fn put_prompt(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    body: Option<Json<PromptReq>>,
) -> Result<Json<PromptResp>, ApiError> {
    require_authorized(&state).await?;
    let peer = resolve_peer(&id)?;
    let Some(Json(req)) = body else {
        return Err(ApiError::bad_input(
            "the request body must be a JSON object with a 'prompt' string",
        ));
    };

    let key = peer.to_string();
    let saved = state
        .set_chat_prompt(&key, &req.prompt, dto::now_unix())
        .await
        .map_err(|e| {
            // ТОЛЬКО путь и ошибка: текст заметки — данные о конкретном
            // человеке, и в логи он не попадает ни при каком исходе.
            tracing::warn!(
                target: "forge_vk",
                error = %e,
                "forge-vk: could not persist the chat prompt"
            );
            ApiError::storage_failed(format!("could not save the chat context: {e}"))
        })?;

    tracing::debug!(
        target: "forge_vk",
        dialog_id = %key,
        chars = saved.as_ref().map_or(0, |c| c.prompt.chars().count()),
        "forge-vk: chat prompt updated (the text itself is never logged)"
    );
    Ok(resp(saved))
}

// ---------------------------------------------------------------------------
// Сохранённый разбор.
// ---------------------------------------------------------------------------

fn saved_resp(analysis: Option<ChatAnalysis>, thread: Vec<StoredTurn>) -> Json<SavedAnalysisResp> {
    let thread = thread
        .into_iter()
        .map(|t| SavedTurnResp {
            q: t.q,
            a: t.a,
            at: t.at,
        })
        .collect();
    Json(match analysis {
        Some(a) => SavedAnalysisResp {
            text: a.text,
            last_message_id: Some(a.last_message_id),
            messages_used: a.messages_used,
            updated_at: Some(a.updated_at),
            max_chars: MAX_ANALYSIS_CHARS,
            thread,
        },
        None => SavedAnalysisResp {
            max_chars: MAX_ANALYSIS_CHARS,
            thread,
            ..SavedAnalysisResp::default()
        },
    })
}

/// `GET /api/vk/dialogs/:id/analysis` — последний сохранённый разбор и тред
/// вопросов.
///
/// Ни одного вызова VK — как и у `GET /prompt`: разбор и тред лежат в том же
/// локальном файле. Именно поэтому попап показывает прошлый разбор и при
/// мёртвой сети, а история вопросов переживает перезагрузку страницы.
///
/// Разбора нет — `200` с пустым текстом, а не `404`: «пользователь ещё не
/// разбирал эту беседу» это нормальное состояние, а не отсутствующий ресурс.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`.
pub async fn get_analysis(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
) -> Result<Json<SavedAnalysisResp>, ApiError> {
    require_authorized(&state).await?;
    let peer = resolve_peer(&id)?;
    let key = peer.to_string();
    Ok(saved_resp(
        state.chat_analysis(&key).await,
        state.chat_thread(&key).await,
    ))
}

/// `DELETE /api/vk/dialogs/:id/analysis` — стереть сохранённый разбор.
///
/// Ни заметку, ни тред НЕ трогает: это три разные кнопки в попапе и три разные
/// величины в хранилище. Разбора не было — `200 {"cleared": false}`, а не
/// `404`.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `500 storage_failed`.
pub async fn delete_analysis(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
) -> Result<Json<ClearAnalysisResp>, ApiError> {
    require_authorized(&state).await?;
    let peer = resolve_peer(&id)?;
    let cleared = state
        .clear_chat_analysis(&peer.to_string())
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "forge_vk",
                error = %e,
                "forge-vk: could not clear the chat analysis"
            );
            ApiError::storage_failed(format!("could not clear the saved analysis: {e}"))
        })?;
    Ok(Json(ClearAnalysisResp { cleared }))
}

/// `DELETE /api/vk/dialogs/:id/analysis/thread` — стереть тред вопросов
/// («CLEAR Q&A»).
///
/// Ни разбор, ни заметку НЕ трогает — ровно так же, как `DELETE /analysis` не
/// трогает тред. Пустого треда (и неизвестной беседы) достаточно для
/// `200 {"cleared": false}`: кнопку жмут повторно и из двух вкладок сразу, и
/// `404` здесь был бы ошибкой на месте нормального состояния.
///
/// Ступень «соединение» отсутствует по той же причине, что у `GET /analysis`:
/// тред живёт в локальном файле, VK для его очистки не нужен.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `500 storage_failed`.
pub async fn delete_thread(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
) -> Result<Json<ClearThreadResp>, ApiError> {
    require_authorized(&state).await?;
    let peer = resolve_peer(&id)?;
    let cleared = state.clear_chat_thread(&peer.to_string()).await.map_err(|e| {
        tracing::warn!(
            target: "forge_vk",
            error = %e,
            "forge-vk: could not clear the chat q&a thread"
        );
        ApiError::storage_failed(format!("could not clear the saved q&a thread: {e}"))
    })?;
    Ok(Json(ClearThreadResp { cleared }))
}

// ---------------------------------------------------------------------------
// Запуск и отмена анализа.
// ---------------------------------------------------------------------------

/// `POST /api/vk/dialogs/:id/analysis` — запустить анализ или задать вопрос.
///
/// Порядок проверок — из шапки модуля, и он существенен: всё, что может
/// отказать, обязано отказать ДО выгрузки истории (до ~1.7 с запросов к VK).
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`,
/// `400 invalid_input` (нет тела, незнакомый `kind`, `ask` без вопроса),
/// `409 claude_unavailable` (бинаря CLI нет), `409 invalid_state` (заняты все
/// [`crate::analysis::MAX_PARALLEL_ANALYSIS`] слотов ДРУГИМИ беседами), плюс
/// любая ошибка VK при выгрузке истории — своим кодом.
pub async fn post_analysis(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    body: Option<Json<AnalysisReq>>,
) -> Result<Json<AnalysisResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let Some(Json(req)) = body else {
        return Err(ApiError::bad_input(
            "the request body must be a JSON object with a 'kind' field",
        ));
    };
    let dialog_id = peer.to_string();

    // Задача модели — раньше всего остального: незнакомый `kind` это опечатка
    // клиента, и она не должна стоить ни одного запроса к VK. Сохранённый
    // разбор читается здесь же: это локальный файл, а не сеть, и без него
    // `refresh` не имеет смысла.
    let saved = state.chat_analysis(&dialog_id).await;
    let task = build_task(&req, saved.as_ref())?;

    // Уже идущий run по этой беседе: отдаём его как есть, вместе с размерами
    // ЕГО транскрипта. Второй обход истории ради ответа «подключайся к тому
    // же» был бы чистой тратой лимита 3 rps.
    if let Some(run) = state.analysis.attached_run(&dialog_id).await {
        return Ok(Json(AnalysisResp {
            run_id: Some(run.run_id),
            attached: true,
            messages_used: run.messages_used,
            truncated: run.truncated,
            no_new_messages: false,
        }));
    }

    // `ClaudeRunner::new` отсутствие бинаря только warn'ит — этого мало:
    // без явной проверки пользователь получил бы `spawn failed` кадром
    // `analysis_error` уже после выгрузки истории, вместо инструкции.
    if !state.analysis.cli_available() {
        return Err(map_analysis_error(&state, &AnalysisError::CliUnavailable));
    }

    // Явный отказ, а не молчаливое ожидание на семафоре.
    if let Err(e) = state.analysis.has_free_slot().await {
        return Err(map_analysis_error(&state, &e));
    }

    let depth = context::clamp_depth(req.depth.unwrap_or(DEFAULT_DEPTH));
    // REFRESH выгружает ТОЛЬКО новое: в этом весь его смысл — и по деньгам
    // (в промпт едет десяток строк вместо двухсот), и по скорости (один вызов
    // VK вместо пяти).
    let (messages, mut transcript_truncated) = match &saved {
        Some(a) if matches!(task, Task::Refresh { .. }) => {
            let fresh =
                context::fetch_since(&state, &client, peer, a.last_message_id, depth).await?;
            (fresh.messages, fresh.truncated)
        }
        _ => (context::fetch(&state, &client, peer, depth).await?, false),
    };
    let transcript = context::render(&messages, state.config.analysis_max_chars);
    transcript_truncated |= transcript.truncated;

    // Обновлять нечего — и это штатный ответ, а не ошибка (см. шапку модуля).
    // Проверка идёт по ТРАНСКРИПТУ, а не по числу выгруженных сообщений:
    // сервисные записи без текста и вложений в него не попадают, и запускать
    // модель на пустом `<transcript>` значило бы заменить хороший разбор
    // ответом «ничего нового не произошло».
    if matches!(task, Task::Refresh { .. }) && transcript.messages_used == 0 {
        return Ok(Json(AnalysisResp {
            run_id: None,
            attached: false,
            messages_used: 0,
            truncated: false,
            no_new_messages: true,
        }));
    }

    // Имя беседы берётся из кэша, уже прогретого `context::fetch`
    // (`resolve_names` резолвит и сам peer). Пустое имя не беда — сборщик
    // промпта подставит «без названия».
    let title = state.peers.get(peer).map(|p| p.title).unwrap_or_default();
    let custom = state.chat_prompt(&dialog_id).await.map(|c| c.prompt);
    let prompt = prompt::build(&title, custom.as_deref(), &transcript, &task);

    let kind = match task {
        Task::Ask { .. } => KIND_ASK,
        Task::Refresh { .. } => KIND_REFRESH,
        Task::Analyze => KIND_ANALYZE,
    };
    // Вопрос берётся из УЖЕ разобранного `Task`, а не из тела запроса: там он
    // прошёл `trim` и проверку на пустоту, и именно в этом виде уехал модели —
    // в тред обязано попасть ровно то, что было спрошено.
    let question = match &task {
        Task::Ask { question, .. } => Some(question.clone()),
        _ => None,
    };
    let messages_used = u32::try_from(transcript.messages_used).unwrap_or(u32::MAX);

    let outcome = state
        .analysis
        .start(
            state.analysis_sink(),
            StartRequest {
                dialog_id: dialog_id.clone(),
                kind: kind.to_string(),
                prompt,
                messages_used,
                truncated: transcript_truncated,
                // Точка отсчёта следующего REFRESH: сохранится только по
                // успешному завершению run'а (см. шапку `analysis::hub`).
                newest_id: transcript.newest_id,
                question,
            },
        )
        .await
        .map_err(|e| {
            // Сюда попадаем только гонкой: слот заняли между проверкой и
            // стартом. Ответ тот же самый.
            map_analysis_error(&state, &e)
        })?;

    // Гонка в другую сторону: пока мы ходили в VK, run по этой беседе успел
    // начаться. Тогда отдаём размеры ЕГО транскрипта, а не своего — иначе
    // счётчик в попапе не сойдётся с текстом, который в него польётся.
    let (messages_used, truncated) = if outcome.attached() {
        match state.analysis.run(outcome.run_id()).await {
            Some(run) => (run.messages_used, run.truncated),
            None => (messages_used, transcript_truncated),
        }
    } else {
        (messages_used, transcript_truncated)
    };

    Ok(Json(AnalysisResp {
        run_id: Some(outcome.run_id().to_string()),
        attached: outcome.attached(),
        messages_used,
        truncated,
        no_new_messages: false,
    }))
}

/// `POST /api/vk/dialogs/:id/analysis/cancel` — остановить анализ.
///
/// Неизвестный (или уже завершившийся) run — НЕ ошибка: `200
/// {"cancelled": false}`. Гонка «нажал стоп ровно тогда, когда модель
/// дописала» происходит регулярно, и отвечать на неё `404` значило бы пугать
/// пользователя ошибкой там, где всё в порядке.
///
/// Финальный кадр `analysis_done{ok:false, reason:"cancelled"}` шлёт ИМЕННО
/// этот роут: пампер после abort'а уже мёртв и отправить ничего не может (см.
/// шапку [`crate::analysis::hub`]).
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`.
pub async fn post_analysis_cancel(
    State(state): State<Arc<VkState>>,
    Path(id): Path<String>,
    body: Option<Json<CancelReq>>,
) -> Result<Json<CancelResp>, ApiError> {
    require_ready(&state).await?;
    let peer = resolve_peer(&id)?;
    let dialog_id = peer.to_string();
    // Отсутствующее или битое тело здесь — НЕ ошибка (в отличие от `PUT
    // /prompt`): «останови то, что идёт по этой беседе» — полноценное и
    // однозначное намерение, а `run_id` вкладка могла потерять при
    // перезагрузке.
    let requested = body.and_then(|Json(b)| b.run_id).filter(|s| !s.is_empty());

    let run_id = match requested {
        Some(run_id) => state.analysis.cancel(&run_id).await.map(|_| run_id),
        None => state.analysis.cancel_dialog(&dialog_id).await,
    };

    let Some(run_id) = run_id else {
        return Ok(Json(CancelResp { cancelled: false }));
    };
    state.broadcast_event(VkServerMsg::AnalysisDone {
        run_id,
        dialog_id,
        ok: false,
        reason: Some(REASON_CANCELLED.to_string()),
        input_tokens: 0,
        output_tokens: 0,
    });
    Ok(Json(CancelResp { cancelled: true }))
}

/// Текст отказа «обновлять нечего» — один на оба пути (разбор `kind` и
/// перевод [`AnalysisError`]), чтобы формулировка не разъехалась между ними.
const NO_PREVIOUS_ANALYSIS: &str = "нечего обновлять: сначала выполните полный разбор этой беседы";

/// ЕДИНАЯ таблица перевода ошибок ядра анализа в конверт плагина.
///
/// Ядро (`forge-chat-analysis`) про HTTP не знает вовсе: оно общее на три
/// мессенджера, а коды и тексты у каждого свои — у VK они перечислены в шапке
/// [`crate::routes::error`] и в таблице `routes/mod.rs`. Соответствие ровно
/// такое, каким было до выноса ядра, и менять его нельзя: коды ответа —
/// публичный контракт вкладки.
///
/// Путь к бинарю берётся здесь, а не приезжает в ошибке: `Display` ошибки
/// уходит в том числе в логи, а путь — это кусок файловой системы машины.
/// Показать его в ответе — решение ПЛАГИНА (у VK это инструкция «поставьте
/// CLI сюда»).
fn map_analysis_error(state: &VkState, e: &AnalysisError) -> ApiError {
    match e {
        // 409 invalid_state: слоты заняты ДРУГИМИ беседами.
        AnalysisError::Busy(n) => ApiError::invalid_state(format!(
            "уже идёт {n} анализа(ов) по другим беседам; дождитесь завершения или остановите один"
        )),
        // 409 claude_unavailable — с путём и инструкцией.
        AnalysisError::CliUnavailable => {
            ApiError::claude_unavailable(state.analysis.cli_path().display())
        }
        // 400 invalid_input: REFRESH без сохранённого разбора.
        AnalysisError::NoPreviousAnalysis => ApiError::bad_input(NO_PREVIOUS_ANALYSIS),
        // 500 storage_failed: локальный файл заметок не записался.
        AnalysisError::Storage(msg) => ApiError::storage_failed(msg.clone()),
    }
}

/// Тело запроса → [`Task`], с проверкой `kind` и обязательности входных данных.
///
/// `saved` — сохранённый разбор беседы: он ОБЯЗАТЕЛЕН для `refresh` (обновлять
/// нечего) и не нужен остальным видам.
///
/// # Errors
///
/// `400 invalid_input`: `kind` вне закрытого списка, `ask` без вопроса либо
/// `refresh` без сохранённого разбора.
fn build_task(req: &AnalysisReq, saved: Option<&ChatAnalysis>) -> Result<Task, ApiError> {
    match req.kind.trim() {
        KIND_ANALYZE => Ok(Task::Analyze),
        KIND_REFRESH => {
            let previous = saved.ok_or_else(|| ApiError::bad_input(NO_PREVIOUS_ANALYSIS))?;
            Ok(Task::Refresh {
                previous: previous.text.clone(),
            })
        }
        KIND_ASK => {
            let question = req
                .question
                .as_deref()
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| {
                    ApiError::bad_input("'question' is required when kind is 'ask'")
                })?;
            let history = req
                .history
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|t| QaTurn {
                    q: t.q.clone(),
                    a: t.a.clone(),
                })
                .collect();
            Ok(Task::Ask {
                question: question.to_string(),
                history,
            })
        }
        other => Err(ApiError::bad_input(format!(
            "unknown analysis kind '{other}'; expected '{KIND_ANALYZE}', '{KIND_REFRESH}' \
             or '{KIND_ASK}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{SecretToken, VkConfig};
    use crate::login::LoginFlow;

    /// Авторизованное состояние с ЗАВЕДОМО МЁРТВОЙ сетью (порт 1 закрыт
    /// всегда) и изолированным файлом заметок.
    async fn authorized() -> (tempfile::TempDir, Arc<VkState>) {
        authorized_with(|_| {}).await
    }

    /// То же, но с правкой конфига — тестам анализа нужен свой
    /// `claude_cli_path`.
    async fn authorized_with(
        cfg: impl FnOnce(&mut VkConfig),
    ) -> (tempfile::TempDir, Arc<VkState>) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = VkConfig {
            access_token: SecretToken::new("vk1.a.TEST"),
            base_url: "http://127.0.0.1:1/method".into(),
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("vk.toml"),
            prompts_path: dir.path().join("prompts.json"),
            // По умолчанию — заведомо отсутствующий бинарь: тест, забывший
            // подменить путь, обязан упереться в `claude_unavailable`, а не
            // случайно запустить настоящий CLI разработчика.
            claude_cli_path: dir.path().join("no-such-claude"),
            ..VkConfig::default()
        };
        cfg(&mut c);
        let state = VkState::new(c);
        state
            .set_login(LoginFlow::Authorized {
                user: crate::login::VkUserDto {
                    id: "1234".into(),
                    ..Default::default()
                },
            })
            .await;
        (dir, state)
    }

    async fn call(state: &Arc<VkState>, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, serde_json::Value) {
        let req = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = super::super::build_router(state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    /// ФИКСАЦИЯ РЕШЕНИЯ ЛЕСТНИЦЫ: локальный файл не требует соединения.
    ///
    /// Состояние авторизовано, сеть мертва (`127.0.0.1:1`) — роут обязан
    /// ответить `200`, а не `503`. Добавленный когда-нибудь `require_ready`
    /// покрасит этот тест.
    #[tokio::test]
    async fn get_answers_200_with_a_dead_network() {
        let (_dir, state) = authorized().await;
        let (status, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/prompt", None).await;
        assert_eq!(status, StatusCode::OK, "локальный файл не требует сети");
        assert_eq!(json["prompt"], "");
        assert!(json["updated_at"].is_null());
        assert_eq!(json["max_chars"], MAX_PROMPT_CHARS);
    }

    #[tokio::test]
    async fn put_then_get_round_trips_through_the_file() {
        let (dir, state) = authorized().await;
        let (status, json) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/2000000001/prompt",
            Some(r#"{"prompt":"  Это заказчик, писать формально\r\n  "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "Это заказчик, писать формально");
        assert!(json["updated_at"].as_i64().unwrap() > 0);

        let (_, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/prompt", None).await;
        assert_eq!(json["prompt"], "Это заказчик, писать формально");

        // И это доехало до диска в согласованном виде.
        let store = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert_eq!(store.text("2000000001"), "Это заказчик, писать формально");
    }

    /// Правка второй беседы не теряет первую — приёмочный сценарий фазы.
    #[tokio::test]
    async fn a_second_chat_does_not_clobber_the_first() {
        let (dir, state) = authorized().await;
        for (id, text) in [("2000000001", "первый"), ("105431859", "второй")] {
            let (status, _) = call(
                &state,
                "PUT",
                &format!("/api/vk/dialogs/{id}/prompt"),
                Some(&format!(r#"{{"prompt":"{text}"}}"#)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let store = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert_eq!(store.text("2000000001"), "первый");
        assert_eq!(store.text("105431859"), "второй");
    }

    #[tokio::test]
    async fn an_empty_prompt_erases_the_note() {
        let (_dir, state) = authorized().await;
        call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(r#"{"prompt":"есть"}"#),
        )
        .await;

        let (status, json) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(r#"{"prompt":"   "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "");
        assert!(json["updated_at"].is_null(), "стёртая заметка не имеет времени");
        assert!(state.chat_prompt("1234").await.is_none());
    }

    /// Битое и отсутствующее тело — `400`, а НЕ «стереть заметку».
    #[tokio::test]
    async fn a_broken_body_is_rejected_and_keeps_the_note() {
        let (_dir, state) = authorized().await;
        call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(r#"{"prompt":"беречь"}"#),
        )
        .await;

        for body in [None, Some("{"), Some(r#"{"prompt":42}"#)] {
            let (status, json) = call(&state, "PUT", "/api/vk/dialogs/1234/prompt", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
            assert_eq!(json["error"]["code"], "invalid_input", "{body:?}");
        }
        assert_eq!(
            state.chat_prompt("1234").await.unwrap().prompt,
            "беречь",
            "оборванный запрос не стирает заметку"
        );
    }

    /// Разные написания одного id — ОДНА заметка (ключ канонический).
    #[tokio::test]
    async fn spelling_variants_of_an_id_share_one_note() {
        let (_dir, state) = authorized().await;
        call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(r#"{"prompt":"один"}"#),
        )
        .await;
        let (status, json) = call(&state, "GET", "/api/vk/dialogs/%201234%20/prompt", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "один", "пробелы вокруг id — та же беседа");
        assert_eq!(state.prompts.lock().await.chats.len(), 1);
    }

    /// Обрезка по лимиту происходит НА СЕРВЕРЕ и видна клиенту сразу.
    #[tokio::test]
    async fn an_overlong_prompt_comes_back_truncated() {
        let (_dir, state) = authorized().await;
        let long = "я".repeat(MAX_PROMPT_CHARS + 100);
        let (status, json) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(&serde_json::json!({ "prompt": long }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "кириллица не роняет хендлер");
        assert_eq!(
            json["prompt"].as_str().unwrap().chars().count(),
            MAX_PROMPT_CHARS
        );
    }

    /// Заметка — данные о конкретном человеке: без логина её не отдают и не
    /// принимают, причём ДО разбора чего бы то ни было.
    #[tokio::test]
    async fn both_routes_demand_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let state = VkState::new(VkConfig {
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("prompts.json"),
            ..VkConfig::default()
        });
        for (method, body) in [("GET", None), ("PUT", Some(r#"{"prompt":"x"}"#)), ("PUT", None)] {
            let (status, json) = call(&state, method, "/api/vk/dialogs/1234/prompt", body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{method}");
            assert_eq!(json["error"]["code"], "not_authorized", "{method}");
        }
        assert!(!dir.path().join("prompts.json").exists(), "файл не создан");
    }

    /// PUT-половина инварианта `hostile_dialog_ids_hit_auth_before_any_parsing`
    /// ([`super`] проверяет GET-половину).
    ///
    /// Враждебный `:id` ВМЕСТЕ с битым телом обязан дать `409 not_authorized`:
    /// это и есть причина, по которой тело объявлено `Option<Json<T>>` —
    /// экстрактор `Json<T>` отдал бы `400` раньше авторизации.
    #[tokio::test]
    async fn a_hostile_id_with_a_broken_body_still_hits_auth_first() {
        let dir = tempfile::tempdir().unwrap();
        let state = VkState::new(VkConfig {
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("prompts.json"),
            ..VkConfig::default()
        });
        for id in [
            "..%2F..%2Fetc%2Fpasswd",
            "+123",
            "0",
            "%F0%9F%98%80",
            "99999999999999999999999999",
        ] {
            let (status, json) = call(
                &state,
                "PUT",
                &format!("/api/vk/dialogs/{id}/prompt"),
                Some("{not json"),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{id}");
            assert_eq!(json["error"]["code"], "not_authorized", "{id}");
        }
    }

    /// Мусорный id у АВТОРИЗОВАННОГО клиента — `404`, и заметка не заводится.
    #[tokio::test]
    async fn a_garbage_id_is_404_for_an_authorized_client() {
        let (_dir, state) = authorized().await;
        for id in ["0", "+123", "abc", "%F0%9F%98%80"] {
            let (status, json) = call(
                &state,
                "PUT",
                &format!("/api/vk/dialogs/{id}/prompt"),
                Some(r#"{"prompt":"x"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
            assert_eq!(json["error"]["code"], "unknown_dialog", "{id}");
        }
        assert!(state.prompts.lock().await.chats.is_empty());
    }

    /// Отказ записи (каталог вместо файла) — `500 storage_failed`, а не
    /// паника и не молчаливый успех.
    #[tokio::test]
    async fn an_unwritable_store_answers_storage_failed() {
        let dir = tempfile::tempdir().unwrap();
        let busy = dir.path().join("prompts.json");
        std::fs::create_dir(&busy).unwrap();
        std::fs::write(busy.join("inhabitant"), b"x").unwrap();

        let state = VkState::new(VkConfig {
            access_token: SecretToken::new("vk1.a.TEST"),
            cache_dir: dir.path().join("cache"),
            prompts_path: busy,
            ..VkConfig::default()
        });
        state
            .set_login(LoginFlow::Authorized {
                user: crate::login::VkUserDto {
                    id: "1".into(),
                    ..Default::default()
                },
            })
            .await;

        let (status, json) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(r#"{"prompt":"CANARY-NOTE-TEXT"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"]["code"], "storage_failed");
        assert!(
            !json.to_string().contains("CANARY-NOTE-TEXT"),
            "текст заметки не уезжает в сообщение об ошибке: {json}"
        );
    }

    // -----------------------------------------------------------------------
    // POST /analysis и /analysis/cancel (АЧ-Ф3).
    // -----------------------------------------------------------------------

    /// Mock CLI, который висит: run остаётся активным столько, сколько нужно
    /// тесту, и запись в реестре не исчезает под ногами.
    const SLOW_CLI: &str = r#"#!/bin/sh
sleep 30
"#;

    fn mock_cli(dir: &tempfile::TempDir) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, SLOW_CLI).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Состояние с РАБОТАЮЩИМ (висящим) mock-CLI.
    async fn with_mock_cli() -> (tempfile::TempDir, Arc<VkState>) {
        let probe = tempfile::tempdir().unwrap();
        let cli = mock_cli(&probe);
        let (dir, state) = authorized_with(move |c| c.claude_cli_path = cli).await;
        // `probe` держит скрипт живым ровно столько же, сколько `dir`.
        std::mem::forget(probe);
        (dir, state)
    }

    /// Заводит активный run напрямую через hub — без сети и без выгрузки
    /// истории (её в тестах взять негде: `base_url` указывает в закрытый порт).
    async fn seed_run(state: &Arc<VkState>, dialog_id: &str, messages_used: u32) -> String {
        state
            .analysis
            .start(
                state.analysis_sink(),
                crate::analysis::StartRequest {
                    dialog_id: dialog_id.to_string(),
                    kind: KIND_ANALYZE.to_string(),
                    prompt: "<task>x</task>".into(),
                    messages_used,
                    truncated: true,
                    newest_id: Some(7042),
                    question: None,
                },
            )
            .await
            .unwrap()
            .run_id()
            .to_string()
    }

    /// Нет бинаря — `409 claude_unavailable` С ПУТЁМ, и это происходит ДО
    /// единого запроса к VK (сеть в тесте мертва: дошли бы до неё — получили
    /// бы 503).
    #[tokio::test]
    async fn analysis_without_the_claude_binary_is_refused_with_its_path() {
        let (dir, state) = authorized().await;
        let (status, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/2000000001/analysis",
            Some(r#"{"kind":"analyze"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "claude_unavailable");
        let message = json["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(dir.path().join("no-such-claude").to_str().unwrap()),
            "проверенный путь обязан быть в сообщении: {message}"
        );
        assert!(message.contains("claude_cli_path"), "{message}");
    }

    /// ДЕДУП: повторный POST по той же беседе отдаёт ТОТ ЖЕ `run_id` с
    /// `attached: true`, размеры транскрипта берёт у идущего run'а и НЕ
    /// запускает второго процесса (и, что тоже важно, не ходит в VK — иначе
    /// мёртвая сеть теста дала бы 503).
    #[tokio::test]
    async fn a_repeated_post_attaches_to_the_running_analysis() {
        let (_dir, state) = with_mock_cli().await;
        let run_id = seed_run(&state, "2000000001", 137).await;

        for _ in 0..2 {
            let (status, json) = call(
                &state,
                "POST",
                "/api/vk/dialogs/2000000001/analysis",
                Some(r#"{"kind":"analyze"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{json}");
            assert_eq!(json["run_id"], run_id);
            assert_eq!(json["attached"], true);
            assert_eq!(json["messages_used"], 137);
            assert_eq!(json["truncated"], true);
        }
        assert_eq!(state.analysis.active_count().await, 1);
        state.analysis.shutdown().await;
    }

    /// ЛИМИТ: третья беседа получает `409 invalid_state` — явный отказ вместо
    /// молчаливого ожидания, и снова ДО выгрузки истории.
    #[tokio::test]
    async fn a_third_dialog_is_refused_with_invalid_state() {
        let (_dir, state) = with_mock_cli().await;
        seed_run(&state, "1234", 10).await;
        seed_run(&state, "105431859", 10).await;

        let (status, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/2000000001/analysis",
            Some(r#"{"kind":"analyze"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{json}");
        assert_eq!(json["error"]["code"], "invalid_state");
        assert!(
            json["error"]["message"].as_str().unwrap().contains('2'),
            "в отказе обязано быть число идущих анализов: {json}"
        );
        state.analysis.shutdown().await;
    }

    /// Незнакомый `kind` и `ask` без вопроса — `400`, тоже без сети.
    #[tokio::test]
    async fn a_malformed_analysis_body_is_400() {
        let (_dir, state) = with_mock_cli().await;
        for body in [
            None,
            Some("{ битый json"),
            Some(r#"{"kind":"summarize"}"#),
            Some(r#"{"kind":""}"#),
            Some(r#"{"kind":"ask"}"#),
            Some(r#"{"kind":"ask","question":"   "}"#),
        ] {
            let (status, json) =
                call(&state, "POST", "/api/vk/dialogs/2000000001/analysis", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?} → {json}");
            assert_eq!(json["error"]["code"], "invalid_input", "{body:?}");
        }
        assert_eq!(state.analysis.active_count().await, 0);
    }

    /// Отмена неизвестного run'а — НЕ ошибка: гонка «нажал стоп ровно когда
    /// модель дописала» штатна.
    #[tokio::test]
    async fn cancelling_an_unknown_run_answers_200() {
        let (_dir, state) = with_mock_cli().await;
        for body in [
            None,
            Some("{ битый json"),
            Some(r#"{"run_id":"нет такого"}"#),
            Some("{}"),
        ] {
            let (status, json) = call(
                &state,
                "POST",
                "/api/vk/dialogs/2000000001/analysis/cancel",
                body,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body:?} → {json}");
            assert_eq!(json["cancelled"], false, "{body:?}");
        }
    }

    /// Отмена гасит run И САМА шлёт финальный кадр: пампер после abort'а мёртв
    /// и отправить `analysis_done` уже не может.
    #[tokio::test]
    async fn cancel_stops_the_run_and_broadcasts_the_done_frame() {
        let (_dir, state) = with_mock_cli().await;
        let run_id = seed_run(&state, "2000000001", 5).await;
        let mut rx = state.subscribe();

        let (status, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/2000000001/analysis/cancel",
            Some(&format!(r#"{{"run_id":"{run_id}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["cancelled"], true);
        assert_eq!(state.analysis.active_count().await, 0);

        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("кадр отмены не пришёл")
            .unwrap();
        match frame {
            VkServerMsg::AnalysisDone {
                run_id: got, ok, reason, ..
            } => {
                assert_eq!(got, run_id);
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some(REASON_CANCELLED));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Тело `cancel` необязательно: «останови то, что идёт по этой беседе» —
    /// полноценное намерение, а `run_id` вкладка теряет при перезагрузке.
    #[tokio::test]
    async fn cancel_without_a_run_id_stops_the_dialogs_run() {
        let (_dir, state) = with_mock_cli().await;
        let run_id = seed_run(&state, "2000000001", 5).await;
        let (status, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/2000000001/analysis/cancel",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["cancelled"], true);
        assert!(state.analysis.run(&run_id).await.is_none());
    }

    /// Мусорный `:id` у авторизованного клиента — `404`, и ни один run при
    /// этом не заводится.
    #[tokio::test]
    async fn a_garbage_id_never_starts_an_analysis() {
        let (_dir, state) = with_mock_cli().await;
        for id in ["0", "+123", "abc", "%F0%9F%98%80"] {
            for tail in ["analysis", "analysis/cancel"] {
                let (status, json) = call(
                    &state,
                    "POST",
                    &format!("/api/vk/dialogs/{id}/{tail}"),
                    Some(r#"{"kind":"analyze"}"#),
                )
                .await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{id}/{tail}");
                assert_eq!(json["error"]["code"], "unknown_dialog", "{id}/{tail}");
            }
        }
        assert_eq!(state.analysis.active_count().await, 0);
    }

    // -----------------------------------------------------------------------
    // Сохранённый разбор: GET / DELETE / kind=refresh.
    // -----------------------------------------------------------------------

    /// GET и DELETE стоят на лестнице `/prompt`: сеть мертва — ответ всё равно
    /// `200`. Разбора нет — пустая форма, а не `404`.
    #[tokio::test]
    async fn saved_analysis_routes_answer_200_with_a_dead_network() {
        let (_dir, state) = authorized().await;

        let (status, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(status, StatusCode::OK, "локальный файл не требует сети");
        assert_eq!(json["text"], "");
        assert!(json["last_message_id"].is_null());
        assert_eq!(json["messages_used"], 0);
        assert!(json["updated_at"].is_null());
        assert_eq!(json["max_chars"], MAX_ANALYSIS_CHARS);

        let (status, json) =
            call(&state, "DELETE", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["cleared"], false, "стирать было нечего — и это не ошибка");
    }

    /// Сохранённый разбор доезжает до клиента и стирается DELETE'ом, НЕ уводя
    /// с собой заметку: в попапе это две разные кнопки.
    #[tokio::test]
    async fn a_saved_analysis_round_trips_and_delete_spares_the_note() {
        let (_dir, state) = authorized().await;
        state
            .set_chat_prompt("2000000001", "это заказчик", 5)
            .await
            .unwrap();
        state
            .set_chat_analysis("2000000001", "1. О чём разговор — про смету.", 7042, 118, 1_800_000_000)
            .await
            .unwrap();

        let (status, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["text"], "1. О чём разговор — про смету.");
        assert_eq!(json["last_message_id"], 7042);
        assert_eq!(json["messages_used"], 118);
        assert_eq!(json["updated_at"], 1_800_000_000);

        let (status, json) =
            call(&state, "DELETE", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["cleared"], true);
        assert!(state.chat_analysis("2000000001").await.is_none());
        assert_eq!(
            state.chat_prompt("2000000001").await.unwrap().prompt,
            "это заказчик",
            "очистка разбора не имеет права стереть заметку"
        );

        let (_, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(json["text"], "");
    }

    // -----------------------------------------------------------------------
    // Тред вопросов: GET отдаёт, DELETE .../thread стирает, и ни одна очистка
    // не задевает соседние половины записи.
    // -----------------------------------------------------------------------

    /// Хранилище с заметкой, разбором и двумя парами треда — общий фон
    /// сценариев независимости.
    async fn seed_all_three(state: &Arc<VkState>, id: &str) {
        state.set_chat_prompt(id, "это заказчик", 5).await.unwrap();
        state
            .set_chat_analysis(id, "1. О чём разговор — про смету.", 7042, 118, 10)
            .await
            .unwrap();
        {
            let mut guard = state.prompts.lock().await;
            guard.push_turn(id, "а что со сроками?", "к пятнице", 11);
            guard.push_turn(id, "точно?", "да", 12);
            crate::analysis::prompts::save(&state.config.prompts_path, &guard).unwrap();
        }
    }

    /// Тред приезжает в `GET /analysis` целиком и в порядке «от старых».
    /// Пустой тред — пустой МАССИВ, а не отсутствие поля.
    #[tokio::test]
    async fn get_analysis_carries_the_thread() {
        let (_dir, state) = authorized().await;
        let (_, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(json["thread"], serde_json::json!([]), "пустой тред — массив");

        seed_all_three(&state, "2000000001").await;
        let (status, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["thread"],
            serde_json::json!([
                { "q": "а что со сроками?", "a": "к пятнице", "at": 11 },
                { "q": "точно?", "a": "да", "at": 12 },
            ])
        );
    }

    /// `DELETE /analysis/thread` стоит на лестнице `/prompt`: сеть мертва —
    /// `200`. Треда не было (и беседы в файле тоже) — `cleared: false`, а не
    /// `404` и не `500`.
    #[tokio::test]
    async fn delete_thread_is_idempotent_and_needs_no_network() {
        let (_dir, state) = authorized().await;
        for _ in 0..2 {
            let (status, json) = call(
                &state,
                "DELETE",
                "/api/vk/dialogs/2000000001/analysis/thread",
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "локальный файл не требует сети: {json}");
            assert_eq!(json["cleared"], false);
        }
    }

    /// ГЛАВНОЕ ТРЕБОВАНИЕ ДОРАБОТКИ: две кнопки очистки независимы, и это
    /// проверяется ПОСЛЕ реального round-trip через файл — молчаливая потеря
    /// случается именно на записи.
    #[tokio::test]
    async fn the_two_clears_never_touch_each_other_nor_the_note() {
        // 1. CLEAR анализа не трогает тред и заметку.
        let (dir, state) = authorized().await;
        seed_all_three(&state, "2000000001").await;
        let (status, json) =
            call(&state, "DELETE", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["cleared"], true);

        let (_, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(json["text"], "", "разбор стёрт");
        assert_eq!(json["thread"].as_array().unwrap().len(), 2, "тред уцелел");
        let (_, note) = call(&state, "GET", "/api/vk/dialogs/2000000001/prompt", None).await;
        assert_eq!(note["prompt"], "это заказчик");

        // И это состояние ФАЙЛА, а не только памяти.
        let from_disk = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert!(from_disk.analysis("2000000001").is_none());
        assert_eq!(from_disk.thread("2000000001").len(), 2);
        assert_eq!(from_disk.text("2000000001"), "это заказчик");

        // 2. CLEAR Q&A не трогает разбор и заметку.
        let (dir, state) = authorized().await;
        seed_all_three(&state, "2000000001").await;
        let (status, json) = call(
            &state,
            "DELETE",
            "/api/vk/dialogs/2000000001/analysis/thread",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["cleared"], true);

        let (_, json) = call(&state, "GET", "/api/vk/dialogs/2000000001/analysis", None).await;
        assert_eq!(json["thread"], serde_json::json!([]), "тред стёрт");
        assert_eq!(json["text"], "1. О чём разговор — про смету.", "разбор уцелел");
        let (_, note) = call(&state, "GET", "/api/vk/dialogs/2000000001/prompt", None).await;
        assert_eq!(note["prompt"], "это заказчик");

        let from_disk = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert!(from_disk.thread("2000000001").is_empty());
        assert_eq!(from_disk.analysis("2000000001").unwrap().last_message_id, 7042);

        // 3. Обе очистки подряд, в ОБОИХ порядках — остаётся только заметка.
        for order in [
            ["/analysis", "/analysis/thread"],
            ["/analysis/thread", "/analysis"],
        ] {
            let (dir, state) = authorized().await;
            seed_all_three(&state, "2000000001").await;
            for tail in order {
                let (status, _) = call(
                    &state,
                    "DELETE",
                    &format!("/api/vk/dialogs/2000000001{tail}"),
                    None,
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{tail}");
            }
            let from_disk = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
            assert!(from_disk.analysis("2000000001").is_none(), "{order:?}");
            assert!(from_disk.thread("2000000001").is_empty(), "{order:?}");
            assert_eq!(from_disk.text("2000000001"), "это заказчик", "{order:?}");
        }
    }

    /// Третья кнопка — стирание заметки — тоже никого не задевает: пустой
    /// `PUT /prompt` не имеет права унести с собой разбор и тред.
    #[tokio::test]
    async fn erasing_the_note_keeps_the_analysis_and_the_thread() {
        let (dir, state) = authorized().await;
        seed_all_three(&state, "2000000001").await;

        let (status, json) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/2000000001/prompt",
            Some(r#"{"prompt":"   "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "");

        let from_disk = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert_eq!(from_disk.text("2000000001"), "", "заметка стёрта");
        assert_eq!(from_disk.thread("2000000001").len(), 2, "тред уцелел");
        assert_eq!(
            from_disk.analysis("2000000001").unwrap().last_message_id,
            7042,
            "разбор уцелел"
        );
    }

    /// Непишущийся файл на очистке треда — `500 storage_failed`, как и у
    /// соседних роутов, без утечки текста в сообщение.
    #[tokio::test]
    async fn an_unwritable_store_fails_the_thread_clear() {
        let dir = tempfile::tempdir().unwrap();
        let busy = dir.path().join("prompts.json");
        std::fs::create_dir(&busy).unwrap();
        std::fs::write(busy.join("inhabitant"), b"x").unwrap();

        let state = VkState::new(VkConfig {
            access_token: SecretToken::new("vk1.a.TEST"),
            cache_dir: dir.path().join("cache"),
            prompts_path: busy,
            ..VkConfig::default()
        });
        state
            .set_login(LoginFlow::Authorized {
                user: crate::login::VkUserDto {
                    id: "1".into(),
                    ..Default::default()
                },
            })
            .await;
        state
            .prompts
            .lock()
            .await
            .push_turn("1234", "CANARY-QUESTION", "CANARY-ANSWER", 10);

        let (status, json) = call(&state, "DELETE", "/api/vk/dialogs/1234/analysis/thread", None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{json}");
        assert_eq!(json["error"]["code"], "storage_failed");
        let body = json.to_string();
        assert!(
            !body.contains("CANARY-QUESTION") && !body.contains("CANARY-ANSWER"),
            "переписка не уезжает в сообщение об ошибке: {json}"
        );
    }

    /// REFRESH без сохранённого разбора — `400 invalid_input`, и это
    /// происходит ДО единого запроса к VK (сеть в тесте мертва).
    #[tokio::test]
    async fn refresh_without_a_saved_analysis_is_400() {
        let (_dir, state) = with_mock_cli().await;
        let (status, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/2000000001/analysis",
            Some(r#"{"kind":"refresh"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["error"]["code"], "invalid_input");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("полный разбор"),
            "отказ обязан объяснять, что делать: {json}"
        );
        assert_eq!(state.analysis.active_count().await, 0);
    }

    /// Уже идущий run отдаётся `run_id`-ом СТРОКОЙ, а не `null`: поле стало
    /// `Option` только ради `no_new_messages`.
    #[tokio::test]
    async fn an_attached_run_still_carries_a_run_id() {
        let (_dir, state) = with_mock_cli().await;
        let run_id = seed_run(&state, "2000000001", 137).await;
        let (status, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/2000000001/analysis",
            Some(r#"{"kind":"analyze"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["run_id"], run_id);
        assert_eq!(json["no_new_messages"], false);
        state.analysis.shutdown().await;
    }

    /// Разбор тела в [`Task`] — чистый и проверяется отдельно от HTTP.
    #[test]
    fn build_task_maps_kinds_and_clamps_nothing_silently() {
        assert_eq!(
            build_task(
                &AnalysisReq {
                    kind: "analyze".into(),
                    ..AnalysisReq::default()
                },
                None
            )
            .unwrap(),
            Task::Analyze
        );
        let ask = build_task(
            &AnalysisReq {
                kind: " ask ".into(),
                question: Some("  что решили?  ".into()),
                history: Some(vec![QaTurnDto {
                    q: "кто это".into(),
                    a: "заказчик".into(),
                }]),
                ..AnalysisReq::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            ask,
            Task::Ask {
                question: "что решили?".into(),
                history: vec![QaTurn {
                    q: "кто это".into(),
                    a: "заказчик".into()
                }],
            }
        );
        assert!(
            build_task(&AnalysisReq::default(), None).is_err(),
            "пустой kind"
        );

        // REFRESH существует только поверх сохранённого разбора.
        let refresh = AnalysisReq {
            kind: "refresh".into(),
            ..AnalysisReq::default()
        };
        assert!(build_task(&refresh, None).is_err(), "нечего обновлять");
        let saved = ChatAnalysis {
            text: "прошлый разбор".into(),
            last_message_id: 7042,
            messages_used: 118,
            updated_at: 10,
        };
        assert_eq!(
            build_task(&refresh, Some(&saved)).unwrap(),
            Task::Refresh {
                previous: "прошлый разбор".into()
            }
        );
    }

    // -----------------------------------------------------------------------
    // ИНВАРИАНТЫ УТЕЧКИ (АЧ-Ф6).
    //
    // Две разные тайны живут рядом и НЕ ИМЕЮТ ПРАВА встретиться:
    //   * токен доступа VK — секрет входа, он не должен оказаться ни в файле
    //     заметок, ни в одном ответе новых роутов;
    //   * текст заметки и транскрипт переписки — приватные данные о живых
    //     людях, они не должны оказаться в логах.
    //
    // Тесты ниже краснеют ровно тогда, когда кто-нибудь вернёт «удобное»
    // логирование вида `?req` / `%prompt` или начнёт складывать конфиг
    // целиком рядом с заметками.
    // -----------------------------------------------------------------------

    /// Токен-канарейка: длинный и ни на что не похожий, чтобы подстрока
    /// находилась однозначно.
    const TOKEN_CANARY: &str = "vk1.a.SECRET-CANARY_0123456789abcdef";

    /// Ни файл заметок, ни ответы четырёх новых роутов не содержат токен VK.
    #[tokio::test]
    async fn the_prompts_file_and_the_new_routes_never_carry_the_vk_token() {
        let (dir, state) = authorized_with(|c| {
            c.access_token = SecretToken::new(TOKEN_CANARY);
        })
        .await;
        let path = dir.path().join("prompts.json");

        let mut bodies = Vec::new();
        let (st, json) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(r#"{"prompt":"Это чат по проекту X"}"#),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        bodies.push(json.to_string());

        let (_, json) = call(&state, "GET", "/api/vk/dialogs/1234/prompt", None).await;
        bodies.push(json.to_string());

        // Сеть мертва, CLI отсутствует — интересен не код ответа, а его тело.
        let (_, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/1234/analysis",
            Some(r#"{"kind":"analyze"}"#),
        )
        .await;
        bodies.push(json.to_string());

        let (_, json) = call(
            &state,
            "POST",
            "/api/vk/dialogs/1234/analysis/cancel",
            Some(r#"{"run_id":"nope"}"#),
        )
        .await;
        bodies.push(json.to_string());

        for body in &bodies {
            assert!(
                !body.contains(TOKEN_CANARY) && !body.contains("SECRET-CANARY"),
                "токен VK утёк в ответ роута: {body}"
            );
        }

        let file = std::fs::read_to_string(&path).expect("файл заметок обязан существовать");
        assert!(
            file.contains("Это чат по проекту X"),
            "заметка обязана сохраниться: {file}"
        );
        assert!(
            !file.contains(TOKEN_CANARY) && !file.contains("SECRET-CANARY"),
            "токен VK утёк в файл заметок: {file}"
        );
    }

    /// Текст заметки не попадает в логи НИ НА ОДНОМ пути: ни при сохранении,
    /// ни при чтении, ни при перезагрузке файла. Логируется только длина.
    #[tokio::test]
    async fn the_note_text_never_reaches_the_logs() {
        const NOTE_CANARY: &str = "КАНАРЕЙКА-ЗАМЕТКИ-77 Анна — заказчик";
        let (dir, state) = authorized().await;

        let mark = crate::testlog::mark();
        let (st, _) = call(
            &state,
            "PUT",
            "/api/vk/dialogs/1234/prompt",
            Some(&serde_json::json!({ "prompt": NOTE_CANARY }).to_string()),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, json) = call(&state, "GET", "/api/vk/dialogs/1234/prompt", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(json["prompt"], NOTE_CANARY, "по HTTP заметка отдаётся целиком");

        // И отдельный путь: чтение файла с нуля другим экземпляром хранилища.
        let reloaded = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert_eq!(
            reloaded.chats.get("1234").map(|c| c.prompt.as_str()),
            Some(NOTE_CANARY)
        );

        let logged = crate::testlog::since(mark);
        assert!(
            !logged.contains("КАНАРЕЙКА-ЗАМЕТКИ-77") && !logged.contains("Анна"),
            "текст заметки утёк в логи:\n{logged}"
        );
        assert!(
            logged.contains("chars=") || logged.contains("chats="),
            "в логах обязаны остаться только размеры:\n{logged}"
        );
    }
}
