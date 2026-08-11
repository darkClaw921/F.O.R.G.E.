//! Транскрипт переписки: выгрузка истории диалога и её рендер в текст для
//! Claude.
//!
//! # Сеть и рендер РАЗДЕЛЕНЫ, и это главное решение модуля
//!
//! [`fetch`] ходит в Telegram и не форматирует ни одной строки; [`render`] —
//! ЧИСТАЯ функция над `&[MessageDto]` и не знает ни про клиент, ни про
//! состояние. Слитые в одну функцию, они были бы непроверяемы: единственный
//! способ убедиться, что транскрипт читаем и не содержит адресов, — прогнать
//! рендер на литеральных DTO без сети и без сессии.
//!
//! Сама пагинация тоже отвязана от клиента — через трейт [`HistorySource`]
//! (тот же приём, что у [`crate::peers::PeerSource`]). Иначе «стык страниц не
//! теряет и не дублирует сообщение» проверялся бы только живым аккаунтом, то
//! есть не проверялся бы вовсе.
//!
//! # Курсор `offset_id` ЭКСКЛЮЗИВЕН
//!
//! `client.iter_messages(peer).offset_id(before)` отдаёт сообщения СТРОГО
//! старше `before` — само сообщение с этим id в выдачу не попадает. Поэтому
//! курсором следующей страницы служит МИНИМАЛЬНЫЙ id предыдущей: взять
//! `min_id - 1` значило бы потерять ровно одно сообщение на каждом стыке, а
//! `min_id + 1` — задвоить его.
//!
//! # Форма строки
//!
//! ```text
//! [07.08 12:31] Я: текст
//! [07.08 12:33] Игорь: ↳ Анна: «превью цитаты» | текст
//! [07.08 12:40] Анна: [фото] [документ: смета.xlsx]
//! ```
//!
//! Одно сообщение — РОВНО ОДНА строка (переводы строк внутри текста
//! схлопываются в пробел): построчная форма позволяет резать транскрипт по
//! границам сообщений, а модели — ссылаться на конкретную реплику.
//!
//! # Чего у Telegram НЕТ (в отличие от VK)
//!
//! Ни `attachments` (медиа ровно одно — `Option<MediaDto>`), ни `fwd`, ни
//! `action`, ни `cmid`. Соответствующие ветки VK-рендера сюда не переехали
//! вовсе — не закомментированы, а отсутствуют. Зато `sender` уже заполнен
//! проекцией, поэтому резолв имён (самая дорогая часть VK-выгрузки) здесь не
//! нужен.
//!
//! # Инвариант «адреса не покидают бэкенд»
//!
//! В транскрипте НЕТ ни одного URL: медиа превращается в компактные метки
//! (`[фото]`, `[документ: смета.xlsx]`), а байты и ссылки на файловый кэш не
//! переносятся вовсе. Здесь это правило строже, чем у DTO: подписанный адрес,
//! уехавший в промпт, — утечка в ЧУЖОЙ процесс, а не в свою же вкладку.
//! Инвариант закреплён тестом `rendered_text_never_contains_urls`.
//!
//! # Потолок символов режет СТАРЫЕ сообщения
//!
//! Транскрипт собирается С КОНЦА: свежие реплики важнее — вопрос пользователя
//! почти всегда о том, чем разговор кончился. Отброшенное признаётся вслух
//! первой строкой `[…опущено N более старых сообщений]`, чтобы модель не
//! считала начало переписки началом истории.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use async_trait::async_trait;
use grammers_client::Client;
use grammers_session::types::PeerRef;

use crate::dto::{MediaDto, MessageDto, ReplyToDto};
use crate::login::TgUserDto;
use crate::routes::dialogs::map_invocation;
use crate::routes::error::ApiError;
use crate::state::TelegramState;

/// Допустимые значения глубины выгрузки (сообщений).
///
/// Закрытый список, а не диапазон: глубина приезжает с фронта селектом, и
/// произвольное число здесь означало бы, что пользователь способен заказать
/// выгрузку на сотню RPC.
pub const DEPTH_CHOICES: [i64; 3] = [50, 200, 500];

/// Глубина по умолчанию.
///
/// 200 — это 2 запроса к Telegram (страница режется [`MAX_PAGE`] до 100).
/// 500 не сделано дефолтом намеренно: это уже 5 запросов на КАЖДЫЙ анализ.
pub const DEFAULT_DEPTH: i64 = 200;

/// Потолок транскрипта в CODE POINTS.
///
/// ~15 тысяч токенов — заметная, но не разорительная доля окна; всё, что
/// сверху, режется по старым сообщениям.
pub const MAX_TRANSCRIPT_CHARS: usize = 60_000;

/// Потолок одного сообщения в CODE POINTS.
///
/// Одна простыня на 50 000 символов не должна съедать весь транскрипт: для
/// понимания разговора хватает начала реплики.
pub const MAX_MESSAGE_CHARS: usize = 1_500;

/// Страховка от бесконечного цикла пагинации.
///
/// Больше, чем нужно самой глубокой выгрузке (500 = 5 страниц), и меньше, чем
/// «навсегда»: если Telegram перестанет двигать курсор, цикл обязан
/// закончиться сам.
pub const MAX_PAGES: usize = 8;

/// Максимум сообщений за один запрос — столько же за раз отдаёт Telegram
/// (см. [`crate::routes::dialogs::MAX_MESSAGES`]).
pub const MAX_PAGE: i64 = 100;

