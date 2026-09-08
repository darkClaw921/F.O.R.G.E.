//! WebSocket-handler `/ws/bitrix?token=` — ОДИН сокет на ВСЕ порталы.
//!
//! ## Жизненный цикл соединения
//!
//! 1. [`bitrix_ws`] апгрейдит HTTP в WebSocket. Bearer-auth (в remote-mode)
//!    уже выполнен middleware'ом хоста — query-параметр `token` существует
//!    только потому, что браузерный `WebSocket` не умеет слать заголовок
//!    `Authorization`.
//! 2. [`handle_socket`] подписывается на сигнал состава реестра, затем на
//!    broadcast КАЖДОГО портала ([`PortalFanIn`]) и только после этого шлёт
//!    снапшот: `login_state` каждого слота плюс кадр `portals` со строкой
//!    вкладок. Клиенту не нужно ждать события, чтобы отрисовать панель.
//! 3. Дальше — один `tokio::select!`:
//!    - событие из broadcast ЛЮБОГО портала → кадр клиенту (БЕЗ фильтрации,
//!      см. ниже) с `portal_id` этого портала;
//!    - изменился состав реестра → пересборка набора подписок и новый
//!      снапшот для новичков;
//!    - входящий кадр → парсим [`B24ClientMsg`], `Pong` сбрасывает
//!      idle-таймер;
//!    - тик heartbeat'а (15 с) → [`B24ServerMsg::Ping`];
//!    - тишина [`IDLE_TIMEOUT`] (60 с) → закрываем соединение;
//!    - `shutdown`-токен РЕЕСТРА → закрываем соединение.
//! 4. `RecvError::Lagged(n)` НЕ рвёт соединение: клиенту уходит
//!    [`B24ServerMsg::Resync`], он перечитывает состояние по REST (Р19).
//!
//! ## Агрегация: почему `select_all`, а не задачи-пересылки
//!
//! Приёмники всех порталов лежат в [`PortalFanIn`] и опрашиваются
//! `futures_util::future::select_all` прямо в цикле сокета. Альтернатива —
//! задача на портал, пересылающая кадры в общий `mpsc`, — стоила бы дороже
//! ровно там, где важнее всего:
//!
//! - **Каденс.** `state.ws_subscribers()` — это `receiver_count()` broadcast'а
//!   портала, то есть счётчик живёт ТАМ ЖЕ, где приёмник. Приёмник внутри
//!   задачи пережил бы закрытие сокета на время её отмены, и портал
//!   продолжал бы опрашиваться в active-каденсе с закрытой панелью (Ф5.3).
//! - **`Lagged`.** Отставание принадлежит КОНКРЕТНОМУ приёмнику. Пройди кадры
//!   через общий `mpsc`, отстать мог бы уже он — и `resync` перестал бы быть
//!   адресным.
//!
//! Здесь же приёмники принадлежат сокету и умирают вместе с ним, в тот же
//! момент. `broadcast::Receiver::recv` cancel-safe, поэтому пересобирать
//! набор futures на каждой итерации `select!` безопасно: непрочитанный кадр
//! остаётся в канале.
//!
//! ## Инварианты, которые легко потерять при переписывании
//!
//! - **Подписка ДО снапшота — для КАЖДОГО портала.** Событие, случившееся
//!   между снапшотом и подпиской, иначе потерялось бы навсегда. Правило
//!   распространяется и на портал, добавленный в рантайме: [`PortalFanIn::sync`]
//!   сначала подписывается, и только потом вызывающий шлёт его снапшот.
//! - **`biased` в `select!`.** Порядок веток — это приоритет: shutdown
//!   первым, чтобы остановка сервера не ждала heartbeat'а.
//! - **Битый входящий кадр не рвёт соединение.** Неизвестный кадр может
//!   прийти от более новой версии фронта (или от старой из кэша PWA).
//! - **`Lagged` даёт АДРЕСНЫЙ `resync`.** Отстал один портал — перечитывать
//!   состояние остальных незачем, поэтому кадр уезжает с `portal_id`
//!   отставшего.
//! - **Закрытый канал ОДНОГО портала не закрывает сокет.** Он означает ровно
//!   «этот портал отвязали»: подписка снимается, остальные продолжают вещать.
//!
//! ## `portal_id` есть в КАЖДОМ кадре
//!
//! Его приклеивает конверт [`crate::ws::protocol::B24Frame`] — единственный
//! способ отправить кадр в сокет. Кадрам, у которых нет портала-владельца,
//! достаются зарезервированные значения [`VACANT_PORTAL_ID`] и
//! [`SOCKET_PORTAL_ID`]; пустым `portal_id` не бывает никогда.
//!
//! ## Почему нет фильтрации по активному диалогу
//!
//! Бейдж непрочитанного на кнопке вкладки и тосты о новых сообщениях должны
//! работать, когда открыта ЛЮБАЯ вкладка devforge. Поэтому сервер вещает все
//! события, а решает, что показать, фронтенд (в том числе применяя белый
//! список чатов, Ф11).
//!
//! Число живых подписчиков — не только статистика: воркер событий (Ф7)
//! выбирает по нему каденс опроса портала (`ws_subscribers() == 0` ⇒
//! idle-интервал, Р4). Счётчик ПЕР-ПОРТАЛЬНЫЙ и остаётся таким при общем
//! сокете: одно соединение даёт каждому порталу ровно одного подписчика, а не
//! одному порталу N (Ф5.3).

