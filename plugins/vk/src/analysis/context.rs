//! Транскрипт переписки: выгрузка истории беседы и её рендер в текст для
//! Claude.
//!
//! # Сеть и рендер РАЗДЕЛЕНЫ, и это главное решение модуля
//!
//! [`fetch`] ходит в VK и не форматирует ни одной строки; [`render`] —
//! ЧИСТАЯ функция над `&[MessageDto]` и не знает ни про клиент, ни про
//! состояние. Слитые в одну функцию, они были бы непроверяемы: единственный
//! способ убедиться, что транскрипт читаем и не содержит адресов, — прогнать
//! рендер на литеральных DTO без стенда и без токена.
//!
//! # Форма строки
//!
//! ```text
//! [07.08 12:31] Я: текст
//! [07.08 12:33] Игорь Герасимов: ↳ «превью цитаты» | текст
//! [07.08 12:40] Анна: [фото] [документ: смета.xlsx]
//! [07.08 12:41] * событие: Игорь пригласил Анну
//! [07.08 12:45] Анна: » переслано от Пётр (05.08): превью
//! ```
//!
//! Одно сообщение — РОВНО ОДНА строка (переводы строк внутри текста
//! схлопываются в пробел): построчная форма позволяет резать транскрипт по
//! границам сообщений, а модели — ссылаться на конкретную реплику.
//!
//! # Инвариант «адреса не покидают бэкенд»
//!
//! В транскрипте НЕТ ни одного URL: вложения превращаются в компактные метки
//! (`[фото]`, `[документ: смета.xlsx]`), а `player_url`/`link_url` из
//! [`MediaDto`] не переносятся вовсе. Это то же правило, что и у DTO
//! (`dto.rs:213`), только здесь оно строже: подписанный адрес CDN, уехавший в
//! промпт, — это утечка в чужой процесс, а не в свою же вкладку. Инвариант
//! закреплён тестом `rendered_text_never_contains_urls`.
//!
//! # Потолок символов режет СТАРЫЕ сообщения
//!
//! Транскрипт собирается С КОНЦА: свежие реплики важнее — вопрос
//! пользователя почти всегда о том, чем разговор кончился. Отброшенное
//! признаётся вслух первой строкой `[…опущено N более старых сообщений]`,
//! чтобы модель не считала начало переписки началом истории.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use crate::api::methods::clamp_limit;
use crate::api::VkClient;
use crate::dto::{self, MediaDto, MessageDto};
use crate::routes::error::ApiError;
use crate::state::VkState;

/// Допустимые значения глубины выгрузки (сообщений).
///
/// Закрытый список, а не диапазон: глубина приезжает с фронта селектом, и
/// произвольное число здесь означало бы, что пользователь способен заказать
/// выгрузку на сотню запросов при бюджете 3 rps.
pub const DEPTH_CHOICES: [i64; 3] = [50, 200, 500];

/// Глубина по умолчанию.
///
/// 200 — это 2 запроса к VK (страница режется [`clamp_limit`] до 100) и
/// примерно 0.7 с ожидания. 500 не сделано дефолтом намеренно: те же 5
/// запросов при лимите 3 rps — это уже ~1.7 с на КАЖДЫЙ анализ.
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
/// Больше, чем нужно самой глубокой выгрузке (500 = 5 страниц), и меньше,
/// чем «навсегда»: если VK перестанет двигать `start_message_id`, цикл обязан
/// закончиться сам.
pub const MAX_PAGES: usize = 8;

// Сам тип транскрипта живёт в общем крейте (`forge_chat_analysis::Transcript`):
// собирает его ПЛАГИН — рендер ниже, — а потребляет ядро (`prompt::build`
// кладёт текст в `<transcript>`, hub сохраняет `newest_id` точкой отсчёта
// следующего REFRESH). Объяви его каждый плагин у себя, `prompt::build`
// пришлось бы делать generic ради трёх одинаковых наборов из пяти полей.
// Переэкспорт — чтобы путь `analysis::context::Transcript` не поменялся.
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

// ---------------------------------------------------------------------------
// Сеть.
// ---------------------------------------------------------------------------

