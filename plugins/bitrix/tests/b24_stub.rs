//! Локальный стенд, изображающий REST-портал Bitrix24, — флагманский тест
//! крейта.
//!
//! Секретов не требует, работает без сети наружу и гоняет ВЕСЬ транспорт через
//! настоящий `reqwest`: сборку URL, разбор конверта, оба лимита, политику
//! ретраев, нарезку `batch` и очистку секрета из логов.
//!
//! # Почему свой стенд, а не `wiremock`/`httpmock`
//!
//! Новых dev-зависимостей крейт не заводит намеренно (инвариант дерева
//! зависимостей): стенд собирается из уже имеющихся `axum` +
//! `tokio::net::TcpListener::bind("127.0.0.1:0")` и занимает меньше места, чем
//! конфигурация мок-сервера. Ровно ради него существует
//! `BitrixConfig::base_url_override`, а правило Р17 разрешает `http://` на
//! петлевой адрес.
//!
//! # Что стенд умеет изображать
//!
//! | сценарий | [`Reply`] | зачем |
//! |---|---|---|
//! | успех с `time` | [`Reply::Result`], [`Reply::ResultWithTime`] | обычный ответ + оба поля второго лимита |
//! | отказ портала | [`Reply::ApiError`] | вся таблица §4, с любым HTTP-статусом |
//! | превышение интенсивности | `ApiError` 503 `QUERY_LIMIT_EXCEEDED` | первый лимит |
//! | превышение `operating` | `ApiError` 429 `OPERATION_TIME_LIMIT` + `operating_reset_at` | второй лимит |
//! | **пустое тело при 200** | [`Reply::EmptyOk`] | так выглядит троттлинг живьём |
//! | обрыв TCP | [`Reply::Drop`] | заголовки доехали, тело — нет |
//! | зависание | [`Reply::Hang`] | портал перестал отвечать; ловится таймаутом |
//! | мусор вместо JSON | [`Reply::Garbage`] | captive portal, балансировщик |
//!
//! # Порт
//!
//! Всегда `127.0.0.1:0` — операционная система выдаёт свободный. Жёсткий порт
//! однажды упёрся бы в живой инстанс devforge на 7331 и сломал бы прогон на
//! машине разработчика.
//!
//! # Переменные окружения
//!
//! Стенд их НЕ трогает. Тесты собирают `BitrixConfig` значением и зовут
//! `B24Client::new` напрямую, поэтому `FORGE_BITRIX_CONFIG_PATH` не нужен, а
//! `set_var` в многопоточном тестовом бинаре был бы гонкой на ровном месте.
//! Если тест будущей фазы позовёт `forge_bitrix::init`, переменную придётся
//! выставить — Р17 иначе отвергнет `http://127.0.0.1`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use forge_bitrix::api::batch::BatchCall;
use forge_bitrix::api::error::{B24Error, Retry};
use forge_bitrix::api::B24Client;
use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::state::ConnHealth;

/// Код вебхука, который стенд обязан увидеть в ПУТИ запроса и который не имеет
/// права появиться нигде больше — ни в логе, ни в тексте ошибки.
const CODE: &str = "wqdzbfhmn83d23j4";

/// Идентификатор пользователя вебхука.
const UID: &str = "1";

/// Окно `operating` портала — ровно десять минут (замерено живьём:
/// `operating_reset_at − time.start = 600` во всех ответах).
const OPERATING_WINDOW_SECS: i64 = 600;

// ===========================================================================
// Стенд.
// ===========================================================================

/// Что стенд ответит на очередной вызов метода.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum Reply {
    /// Успех: `{"result": …, "time": {…}}` с нулевым `operating`.
    Result(Value),
    /// Успех с заданными полями второго лимита.
    ResultWithTime {
        result: Value,
        operating: f64,
        /// Смещение `operating_reset_at` от «сейчас», секунды.
        reset_in: i64,
    },
    /// Отказ портала. Статус любой — ошибка опознаётся по телу.
    ApiError {
        status: u16,
        code: String,
        description: String,
        /// Приложить ли `time` с `operating_reset_at` (так портал иногда
        /// отвечает на `OPERATION_TIME_LIMIT`).
        reset_in: Option<i64>,
    },
    /// Оборвать тело ответа: заголовки доехали, тело — нет.
    Drop,
    /// ПУСТОЕ тело при HTTP 200 — так выглядит троттлинг живьём.
    EmptyOk,
    /// Не-JSON с заданным статусом (captive portal, балансировщик).
    Garbage(u16),
    /// Не отвечать заданное время — портал «завис».
    Hang(Duration),
}

impl Reply {
    /// Отказ по интенсивности — первый лимит портала.
    fn query_limit() -> Self {
        Self::ApiError {
            status: 503,
            code: "QUERY_LIMIT_EXCEEDED".into(),
            description: "Too many requests".into(),
            reset_in: None,
        }
    }

    /// Отказ по `operating`-времени метода — второй лимит портала.
    fn operating_limit(reset_in: i64) -> Self {
        Self::ApiError {
            status: 429,
            code: "OPERATION_TIME_LIMIT".into(),
            description: "Method operating time limit is reached".into(),
            reset_in: Some(reset_in),
        }
    }

    /// Произвольный отказ портала с HTTP 200 — самая частая форма.
    fn error(code: &str) -> Self {
        Self::ApiError {
            status: 200,
            code: code.into(),
            description: format!("stub: {code}"),
            reset_in: None,
        }
    }
}

/// Запись о вызове.
#[derive(Debug, Clone)]
struct Call {
    /// Имя метода без суффикса `.json`.
    method: String,
    /// Код вебхука из ПУТИ — по нему видно, что клиент собрал URL правильно.
    code: String,
    /// Тело запроса.
    body: Value,
    /// Когда пришёл запрос — нужен тестам троттлинга.
    at: Instant,
    /// Сколько команд было в `cmd`, если это `batch`.
    batch_len: usize,
}

/// Состояние стенда.
#[derive(Default)]
struct StubState {
    /// Метод → очередь заготовленных ответов. Пусто ⇒ дефолтный успех.
    script: Mutex<HashMap<String, VecDeque<Reply>>>,
    /// Журнал вызовов.
    calls: Mutex<Vec<Call>>,
    /// Методы, которые внутри `batch` обязаны отдать `result_error`.
    batch_failures: Mutex<Vec<String>>,
    /// Метод → РЕАЛЬНЫЙ результат вложенного вызова `batch`.
    ///
    /// По умолчанию стенд кладёт во вложенный результат диагностический объект
    /// (`{method, command, result}`) — по нему тесты транспорта проверяют, что
    /// команда доехала urlencode'нутой. Тестам данных (Ф4+) нужен НАСТОЯЩИЙ
    /// ответ метода, иначе разбор `im.recent.get` внутри пакета проверить
    /// нечем.
    batch_results: Mutex<HashMap<String, Value>>,
    /// Очередь событий `im.v2.Event.get` (Ф7).
    events: Mutex<EventQueue>,
}

/// Очередь событий портала с ПОДТВЕРЖДЕНИЕМ вместо позиции чтения.
///
/// Модель дословно повторяет живое поведение (§4 плана):
///
/// - `offset` в запросе — это ack: всё, что старше него, портал считает
///   обработанным и больше не отдаёт;
/// - вызов БЕЗ `offset` возвращает всё неподтверждённое с самого начала
///   (именно так холодный старт получает хвост очереди);
/// - `nextOffset` = идентификатор последнего отданного события + 1;
/// - `hasMore` честен: остаток очереди виден следующему вызову.
///
/// `page_cap` существует ради теста каденса: заполнять очередь сотней событий
/// только чтобы увидеть `hasMore`, дороже, чем разрешить стенду отдавать по
/// одному.
#[derive(Default)]
struct EventQueue {
    /// События с их `eventId`.
    items: Vec<(i64, Value)>,
    /// Подтверждённая позиция.
    acked: i64,
    /// Искусственный потолок страницы (для `hasMore`).
    page_cap: Option<usize>,
}

impl StubState {
    fn script(&self, method: &str, replies: Vec<Reply>) {
        self.script
            .lock()
            .unwrap()
            .insert(method.to_string(), replies.into());
    }

    fn fail_in_batch(&self, method: &str) {
        self.batch_failures.lock().unwrap().push(method.to_string());
    }

    /// Что вложенный вызов `method` вернёт внутри `batch`.
    fn batch_result(&self, method: &str, result: Value) {
        self.batch_results
            .lock()
            .unwrap()
            .insert(method.to_string(), result);
    }

    fn next_reply(&self, method: &str) -> Option<Reply> {
        self.script
            .lock()
            .unwrap()
            .get_mut(method)
            .and_then(VecDeque::pop_front)
    }

    /// Кладёт событие в очередь портала и возвращает его `eventId`.
    ///
    /// Идентификаторы растут ЧЕРЕЗ ОДИН — ровно как на живом портале
    /// (`eventId` 5, 7, 9…): арифметика «следующий = текущий + 1» на этом
    /// ломается, и хорошо, что ломается в тесте.
    fn push_event(&self, kind: &str, data: Value) -> i64 {
        let mut q = self.events.lock().unwrap();
        let id = q.items.last().map_or(5, |(id, _)| id + 2);
        q.items.push((
            id,
            json!({"eventId": id, "type": kind, "date": "2026-08-05T20:20:36+03:00", "data": data}),
        ));
        id
    }

    /// Ограничивает размер страницы событий — так проверяется `hasMore`.
    fn events_page_cap(&self, cap: usize) {
        self.events.lock().unwrap().page_cap = Some(cap);
    }

    /// Подтверждённая порталом позиция.
    fn acked(&self) -> i64 {
        self.events.lock().unwrap().acked
    }

    /// Есть ли заготовленный ответ для метода (иначе очередь событий отвечает
    /// сама).
    fn has_script(&self, method: &str) -> bool {
        self.script
            .lock()
            .unwrap()
            .get(method)
            .is_some_and(|q| !q.is_empty())
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn calls_of(&self, method: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == method)
            .collect()
    }

    fn count_of(&self, method: &str) -> usize {
        self.calls_of(method).len()
    }
}

/// Адреса поднятого стенда.
struct Stub {
    /// Значение для `BitrixConfig::base_url_override`.
    base_url: String,
    state: Arc<StubState>,
}

/// Поднимает стенд на свободном порту.
async fn start_stub() -> Stub {
    install_log_capture();
    let state = Arc::new(StubState::default());
    let app = Router::new()
        .route("/rest/:uid/:code/:method", post(handle_method))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Stub {
        base_url: format!("http://{addr}/rest/{UID}/{CODE}"),
        state,
    }
}

/// Блок `time` успешного ответа — ровно той формы, что приходит от портала.
fn time_block(operating: f64, reset_in: i64) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "start": now,
        "finish": now as f64 + 0.12,
        "duration": 0.12,
        "processing": 0,
        "date_start": "2026-08-05T20:14:58+03:00",
        "date_finish": "2026-08-05T20:14:59+03:00",
        "operating_reset_at": now + reset_in,
        "operating": operating
    })
}

/// Дефолтный успешный результат метода — слеплен с живых ответов портала.
fn default_result(method: &str) -> Value {
    match method {
        "profile" => json!({
            "ID": "1", "ADMIN": true, "NAME": "Игорь", "LAST_NAME": "Герасимов",
            "TIME_ZONE": "Europe/Moscow",
            "PERSONAL_PHOTO": "https://cdn-ru.bitrix24.ru/b38365992/main/8b7/x.jpg"
        }),
        "scope" => json!(["im", "task", "tasks", "tasks_extended"]),
        "im.counters.get" => json!({
            "TYPE": {"ALL": 9, "NOTIFY": 5, "CHAT": 0, "DIALOG": 4, "MESSENGER": 4,
                     "TASKS_TASK": 0, "LINES": 0, "COPILOT": 0, "COLLAB": 0},
            "DIALOG": {"19": 1, "17": 2, "3": 1},
            "CHAT": [], "CHAT_UNREAD": []
        }),
        "server.time" => json!("2026-08-05T20:14:59+03:00"),
        "im.message.add" => json!(114),
        _ => json!(true),
    }
}

/// Ответ с оборванным телом — «соединение упало в середине».
fn broken_response() -> Response {
    let stream = futures_util::stream::once(async {
        Err::<Vec<u8>, std::io::Error>(std::io::Error::other("stub: connection dropped"))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        // Заголовки доезжают, тело — нет.
        .header(axum::http::header::CONTENT_LENGTH, "4096")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn handle_method(
    State(state): State<Arc<StubState>>,
    Path((_uid, code, method_json)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let method = method_json
        .strip_suffix(".json")
        .unwrap_or(&method_json)
        .to_string();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let batch_len = parsed
        .get("cmd")
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);

    state.calls.lock().unwrap().push(Call {
        method: method.clone(),
        code: code.clone(),
        body: parsed.clone(),
        at: Instant::now(),
        batch_len,
    });

    // Поток событий обслуживает очередь, а не таблица дефолтов: его семантика
    // (ack вместо курсора) — предмет проверки Фазы 7, и подменить её на
    // `{"result": true}` значило бы тестировать заглушку. Заготовленный ответ
    // при этом побеждает — им тесты изображают отказы портала.
    if method == "im.v2.Event.get" && !state.has_script(&method) {
        return events_response(&state, &parsed);
    }

    let reply = state
        .next_reply(&method)
        .unwrap_or_else(|| Reply::Result(default_result(&method)));

    match reply {
        Reply::Hang(d) => {
            tokio::time::sleep(d).await;
            ok_response(json!(true), 0.0, OPERATING_WINDOW_SECS)
        }
        Reply::Drop => broken_response(),
        Reply::EmptyOk => Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .unwrap(),
        Reply::Garbage(status) => Response::builder()
            .status(StatusCode::from_u16(status).unwrap())
            .header(axum::http::header::CONTENT_TYPE, "text/html")
            .body(Body::from("<html>captive portal</html>"))
            .unwrap(),
        Reply::ApiError {
            status,
            code,
            description,
            reset_in,
        } => {
            let mut body = json!({"error": code, "error_description": description});
            if let Some(reset_in) = reset_in {
                body["time"] = time_block(480.0, reset_in);
            }
            (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
        }
        Reply::Result(result) => {
            if method == "batch" {
                return batch_response(&state, &parsed);
            }
            ok_response(result, 0.0, OPERATING_WINDOW_SECS)
        }
        Reply::ResultWithTime {
            result,
            operating,
            reset_in,
        } => ok_response(result, operating, reset_in),
    }
}

/// Ответ `im.v2.Event.get` из очереди стенда.
///
/// ГЛАВНОЕ, что здесь моделируется: `offset` — это ПОДТВЕРЖДЕНИЕ. Пока пачка не
/// подтверждена, она приезжает снова и снова; подтверждённое исчезает
/// безвозвратно.
fn events_response(state: &Arc<StubState>, parsed: &Value) -> Response {
    let mut q = state.events.lock().unwrap();
    // Вызов без `offset` не подтверждает ничего и отдаёт всё с последней
    // подтверждённой позиции — именно так холодный старт видит хвост очереди.
    let from = parsed
        .get("offset")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        })
        .unwrap_or(q.acked);
    q.acked = q.acked.max(from);

    let pending: Vec<Value> = q
        .items
        .iter()
        .filter(|(id, _)| *id >= from)
        .map(|(_, ev)| ev.clone())
        .collect();
    let cap = q.page_cap.unwrap_or(pending.len().max(1));
    let page: Vec<Value> = pending.iter().take(cap).cloned().collect();
    let has_more = pending.len() > page.len();
    let next_offset = page
        .last()
        .and_then(|e| e["eventId"].as_i64())
        .map_or(from, |id| id + 1);

    ok_response(
        json!({"events": page, "nextOffset": next_offset, "hasMore": has_more}),
        0.0,
        OPERATING_WINDOW_SECS,
    )
}

fn ok_response(result: Value, operating: f64, reset_in: i64) -> Response {
    axum::Json(json!({"result": result, "time": time_block(operating, reset_in)})).into_response()
}

/// Ответ на `batch`: демультиплексируем `cmd` и отвечаем ровно теми же
/// ключами.
///
/// Команды после 50-й НЕ выполняются и приезжают в `result_error` — ровно так,
/// как это делает портал, БЕЗ общей ошибки. Именно эта тихая потеря и есть
/// причина автоматической нарезки на пачки.
fn batch_response(state: &Arc<StubState>, parsed: &Value) -> Response {
    let failures = state.batch_failures.lock().unwrap().clone();
    let overrides = state.batch_results.lock().unwrap().clone();
    let mut ok = serde_json::Map::new();
    let mut errors = serde_json::Map::new();

    if let Some(cmd) = parsed.get("cmd").and_then(Value::as_object) {
        for (i, (key, value)) in cmd.iter().enumerate() {
            let command = value.as_str().unwrap_or_default().to_string();
            let method = command.split('?').next().unwrap_or_default().to_string();

            if i >= 50 {
                errors.insert(
                    key.clone(),
                    json!({"error": "ERROR_BATCH_LENGTH_EXCEEDED",
                           "error_description": "Max batch length exceeded"}),
                );
                continue;
            }
            if failures.contains(&method) {
                errors.insert(
                    key.clone(),
                    json!({"error": "PARAMS_ERROR",
                           "error_description": format!("stub: {method} rejected")}),
                );
                continue;
            }
            // Настоящий ответ метода — если тест его заготовил (тесты
            // данных), иначе диагностический объект с самим запросом: по нему
            // тесты транспорта видят, что значения доехали urlencode'нутыми.
            match overrides.get(&method) {
                Some(result) => {
                    ok.insert(key.clone(), result.clone());
                }
                None => {
                    ok.insert(
                        key.clone(),
                        json!({"method": method, "command": command, "result": default_result(&method)}),
                    );
                }
            }
        }
    }

    ok_response(
        json!({
            "result": Value::Object(ok),
            "result_error": if errors.is_empty() { json!([]) } else { Value::Object(errors) },
            "result_total": [],
            "result_next": [],
            "result_time": []
        }),
        0.0,
        OPERATING_WINDOW_SECS,
    )
}

// ===========================================================================
// Захват логов (копия `src/testlog.rs`: интеграционный тест — отдельный
// бинарь, и `#[cfg(test)]`-модули крейта ему не видны).
// ===========================================================================

static LOG_LINES: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();

struct LogCapture {
    lines: Arc<Mutex<Vec<String>>>,
}

/// Ставит ГЛОБАЛЬНЫЙ подписчик один раз на процесс.
///
/// Именно глобально, а не `set_default` на поток: `tracing` кэширует «интерес»
/// к callsite ОДИН раз на весь процесс, и тест, выполнившийся первым без
/// подписчика, навсегда пометил бы `tracing::warn!` в `api/mod.rs` как
/// неинтересный — тогда захват в соседнем тесте молча получал бы пустоту.
/// Вызывается из [`start_stub`], то есть заведомо раньше первого события.
fn install_log_capture() -> Arc<Mutex<Vec<String>>> {
    LOG_LINES
        .get_or_init(|| {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let _ = tracing::subscriber::set_global_default(LogCapture {
                lines: lines.clone(),
            });
            lines
        })
        .clone()
}

/// Сколько строк уже накоплено — курсор «своего» отрезка.
fn log_mark() -> usize {
    install_log_capture().lock().unwrap().len()
}

/// Строки, записанные ПОСЛЕ отметки, одним текстом.
fn log_since(mark: usize) -> String {
    let all = install_log_capture().lock().unwrap().clone();
    all.get(mark..).unwrap_or(&[]).join("\n")
}

struct FieldCollector(String);

impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={:?} ", field.name(), value));
    }
}

