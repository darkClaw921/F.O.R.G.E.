//! Роуты подключения и отвязки портала.
//!
//! | метод | путь | что делает |
//! |---|---|---|
//! | POST | `/api/bitrix/login/webhook` | принять вставленный URL, сохранить его 0600 и проверить |
//! | POST | `/api/bitrix/logout`       | стереть вебхук АДРЕСУЕМОГО портала ЛОКАЛЬНО — на портал не ходим |
//!
//! # Оба роута работают с АДРЕСУЕМЫМ порталом
//!
//! Экран подключения старше мультипортальности и остаётся ровно там же: это
//! он показывается, когда порталов ещё нет, и он же — когда вебхук вкладки
//! перестал работать. С реестром меняется только одно: «портал» здесь больше
//! не значит «единственный», а значит «тот, который выбрал экстрактор
//! [`Portal`]» (заголовок `X-Forge-Portal` → `?portal=` → первый слот).
//!
//! Отсюда два следствия, каждое из которых легко потерять:
//!
//! - подключение НЕ переписывает список порталов целиком (см.
//!   [`persist_webhook`]);
//! - выход отвязывает ОДИН портал, а не все (см. [`post_logout`]).
//!
//! Управление составом вкладок живёт отдельно, в [`crate::routes::portals`].
//!
//! Оба отвечают одним и тем же [`LoginStateDto`] — той же формы, что снапшот
//! `GET /api/bitrix/status` и WS-кадр `login_state`. Три источника одной формы
//! означают ОДНУ функцию отрисовки во фронте вместо трёх ветвлений.
//!
//! # Инвариант секрета в HTTP-слое
//!
//! Тело `POST /api/bitrix/login/webhook` СОДЕРЖИТ СЕКРЕТ целиком: у Bitrix24
//! вебхук — это весь URL. `TraceLayer` хоста тела запросов не пишет, но
//! полагаться только на это нельзя, поэтому:
//!
//! - хендлер не логирует ни тело, ни его длину, ни отдельные поля;
//! - [`parse_webhook_input`] чистая и молчит по построению;
//! - в лог уходит ТОЛЬКО отпечаток ([`crate::config::WebhookUrl::fingerprint`])
//!   и только из `config::save_webhook_to`;
//! - ответ роута — обычный `LoginStateDto`, в котором нет ни кода, ни хоста.
//!
//! # Порядок действий при подключении обратным быть не может
//!
//! 1. **разобрать** — мусор отвергается `400 invalid_input` и состоянием
//!    `invalid_url` ДО того, как что-то попадёт на диск и в сеть;
//! 2. **сохранить** в `bitrix.toml` (атомарно, 0600, чужие ключи целы);
//! 3. **применить к состоянию** — [`crate::state::B24State::set_webhook`]
//!    заодно сбрасывает кэш клиента, то есть и его троттлер;
//! 4. **проверить** сетью — `profile` → `scope`.
//!
//! Сохранение ПЕРЕД проверкой намеренно: проверка строит клиент из состояния, а
//! пользователь, закрывший вкладку на середине, всё равно останется с
//! введённым вебхуком и увидит результат после перезапуска. Обратный порядок
//! потребовал бы второго пути «проверить, не сохраняя» — то есть второго места,
//! где секрет живёт в памяти.
//!
//! # Почему logout ничего не отзывает на портале
//!
//! Отозвать входящий вебхук через REST невозможно: он живёт, пока владелец не
//! удалит его в интерфейсе Битрикс24 (Разработчикам → Другое → Входящий
//! вебхук). Мы стираем его у себя и говорим об этом прямо в UI — делать вид,
//! что «отвязали», значит врать пользователю о состоянии его портала.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::config::WebhookUrl;
use crate::login::{
    apply_login_error, parse_webhook_input, verify_webhook, HttpBackend, LoginFlow, LoginStateDto,
};
use crate::registry::B24Registry;
use crate::routes::error::ApiError;
use crate::routes::Portal;

/// Тело `POST /api/bitrix/login/webhook`.
///
/// **СОДЕРЖИТ СЕКРЕТ.** `Debug` не выводится намеренно: структура с голой
/// `String` внутри попала бы в отладочную печать целиком.
///
/// Имя поля `webhook` — часть замороженного контракта с
/// `tmux-web/static/js/bitrix/api.js`.
#[derive(Deserialize)]
pub struct WebhookReq {
    /// То, что пользователь вставил в поле: адрес с методом, без метода, со
    /// слэшем или без. Разбор — [`parse_webhook_input`], и только здесь, на
    /// бэкенде.
    pub webhook: String,
}

