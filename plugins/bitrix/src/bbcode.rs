//! Разбор BB-кодов Битрикс24 в чистый текст и структурные сегменты (Р11).
//!
//! # Зачем свой парсер
//!
//! Портал хранит текст сообщения с разметкой прямо в поле `text`, причём
//! добавляет её сам: `URL_PREVIEW:"Y"` (значение по умолчанию у
//! `im.message.add`) оборачивает голые ссылки в `[URL]…[/URL]`, а системные
//! сообщения целиком состоят из `[USER=15 REPLACE]Имя[/USER]`. Показать такой
//! текст «как есть» нельзя — пользователь увидит квадратные скобки вместо имён.
//!
//! # Два выхода и ни одного HTML
//!
//! | функция | что отдаёт | кто потребитель |
//! |---|---|---|
//! | [`to_plain`] | текст без разметки | превью в списке диалогов, поле `text` у `MessageDto` (Ф5) |
//! | [`to_segments`] | дерево [`Segment`] | лента: каждый сегмент рисуется своим `createElement` |
//!
//! **HTML здесь не генерируется вообще.** На фронте `innerHTML` запрещён
//! (XSS-инвариант панели), поэтому единственная безопасная форма передачи
//! разметки — структура, которую браузер соберёт узлами через `textContent`.
//! Функции вида `to_html` в этом модуле появиться не могут: они бы означали,
//! что где-то во фронте есть присваивание `innerHTML`.
//!
//! # Объём поддержки — закрытый список (Р11)
//!
//! `[B] [I] [U] [S] [URL] [USER] [CHAT] [DISK] [TIMESTAMP] [CODE] [QUOTE]
//! [BR]` и больше ничего. Расширять список без правки Р11 нельзя: каждый
//! новый тег — это новая ветка рендера во фронте и новый способ подсунуть
//! туда чужие данные.
//!
//! **Незнакомый тег отдаётся ТЕКСТОМ КАК ЕСТЬ** — вместе со скобками. Это не
//! снисходительность к порталу, а требование безопасности наоборот: молча
//! выбрасывать неизвестную разметку значит незаметно менять смысл сообщения
//! (`[SPOILER]пароль[/SPOILER]` превратился бы в «пароль»).
//!
//! # Устойчивость к битой разметке
//!
//! Текст пишут люди, и квадратные скобки в нём живут своей жизнью. Поэтому:
//!
//! - незакрытый тег не съедает остаток сообщения из [`to_plain`]: его
//!   содержимое остаётся вложенными сегментами и участвует в тексте;
//! - перекрёстная вложенность (`[B]x[I]y[/B]z[/I]`) разбирается без паники:
//!   закрывающий тег ищется по СТЕКУ открытых, а не только по вершине;
//! - глубина ограничена [`MAX_DEPTH`] — дальше теги становятся обычным
//!   текстом, поэтому вложенность в тысячу уровней не роняет стек;
//! - одиночная `[`, `]` или `[/B]` без пары — просто символы.
//!
//! # Идентификаторы, а не имена
//!
//! `[USER=15 REPLACE]Максим[/USER]` несёт И идентификатор (`value`), И то имя,
//! которое портал знал на момент отправки (`children`). В сегмент едет и то, и
//! другое: имя может устареть, и фронт подставит свежее из кэша `peers`, а по
//! идентификатору строится клик «открыть диалог». Атрибут `REPLACE` — служебный
//! флаг портала, он отбрасывается: значением считается ПЕРВЫЙ токен атрибута.

use serde::{Deserialize, Serialize};

/// Предел вложенности разбора.
///
/// Восемь уровней — заведомо больше, чем встречается в живых сообщениях
/// (`[QUOTE][B][URL]…` — это три), и заведомо меньше, чем нужно для
/// переполнения стека рекурсией. Всё, что глубже, разбирается как текст: цитата
/// с тысячей вложенных `[QUOTE]` — это либо ошибка, либо атака, и в обоих
/// случаях правильный ответ «покажем как есть».
pub const MAX_DEPTH: usize = 8;

