//! Нормализация текста сообщений человека и признаки «автопилота».
//!
//! ### Зачем отдельный модуль
//!
//! Метрики разнообразия (чарт 11 плана) — TTR, доля повторяющихся промптов,
//! доля слэш-команд и доля односложных подтверждений — считаются по одному и
//! тому же тексту, и все четыре ломаются от малейшего расхождения в правилах
//! нормализации: «Ок.» и «ок» обязаны быть одним и тем же промптом, иначе
//! доля повторов занижена, а доля подтверждений — не полна. Поэтому правила
//! живут в одном месте, а не размазаны по разбору и роллапу.
//!
//! ### Правила (зафиксированы, менять только вместе с версией кэша)
//!
//! * **Слово** — максимальная последовательность алфавитно-цифровых символов
//!   (`char::is_alphanumeric`, то есть кириллица и цифры считаются наравне с
//!   латиницей), приведённая к нижнему регистру. Стоп-слова НЕ отбрасываются:
//!   «я», «и», «the» — такая же часть словаря человека, а список стоп-слов
//!   для смеси русского с английским и с именами файлов был бы произволом.
//! * **TTR за день** = число различных слов / общее число слов во ВСЕХ
//!   настоящих сообщениях человека за день. Падение TTR — признак того, что
//!   человек пишет всё более однообразно (автопилот).
//! * **Нормализованный промпт** — слова сообщения, приведённые к нижнему
//!   регистру и склеенные одним пробелом. Пунктуация и переносы строк
//!   игнорируются, поэтому «продолжай!» и «Продолжай» — один промпт.
//! * **Слэш-команда** — сообщение, первый непробельный символ которого `/`.
//!   Слэш-команда НИКОГДА не считается односложным подтверждением, даже если
//!   её имя попало бы в белый список: `/continue` — это команда, а не «ок».
//! * **Односложное подтверждение** — сообщение не длиннее трёх слов, чей
//!   нормализованный текст целиком лежит в белом списке [`ACK_PHRASES`], либо
//!   состоящее из одних плюсов («+», «++»). Это прямой индикатор работы на
//!   автопилоте: человек соглашается, не формулируя.
//!
//! ### Три роли записи `user`
//!
//! Структурный фильтр (`type == "user"`, не `isMeta`, не `isSidechain`, в
//! контенте нет `tool_result`) пропускает не только реплики человека. В живом
//! архиве под него подпадают ещё три вида записей, и все три до Фазы 7
//! считались сообщениями: прерывания (`[Request interrupted by user]`, 91
//! запись), слэш-команды в разметке `<command-name>…` (161) и вывод этих
//! команд `<local-command-stdout>` (148). Роль записи определяет
//! [`classify_user_text`]; поля `toolUseResult.interrupted`, на которое
//! рассчитывал план, в архиве нет ни разу.
//!
//! Полный текст сообщений здесь не сохраняется и наружу не отдаётся: модуль
//! возвращает только признаки и 64-битные хеши, а в [`super::rollup`]
//! оседают исключительно агрегаты.

/// Белый список односложных подтверждений в нормализованном виде.
///
/// Список намеренно закрытый и короткий: сюда попадают только реплики,
/// которые не несут ни одной инструкции — чистое «согласен, продолжай».
/// «Давай сделаем X» под это правило не подпадает (больше трёх слов и не в
/// списке), и это верно: там человек всё-таки думал.
const ACK_PHRASES: [&str; 46] = [
    // Русский
    "да",
    "да да",
    "да давай",
    "ага",
    "угу",
    "ок",
    "ок давай",
    "окей",
    "оке",
    "ладно",
    "хорошо",
    "норм",
    "нормально",
    "отлично",
    "супер",
    "класс",
    "ясно",
    "понял",
    "понятно",
    "давай",
    "давай дальше",
    "дальше",
    "продолжай",
    "продолжай дальше",
    "продолжаем",
    "продолжи",
    "поехали",
    "погнали",
    "вперед",
    "вперёд",
    "го",
    "спасибо",
    "верно",
    "точно",
    // Английский
    "ok",
    "okay",
    "k",
    "y",
    "yes",
    "yep",
    "yeah",
    "sure",
    "go",
    "go ahead",
    "continue",
    "proceed",
];

