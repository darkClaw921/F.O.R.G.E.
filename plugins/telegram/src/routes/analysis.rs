//! Роуты анализа чата: пер-чатовый контекст и запуск разбора через Claude.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET | `/api/telegram/dialogs/:id/prompt` | заметка пользователя к диалогу |
//! | PUT | `/api/telegram/dialogs/:id/prompt` | сохранить заметку (пустая строка — стереть) |
//! | GET | `/api/telegram/dialogs/:id/analysis` | ПОСЛЕДНИЙ сохранённый разбор + тред вопросов |
//! | DELETE | `/api/telegram/dialogs/:id/analysis` | стереть сохранённый разбор (тред и заметка целы) |
//! | POST | `/api/telegram/dialogs/:id/analysis` | запустить анализ, обновление либо вопрос |
//! | DELETE | `/api/telegram/dialogs/:id/analysis/thread` | стереть тред вопросов (разбор и заметка целы) |
//! | POST | `/api/telegram/dialogs/:id/analysis/cancel` | остановить идущий анализ |
//!
//! # Почему у треда ОТДЕЛЬНЫЙ путь, а не `?thread=1`
//!
//! Типовой экстрактор `Query<T>` отрабатывает ДО тела хендлера и тем самым
//! отвечает `400` раньше проверки авторизации, то есть ломает лестницу (та же
//! причина, по которой тело здесь `Option<Json<T>>`). Отдельный путь не
//! требует ни парсинга, ни исключений в auth-ladder: это просто ещё один
//! маршрут на той же ступени, и сторожевые тесты
//! `hostile_dialog_ids_hit_auth_before_any_parsing` в [`super`] перебирают его
//! наравне с остальными.
//!
//! # ОДИН ПУТЬ, ДВЕ ЛЕСТНИЦЫ
//!
//! У `/analysis` разные методы стоят на РАЗНЫХ лестницах, и это не
//! непоследовательность: `GET`/`DELETE` работают с локальным файлом (как
//! `/prompt`), `POST` начинается с выгрузки истории из Telegram. Требовать
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
//! У `/prompt` ступени «соединение» НЕТ: заметка лежит в локальном файле, и
//! `503 not_connected` на чтение локального файла был бы враньём — вкладка
//! перестала бы открывать контекст ровно тогда, когда он нужнее всего
//! (MTProto отвалился, пользователь читает историю из кэша).
//!
//! У `POST /analysis` ступень «соединение» ОБЯЗАТЕЛЬНА и стоит первой:
//! анализ начинается с выгрузки истории через `client.iter_messages`, то есть
//! без Telegram не существует вовсе. Запускать процесс `claude`, чтобы через
//! полторы секунды выяснить, что транскрипт пустой, — это сожжённые токены и
//! бессмысленный ответ модели.
//!
//! # Резолв `:id` у Telegram АСИНХРОННЫЙ и даёт `PeerRef`
//!
//! В отличие от VK, где `resolve_peer` — чистая функция над строкой,
//! [`crate::peers::resolve_peer`] ходит в кэш, в сессию и (в крайнем случае) в
//! сеть. Ключом хранилища при этом служит НЕ `PeerRef`, а канонический
//! `dialog_id` — [`crate::dto::dialog_id_string`] от `peer.id`. Причина та же,
//! что у VK: `/dialogs/1234/prompt` и `/dialogs/%201234%20/prompt` (оба
//! валидны для [`crate::dto::parse_dialog_id`]) иначе завели бы ДВЕ разные
//! заметки на один диалог. Обратная конвертация обязана быть стабильной между
//! сессиями — иначе сохранённый разбор «терялся» бы после перезахода.
//!
//! # Порядок отказов у `POST /analysis` зафиксирован
//!
//! `require_ready` → `resolve_peer` → тело и `kind` → **уже идущий run по
//! этому диалогу** → наличие Claude CLI → свободный слот → выгрузка истории →
//! старт. Три последних проверки стоят ДО выгрузки не для красоты: выгрузка —
//! это до пяти RPC. Отказ «уже идёт два анализа» после полутора секунд
//! ожидания выглядит как поломка, а повторный клик по кнопке не имеет права
//! стоить второго обхода истории.
//!
//! # `kind: "refresh"` и ответ «новых сообщений нет»
//!
//! Обновление разбора требует ДВУХ вещей: сохранённого разбора (иначе
//! обновлять нечего — `400 invalid_input`) и хотя бы одного сообщения новее
//! него. Второго может не оказаться: пользователь нажал «обновить» дважды
//! подряд. Это НЕ ошибка, а штатный ответ
//! `200 {run_id: null, no_new_messages: true}` — модель при этом не
//! запускается вовсе. Отвечать здесь ошибкой значило бы окрашивать в красное
//! совершенно нормальное «всё уже разобрано», а запускать модель на пустом
//! транскрипте — жечь токены ради ответа «изменений нет» и, что хуже,
//! ЗАМЕНЯТЬ им сохранённый разбор.
//!
//! # Ответ уходит СРАЗУ, остальное — кадрами WS
//!
//! `POST /analysis` возвращает `{run_id, attached, messages_used, truncated}`
//! в тот момент, когда процесс только запущен. Текст модели приезжает кадрами
//! `analysis_delta` (см. [`crate::ws::protocol`]), финал — `analysis_done`.
//! Держать HTTP-соединение открытым на всю генерацию нельзя: она длится
//! минуты, а любой прокси между браузером и devforge оборвёт её раньше.
//!
//! # Почему тело — `Option<Json<T>>`, а не `Json<T>`
//!
//! Экстрактор `Json<T>` отрабатывает ДО тела хендлера, то есть отверг бы битый
//! JSON раньше проверки авторизации, — и превратил бы роут в оракул валидности
//! идентификаторов. `Option<Json<T>>` превращает неразобранное тело в `None`,
//! которое хендлер обрабатывает уже ПОСЛЕ лестницы.
//!
//! Отсутствующее и битое тело — это `400 invalid_input`, а НЕ «пустая
//! заметка». Стирание требует явного `{"prompt": ""}`: молча удалять текст
//! из-за оборванного запроса недопустимо.
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
use crate::peers;
use crate::routes::dialogs::{require_authorized, require_ready};
use crate::routes::error::ApiError;
use crate::state::TelegramState;
use crate::ws::protocol::TgServerMsg;