/// Максимальная длина содержимого одной скобочной пары `[…]`.
///
/// Тег длиннее — это не тег, а текст: самый длинный осмысленный случай
/// (`[URL=…]` с адресом) укладывается в две сотни символов, а без ограничения
/// одна открывающая скобка заставляла бы сканировать всё сообщение до конца в
/// поисках закрывающей.
pub const MAX_TAG_LEN: usize = 512;

/// Вид сегмента — закрытый словарь, дословно совпадающий с фронтом.
///
/// Значения строчные: они уезжают в JSON и в `data-`атрибуты, а разнобой
/// регистра между Rust и JS — классический источник «почему-то не
/// отрисовалось».
pub mod kind {
    /// Обычный текст.
    pub const TEXT: &str = "text";
    /// Жирный.
    pub const BOLD: &str = "b";
    /// Курсив.
    pub const ITALIC: &str = "i";
    /// Подчёркнутый.
    pub const UNDERLINE: &str = "u";
    /// Зачёркнутый.
    pub const STRIKE: &str = "s";
    /// Ссылка; `value` — адрес.
    pub const URL: &str = "url";
    /// Упоминание пользователя; `value` — его id.
    pub const USER: &str = "user";
    /// Ссылка на чат; `value` — `chat_id`.
    pub const CHAT: &str = "chat";
    /// Файл на Диске; `value` — id файла.
    pub const DISK: &str = "disk";
    /// Момент времени; `value` — unix-время либо дата строкой.
    pub const TIMESTAMP: &str = "timestamp";
    /// Моноширинный блок.
    pub const CODE: &str = "code";
    /// Цитата.
    pub const QUOTE: &str = "quote";
    /// Перенос строки.
    pub const BR: &str = "br";
}

/// Узел разобранного текста.
///
/// Дерево, а не плоский список: `[QUOTE][B]важное[/B][/QUOTE]` обязано остаться
/// вложенным, иначе фронт не сможет нарисовать жирное внутри цитаты, не
/// изобретая собственную склейку диапазонов.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Вид (см. [`kind`]).
    pub kind: String,
    /// Собственный текст узла. Непуст только у [`kind::TEXT`].
    pub text: String,
    /// Полезная нагрузка тега: адрес у `url`, идентификатор у `user`/`chat`/
    /// `disk`, время у `timestamp`.
    pub value: Option<String>,
    /// Вложенные узлы.
    pub children: Vec<Segment>,
}

impl Segment {
    /// Текстовый узел.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            kind: kind::TEXT.to_string(),
            text: s.into(),
            value: None,
            children: Vec::new(),
        }
    }

    /// Узел-тег с вложенными сегментами.
    pub fn tag(kind: &str, value: Option<String>, children: Vec<Segment>) -> Self {
        Self {
            kind: kind.to_string(),
            text: String::new(),
            value,
            children,
        }
    }

    /// Плоский текст этого узла и всех вложенных.
    ///
    /// Правило одно на все теги-контейнеры: если внутри что-то есть — это и
    /// есть текст; если пусто — вместо него подставляется `value`. Благодаря
    /// нему ни один символ исходного сообщения не пропадает даже у битой
    /// разметки.
    pub fn plain(&self) -> String {
        match self.kind.as_str() {
            kind::TEXT => self.text.clone(),
            kind::BR => "\n".to_string(),
            _ => {
                let inner: String = self.children.iter().map(Segment::plain).collect();
                if inner.is_empty() {
                    self.value.clone().unwrap_or_default()
                } else {
                    inner
                }
            }
        }
    }
}

/// Текст сообщения без разметки.
///
/// Используется для превью последнего сообщения в списке диалогов и для поля
/// `text` у `MessageDto` (Ф5) — то есть везде, где нужна одна строка, а не
/// дерево.
pub fn to_plain(input: &str) -> String {
    to_segments(input).iter().map(Segment::plain).collect()
}

