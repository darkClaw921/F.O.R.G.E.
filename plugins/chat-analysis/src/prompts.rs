//! Пер-чатовое хранилище пользовательского контекста для анализа чата.
//!
//! Пользователь пишет к КАЖДОЙ беседе свою заметку («это заказчик, общаться
//! формально», «здесь про переезд склада»), и она подмешивается в запрос к
//! Claude. Хранится всё в отдельном файле — по одному на плагин
//! (`~/.forge/vk-chat-prompts.json`, `~/.forge/telegram-chat-prompts.json`,
//! `~/.forge/bitrix-chat-prompts.json`).
//!
//! **Путь — ПАРАМЕТР [`load`] и [`save`], а не константа модуля.** Иначе три
//! мессенджера писали бы заметки о разных людях в один файл, а ключом там
//! служит `dialog_id`, который у них совпадает по написанию и не совпадает по
//! смыслу. Плагин берёт путь из своего конфига (env-override там же).
//!
//! ```json
//! { "version": 1, "chats": { "2000000001": {
//!     "prompt": "…", "updated_at": 1800000000,
//!     "analysis": { "text": "…", "last_message_id": 7042,
//!                   "messages_used": 118, "updated_at": 1800000500 },
//!     "thread": [ { "q": "а что со сроками?", "a": "…", "at": 1800000600 } ]
//! } } }
//! ```
//!
//! # Последний разбор лежит РЯДОМ с заметкой
//!
//! Поле `analysis` ([`ChatAnalysis`]) — это результат последнего успешного
//! анализа беседы. Он хранится на бэкенде, а не во вкладке, по трём причинам:
//! разбор переживает закрытие попапа и перезагрузку страницы, виден со второго
//! устройства и — главное — служит ВХОДОМ инкрементального обновления
//! (`kind: "refresh"` отдаёт модели прошлый разбор плюс только новые
//! сообщения). Место выбрано то же, что у заметки: обе величины пер-чатовые,
//! обе приватные и обе обязаны переживать перезапуск, а второй файл рядом
//! означал бы вторую блокировку и два источника истины на одну беседу.
//!
//! Поле ОПЦИОНАЛЬНОЕ, и версия файла остаётся `1`: старый файл (без
//! `analysis`) читается без потерь, а запись анализа не трогает заметку и
//! наоборот — правки идут по разным полям одной записи под одним `Mutex`.
//!
//! # Тред вопросов лежит ТАМ ЖЕ
//!
//! Поле `thread` ([`QaTurn`]) — история вопросов пользователя к беседе и
//! ответов модели. Причина та же и решение то же, что у `analysis`: тред обязан
//! пережить перезагрузку страницы и жить до явного «CLEAR Q&A», а держать его
//! в памяти вкладки означало терять его на каждом F5.
//!
//! Три половины записи (`prompt` — заметка, `analysis` — разбор, `thread` —
//! тред) НЕЗАВИСИМЫ во всех операциях: правка или очистка одной не трогает две
//! другие, и это проверяется тестами на обоих уровнях (хранилище и HTTP).
//! Запись живёт, пока жива хотя бы одна из трёх.
//!
//! # Три решения формата
//!
//! 1. **Отдельный файл, а не ключ в конфиге плагина.** Конфиг содержит токен и
//!    переписывается целиком при логине/логауте; смешивать секрет и растущие
//!    пользовательские тексты в одном файле — значит рисковать токеном на
//!    каждом сохранении заметки.
//! 2. **Ключ — `dialog_id` СТРОКОЙ**, а не числом. Это тот же инвариант «id —
//!    строки», что и в WS-протоколе каждого плагина, и в его DTO: одно
//!    написание идентификатора на всех уровнях, иначе ключ фронта
//!    (`"2000000001"`) и ключ файла (`2000000001`) разъехались бы на JS-числах.
//! 3. **Плоская мапа**, а не файл на чат. Правка одного чата не переписывает
//!    соседей (мапа читается целиком, меняется одна запись, пишется целиком
//!    атомарно), а 500 мелких файлов в `~/.forge` — это каталог, который
//!    невозможно ни просмотреть, ни забэкапить руками.
//!
//! # Почему [`load`] не возвращает `Result`
//!
//! Сломанный или чужой файл не должен ломать вкладку: единственная разумная
//! реакция на мусор — warn-лог и ПУСТОЕ хранилище, после чего пользователь
//! просто заново напишет заметку и перезапишет файл. Ровно та же логика — у
//! чтения конфига в каждом из плагинов.
//!
//! # Инвариант утечки
//!
//! Тексты заметок — это записи пользователя о КОНКРЕТНЫХ ЛЮДЯХ. Поэтому:
//! файл пишется с правами `0600` (через единую
//! [`crate::atomic::write_bytes_atomic`]), а в `tracing` попадают только
//! длины и количества, НИКОГДА не сам текст.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Версия формата файла. Файл с другой версией читается как ПУСТОЙ.
///
/// Молчаливая миграция «прочитать как получится» опаснее потери заметок:
/// формат будущей версии мы по определению не знаем, а записать поверх него
/// свою интерпретацию — значит уничтожить данные новее наших.
pub const PROMPTS_VERSION: u32 = 1;

/// Потолок длины одной заметки — в CODE POINTS, а не в байтах.
///
/// 4000 символов это ~1000 токенов: заметно меньше самого транскрипта и при
/// этом достаточно для развёрнутого контекста. Единица измерения — символы,
/// потому что байтовый лимит на кириллице означал бы вдвое меньший реальный
/// лимит, а байтовый СРЕЗ — панику посреди символа.
pub const MAX_PROMPT_CHARS: usize = 4_000;

/// Потолок сохраняемого разбора — в CODE POINTS.
///
/// 12 000 символов это ~3 000 токенов: полный структурированный разбор (пять
/// пунктов `TASK_ANALYZE`) укладывается с запасом, а в промпт REFRESH такой
/// блок влезает, не вытесняя транскрипт. Единица — символы по той же причине,
/// что и у [`MAX_PROMPT_CHARS`]: байтовый срез на кириллице паникует посреди
/// символа.
pub const MAX_ANALYSIS_CHARS: usize = 12_000;

