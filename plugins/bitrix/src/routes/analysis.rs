//! Роуты анализа чата: пер-чатовый контекст и запуск разбора через Claude.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET | `/api/bitrix/dialogs/:id/prompt` | заметка пользователя к диалогу |
//! | PUT | `/api/bitrix/dialogs/:id/prompt` | сохранить заметку (пустая строка — стереть) |
//! | GET | `/api/bitrix/dialogs/:id/analysis` | ПОСЛЕДНИЙ сохранённый разбор + тред вопросов |
//! | DELETE | `/api/bitrix/dialogs/:id/analysis` | стереть сохранённый разбор (тред и заметка целы) |
//! | POST | `/api/bitrix/dialogs/:id/analysis` | запустить анализ, обновление либо вопрос |
//! | DELETE | `/api/bitrix/dialogs/:id/analysis/thread` | стереть тред вопросов (разбор и заметка целы) |
//! | POST | `/api/bitrix/dialogs/:id/analysis/cancel` | остановить идущий анализ |
//!
//! # Почему у треда ОТДЕЛЬНЫЙ путь, а не `?thread=1`
//!
//! Типовой экстрактор `Query<T>` отрабатывает ДО тела хендлера и тем самым
//! отвечает `400` раньше проверки авторизации, то есть ломает лестницу (та же
//! причина, по которой тело здесь `Option<Json<T>>`, а весь остальной query
//! плагина разбирается вручную). Отдельный путь не требует ни парсинга, ни
//! исключений в auth-ladder: это просто ещё один маршрут на той же ступени.
//!
//! # ОДИН ПУТЬ, ДВЕ ЛЕСТНИЦЫ
//!
//! У `/analysis` разные методы стоят на РАЗНЫХ лестницах, и это не
//! непоследовательность: `GET`/`DELETE` работают с локальным файлом (как
//! `/prompt`), `POST` начинается с выгрузки истории из портала. Требовать
//! связи, чтобы ПОКАЗАТЬ уже сделанный разбор, значило бы прятать его ровно
//! тогда, когда портал отвалился, — то есть когда он полезнее всего.
//!
//! ```text
//!   /prompt, GET|DELETE /analysis  POST /analysis, /analysis/cancel
//!   DELETE /analysis/thread
//!   ───────────────────────────    ────────────────────────────────
//!   вебхук подключён (409)         вебхук подключён (409)
//!        ▼                              ▼
//!                                  связь с порталом (503) ← require_ready
//!        ▼                              ▼
//!   форма `:id` (404)              форма `:id` (404)
//!        ▼                              ▼
//!   разбор тела (400)              разбор тела (400)
//! ```
//!
//! У `/prompt` ступени «связь» НЕТ: заметка лежит в локальном файле, и
//! `503 not_connected` на чтение локального файла был бы враньём — вкладка
//! перестала бы открывать контекст ровно тогда, когда он нужнее всего
//! (портал душит интенсивность, пользователь читает историю из кэша).
//!
//! У `POST /analysis` ступень «связь» ОБЯЗАТЕЛЬНА и стоит первой: анализ
//! начинается с выгрузки истории через [`crate::routes::dialogs::load_page`],
//! то есть без портала не существует вовсе. Запускать процесс `claude`, чтобы
//! через полторы секунды выяснить, что транскрипт пустой, — это сожжённые
//! токены и бессмысленный ответ модели.
//!
//! # Ключ хранилища — `dialog_id` СТРОКОЙ, и канонизировать его не нужно
//!
//! Здесь Bitrix проще обоих соседей. У VK ключом служит числовой `peer_id`, у
//! Telegram — `PeerRef`, который приходится конвертировать обратно в
//! канонический `dialog_id`, иначе `/dialogs/1234/prompt` и
//! `/dialogs/%201234%20/prompt` завели бы ДВЕ заметки на один диалог. У
//! портала же идентификатор диалога строкой был изначально, и
//! [`crate::routes::dialogs::require_dialog_id`] отдаёт его в единственной
//! канонической форме (`"chat27"`, `"19"`) — ровно той, что уходит в
//! хранилище. Никакой второй функции для этого не заводится.
//!
//! # Порядок отказов у `POST /analysis` зафиксирован
//!
//! `require_ready` → `require_dialog_id` → тело и `kind` → **уже идущий run по
//! этому диалогу** → наличие Claude CLI → свободный слот → выгрузка истории →
//! старт. Три последних проверки стоят ДО выгрузки не для красоты: выгрузка —
//! это до [`context::MAX_PAGES`] запросов к порталу при общем бюджете 2 rps.
//! Отказ «уже идут два анализа» после трёх секунд ожидания выглядит как
//! поломка, а повторный клик по кнопке не имеет права стоить второго обхода
//! истории — и, что важнее у Bitrix, второго куска бюджета, за которым стоит
//! воркер событий.
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
//! идентификаторов портала. `Option<Json<T>>` превращает неразобранное тело в
//! `None`, которое хендлер обрабатывает уже ПОСЛЕ лестницы.
//!
//! Отсутствующее и битое тело — это `400 invalid_input`, а НЕ «пустая
//! заметка». Стирание требует явного `{"prompt": ""}`: молча удалять текст
//! из-за оборванного запроса недопустимо.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::extract::Path;
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
use crate::registry::B24Registry;
use crate::routes::dialogs::{require_authorized, require_dialog_id, require_ready};
use crate::routes::error::ApiError;
use crate::routes::Portal;
use crate::state::B24State;
use crate::ws::protocol::B24ServerMsg;

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
    ///
    /// У Bitrix лестница глубин КОРОЧЕ, чем у VK и Telegram
    /// ([`context::DEPTH_CHOICES`]) — обоснование в шапке
    /// [`crate::analysis::context`].
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
pub fn router() -> Router<Arc<B24Registry>> {
    // Имя параметра `:id` — то же, что у соседних роутов
    // `/api/bitrix/dialogs/*`: axum паникует при сборке, если один сегмент
    // назван по-разному.
    Router::new()
        .route(
            "/api/bitrix/dialogs/:id/prompt",
            get(get_prompt).put(put_prompt),
        )
        .route(
            "/api/bitrix/dialogs/:id/analysis",
            post(post_analysis).get(get_analysis).delete(delete_analysis),
        )
        .route(
            "/api/bitrix/dialogs/:id/analysis/cancel",
            post(post_analysis_cancel),
        )
        // Тот же уровень лестницы, что у `GET|DELETE /analysis`: локальный
        // файл, без `require_ready`.
        .route(
            "/api/bitrix/dialogs/:id/analysis/thread",
            axum::routing::delete(delete_thread),
        )
}

