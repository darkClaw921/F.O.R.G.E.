//! Пакетный вызов `batch` — главный инструмент экономии бюджета запросов.
//!
//! # Зачем
//!
//! Внешний `batch` стоит ОДИН хит лимита интенсивности (2 req/s) и **не
//! расходует `operating`-время** ни одного из вложенных методов. Холодный
//! старт панели — это `im.recent.get` + `im.recent.list` + `im.counters.get` +
//! добор имён пользователей: по отдельности это четыре хита и две секунды
//! ожидания на боевом лимите, одним `batch` — один хит и один round-trip.
//!
//! # Формат, который принимает портал
//!
//! ```json
//! {"halt": 0, "cmd": {"recent": "im.recent.get?SKIP_OPENLINES=Y",
//!                     "counters": "im.counters.get"}}
//! ```
//!
//! То есть каждая команда — это СТРОКА `метод?query`, а не объект. Отсюда два
//! требования, которые легко нарушить:
//!
//! 1. **Значения обязаны быть urlencode.** Ловушка живая: `+` в датах
//!    (`2026-08-05T20:14:43+03:00`) без кодирования читается порталом как
//!    пробел, и фильтр по дате молча съезжает на три часа. Кодируется всё,
//!    кроме `A-Za-z0-9-_.~` — см. [`percent_encode`].
//! 2. **Вложенность выражается скобками в ИМЕНИ**: `filter[>ID]=10`,
//!    `select[0]=ID`. JSON-объект разворачивается в такие имена
//!    [`encode_params`].
//!
//! # Лимит 50 и молчаливая потеря
//!
//! Команд в пачке не больше **50**. 51-я и дальше не выполняются, но общей
//! ошибки при этом НЕТ: они приезжают поключево в `result_error` с кодом
//! `ERROR_BATCH_LENGTH_EXCEEDED`. Вызывающий, который смотрит только на
//! `Ok`/`Err`, потеряет их молча — поэтому нарезка на пачки по 50 сделана
//! автоматической ([`chunks`]), а не оставлена на дисциплину.
//!
//! # Ответ
//!
//! ```json
//! {"result": {"result": {"recent": […]},
//!             "result_error": [],
//!             "result_total": {"dialogusers": 2},
//!             "result_next": [],
//!             "result_time": {"recent": {…}}}}
//! ```
//!
//! `result_error`, `result_next` и `result_total` — PHP-массивы: пустыми они
//! приезжают как `[]`, заполненными как объект (см.
//! [`super::schema::PhpMap`]). Демультиплексирование идёт по ТЕМ ЖЕ ключам,
//! что были в запросе.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::error::B24Error;
use super::methods::M_BATCH;
use super::B24Client;

/// Максимум команд в одной пачке.
///
/// Ровно платформенный: 51-я команда молча не выполняется.
pub const MAX_BATCH_COMMANDS: usize = 50;

/// Символы, которые НЕ кодируются (RFC 3986, unreserved).
///
/// Всё остальное — включая `+`, `:`, `/`, пробел и кириллицу — уезжает в
/// `%XX`. Список намеренно минимальный: «безопасные» символы у разных
/// декодеров разные, а лишнее кодирование портал понимает всегда.
const UNRESERVED: &[u8] = b"-_.~";

/// Одна команда пачки.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchCall {
    /// Ключ, по которому вызывающий найдёт результат. Возвращается порталом
    /// как есть.
    pub key: String,
    /// Имя метода портала.
    pub method: String,
    /// Параметры метода (JSON-объект; вложенность допустима).
    pub params: Value,
}

impl BatchCall {
    /// Собирает команду.
    ///
    /// `params` ожидается объектом; скаляр или массив на верхнем уровне
    /// трактуется как «параметров нет» — портал такой формы не понимает, и
    /// тихо отправить её было бы хуже, чем проигнорировать.
    pub fn new(key: impl Into<String>, method: impl Into<String>, params: Value) -> Self {
        Self {
            key: key.into(),
            method: method.into(),
            params,
        }
    }

    /// Строка команды: `метод?query` либо просто `метод`.
    pub fn to_command(&self) -> String {
        let query = encode_params(&self.params);
        if query.is_empty() {
            self.method.clone()
        } else {
            format!("{}?{}", self.method, query)
        }
    }
}