impl tracing::Subscriber for LogCapture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut collector = FieldCollector(String::new());
        event.record(&mut collector);
        self.lines.lock().unwrap().push(collector.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

// ===========================================================================
// Сборка клиента.
// ===========================================================================

/// Конфиг, указывающий на стенд.
///
/// `rate_limit_rps` и `burst` приходят параметрами: тестам троттлинга нужен
/// боевой лимит, всем остальным — заведомо не мешающий.
fn stub_config(stub: &Stub, rps: u32, burst: u32) -> BitrixConfig {
    BitrixConfig {
        base_url_override: stub.base_url.clone(),
        rate_limit_rps: rps,
        burst,
        connect_timeout_secs: 1,
        request_timeout_secs: 1,
        cache_dir: std::env::temp_dir().join("forge-bitrix-stub-tests"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: std::env::temp_dir().join("forge-bitrix-stub-tests").join("prompts.json"),
        claude_cli_path: std::env::temp_dir().join("forge-bitrix-stub-tests").join("no-such-claude"),
        ..BitrixConfig::default()
    }
}

/// Клиент на стенде: быстрый повтор, чтобы тесты не спали по полсекунды.
fn client_with(stub: &Stub, rps: u32, burst: u32) -> B24Client {
    B24Client::new(
        &stub_config(stub, rps, burst),
        // Вебхук ЗАДАН и содержит настоящий секрет: override побеждает при
        // сборке URL, но проверки «код не утёк» обязаны видеть реальную
        // строку, а не пустоту.
        &WebhookUrl::new(format!("https://test-work-2.bitrix24.ru/rest/{UID}/{CODE}/")),
        Arc::new(tokio::sync::RwLock::new(ConnHealth::default())),
    )
    .unwrap()
    .with_retry_backoff(Duration::from_millis(20))
}

/// Клиент с заведомо не мешающим лимитом — для тестов, где лимит не предмет.
fn client(stub: &Stub) -> B24Client {
    client_with(stub, 10, 50)
}

// ===========================================================================
// Стенд работает.
// ===========================================================================

/// Дымовой тест самого стенда: типизированная обёртка проходит весь путь —
/// сборка URL с секретом в пути, `POST` JSON, разбор конверта.
#[tokio::test]
async fn the_stub_serves_typed_methods_over_the_real_transport() {
    let stub = start_stub().await;
    let c = client(&stub);

    let profile = c.profile().await.unwrap();
    assert_eq!(profile.id.as_string(), "1");
    assert_eq!(profile.name, "Игорь");
    assert!(profile.admin);

    let scope = c.scope().await.unwrap();
    assert!(forge_bitrix::api::methods::has_im_scope(&scope));
    assert!(forge_bitrix::api::methods::has_task_scope(&scope));

    let counters = c.counters().await.unwrap();
    assert_eq!(counters.types.all, 9);
    assert!(counters.chat.is_empty(), "пустой PHP-массив разобран");

    // Клиент собрал URL по контракту §4: секрет — в ПУТИ, метод с `.json`.
    let calls = stub.state.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].method, "profile");
    assert_eq!(calls[0].code, CODE);
    // Параметры уехали JSON-телом, а не query-строкой.
    assert!(calls[0].body.is_object(), "{:?}", calls[0].body);
}

/// Скрипт отрабатывает по очереди и заканчивается дефолтным успехом.
#[tokio::test]
async fn the_script_is_consumed_in_order() {
    let stub = start_stub().await;
    stub.state.script(
        "server.time",
        vec![
            Reply::error("ERROR_METHOD_NOT_FOUND"),
            Reply::Result(json!("2026-08-05T20:14:59+03:00")),
        ],
    );
    let c = client(&stub);

    let err = c.server_time().await.unwrap_err();
    assert_eq!(err.code(), Some("ERROR_METHOD_NOT_FOUND"));
    assert_eq!(c.server_time().await.unwrap(), "2026-08-05T20:14:59+03:00");
    // Очередь кончилась — снова дефолт.
    assert!(c.server_time().await.is_ok());
    assert_eq!(stub.state.count_of("server.time"), 3);
}

/// Стенд умеет изображать все виды отказа, а клиент — их различать.
#[tokio::test]
async fn the_stub_can_fake_every_failure_mode() {
    let stub = start_stub().await;
    let c = client(&stub);

    // 1. Мусор вместо JSON при не-200 — посредник, а не портал.
    //    Заготовлены ДВА ответа: 5xx повторяется, и одного стенду не хватило
    //    бы — повтор увидел бы дефолтный успех.
    stub.state
        .script("server.time", vec![Reply::Garbage(502), Reply::Garbage(502)]);
    assert!(matches!(
        c.server_time().await.unwrap_err(),
        B24Error::Http { status: 502 }
    ));

    // 2. Обрыв TCP: повтор разрешён, второй ответ уже успешный.
    stub.state.script("server.time", vec![Reply::Drop]);
    assert!(c.server_time().await.is_ok());

    // 3. Зависание дольше таймаута — тоже транспортный отказ.
    stub.state.script(
        "server.time",
        vec![Reply::Hang(Duration::from_secs(3)), Reply::Hang(Duration::from_secs(3))],
    );
    let err = c.server_time().await.unwrap_err();
    assert!(err.is_transport(), "{err:?}");

    // 4. Интенсивность: 503 QUERY_LIMIT_EXCEEDED — повторяемо.
    stub.state
        .script("server.time", vec![Reply::query_limit(), Reply::query_limit()]);
    let err = c.server_time().await.unwrap_err();
    assert!(matches!(err, B24Error::Throttled { .. }), "{err:?}");
    assert_eq!(forge_bitrix::api::retry_for(&err), Retry::AfterBackoff);
}

// ===========================================================================
// ЛИМИТ 1: интенсивность.
// ===========================================================================

/// Боевая настройка (`rps=2`, `burst=5`): залп ёмкостью ведра проходит
/// мгновенно, дальше вызовы разносятся ровно на `1/rps`.
///
/// Фиксированное окно вместо ведра дало бы здесь нули и секундные провалы —
/// то есть пограничный залп, на который портал отвечает обрывами.
#[tokio::test]
async fn throttle_spaces_calls_evenly() {
    let stub = start_stub().await;
    let c = client_with(&stub, 2, 5);

    for _ in 0..8 {
        c.server_time().await.unwrap();
    }

    let calls = stub.state.calls_of("server.time");
    assert_eq!(calls.len(), 8);
    let at: Vec<Instant> = calls.iter().map(|c| c.at).collect();

    // 1. Залп из пяти — мгновенно.
    let burst = at[4].duration_since(at[0]);
    assert!(
        burst < Duration::from_millis(300),
        "залп ёмкостью бака обязан проходить сразу, занял {burst:?}"
    );

    // 2. Дальше — равномерно, по полсекунды на вызов.
    for i in 5..8 {
        let gap = at[i].duration_since(at[i - 1]);
        assert!(
            gap >= Duration::from_millis(430) && gap <= Duration::from_millis(900),
            "интервал {i} = {gap:?} — троттлер обязан разносить вызовы ровно"
        );
    }

    // 3. Установившийся режим: три вызова сверх ёмкости занимают ~1.5 с, то
    //    есть в скользящую секунду укладывается не больше двух.
    let window = at[7].duration_since(at[4]);
    assert!(
        window >= Duration::from_millis(1300),
        "три вызова сверх ёмкости уложились в {window:?} — это больше 2 rps"
    );
}

// ===========================================================================
// ЛИМИТ 2: ресурсоёмкость метода.
// ===========================================================================

/// `OPERATION_TIME_LIMIT` на методе A не мешает вызывать метод B, а сам A
/// возобновляется после `operating_reset_at`.
///
/// Это ключевое отличие второго лимита от первого: блокируется КОНКРЕТНЫЙ
/// метод, а не портал. Реализация, которая ставит на паузу весь клиент,
/// провалит середину теста; реализация, которая паузу игнорирует, — конец.
#[tokio::test]
async fn operating_limit_blocks_only_one_method_until_reset_at() {
    let stub = start_stub().await;
    // Лимит интенсивности заведомо не мешает: предмет теста — второй лимит.
    let c = client_with(&stub, 10, 50);

    // Портал отказал методу A и сообщил точный момент разблокировки.
    stub.state
        .script("im.recent.get", vec![Reply::operating_limit(2)]);

    let err = c
        .call_value("im.recent.get", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, B24Error::OperatingLimit { .. }),
        "ожидался OperatingLimit, пришло {err:?}"
    );
    // Повтор ЗАПРЕЩЁН: он только продлил бы блокировку.
    assert_eq!(forge_bitrix::api::retry_for(&err), Retry::Never);
    let api = forge_bitrix::api::to_api_error(&err, chrono::Utc::now().timestamp());
    assert_eq!(api.status.as_u16(), 429);
    assert_eq!(api.body.code, "flood_wait");
    let wait = api.body.retry_after_secs.unwrap();
    assert!((1..=3).contains(&wait), "обратный отсчёт {wait} c");

    // ДРУГОЙ метод обязан работать немедленно.
    let started = Instant::now();
    c.counters().await.unwrap();
    let b_elapsed = started.elapsed();
    assert!(
        b_elapsed < Duration::from_millis(400),
        "пауза одного метода задержала другой на {b_elapsed:?}"
    );

    // Метод A возобновляется после reset_at — вызов ждёт паузу и проходит.
    let started = Instant::now();
    c.call_value("im.recent.get", json!({})).await.unwrap();
    let a_elapsed = started.elapsed();
    assert!(
        a_elapsed >= Duration::from_millis(900),
        "вызов заблокированного метода не подождал разблокировки: {a_elapsed:?}"
    );
    assert!(a_elapsed < Duration::from_secs(4), "{a_elapsed:?}");

    // Сеть трогали ровно дважды: заблокированный вызов ждал у нас, а не в
    // портале.
    assert_eq!(stub.state.count_of("im.recent.get"), 2);
    assert_eq!(stub.state.count_of("im.counters.get"), 1);
    let a_calls = stub.state.calls_of("im.recent.get");
    assert!(
        a_calls[1].at.duration_since(a_calls[0].at) >= Duration::from_millis(900),
        "второй вызов ушёл в портал раньше разблокировки"
    );
}

// ===========================================================================
// Конверт и ретраи.
// ===========================================================================

/// ЛОВУШКА ПЛАТФОРМЫ: пустое тело при HTTP 200 — это троттлинг, а не битый
/// ответ.
///
/// Разница не академическая: `Decode` означал бы «чините код» и не
/// повторялся бы, `Transport` означает «портал душит» и повторяется.
#[tokio::test]
async fn empty_body_on_200_is_treated_as_transport_error() {
    let stub = start_stub().await;
    let c = client(&stub);

    // Оба ответа пустые — повтор тоже упирается в пустоту.
    stub.state
        .script("server.time", vec![Reply::EmptyOk, Reply::EmptyOk]);
    let err = c.server_time().await.unwrap_err();

    assert!(
        matches!(err, B24Error::Transport { .. }),
        "пустое тело при 200 обязано быть Transport, пришло {err:?}"
    );
    assert!(err.is_transport());
    assert_eq!(forge_bitrix::api::retry_for(&err), Retry::AfterBackoff);
    let api = forge_bitrix::api::to_api_error(&err, 0);
    assert_eq!(api.status.as_u16(), 503);
    assert_eq!(api.body.code, "not_connected");
    // Повтор состоялся: два обращения к порталу.
    assert_eq!(stub.state.count_of("server.time"), 2);

    // …а одиночная пустота лечится тем самым повтором.
    stub.state.script("im.counters.get", vec![Reply::EmptyOk]);
    c.counters().await.unwrap();
    assert_eq!(stub.state.count_of("im.counters.get"), 2);
}

/// Пачка длиннее 50 режется автоматически, а `result_error` раскладывается по
/// ИСХОДНЫМ ключам.
///
/// Вторая половина теста показывает, от чего защищает нарезка: та же пачка,
/// отправленная одним куском, теряет пять команд — портал возвращает их в
/// `result_error`, но общей ошибки не даёт.
#[tokio::test]
async fn batch_splits_at_fifty_and_demuxes_result_error() {
    let stub = start_stub().await;
    let c = client(&stub);
    stub.state.fail_in_batch("im.recent.get");

    let mut calls: Vec<BatchCall> = (0..54)
        .map(|i| BatchCall::new(format!("k{i:03}"), "im.counters.get", json!({})))
        .collect();
    // Одна команда, которая обязана отказать, — она не должна утянуть пачку.
    calls.push(BatchCall::new(
        "recent",
        "im.recent.get",
        json!({"LAST_SYNC_DATE": "2026-08-05T20:14:43+03:00"}),
    ));

    let out = c.batch(calls).await.unwrap();

    // 1. Две пачки: 50 + 5.
    let batches = stub.state.calls_of("batch");
    assert_eq!(batches.len(), 2, "55 команд обязаны уехать двумя пачками");
    assert_eq!(batches[0].batch_len, 50);
    assert_eq!(batches[1].batch_len, 5);

    // 2. Ни одна команда не потеряна.
    assert_eq!(out.ok.len() + out.errors.len(), 55);
    for i in 0..54 {
        let key = format!("k{i:03}");
        assert!(out.get(&key).is_some(), "потерян ключ {key}");
    }

    // 3. Отказ одной команды разложен по её собственному ключу.
    assert_eq!(out.error("recent").unwrap().code, "PARAMS_ERROR");
    assert!(out.get("recent").is_none());

    // 4. Значения доехали urlencode'нутыми: `+` в дате стал `%2B`.
    let command = batches
        .iter()
        .find_map(|c| c.body.get("cmd")?.get("recent")?.as_str().map(str::to_string))
        .expect("команда `recent` не найдена в теле пачки");
    assert!(command.contains("%2B03%3A00"), "{command}");
    assert!(!command.contains('+'), "{command}");

    // 5. Та же пачка ОДНИМ куском: пять команд молча не выполняются, но общей
    //    ошибки нет — ровно поэтому нарезка автоматическая.
    let oversized: Vec<BatchCall> = (0..55)
        .map(|i| BatchCall::new(format!("n{i:03}"), "im.counters.get", json!({})))
        .collect();
    let resp = c
        .call_value("batch", forge_bitrix::api::batch::body(&oversized))
        .await
        .expect("портал не даёт общей ошибки на переполненную пачку");
    let out = forge_bitrix::api::batch::demux(&resp.result);
    assert_eq!(out.ok.len(), 50);
    assert_eq!(out.errors.len(), 5);
    assert!(out
        .errors
        .values()
        .all(|e| e.code == "ERROR_BATCH_LENGTH_EXCEEDED"));
}

/// Транспортная ошибка повторяется РОВНО один раз, а отправка не повторяется
/// НИКОГДА (Р9).
///
/// У `im.message.add` и `im.v2.File.upload` нет дедуп-идентификатора: повтор
/// после обрыва создал бы у собеседника второе сообщение. Счётчик стенда —
/// единственный способ это доказать: по возвращённой ошибке повтор не виден.
#[tokio::test]
async fn transport_error_is_retried_once_and_message_add_never() {
    let stub = start_stub().await;
    let c = client(&stub);

    // 1. Обычный метод: один обрыв лечится повтором.
    stub.state.script("server.time", vec![Reply::Drop]);
    c.server_time().await.unwrap();
    assert_eq!(stub.state.count_of("server.time"), 2, "повтор не состоялся");

    // 2. Два обрыва подряд — ошибка, и повторов РОВНО один (всего 2 запроса).
    stub.state.script(
        "im.counters.get",
        vec![Reply::Drop, Reply::Drop, Reply::Drop],
    );
    let err = c.counters().await.unwrap_err();
    assert!(err.is_transport(), "{err:?}");
    assert_eq!(
        stub.state.count_of("im.counters.get"),
        2,
        "повтор обязан быть ровно один"
    );

    // 3. Отправка сообщения НЕ повторяется ни разу.
    stub.state
        .script("im.message.add", vec![Reply::Drop, Reply::Drop]);
    let err = c
        .call_value("im.message.add", json!({"DIALOG_ID": "chat7", "MESSAGE": "hi"}))
        .await
        .unwrap_err();
    assert!(err.is_transport(), "{err:?}");
    assert_eq!(
        stub.state.count_of("im.message.add"),
        1,
        "повтор отправки создал бы дубль сообщения"
    );

    // 4. Заливка файла — тоже.
    stub.state
        .script("im.v2.File.upload", vec![Reply::Drop, Reply::Drop]);
    let err = c
        .call_value("im.v2.File.upload", json!({"fileContent": "AAA"}))
        .await
        .unwrap_err();
    assert!(err.is_transport(), "{err:?}");
    assert_eq!(stub.state.count_of("im.v2.File.upload"), 1);

    // 5. …и запрет не обходится вложением отправки в batch.
    stub.state.script("batch", vec![Reply::Drop, Reply::Drop]);
    let err = c
        .batch(vec![BatchCall::new(
            "send",
            "im.message.add",
            json!({"DIALOG_ID": "chat7", "MESSAGE": "hi"}),
        )])
        .await
        .unwrap_err();
    assert!(err.is_transport(), "{err:?}");
    assert_eq!(
        stub.state.count_of("batch"),
        1,
        "batch с отправкой внутри повторять нельзя"
    );

    // 6. Команда бота (forge-zw3d) — тоже: повтор означал бы второе
    // исполнение (второй голос, повторная заявка).
    stub.state
        .script("im.message.command", vec![Reply::Drop, Reply::Drop]);
    let err = c
        .message_command(51, 17, "careButton", Some("5"))
        .await
        .unwrap_err();
    assert!(err.is_transport(), "{err:?}");
    assert_eq!(
        stub.state.count_of("im.message.command"),
        1,
        "повтор команды создал бы второе исполнение"
    );
}