/// Сколько пар «вопрос — ответ» хранится в треде ОДНОЙ беседы.
///
/// # Почему хранимый тред больше того, что уходит модели
///
/// В промпт `ask` едет не весь тред, а его хвост:
/// [`crate::prompt::MAX_HISTORY_TURNS`] = 6 пар и
/// [`crate::prompt::MAX_HISTORY_CHARS`] = 8 000
/// символов ([`crate::prompt`], `clamp_history`). Разница НАМЕРЕННАЯ, а не
/// рассинхрон констант: у двух чисел разные задачи. Тред на экране — это
/// история переписки пользователя с моделью, и она обязана пережить
/// перезагрузку страницы целиком, до нажатия «CLEAR Q&A». Тред в промпте — это
/// расход контекстного окна и денег на каждом вопросе, и седьмая пара давности
/// в нём почти всегда только вытесняет транскрипт. Поэтому: помним больше, чем
/// отправляем — 12 пар против 6 и 10 000 символов против 8 000.
pub const MAX_THREAD_TURNS: usize = 12;

/// Потолок СУММАРНОЙ длины треда одной беседы (все `q` + все `a`) — в CODE
/// POINTS. Сверх лимита выбрасываются самые СТАРЫЕ пары.
///
/// # Почему 10 000, а не 12 000 из плана
///
/// Эта величина входит множителем в худший случай размера файла (см.
/// [`MAX_FILE_BYTES`]): каждая тысяча символов треда — это ещё 2 МиБ потолка,
/// который проверяется до чтения файла ЦЕЛИКОМ в память. С 12 000 потолок
/// пришлось бы поднять до 72 МиБ, и это уже не «граница защиты от подсунутого
/// мусора», а обещание однажды прочитать 72 МиБ JSON. 10 000 оставляет потолок
/// на 64 МиБ и при этом сохраняет инвариант «помним больше, чем отправляем»:
/// 10 000 > `MAX_HISTORY_CHARS` (8 000), как 12 пар > 6 пар.
///
/// Единица — символы, а не байты, по той же причине, что и у
/// [`MAX_PROMPT_CHARS`]: байтовый СРЕЗ на кириллице паникует посреди символа.
pub const MAX_THREAD_CHARS: usize = 10_000;

/// Потолок ОДНОГО поля пары (`q` либо `a`) — половина бюджета треда.
///
/// Нужен ради инварианта «последняя пара всегда влезает»: без него один
/// гигантский ответ модели не помещался бы в [`MAX_THREAD_CHARS`] даже
/// в одиночестве, и обрезка со старых пар выбрасывала бы весь тред, включая
/// только что заданный вопрос. Половина бюджета гарантирует `q + a <=
/// MAX_THREAD_CHARS` для любой пары.
pub const MAX_TURN_CHARS: usize = MAX_THREAD_CHARS / 2;

/// Сколько бесед помнит хранилище. Сверх лимита вытесняется самая старая по
/// `updated_at`: файл не должен расти бесконечно от одного лишь пролистывания
/// вкладки.
pub const MAX_CHATS: usize = 500;

/// Потолок размера файла. Больше — читаем как пустой, не разбирая.
///
/// Величина выведена из ХУДШЕГО случая, а не из наблюдений: одна запись это
/// `MAX_PROMPT_CHARS + MAX_ANALYSIS_CHARS + MAX_THREAD_CHARS` =
/// 4 000 + 12 000 + 10 000 = 26 000 code points, каждый из которых в UTF-8
/// занимает до 4 байт, и таких записей может быть [`MAX_CHATS`]:
/// 500 × 26 000 × 4 = 52 000 000 байт полезной нагрузки. Сверху — накладные
/// расходы JSON (ключи, кавычки, экранирование, отступы `to_vec_pretty`),
/// которые на текстах такого размера не превышают четверти: 52 000 000 × 1.25
/// = 65 000 000 байт. Отсюда потолок 64 МиБ = 67 108 864 байт (тест
/// `the_file_cap_covers_the_worst_case` держит эту арифметику целиком).
///
/// Дальше потолок не растёт: вместо этого снижен [`MAX_THREAD_CHARS`] (см. его
/// доккомментарий) — файл читается в память ЦЕЛИКОМ, и обещать себе чтение
/// сотни мегабайт ради худшего случая, которого в реальности не бывает, хуже,
/// чем помнить чуть более короткий тред.
///
/// Проверка идёт ДО чтения в память: иначе подсунутый гигабайт съел бы RAM
/// процесса ещё до разбора. Реальный файл при этом на два-три порядка меньше
/// потолка — это граница защиты, а не ожидаемый размер.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Последний успешный разбор беседы.
///
/// Пишется ТОЛЬКО по успешно завершённому run'у `analyze`/`refresh`
/// ([`crate::hub`]): оборванный отменой или таймаутом текст не имеет
/// права стать «прошлым разбором», иначе следующий REFRESH получил бы на вход
/// огрызок и повторил бы его в ответе.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAnalysis {
    /// Текст разбора — уже прошедший [`sanitize_analysis`].
    pub text: String,
    /// Максимальный id сообщения, попавшего в разбор. Точка отсчёта
    /// инкрементального обновления: REFRESH выгружает только `id >` этого.
    pub last_message_id: i32,
    /// Сколько сообщений было в транскрипте разбора.
    pub messages_used: u32,
    /// Unix-время записи.
    pub updated_at: i64,
}

/// Пара «вопрос пользователя — ответ модели» сохранённого треда Q&A.
///
/// Пишется ТОЛЬКО по успешно завершённому run'у `ask` ([`crate::hub`]) — по
/// той же логике, по которой оборванный текст не становится «прошлым
/// разбором»: половина ответа в треде вводила бы в заблуждение и человека, и
/// следующий вопрос (тред уходит модели как контекст).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QaTurn {
    /// Вопрос пользователя — уже прошедший [`sanitize_turn`].
    pub q: String,
    /// Ответ модели — уже прошедший [`sanitize_turn`].
    pub a: String,
    /// Unix-время записи пары.
    pub at: i64,
}

/// Запись беседы: заметка пользователя, последний разбор и тред вопросов.
///
/// Все три части независимы: запись анализа не трогает `prompt`, запись
/// заметки не трогает `analysis`, правка треда не трогает ни того, ни другого.
/// Запись существует, пока жива хотя бы одна из них.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPrompt {
    /// Текст заметки — уже прошедший [`sanitize`]. Пустая строка означает
    /// «заметки нет» (запись при этом может жить ради `analysis`).
    #[serde(default)]
    pub prompt: String,
    /// Unix-время последней записи. Ось вытеснения по [`MAX_CHATS`] и
    /// значение, которое видит фронт («сохранено тогда-то»).
    pub updated_at: i64,
    /// Последний успешный разбор, если он есть.
    ///
    /// `skip_serializing_if` — не косметика: без него у КАЖДОЙ беседы, где
    /// пользователь только написал заметку, появилось бы `"analysis": null`, и
    /// файл старого формата перестал бы совпадать сам с собой после первой же
    /// перезаписи.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<ChatAnalysis>,
    /// Тред вопросов к беседе, от СТАРЫХ к новым.
    ///
    /// `skip_serializing_if` по той же причине, что и у `analysis`: у беседы,
    /// где вопросов не задавали, в файле не должно появиться `"thread": []` —
    /// иначе файл, написанный до этой доработки, перестал бы совпадать сам с
    /// собой после первой же перезаписи.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thread: Vec<QaTurn>,
}