/// Ответ обоих роутов `/prompt`.
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

/// Тело `PUT /prompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptReq {
    /// Новый текст заметки. Пустая строка (или строка из одних пробелов) —
    /// стереть заметку.
    #[serde(default)]
    pub prompt: String,
}

/// Тело `POST /analysis`.
///
/// Все поля кроме `kind` необязательны: кнопка «Анализ» шлёт
/// `{"kind": "analyze"}` и ничего больше.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisReq {
    /// `analyze` — структурированный разбор, `refresh` — дочитать новое,
    /// `ask` — свободный вопрос.
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
    /// Был ли разбор, который стёрли. Отсутствие разбора — НЕ ошибка.
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
    /// Какой run гасить. `None` ⇒ тот, что идёт по этому диалогу — так кнопка
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
pub fn router() -> Router<Arc<TelegramState>> {
    // Имя параметра `:id` — то же, что у соседних роутов
    // `/api/telegram/dialogs/*`: axum паникует при сборке, если один сегмент
    // назван по-разному.
    Router::new()
        .route(
            "/api/telegram/dialogs/:id/prompt",
            get(get_prompt).put(put_prompt),
        )
        .route(
            "/api/telegram/dialogs/:id/analysis",
            post(post_analysis).get(get_analysis).delete(delete_analysis),
        )
        .route(
            "/api/telegram/dialogs/:id/analysis/cancel",
            post(post_analysis_cancel),
        )
        // Тот же уровень лестницы, что у `GET|DELETE /analysis`: локальный
        // файл, без `require_ready`.
        .route(
            "/api/telegram/dialogs/:id/analysis/thread",
            axum::routing::delete(delete_thread),
        )
}

/// Канонический ключ хранилища для уже отрезолвленного peer'а.
///
/// Одна строка, но вынесена намеренно: это ЕДИНСТВЕННОЕ место, где `PeerRef`
/// превращается в ключ файла заметок, и все семь хендлеров обязаны звать
/// именно его. Разъехавшийся ключ означал бы «разбор пропал после
/// перезахода», причём молча.
fn storage_key(peer: grammers_session::types::PeerRef) -> String {
    dto::dialog_id_string(peer.id)
}

/// Авторизация + резолв диалога — общая ступень «локальных» роутов.
///
/// Соединения НЕ требует: см. шапку модуля.
async fn local_ladder(
    state: &Arc<TelegramState>,
    id: &str,
) -> Result<String, ApiError> {
    require_authorized(state).await?;
    let peer = peers::resolve_peer(state, id).await?;
    Ok(storage_key(peer))
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

/// `GET /api/telegram/dialogs/:id/prompt` — заметка пользователя к диалогу.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `503 not_connected` (peer'а нет
/// в кэше и резолвить его нечем).
pub async fn get_prompt(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
) -> Result<Json<PromptResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    Ok(resp(state.chat_prompt(&key).await))
}

/// `PUT /api/telegram/dialogs/:id/prompt` — сохранить заметку.
///
/// Текст проходит `forge_chat_analysis::prompts::sanitize` (нормализация
/// переводов строк, вырезание управляющих символов, обрезка до
/// [`MAX_PROMPT_CHARS`] по code points), после чего файл переписывается
/// атомарно с правами `0600`. В ответ уходит то, что РЕАЛЬНО сохранено, — а не
/// то, что прислал клиент: обрезанный хвост обязан исчезнуть в textarea сразу,
/// а не при следующем открытии попапа.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `400 invalid_input` (нет тела
/// или оно не разобралось), `500 storage_failed`.
pub async fn put_prompt(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    body: Option<Json<PromptReq>>,
) -> Result<Json<PromptResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    let Some(Json(req)) = body else {
        return Err(ApiError::bad_input(
            "the request body must be a JSON object with a 'prompt' string",
        ));
    };

    let saved = state
        .set_chat_prompt(&key, &req.prompt, dto::now_unix())
        .await
        .map_err(|e| {
            // ТОЛЬКО ошибка: текст заметки — данные о конкретном человеке, и в
            // логи он не попадает ни при каком исходе.
            tracing::warn!(
                target: "forge_telegram",
                error = %e,
                "forge-telegram: could not persist the chat prompt"
            );
            ApiError::storage_failed(format!("could not save the chat context: {e}"))
        })?;

    tracing::debug!(
        target: "forge_telegram",
        dialog_id = %key,
        chars = saved.as_ref().map_or(0, |c| c.prompt.chars().count()),
        "forge-telegram: chat prompt updated (the text itself is never logged)"
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

/// `GET /api/telegram/dialogs/:id/analysis` — последний сохранённый разбор и
/// тред вопросов.
///
/// Ни одного вызова Telegram — как и у `GET /prompt`: разбор и тред лежат в том
/// же локальном файле. Именно поэтому попап показывает прошлый разбор и при
/// мёртвой сети, а история вопросов переживает перезагрузку страницы.
///
/// Разбора нет — `200` с пустым текстом, а не `404`: «пользователь ещё не
/// разбирал этот диалог» это нормальное состояние, а не отсутствующий ресурс.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `503 not_connected`.
pub async fn get_analysis(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
) -> Result<Json<SavedAnalysisResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    Ok(saved_resp(
        state.chat_analysis(&key).await,
        state.chat_thread(&key).await,
    ))
}

/// `DELETE /api/telegram/dialogs/:id/analysis` — стереть сохранённый разбор.
///
/// Ни заметку, ни тред НЕ трогает: это три разные кнопки в попапе и три разные
/// величины в хранилище. Разбора не было — `200 {"cleared": false}`, а не
/// `404`.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `503 not_connected`,
/// `500 storage_failed`.
pub async fn delete_analysis(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
) -> Result<Json<ClearAnalysisResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    let cleared = state.clear_chat_analysis(&key).await.map_err(|e| {
        tracing::warn!(
            target: "forge_telegram",
            error = %e,
            "forge-telegram: could not clear the chat analysis"
        );
        ApiError::storage_failed(format!("could not clear the saved analysis: {e}"))
    })?;
    Ok(Json(ClearAnalysisResp { cleared }))
}