pub mod protocol;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{broadcast, Mutex};
use tokio::time::Instant;

use crate::registry::B24Registry;
use crate::state::B24State;
use crate::ws::protocol::{B24ClientMsg, B24Frame, B24ServerEvent, B24ServerMsg};

/// Период heartbeat-пинга. Совпадает с Echo, Telegram и VK (15 с):
/// realtime-вкладка должна замечать разрыв быстро, чтобы фронт успел
/// переподключиться.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Если 60 с не пришло ни одного кадра (включая `pong`) — соединение
/// считается мёртвым и закрывается. Четыре пропущенных пинга подряд.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// `portal_id` кадров слота, который ещё НЕ портал.
///
/// Реестр никогда не пуст: свежая установка — это один слот с пустым вебхуком
/// и пустым `id` (см. шапку [`crate::registry`]). Его автомат подключения
/// вещает `login_state` (`checking` → `invalid_credentials` → …), и эти кадры
/// нужны фронту, чтобы экран подключения жил без перезагрузки страницы.
///
/// Пустая строка сюда не годится: фронт различает вкладки по `portal_id`, а
/// пустое значение неотличимо от «поле забыли заполнить». Подчёркивание
/// невозможно у настоящего идентификатора по построению —
/// [`crate::config::normalize_portal_id`] оставляет только `[a-z0-9-]` и
/// никогда не начинает результат с дефиса, — так что коллизии с порталом нет.
pub const VACANT_PORTAL_ID: &str = "_vacant";

/// `portal_id` служебных кадров САМОГО СОКЕТА: `ping` и `portals`.
///
/// Они не принадлежат ни одному порталу: heartbeat относится к соединению, а
/// строка вкладок — к реестру целиком. Отдельное значение (а не `portal_id`
/// первого слота) избавляет фронт от соблазна маршрутизировать их в ленту.
pub const SOCKET_PORTAL_ID: &str = "_socket";

/// Query `/ws/bitrix?token=<bearer>&portal=<id>`.
#[derive(Debug, Default, Deserialize)]
pub struct B24WsQuery {
    /// Опциональный bearer для браузерного WebSocket. Проверку делает
    /// middleware хоста, здесь параметр только принимается и игнорируется.
    #[serde(default)]
    pub token: Option<String>,
    /// Опциональный идентификатор портала.
    ///
    /// С Ф5 ИГНОРИРУЕТСЯ: сокет несёт кадры всех порталов сразу, и выбирать
    /// один незачем. Поле объявлено, чтобы старый фронт из кэша PWA (и
    /// [`crate::routes::Portal`], который читает тот же параметр в HTTP-роутах)
    /// не получал 400 на апгрейде из-за лишнего query-параметра.
    ///
    /// Заголовка `X-Forge-Portal` у браузерного `WebSocket` быть не может —
    /// ровно та же причина, по которой рядом живёт `token`.
    #[serde(default)]
    pub portal: Option<String>,
}

