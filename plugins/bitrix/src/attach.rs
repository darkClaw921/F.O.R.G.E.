//! Вложения сообщения: файлы, карточки `ATTACH` и клавиатуры `KEYBOARD`.
//!
//! # Три независимых сущности в одном поле
//!
//! Всё, что не текст, портал складывает в `params` сообщения — и это три
//! РАЗНЫЕ вещи с разной судьбой:
//!
//! | ключ `params` | что это | где живёт полезная нагрузка | DTO |
//! |---|---|---|---|
//! | `FILE_ID: [95]` | приложенные файлы | корневой `files[]` ОТВЕТА | [`MediaDto`] |
//! | `ATTACH: [{BLOCKS:[…]}]` | карточка бота/робота | здесь же | [`CardDto`] |
//! | `KEYBOARD: [{…}]` | inline-кнопки | здесь же | [`ReplyMarkupDto`] |
//!
//! # Ловушка №1: файл лежит НЕ в сообщении
//!
//! Сообщение несёт только идентификаторы (`params.FILE_ID: [95]`), а сами
//! дескрипторы — в корне ответа `im.dialog.messages.get` (`files[]`), общим
//! списком на всю страницу. Связка делается по идентификатору
//! ([`index_files`]), а ПОРЯДКОВЫЙ НОМЕР в `FILE_ID` становится
//! [`MediaDto::index`].
//!
//! Индекс важен не для красоты: он входит в ключ кэша и в путь прокси
//! (`/api/bitrix/media/file/:dialog/:mid/:index`, Ф8). Без него два файла
//! одного сообщения имели бы один ключ, и второй затирал бы первый.
//!
//! Файл, на который сослались, но дескриптора которого в ответе нет,
//! **не выбрасывается**: карточка рисуется заглушкой с одним `file_id`.
//! Скачивание в Ф8 адресуется именно им, а молча потерянное вложение
//! выглядело бы как «собеседник ничего не присылал».
//!
//! # Ловушка №2: внешних адресов файла в DTO нет НИКОГДА
//!
//! `urlDownload`, `urlShow` и `viewerAttrs.unifiedLink` портал присылает, но
//! **они не работают без сессионной куки** (живая проверка: первые два отдают
//! HTML страницы логина, третий — 403). Единственный рабочий путь —
//! `im.v2.File.download` → `downloadUrl`, а тот содержит код вебхука и наружу
//! не отдаётся никогда. Поэтому этих полей нет ни в [`MediaDto`], ни в
//! [`crate::api::schema::FileRaw`] — не объявить их проще, чем не отдать.
//! Инвариант проверяется тестом-стражем.
//!
//! # Ловушка №3: адрес в карточке — это ЧУЖОЙ ТЕКСТ
//!
//! `BLOCKS[].LINK[].LINK` пишет отправитель, а `BG_COLOR` кнопки — бот. И то,
//! и другое попадает в DOM, поэтому фильтруется ЗДЕСЬ: адрес пропускается
//! только со схемой `http`/`https` ([`http_url`]), цвет — только шестнадцатеричный
//! ([`hex_color`]). Фронт проверяет повторно (рубежа должно быть два), но
//! первый рубеж — этот.
//!
//! # Что платформа не даёт
//!
//! `waveform` (огибающая голосового) и `player_url` (страница-плеер) в
//! [`MediaDto`] объявлены и ВСЕГДА пусты: у Битрикс24 нет ни того, ни другого.
//! Поля сохранены ради ИДЕНТИЧНОСТИ ФОРМЫ с панелями VK и Telegram — их фронты
//! рисуют карточку одним и тем же кодом, и каждое удалённое поле превратилось
//! бы в `if`.
//!
//! Кнопки `CALL` и `DIALOG` требуют scope `imbot`, которого у входящего
//! вебхука нет и быть не может. Они приезжают ВЫКЛЮЧЕННЫМИ и с объяснением в
//! [`ButtonDto::disabled_reason`] — честная деградация вместо кнопки, которая
//! молча ничего не делает.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::schema::FileRaw;
use crate::bbcode::{self, Segment};

/// Ключ `params`: идентификаторы приложенных файлов.
pub const P_FILE_ID: &str = "FILE_ID";

/// Ключ `params`: карточки бота.
pub const P_ATTACH: &str = "ATTACH";

/// Ключ `params`: inline-клавиатура.
pub const P_KEYBOARD: &str = "KEYBOARD";

/// Ключ `params`: сообщение отредактировано.
pub const P_IS_EDITED: &str = "IS_EDITED";

/// Ключ `params`: список поставивших «лайк».
pub const P_LIKE: &str = "LIKE";

/// Ключ `params`: код системного действия ленты (`CHAT_JOIN`, `CHAT_LEAVE`…).
pub const P_CODE: &str = "CODE";

/// Ключ `params`: сообщение отправлено как системное (`im.message.add
/// SYSTEM=Y`).
pub const P_SYSTEM: &str = "SYSTEM";

/// Ключ `params`: сообщение УДАЛЕНО.
///
/// # Ловушка: удаление у портала не стирает строку
///
/// `im.message.delete` не убирает сообщение из истории, а ЗАМЕНЯЕТ его текст на
/// «Это сообщение было удалено» и ставит `IS_DELETED: "Y"`. Страница истории
/// продолжает его отдавать — поэтому фронту нужен явный признак: иначе строка,
/// убранная по кадру `message_delete`, ВЕРНУЛАСЬ БЫ при следующей загрузке
/// ленты, да ещё и с текстом-заглушкой портала вместо честной пометки.
pub const P_IS_DELETED: &str = "IS_DELETED";

/// Сколько вложений разбирается у одного сообщения.
///
/// Портал не ограничивает `FILE_ID` явно, а карточка на сотню файлов — это
/// заведомо не то, что можно показать в ленте.
pub const MAX_MEDIA: usize = 20;

/// Сколько карточек `ATTACH` разбирается у одного сообщения.
pub const MAX_CARDS: usize = 10;

/// Сколько блоков разбирается в одной карточке.
pub const MAX_BLOCKS: usize = 24;

/// Сколько строк разбирается в одном блоке `GRID`/`LINK`.
pub const MAX_ROWS: usize = 40;

/// Сколько кнопок разбирается в одной клавиатуре.
///
/// Ровно столько же (`MAX_ROWS × MAX_ROW_BUTTONS`) готов нарисовать фронт;
/// клавиатура крупнее — это либо ошибка бота, либо попытка распереть панель.
pub const MAX_BUTTONS: usize = 64;

/// Длина подписи кнопки и названия поля в code point'ах.
pub const MAX_LABEL_CHARS: usize = 120;

/// Длина текстового блока карточки в code point'ах.
pub const MAX_CARD_TEXT_CHARS: usize = 2000;