/// Потолок подписи вложения: имена файлов бывают длиной с абзац, а роли в
/// разговоре у них не больше, чем у слова «документ».
const MEDIA_TITLE_CHARS: usize = 80;

// Сам тип транскрипта живёт в общем крейте (`forge_chat_analysis::Transcript`):
// собирает его ПЛАГИН — рендер ниже, — а потребляет ядро (`prompt::build`
// кладёт текст в `<transcript>`, hub сохраняет `newest_id` точкой отсчёта
// следующего REFRESH).
pub use forge_chat_analysis::Transcript;

/// Приводит запрошенную глубину к ближайшему значению [`DEPTH_CHOICES`].
///
/// Именно к ближайшему, а не «к дефолту при незнакомом значении»: клиент,
/// попросивший 400, хотел «побольше», и молча отдать ему 200 — значит соврать
/// ровно там, где он старался.
#[must_use]
pub fn clamp_depth(requested: i64) -> i64 {
    let mut best = DEFAULT_DEPTH;
    let mut best_dist = i64::MAX;
    for choice in DEPTH_CHOICES {
        let dist = (choice - requested).abs();
        if dist < best_dist {
            best_dist = dist;
            best = choice;
        }
    }
    best
}

/// Режет размер страницы к тому, что Telegram отдаёт за один запрос.
#[must_use]
fn clamp_page(want: i64) -> usize {
    usize::try_from(want.clamp(1, MAX_PAGE)).unwrap_or(1)
}

// ---------------------------------------------------------------------------
// Сеть.
// ---------------------------------------------------------------------------

/// Откуда берутся страницы истории.
///
/// Существует ради тестируемости пагинации: [`fetch_with`] и
/// [`fetch_since_with`] — это ЛОГИКА (курсор, потолки, обрезка), и проверять
/// её живым аккаунтом невозможно. Продовая реализация ровно одна —
/// [`ClientSource`], и она не содержит ни одного `if`.
#[async_trait]
pub trait HistorySource: Send + Sync {
    /// Одна страница, СТРОГО старше `before` (курсор эксклюзивен), не более
    /// `limit` записей. Порядок внутри страницы значения не имеет —
    /// вызывающий всё равно сортирует.
    ///
    /// # Errors
    ///
    /// Любая ошибка Telegram, пропущенная через [`map_invocation`].
    async fn page(
        &self,
        peer: PeerRef,
        limit: usize,
        before: Option<i32>,
    ) -> Result<Vec<MessageDto>, ApiError>;
}

/// Продовый источник: живой MTProto-клиент.
pub struct ClientSource {
    client: Client,
}

impl ClientSource {
    /// Оборачивает клон клиента, взятый `require_ready`.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl HistorySource for ClientSource {
    async fn page(
        &self,
        peer: PeerRef,
        limit: usize,
        before: Option<i32>,
    ) -> Result<Vec<MessageDto>, ApiError> {
        // Ровно тот же вызов, что у `routes::dialogs::get_messages` — иначе
        // анализ видел бы историю не такой, какой её видит пользователь.
        let mut iter = self.client.iter_messages(peer).limit(limit);
        if let Some(b) = before {
            iter = iter.offset_id(b);
        }
        let mut out = Vec::with_capacity(limit.min(64));
        while let Some(msg) = iter.next().await.map_err(|e| map_invocation(&e))? {
            out.push(crate::dto::from_grammers_message(&msg));
        }
        Ok(out)
    }
}

/// Выгружает последние `depth` сообщений диалога, от старых к новым.
///
/// # Errors
///
/// Любая ошибка Telegram, пропущенная через [`map_invocation`].
pub async fn fetch(
    _state: &Arc<TelegramState>,
    client: &Client,
    peer: PeerRef,
    depth: i64,
) -> Result<Vec<MessageDto>, ApiError> {
    fetch_with(&ClientSource::new(client.clone()), peer, depth).await
}

/// Тело [`fetch`] над произвольным источником страниц.
///
/// # Errors
///
/// Ошибка источника.
pub async fn fetch_with<S: HistorySource + ?Sized>(
    source: &S,
    peer: PeerRef,
    depth: i64,
) -> Result<Vec<MessageDto>, ApiError> {
    let depth = clamp_depth(depth);
    let mut out: Vec<MessageDto> = Vec::new();
    let mut before: Option<i32> = None;

    for _ in 0..MAX_PAGES {
        let want = depth - out.len() as i64;
        if want <= 0 {
            break;
        }
        let limit = clamp_page(want);
        let page = source.page(peer, limit, before).await?;
        let min_id = page.iter().map(|m| m.id).min();
        let full = page.len() >= limit;
        out.extend(page);

        // Страница короче запрошенной ⇒ история кончилась.
        if !full {
            break;
        }
        // Курсор не сдвинулся — выходим, иначе следующая страница пришла бы
        // та же самая. Единственная защита сверх [`MAX_PAGES`].
        match min_id {
            Some(v) if v > 0 => before = Some(v),
            _ => break,
        }
    }

    // Контракт наружу — возрастание id, без дублей (страницы могут
    // пересечься, если между запросами историю подвинули).
    order(&mut out);
    if out.len() > depth as usize {
        // Лишнее срезается СПЕРЕДИ: просили последние `depth` сообщений.
        out.drain(..out.len() - depth as usize);
    }
    Ok(out)
}

