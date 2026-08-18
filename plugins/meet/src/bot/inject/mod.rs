//! Сборка инжектируемого скрипта из кусков.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Что во что вкладывается
//!
//! ```text
//! forge_cdp::build_inject_source(              ← обёртка: биндинг, кольцевой
//!   InjectConfig { user_script: … }              буфер, счётчик потерь
//! )
//!   └── FORGE_MEET_CFG = {…}                   ← конфиг встречи данными
//!   └── pcm.js                                 ← чистые функции (node --test)
//!   └── transport.js                           ← кодировка кадра и отправка
//!   └── capture.js                             ← ТРИ ТАПА захвата
//!
//! отдельно, в свой Blob-URL:
//!   worklet_source()
//!     └── pcm.js + vad.js                      ← те же чистые функции
//!     └── worklet.js                           ← AudioWorkletProcessor
//! ```
//!
//! # Почему исходники — файлы, а не строки в Rust
//!
//! Их читает и правит человек, а `node --test` прогоняет чистые части
//! (`pcm.js`, `vad.js`) напрямую. Строковый литерал в `.rs` лишил бы и
//! подсветки, и тестов, и внятного diff'а.
//!
//! # Маркер `FORGE_STRIP_FOR_WORKLET`
//!
//! `pcm.js` и `vad.js` — модули ES с `export`, потому что их импортирует
//! `node --test`. В `AudioWorkletGlobalScope` модулей нет вовсе, и `export`
//! там — синтаксическая ошибка. Поэтому строки с экспортом помечены
//! маркером, а сборщик снимает с них слово `export`.
//!
//! Вырезать строку целиком было бы ошибкой: вместе с `export` исчезло бы и
//! само объявление функции. На это есть тест.

use serde::Serialize;

use crate::config::MeetConfig;

/// Чистые функции звука. Разделяются между главным потоком и worklet'ом.
pub const PCM_JS: &str = include_str!("pcm.js");

/// Детект речи. Живёт в worklet'е; в `node --test` импортируется как модуль.
pub const VAD_JS: &str = include_str!("vad.js");

/// `AudioWorkletProcessor` захвата.
pub const WORKLET_JS: &str = include_str!("worklet.js");

/// Кодировка кадра и отправка через биндинг.
pub const TRANSPORT_JS: &str = include_str!("transport.js");

/// Три независимых тапа захвата.
pub const CAPTURE_JS: &str = include_str!("capture.js");

/// Чтение состояния встречи по пробам (Ф4).
///
/// В инжект НЕ входит и входить не должен: наблюдение живёт отдельным
/// `Runtime.evaluate`, потому что запись не зависит ни от одной DOM-пробы.
/// Собирается в выражение через [`crate::bot::platforms::observe_expression`].
pub const OBSERVE_JS: &str = include_str!("observe.js");

/// Маркер строк, с которых сборщик снимает `export`.
const STRIP_MARKER: &str = "// FORGE_STRIP_FOR_WORKLET";

/// Имя процессора в `registerProcessor`.
///
/// Случайности здесь не нужно: имя видно только внутри нашего же
/// blob-модуля, а фиксированное проще искать в логе.
pub const WORKLET_PROCESSOR_NAME: &str = "forge-capture";

/// Параметры VAD в том виде, в каком они уезжают в скрипт.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct VadParams {
    /// Порог ВКЛЮЧЕНИЯ, дБ над адаптивным шумовым полом.
    #[serde(rename = "onDb")]
    pub on_db: f32,
    /// Порог ВЫКЛЮЧЕНИЯ, дБ. Строго меньше порога включения: гистерезис —
    /// единственное, что не даёт границе речи дребезжать.
    #[serde(rename = "offDb")]
    pub off_db: f32,
    /// Сколько миллисекунд звук обязан держаться выше порога.
    #[serde(rename = "confirmMs")]
    pub confirm_ms: u64,
    /// Сколько миллисекунд речь считается продолжающейся после падения ниже
    /// порога выключения.
    #[serde(rename = "hangoverMs")]
    pub hangover_ms: u64,
}

