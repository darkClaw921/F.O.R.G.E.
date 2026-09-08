//! Экстрактор [`Portal`] — «какому порталу адресован этот запрос».
//!
//! # Зачем он существует
//!
//! До мультипортальности состояние приезжало в хендлер через
//! `State<Arc<B24State>>`, то есть выбиралось В МОМЕНТ СБОРКИ роутера. С
//! реестром так нельзя: [`B24Registry::add`] и [`B24Registry::remove`] меняют
//! состав порталов на ЖИВОМ процессе, а роутер собран один раз при старте —
//! запрос продолжал бы уходить в тот `Arc`, который был первым на момент
//! регистрации, даже после того как этот портал отключили.
//!
//! Поэтому состоянием роутера стал сам реестр (`Router<Arc<B24Registry>>`), а
//! портал выбирает КАЖДЫЙ запрос — через этот экстрактор.
//!
//! # Порядок резолва
//!
//! 1. Заголовок [`PORTAL_HEADER`] (`X-Forge-Portal`) — основной канал: его
//!    ставит `js/bitrix/api.js` на каждый вызов, и он переживает любой путь,
//!    включая `<img src>`-подобные адреса медиа только потому, что там его
//!    дублирует пункт 2.
//! 2. Query-параметр `?portal=` — фолбэк для адресов, которые браузер
//!    открывает без заголовков: прямые ссылки на медиа (браузер не умеет
//!    ставить заголовки на `<img src>`).
//!
//!    `/ws/bitrix` в этом списке БОЛЬШЕ НЕТ: с Ф5 сокет один на все порталы и
//!    экстрактором не пользуется вовсе — он несёт кадры каждого портала с
//!    полем `portal_id`. Параметр там по-прежнему принимается и игнорируется
//!    (см. [`crate::ws::B24WsQuery`]).
//! 3. Ни того, ни другого — ПЕРВЫЙ слот реестра.
//!
//! Заголовок приоритетнее query: query виден в логах прокси и в истории
//! браузера, поэтому фронт ставит его лишь там, где заголовок невозможен, — а
//! случайно оставшийся в URL `?portal=` не должен перебивать явное намерение
//! запроса.
//!
//! # Три решения, которые легко случайно отменить
//!
//! 1. **Фолбэк отдаёт ПЕРВЫЙ СЛОТ, а не первый ПОДКЛЮЧЁННЫЙ портал.** На
//!    свежей установке единственный слот пуст (см. шапку [`crate::registry`]),
//!    и именно он показывает экран подключения. Отфильтруй фолбэк по наличию
//!    вебхука — и `GET /api/bitrix/status` на свежей установке начал бы
//!    отвечать 503 вместо `not_configured`, то есть панель перестала бы
//!    подключаться вовсе.
//! 2. **Адресный запрос по неизвестному `id` — 404, а не фолбэк.** Молчаливое
//!    сползание на первый портал означало бы, что закрытая в соседнем окне
//!    вкладка продолжает «работать», показывая ЧУЖУЮ переписку под своим
//!    именем.
//! 3. **Экстрактор не отдаёт 400 никогда.** Лестница проверок data-роутов
//!    начинается с 409 (`не подключён`), и мусорный ввод обязан доезжать до
//!    неё, а не отбиваться раньше: иначе неподключённый клиент получил бы
//!    оракул валидности идентификаторов. Пустой (`X-Forge-Portal: `) и
//!    неразбираемый заголовок трактуются по-разному и намеренно: пустой — это
//!    «портал не назван» (фолбэк), непустой мусор — «назван несуществующий»
//!    (404).
//!
//! # Почему это единственный экстрактор-исключение
//!
//! В шапке [`crate::routes`] записано, что query разбирается ВРУЧНУЮ внутри
//! хендлера, а тело — через `Option<Json<T>>`: экстрактор axum отработал бы ДО
//! тела хендлера и отдал 400 раньше проверки авторизации. `Portal` — ровно тот
//! случай, где «раньше всего» и требуется: он выбирает состояние, на котором
//! эта лестница потом и работает. Он и разбирает query сам, той же функцией
//! [`crate::routes::dialogs::query_str`], а не `Query<T>`, — чтобы у него не
//! было ни одной ветки в 400.

use std::sync::Arc;

use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::config::normalize_portal_id;
use crate::registry::B24Registry;
use crate::routes::error::ApiError;
use crate::state::B24State;

/// Заголовок с идентификатором портала.
///
/// Имя в общем пространстве `X-Forge-*` (рядом с `X-Forge-File-Name` загрузки):
/// заголовок наш, а не порталa, и путать его с заголовками Bitrix24 нельзя.
///
/// **Дословно совпадает с константой во фронте** (`js/bitrix/api.js`, Ф8.2).
/// Расхождение здесь даёт не ошибку сборки, а молчаливый сполз на первый
/// портал — то есть чужую переписку под своим именем.
pub const PORTAL_HEADER: &str = "x-forge-portal";

/// Query-параметр-фолбэк для адресов, которые браузер открывает без
/// заголовков.
pub const PORTAL_QUERY: &str = "portal";