/// Содержимое файла целиком.
///
/// `BTreeMap`, а не `HashMap`, намеренно: порядок ключей в файле обязан быть
/// стабильным, иначе каждое сохранение одной заметки давало бы полностью
/// перетасованный файл — нечитаемый глазами и бесполезный в `diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptStore {
    /// Версия формата; при чтении обязана совпасть с [`PROMPTS_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    /// `dialog_id` (строкой) → заметка.
    #[serde(default)]
    pub chats: BTreeMap<String, ChatPrompt>,
}

fn default_version() -> u32 {
    PROMPTS_VERSION
}

impl Default for PromptStore {
    fn default() -> Self {
        Self {
            version: PROMPTS_VERSION,
            chats: BTreeMap::new(),
        }
    }
}

impl PromptStore {
    /// Заметка беседы, если она есть.
    #[must_use]
    pub fn get(&self, dialog_id: &str) -> Option<&ChatPrompt> {
        self.chats.get(dialog_id)
    }

    /// Текст заметки беседы (пустая строка, если её нет) — форма, удобная
    /// сборщику промпта (Ф2) и роуту `GET`.
    #[must_use]
    pub fn text(&self, dialog_id: &str) -> &str {
        self.chats.get(dialog_id).map_or("", |c| c.prompt.as_str())
    }

    /// Сохранённый разбор беседы, если он есть.
    #[must_use]
    pub fn analysis(&self, dialog_id: &str) -> Option<&ChatAnalysis> {
        self.chats.get(dialog_id).and_then(|c| c.analysis.as_ref())
    }

    /// Сохранённый тред вопросов беседы (пустой срез, если его нет).
    #[must_use]
    pub fn thread(&self, dialog_id: &str) -> &[QaTurn] {
        self.chats
            .get(dialog_id)
            .map_or(&[][..], |c| c.thread.as_slice())
    }

    /// Записывает заметку беседы.
    ///
    /// Сырой текст проходит [`sanitize`]. ПУСТОЙ результат означает стирание
    /// заметки: «стереть контекст» и «сохранить пустой контекст» — это одно и
    /// то же намерение пользователя, и хранить пустышку значило бы занимать
    /// слот вытеснения ничем. Запись при этом ВЫЖИВАЕТ, если в ней лежит
    /// разбор: стереть заметку и потерять разбор — два разных действия
    /// пользователя, и у каждого своя кнопка.
    ///
    /// Ни разбор, ни тред не трогаются ни в одной ветке.
    ///
    /// Возвращает сохранённую запись либо `None`, если заметка была стёрта.
    pub fn set(&mut self, dialog_id: &str, raw: &str, now: i64) -> Option<ChatPrompt> {
        let prompt = sanitize(raw);
        if prompt.is_empty() {
            // Запись ВЫЖИВАЕТ, пока в ней жива хоть одна из двух других
            // половин: стереть заметку — не то же самое, что стереть разбор
            // или тред, и у каждого действия своя кнопка.
            let mut orphaned = false;
            if let Some(entry) = self.chats.get_mut(dialog_id) {
                entry.prompt = String::new();
                entry.updated_at = now;
                orphaned = entry.analysis.is_none() && entry.thread.is_empty();
            }
            if orphaned {
                self.chats.remove(dialog_id);
            }
            return None;
        }
        let entry = self.chats.entry(dialog_id.to_string()).or_insert(ChatPrompt {
            prompt: String::new(),
            updated_at: now,
            analysis: None,
            thread: Vec::new(),
        });
        entry.prompt = prompt;
        entry.updated_at = now;
        let saved = entry.clone();
        self.evict();
        Some(saved)
    }

    /// Добавляет пару «вопрос — ответ» в конец треда беседы.
    ///
    /// Пара проходит [`sanitize_turn`] по каждому полю; пустой вопрос ЛИБО
    /// пустой ответ означают, что сохранять нечего (оборванный run, ответ из
    /// одних пробелов) — пара не добавляется.
    ///
    /// Тред обрезается со СТАРЫХ пар сразу по двум осям: [`MAX_THREAD_TURNS`]
    /// пар и [`MAX_THREAD_CHARS`] суммарных символов. Последняя (только что
    /// добавленная) пара остаётся всегда — этот инвариант держится
    /// [`MAX_TURN_CHARS`].
    ///
    /// Ни заметка, ни разбор при этом не трогаются. `updated_at` записи
    /// двигается, как у [`Self::set`] и [`Self::set_analysis`]: беседа, где
    /// сегодня задавали вопросы, не должна вылететь по [`MAX_CHATS`] из-за
    /// того, что заметку к ней писали год назад.
    ///
    /// Возвращает сохранённую пару либо `None`, если сохранять было нечего.
    pub fn push_turn(&mut self, dialog_id: &str, q: &str, a: &str, now: i64) -> Option<QaTurn> {
        let q = sanitize_turn(q);
        let a = sanitize_turn(a);
        if q.is_empty() || a.is_empty() {
            return None;
        }
        let turn = QaTurn { q, a, at: now };
        let entry = self.chats.entry(dialog_id.to_string()).or_insert(ChatPrompt {
            prompt: String::new(),
            updated_at: now,
            analysis: None,
            thread: Vec::new(),
        });
        entry.thread.push(turn.clone());
        entry.updated_at = now;
        clamp_thread(&mut entry.thread);
        self.evict();
        Some(turn)
    }

    /// Стирает ТОЛЬКО тред беседы (кнопка «CLEAR Q&A»). Заметку и разбор не
    /// трогает; запись, лишившаяся всех трёх половин, удаляется целиком.
    ///
    /// Возвращает `true`, если тред действительно был непустым.
    pub fn clear_thread(&mut self, dialog_id: &str) -> bool {
        let Some(entry) = self.chats.get_mut(dialog_id) else {
            return false;
        };
        let had = !entry.thread.is_empty();
        entry.thread.clear();
        if entry.prompt.is_empty() && entry.analysis.is_none() {
            self.chats.remove(dialog_id);
        }
        had
    }