impl From<&MeetConfig> for VadParams {
    fn from(cfg: &MeetConfig) -> Self {
        Self {
            on_db: cfg.vad_on_db,
            off_db: cfg.vad_off_db,
            confirm_ms: cfg.vad_confirm_ms,
            hangover_ms: cfg.vad_hangover_ms,
        }
    }
}

/// Конфигурация, уезжающая в страницу объектом `FORGE_MEET_CFG`.
///
/// Данными, а не подстановкой в текст скрипта: строка, собранная
/// конкатенацией, однажды получила бы в себя кавычку из имени бота.
#[derive(Debug, Clone, Serialize)]
pub struct InjectParams {
    /// Целевая частота по проводу. Всегда 16 000 — столько хочет whisper.
    #[serde(rename = "sampleRate")]
    pub sample_rate: u32,
    /// Длина кадра, мс.
    #[serde(rename = "frameMs")]
    pub frame_ms: u64,
    /// Как часто обходить `pc.getReceivers()` в поисках дорожек, добавленных
    /// без события `track`.
    #[serde(rename = "receiverScanMs")]
    pub receiver_scan_ms: u64,
    pub vad: VadParams,
    /// Имя процессора для `registerProcessor` / `new AudioWorkletNode`.
    #[serde(rename = "workletName")]
    pub worklet_name: String,
    /// Полный исходник worklet'а: страница кладёт его в Blob-URL.
    #[serde(rename = "workletSource")]
    pub worklet_source: String,
}

/// Как часто обходить получателей. Две секунды из плана: дорожка, добавленная
/// без события, не должна ждать дольше.
pub const DEFAULT_RECEIVER_SCAN_MS: u64 = 2_000;

impl InjectParams {
    /// Собирает параметры из конфигурации плагина.
    pub fn from_config(cfg: &MeetConfig) -> Self {
        Self {
            sample_rate: cfg.audio_sample_rate,
            frame_ms: cfg.audio_frame_ms,
            receiver_scan_ms: DEFAULT_RECEIVER_SCAN_MS,
            vad: VadParams::from(cfg),
            worklet_name: WORKLET_PROCESSOR_NAME.to_string(),
            worklet_source: worklet_source(),
        }
    }
}