/// Ошибка ОДНОЙ команды пачки.
///
/// Отдельный тип, а не [`B24Error`]: у команды нет ни HTTP-статуса, ни
/// транспорта — только код и описание. Превращение в контракт REST делает
/// вызывающий через [`super::classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchError {
    /// Машинный код портала (`ERROR_BATCH_LENGTH_EXCEEDED`, `PARAMS_ERROR`, …).
    pub code: String,
    /// Пояснение портала.
    pub description: String,
}

/// Демультиплексированный результат пачки.
///
/// Успехи и отказы РАЗДЕЛЕНЫ по исходным ключам: `batch` с `halt=0` выполняет
/// все команды независимо, и «одна упала — всё пропало» здесь было бы неправдой.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchOutcome {
    /// Успешные результаты по ключам запроса.
    pub ok: HashMap<String, Value>,
    /// Отказавшие команды по тем же ключам.
    pub errors: HashMap<String, BatchError>,
    /// `result_total` — общее число записей у постраничных методов.
    pub totals: HashMap<String, i64>,
    /// `result_next` — смещение следующей страницы.
    pub next: HashMap<String, i64>,
}

impl BatchOutcome {
    /// Забирает результат команды по ключу.
    pub fn take(&mut self, key: &str) -> Option<Value> {
        self.ok.remove(key)
    }

    /// Результат команды по ключу.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.ok.get(key)
    }

    /// Ошибка команды по ключу.
    pub fn error(&self, key: &str) -> Option<&BatchError> {
        self.errors.get(key)
    }

    /// Влить в себя результат следующей пачки.
    fn merge(&mut self, other: BatchOutcome) {
        self.ok.extend(other.ok);
        self.errors.extend(other.errors);
        self.totals.extend(other.totals);
        self.next.extend(other.next);
    }
}

/// Процентное кодирование одного значения.
///
/// Кодируется ВСЁ, кроме `A-Za-z0-9-_.~`. Пробел становится `%20`, а не `+`:
/// плюс в query означает пробел, и «умное» кодирование пробела плюсом сделало
/// бы невозможным передать сам плюс.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || UNRESERVED.contains(b) {
            out.push(*b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Разворачивает JSON-объект параметров в query-строку формата Bitrix24.
///
/// Правила:
/// - вложенный объект → `parent[key]`;
/// - массив → `parent[index]` (индексы обязательны: `select[]=` портал
///   принимает не везде);
/// - `null` → пустое значение;
/// - `true`/`false` → `1`/`0`;
/// - имена и значения кодируются [`percent_encode`], структурные скобки
///   остаются литеральными.
///
/// Не-объект на верхнем уровне даёт пустую строку: параметров нет.
pub fn encode_params(params: &Value) -> String {
    let mut out = Vec::new();
    if let Some(obj) = params.as_object() {
        for (k, v) in obj {
            encode_into(&percent_encode(k), v, &mut out);
        }
    }
    out.join("&")
}

fn encode_into(name: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                encode_into(&format!("{name}[{}]", percent_encode(k)), v, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                encode_into(&format!("{name}[{i}]"), v, out);
            }
        }
        Value::Null => out.push(format!("{name}=")),
        Value::Bool(b) => out.push(format!("{name}={}", u8::from(*b))),
        Value::Number(n) => out.push(format!("{name}={}", percent_encode(&n.to_string()))),
        Value::String(s) => out.push(format!("{name}={}", percent_encode(s))),
    }
}

/// Режет список команд на пачки по [`MAX_BATCH_COMMANDS`].
///
/// Существует ради молчаливой потери: 51-я команда не выполняется, а общей
/// ошибки нет. Пустой список даёт пустой результат, а не одну пустую пачку.
pub fn chunks(calls: Vec<BatchCall>) -> Vec<Vec<BatchCall>> {
    if calls.is_empty() {
        return Vec::new();
    }
    calls
        .chunks(MAX_BATCH_COMMANDS)
        .map(<[BatchCall]>::to_vec)
        .collect()
}

/// Собирает тело одного `batch`-запроса.
///
/// `halt: 0` — «не останавливаться на первой ошибке»: панели нужны все
/// результаты, которые портал в состоянии отдать, а не первый успешный
/// префикс.
pub fn body(chunk: &[BatchCall]) -> Value {
    let mut cmd = Map::new();
    for call in chunk {
        cmd.insert(call.key.clone(), Value::String(call.to_command()));
    }
    json!({"halt": 0, "cmd": Value::Object(cmd)})
}