/// Авторизация + форма `:id` — общая ступень «локальных» роутов.
///
/// Связи с порталом НЕ требует: см. шапку модуля. Возвращает канонический
/// `dialog_id`, он же ключ хранилища.
async fn local_ladder(state: &Arc<B24State>, id: &str) -> Result<String, ApiError> {
    require_authorized(state).await?;
    require_dialog_id(id)
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

/// `GET /api/bitrix/dialogs/:id/prompt` — заметка пользователя к диалогу.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`.
pub async fn get_prompt(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<PromptResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    Ok(resp(state.chat_prompt(&key).await))
}

/// `PUT /api/bitrix/dialogs/:id/prompt` — сохранить заметку.
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
    Portal(state): Portal,
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
                target: "forge_bitrix",
                error = %e,
                "forge-bitrix: could not persist the chat prompt"
            );
            ApiError::storage_failed(format!("could not save the chat context: {e}"))
        })?;

    tracing::debug!(
        target: "forge_bitrix",
        dialog_id = %key,
        chars = saved.as_ref().map_or(0, |c| c.prompt.chars().count()),
        "forge-bitrix: chat prompt updated (the text itself is never logged)"
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

/// `GET /api/bitrix/dialogs/:id/analysis` — последний сохранённый разбор и
/// тред вопросов.
///
/// Ни одного вызова портала — как и у `GET /prompt`: разбор и тред лежат в том
/// же локальном файле. Именно поэтому попап показывает прошлый разбор и при
/// мёртвой связи, а история вопросов переживает перезагрузку страницы.
///
/// Разбора нет — `200` с пустым текстом, а не `404`: «пользователь ещё не
/// разбирал этот диалог» это нормальное состояние, а не отсутствующий ресурс.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`.
pub async fn get_analysis(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<SavedAnalysisResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    Ok(saved_resp(
        state.chat_analysis(&key).await,
        state.chat_thread(&key).await,
    ))
}

/// `DELETE /api/bitrix/dialogs/:id/analysis` — стереть сохранённый разбор.
///
/// Ни заметку, ни тред НЕ трогает: это три разные кнопки в попапе и три разные
/// величины в хранилище. Разбора не было — `200 {"cleared": false}`, а не
/// `404`.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `500 storage_failed`.
pub async fn delete_analysis(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<ClearAnalysisResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    let cleared = state.clear_chat_analysis(&key).await.map_err(|e| {
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not clear the chat analysis"
        );
        ApiError::storage_failed(format!("could not clear the saved analysis: {e}"))
    })?;
    Ok(Json(ClearAnalysisResp { cleared }))
}

