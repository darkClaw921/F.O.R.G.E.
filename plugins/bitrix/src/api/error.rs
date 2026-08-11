//! Ошибки транспорта Bitrix24, разбор конверта ответа и таблица кодов.
//!
//! Модуль отвечает на четыре вопроса и больше ни на что:
//!
//! 1. **Что вообще пришло от портала** — [`parse_envelope`], единственное
//!    место, где JSON превращается в `Result`.
//! 2. **Как показать это пользователю** — [`classify`] и [`to_api_error`],
//!    отображение кода портала в контракт [`crate::routes::error::ApiError`].
//! 3. **Повторять ли запрос** — [`retry_for`], политика ретраев.
//! 4. **Не утёк ли секрет** — [`B24Error::without_url`], обязательный к вызову
//!    на КАЖДОЙ транспортной ошибке.
//!
//! Все они — ЧИСТЫЕ функции без сети и без состояния, поэтому вся таблица §4
//! проверяется юнит-тестами, а не «когда-нибудь на живом портале».
//!
//! # Инвариант секрета: URL Bitrix24 содержит учётные данные
//!
//! У VK секрет едет в теле формы, и URL безопасен. У Bitrix24 всё наоборот:
//! вебхук — это ПУТЬ запроса `https://<портал>/rest/<uid>/<код>/<метод>.json`.
//! Значит, любой текст, цитирующий URL, цитирует и секрет — а `reqwest` делает
//! это по умолчанию в каждом `Display`:
//!
//! ```text
//! error sending request for url (https://p.bitrix24.ru/rest/1/wqdzbf…/profile.json)
//! ```
//!
//! Поэтому:
//! - [`transport_error`] — ЕДИНСТВЕННЫЙ путь превращения `reqwest::Error` в
//!   [`B24Error`], и он вызывает `reqwest::Error::without_url()`;
//! - [`B24Error::without_url`] дополнительно вычищает URL из ЛЮБОГО текста
//!   (`reqwest` прячет URL только у себя, но описание могло прийти и от
//!   портала, и от `serde`);
//! - в логи уходят только имя метода и код ошибки.
//!
//! # Три ловушки платформы, зашитые в разбор
//!
//! 1. **Ошибка приходит с любым HTTP-статусом.** `{"error":…}` живьём
//!    наблюдался и с 200, и с 403, и с 503 — ориентир только тело.
//! 2. **Пустое тело при HTTP 200 — это троттлинг.** Портал при превышении
//!    интенсивности не отвечает JSON-ошибкой, а обрывает ответ. Такой случай
//!    обязан стать retryable [`B24Error::Transport`], а НЕ [`B24Error::Decode`]:
//!    Decode означал бы «чините код», хотя чинить надо частоту.
//! 3. **`OPERATION_TIME_LIMIT` блокирует КОНКРЕТНЫЙ метод**, а не портал, и
//!    автоповтор его только продлевает ⇒ [`Retry::Never`] плюс пауза метода до
//!    `operating_reset_at` (см. [`crate::api::throttle`]).

use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::routes::error::ApiError;

// ---------------------------------------------------------------------------
// Коды портала, на которые завязана ЛОГИКА (а не только текст ошибки).
// ---------------------------------------------------------------------------

/// Неверный `user_id` или код вебхука; либо прав пользователя не хватает.
pub const ERR_INVALID_CREDENTIALS: &str = "INVALID_CREDENTIALS";

/// Авторизация не найдена: вебхук отозван.
pub const ERR_NO_AUTH_FOUND: &str = "NO_AUTH_FOUND";

/// Метод вне scope вебхука. Приходит СТРОЧНЫМИ буквами — это не опечатка.
pub const ERR_INSUFFICIENT_SCOPE: &str = "insufficient_scope";

/// Тип авторизации запрещён для метода (`event.bind`, `pull.*`): нужен OAuth.
pub const ERR_WRONG_AUTH_TYPE: &str = "WRONG_AUTH_TYPE";

/// Метода нет на этой редакции портала.
pub const ERR_METHOD_NOT_FOUND: &str = "ERROR_METHOD_NOT_FOUND";

/// Нет доступа к объекту («вы не можете отправлять сообщения этому получателю»).
pub const ERR_NO_ACCESS: &str = "ERROR_NO_ACCESS";

/// REST закрыт тарифом либо бот не в чате.
pub const ERR_ACCESS_DENIED: &str = "ACCESS_DENIED";

/// Сообщение уже нельзя редактировать.
pub const ERR_CANT_EDIT_MESSAGE: &str = "CANT_EDIT_MESSAGE";

/// Пустой `DIALOG_ID`.
pub const ERR_DIALOG_ID_EMPTY: &str = "DIALOG_ID_EMPTY";

/// Пустой текст сообщения.
pub const ERR_MESSAGE_EMPTY: &str = "MESSAGE_EMPTY";

/// Превышена интенсивность (leaky bucket портала). Живьём — HTTP 503.
pub const ERR_QUERY_LIMIT_EXCEEDED: &str = "QUERY_LIMIT_EXCEEDED";

/// Превышено `operating`-время КОНКРЕТНОГО метода. Живьём — HTTP 429.
pub const ERR_OPERATION_TIME_LIMIT: &str = "OPERATION_TIME_LIMIT";

/// Внутренняя ошибка портала.
pub const ERR_INTERNAL_SERVER_ERROR: &str = "INTERNAL_SERVER_ERROR";