/// Виды вложения — закрытый словарь, дословно совпадающий с фронтом.
pub mod media_kind {
    /// Картинка (`type: "image"`, есть размеры).
    pub const IMAGE: &str = "image";
    /// Видео.
    pub const VIDEO: &str = "video";
    /// Аудиозапись.
    pub const AUDIO: &str = "audio";
    /// Голосовое сообщение (`isVoiceNote`).
    pub const VOICE: &str = "voice";
    /// Кружок-видеосообщение (`isVideoNote`).
    pub const VIDEO_NOTE: &str = "video_note";
    /// Всё остальное, включая незнакомые типы портала.
    pub const FILE: &str = "file";
}

/// Виды блока карточки `ATTACH` — закрытый словарь.
pub mod block_kind {
    /// Текстовый блок (`{"MESSAGE": "…"}`), в тексте живут BB-коды.
    pub const MESSAGE: &str = "message";
    /// Ссылки (`{"LINK": [{NAME, LINK, DESC}]}`).
    pub const LINK: &str = "link";
    /// Таблица «поле: значение» (`{"GRID": [{NAME, VALUE, DISPLAY}]}`).
    pub const GRID: &str = "grid";
}

/// Действия кнопки — закрытый словарь.
pub mod button_action {
    /// Вставить текст в поле ввода (работает клиентски).
    pub const PUT: &str = "put";
    /// Отправить текст сообщением (работает клиентски).
    pub const SEND: &str = "send";
    /// Позвонить — требует scope `imbot`.
    pub const CALL: &str = "call";
    /// Открыть диалог — требует scope `imbot`.
    pub const DIALOG: &str = "dialog";
    /// Открыть ссылку (поле `LINK`) — работает клиентски, `window.open`.
    ///
    /// Живой пример: системный бот «Команда заботы Битрикс24» шлёт кнопку
    /// «Получить консультацию» именно так, а не через `ACTION`.
    pub const LINK: &str = "link";
    /// Выполнить команду бота (`COMMAND`+`COMMAND_PARAMS`) — `im.message.command`.
    ///
    /// Требует ТОЛЬКО scope `im` (уже есть у входящего вебхука), в отличие от
    /// `CALL`/`DIALOG`: живая проверка портала подтвердила успешный вызов без
    /// `imbot`. Живой пример: кнопки-цифры оценки в чате «Команда заботы
    /// Битрикс24» (`COMMAND: "careButton"`).
    pub const COMMAND: &str = "command";
    /// Незнакомое действие портала.
    pub const OTHER: &str = "other";
}

/// Почему кнопка выключена: у входящего вебхука нет scope `imbot`.
pub const IMBOT_REQUIRED: &str =
    "Кнопка требует прав чат-бота (scope imbot), которых у входящего вебхука нет";

/// Почему кнопка выключена: так сказал сам портал (`DISABLED: "Y"`).
pub const DISABLED_BY_PORTAL: &str = "Кнопка выключена отправителем";

/// Почему кнопка выключена: у `COMMAND` нет `BOT_ID` — `im.message.command`
/// без него не выполнить (портал отвечает `BOT_ID_ERROR`).
pub const COMMAND_NEEDS_BOT_ID: &str = "У кнопки нет идентификатора бота";

/// Приложенный файл в проводной форме.
///
/// Ни одного адреса портала: скачивание идёт через прокси плагина, который
/// адресуется тройкой диалог/сообщение/[`MediaDto::index`] (Ф8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDto {
    /// Порядковый номер вложения В СООБЩЕНИИ (позиция в `params.FILE_ID`).
    ///
    /// Входит в ключ кэша и в путь прокси: без него два файла одного
    /// сообщения делили бы один ключ.
    pub index: u32,
    /// Идентификатор файла на Диске портала — им адресуется
    /// `im.v2.File.download` (Ф8).
    pub file_id: i64,
    /// Вид (см. [`media_kind`]). Незнакомый тип портала — это `file`, а не
    /// ошибка.
    pub kind: String,
    /// Имя файла, как его прислал портал.
    pub name: Option<String>,
    /// Расширение без точки.
    pub extension: Option<String>,
    /// Размер в байтах.
    pub size: Option<i64>,
    /// Ширина картинки, пиксели.
    pub width: Option<i64>,
    /// Высота картинки, пиксели.
    pub height: Option<i64>,
    /// Есть ли превью, которое можно показать до полной загрузки.
    ///
    /// Считается по НЕПУСТОМУ `mediaUrl.preview["250"]`: ключ портал
    /// присылает всегда, а значение у документа пустое.
    pub has_thumb: bool,
    /// Есть ли что скачивать. `false` бывает у вложения, дескриптор которого
    /// в ответе не приехал.
    pub has_full: bool,
    /// ВСЕГДА `None`: страницы-плеера у портала нет. Поле — ради формы.
    pub player_url: Option<String>,
    /// ВСЕГДА пусто: огибающей голосового портал не отдаёт. Поле — ради формы.
    pub waveform: Vec<i32>,
}

/// Ссылка внутри блока `LINK` карточки.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardLinkDto {
    /// Подпись ссылки.
    pub name: Option<String>,
    /// Адрес — ТОЛЬКО `http`/`https` ([`http_url`]); всё остальное `None`.
    pub url: Option<String>,
    /// Описание под ссылкой.
    pub desc: Option<String>,
}

/// Строка блока `GRID` карточки.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardGridRowDto {
    /// Название поля.
    pub name: Option<String>,
    /// Значение поля.
    pub value: Option<String>,
    /// Раскладка портала: `line` (в строку), `column` (в столбец), `block`.
    pub display: String,
    /// Цвет значения (шестнадцатеричный либо `None`).
    pub color: Option<String>,
}

/// Блок карточки `ATTACH`.
///
/// Один блок — ровно один вид ([`block_kind`]); незнакомые виды до DTO не
/// доезжают вовсе.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardBlockDto {
    /// Вид блока.
    pub kind: String,
    /// Текст блока `MESSAGE` после разбора BB-кодов.
    pub text: Option<String>,
    /// Структурные сегменты того же текста — фронт рисует их узлами.
    pub segments: Vec<Segment>,
    /// Ссылки блока `LINK`.
    pub links: Vec<CardLinkDto>,
    /// Строки блока `GRID`.
    pub rows: Vec<CardGridRowDto>,
}

/// Карточка `params.ATTACH[]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDto {
    /// Идентификатор карточки у портала (строкой: приходит и числом, и
    /// строкой).
    pub id: Option<String>,
    /// Цвет полосы карточки (шестнадцатеричный либо `None`).
    pub color: Option<String>,
    /// Описание карточки.
    pub description: Option<String>,
    /// Блоки в порядке показа.
    pub blocks: Vec<CardBlockDto>,
}

