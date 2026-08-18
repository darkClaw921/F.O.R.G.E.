//! `forge-cdp` — тонкий клиент Chrome DevTools Protocol для плагина `forge-meet`.
//!
//! Крейт делает ровно то, что нужно боту видеовстреч, и ничего сверх:
//! запускает СВОЙ экземпляр Chrome, подключается к нему по WebSocket,
//! раздаёт команды по flat-сессиям, кладёт наш скрипт в страницу ДО навигации,
//! принимает из страницы данные через биндинг, умеет кликать, печатать,
//! двигать окно, снимать скриншот и убирать за собой осиротевшие процессы.
//!
//! # Три решения, которые определяют устройство крейта
//!
//! **1. Свой клиент, а не chromiumoxide.** `chromiumoxide` тянет
//! `async-tungstenite`, то есть второй ws-стек в дерево, где уже есть
//! `tokio-tungstenite 0.24`; гейт `cargo tree -d` краснеет немедленно.
//! Нужно около двадцати методов из стабильного ядра протокола — это дешевле
//! второго стека и генерируемых типов. Ставка застрахована трейтом
//! [`CdpTransport`]: подмена реализации — работа внутри этого крейта.
//!
//! **2. Только свой Chrome.** С версии 136 `--remote-debugging-port`
//! ИГНОРИРУЕТСЯ, если не задан нестандартный `--user-data-dir`. Подключиться к
//! рабочему браузеру пользователя невозможно в принципе — не «неудобно», а
//! невозможно. Отсюда весь [`launch`]: свой профиль, свой порт из файла
//! `DevToolsActivePort`, своя группа процессов, свой файл тишины для фейкового
//! микрофона.
//!
//! **3. Инжект строго ДО навигации.** Живая проба Яндекс Телемоста (фаза 0)
//! прямым тестом показала: CSP страницы блокирует инлайн-скрипт
//! (`cspBlocksInlineScript: true`). Значит `Page.setBypassCSP` — не страховка
//! «на всякий случай», а несущая часть, и порядок команд
//! (`setBypassCSP` → `grantPermissions` → `addBinding` →
//! `addScriptToEvaluateOnNewDocument` → `setAutoAttach` → `navigate`) —
//! не стиль, а условие работоспособности. Инжект после навигации бесполезен:
//! `RTCPeerConnection` сайта уже создан, патчить нечего.
//!
//! # Используемые методы протокола
//!
//! Это исчерпывающий список того, что крейт реально шлёт и слушает, — то
//! есть ровно то, что придётся поддержать при гипотетической подмене
//! реализации транспорта. Двадцать одна команда и два события, все из
//! стабильного ядра протокола:
//!
//! | Домен | Команды | События |
//! |---|---|---|
//! | `Target` | `createTarget`, `attachToTarget`, `setAutoAttach`, `closeTarget` | `attachedToTarget` |
//! | `Page`   | `enable`, `navigate`, `setBypassCSP`, `addScriptToEvaluateOnNewDocument`, `captureScreenshot`, `bringToFront` | — |
//! | `Runtime`| `enable`, `evaluate`, `addBinding`, `runIfWaitingForDebugger` | `bindingCalled` |
//! | `Input`  | `dispatchMouseEvent`, `insertText` | — |
//! | `Browser`| `grantPermissions`, `getWindowForTarget`, `setWindowBounds`, `getWindowBounds` | — |
//! | `Log`    | `enable` | — |
//!
//! Список проверяется командой
//! `grep -rhoE '"(Target|Page|Runtime|Input|Browser|Log)\.[A-Za-z]+"' plugins/forge-cdp/src/*.rs | sort -u`.
//!
//! # Порядок работы
//!
//! `launch` → `WsTransport::connect` по адресу из `DevToolsActivePort` →
//! `CdpBrowser::new_page` (`createTarget` + `attachToTarget{flatten:true}` +
//! `enable` доменов) → `CdpPage::prepare_and_navigate` (инжект, биндинг,
//! автоаттач, навигация) → клики/ввод/скриншот → `sweep_orphans` на старте
//! следующего запуска devforge.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod inject;
pub mod launch;
pub mod orphans;
pub mod page;
pub mod protocol;
pub mod session;
pub mod transport;
pub mod ws;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use inject::{
    build_inject_source, parse_binding_payload, random_binding_name, BindingMessage,
    BindingReceiver, InjectConfig, InjectSetup, BINDING_META_TYPE,
};
pub use page::{
    origin_of, CdpPage, ElementCenter, PageSetup, ScreenshotOptions, WindowBounds, WindowPlacement,
};
pub use session::{AutoAttachConfig, AutoAttachPump, CdpBrowser, CdpSession, SessionSetup};
pub use ws::WsTransport;

pub use launch::{
    build_args, ensure_silence_wav, find_chrome, launch, ChromeProcess, LaunchConfig, WindowMode,
    FORBIDDEN_ARGS,
};
pub use orphans::{
    read_pid_file, sweep_orphans, write_pid_file, PidRecord, SweepConfig, SweepOutcome, SweepReport,
};
pub use protocol::{
    CdpCommand, CdpError, CdpEvent, CdpMessage, CdpResponse, EventFilter, ProtocolError,
    SessionFilter, SessionId,
};
pub use transport::{wait_for_event, CdpTransport, SharedTransport, TransportConfig};
