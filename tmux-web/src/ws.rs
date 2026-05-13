//! WebSocket-handler `/ws/attach` — bridge между браузером и PTY с tmux attach.
//!
//! ### Endpoint
//!
//! `GET /ws/attach?session=<name>&cols=<u16>&rows=<u16>` — апгрейд в WebSocket.
//! Дефолты: `cols=80`, `rows=24`. `session` обязателен (если отсутствует —
//! axum вернёт 400 от Query-extractor'а).
//!
//! ### Wire-протокол
//!
//! - **Binary frames** в обе стороны — сырые байты PTY (escape-последовательности
//!   xterm-256color). Browser xterm.js пишет введённые символы как Binary,
//!   сервер шлёт stdout PTY как Binary.
//! - **Text frames** от клиента — JSON control-сообщения:
//!   - `{"type":"resize","cols":120,"rows":40}` — `pty.resize()`.
//!   - `{"type":"switch","session":"other"}` — kill старого PTY, spawn нового.
//!   Невалидный JSON / неизвестный type — лог `warn`, WS остаётся живым.
//! - **Close frame** — корректный teardown.
//!
//! ### Архитектура внутри handle_socket
//!
//! Handler владеет `Arc<Mutex<Option<PtyHandle>>>` и `Arc<AtomicBool>` (cancel).
//! Запускает три tokio-таска:
//! 1. **PTY→WS reader-task** (spawn_blocking + mpsc): синхронно `read()` из
//!    PTY reader'а, шлёт байты через mpsc в WS-writer-task.
//! 2. **WS-writer-task**: получает из mpsc и шлёт `Message::Binary` в WS.
//! 3. **WS-reader-task** (текущий future): читает из WS и обрабатывает Binary
//!    (запись в PTY writer через spawn_blocking) и Text (JSON control).
//!
//! При закрытии WS / EOF на PTY / любой ошибке — cancel-флаг ставится в true,
//! mpsc-receiver дропается → writer-task завершается, PtyHandle дропается →
//! tmux child kill+wait, reader-task получает EOF и завершается естественно.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex, Notify};

use crate::pty::{spawn_lazygit, spawn_tmux_attach, PtyHandle};

/// Query-параметры для `/ws/attach`.
///
/// `session` — имя tmux-сессии, обязательное.
/// `cols`/`rows` — стартовый размер PTY, дефолт 80×24.
#[derive(Debug, Deserialize)]
pub struct AttachQuery {
    pub session: String,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

/// JSON control-сообщения от клиента (frame `Message::Text`).
///
/// Тэг discriminator-а — поле `type` в lowercase.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Control {
    /// Изменить размер PTY (вызывает SIGWINCH в tmux).
    Resize { cols: u16, rows: u16 },
    /// Сменить сессию: kill старого `tmux attach`, spawn нового на `session`.
    Switch { session: String },
}

/// Query-параметры для `/ws/lazygit`.
///
/// `cwd` — обязательный абсолютный путь к git-репозиторию (или к любой папке
/// внутри него: lazygit сам найдёт ближайший `.git`). `cols`/`rows` — стартовый
/// размер PTY, дефолт 80×24.
#[derive(Debug, Deserialize)]
pub struct LazygitQuery {
    pub cwd: String,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

/// JSON control-сообщения от клиента lazygit-handler'а (frame `Message::Text`).
///
/// Tag `type` сериализуется в `snake_case` — это согласуется с frontend'ом:
/// `{"type":"resize",...}` и `{"type":"switch_cwd","cwd":"..."}`.
///
/// Семантически отделён от [`Control`] (tmux-attach), потому что:
/// - lazygit оперирует *путём к проекту*, а не *именем сессии*;
/// - смешивать одни DTO для двух семантик — путь к багам при будущей эволюции
///   (например, добавление в Lazygit-control'ом флагов вроде `read_only`).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LazygitControl {
    /// Изменить размер PTY: вызывает SIGWINCH в lazygit, lazygit перерисует TUI.
    Resize { cols: u16, rows: u16 },
    /// Переключить рабочий каталог: убить старый lazygit, запустить новый
    /// в `cwd`. Используется при смене активного проекта во фронтенде.
    SwitchCwd { cwd: String },
}