// ===========================================================================
// СТРАЖИ СЕКРЕТА.
// ===========================================================================

/// ГЛАВНЫЙ ИНВАРИАНТ ПЛАГИНА: код вебхука и хост портала не появляются ни в
/// одной строке лога и ни в одном тексте ошибки — на ВСЕХ ветках сразу.
///
/// Проверяются четыре сценария, каждый из которых цитирует URL по-своему:
/// успех (лог `debug`), отказ портала (лог `warn` + текст ошибки), обрыв TCP
/// (текст `reqwest`, который печатает URL целиком) и таймаут. Отдельно
/// проверяется `without_url()` — он и есть механизм, на котором всё держится.
#[tokio::test]
async fn webhook_code_never_appears_in_logs_or_errors() {
    let stub = start_stub().await;
    let c = client(&stub);
    let host = "test-work-2.bitrix24.ru";
    let mark = log_mark();

    // 1. Успешный вызов.
    c.profile().await.unwrap();

    // 2. Отказ портала, в тексте которого САМ ПОРТАЛ процитировал наш URL.
    stub.state.script(
        "im.message.add",
        vec![Reply::ApiError {
            status: 200,
            code: "PARAMS_ERROR".into(),
            description: format!("bad call to {}/im.message.add.json", stub.base_url),
            reset_in: None,
        }],
    );
    let api_err = c
        .call_value("im.message.add", json!({"DIALOG_ID": "chat7"}))
        .await
        .unwrap_err();

    // 3. Обрыв TCP: `reqwest` печатает URL целиком — это самая вероятная
    //    утечка во всём плагине.
    stub.state
        .script("server.time", vec![Reply::Drop, Reply::Drop]);
    let transport_err = c.server_time().await.unwrap_err();

    // 4. Таймаут.
    stub.state.script(
        "im.counters.get",
        vec![
            Reply::Hang(Duration::from_secs(3)),
            Reply::Hang(Duration::from_secs(3)),
        ],
    );
    let timeout_err = c.counters().await.unwrap_err();

    // --- Логи ---
    let logs = log_since(mark);
    assert!(!logs.is_empty(), "захват логов не работает — проверка пуста");
    assert!(!logs.contains(CODE), "код вебхука в логе:\n{logs}");
    assert!(!logs.contains(host), "хост портала в логе:\n{logs}");
    assert!(
        !logs.contains("/rest/1/"),
        "путь вебхука в логе:\n{logs}"
    );
    // Полезное в логе остаться ОБЯЗАНО, иначе «чисто» достигалось бы
    // молчанием.
    assert!(logs.contains("im.message.add"), "{logs}");
    assert!(logs.contains("PARAMS_ERROR"), "{logs}");

    // --- Тексты ошибок ---
    let now = chrono::Utc::now().timestamp();
    for err in [&api_err, &transport_err, &timeout_err] {
        let text = err.to_string();
        assert!(!text.contains(CODE), "код вебхука в ошибке: {text}");
        assert!(!text.contains(host), "хост портала в ошибке: {text}");
        assert!(!text.contains("127.0.0.1"), "адрес стенда в ошибке: {text}");

        // …и в том, что уедет браузеру.
        let api = forge_bitrix::api::to_api_error(err, now);
        assert!(!api.body.message.contains(CODE), "{}", api.body.message);
        assert!(!api.body.message.contains(host), "{}", api.body.message);
    }

    // --- Сам механизм ---
    let dirty = B24Error::Transport {
        reason: format!("error sending request for url ({}/profile.json)", stub.base_url),
    };
    assert!(dirty.to_string().contains(CODE), "проверка потеряла смысл");
    let clean = dirty.without_url().to_string();
    assert!(!clean.contains(CODE), "{clean}");
    assert!(!clean.contains("127.0.0.1"), "{clean}");
}