/// Внутренняя ошибка портала (второй вид).
pub const ERR_UNEXPECTED_ANSWER: &str = "ERROR_UNEXPECTED_ANSWER";

/// Превышена длина `batch` (>50 команд). Приходит ПОКЛЮЧЕВО в `result_error`,
/// общей ошибки при этом нет — см. [`crate::api::batch`].
pub const ERR_BATCH_LENGTH_EXCEEDED: &str = "ERROR_BATCH_LENGTH_EXCEEDED";

/// Сколько ждать после `QUERY_LIMIT_EXCEEDED`.
///
/// Ведро портала пополняется 2 токенами в секунду, поэтому секунда — это ровно
/// один «тик» лимита, а не угаданная величина.
pub const RETRY_AFTER_QUERY_LIMIT: u64 = 1;

/// Запасное время паузы метода, если `operating_reset_at` неизвестен.
///
/// Окно `operating` у портала — десять минутных корзин; когда точный момент
/// разблокировки не пришёл ни в ошибке, ни в последнем успешном ответе,
/// честнее подождать минуту (одну корзину), чем звать метод сразу и продлевать
/// блокировку.
pub const RETRY_AFTER_OPERATING_FALLBACK: u64 = 60;

// ---------------------------------------------------------------------------
// Конверт ответа.
// ---------------------------------------------------------------------------

/// Блок `time` успешного ответа портала.
///
/// Присутствует в КАЖДОМ успешном ответе и является единственным источником
/// данных для второго лимита: `operating` — накопленное время метода в текущем
/// десятиминутном окне, `operating_reset_at` — unix-время, когда самая старая
/// минутная корзина выпадет из расчёта.
///
/// Заголовков `X-RateLimit-*` и `Retry-After` портал не присылает, поэтому
/// других источников нет.
///
/// `Serialize` НЕ выводится: сырьё портала не должно уметь уехать в HTTP-ответ
/// браузеру.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct TimeInfo {
    /// Unix-время начала обработки.
    #[serde(default)]
    pub start: f64,
    /// Unix-время конца обработки.
    #[serde(default)]
    pub finish: f64,
    /// Длительность запроса, секунды.
    #[serde(default)]
    pub duration: f64,
    /// Время постобработки, секунды.
    #[serde(default)]
    pub processing: f64,
    /// Накопленное `operating`-время метода в текущем окне, секунды.
    #[serde(default)]
    pub operating: f64,
    /// Unix-время сброса самой старой минутной корзины окна.
    #[serde(default)]
    pub operating_reset_at: i64,
}

/// Разобранный успешный ответ портала.
///
/// `result` отдаётся сырым `Value`: типизацией занимаются обёртки
/// [`crate::api::methods`], а транспорт обязан пропускать любой метод, включая
/// те, которых ещё нет в коде.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct B24Response {
    /// Значение ключа `result`.
    pub result: Value,
    /// Блок `time`, если он пришёл.
    pub time: Option<TimeInfo>,
    /// Значение корневого `next` (постраничные методы вроде `im.recent.list`).
    pub next: Option<i64>,
    /// Значение корневого `total`.
    ///
    /// У `im.recent.list` он живьём равен `-1` — поэтому знаковый тип, а не
    /// `u64`: строгий разбор ронял бы совершенно исправный ответ.
    pub total: Option<i64>,
}

// ---------------------------------------------------------------------------
// Тип ошибки.
// ---------------------------------------------------------------------------

/// Ошибка обращения к порталу.
///
/// Живёт ВНУТРИ плагина: наружу (в HTTP) она превращается [`to_api_error`] в
/// [`ApiError`]. Разделение нужно, потому что решения «повторить» и «показать»
/// принимаются на разных уровнях: транспорт знает про ретраи, REST-слой — про
/// контракт с фронтом.
///
/// `Display` и `Debug` безопасны для логов при одном условии: ошибка прошла
/// через [`B24Error::without_url`] (транспортный путь делает это сам, см.
/// [`transport_error`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum B24Error {
    /// До портала не доехали: DNS, connect, таймаут, обрыв TCP, ошибка TLS —
    /// **а также пустое тело при HTTP 200** (так выглядит троттлинг портала).
    Transport {
        /// Текст причины. URL из него вычищен.
        reason: String,
    },
    /// Статус ответа не 200, а тело не разобралось как JSON-конверт портала.
    ///
    /// Это прокси, captive portal или балансировщик — то есть тот же класс,
    /// что и [`B24Error::Transport`].
    Http {
        /// Полученный статус.
        status: u16,
    },
    /// Тело ответа не разобралось: не JSON либо JSON без `result` и без
    /// `error`.
    Decode {
        /// Что именно не сошлось (без самого тела: в нём данные пользователя).
        reason: String,
    },
    /// Портал отказал по существу.
    Api {
        /// Значение `error` — СТАБИЛЬНЫЙ машинный код.
        code: String,
        /// Значение `error_description` — только для логов и текста ошибки,
        /// формулировка может меняться между релизами портала.
        description: String,
    },
    /// Превышена интенсивность (`QUERY_LIMIT_EXCEEDED`).
    ///
    /// Отдельный вариант, а не [`B24Error::Api`]: это единственный отказ
    /// портала, который лечится ПОВТОРОМ, и путать его с прикладными ошибками
    /// нельзя.
    Throttled {
        /// Через сколько секунд имеет смысл повторить.
        retry_after_secs: u64,
    },
    /// Превышено `operating`-время метода (`OPERATION_TIME_LIMIT`).
    ///
    /// Тоже отдельный вариант: блокируется КОНКРЕТНЫЙ метод, а повтор её
    /// продлевает. Транспорт обязан не ретраить и поставить метод на паузу.
    OperatingLimit {
        /// Момент разблокировки из ответа портала, если он пришёл.
        ///
        /// `None` ⇒ вызывающий подставит `operating_reset_at` из последнего
        /// успешного ответа этого метода (он точнее любой оценки), а если и
        /// его нет — [`RETRY_AFTER_OPERATING_FALLBACK`].
        reset_at: Option<i64>,
    },
}