/// Снимает `export` со строк, помеченных маркером.
///
/// Именно слово, а не строку целиком: вместе с `export` исчезло бы само
/// объявление функции, и worklet упал бы на первом же вызове.
///
/// Публичная: тот же маркер снимает `export` и с `observe.js`, который
/// вставляется в `Runtime.evaluate` (`bot/platforms.rs`). Механизм один, и
/// второй копии ему не нужно.
pub fn strip_exports(src: &str) -> String {
    src.lines()
        .map(|line| {
            if !line.contains(STRIP_MARKER) {
                return line.to_string();
            }
            let without_marker = line.replace(STRIP_MARKER, "");
            let trimmed_start = without_marker.trim_start();
            let indent = &without_marker[..without_marker.len() - trimmed_start.len()];
            let body = trimmed_start.strip_prefix("export ").unwrap_or(trimmed_start);
            format!("{indent}{}", body.trim_end())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Полный исходник worklet'а.
pub fn worklet_source() -> String {
    let name = serde_json::to_string(WORKLET_PROCESSOR_NAME)
        .unwrap_or_else(|_| "\"forge-capture\"".to_string());
    format!(
        "// forge-meet: AudioWorkletProcessor захвата. Собран из pcm.js, vad.js и worklet.js.\n\
         const FORGE_WORKLET_NAME = {name};\n\
         {pcm}\n{vad}\n{worklet}\n",
        pcm = strip_exports(PCM_JS),
        vad = strip_exports(VAD_JS),
        worklet = WORKLET_JS,
    )
}

/// Полный исходник скрипта страницы (без обёртки [`forge_cdp`]).
pub fn page_source(params: &InjectParams) -> String {
    let cfg = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());
    format!(
        "// forge-meet: три тапа захвата, транспорт через CDP-биндинг.\n\
         var FORGE_MEET_CFG = {cfg};\n\
         {pcm}\n{transport}\n{capture}\n",
        pcm = strip_exports(PCM_JS),
        transport = TRANSPORT_JS,
        capture = CAPTURE_JS,
    )
}

/// Готовая конфигурация инжекта для [`forge_cdp`].
pub fn inject_config(cfg: &MeetConfig) -> forge_cdp::InjectConfig {
    forge_cdp::InjectConfig::with_script(page_source(&InjectParams::from_config(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Есть ли в строке ПРИСВАИВАНИЕ свойству `window`.
    ///
    /// Отдельной функцией, потому что наивное «строка содержит `window.` и
    /// `=`» ловит чтения (`var p = window.X && …`) и делает проверку
    /// бесполезной: она либо падает на безобидном коде, либо её ослабляют до
    /// нуля.
    fn assigns_to_window(line: &str) -> bool {
        let mut rest = line;
        while let Some(pos) = rest.find("window.") {
            let after = &rest[pos + "window.".len()..];
            let end = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '.')
                .unwrap_or(after.len());
            let tail = after[end..].trim_start();
            if let Some(next) = tail.strip_prefix('=') {
                if !next.starts_with('=') {
                    return true;
                }
            }
            rest = &after[end..];
        }
        false
    }

    #[test]
    fn the_window_write_detector_tells_reads_from_writes() {
        assert!(assigns_to_window("window.RTCPeerConnection = ForgePeerConnection;"));
        assert!(assigns_to_window("  window.AudioNode.prototype.connect = function (d) {"));
        assert!(!assigns_to_window("var proto = window.HTMLMediaElement && window.HTMLMediaElement.prototype;"));
        assert!(!assigns_to_window("if (window.AudioNode == null) { return; }"));
        assert!(!assigns_to_window("var Ctor = window.AudioContext || window.webkitAudioContext;"));
    }

    fn params() -> InjectParams {
        InjectParams::from_config(&MeetConfig::default())
    }

    /// Маркер снимает СЛОВО, а не строку: пропади объявление — worklet упал бы
    /// на первом вызове, и разбирались бы с этим уже на живой встрече.
    #[test]
    fn stripping_exports_keeps_the_declarations() {
        let src = "export function f(a) { // FORGE_STRIP_FOR_WORKLET\n  return a;\n}";
        let out = strip_exports(src);
        assert!(out.contains("function f(a)"), "{out}");
        assert!(!out.contains("export"), "{out}");
        assert!(!out.contains("FORGE_STRIP_FOR_WORKLET"), "{out}");
        assert!(out.contains("return a;"), "многострочное тело обязано уцелеть: {out}");
    }

    #[test]
    fn stripping_preserves_indentation() {
        let src = "  export const X = 1; // FORGE_STRIP_FOR_WORKLET";
        assert_eq!(strip_exports(src), "  const X = 1;");
    }

    /// В `AudioWorkletGlobalScope` нет ни модулей, ни `import`, ни `window`,
    /// ни `URL`. Любое из этих слов в собранном исходнике — отложенная
    /// ошибка, которая проявится только на живой встрече.
    #[test]
    fn the_worklet_source_uses_nothing_the_worklet_scope_lacks() {
        let src = worklet_source();
        for forbidden in ["export ", "import ", "window.", "document.", "URL.createObjectURL"] {
            assert!(
                !src.contains(forbidden),
                "worklet не имеет доступа к «{forbidden}», а исходник его содержит"
            );
        }
        assert!(src.contains("registerProcessor(FORGE_WORKLET_NAME"));
        assert!(src.contains("const FORGE_WORKLET_NAME = \"forge-capture\""));
        // Чистые функции обязаны доехать: без них процессор не соберётся.
        for needed in ["createResampler", "floatToPcm16", "createVadDetector", "toDb", "rms"] {
            assert!(src.contains(needed), "в worklet не доехала функция {needed}");
        }
    }

    /// Конфиг едет ДАННЫМИ. Собери мы скрипт конкатенацией — имя бота с
    /// кавычкой однажды сломало бы весь инжект.
    #[test]
    fn the_config_travels_as_json_not_as_string_splicing() {
        let src = page_source(&params());
        let line = src
            .lines()
            .find(|l| l.starts_with("var FORGE_MEET_CFG ="))
            .expect("нет объявления конфига");
        let json = line
            .trim_start_matches("var FORGE_MEET_CFG = ")
            .trim_end_matches(';');
        let v: serde_json::Value = serde_json::from_str(json).expect("конфиг не разбирается");
        assert_eq!(v["sampleRate"], 16_000, "whisper хочет ровно 16 кГц");
        assert_eq!(v["frameMs"], 20);
        assert_eq!(v["receiverScanMs"], 2_000);
        assert_eq!(v["vad"]["onDb"], 12.0);
        assert_eq!(v["vad"]["offDb"], 6.0);
        assert_eq!(v["vad"]["confirmMs"], 120);
        assert_eq!(v["vad"]["hangoverMs"], 500);
        assert_eq!(v["workletName"], "forge-capture");
        assert!(v["workletSource"].as_str().unwrap().contains("registerProcessor"));
    }

    /// Порог выключения обязан быть НИЖЕ порога включения: без гистерезиса
    /// граница речи дребезжит на каждом вдохе.
    #[test]
    fn the_vad_hysteresis_is_a_real_hysteresis() {
        let p = VadParams::from(&MeetConfig::default());
        assert!(p.off_db < p.on_db, "{p:?}");
        assert!(p.confirm_ms > 0 && p.hangover_ms > p.confirm_ms, "{p:?}");
    }

    /// Скрипт трогает `window` РОВНО в тех местах, где это и есть работа
    /// тапа. Всё остальное — глобал на чужом сайте, то есть готовый признак
    /// автоматизации и источник конфликта с кодом площадки.
    ///
    /// Собственные объявления (`var FORGE_MEET_CFG`, `B64`,
    /// `forgeTransportStats`) глобалами НЕ становятся: `forge_cdp`
    /// оборачивает весь скрипт в IIFE, и они живут в её области видимости.
    /// На это есть тест в самом `forge-cdp`.
    #[test]
    fn the_page_script_touches_window_only_where_a_tap_requires_it() {
        let src = page_source(&params());
        // Присваивание, а не чтение: `var p = window.X && …` трогать window
        // вправе, а `window.X = …` — только в трёх местах.
        let writes: Vec<String> = src
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//") && !l.starts_with('*'))
            .filter(|l| assigns_to_window(l))
            .map(str::to_string)
            .collect();
        for line in &writes {
            let allowed = line.contains("window.RTCPeerConnection = ForgePeerConnection")
                || line.contains("window.webkitRTCPeerConnection = ForgePeerConnection")
                || line.contains("window.AudioNode.prototype.connect = function");
            assert!(allowed, "скрипт пишет в window вне тапа: {line}");
        }
        assert_eq!(writes.len(), 3, "изменился набор точек патча: {writes:?}");

        // Обёртка `forge_cdp` обязана спрятать всё это в функцию: снаружи
        // остаётся ровно ноль новых имён.
        let wrapped = forge_cdp::build_inject_source(&inject_config(&MeetConfig::default()));
        assert!(wrapped.starts_with("(function ()"), "инжект обязан быть IIFE");
        assert!(wrapped.contains("delete window[NAME]"), "имя биндинга обязано исчезать");
    }

    /// Три тапа — не пожелание, а требование: без второго Zoom не даёт звука
    /// вовсе, без третьего теряются клиенты, вешающие поток на элемент.
    #[test]
    fn all_three_taps_are_present_and_ranked() {
        let src = page_source(&params());
        assert!(src.contains("installPeerConnectionTap"), "нет тапа RTCPeerConnection");
        assert!(src.contains("installWebAudioTap"), "нет тапа Web Audio (единственный путь Zoom)");
        assert!(src.contains("installMediaElementTap"), "нет тапа srcObject");
        // Приоритет источника 1 → 3 → 2 закреплён рангами.
        assert!(src.contains("PATH_RANK[PATH_RTC] = 3"));
        assert!(src.contains("PATH_RANK[PATH_ELEMENT] = 2"));
        assert!(src.contains("PATH_RANK[PATH_WEBAUDIO] = 1"));
    }

    /// Патч `RTCPeerConnection` обязан сохранить ОБА звена цепочки
    /// прототипов, иначе `instanceof` в коде площадки упадёт и сломает сайт.
    #[test]
    fn the_peer_connection_patch_preserves_both_prototype_links() {
        let src = CAPTURE_JS;
        assert!(src.contains("ForgePeerConnection.prototype = Native.prototype"));
        assert!(src.contains("Object.setPrototypeOf(ForgePeerConnection, Native)"));
        // Подписка через addEventListener, а НЕ через pc.ontrack: присваивание
        // перетёрло бы обработчик площадки.
        assert!(src.contains("pc.addEventListener(\"track\""));
        assert!(
            !src.contains("pc.ontrack ="),
            "присваивание ontrack перетирает обработчик площадки"
        );
    }

    /// Тап Web Audio обязан ДОБАВЛЯТЬ ребро, а не заменять его, и защищаться
    /// от петли: собственное `connect` иначе снова попало бы в патч.
    #[test]
    fn the_web_audio_tap_adds_an_edge_and_guards_against_a_loop() {
        let src = CAPTURE_JS;
        assert!(src.contains("var result = nativeConnect.apply(this, arguments)"));
        assert!(src.contains("nativeConnect.call(this, mirror)"), "зеркало через нативный connect");
        assert!(src.contains("new WeakSet()"), "нет защиты от петли");
        assert!(src.contains("state.mirrored.has(this)"));
    }

    /// Обход известного бага: источник от удалённого трека молчит, пока поток
    /// нигде не воспроизводится.
    #[test]
    fn every_captured_stream_gets_a_hidden_muted_sink() {
        let src = CAPTURE_JS;
        assert!(src.contains("function keepAlive(stream)"));
        assert!(src.contains("el.muted = true"));
        assert!(src.contains("data-forge-sink"));
        // И собственный приёмник не должен ловиться тапом 3 — иначе дубль.
        assert!(src.contains("this.hasAttribute(\"data-forge-sink\")"));
    }

    /// Индекс дорожки не переиспользуется: иначе звук двух разных людей
    /// склеился бы в один файл.
    #[test]
    fn the_track_index_never_gets_reused() {
        let src = CAPTURE_JS;
        assert!(src.contains("var idx = state.nextIdx++"));
        assert!(
            !src.contains("nextIdx--") && !src.contains("nextIdx ="),
            "индекс где-то возвращается назад"
        );
    }

    /// Ось времени ОДНА и берётся из `performance.now()`. `Date.now()`
    /// допустим ровно в одном месте — в поле `unix` события старта, которое
    /// в арифметике таймлайна не участвует.
    #[test]
    fn the_time_axis_comes_only_from_performance_now() {
        let src = CAPTURE_JS;
        assert!(src.contains("return performance.now()"));
        let date_uses: Vec<&str> = src.lines().filter(|l| l.contains("Date.now()")).collect();
        assert_eq!(
            date_uses.len(),
            1,
            "Date.now() допустим только в поле unix события старта: {date_uses:?}"
        );
        assert!(date_uses[0].contains("unix:"), "{:?}", date_uses[0]);
    }
}