/// Результат [`fetch_since`]: новые сообщения и признак «взяли не всё».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewMessages {
    /// Сообщения с `id > since_id`, по возрастанию id, без дублей.
    pub messages: Vec<MessageDto>,
    /// Часть новых сообщений отброшена потолком выгрузки (взяты самые новые).
    pub truncated: bool,
}

/// Только сообщения НОВЕЕ уже разобранных — вход инкрементального обновления.
///
/// Тот же постраничный цикл, что и у [`fetch`], те же потолки. Отличий ровно
/// два:
///
/// 1. **Цикл останавливается на границе.** Как только на странице встретилось
///    сообщение с `id <= since_id`, дальше листать нечего: история идёт от
///    новых к старым, и всё, что осталось, пользователь уже видел в разборе.
///    Поэтому обычный REFRESH стоит ОДНОГО запроса, а не пяти.
/// 2. **Переполнение честно объявляется.** Если новых сообщений оказалось
///    больше `cap` (или они не уместились в [`MAX_PAGES`]), берутся САМЫЕ
///    НОВЫЕ, а [`NewMessages::truncated`] поднимается — модель обязана знать,
///    что кусок переписки между прошлым разбором и этим она не видела.
///
/// # Errors
///
/// Любая ошибка Telegram, пропущенная через [`map_invocation`].
pub async fn fetch_since(
    _state: &Arc<TelegramState>,
    client: &Client,
    peer: PeerRef,
    since_id: i32,
    cap: i64,
) -> Result<NewMessages, ApiError> {
    fetch_since_with(&ClientSource::new(client.clone()), peer, since_id, cap).await
}

/// Тело [`fetch_since`] над произвольным источником страниц.
///
/// # Errors
///
/// Ошибка источника.
pub async fn fetch_since_with<S: HistorySource + ?Sized>(
    source: &S,
    peer: PeerRef,
    since_id: i32,
    cap: i64,
) -> Result<NewMessages, ApiError> {
    let cap = clamp_depth(cap);
    let mut out: Vec<MessageDto> = Vec::new();
    let mut before: Option<i32> = None;
    // Дошли до уже разобранного ⇒ ничего старше нам не нужно и ничего не
    // потеряно. Пока флаг не взведён, между выборкой и разбором остаётся
    // непросмотренный кусок, и это и есть `truncated`.
    let mut reached_known = false;
    let mut history_ended = false;

    for _ in 0..MAX_PAGES {
        let want = cap - out.len() as i64;
        if want <= 0 {
            break;
        }
        let limit = clamp_page(want);
        let page = source.page(peer, limit, before).await?;
        let min_id = page.iter().map(|m| m.id).min();
        let full = page.len() >= limit;
        if page.iter().any(|m| m.id <= since_id) {
            reached_known = true;
        }
        out.extend(page.into_iter().filter(|m| m.id > since_id));

        if reached_known {
            break;
        }
        if !full {
            history_ended = true;
            break;
        }
        match min_id {
            Some(v) if v > 0 => before = Some(v),
            _ => {
                history_ended = true;
                break;
            }
        }
    }

    order(&mut out);
    let mut truncated = !reached_known && !history_ended;
    if out.len() > cap as usize {
        // Как и в [`fetch`], лишнее срезается спереди: свежие реплики важнее.
        out.drain(..out.len() - cap as usize);
        truncated = true;
    }
    Ok(NewMessages {
        messages: out,
        truncated,
    })
}

/// Возрастание id без дублей — общий контракт обеих выгрузок.
fn order(out: &mut Vec<MessageDto>) {
    out.sort_by_key(|m| m.id);
    out.dedup_by_key(|m| m.id);
}