/// HTTP-handler апгрейда в WebSocket.
///
/// Состоянием берётся ВЕСЬ реестр, а не портал: сокет несёт кадры всех
/// порталов сразу, поэтому экстрактор [`crate::routes::Portal`] здесь был бы
/// вреден — он выбрал бы один слот и навсегда приклеил бы к нему соединение,
/// открытое на живом процессе до подключения остальных порталов.
///
/// `?portal=` при этом принимается и игнорируется: старый фронт из кэша PWA
/// продолжает его слать, и отвечать ему 400 на апгрейд было бы худшим из
/// возможных ответов.
pub async fn bitrix_ws(
    ws: WebSocketUpgrade,
    State(registry): State<Arc<B24Registry>>,
    Query(_q): Query<B24WsQuery>,
) -> Response {
    tracing::debug!(
        target: "forge_bitrix",
        portals = registry.len(),
        "bitrix_ws: upgrade"
    );
    ws.on_upgrade(move |socket| handle_socket(socket, registry))
}

/// Тип отправляющей половины сокета под общим Mutex'ом.
type WsSink = Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

/// Главный цикл одного WS-соединения.
async fn handle_socket(socket: WebSocket, registry: Arc<B24Registry>) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx: WsSink = Arc::new(Mutex::new(ws_tx));

    // Порядок подписок обязателен и читается снизу вверх:
    //   1) сигнал состава — иначе портал, добавленный между сборкой набора
    //      подписок и подпиской на сигнал, остался бы немым до следующего
    //      изменения реестра;
    //   2) broadcast каждого портала — ДО снапшота (см. шапку модуля).
    let mut composition = registry.subscribe_composition();
    let mut fan = PortalFanIn::new(&registry);

    // Снапшот: состояние подключения КАЖДОГО слота плюс строка вкладок.
    for state in fan.states() {
        let msg = B24ServerMsg::LoginState(state.login_state_dto().await);
        if send_server_msg(&ws_tx, &frame_portal_id(&state), &msg)
            .await
            .is_err()
        {
            return;
        }
    }
    if send_portals(&ws_tx, &registry).await.is_err() {
        return;
    }

    // Тайминги берутся из СОСТОЯНИЯ, а не из констант: в проде они равны
    // HEARTBEAT_INTERVAL/IDLE_TIMEOUT, а интеграционный тест поднимает
    // состояние с сотнями миллисекунд и проверяет настоящий цикл, не дожидаясь
    // боевой минуты (см. B24State::with_ws_timings).
    //
    // Берутся у ПЕРВОГО слота и фиксируются на всё соединение. У порталов они
    // одинаковы по построению (общие настройки раздаются копией), а
    // перечитывать их на каждой итерации значило бы менять heartbeat открытого
    // сокета при отвязке портала.
    let (hb_interval, idle_timeout) = {
        let first = registry.primary();
        (first.ws_heartbeat, first.ws_idle_timeout)
    };
    let mut heartbeat = tokio::time::interval(hb_interval);
    heartbeat.tick().await; // первый тик мгновенный — пропускаем.
    let mut last_activity = Instant::now();

    loop {
        let idle_deadline = last_activity + idle_timeout;

        // `select!` только ВЫБИРАЕТ событие, а обрабатывается оно ниже.
        // Причина техническая и жёсткая: ветки держат `&mut fan` до конца
        // всего выражения, поэтому тронуть набор подписок прямо в теле ветки
        // (снять отвалившийся портал, пересобрать состав) нельзя — заимствован
        // он будет ещё соседней веткой.
        let wake = tokio::select! {
            biased;

            // Остановка сервера — закрываемся первыми. Токен РЕЕСТРА, а не
            // портала: отвязка одного портала сокет не закрывает.
            () = registry.shutdown.cancelled() => Wake::Shutdown,

            // Idle-таймаут.
            () = tokio::time::sleep_until(idle_deadline) => Wake::Idle,

            // Heartbeat.
            _ = heartbeat.tick() => Wake::Heartbeat,

            // Состав реестра изменился: портал подключили, отвязали,
            // переименовали или переставили вкладки.
            //
            // `changed()` cancel-safe и схлопывает серию правок в один
            // пересбор — большего и не нужно: состав читается из реестра, а не
            // из канала.
            changed = composition.changed() => Wake::Composition(changed.is_ok()),

            // Событие из broadcast ЛЮБОГО портала — без фильтрации.
            (idx, portal_id, ev) = fan.recv() => Wake::Portal(idx, portal_id, Box::new(ev)),

            // Входящий кадр.
            opt = ws_rx.next() => Wake::Incoming(opt),
        };

        match wake {
            Wake::Shutdown => {
                tracing::debug!(target: "forge_bitrix", "bitrix_ws: shutdown, closing");
                break;
            }

            Wake::Idle => {
                tracing::info!(target: "forge_bitrix", "bitrix_ws: idle timeout, closing");
                break;
            }

            Wake::Heartbeat => {
                if send_server_msg(&ws_tx, SOCKET_PORTAL_ID, &B24ServerMsg::Ping)
                    .await
                    .is_err()
                {
                    break;
                }
            }

            // Реестр уничтожен — на живом процессе такого не бывает, но
            // держать сокет над несуществующим реестром незачем.
            Wake::Composition(false) => break,

            Wake::Composition(true) => {
                let fresh = fan.sync(&registry);
                // Снапшот новичкам — ПОСЛЕ подписки, которую уже сделал
                // `sync`: иначе первое событие только что подключённого
                // портала терялось бы ровно так же, как терялось бы первое
                // событие при открытии сокета.
                let mut broken = false;
                for state in fresh {
                    let msg = B24ServerMsg::LoginState(state.login_state_dto().await);
                    if send_server_msg(&ws_tx, &frame_portal_id(&state), &msg)
                        .await
                        .is_err()
                    {
                        broken = true;
                        break;
                    }
                }
                if broken || send_portals(&ws_tx, &registry).await.is_err() {
                    break;
                }
            }

            Wake::Portal(idx, portal_id, ev) => match broadcast_frame(*ev) {
                Some(msg) => {
                    let status_changed = matches!(msg, B24ServerMsg::LoginState(_));
                    if send_server_msg(&ws_tx, &portal_id, &msg).await.is_err() {
                        break;
                    }
                    // Сменилось состояние подключения — сменился и СТАТУС
                    // вкладки (`connected`, `login`). Строка вкладок обязана
                    // отражать это сразу, иначе портал, ушедший в
                    // `throttled`, выглядел бы живым до ближайшей правки
                    // состава.
                    //
                    // Только на `login_state`, а не на любой кадр: переходы
                    // автомата редки, а вот `counters` идут постоянно — и
                    // полный список вкладок на каждый из них был бы чистым
                    // шумом. Бейдж непрочитанного фронт двигает самим кадром
                    // `counters` (у него есть `portal_id`), а `unread` в
                    // строке вкладок — лишь снимок для холодного старта.
                    if status_changed && send_portals(&ws_tx, &registry).await.is_err() {
                        break;
                    }
                }
                // Канал закрыт ⇒ состояние портала уничтожено. Это НЕ конец
                // соединения: снимаем подписку и продолжаем нести остальные
                // порталы.
                None => {
                    tracing::debug!(
                        target: "forge_bitrix",
                        portal = %portal_id,
                        "bitrix_ws: portal channel closed, dropping its subscription"
                    );
                    fan.drop_at(idx);
                }
            },

            Wake::Incoming(opt) => match opt {
                Some(Ok(Message::Close(_))) | None => {
                    tracing::debug!(target: "forge_bitrix", "bitrix_ws: client closed");
                    break;
                }
                Some(Ok(Message::Text(text))) => {
                    last_activity = Instant::now();
                    // Битый кадр НЕ рвёт соединение — см. `client_action`.
                    if client_action(&text) == ClientAction::Ignore {
                        tracing::warn!(
                            target: "forge_bitrix",
                            "bitrix_ws: malformed client frame, ignored"
                        );
                    }
                }
                // Протокольные ping/pong тоже считаем активностью.
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(_)) => {
                    last_activity = Instant::now();
                }
                Some(Err(e)) => {
                    tracing::debug!(target: "forge_bitrix", error = ?e, "bitrix_ws: recv error");
                    break;
                }
            },
        }
    }

    let mut guard = ws_tx.lock().await;
    let _ = guard.send(Message::Close(None)).await;
    let _ = guard.close().await;
    tracing::debug!(target: "forge_bitrix", "bitrix_ws: terminated");
}