/// `DELETE /api/telegram/dialogs/:id/analysis/thread` — стереть тред вопросов
/// («CLEAR Q&A»).
///
/// Ни разбор, ни заметку НЕ трогает — ровно так же, как `DELETE /analysis` не
/// трогает тред. Пустого треда достаточно для `200 {"cleared": false}`: кнопку
/// жмут повторно и из двух вкладок сразу, и `404` здесь был бы ошибкой на
/// месте нормального состояния.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `503 not_connected`,
/// `500 storage_failed`.
pub async fn delete_thread(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
) -> Result<Json<ClearThreadResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    let cleared = state.clear_chat_thread(&key).await.map_err(|e| {
        tracing::warn!(
            target: "forge_telegram",
            error = %e,
            "forge-telegram: could not clear the chat q&a thread"
        );
        ApiError::storage_failed(format!("could not clear the saved q&a thread: {e}"))
    })?;
    Ok(Json(ClearThreadResp { cleared }))
}

// ---------------------------------------------------------------------------
// Запуск и отмена анализа.
// ---------------------------------------------------------------------------

/// `POST /api/telegram/dialogs/:id/analysis` — запустить анализ или задать
/// вопрос.
///
/// Порядок проверок — из шапки модуля, и он существенен: всё, что может
/// отказать, обязано отказать ДО выгрузки истории.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`,
/// `400 invalid_input` (нет тела, незнакомый `kind`, `ask` без вопроса,
/// `refresh` без сохранённого разбора), `409 claude_unavailable`,
/// `409 invalid_state` (заняты все слоты ДРУГИМИ диалогами), плюс любая
/// ошибка Telegram при выгрузке истории — своим кодом.
pub async fn post_analysis(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    body: Option<Json<AnalysisReq>>,
) -> Result<Json<AnalysisResp>, ApiError> {
    let client = require_ready(&state).await?;
    let peer = peers::resolve_peer(&state, &id).await?;
    let Some(Json(req)) = body else {
        return Err(ApiError::bad_input(
            "the request body must be a JSON object with a 'kind' field",
        ));
    };
    let dialog_id = storage_key(peer);

    // Задача модели — раньше всего остального: незнакомый `kind` это опечатка
    // клиента, и она не должна стоить ни одного RPC. Сохранённый разбор
    // читается здесь же: это локальный файл, а не сеть, и без него `refresh`
    // не имеет смысла.
    let saved = state.chat_analysis(&dialog_id).await;
    let task = build_task(&req, saved.as_ref())?;

    // Уже идущий run по этому диалогу: отдаём его как есть, вместе с размерами
    // ЕГО транскрипта. Второй обход истории ради ответа «подключайся к тому же»
    // был бы чистой тратой RPC.
    if let Some(run) = state.analysis.attached_run(&dialog_id).await {
        return Ok(Json(AnalysisResp {
            run_id: Some(run.run_id),
            attached: true,
            messages_used: run.messages_used,
            truncated: run.truncated,
            no_new_messages: false,
        }));
    }

    // `ClaudeRunner::new` отсутствие бинаря только warn'ит — этого мало: без
    // явной проверки пользователь получил бы `spawn failed` кадром
    // `analysis_error` уже после выгрузки истории, вместо инструкции.
    if !state.analysis.cli_available() {
        return Err(map_analysis_error(&state, &AnalysisError::CliUnavailable));
    }

    // Явный отказ, а не молчаливое ожидание на семафоре.
    if let Err(e) = state.analysis.has_free_slot().await {
        return Err(map_analysis_error(&state, &e));
    }

    let depth = context::clamp_depth(req.depth.unwrap_or(DEFAULT_DEPTH));
    // REFRESH выгружает ТОЛЬКО новое: в этом весь его смысл — и по деньгам (в
    // промпт едет десяток строк вместо двухсот), и по скорости (один RPC
    // вместо пяти).
    let (messages, mut transcript_truncated) = match &saved {
        Some(a) if matches!(task, Task::Refresh { .. }) => {
            let fresh =
                context::fetch_since(&state, &client, peer, a.last_message_id, depth).await?;
            (fresh.messages, fresh.truncated)
        }
        _ => (
            context::fetch(&state, &client, peer, depth).await?,
            false,
        ),
    };
    let transcript = context::render(&messages, state.config.analysis_max_chars);
    transcript_truncated |= transcript.truncated;

    // Обновлять нечего — и это штатный ответ, а не ошибка (см. шапку модуля).
    // Проверка идёт по ТРАНСКРИПТУ, а не по числу выгруженных сообщений:
    // сервисные записи без текста и медиа в него не попадают, и запускать
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

    // Имя диалога берётся ИЗ УЖЕ ВЫГРУЖЕННЫХ сообщений, а не отдельным
    // запросом: кэш peer'ов (`state.peers`) хранит только `PeerRef` без
    // title'ов, а лишний RPC на каждый разбор — реальная цена за украшение
    // шапки промпта. Пустое имя не беда: сборщик подставит «без названия».
    let title = context::title_from_messages(&messages);
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
                // успешному завершению run'а.
                newest_id: transcript.newest_id,
                question,
            },
        )
        .await
        // Сюда попадаем только гонкой: слот заняли между проверкой и стартом.
        // Ответ тот же самый.
        .map_err(|e| map_analysis_error(&state, &e))?;

    // Гонка в другую сторону: пока мы ходили в Telegram, run по этому диалогу
    // успел начаться. Тогда отдаём размеры ЕГО транскрипта, а не своего —
    // иначе счётчик в попапе не сойдётся с текстом, который в него польётся.
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

