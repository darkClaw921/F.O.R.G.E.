//! Разбор вложений VK в [`MediaDto`].
//!
//! Аналога в Telegram-плагине нет: там у сообщения ОДНО вложение и его вид
//! выводится из типа `grammers`. У VK вложений до десяти, они разнородны, и
//! форма каждого документирована неполно — поэтому классификация вынесена в
//! отдельный модуль с табличным тестом на каждый вид.
//!
//! # Две половины модуля
//!
//! 1. **Классификация и метаданные** ([`media_list`], [`media_from_attachment`])
//!    — вид, mime, размер, геометрия, длительность, имя файла, заголовок,
//!    огибающая голосового. Отсюда рождается [`MediaDto`], и URL вложений в
//!    него не попадают НИКОГДА (кроме `player_url` видео, см. ниже).
//! 2. **Выбор файла для скачивания** ([`pick_size`], [`url_for`]) — Ф7. Адрес
//!    существует ровно на время одного скачивания и живёт только внутри
//!    бэкенда: ссылки VK подписанные и протухающие, поэтому они добываются
//!    непосредственно перед обращением к CDN и нигде не кэшируются.
//!
//! Обе половины ЧИСТЫЕ: ни сети, ни диска, ни состояния.
//!
//! # Инвариант устойчивости
//!
//! Незнакомый `attachment.type` даёт [`MediaDto`] с `kind: "unsupported"`, а
//! НЕ ошибку разбора. VK добавляет типы вложений без предупреждения, и падение
//! на новом типе стоило бы пользователю всей страницы переписки ради одной
//! карточки, которую он и так не смог бы открыть.
//!
//! # Ссылки наружу
//!
//! Полей со ссылкой ровно два, и НИ ОДНО не ведёт на CDN вложений:
//!
//! * [`MediaDto::player_url`] у видео — страница-плеер VK (файла нет, см. ниже);
//! * [`MediaDto::link_url`] у вложения `link` — адрес, который прислал
//!   собеседник. Это содержимое переписки, а не инфраструктура VK: карточка
//!   без перехода по такой ссылке не делает того единственного, ради чего
//!   ссылку и присылают.
//!
//! Все URL ФАЙЛОВ остаются на бэкенде: байты отдаёт
//! `GET /api/vk/media/:peer/:mid/:index` (Ф7). Именно эта граница и защищена
//! тестом `attachment_cdn_urls_never_leak`.
//!
//! ## Честное ограничение видео
//!
//! Вложение `video` у VK **не содержит воспроизводимого файла**. `video.get`
//! отдаёт `player` — адрес СТРАНИЦЫ-плеера, а прямые `files.*` доступны только
//! владельцу ролика и приложениям с правами на видео. Значит проксировать и
//! кэшировать видеофайл мы не можем — и не должны делать вид, что можем:
//! `kind: "video"` несёт превью (`has_thumb`) и `player_url` для открытия во
//! вкладке, а `has_full` у него ВСЕГДА `false`.

use crate::api::schema::{
    AttachmentRaw, AudioMessageRaw, AudioRaw, DocRaw, GraffitiRaw, LinkRaw, PhotoRaw,
    PhotoSizeRaw, StickerImageRaw, StickerRaw, VideoRaw, KIND_AUDIO, KIND_DOCUMENT, KIND_GRAFFITI,
    KIND_OTHER, KIND_PHOTO, KIND_STICKER, KIND_VIDEO, KIND_VOICE,
};
use crate::dto::MediaDto;

/// Сколько вложений у сообщения VK максимум.
///
/// Документированный предел — 10, и он же ограничивает индекс в ключе
/// медиа-кэша (`MAX_ATTACH_INDEX = 9`, Ф7). Остаток отбрасывается здесь, а не
/// «когда-нибудь в UI»: индекс объявлен `u8`, и вложение №300 из испорченного
/// ответа не должно уметь переполнить ключ кэша.
pub const MAX_ATTACHMENTS: usize = 10;

/// Максимальная длина заголовка/имени файла, символов.
///
/// Имя файла у VK приходит от отправителя, то есть это чужой ввод. Обрезка
/// здесь, а не в вёрстке: строка в 100 КБ дороже обходится JSON'у и памяти
/// вкладки, чем ширине карточки.
pub const MAX_TITLE_CHARS: usize = 200;

/// Сколько столбиков огибающей голосового отдаём наружу.
///
/// VK присылает около сотни; больше двух сотен рисовать всё равно негде, а
/// массив произвольной длины из ответа — это неограниченный рост DTO.
pub const MAX_WAVEFORM: usize = 200;

/// Все вложения сообщения в проекции для UI, по одному [`MediaDto`] на штуку.
///
/// `index` — ПОЗИЦИЯ в исходном массиве: именно она входит в ключ медиа-кэша и
/// в маршрут `/api/vk/media/:peer/:mid/:index`, поэтому пересортировка или
/// фильтрация «ненужных» видов здесь запрещены — они сдвинули бы индексы и
/// показали бы пользователю не то вложение.
pub fn media_list(atts: &[AttachmentRaw]) -> Vec<MediaDto> {
    atts.iter()
        .take(MAX_ATTACHMENTS)
        .enumerate()
        .map(|(i, a)| media_from_attachment(i as u8, a))
        .collect()
}

/// Вид ПЕРВОГО вложения — для превью цитаты и пересланного.
///
/// Отдельная функция, потому что цитате не нужен весь [`MediaDto`]: там
/// рисуется один значок, а собирать ради него полный список вложений каждого
/// пересланного сообщения (а их до десяти на каждом из трёх уровней) — лишняя
/// работа на каждой строке ленты.
pub fn first_kind(atts: &[AttachmentRaw]) -> Option<String> {
    atts.first().map(|a| a.normalized_kind().to_string())
}

/// Одно вложение → [`MediaDto`].
///
/// Ветка на каждый известный вид; неизвестный отдаёт `kind: "unsupported"` с
/// пустыми метаданными.
///
/// # `has_thumb`/`has_full` считает [`url_for`], а не ветка вида
///
/// Это НЕ стилистика, а починка целого класса дефектов. Раньше каждая ветка
/// `from_*` выставляла флаги сама («у ссылки есть `photo.sizes` ⇒ превью
/// есть»), а роут `/api/vk/media/…` спрашивал адрес у [`url_for`] — то есть
/// ОБЕЩАНИЕ и СПОСОБНОСТЬ его выполнить жили в двух разных функциях и могли
/// разъехаться от любой правки одной из них. Наружу это вылезает как карточка,
/// которая просит превью и получает `404 no_media`.
///
/// Теперь флаг — это буквально «а адрес-то нашёлся?»:
///
/// ```text
/// has_thumb ≡ url_for(att, Thumb).is_some()
/// has_full  ≡ url_for(att, Full ).is_some()
/// ```
///
/// Разъехаться нечему: источник знания один. Побочный (и желанный) эффект —
/// флаги стали строже там, где VK прислал размер без адреса или с адресом
/// нечитаемой схемы: раньше это обещало превью, которого не существует.
/// Инвариант закреплён тестом `promises_never_outrun_url_for`.
pub fn media_from_attachment(index: u8, att: &AttachmentRaw) -> MediaDto {
    use crate::media_cache::MediaKind;
    let mut m = describe(index, att);
    m.has_thumb = url_for(att, MediaKind::Thumb).is_some();
    m.has_full = url_for(att, MediaKind::Full).is_some();
    m
}

