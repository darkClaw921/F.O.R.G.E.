//! Страница встречи: подготовка цели, навигация, ввод, окно, скриншот.
//!
//! # Порядок команд старта — не стиль, а условие работоспособности
//!
//! ```text
//! Page.setBypassCSP
//!   → Browser.grantPermissions
//!     → Runtime.addBinding
//!       → Page.addScriptToEvaluateOnNewDocument
//!         → Target.setAutoAttach{waitForDebuggerOnStart:true}
//!           → Page.navigate
//! ```
//!
//! Каждая стрелка обязательна:
//! * CSP снимается ДО всего — иначе инжект просто не выполнится (измерено на
//!   Телемосте: `cspBlocksInlineScript: true`);
//! * биндинг привязывается ДО скрипта — окно, в котором кадры копятся в
//!   кольцевом буфере, должно быть как можно короче;
//! * автоаттач включается ДО навигации — иначе OOPIF-фреймы, созданные при
//!   загрузке, останутся без инжекта;
//! * навигация ПОСЛЕДНЯЯ — после загрузки патчить `RTCPeerConnection` уже
//!   поздно, он создан.
//!
//! Нарушение порядка ломает всё МОЛЧА: страница откроется, кнопки нажмутся,
//! и только звука не будет.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::inject::{BindingReceiver, InjectConfig, InjectSetup};
use crate::launch::WindowMode;
use crate::protocol::CdpError;
use crate::session::{AutoAttachConfig, AutoAttachPump, CdpBrowser, CdpSession};

/// Параметры подготовки страницы.
#[derive(Debug, Clone)]
pub struct PageSetup {
    /// Инжект: имя биндинга, кольцевой буфер, наш скрипт.
    pub inject: InjectConfig,
    /// Звать ли `Page.setBypassCSP`.
    pub bypass_csp: bool,
    /// Разрешения, выдаваемые источнику встречи.
    pub permissions: Vec<String>,
    /// Настройки автоаттача к OOPIF.
    pub auto_attach: AutoAttachConfig,
    /// В каком виде окно живёт, пока бот работает САМ.
    ///
    /// Применяется здесь, а не при запуске браузера, по простой причине: до
    /// создания вкладки окна нет вовсе (`--no-startup-window`), и свернуть
    /// нечего. Значение запоминается страницей и определяет, куда окно
    /// возвращается после «покажи, что видит бот»
    /// ([`CdpPage::set_window_visible`]).
    pub window_mode: WindowMode,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            inject: InjectConfig::default(),
            bypass_csp: true,
            // Микрофон и камера. Камера нужна не для съёмки, а потому что
            // часть площадок запрашивает оба устройства сразу и падает на
            // отказе в одном из них.
            permissions: vec!["audioCapture".to_string(), "videoCapture".to_string()],
            auto_attach: AutoAttachConfig::default(),
            window_mode: WindowMode::default(),
        }
    }
}

/// Положение и размер окна браузера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowBounds {
    /// Отступ слева.
    pub left: i32,
    /// Отступ сверху.
    pub top: i32,
    /// Ширина.
    pub width: u32,
    /// Высота.
    pub height: u32,
}

/// Параметры скриншота.
#[derive(Debug, Clone)]
pub struct ScreenshotOptions {
    /// Формат: `png` или `jpeg`.
    pub format: String,
    /// Качество для `jpeg`, 0..100.
    pub quality: Option<u8>,
    /// Снимать с поверхности окна.
    ///
    /// ИЗМЕРЕНО (macOS 15, Chrome 151) и это противоположно ожиданию:
    /// для НЕАКТИВНОЙ вкладки (`Target.createTarget{background:true}` при
    /// `--no-startup-window`, то есть в штатном режиме бота)
    /// `fromSurface: false` ОТКАЗЫВАЕТ — `-32000 Unable to capture
    /// screenshot`, — а `fromSurface: true` работает и отдаёт настоящий
    /// кадр. Рендерерный путь требует активного вида, поверхностный — нет.
    /// `Emulation.setDeviceMetricsOverride` положение не спасает: с ним
    /// `fromSurface: false` отказывает так же.
    ///
    /// Для активной вкладки работают оба, поэтому `true` — единственное
    /// значение, годное в обоих случаях, и оно же дефолт. Поле оставлено
    /// на случай, когда понадобится кадр именно из рендерера.
    pub from_surface: bool,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            format: "png".to_string(),
            quality: None,
            from_surface: true,
        }
    }
}

/// Где держать окно в каждом из режимов.
///
/// Оба положения — данные, а не константы, и вот почему это важно.
/// ИЗМЕРЕНО на macOS 15 (Chrome 151): оконный сервер ПОДРЕЗАЕТ координаты и
/// не даёт увести окно полностью. Запрос `left: -4000, top: 0` для окна
/// 1280×800 превращается в фактические `left: -930, top: 30` — на экране
/// остаётся полоса около 350 пикселей, а `top` упирается в строку меню.
/// То же происходит и с `--window-position` при запуске.
///
/// Следствие для фаз 3–4: «за экраном» на macOS означает «почти за экраном».
/// Если полоса поверх работы пользователя окажется неприемлемой, вариантов
/// два — `windowState: "minimized"` (скриншот через `fromSurface: false`
/// продолжает работать) либо `WindowMode::Headless` со всеми его минусами.
/// Значения оставлены полем именно ради такой подстройки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPlacement {
    /// Куда ставить окно, когда его показывают человеку.
    pub visible: WindowBounds,
    /// Куда убирать окно, когда бот работает сам.
    pub offscreen: WindowBounds,
}