/// Frame с описанием ошибки, который мы посылаем клиенту перед закрытием WS
/// (или при switch-fail). Сериализуется в JSON и отправляется как
/// `Message::Text`.
///
/// Пример: `{"type":"error","message":"spawn failed: lazygit not found in PATH..."}`
#[derive(Debug, Serialize)]
struct ErrorFrame<'a> {
    #[serde(rename = "type")]
    ty: &'static str,
    message: &'a str,
}

impl<'a> ErrorFrame<'a> {
    fn new(message: &'a str) -> Self {
        Self {
            ty: "error",
            message,
        }
    }
}

/// `GET /ws/attach` — upgrade и переход в [`handle_socket`].
///
/// Регистрируется в `main.rs` как `.route("/ws/attach", get(ws::attach))`.
pub async fn attach(ws: WebSocketUpgrade, Query(q): Query<AttachQuery>) -> Response {
    tracing::info!(session = %q.session, cols = q.cols, rows = q.rows, "ws upgrade");
    ws.on_upgrade(move |socket| handle_socket(socket, q))
}

/// Размер чанка чтения из PTY. 8 KiB достаточно для типичных escape-всплесков.
const READ_BUF: usize = 8 * 1024;

/// Глубина mpsc-канала PTY→WS. 64 чанка ≈ 0.5 MiB буферизации — щедро.
const CHAN_DEPTH: usize = 64;