    /// Записывает разбор беседы, НЕ трогая заметку.
    ///
    /// Текст проходит [`sanitize_analysis`] (обрезка до [`MAX_ANALYSIS_CHARS`]
    /// по code points). Пустой результат означает то же, что и отсутствие
    /// разбора, — запись анализа снимается (см. [`Self::clear_analysis`]).
    ///
    /// Возвращает сохранённый разбор либо `None`, если сохранять было нечего.
    pub fn set_analysis(
        &mut self,
        dialog_id: &str,
        raw: &str,
        last_message_id: i32,
        messages_used: u32,
        now: i64,
    ) -> Option<ChatAnalysis> {
        let text = sanitize_analysis(raw);
        if text.is_empty() {
            self.clear_analysis(dialog_id);
            return None;
        }
        let analysis = ChatAnalysis {
            text,
            last_message_id,
            messages_used,
            updated_at: now,
        };
        let entry = self.chats.entry(dialog_id.to_string()).or_insert(ChatPrompt {
            prompt: String::new(),
            updated_at: now,
            analysis: None,
            thread: Vec::new(),
        });
        entry.analysis = Some(analysis.clone());
        // `updated_at` записи — это ось вытеснения по [`MAX_CHATS`], а не
        // «когда сохранена заметка»: беседа, которую разбирали сегодня, не
        // должна вылететь из-за того, что заметку к ней писали год назад.
        entry.updated_at = now;
        self.evict();
        Some(analysis)
    }

    /// Стирает разбор беседы, НЕ трогая ни заметку, ни тред. Запись, в которой
    /// не осталось ничего из трёх, удаляется целиком.
    ///
    /// Возвращает `true`, если разбор действительно был.
    pub fn clear_analysis(&mut self, dialog_id: &str) -> bool {
        let Some(entry) = self.chats.get_mut(dialog_id) else {
            return false;
        };
        let had = entry.analysis.take().is_some();
        if entry.prompt.is_empty() && entry.thread.is_empty() {
            self.chats.remove(dialog_id);
        }
        had
    }

    /// Вытесняет самые старые записи, пока их больше [`MAX_CHATS`].
    ///
    /// Тай-брейк по ключу — чтобы результат не зависел от порядка обхода: два
    /// одинаковых `updated_at` встречаются на раз (секундная гранулярность).
    fn evict(&mut self) {
        while self.chats.len() > MAX_CHATS {
            let Some(victim) = self
                .chats
                .iter()
                .min_by(|(ak, av), (bk, bv)| av.updated_at.cmp(&bv.updated_at).then(ak.cmp(bk)))
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.chats.remove(&victim);
        }
    }

    /// Приводит прочитанное с диска к инвариантам модуля.
    ///
    /// Файл мог быть отредактирован руками: заметка длиннее лимита, пустая
    /// строка, `\r\n` из Windows-редактора, лишние записи сверх [`MAX_CHATS`].
    /// Всё это чинится молча — падать из-за формата пользовательского файла
    /// нельзя (см. шапку модуля).
    fn normalize(&mut self) {
        let mut fixed = BTreeMap::new();
        for (id, entry) in std::mem::take(&mut self.chats) {
            let prompt = sanitize(&entry.prompt);
            let analysis = entry.analysis.and_then(|a| {
                let text = sanitize_analysis(&a.text);
                if text.is_empty() {
                    return None;
                }
                Some(ChatAnalysis { text, ..a })
            });
            let mut thread: Vec<QaTurn> = entry
                .thread
                .into_iter()
                .filter_map(|t| {
                    let q = sanitize_turn(&t.q);
                    let a = sanitize_turn(&t.a);
                    (!q.is_empty() && !a.is_empty()).then_some(QaTurn { q, a, at: t.at })
                })
                .collect();
            clamp_thread(&mut thread);
            // Пустая запись (ни заметки, ни разбора, ни треда) не хранится: она
            // заняла бы слот вытеснения ничем.
            if prompt.is_empty() && analysis.is_none() && thread.is_empty() {
                continue;
            }
            fixed.insert(
                id,
                ChatPrompt {
                    prompt,
                    updated_at: entry.updated_at,
                    analysis,
                    thread,
                },
            );
        }
        self.chats = fixed;
        self.version = PROMPTS_VERSION;
        self.evict();
    }
}

/// Приводит текст заметки к каноническому виду.
///
/// - `\r\n` и одиночный `\r` → `\n` (буфер обмена Windows приносит именно их);
/// - управляющие символы, кроме `\n` и `\t`, выбрасываются — они не несут
///   смысла в промпте, зато ломают и JSON-просмотр файла, и терминальный вывод;
/// - `trim` по краям;
/// - обрезка до [`MAX_PROMPT_CHARS`] по CODE POINTS.
///
/// # Почему обрезка по символам — это не стилистика
///
/// Байтовый срез `&s[..MAX]` на кириллице или emoji почти гарантированно
/// попадает в середину многобайтового символа, и Rust на это ПАНИКУЕТ. Для
/// панели, где каждая вторая заметка написана по-русски, такой лимит означал
/// бы падение хендлера на обычном пользовательском вводе.
#[must_use]
pub fn sanitize(raw: &str) -> String {
    clean(raw, MAX_PROMPT_CHARS)
}

/// То же самое для текста разбора, но с потолком [`MAX_ANALYSIS_CHARS`].
///
/// Отдельная функция, а не параметр у [`sanitize`]: разбор и заметка приходят
/// из разных источников (модель и человек), и общий лимит на двоих однажды
/// молча урезал бы один из них до чужой границы.
#[must_use]
pub fn sanitize_analysis(raw: &str) -> String {
    clean(raw, MAX_ANALYSIS_CHARS)
}

/// То же самое для одного поля пары треда (`q` либо `a`), потолок —
/// [`MAX_TURN_CHARS`].
///
/// Отдельный потолок на ПОЛЕ, а не только на весь тред: без него один
/// гигантский ответ съел бы бюджет треда целиком, и обрезка со старых пар
/// выбросила бы всю историю ради него одного.
#[must_use]
pub fn sanitize_turn(raw: &str) -> String {
    clean(raw, MAX_TURN_CHARS)
}

/// Обрезает тред со СТАРЫХ пар до [`MAX_THREAD_TURNS`] и [`MAX_THREAD_CHARS`].
///
/// Последняя пара не выбрасывается никогда: она только что задана
/// пользователем, а её размер ограничен [`MAX_TURN_CHARS`] так, что одна пара
/// в бюджет заведомо влезает.
fn clamp_thread(thread: &mut Vec<QaTurn>) {
    let cost = |t: &QaTurn| t.q.chars().count() + t.a.chars().count();
    let mut total: usize = thread.iter().map(&cost).sum();
    while thread.len() > 1 && (thread.len() > MAX_THREAD_TURNS || total > MAX_THREAD_CHARS) {
        total -= cost(&thread.remove(0));
    }
}