/// Построчный grep по `src/`: ни одна строка не должна одновременно содержать
/// вызов логирования и раскрытие секрета.
///
/// Тест защищает БУДУЩИЕ фазы: `WebhookUrl` не сериализуем, но `expose()`
/// возвращает обычную строку, и `tracing::info!("{}", url.expose())`
/// компилируется прекрасно.
///
/// Эвристика построчная и это осознанно: многострочный вызов макроса с
/// `expose()` на отдельной строке она пропустит, зато не даёт ложных
/// срабатываний и читается без парсера. Настоящая защита — отсутствие
/// `Serialize` у `WebhookUrl`; тест ловит вторую по вероятности ошибку.
#[test]
fn no_source_line_logs_the_exposed_url() {
    /// Вызовы логирования.
    const LOG_MACROS: [&str; 6] = [
        "tracing::",
        "trace!",
        "debug!",
        "info!",
        "warn!",
        "error!",
    ];
    /// Способы получить секрет строкой.
    const SECRET_SOURCES: [&str; 4] = [".expose()", ".api_base()", "self.base_url", ".webhook()"];

    /// Нарушает ли строка правило.
    fn offends(line: &str) -> bool {
        let trimmed = line.trim_start();
        // Комментарии пропускаем: в них живут ровно те примеры, ради запрета
        // которых тест и написан (включая этот).
        if trimmed.starts_with("//") {
            return false;
        }
        SECRET_SOURCES.iter().any(|s| line.contains(s))
            && LOG_MACROS.iter().any(|m| line.contains(m))
    }

    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    // Детектор обязан ловить нарушение — иначе «зелёный» тест ничего не
    // доказывает.
    assert!(offends(r#"    tracing::info!(url = %self.webhook.expose(), "oops");"#));
    assert!(offends(r#"    warn!("{}", cfg.api_base().unwrap());"#));
    assert!(!offends(r#"    // tracing::info!("{}", url.expose());"#));
    assert!(!offends(r#"    let base = webhook.api_base();"#));

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(files.len() >= 8, "исходники не нашлись: {files:?}");

    let mut offenders = Vec::new();
    for file in files {
        let body = std::fs::read_to_string(&file).unwrap();
        for (n, line) in body.lines().enumerate() {
            if offends(line) {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "секрет уезжает в лог:\n{}",
        offenders.join("\n")
    );
}

/// `batch` на стенде: ключи возвращаются те же, что были в запросе, а
/// отказавшая команда попадает в `result_error`, не роняя пачку.
#[tokio::test]
async fn the_stub_demultiplexes_a_batch() {
    let stub = start_stub().await;
    stub.state.fail_in_batch("im.recent.get");
    let c = client(&stub);

    let out = c
        .batch(vec![
            BatchCall::new("counters", "im.counters.get", json!({})),
            BatchCall::new("recent", "im.recent.get", json!({"SKIP_OPENLINES": "Y"})),
        ])
        .await
        .unwrap();

    assert!(out.get("counters").is_some());
    assert_eq!(out.error("recent").unwrap().code, "PARAMS_ERROR");
    assert_eq!(stub.state.count_of("batch"), 1);
    assert_eq!(stub.state.calls_of("batch")[0].batch_len, 2);
}

// ===========================================================================
// ПОДКЛЮЧЕНИЕ (Ф3): роуты логина по проводу через стенд портала.
//
// Здесь проверяется то, что не видно ни юнит-тестам автомата (они ходят через
// подменённый `LoginBackend`), ни юнит-тестам конфига (они не знают про HTTP):
// связка «разбор вставленной строки → запись 0600 → profile → scope →
// состояние → рассылка в WS» целиком, на настоящем транспорте.
// ===========================================================================

/// Стенд роутов подключения: плагин с изолированным конфигом, смотрящий на
/// поднятый стенд портала.
struct LoginHarness {
    /// Держит временный каталог живым до конца теста.
    _dir: tempfile::TempDir,
    /// Путь конфига — по нему проверяются права и содержимое.
    config_path: std::path::PathBuf,
    /// Слот, с которого стенд НАЧИНАЕТ, — пустой (вебхука нет).
    ///
    /// После первого подключения он больше не портал: занятие пустого слота
    /// ПЕРЕСОБИРАЕТ состояние, потому что дисковые пути выведены из
    /// идентификатора портала и лежат в неизменяемом `BitrixConfig` (Ф6).
    /// Живой портал спрашивают у реестра — [`LoginHarness::registry`].
    state: Arc<forge_bitrix::state::B24State>,
    registry: Arc<forge_bitrix::B24Registry>,
    app: Router,
}

const LOGIN_PORTAL: &str = "login-stand.bitrix24.ru";

/// Код вебхука, который «вставляет пользователь».
///
/// Он НАМЕРЕННО отличается от [`CODE`] (кода в пути стенда): захват логов
/// глобален на весь тестовый бинарь, тесты идут параллельно, и совпадение
/// констант превратило бы соседний страж Ф2 в источник ложных падений.
const LOGIN_CODE: &str = "lgnc0de9876543210";

/// То, что пользователь копирует из портала (канонический вид).
const WEBHOOK_INPUT: &str = "https://login-stand.bitrix24.ru/rest/1/lgnc0de9876543210/";

fn login_harness(stub: &Stub) -> LoginHarness {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("bitrix.toml");
    let cfg = BitrixConfig {
        // Секрет в теле запроса настоящий, а ходит клиент на стенд: так
        // проверки «код не утёк» видят реальную строку.
        base_url_override: stub.base_url.clone(),
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: config_path.clone(),
        connect_timeout_secs: 2,
        request_timeout_secs: 2,
        rate_limit_rps: 10,
        burst: 50,
        ..BitrixConfig::default()
    };
    let state = forge_bitrix::state::B24State::new(cfg);
    let registry = forge_bitrix::B24Registry::single(state.clone());
    let app = forge_bitrix::routes::build_router(registry.clone());
    LoginHarness {
        _dir: dir,
        config_path,
        state,
        registry,
        app,
    }
}

/// Запрос ЛЮБЫМ методом с необязательным JSON-телом.
///
/// Один помощник на все методы: Ф6 добавила `PATCH` и `DELETE`, и три почти
/// одинаковых функции разъехались бы по заголовкам при первой же правке.
async fn request_json(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn post_json(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    request_json(app, "POST", uri, Some(body)).await
}

/// PATCH с JSON-телом (правка сообщения, Ф6).
async fn post_patch(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    request_json(app, "PATCH", uri, Some(body)).await
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn connect(app: &Router, raw: &str) -> (StatusCode, Value) {
    post_json(app, "/api/bitrix/login/webhook", json!({ "webhook": raw })).await
}

/// ГЛАВНЫЙ КРИТЕРИЙ ФАЗЫ: три формы ввода, которые реально копируют из
/// портала, дают ОДИН результат — и на проводе, и в файле конфига.
#[tokio::test]
async fn all_three_input_forms_connect_and_save_the_same_canonical_webhook() {
    let forms = [
        // 1. Полный URL с именем метода на хвосте.
        format!("https://{LOGIN_PORTAL}/rest/1/{LOGIN_CODE}/profile.json"),
        // 2. URL без метода, с хвостовым слэшем.
        format!("https://{LOGIN_PORTAL}/rest/1/{LOGIN_CODE}/"),
        // 3. URL без метода и без слэша.
        format!("https://{LOGIN_PORTAL}/rest/1/{LOGIN_CODE}"),
    ];

    let mut saved = Vec::new();
    for raw in forms {
        let stub = start_stub().await;
        let h = login_harness(&stub);

        let (status, body) = connect(&h.app, &raw).await;
        assert_eq!(status, StatusCode::OK, "вход {raw:?}: {body}");
        assert_eq!(body["state"], "authorized", "вход {raw:?}: {body}");
        assert_eq!(body["user"]["id"], "1");
        assert_eq!(body["user"]["name"], "Игорь");
        assert_eq!(body["user"]["last_name"], "Герасимов");

        // Портал спрошен ровно дважды и в единственно верном порядке.
        let methods: Vec<String> = stub.state.calls().into_iter().map(|c| c.method).collect();
        assert_eq!(methods, vec!["profile", "scope"], "вход {raw:?}");

        saved.push(std::fs::read_to_string(&h.config_path).unwrap());
    }

    assert_eq!(saved[0], saved[1], "формы 1 и 2 обязаны дать один файл");
    assert_eq!(saved[1], saved[2], "формы 2 и 3 обязаны дать один файл");
    assert!(
        saved[0].contains(&format!("https://{LOGIN_PORTAL}/rest/1/{LOGIN_CODE}/")),
        "{}",
        saved[0]
    );
    // Имя метода в файл не уехало: сохраняется канон, а не то, что вставили.
    assert!(!saved[0].contains("profile.json"), "{}", saved[0]);
}

/// ПОРЯДОК `profile` → `scope` неизменен, и `scope` не зовётся, если вебхук
/// уже отвергнут: иначе пользователь получил бы «не хватает прав» на чужом
/// вебхуке и пошёл чинить не то.
#[tokio::test]
async fn a_rejected_webhook_never_reaches_the_scope_call() {
    let stub = start_stub().await;
    stub.state
        .script("profile", vec![Reply::error("INVALID_CREDENTIALS")]);
    let h = login_harness(&stub);

    let (status, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "invalid_credentials", "{body}");
    assert_eq!(stub.state.count_of("profile"), 1);
    assert_eq!(
        stub.state.count_of("scope"),
        0,
        "порядок обратным быть не может"
    );
}

/// Мусорный ввод не доезжает ни до диска, ни до сети.
#[tokio::test]
async fn garbage_input_never_reaches_the_portal_or_the_disk() {
    let stub = start_stub().await;
    let h = login_harness(&stub);

    for raw in [
        "",
        "не урл",
        "https://login-stand.bitrix24.ru/",
        "https://login-stand.bitrix24.ru/rest/abc/1/",
        "http://evil.example/rest/1/lgnc0de9876543210/",
    ] {
        let (status, body) = connect(&h.app, raw).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "вход {raw:?}: {body}");
        assert_eq!(body["error"]["code"], "invalid_input", "вход {raw:?}");
        // Текст отказа не цитирует вставленное значение.
        let text = body["error"]["message"].as_str().unwrap_or_default();
        assert!(!text.contains(LOGIN_CODE), "вход {raw:?}: {text}");
    }

    assert!(
        stub.state.calls().is_empty(),
        "невалидный ввод обязан отвергаться ДО сети: {:?}",
        stub.state.calls()
    );
    assert!(
        !h.config_path.exists(),
        "невалидный ввод не должен создавать конфиг"
    );

    // Состояние при этом изменилось — соседние вкладки узнают об отказе
    // только кадром `login_state`.
    let (_, status_body) = get_json(&h.app, "/api/bitrix/status").await;
    assert_eq!(status_body["login"]["state"], "invalid_url");
}

/// Отказы портала раскладываются по СВОИМ экранам: три разных кода — три
/// разных состояния, а не один «что-то пошло не так».
#[tokio::test]
async fn portal_failures_map_to_their_own_login_states() {
    // 1. Вебхук отозван / чужой.
    let stub = start_stub().await;
    let h = login_harness(&stub);
    stub.state
        .script("profile", vec![Reply::error("NO_AUTH_FOUND")]);
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "invalid_credentials", "{body}");

    // 2. Портал отверг сам вызов по правам.
    let stub = start_stub().await;
    let h = login_harness(&stub);
    stub.state
        .script("scope", vec![Reply::error("insufficient_scope")]);
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "scope_missing", "{body}");

    // 3. Права пришли, но их не хватает — перечисляем недостающие.
    let stub = start_stub().await;
    let h = login_harness(&stub);
    stub.state
        .script("scope", vec![Reply::Result(json!(["im", "crm"]))]);
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "scope_missing", "{body}");
    let text = body["error"].as_str().unwrap_or_default();
    assert!(text.contains("task"), "нет перечисления недостающих прав: {text}");

    // 4. Пустой scope — это «прав нет», а не «наверное, есть».
    let stub = start_stub().await;
    let h = login_harness(&stub);
    stub.state.script("scope", vec![Reply::Result(json!([]))]);
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "scope_missing", "{body}");
}

/// Троттлинг портала (Р16) — ОТДЕЛЬНОЕ состояние, а не `disconnected`: оно
/// лечится ожиданием, и статусбар панели показывает его своей меткой.
#[tokio::test]
async fn throttling_during_the_check_gets_its_own_state_with_a_countdown() {
    let stub = start_stub().await;
    let h = login_harness(&stub);
    // Два ответа: `QUERY_LIMIT_EXCEEDED` повторяем ровно один раз (§4).
    stub.state.script(
        "profile",
        vec![Reply::query_limit(), Reply::query_limit()],
    );

    let (status, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "throttled", "{body}");
    assert!(
        body["retry_after_secs"].as_u64().unwrap_or_default() >= 1,
        "обратный отсчёт обязан доехать до фронта: {body}"
    );
    assert_eq!(stub.state.count_of("profile"), 2, "ровно один повтор");
}

/// Сети нет — экран `disconnected`, а вебхук всё равно СОХРАНЁН: пользователь
/// не должен вставлять его заново из-за одного обрыва.
#[tokio::test]
async fn a_transport_failure_keeps_the_webhook_and_shows_disconnected() {
    let stub = start_stub().await;
    let h = login_harness(&stub);
    stub.state.script(
        "profile",
        vec![
            Reply::Hang(Duration::from_secs(5)),
            Reply::Hang(Duration::from_secs(5)),
        ],
    );

    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "disconnected", "{body}");
    assert!(h.config_path.exists(), "вебхук обязан остаться сохранённым");
    assert!(std::fs::read_to_string(&h.config_path).unwrap().contains(LOGIN_CODE));
}

/// Файл конфига создаётся с правами `0600` — по настоящему пути роута, а не
/// только в юнит-тесте `config.rs`.
#[cfg(unix)]
#[tokio::test]
async fn the_login_route_writes_an_owner_only_config() {
    use std::os::unix::fs::PermissionsExt;

    let stub = start_stub().await;
    let h = login_harness(&stub);
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "authorized", "{body}");

    let mode = std::fs::metadata(&h.config_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "секрет обязан лежать 0600");

    // Временного файла после атомарной записи не остаётся.
    let leftovers: Vec<String> = std::fs::read_dir(h.config_path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Выход стирает секрет и с диска, и из состояния — и НЕ ходит на портал.
#[tokio::test]
async fn logout_wipes_the_webhook_locally_without_calling_the_portal() {
    let stub = start_stub().await;
    let h = login_harness(&stub);
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "authorized", "{body}");
    let calls_before = stub.state.calls().len();

    let (status, body) = post_json(&h.app, "/api/bitrix/logout", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "not_configured", "{body}");

    let saved = std::fs::read_to_string(&h.config_path).unwrap();
    assert!(!saved.contains(LOGIN_CODE), "секрет остался в файле: {saved}");
    assert!(!h.state.has_webhook().await);
    assert!(
        h.state.client.read().await.is_none(),
        "кэш клиента (а с ним троттлер) обязан быть сброшен"
    );
    assert_eq!(
        stub.state.calls().len(),
        calls_before,
        "logout не должен ходить на портал: отозвать вебхук через REST невозможно"
    );

    // Повторный выход — не ошибка.
    let (status, body) = post_json(&h.app, "/api/bitrix/logout", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "not_configured");

    // И `/status` больше не знает ни о каком подключении.
    let (_, status_body) = get_json(&h.app, "/api/bitrix/status").await;
    assert_eq!(status_body["login"]["state"], "not_configured");
    assert!(status_body["login"]["user"].is_null());
}

/// Вставка вебхука В РАНТАЙМЕ доезжает до подписчиков БЕЗ переподключения:
/// именно на этом держится живой экран подключения в соседних вкладках (и,
/// с Ф7, поднятие realtime без рестарта процесса).
///
/// # Два разных пути, и оба проверяются здесь
///
/// 1. **Первое подключение занимает пустой слот**, а с Ф6 занятие ПЕРЕСОБИРАЕТ
///    состояние (дисковые пути выведены из идентификатора портала). Подписчик,
///    сидевший на пустом слоте, поэтому не увидит переходов нового портала — и
///    не должен: сокет узнаёт о смене состава сигналом
///    (`subscribe_composition`), переподписывается по `Arc::ptr_eq`
///    (`PortalFanIn::sync`) и берёт СНИМОК состояния уже после подписки.
///    Потерять итоговое состояние на этом пути невозможно, и проверяется
///    именно это: сигнал был, живой портал авторизован.
/// 2. **Повторное подключение УЖЕ известного портала** состояние не
///    пересобирает — вкладка та же. Здесь подписчик обязан увидеть оба
///    перехода по порядку, и ровно это утверждала исходная версия теста.
#[tokio::test]
async fn connecting_at_runtime_pushes_login_state_to_existing_subscribers() {
    let stub = start_stub().await;
    let h = login_harness(&stub);

    // -- путь 1: пустой слот превращается в портал ------------------------
    let composition = h.registry.subscribe_composition();
    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "authorized", "{body}");

    assert!(
        composition.has_changed().unwrap_or(false),
        "смена состава обязана объявляться сигналом, иначе сокет не переподпишется"
    );
    let portal = h.registry.primary();
    assert!(
        !Arc::ptr_eq(&portal, &h.state),
        "занятый слот пересобран (Ф6)"
    );
    assert_eq!(
        portal.login_state_dto().await.state,
        "authorized",
        "снимок, который сокет возьмёт после переподписки, уже авторизован"
    );

    // -- путь 2: тот же портал подключают ещё раз -------------------------
    // Подписка ДО подключения — ровно как у уже открытого WS-клиента.
    let mut rx = portal.subscribe();
    // Воркер (Ф7) ждёт на этом же уведомлении, не делая сетевых вызовов.
    let notified = portal.authorized_notify.notified();
    tokio::pin!(notified);

    let (_, body) = connect(&h.app, WEBHOOK_INPUT).await;
    assert_eq!(body["state"], "authorized", "{body}");
    assert!(
        Arc::ptr_eq(&portal, &h.registry.primary()),
        "известный портал пересобирать не за чем: идентификатор не менялся"
    );

    let mut states = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let forge_bitrix::B24ServerMsg::LoginState(dto) = msg {
            states.push(dto.state);
        }
    }
    assert_eq!(
        states,
        vec!["checking", "authorized"],
        "подписчик обязан увидеть ОБА перехода и в правильном порядке"
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut notified)
            .await
            .is_ok(),
        "authorized_notify обязан будить воркеров без рестарта процесса"
    );
}

/// Смена секрета обязана давать НОВЫЙ клиент: иначе остался бы троттлер от
/// прошлого вебхука, а после logout — клиент с уже стёртым кодом.
#[tokio::test]
async fn changing_the_webhook_invalidates_the_cached_client() {
    let stub = start_stub().await;
    let h = login_harness(&stub);

    // Клиент спрашивается у ЖИВОГО портала: первое подключение занимает пустой
    // слот и пересобирает его состояние (Ф6), поэтому `h.state` после него —
    // уже не портал.
    connect(&h.app, WEBHOOK_INPUT).await;
    let first = h.registry.primary().api_client().await.unwrap();

    connect(
        &h.app,
        "https://login-stand.bitrix24.ru/rest/1/otherc0de9999/",
    )
    .await;
    let second = h.registry.primary().api_client().await.unwrap();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "второй клиент = второй троттлер = двойной бюджет запросов"
    );

    // В файле остался только последний вебхук — мультипортальности нет (Р5).
    let saved = std::fs::read_to_string(&h.config_path).unwrap();
    assert_eq!(saved.matches("webhook =").count(), 1, "{saved}");
    assert!(saved.contains("otherc0de9999"), "{saved}");
    assert!(!saved.contains(LOGIN_CODE), "{saved}");
}

/// ТЕСТ-СТРАЖ ФАЗЫ: ни один ответ роутов подключения не содержит секрета —
/// ни целиком, ни куском.
///
/// Проверяются ВСЕ достижимые состояния и сравнение идёт по сырому телу, а не
/// по разобранным полям: утечка через новое поле обязана краснеть здесь
/// автоматически.
#[tokio::test]
async fn no_login_response_ever_contains_the_webhook_or_a_piece_of_it() {
    /// Сценарий: ожидаемое состояние автомата + скрипт стенда, который к нему
    /// приводит.
    type Scenario = (&'static str, Box<dyn Fn(&Stub)>);

    let scenarios: Vec<Scenario> = vec![
        ("authorized", Box::new(|_: &Stub| {})),
        (
            "invalid_credentials",
            Box::new(|s: &Stub| s.state.script("profile", vec![Reply::error("INVALID_CREDENTIALS")])),
        ),
        (
            "scope_missing",
            Box::new(|s: &Stub| s.state.script("scope", vec![Reply::Result(json!([]))])),
        ),
        (
            "throttled",
            Box::new(|s: &Stub| {
                s.state
                    .script("profile", vec![Reply::query_limit(), Reply::query_limit()])
            }),
        ),
        (
            "disconnected",
            Box::new(|s: &Stub| {
                s.state.script(
                    "profile",
                    vec![Reply::Garbage(502), Reply::Garbage(502)],
                )
            }),
        ),
    ];

    for (expected, script) in scenarios {
        let stub = start_stub().await;
        script(&stub);
        let h = login_harness(&stub);

        let (_, login_body) = connect(&h.app, WEBHOOK_INPUT).await;
        assert_eq!(login_body["state"], expected, "{login_body}");

        let (_, status_body) = get_json(&h.app, "/api/bitrix/status").await;
        let (_, logout_body) = post_json(&h.app, "/api/bitrix/logout", json!({})).await;

        for (what, body) in [
            ("login", &login_body),
            ("status", &status_body),
            ("logout", &logout_body),
        ] {
            assert_no_secret(&serde_json::to_string(body).unwrap(), &format!("{what}/{expected}"));
        }
    }
}

/// Проверяет, что в тексте нет ни кода вебхука, ни хоста портала, ни ЛЮБОГО
/// куска кода длиной от восьми символов.
///
/// Порог в восемь символов не косметический: кода `wqdzbfhmn83d23j4` длиной 16
/// хватит и половины, чтобы перебор стал реальным, а «показать первые восемь
/// символов для наглядности» — ровно тот соблазн, который этот страж и должен
/// ловить.
fn assert_no_secret(text: &str, what: &str) {
    const MIN_PIECE: usize = 8;
    assert!(!text.contains(LOGIN_PORTAL), "{what}: хост портала в {text}");
    assert!(!text.contains("fingerprint"), "{what}: отпечаток в {text}");
    let chars: Vec<char> = LOGIN_CODE.chars().collect();
    for start in 0..=chars.len() - MIN_PIECE {
        let piece: String = chars[start..start + MIN_PIECE].iter().collect();
        assert!(
            !text.contains(&piece),
            "{what}: кусок кода вебхука «{piece}» в ответе: {text}"
        );
    }
}

/// Тело запроса подключения СОДЕРЖИТ СЕКРЕТ — и он не имеет права попасть в
/// лог ни целиком, ни отпечатком сверх того, что пишет `config::save_*`.
#[tokio::test]
async fn the_login_route_keeps_the_webhook_out_of_the_logs() {
    let stub = start_stub().await;
    let h = login_harness(&stub);
    let mark = log_mark();

    connect(&h.app, WEBHOOK_INPUT).await;
    post_json(&h.app, "/api/bitrix/logout", json!({})).await;
    // И мусорный ввод: его текст тоже мог бы уехать в лог «для диагностики».
    connect(&h.app, "https://login-stand.bitrix24.ru/rest/1/leakme123456/x/y").await;

    let logs = log_since(mark);
    assert!(!logs.contains(LOGIN_CODE), "код вебхука в логе:\n{logs}");
    assert!(!logs.contains("leakme123456"), "мусорный ввод в логе:\n{logs}");
    // Отпечаток — законное исключение: он и нужен, чтобы понять, ТОТ ли
    // вебхук вставлен, и середины кода в нём нет.
    assert!(
        logs.contains("login-stand.bitrix24.ru#1…543210"),
        "отпечаток обязан быть в логе сохранения:\n{logs}"
    );
}

// ===========================================================================
// СПИСОК ДИАЛОГОВ (Ф4): холодный старт и добор имён — по проводу.
//
// Оба теста про ОДНО И ТО ЖЕ: бюджет портала — 2 запроса в секунду на пару
// портал+IP, и цена ошибки здесь не «медленно», а «панель открывается пять
// секунд и половину времени видна пустой». Поэтому число запросов проверяется
// счётчиком стенда, а не читается глазами по коду.
// ===========================================================================

/// Живой ответ `im.recent.get` тестового портала.
const RECENT_GET_FIXTURE: &str = include_str!("fixtures/recent_get.json");

/// Результат `im.recent.get` из живой фикстуры — то, что стенд отдаст внутри
/// пакета.
fn recent_get_result() -> Value {
    let v: Value = serde_json::from_str(RECENT_GET_FIXTURE).unwrap();
    v["result"].clone()
}

/// ХОЛОДНЫЙ СТАРТ СТОИТ РОВНО ОДИН ЗАПРОС (Р3).
///
/// Список диалогов и счётчики приезжают одним `batch`: по отдельности это два
/// хита лимита интенсивности и лишние полсекунды ожидания ровно в тот момент,
/// когда пользователь смотрит на пустую панель.
#[tokio::test]
async fn the_cold_start_costs_exactly_one_request() {
    let stub = start_stub().await;
    stub.state
        .batch_result("im.recent.get", recent_get_result());
    stub.state
        .batch_result("im.counters.get", default_result("im.counters.get"));
    let c = client(&stub);

    let (recent, counters) = c.cold_start().await.unwrap();

    assert_eq!(stub.state.calls().len(), 1, "холодный старт ушёл не одним запросом");
    let call = &stub.state.calls_of("batch")[0];
    assert_eq!(call.batch_len, 2, "в пакете обязаны быть обе команды");
    assert_eq!(call.body["halt"], 0, "halt:1 отменил бы счётчики из-за списка");
    assert!(
        call.body["cmd"]["recent"]
            .as_str()
            .unwrap()
            .starts_with("im.recent.get"),
        "{:?}",
        call.body["cmd"]
    );

    // И ответ разобран: обе половины на месте.
    assert_eq!(recent.len(), 6, "строки списка не разобрались");
    assert_eq!(counters.types.all, 9);
    // Чат задачи из `im.recent.get` доехал — ради него метод и выбран основой.
    assert!(recent
        .iter()
        .any(|i| i.chat.entity_type.as_deref() == Some("TASKS_TASK")));
}

/// Отказ ОДНОЙ команды пакета не отменяет вторую: `halt:0` выполняет их
/// независимо, и список обязан приехать даже без счётчиков.
#[tokio::test]
async fn a_failing_counters_command_still_yields_the_dialog_list() {
    let stub = start_stub().await;
    stub.state
        .batch_result("im.recent.get", recent_get_result());
    stub.state.fail_in_batch("im.counters.get");
    let c = client(&stub);

    let (recent, counters) = c.cold_start().await.unwrap();
    assert_eq!(recent.len(), 6);
    assert_eq!(counters.types.all, 0, "счётчиков нет — но это не ошибка");
    assert_eq!(stub.state.calls().len(), 1);
}

/// ТРИДЦАТЬ НЕИЗВЕСТНЫХ ИМЁН — ОДИН ЗАПРОС.
///
/// Резолв по одному id при бюджете 2 rps означал бы пятнадцать секунд на
/// открытие чата из тридцати участников.
#[tokio::test]
async fn thirty_unknown_peers_are_resolved_in_a_single_batch() {
    let stub = start_stub().await;

    let ids: Vec<i64> = (100..130).collect();
    let mut map = serde_json::Map::new();
    for id in &ids {
        map.insert(
            id.to_string(),
            json!({"id": id, "name": format!("Сотрудник {id}"), "status": "offline",
                   "birthday": false, "last_activity_date": false, "work_position": null}),
        );
    }
    stub.state
        .batch_result("im.user.list.get", Value::Object(map));

    let c = client(&stub);
    let cache = forge_bitrix::peers::PeerCache::new();
    cache.resolve_uncached(&c, &ids).await.unwrap();

    assert_eq!(stub.state.calls().len(), 1, "добор ушёл не одним запросом");
    assert_eq!(stub.state.count_of("batch"), 1);
    assert_eq!(
        stub.state.count_of("im.user.list.get"),
        0,
        "прямых вызовов быть не должно — только внутри пакета"
    );
    // Тридцать id уложились в ОДНУ команду (потолок пачки — 50).
    assert_eq!(stub.state.calls_of("batch")[0].batch_len, 1);

    assert_eq!(cache.len(), 30);
    assert_eq!(
        forge_bitrix::dto::PeerNames::display_name(&cache, 115).as_deref(),
        Some("Сотрудник 115")
    );

    // Повторный запрос теми же id не стоит НИЧЕГО: всё уже в кэше.
    cache.resolve_uncached(&c, &ids).await.unwrap();
    assert_eq!(stub.state.calls().len(), 1, "кэш не сработал");
}

/// Отказ команды добора не роняет страницу списка: имена — украшение, а не
/// данные. Подписью станет идентификатор, и это лучше пустого экрана.
#[tokio::test]
async fn a_failed_name_lookup_never_fails_the_page() {
    let stub = start_stub().await;
    stub.state.fail_in_batch("im.user.list.get");
    let c = client(&stub);

    let cache = forge_bitrix::peers::PeerCache::new();
    cache
        .resolve_uncached(&c, &[15, 19])
        .await
        .expect("отказ ОДНОЙ команды пакета ошибкой пакета не является");
    assert!(cache.is_empty());
}

/// `im.recent.list` умеет страницы, но чатов задач не отдаёт, а `im.recent.get`
/// отдаёт всё и без страниц. Полный список поэтому собирается из ОБОИХ (Р3), и
/// склейка не должна давать дублей.
#[tokio::test]
async fn the_full_list_needs_both_sources_and_merges_without_duplicates() {
    let stub = start_stub().await;
    let full = recent_get_result();
    // `im.recent.list` — те же строки МИНУС чаты задач, плюс своя обёртка.
    let without_tasks: Vec<Value> = full
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["chat"]["entity_type"] != "TASKS_TASK")
        .cloned()
        .collect();
    stub.state.script(
        "im.recent.get",
        vec![Reply::Result(full.clone())],
    );
    stub.state.script(
        "im.recent.list",
        vec![Reply::Result(
            json!({"items": without_tasks, "hasMore": false, "hasMorePages": false}),
        )],
    );

    let c = client(&stub);
    let from_get = c.recent_get(None).await.unwrap();
    let page = c.recent_list(50, 0).await.unwrap();

    let tasks_in_get = from_get
        .iter()
        .filter(|i| i.chat.entity_type.as_deref() == Some("TASKS_TASK"))
        .count();
    assert!(tasks_in_get >= 2, "фикстура обязана содержать чаты задач");
    assert!(
        page.items
            .iter()
            .all(|i| i.chat.entity_type.as_deref() != Some("TASKS_TASK")),
        "im.recent.list чатов задач не отдаёт — на этом и построен Р3"
    );
    assert!(!page.has_more);

    let peers = forge_bitrix::peers::PeerCache::new();
    let to_dto = |items: &[forge_bitrix::api::schema::RecentItemRaw]| {
        items
            .iter()
            .filter_map(|i| forge_bitrix::dto::dialog_from_raw(i, 1, &peers))
            .collect::<Vec<_>>()
    };
    let merged = forge_bitrix::dto::merge_dialogs(to_dto(&from_get), to_dto(&page.items));
    assert_eq!(merged.len(), from_get.len(), "склейка дала дубли");
    assert!(merged.iter().any(|d| d.kind == "task"));
}

// ===========================================================================
// РОУТЫ СПИСКА (Ф4): `/api/bitrix/dialogs` и `/api/bitrix/filters` по проводу.
// ===========================================================================

/// Подключённый плагин, смотрящий на стенд: лестница проверок пройдена, и
/// видно поведение самих данных.
async fn data_harness(stub: &Stub) -> LoginHarness {
    let h = login_harness(stub);
    h.state
        .set_login(forge_bitrix::login::LoginFlow::Authorized {
            user: forge_bitrix::login::B24UserDto {
                id: "1".into(),
                name: Some("Игорь".into()),
                ..Default::default()
            },
            portal: LOGIN_PORTAL.into(),
        })
        .await;
    h.state
        .set_webhook(WebhookUrl::new(WEBHOOK_INPUT))
        .await;
    h
}

/// Заготавливает стенду ответы холодного старта.
fn script_cold_start(stub: &Stub) {
    stub.state
        .batch_result("im.recent.get", recent_get_result());
    stub.state
        .batch_result("im.counters.get", default_result("im.counters.get"));
}

/// Страница списка: ОДИН запрос на всё, порядок «пины → дата», директории
/// фильтруют один и тот же список.
#[tokio::test]
async fn the_dialog_list_costs_one_request_and_splits_into_directories() {
    let stub = start_stub().await;
    script_cold_start(&stub);
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs?limit=50").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dialogs = body["dialogs"].as_array().unwrap();
    assert_eq!(dialogs.len(), 6);
    assert_eq!(body["has_more"], false);

    // ОДИН запрос на весь список: имена приехали блоками `user` того же
    // ответа, добирать нечего.
    assert_eq!(stub.state.calls().len(), 1, "{:?}", stub.state.calls());
    assert_eq!(stub.state.count_of("batch"), 1);
    assert_eq!(stub.state.count_of("im.user.list.get"), 0);

    // Порядок — по дате изменения, сверху самый свежий.
    let ids: Vec<&str> = dialogs.iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["chat7", "19", "chat11", "chat9", "chat3", "17"]);

    // Имена, счётчики и «своё сообщение» — на месте.
    let personal = dialogs.iter().find(|d| d["id"] == "19").unwrap();
    assert_eq!(personal["title"], "Алексей Пруцков");
    assert_eq!(personal["kind"], "user");
    assert_eq!(personal["unread_count"], 1);
    assert_eq!(personal["last_message"]["text"], "Я с вами в команде!");
    assert_eq!(personal["last_message"]["out"], false);

    let task = dialogs.iter().find(|d| d["id"] == "chat7").unwrap();
    assert_eq!(task["kind"], "task");
    assert_eq!(task["task_id"], "1");
    assert_eq!(task["role"], "owner", "OWNER обязан приехать строчными");
    assert_eq!(task["last_message"]["out"], true, "author_id 1 == self_id 1");
    assert_eq!(task["archived"], false);
    assert!(task["folder_id"].is_null());

    // Директории делят ОДИН И ТОТ ЖЕ список.
    for (dir, expected) in [
        ("tasks", vec!["chat7", "chat9"]),
        ("chats", vec!["19", "chat11", "chat3", "17"]),
        ("unread", vec!["19", "17"]),
        ("выдумка", vec!["chat7", "19", "chat11", "chat9", "chat3", "17"]),
    ] {
        let (status, body) =
            get_json(&h.app, &format!("/api/bitrix/dialogs?limit=50&filter={dir}")).await;
        assert_eq!(status, StatusCode::OK, "{dir}: {body}");
        let ids: Vec<&str> = body["dialogs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, expected, "директория {dir}");
    }
}

/// `has_more` честен на границе страницы, и пагинация не теряет строк.
#[tokio::test]
async fn has_more_is_exact_at_the_page_boundary() {
    let stub = start_stub().await;
    script_cold_start(&stub);
    let h = data_harness(&stub).await;

    let (_, first) = get_json(&h.app, "/api/bitrix/dialogs?limit=4&offset=0").await;
    assert_eq!(first["dialogs"].as_array().unwrap().len(), 4);
    assert_eq!(first["has_more"], true);

    let (_, second) = get_json(&h.app, "/api/bitrix/dialogs?limit=4&offset=4").await;
    assert_eq!(second["dialogs"].as_array().unwrap().len(), 2);
    assert_eq!(second["has_more"], false, "ровно на границе — больше нет");

    // Ровно вся страница: строк столько же, сколько записей.
    let (_, exact) = get_json(&h.app, "/api/bitrix/dialogs?limit=6&offset=0").await;
    assert_eq!(exact["has_more"], false);

    // За границей списка — пусто, но не ошибка.
    let (status, beyond) = get_json(&h.app, "/api/bitrix/dialogs?limit=10&offset=999").await;
    assert_eq!(status, StatusCode::OK);
    assert!(beyond["dialogs"].as_array().unwrap().is_empty());
    assert_eq!(beyond["has_more"], false);

    // Директория считает СВОЮ границу, а не общую.
    let (_, tasks) = get_json(&h.app, "/api/bitrix/dialogs?limit=1&filter=tasks").await;
    assert_eq!(tasks["dialogs"].as_array().unwrap().len(), 1);
    assert_eq!(tasks["has_more"], true);
}

/// `GET /filters` после холодного старта не стоит НИ ОДНОГО запроса: счётчики
/// приехали тем же пакетом (Р3).
#[tokio::test]
async fn filters_reuse_the_counters_from_the_cold_start() {
    let stub = start_stub().await;
    script_cold_start(&stub);
    let h = data_harness(&stub).await;

    get_json(&h.app, "/api/bitrix/dialogs?limit=50").await;
    assert_eq!(stub.state.calls().len(), 1);

    let (status, body) = get_json(&h.app, "/api/bitrix/filters").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        stub.state.calls().len(),
        1,
        "директории сходили в портал за уже полученными счётчиками"
    );

    let filters = body["filters"].as_array().unwrap();
    let keys: Vec<&str> = filters.iter().map(|f| f["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["all", "chats", "tasks", "unread"]);
    let titles: Vec<&str> = filters
        .iter()
        .map(|f| f["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["~/all", "~/chats", "~/tasks", "~/unread"]);

    // Счётчики живого портала: ALL=9, DIALOG=4, NOTIFY=5, CHAT=0, TASKS=0.
    assert_eq!(body["counters"]["total"], 9);
    assert_eq!(body["counters"]["dialog"], 4);
    assert_eq!(body["counters"]["notify"], 5);
    let unread = filters.iter().find(|f| f["key"] == "unread").unwrap();
    assert_eq!(unread["unread"], 4, "уведомления портала сюда не входят (Р13)");
    let all = filters.iter().find(|f| f["key"] == "all").unwrap();
    assert_eq!(all["unread"], 9, "а в бейдж — входят");
}

/// Без снимка счётчиков `GET /filters` честно ходит в портал — ровно одним
/// вызовом `im.counters.get`.
#[tokio::test]
async fn filters_fetch_the_counters_when_there_is_no_fresh_snapshot() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/filters").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stub.state.count_of("im.counters.get"), 1);
    assert_eq!(body["counters"]["total"], 9);

    // Второй вызов подряд — уже из снимка.
    get_json(&h.app, "/api/bitrix/filters").await;
    assert_eq!(stub.state.count_of("im.counters.get"), 1);
}

/// Отказ портала на списке доезжает до фронта СВОИМ кодом, а не «500».
#[tokio::test]
async fn a_portal_failure_on_the_list_keeps_its_contract_code() {
    let stub = start_stub().await;
    stub.state
        .script("batch", vec![Reply::error("insufficient_scope")]);
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "scope_missing");

    // Троттлинг портала — 429 с обратным отсчётом, а не 502. Отказ
    // заготовлен ДВАЖДЫ: он повторяемый, и транспорт по контракту делает
    // ровно один повтор — на второй ответ и смотрит фронт.
    stub.state.script(
        "batch",
        vec![Reply::query_limit(), Reply::query_limit()],
    );
    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "flood_wait");
    assert!(body["error"]["retry_after_secs"].as_u64().unwrap() >= 1);
}

// ===========================================================================
// ЛЕНТА СООБЩЕНИЙ (Ф5).
//
// Здесь проверяется то, чего не видно ни в юнит-тестах проекции, ни глазами по
// коду: КАКИЕ параметры реально уехали на портал и КАКИМ порядком страница
// вернулась фронту. Портал отдаёт историю двумя разными порядками (по убыванию
// на базовом вызове и `LAST_ID`, по возрастанию на `FIRST_ID`), и цена ошибки
// здесь — лента, которая при догрузке снизу переворачивается.
// ===========================================================================

/// Живая страница `im.dialog.messages.get` тестового портала (chat27).
const MSGS_FIXTURE: &str = include_str!("fixtures/msgs_chat27.json");

/// Результат `im.dialog.messages.get` из живой фикстуры.
fn msgs_result() -> Value {
    let v: Value = serde_json::from_str(MSGS_FIXTURE).unwrap();
    v["result"].clone()
}

/// Страница ленты со СВОИМ набором сообщений и своим порядком.
///
/// `users` и `files` пусты намеренно: живьём портал чаще всего именно так и
/// отвечает, и панель обязана работать без них.
fn page_of(ids: &[i64]) -> Value {
    let messages: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id, "chat_id": 27, "author_id": 1,
                "date": "2026-08-05T20:20:35+03:00",
                "text": format!("строка {id}"), "unread": false, "params": []
            })
        })
        .collect();
    json!({"chat_id": 27, "messages": messages, "users": [], "files": []})
}