impl Default for WindowPlacement {
    fn default() -> Self {
        Self {
            visible: WindowBounds {
                left: 80,
                top: 80,
                width: 1280,
                height: 800,
            },
            offscreen: WindowBounds {
                left: -4000,
                top: 0,
                width: 1280,
                height: 800,
            },
        }
    }
}

/// Страница встречи.
pub struct CdpPage {
    browser: CdpBrowser,
    session: CdpSession,
    binding_name: String,
    placement: WindowPlacement,
    /// Куда окно возвращается, когда его перестают показывать человеку.
    hidden_mode: WindowMode,
    // Насос живёт столько же, сколько страница: его `Drop` останавливает
    // фоновую задачу, поэтому терять хэндл нельзя.
    pump: Option<AutoAttachPump>,
}

impl CdpPage {
    /// Оборачивает уже открытую и подключённую цель.
    pub fn new(browser: CdpBrowser, session: CdpSession) -> Self {
        Self {
            browser,
            session,
            binding_name: String::new(),
            placement: WindowPlacement::default(),
            hidden_mode: WindowMode::default(),
            pump: None,
        }
    }

    /// Задаёт режим, в котором окно живёт без человека.
    pub fn set_hidden_mode(&mut self, mode: WindowMode) {
        self.hidden_mode = mode;
    }

    /// Режим, в котором окно живёт без человека.
    pub fn hidden_mode(&self) -> WindowMode {
        self.hidden_mode
    }

    /// Задаёт положения окна для режимов «видимо» и «за экраном».
    pub fn set_placement(&mut self, placement: WindowPlacement) {
        self.placement = placement;
    }

    /// Текущие настройки положения окна.
    pub fn placement(&self) -> WindowPlacement {
        self.placement
    }

    /// Открывает новую вкладку `about:blank` и подключается к ней.
    ///
    /// Именно `about:blank`, а не адрес встречи: настоящая навигация идёт
    /// только после инжекта.
    pub async fn open(browser: &CdpBrowser) -> Result<Self, CdpError> {
        let session = browser.open_target("about:blank").await?;
        Ok(Self::new(browser.clone(), session))
    }

    /// Сессия страницы.
    pub fn session(&self) -> &CdpSession {
        &self.session
    }

    /// Браузер, которому принадлежит страница.
    pub fn browser(&self) -> &CdpBrowser {
        &self.browser
    }

    /// Имя биндинга этой встречи (пусто до `prepare_and_navigate`).
    pub fn binding_name(&self) -> &str {
        &self.binding_name
    }

    /// Приёмник сообщений из страницы.
    ///
    /// Подписываться нужно ДО навигации: первые кадры приходят сразу после
    /// загрузки, и окно между `navigate` и `subscribe` — это потерянные
    /// данные.
    pub fn binding_events(&self) -> BindingReceiver {
        BindingReceiver::new(self.session.subscribe(), self.binding_name.clone())
    }

    /// Полный старт: снятие CSP, разрешения, биндинг, скрипт, автоаттач,
    /// навигация — строго в этом порядке.
    pub async fn prepare_and_navigate(
        &mut self,
        url: &str,
        setup: &PageSetup,
    ) -> Result<(), CdpError> {
        self.binding_name = setup.inject.binding_name.clone();
        self.hidden_mode = setup.window_mode;

        // 0. Режим окна. Именно ЗДЕСЬ, а не при запуске браузера: до создания
        //    вкладки окна нет вовсе (`--no-startup-window`), а свернуть
        //    несуществующее окно нельзя. Отказ не срывает встречу — окно
        //    останется видимым, и это неприятно, но не смертельно.
        if setup.window_mode == WindowMode::Minimized {
            if let Err(e) = self.minimize().await {
                tracing::warn!(error = %e, "forge-cdp: окно не свернулось, бот работает с видимым окном");
            }
        }

        // 1. CSP — до всего остального.
        if setup.bypass_csp {
            self.session
                .call("Page.setBypassCSP", json!({"enabled": true}))
                .await?;
        }

        // 2. Разрешения источнику встречи. Второй, независимый от
        //    --use-fake-ui-for-media-stream барьер: потерянный флаг
        //    командной строки означал бы встречу без звука.
        let perms: Vec<&str> = setup.permissions.iter().map(String::as_str).collect();
        if !perms.is_empty() {
            self.browser
                .grant_permissions(origin_of(url).as_deref(), &perms)
                .await?;
        }

        // 3-4. Биндинг и скрипт. CSP уже снят, поэтому здесь bypass выключен —
        //      иначе команда ушла бы дважды и порядок в тесте порядка поехал.
        let child_setup = InjectSetup {
            config: setup.inject.clone(),
            bypass_csp: setup.bypass_csp,
        };
        let main_setup = InjectSetup {
            config: setup.inject.clone(),
            bypass_csp: false,
        };
        main_setup.apply(&self.session).await?;

        // 5. Насос ДО setAutoAttach: он должен уже слушать, когда прилетит
        //    первое `attachedToTarget`, иначе фрейм останется замершим.
        let mut auto_cfg = setup.auto_attach.clone();
        auto_cfg
            .ignore_sessions
            .push(self.session.session_id().to_string());
        self.pump = Some(
            self.browser
                .spawn_auto_attach(Some(Arc::new(child_setup)), auto_cfg),
        );
        self.session
            .set_auto_attach(setup.auto_attach.wait_for_debugger)
            .await?;

        // 6. И только теперь навигация.
        self.navigate(url).await
    }