/// Метаданные вложения БЕЗ флагов наличия файлов.
///
/// Отделена от [`media_from_attachment`] ровно затем, чтобы флаги не мог
/// проставить никто, кроме [`url_for`], — см. шапку вызывающей функции.
fn describe(index: u8, att: &AttachmentRaw) -> MediaDto {
    let kind = att.normalized_kind();
    let base = MediaDto {
        index,
        kind: kind.to_string(),
        ..MediaDto::default()
    };
    match kind {
        KIND_PHOTO => match att.photo.as_ref() {
            Some(p) => from_photo(base, p),
            None => base,
        },
        KIND_VIDEO => match att.video.as_ref() {
            Some(v) => from_video(base, v),
            None => base,
        },
        KIND_AUDIO => match att.audio.as_ref() {
            Some(a) => from_audio(base, a),
            None => base,
        },
        KIND_VOICE => match att.audio_message.as_ref() {
            Some(a) => from_audio_message(base, a),
            None => base,
        },
        KIND_DOCUMENT => match att.doc.as_ref() {
            Some(d) => from_doc(base, d),
            None => base,
        },
        KIND_STICKER => match att.sticker.as_ref() {
            Some(s) => from_sticker(base, s),
            None => base,
        },
        KIND_GRAFFITI => match att.graffiti.as_ref() {
            Some(g) => from_graffiti(base, g),
            None => base,
        },
        // Известный вид без собственного файла: ссылка, опрос, товар, запись
        // на стене. Заголовок берём у тех, у кого он есть, — карточка без
        // единого слова выглядит поломкой рендера.
        KIND_OTHER => match att.link.as_ref() {
            Some(l) => from_link(base, l),
            None => base,
        },
        // KIND_UNSUPPORTED и всё, что могло бы появиться в словаре позже.
        _ => base,
    }
}

// ---------------------------------------------------------------------------
// Ветки по видам.
// ---------------------------------------------------------------------------

/// Фотография: геометрия по САМОМУ БОЛЬШОМУ из присланных размеров.
///
/// Именно по большему, а не по первому: порядок `sizes` VK не гарантирует, а
/// соотношение сторон в UI считается по width/height и на превью 75×75 дало бы
/// квадрат вместо панорамы.
fn from_photo(mut m: MediaDto, p: &PhotoRaw) -> MediaDto {
    let (w, h) = largest_size(&p.sizes);
    m.width = w;
    m.height = h;
    m.mime = Some("image/jpeg".to_string());
    m
}

/// Видео: превью + страница-плеер, файла нет (см. шапку модуля).
fn from_video(mut m: MediaDto, v: &VideoRaw) -> MediaDto {
    m.title = clean_title(&v.title);
    m.duration_secs = (v.duration > 0).then(|| clamp_i32(v.duration));
    let (iw, ih) = largest_size(&v.image);
    m.width = if v.width > 0 {
        Some(clamp_i32(v.width))
    } else {
        iw
    };
    m.height = if v.height > 0 {
        Some(clamp_i32(v.height))
    } else {
        ih
    };
    // ↓ `has_full` останется false: `url_for(_, Full)` у видео возвращает None
    //   ВСЕГДА — воспроизводимого файла во вложении video не существует. Это
    //   ограничение платформы, а не недоделка (см. шапку модуля).
    m.player_url = (!v.player.is_empty()).then(|| v.player.clone());
    m
}

/// Аудиозапись: «исполнитель — название».
fn from_audio(mut m: MediaDto, a: &AudioRaw) -> MediaDto {
    let artist = a.artist.trim();
    let title = a.title.trim();
    m.title = clean_title(&match (artist.is_empty(), title.is_empty()) {
        (false, false) => format!("{artist} — {title}"),
        (true, false) => title.to_string(),
        (false, true) => artist.to_string(),
        (true, true) => String::new(),
    });
    m.duration_secs = (a.duration > 0).then(|| clamp_i32(a.duration));
    m.mime = Some("audio/mpeg".to_string());
    // Ссылку на музыку VK отдаёт не всегда (правообладатели); её отсутствие
    // означает «файла для нас нет», и карточка обязана показать это сразу, а
    // не после неудачной попытки скачивания. Считает это `url_for`.
    m
}

/// Голосовое сообщение: длительность + огибающая.
fn from_audio_message(mut m: MediaDto, a: &AudioMessageRaw) -> MediaDto {
    m.duration_secs = (a.duration > 0).then(|| clamp_i32(a.duration));
    m.mime = Some(if a.link_ogg.is_empty() {
        "audio/mpeg".to_string()
    } else {
        "audio/ogg".to_string()
    });
    m.waveform = a
        .waveform
        .iter()
        .take(MAX_WAVEFORM)
        .map(|v| clamp_i32(*v))
        .collect();
    m
}

/// Документ: имя, размер, mime и превью, если VK его сделал.
///
/// Числовой `doc.type` VK (3 — гиф, 4 — изображение, 5 — аудио, 6 — видео)
/// используется ТОЛЬКО как источник mime при неизвестном расширении: сам вид
/// остаётся `document`, потому что словарь [`MediaDto::kind`] — это то, какой
/// КАРТОЧКОЙ рисовать вложение, а гифка из документа остаётся файлом с именем
/// и размером.
fn from_doc(mut m: MediaDto, d: &DocRaw) -> MediaDto {
    m.file_name = clean_title(&d.title);
    m.title = m.file_name.clone();
    m.size = (d.size > 0).then_some(d.size);
    m.mime = doc_mime(d);
    if let Some(preview) = d.preview.as_ref().and_then(|p| p.photo.as_ref()) {
        let (w, h) = largest_size(&preview.sizes);
        m.width = w;
        m.height = h;
    }
    m
}

/// Стикер: картинка на прозрачном фоне.
fn from_sticker(mut m: MediaDto, s: &StickerRaw) -> MediaDto {
    let images = if s.images.is_empty() {
        &s.images_with_background
    } else {
        &s.images
    };
    let (w, h) = largest_sticker(images);
    m.width = w;
    m.height = h;
    m.mime = Some("image/png".to_string());
    m
}

/// Граффити — по сути картинка, но отдельным видом (у него своя карточка).
fn from_graffiti(mut m: MediaDto, g: &GraffitiRaw) -> MediaDto {
    m.width = (g.width > 0).then(|| clamp_i32(g.width));
    m.height = (g.height > 0).then(|| clamp_i32(g.height));
    m.mime = Some("image/png".to_string());
    m
}

/// Ссылка с превью: заголовок, геометрия превью и САМ АДРЕС.
///
/// # Почему адрес всё-таки уезжает наружу
///
/// Раньше поля под него не было вовсе, и карточка ссылки состояла из заголовка
/// с отключёнными кнопками — то есть не делала ровно того единственного, ради
/// чего ссылку присылают. `link.url` — это НЕ инфраструктурный адрес VK
/// (подписанный CDN вложений), а содержимое переписки: его набрал собеседник,
/// и пользователь идёт по нему сознательно. Инвариант «браузер не ходит на
/// серверы VK» этим не задет — см. шапку [`crate::dto::MediaDto`].
///
/// [`http_url`] здесь обязателен: `link.url` приходит из чужого аккаунта, а
/// значит `javascript:`/`data:` в нём — вопрос желания отправителя, а не
/// теории. Фронт проверяет второй раз (`js/vk/format.js::webUrl`).
fn from_link(mut m: MediaDto, l: &LinkRaw) -> MediaDto {
    let title = if l.title.trim().is_empty() {
        l.caption.trim()
    } else {
        l.title.trim()
    };
    m.title = clean_title(title);
    m.link_url = http_url(&l.url);
    if let Some(p) = l.photo.as_ref() {
        let (w, h) = largest_size(&p.sizes);
        m.width = w;
        m.height = h;
    }
    m
}

// ---------------------------------------------------------------------------
// Выбор файла для скачивания (Ф7).
//
// Всё, что ниже, — ЧИСТЫЕ функции: они не ходят в сеть и ничего не пишут на
// диск, поэтому проверяются табличными тестами без стенда. Результат
// [`url_for`] живёт ровно до конца одного скачивания: URL вложений VK
// подписанные и протухающие, и плагин НЕ КЭШИРУЕТ их никогда (см. шапку
// `media_cache`).
// ---------------------------------------------------------------------------