/// Ответ обоих роутов — тот же снимок, что у `/status` и WS-кадра.
pub type LoginResp = LoginStateDto;

/// Роуты подключения.
pub fn router() -> Router<Arc<B24Registry>> {
    Router::new()
        .route("/api/bitrix/login/webhook", post(post_login_webhook))
        .route("/api/bitrix/logout", post(post_logout))
}

/// `POST /api/bitrix/login/webhook` — принять вставленный адрес вебхука.
///
/// Порядок — см. шапку модуля. Невалидный ввод НЕ доходит до сети: счётчик
/// вызовов портала после мусорной строки обязан остаться нулевым (тест
/// `garbage_input_never_reaches_the_portal`).
///
/// # Errors
///
/// `400 invalid_input` — строка не похожа на адрес входящего вебхука
/// (состояние автомата при этом становится `invalid_url`); `500 io_error` —
/// не удалось записать конфиг, вебхук в этом случае НЕ применяется.
pub async fn post_login_webhook(
    Portal(state): Portal,
    State(registry): State<Arc<B24Registry>>,
    // ↓ ТЕЛО СОДЕРЖИТ СЕКРЕТ: не логировать ни целиком, ни по полям.
    Json(req): Json<WebhookReq>,
) -> Result<Json<LoginStateDto>, ApiError> {
    let webhook = match parse_webhook_input(&req.webhook) {
        Ok(w) => w,
        Err(e) => {
            // Состояние обязано измениться ВМЕСТЕ с ответом: экран подключения
            // открыт и в соседних вкладках, а они узнают об отказе только
            // кадром `login_state`.
            let reason = e.to_string();
            apply_login_error(&state, e).await;
            return Err(ApiError::bad_input(reason));
        }
    };

    let state = persist_webhook(&registry, &state, &webhook).await?;

    let client = state.api_client().await.map_err(|e| {
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not build the API client for the new webhook"
        );
        ApiError::bad_input("this webhook cannot be used to call the portal")
    })?;
    let flow = verify_webhook(&state, &HttpBackend::new(client)).await;
    Ok(Json(flow.to_dto()))
}

/// Записывает секрет так, чтобы СОСЕДНИЕ порталы остались целы, и возвращает
/// состояние, к которому он применён.
///
/// Историческая реализация звала [`save_webhook_to`], а та ЗАМЕНЯЕТ весь
/// список одной записью: при одном портале разницы не было, а со строкой
/// вкладок подключение на одной вкладке стирало бы секреты всех остальных.
/// Развилок ровно две, и различает их ХОСТ:
///
/// 1. **Пустой слот либо другой хост** — это подключение НОВОГО портала:
///    [`B24Registry::add`] занимает пустой слот на свежей установке и заводит
///    вкладку рядом в остальных случаях. Старая вкладка при этом остаётся —
///    вставить чужой вебхук в чужую вкладку не значит потребовать удалить её
///    портал.
/// 2. **Тот же хост, другой код** — вебхук отозвали на портале и выпустили
///    заново. Это ТА ЖЕ вкладка: секрет меняется в её записи
///    ([`crate::config::replace_portal_webhook_in`]), `id`, имя и позиция
///    сохраняются.
///
/// Сброс кэша клиента (а с ним и троттлера) делает `set_webhook` — иначе
/// проверка пошла бы в портал со СТАРЫМ секретом. В первой ветке его зовёт сам
/// реестр.
///
/// # Errors
///
/// `500 io_error` — не удалось записать конфиг; вебхук в этом случае НЕ
/// применяется.
async fn persist_webhook(
    registry: &Arc<B24Registry>,
    state: &Arc<crate::state::B24State>,
    webhook: &WebhookUrl,
) -> Result<Arc<crate::state::B24State>, ApiError> {
    let io = |e: anyhow::Error| {
        // Текст ошибки ФС может цитировать путь, но не сам секрет.
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            "forge-bitrix: could not persist the webhook"
        );
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            format!("could not save the webhook to the config file: {e}"),
        )
    };

    let portal_id = state.portal_id();
    let same_portal = !portal_id.is_empty()
        && state.webhook().await.portal_host() == webhook.portal_host()
        && webhook.portal_host().is_some();

    if !same_portal {
        return registry.add(webhook, None).await.map_err(io);
    }

    if !crate::config::replace_portal_webhook_in(registry.config_path(), &portal_id, webhook)
        .map_err(io)?
    {
        // Записи на диске нет (например, портал подняли из legacy-ключа):
        // дописываем её обычным путём, вместо того чтобы молча оставить
        // секрет только в памяти.
        return registry.add(webhook, None).await.map_err(io);
    }
    state.set_webhook(webhook.clone()).await;
    Ok(state.clone())
}