impl fmt::Display for B24Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { reason } => write!(f, "bitrix transport failure: {reason}"),
            Self::Http { status } => write!(f, "bitrix replied with HTTP {status}"),
            Self::Decode { reason } => write!(f, "bitrix response is not parseable: {reason}"),
            Self::Api { code, description } if description.is_empty() => {
                write!(f, "bitrix error {code}")
            }
            Self::Api { code, description } => write!(f, "bitrix error {code}: {description}"),
            Self::Throttled { retry_after_secs } => write!(
                f,
                "bitrix intensity limit ({ERR_QUERY_LIMIT_EXCEEDED}), retry in {retry_after_secs}s"
            ),
            Self::OperatingLimit { .. } => {
                write!(f, "bitrix operating time limit ({ERR_OPERATION_TIME_LIMIT})")
            }
        }
    }
}

impl std::error::Error for B24Error {}

impl B24Error {
    /// Машинный код ошибки — то, что можно писать в лог.
    ///
    /// Для [`B24Error::Throttled`] и [`B24Error::OperatingLimit`] возвращается
    /// исходный код портала: они выделены в отдельные варианты ради политики
    /// ретраев, но в логах и метриках обязаны выглядеть так же, как их видит
    /// администратор портала.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => Some(code.as_str()),
            Self::Throttled { .. } => Some(ERR_QUERY_LIMIT_EXCEEDED),
            Self::OperatingLimit { .. } => Some(ERR_OPERATION_TIME_LIMIT),
            _ => None,
        }
    }

    /// Транспортный ли это отказ.
    ///
    /// По нему [`crate::api::B24Client`] решает, трогать ли
    /// [`crate::state::ConnHealth`]: «сеть лежит» — это про
    /// [`B24Error::Transport`] и [`B24Error::Http`], а не про отказ портала по
    /// существу (тот приходит по живой сети).
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Http { .. })
    }

    /// Вырезает URL из текста ошибки.
    ///
    /// ОБЯЗАТЕЛЕН на каждой транспортной ошибке: у Bitrix24 весь секрет лежит в
    /// пути запроса, поэтому процитированный URL — это утёкший вебхук.
    /// Вычищаются две формы:
    ///
    /// 1. `http(s)://…` до ближайшего пробела или закрывающей скобки/кавычки;
    /// 2. `/rest/<uid>/<код>` без схемы — на случай, если URL уже был разрезан
    ///    чужим кодом и от него остался только путь.
    ///
    /// Функция потребляет и возвращает ошибку, чтобы вызов нельзя было забыть
    /// «на половине»: `let e = e.without_url();` — единственная форма.
    #[must_use]
    pub fn without_url(self) -> Self {
        match self {
            Self::Transport { reason } => Self::Transport {
                reason: scrub_urls(&reason),
            },
            Self::Decode { reason } => Self::Decode {
                reason: scrub_urls(&reason),
            },
            Self::Api { code, description } => Self::Api {
                code,
                description: scrub_urls(&description),
            },
            other => other,
        }
    }
}

/// Превращает `reqwest::Error` в [`B24Error::Transport`].
///
/// ЕДИНСТВЕННЫЙ путь этого превращения во всём крейте. Двухступенчатая
/// защита:
///
/// 1. `reqwest::Error::without_url()` — штатный способ убрать URL из `Display`
///    самого `reqwest`;
/// 2. [`B24Error::without_url`] поверх — на случай, если URL всё же оказался в
///    тексте вложенной ошибки (`hyper`, resolver) или в собственном описании.
///
/// Таймаут (`e.is_timeout()`) сознательно НЕ выделяется в отдельный вариант:
/// для политики повторов он неотличим от обрыва, а для пользователя — от
/// «портал недоступен».
pub fn transport_error(e: reqwest::Error) -> B24Error {
    B24Error::Transport {
        reason: e.without_url().to_string(),
    }
    .without_url()
}