/// `POST /api/telegram/dialogs/:id/analysis/cancel` — остановить анализ.
///
/// Неизвестный (или уже завершившийся) run — НЕ ошибка: `200
/// {"cancelled": false}`. Гонка «нажал стоп ровно тогда, когда модель
/// дописала» происходит регулярно, и отвечать на неё `404` значило бы пугать
/// пользователя ошибкой там, где всё в порядке.
///
/// Финальный кадр `analysis_done{ok:false, reason:"cancelled"}` шлёт ИМЕННО
/// этот роут: пампер после abort'а уже мёртв и отправить ничего не может.
///
/// # Errors
///
/// `409 not_authorized`, `503 not_connected`, `404 unknown_dialog`.
pub async fn post_analysis_cancel(
    State(state): State<Arc<TelegramState>>,
    Path(id): Path<String>,
    body: Option<Json<CancelReq>>,
) -> Result<Json<CancelResp>, ApiError> {
    require_ready(&state).await?;
    let peer = peers::resolve_peer(&state, &id).await?;
    let dialog_id = storage_key(peer);
    // Отсутствующее или битое тело здесь — НЕ ошибка (в отличие от
    // `PUT /prompt`): «останови то, что идёт по этому диалогу» — полноценное и
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
    state.broadcast_event(TgServerMsg::AnalysisDone {
        run_id,
        dialog_id,
        ok: false,
        reason: Some(REASON_CANCELLED.to_string()),
        input_tokens: 0,
        output_tokens: 0,
    });
    Ok(Json(CancelResp { cancelled: true }))
}

/// Текст отказа «обновлять нечего» — один на оба пути (разбор `kind` и перевод
/// [`AnalysisError`]), чтобы формулировка не разъехалась между ними.
const NO_PREVIOUS_ANALYSIS: &str = "нечего обновлять: сначала выполните полный разбор этого чата";