/// Идентификаторы страницы в том порядке, в каком её отдал роут.
fn ids_of(body: &Value) -> Vec<i64> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect()
}

/// ЖИВАЯ СТРАНИЦА ИСТОРИИ ЦЕЛИКОМ: порядок нормализован, вложения, карточки,
/// клавиатура и системные строки доехали до фронта.
#[tokio::test]
async fn a_history_page_arrives_sorted_ascending_with_everything_projected() {
    let stub = start_stub().await;
    stub.state
        .script("im.dialog.messages.get", vec![Reply::Result(msgs_result())]);
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Портал отдал по убыванию — фронт получил по возрастанию.
    let ids = ids_of(&body);
    assert_eq!(ids.first(), Some(&67));
    assert_eq!(ids.last(), Some(&113));
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");

    let msg = |id: i64| -> Value {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("нет сообщения {id}"))
            .clone()
    };

    // Системная строка: автора нет, вид отдельный, код действия на месте.
    let system = msg(71);
    assert_eq!(system["system"], true);
    assert_eq!(system["out"], false);
    assert!(system["sender"].is_null());
    assert_eq!(system["action"], "CHAT_JOIN");
    assert_eq!(system["text"], "Игорь Герасимов пригласил в чат Максим Крылов");

    // Файл: карточка собралась из params.FILE_ID и корневого files[].
    let file = msg(97);
    assert_eq!(file["media"][0]["file_id"], 95);
    assert_eq!(file["media"][0]["index"], 0);
    assert_eq!(file["media"][0]["name"], "forge_probe.txt");

    // Карточка ATTACH и клавиатура KEYBOARD.
    assert_eq!(msg(79)["cards"][0]["blocks"][1]["kind"], "link");
    assert_eq!(msg(81)["reply_markup"]["rows"][0][0]["action"], "put");
    assert_eq!(msg(81)["reply_markup"]["rows"][0][0]["enabled"], true);

    // Правка, лайк и «своё сообщение».
    assert_eq!(msg(111)["edited"], true);
    assert_eq!(msg(85)["likes"][0], "1");
    assert_eq!(msg(85)["out"], true, "author_id 1 == self_id 1");

    // Ни одного адреса портала в ленте — файл скачивается только прокси (Ф8).
    let text = serde_json::to_string(&body).unwrap();
    for leak in ["urlDownload", "urlShow", "unifiedLink", "downloadUrl"] {
        assert!(!text.contains(leak), "{leak} утёк в ленту");
    }
}

/// LIMIT КЛАМПИТСЯ ПОТОЛКОМ ПЛАТФОРМЫ (50) — молча, а не ошибкой.
///
/// Проверяется по ЖУРНАЛУ СТЕНДА: по ответу видно только то, сколько строк
/// пришло, а не то, сколько мы попросили.
#[tokio::test]
async fn the_limit_is_clamped_to_the_platform_ceiling() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    for (query, expected) in [
        ("?limit=500", 50),
        ("?limit=50", 50),
        ("?limit=20", 20),
        ("?limit=0", 1),
        ("?limit=-7", 1),
        ("", 50),
        ("?limit=", 50),
    ] {
        stub.state
            .script("im.dialog.messages.get", vec![Reply::Result(page_of(&[1]))]);
        let (status, body) =
            get_json(&h.app, &format!("/api/bitrix/dialogs/chat27/messages{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {body}");

        let call = stub.state.calls_of("im.dialog.messages.get").pop().unwrap();
        assert_eq!(call.body["LIMIT"], expected, "запрос {query}");
        assert_eq!(call.body["DIALOG_ID"], "chat27");
    }
}

/// ДВА НАПРАВЛЕНИЯ — ДВА ПАРАМЕТРА, И ОДИН ПОРЯДОК НА ВЫХОДЕ.
///
/// `before_id` → `LAST_ID` (строго старее, ответ по убыванию), `after_id` →
/// `FIRST_ID` (строго новее, ответ по возрастанию). Фронт обеих разниц не
/// видит: страница всегда приходит по возрастанию.
#[tokio::test]
async fn both_pagination_directions_normalise_to_one_order() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    // Вверх: портал отвечает по УБЫВАНИЮ.
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(page_of(&[85, 83, 81]))],
    );
    let (status, body) = get_json(
        &h.app,
        "/api/bitrix/dialogs/chat27/messages?limit=3&before_id=95",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let call = stub.state.calls_of("im.dialog.messages.get").pop().unwrap();
    assert_eq!(call.body["LAST_ID"], 95, "before_id обязан стать LAST_ID");
    assert!(call.body.get("FIRST_ID").is_none());
    assert_eq!(ids_of(&body), vec![81, 83, 85], "порядок перевёрнут на выход");
    assert_eq!(body["has_more"], true, "страница пришла полной");

    // Вниз: портал отвечает по ВОЗРАСТАНИЮ — и это тот же самый порядок.
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(page_of(&[97, 99, 101]))],
    );
    let (status, body) = get_json(
        &h.app,
        "/api/bitrix/dialogs/chat27/messages?limit=50&after_id=95",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let call = stub.state.calls_of("im.dialog.messages.get").pop().unwrap();
    assert_eq!(call.body["FIRST_ID"], 95, "after_id обязан стать FIRST_ID");
    assert!(call.body.get("LAST_ID").is_none());
    assert_eq!(ids_of(&body), vec![97, 99, 101]);
    assert_eq!(body["has_more"], false, "страница неполная — истории больше нет");

    // Обе границы разом: побеждает верхняя, вторая не уезжает вовсе.
    stub.state
        .script("im.dialog.messages.get", vec![Reply::Result(page_of(&[81]))]);
    get_json(
        &h.app,
        "/api/bitrix/dialogs/chat27/messages?before_id=95&after_id=10",
    )
    .await;
    let call = stub.state.calls_of("im.dialog.messages.get").pop().unwrap();
    assert_eq!(call.body["LAST_ID"], 95);
    assert!(call.body.get("FIRST_ID").is_none());
}

/// ПУСТОЙ `users[]` — ШТАТНЫЙ СЛУЧАЙ: имена берутся из кэша, а неизвестные
/// добираются ОДНИМ пакетом на всю страницу.
#[tokio::test]
async fn names_come_from_peers_when_the_response_carries_no_users() {
    let stub = start_stub().await;
    let mut messages = Vec::new();
    for (i, author) in [15, 19, 15, 17].into_iter().enumerate() {
        messages.push(json!({
            "id": 100 + i as i64, "chat_id": 27, "author_id": author,
            "date": "2026-08-05T20:20:35+03:00", "text": "привет", "params": []
        }));
    }
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(json!({
            "chat_id": 27, "messages": messages, "users": [], "files": []
        }))],
    );
    // Добор идёт пакетом — стенд отвечает на вложенный `im.user.list.get`.
    stub.state.batch_result(
        "im.user.list.get",
        json!({
            "15": {"id": 15, "name": "Максим Крылов", "color": "#3e99ce", "status": "online"},
            "19": {"id": 19, "name": "Алексей Пруцков", "color": "#58cc47"},
            "17": {"id": 17, "name": "Ирина Кузнецова"}
        }),
    );
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Один запрос истории + один пакет добора имён. Ни одного `im.user.get`.
    assert_eq!(stub.state.count_of("im.dialog.messages.get"), 1);
    assert_eq!(stub.state.count_of("batch"), 1, "три имени — один пакет");
    assert_eq!(stub.state.count_of("im.user.get"), 0);

    let names: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["sender"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["Максим Крылов", "Алексей Пруцков", "Максим Крылов", "Ирина Кузнецова"]
    );
    assert_eq!(body["messages"][0]["sender"]["color"], "#3e99ce");

    // Вторая страница ТЕХ ЖЕ авторов имён уже не стоит.
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(json!({
            "chat_id": 27,
            "messages": [{"id": 200, "author_id": 15, "text": "ещё", "params": []},
                         {"id": 202, "author_id": 19, "text": "и ещё", "params": []}],
            "users": [], "files": []
        }))],
    );
    get_json(&h.app, "/api/bitrix/dialogs/chat27/messages?before_id=100").await;
    assert_eq!(stub.state.count_of("batch"), 1, "кэш имён обязан переживать страницу");
}

