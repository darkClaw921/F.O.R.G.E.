//! Управление составом порталов — CRUD над [`crate::registry::B24Registry`].
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | GET    | `/api/bitrix/portals`      | список вкладок (без единого следа секрета) |
//! | POST   | `/api/bitrix/portals`      | подключить портал по вебхуку |
//! | PATCH  | `/api/bitrix/portals/:pid` | переименовать и/или переставить вкладку |
//! | DELETE | `/api/bitrix/portals/:pid` | отвязать портал (с кэшем или без) |
//!
//! # Этот модуль — не [`crate::routes::portal`]
//!
//! Имена различаются одной буквой, и это стоит держать в голове: `portal` —
//! экстрактор «какому порталу адресован запрос», он читает реестр; `portals`
//! (здесь) — роуты, которые реестр МЕНЯЮТ. Соседние по имени, но
//! противоположные по направлению.
//!
//! Отсюда и разница в состоянии хендлеров: здесь берётся
//! `State<Arc<B24Registry>>`, а не [`crate::routes::Portal`]. Портал,
//! адресуемый заголовком, к составу списка отношения не имеет — а
//! `DELETE /portals/:pid` с адресацией через экстрактор означал бы «удали тот
//! портал, который сейчас открыт», то есть удаление не тем жестом, каким его
//! просили.
//!
//! # Секрет не выходит наружу ни в одном ответе
//!
//! Вебхук проходит через фронт РОВНО ОДИН РАЗ — телом `POST /portals` — и
//! нигде не задерживается: ни в query, ни в логах, ни в ответе. Тип
//! [`crate::config::WebhookUrl`] не реализует `Serialize`, так что положить
//! его в DTO нельзя даже по невнимательности, но [`PortalDto`] собирается
//! вручную, и на это есть отдельный тест
//! (`the_portal_list_never_leaks_the_webhook`).
//!
//! ## Хоста портала в ответе тоже нет — решение, а не упущение
//!
//! `GET /api/bitrix/status` намеренно не отдаёт хост портала ни в одном
//! состоянии автомата, и на это есть замороженный тест
//! (`status_never_leaks_the_webhook_in_any_state`). Список порталов —
//! соседний по смыслу ответ, и два соседних роута с противоположными
//! правилами про одно и то же поле разъедутся при первой же правке.
//!
//! Поэтому правило единое: **хост вебхука не пересекает границу HTTP**.
//! Вкладке он и не нужен — узнаваемое имя портала несёт `id`
//! (`https://test-work-2.bitrix24.ru/rest/…` ⇒ `test-work-2`, см.
//! [`crate::config::portal_id_from_host`]), а поверх него пользователь ставит
//! свой `title`. Разница между `id` и хостом — только доменный суффикс, и у
//! коробочного портала это как раз внутреннее имя инфраструктуры, то есть
//! единственная часть, которую стоило бы не публиковать.
//!
//! Следствие для фронта: пустой `title` показывается как `id`, а не как хост
//! (Ф8).
//!
//! # Автоповторов нет ни при каких условиях
//!
//! Ни `POST /portals`, ни любой другой роут здесь не повторяет обращение к
//! порталу сам. Портал душит интенсивность отдельным состоянием автомата
//! (`throttled`, Р16), и автоповтор его только продлевает: каждый лишний
//! запрос отодвигает момент, когда портал снова начнёт отвечать. Повтор
//! всегда инициирует пользователь — он видит `retry_after_secs` и решает.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::config::normalize_portal_id;
use crate::login::{
    apply_login_error, parse_webhook_input, verify_webhook, HttpBackend, LoginStateDto,
};
use crate::registry::B24Registry;
use crate::routes::error::ApiError;

// ---------------------------------------------------------------------------
// DTO.
// ---------------------------------------------------------------------------

