//! Транскрипт переписки: выгрузка истории диалога и её рендер в текст для
//! Claude.
//!
//! # Сеть и рендер РАЗДЕЛЕНЫ, и это главное решение модуля
//!
//! [`fetch`] ходит в портал и не форматирует ни одной строки; [`render`] —
//! ЧИСТАЯ функция над `&[MessageDto]` и не знает ни про клиент, ни про
//! состояние. Слитые в одну функцию, они были бы непроверяемы: единственный
//! способ убедиться, что транскрипт читаем и не содержит адресов, — прогнать
//! рендер на литеральных DTO без сети и без вебхука.
//!
//! Сама пагинация тоже отвязана от клиента — через трейт [`HistorySource`]
//! (тот же приём, что у Telegram). Иначе «стык страниц не теряет и не дублирует
//! сообщение» проверялся бы только живым порталом, то есть не проверялся бы
//! вовсе.
//!
//! # Страницу берёт `load_page`, а НЕ клиент напрямую
//!
//! Продовый источник [`RoutePageSource`] зовёт
//! [`crate::routes::dialogs::load_page`] — ту же функцию, что обслуживает
//! `GET /api/bitrix/dialogs/:id/messages`. Она уже делает `absorb_users`,
//! `resolve_uncached`, проекцию `message_from_raw` и нормализацию порядка
//! ([`crate::dto::sort_messages_ascending`] — порядок ответа портала зависит от
//! направления пагинации). Собрать это заново значило бы завести вторую
//! реализацию истории, которая разъедется с первой на первом же исправлении, и
//! анализ видел бы переписку не такой, какой её видит пользователь.
//!
//! # БЮДЖЕТ ВЫГРУЗКИ: почему у Bitrix глубины не такие, как у VK и Telegram
//!
//! Потолок страницы у платформы — 50 сообщений
//! ([`crate::api::methods::MESSAGES_MAX_LIMIT`]), и подвинуть его нельзя:
//! `LIMIT=100` портал молча урезает. Бюджет интенсивности — 2 запроса в
//! секунду на пару портал+вебхук, и он ОБЩИЙ (`crate::api::throttle`): его
//! делят воркер событий (Ф7), фолбэк-синхронизация (раз в минуту) и все роуты
//! панели.
//!
//! Считаем VK-шные 500 сообщений:
//!
//! | | значение |
//! |---|---|
//! | страниц | 500 / 50 = **10** |
//! | вызовов портала | 10 (страницы) + до 10 (добор имён авторов внутри `load_page`) |
//! | из ведра бесплатно | `burst` = 5 |
//! | ожидание ведра | (10 − 5) / 2 rps = **2.5 с** минимум, и это без конкурентов |
//! | RTT портала | 0.3–0.8 с × 10 = **3–8 с** |
//! | **итого** | **5.5–10.5 с**, а при активном воркере событий — вдвое |
//!
//! Пагинация последовательна по построению (курсор следующей страницы — это
//! минимальный id предыдущей), распараллелить её нельзя. Десять секунд на
//! нажатие кнопки — это не «медленно», это выглядит как зависшая панель, и всё
//! это время realtime-события стоят в очереди за тем же ведром.
//!
//! **Решение — уменьшить глубину, а не показывать прогресс**:
//! [`DEPTH_CHOICES`] = `[50, 100, 200]`, дефолт [`DEFAULT_DEPTH`] = 100.
//! Худший случай теперь 200 сообщений = 4 страницы: `burst` покрывает их
//! целиком, остаётся 4 × RTT ≈ 1.5–3 с. Дефолтные 100 — это 2 страницы,
//! то есть меньше секунды.
//!
//! Прогресс отдельным кадром при этом НЕ вводится, и это не экономия, а
//! следствие: панель не молчит вообще ни секунды. Ядро попапа
//! (`js/messenger/analysis-core.js`) ставит статус «выгружаю переписку…» В ТОТ
//! ЖЕ КАДР, что и клик, ещё до отправки `POST /analysis` — то есть «молчащая
//! панель дольше двух секунд» отсутствует по построению, а не потому, что мы
//! успели за две секунды. Новый тип кадра ради дублирования уже показанного
//! статуса означал бы новую ось замороженного контракта на ровном месте.
//!
//! Бюджет воркера выгрузка не выедает: она идёт через ТОТ ЖЕ
//! `Arc<Throttle>` внутри [`crate::api::B24Client`] (один на секрет), то есть
//! физически не может обогнать лимит — token bucket выдаёт токены в порядке
//! ожидания, и разбор занимает своё место в очереди наравне с опросом событий,
//! а не поверх него. Верхняя граница расхода жёсткая: [`MAX_PAGES`] страниц на
//! запуск, сколько бы ни просил клиент.
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
//! границам сообщений, а модели — ссылаться на конкретную реплику. Форма
//! дословно совпадает с VK и Telegram: системная инструкция у трёх
//! мессенджеров ОДНА (она живёт в общем крейте), и разный формат транскрипта
//! означал бы, что промпт, отлаженный на одной вкладке, хуже работает в
//! соседней.
//!
//! # Дата приезжает СТРОКОЙ ATOM, а не unix-секундами
//!
//! `MessageDto.date` у Bitrix — `Option<String>` в формате
//! `2026-08-07T12:31:00+03:00`, потому что ровно так его отдаёт портал.
//! Отсюда [`parse_atom`] и два формата поверх неё: [`stamp`] (`ДД.ММ ЧЧ:ММ`) и
//! [`day`] (`ДД.ММ`). Смещение из строки СОХРАНЯЕТСЯ и переводится в локальную
//! зону — то есть транскрипт показывает то же время, что и лента, даже если
//! портал живёт в другом часовом поясе. Ни `unwrap`, ни `expect`: `None` и
//! мусор дают запасную метку `??.?? ??:??`, а не панику на чужих данных.
//!
//! # Цитаты живут ВНУТРИ BB-кодов
//!
//! `reply_to` и `fwd` у Bitrix ВСЕГДА `None` (Р10, §6) — соответствующих
//! веток VK-рендера здесь нет вовсе, они не закомментированы, а отсутствуют.
//! Единственный след ответа — текстовая цитата в самом сообщении, в одной из
//! двух форм:
//!
//! | форма | откуда |
//! |---|---|
//! | `[QUOTE]Автор: текст[/QUOTE]` | веб-клиент портала |
//! | `> Автор: текст` ведущими строками | наша отправка (`compose_reply`) |
//!
//! [`split_quote`] отделяет её от тела ответа — построчный аналог фронтового
//! `js/bitrix/reply.js::splitQuote`, вплоть до потолка длины автора. Важно
//! именно ОТДЕЛИТЬ, а не просто найти: тело строки берётся из ОСТАТКА
//! сегментов, поэтому процитированный текст попадает в транскрипт ровно один
//! раз — как цитата, а не вторым экземпляром внутри реплики.
//!
//! # Инвариант «адреса не покидают бэкенд»
//!
//! В транскрипте НЕТ ни одного URL, который добавили мы: медиа превращается в
//! компактные метки (`[фото]`, `[документ: смета.xlsx]`), а адреса прокси,
//! Диска портала и аватаров не переносятся вовсе. Здесь это правило строже,
//! чем у DTO: подписанный адрес, уехавший в промпт, — утечка в ЧУЖОЙ процесс,
//! а не в свою же вкладку. Инвариант закреплён тестом
//! `rendered_text_never_contains_urls`.
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

use crate::api::B24Client;
use crate::attach::{media_kind, MediaDto};
use crate::bbcode::{kind, Segment};
use crate::dto::{MessageDto, SenderDto};
use crate::routes::error::ApiError;
use crate::state::B24State;