/// Вычищает URL и путь вебхука из произвольного текста.
///
/// Отдельная функция, потому что её же зовёт [`crate::api::B24Client`] для
/// текстов, приходящих не из `reqwest`.
pub fn scrub_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &text[i..];
        let scheme = if rest.starts_with("https://") {
            Some(8)
        } else if rest.starts_with("http://") {
            Some(7)
        } else {
            None
        };
        if let Some(skip) = scheme {
            // URL кончается там, где начинается разделитель текста. Скобки и
            // кавычки включены, потому что reqwest цитирует URL в скобках.
            let end = rest[skip..]
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\'' | '>' | ','))
                .map_or(rest.len(), |p| skip + p);
            out.push_str("<url>");
            i += end;
            continue;
        }
        if let Some(consumed) = rest_path_len(rest) {
            out.push_str("<url>");
            i += consumed;
            continue;
        }
        let ch = rest.chars().next().unwrap_or('\u{0}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Длина префикса `/rest/<uid>/<код>` в начале строки, если он там есть.
///
/// Нужен ради второй формы утечки: URL без схемы. Числовой `uid` и
/// алфавитно-цифровой код — ровно та форма, что описана регекспом §4.
fn rest_path_len(s: &str) -> Option<usize> {
    let tail = s.strip_prefix("/rest/")?;
    let mut it = tail.splitn(3, '/');
    let uid = it.next()?;
    let code = it.next()?;
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code_len = code
        .bytes()
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    if code_len == 0 {
        return None;
    }
    Some("/rest/".len() + uid.len() + 1 + code_len)
}

// ---------------------------------------------------------------------------
// Разбор конверта.
// ---------------------------------------------------------------------------

/// ЕДИНСТВЕННОЕ место, где JSON от портала превращается в `Result`.
///
/// Правила В ПОРЯДКЕ ПРОВЕРКИ:
///
/// 1. **пустое тело при HTTP 200** ⇒ [`B24Error::Transport`] — это троттлинг,
///    а не битый ответ (см. шапку модуля);
/// 2. пустое тело при любом другом статусе ⇒ [`B24Error::Http`];
/// 3. тело не JSON: статус 200 ⇒ [`B24Error::Decode`], иначе
///    [`B24Error::Http`] (значит, отвечал не портал, а посредник);
/// 4. есть ключ `error` ⇒ отказ портала — **независимо от статуса**
///    (`QUERY_LIMIT_EXCEEDED` живьём приезжает с 503, `OPERATION_TIME_LIMIT` с
///    429, прочее — с 200 и 403);
/// 5. есть ключ `result` ⇒ успех;
/// 6. иначе ⇒ [`B24Error::Decode`].
///
/// Порядок 4 перед 5 важен: `batch` возвращает `result` даже тогда, когда
/// часть команд провалилась, а общий `error` означает отказ всего вызова.
///
/// Функция чистая: ни логов, ни состояния, ни сети.
pub fn parse_envelope(status: u16, body: &[u8]) -> Result<B24Response, B24Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        if status == 200 {
            // ↓↓↓ ЛОВУШКА ПЛАТФОРМЫ: пустое тело при 200 — это троттлинг ↓↓↓
            return Err(B24Error::Transport {
                reason: "portal returned an empty body with HTTP 200 (intensity throttling)"
                    .to_string(),
            });
        }
        return Err(B24Error::Http { status });
    }

    let root: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            if status != 200 {
                // Не портал: у портала любой ответ — JSON.
                return Err(B24Error::Http { status });
            }
            // В `reason` едет только описание ошибки serde (позиция и вид), но
            // НЕ само тело: в нём могут быть данные пользователя.
            return Err(B24Error::Decode {
                reason: e.to_string(),
            });
        }
    };

    let time = root
        .get("time")
        .and_then(|v| serde_json::from_value::<TimeInfo>(v.clone()).ok());

    if let Some(err) = root.get("error") {
        let code = err.as_str().unwrap_or_default().to_string();
        let description = root
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        return Err(api_error(code, description, time));
    }

    let Some(result) = root.get("result") else {
        return Err(B24Error::Decode {
            reason: "neither `result` nor `error` is present".to_string(),
        });
    };

    Ok(B24Response {
        result: result.clone(),
        time,
        next: root.get("next").and_then(Value::as_i64),
        total: root.get("total").and_then(Value::as_i64),
    })
}

/// Собирает вариант ошибки по коду портала.
///
/// Два кода получают собственные варианты, потому что от них зависит ПОВЕДЕНИЕ
/// транспорта, а не только текст: интенсивность лечится повтором, а
/// `operating`-лимит — паузой конкретного метода.
fn api_error(code: String, description: String, time: Option<TimeInfo>) -> B24Error {
    match code.as_str() {
        ERR_QUERY_LIMIT_EXCEEDED => B24Error::Throttled {
            retry_after_secs: RETRY_AFTER_QUERY_LIMIT,
        },
        ERR_OPERATION_TIME_LIMIT => B24Error::OperatingLimit {
            // Портал ИНОГДА кладёт `time` даже в тело ошибки — тогда момент
            // разблокировки точный. Если нет, вызывающий возьмёт его из
            // последнего успешного ответа этого метода.
            reset_at: time.map(|t| t.operating_reset_at).filter(|v| *v > 0),
        },
        _ => B24Error::Api { code, description },
    }
    .without_url()
}

// ---------------------------------------------------------------------------
// Таблица кодов.
// ---------------------------------------------------------------------------