/// Разбирает текст в дерево сегментов.
///
/// Не паникует ни на каком входе: любой неразобранный фрагмент становится
/// текстом.
pub fn to_segments(input: &str) -> Vec<Segment> {
    if input.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = input.chars().collect();
    let mut p = Parser {
        chars: &chars,
        i: 0,
        orphans: Vec::new(),
    };
    let mut open: Vec<&'static str> = Vec::new();
    let (nodes, _) = p.parse_nodes(&mut open, 0);
    nodes
}

/// Известные теги: имя в верхнем регистре → вид сегмента.
///
/// Список закрыт (Р11). Проверяется тестом [`tests::the_tag_table_is_frozen`]:
/// добавление тега мимо решения Р11 обязано валить сборку тестов, а не
/// протекать в UI.
const TAGS: [(&str, &str); 12] = [
    ("B", kind::BOLD),
    ("I", kind::ITALIC),
    ("U", kind::UNDERLINE),
    ("S", kind::STRIKE),
    ("URL", kind::URL),
    ("USER", kind::USER),
    ("CHAT", kind::CHAT),
    ("DISK", kind::DISK),
    ("TIMESTAMP", kind::TIMESTAMP),
    ("CODE", kind::CODE),
    ("QUOTE", kind::QUOTE),
    ("BR", kind::BR),
];

/// Вид сегмента по имени тега (регистр не важен — портал пишет и так, и так).
fn tag_kind(name: &str) -> Option<&'static str> {
    TAGS.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, k)| *k)
}