/// Основной обработчик одного WS-соединения.
///
/// Создаёт PTY, spawn-нутые таски, и крутится в WS-receive-loop. По выходу
/// из loop'а явно дропает PtyHandle и cancel-токен — все остальные таски
/// останавливаются естественно (см. модуль-doc).
async fn handle_socket(socket: WebSocket, q: AttachQuery) {
    let session_name = q.session.clone();
    let mut cur_cols = q.cols;
    let mut cur_rows = q.rows;

    // Создаём первый PTY. Ошибка → шлём text-сообщение об ошибке и закрываем.
    let pty_initial = match spawn_tmux_attach(&session_name, cur_cols, cur_rows) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = ?e, session = %session_name, "spawn_tmux_attach failed");
            let mut s = socket;
            let _ = s
                .send(Message::Text(format!(
                    "{{\"type\":\"error\",\"message\":\"spawn failed: {e}\"}}"
                )))
                .await;
            let _ = s.send(Message::Close(None)).await;
            return;
        }
    };

    // Shared state: PtyHandle живёт под Mutex, чтобы control-handler мог
    // делать swap/resize, а WS-writer-task — записывать.
    let pty: Arc<Mutex<Option<PtyHandle>>> = Arc::new(Mutex::new(Some(pty_initial)));

    // cancel — общий флаг останова. Reader-task проверяет его между read'ами.
    let cancel = Arc::new(AtomicBool::new(false));

    // Канал PTY→WS: blocking-reader → mpsc → ws_tx.
    let (pty_to_ws_tx, mut pty_to_ws_rx) = mpsc::channel::<Vec<u8>>(CHAN_DEPTH);

    // pty_eof_notify — сигнал от reader-task о EOF/ошибке PTY (внешний kill
    // tmux-сессии и т.п.). Главный loop делает select! на ws_rx и этот notify,
    // чтобы корректно завершить WS даже если клиент молчит.
    let pty_eof_notify = Arc::new(Notify::new());

    // Запускаем reader-task для первого PTY.
    let reader_handle = spawn_pty_reader(
        &pty,
        pty_to_ws_tx.clone(),
        Arc::clone(&cancel),
        Arc::clone(&pty_eof_notify),
    )
    .await;

    // Split WS на отправляющую и принимающую половины.
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // WS-writer task: получает байты из mpsc и шлёт Binary в WS.
    // ВАЖНО: writer НЕ проверяет общий cancel-флаг — он живёт ровно до тех
    // пор, пока есть senders (т.е. живой reader-task). При switch старый
    // reader дропает свой Sender-clone, но мы держим оригинальный
    // pty_to_ws_tx в handle_socket, поэтому канал остаётся открытым.
    // При финальном teardown handle_socket дропает свой tx, и writer
    // получает None из recv().
    let ws_tx_clone = Arc::clone(&ws_tx);
    let writer_task = tokio::spawn(async move {
        while let Some(chunk) = pty_to_ws_rx.recv().await {
            let mut guard = ws_tx_clone.lock().await;
            if let Err(e) = guard.send(Message::Binary(chunk)).await {
                tracing::debug!(error = ?e, "ws send Binary failed; closing");
                break;
            }
        }
        tracing::debug!("ws-writer task finished");
    });

    // Текущий reader-task в Mutex — чтобы control-handler мог его абортить
    // при switch (а потом запустить новый).
    let reader_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Some(reader_handle)));

    // ============================================================
    // Главный loop — WS receive + PTY EOF notify.
    // ============================================================
    // Флаг: был ли последний reader-task запущен в контексте switch
    // (тогда EOF старого reader'а — ожидаемое явление, не повод закрывать WS).
    // Используем семафорный паттерн: при switch ставим suppress_eof=true,
    // ждём notify (которого может и не быть — switch теперь дропает старый
    // PTY и await'ит handle, а новый reader получит свой собственный notify
    // — далее), затем сбрасываем.
    //
    // Простейшая реализация: каждый раз, когда мы делаем switch, мы создаём
    // *новый* Notify и подписываемся на него; старый отбрасываем. Так EOF
    // старого reader'а уже никого не разбудит.
    let mut pty_eof_notify = pty_eof_notify;
    loop {
        let msg = tokio::select! {
            biased;
            // PTY died (EOF / error) — pretty rare path: client didn't send
            // Close, but tmux exited or got killed externally.
            _ = pty_eof_notify.notified() => {
                tracing::info!(session = %session_name, "pty EOF / death — closing WS");
                break;
            }
            opt = ws_rx.next() => match opt {
                Some(Ok(m)) => m,
                Some(Err(e)) => {
                    tracing::debug!(error = ?e, "ws recv error; tearing down");
                    break;
                }
                None => {
                    tracing::debug!("ws stream ended");
                    break;
                }
            }
        };

        match msg {
            // Binary frame от клиента — это user input в PTY.
            Message::Binary(bytes) => {
                // Пишем в writer под Mutex'ом. write_all — блокирующий, так
                // что выносим в spawn_blocking.
                let pty_arc = Arc::clone(&pty);
                let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                    let mut guard = pty_arc.blocking_lock();
                    if let Some(handle) = guard.as_mut() {
                        if let Some(writer) = handle.writer_mut() {
                            writer.write_all(&bytes)?;
                            writer.flush()?;
                        }
                    }
                    Ok(())
                })
                .await;

                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::debug!(error = ?e, "pty write failed; tearing down");
                        break;
                    }
                    Err(e) => {
                        tracing::debug!(error = ?e, "spawn_blocking pty write join error");
                        break;
                    }
                }
            }
            // Text frame — JSON control.
            Message::Text(text) => {
                match serde_json::from_str::<Control>(&text) {
                    Ok(Control::Resize { cols, rows }) => {
                        cur_cols = cols;
                        cur_rows = rows;
                        let guard = pty.lock().await;
                        if let Some(h) = guard.as_ref() {
                            if let Err(e) = h.resize(cols, rows) {
                                tracing::warn!(error = ?e, cols, rows, "resize failed");
                            } else {
                                tracing::debug!(cols, rows, "pty resized");
                            }
                        }
                    }
                    Ok(Control::Switch { session: new_session }) => {
                        tracing::info!(from = %session_name, to = %new_session, "switch session");

                        // 1) Сигнализируем старому reader-task'у завершиться.
                        cancel.store(true, Ordering::Relaxed);

                        // 2) Дроп старого PtyHandle → kill+wait tmux attach
                        //    → reader (spawn_blocking) получит EOF и
                        //    завершится естественно.
                        {
                            let mut guard = pty.lock().await;
                            *guard = None; // Drop старого handle.
                        }

                        // 3) Дожидаемся завершения старого reader-task'а
                        //    (он сам выйдет по EOF).
                        if let Some(h) = reader_handle.lock().await.take() {
                            let _ = h.await;
                        }

                        // 4) Spawn новый PTY с теми же cur_cols/cur_rows.
                        let new_handle =
                            match spawn_tmux_attach(&new_session, cur_cols, cur_rows) {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::error!(error = ?e, session = %new_session, "switch: spawn failed");
                                    let err_json = format!(
                                        "{{\"type\":\"error\",\"message\":\"switch spawn failed: {e}\"}}"
                                    );
                                    let mut g = ws_tx.lock().await;
                                    let _ = g.send(Message::Text(err_json)).await;
                                    break;
                                }
                            };

                        {
                            let mut guard = pty.lock().await;
                            *guard = Some(new_handle);
                        }

                        // 5) Сбрасываем cancel, выпускаем новый Notify
                        //    (старый reader уже завершился, его notify никому
                        //    не нужен), и запускаем новый reader-task.
                        cancel.store(false, Ordering::Relaxed);
                        pty_eof_notify = Arc::new(Notify::new());
                        let new_reader = spawn_pty_reader(
                            &pty,
                            pty_to_ws_tx.clone(),
                            Arc::clone(&cancel),
                            Arc::clone(&pty_eof_notify),
                        )
                        .await;
                        *reader_handle.lock().await = Some(new_reader);
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, raw = %text, "invalid control JSON; ignored");
                    }
                }
            }
            Message::Close(_) => {
                tracing::debug!("ws close received");
                break;
            }
            // Ping / Pong — axum обрабатывает автоматически.
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    // ============================================================
    // Teardown — гарантированный, в любом случае.
    // ============================================================
    cancel.store(true, Ordering::Relaxed);

    // Drop PtyHandle → kill+wait tmux attach.
    {
        let mut guard = pty.lock().await;
        *guard = None;
    }

    // Дожидаемся reader-task'а (он завершится по EOF).
    if let Some(h) = reader_handle.lock().await.take() {
        let _ = h.await;
    }

    // Закрываем mpsc — writer-task увидит None и завершится.
    drop(pty_to_ws_tx);
    let _ = writer_task.await;

    // Закрываем WS-handshake (best-effort).
    {
        let mut g = ws_tx.lock().await;
        let _ = g.send(Message::Close(None)).await;
        let _ = g.close().await;
    }

    tracing::info!(session = %session_name, "ws session terminated cleanly");
}