/// `POST /api/bitrix/logout` — отвязать АДРЕСУЕМЫЙ портал.
///
/// **На портал не ходим вовсе** (см. шапку модуля).
///
/// # Отвязывается ОДИН портал, а не все
///
/// Исторически logout звал [`clear_webhook_to`], а та снимает ОБА
/// представления секрета сразу — и legacy-ключ верхнего уровня, и весь массив
/// `[[portals]]`. При одном портале разницы не было; со строкой вкладок это
/// означало бы, что «выйти» на одной вкладке молча отключает и все остальные.
///
/// Поэтому отвязка идёт через [`B24Registry::remove`] — тем же путём, что
/// `DELETE /api/bitrix/portals/:pid`: один портал, его секрет, его воркеры.
/// Второго пути отключения у плагина нет и быть не должно: он неизбежно
/// разъехался бы с первым в порядке шагов (снять со списка → погасить →
/// стереть секрет). `clear_webhook_to` остаётся в [`crate::config`] —
/// её семантика «снести обе формы» нужна миграции, — но отсюда больше не
/// зовётся.
///
/// Полного «выйти отовсюду» отдельным жестом нет намеренно: фронт зовёт
/// logout по каждой вкладке, а один необратимый жест, снимающий всё разом,
/// стоит ровно один промах мимо нужной кнопки.
///
/// # Кэш переживает logout
///
/// `purge_cache: false` — в отличие от `DELETE /portals/:pid`, где флаг
/// решает пользователь. Старый экран подключения такого выбора не предлагает,
/// а тихо стирать данные жестом, который их стирать не обещал, нельзя.
///
/// # Errors
///
/// `404 unknown_portal` — портал уже отвязан (в том числе повторным logout);
/// `500 io_error` — не удалось переписать конфиг (например, каталог только для
/// чтения).
pub async fn post_logout(
    Portal(state): Portal,
    State(registry): State<Arc<B24Registry>>,
) -> Result<Json<LoginStateDto>, ApiError> {
    // Пустой слот свежей установки снимать со СПИСКА не от чего: `portal_id` у
    // него пуст, реестр такой адресации не знает, а на диске записи и не
    // было. Но уборку состояния он проходит наравне со всеми, и это не
    // формальность: неудачная попытка подключения оставляет ПУСТОЙ слот в
    // `invalid_url` / `invalid_credentials` / `scope_missing`, а вебхук с
    // экрана входа мог лечь в рантаймовое поле в обход реестра. Верни мы
    // снимок как есть — «выйти» на свежей установке не возвращало бы к
    // чистому экрану подключения, а замирало бы на прошлой ошибке.
    //
    // Инвариант ловится по проводу: `tmux-web/tests/bitrix_smoke.rs::
    // test_login_routes_reject_garbage_and_never_leak`.
    let portal_id = state.portal_id();
    if portal_id.is_empty() {
        let dropped_uploads = reset_portal_state(&state).await;
        tracing::info!(
            target: "forge_bitrix",
            dropped_uploads,
            "forge-bitrix: logged out of a slot that holds no portal; the connection screen is reset"
        );
        return Ok(Json(state.login_state_dto().await));
    }

    if !registry
        .remove(&portal_id, false)
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "forge_bitrix",
                error = %e,
                "forge-bitrix: logout failed to rewrite the config"
            );
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "io_error",
                format!("could not remove the webhook from the config file: {e}"),
            )
        })?
    {
        return Err(ApiError::unknown_portal());
    }

    // `remove` уже погасила воркеров портала и стёрла секрет с диска; уборка
    // ниже чистит ПАМЯТЬ — этот же `Arc` мог быть захвачен запросом,
    // начавшимся раньше.
    let dropped_uploads = reset_portal_state(&state).await;

    tracing::info!(
        target: "forge_bitrix",
        dropped_uploads,
        "forge-bitrix: logged out; the webhook is removed locally and is still valid on the portal"
    );
    Ok(Json(state.login_state_dto().await))
}