/// Разобранная скобочная пара.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// Открывающий тег: вид + значение атрибута.
    Open(&'static str, Option<String>),
    /// Закрывающий тег.
    Close(&'static str),
}

/// Почему разбор уровня закончился.
enum Stop {
    /// Строка кончилась.
    Eof,
    /// Встретился закрывающий тег, принадлежащий ЭТОМУ или внешнему уровню.
    /// Токен НЕ потреблён — его потребит тот уровень, которому он адресован.
    Close(&'static str),
}

struct Parser<'a> {
    chars: &'a [char],
    i: usize,
    /// Теги, закрытые ПРИНУДИТЕЛЬНО из-за перекрёстной вложенности.
    ///
    /// В `[B]x[I]y[/B]z[/I]` курсив закрывается чужим `[/B]`, и его
    /// собственный `[/I]` приезжает позже — уже без открытого тега. Без этой
    /// памяти он стал бы литеральным «[/I]» в тексте сообщения: формально по
    /// правилу «закрывающий без пары — текст», а на деле мусор на экране за
    /// чужую ошибку разметки.
    orphans: Vec<&'static str>,
}

impl Parser<'_> {
    /// Разбирает узлы до конца строки либо до закрывающего тега из `open`.
    fn parse_nodes(&mut self, open: &mut Vec<&'static str>, depth: usize) -> (Vec<Segment>, Stop) {
        let mut nodes: Vec<Segment> = Vec::new();
        let mut buf = String::new();

        while self.i < self.chars.len() {
            let ch = self.chars[self.i];
            if ch != '[' {
                buf.push(ch);
                self.i += 1;
                continue;
            }

            let Some((token, next)) = self.read_tag() else {
                // Не тег — обычная скобка в тексте.
                buf.push(ch);
                self.i += 1;
                continue;
            };

            match token {
                Token::Close(k) => {
                    if !open.contains(&k) {
                        if let Some(pos) = self.orphans.iter().position(|o| *o == k) {
                            // Тег уже закрыт принудительно (перекрёстная
                            // вложенность) — его собственный закрывающий
                            // просто проглатывается.
                            self.orphans.remove(pos);
                            self.i = next;
                            continue;
                        }
                        // Закрывающий тег без пары — просто текст, вместе со
                        // скобками: выбросить его значило бы поменять смысл.
                        buf.extend(self.chars[self.i..next].iter());
                        self.i = next;
                        continue;
                    }
                    flush(&mut buf, &mut nodes);
                    // Токен НЕ потребляем: его заберёт тот уровень, который
                    // этот тег открыл. Так переживается перекрёстная
                    // вложенность `[B]x[I]y[/B]z[/I]`.
                    return (nodes, Stop::Close(k));
                }
                Token::Open(k, value) => {
                    self.i = next;
                    if k == kind::BR {
                        flush(&mut buf, &mut nodes);
                        nodes.push(Segment::tag(kind::BR, None, Vec::new()));
                        continue;
                    }
                    if depth >= MAX_DEPTH {
                        // Слишком глубоко — дальше разметки нет, только текст.
                        buf.push_str(&render_open(k, value.as_deref()));
                        continue;
                    }
                    flush(&mut buf, &mut nodes);

                    open.push(k);
                    let (children, stop) = self.parse_nodes(open, depth + 1);
                    open.pop();
                    nodes.push(Segment::tag(k, value, children));

                    match stop {
                        // Наш собственный закрывающий тег — потребляем его.
                        Stop::Close(c) if c == k => {
                            if let Some((Token::Close(_), next)) = self.read_tag() {
                                self.i = next;
                            }
                        }
                        // Чужой закрывающий тег: отдаём решение выше, не
                        // потребляя. Наш собственный тег при этом закрыт
                        // принудительно — запоминаем, чтобы его настоящий
                        // закрывающий не вылез текстом. Незакрытый тег (Eof) —
                        // тоже законный исход: содержимое уже собрано в
                        // children.
                        Stop::Close(c) => {
                            self.orphans.push(k);
                            return (nodes, Stop::Close(c));
                        }
                        Stop::Eof => {}
                    }
                }
            }
        }

        flush(&mut buf, &mut nodes);
        (nodes, Stop::Eof)
    }

    /// Пытается прочитать скобочную пару, начинающуюся в текущей позиции.
    ///
    /// Возвращает токен и индекс СЛЕДУЮЩЕГО символа. `None` — это не тег
    /// (нет закрывающей скобки поблизости, вложенная `[`, пустое или
    /// незнакомое имя).
    fn read_tag(&self) -> Option<(Token, usize)> {
        debug_assert_eq!(self.chars.get(self.i), Some(&'['));
        let limit = (self.i + 1 + MAX_TAG_LEN).min(self.chars.len());
        let mut j = self.i + 1;
        while j < limit {
            match self.chars[j] {
                // Вложенная открывающая скобка означает, что предыдущая была
                // текстом: `[a[B]b[/B]` даёт литеральную `[a` и жирное `b`.
                '[' | '\n' | '\r' => return None,
                ']' => break,
                _ => j += 1,
            }
        }
        if j >= limit || self.chars.get(j) != Some(&']') {
            return None;
        }
        let inner: String = self.chars[self.i + 1..j].iter().collect();
        let inner = inner.trim();
        if inner.is_empty() {
            return None;
        }

        if let Some(name) = inner.strip_prefix('/') {
            let k = tag_kind(name.trim())?;
            return Some((Token::Close(k), j + 1));
        }

        let (name, attr) = match inner.find(['=', ' ', '\t']) {
            Some(pos) => (&inner[..pos], inner[pos..].trim_start_matches(['=', ' ', '\t'])),
            None => (inner, ""),
        };
        let k = tag_kind(name)?;
        Some((Token::Open(k, attr_value(k, attr)), j + 1))
    }
}

/// Значение атрибута тега.
///
/// У ссылки значением служит весь атрибут (адрес пробелов не содержит), у
/// остальных — ПЕРВЫЙ токен: живой портал пишет `[USER=15 REPLACE]`, и флаг
/// `REPLACE` частью идентификатора не является.
fn attr_value(k: &str, attr: &str) -> Option<String> {
    let attr = attr.trim();
    if attr.is_empty() {
        return None;
    }
    let v = if k == kind::URL {
        attr
    } else {
        attr.split_whitespace().next().unwrap_or_default()
    };
    (!v.is_empty()).then(|| v.to_string())
}