/// Нижняя граница «полезного» превью, пиксели.
///
/// Карточка рисует превью высотой 184 px (`css/vk.css::.vk-media-thumb`), и
/// картинка мельче двухсот по большей стороне на ней уже мылит. 200 — ближайший
/// к этому размер из линейки VK (`p`).
pub const THUMB_MIN_PX: i32 = 200;

/// Один вариант картинки, в терминах, не зависящих от формы ответа VK.
///
/// У фотографии, превью документа, кадра видео и картинки стикера структуры
/// РАЗНЫЕ (у стикера, например, нет буквенного типа вовсе), а правило выбора
/// одно — поэтому оно работает с этой проекцией, а не с четырьмя типами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeCandidate {
    /// Буквенный код размера VK (`s m o p q r x y z w`); `'\0'` — его нет.
    pub letter: char,
    /// Ширина в пикселях; `0` ⇒ VK её не прислал.
    pub width: i32,
    /// Высота в пикселях; `0` ⇒ VK её не прислала.
    pub height: i32,
}

/// Выбирает вариант картинки под нужный вид.
///
/// ЧИСТАЯ. `None` ⇒ вариантов нет вовсе (роут отвечает `404 no_media`, фронт
/// рисует значок).
///
/// Правило:
/// * `Full` — максимальный по `max(width, height)`;
/// * `Thumb` — МИНИМАЛЬНЫЙ из тех, у кого `max(width, height) >=`
///   [`THUMB_MIN_PX`]; если такого нет — самый большой из имеющихся (все они
///   мелкие, и «менее размытый» тут лучше «поменьше весом»).
///
/// # Почему по числам, а не по букве
///
/// Буквенный набор VK (`s m o p q r x y z w`) исторически менялся, у разных
/// объектов он разный, а у картинок стикера его нет совсем. Сортировка по
/// числовым размерам устойчива к этому. Буква остаётся ЗАПАСНЫМ ключом: она
/// используется, только когда `width`/`height` равны нулю (старые фото
/// приезжают без геометрии) и как детерминированный разрыв ничьей.
pub fn pick_size(sizes: &[SizeCandidate], kind: crate::media_cache::MediaKind) -> Option<usize> {
    use crate::media_cache::MediaKind;
    if sizes.is_empty() {
        return None;
    }
    if kind == MediaKind::Thumb {
        let smallest_usable = sizes
            .iter()
            .enumerate()
            .filter(|(_, c)| size_px(c) >= THUMB_MIN_PX)
            // min_by_key при равенстве оставляет ПЕРВЫЙ — дубликаты размеров
            // не должны зависеть от порядка обхода.
            .min_by_key(|(_, c)| score(c))
            .map(|(i, _)| i);
        if smallest_usable.is_some() {
            return smallest_usable;
        }
    }
    largest_index(sizes)
}

/// Индекс самого большого варианта; при ничьей — первый по порядку.
fn largest_index(sizes: &[SizeCandidate]) -> Option<usize> {
    sizes
        .iter()
        .enumerate()
        // `max_by` возвращает ПОСЛЕДНИЙ максимум, поэтому ничья разрывается
        // обратным сравнением индексов: побеждает тот, что встретился раньше.
        .max_by(|(ia, a), (ib, b)| score(a).cmp(&score(b)).then(ib.cmp(ia)))
        .map(|(i, _)| i)
}

/// Размер варианта в пикселях по большей стороне.
///
/// Ноль в `width`/`height` означает «VK не прислал геометрию» (так приезжают
/// старые фотографии) — тогда и ТОЛЬКО тогда в дело идёт оценка по букве.
fn size_px(c: &SizeCandidate) -> i32 {
    let m = c.width.max(c.height);
    if m > 0 {
        m
    } else {
        letter_px(c.letter)
    }
}

/// Ключ сравнения: сначала пиксели, затем буква (детерминированная ничья).
fn score(c: &SizeCandidate) -> (i32, i32) {
    (size_px(c), letter_px(c.letter))
}

/// Табличная оценка размера по буквенному коду VK, пиксели.
///
/// Только для случая «геометрии в ответе нет». Значения — документированные
/// размеры линейки VK по большей стороне; незнакомая буква даёт `0`, то есть
/// «меньше всех», и такой вариант выбирается последним.
fn letter_px(letter: char) -> i32 {
    match letter {
        's' => 75,
        'm' | 'o' => 130,
        'p' => 200,
        'q' => 320,
        'r' => 510,
        'x' => 604,
        'y' => 807,
        'z' => 1080,
        'w' => 2560,
        _ => 0,
    }
}

/// Проекция размеров фотографии в кандидаты.
fn photo_candidates(sizes: &[PhotoSizeRaw]) -> Vec<SizeCandidate> {
    sizes
        .iter()
        .map(|s| SizeCandidate {
            letter: s.kind.chars().next().unwrap_or('\0'),
            width: clamp_i32(s.width),
            height: clamp_i32(s.height),
        })
        .collect()
}

/// Проекция картинок стикера (буквенного кода у них нет).
fn sticker_candidates(images: &[StickerImageRaw]) -> Vec<SizeCandidate> {
    images
        .iter()
        .map(|s| SizeCandidate {
            letter: '\0',
            width: clamp_i32(s.width),
            height: clamp_i32(s.height),
        })
        .collect()
}

/// Адрес выбранного размера фотографии.
fn photo_url(sizes: &[PhotoSizeRaw], kind: crate::media_cache::MediaKind) -> Option<String> {
    let idx = pick_size(&photo_candidates(sizes), kind)?;
    http_url(&sizes[idx].url)
}

/// Адрес выбранной картинки стикера.
fn sticker_url(images: &[StickerImageRaw], kind: crate::media_cache::MediaKind) -> Option<String> {
    let idx = pick_size(&sticker_candidates(images), kind)?;
    http_url(&images[idx].url)
}

/// Пропускает только `http`/`https`.
///
/// Два применения, и оба про чужой ввод:
///
/// 1. адрес файла из ответа VK уходит прямо в `VkClient::get_stream` — схемы
///    вроде `file://` в ответе взяться неоткуда, но проверка стоит одну
///    строку, а её отсутствие превращает любую подмену ответа в чтение
///    локальных файлов сервера;
/// 2. `link.url` из вложения `link` уезжает в браузер и становится `href` —
///    здесь `javascript:` уже не теория, а то, что отправитель может набрать
///    руками.
///
/// `http` разрешён наравне с `https` намеренно: на нём стоит интеграционный
/// стенд (`tests/vk_stub.rs`), а боевой CDN VK всё равно отдаёт только `https`.
pub fn http_url(raw: &str) -> Option<String> {
    let t = raw.trim();
    (t.starts_with("https://") || t.starts_with("http://")).then(|| t.to_string())
}