// ---------------------------------------------------------------------------
// Пробуждение цикла.
// ---------------------------------------------------------------------------

/// Что именно разбудило цикл сокета.
///
/// Существует ради одного ограничения `tokio::select!`: его ветки держат
/// заимствования до конца ВСЕГО выражения, поэтому набор подписок нельзя
/// править прямо в теле ветки. `select!` выбирает событие, `match` его
/// обрабатывает — и заодно весь цикл читается сверху вниз одним списком.
enum Wake {
    /// Отменён общий токен реестра — останавливается сервер.
    Shutdown,
    /// Тишина дольше `ws_idle_timeout`.
    Idle,
    /// Пора слать [`B24ServerMsg::Ping`].
    Heartbeat,
    /// Изменился состав реестра; `false` ⇒ канал состава закрыт.
    Composition(bool),
    /// Кадр (или ошибка приёма) от портала: его индекс в [`PortalFanIn`],
    /// `portal_id` на момент получения и сам результат.
    ///
    /// Кадр под `Box`: `B24ServerMsg` — самый крупный вариант (сотни байт с
    /// текстом сообщения), и без коробки его размер платил бы КАЖДЫЙ проход
    /// цикла, включая heartbeat и pong.
    Portal(
        usize,
        String,
        Box<Result<B24ServerEvent, broadcast::error::RecvError>>,
    ),
    /// Что-то пришло от клиента.
    Incoming(Option<Result<Message, axum::Error>>),
}