/// Отказ добора имён НЕ роняет ленту: подписью станет идентификатор, и это
/// лучше пустого экрана.
#[tokio::test]
async fn a_failed_name_lookup_never_fails_the_history_page() {
    let stub = start_stub().await;
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(json!({
            "chat_id": 27,
            "messages": [{"id": 63, "author_id": 42, "text": "привет", "params": []}],
            "users": [], "files": []
        }))],
    );
    stub.state.fail_in_batch("im.user.list.get");
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["messages"][0]["sender"]["id"], "42");
    assert!(body["messages"][0]["sender"]["name"].is_null());
}

/// ОДНО СООБЩЕНИЕ достаётся страницей на одну строку: метода «дай по id» у
/// портала нет вовсе.
#[tokio::test]
async fn a_single_message_is_fetched_as_a_one_row_page() {
    let stub = start_stub().await;
    stub.state
        .script("im.dialog.messages.get", vec![Reply::Result(page_of(&[113]))]);
    let h = data_harness(&stub).await;

    let (status, body) = get_json(&h.app, "/api/bitrix/messages/113?dialog_id=chat27").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], 113);
    assert_eq!(body["dialog_id"], "chat27");

    let call = stub.state.calls_of("im.dialog.messages.get").pop().unwrap();
    assert_eq!(call.body["LIMIT"], 1);
    assert_eq!(
        call.body["LAST_ID"], 114,
        "граница строгая: LAST_ID = mid + 1, а id портала растут шагом 2"
    );

    // Портал отдал СОСЕДА (запрошенного сообщения нет) — это 404, а не чужая
    // строка под запрошенным идентификатором.
    stub.state
        .script("im.dialog.messages.get", vec![Reply::Result(page_of(&[111]))]);
    let (status, body) = get_json(&h.app, "/api/bitrix/messages/113?dialog_id=chat27").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "unknown_message");
}

/// Отказ портала на ленте доезжает до фронта СВОИМ кодом, а не «500».
#[tokio::test]
async fn a_portal_failure_on_the_history_keeps_its_contract_code() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    // Диалог недоступен владельцу вебхука — 409, «чинить нечего».
    stub.state
        .script("im.dialog.messages.get", vec![Reply::error("ERROR_NO_ACCESS")]);
    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "send_forbidden");

    // А `ACCESS_ERROR` (с суффиксом `_ERROR`) портал отдаёт и на кривой
    // `DIALOG_ID` — это 400 по таблице §4, и путать их нельзя: первый случай
    // лечится правами, второй запросом.
    stub.state
        .script("im.dialog.messages.get", vec![Reply::error("ACCESS_ERROR")]);
    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_input");

    // Портал душит интенсивность — 429 с обратным отсчётом (повтор ровно один,
    // поэтому отказ заготовлен дважды).
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::query_limit(), Reply::query_limit()],
    );
    let (status, body) = get_json(&h.app, "/api/bitrix/dialogs/chat27/messages").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "flood_wait");
}

// ===========================================================================
// ЦИКЛ ПЕРЕПИСКИ (Ф6): отправка, правка, удаление, отметки, «печатает», лайк.
//
// Здесь проверяется то, чего не видно ни в юнит-тестах (они не знают про сеть),
// ни глазами по коду: СКОЛЬКО РАЗ и С ЧЕМ ИМЕННО плагин обратился к порталу.
// Три вопроса из четырёх в этой фазе — именно про число вызовов:
//
//   • отправка НЕ повторяется (Р9) — ровно один `im.message.add` на обрыве;
//   • сверхдлинный текст не доходит до портала вовсе (Р12);
//   • `im.dialog.unread` без `MESSAGE_ID` не порождает вызова;
//   • десять отметок о прочтении подряд стоят одного запроса.
// ===========================================================================

/// Страница ленты с ОДНИМ сообщением заданного вида.
///
/// Отдельно от [`page_of`]: тестам Ф6 нужны `params` (метка правки, состав
/// лайков) и свой текст, а не «строка N».
fn one_message(id: i64, text: &str, params: Value) -> Value {
    json!({
        "chat_id": 27,
        "messages": [{
            "id": id, "chat_id": 27, "author_id": 1,
            "date": "2026-08-05T20:20:35+03:00",
            "text": text, "unread": false, "params": params
        }],
        "users": [], "files": []
    })
}

/// Отправить текст в диалог.
async fn send(app: &Router, dialog: &str, body: Value) -> (StatusCode, Value) {
    post_json(app, &format!("/api/bitrix/dialogs/{dialog}/messages"), body).await
}

/// Р12: сверхдлинный текст отвергается ДО единого сетевого вызова.
///
/// Портал на таком тексте отвечает УСПЕХОМ, молча обрезав его на 20 006
/// символах, — то есть отправитель уверен, что собеседник прочитал всё. Именно
/// поэтому проверка живёт на нашей стороне, и именно поэтому здесь проверяется
/// не только код ответа, но и ПУСТОЙ журнал стенда.
#[tokio::test]
async fn a_too_long_message_is_rejected_without_touching_the_portal() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    let over = "я".repeat(20_001);
    let (status, body) = send(&h.app, "chat27", json!({ "text": over })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_input");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("20001"),
        "фронту нужна длина, чтобы показать её пользователю: {body}"
    );
    assert_eq!(
        stub.state.count_of("im.message.add"),
        0,
        "сверхдлинный текст не имеет права доехать до портала"
    );

    // Пустой текст — тоже 400 и тоже без сети.
    let (status, body) = send(&h.app, "chat27", json!({ "text": "   " })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(stub.state.count_of("im.message.add"), 0);

    // …а ровно потолок проходит: 20 000 символов — это ВАЛИДНОЕ сообщение.
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(one_message(114, "ок", json!([])))],
    );
    let exact = "я".repeat(20_000);
    let (status, body) = send(&h.app, "chat27", json!({ "text": exact })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stub.state.count_of("im.message.add"), 1);
}