/// Допустимые значения глубины выгрузки (сообщений).
///
/// Закрытый список, а не диапазон: глубина приезжает с фронта селектом, и
/// произвольное число здесь означало бы, что пользователь способен заказать
/// выгрузку на сотню запросов к порталу.
///
/// Список КОРОЧЕ VK-шного (`[50, 200, 500]`) — обоснование в шапке модуля:
/// потолок страницы 50 при общем бюджете 2 rps делает 500 сообщений
/// пятисекундной паузой в лучшем случае.
pub const DEPTH_CHOICES: [i64; 3] = [50, 100, 200];

/// Глубина по умолчанию.
///
/// 100 — это ДВЕ страницы портала, то есть два вызова из пятитокенного залпа:
/// разбор начинается быстрее, чем пользователь успевает перевести взгляд на
/// попап. 200 не сделано дефолтом намеренно: это уже четыре вызова на КАЖДЫЙ
/// разбор, и на активном чате они конкурируют с воркером событий.
pub const DEFAULT_DEPTH: i64 = 100;

/// Потолок транскрипта в CODE POINTS.
///
/// ~15 тысяч токенов — заметная, но не разорительная доля окна; всё, что
/// сверху, режется по старым сообщениям. Совпадает с VK и Telegram.
pub const MAX_TRANSCRIPT_CHARS: usize = 60_000;

/// Потолок одного сообщения в CODE POINTS.
///
/// Одна простыня на 50 000 символов не должна съедать весь транскрипт: для
/// понимания разговора хватает начала реплики.
pub const MAX_MESSAGE_CHARS: usize = 1_500;

/// Потолок превью цитаты в CODE POINTS.
///
/// Совпадает с `crate::routes::dialogs::QUOTE_MAX_CHARS`: ровно столько
/// оставляет от оригинала наша же отправка, и показывать в транскрипте больше
/// всё равно нечего.
pub const MAX_QUOTE_CHARS: usize = 200;

/// Максимальная длина имени автора в цитате, символов.
///
/// Дословно совпадает с фронтовым
/// `js/bitrix/reply.js::QUOTE_AUTHOR_MAX_CHARS`. Двоеточие дальше этой границы
/// — знак препинания, а не подпись: «Внимание: смотри вложение» автора не
/// содержит.
pub const QUOTE_AUTHOR_MAX_CHARS: usize = 64;

/// Страховка от бесконечного цикла пагинации И ЖЁСТКИЙ ПОТОЛОК РАСХОДА.
///
/// Ровно столько страниц нужно самой глубокой выгрузке (200 / 50 = 4) плюс
/// одна на случай, если портал отдал короткую страницу в середине истории.
/// Дальше цикл обязан закончиться сам, сколько бы клиент ни просил: это
/// верхняя граница того, что один разбор способен отнять у бюджета 2 rps.
pub const MAX_PAGES: usize = 5;

/// Максимум сообщений за один запрос — платформенный потолок
/// [`crate::api::methods::MESSAGES_MAX_LIMIT`].
///
/// Продублирован здесь не ради удобства, а ради читаемости расчёта бюджета в
/// шапке модуля: все числа этого модуля выведены из него.
pub const MAX_PAGE: i64 = crate::api::methods::MESSAGES_MAX_LIMIT;

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
/// попросивший 400, хотел «побольше», и молча отдать ему 100 — значит соврать
/// ровно там, где он старался. Заодно это делает старый закешированный фронт
/// (с VK-шными 500 в селекте) работоспособным: он получит 200, а не отказ.
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

/// Режет размер страницы к платформенному потолку.
#[must_use]
fn clamp_page(want: i64) -> i64 {
    want.clamp(1, MAX_PAGE)
}

// ---------------------------------------------------------------------------
// Сеть.
// ---------------------------------------------------------------------------

/// Откуда берутся страницы истории.
///
/// Существует ради тестируемости пагинации: [`fetch_with`] и
/// [`fetch_since_with`] — это ЛОГИКА (курсор, потолки, обрезка), и проверять
/// её живым порталом невозможно. Продовая реализация ровно одна —
/// [`RoutePageSource`], и она не содержит ни одного `if`.
#[async_trait]
pub trait HistorySource: Send + Sync {
    /// Одна страница истории, СТРОГО старее `before_id` (`LAST_ID`, листаем
    /// вверх), не более `limit` записей.
    ///
    /// Направление ровно одно, хотя портал умеет и `FIRST_ID` («строго
    /// новее»): ОБЕ выгрузки — и полная, и инкрементальная — листают историю
    /// от свежих к старым. Почему так даже у REFRESH, объяснено у
    /// [`fetch_since_with`].
    ///
    /// Порядок внутри страницы значения не имеет — вызывающий всё равно
    /// сортирует.
    ///
    /// # Errors
    ///
    /// Любая ошибка портала, уже переведённая в конверт плагина.
    async fn page(&self, limit: i64, before_id: Option<i64>)
        -> Result<Vec<MessageDto>, ApiError>;
}

/// Продовый источник: страницу берёт [`crate::routes::dialogs::load_page`].
///
/// Держит `Arc` на состояние и клиента, а не ссылки: источник переживает
/// несколько `await`, и заимствования сделали бы сигнатуры [`fetch`]
/// неприятными без единой выгоды.
pub struct RoutePageSource {
    state: Arc<B24State>,
    client: Arc<B24Client>,
    dialog_id: String,
}

impl RoutePageSource {
    /// Оборачивает состояние и клиента, взятых `require_ready`.
    #[must_use]
    pub fn new(state: Arc<B24State>, client: Arc<B24Client>, dialog_id: String) -> Self {
        Self {
            state,
            client,
            dialog_id,
        }
    }
}

#[async_trait]
impl HistorySource for RoutePageSource {
    async fn page(
        &self,
        limit: i64,
        before_id: Option<i64>,
    ) -> Result<Vec<MessageDto>, ApiError> {
        crate::routes::dialogs::load_page(
            &self.state,
            &self.client,
            &self.dialog_id,
            limit,
            before_id,
            // `after_id` не передаётся никогда: обе выгрузки листают вверх.
            None,
        )
        .await
    }
}

/// Выгружает последние `depth` сообщений диалога, от старых к новым.
///
/// # Errors
///
/// Любая ошибка портала (см. [`HistorySource::page`]).
pub async fn fetch(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    dialog_id: &str,
    depth: i64,
) -> Result<Vec<MessageDto>, ApiError> {
    let source = RoutePageSource::new(state.clone(), client.clone(), dialog_id.to_string());
    fetch_with(&source, depth).await
}