/// Код портала → ошибка REST-контракта (§4 плана).
///
/// ЧИСТАЯ функция от одной строки: сообщение здесь каноническое (текст портала
/// подмешивает [`to_api_error`]).
///
/// | `error` | HTTP | `code` | ретрай |
/// |---|---|---|---|
/// | `INVALID_CREDENTIALS`, `NO_AUTH_FOUND` | 409 | `not_authorized` | нет |
/// | `insufficient_scope` | 409 | `scope_missing` | нет |
/// | `WRONG_AUTH_TYPE` | 409 | `unsupported_auth` | нет |
/// | `ERROR_METHOD_NOT_FOUND` | 501 | `method_unavailable` | нет |
/// | `ERROR_NO_ACCESS`, `ACCESS_DENIED` | 409 | `send_forbidden` | нет |
/// | `DIALOG_ID_EMPTY`, `MESSAGE_EMPTY`, `PARAMS_ERROR`, `*_ERROR` | 400 | `invalid_input` | нет |
/// | `CANT_EDIT_MESSAGE` | 409 | `invalid_state` | нет |
/// | `QUERY_LIMIT_EXCEEDED` | 429 | `flood_wait` (1 c) | ДА |
/// | `OPERATION_TIME_LIMIT` | 429 | `flood_wait` (до `operating_reset_at`) | нет |
/// | `INTERNAL_SERVER_ERROR`, `ERROR_UNEXPECTED_ANSWER` | 502 | `bitrix_error` | ДА |
/// | пустое тело при 200 / обрыв TCP | 503 | `not_connected` | ДА |
/// | прочее | 502 | `bitrix_error` | нет |
///
/// # Порядок ветвей значим
///
/// Правило «`*_ERROR` ⇒ `invalid_input`» стоит ПОСЛЕ явных кодов, потому что
/// `INTERNAL_SERVER_ERROR` тоже оканчивается на `_ERROR`, но означает
/// противоположное: не «поправьте ввод», а «портал сломался, повторите».
pub fn classify(code: &str) -> ApiError {
    match code {
        ERR_INVALID_CREDENTIALS | ERR_NO_AUTH_FOUND => ApiError::not_authorized(
            "Bitrix24 rejected the webhook: it is invalid, revoked, or belongs to another user",
        ),
        ERR_INSUFFICIENT_SCOPE => ApiError::scope_missing(
            "the webhook has no permission for this method: re-create it with the `im` and `task` scopes",
        ),
        ERR_WRONG_AUTH_TYPE => ApiError::unsupported_auth(
            "this Bitrix24 method is not available to inbound webhooks at all (it requires an OAuth application)",
        ),
        ERR_METHOD_NOT_FOUND => {
            ApiError::method_unavailable("this Bitrix24 portal has no such method")
        }
        ERR_NO_ACCESS | ERR_ACCESS_DENIED => {
            ApiError::send_forbidden("Bitrix24 denied access to this object")
        }
        ERR_CANT_EDIT_MESSAGE => {
            ApiError::invalid_state("this Bitrix24 message can no longer be edited")
        }
        ERR_QUERY_LIMIT_EXCEEDED => ApiError::flood_wait(
            RETRY_AFTER_QUERY_LIMIT,
            "Bitrix24 intensity limit reached: too many requests per second",
        ),
        // Точное время подставляет `to_api_error` из `operating_reset_at`:
        // из одного кода его не вывести.
        ERR_OPERATION_TIME_LIMIT => ApiError::flood_wait(
            RETRY_AFTER_OPERATING_FALLBACK,
            "Bitrix24 operating-time limit reached for this method: it is paused until the portal resets its window",
        ),
        ERR_INTERNAL_SERVER_ERROR | ERR_UNEXPECTED_ANSWER => {
            ApiError::bitrix_error("Bitrix24 internal error")
        }
        ERR_DIALOG_ID_EMPTY => ApiError::bad_input("Bitrix24: the dialog id is empty"),
        ERR_MESSAGE_EMPTY => ApiError::bad_input("Bitrix24: the message is empty"),
        // `*_ERROR` — семейство «портал не принял параметры»: `PARAMS_ERROR`,
        // `MESSAGE_ERROR`, `DIALOG_ERROR` и прочие. Ветка стоит последней.
        c if c.to_ascii_uppercase().ends_with("_ERROR") => {
            ApiError::bad_input("Bitrix24 rejected the request parameters")
        }
        _ => ApiError::bitrix_error("Bitrix24 returned an error"),
    }
}

/// Полное отображение [`B24Error`] в контракт REST.
///
/// Здесь же подставляется то, чего нет в [`classify`]: точное время паузы
/// метода из `operating_reset_at` и дословный текст портала.
///
/// `now` передаётся параметром (unix-время), потому что `retry_after_secs` —
/// это ОТНОСИТЕЛЬНАЯ величина, а `operating_reset_at` — абсолютная: без
/// внешних часов эту арифметику нельзя было бы проверить тестом, не дожидаясь
/// реального времени.
pub fn to_api_error(err: &B24Error, now: i64) -> ApiError {
    match err {
        B24Error::Transport { reason } => {
            ApiError::not_connected(format!("Bitrix24 is unreachable: {reason}"))
        }
        B24Error::Http { status } => {
            ApiError::not_connected(format!("Bitrix24 replied with HTTP {status}"))
        }
        B24Error::Decode { reason } => {
            ApiError::bitrix_error(format!("Bitrix24 response is not parseable: {reason}"))
        }
        B24Error::Throttled { retry_after_secs } => ApiError::flood_wait(
            *retry_after_secs,
            "Bitrix24 intensity limit reached: too many requests per second",
        ),
        B24Error::OperatingLimit { reset_at } => {
            let secs = reset_at
                .map(|at| (at - now).clamp(1, 600) as u64)
                .unwrap_or(RETRY_AFTER_OPERATING_FALLBACK);
            ApiError::flood_wait(
                secs,
                "Bitrix24 operating-time limit reached for this method: it is paused until the portal resets its window",
            )
        }
        B24Error::Api { code, description } => {
            let mut api = classify(code);
            if !description.is_empty() {
                api.body.message = format!("{}: {}", api.body.message, scrub_urls(description));
            }
            api
        }
    }
}