/// Маркер прерывания хода человеком.
///
/// CLI пишет прерывание ОБЫЧНОЙ записью `user` с текстовым блоком
/// `[Request interrupted by user]` (вариант — `… for tool use`), а вовсе не
/// полем `toolUseResult.interrupted`, которого в живом архиве нет ни разу.
/// Без этой проверки прерывания считались бы сообщениями человека: и счётчик
/// сообщений завышался бы, и пауза «читает ли код» мерилась бы до момента,
/// когда человек НЕ дописывал задачу, а обрывал ход.
const INTERRUPT_PREFIX: &str = "[Request interrupted by user";

/// Открывающие теги записей, которые CLI подставляет от имени пользователя.
///
/// `<local-command-stdout>` — это вывод слэш-команды, а не реплика человека;
/// такие записи приходят с `isMeta` не выставленным и структурный фильтр их
/// не ловит.
const CLI_OUTPUT_TAGS: [&str; 3] = [
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<command-stdout>",
];

/// Открывающий тег слэш-команды.
///
/// Слэш-команду CLI записывает НЕ текстом «/clear», а разметкой
/// `<command-name>/clear</command-name><command-message>…`. Считать её текст
/// как обычную реплику нельзя: имена тегов раздували бы словарь TTR, а сама
/// команда не попадала бы в долю слэш-команд.
const COMMAND_NAME_OPEN: &str = "<command-name>";
const COMMAND_NAME_CLOSE: &str = "</command-name>";

/// Чем на самом деле является запись `user`, прошедшая структурный фильтр
/// [`super::parse::Record::is_human_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRecord {
    /// Настоящая реплика человека.
    Message(HumanText),
    /// Человек оборвал ход (`[Request interrupted by user]`).
    Interruption,
    /// Вывод слэш-команды, подставленный CLI от имени пользователя.
    CliOutput,
}

/// Разбирает текст записи `user` в одну из трёх ролей.
///
/// Правила выведены из живого архива (см. константы выше) и проверены на нём
/// же: 91 запись прерывания, 161 слэш-команда, 148 записей вывода команд —
/// все три вида до этой проверки считались репликами человека.
pub fn classify_user_text(text: &str) -> UserRecord {
    let trimmed = text.trim_start();
    if trimmed.starts_with(INTERRUPT_PREFIX) {
        return UserRecord::Interruption;
    }
    if CLI_OUTPUT_TAGS.iter().any(|t| trimmed.starts_with(t)) {
        return UserRecord::CliOutput;
    }
    if trimmed.starts_with(COMMAND_NAME_OPEN) {
        let mut c = classify(&command_text(trimmed));
        c.len = text.chars().count();
        c.is_slash = true;
        // Команда — это команда, а не «ок»: даже `/continue` не должен
        // попадать в долю односложных подтверждений.
        c.is_ack = false;
        return UserRecord::Message(c);
    }
    UserRecord::Message(classify(text))
}

/// Извлекает имя слэш-команды из разметки `<command-name>…</command-name>`.
///
/// Всё остальное (`<command-message>`, `<command-args>`) отбрасывается: имена
/// тегов и служебные слова не должны попадать в словарь TTR.
fn command_text(trimmed: &str) -> String {
    let rest = &trimmed[COMMAND_NAME_OPEN.len()..];
    match rest.find(COMMAND_NAME_CLOSE) {
        Some(end) => rest[..end].to_string(),
        None => rest.to_string(),
    }
}

/// Признаки одного настоящего сообщения человека.
///
/// Собираются один раз при разборе строки транскрипта и сразу же
/// «схлопываются» в счётчики дневного бакета — сам текст никуда не уезжает.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HumanText {
    /// Длина сообщения в символах (чарт 9 «длина сообщений»).
    pub len: usize,
    /// Хеши слов сообщения — сырьё оценки числа различных слов за день.
    pub word_hashes: Vec<u64>,
    /// Сколько всего слов в сообщении (знаменатель TTR).
    pub word_count: u64,
    /// Хеш нормализованного текста — ключ подсчёта повторов.
    pub prompt_hash: u64,
    /// Сообщение начинается со слэша.
    pub is_slash: bool,
    /// Сообщение — односложное подтверждение.
    pub is_ack: bool,
}