// ---------------------------------------------------------------------------
// Мультиплексор порталов.
// ---------------------------------------------------------------------------

/// Одна подписка: портал и его приёмник.
struct PortalSub {
    /// Состояние портала. Держится, чтобы `portal_id` читался НА МОМЕНТ
    /// ОТПРАВКИ, а не на момент подписки: пустой слот, который заняли
    /// вебхуком, обязан начать подписывать свои кадры настоящим
    /// идентификатором сам, без пересборки подписки.
    state: Arc<B24State>,
    /// Приёмник broadcast'а портала. Живёт здесь, а не в отдельной задаче —
    /// см. «Агрегация» в шапке модуля.
    rx: broadcast::Receiver<B24ServerEvent>,
}

/// Набор подписок на broadcast ВСЕХ порталов реестра.
///
/// Владелец приёмников — сокет: `receiver_count()` каждого портала растёт на
/// единицу при открытии соединения и падает обратно ровно в момент его
/// закрытия. Именно на этом держится каденс опроса (Ф5.3).
pub struct PortalFanIn {
    subs: Vec<PortalSub>,
}

impl PortalFanIn {
    /// Подписывается на broadcast каждого слота реестра.
    ///
    /// Вызывать ДО отправки снапшота: событие, случившееся между снапшотом и
    /// подпиской, иначе потерялось бы навсегда.
    ///
    /// Подписываются ВСЕ слоты, включая ещё не занятый: его автомат вещает
    /// `login_state`, и без подписки экран подключения не оживал бы без
    /// перезагрузки страницы.
    #[must_use]
    pub fn new(registry: &Arc<B24Registry>) -> Self {
        let subs = registry
            .list()
            .into_iter()
            .map(|state| PortalSub {
                rx: state.subscribe(),
                state,
            })
            .collect();
        Self { subs }
    }

    /// Состояния, на которые сейчас есть подписка, в порядке реестра.
    #[must_use]
    pub fn states(&self) -> Vec<Arc<B24State>> {
        self.subs.iter().map(|s| s.state.clone()).collect()
    }

    /// Сколько порталов сейчас в наборе.
    #[must_use]
    pub fn len(&self) -> usize {
        self.subs.len()
    }

    /// Пуст ли набор (реестр без слотов — состояние, которого не бывает).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    /// Приводит набор подписок в соответствие с составом реестра.
    ///
    /// Возвращает состояния, на которые подписка ПОЯВИЛАСЬ только что — их
    /// снапшот вызывающий шлёт сам, уже после этой подписки.
    ///
    /// Сравнение идёт по указателю `Arc`, а не по `portal_id`: у пустого слота
    /// идентификатора нет вовсе, а занятый слот меняет его на живом процессе —
    /// сравнение по имени приняло бы одно и то же состояние за новый портал и
    /// подписалось бы на него вторым приёмником, то есть удвоило бы кадры в
    /// сокете.
    pub fn sync(&mut self, registry: &Arc<B24Registry>) -> Vec<Arc<B24State>> {
        let current = registry.list();
        self.subs
            .retain(|sub| current.iter().any(|s| Arc::ptr_eq(s, &sub.state)));
        let mut fresh = Vec::new();
        for state in current {
            if self.subs.iter().any(|sub| Arc::ptr_eq(&sub.state, &state)) {
                continue;
            }
            self.subs.push(PortalSub {
                rx: state.subscribe(),
                state: state.clone(),
            });
            fresh.push(state);
        }
        if !fresh.is_empty() {
            tracing::debug!(
                target: "forge_bitrix",
                added = fresh.len(),
                portals = self.subs.len(),
                "bitrix_ws: subscribed to the portals added at runtime"
            );
        }
        fresh
    }