/// Р9: транспортный отказ отправки НЕ повторяется — ровно один вызов.
///
/// Счётчик стенда — единственный способ это доказать: по ответу `503` повтор
/// неотличим от его отсутствия, а цена ошибки — второе сообщение у собеседника.
#[tokio::test]
async fn a_failed_send_reaches_the_portal_exactly_once() {
    let stub = start_stub().await;
    stub.state
        .script("im.message.add", vec![Reply::Drop, Reply::Drop]);
    let h = data_harness(&stub).await;

    let (status, body) = send(&h.app, "chat27", json!({ "text": "привет" })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "not_connected");
    assert_eq!(
        stub.state.count_of("im.message.add"),
        1,
        "повтор отправки создал бы дубль сообщения (Р9)"
    );

    // Отказ портала по существу — свой код контракта, и тоже без повтора.
    //
    // Здоровье транспорта после обрыва помечено «связи нет», и вторая ступень
    // лестницы (503) сработала бы РАНЬШЕ портала — поэтому оно возвращается в
    // норму явно: предмет проверки здесь другой.
    h.state.note_ok(chrono::Utc::now().timestamp()).await;
    stub.state
        .script("im.message.add", vec![Reply::error("ERROR_NO_ACCESS")]);
    let (status, body) = send(&h.app, "chat27", json!({ "text": "привет" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "send_forbidden");
    assert_eq!(stub.state.count_of("im.message.add"), 2);
}

/// Р10: `REPLY_ID` и текстовая цитата уходят ОДНИМ вызовом.
///
/// Портал не отражает `REPLY_ID` при чтении (`reply_to` в DTO всегда `null`),
/// поэтому без цитаты ответ в ленте неотличим от новой реплики. Разъедься эти
/// две половины — и ответ терялся бы визуально, не давая ни одной ошибки.
#[tokio::test]
async fn a_reply_sends_the_reply_id_and_the_quote_in_one_call() {
    let stub = start_stub().await;
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(one_message(114, "ответ", json!([])))],
    );
    let h = data_harness(&stub).await;

    let (status, body) = send(
        &h.app,
        "chat27",
        json!({
            "text": "мой ответ",
            "reply_to": 85,
            "reply_author": "Максим Крылов",
            "reply_text": "исходная реплика"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], 114);

    let calls = stub.state.calls_of("im.message.add");
    assert_eq!(calls.len(), 1, "цитата не имеет права стоить второго вызова");
    assert_eq!(calls[0].body["REPLY_ID"], 85);
    assert_eq!(
        calls[0].body["MESSAGE"],
        "> Максим Крылов: исходная реплика\nмой ответ"
    );
    // `URL_PREVIEW` мы не переопределяем — дефолт портала.
    assert!(calls[0].body.get("URL_PREVIEW").is_none(), "{:?}", calls[0].body);
}

/// Строка после отправки ПЕРЕЧИТЫВАЕТСЯ у портала: текст в ленте обязан быть
/// каноническим, а не тем, что отправил фронт.
///
/// `URL_PREVIEW: "Y"` (дефолт) переписывает голые ссылки в `[URL]…[/URL]`, и
/// строка, собранная локально, разъехалась бы с историей на первом же
/// сообщении со ссылкой.
#[tokio::test]
async fn a_sent_message_comes_back_as_the_portal_stored_it() {
    let stub = start_stub().await;
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(one_message(
            114,
            "смотри [URL]https://example.com[/URL]",
            json!([]),
        ))],
    );
    let h = data_harness(&stub).await;

    let (status, body) = send(
        &h.app,
        "chat27",
        json!({ "text": "смотри https://example.com" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["echoed"], true);
    assert_eq!(body["message"]["id"], 114);
    assert_eq!(body["message"]["out"], true, "своё сообщение");
    // Текст пришёл ПОСЛЕ разбора BB-кодов, ссылка отдельным сегментом.
    assert_eq!(body["message"]["text"], "смотри https://example.com");
    let kinds: Vec<&str> = body["message"]["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"url"), "ссылка обязана стать сегментом: {kinds:?}");

    // Перечитать не вышло — отправка всё равно УСПЕШНА: иначе пользователь
    // повторил бы её и получил дубль.
    stub.state
        .script("im.dialog.messages.get", vec![Reply::error("ACCESS_ERROR")]);
    let (status, body) = send(&h.app, "chat27", json!({ "text": "вторая" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["echoed"], false, "строка собрана локально");
    assert_eq!(body["message"]["text"], "вторая");
    assert_eq!(body["message"]["out"], true);
}

/// После `im.message.update` в DTO появляется `edited: true` — метку ставит
/// портал, а не мы.
#[tokio::test]
async fn an_edit_makes_the_message_edited() {
    let stub = start_stub().await;
    stub.state.script(
        "im.dialog.messages.get",
        vec![Reply::Result(one_message(
            85,
            "исправленный текст",
            json!({"IS_EDITED": "Y"}),
        ))],
    );
    let h = data_harness(&stub).await;

    let (status, body) = post_patch(
        &h.app,
        "/api/bitrix/messages/85",
        json!({ "text": "исправленный текст", "dialog_id": "chat27" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], 85);
    assert_eq!(body["echoed"], true);
    assert_eq!(body["message"]["edited"], true);
    assert_eq!(body["message"]["text"], "исправленный текст");

    let calls = stub.state.calls_of("im.message.update");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["MESSAGE_ID"], 85);
    assert_eq!(calls[0].body["MESSAGE"], "исправленный текст");
    assert!(
        calls[0].body.get("IS_EDITED").is_none(),
        "метку ставит портал, подделывать её нечем"
    );
}

/// Пустая правка НЕ доходит до портала: у него пустой `MESSAGE` УДАЛЯЕТ
/// сообщение, а удаление обязано быть явным действием с подтверждением.
#[tokio::test]
async fn an_empty_edit_never_reaches_the_portal() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    for text in ["", "   ", "\n"] {
        let (status, body) = post_patch(
            &h.app,
            "/api/bitrix/messages/85",
            json!({ "text": text, "dialog_id": "chat27" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text:?}: {body}");
        assert_eq!(body["error"]["code"], "invalid_input");
    }
    assert_eq!(stub.state.count_of("im.message.update"), 0);

    // Сверхдлинная правка — тоже 400 и тоже без сети (Р12 действует и здесь).
    let (status, _) = post_patch(
        &h.app,
        "/api/bitrix/messages/85",
        json!({ "text": "я".repeat(20_001), "dialog_id": "chat27" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(stub.state.count_of("im.message.update"), 0);
}

/// Истёкшее окно правки — `409 invalid_state`, а не «повторите позже».
#[tokio::test]
async fn an_expired_edit_window_is_an_invalid_state() {
    let stub = start_stub().await;
    stub.state
        .script("im.message.update", vec![Reply::error("CANT_EDIT_MESSAGE")]);
    let h = data_harness(&stub).await;

    let (status, body) = post_patch(
        &h.app,
        "/api/bitrix/messages/85",
        json!({ "text": "поздно", "dialog_id": "chat27" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "invalid_state");
}

/// Удаление адресуется ОДНИМ `MESSAGE_ID` — диалог методу не нужен вовсе.
#[tokio::test]
async fn a_delete_needs_nothing_but_the_message_id() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    let (status, body) = request_json(&h.app, "DELETE", "/api/bitrix/messages/85", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    let calls = stub.state.calls_of("im.message.delete");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["MESSAGE_ID"], 85);

    // Мусорный идентификатор — 404 и ни одного вызова сверх прежнего.
    let (status, body) = request_json(&h.app, "DELETE", "/api/bitrix/messages/0", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "unknown_message");
    assert_eq!(stub.state.count_of("im.message.delete"), 1);
}

/// Лайк ПЕРЕКЛЮЧАЕТСЯ: два вызова возвращают состояние к исходному.
///
/// Ответ портала всегда `true` и о состоянии не говорит ничего — поэтому строка
/// перечитывается, и `likes` фронт берёт из неё.
#[tokio::test]
async fn a_double_like_returns_the_message_to_its_original_state() {
    let stub = start_stub().await;
    stub.state.script(
        "im.dialog.messages.get",
        vec![
            Reply::Result(one_message(99, "текст", json!({"LIKE": ["1"]}))),
            Reply::Result(one_message(99, "текст", json!([]))),
        ],
    );
    let h = data_harness(&stub).await;

    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/messages/99/like",
        json!({ "dialog_id": "chat27" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"]["likes"], json!(["1"]), "лайк поставлен");

    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/messages/99/like",
        json!({ "dialog_id": "chat27" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"]["likes"], json!([]), "лайк снят");

    let calls = stub.state.calls_of("im.message.like");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body["MESSAGE_ID"], 99);
    assert_eq!(
        calls[0].body["ACTION"], "auto",
        "по умолчанию метод ПЕРЕКЛЮЧАЮЩИЙ"
    );

    // Незнакомое действие — 400 и никакого вызова.
    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/messages/99/like",
        json!({ "dialog_id": "chat27", "action": "dislike" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(stub.state.count_of("im.message.like"), 2);
}

/// `POST /api/bitrix/messages/:mid/command` — кнопка `COMMAND` клавиатуры
/// бота (forge-zw3d). Живой пример — кнопки оценки «Команда заботы
/// Битрикс24»: `BOT_ID`, `COMMAND`, `COMMAND_PARAMS` уходят В ТОЧНОСТИ теми
/// полями, что фронт получил в `ButtonDto`.
#[tokio::test]
async fn a_command_button_calls_im_message_command_with_its_fields() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/messages/51/command",
        json!({ "bot_id": 17, "command": "careButton", "command_params": "5" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["sent"], true);

    let calls = stub.state.calls_of("im.message.command");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["MESSAGE_ID"], 51);
    assert_eq!(calls[0].body["BOT_ID"], 17);
    assert_eq!(calls[0].body["COMMAND"], "careButton");
    assert_eq!(calls[0].body["COMMAND_PARAMS"], "5");

    // Пустая команда — 400 ДО сети: до портала запрос не доезжает.
    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/messages/51/command",
        json!({ "bot_id": 17, "command": "   " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(stub.state.count_of("im.message.command"), 1, "второго вызова быть не должно");

    // Мусорный идентификатор сообщения — 404, никакого вызова сверх прежнего.
    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/messages/0/command",
        json!({ "bot_id": 17, "command": "careButton" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "unknown_message");
    assert_eq!(stub.state.count_of("im.message.command"), 1);
}

/// Десять отметок о прочтении подряд стоят ОДНОГО запроса к порталу.
///
/// Отметку порождает прокрутка — десятки событий в секунду при бюджете два
/// запроса в секунду на весь плагин. Подавленные вызовы отвечают `sent: false`
/// и это не ошибка.
#[tokio::test]
async fn ten_read_marks_in_a_row_cost_one_portal_call() {
    let stub = start_stub().await;
    stub.state.script(
        "im.dialog.read",
        vec![Reply::Result(json!({"dialogId": 27, "chatId": 27, "lastId": 113, "counter": 0}))],
    );
    let h = data_harness(&stub).await;

    let mut sent = 0;
    for _ in 0..10 {
        let (status, body) = post_json(
            &h.app,
            "/api/bitrix/dialogs/chat27/read",
            json!({ "message_id": 113 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], true);
        if body["sent"] == json!(true) {
            sent += 1;
        }
    }
    assert_eq!(sent, 1, "в портал обязан уйти ровно первый вызов");
    assert_eq!(stub.state.count_of("im.dialog.read"), 1);

    // Окно считается ПО ДИАЛОГУ: соседний чат отмечается немедленно.
    let (status, body) = post_json(&h.app, "/api/bitrix/dialogs/19/read", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sent"], true);
    assert_eq!(stub.state.count_of("im.dialog.read"), 2);
    // Без границы читается ВЕСЬ диалог — `MESSAGE_ID` в параметрах нет.
    let calls = stub.state.calls_of("im.dialog.read");
    assert_eq!(calls[0].body["MESSAGE_ID"], 113);
    assert!(calls[1].body.get("MESSAGE_ID").is_none());
}

/// ЛОВУШКА ПЛАТФОРМЫ: `im.dialog.unread` без `MESSAGE_ID` не вызывается вовсе.
///
/// Портал отвечает на такой вызов `MESSAGE_ID_ERROR`, то есть запрос из бюджета
/// 2 rps тратится на заведомый отказ. Проверяется не только код ответа, но и
/// ПУСТОЙ журнал стенда.
#[tokio::test]
async fn marking_unread_without_a_message_id_never_reaches_the_portal() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    for body in [json!({}), json!({"message_id": 0}), json!({"message_id": -5})] {
        let (status, resp) = post_json(&h.app, "/api/bitrix/dialogs/chat27/unread", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
        assert_eq!(resp["error"]["code"], "invalid_input");
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("message_id"),
            "{resp}"
        );
    }
    assert_eq!(
        stub.state.count_of("im.dialog.unread"),
        0,
        "заведомо отказной вызов не имеет права тратить бюджет запросов"
    );

    // С идентификатором — уходит, и ровно с ним.
    let (status, body) = post_json(
        &h.app,
        "/api/bitrix/dialogs/chat27/unread",
        json!({ "message_id": 113 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = stub.state.calls_of("im.dialog.unread");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["DIALOG_ID"], "chat27");
    assert_eq!(calls[0].body["MESSAGE_ID"], 113);
}

/// «Печатает…» уходит не чаще раза в восемь секунд на диалог.
#[tokio::test]
async fn typing_is_sent_at_most_once_per_window() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    let (_, first) = post_json(&h.app, "/api/bitrix/dialogs/chat27/typing", json!({})).await;
    assert_eq!(first["sent"], true);
    for _ in 0..5 {
        let (status, body) = post_json(&h.app, "/api/bitrix/dialogs/chat27/typing", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sent"], false, "окно 8 с ещё не истекло");
    }
    assert_eq!(stub.state.count_of("im.dialog.writing"), 1);

    // Соседний диалог — своё окно.
    let (_, body) = post_json(&h.app, "/api/bitrix/dialogs/19/typing", json!({})).await;
    assert_eq!(body["sent"], true);
    assert_eq!(stub.state.count_of("im.dialog.writing"), 2);
}

/// Отметка о прочтении ВЫБРАСЫВАЕТ снимок счётчиков: пользователь обнулил
/// непрочитанное своими руками и обязан увидеть это сразу, а не через TTL.
#[tokio::test]
async fn reading_a_dialog_invalidates_the_counters_snapshot() {
    let stub = start_stub().await;
    script_cold_start(&stub);
    let h = data_harness(&stub).await;

    // Холодный старт положил снимок — директории отвечают бесплатно.
    get_json(&h.app, "/api/bitrix/dialogs?limit=50").await;
    get_json(&h.app, "/api/bitrix/filters").await;
    assert_eq!(stub.state.count_of("im.counters.get"), 0, "снимок из batch");

    post_json(
        &h.app,
        "/api/bitrix/dialogs/chat27/read",
        json!({ "message_id": 113 }),
    )
    .await;

    get_json(&h.app, "/api/bitrix/filters").await;
    assert_eq!(
        stub.state.count_of("im.counters.get"),
        1,
        "после прочтения счётчики обязаны перезапроситься"
    );
}

/// Закрепление и скрытие (Р14) — последние две операции управления чатами.
#[tokio::test]
async fn pin_and_hide_are_the_whole_chat_management_surface() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    // Без тела — закрепить (значение по умолчанию).
    let (status, body) = post_json(&h.app, "/api/bitrix/dialogs/chat27/pin", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);

    let (status, _) = post_json(
        &h.app,
        "/api/bitrix/dialogs/chat27/pin",
        json!({ "pinned": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let calls = stub.state.calls_of("im.recent.pin");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body["PIN"], "Y", "булев флаг портала — это Y/N");
    assert_eq!(calls[1].body["PIN"], "N");

    let (status, body) = post_json(&h.app, "/api/bitrix/dialogs/chat27/hide", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = stub.state.calls_of("im.recent.hide");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["DIALOG_ID"], "chat27");
}

/// ЛЕСТНИЦА ПРОВЕРОК держится и на новых роутах: враждебный `:id` получает 404
/// ПОСЛЕ авторизации, а тело разбирается последним.
#[tokio::test]
async fn the_write_routes_keep_the_check_ladder() {
    let stub = start_stub().await;
    let h = data_harness(&stub).await;

    // 404 по `:id` — раньше разбора тела: тело здесь заведомо негодное.
    let (status, body) = send(&h.app, "мусор", json!({ "text": "" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "unknown_dialog");

    // Правка и лайк без диалога — тоже 404 (перечитать строку нечем).
    let (status, body) = post_patch(
        &h.app,
        "/api/bitrix/messages/85",
        json!({ "text": "текст" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "unknown_dialog");

    // Ни один из отказов не стоил портале ни одного вызова.
    assert_eq!(stub.state.calls().len(), 0, "{:?}", stub.state.calls());

    // А НЕподключённый плагин отвечает 409 на всё это ещё раньше — на первой
    // ступени лестницы (проверяется на отдельном состоянии).
    let cold = login_harness(&stub);
    for (method, uri) in [
        ("POST", "/api/bitrix/dialogs/chat27/messages"),
        ("PATCH", "/api/bitrix/messages/85"),
        ("DELETE", "/api/bitrix/messages/85"),
        ("POST", "/api/bitrix/messages/85/like"),
        ("POST", "/api/bitrix/dialogs/chat27/read"),
        ("POST", "/api/bitrix/dialogs/chat27/unread"),
        ("POST", "/api/bitrix/dialogs/chat27/typing"),
        ("POST", "/api/bitrix/dialogs/chat27/pin"),
        ("POST", "/api/bitrix/dialogs/chat27/hide"),
    ] {
        let (status, body) = request_json(&cold.app, method, uri, Some(json!({}))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {body}");
        assert_eq!(body["error"]["code"], "not_authorized", "{method} {uri}");
    }
    assert_eq!(stub.state.calls().len(), 0);
}

// ===========================================================================
// REALTIME (Ф7): поток событий `im.v2.Event.*` и фолбэк-синхронизация.
//
// Здесь проверяется то, чего не видно ни юнит-тестам проекции (они не знают
// про сеть), ни тестам роутов (они не знают про воркеров): ЖИВОЙ ЦИКЛ опроса
// портала целиком — подписка, семантика `offset` как подтверждения, дедуп,
// персист курсора между «процессами», каденс и подъём realtime без рестарта.
//
// Тайминги инъецируются (`PollTimings`): боевые 4 и 20 секунд превратили бы
// каждый тест в полминуты сна, а проверяют они не длительности, а ПРАВИЛА.
// ===========================================================================

use forge_bitrix::state::{B24State, EventCursor, PollTimings};
use forge_bitrix::ws::protocol::B24ServerMsg;

/// Тайминги стенда: те же правила, но в миллисекундах.
fn fast_timings() -> PollTimings {
    PollTimings {
        active: Duration::from_millis(40),
        idle: Duration::from_millis(400),
        has_more: Duration::from_millis(5),
        recent_sync: Duration::from_millis(60),
        auth_poll: Duration::from_millis(15),
        backoff: vec![Duration::from_millis(10), Duration::from_millis(20)],
        // Каждый цикл: тест на переживание рестарта иначе зависел бы от того,
        // успел ли воркер сделать десять кругов.
        persist_every: 1,
    }
}

/// Состояние плагина, смотрящее на стенд, с изолированным `cache_dir`.
///
/// `cache_dir` приходит параметром намеренно: тест «курсор переживает
/// перезапуск процесса» поднимает ДВА состояния на одном каталоге — ровно так
/// же, как это делает перезапуск devforge.
fn realtime_state(stub: &Stub, cache_dir: &std::path::Path, poll: PollTimings) -> Arc<B24State> {
    let cfg = BitrixConfig {
        base_url_override: stub.base_url.clone(),
        cache_dir: cache_dir.to_path_buf(),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: cache_dir.join("prompts.json"),
        claude_cli_path: cache_dir.join("no-such-claude"),
        config_path: cache_dir.join("bitrix.toml"),
        connect_timeout_secs: 2,
        request_timeout_secs: 2,
        rate_limit_rps: 50,
        burst: 50,
        ..BitrixConfig::default()
    };
    B24State::with_timings(
        cfg,
        Duration::from_millis(500),
        Duration::from_millis(2000),
        poll,
    )
}

/// Отмечает состояние подключённым — так же, как это делает автомат логина.
async fn authorize(state: &Arc<B24State>) {
    state
        .set_webhook(WebhookUrl::new(WEBHOOK_INPUT))
        .await;
    state
        .set_login(forge_bitrix::login::LoginFlow::Authorized {
            user: forge_bitrix::login::B24UserDto {
                id: "1".into(),
                name: Some("Игорь".into()),
                ..Default::default()
            },
            portal: LOGIN_PORTAL.into(),
        })
        .await;
}

/// Событие «новое сообщение» в форме живого портала (camelCase + дубль
/// snake_case внутри `message`).
fn message_event(message_id: i64, dialog: &str, chat_id: i64, author: i64, text: &str) -> Value {
    json!({
        "message": {
            "id": message_id,
            "chatId": chat_id, "chat_id": chat_id,
            "authorId": author, "author_id": author,
            "date": "2026-08-05T20:20:36+03:00",
            "text": text,
            "isSystem": false,
            "params": []
        },
        "chat": {"id": chat_id, "dialogId": dialog, "name": "FORGE probe chat", "type": "chat"},
        "user": {"id": author, "name": "Максим Крылов", "firstName": "Максим"},
        "language": "ru"
    })
}

/// Ждёт выполнения условия, опрашивая его каждые 5 мс. `false` ⇒ не дождались.
async fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Ждёт ХОЛОДНЫЙ СТАРТ воркера — первый `im.v2.Event.get` без `offset`.
///
/// Отдельный шаг, потому что события, попавшие в очередь ДО него, воркер
/// отбрасывает намеренно (ретроспективы у платформы нет): тест, толкающий
/// событие до старта, проверял бы не доставку, а отбрасывание хвоста.
async fn wait_cold_start(stub: &Stub) {
    assert!(
        wait_for(Duration::from_secs(3), || {
            stub.state.count_of("im.v2.Event.get") >= 1
        })
        .await,
        "воркер не дошёл до холодного старта: {:?}",
        stub.state.calls()
    );
}

/// Собирает всё, что успело прийти в broadcast, не блокируясь.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<B24ServerMsg>) -> Vec<B24ServerMsg> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        out.push(msg);
    }
    out
}

/// Строка списка для фолбэк-синхронизации: диалог с заданным последним
/// сообщением.
fn recent_row(dialog: &str, chat_id: i64, message_id: i64, text: &str) -> Value {
    json!([{
        "id": dialog,
        "chat_id": chat_id,
        "type": "chat",
        "title": "FORGE probe chat",
        "message": {"id": message_id, "text": text, "author_id": 15,
                    "date": "2026-08-05T20:20:36+03:00", "file": false},
        "date_update": "2026-08-05T20:20:36+03:00",
        "counter": 0,
        "chat": {"id": chat_id, "name": "FORGE probe chat", "type": "chat",
                 "entity_type": "", "text_field_enabled": true},
        "user": {"id": 0}
    }])
}

// ---------------------------------------------------------------------------
// Подписка и курсор (5 именованных тестов §8/Ф7).
// ---------------------------------------------------------------------------

/// ПОДПИСКА ОДНА НА ПОДЪЁМ ВОРКЕРА.
///
/// `im.v2.Event.subscribe` идемпотентен, но каждый его вызов стоит хита лимита
/// интенсивности (2 rps на портал+IP). Воркер, зовущий подписку на каждом
/// цикле, съедал бы половину бюджета ни на что.
#[tokio::test]
async fn subscribe_is_called_once_and_is_idempotent() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = realtime_state(&stub, dir.path(), fast_timings());
    authorize(&state).await;

    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(63, "chat27", 27, 15, "первое"));

    forge_bitrix::events::spawn(&state).await;

    // Ждём НЕСКОЛЬКО циклов опроса: одна подписка на много `get` — и есть
    // проверяемое правило.
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.count_of("im.v2.Event.get") >= 3).await,
        "воркер не сделал трёх циклов: {:?}",
        stub.state.calls()
    );
    assert_eq!(
        stub.state.count_of("im.v2.Event.subscribe"),
        1,
        "подписка обязана быть ровно одна на подъём воркера"
    );

    // ИДЕМПОТЕНТНОСТЬ: повторный вызов не ломает подписку и не порождает
    // вторую очередь — события продолжают приходить той же чередой.
    let client = state.api_client().await.unwrap();
    assert!(client.event_subscribe().await.unwrap());
    assert!(client.event_subscribe().await.unwrap());
    let before = stub.state.acked();
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(65, "chat27", 27, 15, "второе"));
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.acked() > before).await,
        "после повторной подписки поток обязан продолжиться"
    );

    forge_bitrix::registry::shutdown_portal(&state).await;
}

/// `offset` — ЭТО ПОДТВЕРЖДЕНИЕ, А НЕ ПОЗИЦИЯ ЧТЕНИЯ.
///
/// Самое неочевидное место всей фазы, проверенное живьём: пока пачка не
/// подтверждена, портал отдаёт её снова; после подтверждения повтор того же
/// `offset` не приносит ничего.
///
/// Тест идёт на уровне КЛИЕНТА, без воркера: правило принадлежит протоколу
/// портала, и проверять его через цикл опроса значило бы смешивать две ошибки в
/// одну.
#[tokio::test]
async fn offset_acks_and_a_repeated_offset_returns_nothing() {
    let stub = start_stub().await;
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(63, "chat27", 27, 15, "раз"));
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(65, "chat27", 27, 15, "два"));
    let c = client(&stub);

    // Холодный старт: без `offset` приезжает вся неподтверждённая очередь.
    let first = c.events_get(None, 1000).await.unwrap();
    assert_eq!(first.events.len(), 2);
    assert!(!first.has_more);
    let next = first.next_offset.as_string();
    assert_eq!(next, "8", "nextOffset = последний eventId + 1");

    // Повтор БЕЗ подтверждения отдаёт ровно то же самое — вот из-за чего лента
    // задвоилась бы без ack.
    let again = c.events_get(None, 1000).await.unwrap();
    assert_eq!(
        again.events.len(),
        2,
        "неподтверждённые события портал переотдаёт"
    );

    // Подтверждаем — очередь пуста.
    let acked = c.events_get(Some(&next), 100).await.unwrap();
    assert!(acked.events.is_empty(), "{:?}", acked.events);
    assert_eq!(acked.next_offset.as_string(), next, "курсор не сдвинулся");
    assert!(!acked.has_more);

    // И повтор ТОГО ЖЕ offset тоже ничего не приносит.
    let repeat = c.events_get(Some(&next), 100).await.unwrap();
    assert!(repeat.events.is_empty(), "{:?}", repeat.events);

    // А новое событие после подтверждения приезжает как ни в чём не бывало.
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(67, "chat27", 27, 15, "три"));
    let fresh = c.events_get(Some(&next), 100).await.unwrap();
    assert_eq!(fresh.events.len(), 1);
    assert_eq!(fresh.events[0].data.message.as_ref().unwrap().id, 67);
}

/// ДУБЛИ РЕАЛЬНЫ (Р19): один `message.id` под двумя `eventId` обязан попасть в
/// ленту РОВНО ОДИН РАЗ.
///
/// Наблюдалось живьём на тестовом портале: `message.id = 61` приехал под
/// `eventId` 1 и 3. Без кольцевого буфера каждая такая пара давала бы две
/// одинаковые строки — и починить это перезагрузкой вкладки было бы нельзя,
/// потому что дубль настоящий, он в потоке.
#[tokio::test]
async fn duplicate_message_ids_are_deduplicated() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = realtime_state(&stub, dir.path(), fast_timings());
    authorize(&state).await;
    let mut rx = state.subscribe();

    forge_bitrix::events::spawn(&state).await;
    wait_cold_start(&stub).await;

    // ОДНО И ТО ЖЕ сообщение под разными eventId — плюс соседнее, чтобы было
    // видно, что дедуп глушит не всё подряд.
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(63, "chat27", 27, 15, "дубль"));
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(63, "chat27", 27, 15, "дубль"));
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(65, "chat27", 27, 15, "другое"));

    let mut ids: Vec<i64> = Vec::new();
    assert!(
        wait_for(Duration::from_secs(5), || {
            for frame in drain(&mut rx) {
                if let B24ServerMsg::MessageNew { message, .. } = frame {
                    ids.push(message.id);
                }
            }
            ids.len() >= 2
        })
        .await,
        "не дождались обеих строк: {ids:?}"
    );
    // Ещё один цикл — на случай, если дубль отстал от соседа.
    tokio::time::sleep(Duration::from_millis(120)).await;
    for frame in drain(&mut rx) {
        if let B24ServerMsg::MessageNew { message, .. } = frame {
            ids.push(message.id);
        }
    }
    forge_bitrix::registry::shutdown_portal(&state).await;

    assert_eq!(ids, vec![63, 65], "дубль обязан отсеяться, соседи — нет");
}

/// КУРСОР ПЕРЕЖИВАЕТ ПЕРЕЗАПУСК ПРОЦЕССА.
///
/// Без персиста после каждого рестарта devforge поток начинался бы с
/// неподтверждённого хвоста очереди: портал переотдал бы всё, что мы уже
/// показали, и лента задвоилась бы ровно в момент, когда пользователь ждёт
/// работающую панель.
#[tokio::test]
async fn the_cursor_survives_a_process_restart() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();

    // --- «первый процесс» ---
    let first = realtime_state(&stub, dir.path(), fast_timings());
    authorize(&first).await;
    let mut rx1 = first.subscribe();

    forge_bitrix::events::spawn(&first).await;
    wait_cold_start(&stub).await;
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(63, "chat27", 27, 15, "до рестарта"));

    let mut shown_before: Vec<i64> = Vec::new();
    assert!(
        wait_for(Duration::from_secs(5), || {
            for frame in drain(&mut rx1) {
                if let B24ServerMsg::MessageNew { message, .. } = frame {
                    shown_before.push(message.id);
                }
            }
            !shown_before.is_empty()
        })
        .await,
        "первый процесс не показал сообщение"
    );
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.acked() >= 6).await,
        "первый процесс не подтвердил событие"
    );
    forge_bitrix::registry::shutdown_portal(&first).await;
    assert_eq!(shown_before, vec![63]);

    // Курсор лежит на диске в <cache_dir>/events.json.
    let path = dir.path().join("events.json");
    assert!(path.is_file(), "курсор не записан");
    let saved: EventCursor = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.offset, "6", "подтверждена позиция после eventId 5");
    assert!(saved.saved_at > 0);

    // --- «второй процесс»: НОВОЕ состояние на том же каталоге ---
    let calls_before = stub.state.count_of("im.v2.Event.get");
    let second = realtime_state(&stub, dir.path(), fast_timings());
    authorize(&second).await;
    let mut rx2 = second.subscribe();
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(65, "chat27", 27, 15, "после рестарта"));

    forge_bitrix::events::spawn(&second).await;
    assert!(
        wait_for(Duration::from_secs(3), || {
            stub.state.count_of("im.v2.Event.get") > calls_before + 1
        })
        .await,
        "второй процесс не пошёл в поток"
    );
    forge_bitrix::registry::shutdown_portal(&second).await;

    // Первый же запрос после рестарта НЕСЁТ сохранённый offset — то есть
    // холодного старта не было.
    let after = &stub.state.calls_of("im.v2.Event.get")[calls_before];
    assert_eq!(
        after.body["offset"], "6",
        "воркер обязан продолжить с сохранённой позиции: {:?}",
        after.body
    );
    assert_eq!(after.body["limit"], 100, "цикл, а не холодный старт");

    let shown_after: Vec<i64> = drain(&mut rx2)
        .into_iter()
        .filter_map(|f| match f {
            B24ServerMsg::MessageNew { message, .. } => Some(message.id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shown_after,
        vec![65],
        "старое сообщение не показано повторно"
    );
}

/// Р18: КУРСОР СТАРШЕ ШЕСТИ ЧАСОВ НЕ ЗАСЛУЖИВАЕТ ДОВЕРИЯ.
///
/// Дело не в самом `offset` — он сработал бы, — а в состоянии панели: за
/// полсуток простоя список, счётчики и открытая лента разъехались с порталом
/// настолько, что догонять их событиями дороже и ненадёжнее, чем перечитать.
#[tokio::test]
async fn a_stale_cursor_triggers_a_full_resync() {
    let stub = start_stub().await;
    stub.state
        .batch_result("im.recent.get", recent_row("chat27", 27, 95, "последнее"));
    stub.state
        .batch_result("im.counters.get", default_result("im.counters.get"));

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    // Курсор семичасовой давности — на час старше порога.
    let stale = EventCursor {
        offset: "2".into(),
        saved_at: chrono::Utc::now().timestamp() - 7 * 3600,
    };
    std::fs::write(
        dir.path().join("events.json"),
        serde_json::to_vec(&stale).unwrap(),
    )
    .unwrap();

    let state = realtime_state(&stub, dir.path(), fast_timings());
    authorize(&state).await;
    let mut rx = state.subscribe();

    // Хвост очереди, накопившийся за время простоя: показывать его нельзя —
    // историю грузит UI.
    for id in [63, 65, 67] {
        stub.state
            .push_event("ONIMV2MESSAGEADD", message_event(id, "chat27", 27, 15, "старьё"));
    }

    forge_bitrix::events::spawn(&state).await;
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.count_of("im.v2.Event.get") >= 2).await,
        "воркер не поднялся"
    );
    forge_bitrix::registry::shutdown_portal(&state).await;

    let frames = drain(&mut rx);
    assert!(
        frames.iter().any(|f| matches!(f, B24ServerMsg::Resync)),
        "фронту обязан уйти resync: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, B24ServerMsg::DialogsPatch { .. })),
        "список обязан перезагрузиться целиком: {frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, B24ServerMsg::MessageNew { .. })),
        "накопленный хвост очереди в ленту НЕ показывается: {frames:?}"
    );

    // Полная перезагрузка списка — это `im.recent.get` БЕЗ `LAST_SYNC_DATE`.
    let batch = stub.state.calls_of("batch");
    assert!(!batch.is_empty(), "фолбэк-синхронизация не позвана");
    let cmd = batch[0].body["cmd"]["recent"].as_str().unwrap();
    assert!(
        !cmd.contains("LAST_SYNC_DATE"),
        "перезагрузка обязана быть полной: {cmd}"
    );

    // Подписка сделана заново, а курсор пересобран холодным стартом: первый
    // `get` ушёл БЕЗ offset и с потолком платформы.
    assert_eq!(stub.state.count_of("im.v2.Event.subscribe"), 1);
    let first = &stub.state.calls_of("im.v2.Event.get")[0];
    assert!(
        first.body.get("offset").is_none(),
        "устаревший offset не подтверждается: {:?}",
        first.body
    );
    assert_eq!(first.body["limit"], 1000);
}

// ---------------------------------------------------------------------------
// Каденс и рантайм (4 именованных теста §8/Ф7 + Lagged и idle).
// ---------------------------------------------------------------------------

/// ВЕБХУК, ВСТАВЛЕННЫЙ В РАБОТАЮЩЕМ ПРОЦЕССЕ, ПОДНИМАЕТ REALTIME БЕЗ РЕСТАРТА.
///
/// Ради этого воркеры и стартуют всегда: до подключения они ждут на `Notify` и
/// не делают НИ ОДНОГО сетевого вызова, а `set_login(Authorized)` будит их
/// немедленно.
#[tokio::test]
async fn a_webhook_inserted_at_runtime_brings_realtime_up_without_a_restart() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = realtime_state(&stub, dir.path(), fast_timings());
    // ВЕБХУКА НЕТ: состояние `not_configured`, как после первого запуска.
    assert_eq!(state.login_state_dto().await.state, "not_configured");

    forge_bitrix::events::spawn(&state).await;
    forge_bitrix::recent_sync::spawn(&state).await;

    // Дали воркерам заведомо больше времени, чем их auth_poll: они обязаны
    // молчать, а не долбить портал в ожидании подключения.
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        stub.state.calls().is_empty(),
        "до подключения воркеры не имеют права ходить в сеть: {:?}",
        stub.state.calls()
    );

    // Пользователь вставил вебхук — рестарта процесса НЕ БЫЛО.
    let mut rx = state.subscribe();
    authorize(&state).await;

    assert!(
        wait_for(Duration::from_secs(3), || {
            stub.state.count_of("im.v2.Event.subscribe") == 1
                && stub.state.count_of("im.v2.Event.get") >= 1
        })
        .await,
        "realtime не поднялся после вставки вебхука: {:?}",
        stub.state.calls()
    );
    // Сообщение приходит уже ПОСЛЕ подъёма потока: то, что успело накопиться
    // до подписки, воркер отбрасывает намеренно (ретроспективы нет).
    stub.state
        .push_event("ONIMV2MESSAGEADD", message_event(63, "chat27", 27, 15, "живьём"));
    assert!(
        wait_for(Duration::from_secs(3), || {
            drain(&mut rx)
                .iter()
                .any(|f| matches!(f, B24ServerMsg::MessageNew { .. }))
        })
        .await,
        "сообщение не доехало до подписчиков"
    );

    forge_bitrix::registry::shutdown_portal(&state).await;
}

/// КАДЕНС (Р4): `hasMore: true` укорачивает паузу до двух секунд.
///
/// Проверяется в двух измерениях сразу:
///
/// 1. **правило на БОЕВЫХ значениях** — именно они уедут в прод;
/// 2. **живой цикл на стенде** — что правило действительно применяется, а не
///    лежит неиспользованной функцией.
#[tokio::test]
async fn has_more_shortens_the_interval_to_two_seconds() {
    // 1. Правило на боевом конфиге.
    let prod = PollTimings::from_config(&BitrixConfig::default());
    assert_eq!(prod.has_more, Duration::from_secs(2));
    assert_eq!(
        forge_bitrix::events::next_interval(&prod, true, 1),
        Duration::from_secs(2),
        "hasMore обязан укорачивать паузу до двух секунд"
    );
    assert_eq!(
        forge_bitrix::events::next_interval(&prod, false, 1),
        Duration::from_secs(4)
    );

    // 2. Живой цикл: очередь отдаётся по одному событию, значит первые ответы
    // приходят с `hasMore: true`.
    let stub = start_stub().await;
    stub.state.events_page_cap(1);
    for id in [63, 65, 67] {
        stub.state
            .push_event("ONIMV2MESSAGEADD", message_event(id, "chat27", 27, 15, "поток"));
    }

    let dir = tempfile::tempdir().unwrap();
    let mut poll = fast_timings();
    // Разрыв между «дренажом» и «обычным» интервалом сделан заведомо большим,
    // чтобы измерение не зависело от планировщика.
    poll.has_more = Duration::from_millis(10);
    poll.active = Duration::from_millis(400);
    poll.idle = Duration::from_millis(400);
    let state = realtime_state(&stub, dir.path(), poll);
    authorize(&state).await;

    forge_bitrix::events::spawn(&state).await;
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.count_of("im.v2.Event.get") >= 4).await,
        "очередь не продренировалась"
    );
    forge_bitrix::registry::shutdown_portal(&state).await;

    let calls = stub.state.calls_of("im.v2.Event.get");
    // Пока `hasMore` был истиной (первые три ответа), паузы обязаны быть
    // КОРОТКИМИ — иначе очередь из трёх событий разгребалась бы больше секунды.
    let drain_gap = calls[2].at.duration_since(calls[1].at);
    assert!(
        drain_gap < Duration::from_millis(200),
        "дренаж очереди обязан идти учащённо, а пауза была {drain_gap:?}"
    );
    // А когда очередь опустела — пауза стала обычной.
    let idle_gap = calls[3].at.duration_since(calls[2].at);
    assert!(
        idle_gap >= Duration::from_millis(300),
        "после дренажа обязан вернуться обычный интервал, а пауза была {idle_gap:?}"
    );
}