    /// Навигация с проверкой ошибки загрузки.
    ///
    /// `Page.navigate` возвращает `errorText` вместо ошибки протокола — не
    /// прочитать его значит принять «сайт недоступен» за успешный вход.
    pub async fn navigate(&self, url: &str) -> Result<(), CdpError> {
        let res = self.session.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = res.get("errorText").and_then(Value::as_str) {
            return Err(CdpError::Launch(format!("навигация на {url}: {err}")));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Ввод, окно, скриншот
// ---------------------------------------------------------------------------

/// Центр и размеры найденного элемента в координатах вьюпорта (CSS-пиксели).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElementCenter {
    /// X центра.
    pub x: f64,
    /// Y центра.
    pub y: f64,
    /// Ширина бокса.
    pub width: f64,
    /// Высота бокса.
    pub height: f64,
}

impl CdpPage {
    /// Вычисляет центр элемента по JS-выражению.
    ///
    /// Координаты берутся из `getBoundingClientRect`, а НЕ со скриншота: на
    /// живой пробе фазы 0 клик по координатам, снятым со скриншота,
    /// промахнулся — скриншот отмасштабирован относительно вьюпорта, а
    /// `Input.dispatchMouseEvent` ждёт CSS-пиксели вьюпорта. Скриншот остаётся
    /// инструментом диагностики и ассистированного входа, но не источником
    /// координат.
    ///
    /// Через `Runtime.evaluate`, а не `DOM.getBoxModel`: не нужно включать
    /// домен `DOM` и держать `nodeId`, а выражение может само пройти сквозь
    /// shadow DOM — на площадках встреч это встречается.
    pub async fn element_center(&self, js_expr: &str) -> Result<ElementCenter, CdpError> {
        let expression = format!(
            r#"(function () {{
  var el = ({js_expr});
  if (!el) return null;
  if (el.scrollIntoViewIfNeeded) {{ el.scrollIntoViewIfNeeded(); }}
  else if (el.scrollIntoView) {{ el.scrollIntoView({{block: "center", inline: "center"}}); }}
  var r = el.getBoundingClientRect();
  return JSON.stringify({{x: r.left + r.width / 2, y: r.top + r.height / 2, w: r.width, h: r.height}});
}})()"#
        );
        let res = self
            .session
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": false}),
            )
            .await?;
        check_js_exception(&res)?;
        let raw = res["result"]["value"].as_str().ok_or_else(|| {
            CdpError::ElementNotFound(crate::protocol::truncate_for_log(js_expr, 120))
        })?;
        let v: Value = serde_json::from_str(raw)
            .map_err(|e| CdpError::UnexpectedShape(format!("бокс элемента не разобран: {e}")))?;
        let center = ElementCenter {
            x: v["x"].as_f64().unwrap_or(0.0),
            y: v["y"].as_f64().unwrap_or(0.0),
            width: v["w"].as_f64().unwrap_or(0.0),
            height: v["h"].as_f64().unwrap_or(0.0),
        };
        if center.width <= 0.0 || center.height <= 0.0 {
            return Err(CdpError::ElementNotFound(format!(
                "{}: элемент найден, но его бокс нулевой (скрыт)",
                crate::protocol::truncate_for_log(js_expr, 120)
            )));
        }
        Ok(center)
    }

    /// Кликает по точке вьюпорта.
    ///
    /// Только `Input.dispatchMouseEvent`, никогда `el.click()`: Zoom и Teams
    /// местами проверяют `isTrusted`, а синтетический клик из JS его не имеет.
    /// Перед нажатием шлётся `mouseMoved` — часть интерфейсов показывает
    /// кнопку только под курсором, и клик без наведения промахнётся по
    /// невидимому элементу.
    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), CdpError> {
        let base = json!({"x": x, "y": y, "button": "left", "clickCount": 1});
        let mut moved = base.clone();
        moved["type"] = json!("mouseMoved");
        moved["button"] = json!("none");
        moved["buttons"] = json!(0);
        self.session.call("Input.dispatchMouseEvent", moved).await?;

        let mut pressed = base.clone();
        pressed["type"] = json!("mousePressed");
        pressed["buttons"] = json!(1);
        self.session
            .call("Input.dispatchMouseEvent", pressed)
            .await?;

        let mut released = base;
        released["type"] = json!("mouseReleased");
        released["buttons"] = json!(0);
        self.session
            .call("Input.dispatchMouseEvent", released)
            .await?;
        Ok(())
    }

    /// Кликает по элементу, найденному JS-выражением.
    ///
    /// Выражение, а не только селектор: на Телемосте кнопку «Продолжить в
    /// браузере» надёжнее искать по тексту, чем по классу с хешем сборки.
    pub async fn click_expression(&self, js_expr: &str) -> Result<ElementCenter, CdpError> {
        let center = self.element_center(js_expr).await?;
        self.click_at(center.x, center.y).await?;
        Ok(center)
    }

    /// Кликает по элементу селектора.
    pub async fn click_selector(&self, selector: &str) -> Result<ElementCenter, CdpError> {
        let expr = format!(
            "document.querySelector({})",
            serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into())
        );
        self.click_expression(&expr).await
    }

    /// Наводит фокус на элемент и печатает текст.
    ///
    /// Только `Input.insertText`, никогда `el.value = x`: React хранит
    /// последнее известное значение в трекере на самом узле и на прямое
    /// присваивание не реагирует вовсе — событие `input` не возникает, и
    /// состояние компонента остаётся прежним. `Input.insertText` порождает
    /// НАСТОЯЩЕЕ событие ввода, то самое, на которое React и рассчитывает.
    pub async fn type_text(&self, selector: &str, text: &str) -> Result<(), CdpError> {
        let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into());
        let expression = format!(
            r#"(function () {{
  var el = document.querySelector({sel_json});
  if (!el) return "missing";
  if (el.focus) el.focus();
  return "ok";
}})()"#
        );
        let res = self
            .session
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true}),
            )
            .await?;
        check_js_exception(&res)?;
        if res["result"]["value"].as_str() != Some("ok") {
            return Err(CdpError::ElementNotFound(selector.to_string()));
        }
        self.session
            .call("Input.insertText", json!({ "text": text }))
            .await?;
        Ok(())
    }

    /// Идентификатор окна браузера, в котором живёт цель.
    pub async fn window_id(&self) -> Result<i64, CdpError> {
        let mut params = json!({});
        if let Some(target_id) = self.session.target_id() {
            params["targetId"] = json!(target_id);
        }
        let res = self
            .session
            .call("Browser.getWindowForTarget", params)
            .await?;
        res["windowId"].as_i64().ok_or_else(|| {
            CdpError::UnexpectedShape(format!("Browser.getWindowForTarget без windowId: {res}"))
        })
    }

    /// Текущие границы окна.
    pub async fn window_bounds(&self) -> Result<WindowBounds, CdpError> {
        let window_id = self.window_id().await?;
        let res = self
            .session
            .call("Browser.getWindowBounds", json!({ "windowId": window_id }))
            .await?;
        let b = &res["bounds"];
        Ok(WindowBounds {
            left: b["left"].as_i64().unwrap_or(0) as i32,
            top: b["top"].as_i64().unwrap_or(0) as i32,
            width: b["width"].as_u64().unwrap_or(0) as u32,
            height: b["height"].as_u64().unwrap_or(0) as u32,
        })
    }

    /// Текущее состояние окна: `normal`, `minimized`, `maximized`,
    /// `fullscreen`.
    ///
    /// Отдельно от [`CdpPage::window_bounds`], потому что состояние —
    /// единственное, по чему видно, свёрнуто ли окно на самом деле:
    /// координаты свёрнутое окно продолжает отдавать те же, что и до
    /// сворачивания.
    pub async fn window_state(&self) -> Result<String, CdpError> {
        let window_id = self.window_id().await?;
        let res = self
            .session
            .call("Browser.getWindowBounds", json!({ "windowId": window_id }))
            .await?;
        Ok(res["bounds"]["windowState"]
            .as_str()
            .unwrap_or("normal")
            .to_string())
    }

    /// Переносит окно.
    ///
    /// `windowState: "normal"` обязателен: свёрнутое или развёрнутое окно
    /// не принимает координаты, и перенос молча ничего не сделает.
    pub async fn set_window_bounds(&self, bounds: WindowBounds) -> Result<(), CdpError> {
        let window_id = self.window_id().await?;
        self.session
            .call(
                "Browser.setWindowBounds",
                json!({
                    "windowId": window_id,
                    "bounds": {
                        "left": bounds.left,
                        "top": bounds.top,
                        "width": bounds.width,
                        "height": bounds.height,
                        "windowState": "normal"
                    }
                }),
            )
            .await
            .map(|_| ())
    }

    /// Меняет СОСТОЯНИЕ окна: `normal`, `minimized`, `maximized`,
    /// `fullscreen`.
    ///
    /// Отдельно от [`CdpPage::set_window_bounds`] потому, что состояние и
    /// координаты в протоколе несовместимы: `Browser.setWindowBounds` с
    /// `windowState`, отличным от `normal`, ОТКАЗЫВАЕТСЯ принимать
    /// `left`/`top`/`width`/`height` — их надо слать отдельным вызовом.
    ///
    /// # Зачем нужно `minimized`
    ///
    /// ИЗМЕРЕНО в Ф1 (macOS 15, Chrome 151): «за экраном» на macOS означает
    /// «почти за экраном» — запрос `left: -4000` оконный сервер подрезает до
    /// фактических `left: -930`, и на экране остаётся полоса около 350
    /// пикселей. Для часовой встречи это чужое окно поверх работы
    /// пользователя.
    ///
    /// Свёрнутое окно исчезает с экрана полностью. Плата за это — окно
    /// становится occluded, а Chrome троттлит таймеры рендерера occluded-окна
    /// до секунды. Плагин к этому готов структурно (VAD живёт в
    /// `AudioWorkletProcessor`, то есть в аудиопотоке, а не на таймерах), но
    /// величина троттлинга — вопрос измерения, а не рассуждения; стенд для
    /// него — `plugins/meet/tests/window_mode.rs`.
    pub async fn set_window_state(&self, state: &str) -> Result<(), CdpError> {
        let window_id = self.window_id().await?;
        self.session
            .call(
                "Browser.setWindowBounds",
                json!({
                    "windowId": window_id,
                    "bounds": { "windowState": state }
                }),
            )
            .await
            .map(|_| ())
    }

    /// Сворачивает окно. Оно исчезает с экрана целиком.
    pub async fn minimize(&self) -> Result<(), CdpError> {
        self.set_window_state("minimized").await
    }

    /// Разворачивает свёрнутое окно обратно в нормальное состояние.
    ///
    /// Фокус НЕ отбирается: `Page.bringToFront` здесь не шлётся. Вывод окна
    /// вперёд — отдельное действие и только по просьбе человека
    /// ([`CdpPage::set_window_visible`]).
    pub async fn restore(&self) -> Result<(), CdpError> {
        self.set_window_state("normal").await
    }

    /// Показывает окно человеку или убирает его за экран.
    ///
    /// Возвращает ФАКТИЧЕСКИЕ границы после переноса: оконный сервер вправе
    /// подрезать координаты, и разница между «попросили» и «получилось» —
    /// это то, что нужно знать ассистированному входу, а не догадываться.
    ///
    /// `Page.bringToFront` шлётся ТОЛЬКО при показе. Это единственное место
    /// крейта, которое имеет право отобрать фокус у пользователя, и оно
    /// вызывается человеком: «покажи, что там у бота» — просьба вывести окно
    /// вперёд. Всё остальное (старт браузера, создание вкладки, работа бота)
    /// обязано идти незаметно.
    pub async fn set_window_visible(&self, visible: bool) -> Result<WindowBounds, CdpError> {
        // ВОЗВРАТ — В ТОТ ЖЕ РЕЖИМ, ИЗ КОТОРОГО ПОКАЗАЛИ. Свёрнутый бот,
        // которого показали человеку и «убрали» переносом за экран, оказался
        // бы на экране: на macOS координаты подрезаются, и «за экраном»
        // означает «почти за экраном» (измерено дважды: Ф1 дала left=-930,
        // повторное измерение Ф3 — left=310, то есть окно целиком на виду).
        if !visible && self.hidden_mode == WindowMode::Minimized {
            self.minimize().await?;
            return self.window_bounds().await;
        }
        // Свёрнутое окно координат не принимает, поэтому сначала возвращаем
        // его в нормальное состояние. Ошибку глотаем: окно могло и не быть
        // свёрнутым, а отказ на этом шаге не повод не показать окно.
        if visible {
            let _ = self.restore().await;
        }
        let target = if visible {
            self.placement.visible
        } else {
            self.placement.offscreen
        };
        self.set_window_bounds(target).await?;
        if visible {
            self.session.call("Page.bringToFront", json!({})).await?;
        }
        let actual = self.window_bounds().await?;
        if actual != target {
            tracing::debug!(
                requested = ?target,
                actual = ?actual,
                "forge-cdp: оконный сервер подрезал координаты окна"
            );
        }
        Ok(actual)
    }

    /// Снимок страницы, PNG-байты.
    ///
    /// «Что видит бот» — для ассистированного входа и диагностики; дёргается
    /// примерно раз в 5 секунд, поэтому дешевизна важнее точности цвета.
    pub async fn screenshot(&self, opts: &ScreenshotOptions) -> Result<Vec<u8>, CdpError> {
        let mut params = json!({
            "format": opts.format,
            "fromSurface": opts.from_surface,
            "captureBeyondViewport": false,
        });
        if let Some(q) = opts.quality {
            params["quality"] = json!(q);
        }
        let res = self.session.call("Page.captureScreenshot", params).await?;
        let data = res["data"].as_str().ok_or_else(|| {
            CdpError::UnexpectedShape(format!("Page.captureScreenshot без data: {res}"))
        })?;
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| CdpError::UnexpectedShape(format!("скриншот не декодируется из base64: {e}")))
    }
}