/// `DELETE /api/bitrix/dialogs/:id/analysis/thread` — стереть тред вопросов
/// («CLEAR Q&A»).
///
/// Ни разбор, ни заметку НЕ трогает — ровно так же, как `DELETE /analysis` не
/// трогает тред. Пустого треда достаточно для `200 {"cleared": false}`: кнопку
/// жмут повторно и из двух вкладок сразу, и `404` здесь был бы ошибкой на
/// месте нормального состояния.
///
/// # Errors
///
/// `409 not_authorized`, `404 unknown_dialog`, `500 storage_failed`.
pub async fn delete_thread(
    Portal(state): Portal,
    Path(id): Path<String>,
) -> Result<Json<ClearThreadResp>, ApiError> {
    let key = local_ladder(&state, &id).await?;
    let cleared = state.clear_chat_thread(&key).await.map_err(|e| {
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not clear the chat q&a thread"
        );
        ApiError::storage_failed(format!("could not clear the saved q&a thread: {e}"))
    })?;
    Ok(Json(ClearThreadResp { cleared }))
}

// ---------------------------------------------------------------------------
// Запуск и отмена анализа.
// ---------------------------------------------------------------------------

/// `POST /api/bitrix/dialogs/:id/analysis` — запустить анализ или задать
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
/// ошибка портала при выгрузке истории — своим кодом.
pub async fn post_analysis(
    Portal(state): Portal,
    Path(id): Path<String>,
    body: Option<Json<AnalysisReq>>,
) -> Result<Json<AnalysisResp>, ApiError> {
    let client = require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;
    let Some(Json(req)) = body else {
        return Err(ApiError::bad_input(
            "the request body must be a JSON object with a 'kind' field",
        ));
    };

    // Задача модели — раньше всего остального: незнакомый `kind` это опечатка
    // клиента, и она не должна стоить ни одного запроса к порталу.
    // Сохранённый разбор читается здесь же: это локальный файл, а не сеть, и
    // без него `refresh` не имеет смысла.
    let saved = state.chat_analysis(&dialog_id).await;
    let task = build_task(&req, saved.as_ref())?;

    // Уже идущий run по этому диалогу: отдаём его как есть, вместе с размерами
    // ЕГО транскрипта. Второй обход истории ради ответа «подключайся к тому же»
    // был бы чистой тратой бюджета 2 rps.
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
    // промпт едет десяток строк вместо двухсот), и по бюджету портала (один
    // запрос вместо четырёх).
    let (messages, mut transcript_truncated) = match &saved {
        Some(a) if matches!(task, Task::Refresh { .. }) => {
            let fresh = context::fetch_since(
                &state,
                &client,
                &dialog_id,
                i64::from(a.last_message_id),
                depth,
            )
            .await?;
            (fresh.messages, fresh.truncated)
        }
        _ => (
            context::fetch(&state, &client, &dialog_id, depth).await?,
            false,
        ),
    };
    let transcript = context::render(&messages, state.config.analysis_max_chars);
    transcript_truncated |= transcript.truncated;

    // Обновлять нечего — и это штатный ответ, а не ошибка (см. шапку модуля).
    // Проверка идёт по ТРАНСКРИПТУ, а не по числу выгруженных сообщений:
    // служебные строки ленты («Игорь добавил Анну») без текста в него не
    // попадают, и запускать модель на пустом `<transcript>` значило бы
    // заменить хороший разбор ответом «ничего нового не произошло».
    if matches!(task, Task::Refresh { .. }) && transcript.messages_used == 0 {
        return Ok(Json(AnalysisResp {
            run_id: None,
            attached: false,
            messages_used: 0,
            truncated: false,
            no_new_messages: true,
        }));
    }

    // Имя чата берётся ИЗ УЖЕ ВЫГРУЖЕННЫХ сообщений, а не карточкой диалога:
    // `im.dialog.get` — это лишний запрос к порталу на КАЖДОМ разборе, то есть
    // половина секунды общего бюджета за украшение шапки промпта. Пустое имя
    // не беда: сборщик подставит «без названия».
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

    // Гонка в другую сторону: пока мы ходили в портал, run по этому диалогу
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

/// `POST /api/bitrix/dialogs/:id/analysis/cancel` — остановить анализ.
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
    Portal(state): Portal,
    Path(id): Path<String>,
    body: Option<Json<CancelReq>>,
) -> Result<Json<CancelResp>, ApiError> {
    require_ready(&state).await?;
    let dialog_id = require_dialog_id(&id)?;
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
    state.broadcast_event(B24ServerMsg::AnalysisDone {
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
/// как у VK и Telegram, и менять его нельзя: коды ответа — публичный контракт
/// вкладки, а разбирает их ОДИН общий фронтовый модуль.
///
/// Путь к бинарю берётся здесь, а не приезжает в ошибке: `Display` ошибки
/// уходит в том числе в логи, а путь — это кусок файловой системы машины.
/// Показать его в ответе — решение ПЛАГИНА.
fn map_analysis_error(state: &B24State, e: &AnalysisError) -> ApiError {
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
    use axum::http::StatusCode;

    use super::*;
    use crate::config::BitrixConfig;

    /// Изолированное состояние: хранилище заметок и «бинарь» Claude — в
    /// tempdir.
    ///
    /// `BitrixConfig::default()` указывает на РЕАЛЬНЫЙ
    /// `~/.forge/bitrix-chat-prompts.json`, и тест, сохранивший заметку, затёр
    /// бы живые записи разработчика. Путь к CLI ведёт в заведомо
    /// отсутствующий файл: тест, забывший его подменить, обязан упереться в
    /// `claude_unavailable`, а не случайно запустить настоящий CLI.
    fn state_with() -> (tempfile::TempDir, Arc<B24State>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BitrixConfig {
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("bitrix.toml"),
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            // Дефолт конфига — облако polza.ai; этот стенд про CLI-пути и
            // 409 claude_unavailable, поэтому бэкенд фиксируем на claude.
            llm_backend: forge_claude_runner::LlmBackend::Claude,
            ..BitrixConfig::default()
        };
        (dir, B24State::new(cfg))
    }

    fn saved(text: &str) -> ChatAnalysis {
        ChatAnalysis {
            text: text.into(),
            last_message_id: 7042,
            messages_used: 137,
            updated_at: 1_800_000_000,
        }
    }

    // -----------------------------------------------------------------------
    // Разбор тела и перевод ошибок ядра.
    //
    // Через роут `POST /analysis` эти ветки в юнит-тесте недостижимы: у него
    // первой ступенью стоит `require_ready`, а живого портала в тесте взять
    // негде (см. `the_analysis_routes_stand_on_two_different_ladders` в
    // `tests/dialogs_ladder.rs`). Поэтому логика проверяется там, где она
    // живёт, — в чистых функциях.
    // -----------------------------------------------------------------------

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
        let (dir, state) = state_with();
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
        assert!(
            err.body.message.contains("claude_cli_path"),
            "{}",
            err.body.message
        );
    }

    /// Остальные три ошибки ядра — те же коды, что у VK и Telegram: их
    /// разбирает ОДИН общий фронтовый модуль.
    #[tokio::test]
    async fn core_errors_map_to_the_shared_codes() {
        let (_dir, state) = state_with();
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

    /// КЛЮЧ ХРАНИЛИЩА — канонический `dialog_id`, и второй функции для этого
    /// не заведено.
    ///
    /// У Telegram здесь живёт `storage_key(PeerRef)`, у VK — конверсия
    /// числового `peer_id`. У Bitrix ту же работу делает уже существующая
    /// `require_dialog_id`, и разные написания одного диалога сходятся в один
    /// ключ БЕЗ единой новой строки кода. Тест фиксирует это как решение, а не
    /// как совпадение.
    #[tokio::test]
    async fn spelling_variants_of_an_id_share_one_note() {
        let (_dir, state) = state_with();
        state
            .set_login(crate::login::LoginFlow::Authorized {
                user: crate::login::B24UserDto {
                    id: "1".into(),
                    ..Default::default()
                },
                portal: "p.bitrix24.ru".into(),
            })
            .await;

        let mut keys = Vec::new();
        for raw in ["chat27", " chat27 ", "\tchat27\n"] {
            keys.push(local_ladder(&state, raw).await.unwrap());
        }
        assert_eq!(keys[0], keys[1], "пробелы вокруг id — тот же диалог");
        assert_eq!(keys[0], keys[2], "перевод строки вокруг id — тот же диалог");
        assert_eq!(keys[0], "chat27");

        // Регистр при этом ЗНАЧИМ и это не упущение: `parse_dialog_id`
        // принимает ровно те написания, которые отдаёт сам портал, а `CHAT27`
        // среди них нет. Приводить его к нижнему регистру означало бы
        // придумать форму идентификатора, которой у платформы не существует.
        assert!(local_ladder(&state, "CHAT27").await.is_err());

        state
            .set_chat_prompt(&keys[0], "заказчик", 1_800_000_000)
            .await
            .unwrap();
        assert_eq!(state.prompts.lock().await.chats.len(), 1, "заметка одна");
    }

    /// Отказ записи (каталог вместо файла) — `500 storage_failed`, а не паника
    /// и не молчаливый успех. Текст заметки в ответ НЕ утекает.
    #[tokio::test]
    async fn an_unwritable_store_answers_storage_failed() {
        let dir = tempfile::tempdir().unwrap();
        let busy = dir.path().join("prompts.json");
        std::fs::create_dir(&busy).unwrap();
        std::fs::write(busy.join("inhabitant"), b"x").unwrap();

        let state = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            config_path: dir.path().join("bitrix.toml"),
            prompts_path: busy,
            claude_cli_path: dir.path().join("no-such-claude"),
            ..BitrixConfig::default()
        });
        let err = state
            .set_chat_prompt("chat7", "CANARY-NOTE-TEXT", 1_800_000_000)
            .await
            .unwrap_err();
        let api = ApiError::storage_failed(format!("could not save the chat context: {err}"));
        assert_eq!(api.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(api.body.code, "storage_failed");
        assert!(
            !api.body.message.contains("CANARY-NOTE-TEXT"),
            "текст заметки не уезжает в сообщение об ошибке: {}",
            api.body.message
        );
    }
}