/// ЕДИНАЯ таблица перевода ошибок ядра анализа в конверт плагина.
///
/// Ядро (`forge-chat-analysis`) про HTTP не знает вовсе: оно общее на три
/// мессенджера, а коды и тексты у каждого свои. Соответствие ровно такое же,
/// как у VK и Bitrix, и менять его нельзя: коды ответа — публичный контракт
/// вкладки, а разбирает их ОДИН общий фронтовый модуль.
///
/// Путь к бинарю берётся здесь, а не приезжает в ошибке: `Display` ошибки
/// уходит в том числе в логи, а путь — это кусок файловой системы машины.
/// Показать его в ответе — решение ПЛАГИНА.
fn map_analysis_error(state: &TelegramState, e: &AnalysisError) -> ApiError {
    match e {
        // 409 invalid_state: слоты заняты ДРУГИМИ диалогами.
        AnalysisError::Busy(n) => ApiError::invalid_state(format!(
            "уже идёт {n} анализа(ов) по другим чатам; дождитесь завершения или остановите один"
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
/// `saved` — сохранённый разбор диалога: он ОБЯЗАТЕЛЕН для `refresh` (обновлять
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
                .ok_or_else(|| ApiError::bad_input("'question' is required when kind is 'ask'"))?;
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
    use grammers_session::types::{PeerAuth, PeerId, PeerRef};
    use tower::ServiceExt;

    use super::*;
    use crate::config::TelegramConfig;
    use crate::login::{LoginFlow, TgUserDto};

    /// Диалог, который резолвится БЕЗ сети: peer положен прямо в кэш.
    ///
    /// Это единственный способ пройти ступень `resolve_peer` в тестах — она у
    /// Telegram (в отличие от VK) асинхронная и в общем случае ходит в сессию
    /// и в сеть. Кэш стоит ПЕРВЫМ эшелоном ровно для таких случаев.
    const DIALOG: i64 = 777_000;

    fn peer_ref(raw: i64) -> PeerRef {
        PeerRef {
            id: PeerId::from_bot_api_dialog_id(raw).expect("валидный id"),
            auth: PeerAuth::from_hash(raw.wrapping_mul(31)),
        }
    }

    /// Изолированное состояние: сессия, кэш и файл заметок — в tempdir.
    ///
    /// `TelegramConfig::default()` указывает на РЕАЛЬНЫЙ
    /// `~/.forge/telegram-chat-prompts.json`, и тест, сохранивший заметку,
    /// затёр бы живые записи разработчика. `TempDir` возвращается наружу,
    /// чтобы каталог жил до конца теста.
    fn state_with(cfg: impl FnOnce(&mut TelegramConfig)) -> (tempfile::TempDir, Arc<TelegramState>) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = TelegramConfig {
            session_path: dir.path().join("telegram.session"),
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("prompts.json"),
            // По умолчанию — заведомо отсутствующий бинарь: тест, забывший
            // подменить путь, обязан упереться в `claude_unavailable`, а не
            // случайно запустить настоящий CLI разработчика.
            claude_cli_path: dir.path().join("no-such-claude"),
            ..TelegramConfig::default()
        };
        cfg(&mut c);
        (dir, TelegramState::new(c))
    }

    /// Авторизованное состояние с УЖЕ ОТРЕЗОЛВЛЕННЫМ диалогом в кэше.
    async fn authorized() -> (tempfile::TempDir, Arc<TelegramState>) {
        let (dir, state) = state_with(|_| {});
        state
            .set_login(LoginFlow::Authorized {
                user: TgUserDto {
                    id: "1".into(),
                    username: None,
                    first_name: None,
                    phone: None,
                },
            })
            .await;
        crate::peers::cache_peer(&state, peer_ref(DIALOG)).await;
        (dir, state)
    }

    async fn call(
        state: &Arc<TelegramState>,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
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

    fn url(tail: &str) -> String {
        format!("/api/telegram/dialogs/{DIALOG}{tail}")
    }

    // -----------------------------------------------------------------------
    // Лестница проверок.
    // -----------------------------------------------------------------------

    /// ФИКСАЦИЯ РЕШЕНИЯ ЛЕСТНИЦЫ: локальный файл не требует соединения.
    ///
    /// Состояние авторизовано, MTProto-рантайма НЕТ — «локальные» роуты обязаны
    /// ответить `200`, а не `503`. Добавленный когда-нибудь `require_ready`
    /// покрасит этот тест.
    #[tokio::test]
    async fn local_routes_answer_200_without_a_connection() {
        let (_dir, state) = authorized().await;

        let (status, json) = call(&state, "GET", &url("/prompt"), None).await;
        assert_eq!(status, StatusCode::OK, "заметка не требует сети");
        assert_eq!(json["prompt"], "");
        assert!(json["updated_at"].is_null());
        assert_eq!(json["max_chars"], MAX_PROMPT_CHARS);

        let (status, json) = call(&state, "GET", &url("/analysis"), None).await;
        assert_eq!(status, StatusCode::OK, "разбор не требует сети");
        assert_eq!(json["text"], "");
        assert!(json["last_message_id"].is_null());
        assert_eq!(json["max_chars"], MAX_ANALYSIS_CHARS);
        assert!(json["thread"].as_array().unwrap().is_empty(), "всегда массив");

        for (method, tail) in [("DELETE", "/analysis"), ("DELETE", "/analysis/thread")] {
            let (status, json) = call(&state, method, &url(tail), None).await;
            assert_eq!(status, StatusCode::OK, "{method} {tail}");
            assert_eq!(json["cleared"], false, "стирать было нечего — не ошибка");
        }
    }

    /// А вот `POST /analysis` и `/analysis/cancel` соединения ТРЕБУЮТ: разбор
    /// начинается с выгрузки истории, и без Telegram его не существует.
    #[tokio::test]
    async fn running_routes_demand_a_connection() {
        let (_dir, state) = authorized().await;
        for (uri, body) in [
            (url("/analysis"), Some(r#"{"kind":"analyze"}"#)),
            (url("/analysis/cancel"), Some("{}")),
        ] {
            let (status, json) = call(&state, "POST", &uri, body).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(json["error"]["code"], "not_connected", "{uri}");
        }
    }

    /// Заметки — записи пользователя о конкретных людях: без логина их не
    /// отдают и не принимают, причём ДО разбора чего бы то ни было.
    ///
    /// Это PUT-половина инварианта `hostile_dialog_ids_hit_auth_before_any_parsing`
    /// ([`super::super`] проверяет остальные роуты): враждебный `:id` ВМЕСТЕ с
    /// битым телом обязан дать `409 not_authorized`. Именно поэтому тело
    /// объявлено `Option<Json<T>>` — экстрактор `Json<T>` отдал бы `400`
    /// раньше авторизации и превратил бы роут в оракул валидности id.
    #[tokio::test]
    async fn every_analysis_route_hits_auth_before_any_parsing() {
        let (dir, state) = state_with(|_| {});
        let hostile = [
            "..%2F..%2Fetc%2Fpasswd",
            "+123",
            "0",
            "%F0%9F%98%80",
            "99999999999999999999999999",
            "%3Cscript%3Ealert(1)%3C%2Fscript%3E",
        ];
        for id in hostile {
            for (method, tail, body) in [
                ("GET", "/prompt", None),
                ("PUT", "/prompt", Some("{not json")),
                ("GET", "/analysis", None),
                ("POST", "/analysis", Some("{not json")),
                ("DELETE", "/analysis", None),
                ("POST", "/analysis/cancel", Some("{not json")),
                ("DELETE", "/analysis/thread", None),
            ] {
                let uri = format!("/api/telegram/dialogs/{id}{tail}");
                let (status, json) = call(&state, method, &uri, body).await;
                assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}");
                assert_eq!(json["error"]["code"], "not_authorized", "{method} {uri}");
            }
        }
        assert!(!dir.path().join("prompts.json").exists(), "файл не создан");
    }

    /// Мусорный id у АВТОРИЗОВАННОГО клиента — `404`, и заметка не заводится.
    ///
    /// `404`, а не `503`: ФОРМА id проверяется ДО состояния соединения
    /// (контракт [`crate::peers::resolve_peer`]), и «переподключись и повтори»
    /// тут не поможет.
    #[tokio::test]
    async fn a_garbage_id_is_404_for_an_authorized_client() {
        let (_dir, state) = authorized().await;
        for id in ["0", "-0", "abc", "%F0%9F%98%80", "99999999999999999999999999"] {
            let uri = format!("/api/telegram/dialogs/{id}/prompt");
            let (status, json) = call(&state, "PUT", &uri, Some(r#"{"prompt":"x"}"#)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
            assert_eq!(json["error"]["code"], "unknown_dialog", "{id}");
        }
        assert!(state.prompts.lock().await.chats.is_empty());
    }

    /// РАЗЛИЧИЕ С VK, которое легко принять за баг: `+123` — это НЕ мусор.
    ///
    /// У VK ведущий `+` отвергается разбором, и такой id даёт `404`. У Telegram
    /// [`crate::dto::parse_dialog_id`] разбирает строку через `i64::from_str`, а
    /// тот принимает явный знак, — то есть `+123` законное написание `123`.
    /// Поэтому ответ здесь `503 not_connected` («id понятен, но peer'а нет ни в
    /// кэше, ни откуда его достать»), а не `404`.
    ///
    /// Тест существует, чтобы разница между плагинами была ЗАФИКСИРОВАНА, а не
    /// «обнаружена» при следующем сравнении их лестниц.
    #[tokio::test]
    async fn a_signed_id_is_a_legal_spelling_not_garbage() {
        let (_dir, state) = authorized().await;
        let (status, json) = call(
            &state,
            "PUT",
            "/api/telegram/dialogs/+123/prompt",
            Some(r#"{"prompt":"x"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["error"]["code"], "not_connected");

        // И раз это то же число — кэшированный peer делает его резолвимым, а
        // ключ хранилища выходит КАНОНИЧЕСКИЙ, без знака.
        crate::peers::cache_peer(&state, peer_ref(123)).await;
        let (status, _) = call(
            &state,
            "PUT",
            "/api/telegram/dialogs/+123/prompt",
            Some(r#"{"prompt":"плюс"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(state.chat_prompt("123").await.unwrap().prompt, "плюс");
    }

    // -----------------------------------------------------------------------
    // Заметка к диалогу.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_then_get_round_trips_through_the_file() {
        let (dir, state) = authorized().await;
        let (status, json) = call(
            &state,
            "PUT",
            &url("/prompt"),
            Some(r#"{"prompt":"  Это заказчик, писать формально\r\n  "}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "Это заказчик, писать формально");
        assert!(json["updated_at"].as_i64().unwrap() > 0);

        let (_, json) = call(&state, "GET", &url("/prompt"), None).await;
        assert_eq!(json["prompt"], "Это заказчик, писать формально");

        // И это доехало до диска в согласованном виде.
        let store = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert_eq!(store.text(&DIALOG.to_string()), "Это заказчик, писать формально");
    }

    /// КЛЮЧ ХРАНИЛИЩА КАНОНИЧЕСКИЙ: разные написания одного id — ОДНА заметка.
    ///
    /// У Telegram `resolve_peer` отдаёт `PeerRef`, и ключом служит
    /// [`crate::dto::dialog_id_string`] от `peer.id`, а не сырой сегмент URL.
    /// Иначе `/dialogs/777000/prompt` и `/dialogs/%20777000%20/prompt` завели бы
    /// ДВЕ разные заметки на один диалог, и разбор «пропадал» бы после
    /// перезахода.
    #[tokio::test]
    async fn spelling_variants_of_an_id_share_one_note() {
        let (_dir, state) = authorized().await;
        call(&state, "PUT", &url("/prompt"), Some(r#"{"prompt":"один"}"#)).await;

        let (status, json) = call(
            &state,
            "GET",
            &format!("/api/telegram/dialogs/%20{DIALOG}%20/prompt"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "один", "пробелы вокруг id — тот же диалог");
        assert_eq!(state.prompts.lock().await.chats.len(), 1);
    }

    /// Резолв одного и того же диалога дважды даёт один и тот же ключ.
    #[tokio::test]
    async fn the_storage_key_is_stable_across_resolves() {
        let (_dir, state) = authorized().await;
        let a = super::storage_key(crate::peers::resolve_peer(&state, &DIALOG.to_string()).await.unwrap());
        let b = super::storage_key(
            crate::peers::resolve_peer(&state, &format!("  {DIALOG}  "))
                .await
                .unwrap(),
        );
        assert_eq!(a, b);
        assert_eq!(a, DIALOG.to_string());
    }

    #[tokio::test]
    async fn an_empty_prompt_erases_the_note() {
        let (_dir, state) = authorized().await;
        call(&state, "PUT", &url("/prompt"), Some(r#"{"prompt":"есть"}"#)).await;

        let (status, json) = call(&state, "PUT", &url("/prompt"), Some(r#"{"prompt":"   "}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["prompt"], "");
        assert!(json["updated_at"].is_null(), "стёртая заметка не имеет времени");
        assert!(state.chat_prompt(&DIALOG.to_string()).await.is_none());
    }

    /// Битое и отсутствующее тело — `400`, а НЕ «стереть заметку».
    #[tokio::test]
    async fn a_broken_body_is_rejected_and_keeps_the_note() {
        let (_dir, state) = authorized().await;
        call(&state, "PUT", &url("/prompt"), Some(r#"{"prompt":"беречь"}"#)).await;

        for body in [None, Some("{"), Some(r#"{"prompt":42}"#)] {
            let (status, json) = call(&state, "PUT", &url("/prompt"), body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
            assert_eq!(json["error"]["code"], "invalid_input", "{body:?}");
        }
        assert_eq!(
            state.chat_prompt(&DIALOG.to_string()).await.unwrap().prompt,
            "беречь",
            "оборванный запрос не стирает заметку"
        );
    }

    /// Обрезка по лимиту происходит НА СЕРВЕРЕ и видна клиенту сразу.
    #[tokio::test]
    async fn an_overlong_prompt_comes_back_truncated() {
        let (_dir, state) = authorized().await;
        let long = "я".repeat(MAX_PROMPT_CHARS + 100);
        let (status, json) = call(
            &state,
            "PUT",
            &url("/prompt"),
            Some(&serde_json::json!({ "prompt": long }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "кириллица не роняет хендлер");
        assert_eq!(
            json["prompt"].as_str().unwrap().chars().count(),
            MAX_PROMPT_CHARS
        );
    }

    /// Отказ записи (каталог вместо файла) — `500 storage_failed`, а не паника
    /// и не молчаливый успех. Текст заметки в ответ НЕ утекает.
    #[tokio::test]
    async fn an_unwritable_store_answers_storage_failed() {
        let dir = tempfile::tempdir().unwrap();
        let busy = dir.path().join("prompts.json");
        std::fs::create_dir(&busy).unwrap();
        std::fs::write(busy.join("inhabitant"), b"x").unwrap();

        let (_d, state) = state_with(|c| c.prompts_path = busy);
        state
            .set_login(LoginFlow::Authorized {
                user: TgUserDto {
                    id: "1".into(),
                    username: None,
                    first_name: None,
                    phone: None,
                },
            })
            .await;
        crate::peers::cache_peer(&state, peer_ref(DIALOG)).await;

        let (status, json) = call(
            &state,
            "PUT",
            &url("/prompt"),
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

    /// Правка второго диалога не теряет первый.
    #[tokio::test]
    async fn a_second_chat_does_not_clobber_the_first() {
        let (dir, state) = authorized().await;
        let other: i64 = 105_431_859;
        crate::peers::cache_peer(&state, peer_ref(other)).await;

        for (id, text) in [(DIALOG, "первый"), (other, "второй")] {
            let (status, _) = call(
                &state,
                "PUT",
                &format!("/api/telegram/dialogs/{id}/prompt"),
                Some(&serde_json::json!({ "prompt": text }).to_string()),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let store = crate::analysis::prompts::load(&dir.path().join("prompts.json"));
        assert_eq!(store.text(&DIALOG.to_string()), "первый");
        assert_eq!(store.text(&other.to_string()), "второй");
    }

    // -----------------------------------------------------------------------
    // Разбор, тред и очистки.
    // -----------------------------------------------------------------------

    /// CLEAR ✕ и CLEAR Q&A НЕЗАВИСИМЫ, и ни один из них не трогает заметку.
    ///
    /// Это три разные кнопки в попапе и три разные величины в хранилище —
    /// главное функциональное требование доработки треда.
    #[tokio::test]
    async fn clearing_the_analysis_and_the_thread_are_independent() {
        let (_dir, state) = authorized().await;
        let key = DIALOG.to_string();
        call(&state, "PUT", &url("/prompt"), Some(r#"{"prompt":"заметка"}"#)).await;
        {
            let mut guard = state.prompts.lock().await;
            guard.set_analysis(&key, "РАЗБОР", 7042, 137, 1_800_000_000);
            guard.push_turn(&key, "вопрос", "ответ", 1_800_000_001);
            crate::analysis::prompts::save(&state.config.prompts_path, &guard).unwrap();
        }

        // Всё на месте и приезжает одним ответом.
        let (_, json) = call(&state, "GET", &url("/analysis"), None).await;
        assert_eq!(json["text"], "РАЗБОР");
        assert_eq!(json["last_message_id"], 7042);
        assert_eq!(json["messages_used"], 137);
        assert_eq!(json["thread"][0]["q"], "вопрос");
        assert_eq!(json["thread"][0]["a"], "ответ");

        // Стираем ТРЕД: разбор и заметка целы.
        let (status, json) = call(&state, "DELETE", &url("/analysis/thread"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["cleared"], true);
        let (_, json) = call(&state, "GET", &url("/analysis"), None).await;
        assert_eq!(json["text"], "РАЗБОР", "разбор не тронут");
        assert!(json["thread"].as_array().unwrap().is_empty());
        assert_eq!(state.chat_prompt(&key).await.unwrap().prompt, "заметка");

        // Стираем РАЗБОР: заметка по-прежнему цела.
        let (status, json) = call(&state, "DELETE", &url("/analysis"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["cleared"], true);
        let (_, json) = call(&state, "GET", &url("/analysis"), None).await;
        assert_eq!(json["text"], "");
        assert_eq!(state.chat_prompt(&key).await.unwrap().prompt, "заметка");

        // Повторное нажатие — НЕ ошибка (две вкладки жмут подряд).
        for tail in ["/analysis", "/analysis/thread"] {
            let (status, json) = call(&state, "DELETE", &url(tail), None).await;
            assert_eq!(status, StatusCode::OK, "{tail}");
            assert_eq!(json["cleared"], false, "{tail}");
        }
    }

    // -----------------------------------------------------------------------
    // Разбор тела и перевод ошибок ядра.
    //
    // Через роут `POST /analysis` эти ветки в юнит-тесте недостижимы: у него
    // первой ступенью стоит `require_ready`, а живого MTProto-клиента в тесте
    // взять негде (см. `running_routes_demand_a_connection`). Поэтому логика
    // проверяется там, где она живёт, — в чистых функциях.
    // -----------------------------------------------------------------------

    fn saved(text: &str) -> ChatAnalysis {
        ChatAnalysis {
            text: text.into(),
            last_message_id: 7042,
            messages_used: 137,
            updated_at: 1_800_000_000,
        }
    }

    /// Незнакомый `kind`, `ask` без вопроса и `refresh` без разбора — `400`.
    #[test]
    fn a_malformed_analysis_body_is_400() {
        let cases: [(&str, Option<&str>, Option<ChatAnalysis>); 5] = [
            ("summarize", None, None),
            ("", None, None),
            ("ask", None, None),
            ("ask", Some("   "), None),
            ("refresh", None, None),
        ];
        for (kind, question, prev) in cases {
            let req = AnalysisReq {
                kind: kind.into(),
                question: question.map(str::to_string),
                ..AnalysisReq::default()
            };
            let err = build_task(&req, prev.as_ref()).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{kind} {question:?}");
            assert_eq!(err.body.code, "invalid_input", "{kind} {question:?}");
        }
    }

    /// Разобранные виды задач: `refresh` требует сохранённого разбора,
    /// `ask` — вопроса, и оба берут ТОЛЬКО обрезанное значение.
    #[test]
    fn build_task_accepts_the_three_kinds() {
        assert!(matches!(
            build_task(
                &AnalysisReq {
                    kind: " analyze ".into(),
                    ..AnalysisReq::default()
                },
                None
            )
            .unwrap(),
            Task::Analyze
        ));

        let prev = saved("ПРОШЛЫЙ РАЗБОР");
        match build_task(
            &AnalysisReq {
                kind: "refresh".into(),
                ..AnalysisReq::default()
            },
            Some(&prev),
        )
        .unwrap()
        {
            Task::Refresh { previous } => assert_eq!(previous, "ПРОШЛЫЙ РАЗБОР"),
            other => panic!("ожидался Refresh, получен {other:?}"),
        }

        match build_task(
            &AnalysisReq {
                kind: "ask".into(),
                question: Some("  что решили?  ".into()),
                history: Some(vec![QaTurnDto {
                    q: "а раньше?".into(),
                    a: "ничего".into(),
                }]),
                ..AnalysisReq::default()
            },
            None,
        )
        .unwrap()
        {
            Task::Ask { question, history } => {
                assert_eq!(question, "что решили?", "вопрос обрезан по краям");
                assert_eq!(history.len(), 1);
            }
            other => panic!("ожидался Ask, получен {other:?}"),
        }
    }

    /// НЕТ CLI ⇒ `409 claude_unavailable` С ПУТЁМ и с именем поля конфига.
    ///
    /// Без пути единственный способ понять, где искали бинарь, — читать логи
    /// сервера.
    #[tokio::test]
    async fn a_missing_claude_binary_maps_to_409_with_its_path() {
        let (dir, state) = state_with(|_| {});
        assert!(!state.analysis.cli_available(), "бинаря заведомо нет");

        let err = map_analysis_error(&state, &AnalysisError::CliUnavailable);
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.body.code, "claude_unavailable");
        assert!(
            err.body
                .message
                .contains(dir.path().join("no-such-claude").to_str().unwrap()),
            "проверенный путь обязан быть в сообщении: {}",
            err.body.message
        );
        assert!(err.body.message.contains("claude_cli_path"), "{}", err.body.message);
    }

    /// Остальные три ошибки ядра — те же коды, что у VK и Bitrix: их разбирает
    /// ОДИН общий фронтовый модуль.
    #[tokio::test]
    async fn core_errors_map_to_the_shared_codes() {
        let (_dir, state) = state_with(|_| {});
        let table: [(AnalysisError, StatusCode, &str); 3] = [
            (AnalysisError::Busy(2), StatusCode::CONFLICT, "invalid_state"),
            (
                AnalysisError::NoPreviousAnalysis,
                StatusCode::BAD_REQUEST,
                "invalid_input",
            ),
            (
                AnalysisError::Storage("disk full".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_failed",
            ),
        ];
        for (e, status, code) in table {
            let err = map_analysis_error(&state, &e);
            assert_eq!(err.status, status, "{code}");
            assert_eq!(err.body.code, code);
        }
        // В отказе «занято» обязано быть число идущих анализов.
        let busy = map_analysis_error(&state, &AnalysisError::Busy(2));
        assert!(busy.body.message.contains('2'), "{}", busy.body.message);
    }

    // -----------------------------------------------------------------------
    // Дедуп и лимит — на уровне хаба (сети они не требуют).
    // -----------------------------------------------------------------------

    /// Mock CLI, который висит: run остаётся активным столько, сколько нужно
    /// тесту, и запись в реестре не исчезает под ногами.
    const SLOW_CLI: &str = "#!/bin/sh\nsleep 30\n";

    fn mock_cli(dir: &tempfile::TempDir) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("mock-claude");
        std::fs::write(&path, SLOW_CLI).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    async fn seed_run(state: &Arc<TelegramState>, dialog_id: &str, messages_used: u32) -> String {
        state
            .analysis
            .start(
                state.analysis_sink(),
                StartRequest {
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

    /// ДЕДУП: второй старт по тому же диалогу отдаёт ТОТ ЖЕ run с размерами
    /// ЕГО транскрипта и НЕ запускает второго процесса.
    #[tokio::test]
    async fn a_second_start_attaches_to_the_running_analysis() {
        let probe = tempfile::tempdir().unwrap();
        let cli = mock_cli(&probe);
        let (_dir, state) = state_with(move |c| c.claude_cli_path = cli);

        let run_id = seed_run(&state, &DIALOG.to_string(), 137).await;
        let attached = state
            .analysis
            .attached_run(&DIALOG.to_string())
            .await
            .expect("идущий run обязан находиться по диалогу");
        assert_eq!(attached.run_id, run_id);
        assert_eq!(attached.messages_used, 137);
        assert!(attached.truncated);
        assert_eq!(state.analysis.active_count().await, 1);

        state.analysis.shutdown().await;
        assert_eq!(state.analysis.active_count().await, 0, "shutdown убил run");
    }

    /// ЛИМИТ: третий диалог получает `409 invalid_state` — явный отказ вместо
    /// молчаливого ожидания на семафоре.
    #[tokio::test]
    async fn a_third_dialog_is_refused_with_invalid_state() {
        let probe = tempfile::tempdir().unwrap();
        let cli = mock_cli(&probe);
        let (_dir, state) = state_with(move |c| c.claude_cli_path = cli);

        seed_run(&state, "1234", 10).await;
        seed_run(&state, "105431859", 10).await;

        let e = state
            .analysis
            .has_free_slot()
            .await
            .expect_err("свободных слотов быть не должно");
        let err = map_analysis_error(&state, &e);
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.body.code, "invalid_state");

        state.analysis.shutdown().await;
    }

    /// Отмена неизвестного run'а — НЕ ошибка: гонка «нажал стоп ровно когда
    /// модель дописала» штатна.
    #[tokio::test]
    async fn cancelling_an_unknown_run_is_not_an_error() {
        let (_dir, state) = state_with(|_| {});
        assert!(state.analysis.cancel("нет такого").await.is_none());
        assert!(state.analysis.cancel_dialog(&DIALOG.to_string()).await.is_none());
    }

    /// ОТМЕНА РЕАЛЬНО УБИВАЕТ ПРОЦЕСС — проверка по НАСТОЯЩЕМУ pid, а не по
    /// записи в реестре.
    ///
    /// Это пункт живой приёмки («после STOP `pgrep claude` пуст»), сведённый к
    /// воспроизводимому тесту. Реестр хаба может опустеть, а процесс остаться —
    /// именно так выглядит утечка, которую этот тест ловит: осиротевший
    /// `claude -p` продолжает жечь токены и держать файл заметок уже после
    /// того, как пользователь нажал «стоп».
    ///
    /// Mock CLI печатает свой pid и засыпает; тест читает pid из файла и после
    /// отмены убеждается, что процесса нет (`kill(pid, 0)` → ESRCH).
    #[tokio::test]
    async fn cancelling_a_run_actually_kills_the_process() {
        use std::os::unix::fs::PermissionsExt;

        let probe = tempfile::tempdir().unwrap();
        let pidfile = probe.path().join("pid");
        let cli = probe.path().join("mock-claude");
        std::fs::write(
            &cli,
            format!("#!/bin/sh\necho $$ > {}\nsleep 60\n", pidfile.display()),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&cli).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&cli, perms).unwrap();

        let cli_path = cli.clone();
        let (_dir, state) = state_with(move |c| c.claude_cli_path = cli_path);
        let run_id = seed_run(&state, &DIALOG.to_string(), 10).await;

        // Ждём, пока mock успеет записать pid (spawn асинхронный).
        let mut pid: Option<i32> = None;
        for _ in 0..100 {
            if let Ok(raw) = std::fs::read_to_string(&pidfile) {
                if let Ok(v) = raw.trim().parse::<i32>() {
                    pid = Some(v);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pid = pid.expect("mock CLI не записал pid — процесс не запустился");
        assert!(alive(pid), "перед отменой процесс обязан быть живым");

        // `cancel` отдаёт dialog_id отменённого run'а (не run_id) — на этом
        // же построен `cancel_dialog`, который делает обратное отображение.
        assert_eq!(
            state.analysis.cancel(&run_id).await.as_deref(),
            Some(DIALOG.to_string().as_str()),
            "отмена обязана найти идущий run и вернуть его диалог"
        );

        // Смерть процесса асинхронна: ждём её, а не предполагаем.
        let mut died = false;
        for _ in 0..100 {
            if !alive(pid) {
                died = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(died, "процесс {pid} пережил отмену — это утечка «claude -p»");
        assert_eq!(state.analysis.active_count().await, 0, "слот освобождён");
    }

    /// Жив ли процесс: `kill -0` ничего не посылает, только проверяет.
    ///
    /// Через `/bin/kill`, а не через `libc::kill`: ради одной проверки в одном
    /// тесте заводить в плагине зависимость `libc` (и блок `unsafe`) — плохой
    /// обмен. Утилита есть на любой POSIX-системе, а стоимость запуска здесь
    /// никого не волнует.
    fn alive(pid: i32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