/// Имя собеседника по уже выгруженным сообщениям — заголовок для шапки
/// промпта.
///
/// Ноль запросов к сети: кэш peer'ов Telegram хранит `PeerRef` без title'ов, а
/// отдельный RPC ради украшения шапки платился бы на КАЖДОМ разборе. Берётся
/// автор первого ВХОДЯЩЕГО сообщения — для лички это ровно собеседник, для
/// группы просто первый заговоривший, и в обоих случаях это лучше пустоты.
///
/// Пустая строка — легальный ответ (весь кусок истории исходящий либо авторов
/// нет вовсе): `prompt::build` подставит «без названия».
#[must_use]
pub fn title_from_messages(messages: &[MessageDto]) -> String {
    messages
        .iter()
        .filter(|m| !m.out)
        .filter_map(|m| m.sender.as_ref())
        .map(speaker_name)
        .find(|n| !n.is_empty())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Рендер.
// ---------------------------------------------------------------------------

/// Сообщения → транскрипт, обрезанный по `budget` СИМВОЛАМ.
///
/// Чистая функция: ни сети, ни времени, ни состояния. Порядок строк — по
/// возрастанию `id` независимо от порядка входа.
///
/// Сообщение без текста и без медиа ПРОПУСКАЕТСЯ: пустая строка
/// «[12:31] Анна:» не несёт ни факта, ни контекста, а место в бюджете
/// занимает.
#[must_use]
pub fn render(messages: &[MessageDto], budget: usize) -> Transcript {
    let mut ordered: Vec<&MessageDto> = messages.iter().collect();
    ordered.sort_by_key(|m| m.id);

    // (id, строка) — id нужен для `oldest_id` после обрезки бюджетом.
    let lines: Vec<(i32, String)> = ordered
        .iter()
        .filter_map(|m| render_message(m).map(|line| (m.id, line)))
        .collect();

    // Собираем С КОНЦА: новые сообщения важнее старых.
    let mut kept: Vec<(i32, String)> = Vec::new();
    let mut used = 0usize;
    let mut dropped = 0usize;
    for (i, (id, line)) in lines.iter().enumerate().rev() {
        let cost = line.chars().count() + 1;
        if used + cost > budget {
            // Всё, что старше, тем более не поместится.
            dropped = i + 1;
            break;
        }
        used += cost;
        kept.push((*id, line.clone()));
    }
    kept.reverse();

    let truncated = dropped > 0;
    let mut marker = omitted_marker(dropped);
    // Маркер тоже стоит символов: освобождаем под него место, отдавая самые
    // старые из уцелевших строк. Без этого транскрипт вылезал бы за бюджет
    // ровно в тот момент, когда бюджет и сработал.
    while dropped > 0 && !kept.is_empty() && used + marker.chars().count() + 1 > budget {
        let (_, line) = kept.remove(0);
        used -= line.chars().count() + 1;
        dropped += 1;
        marker = omitted_marker(dropped);
    }

    let mut text = String::new();
    // Крайний случай: бюджет так мал, что не вмещает даже маркер. Тогда его
    // просто нет — вылезти за границу транскрипт не имеет права, а флаг
    // `truncated` вызывающий увидит и без строки.
    if truncated && marker.chars().count() < budget {
        text.push_str(&marker);
        text.push('\n');
    }
    for (_, line) in &kept {
        text.push_str(line);
        text.push('\n');
    }

    Transcript {
        text,
        messages_used: kept.len(),
        truncated,
        oldest_id: kept.first().map(|(id, _)| *id),
        newest_id: kept.last().map(|(id, _)| *id),
    }
}

/// Первая строка обрезанного транскрипта.
fn omitted_marker(dropped: usize) -> String {
    format!("[…опущено {dropped} более старых сообщений]")
}

/// Одно сообщение → строка транскрипта; `None`, если рисовать нечего.
fn render_message(m: &MessageDto) -> Option<String> {
    let text = one_line(&m.text);
    if text.is_empty() && m.media.is_none() && m.reply_to.is_none() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();

    if let Some(reply) = &m.reply_to {
        parts.push(reply_label(reply));
    }
    if !text.is_empty() {
        parts.push(truncate_chars(&text, MAX_MESSAGE_CHARS));
    }
    if let Some(media) = &m.media {
        parts.push(media_label(media));
    }

    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "[{}] {}: {}",
        stamp(m.date),
        display_speaker(m),
        parts.join(" | ")
    ))
}

/// Кто говорит: `Я` у исходящих, иначе отображаемое имя, иначе `—`.
fn display_speaker(m: &MessageDto) -> String {
    if m.out {
        return "Я".to_string();
    }
    let name = m.sender.as_ref().map(speaker_name).unwrap_or_default();
    if name.is_empty() {
        "—".to_string()
    } else {
        name
    }
}

/// Имя автора: `first_name`, иначе `@username`, иначе числовой id.
///
/// Собственный хелпер, а не метод DTO: числовой id в транскрипте — крайний
/// случай, и подставлять его вместо имени осмысленно только здесь (в UI это
/// делает фронт, и правила у него свои).
fn speaker_name(u: &TgUserDto) -> String {
    for v in [u.first_name.as_deref(), u.username.as_deref()]
        .into_iter()
        .flatten()
    {
        let t = v.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    u.id.trim().to_string()
}

/// Цитата одной вставкой: `↳ Автор: «превью»`.
///
/// Приоритет превью — процитированный фрагмент, потом текст оригинала, потом
/// вид медиа. Фрагмент важнее целого текста намеренно: отвечающий выделил его
/// руками, то есть сам сказал, о чём именно речь.
fn reply_label(r: &ReplyToDto) -> String {
    let preview = r
        .quote
        .as_deref()
        .map(one_line)
        .filter(|t| !t.is_empty())
        .or_else(|| {
            r.text
                .as_deref()
                .map(one_line)
                .filter(|t: &String| !t.is_empty())
        })
        .or_else(|| r.media_kind.as_deref().map(|k| kind_word(k).to_string()))
        .unwrap_or_else(|| {
            if r.unavailable {
                "удалённое сообщение".to_string()
            } else {
                "сообщение".to_string()
            }
        });
    let author = r.sender.as_deref().map(str::trim).unwrap_or("");
    if author.is_empty() {
        format!("↳ «{preview}»")
    } else {
        format!("↳ {author}: «{preview}»")
    }
}

/// Компактная метка вложения. НИ ОДНОГО адреса — см. шапку модуля.
fn media_label(m: &MediaDto) -> String {
    let file = m
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let word = match m.kind.as_str() {
        "photo" => return "[фото]".to_string(),
        "sticker" => return "[стикер]".to_string(),
        "video" => "видео",
        "audio" => "аудио",
        "voice" => "голосовое",
        "document" => "документ",
        // `other` — закрытая корзина словаря DTO: всё, что Telegram прислал в
        // виде, который UI не умеет разложить.
        _ => "вложение",
    };
    labeled(word, file, m.duration_secs)
}

/// `[слово]`, `[слово: файл]`, `[слово M:SS]`, `[слово: файл M:SS]`.
fn labeled(word: &str, file: Option<&str>, duration_secs: Option<i32>) -> String {
    let mut body = match file {
        Some(f) => format!("{word}: {}", truncate_chars(f, MEDIA_TITLE_CHARS)),
        None => word.to_string(),
    };
    if let Some(secs) = duration_secs {
        if secs > 0 {
            body.push(' ');
            body.push_str(&duration(secs));
        }
    }
    format!("[{body}]")
}

/// Словарь `kind` → короткое русское слово (для превью цитат).
fn kind_word(kind: &str) -> &str {
    match kind {
        "photo" => "фото",
        "video" => "видео",
        "audio" => "аудио",
        "voice" => "голосовое",
        "document" => "документ",
        "sticker" => "стикер",
        other => other,
    }
}

/// Секунды → `M:SS`.
fn duration(secs: i32) -> String {
    let s = secs.max(0);
    format!("{}:{:02}", s / 60, s % 60)
}

/// Unix-время → `ДД.ММ ЧЧ:ММ` в локальной зоне пользователя.
///
/// Локальная, а не UTC, намеренно: транскрипт читает модель, отвечающая
/// человеку («вчера вечером договорились…»), и сдвиг на часовой пояс сделал бы
/// такие выводы неверными.
fn stamp(date: i64) -> String {
    match chrono::DateTime::from_timestamp(date, 0) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%d.%m %H:%M")
            .to_string(),
        None => "??.?? ??:??".to_string(),
    }
}

