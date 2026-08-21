//! Ранние предупреждения о том, что бот сидит во встрече, но НЕ СЛЫШИТ её.
//!
//! # Зачем
//!
//! Худший исход записи — не падение, а тишина. Живой случай: встреча на
//! `vcc.airlabs.pro` шла 63 минуты, бот вошёл, дорожки завелись, DOM исправно
//! показывал говорящих (1372 события `active_speaker`) — а во всех трёх файлах
//! оказались ровно нули (`mean_volume = max_volume = -91,0 dB`, пол 16-битной
//! сетки). Медиа-канал не поднялся: DNS медиасервера площадки не разрешился
//! изнутри сетевого сервиса Chrome, TURN ответил 403, соединение вырезано. Всё
//! это было напечатано в `chrome.log` ВО ВРЕМЯ встречи и не покинуло файла.
//!
//! Пост-обработка такую запись теперь останавливает (гейт тишины в
//! [`crate::pipeline::run`]), но остановка — это честный отказ ПОСЛЕ. Пока
//! встреча идёт, её ещё можно спасти: перезапустить бота, сменить сеть,
//! записать со своей стороны. Ради этого окна модуль и существует.
//!
//! # Два источника, и оба нужны
//!
//! * [`DeafWatch`] — расхождение «DOM показывает речь, VAD молчит». Ловит
//!   ЛЮБУЮ причину глухоты, включая ещё не встречавшиеся, но не называет её.
//! * [`MediaLogWatch`] — известные строки отказа медиа-канала из `chrome.log`.
//!   Называет причину, но знает только то, что уже видели.
//!
//! Первый без второго оставляет человека с вопросом «почему», второй без
//! первого молчит о новых поломках.

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Код предупреждения «бот в комнате, но звука не слышит».
pub const CODE_DEAF_BOT: &str = "deaf_bot";

/// Код предупреждения «медиа-канал до площадки не поднялся».
pub const CODE_MEDIA_TRANSPORT: &str = "media_transport";

/// Сколько НАБЛЮДАЕМОЙ В DOM РЕЧИ должно пройти впустую, прежде чем говорить
/// человеку, мс.
///
/// Считается не стенное время, а сумма тиков, на которых площадка показывала
/// говорящего: встреча, начавшаяся с трёх минут молчания, — это норма, а не
/// поломка, и ругаться на неё нельзя. Минуты чужой речи мимо микрофона хватает
/// с запасом: в живом случае DOM показывал активных спикеров непрерывно.
pub const DEAF_WARN_AFTER_MS: u64 = 60_000;

/// Расхождение «DOM говорит, VAD молчит».
///
/// Одноразовый: предупредив, замолкает навсегда. Повторять раз в секунду
/// целый час означало бы утопить журнал встречи в одном и том же.
#[derive(Debug, Default)]
pub struct DeafWatch {
    dom_speech_ms: u64,
    warned: bool,
}

impl DeafWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Один тик наблюдения.
    ///
    /// * `dom_speaking` — площадка показывает, что кто-то говорит;
    /// * `vad_ever_fired` — VAD хоть раз за встречу нашёл речь в звуке;
    /// * `tick_ms` — период наблюдения.
    ///
    /// Возвращает текст предупреждения ровно один раз за встречу.
    pub fn observe(
        &mut self,
        dom_speaking: bool,
        vad_ever_fired: bool,
        tick_ms: u64,
    ) -> Option<String> {
        if self.warned {
            return None;
        }
        if vad_ever_fired {
            // Звук есть — накопленное подозрение больше не имеет смысла, и
            // держать его нельзя: иначе поздняя пауза досчитала бы порог.
            self.dom_speech_ms = 0;
            self.warned = true;
            return None;
        }
        if !dom_speaking {
            return None;
        }
        self.dom_speech_ms = self.dom_speech_ms.saturating_add(tick_ms);
        if self.dom_speech_ms < DEAF_WARN_AFTER_MS {
            return None;
        }
        self.warned = true;
        Some(format!(
            "бот в комнате, но не слышит её: площадка показывает говорящих уже {} с, \
             а в звуке нет ни одного интервала речи. Скорее всего медиа-канал до площадки \
             не поднялся — запись будет пустой.",
            self.dom_speech_ms / 1000
        ))
    }
}