/// `GET /ws/lazygit` — upgrade и переход в [`handle_lazygit_socket`].
///
/// Регистрируется в `main.rs` как `.route("/ws/lazygit", get(ws::lazygit_attach))`.
/// Открывает PTY с `lazygit`, работающим в каталоге `cwd`, и пробрасывает stdin/
/// stdout через WebSocket в xterm.js во фронтенде.
pub async fn lazygit_attach(
    ws: WebSocketUpgrade,
    Query(q): Query<LazygitQuery>,
) -> Response {
    tracing::info!(cwd = %q.cwd, cols = q.cols, rows = q.rows, "ws lazygit upgrade");
    ws.on_upgrade(move |socket| handle_lazygit_socket(socket, q))
}

/// Основной обработчик WS-соединения для lazygit.
///
/// Симметричен [`handle_socket`], но использует [`spawn_lazygit`] вместо
/// [`spawn_tmux_attach`]. Логика switch'а другая — `SwitchCwd { cwd }` вместо
/// `Switch { session }`. Reader-task переиспользуется: [`spawn_pty_reader`].
async fn handle_lazygit_socket(socket: WebSocket, q: LazygitQuery) {
    let mut cur_cwd = PathBuf::from(&q.cwd);
    let mut cur_cols = q.cols;
    let mut cur_rows = q.rows;

    // Создаём первый PTY с lazygit. При ошибке — Text frame с типом "error",
    // потом Close. Это контракт с фронтендом: error-баннер в git-tab.
    let pty_initial = match spawn_lazygit(&cur_cwd, cur_cols, cur_rows) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = ?e, cwd = ?cur_cwd, "spawn_lazygit failed");
            let mut s = socket;
            let msg = format!("spawn failed: {e}");
            let payload = serde_json::to_string(&ErrorFrame::new(&msg))
                .unwrap_or_else(|_| {
                    "{\"type\":\"error\",\"message\":\"spawn failed\"}".to_string()
                });
            let _ = s.send(Message::Text(payload)).await;
            let _ = s.send(Message::Close(None)).await;
            return;
        }
    };

    let pty: Arc<Mutex<Option<PtyHandle>>> = Arc::new(Mutex::new(Some(pty_initial)));
    let cancel = Arc::new(AtomicBool::new(false));
    let (pty_to_ws_tx, mut pty_to_ws_rx) = mpsc::channel::<Vec<u8>>(CHAN_DEPTH);
    let pty_eof_notify = Arc::new(Notify::new());

    let reader_handle = spawn_pty_reader(
        &pty,
        pty_to_ws_tx.clone(),
        Arc::clone(&cancel),
        Arc::clone(&pty_eof_notify),
    )
    .await;

    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    let ws_tx_clone = Arc::clone(&ws_tx);
    let writer_task = tokio::spawn(async move {
        while let Some(chunk) = pty_to_ws_rx.recv().await {
            let mut guard = ws_tx_clone.lock().await;
            if let Err(e) = guard.send(Message::Binary(chunk)).await {
                tracing::debug!(error = ?e, "lazygit ws send Binary failed; closing");
                break;
            }
        }
        tracing::debug!("lazygit ws-writer task finished");
    });

    let reader_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Some(reader_handle)));

    let mut pty_eof_notify = pty_eof_notify;
    loop {
        let msg = tokio::select! {
            biased;
            _ = pty_eof_notify.notified() => {
                tracing::info!(cwd = ?cur_cwd, "lazygit pty EOF / death — closing WS");
                break;
            }
            opt = ws_rx.next() => match opt {
                Some(Ok(m)) => m,
                Some(Err(e)) => {
                    tracing::debug!(error = ?e, "lazygit ws recv error; tearing down");
                    break;
                }
                None => {
                    tracing::debug!("lazygit ws stream ended");
                    break;
                }
            }
        };

        match msg {
            Message::Binary(bytes) => {
                let pty_arc = Arc::clone(&pty);
                let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                    let mut guard = pty_arc.blocking_lock();
                    if let Some(handle) = guard.as_mut() {
                        if let Some(writer) = handle.writer_mut() {
                            writer.write_all(&bytes)?;
                            writer.flush()?;
                        }
                    }
                    Ok(())
                })
                .await;

                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::debug!(error = ?e, "lazygit pty write failed; tearing down");
                        break;
                    }
                    Err(e) => {
                        tracing::debug!(error = ?e, "lazygit spawn_blocking pty write join error");
                        break;
                    }
                }
            }
            Message::Text(text) => {
                match serde_json::from_str::<LazygitControl>(&text) {
                    Ok(LazygitControl::Resize { cols, rows }) => {
                        cur_cols = cols;
                        cur_rows = rows;
                        let guard = pty.lock().await;
                        if let Some(h) = guard.as_ref() {
                            if let Err(e) = h.resize(cols, rows) {
                                tracing::warn!(error = ?e, cols, rows, "lazygit resize failed");
                            } else {
                                tracing::debug!(cols, rows, "lazygit pty resized");
                            }
                        }
                    }
                    Ok(LazygitControl::SwitchCwd { cwd: new_cwd }) => {
                        let new_path = PathBuf::from(&new_cwd);
                        tracing::info!(from = ?cur_cwd, to = ?new_path, "lazygit switch cwd");

                        // 1) Сигнал старому reader'у завершиться.
                        cancel.store(true, Ordering::Relaxed);

                        // 2) Дроп старого PtyHandle → kill+wait lazygit.
                        {
                            let mut guard = pty.lock().await;
                            *guard = None;
                        }

                        // 3) Дожидаемся завершения старого reader-task'а.
                        if let Some(h) = reader_handle.lock().await.take() {
                            let _ = h.await;
                        }

                        // 4) Spawn новый lazygit в новом cwd.
                        let new_handle = match spawn_lazygit(&new_path, cur_cols, cur_rows) {
                            Ok(h) => h,
                            Err(e) => {
                                tracing::error!(error = ?e, cwd = ?new_path, "switch_cwd: spawn_lazygit failed");
                                let msg = format!("switch spawn failed: {e}");
                                let payload = serde_json::to_string(&ErrorFrame::new(&msg))
                                    .unwrap_or_else(|_| {
                                        "{\"type\":\"error\",\"message\":\"switch spawn failed\"}"
                                            .to_string()
                                    });
                                let mut g = ws_tx.lock().await;
                                let _ = g.send(Message::Text(payload)).await;
                                break;
                            }
                        };

                        cur_cwd = new_path;

                        {
                            let mut guard = pty.lock().await;
                            *guard = Some(new_handle);
                        }

                        // 5) Сбрасываем cancel, выпускаем новый Notify, рестарт reader.
                        cancel.store(false, Ordering::Relaxed);
                        pty_eof_notify = Arc::new(Notify::new());
                        let new_reader = spawn_pty_reader(
                            &pty,
                            pty_to_ws_tx.clone(),
                            Arc::clone(&cancel),
                            Arc::clone(&pty_eof_notify),
                        )
                        .await;
                        *reader_handle.lock().await = Some(new_reader);
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, raw = %text, "invalid lazygit control JSON; ignored");
                    }
                }
            }
            Message::Close(_) => {
                tracing::debug!("lazygit ws close received");
                break;
            }
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    // Teardown.
    cancel.store(true, Ordering::Relaxed);
    {
        let mut guard = pty.lock().await;
        *guard = None;
    }
    if let Some(h) = reader_handle.lock().await.take() {
        let _ = h.await;
    }
    drop(pty_to_ws_tx);
    let _ = writer_task.await;
    {
        let mut g = ws_tx.lock().await;
        let _ = g.send(Message::Close(None)).await;
        let _ = g.close().await;
    }
    tracing::info!(cwd = ?cur_cwd, "lazygit ws session terminated cleanly");
}