// ---------------------------------------------------------------------------
// Политика ретраев.
// ---------------------------------------------------------------------------

/// Решение о повторе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Повтор запрещён.
    Never,
    /// Повторить один раз после паузы.
    AfterBackoff,
}

/// Повторять ли запрос — строго по таблице [`classify`].
///
/// Повтор ВСЕГДА не более одного (это свойство [`crate::api::B24Client`]), а
/// методы отправки не повторяются вовсе — но это уже решение уровня метода
/// (Р9), а не уровня ошибки, поэтому оно живёт в клиенте.
///
/// # Что НЕ ретраится и почему
///
/// - `OPERATION_TIME_LIMIT` — повтор продлевает блокировку метода: портал
///   считает время, потраченное в том числе на отказавшие вызовы;
/// - любой прикладной отказ (`INVALID_CREDENTIALS`, `insufficient_scope`,
///   `*_ERROR`) — повтор ничего не изменит, а задержку удвоит;
/// - HTTP 4xx без JSON — это посредник, отвечающий отказом осознанно.
pub fn retry_for(err: &B24Error) -> Retry {
    match err {
        // Сюда же попадает пустое тело при 200 — главная ветка ретрая.
        B24Error::Transport { .. } => Retry::AfterBackoff,
        B24Error::Http { status } if *status >= 500 => Retry::AfterBackoff,
        B24Error::Http { .. } => Retry::Never,
        B24Error::Decode { .. } => Retry::Never,
        B24Error::Throttled { .. } => Retry::AfterBackoff,
        B24Error::OperatingLimit { .. } => Retry::Never,
        B24Error::Api { code, .. } => match code.as_str() {
            ERR_INTERNAL_SERVER_ERROR | ERR_UNEXPECTED_ANSWER => Retry::AfterBackoff,
            _ => Retry::Never,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(v: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    // -----------------------------------------------------------------
    // Конверт.
    // -----------------------------------------------------------------

    #[test]
    fn success_envelope_carries_result_and_time() {
        let raw = body(serde_json::json!({
            "result": {"ID": "1"},
            "time": {
                "start": 1785950098.0, "finish": 1785950099.0, "duration": 1.06,
                "processing": 1, "operating_reset_at": 1785950698, "operating": 0.38
            }
        }));
        let resp = parse_envelope(200, &raw).unwrap();
        assert_eq!(resp.result["ID"], "1");
        let time = resp.time.unwrap();
        assert_eq!(time.operating_reset_at, 1785950698);
        assert!((time.operating - 0.38).abs() < 1e-9);
    }

    /// `im.recent.list` кладёт `next`/`total` рядом с `result`, причём
    /// `total` живьём равен `-1`.
    #[test]
    fn paging_fields_survive_a_negative_total() {
        let raw = body(serde_json::json!({"result": {"items": []}, "next": 4, "total": -1}));
        let resp = parse_envelope(200, &raw).unwrap();
        assert_eq!(resp.next, Some(4));
        assert_eq!(resp.total, Some(-1));
    }

    /// ГЛАВНАЯ ЛОВУШКА ФАЗЫ: пустое тело при 200 — троттлинг, а не Decode.
    #[test]
    fn empty_body_on_200_is_a_retryable_transport_error() {
        for raw in [b"".as_slice(), b"   ".as_slice(), b"\n".as_slice()] {
            let err = parse_envelope(200, raw).unwrap_err();
            assert!(
                matches!(err, B24Error::Transport { .. }),
                "пустое тело при 200 обязано быть Transport, получено {err:?}"
            );
            assert_eq!(retry_for(&err), Retry::AfterBackoff);
            assert_eq!(to_api_error(&err, 0).status.as_u16(), 503);
        }
    }

    /// Пустое тело при НЕ-200 — это уже посредник, а не троттлинг портала.
    #[test]
    fn empty_body_on_non_200_is_an_http_error() {
        assert_eq!(
            parse_envelope(502, b"").unwrap_err(),
            B24Error::Http { status: 502 }
        );
    }

    #[test]
    fn garbage_body_is_decode_on_200_and_http_otherwise() {
        assert!(matches!(
            parse_envelope(200, b"<html>captive portal</html>").unwrap_err(),
            B24Error::Decode { .. }
        ));
        assert_eq!(
            parse_envelope(504, b"<html>gateway timeout</html>").unwrap_err(),
            B24Error::Http { status: 504 }
        );
    }

    #[test]
    fn json_without_result_and_error_is_decode() {
        let raw = body(serde_json::json!({"time": {"operating": 0}}));
        assert!(matches!(
            parse_envelope(200, &raw).unwrap_err(),
            B24Error::Decode { .. }
        ));
    }

    /// Ошибка портала опознаётся по ТЕЛУ, а не по статусу: живьём один и тот
    /// же конверт приезжает с 200, 403 и 503.
    #[test]
    fn errors_are_recognised_by_the_body_at_any_status() {
        let raw = body(serde_json::json!({
            "error": ERR_INVALID_CREDENTIALS,
            "error_description": "Invalid request credentials"
        }));
        for status in [200, 401, 403, 500] {
            match parse_envelope(status, &raw).unwrap_err() {
                B24Error::Api { code, description } => {
                    assert_eq!(code, ERR_INVALID_CREDENTIALS);
                    assert_eq!(description, "Invalid request credentials");
                }
                other => panic!("статус {status}: ожидался Api, пришло {other:?}"),
            }
        }
    }

    /// `result` рядом с `error` не должен проехать как успех.
    #[test]
    fn an_error_wins_over_a_partial_result() {
        let raw = body(serde_json::json!({"result": {"a": 1}, "error": "PARAMS_ERROR"}));
        assert!(matches!(
            parse_envelope(200, &raw).unwrap_err(),
            B24Error::Api { .. }
        ));
    }

    #[test]
    fn query_limit_and_operating_limit_get_their_own_variants() {
        let raw = body(serde_json::json!({"error": ERR_QUERY_LIMIT_EXCEEDED}));
        assert_eq!(
            parse_envelope(503, &raw).unwrap_err(),
            B24Error::Throttled {
                retry_after_secs: RETRY_AFTER_QUERY_LIMIT
            }
        );

        let raw = body(serde_json::json!({"error": ERR_OPERATION_TIME_LIMIT}));
        assert_eq!(
            parse_envelope(429, &raw).unwrap_err(),
            B24Error::OperatingLimit { reset_at: None }
        );

        // …а если портал приложил `time`, момент разблокировки точный.
        let raw = body(serde_json::json!({
            "error": ERR_OPERATION_TIME_LIMIT,
            "time": {"operating_reset_at": 1785951383, "operating": 481.0}
        }));
        assert_eq!(
            parse_envelope(429, &raw).unwrap_err(),
            B24Error::OperatingLimit {
                reset_at: Some(1785951383)
            }
        );
    }

    // -----------------------------------------------------------------
    // Таблица §4 — построчно.
    // -----------------------------------------------------------------

    #[test]
    fn the_error_table_matches_the_plan() {
        let table: [(&str, u16, &str, Retry); 16] = [
            (ERR_INVALID_CREDENTIALS, 409, "not_authorized", Retry::Never),
            (ERR_NO_AUTH_FOUND, 409, "not_authorized", Retry::Never),
            (ERR_INSUFFICIENT_SCOPE, 409, "scope_missing", Retry::Never),
            (ERR_WRONG_AUTH_TYPE, 409, "unsupported_auth", Retry::Never),
            (
                ERR_METHOD_NOT_FOUND,
                501,
                "method_unavailable",
                Retry::Never,
            ),
            (ERR_NO_ACCESS, 409, "send_forbidden", Retry::Never),
            (ERR_ACCESS_DENIED, 409, "send_forbidden", Retry::Never),
            (ERR_DIALOG_ID_EMPTY, 400, "invalid_input", Retry::Never),
            (ERR_MESSAGE_EMPTY, 400, "invalid_input", Retry::Never),
            ("PARAMS_ERROR", 400, "invalid_input", Retry::Never),
            ("MESSAGE_ERROR", 400, "invalid_input", Retry::Never),
            (ERR_CANT_EDIT_MESSAGE, 409, "invalid_state", Retry::Never),
            (
                ERR_INTERNAL_SERVER_ERROR,
                502,
                "bitrix_error",
                Retry::AfterBackoff,
            ),
            (
                ERR_UNEXPECTED_ANSWER,
                502,
                "bitrix_error",
                Retry::AfterBackoff,
            ),
            (ERR_BATCH_LENGTH_EXCEEDED, 502, "bitrix_error", Retry::Never),
            ("SOMETHING_NEW_FROM_2030", 502, "bitrix_error", Retry::Never),
        ];

        for (code, status, api_code, retry) in table {
            let api = classify(code);
            assert_eq!(api.status.as_u16(), status, "код {code}");
            assert_eq!(api.body.code, api_code, "код {code}");

            let err = B24Error::Api {
                code: code.to_string(),
                description: String::new(),
            };
            assert_eq!(retry_for(&err), retry, "ретрай для {code}");
            let mapped = to_api_error(&err, 0);
            assert_eq!(mapped.status.as_u16(), status, "to_api_error для {code}");
            assert_eq!(mapped.body.code, api_code, "to_api_error для {code}");
        }
    }

    /// `INTERNAL_SERVER_ERROR` оканчивается на `_ERROR`, но означает
    /// «повторите», а не «поправьте ввод»: порядок ветвей [`classify`].
    #[test]
    fn the_suffix_rule_does_not_swallow_the_explicit_codes() {
        assert_eq!(classify(ERR_INTERNAL_SERVER_ERROR).status.as_u16(), 502);
        assert_eq!(classify("PARAMS_ERROR").status.as_u16(), 400);
        assert_eq!(classify("params_error").status.as_u16(), 400);
    }

    #[test]
    fn both_limits_become_flood_wait_but_with_different_waits() {
        let api = to_api_error(
            &B24Error::Throttled {
                retry_after_secs: RETRY_AFTER_QUERY_LIMIT,
            },
            0,
        );
        assert_eq!(api.status.as_u16(), 429);
        assert_eq!(api.body.code, "flood_wait");
        assert_eq!(api.body.retry_after_secs, Some(1));

        // Абсолютный `operating_reset_at` превращается в относительную паузу.
        let api = to_api_error(
            &B24Error::OperatingLimit {
                reset_at: Some(1_000_240),
            },
            1_000_000,
        );
        assert_eq!(api.body.retry_after_secs, Some(240));

        // Просроченный момент разблокировки не даёт ни нуля, ни отрицательного
        // ожидания: секунда — минимум, который UI умеет показать.
        let api = to_api_error(
            &B24Error::OperatingLimit {
                reset_at: Some(1_000_000),
            },
            1_000_500,
        );
        assert_eq!(api.body.retry_after_secs, Some(1));

        // Момент неизвестен — ждём одну минутную корзину окна.
        let api = to_api_error(&B24Error::OperatingLimit { reset_at: None }, 0);
        assert_eq!(
            api.body.retry_after_secs,
            Some(RETRY_AFTER_OPERATING_FALLBACK)
        );
    }

    #[test]
    fn transport_and_http_map_to_not_connected() {
        let err = B24Error::Transport {
            reason: "connection closed".into(),
        };
        assert_eq!(to_api_error(&err, 0).status.as_u16(), 503);
        assert_eq!(retry_for(&err), Retry::AfterBackoff);

        assert_eq!(
            to_api_error(&B24Error::Http { status: 502 }, 0)
                .status
                .as_u16(),
            503
        );
        assert_eq!(retry_for(&B24Error::Http { status: 503 }), Retry::AfterBackoff);
        // 4xx от посредника — осознанный отказ, повторять нечего.
        assert_eq!(retry_for(&B24Error::Http { status: 403 }), Retry::Never);
    }

    #[test]
    fn the_error_code_is_loggable_for_every_variant() {
        assert_eq!(
            B24Error::Throttled {
                retry_after_secs: 1
            }
            .code(),
            Some(ERR_QUERY_LIMIT_EXCEEDED)
        );
        assert_eq!(
            B24Error::OperatingLimit { reset_at: None }.code(),
            Some(ERR_OPERATION_TIME_LIMIT)
        );
        assert_eq!(
            B24Error::Api {
                code: "PARAMS_ERROR".into(),
                description: String::new()
            }
            .code(),
            Some("PARAMS_ERROR")
        );
        assert_eq!(
            B24Error::Transport {
                reason: "x".into()
            }
            .code(),
            None
        );
    }

    // -----------------------------------------------------------------
    // Инвариант секрета.
    // -----------------------------------------------------------------

    #[test]
    fn scrub_removes_the_whole_url_including_the_webhook_code() {
        let text = "error sending request for url (https://test.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/profile.json): connection refused";
        let clean = scrub_urls(text);
        assert!(!clean.contains("wqdzbfhmn83d23j4"), "{clean}");
        assert!(!clean.contains("test.bitrix24.ru"), "{clean}");
        assert!(clean.contains("<url>"), "{clean}");
        assert!(clean.contains("connection refused"), "{clean}");

        // Форма без схемы (URL уже разрезали до нас).
        let clean = scrub_urls("failed at /rest/1/wqdzbfhmn83d23j4/im.recent.get.json");
        assert!(!clean.contains("wqdzbfhmn83d23j4"), "{clean}");
        assert!(clean.contains("<url>"), "{clean}");

        // Обычный текст не портится, в том числе с не-ASCII.
        assert_eq!(scrub_urls("портал недоступен"), "портал недоступен");
        assert_eq!(scrub_urls(""), "");
    }

    #[test]
    fn without_url_cleans_every_textual_variant() {
        let dirty = "https://p.bitrix24.ru/rest/1/SECRETCODE01/profile.json";
        for err in [
            B24Error::Transport {
                reason: dirty.into(),
            },
            B24Error::Decode {
                reason: dirty.into(),
            },
            B24Error::Api {
                code: "PARAMS_ERROR".into(),
                description: dirty.into(),
            },
        ] {
            let clean = err.without_url().to_string();
            assert!(!clean.contains("SECRETCODE01"), "{clean}");
            assert!(!clean.contains("p.bitrix24.ru"), "{clean}");
        }
    }

    /// Живая `reqwest::Error` цитирует URL целиком — значит, и код вебхука.
    /// [`transport_error`] обязан вычистить и хост, и код.
    #[tokio::test]
    async fn transport_error_strips_the_url_from_a_real_reqwest_error() {
        let http = reqwest::Client::builder().build().unwrap();
        // Порт 1 закрыт всегда: connect падает мгновенно, сети не требуется.
        let e = http
            .post("http://127.0.0.1:1/rest/1/wqdzbfhmn83d23j4/profile.json")
            .send()
            .await
            .expect_err("порт 1 обязан отказать");
        // Именно эта строка и есть утечка, ради которой всё написано.
        assert!(
            e.to_string().contains("wqdzbfhmn83d23j4"),
            "reqwest перестал цитировать URL — проверка потеряла смысл: {e}"
        );

        let err = transport_error(e);
        let text = err.to_string();
        assert!(err.is_transport(), "{err:?}");
        assert!(!text.contains("wqdzbfhmn83d23j4"), "код вебхука в тексте: {text}");
        assert!(!text.contains("127.0.0.1"), "хост в тексте: {text}");
        assert_eq!(retry_for(&err), Retry::AfterBackoff);
    }

    /// Текст портала подмешивается в сообщение, но тоже проходит очистку:
    /// `error_description` теоретически может процитировать наш же URL.
    #[test]
    fn the_portal_text_is_appended_but_scrubbed() {
        let err = B24Error::Api {
            code: "PARAMS_ERROR".into(),
            description: "bad call to https://p.bitrix24.ru/rest/1/SECRETCODE01/im.message.add.json"
                .into(),
        };
        let api = to_api_error(&err, 0);
        assert_eq!(api.status.as_u16(), 400);
        assert!(!api.body.message.contains("SECRETCODE01"), "{}", api.body.message);
        assert!(api.body.message.contains("bad call to"), "{}", api.body.message);
    }
}