/// Состояние портала, которому адресован запрос.
///
/// Заменяет `State<Arc<B24State>>` во ВСЕХ хендлерах плагина: состояние теперь
/// выбирает запрос, а не момент сборки роутера. Разворачивается кортежным
/// паттерном ровно как `State`:
///
/// ```ignore
/// pub async fn get_dialogs(Portal(state): Portal, uri: Uri) -> Result<Json<T>, ApiError> {
///     require_ready(&state).await?;   // лестница 409 → 503 → 404 → 400 не меняется
///     // ...
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Portal(pub Arc<B24State>);

impl Portal {
    /// Состояние портала. Удобно там, где кортежный паттерн неуместен.
    pub fn state(&self) -> &Arc<B24State> {
        &self.0
    }

    /// Сам резолв, без HTTP. Вынесен отдельно, чтобы его можно было проверять
    /// таблицей входов, не поднимая роутер, и чтобы им пользовались будущие
    /// точки, у которых `Parts` нет (агрегирующий WS Ф5).
    ///
    /// `named` — то, что назвал запрос (заголовок либо query), уже
    /// нормализованное. `None` означает «портал не назван» — фолбэк.
    ///
    /// # Errors
    ///
    /// - [`ApiError::unknown_portal`] (404) — портал назван, но его нет;
    /// - [`ApiError::no_portal`] (503) — в реестре нет ни одного слота.
    pub fn resolve(registry: &B24Registry, named: Option<&str>) -> Result<Self, ApiError> {
        match named {
            Some(id) => registry.get(id).map(Portal).ok_or_else(ApiError::unknown_portal),
            // Именно `list()`, а не `connected()`: первый слот может быть
            // ПУСТЫМ, и это ровно тот слот, который показывает экран
            // подключения на свежей установке.
            None => registry
                .list()
                .into_iter()
                .next()
                .map(Portal)
                .ok_or_else(ApiError::no_portal),
        }
    }
}

/// Идентификатор портала, названный запросом.
///
/// Возвращает `None`, когда портал не назван вовсе (нет ни заголовка, ни
/// параметра, либо они пусты) — это фолбэк, а не ошибка. Непустое, но
/// нераспознаваемое значение возвращается нормализованным (возможно, пустой
/// строкой) — и не резолвится, потому что [`B24Registry::get`] пустой `id` не
/// принимает.
fn named_portal(parts: &Parts) -> Option<String> {
    // Заголовок приоритетнее query, и наличие заголовка ЗАКРЫВАЕТ вопрос: в
    // query экстрактор проваливается только когда заголовка нет вовсе.
    // Нечитаемое как текст значение — это «назван несуществующий портал»
    // (пустой `id` не резолвится ⇒ 404), а не «портал не назван»: сползание на
    // соседа показало бы чужую переписку под чужим именем.
    let raw = match parts.headers.get(PORTAL_HEADER) {
        Some(v) => match v.to_str() {
            Ok(s) if s.trim().is_empty() => return None,
            Ok(s) => s.to_string(),
            Err(_) => return Some(String::new()),
        },
        None => crate::routes::dialogs::query_str(parts.uri.query(), PORTAL_QUERY)
            .map(crate::routes::upload::percent_decode)
            .filter(|v| !v.trim().is_empty())?,
    };

    // Санитизация обязательна: значение пришло из сети, а `portal_id` уезжает
    // в лог, в имя каталога кэша (Ф6) и в ключ пер-портальных настроек (Ф7).
    // Идентификаторы в реестре уже нормализованы (`portal_id_from_host`),
    // поэтому сравнение нормализованного с нормализованным — единственное
    // корректное.
    Some(normalize_portal_id(&raw))
}