/// Одна вкладка панели.
///
/// **Ни вебхука, ни его отпечатка, ни длины, ни хоста** — см. шапку модуля.
///
/// `Deserialize` и `PartialEq` — не для роутов: то же DTO едет WS-кадром
/// [`crate::ws::protocol::B24ServerMsg::Portals`], а весь enum кадров
/// разбирается и сравнивается в тестах протокола. Ровно та же причина, по
/// которой их имеют [`crate::dto::DialogDto`] и [`crate::dto::MessageDto`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PortalDto {
    /// Стабильный идентификатор — он же значение заголовка
    /// [`crate::routes::portal::PORTAL_HEADER`]. У ещё не занятого слота
    /// пуст, но такие слоты в список не попадают.
    pub id: String,

    /// Имя, которое дал пользователь. Пусто ⇒ фронт показывает `id`.
    pub title: String,

    /// Живо ли соединение прямо сейчас (тот же признак, что в
    /// `GET /api/bitrix/status`).
    pub connected: bool,

    /// Состояние автомата подключения — той же формы, что снапшот `/status` и
    /// WS-кадр `login_state`. Одна форма на три источника означает одну
    /// функцию отрисовки во фронте.
    pub login: LoginStateDto,

    /// Непрочитанное для бейджа вкладки.
    ///
    /// Берётся ИЗ СНИМКА счётчиков и никогда не ходит в сеть: список вкладок
    /// перерисовывается на каждое переключение портала, а сетевой вариант
    /// стоил бы N запросов из бюджета 2 rps КАЖДОГО портала на один клик — то
    /// есть сам загонял бы порталы в `throttled`. Снимка нет ⇒ `0`; настоящее
    /// число приедет ближайшим кадром WS.
    pub unread: i64,
}

/// Ответ `GET /api/bitrix/portals`.
///
/// Объект, а не голый массив: у соседних роутов панели (`FiltersResp`,
/// `TasksResp`) тело — объект, и добавить рядом со списком поле (например,
/// активный портал) в объект можно, не ломая фронт, а в массив — нет.
#[derive(Debug, Clone, Serialize)]
pub struct PortalsResp {
    /// Вкладки В ПОРЯДКЕ РЕЕСТРА — он же порядок вкладок на экране.
    pub portals: Vec<PortalDto>,
}

// ---------------------------------------------------------------------------
// Роутер.
// ---------------------------------------------------------------------------

/// Роуты управления порталами.
pub fn router() -> Router<Arc<B24Registry>> {
    Router::new()
        .route(
            "/api/bitrix/portals",
            get(get_portals).post(post_portals),
        )
        .route(
            "/api/bitrix/portals/:pid",
            patch(patch_portal).delete(delete_portal),
        )
}

// ---------------------------------------------------------------------------
// Проекция.
// ---------------------------------------------------------------------------

/// Проекция одного слота во вкладку.
async fn portal_dto(state: &Arc<crate::state::B24State>, now: i64) -> PortalDto {
    let ident = state.portal_identity();
    let unread = (*state.counters.read().await)
        .map(|(c, _)| c.dialogs_unread())
        .unwrap_or(0);
    PortalDto {
        id: ident.id,
        title: ident.title,
        connected: state.connected(now).await,
        login: state.login_state_dto().await,
        unread,
    }
}

// ---------------------------------------------------------------------------
// Хендлеры.
// ---------------------------------------------------------------------------

/// `GET /api/bitrix/portals` → 200 всегда.
///
/// Как и `/status`, отвечает в любом состоянии: по нему фронт решает, рисовать
/// ли строку вкладок вообще.
///
/// # Пустые слоты отфильтрованы, и это обязательно
///
/// Реестр НИКОГДА не пуст: на свежей установке (и после отвязки последнего
/// портала) в нём стоит слот с пустым вебхуком и пустым `id` — место для
/// портала, а не портал. Отдай его сюда, и фронт нарисовал бы вкладку-призрак:
/// безымянную, неадресуемую (пустой `id` не резолвится [`B24Registry::get`]) и
/// неудаляемую.
///
/// Свежая установка ⇒ пустой список. Это и есть признак «порталов нет,
/// показывай экран подключения».
///
/// # Фильтр — по `id`, а не по [`B24Registry::connected`]
///
/// Соблазн взять готовую `connected()` велик, но она смотрит на КОНФИГОВЫЙ
/// вебхук, а он у только что занятого пустого слота ещё пуст — вебхук туда
/// кладёт `set_webhook`, то есть рантаймовое поле. Портал, подключённый
/// первым на свежей установке, не появлялся бы в строке вкладок до
/// перезапуска devforge, притом что панель им уже пользуется.
///
/// `portal_id` — ровно тот признак, который нужен: реестр присваивает его
/// вместе с секретом ([`B24Registry::add`]), он же адресует вкладку
/// заголовком, и «слот, который нельзя назвать» — это в точности «слот,
/// которого во вкладках быть не должно».
pub async fn get_portals(State(registry): State<Arc<B24Registry>>) -> Json<PortalsResp> {
    Json(PortalsResp {
        portals: portals_snapshot(&registry).await,
    })
}