    /// Ждёт кадр от ЛЮБОГО портала.
    ///
    /// Возвращает индекс подписки, её `portal_id` на этот момент и результат
    /// приёма (в том числе `Lagged`/`Closed` — их разбирает
    /// [`broadcast_frame`]).
    ///
    /// **Cancel-safe.** Набор futures пересобирается на каждом вызове, а
    /// `broadcast::Receiver::recv` не теряет кадр при отмене — иначе каждый
    /// heartbeat съедал бы по событию.
    ///
    /// Пустой набор ждёт вечно: сокет над реестром без слотов существовать не
    /// может, но крутиться в горячем цикле он тем более не должен.
    pub async fn recv(
        &mut self,
    ) -> (
        usize,
        String,
        Result<B24ServerEvent, broadcast::error::RecvError>,
    ) {
        if self.subs.is_empty() {
            std::future::pending::<()>().await;
        }
        let futures: Vec<_> = self
            .subs
            .iter_mut()
            .map(|sub| Box::pin(sub.rx.recv()))
            .collect();
        let (res, idx, rest) = futures_util::future::select_all(futures).await;
        drop(rest);
        (idx, self.portal_id(idx), res)
    }

    /// `portal_id` подписки для конверта кадра.
    #[must_use]
    pub fn portal_id(&self, idx: usize) -> String {
        self.subs
            .get(idx)
            .map_or_else(|| SOCKET_PORTAL_ID.to_string(), |s| frame_portal_id(&s.state))
    }

    /// Снимает подписку — портал отвязали и его состояние уничтожено.
    pub fn drop_at(&mut self, idx: usize) {
        if idx < self.subs.len() {
            self.subs.remove(idx);
        }
    }
}

/// `portal_id` кадра для конверта.
///
/// Пустой идентификатор означает слот без портала и заменяется
/// [`VACANT_PORTAL_ID`]: пустым `portal_id` на проводе не бывает никогда (см.
/// [`crate::ws::protocol::B24Frame`]).
#[must_use]
pub fn frame_portal_id(state: &Arc<B24State>) -> String {
    let id = state.portal_id();
    if id.is_empty() {
        VACANT_PORTAL_ID.to_string()
    } else {
        id
    }
}

// ---------------------------------------------------------------------------
// Кадры.
// ---------------------------------------------------------------------------

/// Что делать с текстовым кадром клиента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAction {
    /// Валидный `pong` — idle-таймер уже сброшен вызывающим.
    Pong,
    /// Кадр не разобран. **Соединение не рвём**: он мог прийти от более новой
    /// версии фронта — или от старой, поднятой из кэша PWA. Разрыв в обоих
    /// случаях означал бы бесконечный цикл реконнектов вместо одной строки в
    /// консоли.
    Ignore,
}

/// Разбирает текстовый кадр клиента.
///
/// Отдельная функция ровно по той же причине, что и [`broadcast_frame`]:
/// правило «битый кадр не рвёт соединение» проверяется тестом, а внутри
/// `handle_socket` его пришлось бы проверять поднятым сокетом с настоящим
/// апгрейдом — то есть не проверять вовсе.
#[must_use]
pub fn client_action(text: &str) -> ClientAction {
    match serde_json::from_str::<B24ClientMsg>(text) {
        Ok(B24ClientMsg::Pong) => ClientAction::Pong,
        Err(_) => ClientAction::Ignore,
    }
}

/// Отправляет кадр `portals` — строку вкладок целиком.
///
/// Список собирает [`crate::routes::portals::portals_snapshot`], то есть тот
/// же код, что отвечает на `GET /api/bitrix/portals`: вкладка, приехавшая
/// кадром, обязана выглядеть ровно так же, как после перезагрузки страницы.
async fn send_portals(ws_tx: &WsSink, registry: &Arc<B24Registry>) -> Result<(), axum::Error> {
    let msg = B24ServerMsg::Portals {
        portals: crate::routes::portals::portals_snapshot(registry).await,
    };
    send_server_msg(ws_tx, SOCKET_PORTAL_ID, &msg).await
}