/// Известная строка отказа медиа-канала.
struct Pattern {
    /// Класс отказа: по нему предупреждение дедуплицируется.
    class: &'static str,
    /// Подстроки, которые обязаны встретиться в строке ВСЕ.
    needles: &'static [&'static str],
    /// Что сказать человеку.
    detail: &'static str,
}

/// Таблица известных отказов.
///
/// Подстроки, а не регулярные выражения: строки Chrome меняются от версии к
/// версии, и чем короче зацепка, тем дольше она живёт. Класс отказа отделён от
/// текста нарочно — одна и та же причина печатается Chrome десятки раз подряд
/// (в живом случае DNS не разрешился семь раз, TURN отказывал непрерывно), а
/// человеку нужно одно предупреждение, а не сто.
const PATTERNS: &[Pattern] = &[
    Pattern {
        class: "dns",
        needles: &["Failed to resolve address for"],
        detail: "адрес медиасервера площадки не разрешается по DNS",
    },
    Pattern {
        class: "turn",
        needles: &["TURN", "code=403"],
        detail: "ретранслятор TURN площадки отказал (403)",
    },
    Pattern {
        class: "turn",
        needles: &["TURN", "error response"],
        detail: "ретранслятор TURN площадки ответил ошибкой",
    },
    Pattern {
        class: "ice",
        needles: &["Failed to create connection to relay"],
        detail: "соединение с ретранслятором не создалось",
    },
];

/// Класс и текст отказа для строки лога. `None` — строка обычная.
pub fn classify(line: &str) -> Option<(&'static str, &'static str)> {
    PATTERNS
        .iter()
        .find(|p| p.needles.iter().all(|n| line.contains(n)))
        .map(|p| (p.class, p.detail))
}

/// Дочитыватель `chrome.log`.
///
/// Chrome пишет туда свои stdout и stderr (перенаправление живёт в
/// `forge_cdp::launch`), файл только растёт, и читать его достаточно с того
/// места, где остановились в прошлый раз. НЕДОЧИТАННЫЙ ХВОСТ БЕЗ ПЕРЕВОДА
/// СТРОКИ НЕ РАЗБИРАЕТСЯ и не сдвигает смещение: Chrome пишет строку не
/// атомарно, и половина строки — это не «строка без совпадения», а ещё не
/// написанная строка.
#[derive(Debug)]
pub struct MediaLogWatch {
    path: PathBuf,
    offset: u64,
    seen: BTreeSet<&'static str>,
}

impl MediaLogWatch {
    pub fn new(meeting_dir: &Path) -> Self {
        Self {
            path: meeting_dir.join("chrome.log"),
            offset: 0,
            seen: BTreeSet::new(),
        }
    }

    /// Дочитывает лог и отдаёт тексты НОВЫХ классов отказа.
    ///
    /// Пустой вектор — обычное состояние: файла нет, он не вырос или в новых
    /// строках нет ничего знакомого.
    pub fn poll(&mut self) -> Vec<String> {
        let chunk = match self.read_new() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        for line in chunk.lines() {
            let Some((class, detail)) = classify(line) else {
                continue;
            };
            if !self.seen.insert(class) {
                continue;
            }
            out.push(format!(
                "медиа-канал до площадки не поднялся: {detail}. Пока это так, \
                 запись будет пустой, даже если во встрече говорят."
            ));
        }
        out
    }