/// Строка вкладок целиком — общий источник для роута и для WS-кадра
/// [`crate::ws::protocol::B24ServerMsg::Portals`].
///
/// Отдельная функция, а не тело хендлера, ровно по одной причине: сокет Ф5
/// шлёт тот же список на каждое изменение состава реестра, и вторая проекция
/// разъехалась бы с этой на первой же правке — например, снова показала бы
/// вкладку-призрак для пустого слота (см. фильтр по `id` выше).
///
/// В СЕТЬ НЕ ХОДИТ: `unread` берётся из снимка счётчиков. Кадр `portals`
/// отправляется на каждое изменение состава, и сетевой вариант превращал бы
/// перетаскивание вкладок в N запросов из бюджета 2 rps каждого портала.
pub async fn portals_snapshot(registry: &Arc<B24Registry>) -> Vec<PortalDto> {
    let now = crate::dto::now_unix();
    let mut portals = Vec::new();
    for state in registry.list() {
        if state.portal_id().is_empty() {
            continue;
        }
        portals.push(portal_dto(&state, now).await);
    }
    portals
}

/// Тело `POST /api/bitrix/portals`.
///
/// **СОДЕРЖИТ СЕКРЕТ.** `Debug` не выводится намеренно — ровно по той же
/// причине, что у [`crate::routes::login::WebhookReq`]: структура с голой
/// `String` внутри попала бы в отладочную печать целиком.
#[derive(Deserialize)]
pub struct AddPortalReq {
    /// То, что пользователь вставил в поле. Разбор трёх форм адреса —
    /// [`parse_webhook_input`], и только на бэкенде.
    pub webhook: String,