/// Какой URL качать для этого вложения и вида.
///
/// ЧИСТАЯ и ЕДИНСТВЕННАЯ точка, где адрес вложения покидает модуль. `None`
/// означает «файла нет» — роут отвечает `404 no_media`.
///
/// # Она же — источник `has_thumb`/`has_full`
///
/// [`media_from_attachment`] спрашивает флаги ИМЕННО ЗДЕСЬ, а не считает их по
/// своим соображениям. Значит, любая правка таблицы ниже автоматически
/// доезжает до DTO, и карточка не может попросить превью, которого роут не
/// найдёт (см. шапку [`media_from_attachment`]).
///
/// Таблица (`thumb` / `full`):
///
/// | вид | thumb | full |
/// |---|---|---|
/// | `photo` | `sizes[]` по [`pick_size`] | `sizes[]` максимум |
/// | `doc` | `preview.photo.sizes[]` | `doc.url` |
/// | `audio_message` | нет | `link_ogg`, иначе `link_mp3` |
/// | `audio` | нет | `audio.url` (VK отдаёт его не всегда) |
/// | `sticker` | `images[]` по [`pick_size`] | `images_with_background[]` максимум |
/// | `graffiti` | `url` | `url` |
/// | `video` | `image[]` (иначе `first_frame[]`) | **НЕТ** |
/// | `link` и прочее `other` | превью ссылки, если есть | нет |
///
/// # Видео — честное ограничение
///
/// У вложения `video` НЕТ воспроизводимого файла: `video.get` отдаёт
/// `player` — адрес страницы, а прямые `files.*` доступны только владельцу
/// ролика. Поэтому `full` у видео — всегда `None`, а UI открывает
/// [`crate::dto::MediaDto::player_url`] во вкладке.
pub fn url_for(att: &AttachmentRaw, kind: crate::media_cache::MediaKind) -> Option<String> {
    use crate::media_cache::MediaKind;
    let full = kind == MediaKind::Full;
    match att.normalized_kind() {
        KIND_PHOTO => photo_url(&att.photo.as_ref()?.sizes, kind),
        KIND_VIDEO => {
            if full {
                // ↓ НЕ недоделка: файла во вложении video не существует.
                return None;
            }
            let v = att.video.as_ref()?;
            let frames = if v.image.is_empty() {
                &v.first_frame
            } else {
                &v.image
            };
            photo_url(frames, kind)
        }
        KIND_AUDIO => full.then(|| http_url(&att.audio.as_ref()?.url)).flatten(),
        KIND_VOICE => {
            if !full {
                // Огибающая рисуется по `waveform` из DTO — картинки у
                // голосового нет и запрашивать её незачем.
                return None;
            }
            let a = att.audio_message.as_ref()?;
            http_url(&a.link_ogg).or_else(|| http_url(&a.link_mp3))
        }
        KIND_DOCUMENT => {
            let d = att.doc.as_ref()?;
            if full {
                return http_url(&d.url);
            }
            photo_url(&d.preview.as_ref()?.photo.as_ref()?.sizes, kind)
        }
        KIND_STICKER => {
            let s = att.sticker.as_ref()?;
            // Для оригинала предпочтителен вариант С ФОНОМ: прозрачный PNG на
            // светлой теме панели превращается в невидимое пятно. Пустой набор
            // (у обоих видов) отдаёт второй — иначе стикер без прозрачных
            // картинок не показался бы вовсе.
            let prefer_background = full || s.images.is_empty();
            let images = if prefer_background && !s.images_with_background.is_empty() {
                &s.images_with_background
            } else {
                &s.images
            };
            sticker_url(images, kind)
        }
        KIND_GRAFFITI => http_url(&att.graffiti.as_ref()?.url),
        // Ссылка, опрос, товар, запись на стене: файла нет, но превью бывает.
        //
        // Превью здесь ТОЛЬКО у `link`: у остальных членов группы (`wall`,
        // `market`, `article`, `poll`, …) содержимое в [`AttachmentRaw`] не
        // разбирается вовсе — их картинки лежат внутри `wall.attachments`,
        // `market.thumb_photo` и прочих форм, у каждой своей. Пока разбора нет,
        // `url_for` честно отвечает `None`, а `has_thumb` — `false`, и карточка
        // сразу рисует значок вместо запроса, который вернул бы `no_media`.
        KIND_OTHER => {
            if full {
                return None;
            }
            photo_url(&att.link.as_ref()?.photo.as_ref()?.sizes, kind)
        }
        // KIND_UNSUPPORTED и всё, что появится в словаре VK позже.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Вспомогательное.
// ---------------------------------------------------------------------------

/// MIME документа: по расширению имени, иначе по числовому типу VK.
///
/// Расширение первично, потому что оно приходит от отправителя вместе с
/// файлом; числовой тип — грубая категория («видео», «аудио»), и подставлять
/// по ней конкретный контейнер было бы враньём.
fn doc_mime(d: &DocRaw) -> Option<String> {
    let name = if d.ext.is_empty() {
        d.title.clone()
    } else {
        format!("file.{}", d.ext)
    };
    if let Some(m) = mime_guess::from_path(&name).first() {
        return Some(m.to_string());
    }
    match d.kind {
        3 => Some("image/gif".to_string()),
        4 => Some("image/jpeg".to_string()),
        5 => Some("audio/mpeg".to_string()),
        6 => Some("video/mp4".to_string()),
        _ => None,
    }
}

/// Наибольший из размеров фотографии.
fn largest_size(sizes: &[PhotoSizeRaw]) -> (Option<i32>, Option<i32>) {
    match sizes
        .iter()
        .max_by_key(|s| s.width.saturating_mul(s.height))
    {
        Some(s) if s.width > 0 && s.height > 0 => {
            (Some(clamp_i32(s.width)), Some(clamp_i32(s.height)))
        }
        _ => (None, None),
    }
}

/// То же для картинок стикера (у них своя структура).
fn largest_sticker(images: &[StickerImageRaw]) -> (Option<i32>, Option<i32>) {
    match images
        .iter()
        .max_by_key(|s| s.width.saturating_mul(s.height))
    {
        Some(s) if s.width > 0 && s.height > 0 => {
            (Some(clamp_i32(s.width)), Some(clamp_i32(s.height)))
        }
        _ => (None, None),
    }
}

/// Непустой заголовок, обрезанный по [`MAX_TITLE_CHARS`].
fn clean_title(raw: &str) -> Option<String> {
    let t = raw.trim();
    (!t.is_empty()).then(|| crate::dto::truncate_chars(t, MAX_TITLE_CHARS))
}

/// Насыщающее сужение до i32 (геометрия и длительности у VK 32-битные).
fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        DocPreviewPhotoRaw, DocPreviewRaw, ItemsRaw, MessageRaw, KIND_UNSUPPORTED,
    };

    fn att(kind: &str) -> AttachmentRaw {
        AttachmentRaw {
            kind: kind.to_string(),
            ..AttachmentRaw::default()
        }
    }

    fn size(w: i64, h: i64) -> PhotoSizeRaw {
        PhotoSizeRaw {
            url: "https://sun.example/x.jpg".into(),
            width: w,
            height: h,
            kind: "x".into(),
        }
    }

    /// ТАБЛИЦА ВИДОВ целиком: каждый тип VK обязан получить свой `kind`, и
    /// незнакомый — `unsupported`, а не ошибку.
    #[test]
    fn kind_table_covers_every_documented_type() {
        let table = [
            ("photo", "photo"),
            ("video", "video"),
            ("audio", "audio"),
            ("audio_message", "voice"),
            ("doc", "document"),
            ("sticker", "sticker"),
            ("graffiti", "graffiti"),
            ("wall", "other"),
            ("wall_reply", "other"),
            ("link", "other"),
            ("market", "other"),
            ("market_album", "other"),
            ("poll", "other"),
            ("gift", "other"),
            ("story", "other"),
            ("call", "other"),
            ("podcast", "other"),
            ("article", "other"),
            // Из будущего — и это НЕ ошибка разбора.
            ("app_action", "unsupported"),
            ("hologram", "unsupported"),
            ("", "unsupported"),
        ];
        for (raw, expected) in table {
            let m = media_from_attachment(0, &att(raw));
            assert_eq!(m.kind, expected, "тип {raw:?}");
            assert_eq!(m.index, 0);
        }
    }

    /// Индекс — это ПОЗИЦИЯ, и она не должна зависеть от вида вложения:
    /// на ней держится ключ медиа-кэша.
    #[test]
    fn indexes_follow_positions_and_are_capped() {
        let atts: Vec<AttachmentRaw> = ["photo", "hologram", "doc", "video"]
            .iter()
            .map(|k| att(k))
            .collect();
        let list = media_list(&atts);
        assert_eq!(
            list.iter().map(|m| m.index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
            "неизвестный вид НЕ выбрасывается — иначе индексы сдвинулись бы"
        );
        assert_eq!(list[1].kind, "unsupported");

        // Испорченный ответ с сотней вложений не должен переполнить u8-индекс.
        let many: Vec<AttachmentRaw> = (0..300).map(|_| att("photo")).collect();
        let list = media_list(&many);
        assert_eq!(list.len(), MAX_ATTACHMENTS);
        assert_eq!(list.last().unwrap().index, (MAX_ATTACHMENTS - 1) as u8);
    }

    /// Вложение, у которого нет НИ ОДНОГО необязательного поля, не должно
    /// ронять разбор: у VK так приезжают удалённые и приватные объекты.
    #[test]
    fn bare_attachments_do_not_panic() {
        for kind in ["photo", "video", "audio", "audio_message", "doc", "sticker", "graffiti", "link"] {
            let m = media_from_attachment(3, &att(kind));
            assert_eq!(m.index, 3, "{kind}");
            assert!(m.width.is_none() && m.height.is_none(), "{kind}");
            assert!(m.waveform.is_empty(), "{kind}");
        }
    }

    #[test]
    fn photo_takes_the_largest_size() {
        let mut a = att("photo");
        a.photo = Some(PhotoRaw {
            sizes: vec![size(75, 75), size(1920, 1080), size(130, 130)],
            ..PhotoRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!((m.width, m.height), (Some(1920), Some(1080)));
        assert!(m.has_thumb && m.has_full);
    }

    /// Видео: превью есть, файла НЕТ — и это ограничение платформы.
    #[test]
    fn video_carries_a_player_page_but_never_a_file() {
        let mut a = att("video");
        a.video = Some(VideoRaw {
            title: "  Ролик  ".into(),
            duration: 95,
            width: 1280,
            height: 720,
            image: vec![size(320, 180)],
            player: "https://vk.com/video_ext.php?oid=1".into(),
            ..VideoRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!(m.title.as_deref(), Some("Ролик"));
        assert_eq!(m.duration_secs, Some(95));
        assert_eq!((m.width, m.height), (Some(1280), Some(720)));
        assert!(m.has_thumb, "превью кэшируется");
        assert!(!m.has_full, "воспроизводимого файла у вложения video нет");
        assert_eq!(
            m.player_url.as_deref(),
            Some("https://vk.com/video_ext.php?oid=1")
        );
    }

    #[test]
    fn voice_carries_duration_and_capped_waveform() {
        let mut a = att("audio_message");
        a.audio_message = Some(AudioMessageRaw {
            duration: 7,
            waveform: (0..500).collect(),
            link_ogg: "https://psv4.example/voice.ogg".into(),
            ..AudioMessageRaw::default()
        });
        let m = media_from_attachment(1, &a);
        assert_eq!(m.duration_secs, Some(7));
        assert_eq!(m.waveform.len(), MAX_WAVEFORM);
        assert_eq!(m.mime.as_deref(), Some("audio/ogg"));
        assert!(m.has_full);
    }

    #[test]
    fn audio_joins_artist_and_title_and_reports_a_missing_file() {
        let mut a = att("audio");
        a.audio = Some(AudioRaw {
            artist: "Гражданская оборона".into(),
            title: "Всё идёт по плану".into(),
            duration: 180,
            url: String::new(),
            ..AudioRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!(
            m.title.as_deref(),
            Some("Гражданская оборона — Всё идёт по плану")
        );
        assert_eq!(m.duration_secs, Some(180));
        assert!(!m.has_full, "пустой url ⇒ файла для нас нет");
    }

    #[test]
    fn doc_reports_name_size_mime_and_preview() {
        let mut a = att("doc");
        a.doc = Some(DocRaw {
            title: "смета.pdf".into(),
            size: 4096,
            ext: "pdf".into(),
            url: "https://vk.com/doc1_2".into(),
            kind: 8,
            preview: Some(DocPreviewRaw {
                photo: Some(DocPreviewPhotoRaw {
                    sizes: vec![size(200, 300)],
                }),
                ..DocPreviewRaw::default()
            }),
            ..DocRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!(m.file_name.as_deref(), Some("смета.pdf"));
        assert_eq!(m.size, Some(4096));
        assert_eq!(m.mime.as_deref(), Some("application/pdf"));
        assert_eq!((m.width, m.height), (Some(200), Some(300)));
        assert!(m.has_thumb && m.has_full);
    }

    /// Расширения нет — mime берётся из числового типа VK (гифка).
    #[test]
    fn doc_falls_back_to_the_numeric_type_for_mime() {
        let mut a = att("doc");
        a.doc = Some(DocRaw {
            title: "безымянный".into(),
            kind: 3,
            ..DocRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!(m.mime.as_deref(), Some("image/gif"));
        assert_eq!(m.kind, "document", "вид карточки остаётся документом");
    }

    #[test]
    fn sticker_falls_back_to_images_with_background() {
        let mut a = att("sticker");
        a.sticker = Some(StickerRaw {
            images: Vec::new(),
            images_with_background: vec![StickerImageRaw {
                url: "https://vk.com/sticker.png".into(),
                width: 512,
                height: 512,
            }],
            ..StickerRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!((m.width, m.height), (Some(512), Some(512)));
        assert!(m.has_full);
    }

    /// Ссылка отдаёт заголовок, ПРЕВЬЮ и СВОЙ адрес.
    ///
    /// Превью здесь — регресс на живой дефект: карточка со ссылкой на
    /// `bitrix24.ru` показывала значок-заглушку вместо картинки, хотя размеры
    /// превью (1200×630) до фронта доезжали.
    #[test]
    fn link_carries_a_preview_and_its_own_address() {
        let mut a = att("link");
        a.link = Some(LinkRaw {
            url: "https://example.com/путь?token=1".into(),
            title: "Статья".into(),
            photo: Some(PhotoRaw {
                sizes: vec![size(1200, 630)],
                ..PhotoRaw::default()
            }),
            ..LinkRaw::default()
        });
        let m = media_from_attachment(0, &a);
        assert_eq!(m.title.as_deref(), Some("Статья"));
        assert_eq!((m.width, m.height), (Some(1200), Some(630)));
        assert!(m.has_thumb, "у ссылки с photo.sizes превью ЕСТЬ");
        assert!(!m.has_full, "файла у ссылки не бывает");
        assert_eq!(
            m.link_url.as_deref(),
            Some("https://example.com/путь?token=1"),
            "адрес собеседника доезжает до карточки"
        );
    }

    /// Схему адреса ссылки выбирает ОТПРАВИТЕЛЬ — значит, её надо проверять.
    #[test]
    fn a_link_with_a_hostile_scheme_arrives_without_an_address() {
        for hostile in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox",
            "file:///etc/passwd",
            // `vk:` — deep-link в приложение; на карточке ссылки ему делать
            // нечего, белый список тут уже `http`/`https`.
            "vk://vk.com/im",
            "",
        ] {
            let mut a = att("link");
            a.link = Some(LinkRaw {
                url: hostile.into(),
                title: "Кликни".into(),
                ..LinkRaw::default()
            });
            let m = media_from_attachment(0, &a);
            assert_eq!(m.link_url, None, "схема {hostile} обязана отсеяться");
        }
    }

    /// ГЕЙТ КЛАССА ДЕФЕКТОВ: обещание DTO и способность его выполнить —
    /// ОДНО И ТО ЖЕ ЗНАНИЕ.
    ///
    /// Дефект, ради которого тест написан: `has_thumb` считала ветка
    /// `from_link`, адрес превью искал `url_for`, и разъехаться они могли от
    /// правки любой из двух. Наружу это выглядит как карточка, которая просит
    /// превью и получает `404 no_media`.
    ///
    /// Таблица нарочно перебирает ВСЕ виды словаря, включая заведомо пустые:
    /// новый вид, добавленный только в одну из половин, немедленно провалит
    /// тест.
    ///
    /// МУТАЦИОННАЯ МИШЕНЬ: вернуть в любую ветку `from_*` собственное
    /// присваивание `has_thumb`/`has_full` ⇒ тест краснеет.
    #[test]
    fn promises_never_outrun_url_for() {
        use crate::media_cache::MediaKind;

        let mut cases: Vec<(&str, AttachmentRaw)> = Vec::new();

        let mut photo = att("photo");
        photo.photo = Some(PhotoRaw {
            sizes: vec![size(75, 75), size(1920, 1080)],
            ..PhotoRaw::default()
        });
        cases.push(("photo", photo));

        // Размеры есть, а адреса у них нет: раньше это обещало превью,
        // которого не существует.
        let mut photo_urlless = att("photo");
        photo_urlless.photo = Some(PhotoRaw {
            sizes: vec![PhotoSizeRaw {
                url: String::new(),
                width: 1000,
                height: 1000,
                kind: "w".into(),
            }],
            ..PhotoRaw::default()
        });
        cases.push(("photo без адресов", photo_urlless));

        let mut video = att("video");
        video.video = Some(VideoRaw {
            image: vec![size(320, 180)],
            player: "https://vk.com/video_ext.php?oid=1".into(),
            ..VideoRaw::default()
        });
        cases.push(("video", video));

        let mut video_first_frame = att("video");
        video_first_frame.video = Some(VideoRaw {
            first_frame: vec![size(320, 180)],
            ..VideoRaw::default()
        });
        cases.push(("video только first_frame", video_first_frame));

        let mut audio = att("audio");
        audio.audio = Some(AudioRaw {
            url: "https://cs.example/track.mp3".into(),
            ..AudioRaw::default()
        });
        cases.push(("audio", audio));
        cases.push(("audio без файла", {
            let mut a = att("audio");
            a.audio = Some(AudioRaw::default());
            a
        }));

        let mut voice = att("audio_message");
        voice.audio_message = Some(AudioMessageRaw {
            link_mp3: "https://psv4.example/voice.mp3".into(),
            ..AudioMessageRaw::default()
        });
        cases.push(("audio_message", voice));

        let mut doc = att("doc");
        doc.doc = Some(DocRaw {
            url: "https://vk.com/doc1_2".into(),
            title: "x.pdf".into(),
            preview: Some(DocPreviewRaw {
                photo: Some(DocPreviewPhotoRaw {
                    sizes: vec![size(200, 300)],
                }),
                ..DocPreviewRaw::default()
            }),
            ..DocRaw::default()
        });
        cases.push(("doc", doc));
        cases.push(("doc без превью", {
            let mut a = att("doc");
            a.doc = Some(DocRaw {
                url: "https://vk.com/doc1_3".into(),
                title: "y.bin".into(),
                ..DocRaw::default()
            });
            a
        }));

        let mut sticker = att("sticker");
        sticker.sticker = Some(StickerRaw {
            images_with_background: vec![StickerImageRaw {
                url: "https://vk.com/sticker.png".into(),
                width: 512,
                height: 512,
            }],
            ..StickerRaw::default()
        });
        cases.push(("sticker без прозрачных", sticker));

        let mut graffiti = att("graffiti");
        graffiti.graffiti = Some(GraffitiRaw {
            url: "https://vk.com/graffiti.png".into(),
            width: 300,
            height: 200,
            ..GraffitiRaw::default()
        });
        cases.push(("graffiti", graffiti));

        // ↓ Тот самый дефект: ссылка с превью.
        let mut link = att("link");
        link.link = Some(LinkRaw {
            url: "https://example.com/a".into(),
            title: "Заголовок".into(),
            photo: Some(PhotoRaw {
                sizes: vec![size(1200, 630)],
                ..PhotoRaw::default()
            }),
            ..LinkRaw::default()
        });
        cases.push(("link с превью", link));

        let mut link_bare = att("link");
        link_bare.link = Some(LinkRaw {
            url: "https://example.com/b".into(),
            title: "Без картинки".into(),
            ..LinkRaw::default()
        });
        cases.push(("link без превью", link_bare));

        // Остальные члены группы `other` — содержимое не разбирается вовсе.
        for kind in ["wall", "market", "article", "poll"] {
            cases.push((kind, att(kind)));
        }
        // И вид, о котором мы не знаем ничего.
        cases.push(("hologram", att("hologram")));
        // Вложение, у которого объявлен тип, но нет тела.
        cases.push(("photo без тела", att("photo")));

        for (label, a) in &cases {
            let m = media_from_attachment(0, a);
            assert_eq!(
                m.has_thumb,
                url_for(a, MediaKind::Thumb).is_some(),
                "{label}: has_thumb разошёлся с url_for(Thumb)"
            );
            assert_eq!(
                m.has_full,
                url_for(a, MediaKind::Full).is_some(),
                "{label}: has_full разошёлся с url_for(Full)"
            );
        }

        // Табличка не должна незаметно опустеть.
        assert!(cases.len() >= 18, "покрыто всего {} случаев", cases.len());
        // И перебирать она обязана ВЕСЬ словарь видов.
        let covered: std::collections::BTreeSet<&str> = cases
            .iter()
            .map(|(_, a)| a.normalized_kind())
            .collect();
        assert_eq!(
            covered,
            [
                KIND_AUDIO,
                KIND_DOCUMENT,
                KIND_GRAFFITI,
                KIND_OTHER,
                KIND_PHOTO,
                KIND_STICKER,
                KIND_UNSUPPORTED,
                KIND_VIDEO,
                KIND_VOICE,
            ]
            .into_iter()
            .collect(),
            "новый вид обязан попасть в эту таблицу"
        );
    }

    /// ГЛАВНЫЙ инвариант модуля: адрес ФАЙЛА на серверах VK не уезжает наружу
    /// ни у одного вида вложения. Проверяется по сериализованному JSON, потому
    /// что именно он уезжает в браузер.
    ///
    /// # Что этот тест защищает, а что — нет
    ///
    /// Он про CDN ВЛОЖЕНИЙ (`sun*.userapi.com`, `psv4.*`, `vk.com/doc…`) и про
    /// `access_key`. Такой адрес в DOM означал бы, что браузер пользователя
    /// сам стучится на серверы VK — с нашим Referer и с подписью в пути; плюс
    /// он протухает, то есть «работает» ровно до первой перезагрузки ленты.
    ///
    /// Он НЕ про две ссылки-исключения — [`MediaDto::player_url`] и
    /// [`MediaDto::link_url`]. Раньше тест запрещал и их («единственный URL —
    /// player_url»), и из-за этого карточка ссылки не умела открыть ссылку.
    /// Разница по существу:
    ///
    /// | | CDN вложения | `player_url` / `link_url` |
    /// |---|---|---|
    /// | чей адрес | инфраструктуры VK | страницы-плеера / собеседника |
    /// | подписан и протухает | да | нет |
    /// | пользователь хотел туда пойти | нет | ДА, ровно за этим и кликает |
    /// | есть ли альтернатива | есть, наш прокси | нет: файла не существует |
    ///
    /// Ослабление точечное: `player_url` и `link_url` — единственные поля-URL
    /// в [`MediaDto`], оба проходят [`http_url`], и тест ниже перечисляет их
    /// явно. Любое ТРЕТЬЕ поле с адресом провалит его.
    #[test]
    fn attachment_cdn_urls_never_leak() {
        let mut photo = att("photo");
        photo.photo = Some(PhotoRaw {
            sizes: vec![size(1000, 1000)],
            access_key: "SECRETKEY".into(),
            ..PhotoRaw::default()
        });
        let mut doc = att("doc");
        doc.doc = Some(DocRaw {
            url: "https://vk.com/doc-with-signature".into(),
            access_key: "SECRETKEY".into(),
            title: "x.bin".into(),
            ..DocRaw::default()
        });
        let mut voice = att("audio_message");
        voice.audio_message = Some(AudioMessageRaw {
            link_ogg: "https://psv4.example/voice.ogg".into(),
            access_key: "SECRETKEY".into(),
            ..AudioMessageRaw::default()
        });
        let mut graffiti = att("graffiti");
        graffiti.graffiti = Some(GraffitiRaw {
            url: "https://vk.com/graffiti.png".into(),
            access_key: "SECRETKEY".into(),
            ..GraffitiRaw::default()
        });
        // Видео и ссылка несут КАЖДОЕ по два адреса: разрешённый (плеер /
        // сама ссылка) и запрещённый (кадр превью на CDN). Тест обязан
        // пропустить первый и поймать второй — иначе он проверяет не то.
        let mut video = att("video");
        video.video = Some(VideoRaw {
            player: "https://vk.com/video_ext.php?oid=1".into(),
            image: vec![size(640, 360)],
            access_key: "SECRETKEY".into(),
            ..VideoRaw::default()
        });
        let mut link = att("link");
        link.link = Some(LinkRaw {
            url: "https://psph.example/crm/contact/details/27307/".into(),
            title: "Войти".into(),
            photo: Some(PhotoRaw {
                sizes: vec![size(1200, 630)],
                access_key: "SECRETKEY".into(),
                ..PhotoRaw::default()
            }),
            ..LinkRaw::default()
        });

        let list = media_list(&[photo, doc, voice, graffiti, video, link]);
        let json = serde_json::to_string(&list).unwrap();
        for forbidden in [
            // ↓ `size()` кладёт адрес именно сюда — это и есть «CDN вложений»
            //   для фотографии, кадра видео и превью ссылки разом.
            "sun.example",
            "psv4.example",
            "vk.com/doc",
            "vk.com/graffiti",
            "SECRETKEY",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} утёк в DTO: {json}");
        }

        // Обе разрешённые ссылки на месте — иначе «утечки нет» достигалось бы
        // тем, что наружу не едет вообще ничего.
        assert_eq!(
            list[4].player_url.as_deref(),
            Some("https://vk.com/video_ext.php?oid=1")
        );
        assert_eq!(
            list[5].link_url.as_deref(),
            Some("https://psph.example/crm/contact/details/27307/")
        );

        // И ГЛАВНОЕ: полей с адресом ровно два. Третье поле-URL, добавленное
        // будущей правкой, попадёт сюда и провалит тест — даже если его
        // содержимое случайно не совпало ни с одной строкой выше.
        let obj: serde_json::Value = serde_json::to_value(&list[5]).unwrap();
        let url_fields: Vec<String> = obj
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| {
                v.as_str()
                    .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://"))
            })
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(
            url_fields,
            vec!["link_url".to_string()],
            "у ссылки наружу едет ровно один адрес — её собственный"
        );
    }

    /// Первое вложение — то, что рисуется значком в цитате и пересланном.
    #[test]
    fn first_kind_reads_the_leading_attachment() {
        assert_eq!(first_kind(&[]), None);
        assert_eq!(
            first_kind(&[att("audio_message"), att("photo")]).as_deref(),
            Some("voice")
        );
        assert_eq!(first_kind(&[att("hologram")]).as_deref(), Some("unsupported"));
    }

    // -----------------------------------------------------------------
    // pick_size — ТАБЛИЧНО.
    // -----------------------------------------------------------------

    fn cand(letter: char, w: i32, h: i32) -> SizeCandidate {
        SizeCandidate {
            letter,
            width: w,
            height: h,
        }
    }

    /// Таблица выбора размера: вход → (thumb, full).
    #[test]
    fn pick_size_table() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        /// Строка таблицы: пояснение, набор размеров, ожидаемые thumb и full.
        type Row = (&'static str, Vec<SizeCandidate>, Option<usize>, Option<usize>);
        let table: Vec<Row> = vec![
            ("пустой массив", vec![], None, None),
            (
                "один размер — он же и превью, и оригинал",
                vec![cand('x', 604, 403)],
                Some(0),
                Some(0),
            ),
            (
                "обычная линейка: превью — первый, кто дотянул до 200",
                vec![
                    cand('s', 75, 50),
                    cand('m', 130, 87),
                    cand('x', 604, 403),
                    cand('w', 2560, 1707),
                ],
                Some(2),
                Some(3),
            ),
            (
                "порядок вперемешку значения не меняет",
                vec![
                    cand('w', 2560, 1707),
                    cand('s', 75, 50),
                    cand('x', 604, 403),
                    cand('m', 130, 87),
                ],
                Some(2),
                Some(0),
            ),
            (
                "все мельче порога — берём самый крупный",
                vec![cand('s', 75, 50), cand('m', 130, 87)],
                Some(1),
                Some(1),
            ),
            (
                "ровно на границе THUMB_MIN_PX — уже годится",
                vec![cand('s', 75, 50), cand('p', 200, 133), cand('x', 604, 403)],
                Some(1),
                Some(2),
            ),
            (
                "на единицу ниже границы — не годится",
                vec![cand('s', 75, 50), cand('p', 199, 133), cand('x', 604, 403)],
                Some(2),
                Some(2),
            ),
            (
                "дубликаты размеров: побеждает первый по порядку",
                vec![cand('x', 604, 403), cand('x', 604, 403), cand('s', 75, 50)],
                Some(0),
                Some(0),
            ),
            (
                "все нули — в дело идёт буквенная линейка",
                vec![cand('s', 0, 0), cand('m', 0, 0), cand('x', 0, 0)],
                Some(2),
                Some(2),
            ),
            (
                "все нули и все буквы незнакомые — первый попавшийся",
                vec![cand('\0', 0, 0), cand('\0', 0, 0)],
                Some(0),
                Some(0),
            ),
            (
                "портрет: считаем по БОЛЬШЕЙ стороне",
                vec![cand('m', 87, 130), cand('x', 403, 604)],
                Some(1),
                Some(1),
            ),
            (
                // ↓ МУТАЦИОННАЯ МИШЕНЬ: выбор по букве вместо чисел даст здесь
                //   другой ответ. Набор букв VK исторически менялся, и
                //   рассогласование буквы с геометрией — не гипотеза.
                "буква спорит с числами — верим числам",
                vec![cand('w', 75, 50), cand('s', 1920, 1080)],
                Some(1),
                Some(1),
            ),
        ];
        for (label, sizes, thumb, full) in table {
            assert_eq!(pick_size(&sizes, Thumb), thumb, "thumb: {label}");
            assert_eq!(pick_size(&sizes, Full), full, "full: {label}");
        }
    }

    /// Полный размер — всегда максимум, и он не зависит от вида карточки.
    #[test]
    fn pick_size_full_is_always_the_largest() {
        use crate::media_cache::MediaKind::Full;
        let sizes = vec![
            cand('s', 75, 50),
            cand('z', 1080, 720),
            cand('m', 130, 87),
            cand('w', 2560, 1707),
        ];
        assert_eq!(pick_size(&sizes, Full), Some(3));
    }

    // -----------------------------------------------------------------
    // url_for — по видам вложений.
    // -----------------------------------------------------------------

    fn size_at(url: &str, letter: &str, w: i64, h: i64) -> PhotoSizeRaw {
        PhotoSizeRaw {
            url: url.into(),
            width: w,
            height: h,
            kind: letter.into(),
        }
    }

    #[test]
    fn url_for_photo_picks_thumb_and_full_apart() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        let mut a = att("photo");
        a.photo = Some(PhotoRaw {
            sizes: vec![
                size_at("https://sun.example/s.jpg", "s", 75, 50),
                size_at("https://sun.example/x.jpg", "x", 604, 403),
                size_at("https://sun.example/w.jpg", "w", 2560, 1707),
            ],
            ..PhotoRaw::default()
        });
        assert_eq!(
            url_for(&a, Thumb).as_deref(),
            Some("https://sun.example/x.jpg")
        );
        assert_eq!(
            url_for(&a, Full).as_deref(),
            Some("https://sun.example/w.jpg")
        );
    }

    /// Видео: превью есть, файла НЕТ — и это ограничение платформы.
    #[test]
    fn url_for_video_has_a_preview_but_never_a_file() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        let mut a = att("video");
        a.video = Some(VideoRaw {
            image: vec![size_at("https://sun.example/frame.jpg", "l", 320, 180)],
            player: "https://vk.com/video_ext.php?oid=1".into(),
            ..VideoRaw::default()
        });
        assert_eq!(
            url_for(&a, Thumb).as_deref(),
            Some("https://sun.example/frame.jpg")
        );
        assert_eq!(url_for(&a, Full), None, "воспроизводимого файла нет");

        // Пустой image ⇒ берём первый кадр.
        a.video = Some(VideoRaw {
            image: Vec::new(),
            first_frame: vec![size_at("https://sun.example/ff.jpg", "l", 320, 180)],
            ..VideoRaw::default()
        });
        assert_eq!(
            url_for(&a, Thumb).as_deref(),
            Some("https://sun.example/ff.jpg")
        );
    }

    #[test]
    fn url_for_voice_prefers_ogg_and_has_no_thumb() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        let mut a = att("audio_message");
        a.audio_message = Some(AudioMessageRaw {
            link_ogg: "https://psv4.example/voice.ogg".into(),
            link_mp3: "https://psv4.example/voice.mp3".into(),
            ..AudioMessageRaw::default()
        });
        assert_eq!(
            url_for(&a, Full).as_deref(),
            Some("https://psv4.example/voice.ogg")
        );
        assert_eq!(url_for(&a, Thumb), None, "у голосового превью не бывает");

        // Без ogg — падаем на mp3.
        a.audio_message = Some(AudioMessageRaw {
            link_mp3: "https://psv4.example/voice.mp3".into(),
            ..AudioMessageRaw::default()
        });
        assert_eq!(
            url_for(&a, Full).as_deref(),
            Some("https://psv4.example/voice.mp3")
        );
    }

    #[test]
    fn url_for_doc_takes_preview_for_thumb_and_url_for_full() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        let mut a = att("doc");
        a.doc = Some(DocRaw {
            url: "https://vk.com/doc1_2?key=sig".into(),
            preview: Some(DocPreviewRaw {
                photo: Some(DocPreviewPhotoRaw {
                    sizes: vec![
                        size_at("https://sun.example/p-s.jpg", "s", 75, 100),
                        size_at("https://sun.example/p-m.jpg", "m", 300, 400),
                    ],
                }),
                ..DocPreviewRaw::default()
            }),
            ..DocRaw::default()
        });
        assert_eq!(
            url_for(&a, Full).as_deref(),
            Some("https://vk.com/doc1_2?key=sig")
        );
        assert_eq!(
            url_for(&a, Thumb).as_deref(),
            Some("https://sun.example/p-m.jpg")
        );

        // Документ без превью: превью нет, файл есть.
        a.doc = Some(DocRaw {
            url: "https://vk.com/doc1_2".into(),
            ..DocRaw::default()
        });
        assert_eq!(url_for(&a, Thumb), None);
        assert!(url_for(&a, Full).is_some());
    }

    #[test]
    fn url_for_sticker_prefers_background_for_full() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        let mut a = att("sticker");
        a.sticker = Some(StickerRaw {
            images: vec![
                StickerImageRaw {
                    url: "https://vk.com/st-128.png".into(),
                    width: 128,
                    height: 128,
                },
                StickerImageRaw {
                    url: "https://vk.com/st-512.png".into(),
                    width: 512,
                    height: 512,
                },
            ],
            images_with_background: vec![StickerImageRaw {
                url: "https://vk.com/st-bg-512.png".into(),
                width: 512,
                height: 512,
            }],
            ..StickerRaw::default()
        });
        assert_eq!(
            url_for(&a, Thumb).as_deref(),
            Some("https://vk.com/st-512.png"),
            "превью — минимальное, дотянувшее до порога"
        );
        assert_eq!(
            url_for(&a, Full).as_deref(),
            Some("https://vk.com/st-bg-512.png"),
            "оригинал — с фоном: прозрачный PNG на светлой теме не виден"
        );
    }

    #[test]
    fn url_for_graffiti_and_audio_and_other() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        let mut g = att("graffiti");
        g.graffiti = Some(GraffitiRaw {
            url: "https://vk.com/graffiti.png".into(),
            width: 400,
            height: 300,
            ..GraffitiRaw::default()
        });
        assert_eq!(
            url_for(&g, Thumb).as_deref(),
            Some("https://vk.com/graffiti.png")
        );
        assert_eq!(
            url_for(&g, Full).as_deref(),
            Some("https://vk.com/graffiti.png")
        );

        let mut audio = att("audio");
        audio.audio = Some(AudioRaw {
            url: "https://cs.example/track.mp3".into(),
            ..AudioRaw::default()
        });
        assert_eq!(
            url_for(&audio, Full).as_deref(),
            Some("https://cs.example/track.mp3")
        );
        assert_eq!(url_for(&audio, Thumb), None);
        // Правообладатель закрыл трек — ссылки нет вовсе.
        audio.audio = Some(AudioRaw::default());
        assert_eq!(url_for(&audio, Full), None);

        let mut link = att("link");
        link.link = Some(LinkRaw {
            url: "https://example.com/article".into(),
            photo: Some(PhotoRaw {
                sizes: vec![size_at("https://sun.example/link.jpg", "m", 300, 200)],
                ..PhotoRaw::default()
            }),
            ..LinkRaw::default()
        });
        assert_eq!(
            url_for(&link, Thumb).as_deref(),
            Some("https://sun.example/link.jpg"),
            "превью ссылки кэшируется"
        );
        assert_eq!(
            url_for(&link, Full),
            None,
            "а сам адрес ссылки файлом не является"
        );
    }

    /// Вложение без единого необязательного поля и вид из будущего не должны
    /// давать ни паники, ни адреса.
    #[test]
    fn url_for_reports_absence_instead_of_guessing() {
        use crate::media_cache::MediaKind::{Full, Thumb};
        for kind in [
            "photo",
            "video",
            "audio",
            "audio_message",
            "doc",
            "sticker",
            "graffiti",
            "link",
            "poll",
            "hologram",
            "",
        ] {
            let a = att(kind);
            assert_eq!(url_for(&a, Thumb), None, "{kind} thumb");
            assert_eq!(url_for(&a, Full), None, "{kind} full");
        }
    }

    /// Схемы, кроме http/https, наружу не проходят: адрес уезжает прямо в
    /// `get_stream`, и `file://` там означал бы чтение локальных файлов.
    #[test]
    fn url_for_accepts_only_http_schemes() {
        use crate::media_cache::MediaKind::Full;
        for bad in [
            "file:///etc/passwd",
            "data:image/png;base64,AAAA",
            "ftp://example.com/x",
            "/etc/passwd",
            "",
            "   ",
        ] {
            let mut a = att("doc");
            a.doc = Some(DocRaw {
                url: bad.into(),
                ..DocRaw::default()
            });
            assert_eq!(url_for(&a, Full), None, "схема {bad:?} не должна проходить");
        }
    }

    /// Страница из фикстуры разбирается целиком, вместе с фото и голосовым.
    #[test]
    fn history_fixture_attachments_project_whole() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/get_history.json")).unwrap();
        let page: ItemsRaw<MessageRaw> = serde_json::from_value(v["response"].clone()).unwrap();
        let media = page
            .items
            .iter()
            .find(|m| !m.attachments.is_empty())
            .unwrap();
        let list = media_list(&media.attachments);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].kind, "photo");
        assert_eq!((list[0].width, list[0].height), (Some(1080), Some(1080)));
        assert_eq!(list[1].kind, "voice");
        assert_eq!(list[1].index, 1);
        assert_eq!(list[1].waveform, vec![3, 9, 21, 14, 5]);
    }
}