/// Кнопка inline-клавиатуры.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonDto {
    /// Подпись.
    pub text: String,
    /// Действие (см. [`button_action`]).
    pub action: String,
    /// Полезная нагрузка действия (`ACTION_VALUE`/`LINK`): текст для
    /// `PUT`/`SEND`, номер для `CALL`, `DIALOG_ID` для `DIALOG`, проверенный
    /// URL (только `http`/`https`, см. [`http_url`]) для `LINK`.
    pub value: Option<String>,
    /// Можно ли нажать. `false` — либо портал прислал `DISABLED: "Y"`, либо
    /// действие требует прав, которых у вебхука нет.
    pub enabled: bool,
    /// Почему нажать нельзя — уезжает в `title` кнопки. `None`, когда можно.
    pub disabled_reason: Option<String>,
    /// Цвет фона (шестнадцатеричный либо `None`).
    pub bg_color: Option<String>,
    /// Цвет текста (шестнадцатеричный либо `None`).
    pub text_color: Option<String>,
    /// `COMMAND` — только у [`button_action::COMMAND`].
    pub command: Option<String>,
    /// `COMMAND_PARAMS` — только у [`button_action::COMMAND`].
    pub command_params: Option<String>,
    /// `BOT_ID` кнопки — только у [`button_action::COMMAND`]
    /// (`im.message.command` требует его отдельно от `MESSAGE_ID`).
    pub bot_id: Option<i64>,
    /// `BLOCK: "Y"` — портал просит заблокировать кнопку НАВСЕГДА после
    /// первого успешного нажатия (`careButton`-кнопки оценки, например).
    /// Отличие от [`ButtonDto::enabled`]: то состояние кнопки ДО клика, это —
    /// инструкция фронту на ПОСЛЕ.
    pub lock_after_press: bool,
}

/// Клавиатура сообщения.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMarkupDto {
    /// Вид: у портала бывает только `inline`. Поле — ради формы с VK, где
    /// клавиатура бывает ещё и `keyboard` (полоса над полем ввода).
    pub kind: String,
    /// Ряды кнопок.
    pub rows: Vec<Vec<ButtonDto>>,
}

/// Единственный вид клавиатуры портала.
pub const MARKUP_INLINE: &str = "inline";

// ---------------------------------------------------------------------------
// Санитайзеры чужого текста.
// ---------------------------------------------------------------------------

/// Обрезанная непустая строка либо `None`.
fn text_of(value: Option<&Value>, max: usize) -> Option<String> {
    let raw = match value? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(crate::dto::truncate_chars(trimmed, max))
}

/// Адрес, годный для показа: только `http` и `https`.
///
/// Отсекает `javascript:`, `data:`, `file:` и прочее, что превращает клик по
/// карточке в исполнение чужого кода. Проверяется РЕГИСТРОНЕЗАВИСИМО: браузер
/// одинаково поймёт `JavaScript:` и `javascript:`.
pub fn http_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.is_empty() || url.len() > 2048 {
        return None;
    }
    // Управляющие символы внутри адреса — способ спрятать схему от глазной
    // проверки (`java\nscript:`); адресов с ними не бывает.
    if url.chars().any(|c| c.is_control()) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://")).then(|| url.to_string())
}

/// Цвет, годный для `style`: только `#rgb` и `#rrggbb`.
///
/// Строка уезжает в атрибут стиля, и всё, что не является шестнадцатеричным
/// цветом, там быть не должно: значение пришло от бота.
pub fn hex_color(raw: &str) -> Option<String> {
    let c = raw.trim();
    let hex = c.strip_prefix('#')?;
    let ok = (hex.len() == 3 || hex.len() == 6) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| format!("#{}", hex.to_ascii_lowercase()))
}

/// Значение `params` по ключу; `[]` (пустой PHP-массив) даёт `None`.
pub fn param<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params.as_object()?.get(key)
}

/// Флаг портала: `"Y"` — да, всё остальное — нет.
pub fn is_yes(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(s)) if s.eq_ignore_ascii_case("Y"))
}

/// Массив значений поля `params`, каким бы ни была его форма.
///
/// Портал одинаково легко присылает и массив (`LIKE: ["1"]`), и одиночный
/// объект (`ATTACH: {…}` — так выглядит только что отправленная карточка).
fn array_of(value: Option<&Value>) -> Vec<&Value> {
    match value {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(v @ Value::Object(_)) => vec![v],
        _ => Vec::new(),
    }
}

/// Идентификатор из значения, которое портал шлёт то числом, то строкой.
fn id_of(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Файлы.
// ---------------------------------------------------------------------------

/// Индексирует корневой `files[]` ответа по идентификатору файла.
///
/// Строится ОДИН раз на страницу истории, а не на каждое сообщение: страница
/// из пятидесяти сообщений с десятком файлов иначе давала бы полсотни
/// линейных проходов.
pub fn index_files(files: &[FileRaw]) -> HashMap<i64, &FileRaw> {
    files.iter().filter(|f| f.id > 0).map(|f| (f.id, f)).collect()
}

/// Вид вложения по дескриптору портала.
///
/// Порядок проверок значим: голосовое и «кружок» — это ФЛАГИ, а не `type`
/// (у голосового `type` остаётся `audio`), поэтому они проверяются первыми.
pub fn file_kind(file: &FileRaw) -> &'static str {
    if file.is_voice_note {
        return media_kind::VOICE;
    }
    if file.is_video_note {
        return media_kind::VIDEO_NOTE;
    }
    match file.kind.as_deref().unwrap_or_default().to_ascii_lowercase().as_str() {
        "image" => media_kind::IMAGE,
        "video" => media_kind::VIDEO,
        "audio" => media_kind::AUDIO,
        _ => media_kind::FILE,
    }
}

/// Проекция дескриптора файла в [`MediaDto`].
pub fn media_from_file(index: u32, file: &FileRaw) -> MediaDto {
    let (height, width) = file.image.size().unzip();
    MediaDto {
        index,
        file_id: file.id,
        kind: file_kind(file).to_string(),
        name: file
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| crate::dto::truncate_chars(s, MAX_LABEL_CHARS)),
        extension: file
            .extension
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 16)
            .map(str::to_ascii_lowercase),
        size: (file.size > 0).then_some(file.size),
        width,
        height,
        has_thumb: file.media_url.has_preview(),
        has_full: file.id > 0,
        player_url: None,
        waveform: Vec::new(),
    }
}