/// Приводит состояние слота к «портала здесь нет»: чистит всё, что принадлежит
/// аккаунту, и переводит автомат в `not_configured`.
///
/// Возвращает число выброшенных незавершённых загрузок — оно уходит в лог.
///
/// # Почему это ОДНА функция на обе ветки logout'а
///
/// Веток две (слот с порталом и пустой слот), а забыть в одной из них строчку
/// — значит оставить данные прошлого аккаунта живыми ровно в одном сценарии,
/// который никто не проверяет руками.
///
/// Каждый пункт уборки — отдельный дефект, если его пропустить:
///
/// - **вебхук** сбрасывает и кэш API-клиента: оставленный экземпляр продолжил
///   бы ходить в портал с уже стёртым секретом;
/// - **курсор событий** (Ф7) — данные прошлого подключения: следующий вебхук
///   может принадлежать другому порталу, и чужой offset дал бы либо пустоту,
///   либо чужую очередь;
/// - **имена сотрудников и счётчики** (Ф4) подписали бы чужими именами первый
///   же список следующего портала;
/// - **принятые, но не отправленные файлы** (Ф9) уехали бы в чат СЛЕДУЮЩЕГО
///   портала первой же отправкой, а их байты лежали бы на диске до истечения
///   TTL.
async fn reset_portal_state(state: &Arc<crate::state::B24State>) -> usize {
    state.set_webhook(crate::config::WebhookUrl::default()).await;
    state.set_event_cursor(None).await;
    state.peers.clear();
    *state.counters.write().await = None;
    let dropped_uploads = crate::routes::upload::clear_uploads(state).await;
    state.set_login(LoginFlow::NotConfigured).await;
    dropped_uploads
}