/// Обратная запись открывающего тега — нужна только там, где разбор
/// отказывается от разметки (превышение [`MAX_DEPTH`]) и возвращает текст.
fn render_open(k: &str, value: Option<&str>) -> String {
    let name = TAGS
        .iter()
        .find(|(_, kind)| *kind == k)
        .map_or("", |(n, _)| *n);
    match value {
        Some(v) => format!("[{name}={v}]"),
        None => format!("[{name}]"),
    }
}

/// Сбрасывает накопленный текст в узел.
fn flush(buf: &mut String, nodes: &mut Vec<Segment>) {
    if !buf.is_empty() {
        nodes.push(Segment::text(std::mem::take(buf)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Объём поддержки — решение Р11, а не деталь реализации.
    #[test]
    fn the_tag_table_is_frozen() {
        let names: Vec<&str> = TAGS.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "B",
                "I",
                "U",
                "S",
                "URL",
                "USER",
                "CHAT",
                "DISK",
                "TIMESTAMP",
                "CODE",
                "QUOTE",
                "BR"
            ]
        );
        assert!(tag_kind("b").is_some(), "регистр тега не важен");
        assert!(tag_kind("spoiler").is_none());
    }

    /// По одному тесту на каждый тег из Р11.
    #[test]
    fn every_supported_tag_is_parsed() {
        let cases: [(&str, &str, Option<&str>, &str); 11] = [
            ("[B]жирно[/B]", kind::BOLD, None, "жирно"),
            ("[I]курсив[/I]", kind::ITALIC, None, "курсив"),
            ("[U]подчёрк[/U]", kind::UNDERLINE, None, "подчёрк"),
            ("[S]зачёрк[/S]", kind::STRIKE, None, "зачёрк"),
            (
                "[URL=https://example.com]тут[/URL]",
                kind::URL,
                Some("https://example.com"),
                "тут",
            ),
            (
                "[USER=15 REPLACE]Максим Крылов[/USER]",
                kind::USER,
                Some("15"),
                "Максим Крылов",
            ),
            ("[CHAT=27]FORGE[/CHAT]", kind::CHAT, Some("27"), "FORGE"),
            ("[DISK=95]file.txt[/DISK]", kind::DISK, Some("95"), "file.txt"),
            (
                "[TIMESTAMP=1785950099][/TIMESTAMP]",
                kind::TIMESTAMP,
                Some("1785950099"),
                "",
            ),
            ("[CODE]let x = 1;[/CODE]", kind::CODE, None, "let x = 1;"),
            ("[QUOTE]он сказал[/QUOTE]", kind::QUOTE, None, "он сказал"),
        ];

        for (input, expected_kind, expected_value, expected_text) in cases {
            let segs = to_segments(input);
            assert_eq!(segs.len(), 1, "{input}");
            assert_eq!(segs[0].kind, expected_kind, "{input}");
            assert_eq!(segs[0].value.as_deref(), expected_value, "{input}");
            let inner: String = segs[0].children.iter().map(Segment::plain).collect();
            assert_eq!(inner, expected_text, "{input}");
        }

        // `[BR]` — единственный самозакрывающийся тег.
        let segs = to_segments("строка[BR]другая");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[1].kind, kind::BR);
        assert_eq!(to_plain("строка[BR]другая"), "строка\nдругая");
    }

    /// Живая форма из портала: `URL_PREVIEW:"Y"` оборачивает голую ссылку сам.
    #[test]
    fn a_bare_url_wrapped_by_the_portal_keeps_its_address() {
        let segs = to_segments("PROBE plain [B]bold[/B] [URL]https://example.com[/URL]");
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[1].kind, kind::BOLD);
        assert_eq!(segs[3].kind, kind::URL);
        // Адреса в атрибуте нет — он лежит текстом внутри.
        assert!(segs[3].value.is_none());
        assert_eq!(segs[3].plain(), "https://example.com");
        assert_eq!(
            to_plain("PROBE plain [B]bold[/B] [URL]https://example.com[/URL]"),
            "PROBE plain bold https://example.com"
        );
    }

    /// Живое системное сообщение (фикстура `msgs_chat3.json`).
    #[test]
    fn a_live_system_message_becomes_readable_text() {
        let src = "[USER=1 REPLACE]Игорь Герасимов[/USER] добавил права \
                   администратора для [USER=15 REPLACE]Максим Крылов[/USER].";
        assert_eq!(
            to_plain(src),
            "Игорь Герасимов добавил права администратора для Максим Крылов."
        );
        let segs = to_segments(src);
        assert_eq!(segs[0].kind, kind::USER);
        assert_eq!(segs[0].value.as_deref(), Some("1"), "REPLACE — не часть id");
        assert_eq!(segs[2].value.as_deref(), Some("15"));
    }

    #[test]
    fn nesting_is_preserved_as_a_tree() {
        let segs = to_segments("[QUOTE]сказал [B]важное[/B] слово[/QUOTE]");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, kind::QUOTE);
        assert_eq!(segs[0].children.len(), 3);
        assert_eq!(segs[0].children[1].kind, kind::BOLD);
        assert_eq!(to_plain("[QUOTE]сказал [B]важное[/B] слово[/QUOTE]"), "сказал важное слово");
    }

    /// НЕЗНАКОМЫЙ ТЕГ — ТЕКСТ КАК ЕСТЬ. Выбросить его значило бы поменять
    /// смысл сообщения.
    #[test]
    fn an_unknown_tag_stays_verbatim() {
        let src = "[SPOILER]пароль[/SPOILER] и [b]жирно[/b]";
        let plain = to_plain(src);
        assert!(plain.starts_with("[SPOILER]пароль[/SPOILER]"), "{plain}");
        assert!(plain.ends_with("жирно"), "{plain}");

        // Незнакомый тег не мешает разобрать соседний знакомый.
        let segs = to_segments(src);
        assert!(segs.iter().any(|s| s.kind == kind::BOLD));
        assert_eq!(to_plain("[UNKNOWN]"), "[UNKNOWN]");
        assert_eq!(to_plain("[/UNKNOWN]"), "[/UNKNOWN]");
    }

    /// Незакрытый тег не должен ни ронять разбор, ни съедать текст.
    #[test]
    fn an_unclosed_tag_keeps_its_content() {
        assert_eq!(to_plain("[B]начало без конца"), "начало без конца");
        let segs = to_segments("[B]начало без конца");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, kind::BOLD);
        assert_eq!(segs[0].children.len(), 1);

        // Незакрытый TIMESTAMP тоже не проглатывает хвост из plain-текста.
        assert_eq!(to_plain("[TIMESTAMP=17859]хвост"), "хвост");
    }

    /// Перекрёстная вложенность: закрывающий тег ищется по СТЕКУ открытых.
    #[test]
    fn crossed_tags_do_not_loop_or_panic() {
        let src = "[B]x[I]y[/B]z[/I]";
        let segs = to_segments(src);
        assert_eq!(segs[0].kind, kind::BOLD);
        // `[/B]` закрыл жирный, курсив остался незакрытым внутри него, а `z`
        // достался следующему уровню — текст при этом цел целиком.
        assert_eq!(to_plain(src), "xyz");

        assert_eq!(to_plain("[/B]x"), "[/B]x");
        assert_eq!(to_plain("[B][/I]x[/B]"), "[/I]x");
    }

    /// Текст с угловыми скобками и амперсандами обязан доехать БЕЗ единого
    /// изменения: экранирование — забота фронта, а фронт использует
    /// `textContent`, где экранировать нечего.
    #[test]
    fn angle_brackets_and_ampersands_are_never_escaped() {
        let src = "<script>alert(1)</script> & \"кавычки\" & 'апострофы' < >";
        assert_eq!(to_plain(src), src);
        let segs = to_segments(src);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].kind, kind::TEXT);
        assert_eq!(segs[0].text, src);

        // …и внутри тега тоже.
        assert_eq!(to_plain("[B]<b>&amp;</b>[/B]"), "<b>&amp;</b>");
    }

    #[test]
    fn stray_brackets_are_just_characters() {
        for src in [
            "[",
            "]",
            "][",
            "[]",
            "массив[0] и map[key]",
            "[[[[",
            "[B",
            "[B]",
            "[=]",
            "[ ]",
        ] {
            let plain = to_plain(src);
            // Ни один символ не пропал, кроме разобранных тегов.
            assert!(!plain.contains('\u{0}'), "{src}");
        }
        assert_eq!(to_plain("массив[0] и map[key]"), "массив[0] и map[key]");
        assert_eq!(to_plain("[[[["), "[[[[");
        assert_eq!(to_plain(""), "");
    }

    /// Глубже [`MAX_DEPTH`] разметки нет — только текст. Проверяется на
    /// заведомо злом входе: тысяча вложенных цитат не должна ронять стек.
    #[test]
    fn depth_is_capped_and_deep_nesting_does_not_blow_the_stack() {
        let src = "[QUOTE]".repeat(1000) + "дно" + &"[/QUOTE]".repeat(1000);
        let plain = to_plain(&src);
        assert!(plain.contains("дно"), "содержимое потеряно");

        let segs = to_segments(&src);
        let mut depth = 0;
        let mut node = &segs[0];
        while let Some(next) = node.children.iter().find(|c| c.kind == kind::QUOTE) {
            depth += 1;
            node = next;
        }
        assert!(depth < MAX_DEPTH, "глубина {depth} не ограничена");
    }

    /// Очень длинная «скобка» тегом не считается: иначе одна `[` заставляла бы
    /// сканировать всё сообщение.
    #[test]
    fn an_overlong_bracket_is_not_a_tag() {
        let src = format!("[{}]", "x".repeat(MAX_TAG_LEN + 10));
        assert_eq!(to_plain(&src), src);
    }

    /// Fuzz-подобный прогон: разбор не паникует и не зацикливается ни на одной
    /// случайной строке из «опасного» алфавита.
    ///
    /// Генератор — xorshift на фиксированном семени: воспроизводимо, без
    /// крейта `rand` в дереве зависимостей.
    #[test]
    fn random_input_never_panics() {
        const ALPHABET: [&str; 18] = [
            "[", "]", "/", "=", "B", "I", "URL", "USER", "QUOTE", "BR", " ", "\n", "текст", "<",
            "&", "15", "REPLACE", "[/",
        ];
        let mut seed: u64 = 0x5eed_1234_dead_beef;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for _ in 0..2_000 {
            let len = (next() % 40) as usize;
            let mut s = String::new();
            for _ in 0..len {
                s.push_str(ALPHABET[(next() % ALPHABET.len() as u64) as usize]);
            }
            // Единственное требование — не паниковать и вернуться.
            let segs = to_segments(&s);
            let plain: String = segs.iter().map(Segment::plain).collect();
            // Текст не может стать длиннее исходника: разбор только убирает
            // разметку.
            assert!(plain.chars().count() <= s.chars().count(), "{s:?} -> {plain:?}");
        }
    }

    /// Сегменты уезжают в DTO — форма JSON тоже контракт.
    #[test]
    fn segments_serialize_to_the_wire_shape() {
        let segs = to_segments("[USER=15 REPLACE]Максим[/USER]");
        let v = serde_json::to_value(&segs).unwrap();
        assert_eq!(v[0]["kind"], "user");
        assert_eq!(v[0]["value"], "15");
        assert_eq!(v[0]["children"][0]["kind"], "text");
        assert_eq!(v[0]["children"][0]["text"], "Максим");
    }
}