/// Вложения сообщения: `params.FILE_ID` × корневой `files[]`.
///
/// Идентификатор без дескриптора превращается в заглушку (`has_full: false`,
/// имени нет): скачивание в Ф8 всё равно адресуется `file_id`, а молча
/// потерянное вложение выглядело бы как «ничего не присылали».
pub fn media_from_params(params: &Value, files: &HashMap<i64, &FileRaw>) -> Vec<MediaDto> {
    // `FILE_ID` — массив скаляров (`[95]`), а не объектов; одиночное значение
    // портал тоже присылает (так выглядит только что отправленный файл).
    let ids: Vec<i64> = match param(params, P_FILE_ID) {
        Some(Value::Array(a)) => a.iter().filter_map(id_of).collect(),
        Some(v) => id_of(v).into_iter().collect(),
        None => Vec::new(),
    };

    ids.into_iter()
        .filter(|id| *id > 0)
        .take(MAX_MEDIA)
        .enumerate()
        .map(|(i, id)| {
            let index = i as u32;
            match files.get(&id) {
                Some(file) => media_from_file(index, file),
                None => MediaDto {
                    index,
                    file_id: id,
                    kind: media_kind::FILE.to_string(),
                    has_full: false,
                    ..MediaDto::default()
                },
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Карточки ATTACH.
// ---------------------------------------------------------------------------

/// Карточки `params.ATTACH`.
///
/// Незнакомый вид блока пропускается МОЛЧА: портал добавляет виды между
/// релизами, и падать (или показывать «неизвестный блок») из-за нового вида
/// значило бы ломать ленту на ровном месте.
pub fn cards_from_params(params: &Value) -> Vec<CardDto> {
    array_of(param(params, P_ATTACH))
        .into_iter()
        .take(MAX_CARDS)
        .filter_map(card_from_value)
        .collect()
}

/// Одна карточка. `None`, если в ней не осталось ни одного знакомого блока.
fn card_from_value(raw: &Value) -> Option<CardDto> {
    let obj = raw.as_object()?;
    let blocks: Vec<CardBlockDto> = match obj.get("BLOCKS") {
        Some(Value::Array(a)) => a.iter().take(MAX_BLOCKS).filter_map(block_from_value).collect(),
        // Карточка без обёртки `BLOCKS` — так выглядит то, что отправитель
        // передал в `im.message.add` до нормализации порталом.
        _ => block_from_value(raw).into_iter().collect(),
    };
    let description = text_of(obj.get("DESCRIPTION"), MAX_CARD_TEXT_CHARS);
    if blocks.is_empty() && description.is_none() {
        return None;
    }
    Some(CardDto {
        id: obj.get("ID").and_then(|v| match v {
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
            _ => None,
        }),
        color: obj
            .get("COLOR")
            .and_then(Value::as_str)
            .and_then(hex_color),
        description,
        blocks,
    })
}

/// Один блок карточки. `None` — вид незнаком либо блок пуст.
fn block_from_value(raw: &Value) -> Option<CardBlockDto> {
    let obj = raw.as_object()?;

    if let Some(text) = text_of(obj.get("MESSAGE"), MAX_CARD_TEXT_CHARS) {
        // В тексте блока живут те же BB-коды, что и в тексте сообщения.
        return Some(CardBlockDto {
            kind: block_kind::MESSAGE.to_string(),
            text: Some(bbcode::to_plain(&text)),
            segments: bbcode::to_segments(&text),
            ..CardBlockDto::default()
        });
    }

    if obj.contains_key("LINK") {
        let links: Vec<CardLinkDto> = array_of(obj.get("LINK"))
            .into_iter()
            .take(MAX_ROWS)
            .filter_map(|v| {
                let o = v.as_object()?;
                let name = text_of(o.get("NAME"), MAX_LABEL_CHARS);
                let url = o.get("LINK").and_then(Value::as_str).and_then(http_url);
                let desc = text_of(o.get("DESC"), MAX_CARD_TEXT_CHARS);
                (name.is_some() || url.is_some() || desc.is_some()).then_some(CardLinkDto {
                    name,
                    url,
                    desc,
                })
            })
            .collect();
        return (!links.is_empty()).then(|| CardBlockDto {
            kind: block_kind::LINK.to_string(),
            links,
            ..CardBlockDto::default()
        });
    }

    if obj.contains_key("GRID") {
        let rows: Vec<CardGridRowDto> = array_of(obj.get("GRID"))
            .into_iter()
            .take(MAX_ROWS)
            .filter_map(|v| {
                let o = v.as_object()?;
                let name = text_of(o.get("NAME"), MAX_LABEL_CHARS);
                let value = text_of(o.get("VALUE"), MAX_CARD_TEXT_CHARS);
                (name.is_some() || value.is_some()).then_some(CardGridRowDto {
                    name,
                    value,
                    display: o
                        .get("DISPLAY")
                        .and_then(Value::as_str)
                        .map(str::to_ascii_lowercase)
                        .filter(|d| ["line", "column", "block"].contains(&d.as_str()))
                        .unwrap_or_else(|| "line".to_string()),
                    color: o.get("COLOR").and_then(Value::as_str).and_then(hex_color),
                })
            })
            .collect();
        return (!rows.is_empty()).then(|| CardBlockDto {
            kind: block_kind::GRID.to_string(),
            rows,
            ..CardBlockDto::default()
        });
    }

    None
}

// ---------------------------------------------------------------------------
// Клавиатуры KEYBOARD.
// ---------------------------------------------------------------------------

/// Клавиатура `params.KEYBOARD`.
///
/// # Как плоский список превращается в ряды
///
/// Портал отдаёт кнопки ОДНИМ массивом, а раскладку прячет в двух местах:
/// служебный элемент `{"TYPE":"NEWLINE"}` начинает новый ряд, а `DISPLAY:
/// "BLOCK"` означает «эта кнопка занимает ряд целиком». Всё остальное
/// (`DISPLAY: "LINE"`) идёт в текущий ряд.
pub fn keyboard_from_params(params: &Value) -> Option<ReplyMarkupDto> {
    let raw = array_of(param(params, P_KEYBOARD));
    if raw.is_empty() {
        return None;
    }

    let mut rows: Vec<Vec<ButtonDto>> = Vec::new();
    let mut current: Vec<ButtonDto> = Vec::new();
    let mut taken = 0usize;

    for item in raw {
        let Some(obj) = item.as_object() else { continue };
        let kind = obj
            .get("TYPE")
            .and_then(Value::as_str)
            .unwrap_or("BUTTON")
            .to_ascii_uppercase();
        if kind == "NEWLINE" {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            continue;
        }
        if taken >= MAX_BUTTONS {
            break;
        }
        let Some(button) = button_from_value(item) else { continue };
        taken += 1;

        let block = obj
            .get("DISPLAY")
            .and_then(Value::as_str)
            .is_some_and(|d| d.eq_ignore_ascii_case("BLOCK"));
        if block {
            if !current.is_empty() {
                rows.push(std::mem::take(&mut current));
            }
            rows.push(vec![button]);
            continue;
        }
        current.push(button);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    (!rows.is_empty()).then(|| ReplyMarkupDto {
        kind: MARKUP_INLINE.to_string(),
        rows,
    })
}

/// Одна кнопка. `None`, если у неё нет подписи — нажимать нечего.
///
/// # Три ветки, и в такой очерёдности
///
/// Поле действия у кнопки ровно ОДНО, но угадать заранее, какое именно
/// заполнено, нельзя — портал не присылает служебного признака вида. Живые
/// пробы (чат «Команда заботы Битрикс24») показали оба новых вида в одной
/// клавиатуре разных сообщений: `LINK` («Получить консультацию») и
/// `COMMAND`+`COMMAND_PARAMS` (кнопки оценки 1…10). `ACTION`/`ACTION_VALUE` —
/// это ТРЕТЬЯ, отдельная форма (см. официальный пример со `PUT`), и три ветки
/// проверяются по наличию поля, а не по видовому маркеру, которого у портала
/// нет. Порядок проверки (`LINK` → `COMMAND` → `ACTION`) не имеет значения:
/// поля взаимоисключающие по контракту портала, но `LINK` проверяется первым,
/// потому что он самый дешёвый (не требует сети вовсе).
fn button_from_value(raw: &Value) -> Option<ButtonDto> {
    let obj = raw.as_object()?;
    let text = text_of(obj.get("TEXT"), MAX_LABEL_CHARS)?;

    let link = obj.get("LINK").and_then(Value::as_str).and_then(http_url);
    let command = obj
        .get("COMMAND")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut value = None;
    let mut command_field = None;
    let mut command_params = None;
    let mut bot_id = None;

    let action = if let Some(url) = link {
        value = Some(url);
        button_action::LINK
    } else if let Some(cmd) = command {
        command_field = Some(cmd.to_string());
        command_params = text_of(obj.get("COMMAND_PARAMS"), crate::dto::MAX_MESSAGE_CHARS);
        bot_id = obj.get("BOT_ID").and_then(Value::as_i64);
        button_action::COMMAND
    } else {
        let mapped = match obj
            .get("ACTION")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str()
        {
            "PUT" => button_action::PUT,
            "SEND" => button_action::SEND,
            "CALL" => button_action::CALL,
            "DIALOG" => button_action::DIALOG,
            _ => button_action::OTHER,
        };
        if mapped != button_action::OTHER {
            value = text_of(obj.get("ACTION_VALUE"), crate::dto::MAX_MESSAGE_CHARS);
        }
        mapped
    };

    // Причины отказа РАЗНЫЕ, и пользователю важно их различать: «выключил
    // отправитель» лечится не тем же, что «прав не хватает у вебхука» или
    // «портал не досказал, какого бота звать».
    let disabled_by_portal = is_yes(obj.get("DISABLED"));
    let needs_imbot = matches!(action, "call" | "dialog");
    let missing_bot_id = action == button_action::COMMAND && bot_id.is_none();
    let unknown = action == button_action::OTHER;
    let disabled_reason = if disabled_by_portal {
        Some(DISABLED_BY_PORTAL.to_string())
    } else if needs_imbot {
        Some(IMBOT_REQUIRED.to_string())
    } else if missing_bot_id {
        Some(COMMAND_NEEDS_BOT_ID.to_string())
    } else if unknown {
        Some("Портал прислал незнакомое действие кнопки".to_string())
    } else {
        None
    };

    Some(ButtonDto {
        text,
        action: action.to_string(),
        value,
        enabled: disabled_reason.is_none(),
        disabled_reason,
        bg_color: obj.get("BG_COLOR").and_then(Value::as_str).and_then(hex_color),
        text_color: obj
            .get("TEXT_COLOR")
            .and_then(Value::as_str)
            .and_then(hex_color),
        command: command_field,
        command_params,
        bot_id,
        lock_after_press: is_yes(obj.get("BLOCK")),
    })
}

// ---------------------------------------------------------------------------
// Прочие поля `params`.
// ---------------------------------------------------------------------------

/// Кто поставил «лайк» — идентификаторы СТРОКАМИ.
///
/// # Ловушка: форма плавает
///
/// Разведка сняла `{"LIKE": ["1"]}` (строки), живая проверка сразу после
/// `im.message.like` — `{"LIKE": [1]}` (числа). Это одно и то же поле одного и
/// того же портала, поэтому разбираются ОБЕ формы, а наружу уходит одна —
/// строковая, как все идентификаторы плагина.
pub fn likes_from_params(params: &Value) -> Vec<String> {
    match param(params, P_LIKE) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(id_of)
            .filter(|id| *id > 0)
            .map(|id| id.to_string())
            .collect(),
        Some(v) => id_of(v)
            .filter(|id| *id > 0)
            .map(|id| vec![id.to_string()])
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Код системного действия ленты (`params.CODE[0]`): `CHAT_JOIN`,
/// `CHAT_LEAVE`, …
pub fn action_from_params(params: &Value) -> Option<String> {
    match param(params, P_CODE) {
        Some(Value::Array(a)) => a.first().and_then(Value::as_str),
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
    .map(str::trim)
    .filter(|s| !s.is_empty() && s.len() <= 64)
    .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::MessagesRaw;

    /// Живой ответ `im.dialog.messages.get` тестового портала (chat27).
    const MSGS: &str = include_str!("../tests/fixtures/msgs_chat27.json");

    fn page() -> MessagesRaw {
        let v: Value = serde_json::from_str(MSGS).unwrap();
        serde_json::from_value(v["result"].clone()).expect("живая страница не разобралась")
    }

    fn params_of(id: i64) -> Value {
        page()
            .messages
            .into_iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("нет сообщения {id}"))
            .params
    }

    /// ФАЙЛ ЛЕЖИТ НЕ В СООБЩЕНИИ: `params.FILE_ID: [95]` × корневой `files[]`.
    #[test]
    fn a_file_is_linked_from_the_root_array_by_its_id() {
        let page = page();
        let files = page.files.clone().into_values();
        let index = index_files(&files);
        assert_eq!(index.len(), 1, "живой ответ несёт ровно один файл");

        let media = media_from_params(&params_of(97), &index);
        assert_eq!(media.len(), 1);
        let m = &media[0];
        assert_eq!(m.index, 0, "индекс — позиция в FILE_ID, а не id файла");
        assert_eq!(m.file_id, 95);
        assert_eq!(m.kind, media_kind::FILE);
        assert_eq!(m.name.as_deref(), Some("forge_probe.txt"));
        assert_eq!(m.extension.as_deref(), Some("txt"));
        assert_eq!(m.size, Some(500));
        assert!(!m.has_thumb, "у документа mediaUrl.preview[250] пуст");
        assert!(m.has_full);
        // Платформа не даёт ни того, ни другого — поля есть ради формы.
        assert_eq!(m.player_url, None);
        assert!(m.waveform.is_empty());

        // Сообщение без файлов не выдумывает вложений.
        assert!(media_from_params(&params_of(85), &index).is_empty());
    }

    /// ГЛАВНЫЙ СТРАЖ МОДУЛЯ: адреса портала не доезжают до DTO.
    ///
    /// Сама фикстура их СОДЕРЖИТ (`urlDownload`, `urlShow`, `unifiedLink`) —
    /// иначе тест зеленел бы на пустом месте.
    #[test]
    fn no_portal_download_url_survives_the_projection() {
        assert!(MSGS.contains("urlDownload") && MSGS.contains("unifiedLink"));

        let page = page();
        let files = page.files.clone().into_values();
        let index = index_files(&files);
        let media = media_from_params(&params_of(97), &index);
        let json = serde_json::to_string(&media).unwrap();

        for leak in ["urlDownload", "urlShow", "unifiedLink", "downloadUrl", "http"] {
            assert!(!json.contains(leak), "{leak} утёк в DTO: {json}");
        }
    }

    /// Идентификатор без дескриптора — заглушка, а не потерянное вложение.
    #[test]
    fn a_file_id_without_a_descriptor_still_becomes_a_card() {
        let params = serde_json::json!({"FILE_ID": [777, "778"]});
        let media = media_from_params(&params, &HashMap::new());
        assert_eq!(media.len(), 2, "строковый id — тоже id");
        assert_eq!(media[0].file_id, 777);
        assert_eq!(media[1].file_id, 778);
        assert_eq!(media[1].index, 1, "индексы нумеруются подряд");
        assert!(!media[0].has_full);
        assert_eq!(media[0].kind, media_kind::FILE);
    }

    /// Вид вложения: голосовое и «кружок» — ФЛАГИ, а не `type`.
    #[test]
    fn voice_and_video_notes_are_flags_not_types() {
        let file = |json: Value| -> FileRaw { serde_json::from_value(json).unwrap() };

        assert_eq!(file(serde_json::json!({"type": "image"})).kind.as_deref(), Some("image"));
        assert_eq!(file_kind(&file(serde_json::json!({"type": "image"}))), media_kind::IMAGE);
        assert_eq!(file_kind(&file(serde_json::json!({"type": "video"}))), media_kind::VIDEO);
        assert_eq!(file_kind(&file(serde_json::json!({"type": "audio"}))), media_kind::AUDIO);
        assert_eq!(
            file_kind(&file(serde_json::json!({"type": "audio", "isVoiceNote": true}))),
            media_kind::VOICE
        );
        assert_eq!(
            file_kind(&file(serde_json::json!({"type": "video", "isVideoNote": true}))),
            media_kind::VIDEO_NOTE
        );
        // Незнакомый тип — обычный файл, а не ошибка.
        assert_eq!(file_kind(&file(serde_json::json!({"type": "hologram"}))), media_kind::FILE);
        assert_eq!(file_kind(&file(serde_json::json!({}))), media_kind::FILE);
    }

    /// Картинка несёт размеры и превью, документ — ни того, ни другого
    /// (ловушка `image: false | {…}`).
    #[test]
    fn an_image_carries_its_size_and_preview() {
        let raw: FileRaw = serde_json::from_value(serde_json::json!({
            "id": 101, "type": "image", "name": "shot.png", "extension": "PNG", "size": 4096,
            "image": {"height": 61, "width": 72},
            "mediaUrl": {"preview": {"250": "https://cdn/preview.png"}}
        }))
        .unwrap();
        let m = media_from_file(3, &raw);
        assert_eq!(m.kind, media_kind::IMAGE);
        assert_eq!((m.width, m.height), (Some(72), Some(61)));
        assert!(m.has_thumb);
        assert_eq!(m.extension.as_deref(), Some("png"), "расширение приводится к нижнему");
        assert_eq!(m.index, 3);

        let doc: FileRaw = serde_json::from_value(serde_json::json!({
            "id": 102, "type": "file", "image": false,
            "mediaUrl": {"preview": {"250": ""}}
        }))
        .unwrap();
        let m = media_from_file(0, &doc);
        assert_eq!((m.width, m.height), (None, None));
        assert!(!m.has_thumb, "пустой адрес превью — это «превью нет»");

        // Пустой PHP-массив вместо карты превью.
        let odd: FileRaw =
            serde_json::from_value(serde_json::json!({"id": 103, "mediaUrl": {"preview": []}}))
                .unwrap();
        assert!(!media_from_file(0, &odd).has_thumb);
    }

    /// ЖИВАЯ КАРТОЧКА `ATTACH`: три вида блоков разом.
    #[test]
    fn the_live_attach_card_yields_message_link_and_grid_blocks() {
        let cards = cards_from_params(&params_of(79));
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card.id.as_deref(), Some("1785950436"));
        assert_eq!(card.color.as_deref(), Some("#8d51eb"));

        let kinds: Vec<&str> = card.blocks.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds, vec![block_kind::MESSAGE, block_kind::LINK, block_kind::GRID]);

        assert_eq!(card.blocks[0].text.as_deref(), Some("attach text"));
        assert!(!card.blocks[0].segments.is_empty(), "текст блока — тоже сегменты");

        let link = &card.blocks[1].links[0];
        assert_eq!(link.name.as_deref(), Some("Ссылка"));
        assert_eq!(link.url.as_deref(), Some("https://example.com"));
        assert_eq!(link.desc.as_deref(), Some("описание"));

        let row = &card.blocks[2].rows[0];
        assert_eq!(row.name.as_deref(), Some("Поле"));
        assert_eq!(row.value.as_deref(), Some("Значение"));
        assert_eq!(row.display, "line");
    }

    /// Незнакомый блок пропускается МОЛЧА, а BB-коды в тексте блока
    /// разбираются.
    #[test]
    fn unknown_blocks_are_skipped_and_bbcode_inside_a_block_is_parsed() {
        let params = serde_json::json!({"ATTACH": [{
            "BLOCKS": [
                {"HOLOGRAM": {"whatever": true}},
                {"MESSAGE": "[B]жирно[/B] и [URL]https://example.com[/URL]"},
                {"GRID": []}
            ],
            "COLOR": "javascript:alert(1)"
        }]});
        let cards = cards_from_params(&params);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].blocks.len(), 1, "остался только знакомый блок");
        assert_eq!(
            cards[0].blocks[0].text.as_deref(),
            Some("жирно и https://example.com")
        );
        assert!(cards[0]
            .blocks[0]
            .segments
            .iter()
            .any(|s| s.kind == crate::bbcode::kind::BOLD));
        assert_eq!(cards[0].color, None, "не-цвет в COLOR не доезжает до style");

        // Карточка, в которой не осталось ничего, не рисуется вовсе.
        assert!(cards_from_params(&serde_json::json!({"ATTACH": [{"BLOCKS": [{"X": 1}]}]})).is_empty());
        assert!(cards_from_params(&serde_json::json!([])).is_empty());
    }

    /// Адрес в карточке проходит белый список схем.
    #[test]
    fn card_links_only_survive_with_an_http_scheme() {
        for hostile in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "file:///etc/passwd",
            "//evil.example.com",
            "  ",
            "java\nscript:alert(1)",
        ] {
            assert_eq!(http_url(hostile), None, "принят {hostile:?}");
        }
        assert_eq!(
            http_url(" https://example.com/x?y=1 ").as_deref(),
            Some("https://example.com/x?y=1")
        );
        assert_eq!(http_url("HTTP://EXAMPLE.COM").as_deref(), Some("HTTP://EXAMPLE.COM"));
        assert_eq!(http_url(&"h".repeat(4096)), None);

        let params = serde_json::json!({"ATTACH": [{"BLOCKS": [
            {"LINK": [{"NAME": "Клик", "LINK": "javascript:alert(1)"}]}
        ]}]});
        let cards = cards_from_params(&params);
        assert_eq!(cards[0].blocks[0].links[0].url, None, "подпись осталась, адрес — нет");
        assert_eq!(cards[0].blocks[0].links[0].name.as_deref(), Some("Клик"));
    }

    /// Цвет уезжает в `style`, поэтому проходит через шестнадцатеричный фильтр.
    #[test]
    fn colors_are_hex_only() {
        assert_eq!(hex_color("#29619B").as_deref(), Some("#29619b"));
        assert_eq!(hex_color(" #fff ").as_deref(), Some("#fff"));
        for hostile in ["red", "#12345", "#ggg", "url(x)", "", "#", "expression(alert(1))"] {
            assert_eq!(hex_color(hostile), None, "принят {hostile:?}");
        }
    }

    /// ЖИВАЯ КЛАВИАТУРА: одна кнопка `PUT`, работающая клиентски.
    #[test]
    fn the_live_keyboard_yields_one_working_put_button() {
        let kb = keyboard_from_params(&params_of(81)).expect("клавиатура не разобралась");
        assert_eq!(kb.kind, MARKUP_INLINE);
        assert_eq!(kb.rows.len(), 1);
        assert_eq!(kb.rows[0].len(), 1);

        let b = &kb.rows[0][0];
        assert_eq!(b.text, "Кнопка");
        assert_eq!(b.action, button_action::PUT);
        assert_eq!(b.value.as_deref(), Some("hello"));
        assert!(b.enabled);
        assert_eq!(b.disabled_reason, None);
        assert_eq!(b.bg_color.as_deref(), Some("#29619b"));
        assert_eq!(b.text_color.as_deref(), Some("#fff"));

        assert!(keyboard_from_params(&params_of(85)).is_none());
    }

    /// ЖИВАЯ КЛАВИАТУРА (портал `test-work-2`, чат «Команда заботы
    /// Битрикс24», сообщение #49): кнопка `LINK` — самый частый вид кнопки у
    /// РЕАЛЬНЫХ ботов портала, и `ACTION`/`ACTION_VALUE` его НЕ покрывает —
    /// ровно это и было причиной forge-zw3d («кнопки ботов не нажимаются»).
    #[test]
    fn the_live_link_button_from_bitrix_care_bot_works() {
        let params = serde_json::json!({"KEYBOARD": [{
            "BOT_ID": 17,
            "TYPE": "BUTTON",
            "LINK": "https://www.bitrix24.ru/register/cbot.php",
            "TEXT": "Получить консультацию",
            "BLOCK": "N",
            "DISABLED": "N",
            "DISPLAY": "LINE",
            "BG_COLOR": "#fff2b7",
            "TEXT_COLOR": "#000"
        }]});
        let kb = keyboard_from_params(&params).expect("клавиатура не разобралась");
        let b = &kb.rows[0][0];
        assert_eq!(b.action, button_action::LINK);
        assert_eq!(b.value.as_deref(), Some("https://www.bitrix24.ru/register/cbot.php"));
        assert!(b.enabled);
        assert_eq!(b.disabled_reason, None);
        assert!(!b.lock_after_press);
        assert_eq!(b.command, None);
        assert_eq!(b.bot_id, None, "у LINK bot_id не нужен — он не для im.message.command");

        // Тот же рубеж, что у карточек: чужая схема в LINK кнопки не проходит.
        // Кнопка всё равно рисуется (текст есть), но выключенной — «незнакомое
        // действие», а не молча теряется: у неё нет ни валидного LINK, ни
        // COMMAND, ни ACTION.
        let hostile = serde_json::json!({"KEYBOARD": [{
            "TEXT": "Клик", "LINK": "javascript:alert(1)"
        }]});
        let kb = keyboard_from_params(&hostile).expect("кнопка с текстом всё равно рисуется");
        let b = &kb.rows[0][0];
        assert_eq!(b.action, button_action::OTHER);
        assert!(!b.enabled, "невалидный LINK не даёт кнопке ни одного рабочего действия");
    }

    /// ЖИВАЯ КЛАВИАТУРА (тот же портал, сообщение #51): десять кнопок оценки
    /// `COMMAND: "careButton"` + `COMMAND_PARAMS`. Вторая (после `LINK`)
    /// причина forge-zw3d — `ACTION` у этих кнопок нет вовсе, только
    /// `COMMAND`/`COMMAND_PARAMS`/`BOT_ID`. Живая проверка портала
    /// (`im.message.command` с ровно этими полями) подтвердила `result: true`
    /// БЕЗ scope `imbot` — поэтому кнопка приезжает включённой.
    #[test]
    fn the_live_command_button_from_bitrix_care_bot_works() {
        let params = serde_json::json!({"KEYBOARD": [{
            "BOT_ID": 17,
            "TYPE": "BUTTON",
            "COMMAND": "careButton",
            "COMMAND_PARAMS": "5",
            "TEXT": "5",
            "BLOCK": "Y",
            "DISABLED": "N",
            "DISPLAY": "LINE",
            "BG_COLOR": "#fff2b7",
            "TEXT_COLOR": "#000"
        }]});
        let kb = keyboard_from_params(&params).expect("клавиатура не разобралась");
        let b = &kb.rows[0][0];
        assert_eq!(b.action, button_action::COMMAND);
        assert_eq!(b.value, None, "полезная нагрузка COMMAND — не в value");
        assert_eq!(b.command.as_deref(), Some("careButton"));
        assert_eq!(b.command_params.as_deref(), Some("5"));
        assert_eq!(b.bot_id, Some(17));
        assert!(b.enabled, "COMMAND требует только scope im — он есть у вебхука");
        assert_eq!(b.disabled_reason, None);
        assert!(b.lock_after_press, "BLOCK: Y — кнопка блокируется НАВСЕГДА после клика");
    }

    /// `COMMAND` без `BOT_ID` нажать нечем: `im.message.command` требует его
    /// отдельным полем, и без него портал ответит `BOT_ID_ERROR`.
    #[test]
    fn a_command_button_without_bot_id_arrives_disabled() {
        let params = serde_json::json!({"KEYBOARD": [
            {"TEXT": "Без бота", "COMMAND": "x", "COMMAND_PARAMS": "1"}
        ]});
        let kb = keyboard_from_params(&params).expect("клавиатура не разобралась");
        let b = &kb.rows[0][0];
        assert_eq!(b.action, button_action::COMMAND);
        assert!(!b.enabled);
        assert_eq!(b.disabled_reason.as_deref(), Some(COMMAND_NEEDS_BOT_ID));
    }

    /// CALL и DIALOG требуют scope `imbot` — приезжают выключенными И с
    /// объяснением (§6): кнопка, которая молча ничего не делает, хуже
    /// отсутствующей.
    #[test]
    fn call_and_dialog_buttons_arrive_disabled_with_a_reason() {
        let params = serde_json::json!({"KEYBOARD": [
            {"TEXT": "Позвонить", "ACTION": "CALL", "ACTION_VALUE": "+70000000000"},
            {"TEXT": "Открыть чат", "ACTION": "DIALOG", "ACTION_VALUE": "chat7"},
            {"TEXT": "Отправить", "ACTION": "SEND", "ACTION_VALUE": "да"},
            {"TEXT": "Выключена", "ACTION": "PUT", "DISABLED": "Y"},
            {"TEXT": "Новое действие", "ACTION": "TELEPORT"}
        ]});
        let kb = keyboard_from_params(&params).unwrap();
        let flat: Vec<&ButtonDto> = kb.rows.iter().flatten().collect();
        assert_eq!(flat.len(), 5);

        assert!(!flat[0].enabled);
        assert_eq!(flat[0].disabled_reason.as_deref(), Some(IMBOT_REQUIRED));
        assert!(!flat[1].enabled);
        assert_eq!(flat[1].disabled_reason.as_deref(), Some(IMBOT_REQUIRED));

        assert!(flat[2].enabled, "SEND работает клиентски");
        assert_eq!(flat[2].action, button_action::SEND);

        assert!(!flat[3].enabled, "портал сам выключил кнопку");
        assert_eq!(flat[3].disabled_reason.as_deref(), Some(DISABLED_BY_PORTAL));

        assert_eq!(flat[4].action, button_action::OTHER);
        assert!(!flat[4].enabled, "незнакомое действие не нажимается");
    }

    /// Раскладка: `NEWLINE` рвёт ряд, `DISPLAY: BLOCK` занимает ряд целиком.
    #[test]
    fn rows_are_split_by_newline_and_by_block_display() {
        let params = serde_json::json!({"KEYBOARD": [
            {"TEXT": "1", "ACTION": "PUT", "DISPLAY": "LINE"},
            {"TEXT": "2", "ACTION": "PUT", "DISPLAY": "LINE"},
            {"TYPE": "NEWLINE"},
            {"TEXT": "3", "ACTION": "PUT", "DISPLAY": "LINE"},
            {"TEXT": "широкая", "ACTION": "PUT", "DISPLAY": "BLOCK"},
            {"TEXT": "4", "ACTION": "PUT"}
        ]});
        let kb = keyboard_from_params(&params).unwrap();
        let shape: Vec<Vec<&str>> = kb
            .rows
            .iter()
            .map(|r| r.iter().map(|b| b.text.as_str()).collect())
            .collect();
        assert_eq!(
            shape,
            vec![vec!["1", "2"], vec!["3"], vec!["широкая"], vec!["4"]]
        );

        // Кнопка без подписи нажиматься не может — её нет вовсе.
        let kb = keyboard_from_params(&serde_json::json!({"KEYBOARD": [
            {"ACTION": "PUT", "ACTION_VALUE": "нечего нажимать"},
            {"TEXT": "  "}
        ]}));
        assert!(kb.is_none());
    }

    /// Клавиатура ограничена: сотни кнопок — это либо ошибка бота, либо
    /// попытка распереть панель.
    #[test]
    fn a_giant_keyboard_is_clamped() {
        let buttons: Vec<Value> = (0..500)
            .map(|i| serde_json::json!({"TEXT": format!("к{i}"), "ACTION": "PUT"}))
            .collect();
        let kb = keyboard_from_params(&serde_json::json!({"KEYBOARD": buttons})).unwrap();
        assert_eq!(kb.rows.iter().flatten().count(), MAX_BUTTONS);

        let ids: Vec<Value> = (0..100).map(|i| serde_json::json!(i + 1)).collect();
        let media = media_from_params(&serde_json::json!({"FILE_ID": ids}), &HashMap::new());
        assert_eq!(media.len(), MAX_MEDIA);
    }

    /// ЛОВУШКА: `LIKE` приезжает то строками, то числами — разбираются обе
    /// формы, наружу уходит одна.
    #[test]
    fn likes_parse_both_string_and_number_ids() {
        // Живой ответ сразу после `im.message.like` — ЧИСЛА.
        assert_eq!(likes_from_params(&params_of(85)), vec!["1".to_string()]);
        // Форма из разведки — СТРОКИ.
        assert_eq!(
            likes_from_params(&serde_json::json!({"LIKE": ["1", "15"]})),
            vec!["1".to_string(), "15".to_string()]
        );
        // Мусор и нули отбрасываются, поле без лайков даёт пустой список.
        assert_eq!(
            likes_from_params(&serde_json::json!({"LIKE": [0, "", "abc", 19]})),
            vec!["19".to_string()]
        );
        assert!(likes_from_params(&serde_json::json!([])).is_empty());
        assert!(likes_from_params(&serde_json::json!({})).is_empty());
    }

    /// Код системного действия — `params.CODE[0]`; правка — `IS_EDITED`.
    #[test]
    fn the_action_code_and_the_edited_flag_come_from_params() {
        assert_eq!(action_from_params(&params_of(71)).as_deref(), Some("CHAT_JOIN"));
        assert_eq!(action_from_params(&params_of(103)).as_deref(), Some("CHAT_LEAVE"));
        assert_eq!(action_from_params(&params_of(85)), None);
        assert_eq!(
            action_from_params(&serde_json::json!({"CODE": "CHAT_LEAVE"})).as_deref(),
            Some("CHAT_LEAVE")
        );
        assert_eq!(action_from_params(&serde_json::json!({"CODE": []})), None);

        assert!(is_yes(param(&params_of(111), P_IS_EDITED)));
        assert!(!is_yes(param(&params_of(85), P_IS_EDITED)));
        assert!(is_yes(Some(&serde_json::json!("y"))), "регистр не значим");
        assert!(!is_yes(Some(&serde_json::json!(true))), "true — это не «Y»");
    }
}