/// Запускает spawn_blocking-задачу, которая синхронно читает PTY reader
/// и шлёт чанки в mpsc. Завершается при EOF / ошибке / cancel=true.
///
/// Возвращает `JoinHandle<()>` — caller хранит и при teardown await'ит.
async fn spawn_pty_reader(
    pty: &Arc<Mutex<Option<PtyHandle>>>,
    tx: mpsc::Sender<Vec<u8>>,
    cancel: Arc<AtomicBool>,
    eof_notify: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    // Берём reader из PtyHandle (one-shot).
    let reader: Option<Box<dyn Read + Send>> = {
        let mut guard = pty.lock().await;
        guard.as_mut().and_then(|h| h.take_reader())
    };

    let Some(mut reader) = reader else {
        tracing::error!("spawn_pty_reader: reader was None — pty already dropped or taken");
        // Сразу нотифай — handle_socket иначе зависнет.
        eof_notify.notify_one();
        // Возвращаем уже завершённый handle.
        return tokio::spawn(async {});
    };

    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; READ_BUF];
        // Если task завершился по EOF / read-error (а не по cancel из switch
        // или teardown) — это означает, что PTY (= tmux attach) умер
        // неожиданно. В этом случае надо разбудить главный loop, чтобы он
        // закрыл WS. cancel=true означает legitimate teardown / switch и
        // notify не нужен.
        let mut natural_death = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => {
                    tracing::debug!("pty reader EOF");
                    natural_death = true;
                    break;
                }
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    // blocking_send: нам нужен sync API из spawn_blocking.
                    if tx.blocking_send(chunk).is_err() {
                        // Канал закрыт — ws-writer-task ушёл.
                        tracing::debug!("pty→ws channel closed");
                        break;
                    }
                }
                Err(e) => {
                    // ErrorKind::Interrupted можно игнорировать, остальное —
                    // реальная ошибка / закрытый PTY.
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    tracing::debug!(error = ?e, "pty reader error");
                    natural_death = true;
                    break;
                }
            }
        }
        if natural_death {
            // Дополнительно проверяем, что cancel НЕ установлен — не хотим
            // будить handle_socket в случае switch (там cancel=true).
            if !cancel.load(Ordering::Relaxed) {
                eof_notify.notify_one();
            }
        }
        tracing::debug!("pty-reader task finished");
    })
}