/// Текст для UI: что именно делает выход.
///
/// Живёт на бэкенде по той же причине, что и [`crate::routes::WEBHOOK_HINT`]:
/// разметку браузер держит в кэше PWA неделями, а формулировка обязана быть
/// одинаковой во всех вкладках.
///
/// Первое предложение появилось вместе со строкой вкладок и не косметика:
/// [`post_logout`] отвязывает ОДИН портал — тот, который адресован запросом.
/// Пользователю с несколькими вкладками важно знать это ДО нажатия, а не
/// после, потому что секрет придётся вставлять заново.
pub const LOGOUT_HINT: &str = "Отвяжется только этот портал — остальные вкладки останутся \
подключёнными. Вебхук будет удалён только из этого devforge; на портале он продолжит \
действовать — отзовите его вручную: Разработчикам → Другое → Входящий вебхук → удалить.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BitrixConfig, WebhookUrl};
    use crate::login::{LoginBackend, LoginError};
    use crate::state::B24State;

    /// Ошибка разбора остаётся ошибкой ввода, а не «портал отказал».
    ///
    /// Полный путь роута проверяется на стенде (`tests/b24_stub.rs`): здесь
    /// нужен только контракт «`BadInput` → 400 `invalid_input`».
    #[tokio::test]
    async fn bad_input_becomes_invalid_url_and_never_reaches_the_state_as_authorized() {
        let dir = tempfile::tempdir().unwrap();
        let state = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        });

        let flow = apply_login_error(
            &state,
            LoginError::BadInput {
                reason: "не адрес вебхука".into(),
            },
        )
        .await;
        assert_eq!(flow.kind(), "invalid_url");
        assert!(!state.has_webhook().await);
        assert!(
            !dir.path().join("bitrix.toml").exists(),
            "мусорный ввод не должен создавать конфиг"
        );
    }

    /// Logout стирает секрет СВОЕГО портала и с диска, и из памяти, и гасит
    /// клиента.
    ///
    /// Раньше этот тест проверял, что после выхода вебхука в файле нет ВОВСЕ, —
    /// формулировка, верная ровно до второго портала: она описывала не выход, а
    /// побочный эффект [`crate::config::clear_webhook_to`], снимавшей и
    /// legacy-ключ, и весь массив разом. Проверяется то же по существу
    /// («секрет этого портала исчез отовсюду»), но так, чтобы соседи были
    /// видны: у соседа секрет обязан остаться. Полный путь роутом и обе формы
    /// адресации — в `tests/portals_crud.rs`.
    #[tokio::test]
    async fn logout_wipes_the_secret_of_its_own_portal_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let mine = WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/");
        let neighbour = WebhookUrl::new("https://q.bitrix24.ru/rest/1/zzz999yyy888/");
        crate::config::add_portal_to(&path, &mine, None).unwrap();
        crate::config::add_portal_to(&path, &neighbour, None).unwrap();

        let state = |webhook: &WebhookUrl, id: &str| {
            B24State::new(BitrixConfig {
                cache_dir: dir.path().join("cache"),
                // Герметичность: без явного пути хранилище заметок легло бы в
                // живой ~/.forge/bitrix-chat-prompts.json разработчика, а
                // CLI-заглушка гарантирует, что тест не запустит настоящий
                // claude.
                prompts_path: dir.path().join("prompts.json"),
                claude_cli_path: dir.path().join("no-such-claude"),
                config_path: path.clone(),
                webhook: webhook.clone(),
                portal_id: id.to_string(),
                ..BitrixConfig::default()
            })
        };
        let mine_state = state(&mine, "p");
        let registry = B24Registry::from_states(vec![mine_state.clone(), state(&neighbour, "q")]);

        // Клиент собран — после выхода его быть не должно.
        let _ = mine_state.api_client().await.unwrap();
        assert!(mine_state.client.read().await.is_some());

        let resp = post_logout(Portal(mine_state.clone()), State(registry.clone()))
            .await
            .unwrap();
        assert_eq!(resp.0.state, "not_configured");
        assert!(!mine_state.has_webhook().await);
        assert!(
            mine_state.client.read().await.is_none(),
            "кэш клиента не сброшен"
        );
        assert_eq!(registry.ids(), vec!["q"], "отвязан не тот портал");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("abc123def456"), "секрет не стёрт: {body}");
        assert!(body.contains("zzz999yyy888"), "стёрт секрет соседа: {body}");

        // Повторный выход адресует портал, которого уже нет, — 404, а не 500.
        let err = post_logout(Portal(mine_state), State(registry))
            .await
            .expect_err("портала больше нет");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.body.code, "unknown_portal");
    }

    /// Выход из ПУСТОГО слота возвращает экран подключения к чистому виду.
    ///
    /// Регрессия, найденная смоуком по проводу
    /// (`tmux-web/tests/bitrix_smoke.rs::test_login_routes_reject_garbage_and_
    /// never_leak`): пустой слот `remove` не адресует, и ранняя ветка отдавала
    /// снимок КАК ЕСТЬ. На свежей установке это значило, что после неудачной
    /// попытки подключения «выйти» оставляло панель в `invalid_url`, то есть
    /// на прошлой ошибке, — до мультипортальности logout всегда переводил
    /// автомат в `not_configured`.
    #[tokio::test]
    async fn logout_of_a_vacant_slot_resets_the_connection_screen() {
        let dir = tempfile::tempdir().unwrap();
        let state = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            ..BitrixConfig::default()
        });
        assert!(state.portal_id().is_empty(), "слот обязан быть пустым");

        // Неудачная попытка подключения оставляет слот на ошибке.
        apply_login_error(
            &state,
            LoginError::BadInput {
                reason: "не адрес вебхука".into(),
            },
        )
        .await;
        assert_eq!(state.login_state_dto().await.state, "invalid_url");
        // И вебхук мог лечь в рантаймовое поле в обход реестра (старый экран
        // входа) — он тоже обязан исчезнуть.
        state
            .set_webhook(WebhookUrl::new("https://p.bitrix24.ru/rest/1/abc123def456/"))
            .await;

        let registry = B24Registry::single(state.clone());
        let resp = post_logout(Portal(state.clone()), State(registry.clone()))
            .await
            .expect("выход из пустого слота не может краснеть");
        assert_eq!(resp.0.state, "not_configured");
        assert!(!state.has_webhook().await, "рантаймовый вебхук пережил выход");
        assert_eq!(registry.len(), 1, "слот остаётся на месте");

        // Повторный выход — тот же ответ, а не 404: отвязывать по-прежнему
        // нечего.
        let resp = post_logout(Portal(state), State(registry))
            .await
            .expect("повторный выход не должен краснеть");
        assert_eq!(resp.0.state, "not_configured");
    }

    /// HttpBackend существует ради боевого пути и обязан оставаться объектно
    /// безопасным: `verify_webhook` принимает `&dyn LoginBackend`.
    #[test]
    fn the_backend_trait_stays_object_safe() {
        fn assert_object_safe(_: &dyn LoginBackend) {}
        let _ = assert_object_safe;
    }
}