/// Общая нормализация текста с заданным потолком в CODE POINTS.
fn clean(raw: &str, limit: usize) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // `\r\n` схлопывается в один `\n`, одиночный `\r` — тоже.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }

    let trimmed = out.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(limit).collect();
    cut.trim_end().to_string()
}

/// Читает хранилище с диска. НИКОГДА не возвращает ошибку.
///
/// Нет файла / слишком большой / не JSON / чужая `version` → warn-лог и
/// пустое хранилище (см. шапку модуля). Текст заметок в лог не попадает ни в
/// одной ветке — только размеры и количества.
#[must_use]
pub fn load(path: &Path) -> PromptStore {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => {
            tracing::warn!(
                target: "chat_analysis",
                path = %path.display(),
                bytes = meta.len(),
                limit = MAX_FILE_BYTES,
                "chat-analysis: chat prompts file is too large; starting with an empty store"
            );
            return PromptStore::default();
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                target: "chat_analysis",
                path = %path.display(),
                "chat-analysis: chat prompts file not found; starting with an empty store"
            );
            return PromptStore::default();
        }
        Err(e) => {
            tracing::warn!(
                target: "chat_analysis",
                path = %path.display(),
                error = %e,
                "chat-analysis: cannot stat the chat prompts file; starting with an empty store"
            );
            return PromptStore::default();
        }
    }

    let body = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "chat_analysis",
                path = %path.display(),
                error = %e,
                "chat-analysis: cannot read the chat prompts file; starting with an empty store"
            );
            return PromptStore::default();
        }
    };

    let mut store: PromptStore = match serde_json::from_slice(&body) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "chat_analysis",
                path = %path.display(),
                error = %e,
                "chat-analysis: chat prompts file is not valid JSON; starting with an empty store"
            );
            return PromptStore::default();
        }
    };

    if store.version != PROMPTS_VERSION {
        tracing::warn!(
            target: "chat_analysis",
            path = %path.display(),
            found = store.version,
            expected = PROMPTS_VERSION,
            "chat-analysis: unknown chat prompts version; starting with an empty store"
        );
        return PromptStore::default();
    }

    store.normalize();
    tracing::debug!(
        target: "chat_analysis",
        path = %path.display(),
        chats = store.chats.len(),
        "chat-analysis: loaded chat prompts"
    );
    store
}