/// Выгружает последние `depth` сообщений беседы, от старых к новым.
///
/// Постранично, по образцу [`crate::routes::dialogs::get_messages`]:
/// `messages.getHistory` отдаёт максимум 100 записей за раз ([`clamp_limit`]),
/// поэтому `depth = 200` — это два запроса, а `500` — пять. Курсором служит
/// МИНИМАЛЬНЫЙ id предыдущей страницы (`start_message_id` + `offset=1`, см.
/// [`VkClient::get_history`]).
///
/// # Имена резолвятся ОБЯЗАТЕЛЬНО
///
/// `peers.absorb_extended` + [`crate::routes::dialogs::resolve_names`] — не
/// украшение: без них проекция подставит в `sender` голый числовой id, и
/// транскрипт превратится в диалог «140049 → 2000000001», по которому модель
/// не скажет ничего осмысленного.
///
/// # Errors
///
/// Любая ошибка VK, пропущенная через
/// [`crate::routes::dialogs::map_vk_error`], — чтобы `captcha_required` дошла
/// до фронта своим кодом, а не растворилась в generic 500.
pub async fn fetch(
    state: &Arc<VkState>,
    client: &VkClient,
    peer: i64,
    depth: i64,
) -> Result<Vec<MessageDto>, ApiError> {
    let depth = clamp_depth(depth);
    let mut out: Vec<MessageDto> = Vec::new();
    let mut before: Option<i64> = None;

    for _ in 0..MAX_PAGES {
        let want = depth - out.len() as i64;
        if want <= 0 {
            break;
        }
        let page = fetch_page(state, client, peer, clamp_limit(want), before).await?;
        let min_id = page.items.iter().map(|m| m.id).min();
        out.extend(page.items);

        if !page.has_more {
            break;
        }
        // Курсор не сдвинулся — выходим, иначе следующая страница пришла бы
        // та же самая. Единственная защита сверх [`MAX_PAGES`].
        match min_id {
            Some(v) if v > 0 => before = Some(i64::from(v)),
            _ => break,
        }
    }

    // Контракт наружу — возрастание id, без дублей (страницы могут
    // пересечься, если между запросами пришло новое сообщение).
    order(&mut out);
    if out.len() > depth as usize {
        // Лишнее срезается СПЕРЕДИ: просили последние `depth` сообщений.
        out.drain(..out.len() - depth as usize);
    }
    Ok(out)
}