/// Что отправить клиенту по результату чтения broadcast-канала.
///
/// `None` означает «этот портал больше не вещает», `Some(msg)` — кадр к
/// отправке. Соединение при `None` НЕ закрывается: канал закрывается вместе с
/// состоянием портала, то есть при его отвязке, а остальные порталы этого
/// сокета продолжают работать.
///
/// # Р19: `Lagged` → `resync`, и НИКОГДА не досылка
///
/// Отставший подписчик (канал ёмкостью [`crate::state::BROADCAST_CAPACITY`]
/// переполнился) пропустил `n` кадров, и восстановить их неоткуда: broadcast
/// хранит только хвост. Единственный честный ответ — сказать клиенту
/// «перечитай состояние по REST»; попытка «доcлать пропущенное» дала бы ленту,
/// собранную из дырявой последовательности, причём молча.
///
/// Отдельная функция, а не ветка `match` внутри цикла, по одной причине:
/// правило проверяется тестом. Внутри `handle_socket` его пришлось бы
/// проверять поднятым сокетом с настоящим апгрейдом — то есть не проверять
/// вовсе.
pub fn broadcast_frame(
    ev: Result<B24ServerMsg, tokio::sync::broadcast::error::RecvError>,
) -> Option<B24ServerMsg> {
    match ev {
        Ok(msg) => Some(msg),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                target: "forge_bitrix",
                skipped = n,
                "bitrix_ws: broadcast lagged, sending resync"
            );
            Some(B24ServerMsg::Resync)
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            tracing::debug!(target: "forge_bitrix", "bitrix_ws: broadcast closed");
            None
        }
    }
}