/// Разбирает текст сообщения человека в набор признаков.
pub fn classify(text: &str) -> HumanText {
    let words = word_tokens(text);
    let normalized = words.join(" ");
    let is_slash = is_slash_command(text);
    HumanText {
        len: text.chars().count(),
        word_hashes: words.iter().map(|w| hash64(w)).collect(),
        word_count: words.len() as u64,
        prompt_hash: hash64(&normalized),
        is_slash,
        is_ack: !is_slash && is_ack(text, &words, &normalized),
    }
}

/// Слова сообщения в нижнем регистре (см. правило «Слово» в шапке модуля).
pub fn word_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.extend(ch.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Первый непробельный символ — `/`.
pub fn is_slash_command(text: &str) -> bool {
    text.trim_start().starts_with('/')
}

/// Односложное подтверждение (слэш-команды сюда не попадают — проверяется
/// вызывающим).
fn is_ack(raw: &str, words: &[String], normalized: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    // «+» / «++» — подтверждение без единой буквы, слов в нём нет вовсе.
    if trimmed.chars().all(|c| c == '+') {
        return true;
    }
    if words.is_empty() || words.len() > 3 {
        return false;
    }
    ACK_PHRASES.contains(&normalized)
}

/// 64-битный хеш строки (FNV-1a + финализатор SplitMix64).
///
/// Своя функция, а не `DefaultHasher`: значения хеша уезжают в снимок кэша,
/// и они обязаны совпадать между запусками и версиями компилятора —
/// `RandomState` этого не гарантирует, а `DefaultHasher` не обещает
/// стабильности алгоритма между релизами std.
///
/// Финализатор нужен потому, что FNV-1a плохо перемешивает старшие биты, а
/// оценка числа различных слов ([`super::rollup::Hll`]) берёт индекс регистра
/// именно из них.
pub fn hash64(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    // SplitMix64 finalizer.
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_lowercased_and_split_by_non_alphanumerics() {
        assert_eq!(
            word_tokens("Привет, Мир! foo_bar-42"),
            vec!["привет", "мир", "foo", "bar", "42"]
        );
    }

    #[test]
    fn short_confirmation_is_an_ack() {
        for t in ["ок", "Ок.", "ОК!", "да, давай", "continue", "+", "++"] {
            assert!(classify(t).is_ack, "{t:?} должно считаться подтверждением");
        }
    }

    #[test]
    fn slash_command_is_never_an_ack() {
        let c = classify("/clear");
        assert!(c.is_slash);
        assert!(!c.is_ack);
        let c = classify("  /continue");
        assert!(c.is_slash);
        assert!(!c.is_ack);
    }

    #[test]
    fn a_real_request_is_neither_ack_nor_slash() {
        let c = classify("Давай перепишем разбор транскриптов на потоковый режим");
        assert!(!c.is_ack);
        assert!(!c.is_slash);
        assert_eq!(c.word_count, 7);
    }

    #[test]
    fn interruption_record_is_not_a_human_message() {
        assert_eq!(
            classify_user_text("[Request interrupted by user]"),
            UserRecord::Interruption
        );
        assert_eq!(
            classify_user_text("[Request interrupted by user for tool use]"),
            UserRecord::Interruption
        );
    }

    #[test]
    fn local_command_output_is_not_a_human_message() {
        assert_eq!(
            classify_user_text("<local-command-stdout>ветка master</local-command-stdout>"),
            UserRecord::CliOutput
        );
    }

    #[test]
    fn slash_command_markup_counts_as_a_slash_command_and_not_as_words_of_tags() {
        let rec = classify_user_text(
            "<command-name>/effort</command-name>\n<command-message>effort</command-message>\n<command-args></command-args>",
        );
        let UserRecord::Message(c) = rec else {
            panic!("слэш-команда — это сообщение человека");
        };
        assert!(c.is_slash);
        assert!(!c.is_ack);
        // В словарь TTR попало только имя команды, а не имена тегов.
        assert_eq!(c.word_count, 1);
    }

    #[test]
    fn punctuation_and_case_do_not_change_the_prompt_hash() {
        assert_eq!(classify("Продолжай!").prompt_hash, classify("продолжай").prompt_hash);
        assert_ne!(classify("продолжай").prompt_hash, classify("стоп").prompt_hash);
    }
}