/// БЕЗ WS-ПОДПИСЧИКОВ ИНТЕРВАЛ УХОДИТ В IDLE (Р4).
///
/// Панель закрыта у всех — тратить на неё бюджет 2 rps незачем. Признак
/// «панель открыта» ровно один: `broadcast.receiver_count()`. Никаких таймеров
/// активности пользователя.
#[tokio::test]
async fn without_ws_subscribers_the_interval_drops_to_idle() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let mut poll = fast_timings();
    poll.active = Duration::from_millis(20);
    poll.idle = Duration::from_millis(400);
    let state = realtime_state(&stub, dir.path(), poll);
    authorize(&state).await;

    // Подписчиков нет вовсе.
    assert_eq!(state.ws_subscribers(), 0);
    forge_bitrix::events::spawn(&state).await;
    // Три вызова, а не два: между ХОЛОДНЫМ СТАРТОМ и первым циклом паузы нет
    // вовсе (курсор только что получен), и мерить надо промежуток между
    // циклами.
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.count_of("im.v2.Event.get") >= 3).await,
        "воркер не сделал двух циклов"
    );
    let idle_calls = stub.state.calls_of("im.v2.Event.get");
    let idle_gap = idle_calls[2].at.duration_since(idle_calls[1].at);
    assert!(
        idle_gap >= Duration::from_millis(300),
        "без подписчиков пауза обязана быть длинной, а была {idle_gap:?}"
    );

    // ОТКРЫЛАСЬ ВКЛАДКА. Воркер сейчас спит длинную idle-паузу, и досыпать её
    // до конца значило бы показать человеку первое сообщение через треть
    // минуты (замерено живьём: 13.8 с при боевых настройках). Подписка обязана
    // прервать сон немедленно.
    let woke_at = Instant::now();
    let _rx = state.subscribe();
    let before = stub.state.count_of("im.v2.Event.get");
    assert!(
        wait_for(Duration::from_millis(250), || {
            stub.state.count_of("im.v2.Event.get") > before
        })
        .await,
        "появление подписчика обязано прерывать idle-паузу, а не досыпаться"
    );
    assert!(
        woke_at.elapsed() < Duration::from_millis(300),
        "idle-пауза не прервалась: прошло {:?}",
        woke_at.elapsed()
    );
    assert!(
        wait_for(Duration::from_secs(3), || {
            stub.state.count_of("im.v2.Event.get") >= before + 3
        })
        .await,
        "с подписчиком цикл обязан ускориться"
    );
    let calls = stub.state.calls_of("im.v2.Event.get");
    let active_gap = calls[calls.len() - 1]
        .at
        .duration_since(calls[calls.len() - 2].at);
    assert!(
        active_gap < Duration::from_millis(250),
        "с подписчиком пауза обязана быть короткой, а была {active_gap:?}"
    );

    forge_bitrix::registry::shutdown_portal(&state).await;
}

/// ПРАВКА ПРИЕЗЖАЕТ МЕДЛЕННЫМ ВОРКЕРОМ, А НЕ СОБЫТИЕМ.
///
/// `ONIMV2MESSAGEUPDATE` документирован, но живьём не приходит НИ РАЗУ —
/// поэтому единственный источник чужих правок это фолбэк-синхронизация,
/// сравнивающая строку списка с прошлым снимком. Задержка до
/// `recent_sync_secs` — поведение по проекту, а не баг.
#[tokio::test]
async fn an_edit_is_delivered_by_the_slow_sync_worker_not_by_events() {
    let stub = start_stub().await;
    stub.state
        .batch_result("im.recent.get", recent_row("chat27", 27, 95, "было"));
    stub.state
        .batch_result("im.counters.get", default_result("im.counters.get"));

    let dir = tempfile::tempdir().unwrap();
    let state = realtime_state(&stub, dir.path(), fast_timings());
    authorize(&state).await;
    let mut rx = state.subscribe();

    // Оба воркера, как в бою: поток событий обязан промолчать, медленный —
    // принести правку.
    forge_bitrix::events::spawn(&state).await;
    forge_bitrix::recent_sync::spawn(&state).await;

    // Первый проход синхронизации запомнил строку.
    assert!(
        wait_for(Duration::from_secs(3), || stub.state.count_of("batch") >= 1).await,
        "фолбэк-воркер не сходил в портал"
    );
    let _ = drain(&mut rx);

    // Кто-то отредактировал ПОСЛЕДНЕЕ сообщение: идентификатор тот же, текст
    // другой. Никакого события портал при этом не присылает.
    stub.state
        .batch_result("im.recent.get", recent_row("chat27", 27, 95, "стало"));

    let mut edited = None;
    assert!(
        wait_for(Duration::from_secs(5), || {
            for frame in drain(&mut rx) {
                if let B24ServerMsg::MessageUpdate { message, .. } = frame {
                    edited = Some(message);
                    return true;
                }
            }
            false
        })
        .await,
        "правка не доехала фолбэк-воркером"
    );
    forge_bitrix::registry::shutdown_portal(&state).await;

    let message = edited.unwrap();
    assert_eq!(message.id, 95);
    assert_eq!(message.text, "стало");
    assert!(message.edited, "метка «(изм.)» обязана стоять");

    // И главное: поток событий при этом РАБОТАЛ и не принёс ничего — правки в
    // нём нет вовсе.
    assert!(
        stub.state.count_of("im.v2.Event.get") >= 1,
        "воркер событий обязан крутиться параллельно"
    );
    assert!(
        !stub
            .state
            .calls_of("im.v2.Event.get")
            .is_empty(),
        "поток событий не опрашивался"
    );
}

/// Р19: ОТСТАВШИЙ ПОДПИСЧИК ПОЛУЧАЕТ `resync`, А НЕ ДОСЫЛКУ ПРОПУЩЕННОГО.
///
/// Восстановить пропущенное неоткуда — broadcast хранит только хвост. Единственный
/// честный ответ: «перечитай состояние по REST». Попытка досылки дала бы ленту,
/// собранную из дырявой последовательности, причём молча.
#[tokio::test]
async fn a_lagging_subscriber_gets_resync_instead_of_the_lost_frames() {
    let stub = start_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let state = realtime_state(&stub, dir.path(), fast_timings());
    let mut rx = state.subscribe();

    // Ровно то, что делает активный портал со вкладкой, которую усыпил
    // браузер: кадров больше, чем ёмкость канала.
    for _ in 0..(forge_bitrix::state::BROADCAST_CAPACITY + 16) {
        state.broadcast_event(B24ServerMsg::Ping);
    }

    assert_eq!(
        forge_bitrix::ws::broadcast_frame(rx.recv().await),
        Some(B24ServerMsg::Resync),
        "отставший подписчик обязан получить resync"
    );
    // Дальше поток идёт с ЖИВОГО хвоста — досылки нет и быть не может.
    assert_eq!(
        forge_bitrix::ws::broadcast_frame(rx.recv().await),
        Some(B24ServerMsg::Ping)
    );
}