/// Превращает `exceptionDetails` из `Runtime.evaluate` в ошибку.
///
/// Без этой проверки упавшее выражение выглядит как «элемент не найден», и
/// вместо реальной причины (например, опечатка в селекторе площадки) в лог
/// поедет неверный диагноз.
fn check_js_exception(res: &Value) -> Result<(), CdpError> {
    if let Some(details) = res.get("exceptionDetails") {
        let text = details["exception"]["description"]
            .as_str()
            .or_else(|| details["text"].as_str())
            .unwrap_or("исключение без описания");
        return Err(CdpError::JsException(text.to_string()));
    }
    Ok(())
}

/// Вырезает источник из URL: `https://host:port`.
///
/// Своими руками, без крейта `url`: нужен ровно префикс до третьего слэша, а
/// новый узел в дереве зависимостей ради этого — плохая сделка.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RecordingTransport;

    #[test]
    fn origin_is_cut_before_the_path() {
        assert_eq!(
            origin_of("https://telemost.yandex.ru/j/1234567890").as_deref(),
            Some("https://telemost.yandex.ru")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8081/meet.html?x=1").as_deref(),
            Some("http://127.0.0.1:8081")
        );
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("https://"), None);
    }

    /// Порядок команд старта цели — главный инвариант фазы.
    #[tokio::test]
    async fn start_sequence_order_is_exact() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let session = browser.open_target("about:blank").await.unwrap();
        t.clear();

        let mut page = CdpPage::new(browser, session);
        let setup = PageSetup {
            inject: InjectConfig {
                binding_name: "__forge_seq".into(),
                user_script: "post('ready');".into(),
                ..InjectConfig::default()
            },
            ..PageSetup::default()
        };
        page.prepare_and_navigate("https://telemost.yandex.ru/j/42", &setup)
            .await
            .unwrap();

        assert_eq!(
            t.methods(),
            vec![
                "Page.setBypassCSP",
                "Browser.grantPermissions",
                "Runtime.addBinding",
                "Page.addScriptToEvaluateOnNewDocument",
                "Target.setAutoAttach",
                "Page.navigate",
            ],
            "нарушение порядка ломает инжект молча: страница откроется, а звука не будет"
        );

        // Разрешения выданы именно источнику встречи.
        assert_eq!(
            t.calls_of("Browser.grantPermissions")[0].params["origin"],
            "https://telemost.yandex.ru"
        );
        // Автоаттач с ожиданием отладчика — иначе фрейм успеет выполнить свой код.
        let auto = &t.calls_of("Target.setAutoAttach")[0].params;
        assert_eq!(auto["waitForDebuggerOnStart"], true);
        assert_eq!(auto["flatten"], true);
        // Навигация — на адрес встречи, а не на about:blank.
        assert_eq!(t.calls_of("Page.navigate")[0].params["url"], "https://telemost.yandex.ru/j/42");
        assert_eq!(page.binding_name(), "__forge_seq");
    }

    #[tokio::test]
    async fn navigate_reports_error_text_as_failure() {
        let t = RecordingTransport::new();
        t.set_response(
            "Page.navigate",
            json!({"frameId": "F1", "errorText": "net::ERR_NAME_NOT_RESOLVED"}),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let page = CdpPage::new(browser, session);
        let err = page.navigate("https://nope.invalid").await.unwrap_err();
        assert!(
            err.to_string().contains("ERR_NAME_NOT_RESOLVED"),
            "errorText приходит в result, а не ошибкой протокола: {err}"
        );
    }

    /// Клик = центр бокса из DOM + пара нажатие/отпускание, а не `el.click()`.
    #[tokio::test]
    async fn click_selector_uses_box_center_and_dispatches_real_mouse_events() {
        let t = RecordingTransport::new();
        t.set_response(
            "Runtime.evaluate",
            json!({"result": {"type": "string", "value": r#"{"x":642.5,"y":410.25,"w":120,"h":36}"#}}),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let page = CdpPage::new(browser, session);

        let center = page
            .click_selector("[data-testid=\"enter-conference-button\"]")
            .await
            .unwrap();
        assert_eq!(center.x, 642.5);
        assert_eq!(center.y, 410.25);

        assert_eq!(
            t.methods(),
            vec![
                "Target.attachToTarget",
                "Runtime.evaluate",
                "Input.dispatchMouseEvent",
                "Input.dispatchMouseEvent",
                "Input.dispatchMouseEvent",
            ]
        );
        let evaluate = &t.calls_of("Runtime.evaluate")[0].params;
        let expr = evaluate["expression"].as_str().unwrap();
        assert!(
            expr.contains("getBoundingClientRect"),
            "координаты обязаны считаться из DOM, а не сниматься со скриншота"
        );
        assert!(expr.contains("document.querySelector"));
        assert_eq!(evaluate["returnByValue"], true);

        let clicks = t.calls_of("Input.dispatchMouseEvent");
        assert_eq!(clicks[0].params["type"], "mouseMoved");
        assert_eq!(clicks[1].params["type"], "mousePressed");
        assert_eq!(clicks[2].params["type"], "mouseReleased");
        for c in &clicks {
            assert_eq!(c.params["x"], 642.5);
            assert_eq!(c.params["y"], 410.25);
        }
        assert_eq!(clicks[1].params["buttons"], 1);
        assert_eq!(clicks[1].params["clickCount"], 1);
        assert_eq!(clicks[2].params["buttons"], 0);
    }

    #[tokio::test]
    async fn missing_element_is_reported_not_clicked_blindly() {
        let t = RecordingTransport::new();
        t.set_response("Runtime.evaluate", json!({"result": {"type": "object", "value": null}}));
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let page = CdpPage::new(browser, session);
        let err = page.click_selector("#нет-такого").await.unwrap_err();
        assert!(matches!(err, CdpError::ElementNotFound(_)), "{err:?}");
        assert!(t.calls_of("Input.dispatchMouseEvent").is_empty());
    }

    #[tokio::test]
    async fn hidden_element_with_zero_box_is_not_clicked() {
        let t = RecordingTransport::new();
        t.set_response(
            "Runtime.evaluate",
            json!({"result": {"value": r#"{"x":0,"y":0,"w":0,"h":0}"#}}),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let page = CdpPage::new(browser, session);
        let err = page.click_selector("#hidden").await.unwrap_err();
        assert!(matches!(err, CdpError::ElementNotFound(_)), "{err:?}");
        assert!(t.calls_of("Input.dispatchMouseEvent").is_empty());
    }

    #[tokio::test]
    async fn js_exception_is_not_disguised_as_missing_element() {
        let t = RecordingTransport::new();
        t.set_response(
            "Runtime.evaluate",
            json!({
                "result": {"type": "object"},
                "exceptionDetails": {"text": "Uncaught", "exception": {"description": "SyntaxError: bad selector"}}
            }),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let page = CdpPage::new(browser, session);
        let err = page.click_selector("[[").await.unwrap_err();
        match err {
            CdpError::JsException(msg) => assert!(msg.contains("SyntaxError")),
            other => panic!("ожидалось исключение JS, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn type_text_focuses_then_uses_insert_text() {
        let t = RecordingTransport::new();
        t.set_response("Runtime.evaluate", json!({"result": {"value": "ok"}}));
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();
        let page = CdpPage::new(browser, session);
        page.type_text("#name", "Бот встречи").await.unwrap();

        assert_eq!(t.methods(), vec!["Runtime.evaluate", "Input.insertText"]);
        assert!(t.calls_of("Runtime.evaluate")[0].params["expression"]
            .as_str()
            .unwrap()
            .contains("el.focus()"));
        assert_eq!(t.calls_of("Input.insertText")[0].params["text"], "Бот встречи");
    }

    #[tokio::test]
    async fn type_text_into_missing_input_is_an_error() {
        let t = RecordingTransport::new();
        t.set_response("Runtime.evaluate", json!({"result": {"value": "missing"}}));
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let page = CdpPage::new(browser, session);
        let err = page.type_text("#нет", "текст").await.unwrap_err();
        assert!(matches!(err, CdpError::ElementNotFound(_)), "{err:?}");
        assert!(t.calls_of("Input.insertText").is_empty());
    }

    #[tokio::test]
    async fn window_is_moved_between_offscreen_and_visible() {
        let t = RecordingTransport::new();
        t.set_response("Browser.getWindowForTarget", json!({"windowId": 7}));
        t.set_response(
            "Browser.getWindowBounds",
            json!({"bounds": {"left": 80, "top": 80, "width": 1280, "height": 800}}),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();
        let page = CdpPage::new(browser, session);

        let actual = page.set_window_visible(true).await.unwrap();
        assert_eq!(actual, page.placement().visible);
        // Показ окна человеку — единственное место, где мы имеем право
        // отобрать фокус, и инициирует его человек.
        assert_eq!(t.calls_of("Page.bringToFront").len(), 1);

        // Показ идёт ДВУМЯ вызовами, и порядок обязателен: сначала окно
        // возвращается в нормальное состояние, и только потом принимает
        // координаты. Свёрнутое окно координат не принимает вовсе, и перенос
        // молча не сработал бы.
        let calls = t.calls_of("Browser.setWindowBounds");
        assert_eq!(calls.len(), 2, "ожидались restore + перенос: {calls:?}");
        assert_eq!(calls[0].params["bounds"]["windowState"], "normal");
        assert!(
            calls[0].params["bounds"]["left"].is_null(),
            "restore не имеет права нести координаты: {:?}",
            calls[0].params
        );
        let bounds = &calls[1].params;
        assert_eq!(bounds["windowId"], 7);
        assert_eq!(bounds["bounds"]["left"], 80);
        assert_eq!(bounds["bounds"]["windowState"], "normal");

        t.clear();
        page.set_window_visible(false).await.unwrap();
        let calls = t.calls_of("Browser.setWindowBounds");
        assert_eq!(calls.len(), 1, "пряча окно, разворачивать его незачем");
        let bounds = &calls[0].params;
        assert_eq!(bounds["bounds"]["left"], -4000);
        assert!(
            t.calls_of("Page.bringToFront").is_empty(),
            "уводя окно за экран, фокус не трогаем"
        );
    }

    /// Свёрнутый бот, которого показали человеку, обязан ВЕРНУТЬСЯ СВЁРНУТЫМ.
    ///
    /// Иначе «покажи, что видит бот» превращалось бы в необратимое: окно
    /// уезжало бы за экран, а «за экраном» на macOS означает «на экране» —
    /// измерено дважды, `left: -4000` подрезается до `-930` (Ф1) и до `310`
    /// (повторное измерение Ф3).
    #[tokio::test]
    async fn a_minimized_bot_returns_to_minimized_after_being_shown() {
        let t = RecordingTransport::new();
        t.set_response("Browser.getWindowForTarget", json!({"windowId": 7}));
        t.set_response(
            "Browser.getWindowBounds",
            json!({"bounds": {"left": 80, "top": 80, "width": 1280, "height": 800}}),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        let mut page = CdpPage::new(browser, session);
        page.set_hidden_mode(WindowMode::Minimized);

        page.set_window_visible(true).await.unwrap();
        t.clear();
        page.set_window_visible(false).await.unwrap();

        let calls = t.calls_of("Browser.setWindowBounds");
        assert_eq!(calls.len(), 1, "возврат — один вызов состояния: {calls:?}");
        assert_eq!(calls[0].params["bounds"]["windowState"], "minimized");
        assert!(
            calls[0].params["bounds"]["left"].is_null(),
            "состояние, отличное от normal, не принимает координат"
        );
        assert!(
            t.calls_of("Page.bringToFront").is_empty(),
            "убирая окно, фокус не трогаем"
        );
    }

    /// Режим окна применяется при подготовке страницы, а не при запуске
    /// браузера: до создания вкладки окна нет вовсе (`--no-startup-window`).
    #[tokio::test]
    async fn the_window_mode_is_applied_when_the_page_is_prepared() {
        let t = RecordingTransport::new();
        t.set_response("Browser.getWindowForTarget", json!({"windowId": 7}));
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();
        let mut page = CdpPage::new(browser, session);
        let setup = PageSetup {
            window_mode: WindowMode::Minimized,
            permissions: vec![],
            ..PageSetup::default()
        };
        page.prepare_and_navigate("https://example.org/m", &setup)
            .await
            .unwrap();

        let calls = t.calls_of("Browser.setWindowBounds");
        assert_eq!(calls.len(), 1, "окно обязано свернуться ровно один раз");
        assert_eq!(calls[0].params["bounds"]["windowState"], "minimized");
        assert_eq!(
            page.hidden_mode(),
            WindowMode::Minimized,
            "страница обязана помнить, куда возвращать окно"
        );
        // Сворачивание идёт ДО навигации: окно не имеет права мелькнуть с
        // открытой встречей.
        let methods = t.methods();
        let minimize_at = methods.iter().position(|m| m == "Browser.setWindowBounds").unwrap();
        let navigate_at = methods.iter().position(|m| m == "Page.navigate").unwrap();
        assert!(minimize_at < navigate_at, "{methods:?}");
    }

    /// Режим по умолчанию окна не трогает: `offscreen` и `visible`
    /// расставляются координатами, а не состоянием.
    #[tokio::test]
    async fn the_default_window_mode_sends_no_state_at_all() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();
        let mut page = CdpPage::new(browser, session);
        page.prepare_and_navigate("https://example.org/m", &PageSetup::default())
            .await
            .unwrap();
        assert!(t.calls_of("Browser.setWindowBounds").is_empty());
    }

    #[tokio::test]
    async fn screenshot_decodes_base64_into_png_bytes() {
        let t = RecordingTransport::new();
        // PNG-сигнатура в base64.
        t.set_response(
            "Page.captureScreenshot",
            json!({"data": "iVBORw0KGgo="}),
        );
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();
        let page = CdpPage::new(browser, session);
        let png = page.screenshot(&ScreenshotOptions::default()).await.unwrap();
        assert_eq!(png, vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let params = &t.calls_of("Page.captureScreenshot")[0].params;
        assert_eq!(params["format"], "png");
        assert_eq!(
            params["fromSurface"], true,
            "у неактивной вкладки рендерерный путь отказывает — измерено на Chrome 151"
        );
        assert_eq!(params["captureBeyondViewport"], false);
    }

    #[tokio::test]
    async fn csp_bypass_can_be_disabled_without_breaking_the_rest() {
        let t = RecordingTransport::new();
        let browser = CdpBrowser::new(t.clone());
        let session = browser.attach("T1").await.unwrap();
        t.clear();
        let mut page = CdpPage::new(browser, session);
        let setup = PageSetup {
            bypass_csp: false,
            permissions: vec![],
            ..PageSetup::default()
        };
        page.prepare_and_navigate("https://example.org/m", &setup)
            .await
            .unwrap();
        assert_eq!(
            t.methods(),
            vec![
                "Runtime.addBinding",
                "Page.addScriptToEvaluateOnNewDocument",
                "Target.setAutoAttach",
                "Page.navigate",
            ]
        );
    }
}