/// Схлопывает переводы строк и лишние пробелы: одно сообщение — одна строка.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Обрезка по CODE POINTS с многоточием.
///
/// По code points, а не по байтам: срез по байтам паникует на кириллице, а
/// emoji рвёт пополам.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use grammers_session::types::{PeerAuth, PeerId};

    use super::*;
    use crate::dto::{MediaDto, MessageDto, ReplyToDto};
    use crate::login::TgUserDto;

    fn peer() -> PeerRef {
        PeerRef {
            id: PeerId::from_bot_api_dialog_id(777_000).expect("валидный id"),
            auth: PeerAuth::from_hash(31),
        }
    }

    fn user(name: &str) -> TgUserDto {
        TgUserDto {
            id: "1234".into(),
            username: None,
            first_name: Some(name.into()),
            phone: None,
        }
    }

    fn msg(id: i32, text: &str) -> MessageDto {
        MessageDto {
            id,
            dialog_id: "777000".into(),
            date: 1_800_000_000 + i64::from(id),
            text: text.into(),
            sender: Some(user("Анна")),
            ..MessageDto::default()
        }
    }

    fn media(kind: &str) -> MediaDto {
        MediaDto {
            kind: kind.into(),
            ..MediaDto::default()
        }
    }

    // -----------------------------------------------------------------------
    // Выгрузка (сеть подменена).
    // -----------------------------------------------------------------------

    /// Фейковая история: непрерывный отрезок id `1..=total`, отдаётся страницами
    /// от НОВЫХ к старым, курсор ЭКСКЛЮЗИВЕН — ровно как у Telegram.
    struct FakeHistory {
        total: i32,
        calls: AtomicUsize,
        seen: Mutex<Vec<(usize, Option<i32>)>>,
    }

    impl FakeHistory {
        fn new(total: i32) -> Self {
            Self {
                total,
                calls: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl HistorySource for FakeHistory {
        async fn page(
            &self,
            _peer: PeerRef,
            limit: usize,
            before: Option<i32>,
        ) -> Result<Vec<MessageDto>, ApiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push((limit, before));
            // ЭКСКЛЮЗИВНОСТЬ: `before` в выдачу не попадает.
            let top = before.map_or(self.total, |b| b - 1);
            let mut out = Vec::new();
            let mut id = top;
            while id >= 1 && out.len() < limit {
                out.push(msg(id, &format!("реплика {id}")));
                id -= 1;
            }
            Ok(out)
        }
    }

    /// СТЫК СТРАНИЦ: ни одного пропуска и ни одного дубля.
    ///
    /// Главная ловушка Telegram-выгрузки: `offset_id` эксклюзивен, поэтому
    /// курсором служит МИНИМАЛЬНЫЙ id страницы. `min_id - 1` терял бы ровно
    /// одно сообщение на каждом стыке, `min_id + 1` — задваивал бы его.
    #[tokio::test]
    async fn paging_loses_and_duplicates_nothing_on_the_seam() {
        let src = FakeHistory::new(1000);
        let got = fetch_with(&src, peer(), 200).await.unwrap();

        let ids: Vec<i32> = got.iter().map(|m| m.id).collect();
        let expected: Vec<i32> = (801..=1000).collect();
        assert_eq!(ids, expected, "непрерывный отрезок по возрастанию id");
        assert_eq!(src.calls(), 2, "200 сообщений = ровно две страницы по 100");

        let seen = src.seen.lock().unwrap().clone();
        assert_eq!(seen[0], (100, None), "первая страница без курсора");
        assert_eq!(
            seen[1],
            (100, Some(901)),
            "курсор — МИНИМАЛЬНЫЙ id предыдущей страницы, а не min-1"
        );
    }

    /// Порядок наружу — по возрастанию id, без дублей, ровно `depth` записей.
    #[tokio::test]
    async fn fetch_returns_exactly_depth_newest_messages() {
        for depth in DEPTH_CHOICES {
            let src = FakeHistory::new(1000);
            let got = fetch_with(&src, peer(), depth).await.unwrap();
            assert_eq!(got.len(), depth as usize, "depth {depth}");
            assert_eq!(got.last().unwrap().id, 1000, "самое новое обязано уцелеть");
            assert!(got.windows(2).all(|w| w[0].id < w[1].id), "строгий рост id");
        }
    }

    /// История короче запрошенной — выходим по неполной странице, а не по
    /// [`MAX_PAGES`].
    #[tokio::test]
    async fn a_short_history_stops_on_the_first_partial_page() {
        let src = FakeHistory::new(30);
        let got = fetch_with(&src, peer(), 500).await.unwrap();
        assert_eq!(got.len(), 30);
        assert_eq!(src.calls(), 1, "30 < 100 ⇒ одна страница и выход");
    }

    /// Источник, который всегда отдаёт ПОЛНУЮ страницу с одним и тем же
    /// курсором: без [`MAX_PAGES`] цикл был бы бесконечным.
    struct NeverEnding;

    #[async_trait]
    impl HistorySource for NeverEnding {
        async fn page(
            &self,
            _peer: PeerRef,
            limit: usize,
            _before: Option<i32>,
        ) -> Result<Vec<MessageDto>, ApiError> {
            // Все страницы одинаковы ⇒ курсор не двигается.
            Ok((1..=limit as i32).map(|i| msg(i, "x")).collect())
        }
    }

    /// Курсор не сдвинулся — цикл обязан закончиться сам, а не крутиться.
    #[tokio::test]
    async fn a_stuck_cursor_terminates_instead_of_looping() {
        let got = fetch_with(&NeverEnding, peer(), 500).await.unwrap();
        assert!(!got.is_empty());
        assert!(got.windows(2).all(|w| w[0].id < w[1].id), "дубли схлопнуты");
    }

    /// REFRESH берёт ТОЛЬКО новое и останавливается на первом уже разобранном.
    #[tokio::test]
    async fn fetch_since_takes_only_new_messages_in_one_call() {
        let src = FakeHistory::new(1000);
        let fresh = fetch_since_with(&src, peer(), 990, 200).await.unwrap();

        let ids: Vec<i32> = fresh.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, (991..=1000).collect::<Vec<i32>>());
        assert!(!fresh.truncated, "ничего не потеряно — граница достигнута");
        assert_eq!(src.calls(), 1, "обычный REFRESH стоит ОДНОГО запроса");
    }

    /// Новых сообщений нет вовсе — пустой результат, а не ошибка.
    #[tokio::test]
    async fn fetch_since_at_the_head_returns_nothing() {
        let src = FakeHistory::new(1000);
        let fresh = fetch_since_with(&src, peer(), 1000, 200).await.unwrap();
        assert!(fresh.messages.is_empty());
        assert!(!fresh.truncated);
    }

    /// Новых больше, чем потолок: берутся САМЫЕ НОВЫЕ, и это ОБЪЯВЛЯЕТСЯ.
    ///
    /// Без флага модель считала бы, что видела всё, что произошло с прошлого
    /// разбора, — и молча соврала бы пользователю.
    #[tokio::test]
    async fn fetch_since_marks_truncation_when_new_messages_overflow() {
        let src = FakeHistory::new(1000);
        let fresh = fetch_since_with(&src, peer(), 0, 50).await.unwrap();
        assert_eq!(fresh.messages.len(), 50);
        assert_eq!(fresh.messages.last().unwrap().id, 1000, "самые НОВЫЕ");
        assert_eq!(fresh.messages.first().unwrap().id, 951);
        assert!(fresh.truncated, "обрезка обязана быть объявлена");
    }

    /// Глубина клампится к закрытому списку.
    #[test]
    fn depth_is_clamped_to_the_closed_list() {
        let table = [
            (-100, 50),
            (0, 50),
            (1, 50),
            (50, 50),
            (120, 50),
            (200, 200),
            (400, 500),
            (500, 500),
            (10_000, 500),
        ];
        for (requested, expected) in table {
            assert_eq!(clamp_depth(requested), expected, "depth {requested}");
        }
        for choice in DEPTH_CHOICES {
            assert_eq!(clamp_depth(choice), choice);
        }
        assert!(DEPTH_CHOICES.contains(&DEFAULT_DEPTH));
    }

    // -----------------------------------------------------------------------
    // Рендер.
    // -----------------------------------------------------------------------

    /// Исходящее подписывается «Я», входящее — именем автора: модель обязана
    /// понимать, чью сторону разговора она разбирает.
    #[test]
    fn out_and_in_get_different_speakers() {
        let mut mine = msg(2, "готово");
        mine.out = true;
        let t = render(&[msg(1, "привет"), mine], MAX_TRANSCRIPT_CHARS);
        let lines: Vec<&str> = t.text.lines().collect();
        assert!(lines[0].contains("Анна: привет"), "{}", t.text);
        assert!(lines[1].contains("Я: готово"), "{}", t.text);
        assert_eq!(t.messages_used, 2);
        assert!(!t.truncated);
        assert_eq!(t.oldest_id, Some(1));
        assert_eq!(t.newest_id, Some(2));
    }

    /// Порядок строк — по возрастанию id, каким бы ни был порядок входа.
    #[test]
    fn lines_are_ordered_by_ascending_id() {
        let t = render(&[msg(30, "три"), msg(10, "один"), msg(20, "два")], 10_000);
        let bodies: Vec<&str> = t
            .text
            .lines()
            .map(|l| l.split(": ").nth(1).unwrap())
            .collect();
        assert_eq!(bodies, ["один", "два", "три"]);
    }

    /// Имя автора: `first_name`, иначе `@username`, иначе id — но НИКОГДА не
    /// пустая подпись.
    #[test]
    fn speaker_falls_back_from_name_to_username_to_id() {
        let table: [(TgUserDto, &str); 3] = [
            (user("Пётр"), "Пётр"),
            (
                TgUserDto {
                    id: "42".into(),
                    username: Some("durov".into()),
                    first_name: Some("   ".into()),
                    phone: None,
                },
                "durov",
            ),
            (
                TgUserDto {
                    id: "42".into(),
                    username: None,
                    first_name: None,
                    phone: None,
                },
                "42",
            ),
        ];
        for (u, expected) in table {
            let mut m = msg(1, "текст");
            m.sender = Some(u);
            let t = render(&[m], MAX_TRANSCRIPT_CHARS);
            assert!(t.text.contains(&format!("{expected}: текст")), "{}", t.text);
        }
        // Автора нет вовсе (пост от имени канала) — «—», а не пустота.
        let mut anon = msg(1, "объявление");
        anon.sender = None;
        let t = render(&[anon], MAX_TRANSCRIPT_CHARS);
        assert!(t.text.contains("—: объявление"), "{}", t.text);
    }

    /// По метке на каждый вид медиа — и ни одного адреса.
    #[test]
    fn every_media_kind_gets_a_compact_label() {
        let table: [(MediaDto, &str); 8] = [
            (media("photo"), "[фото]"),
            (media("sticker"), "[стикер]"),
            (media("video"), "[видео]"),
            (
                MediaDto {
                    duration_secs: Some(83),
                    ..media("video")
                },
                "[видео 1:23]",
            ),
            (
                MediaDto {
                    duration_secs: Some(14),
                    ..media("voice")
                },
                "[голосовое 0:14]",
            ),
            (
                MediaDto {
                    file_name: Some("смета.xlsx".into()),
                    ..media("document")
                },
                "[документ: смета.xlsx]",
            ),
            (
                MediaDto {
                    file_name: Some("track.mp3".into()),
                    duration_secs: Some(200),
                    ..media("audio")
                },
                "[аудио: track.mp3 3:20]",
            ),
            (media("other"), "[вложение]"),
        ];
        for (m, expected) in table {
            let kind = m.kind.clone();
            let mut message = msg(1, "");
            message.media = Some(m);
            let t = render(&[message], MAX_TRANSCRIPT_CHARS);
            assert!(t.text.contains(expected), "{kind}: {}", t.text);
        }
    }

    /// ИНВАРИАНТ: в транскрипт не попадает ни один адрес.
    ///
    /// Здесь он строже, чем у DTO: транскрипт уезжает в ЧУЖОЙ процесс.
    #[test]
    fn rendered_text_never_contains_urls() {
        let mut m = msg(1, "смотри");
        m.media = Some(MediaDto {
            mime: Some("video/mp4".into()),
            file_name: Some("clip.mp4".into()),
            ..media("video")
        });
        let mut with_link = msg(2, "ссылка внутри текста тут не рендерится как адрес");
        with_link.media = Some(media("photo"));
        let t = render(&[m, with_link], MAX_TRANSCRIPT_CHARS);
        for needle in ["http://", "https://", "://", "t.me", "/api/telegram/media"] {
            assert!(!t.text.contains(needle), "{needle} в {}", t.text);
        }
    }

    /// Цитата даёт префикс с превью оригинала.
    #[test]
    fn reply_to_adds_a_quote_prefix() {
        let mut m = msg(2, "согласен");
        m.reply_to = Some(ReplyToDto {
            message_id: Some(1),
            sender: Some("Игорь Герасимов".into()),
            text: Some("надо решить\nдо пятницы".into()),
            ..ReplyToDto::default()
        });
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        assert!(
            t.text
                .contains("↳ Игорь Герасимов: «надо решить до пятницы» | согласен"),
            "{}",
            t.text
        );
    }

    /// Выделенный фрагмент важнее целого текста оригинала: отвечающий сам
    /// сказал, о чём именно речь.
    #[test]
    fn a_quoted_fragment_wins_over_the_full_original_text() {
        let mut m = msg(2, "да");
        m.reply_to = Some(ReplyToDto {
            message_id: Some(1),
            sender: Some("Анна".into()),
            text: Some("длинное сообщение обо всём сразу".into()),
            quote: Some("о сроках".into()),
            ..ReplyToDto::default()
        });
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        assert!(t.text.contains("↳ Анна: «о сроках»"), "{}", t.text);
        assert!(!t.text.contains("обо всём сразу"), "{}", t.text);
    }

    /// Оригинал удалён — цитата всё равно рисуется, и честно об этом говорит.
    #[test]
    fn an_unavailable_original_still_renders_a_quote() {
        let mut m = msg(2, "жаль");
        m.reply_to = Some(ReplyToDto {
            message_id: Some(1),
            unavailable: true,
            ..ReplyToDto::default()
        });
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        assert!(t.text.contains("↳ «удалённое сообщение»"), "{}", t.text);
    }

    /// Медиа без текста рисует превью цитаты видом вложения.
    #[test]
    fn a_reply_to_media_uses_the_kind_word() {
        let mut m = msg(2, "красиво");
        m.reply_to = Some(ReplyToDto {
            message_id: Some(1),
            sender: Some("Ольга".into()),
            media_kind: Some("photo".into()),
            ..ReplyToDto::default()
        });
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        assert!(t.text.contains("↳ Ольга: «фото»"), "{}", t.text);
    }

    /// Пустое сообщение не занимает ни строки, ни бюджета.
    #[test]
    fn empty_messages_are_skipped() {
        let t = render(&[msg(1, "   "), msg(2, "есть текст")], MAX_TRANSCRIPT_CHARS);
        assert_eq!(t.messages_used, 1);
        assert_eq!(t.text.lines().count(), 1);
        assert_eq!(t.oldest_id, Some(2));
    }

    /// Бюджет режет СТАРЫЕ сообщения, ставит маркер и поднимает флаг.
    #[test]
    fn budget_drops_the_oldest_and_marks_the_cut() {
        let messages: Vec<MessageDto> = (1..=40).map(|i| msg(i, &format!("реплика {i}"))).collect();
        let t = render(&messages, 300);
        assert!(t.truncated, "обрезка обязана быть объявлена");
        assert!(t.text.starts_with("[…опущено "), "{}", t.text);
        assert!(t.text.contains("реплика 40"), "новейшее обязано уцелеть");
        assert!(!t.text.contains("реплика 1 "), "старое обязано уйти");
        assert!(
            t.text.chars().count() <= 300,
            "бюджет {} символов",
            t.text.chars().count()
        );
        assert_eq!(t.messages_used, t.text.lines().count() - 1);
        assert_eq!(
            t.oldest_id,
            Some(40 - t.messages_used as i32 + 1),
            "oldest_id — самое старое ИЗ ПОПАВШИХ"
        );
    }

    /// Обрезка длинного текста — по code points: срез по байтам паникует на
    /// кириллице, а emoji рвёт пополам.
    #[test]
    fn long_messages_are_cut_by_code_points() {
        for filler in ["я", "😀", "a"] {
            let long = filler.repeat(MAX_MESSAGE_CHARS + 500);
            let t = render(&[msg(1, &long)], MAX_TRANSCRIPT_CHARS);
            let body = t.text.split_once(": ").unwrap().1.trim_end();
            assert_eq!(
                body.chars().count(),
                MAX_MESSAGE_CHARS + 1,
                "{filler} + многоточие"
            );
            assert!(body.ends_with('…'), "{filler}");
        }
    }

    /// Крошечный бюджет не порождает строку сверх лимита и не паникует.
    #[test]
    fn tiny_budget_degrades_gracefully() {
        let messages: Vec<MessageDto> = (1..=5).map(|i| msg(i, "текст")).collect();
        for budget in [0, 1, 5, 20] {
            let t = render(&messages, budget);
            assert!(
                t.text.chars().count() <= budget,
                "бюджет {budget}: {:?}",
                t.text
            );
        }
    }

    /// Заголовок берётся у первого ВХОДЯЩЕГО автора и не стоит ни одного RPC.
    #[test]
    fn title_comes_from_the_first_incoming_sender() {
        let mut mine = msg(1, "первым написал я");
        mine.out = true;
        let mut hers = msg(2, "ответ");
        hers.sender = Some(user("Анна"));
        assert_eq!(title_from_messages(&[mine.clone(), hers]), "Анна");

        // Весь кусок исходящий — пустая строка, `prompt::build` подставит
        // «без названия».
        assert_eq!(title_from_messages(&[mine]), "");
        assert_eq!(title_from_messages(&[]), "");
    }

    /// Правдоподобная переписка целиком: тест ГЛАЗАМИ (`--nocapture` печатает
    /// результат) плюс проверка того, что каждая форма строки действительно
    /// доехала до текста.
    #[test]
    fn a_realistic_conversation_reads_like_a_conversation() {
        let mut hello = msg(101, "Привет! Смету получилось посмотреть?");
        hello.sender = Some(user("Анна"));

        let mut mine = msg(102, "Да, пара правок по монтажу.");
        mine.out = true;
        mine.reply_to = Some(ReplyToDto {
            message_id: Some(101),
            sender: Some("Анна".into()),
            text: Some("Привет! Смету получилось посмотреть?".into()),
            ..ReplyToDto::default()
        });

        let mut docs = msg(103, "Держи актуальную версию");
        docs.media = Some(MediaDto {
            file_name: Some("смета_v3.xlsx".into()),
            ..media("document")
        });

        let mut voice = msg(104, "");
        voice.media = Some(MediaDto {
            duration_secs: Some(14),
            ..media("voice")
        });

        let t = render(&[hello, mine, docs, voice], MAX_TRANSCRIPT_CHARS);
        println!("--- транскрипт ---\n{}------------------", t.text);

        assert_eq!(t.messages_used, 4);
        assert!(!t.truncated);
        assert_eq!(t.oldest_id, Some(101));
        assert_eq!(t.text.lines().count(), 4, "по строке на сообщение");
        for needle in [
            "Анна: Привет!",
            "Я: ↳ Анна:",
            "[документ: смета_v3.xlsx]",
            "[голосовое 0:14]",
        ] {
            assert!(t.text.contains(needle), "{needle} в\n{}", t.text);
        }
    }
}