    /// Новые ЦЕЛЫЕ строки лога; сдвигает смещение только на них.
    fn read_new(&mut self) -> Option<String> {
        let mut file = std::fs::File::open(&self.path).ok()?;
        let len = file.metadata().ok()?.len();
        if len < self.offset {
            // Файл усечён (перезапуск Chrome в том же каталоге) — читаем с
            // начала, иначе смещение навсегда уедет за конец.
            self.offset = 0;
            self.seen.clear();
        }
        if len == self.offset {
            return None;
        }
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut buf = Vec::new();
        file.take(len - self.offset).read_to_end(&mut buf).ok()?;
        let cut = buf.iter().rposition(|b| *b == b'\n')? + 1;
        self.offset += cut as u64;
        Some(String::from_utf8_lossy(&buf[..cut]).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Живые строки Chrome из каталога встречи
    /// `1dc0be7a-2037-4305-a2e5-878a539f936f`, где запись оказалась пустой.
    const LIVE_DNS: &str = "[26383:259:0820/141930.293081:ERROR:services/network/p2p/socket_manager.cc:180] Failed to resolve address for live.airlabs.pro., errorcode: -105";
    const LIVE_TURN: &str = "[26383:259:0820/141935.101003:ERROR:third_party/webrtc/p2p/base/turn_port.cc:1481] Received TURN CreatePermission error response, code=403; pruned connection.";

    #[test]
    fn the_live_failure_lines_are_recognised() {
        assert_eq!(classify(LIVE_DNS).map(|c| c.0), Some("dns"));
        assert_eq!(classify(LIVE_TURN).map(|c| c.0), Some("turn"));
        assert_eq!(classify("[26383:259:0820/141930.293081:INFO:foo.cc:1] всё хорошо"), None);
    }

    /// Одна причина — одно предупреждение, сколько бы раз Chrome её ни
    /// напечатал: в живом логе отказ DNS повторён семь раз, а TURN — десятки.
    #[test]
    fn one_cause_warns_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("chrome.log");
        let mut lines = String::new();
        for _ in 0..7 {
            lines.push_str(LIVE_DNS);
            lines.push('\n');
        }
        std::fs::write(&log, &lines).unwrap();

        let mut watch = MediaLogWatch::new(dir.path());
        let first = watch.poll();
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(first[0].contains("DNS"), "{first:?}");
        assert!(watch.poll().is_empty(), "лог не вырос — говорить нечего");

        // Новая ПРИЧИНА в том же файле — новое предупреждение.
        std::fs::write(&log, format!("{lines}{LIVE_TURN}\n")).unwrap();
        let second = watch.poll();
        assert_eq!(second.len(), 1, "{second:?}");
        assert!(second[0].contains("TURN"), "{second:?}");
    }

    /// Половина строки — это ещё не строка: Chrome пишет её не атомарно, и
    /// разобрать обрывок означало бы потерять его вторую половину.
    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("chrome.log");
        let (head, tail) = LIVE_DNS.split_at(40);
        std::fs::write(&log, head).unwrap();

        let mut watch = MediaLogWatch::new(dir.path());
        assert!(watch.poll().is_empty(), "обрывок разобран как строка");

        std::fs::write(&log, format!("{head}{tail}\n")).unwrap();
        assert_eq!(watch.poll().len(), 1, "дописанная строка не разобралась");
    }

    /// Отсутствующего файла достаточно, чтобы промолчать: `chrome.log`
    /// появляется не мгновенно, а наблюдение начинается сразу.
    #[test]
    fn a_missing_log_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = MediaLogWatch::new(dir.path());
        assert!(watch.poll().is_empty());
    }

    /// ГЛАВНЫЙ СЛУЧАЙ: DOM показывает говорящих, VAD молчит.
    #[test]
    fn dom_speech_without_vad_warns_once() {
        let mut watch = DeafWatch::new();
        let mut warnings = Vec::new();
        // Час наблюдения раз в секунду — ровно как в живой встрече.
        for _ in 0..3600 {
            if let Some(w) = watch.observe(true, false, 1000) {
                warnings.push(w);
            }
        }
        assert_eq!(warnings.len(), 1, "предупреждение обязано быть ровно одно");
        assert!(warnings[0].contains("не слышит"), "{}", warnings[0]);
    }

    /// Молчаливое начало встречи — это норма, а не поломка.
    #[test]
    fn silence_alone_never_warns() {
        let mut watch = DeafWatch::new();
        for _ in 0..3600 {
            assert!(watch.observe(false, false, 1000).is_none());
        }
    }

    /// Звук есть — предупреждения не будет НИКОГДА, даже если потом наступит
    /// долгая пауза при говорящем DOM.
    #[test]
    fn once_the_vad_fires_the_watch_is_silent_forever() {
        let mut watch = DeafWatch::new();
        for _ in 0..30 {
            assert!(watch.observe(true, false, 1000).is_none());
        }
        assert!(watch.observe(true, true, 1000).is_none());
        for _ in 0..3600 {
            assert!(watch.observe(true, false, 1000).is_none());
        }
    }
}