/// Только сообщения НОВЕЕ уже разобранных — вход инкрементального обновления.
///
/// Тот же постраничный цикл, что и у [`fetch`], те же `absorb_extended` и
/// `resolve_names` (см. [`fetch_page`]), те же потолки [`MAX_PAGES`] и
/// [`clamp_depth`]. Отличий ровно два:
///
/// 1. **Цикл останавливается на границе.** Как только на странице встретилось
///    сообщение с `id <= since_id`, дальше листать нечего: история идёт от
///    новых к старым, и всё, что осталось, пользователь уже видел в разборе.
///    Поэтому обычный REFRESH стоит ОДНОГО запроса к VK, а не пяти.
/// 2. **Переполнение честно объявляется.** Если новых сообщений оказалось
///    больше `cap` (или они не уместились в [`MAX_PAGES`]), берутся САМЫЕ
///    НОВЫЕ, а [`NewMessages::truncated`] поднимается — модель обязана знать,
///    что кусок переписки между прошлым разбором и этим она не видела.
///
/// # Errors
///
/// Любая ошибка VK, пропущенная через
/// [`crate::routes::dialogs::map_vk_error`].
pub async fn fetch_since(
    state: &Arc<VkState>,
    client: &VkClient,
    peer: i64,
    since_id: i32,
    cap: i64,
) -> Result<NewMessages, ApiError> {
    let cap = clamp_depth(cap);
    let mut out: Vec<MessageDto> = Vec::new();
    let mut before: Option<i64> = None;
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
        let page = fetch_page(state, client, peer, clamp_limit(want), before).await?;
        let min_id = page.items.iter().map(|m| m.id).min();
        if page.items.iter().any(|m| m.id <= since_id) {
            reached_known = true;
        }
        out.extend(page.items.into_iter().filter(|m| m.id > since_id));

        if reached_known {
            break;
        }
        if !page.has_more {
            history_ended = true;
            break;
        }
        match min_id {
            Some(v) if v > 0 => before = Some(i64::from(v)),
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

/// Результат [`fetch_since`]: новые сообщения и признак «взяли не всё».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewMessages {
    /// Сообщения с `id > since_id`, по возрастанию id, без дублей.
    pub messages: Vec<MessageDto>,
    /// Часть новых сообщений отброшена потолком выгрузки (взяты самые новые).
    pub truncated: bool,
}

/// Одна страница истории, уже спроецированная в DTO с резолвом имён.
struct Page {
    items: Vec<MessageDto>,
    /// Страница пришла полной ⇒ история, возможно, продолжается.
    has_more: bool,
}

/// Общий шаг пагинации обеих выгрузок.
///
/// Вынесен, а не скопирован: `absorb_extended` + `resolve_names` — это не
/// украшение, а условие читаемости транскрипта (без них в тексте окажется
/// «140049 → 2000000001»), и вторая копия этого шага однажды разъехалась бы с
/// первой.
async fn fetch_page(
    state: &Arc<VkState>,
    client: &VkClient,
    peer: i64,
    limit: i64,
    before: Option<i64>,
) -> Result<Page, ApiError> {
    let page = match client.get_history(peer, limit, before).await {
        Ok(p) => p,
        Err(e) => return Err(crate::routes::dialogs::map_vk_error(state, &e).await),
    };

    state.peers.absorb_extended(&page.profiles, &page.groups);
    let mut wanted: Vec<i64> = vec![peer];
    for m in &page.items {
        crate::routes::dialogs::collect_author_ids(m, &mut wanted);
    }
    crate::routes::dialogs::resolve_names(state, client, &wanted).await;

    // `has_more` считается по числу ПРИШЕДШИХ записей: страница короче
    // запрошенной значит, что история кончилась.
    let has_more = page.items.len() as i64 >= limit;
    Ok(Page {
        items: page
            .items
            .iter()
            .map(|m| dto::message_from_raw(m, &state.peers))
            .collect(),
        has_more,
    })
}

/// Возрастание id без дублей — общий контракт обеих выгрузок (страницы
/// пересекаются, если между запросами пришло новое сообщение).
fn order(out: &mut Vec<MessageDto>) {
    out.sort_by_key(|m| m.id);
    out.dedup_by_key(|m| m.id);
}

// ---------------------------------------------------------------------------
// Рендер.
// ---------------------------------------------------------------------------

/// Сообщения → транскрипт, обрезанный по `budget` СИМВОЛАМ.
///
/// Чистая функция: ни сети, ни времени, ни состояния. Порядок строк — по
/// возрастанию `id` независимо от порядка входа.
///
/// Сообщение без текста, без вложений и без служебного действия ПРОПУСКАЕТСЯ:
/// пустая строка «[12:31] Анна:» не несёт ни факта, ни контекста, а место в
/// бюджете занимает.
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
    // `< budget`, а не `+ 1 <= budget`: единица — это перевод строки после
    // маркера, и он тоже обязан поместиться.
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
    let has_action = m.action.is_some();
    if text.is_empty() && m.attachments.is_empty() && !has_action {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();

    // Служебное сообщение — отдельная форма строки: у него нет ни автора в
    // привычном смысле, ни текста от человека.
    if let Some(action) = &m.action {
        let who = speaker(m);
        let what = if action.text.trim().is_empty() {
            action.kind.trim().to_string()
        } else {
            action.text.trim().to_string()
        };
        let body = if what.is_empty() {
            "служебное сообщение".to_string()
        } else if who.is_empty() {
            what
        } else {
            format!("{who} {what}")
        };
        return Some(format!("[{}] * событие: {}", stamp(m.date), body));
    }

    if let Some(reply) = &m.reply_to {
        let preview = reply
            .text
            .as_deref()
            .map(one_line)
            .filter(|t| !t.is_empty())
            .or_else(|| reply.media_kind.as_deref().map(|k| kind_word(k).to_string()))
            .unwrap_or_else(|| "сообщение".to_string());
        let author = reply.sender.as_deref().map(str::trim).unwrap_or("");
        if author.is_empty() {
            parts.push(format!("↳ «{preview}»"));
        } else {
            parts.push(format!("↳ {author}: «{preview}»"));
        }
    }

    if !text.is_empty() {
        parts.push(dto::truncate_chars(&text, MAX_MESSAGE_CHARS));
    }

    let media: Vec<String> = m.attachments.iter().map(media_label).collect();
    if !media.is_empty() {
        parts.push(media.join(" "));
    }

    for fwd in &m.fwd {
        parts.push(fwd_label(fwd));
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
    let name = speaker(m);
    if name.is_empty() {
        "—".to_string()
    } else {
        name
    }
}

/// Имя автора без подстановки «Я»/«—» (нужно служебным сообщениям).
fn speaker(m: &MessageDto) -> String {
    m.sender
        .as_ref()
        .map(crate::login::VkUserDto::display_name)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Пересланное сообщение одной вставкой.
fn fwd_label(f: &crate::dto::FwdDto) -> String {
    let author = f
        .sender
        .as_ref()
        .map(crate::login::VkUserDto::display_name)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "неизвестно".to_string());
    let preview = f
        .text
        .as_deref()
        .map(one_line)
        .filter(|t| !t.is_empty())
        .or_else(|| f.media_kind.as_deref().map(|k| kind_word(k).to_string()))
        .unwrap_or_else(|| "без текста".to_string());
    // Отступ показывает глубину пересылки: «переслал пересланное» на плоском
    // тексте иначе неотличимо от двух пересылок подряд.
    let indent = "»".repeat(usize::from(f.depth) + 1);
    let mut out = if f.date > 0 {
        format!("{indent} переслано от {author} ({}): {preview}", day(f.date))
    } else {
        format!("{indent} переслано от {author}: {preview}")
    };
    if f.omitted > 0 {
        out.push_str(&format!(" …ещё {}", f.omitted));
    }
    out
}

/// Компактная метка вложения. НИ ОДНОГО адреса — см. шапку модуля.
fn media_label(m: &MediaDto) -> String {
    let title = m
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let file = m
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    match m.kind.as_str() {
        "photo" => "[фото]".to_string(),
        "video" => with_title("видео", title.as_deref()),
        "audio" => with_title("аудио", title.as_deref()),
        "voice" => match m.duration_secs {
            Some(s) if s > 0 => format!("[голосовое {}]", duration(s)),
            _ => "[голосовое]".to_string(),
        },
        "document" => with_title("документ", file.as_deref().or(title.as_deref())),
        "sticker" => "[стикер]".to_string(),
        "graffiti" => "[граффити]".to_string(),
        // `link` у нас нормализован в `other` (attach.rs), и отличает его
        // ровно наличие адреса — сам адрес в транскрипт, разумеется, не едет.
        "other" if m.link_url.is_some() => with_title("ссылка", title.as_deref()),
        kind => format!("[вложение: {}]", kind_word(kind)),
    }
}

/// `[слово]` или `[слово: подпись]`.
fn with_title(word: &str, title: Option<&str>) -> String {
    match title {
        Some(t) => format!("[{word}: {}]", dto::truncate_chars(t, MEDIA_TITLE_CHARS)),
        None => format!("[{word}]"),
    }
}

/// Потолок подписи вложения: имена файлов и заголовки статей бывают длиной с
/// абзац, а роли в разговоре у них не больше, чем у слова «документ».
const MEDIA_TITLE_CHARS: usize = 80;

/// Словарь `kind` → короткое русское слово (для превью цитат и пересылок).
fn kind_word(kind: &str) -> &str {
    match kind {
        "photo" => "фото",
        "video" => "видео",
        "audio" => "аудио",
        "voice" => "голосовое",
        "document" => "документ",
        "sticker" => "стикер",
        "graffiti" => "граффити",
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

/// Unix-время → `ДД.ММ` (дата пересланного).
fn day(date: i64) -> String {
    match chrono::DateTime::from_timestamp(date, 0) {
        Some(t) => t.with_timezone(&chrono::Local).format("%d.%m").to_string(),
        None => "??.??".to_string(),
    }
}

/// Схлопывает переводы строк и лишние пробелы: одно сообщение — одна строка.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{ActionDto, FwdDto, MediaDto, MessageDto, ReplyToDto};
    use crate::login::VkUserDto;

    fn user(name: &str) -> VkUserDto {
        VkUserDto {
            id: "1234".into(),
            first_name: Some(name.into()),
            ..VkUserDto::default()
        }
    }

    fn msg(id: i32, text: &str) -> MessageDto {
        MessageDto {
            id,
            dialog_id: "2000000001".into(),
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
    }

    /// Порядок строк — по возрастанию id, каким бы ни был порядок входа.
    #[test]
    fn lines_are_ordered_by_ascending_id() {
        let t = render(&[msg(30, "три"), msg(10, "один"), msg(20, "два")], 10_000);
        let ids: Vec<&str> = t.text.lines().map(|l| l.split(": ").nth(1).unwrap()).collect();
        assert_eq!(ids, ["один", "два", "три"]);
    }

    /// По метке на каждый вид вложения — и ни одного адреса.
    #[test]
    fn every_media_kind_gets_a_compact_label() {
        let table: [(MediaDto, &str); 9] = [
            (media("photo"), "[фото]"),
            (
                MediaDto {
                    title: Some("Отчёт".into()),
                    ..media("video")
                },
                "[видео: Отчёт]",
            ),
            (
                MediaDto {
                    title: Some("Nirvana".into()),
                    ..media("audio")
                },
                "[аудио: Nirvana]",
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
            (media("sticker"), "[стикер]"),
            (media("graffiti"), "[граффити]"),
            (
                MediaDto {
                    title: Some("Статья".into()),
                    link_url: Some("https://example.com/a".into()),
                    ..media("other")
                },
                "[ссылка: Статья]",
            ),
            (media("unsupported"), "[вложение: unsupported]"),
        ];
        for (m, expected) in table {
            let kind = m.kind.clone();
            let mut message = msg(1, "");
            message.attachments = vec![m];
            let t = render(&[message], MAX_TRANSCRIPT_CHARS);
            assert!(t.text.contains(expected), "{kind}: {}", t.text);
        }
    }

    /// Правило плагина «адреса не покидают бэкенд» — здесь оно строже, чем в
    /// DTO: транскрипт уезжает в ЧУЖОЙ процесс.
    #[test]
    fn rendered_text_never_contains_urls() {
        let mut m = msg(1, "смотри");
        m.attachments = vec![
            MediaDto {
                title: Some("Статья".into()),
                link_url: Some("https://evil.example/track?t=secret".into()),
                ..media("other")
            },
            MediaDto {
                player_url: Some("https://vk.com/video-1_2".into()),
                title: Some("Ролик".into()),
                ..media("video")
            },
        ];
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        for needle in ["http://", "https://", "://", "vk.com", "evil.example"] {
            assert!(!t.text.contains(needle), "{needle} в {}", t.text);
        }
    }

    /// Служебное сообщение — своя форма строки.
    #[test]
    fn action_messages_render_as_events() {
        let mut m = msg(1, "");
        m.action = Some(ActionDto {
            kind: "chat_invite_user".into(),
            member_id: Some("777".into()),
            text: "пригласил участника".into(),
        });
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        assert!(
            t.text.contains("* событие: Анна пригласил участника"),
            "{}",
            t.text
        );
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

    /// Пересылки: глубина видна отступом, отброшенный хвост — «…ещё N».
    #[test]
    fn forwards_carry_depth_and_omitted_tail() {
        let mut m = msg(3, "переслал");
        m.fwd = vec![
            FwdDto {
                depth: 0,
                sender: Some(user("Пётр")),
                date: 1_754_000_000,
                text: Some("смета готова".into()),
                omitted: 0,
                ..FwdDto::default()
            },
            FwdDto {
                depth: 1,
                sender: Some(user("Ольга")),
                date: 1_754_000_000,
                media_kind: Some("photo".into()),
                omitted: 4,
                ..FwdDto::default()
            },
        ];
        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        assert!(t.text.contains("» переслано от Пётр ("), "{}", t.text);
        assert!(t.text.contains("смета готова"), "{}", t.text);
        assert!(t.text.contains("»» переслано от Ольга ("), "{}", t.text);
        assert!(t.text.contains("фото"), "{}", t.text);
        assert!(t.text.contains("…ещё 4"), "{}", t.text);
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

    /// Правдоподобная переписка целиком: тест ГЛАЗАМИ (`--nocapture`
    /// печатает результат) плюс проверка того, что каждая форма строки
    /// действительно доехала до текста.
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
        docs.attachments = vec![
            MediaDto {
                file_name: Some("смета_v3.xlsx".into()),
                ..media("document")
            },
            media("photo"),
        ];

        let mut voice = msg(104, "");
        voice.attachments = vec![MediaDto {
            duration_secs: Some(14),
            ..media("voice")
        }];

        let mut event = msg(105, "");
        event.action = Some(ActionDto {
            kind: "chat_invite_user".into(),
            member_id: Some("777".into()),
            text: "пригласил участника".into(),
        });

        let mut forwarded = msg(106, "вот что прислал подрядчик");
        forwarded.fwd = vec![FwdDto {
            depth: 0,
            sender: Some(user("Пётр")),
            date: 1_754_000_000,
            text: Some("работы начнём в понедельник".into()),
            ..FwdDto::default()
        }];

        let t = render(
            &[hello, mine, docs, voice, event, forwarded],
            MAX_TRANSCRIPT_CHARS,
        );
        println!("--- транскрипт ---\n{}------------------", t.text);

        assert_eq!(t.messages_used, 6);
        assert!(!t.truncated);
        assert_eq!(t.oldest_id, Some(101));
        assert_eq!(t.text.lines().count(), 6, "по строке на сообщение");
        for needle in [
            "Анна: Привет!",
            "Я: ↳ Анна:",
            "[документ: смета_v3.xlsx] [фото]",
            "[голосовое 0:14]",
            "* событие: Анна пригласил участника",
            "» переслано от Пётр (",
        ] {
            assert!(t.text.contains(needle), "{needle} в\n{}", t.text);
        }
    }

    /// ИНВАРИАНТ УТЕЧКИ: рендер транскрипта НЕ ЛОГИРУЕТ НИЧЕГО.
    ///
    /// Здесь проходит вся переписка пользователя с живыми людьми, и соблазн
    /// поставить «временный» `tracing::debug!(text = %t.text)` при отладке
    /// бюджета максимальный. Тест краснеет на первом же таком событии: после
    /// [`render`] отрезок лога обязан остаться пустым — не «без канарейки», а
    /// именно пустым, потому что залогировать тут нечего в принципе.
    #[test]
    fn rendering_a_transcript_writes_nothing_to_the_logs() {
        const CANARY: &str = "КАНАРЕЙКА-ПЕРЕПИСКИ-13";
        let mut secret = msg(2, CANARY);
        secret.out = true;
        let messages = [msg(1, "привет"), secret, msg(3, "давай в пятницу")];

        let mark = crate::testlog::mark();
        let full = render(&messages, MAX_TRANSCRIPT_CHARS);
        // И урезанный путь тоже: маркер `[…опущено N…]` строится отдельной
        // веткой, у неё своя история «а давайте посмотрим, что режется».
        let cut = render(&messages, 40);
        let logged = crate::testlog::since(mark);

        assert!(full.text.contains(CANARY), "транскрипт обязан нести текст");
        assert!(cut.truncated, "второй вызов обязан пройти веткой обрезки");
        assert!(
            logged.is_empty(),
            "рендер транскрипта не имеет права писать в логи:\n{logged}"
        );
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
}