/// Тело [`fetch`] над произвольным источником страниц.
///
/// # Errors
///
/// Ошибка источника.
pub async fn fetch_with<S: HistorySource + ?Sized>(
    source: &S,
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
        let limit = clamp_page(want);
        let page = source.page(limit, before).await?;
        let min_id = page.iter().map(|m| m.id).min();
        let full = page.len() as i64 >= limit;
        out.extend(page);

        // Страница короче запрошенной ⇒ история кончилась.
        if !full {
            break;
        }
        // Курсор не сдвинулся — выходим, иначе следующая страница пришла бы
        // та же самая. Единственная защита сверх [`MAX_PAGES`].
        //
        // Курсор ЭКСКЛЮЗИВЕН (`LAST_ID` отдаёт строго старее), поэтому им
        // служит МИНИМАЛЬНЫЙ id страницы: `min_id - 1` терял бы ровно одно
        // сообщение на каждом стыке, а `min_id + 1` — задваивал бы его.
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
///    Поэтому обычный REFRESH стоит ОДНОГО запроса, а не четырёх.
/// 2. **Переполнение честно объявляется.** Если новых сообщений оказалось
///    больше `cap` (или они не уместились в [`MAX_PAGES`]), берутся САМЫЕ
///    НОВЫЕ, а [`NewMessages::truncated`] поднимается — модель обязана знать,
///    что кусок переписки между прошлым разбором и этим она не видела.
///
/// # Почему НЕ `FIRST_ID`, хотя портал его умеет
///
/// «Отдай строго новее указанного» выглядит как прямой ответ на вопрос
/// REFRESH'а и стоит ровно столько же — один запрос, пока нового мало. Но при
/// переполнении такой обход даёт САМЫЕ СТАРЫЕ из новых: он идёт от границы
/// вперёд и упирается в потолок страниц раньше, чем доходит до хвоста
/// переписки. То есть пользователь, не заглядывавший в чат неделю, получил бы
/// разбор позапрошлого понедельника вместо сегодняшнего дня — молча. Обход
/// назад в том же бюджете всегда начинается с конца, и обрезка отсекает
/// правильный край.
///
/// # Errors
///
/// Любая ошибка портала.
pub async fn fetch_since(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    dialog_id: &str,
    since_id: i64,
    cap: i64,
) -> Result<NewMessages, ApiError> {
    let source = RoutePageSource::new(state.clone(), client.clone(), dialog_id.to_string());
    fetch_since_with(&source, since_id, cap).await
}

/// Тело [`fetch_since`] над произвольным источником страниц.
///
/// # Errors
///
/// Ошибка источника.
pub async fn fetch_since_with<S: HistorySource + ?Sized>(
    source: &S,
    since_id: i64,
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
        let limit = clamp_page(want);
        let page = source.page(limit, before).await?;
        let min_id = page.iter().map(|m| m.id).min();
        let full = page.len() as i64 >= limit;
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
/// Ноль запросов к сети: карточка диалога стоила бы отдельного вызова
/// `im.dialog.get` на КАЖДОМ разборе, то есть половины секунды бюджета ради
/// украшения шапки. Берётся автор первого ВХОДЯЩЕГО сообщения — для лички это
/// ровно собеседник, для группового чата просто первый заговоривший, и в обоих
/// случаях это лучше пустоты.
///
/// Пустая строка — легальный ответ (весь кусок истории исходящий либо авторов
/// нет вовсе): `prompt::build` подставит «без названия».
#[must_use]
pub fn title_from_messages(messages: &[MessageDto]) -> String {
    messages
        .iter()
        .filter(|m| !m.out && !m.system)
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
    let lines: Vec<(i64, String)> = ordered
        .iter()
        .filter_map(|m| render_message(m).map(|line| (m.id, line)))
        .collect();

    // Собираем С КОНЦА: новые сообщения важнее старых.
    let mut kept: Vec<(i64, String)> = Vec::new();
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
        // Идентификаторы портала растут ШАГОМ 2 и в `i32` укладываются далеко
        // не всегда — а `Transcript` общий на три мессенджера и хранит `i32`.
        // Сужение насыщением, а не `as`: молча завёрнутый id стал бы курсором
        // следующего REFRESH и увёл бы выгрузку в начало истории.
        oldest_id: kept.first().map(|(id, _)| narrow_id(*id)),
        newest_id: kept.last().map(|(id, _)| narrow_id(*id)),
    }
}

/// Сужает `i64`-идентификатор портала до `i32` общего [`Transcript`].
///
/// Насыщением, а не обрезкой: `as i32` от большого id даёт отрицательное
/// число, которое в роли курсора REFRESH означало бы «выгрузить всё с самого
/// начала». Насыщение в худшем случае даёт «нового ничего нет» — ошибка в
/// безопасную сторону.
#[must_use]
fn narrow_id(id: i64) -> i32 {
    i32::try_from(id).unwrap_or(i32::MAX)
}

/// Первая строка обрезанного транскрипта.
fn omitted_marker(dropped: usize) -> String {
    format!("[…опущено {dropped} более старых сообщений]")
}

/// Одно сообщение → строка транскрипта; `None`, если рисовать нечего.
fn render_message(m: &MessageDto) -> Option<String> {
    // Системная строка — отдельная форма: у неё нет ни автора в привычном
    // смысле, ни текста от человека. Ровно как у VK.
    if m.system {
        let body = one_line(&m.text);
        let body = if body.is_empty() {
            m.action
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("служебное сообщение")
                .to_string()
        } else {
            truncate_chars(&body, MAX_MESSAGE_CHARS)
        };
        return Some(format!("[{}] * событие: {}", stamp(m.date.as_deref()), body));
    }

    // Цитата отделяется ОТ СЕГМЕНТОВ, а не ищется в `text`: тело реплики нужно
    // взять из остатка, иначе процитированный кусок попал бы в строку дважды.
    let split = split_quote(&m.segments);
    let (quote, body_text) = match &split {
        Some(q) => (Some(&q.quote), one_line(&q.body)),
        None => (None, one_line(&m.text)),
    };

    if body_text.is_empty() && m.media.is_empty() && quote.is_none() {
        return None;
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = quote {
        parts.push(quote_label(q));
    }
    if !body_text.is_empty() {
        parts.push(truncate_chars(&body_text, MAX_MESSAGE_CHARS));
    }
    for media in &m.media {
        parts.push(media_label(media));
    }

    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "[{}] {}: {}",
        stamp(m.date.as_deref()),
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

/// Имя автора: `name`, иначе числовой id.
///
/// Собственный хелпер, а не метод DTO: числовой id в транскрипте — крайний
/// случай, и подставлять его вместо имени осмысленно только здесь (в UI это
/// делает фронт, и правила у него свои).
fn speaker_name(u: &SenderDto) -> String {
    if let Some(n) = u.name.as_deref() {
        let t = n.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    u.id.trim().to_string()
}

// ---------------------------------------------------------------------------
// Цитата внутри BB-кодов.
// ---------------------------------------------------------------------------

/// Ведущая цитата, отделённая от тела ответа.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quote {
    /// Автор цитаты, если он в ней подписан. Пусто — подписи не было.
    pub author: String,
    /// Процитированный текст (уже без подписи автора).
    pub text: String,
}

/// Цитата плюс остаток сообщения.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitMessage {
    /// Сама цитата.
    pub quote: Quote,
    /// Плоский текст ответа БЕЗ цитаты.
    pub body: String,
}

/// Отделяет ведущую цитату от самого ответа.
///
/// Разбираются ОБЕ формы, доживающие до ленты (см. шапку модуля). Построчный
/// аналог фронтового `js/bitrix/reply.js::splitQuote` — и это не дублирование
/// ради дублирования: там она нужна для клика по цитате, здесь для того, чтобы
/// модель поняла, кто кому отвечал, а общего слоя между Rust и браузером нет.
///
/// Сообщение, состоящее ИЗ ОДНОЙ цитаты, цитатой не считается: это просто
/// реплика, начатая с `>`, и прятать её текст в подпись значило бы потерять
/// содержимое. Возвращает `None` и на битой разметке — [`crate::bbcode`] не
/// паникует ни на каком входе, любой неразобранный кусок становится текстом.
#[must_use]
pub fn split_quote(segments: &[Segment]) -> Option<SplitMessage> {
    let head = segments.first()?;

    // Форма 1: первый сегмент — `[QUOTE]…[/QUOTE]` веб-клиента портала.
    if head.kind == kind::QUOTE {
        let quoted: String = head.children.iter().map(Segment::plain).collect();
        let quote = parse_quote_body(&quoted)?;
        let body: String = segments[1..].iter().map(Segment::plain).collect();
        if body.trim().is_empty() {
            return None;
        }
        return Some(SplitMessage { quote, body });
    }

    // Форма 2: ведущие строки `> …` внутри первого ТЕКСТОВОГО сегмента —
    // так цитату приписывает наша собственная отправка (`compose_reply`).
    if head.kind != kind::TEXT {
        return None;
    }
    let lines: Vec<&str> = head.text.split('\n').collect();
    let mut quoted: Vec<&str> = Vec::new();
    let mut taken = 0usize;
    while taken < lines.len() {
        match strip_quote_prefix(lines[taken]) {
            Some(rest) => {
                quoted.push(rest);
                taken += 1;
            }
            None => break,
        }
    }
    if quoted.is_empty() {
        return None;
    }

    let rest = lines[taken..].join("\n");
    let tail: String = segments[1..].iter().map(Segment::plain).collect();
    let body = format!("{rest}{tail}");
    if body.trim().is_empty() {
        return None;
    }
    let quote = parse_quote_body(&quoted.join(" "))?;
    Some(SplitMessage { quote, body })
}

/// Снимает префикс цитаты (`>` с необязательным пробелом) — либо `None`, если
/// строка цитатой не является.
///
/// До трёх ведущих пробелов, как у фронтового `QUOTE_PREFIX_RE`: портал иногда
/// добавляет отступ, а четвёртый пробел — это уже форматирование текста, а не
/// цитата.
fn strip_quote_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let rest = trimmed.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Разбирает тело цитаты на автора и текст.
///
/// Разделитель — ПЕРВОЕ `": "`, ровно так его ставит `compose_reply`. Разбор
/// заведомо приблизителен: цитата «Внимание: смотри вложение» без автора даст
/// автора «Внимание». Это осознанный размен — альтернатива (не разбирать
/// автора вовсе) стоила бы модели знания о том, КОМУ отвечали, в подавляющем
/// большинстве реальных цитат.
#[must_use]
pub fn parse_quote_body(raw: &str) -> Option<Quote> {
    let flat = one_line(raw);
    if flat.is_empty() {
        return None;
    }
    // Позиция ищется в СИМВОЛАХ, а не байтах: у кириллического имени байтовый
    // индекс вдвое больше, и потолок в 64 срабатывал бы вдвое раньше.
    if let Some(byte_sep) = flat.find(": ") {
        let chars_before = flat[..byte_sep].chars().count();
        if chars_before > 0 && chars_before <= QUOTE_AUTHOR_MAX_CHARS {
            return Some(Quote {
                author: flat[..byte_sep].to_string(),
                text: flat[byte_sep + 2..].to_string(),
            });
        }
    }
    Some(Quote {
        author: String::new(),
        text: flat,
    })
}

/// Цитата одной вставкой: `↳ Автор: «превью»`.
///
/// Та же форма, что у VK и Telegram, — там она собирается из поля `reply_to`,
/// здесь из BB-кодов, но модель обязана видеть ОДИН формат: системная
/// инструкция общая на три мессенджера.
fn quote_label(q: &Quote) -> String {
    let preview = if q.text.trim().is_empty() {
        "сообщение".to_string()
    } else {
        truncate_chars(q.text.trim(), MAX_QUOTE_CHARS)
    };
    let author = q.author.trim();
    if author.is_empty() {
        format!("↳ «{preview}»")
    } else {
        format!("↳ {author}: «{preview}»")
    }
}

/// Компактная метка вложения. НИ ОДНОГО адреса — см. шапку модуля.
fn media_label(m: &MediaDto) -> String {
    let file = m
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let word = match m.kind.as_str() {
        media_kind::IMAGE => return "[фото]".to_string(),
        media_kind::VIDEO => "видео",
        media_kind::VIDEO_NOTE => "видеосообщение",
        media_kind::AUDIO => "аудио",
        media_kind::VOICE => "голосовое",
        // `file` — закрытая корзина словаря DTO: всё, что портал прислал в
        // виде, который UI не умеет разложить.
        _ => "документ",
    };
    labeled(word, file)
}

/// `[слово]` либо `[слово: файл]`.
///
/// Длительности у портала нет вовсе (в отличие от Telegram): ни голосовое, ни
/// видео её не несут, и ветки под неё здесь нет — она бы никогда не сработала.
fn labeled(word: &str, file: Option<&str>) -> String {
    let body = match file {
        Some(f) => format!("{word}: {}", truncate_chars(f, MEDIA_TITLE_CHARS)),
        None => word.to_string(),
    };
    format!("[{body}]")
}

// ---------------------------------------------------------------------------
// Дата: строка ATOM вместо unix-секунд.
// ---------------------------------------------------------------------------

/// Разбирает дату портала (ATOM / RFC 3339) С СОХРАНЕНИЕМ смещения.
///
/// `None` на отсутствующей и на любой неразобранной строке — никаких
/// `unwrap`/`expect`: дата приезжает из чужой системы, и панику на её формате
/// нельзя ни предсказать, ни воспроизвести у пользователя.
///
/// Смещение сохраняется, а не отбрасывается: портал живёт в часовом поясе
/// компании, и `2026-08-07T12:31:00+03:00` — это 12:31 у коллег, а не 09:31.
#[must_use]
pub fn parse_atom(date: Option<&str>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let raw = date?.trim();
    if raw.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(raw).ok()
}

/// Дата ATOM → `ДД.ММ ЧЧ:ММ` в локальной зоне пользователя.
///
/// Локальная, а не зона портала, намеренно: транскрипт читает модель,
/// отвечающая ЧЕЛОВЕКУ («вчера вечером договорились…»), и человек этот сидит в
/// своём часовом поясе. Ровно так же поступают VK и Telegram — там на входе
/// unix-секунды, но результат тот же.
///
/// Неразобранная дата даёт `??.?? ??:??`: строка транскрипта не имеет права
/// пропасть из-за формата метки времени.
#[must_use]
pub fn stamp(date: Option<&str>) -> String {
    match parse_atom(date) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%d.%m %H:%M")
            .to_string(),
        None => "??.?? ??:??".to_string(),
    }
}

/// Дата ATOM → `ДД.ММ` (день без времени).
///
/// Второй формат поверх того же [`parse_atom`] — для дат, у которых время
/// смысла не несёт (срок, день события). Отдельная функция, а не срез от
/// [`stamp`]: разбор один, а форматов два, и срез строки сломался бы на первой
/// же правке формата метки.
///
/// В самом транскрипте потребителя пока нет — единственный кандидат, карточка
/// задачи для чатов `TASKS_TASK`, отклонён по бюджету (`tasks.task.get` — это
/// лишний запрос при общем лимите 2 rps, см. шапку модуля). Функция остаётся
/// частью публичной поверхности разбора дат вместе со [`stamp`] и покрыта теми
/// же тестами: разводить их появлением потребителя значило бы разводить и
/// проверки разбора.
#[must_use]
pub fn day(date: Option<&str>) -> String {
    match parse_atom(date) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%d.%m")
            .to_string(),
        None => "??.??".to_string(),
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

    use super::*;

    // -----------------------------------------------------------------------
    // Фикстуры.
    // -----------------------------------------------------------------------

    /// Сообщение с текстом, разобранным теми же BB-кодами, что в проде.
    ///
    /// Именно `to_segments`, а не литеральные сегменты: цитата ищется в
    /// сегментах, и тест на литералах проверял бы мою фантазию о разборе, а не
    /// сам разбор.
    fn msg(id: i64, out: bool, who: &str, text: &str, date: &str) -> MessageDto {
        MessageDto {
            id,
            dialog_id: "chat7".into(),
            out,
            date: (!date.is_empty()).then(|| date.to_string()),
            sender: (!who.is_empty()).then(|| SenderDto {
                id: "42".into(),
                name: Some(who.to_string()),
                ..SenderDto::default()
            }),
            text: crate::bbcode::to_plain(text),
            segments: crate::bbcode::to_segments(text),
            ..MessageDto::default()
        }
    }

    fn media(kind: &str, name: Option<&str>) -> MediaDto {
        MediaDto {
            index: 0,
            file_id: 100,
            kind: kind.to_string(),
            name: name.map(str::to_string),
            has_full: true,
            ..MediaDto::default()
        }
    }

    /// Источник страниц из заранее заданной истории — вся сеть теста.
    ///
    /// Держит ПОЛНУЮ историю и режет её так же, как портал под `LAST_ID`:
    /// строго старее курсора и в порядке УБЫВАНИЯ id. Обратный порядок здесь
    /// не педантизм — именно на нём ловится ошибка выбора курсора: с
    /// возрастающей страницей неверный `min_id` дал бы тот же результат.
    struct FakeSource {
        history: Vec<MessageDto>,
        calls: AtomicUsize,
        seen: Mutex<Vec<(i64, Option<i64>)>>,
    }

    impl FakeSource {
        fn new(history: Vec<MessageDto>) -> Self {
            Self {
                history,
                calls: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl HistorySource for FakeSource {
        async fn page(
            &self,
            limit: i64,
            before_id: Option<i64>,
        ) -> Result<Vec<MessageDto>, ApiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push((limit, before_id));

            let take = usize::try_from(limit).unwrap_or(0);
            let mut page: Vec<MessageDto> = self
                .history
                .iter()
                .rev()
                .filter(|m| before_id.is_none_or(|b| m.id < b))
                .take(take)
                .cloned()
                .collect();
            page.sort_by_key(|m| std::cmp::Reverse(m.id));
            Ok(page)
        }
    }

    fn history(count: i64) -> Vec<MessageDto> {
        // Шаг 2 — как у настоящего портала: сплошная нумерация спрятала бы
        // ошибку вида `min_id - 1`, которая на шаге 2 попадает в пустоту.
        (0..count)
            .map(|i| msg(2 + i * 2, i % 2 == 0, "Игорь", &format!("реплика {i}"), ""))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Глубина и бюджет (решение задачи 3.4).
    // -----------------------------------------------------------------------

    /// ЗАФИКСИРОВАННОЕ РЕШЕНИЕ БЮДЖЕТА: у Bitrix глубины короче VK-шных.
    ///
    /// Обоснование целиком — в шапке модуля. Тест существует, чтобы возврат
    /// к 500 требовал осознанной правки, а не проходил как «выравнивание с
    /// остальными плагинами».
    #[test]
    fn the_depth_ladder_fits_the_two_rps_budget() {
        assert_eq!(DEPTH_CHOICES, [50, 100, 200]);
        assert_eq!(DEFAULT_DEPTH, 100);
        assert!(DEPTH_CHOICES.contains(&DEFAULT_DEPTH));
        assert_eq!(MAX_PAGE, crate::api::methods::MESSAGES_MAX_LIMIT);

        // Самая глубокая выгрузка обязана укладываться в потолок страниц —
        // иначе `MAX_PAGES` резал бы то, что пользователь честно заказал.
        let deepest = *DEPTH_CHOICES.iter().max().unwrap();
        let pages = (deepest + MAX_PAGE - 1) / MAX_PAGE;
        assert!(
            pages <= MAX_PAGES as i64,
            "{deepest} сообщений это {pages} страниц при потолке {MAX_PAGES}"
        );
        // …и при этом не должна вылезать за пятитокенный залп ведра больше чем
        // на один вызов: именно из этого выведен список глубин.
        assert!(pages <= 5, "залп burst=5 обязан покрывать выгрузку целиком");
    }

    #[test]
    fn clamp_depth_picks_the_nearest_choice() {
        assert_eq!(clamp_depth(50), 50);
        assert_eq!(clamp_depth(100), 100);
        assert_eq!(clamp_depth(200), 200);
        // Старый закешированный фронт со VK-шными 500 получает 200, а не отказ.
        assert_eq!(clamp_depth(500), 200);
        assert_eq!(clamp_depth(0), 50);
        assert_eq!(clamp_depth(-100), 50);
        assert_eq!(clamp_depth(i64::MAX), 200);
    }

    // -----------------------------------------------------------------------
    // Выгрузка.
    // -----------------------------------------------------------------------

    /// СТЫК СТРАНИЦ не теряет и не дублирует сообщение.
    ///
    /// Главный тест пагинации: курсор `LAST_ID` эксклюзивен, поэтому им служит
    /// МИНИМАЛЬНЫЙ id страницы. `min_id - 1` потерял бы по сообщению на каждом
    /// стыке (а на шаге 2 — попал бы в пустоту и не потерял, из-за чего ошибка
    /// дожила бы до прода), `min_id + 1` — задвоил бы его.
    #[tokio::test]
    async fn page_seams_lose_nothing_and_duplicate_nothing() {
        let all = history(200);
        let src = FakeSource::new(all.clone());
        let got = fetch_with(&src, 200).await.unwrap();

        assert_eq!(got.len(), 200, "выгружено ровно столько, сколько просили");
        assert_eq!(src.calls(), 4, "200 сообщений это 4 страницы по 50");

        let ids: Vec<i64> = got.iter().map(|m| m.id).collect();
        let want: Vec<i64> = all.iter().map(|m| m.id).collect();
        assert_eq!(ids, want, "порядок по возрастанию id, без дыр и дублей");

        // Курсор каждой следующей страницы — минимальный id предыдущей.
        let seen = src.seen.lock().unwrap().clone();
        assert_eq!(seen[0].1, None, "первая страница идёт без курсора");
        assert_eq!(seen[1].1, Some(2 + 150 * 2), "курсор = min id страницы 1");
        assert_eq!(seen[2].1, Some(2 + 100 * 2));
        assert_eq!(seen[3].1, Some(2 + 50 * 2));
    }

    /// История короче запрошенного — цикл останавливается сам, лишних вызовов
    /// нет: каждый из них стоит половины секунды общего бюджета.
    #[tokio::test]
    async fn a_short_history_stops_the_loop_immediately() {
        let src = FakeSource::new(history(7));
        let got = fetch_with(&src, 200).await.unwrap();
        assert_eq!(got.len(), 7);
        assert_eq!(src.calls(), 1, "семь сообщений — это один запрос");
    }

    /// Потолок страниц — ЖЁСТКАЯ верхняя граница расхода бюджета.
    ///
    /// Даже если попросить бесконечность, разбор не имеет права занять портал
    /// больше чем на [`MAX_PAGES`] вызовов: за тем же ведром стоит воркер
    /// событий.
    #[tokio::test]
    async fn the_page_ceiling_caps_what_one_analysis_can_spend() {
        let src = FakeSource::new(history(10_000));
        let got = fetch_with(&src, i64::MAX).await.unwrap();
        assert!(src.calls() <= MAX_PAGES, "вызовов {}", src.calls());
        assert_eq!(got.len(), 200, "глубина всё равно клампится списком");
        // Взяты САМЫЕ НОВЫЕ, а не самые старые: вопрос почти всегда о том, чем
        // разговор кончился.
        assert_eq!(got.last().unwrap().id, 2 + 9_999 * 2);
    }

    /// Источник, перестав двигать курсор, не зацикливает выгрузку.
    #[tokio::test]
    async fn a_stuck_cursor_does_not_spin_forever() {
        struct Stuck;
        #[async_trait]
        impl HistorySource for Stuck {
            async fn page(
                &self,
                limit: i64,
                _before: Option<i64>,
            ) -> Result<Vec<MessageDto>, ApiError> {
                // Всегда полная страница из ОДНОГО и того же сообщения с
                // нулевым id — то есть курсор двинуть нечем.
                Ok((0..limit).map(|_| msg(0, false, "Х", "т", "")).collect())
            }
        }
        let got = fetch_with(&Stuck, 200).await.unwrap();
        assert!(got.len() <= 1, "дубли схлопнулись, цикл вышел");
    }

    /// REFRESH подтягивает ТОЛЬКО новое и стоит одного запроса.
    #[tokio::test]
    async fn refresh_fetches_only_what_is_new() {
        let all = history(120);
        let since = all[100].id;
        let src = FakeSource::new(all.clone());
        let got = fetch_since_with(&src, since, 200).await.unwrap();

        assert_eq!(src.calls(), 1, "одна страница — весь REFRESH");
        assert_eq!(got.messages.len(), 19, "всё, что новее сотого");
        assert!(got.messages.iter().all(|m| m.id > since));
        assert!(!got.truncated);
        // Одна страница с хвоста: курсора нет, и он не понадобился — граница
        // уже разобранного встретилась внутри первой же страницы.
        let seen = src.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, None);
    }

    /// Новых нет — пустой результат, а не ошибка: кнопку жмут дважды подряд.
    #[tokio::test]
    async fn refresh_with_nothing_new_is_empty_and_not_truncated() {
        let all = history(30);
        let newest = all.last().unwrap().id;
        let src = FakeSource::new(all);
        let got = fetch_since_with(&src, newest, 200).await.unwrap();
        assert!(got.messages.is_empty());
        assert!(!got.truncated, "«ничего нового» — это не обрезка");
    }

    /// Новых больше потолка — берутся САМЫЕ НОВЫЕ, и это объявляется вслух.
    ///
    /// Ровно ради этого случая REFRESH листает НАЗАД, а не по `FIRST_ID`:
    /// обход вперёд упёрся бы в потолок страниц на позапрошлой неделе и отдал
    /// бы разбор устаревшего куска переписки, ничего об этом не сказав.
    #[tokio::test]
    async fn refresh_overflow_keeps_the_newest_and_says_so() {
        let all = history(400);
        let src = FakeSource::new(all.clone());
        let got = fetch_since_with(&src, 0, 200).await.unwrap();

        assert_eq!(got.messages.len(), 200);
        assert!(got.truncated, "модель обязана знать, что видела не всё");
        assert_eq!(
            got.messages.last().unwrap().id,
            all.last().unwrap().id,
            "хвост переписки на месте"
        );
        assert!(
            got.messages.first().unwrap().id > all[100].id,
            "обрезали СТАРЫЙ край, а не новый"
        );
    }

    /// Граница разобранного отфильтровывается, а не приезжает вторым
    /// экземпляром: задвоенное сообщение в транскрипте выглядит как повтор
    /// собеседника.
    #[tokio::test]
    async fn the_boundary_message_itself_is_not_repeated() {
        let all = history(30);
        let since = all[20].id;
        let src = FakeSource::new(all);
        let got = fetch_since_with(&src, since, 50).await.unwrap();
        assert!(
            got.messages.iter().all(|m| m.id > since),
            "само разобранное сообщение в выдачу не попадает"
        );
        assert_eq!(got.messages.len(), 9);
    }

    #[test]
    fn the_title_comes_from_the_first_incoming_author() {
        let msgs = vec![
            msg(2, true, "Я", "привет", ""),
            msg(4, false, "Анна Петрова", "здравствуйте", ""),
            msg(6, false, "Игорь", "и я тут", ""),
        ];
        assert_eq!(title_from_messages(&msgs), "Анна Петрова");
        // Весь кусок исходящий — пустая строка, а не выдумка.
        assert_eq!(title_from_messages(&msgs[..1]), "");
        assert_eq!(title_from_messages(&[]), "");
    }

    /// Системная строка автором чата НЕ становится: «Игорь создал чат» — это
    /// событие ленты, а не собеседник.
    #[test]
    fn a_system_line_is_not_a_chat_title() {
        let mut system = msg(2, false, "Робот", "чат создан", "");
        system.system = true;
        let msgs = vec![system, msg(4, false, "Анна", "привет", "")];
        assert_eq!(title_from_messages(&msgs), "Анна");
    }

    // -----------------------------------------------------------------------
    // Дата строкой ATOM (специфическая сложность №2).
    // -----------------------------------------------------------------------

    /// Все четыре случая разбора: валидная строка, ненулевое смещение, `None`,
    /// мусор. Ни одного `unwrap` на чужих данных — панику на формате даты
    /// портала невозможно ни предсказать, ни воспроизвести у пользователя.
    #[test]
    fn atom_dates_parse_or_degrade_but_never_panic() {
        // 1. Валидная ATOM-строка.
        let t = parse_atom(Some("2026-08-07T12:31:00+03:00")).unwrap();
        assert_eq!(t.format("%d.%m %H:%M").to_string(), "07.08 12:31");

        // 2. Ненулевое смещение СОХРАНЯЕТСЯ, а не отбрасывается: полдень в
        //    Москве и полдень в UTC — это разные моменты.
        let msk = parse_atom(Some("2026-08-07T12:00:00+03:00")).unwrap();
        let utc = parse_atom(Some("2026-08-07T12:00:00Z")).unwrap();
        assert_ne!(msk, utc);
        assert_eq!(msk.offset().local_minus_utc(), 3 * 3600);

        // 3. Даты нет вовсе.
        assert!(parse_atom(None).is_none());
        assert!(parse_atom(Some("   ")).is_none());
        assert_eq!(stamp(None), "??.?? ??:??");
        assert_eq!(day(None), "??.??");

        // 4. Мусор — включая правдоподобный, но неполный.
        for junk in [
            "вчера",
            "07.08.2026",
            "2026-08-07 12:31:00",
            "2026-13-45T99:99:99+03:00",
            "",
            "\u{0}",
        ] {
            assert!(parse_atom(Some(junk)).is_none(), "разобрал мусор: {junk:?}");
            assert_eq!(stamp(Some(junk)), "??.?? ??:??", "{junk:?}");
            assert_eq!(day(Some(junk)), "??.??", "{junk:?}");
        }
    }

    /// Формат метки совпадает с VK и Telegram: у трёх мессенджеров ОДНА
    /// системная инструкция, и транскрипт обязан читаться одинаково.
    ///
    /// Сравнение идёт с тем, что дал бы VK-шный `stamp` от эквивалентного
    /// момента: обе стороны приводятся к ЛОКАЛЬНОЙ зоне, поэтому тест не
    /// зависит от того, в каком поясе запущен прогон.
    #[test]
    fn the_stamp_format_matches_the_other_messengers() {
        let atom = "2026-08-07T12:31:00+03:00";
        let unix = chrono::DateTime::parse_from_rfc3339(atom).unwrap().timestamp();
        let vk_like = chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%d.%m %H:%M")
            .to_string();
        assert_eq!(stamp(Some(atom)), vk_like);

        let vk_day = chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%d.%m")
            .to_string();
        assert_eq!(day(Some(atom)), vk_day);
    }

    // -----------------------------------------------------------------------
    // Цитаты внутри BB-кодов (специфическая сложность №3).
    // -----------------------------------------------------------------------

    #[test]
    fn a_bracket_quote_is_split_from_the_reply() {
        let segments = crate::bbcode::to_segments("[QUOTE]Анна: когда смета?[/QUOTE]завтра к обеду");
        let split = split_quote(&segments).expect("цитата обязана найтись");
        assert_eq!(split.quote.author, "Анна");
        assert_eq!(split.quote.text, "когда смета?");
        assert_eq!(split.body.trim(), "завтра к обеду");
    }

    /// Наша собственная форма (`compose_reply`) — ведущие строки `> …`.
    ///
    /// Вход собирается НАСТОЯЩЕЙ функцией отправки, а не литералом: разъедься
    /// они — тест обязан покраснеть, а не проверять мою копию формата.
    #[test]
    fn our_own_angle_quote_is_split_too() {
        let raw = crate::routes::dialogs::compose_reply(
            Some("Игорь Герасимов"),
            Some("а смета готова?"),
            "готова, отправил",
        );
        let split = split_quote(&crate::bbcode::to_segments(&raw)).expect("цитата");
        assert_eq!(split.quote.author, "Игорь Герасимов");
        assert_eq!(split.quote.text, "а смета готова?");
        assert_eq!(split.body.trim(), "готова, отправил");
    }

    #[test]
    fn a_nested_quote_keeps_the_inner_text() {
        let segments =
            crate::bbcode::to_segments("[QUOTE]Анна: [B]срочно[/B] нужна смета[/QUOTE]сделаю");
        let split = split_quote(&segments).unwrap();
        assert_eq!(split.quote.author, "Анна");
        assert_eq!(split.quote.text, "срочно нужна смета");
        assert_eq!(split.body.trim(), "сделаю");
    }

    #[test]
    fn a_quote_without_an_author_keeps_the_whole_text() {
        let split =
            split_quote(&crate::bbcode::to_segments("[QUOTE]просто текст[/QUOTE]ответ")).unwrap();
        assert_eq!(split.quote.author, "");
        assert_eq!(split.quote.text, "просто текст");
    }

    /// Двоеточие посреди фразы — знак препинания, а не подпись.
    #[test]
    fn a_long_prefix_before_the_colon_is_not_an_author() {
        let long = "а".repeat(QUOTE_AUTHOR_MAX_CHARS + 1);
        let q = parse_quote_body(&format!("{long}: хвост")).unwrap();
        assert_eq!(q.author, "", "{}", q.author);
        assert!(q.text.starts_with(&long));
    }

    /// Кириллица не сдвигает потолок автора вдвое: позиция ищется в СИМВОЛАХ.
    #[test]
    fn the_author_ceiling_counts_characters_not_bytes() {
        let name = "я".repeat(QUOTE_AUTHOR_MAX_CHARS);
        let q = parse_quote_body(&format!("{name}: текст")).unwrap();
        assert_eq!(q.author, name, "64 кириллических символа — ещё имя");
        assert_eq!(q.text, "текст");
    }

    #[test]
    fn a_message_without_a_quote_is_not_split() {
        assert!(split_quote(&crate::bbcode::to_segments("обычный текст")).is_none());
        assert!(split_quote(&[]).is_none());
        // Сообщение ИЗ ОДНОЙ цитаты — не цитата, а реплика: прятать её текст в
        // подпись значило бы потерять содержимое.
        assert!(split_quote(&crate::bbcode::to_segments("[QUOTE]только это[/QUOTE]")).is_none());
        assert!(split_quote(&crate::bbcode::to_segments("> только это")).is_none());
    }

    /// Битый BB-код не паникует и не съедает сообщение.
    #[test]
    fn broken_bbcode_never_panics() {
        for raw in [
            "[QUOTE]без закрытия",
            "[/QUOTE]",
            "[QUOTE][QUOTE][QUOTE]глубоко[/QUOTE]",
            "[QUOTE=]=[/QUOTE]",
            ">",
            ">>>>",
            "[QUOTE]\u{0}[/QUOTE]хвост",
            &"[QUOTE]".repeat(64),
        ] {
            let segments = crate::bbcode::to_segments(raw);
            let _ = split_quote(&segments);
            let m = msg(2, false, "Игорь", raw, "2026-08-07T12:31:00+03:00");
            let _ = render_message(&m);
        }
    }

    /// ЦИТАТА НЕ ДУБЛИРУЕТСЯ: в строке она стоит один раз, а не ещё и внутри
    /// тела реплики.
    #[test]
    fn the_quoted_text_appears_exactly_once() {
        let m = msg(
            2,
            false,
            "Игорь",
            "[QUOTE]Анна: когда смета?[/QUOTE]завтра к обеду",
            "2026-08-07T12:31:00+03:00",
        );
        let line = render_message(&m).unwrap();
        assert_eq!(line.matches("когда смета?").count(), 1, "{line}");
        assert!(line.contains("↳ Анна: «когда смета?»"), "{line}");
        assert!(line.ends_with("завтра к обеду"), "{line}");
    }

    /// Ветки `fwd_label` и `reply_to` из VK здесь МЕРТВЫ и их нет.
    ///
    /// Тест сторожит именно это: `reply_to`/`fwd` у портала всегда `None`
    /// (Р10, §6), и попытка вернуть VK-ветку означала бы код, который не
    /// исполняется никогда.
    #[test]
    fn the_vk_reply_and_forward_fields_are_always_empty() {
        let m = msg(2, false, "Игорь", "текст", "2026-08-07T12:31:00+03:00");
        assert!(m.reply_to.is_none());
        assert!(m.fwd.is_none());
        assert!(
            !std::fs::read_to_string(file!())
                .unwrap_or_default()
                .contains("fwd_label"),
            "мёртвая VK-ветка вернулась в модуль"
        );
    }

    // -----------------------------------------------------------------------
    // Рендер.
    // -----------------------------------------------------------------------

    /// Исходящее подписывается «Я», входящее — именем автора: модель обязана
    /// понимать, чью сторону разговора она разбирает.
    #[test]
    fn the_speaker_side_is_explicit() {
        let mine = render_message(&msg(2, true, "", "готово", "2026-08-07T12:31:00+03:00")).unwrap();
        assert_eq!(mine, "[07.08 12:31] Я: готово");

        let theirs =
            render_message(&msg(4, false, "Анна", "спасибо", "2026-08-07T12:33:00+03:00")).unwrap();
        assert!(theirs.contains("] Анна: спасибо"), "{theirs}");

        // Автора нет вовсе — прочерк, а не пустое место перед двоеточием.
        let nobody = render_message(&msg(6, false, "", "хм", "2026-08-07T12:34:00+03:00")).unwrap();
        assert!(nobody.contains("] —: хм"), "{nobody}");
    }

    #[test]
    fn media_becomes_a_compact_label() {
        let mut m = msg(2, false, "Анна", "", "2026-08-07T12:31:00+03:00");
        m.media = vec![
            media(media_kind::IMAGE, Some("photo.jpg")),
            media(media_kind::FILE, Some("смета.xlsx")),
            media(media_kind::VOICE, None),
        ];
        let line = render_message(&m).unwrap();
        assert!(line.contains("[фото]"), "{line}");
        assert!(line.contains("[документ: смета.xlsx]"), "{line}");
        assert!(line.contains("[голосовое]"), "{line}");
    }

    /// Сообщение без текста и без медиа не занимает места в бюджете.
    #[test]
    fn an_empty_message_is_skipped() {
        assert!(render_message(&msg(2, false, "Анна", "", "2026-08-07T12:31:00+03:00")).is_none());
        assert!(render_message(&msg(4, false, "Анна", "   ", "")).is_none());
    }

    /// Системная строка — отдельная форма, без бабла и без автора.
    #[test]
    fn a_system_line_has_its_own_shape() {
        let mut m = msg(2, false, "", "Игорь добавил Анну", "2026-08-07T12:31:00+03:00");
        m.system = true;
        assert_eq!(
            render_message(&m).unwrap(),
            "[07.08 12:31] * событие: Игорь добавил Анну"
        );

        // Текста нет — подставляется код действия, а не пустота.
        let mut m = msg(4, false, "", "", "2026-08-07T12:32:00+03:00");
        m.system = true;
        m.action = Some("CHAT_JOIN".into());
        assert!(render_message(&m).unwrap().ends_with("* событие: CHAT_JOIN"));
    }

    /// Многострочный текст схлопывается в ОДНУ строку: транскрипт режется по
    /// границам сообщений, и вторая строка сломала бы это.
    #[test]
    fn a_message_is_always_exactly_one_line() {
        let m = msg(
            2,
            false,
            "Анна",
            "первая\nвторая\n\n   третья",
            "2026-08-07T12:31:00+03:00",
        );
        let line = render_message(&m).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.ends_with("первая вторая третья"), "{line}");
    }

    #[test]
    fn a_huge_message_is_truncated_by_code_points() {
        let long = "я".repeat(MAX_MESSAGE_CHARS + 500);
        let line = render_message(&msg(2, false, "Анна", &long, "")).unwrap();
        assert!(line.contains('…'));
        // Кириллица не роняет срез и не режется пополам.
        assert!(line.chars().count() < MAX_MESSAGE_CHARS + 100);
    }

    #[test]
    fn the_transcript_is_ordered_and_counted() {
        let msgs = vec![
            msg(6, false, "Анна", "третья", "2026-08-07T12:33:00+03:00"),
            msg(2, true, "", "первая", "2026-08-07T12:31:00+03:00"),
            msg(4, false, "Анна", "вторая", "2026-08-07T12:32:00+03:00"),
        ];
        let t = render(&msgs, MAX_TRANSCRIPT_CHARS);
        assert_eq!(t.messages_used, 3);
        assert!(!t.truncated);
        assert_eq!(t.oldest_id, Some(2));
        assert_eq!(t.newest_id, Some(6));

        let lines: Vec<&str> = t.text.lines().collect();
        assert!(lines[0].ends_with("первая"), "{:?}", lines[0]);
        assert!(lines[1].ends_with("вторая"));
        assert!(lines[2].ends_with("третья"));
    }

    /// Бюджет режет СТАРЫЕ сообщения и объявляет это вслух.
    #[test]
    fn the_budget_drops_the_oldest_and_says_how_many() {
        let msgs: Vec<MessageDto> = (0..100)
            .map(|i| msg(2 + i * 2, false, "Анна", &format!("реплика номер {i}"), ""))
            .collect();
        let t = render(&msgs, 400);

        assert!(t.truncated);
        assert!(t.text.chars().count() <= 400, "вылез за бюджет");
        assert!(t.text.starts_with("[…опущено "), "{}", t.text);
        // Уцелел ХВОСТ переписки, а не её начало.
        assert!(t.text.contains("реплика номер 99"));
        assert!(!t.text.contains("реплика номер 0 "));
        assert_eq!(t.newest_id, Some(2 + 99 * 2));
    }

    /// Бюджет меньше самого маркера — транскрипт всё равно не вылезает.
    #[test]
    fn an_absurd_budget_still_holds_the_line() {
        let msgs: Vec<MessageDto> = (0..10)
            .map(|i| msg(2 + i * 2, false, "Анна", "текст", ""))
            .collect();
        let t = render(&msgs, 5);
        assert!(t.truncated);
        assert!(t.text.chars().count() <= 5, "{:?}", t.text);
    }

    /// ИНВАРИАНТ: ни один URL не покидает бэкенд.
    ///
    /// DTO набивается адресами со всех сторон — прокси вложения, аватар
    /// автора, страница плеера. В транскрипте не должно остаться ни одного.
    #[test]
    fn rendered_text_never_contains_urls() {
        let mut m = msg(
            2,
            false,
            "Анна",
            "[QUOTE]Игорь: смотри[/QUOTE]вот файл",
            "2026-08-07T12:31:00+03:00",
        );
        m.sender = Some(SenderDto {
            id: "42".into(),
            name: Some("Анна".into()),
            avatar_url: Some("/api/bitrix/media/avatar/42".into()),
            ..SenderDto::default()
        });
        m.media = vec![MediaDto {
            index: 0,
            file_id: 7,
            kind: media_kind::VIDEO.to_string(),
            name: Some("отчёт.mp4".into()),
            player_url: Some("https://portal.example/disk/player/7".into()),
            has_full: true,
            ..MediaDto::default()
        }];

        let t = render(&[m], MAX_TRANSCRIPT_CHARS);
        for needle in [
            "http://",
            "https://",
            "/api/bitrix/",
            "portal.example",
            "avatar",
            "player",
        ] {
            assert!(
                !t.text.contains(needle),
                "адрес просочился в промпт ({needle}): {}",
                t.text
            );
        }
    }

    /// Идентификатор портала шире `i32` — курсор REFRESH обязан насытиться, а
    /// не завернуться в отрицательное число («выгрузи всё с начала»).
    #[test]
    fn oversized_ids_saturate_instead_of_wrapping() {
        let big = i64::from(i32::MAX) + 1_000;
        let t = render(&[msg(big, false, "Анна", "текст", "")], MAX_TRANSCRIPT_CHARS);
        assert_eq!(t.newest_id, Some(i32::MAX));
        assert_eq!(t.oldest_id, Some(i32::MAX));
    }

    /// Сквозной прогон на реалистичной пачке: даты ATOM, цитаты в BB-кодах,
    /// медиа, системная строка, исходящее и входящее — всё в одном транскрипте.
    #[test]
    fn a_realistic_batch_renders_as_a_conversation() {
        let mut system = msg(2, false, "", "Игорь создал чат", "2026-08-07T09:00:00+03:00");
        system.system = true;
        let mut with_file = msg(8, false, "Анна", "держи", "2026-08-07T12:40:00+03:00");
        with_file.media = vec![media(media_kind::FILE, Some("смета.xlsx"))];

        let msgs = vec![
            system,
            msg(4, true, "", "привет, нужна смета", "2026-08-07T12:31:00+03:00"),
            msg(
                6,
                false,
                "Анна",
                "[QUOTE]Я: привет, нужна смета[/QUOTE]сейчас пришлю",
                "2026-08-07T12:33:00+03:00",
            ),
            with_file,
        ];

        let t = render(&msgs, MAX_TRANSCRIPT_CHARS);
        assert_eq!(t.messages_used, 4);
        let lines: Vec<&str> = t.text.lines().collect();
        assert_eq!(lines[0], "[07.08 09:00] * событие: Игорь создал чат");
        assert_eq!(lines[1], "[07.08 12:31] Я: привет, нужна смета");
        assert_eq!(
            lines[2],
            "[07.08 12:33] Анна: ↳ Я: «привет, нужна смета» | сейчас пришлю"
        );
        assert_eq!(lines[3], "[07.08 12:40] Анна: держи | [документ: смета.xlsx]");
    }
}