#[async_trait]
impl FromRequestParts<Arc<B24Registry>> for Portal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        registry: &Arc<B24Registry>,
    ) -> Result<Self, Self::Rejection> {
        Portal::resolve(registry, named_portal(parts).as_deref())
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;

    use super::*;
    use crate::config::{BitrixConfig, WebhookUrl};

    /// Реестр из готовых состояний с герметичными путями.
    fn registry(dir: &std::path::Path, portals: &[&str]) -> Arc<B24Registry> {
        let shared = BitrixConfig {
            cache_dir: dir.join("cache"),
            prompts_path: dir.join("prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            config_path: dir.join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        let slots = portals
            .iter()
            .map(|id| {
                let mut cfg = shared.clone();
                cfg.portal_id = (*id).to_string();
                cfg.webhook = if id.is_empty() {
                    WebhookUrl::default()
                } else {
                    WebhookUrl::new(format!("https://{id}.bitrix24.ru/rest/1/abc123def456/"))
                };
                B24State::new(cfg)
            })
            .collect();
        B24Registry::from_states(slots)
    }

    fn parts(uri: &str, header: Option<&str>) -> Parts {
        let mut b = Request::builder().uri(uri);
        if let Some(h) = header {
            b = b.header(PORTAL_HEADER, h);
        }
        b.body(()).unwrap().into_parts().0
    }

    async fn extract(reg: &Arc<B24Registry>, uri: &str, header: Option<&str>) -> Result<Portal, ApiError> {
        let mut p = parts(uri, header);
        Portal::from_request_parts(&mut p, reg).await
    }

    #[tokio::test]
    async fn without_a_header_the_request_lands_on_the_first_portal() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha", "beta"]);
        let got = extract(&reg, "/api/bitrix/status", None).await.unwrap();
        assert_eq!(got.0.portal_id(), "alpha");
    }

    /// Фолбэк обязан отдавать ПУСТОЙ слот свежей установки: иначе экран
    /// подключения перестал бы работать до появления первого портала.
    #[tokio::test]
    async fn the_fallback_returns_the_vacant_slot_of_a_fresh_install() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &[""]);
        let got = extract(&reg, "/api/bitrix/status", None).await.unwrap();
        assert_eq!(got.0.portal_id(), "");
        assert!(got.0.config.webhook.is_empty());
    }

    #[tokio::test]
    async fn the_header_selects_the_portal_by_id() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha", "beta"]);
        let got = extract(&reg, "/api/bitrix/status", Some("beta")).await.unwrap();
        assert_eq!(got.0.portal_id(), "beta");
    }

    #[tokio::test]
    async fn the_header_wins_over_the_query_parameter() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha", "beta"]);
        let got = extract(&reg, "/api/bitrix/status?portal=alpha", Some("beta"))
            .await
            .unwrap();
        assert_eq!(got.0.portal_id(), "beta");
    }

    #[tokio::test]
    async fn the_query_parameter_works_where_a_header_is_impossible() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha", "beta"]);
        let got = extract(&reg, "/ws/bitrix?token=x&portal=beta", None)
            .await
            .unwrap();
        assert_eq!(got.0.portal_id(), "beta");
    }

    /// Неизвестный портал — 404 в общем конверте ошибок, БЕЗ отражения
    /// присланной строки.
    #[tokio::test]
    async fn an_unknown_portal_is_a_404_in_the_common_envelope() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha"]);
        for (uri, header) in [
            ("/api/bitrix/status", Some("gamma")),
            ("/api/bitrix/status?portal=gamma", None),
            // Мусор, который после нормализации не совпадёт ни с чем.
            ("/api/bitrix/status", Some("../../etc/passwd")),
            ("/api/bitrix/status", Some("мусор")),
        ] {
            let err = extract(&reg, uri, header).await.expect_err("404");
            assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND, "{uri} {header:?}");
            assert_eq!(err.body.code, "unknown_portal");
            assert!(
                !err.body.message.contains("passwd") && !err.body.message.contains("gamma"),
                "ответ отражает чужой ввод: {}",
                err.body.message
            );
        }
    }

    /// Пустой заголовок — это «портал не назван», а не «назван пустой».
    #[tokio::test]
    async fn an_empty_header_falls_back_instead_of_404() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha", "beta"]);
        for h in ["", "   "] {
            let got = extract(&reg, "/api/bitrix/status", Some(h)).await.unwrap();
            assert_eq!(got.0.portal_id(), "alpha");
        }
    }

    /// Регистр и хвостовой мусор нормализуются так же, как при выводе `id` из
    /// хоста вебхука, — иначе `X-Forge-Portal: ALPHA` давал бы 404.
    #[tokio::test]
    async fn the_incoming_id_is_normalized_before_the_lookup() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha"]);
        for h in ["ALPHA", "Alpha", "alpha "] {
            let got = extract(&reg, "/api/bitrix/status", Some(h)).await.unwrap();
            assert_eq!(got.0.portal_id(), "alpha", "заголовок {h:?}");
        }
    }

    /// Percent-encoding в `?portal=` декодируется: адрес медиа собирает
    /// `encodeURIComponent`.
    #[tokio::test]
    async fn the_query_value_is_percent_decoded() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha-2"]);
        let got = extract(&reg, "/api/bitrix/status?portal=alpha%2D2", None)
            .await
            .unwrap();
        assert_eq!(got.0.portal_id(), "alpha-2");
    }

    /// Экстрактор НЕ отдаёт 400 ни на каком вводе: лестница data-роутов
    /// начинается с 409, и мусор обязан доезжать до неё.
    #[tokio::test]
    async fn the_extractor_never_answers_400() {
        let d = tempfile::tempdir().unwrap();
        let reg = registry(d.path(), &["alpha"]);
        let long = "a".repeat(4096);
        for (uri, header) in [
            ("/api/bitrix/status?portal=", None),
            ("/api/bitrix/status?portal=%%%", None),
            ("/api/bitrix/status?portal=a&portal=b", None),
            ("/api/bitrix/status", Some("!!!")),
            ("/api/bitrix/status", Some(long.as_str())),
        ] {
            let out = extract(&reg, uri, header).await;
            let status = out.err().map(|e| e.status);
            assert_ne!(
                status,
                Some(axum::http::StatusCode::BAD_REQUEST),
                "{uri} {header:?} отдал 400"
            );
        }
    }
}