/// Отправляет [`B24ServerMsg`] текстовым кадром — ВСЕГДА в конверте
/// [`B24Frame`], то есть всегда с `portal_id`.
///
/// Единственная точка отправки в сокет. Именно поэтому кадра без `portal_id`
/// не бывает: собрать его здесь не из чего.
///
/// Ошибка сериализации не должна рвать соединение (это баг сервера, а не
/// клиента), поэтому логируется и трактуется как успех.
async fn send_server_msg(
    ws_tx: &WsSink,
    portal_id: &str,
    msg: &B24ServerMsg,
) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(&B24Frame::new(portal_id, msg)) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "forge_bitrix", error = %e, "bitrix_ws: serialize failed");
            return Ok(());
        }
    };
    let mut guard = ws_tx.lock().await;
    guard.send(Message::Text(text)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_is_shorter_than_idle_timeout() {
        // Иначе соединение умирало бы раньше, чем клиент успевал ответить.
        assert!(HEARTBEAT_INTERVAL * 2 < IDLE_TIMEOUT);
    }

    #[test]
    fn default_state_uses_production_timings() {
        // Тестовый конструктор существует ради коротких таймаутов — но
        // обычный путь обязан остаться боевым.
        let st = crate::state::B24State::new(crate::config::BitrixConfig::default());
        assert_eq!(st.ws_heartbeat, HEARTBEAT_INTERVAL);
        assert_eq!(st.ws_idle_timeout, IDLE_TIMEOUT);
    }

    /// ЗАМОРОЖЕННОЕ ПРАВИЛО: битый входящий кадр НЕ рвёт соединение.
    ///
    /// Кадр может прийти от более новой версии фронта — или от старой,
    /// поднятой из кэша PWA. Разрыв в обоих случаях означал бы бесконечный
    /// цикл реконнектов: клиент переподключается, шлёт тот же кадр, снова
    /// получает разрыв.
    #[test]
    fn a_malformed_client_frame_is_ignored_not_fatal() {
        assert_eq!(client_action(r#"{"type":"pong"}"#), ClientAction::Pong);
        for junk in [
            r#"{"type":"hello"}"#,   // кадр из будущего
            r#"{"type":"pong","v":2}"#, // ...с лишним полем — тоже валиден
            "{",                     // обрывок
            "",                      // пустая строка
            "не json вовсе",
        ] {
            let got = client_action(junk);
            assert!(
                matches!(got, ClientAction::Ignore | ClientAction::Pong),
                "кадр {junk:?} не должен приводить к разрыву"
            );
        }
        assert_eq!(client_action(r#"{"type":"hello"}"#), ClientAction::Ignore);
    }

    /// Зарезервированные `portal_id` невозможны у настоящего портала.
    ///
    /// Совпади они — кадр экрана подключения или heartbeat приехал бы во
    /// вкладку реального портала, причём молча.
    #[test]
    fn the_reserved_portal_ids_cannot_collide_with_a_real_one() {
        for reserved in [VACANT_PORTAL_ID, SOCKET_PORTAL_ID] {
            assert!(reserved.starts_with('_'));
            assert_ne!(
                crate::config::normalize_portal_id(reserved),
                reserved,
                "идентификатор портала не может содержать подчёркивание"
            );
        }
        assert_ne!(VACANT_PORTAL_ID, SOCKET_PORTAL_ID);
    }

    /// Слот без портала подписывает кадры `_vacant`, занятый — своим `id`.
    #[test]
    fn a_vacant_slot_never_signs_a_frame_with_an_empty_id() {
        let state = crate::state::B24State::new(crate::config::BitrixConfig::default());
        let state = Arc::new(state);
        assert_eq!(frame_portal_id(&state), VACANT_PORTAL_ID);
        state.set_portal_identity("acme", "Акме");
        assert_eq!(frame_portal_id(&state), "acme");
    }

    #[test]
    fn query_token_is_optional_and_tolerates_extras() {
        let q: B24WsQuery = serde_json::from_str("{}").unwrap();
        assert!(q.token.is_none());
        // Новый фронт может дописать параметры в URL реконнекта (версия
        // клиента, id вкладки) — старый сервер обязан их игнорировать, а не
        // отвечать 400 на апгрейд.
        let q: B24WsQuery = serde_json::from_str(r#"{"token":"t","v":"2"}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some("t"));
        // `?token=` (пустое значение) — реальный URL при незаполненном
        // сторадже фронта.
        let q: B24WsQuery = serde_json::from_str(r#"{"token":""}"#).unwrap();
        assert_eq!(q.token.as_deref(), Some(""));
    }

    /// Переполнение broadcast обязано превращаться в `Lagged`, а не в тихую
    /// потерю событий: именно на этом держится кадр `resync` (Р19).
    #[tokio::test]
    async fn broadcast_overflow_reports_lagged() {
        let st = crate::state::B24State::new(crate::config::BitrixConfig::default());
        let mut rx = st.subscribe();
        for _ in 0..(crate::state::BROADCAST_CAPACITY + 8) {
            st.broadcast_event(B24ServerMsg::Ping);
        }
        match rx.recv().await {
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => assert!(n > 0),
            other => panic!("ожидался Lagged, пришло {other:?}"),
        }
    }

    /// Р19 целиком, от переполнения канала до кадра: отставший подписчик
    /// получает `resync` и НЕ получает досылки пропущенного.
    #[tokio::test]
    async fn a_lagging_subscriber_gets_a_resync_frame() {
        let st = crate::state::B24State::new(crate::config::BitrixConfig::default());
        let mut rx = st.subscribe();
        // Заливаем канал заведомо сильнее ёмкости — ровно то, что делает
        // активный портал с вкладкой, которую усыпил браузер.
        for _ in 0..(crate::state::BROADCAST_CAPACITY + 8) {
            st.broadcast_event(B24ServerMsg::Ping);
        }

        assert_eq!(
            broadcast_frame(rx.recv().await),
            Some(B24ServerMsg::Resync),
            "отставший подписчик обязан получить resync"
        );
        // Дальше поток продолжается с ЖИВОГО хвоста, а не с пропущенного
        // начала: досылки нет и быть не может — broadcast хранит только хвост.
        assert_eq!(
            broadcast_frame(rx.recv().await),
            Some(B24ServerMsg::Ping)
        );

        // Обычное событие проходит как есть.
        st.broadcast_event(B24ServerMsg::Resync);
        // Закрытый канал — это конец соединения, а не кадр.
        drop(st);
        let mut drained = rx;
        while let Ok(msg) = drained.try_recv() {
            assert!(matches!(msg, B24ServerMsg::Ping | B24ServerMsg::Resync));
        }
        assert_eq!(broadcast_frame(drained.recv().await), None);
    }
}