/// Разбирает `result` одного `batch`-ответа.
///
/// Все четыре словаря (`result`, `result_error`, `result_total`,
/// `result_next`) — PHP-массивы: пустыми приезжают как `[]`. Форма проверяется
/// на каждом, потому что пустым может оказаться любой из них.
pub fn demux(result: &Value) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();

    if let Some(ok) = result.get("result").and_then(Value::as_object) {
        for (k, v) in ok {
            outcome.ok.insert(k.clone(), v.clone());
        }
    }
    if let Some(errs) = result.get("result_error").and_then(Value::as_object) {
        for (k, v) in errs {
            outcome.errors.insert(k.clone(), batch_error(v));
        }
    }
    if let Some(totals) = result.get("result_total").and_then(Value::as_object) {
        for (k, v) in totals {
            if let Some(n) = as_i64(v) {
                outcome.totals.insert(k.clone(), n);
            }
        }
    }
    if let Some(next) = result.get("result_next").and_then(Value::as_object) {
        for (k, v) in next {
            if let Some(n) = as_i64(v) {
                outcome.next.insert(k.clone(), n);
            }
        }
    }
    outcome
}

/// Ошибка одной команды приезжает объектом `{error, error_description}`, но у
/// части методов — голой строкой. Обе формы дают один тип.
fn batch_error(value: &Value) -> BatchError {
    match value {
        Value::String(s) => BatchError {
            code: s.clone(),
            description: String::new(),
        },
        _ => BatchError {
            code: value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description: value
                .get("error_description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
    }
}

/// Число, присланное числом ИЛИ строкой (портал делает и так, и так).
fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

impl B24Client {
    /// Выполняет пакет команд, автоматически нарезая его по 50.
    ///
    /// Пачки идут ПОСЛЕДОВАТЕЛЬНО: параллельный запуск дал бы тот самый
    /// «несколько параллельных серий запросов с одного IP», который
    /// документация портала прямо запрещает, а троттлер всё равно выстроил бы
    /// их в очередь.
    ///
    /// Пустой список не порождает ни одного запроса.
    ///
    /// # Errors
    ///
    /// Ошибка САМОГО `batch` (транспорт, лимит, отказ портала) — то есть
    /// случай, когда не выполнилась ни одна команда пачки. Отказы отдельных
    /// команд ошибкой НЕ являются и лежат в [`BatchOutcome::errors`].
    pub async fn batch(&self, calls: Vec<BatchCall>) -> Result<BatchOutcome, B24Error> {
        let mut outcome = BatchOutcome::default();
        for chunk in chunks(calls) {
            let resp = self.call_value(M_BATCH, body(&chunk)).await?;
            outcome.merge(demux(&resp.result));
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(key: &str) -> BatchCall {
        BatchCall::new(key, "im.counters.get", json!({}))
    }

    /// ЖИВАЯ ЛОВУШКА: `+` в дате обязан стать `%2B`, иначе портал прочитает
    /// пробел и сдвинет фильтр на часовой пояс.
    #[test]
    fn plus_in_a_date_is_encoded() {
        let params = json!({"LAST_SYNC_DATE": "2026-08-05T20:14:43+03:00"});
        let query = encode_params(&params);
        assert_eq!(query, "LAST_SYNC_DATE=2026-08-05T20%3A14%3A43%2B03%3A00");
        assert!(!query.contains('+'), "{query}");

        // И пробел — как %20, а не как «+»: иначе сам плюс стал бы непередаваем.
        assert_eq!(encode_params(&json!({"Q": "a b"})), "Q=a%20b");
    }

    #[test]
    fn nested_structures_become_bracketed_names() {
        let params = json!({
            "FILTER": {">ID": 10, "REAL_STATUS": "2"},
            "SELECT": ["ID", "TITLE"],
            "IS_COMPLETED": true,
            "PARENT": Value::Null,
        });
        let query = encode_params(&params);
        // Порядок ключей объекта у serde_json — лексикографический (BTreeMap),
        // поэтому строка стабильна и тест не флапает.
        assert_eq!(
            query,
            "FILTER[%3EID]=10&FILTER[REAL_STATUS]=2&IS_COMPLETED=1&PARENT=&SELECT[0]=ID&SELECT[1]=TITLE"
        );
    }

    #[test]
    fn a_command_without_params_has_no_question_mark() {
        assert_eq!(call("c").to_command(), "im.counters.get");
        assert_eq!(
            BatchCall::new("r", "im.recent.get", json!({"SKIP_OPENLINES": "Y"})).to_command(),
            "im.recent.get?SKIP_OPENLINES=Y"
        );
        // Не-объект трактуется как «параметров нет».
        assert_eq!(
            BatchCall::new("x", "profile", json!("nonsense")).to_command(),
            "profile"
        );
    }

    /// Нарезка ровно по 50: 51-я команда обязана уехать во ВТОРУЮ пачку, а не
    /// потеряться молча.
    #[test]
    fn chunks_split_at_exactly_fifty() {
        assert!(chunks(Vec::new()).is_empty(), "пустой список — ноль запросов");

        let calls: Vec<BatchCall> = (0..50).map(|i| call(&format!("k{i}"))).collect();
        let parts = chunks(calls);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].len(), 50);

        let calls: Vec<BatchCall> = (0..51).map(|i| call(&format!("k{i}"))).collect();
        let parts = chunks(calls);
        assert_eq!(parts.len(), 2, "51-я команда обязана уехать во вторую пачку");
        assert_eq!(parts[0].len(), 50);
        assert_eq!(parts[1].len(), 1);
        assert_eq!(parts[1][0].key, "k50");

        let calls: Vec<BatchCall> = (0..120).map(|i| call(&format!("k{i}"))).collect();
        let parts = chunks(calls);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2].len(), 20);
    }

    #[test]
    fn the_body_shape_is_what_the_portal_expects() {
        let b = body(&[
            BatchCall::new("recent", "im.recent.get", json!({"SKIP_OPENLINES": "Y"})),
            call("counters"),
        ]);
        assert_eq!(b["halt"], 0);
        assert_eq!(b["cmd"]["recent"], "im.recent.get?SKIP_OPENLINES=Y");
        assert_eq!(b["cmd"]["counters"], "im.counters.get");
    }

    /// Форма живого ответа (`batch1.json`): пустой `result_error` приезжает
    /// МАССИВОМ, а `result_total` — объектом.
    #[test]
    fn demux_survives_the_live_response_shape() {
        let result = json!({
            "result": {"chatget": {"id": 27}, "userget": [{"ID": 1}]},
            "result_error": [],
            "result_total": {"dialogusers": 2},
            "result_next": [],
            "result_time": {"chatget": {"operating": 0}}
        });
        let out = demux(&result);
        assert_eq!(out.ok.len(), 2);
        assert_eq!(out.get("chatget").unwrap()["id"], 27);
        assert!(out.errors.is_empty());
        assert_eq!(out.totals.get("dialogusers"), Some(&2));
        assert!(out.next.is_empty());
    }

    /// Отказ ОДНОЙ команды не делает пачку неуспешной и находится по своему
    /// ключу.
    #[test]
    fn demux_separates_errors_by_the_request_keys() {
        let result = json!({
            "result": {"ok1": true},
            "result_error": {
                "bad1": {"error": "ERROR_BATCH_LENGTH_EXCEEDED",
                         "error_description": "Max batch length exceeded"},
                "bad2": "PARAMS_ERROR"
            },
            "result_total": [],
            "result_next": {"page": "50"}
        });
        let out = demux(&result);
        assert_eq!(out.ok.len(), 1);
        assert_eq!(out.errors.len(), 2);
        assert_eq!(
            out.error("bad1").unwrap().code,
            "ERROR_BATCH_LENGTH_EXCEEDED"
        );
        assert_eq!(
            out.error("bad1").unwrap().description,
            "Max batch length exceeded"
        );
        // Голая строка вместо объекта — тоже законная форма.
        assert_eq!(out.error("bad2").unwrap().code, "PARAMS_ERROR");
        // `result_next` строкой — портал делает и так.
        assert_eq!(out.next.get("page"), Some(&50));
    }

    #[test]
    fn an_empty_or_broken_result_does_not_panic() {
        assert_eq!(demux(&json!({})), BatchOutcome::default());
        assert_eq!(demux(&json!(null)), BatchOutcome::default());
        assert_eq!(demux(&json!([])), BatchOutcome::default());
    }

    #[test]
    fn merge_keeps_results_of_every_chunk() {
        let mut a = demux(&json!({"result": {"k1": 1}}));
        a.merge(demux(&json!({"result": {"k2": 2}, "result_error": {"k3": "X"}})));
        assert_eq!(a.ok.len(), 2);
        assert_eq!(a.errors.len(), 1);
        assert_eq!(a.take("k2"), Some(json!(2)));
        assert_eq!(a.take("k2"), None, "take потребляет результат");
    }
}