    /// Необязательное имя вкладки. Пусто ⇒ вкладка подписывается `id`.
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /api/bitrix/portals` — подключить портал.
///
/// Порядок действий дословно повторяет экран подключения
/// ([`crate::routes::login::post_login_webhook`]) и обратным быть не может:
///
/// 1. **разобрать** — мусор отвергается `400 invalid_input` ДО того, как
///    что-то попадёт на диск и в сеть;
/// 2. **записать и поднять** — [`B24Registry::add`] пишет секрет в конфиг
///    (read-modify-write, 0600, атомарно), поднимает состояние и его воркеров;
/// 3. **проверить** сетью — `profile` → `scope`.
///
/// # Неудачная проверка портал НЕ откатывает
///
/// `invalid_credentials`, `scope_missing`, `throttled` и `disconnected` — это
/// СОСТОЯНИЯ подключённого портала, а не отказ добавления: вкладка появляется
/// и показывает, что именно не так. Иначе пользователь, у которого портал
/// временно недоступен, не смог бы даже завести вкладку — а вебхук пришлось бы
/// вставлять заново, то есть проносить секрет через фронт второй раз.
///
/// Ответ — та же форма, что у `GET /portals`: вкладка целиком, чтобы фронт
/// вставил её в строку без дополнительного запроса.
///
/// # Errors
///
/// `400 invalid_input` — строка не похожа на адрес входящего вебхука либо
/// вебхук непригоден для вызовов; `500 io_error` — не удалось записать конфиг
/// (портал в этом случае НЕ появляется ни в списке, ни на диске).
pub async fn post_portals(
    State(registry): State<Arc<B24Registry>>,
    // ↓ ТЕЛО СОДЕРЖИТ СЕКРЕТ: не логировать ни целиком, ни по полям.
    Json(req): Json<AddPortalReq>,
) -> Result<Json<PortalDto>, ApiError> {
    let webhook = parse_webhook_input(&req.webhook).map_err(|e| ApiError::bad_input(e.to_string()))?;

    let title = req.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let state = registry.add(&webhook, title).await.map_err(|e| {
        // Текст ошибки ФС может цитировать путь, но не сам секрет.
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not add the portal"
        );
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            format!("could not save the portal to the config file: {e}"),
        )
    })?;

    // Проверка идёт клиентом ЭТОГО портала — своим троттлером и своим
    // секретом. Собрать его не удаётся только у заведомо непригодного
    // вебхука: это ошибка ввода, а не портала.
    match state.api_client().await {
        Ok(client) => {
            verify_webhook(&state, &HttpBackend::new(client)).await;
        }
        Err(e) => {
            tracing::warn!(
                target: "forge_bitrix",
                portal = %state.portal_id(),
                error = %e,
                "forge-bitrix: could not build the API client for the new portal"
            );
            apply_login_error(
                &state,
                crate::login::LoginError::BadInput {
                    reason: "этим вебхуком нельзя обратиться к порталу".to_string(),
                },
            )
            .await;
        }
    }

    Ok(Json(portal_dto(&state, crate::dto::now_unix()).await))
}

/// Тело `PATCH /api/bitrix/portals/:pid`.
///
/// Оба поля необязательны: пустое тело — не ошибка, а «ничего не менять».
/// Отказывать 400 на пустом теле значило бы отличать «не прислал» от
/// «прислал пустое», а фронт шлёт то, что изменилось.
#[derive(Debug, Default, Deserialize)]
pub struct PatchPortalReq {
    /// Новое имя вкладки. Пустая строка — стереть имя (вкладка снова
    /// подписывается `id`), ровно как пустая строка стирает заметку к диалогу.
    #[serde(default)]
    pub title: Option<String>,

    /// Полный порядок вкладок после перестановки — список `id`.
    ///
    /// Именно ПОЛНЫЙ список, а не «позиция этого портала»: перетаскивание
    /// вкладки меняет позиции сразу нескольких, и передавать индекс значило бы
    /// заставлять сервер угадывать, относительно какого состояния списка он
    /// посчитан. [`B24Registry::reorder`] терпит неполный и грязный список —
    /// фронт мог отстать от состава реестра на один запрос.
    #[serde(default)]
    pub order: Option<Vec<String>>,
}

/// `PATCH /api/bitrix/portals/:pid` — переименовать вкладку и/или переставить
/// вкладки.
///
/// Две операции в одном роуте намеренно: обе меняют ОДИН и тот же файл
/// read-modify-write'ом, обе идут через один и тот же мьютекс реестра, и обе
/// вызываются одним жестом контекстного меню. Отдельный `/reorder` был бы
/// вторым путём записи того же списка.
///
/// Порядок применения — сначала имя, потом порядок: перестановка отвечает
/// целым списком, и он обязан уже содержать новое имя.
///
/// # Errors
///
/// `404 unknown_portal` — портала с таким `:pid` в реестре нет; `500 io_error`
/// — не удалось переписать конфиг.
pub async fn patch_portal(
    State(registry): State<Arc<B24Registry>>,
    Path(pid): Path<String>,
    body: Option<Json<PatchPortalReq>>,
) -> Result<Json<PortalsResp>, ApiError> {
    // Резолв ПЕРЕД разбором тела — та же лестница, что в data-роутах:
    // неизвестный портал не должен получать оракул валидности тела.
    let pid = normalize_portal_id(&pid);
    if registry.get(&pid).is_none() {
        return Err(ApiError::unknown_portal());
    }
    let req = body.map(|Json(v)| v).unwrap_or_default();

    if let Some(title) = req.title.as_deref() {
        if !registry.rename(&pid, title.trim()).await.map_err(io_error)? {
            // Портал исчез между резолвом и записью — соседний запрос успел
            // его отвязать. Это 404, а не 500.
            return Err(ApiError::unknown_portal());
        }
    }

    if let Some(order) = req.order.as_deref() {
        let order: Vec<String> = order.iter().map(|id| normalize_portal_id(id)).collect();
        registry.reorder(&order).await.map_err(io_error)?;
    }

    Ok(get_portals(State(registry)).await)
}

/// `DELETE /api/bitrix/portals/:pid?purge_cache=1` — отвязать портал.
///
/// Секрет стирается из конфига, воркеры гасятся, состояние снимается со
/// списка — см. [`B24Registry::remove`]. На портал при этом НЕ ходим: отозвать
/// входящий вебхук через REST невозможно, он живёт, пока владелец не удалит
/// его в интерфейсе Битрикс24 (та же оговорка, что у logout, —
/// [`crate::routes::login::LOGOUT_HINT`]).
///
/// # Отвязка последнего портала не роняет процесс
///
/// На его место встаёт пустой слот, и `GET /portals` начинает отдавать пустой
/// список — фронт возвращается на экран подключения. Ровно то же состояние,
/// что на свежей установке.
///
/// # `?purge_cache=1`
///
/// Query разбирается ВРУЧНУЮ (правило шапки [`crate::routes`]), значение по
/// умолчанию — НЕ удалять: кэш дешевле восстановить, чем вернуть, а до Ф6 все
/// порталы делят один каталог, и [`B24Registry::remove`] сама откажется
/// сносить общий.
///
/// # Errors
///
/// `404 unknown_portal` — портала с таким `:pid` в реестре нет (в том числе на
/// ПОВТОРНЫЙ DELETE: удалять нечего); `500 io_error` — не удалось переписать
/// конфиг.
pub async fn delete_portal(
    State(registry): State<Arc<B24Registry>>,
    Path(pid): Path<String>,
    uri: axum::http::Uri,
) -> Result<Json<PortalsResp>, ApiError> {
    let pid = normalize_portal_id(&pid);
    if registry.get(&pid).is_none() {
        return Err(ApiError::unknown_portal());
    }
    let purge_cache = purge_requested(uri.query());

    if !registry.remove(&pid, purge_cache).await.map_err(io_error)? {
        // Между проверкой и удалением портал успел исчезнуть — 404, не 500.
        return Err(ApiError::unknown_portal());
    }

    // Логирует сама [`B24Registry::remove`] — вторая строка о том же событии
    // только раздувала бы лог.
    Ok(get_portals(State(registry)).await)
}

/// Просили ли снести кэш вместе с порталом (`?purge_cache=1`).
///
/// Приняты ровно две записи истины (`1` и `true`) — те же, что у `?force=` в
/// [`crate::routes::voice`]. Всё остальное, включая отсутствие параметра, —
/// «кэш сохранить»: у необратимого действия значение по умолчанию обязано
/// быть безопасным, а опечатка в параметре не должна стирать данные.
fn purge_requested(raw: Option<&str>) -> bool {
    matches!(
        crate::routes::dialogs::query_str(raw, "purge_cache"),
        Some("1" | "true")
    )
}

/// Ошибка записи конфига в общем конверте.
///
/// Текст ошибки ФС может цитировать путь, но не секрет: сам вебхук в
/// `anyhow`-цепочку не попадает ни из одного примитива записи
/// [`crate::config`].
fn io_error(e: anyhow::Error) -> ApiError {
    tracing::warn!(
        target: "forge_bitrix",
        error = %e,
        "forge-bitrix: could not rewrite the portals config"
    );
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "io_error",
        format!("could not rewrite the config file: {e}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Необратимое действие по умолчанию НЕ выполняется: кэш стирается только
    /// по явному «да».
    #[test]
    fn only_an_explicit_yes_purges_the_cache() {
        for (q, want) in [
            (None, false),
            (Some(""), false),
            (Some("purge_cache=1"), true),
            (Some("purge_cache=true"), true),
            (Some("x=1&purge_cache=1"), true),
            (Some("purge_cache=0"), false),
            (Some("purge_cache="), false),
            (Some("purge_cache=yes"), false),
            (Some("purge_cache"), false),
            (Some("purgecache=1"), false),
        ] {
            assert_eq!(purge_requested(q), want, "{q:?}");
        }
    }

    /// В сериализованной вкладке нет ни одного поля, которого нет в
    /// замороженном контракте: лишнее поле — это то, чем секрет утекает.
    #[test]
    fn the_tab_dto_has_exactly_the_agreed_fields() {
        let dto = PortalDto {
            id: "alpha".into(),
            title: "Рабочий".into(),
            connected: true,
            login: LoginStateDto {
                state: "authorized".into(),
                error: None,
                retry_after_secs: None,
                user: None,
            },
            unread: 3,
        };
        let v = serde_json::to_value(&dto).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["connected", "id", "login", "title", "unread"]);
    }
}