/// Атомарно сохраняет хранилище с правами `0600`.
///
/// Пишет через единую [`crate::atomic::write_bytes_atomic`]: tmp рядом →
/// `fsync` → `rename` → права. При ошибке временный файл убирается, а старое
/// содержимое остаётся нетронутым — потерять все заметки из-за сбоя на одной
/// нельзя.
///
/// # Errors
///
/// Ошибка сериализации либо записи на диск.
pub fn save(path: &Path, store: &PromptStore) -> anyhow::Result<()> {
    let mut body = serde_json::to_vec_pretty(store)
        .map_err(|e| anyhow::anyhow!("failed to serialize chat prompts: {e}"))?;
    body.push(b'\n');
    crate::atomic::write_bytes_atomic(path, &body)?;
    tracing::debug!(
        target: "chat_analysis",
        path = %path.display(),
        chats = store.chats.len(),
        bytes = body.len(),
        "chat-analysis: saved chat prompts (contents are never logged)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_of(pairs: &[(&str, &str, i64)]) -> PromptStore {
        let mut s = PromptStore::default();
        for (id, text, at) in pairs {
            s.set(id, text, *at);
        }
        s
    }

    #[test]
    fn round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat-prompts.json");
        let store = store_of(&[
            ("2000000001", "Рабочая беседа, отвечать формально", 1_800_000_000),
            ("105431859", "Друг, можно неформально 🙂", 1_800_000_001),
        ]);

        save(&path, &store).unwrap();
        assert_eq!(load(&path), store);

        // Форма файла — часть контракта: ключ строкой, поля как в шапке.
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        assert!(raw["chats"]["2000000001"]["prompt"].is_string());
        assert_eq!(raw["chats"]["2000000001"]["updated_at"], 1_800_000_000);
    }

    /// Правка одного чата не трогает соседей — это и есть смысл плоской мапы.
    #[test]
    fn editing_one_chat_keeps_the_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        save(&path, &store_of(&[("1", "первый", 10), ("2", "второй", 11)])).unwrap();

        let mut store = load(&path);
        store.set("2", "второй, обновлён", 12);
        save(&path, &store).unwrap();

        let back = load(&path);
        assert_eq!(back.text("1"), "первый", "сосед обязан пережить правку");
        assert_eq!(back.text("2"), "второй, обновлён");
        assert_eq!(back.get("1").unwrap().updated_at, 10, "чужой updated_at цел");
    }

    #[test]
    fn load_never_fails_on_a_missing_broken_or_alien_file() {
        let dir = tempfile::tempdir().unwrap();

        // 1. Файла нет.
        assert_eq!(load(&dir.path().join("nope.json")), PromptStore::default());

        // 2. Не JSON.
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, b"{\"version\": 1, \"chats\": ").unwrap();
        assert!(load(&broken).chats.is_empty());

        // 3. JSON, но не наша форма.
        let alien_shape = dir.path().join("shape.json");
        std::fs::write(&alien_shape, b"[1,2,3]").unwrap();
        assert!(load(&alien_shape).chats.is_empty());

        // 4. Чужая версия — данные НЕ подхватываются.
        let v999 = dir.path().join("v999.json");
        std::fs::write(
            &v999,
            r#"{"version":999,"chats":{"1":{"prompt":"из будущего","updated_at":1}}}"#,
        )
        .unwrap();
        assert!(load(&v999).chats.is_empty(), "чужая версия не читается");

        // 5. Слишком большой файл — не разбирается вовсе.
        let huge = dir.path().join("huge.json");
        let mut body = br#"{"version":1,"chats":{}}"#.to_vec();
        body.resize(usize::try_from(MAX_FILE_BYTES).unwrap() + 1, b' ');
        std::fs::write(&huge, &body).unwrap();
        assert!(load(&huge).chats.is_empty());
    }

    /// СОВМЕСТИМОСТЬ ФОРМАТА: файл, написанный ДО появления `analysis`,
    /// читается без потерь и переписывается таким же — без `"analysis": null`.
    ///
    /// Это главный тест доработки: у пользователя на диске уже лежит файл
    /// старой формы, и первая же перезапись не имеет права его испортить.
    #[test]
    fn an_old_file_without_analysis_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        std::fs::write(
            &path,
            r#"{"version":1,"chats":{
                "2000000001":{"prompt":"Рабочая беседа","updated_at":1800000000},
                "105431859":{"prompt":"Друг 🙂","updated_at":1800000001}
            }}"#,
        )
        .unwrap();

        let mut store = load(&path);
        assert_eq!(store.text("2000000001"), "Рабочая беседа");
        assert_eq!(store.text("105431859"), "Друг 🙂");
        assert!(store.analysis("2000000001").is_none(), "поля не было — и нет");
        assert!(store.thread("2000000001").is_empty(), "треда не было — и нет");
        assert_eq!(store.get("2000000001").unwrap().updated_at, 1_800_000_000);

        // Перезапись (правка соседней беседы) не портит ни форму, ни данные.
        store.set("105431859", "Друг, обновлено", 1_800_000_002);
        save(&path, &store).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("analysis"),
            "запись без разбора не обязана нести пустое поле: {raw}"
        );
        assert!(
            !raw.contains("thread"),
            "запись без треда не обязана нести пустой массив: {raw}"
        );
        let back = load(&path);
        assert_eq!(back.text("2000000001"), "Рабочая беседа");
        assert_eq!(back.text("105431859"), "Друг, обновлено");
        assert!(back.analysis("2000000001").is_none());
        assert!(back.thread("2000000001").is_empty());
    }

    /// Заметка и разбор живут независимо: правка одной не трогает другую, и
    /// обе переживают файл.
    #[test]
    fn the_note_and_the_analysis_never_clobber_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");

        let mut store = PromptStore::default();
        store.set("1234", "это заказчик", 10);
        store.set_analysis("1234", "1. О чём разговор — про смету.", 7042, 118, 20);
        save(&path, &store).unwrap();

        let mut back = load(&path);
        assert_eq!(back.text("1234"), "это заказчик", "разбор не съел заметку");
        let a = back.analysis("1234").unwrap().clone();
        assert_eq!(a.text, "1. О чём разговор — про смету.");
        assert_eq!((a.last_message_id, a.messages_used, a.updated_at), (7042, 118, 20));

        // Правка заметки сохраняет разбор ЦЕЛИКОМ (включая точку отсчёта).
        back.set("1234", "это заказчик, писать формально", 30);
        assert_eq!(back.analysis("1234"), Some(&a));

        // И новый разбор не трогает заметку.
        back.set_analysis("1234", "обновлённый разбор", 7100, 12, 40);
        assert_eq!(back.text("1234"), "это заказчик, писать формально");
        assert_eq!(back.analysis("1234").unwrap().last_message_id, 7100);
    }

    /// Две кнопки очистки — две независимые операции: стёртая заметка не
    /// уносит разбор, стёртый разбор не уносит заметку, а запись, лишившаяся
    /// обеих половин, исчезает.
    #[test]
    fn erasing_one_half_keeps_the_other() {
        let mut store = PromptStore::default();
        store.set("1", "заметка", 10);
        store.set_analysis("1", "разбор", 5, 3, 11);

        assert!(store.set("1", "   ", 12).is_none(), "заметка стёрта");
        assert_eq!(store.text("1"), "");
        assert!(store.analysis("1").is_some(), "разбор обязан пережить стирание заметки");

        store.set("1", "снова заметка", 13);
        assert!(store.clear_analysis("1"));
        assert!(store.analysis("1").is_none());
        assert_eq!(store.text("1"), "снова заметка", "заметка пережила очистку разбора");

        // Ни заметки, ни разбора — записи нет вовсе.
        store.set("1", "", 14);
        assert!(store.get("1").is_none());
        assert!(store.chats.is_empty());
        assert!(!store.clear_analysis("нет такой"), "очистка пустого — не ошибка");
    }

    /// Разбор длиннее лимита обрезается ПО СИМВОЛАМ — на кириллице и emoji
    /// байтовый срез паниковал бы посреди символа.
    #[test]
    fn analysis_text_is_truncated_by_code_points() {
        for filler in ["я", "🙂", "水"] {
            let raw = filler.repeat(MAX_ANALYSIS_CHARS + 500);
            let out = sanitize_analysis(&raw);
            assert_eq!(out.chars().count(), MAX_ANALYSIS_CHARS, "{filler}");
            assert_eq!(out, filler.repeat(MAX_ANALYSIS_CHARS), "{filler}");
        }

        let mut store = PromptStore::default();
        let saved = store
            .set_analysis("1", &"я".repeat(MAX_ANALYSIS_CHARS + 1_000), 1, 1, 10)
            .unwrap();
        assert_eq!(saved.text.chars().count(), MAX_ANALYSIS_CHARS);
        // Пустой разбор — то же, что его отсутствие.
        assert!(store.set_analysis("1", "  \n ", 1, 1, 11).is_none());
        assert!(store.analysis("1").is_none());
    }

    /// АРИФМЕТИКА ПОТОЛКА ФАЙЛА, зафиксированная тестом.
    ///
    /// `MAX_FILE_BYTES` — не «примерно столько»: это граница, за которой файл
    /// перестаёт читаться ВОВСЕ. Стоит поднять `MAX_ANALYSIS_CHARS` или
    /// `MAX_CHATS`, не тронув потолок, — и полное хранилище одного дня
    /// превратится в пустое молча. Тест краснеет раньше пользователя.
    #[test]
    fn the_file_cap_covers_the_worst_case() {
        // 4 байта на code point — худший случай UTF-8.
        let payload =
            (MAX_CHATS * (MAX_PROMPT_CHARS + MAX_ANALYSIS_CHARS + MAX_THREAD_CHARS) * 4) as u64;
        // Накладные расходы JSON (ключи, кавычки, экранирование, отступы
        // `to_vec_pretty`) — грубым множителем 1.25: на текстах в тысячи
        // символов доля разметки заведомо меньше четверти.
        let with_json_overhead = payload + payload / 4;
        assert!(
            with_json_overhead <= MAX_FILE_BYTES,
            "{MAX_CHATS} чатов × ({MAX_PROMPT_CHARS} + {MAX_ANALYSIS_CHARS} + \
             {MAX_THREAD_CHARS}) символов = {payload} байт, с накладными \
             {with_json_overhead} при потолке {MAX_FILE_BYTES}"
        );
        // Одна пара треда обязана влезать в бюджет треда целиком — иначе
        // обрезка со старых выбрасывала бы весь тред ради последнего ответа.
        const { assert!(MAX_TURN_CHARS * 2 <= MAX_THREAD_CHARS) };
        // «Помним больше, чем отправляем модели» — обе оси сразу.
        const { assert!(MAX_THREAD_TURNS > crate::prompt::MAX_HISTORY_TURNS) };
        const { assert!(MAX_THREAD_CHARS > crate::prompt::MAX_HISTORY_CHARS) };
    }

    // -----------------------------------------------------------------------
    // Тред вопросов.
    // -----------------------------------------------------------------------

    /// Сверх [`MAX_THREAD_TURNS`] выбрасываются САМЫЕ СТАРЫЕ пары.
    #[test]
    fn the_thread_keeps_the_newest_turns() {
        let mut s = PromptStore::default();
        for i in 0..(MAX_THREAD_TURNS + 3) {
            assert!(s.push_turn("1", &format!("вопрос {i}"), &format!("ответ {i}"), 10 + i as i64).is_some());
        }
        let thread = s.thread("1");
        assert_eq!(thread.len(), MAX_THREAD_TURNS);
        assert_eq!(thread[0].q, "вопрос 3", "первые три выброшены");
        assert_eq!(thread[MAX_THREAD_TURNS - 1].q, format!("вопрос {}", MAX_THREAD_TURNS + 2));
        assert_eq!(thread[MAX_THREAD_TURNS - 1].at, 10 + (MAX_THREAD_TURNS + 2) as i64);
    }

    /// Вторая ось обрезки — суммарные символы; последняя пара остаётся всегда.
    #[test]
    fn the_thread_is_clamped_by_total_chars_too() {
        let mut s = PromptStore::default();
        // Пары по 2 × 2 000 символов: три штуки уже не влезают в 10 000.
        for i in 0..5 {
            s.push_turn("1", &"я".repeat(2_000), &format!("{i}{}", "а".repeat(1_999)), 10 + i);
        }
        let thread = s.thread("1");
        let total: usize = thread.iter().map(|t| t.q.chars().count() + t.a.chars().count()).sum();
        assert!(total <= MAX_THREAD_CHARS, "{total}");
        assert!(thread.len() < 5, "старые пары выброшены по символам");
        assert!(thread.last().unwrap().a.starts_with('4'), "последняя пара цела");
    }

    /// Одна огромная пара обрезается ПО СИМВОЛАМ и не роняет функцию — ни на
    /// кириллице, ни на emoji.
    #[test]
    fn a_single_giant_turn_is_truncated_not_dropped() {
        for filler in ["я", "🙂", "水"] {
            let mut s = PromptStore::default();
            let saved = s
                .push_turn(
                    "1",
                    &filler.repeat(MAX_TURN_CHARS + 500),
                    &filler.repeat(MAX_TURN_CHARS + 500),
                    10,
                )
                .unwrap();
            assert_eq!(saved.q.chars().count(), MAX_TURN_CHARS, "{filler}");
            assert_eq!(saved.a.chars().count(), MAX_TURN_CHARS, "{filler}");
            assert_eq!(s.thread("1").len(), 1, "{filler}: пара выжила");
            let total: usize = saved.q.chars().count() + saved.a.chars().count();
            assert!(total <= MAX_THREAD_CHARS, "{filler}");
        }
    }

    /// Пустой вопрос либо пустой ответ парой не становятся.
    #[test]
    fn an_empty_half_is_not_a_turn() {
        let mut s = PromptStore::default();
        assert!(s.push_turn("1", "  \n ", "ответ", 10).is_none());
        assert!(s.push_turn("1", "вопрос", "   ", 11).is_none());
        assert!(s.thread("1").is_empty());
        assert!(s.get("1").is_none(), "пустая запись не заводится");
    }

    /// МАТРИЦА НЕЗАВИСИМОСТИ: три половины одной записи не задевают друг друга
    /// ни при правке, ни при очистке.
    #[test]
    fn the_note_the_analysis_and_the_thread_are_independent() {
        let mut s = PromptStore::default();
        s.set("1", "заметка", 10);
        s.set_analysis("1", "разбор", 42, 7, 11);
        s.push_turn("1", "в1", "о1", 12);
        s.push_turn("1", "в2", "о2", 13);

        // Правка заметки не трогает разбор и тред.
        s.set("1", "заметка 2", 14);
        assert_eq!(s.analysis("1").unwrap().text, "разбор");
        assert_eq!(s.thread("1").len(), 2);

        // Новый разбор не трогает заметку и тред.
        s.set_analysis("1", "разбор 2", 43, 8, 15);
        assert_eq!(s.text("1"), "заметка 2");
        assert_eq!(s.thread("1").len(), 2);

        // Новая пара не трогает заметку и разбор.
        s.push_turn("1", "в3", "о3", 16);
        assert_eq!(s.text("1"), "заметка 2");
        assert_eq!(s.analysis("1").unwrap().text, "разбор 2");

        // Очистка разбора не трогает заметку и тред.
        assert!(s.clear_analysis("1"));
        assert_eq!(s.text("1"), "заметка 2");
        assert_eq!(s.thread("1").len(), 3);

        // Очистка треда не трогает заметку (и уже отсутствующий разбор).
        s.set_analysis("1", "разбор 3", 44, 9, 17);
        assert!(s.clear_thread("1"));
        assert!(s.thread("1").is_empty());
        assert_eq!(s.text("1"), "заметка 2");
        assert_eq!(s.analysis("1").unwrap().text, "разбор 3");

        assert!(!s.clear_thread("нет такой"), "очистка пустого — не ошибка");
    }

    /// Тред в одиночку удерживает запись: стёртая заметка и стёртый разбор не
    /// уносят его с собой.
    #[test]
    fn the_thread_alone_keeps_the_entry_alive() {
        let mut s = PromptStore::default();
        s.set("1", "заметка", 10);
        s.set_analysis("1", "разбор", 1, 1, 11);
        s.push_turn("1", "в", "о", 12);

        assert!(s.set("1", "  ", 13).is_none());
        assert!(s.clear_analysis("1"));
        assert_eq!(s.thread("1").len(), 1, "тред пережил обе очистки");

        // А когда стёрт и он — записи больше нет.
        assert!(s.clear_thread("1"));
        assert!(s.get("1").is_none());
        assert!(s.chats.is_empty());
    }

    /// Тред доезжает до ФАЙЛА и обратно, а очистки на диске так же независимы,
    /// как в памяти: молчаливая потеря случается именно на записи.
    #[test]
    fn the_thread_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");

        let mut store = PromptStore::default();
        store.set("1234", "заметка", 10);
        store.set_analysis("1234", "разбор", 42, 7, 11);
        store.push_turn("1234", "а что со сроками? 🙂", "к пятнице", 12);
        store.push_turn("1234", "точно?", "да", 13);
        save(&path, &store).unwrap();

        let mut back = load(&path);
        assert_eq!(back.thread("1234").len(), 2);
        assert_eq!(back.thread("1234")[0].q, "а что со сроками? 🙂");
        assert_eq!(back.thread("1234")[1].at, 13);
        assert_eq!(back, store, "round-trip без потерь");

        // Очистка разбора через файл не уносит тред и заметку.
        back.clear_analysis("1234");
        save(&path, &back).unwrap();
        let back = load(&path);
        assert!(back.analysis("1234").is_none());
        assert_eq!(back.thread("1234").len(), 2, "CLEAR анализа стёр тред");
        assert_eq!(back.text("1234"), "заметка");

        // И наоборот.
        let mut back = load(&path);
        back.set_analysis("1234", "разбор 2", 43, 8, 14);
        back.clear_thread("1234");
        save(&path, &back).unwrap();
        let back = load(&path);
        assert!(back.thread("1234").is_empty());
        assert_eq!(back.analysis("1234").unwrap().text, "разбор 2", "CLEAR Q&A стёр разбор");
        assert_eq!(back.text("1234"), "заметка");
    }

    /// Отредактированный руками тред чинится на чтении: мусорные пары
    /// выбрасываются, переполнение обрезается.
    #[test]
    fn load_normalizes_a_hand_edited_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        let mut turns: Vec<serde_json::Value> = (0..(MAX_THREAD_TURNS + 5))
            .map(|i| serde_json::json!({ "q": format!("в{i}"), "a": "о", "at": 10 + i }))
            .collect();
        turns.push(serde_json::json!({ "q": "  ", "a": "о", "at": 99 }));
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "chats": { "1": { "prompt": "з", "updated_at": 5, "thread": turns } }
            }))
            .unwrap(),
        )
        .unwrap();

        let store = load(&path);
        let thread = store.thread("1");
        assert_eq!(thread.len(), MAX_THREAD_TURNS, "лишние пары срезаны");
        assert_eq!(thread[0].q, "в5", "срезаны САМЫЕ старые");
        assert!(thread.iter().all(|t| !t.q.trim().is_empty()), "мусорная пара выброшена");
    }

    /// РЕГРЕСС НА ПАНИКУ: байтовый срез на кириллице/emoji рвал бы символ.
    #[test]
    fn sanitize_truncates_by_code_points_not_bytes() {
        for filler in ["я", "🙂", "水"] {
            let raw = filler.repeat(MAX_PROMPT_CHARS + 500);
            let out = sanitize(&raw);
            assert_eq!(out.chars().count(), MAX_PROMPT_CHARS, "{filler}");
            // Символ не разорван: строка валидна и целиком состоит из filler'а.
            assert_eq!(out, filler.repeat(MAX_PROMPT_CHARS), "{filler}");
        }
    }

    #[test]
    fn sanitize_normalizes_newlines_controls_and_edges() {
        assert_eq!(sanitize("  привет\r\nмир  "), "привет\nмир");
        assert_eq!(sanitize("одиночный\rCR"), "одиночный\nCR");
        assert_eq!(sanitize("таб\tсохраняется"), "таб\tсохраняется");
        assert_eq!(sanitize("bell\u{7}nul\u{0}esc\u{1b}"), "bellnulesc");
        assert_eq!(sanitize("   \n\t  "), "");
    }

    #[test]
    fn an_empty_prompt_removes_the_entry() {
        let mut s = store_of(&[("1", "текст", 10)]);
        assert!(s.get("1").is_some());

        assert!(s.set("1", "   \n ", 11).is_none());
        assert!(s.get("1").is_none(), "пустая строка = удаление записи");
        assert_eq!(s.text("1"), "");

        // Удаление отсутствующей записи — не ошибка и не создаёт пустышку.
        assert!(s.set("42", "", 12).is_none());
        assert!(s.chats.is_empty());
    }

    #[test]
    fn the_oldest_chat_is_evicted_past_the_limit() {
        let mut s = PromptStore::default();
        for i in 0..MAX_CHATS {
            s.set(&format!("{i:04}"), "заметка", 1_000 + i as i64);
        }
        assert_eq!(s.chats.len(), MAX_CHATS);

        s.set("new", "свежая", 9_999);
        assert_eq!(s.chats.len(), MAX_CHATS, "лимит держится");
        assert!(s.get("0000").is_none(), "выброшен самый старый по updated_at");
        assert!(s.get("new").is_some());
        assert!(s.get("0001").is_some(), "следующий по возрасту остался");
    }

    /// Отредактированный руками файл сверх лимитов чинится на чтении.
    #[test]
    fn load_normalizes_a_hand_edited_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        let mut chats = serde_json::Map::new();
        chats.insert(
            "1".into(),
            serde_json::json!({ "prompt": "я".repeat(MAX_PROMPT_CHARS + 10), "updated_at": 5 }),
        );
        chats.insert(
            "2".into(),
            serde_json::json!({ "prompt": "   ", "updated_at": 6 }),
        );
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({ "version": 1, "chats": chats })).unwrap(),
        )
        .unwrap();

        let store = load(&path);
        assert_eq!(store.text("1").chars().count(), MAX_PROMPT_CHARS);
        assert!(store.get("2").is_none(), "пустая заметка не хранится");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_leaves_no_temporaries() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        save(&path, &store_of(&[("1", "заметка про человека", 10)])).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "заметки о людях — только владельцу");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n != "p.json")
            .collect();
        assert!(leftovers.is_empty(), "после успеха tmp не остаётся: {leftovers:?}");
    }

    /// И после НЕУДАЧИ временный файл не должен пережить попытку.
    #[test]
    fn a_failed_save_leaves_no_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        // Целевой путь — непустой КАТАЛОГ: запись tmp пройдёт, `rename`
        // упадёт, и уборка обязана сработать именно на этой ветке.
        let path = dir.path().join("busy");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("inhabitant"), b"x").unwrap();

        assert!(save(&path, &store_of(&[("1", "текст", 10)])).is_err());

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n != "busy")
            .collect();
        assert!(leftovers.is_empty(), "после неудачи tmp убран: {leftovers:?}");
        assert!(path.join("inhabitant").is_file(), "чужие данные не тронуты");
    }
}
