//! Приём файла от браузера (`POST /api/bitrix/upload`) и одношаговая заливка
//! его в портал (`im.v2.File.upload`, Р7).
//!
//! | Метод | Путь | Что делает |
//! |-------|------|------------|
//! | POST | `/api/bitrix/upload` | принимает сырые байты файла в staging и выдаёт `upload_id` |
//!
//! Второй половины у роутера здесь нет намеренно: файл уезжает в портал внутри
//! `POST /api/bitrix/dialogs/:id/messages`
//! ([`crate::routes::dialogs::post_message`]) — тем же роутом, что и текст.
//! Причина в самом методе портала: `im.v2.File.upload` — это НЕ «залить файл»,
//! а «залить файл И отправить сообщение» одним вызовом (см. ниже). Отдельный
//! роут `/files` означал бы второй путь отправки сообщения с собственной
//! копией дедупа, рассылки кадра `message_new` и перечитывания строки.
//!
//! # Почему приём и отправка разнесены на два запроса
//!
//! Пользователь вправе выбрать файл и передумать, куда его слать (или не
//! слать вовсе). Поэтому `POST /api/bitrix/upload` кладёт байты на НАШ диск и
//! не делает ни одного обращения к порталу, а `upload_id` — это идентификатор
//! НАШЕГО полуфабриката, а не адрес чего-либо в Bitrix24.
//!
//! # Р7: одношаговая заливка base64 и её цена
//!
//! Классическая связка Диска (`im.disk.folder.get` → `disk.folder.uploadfile`
//! → `im.disk.file.commit`) требует scope `disk`, которого у входящего вебхука
//! НЕТ (живая проверка: `insufficient_scope`). Единственный доступный путь —
//! `im.v2.File.upload` со scope `im`:
//!
//! ```json
//! {"dialogId":"chat27",
//!  "fields":{"name":"probe.txt","content":"<base64>","message":"подпись"}}
//! → {"result":{"file":{…},"messageId":97,"chatId":27,"dialogId":"chat27"}}
//! ```
//!
//! Отсюда три следствия, которые обязаны быть на виду:
//!
//! 1. **`im.message.add` после заливки НЕ вызывается.** Портал уже отправил
//!    сообщение сам и вернул его `messageId`; второй вызов дал бы ВТОРУЮ
//!    строку в чате — пустую или с дублем подписи;
//! 2. **тело запроса — это файл в base64**, то есть примерно 1.33 размера
//!    файла в памяти плюс столько же на сериализованное JSON-тело. Ровно
//!    поэтому дефолтный потолок [`crate::config::DEFAULT_MAX_UPLOAD_BYTES`] —
//!    25 MiB, а не платформенные 100 MiB: сотня мегабайт превратилась бы в
//!    ~266 МБ пиковой памяти на ОДНУ отправку;
//! 3. **прогресса заливки не существует.** Один POST без обратной связи — ни
//!    процентов, ни этапов. Отсюда индетерминированная фаза `portal` (Р8).
//!
//! # Р9: ни заливка, ни отправка не ретраятся
//!
//! `im.v2.File.upload` стоит в [`crate::api::NO_RETRY_METHODS`], и запрет не
//! обходится вложением в `batch` (см. `is_retryable`). Дедуп-идентификатора у
//! портала нет, поэтому повтор означал бы второй файл на Диске и вторую строку
//! в чате. Транспортная ошибка отдаётся как `503`, а решение о повторе
//! принимает пользователь кнопкой на строке.
//!
//! # Порядок проверок приёма (важен!)
//!
//! 1. **авторизация** — `409 not_authorized` ДО чтения тела: иначе клиент зря
//!    зальёт мегабайты и только потом узнает, что портал не подключён;
//! 2. **`Content-Length`** — без него `411 Length Required`: неизвестен размер
//!    ⇒ нельзя ни проверить лимит заранее, ни показать проценты;
//! 3. **лимит** `Content-Length > cfg.max_upload_bytes` ⇒ `413`, и **тело не
//!    читается вообще** — иначе гигабайтный файл был бы принят на диск ровно
//!    затем, чтобы его отвергнуть;
//! 4. **имя файла** — заголовка [`FILE_NAME_HEADER`] нет либо после
//!    percent-decode пусто ⇒ `400`;
//! 5. и только теперь тело стримится на диск.
//!
//! Проверки связи (`503`) на этой лестнице СОЗНАТЕЛЬНО нет: staging — целиком
//! локальная операция, а сетевой отказ штатно отдаётся там, где реально идёт
//! сеть, — в `POST /messages`. Так короткий сетевой чих не заставляет заново
//! выбирать файл.
//!
//! # Жизненный цикл staging-файла
//!
//! `<cache_dir>/uploads/<upload_id>.part` живёт до одного из трёх событий:
//!
//! - успешная (или неуспешная) отправка — [`send_staged_file`] потребляет
//!   запись и убирает файл;
//! - истечение [`crate::state::UPLOAD_TTL_SECS`] — файл выбрали и передумали,
//!   его подберёт [`uploads_gc_loop`];
//! - рестарт процесса — [`cleanup_uploads_dir`] на старте: реестр живёт в
//!   памяти, значит после рестарта ни один `.part` уже никому не принадлежит,
//!   а место занимает вполне реальное.
//!
//! Каталог `uploads/` лежит ВНУТРИ `cache_dir`, но `media_cache` к нему
//! отношения не имеет: `begin_staging`/`commit` кладут результат в `media/`
//! под ключом кэша, а здесь результат — не кэш, а полуфабрикат, который
//! расходуется первой же отправкой.
//!
//! # Про `clippy::result_large_err`
//!
//! Та же причина, что в [`crate::routes::dialogs`]: [`ApiError`] несёт
//! контракт `flood_wait` и потому крупнее указателя. `Box` ради линта добавил
//! бы аллокацию на КАЖДЫЙ отказ, а отказ здесь штатный ответ (409 без
//! подключения приходит на каждый запрос закрытой вкладки).
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::api::B24Client;
use crate::progress::{ProgressReader, UploadReporter};
use crate::routes::dialogs::{map_portal_error, require_authorized};
use crate::routes::error::ApiError;
use crate::state::{B24State, UploadEntry, UPLOAD_TTL_SECS};

/// Подкаталог staging-файлов внутри `cache_dir`.
pub const UPLOADS_SUBDIR: &str = "uploads";

/// Расширение staging-файла (совпадает с привычным «недокачано»).
pub const STAGING_EXT: &str = "part";

/// Заголовок с именем файла (percent-encoded значение `encodeURIComponent`).
///
/// Имя едет заголовком, а не в теле, потому что тело — это и есть файл
/// (`xhr.send(file)` на фронте, `body.into_data_stream()` здесь). Заголовки
/// HTTP — ASCII, а имена файлов бывают любые, отсюда percent-encoding.
pub const FILE_NAME_HEADER: &str = "x-forge-file-name";

/// Максимальная длина имени файла в байтах.
///
/// 255 — предел компонента пути почти во всех ФС; более длинные имена портал
/// всё равно обрезает.
pub const MAX_FILE_NAME_LEN: usize = 255;

/// Имя, которым заменяется пустое или непригодное.
pub const FALLBACK_FILE_NAME: &str = "file";

/// MIME по умолчанию, когда расширение ничего не подсказало.
pub const DEFAULT_MIME: &str = "application/octet-stream";

/// Как часто GC-воркер проверяет реестр загрузок.
pub const UPLOAD_GC_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Размер куска, которым staging-файл читается при сборке base64.
///
/// **Обязан быть кратен трём.** base64 кодирует ровно по три байта в четыре
/// символа, и кусок некратной длины дал бы `=`-набивку ПОСЕРЕДИНЕ строки —
/// портал получил бы битое содержимое, а мы не получили бы ни одной ошибки на
/// нашей стороне. 48 KiB = 49 152 = 3 × 16 384.
pub const B64_CHUNK: usize = 48 * 1024;

/// Ответ `POST /api/bitrix/upload`.
///
/// Форма ДОСЛОВНО совпадает с телеграмной и вэкашной: фронт трёх панелей
/// читает один и тот же набор полей, и расхождение здесь означало бы три
/// разошедшихся `upload.js` вместо одного построчного диффа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadResp {
    /// Идентификатор загрузки — им сообщение ссылается на файл.
    ///
    /// Это НАШ идентификатор, а не адрес чего-либо в Bitrix24 (см. шапку).
    pub upload_id: String,
    /// Санитизированное имя файла (именно оно уйдёт в портал).
    pub file_name: String,
    /// Размер в байтах (совпадает с `Content-Length`).
    pub size: u64,
    /// MIME, угаданный по расширению: по нему фронт подписывает карточку.
    pub mime: String,
}

/// Роутер загрузки.
///
/// `max_upload_bytes` приходит из конфига, поэтому лимит тела навешивается
/// ТОЧЕЧНО на этот роут: глобальный `DefaultBodyLimit` приложения (2 МиБ по
/// умолчанию в axum) остаётся как есть, иначе любой другой JSON-роут хоста
/// внезапно начал бы принимать десятки мегабайт.
///
/// Слой — вторая линия обороны, а не единственная: экстрактор [`Body`] тело не
/// лимитирует (лимит применяют только буферизующие экстракторы — `Bytes`,
/// `String`, `Json`), поэтому фактический предел проверяется по
/// `Content-Length` ДО чтения и по счётчику записанных байт во время чтения.
pub fn router(max_upload_bytes: u64) -> Router<Arc<B24State>> {
    let limit = usize::try_from(max_upload_bytes).unwrap_or(usize::MAX);
    Router::new().route(
        "/api/bitrix/upload",
        post(post_upload).layer(DefaultBodyLimit::max(limit)),
    )
}

/// Каталог staging-файлов.
pub fn uploads_dir(cfg: &crate::config::BitrixConfig) -> PathBuf {
    cfg.cache_dir.join(UPLOADS_SUBDIR)
}

/// Путь staging-файла конкретной загрузки.
///
/// Имя целиком определяется `upload_id` (UUID v4), а НЕ именем файла от
/// клиента: так в ФС не попадает ни один байт пользовательского ввода, и path
/// traversal через [`FILE_NAME_HEADER`] невозможен по построению, а не по
/// качеству санитизации.
pub fn staging_path(cfg: &crate::config::BitrixConfig, upload_id: &str) -> PathBuf {
    uploads_dir(cfg).join(format!("{upload_id}.{STAGING_EXT}"))
}

/// Текущее unix-время в секундах.
fn unix_now() -> i64 {
    crate::dto::now_unix()
}

/// Разбирает `Content-Length`.
///
/// Отсутствие заголовка и мусор в нём — РАЗНЫЕ случаи: первое даёт `411`
/// («пришли размер»), второе `400` («размер нечитаем»). Оба возвращаются как
/// `None` только потому, что вызывающий всё равно обязан отказать; различие
/// сделано порядком проверок в [`post_upload`].
pub fn parse_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Декодирует percent-encoding (`encodeURIComponent`) в UTF-8-строку.
///
/// Своя реализация вместо крейта `percent-encoding`: нужна одна функция на
/// двадцать строк, а любая новая зависимость в этом плагине проходит через
/// гейт дерева (`cargo tree -i openssl-sys`, `cargo tree -d`). Невалидные
/// байты и незакрытые `%` ошибкой не считаются — они остаются как есть, потому
/// что имя файла не должно ронять загрузку.
pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Приводит имя файла к безопасному виду.
///
/// Шаги (порядок важен):
///
/// 1. только последний сегмент пути — и по `/`, и по `\` (Windows-клиенты
///    присылают `C:\dir\file.jpg`, а `../../evil.sh` не должен пролезть даже в
///    имя, которое увидит получатель в чате);
/// 2. управляющие символы (в т.ч. `\r\n`, которыми можно было бы попытаться
///    инжектировать заголовок) выбрасываются;
/// 3. обрезка по краям и по [`MAX_FILE_NAME_LEN`] байт с сохранением границы
///    UTF-8 — полуобрезанный символ сделал бы имя нечитаемым;
/// 4. `.`, `..` и пустая строка ⇒ [`FALLBACK_FILE_NAME`].
///
/// Возвращает `None` ТОЛЬКО для входа, который после decode пуст, — это сигнал
/// «клиент не передал имя» (`400`), а не «подставь заглушку».
pub fn sanitize_upload_name(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    let tail = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = tail.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    let capped = truncate_utf8(trimmed, MAX_FILE_NAME_LEN);
    let name = capped.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Some(FALLBACK_FILE_NAME.to_string());
    }
    Some(name.to_string())
}

/// Обрезает строку до `max` байт, не разрывая символ.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// MIME по расширению имени.
///
/// Только по имени и никогда по содержимому: sniffing потребовал бы читать
/// начало файла (а он ещё не пришёл) и всё равно был бы менее точен, чем
/// расширение. Порталу MIME не отправляется вовсе — `im.v2.File.upload`
/// принимает только `name` и `content`, тип он выводит сам. Значение нужно
/// фронту: по нему рисуется значок карточки и решается, показывать ли
/// миниатюру.
pub fn guess_mime(file_name: &str) -> String {
    mime_guess::from_path(file_name)
        .first()
        .map(|m| m.essence_str().to_string())
        .unwrap_or_else(|| DEFAULT_MIME.to_string())
}

/// Удаляет staging-файл, если загрузка не дошла до реестра.
///
/// Обрыв соединения (axum дропает future хендлера), несовпадение размера,
/// ошибка записи, паника — после любого из них `.part`-файла оставаться не
/// должно. Приём тот же, что у [`crate::routes::media`]: снимать мусор на
/// `Drop`, а не в каждой ветке выхода, потому что веток выхода больше, чем
/// кажется.
struct StagingGuard {
    /// Путь; `None` ⇒ файл уже принадлежит реестру.
    path: Option<PathBuf>,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Загрузка удалась — файл больше не наш.
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            // Синхронное удаление: `Drop` не умеет await, а `remove_file` —
            // один быстрый syscall (файл лежит на локальном диске).
            let _ = std::fs::remove_file(p);
        }
    }
}

/// `POST /api/bitrix/upload` — принять файл в staging.
///
/// Лестница проверок описана в шапке модуля и НАРУШЕНА БЫТЬ НЕ МОЖЕТ:
/// перестановка шага 3 после чтения тела превратила бы отказ по размеру в
/// приём файла на диск ради отказа.
///
/// # Errors
///
/// - `409 not_authorized` — портал не подключён (тело НЕ читается);
/// - `411 length_required` — нет `Content-Length`;
/// - `413 too_large` — размер больше `cfg.max_upload_bytes` (тело НЕ читается);
/// - `400 invalid_input` — нет имени файла, мусорный `Content-Length` либо
///   фактический размер тела не совпал с заявленным;
/// - `503 not_connected` — ошибка файловой системы: для клиента это
///   «повтори», диск и права — не его вина и не его дело.
pub async fn post_upload(
    State(state): State<Arc<B24State>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<UploadResp>, ApiError> {
    // (1) Подключение — до чтения тела.
    require_authorized(&state).await?;

    // (2) Размер обязателен.
    if !headers.contains_key(axum::http::header::CONTENT_LENGTH) {
        return Err(ApiError::length_required(
            "Content-Length is required: without it the upload cannot be size-checked or shown as progress",
        ));
    }
    let declared = parse_content_length(&headers).ok_or_else(|| {
        ApiError::bad_input("Content-Length must be a non-negative integer number of bytes")
    })?;

    // (3) Лимит — ДО единого прочитанного байта (Р7).
    let max = state.config.max_upload_bytes;
    if declared > max {
        return Err(ApiError::too_large(format!(
            "file is {declared} bytes, which exceeds the limit of {max} bytes \
             (raise max_upload_bytes in bitrix.toml to allow it; the portal's own ceiling is 100 MiB)"
        )));
    }

    // (4) Имя файла.
    let raw_name = headers
        .get(FILE_NAME_HEADER)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .ok_or_else(|| {
            ApiError::bad_input(format!(
                "header {FILE_NAME_HEADER} is required (percent-encoded original file name)"
            ))
        })?;
    let file_name = sanitize_upload_name(&percent_decode(&raw_name)).ok_or_else(|| {
        ApiError::bad_input(format!("header {FILE_NAME_HEADER} decodes to an empty name"))
    })?;
    let mime = guess_mime(&file_name);

    // (5) Тело — на диск.
    let upload_id = uuid::Uuid::new_v4().to_string();
    let dir = uploads_dir(&state.config);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        ApiError::not_connected(format!("could not create upload staging directory: {e}"))
    })?;
    let path = staging_path(&state.config, &upload_id);
    let mut guard = StagingGuard::new(path.clone());

    // Фаза `client` прогресса (Р8): проценты здесь ТОЧНЫЕ — знаменатель
    // известен из `Content-Length`.
    //
    // Инициатору отправки эти кадры не нужны: он видит собственный
    // `xhr.upload.onprogress` мгновенно и без сокета. Нужны они ВТОРОЙ
    // открытой вкладке devforge — она обязана видеть, что панель занята
    // отправкой файла, иначе поведение «зависло на минуту» ничем не
    // объясняется. Идентификатор кадра — `upload_id`, поэтому две
    // параллельные отправки не путаются.
    let mut reporter = UploadReporter::new(state.clone(), upload_id.clone(), declared);
    let written = stream_to_file(body, &path, declared, &mut reporter).await?;
    if written != declared {
        return Err(ApiError::bad_input(format!(
            "body ended after {written} bytes, but Content-Length promised {declared}"
        )));
    }

    let entry = UploadEntry {
        path: path.clone(),
        file_name: file_name.clone(),
        size: declared,
        mime: mime.clone(),
        created_at: unix_now(),
    };
    // Ленивая подчистка на каждой новой загрузке: даже если GC-воркер почему-то
    // не запущен (тесты, урезанная сборка), реестр не растёт без предела.
    evict_stale(&state).await;
    state.insert_upload(upload_id.clone(), entry).await;
    guard.disarm();

    tracing::info!(
        target: "forge_bitrix",
        upload_id = %upload_id,
        size = declared,
        mime = %mime,
        "forge-bitrix: file staged for sending"
    );
    Ok(Json(UploadResp {
        upload_id,
        file_name,
        size: declared,
        mime,
    }))
}

/// Стримит тело запроса в файл и возвращает число записанных байт.
///
/// Буферизации целиком в памяти нет ни на одном шаге: чанк пришёл — чанк
/// записан. Поэтому приём 25 МиБ не двигает RSS процесса (в отличие от заливки
/// в портал, где base64 неизбежно живёт в памяти целиком, Р7).
///
/// `limit` — уже проверенный `Content-Length`. Он же второй предохранитель:
/// клиент, приславший больше, чем обещал, обрывается на первом лишнем байте, а
/// не после того, как заполнит диск.
///
/// Финальный кадр фазы `client` уходит В ЛЮБОМ ИСХОДЕ, включая обрыв: полоса,
/// оставшаяся на 97 %, читается пользователем как «зависло», и разницы между
/// «зависло» и «оборвалось» он не увидит.
async fn stream_to_file(
    body: Body,
    path: &Path,
    limit: u64,
    reporter: &mut UploadReporter,
) -> Result<u64, ApiError> {
    let mut written: u64 = 0;
    let outcome = stream_body(body, path, limit, reporter, &mut written).await;
    reporter.finish_client(written);
    outcome.map(|()| written)
}

/// Внутренность [`stream_to_file`]: всё, что может отказать.
async fn stream_body(
    body: Body,
    path: &Path,
    limit: u64,
    reporter: &mut UploadReporter,
    written: &mut u64,
) -> Result<(), ApiError> {
    let file = tokio::fs::File::create(path)
        .await
        .map_err(|e| ApiError::not_connected(format!("could not create staging file: {e}")))?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut stream = body.into_data_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            ApiError::bad_input(format!("upload stream broke after {written} bytes: {e}"))
        })?;
        *written += chunk.len() as u64;
        if *written > limit {
            return Err(ApiError::bad_input(format!(
                "body is larger than the declared Content-Length of {limit} bytes"
            )));
        }
        writer
            .write_all(&chunk)
            .await
            .map_err(|e| ApiError::not_connected(format!("could not write staging file: {e}")))?;
        reporter.report(*written);
    }

    writer
        .flush()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not flush staging file: {e}")))?;
    // `sync_all`, а не только flush: дальше файл читает сборщик base64, и он
    // обязан увидеть все байты, а не хвост в страничном кэше.
    writer
        .into_inner()
        .sync_all()
        .await
        .map_err(|e| ApiError::not_connected(format!("could not sync staging file: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Реестр загрузок: TTL, сироты, потребление.
// ---------------------------------------------------------------------------

/// Открывает staging-файл загрузки.
///
/// # Errors
///
/// `409 invalid_state`, если файла нет: запись в реестре есть, а байтов нет —
/// значит, GC уже прибрал протухшую загрузку либо кто-то почистил кэш руками.
/// Клиенту это «залей файл заново», а не «сервер сломался».
pub async fn open_staging(entry: &UploadEntry) -> Result<tokio::fs::File, ApiError> {
    tokio::fs::File::open(&entry.path).await.map_err(|e| {
        ApiError::invalid_state(format!(
            "staged file for this upload_id is no longer available ({e}): upload the file again"
        ))
    })
}

/// Выбрасывает из реестра протухшие записи вместе с их staging-файлами.
///
/// Возвращает число удалённых записей.
pub async fn evict_stale(state: &Arc<B24State>) -> usize {
    evict_stale_at(state, UPLOAD_TTL_SECS, unix_now()).await
}

/// То же с явными TTL и «сейчас» — точка инъекции для тестов.
///
/// Без неё тест «протухшая запись удаляется» обязан был бы спать
/// полчаса реального времени.
pub async fn evict_stale_at(state: &Arc<B24State>, ttl_secs: i64, now: i64) -> usize {
    let stale = state.evict_stale_uploads(ttl_secs, now).await;
    for entry in &stale {
        let _ = tokio::fs::remove_file(&entry.path).await;
    }
    if !stale.is_empty() {
        tracing::info!(
            target: "forge_bitrix",
            removed = stale.len(),
            ttl_secs,
            "forge-bitrix: expired uploads discarded"
        );
    }
    stale.len()
}

/// Удаляет staging-файл потреблённой загрузки.
///
/// Ошибка удаления не фатальна: файл всё равно подберёт [`evict_stale`] или
/// [`cleanup_uploads_dir`] после рестарта.
pub async fn discard_staging(entry: &UploadEntry) {
    if let Err(e) = tokio::fs::remove_file(&entry.path).await {
        tracing::debug!(
            target: "forge_bitrix",
            path = %entry.path.display(),
            error = %e,
            "forge-bitrix: could not remove staging file"
        );
    }
}

/// ПОТРЕБЛЯЕТ запись реестра под `upload_id`.
///
/// # Почему потребление безвозвратно
///
/// [`crate::state::B24State::take_upload`] убирает запись ЕЩЁ ДО заливки, и
/// это осознанный размен: повторный `POST /messages` с тем же `upload_id`
/// обязан получить `409`, а не отправить файл второй раз. Двойной клик по
/// «Отправить» и автоповтор фронта — вещи куда более частые, чем неудачная
/// заливка, и цена ошибки у них разная: лишний файл в чате у собеседника не
/// отзывается, а залить заново можно всегда.
///
/// # Errors
///
/// `409 invalid_state` — записи нет: либо `upload_id` уже потрачен, либо
/// протух по [`crate::state::UPLOAD_TTL_SECS`], либо его не было вовсе.
pub async fn take_upload(state: &Arc<B24State>, upload_id: &str) -> Result<UploadEntry, ApiError> {
    state.take_upload(upload_id).await.ok_or_else(|| {
        // Идентификатор в текст НЕ подставляется: ответ не должен работать
        // рефлектором чужого ввода.
        ApiError::invalid_state(
            "this upload_id is unknown, already used or expired: upload the file again".to_string(),
        )
    })
}

/// Чистит каталог staging'а на старте процесса.
///
/// Реестр `uploads` живёт только в памяти, поэтому после рестарта каждый
/// `.part` — сирота: сослаться на него уже некому, а место он занимает вполне
/// реальное. Возвращает число удалённых файлов.
pub async fn cleanup_uploads_dir(cfg: &crate::config::BitrixConfig) -> usize {
    let dir = uploads_dir(cfg);
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let is_file = entry.metadata().await.map(|m| m.is_file()).unwrap_or(false);
        if is_file && tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            target: "forge_bitrix",
            removed,
            dir = %dir.display(),
            "forge-bitrix: orphaned upload staging files removed on startup"
        );
    }
    removed
}

/// Сбрасывает ВЕСЬ реестр вместе со staging-файлами.
///
/// Зовётся при logout: невыбранный файл — это данные аккаунта ровно так же,
/// как имена сотрудников и счётчики. Пережив отвязку, он уехал бы в чат
/// СЛЕДУЮЩЕГО портала первой же отправкой.
pub async fn clear_uploads(state: &Arc<B24State>) -> usize {
    let entries: Vec<UploadEntry> = {
        let mut guard = state.uploads.lock().await;
        guard.drain().map(|(_, e)| e).collect()
    };
    for entry in &entries {
        discard_staging(entry).await;
    }
    // И каталог целиком: в нём могли остаться `.part` от прошлых процессов.
    cleanup_uploads_dir(&state.config).await;
    entries.len()
}

/// Фоновый воркер: подчистка протухших загрузок раз в [`UPLOAD_GC_INTERVAL`].
///
/// Первый прогон — сразу на старте (он же убирает сирот от прошлого процесса),
/// дальше по таймеру. Останавливается по `shutdown`-токену.
pub async fn uploads_gc_loop(state: Arc<B24State>) {
    cleanup_uploads_dir(&state.config).await;
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => {
                tracing::debug!(target: "forge_bitrix", "forge-bitrix: uploads GC stopped");
                return;
            }
            _ = tokio::time::sleep(UPLOAD_GC_INTERVAL) => {}
        }
        evict_stale(&state).await;
    }
}

/// Запускает GC-воркер загрузок и регистрирует его для graceful shutdown.
pub async fn spawn_uploads_gc(state: &Arc<B24State>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { uploads_gc_loop(st).await });
    state.register_worker(handle).await;
    tracing::info!(
        target: "forge_bitrix",
        interval_secs = UPLOAD_GC_INTERVAL.as_secs(),
        ttl_secs = UPLOAD_TTL_SECS,
        "forge-bitrix: uploads GC started"
    );
}

// ---------------------------------------------------------------------------
// Вторая половина: staging-файл → портал одним вызовом (Р7).
// ---------------------------------------------------------------------------

/// Отчётчик прогресса, разделённый между колбэком ридера и вызывающим.
///
/// `std::sync::Mutex`, а не `tokio::sync::Mutex`: колбэк дёргается из
/// `poll_read`, то есть из синхронного контекста, где `await` невозможен.
/// Критическая секция — одна арифметическая проверка троттлера плюс
/// `broadcast::send`, поэтому блокировки исполнителя не создаёт.
pub type SharedReporter = Arc<std::sync::Mutex<UploadReporter>>;

/// Заливает staging-файл в портал и отправляет сообщение — ОДНИМ вызовом.
///
/// Последовательность:
///
/// 1. [`take_upload`] ПОТРЕБЛЯЕТ запись — повторить ту же отправку уже нельзя;
/// 2. отчётчик переводится в фазу `portal` ([`UploadReporter::begin_portal`]),
///    и кадр помечается `indeterminate` — процентов у одношаговой заливки нет;
/// 3. файл читается кусками и кодируется в base64 (память ~1.33 размера);
/// 4. `im.v2.File.upload` — БЕЗ ПОВТОРА (Р9);
/// 5. финальный кадр прогресса уходит В ЛЮБОМ ИСХОДЕ, включая ошибку;
/// 6. staging-файл удаляется тоже в любом исходе: повторять его нельзя, а
///    место он занимает.
///
/// # Почему финальный кадр уходит и при ошибке
///
/// Это единственное поведенческое расхождение с VK, и оно намеренное (Р8):
/// индикатор обязан завершиться при любом исходе, иначе полоса живёт до
/// перезагрузки вкладки. Признаком УСПЕХА кадр при этом не является — успех
/// приезжает ответом REST, а провал ошибкой REST; фронт помечает строку по
/// ответу, а не по прогрессу.
///
/// # Errors
///
/// `409 invalid_state` — `upload_id` неизвестен, потрачен или протух;
/// `503 not_connected` — файл не читается либо портал недоступен; всё, чем
/// может ответить портал, — по таблице §4 ([`map_portal_error`]).
pub async fn send_staged_file(
    state: &Arc<B24State>,
    client: &B24Client,
    dialog_id: &str,
    upload_id: &str,
    caption: &str,
) -> Result<i64, ApiError> {
    let entry = take_upload(state, upload_id).await?;

    let reporter: SharedReporter = Arc::new(std::sync::Mutex::new(UploadReporter::new(
        state.clone(),
        upload_id,
        entry.size,
    )));
    if let Ok(mut r) = reporter.lock() {
        r.begin_portal();
    }

    let outcome = upload_to_portal(client, dialog_id, &entry, caption, &reporter).await;

    // Финал — ВСЕГДА (Р8) и ровно один раз.
    if let Ok(mut r) = reporter.lock() {
        r.finish(entry.size);
    }
    // Полуфабрикат израсходован в любом исходе: повторять его запрещено (Р9),
    // а лежать он будет до истечения TTL.
    discard_staging(&entry).await;

    match &outcome {
        Ok(message_id) => tracing::info!(
            target: "forge_bitrix",
            upload_id = %upload_id,
            dialog_id = %dialog_id,
            size = entry.size,
            message_id,
            "forge-bitrix: file uploaded and sent"
        ),
        Err(e) => tracing::warn!(
            target: "forge_bitrix",
            upload_id = %upload_id,
            dialog_id = %dialog_id,
            size = entry.size,
            code = %e.body.code,
            "forge-bitrix: file upload failed; NOT retrying (Р9)"
        ),
    }
    outcome
}

/// Собственно заливка: base64 из файла → `im.v2.File.upload`.
async fn upload_to_portal(
    client: &B24Client,
    dialog_id: &str,
    entry: &UploadEntry,
    caption: &str,
    reporter: &SharedReporter,
) -> Result<i64, ApiError> {
    let file = open_staging(entry).await?;
    let content = encode_base64(file, entry.size, reporter).await?;
    client
        .file_upload(dialog_id, &entry.file_name, &content, caption)
        .await
        .map_err(|e| map_portal_error(&e))
}

/// Колбэк для [`ProgressReader`].
///
/// Отравленный mutex (паника в другом потоке) молча игнорируется: прогрессбар
/// — не причина ронять уже идущую отправку файла.
fn progress_hook(reporter: &SharedReporter) -> impl FnMut(u64) + Unpin {
    let rep = reporter.clone();
    move |sent: u64| {
        if let Ok(mut r) = rep.lock() {
            r.report(sent);
        }
    }
}

/// Собирает base64 ИЗ ФАЙЛА, кусками по [`B64_CHUNK`] байт.
///
/// Читать файл целиком в `Vec<u8>` и кодировать одним вызовом было бы на
/// строку короче и стоило бы ВТОРОЙ копии размера файла в памяти: сначала
/// байты, потом строка. Здесь в памяти живёт только результат (~1.33 размера)
/// плюс буфер в 48 KiB.
///
/// Кусок кратен трём, поэтому конкатенация кодированных кусков даёт ровно ту
/// же строку, что и кодирование целиком (проверяется тестом
/// `chunked_base64_matches_whole_file_base64`).
///
/// Кадры прогресса на этом шаге помечены `indeterminate` (фаза `portal`):
/// байты читаются С ДИСКА, а не уходят в портал, и выдавать их за отправленные
/// значило бы врать процентами.
///
/// # Errors
///
/// `503 not_connected` — файл не читается (диск, права, файл исчез).
async fn encode_base64(
    file: tokio::fs::File,
    size: u64,
    reporter: &SharedReporter,
) -> Result<String, ApiError> {
    let mut reader = ProgressReader::new(file, progress_hook(reporter));
    // Точный размер результата: 4 символа на каждые 3 байта, с выравниванием
    // вверх. Одна аллокация вместо десятков перевыделений.
    let mut out = String::with_capacity(((size as usize).div_ceil(3)) * 4 + 4);
    let mut buf = vec![0u8; B64_CHUNK];
    loop {
        let n = read_full(&mut reader, &mut buf).await.map_err(|e| {
            ApiError::not_connected(format!("could not read the staged file: {e}"))
        })?;
        if n == 0 {
            break;
        }
        out.push_str(&base64::engine::general_purpose::STANDARD.encode(&buf[..n]));
        if n < buf.len() {
            break;
        }
    }
    Ok(out)
}

/// Читает ровно `buf.len()` байт либо меньше — но только на конце файла.
///
/// `AsyncReadExt::read` вправе вернуть меньше запрошенного в любой момент, и
/// наивный цикл «прочитал сколько дали — закодировал» разорвал бы base64 на
/// некратной трём границе. Здесь буфер добивается до конца, и короткий
/// результат означает ровно одно: файл кончился.
async fn read_full<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BitrixConfig;
    use crate::state::{B24UserDto, LoginFlow};
    use axum::http::{Request, StatusCode};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    fn state_in(dir: &Path) -> Arc<B24State> {
        B24State::new(BitrixConfig {
            cache_dir: dir.join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.join("prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            config_path: dir.join("bitrix.toml"),
            // Заведомо закрытый порт: ни один тест этого модуля не имеет права
            // уйти в живой портал, а состояние `authorized` без этого поля
            // отправило бы его именно туда.
            base_url_override: "https://127.0.0.1:1/rest/1/abc123".into(),
            ..BitrixConfig::default()
        })
    }

    async fn authorized(dir: &Path) -> Arc<B24State> {
        let st = state_in(dir);
        st.set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "portal.bitrix24.ru".into(),
        })
        .await;
        st
    }

    fn app(state: Arc<B24State>) -> Router {
        let limit = state.config.max_upload_bytes;
        router(limit).with_state(state)
    }

    fn upload_req(name: Option<&str>, len: Option<usize>, body: &'static [u8]) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/bitrix/upload")
            .header("content-type", "application/octet-stream");
        if let Some(n) = name {
            b = b.header(FILE_NAME_HEADER, n);
        }
        if let Some(l) = len {
            b = b.header("content-length", l);
        }
        b.body(Body::from(body)).unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    // ---------------- чистые функции ----------------

    #[test]
    fn percent_decode_handles_encode_uri_component_output() {
        assert_eq!(percent_decode("plain.txt"), "plain.txt");
        assert_eq!(percent_decode("my%20photo.jpg"), "my photo.jpg");
        // Кириллица: encodeURIComponent('отчёт.pdf').
        assert_eq!(
            percent_decode("%D0%BE%D1%82%D1%87%D1%91%D1%82.pdf"),
            "отчёт.pdf"
        );
        // Незакрытый и мусорный процент остаются как есть, а не рушат разбор.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zz.bin"), "a%zz.bin");
        assert_eq!(percent_decode("%2F%2E%2E%2Fetc"), "/../etc");
        // Оборванный escape в конце строки не должен ни паниковать, ни съедать
        // символы.
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("file%2"), "file%2");
        assert_eq!(percent_decode("%%41"), "%A");
    }

    #[test]
    fn sanitize_upload_name_strips_paths_and_controls() {
        assert_eq!(sanitize_upload_name("report.pdf").unwrap(), "report.pdf");
        assert_eq!(sanitize_upload_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(
            sanitize_upload_name("C:\\Windows\\evil.exe").unwrap(),
            "evil.exe"
        );
        assert_eq!(
            sanitize_upload_name("a\r\nX-Evil: 1.txt").unwrap(),
            "aX-Evil: 1.txt"
        );
        assert_eq!(sanitize_upload_name("..").unwrap(), FALLBACK_FILE_NAME);
        assert_eq!(sanitize_upload_name("dir/").unwrap(), FALLBACK_FILE_NAME);
        assert!(sanitize_upload_name("").is_none());
        assert!(sanitize_upload_name("   ").is_none());
        assert!(sanitize_upload_name("\r\n\t").is_none());
    }

    /// Traversal, NUL и CRLF, протащенные через percent-encoding, не должны
    /// доживать до имени: decode и sanitize работают в связке.
    #[test]
    fn decoded_traversal_and_nul_bytes_do_not_reach_the_name() {
        assert_eq!(
            sanitize_upload_name(&percent_decode("..%2F..%2Fetc%2Fpasswd")).unwrap(),
            "passwd"
        );
        assert_eq!(
            sanitize_upload_name(&percent_decode("a%00b.txt")).unwrap(),
            "ab.txt"
        );
        assert_eq!(
            sanitize_upload_name(&percent_decode("evil%0D%0A.txt")).unwrap(),
            "evil.txt"
        );
    }

    /// Граница в 255 байт: ровно 255 — без изменений, больше — обрезка по
    /// границе символа.
    #[test]
    fn sanitize_upload_name_at_the_255_byte_boundary() {
        let exact = "a".repeat(MAX_FILE_NAME_LEN);
        assert_eq!(sanitize_upload_name(&exact).unwrap(), exact);

        let over = "a".repeat(MAX_FILE_NAME_LEN + 1);
        assert_eq!(sanitize_upload_name(&over).unwrap().len(), MAX_FILE_NAME_LEN);

        // 64 emoji по 4 байта = 256 байт ⇒ корректная обрезка даёт 63 целых.
        let emoji = "🎉".repeat(64);
        let got = sanitize_upload_name(&emoji).unwrap();
        assert_eq!(got.chars().count(), 63, "len={}", got.len());

        let long = "я".repeat(400);
        let got = sanitize_upload_name(&long).unwrap();
        assert!(got.len() <= MAX_FILE_NAME_LEN, "{}", got.len());
        assert!(got.chars().all(|c| c == 'я'));
    }

    #[test]
    fn guess_mime_uses_extension() {
        assert_eq!(guess_mime("a.jpg"), "image/jpeg");
        assert_eq!(guess_mime("a.png"), "image/png");
        assert_eq!(guess_mime("a.pdf"), "application/pdf");
        assert_eq!(guess_mime("noext"), DEFAULT_MIME);
        // Камеры и Windows-клиенты присылают расширения в верхнем регистре.
        assert_eq!(guess_mime("DSC0001.JPG"), "image/jpeg");
    }

    #[test]
    fn parse_content_length_rejects_garbage() {
        let mut h = HeaderMap::new();
        assert_eq!(parse_content_length(&h), None);
        h.insert("content-length", "1024".parse().unwrap());
        assert_eq!(parse_content_length(&h), Some(1024));
        h.insert("content-length", "-1".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
        h.insert("content-length", "abc".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
        // u64::MAX разбирается, а u64::MAX + 1 — мусор (без усечения).
        h.insert("content-length", u64::MAX.to_string().parse().unwrap());
        assert_eq!(parse_content_length(&h), Some(u64::MAX));
        h.insert("content-length", "18446744073709551616".parse().unwrap());
        assert_eq!(parse_content_length(&h), None);
    }

    #[test]
    fn staging_path_never_contains_client_input() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            ..BitrixConfig::default()
        };
        let p = staging_path(&cfg, "0191b0de-1111-4222-8333-444455556666");
        assert!(p.starts_with(uploads_dir(&cfg)));
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "0191b0de-1111-4222-8333-444455556666.part"
        );
    }

    /// Кусок кратен трём ⇒ склейка кодированных кусков равна кодированию
    /// целиком. Промах здесь дал бы `=` посреди строки и битый файл в портале
    /// БЕЗ единой ошибки на нашей стороне.
    #[tokio::test]
    async fn chunked_base64_matches_whole_file_base64() {
        assert_eq!(B64_CHUNK % 3, 0, "кусок обязан быть кратен трём");

        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        // Размер заведомо больше куска и НЕ кратен ни куску, ни трём.
        let payload: Vec<u8> = (0..=255u8).cycle().take(B64_CHUNK * 2 + 7).collect();
        let path = dir.path().join("blob.bin");
        tokio::fs::write(&path, &payload).await.unwrap();

        let reporter: SharedReporter = Arc::new(std::sync::Mutex::new(UploadReporter::new(
            st.clone(),
            "u-chunk",
            payload.len() as u64,
        )));
        let file = tokio::fs::File::open(&path).await.unwrap();
        let got = encode_base64(file, payload.len() as u64, &reporter)
            .await
            .unwrap();
        assert_eq!(
            got,
            base64::engine::general_purpose::STANDARD.encode(&payload)
        );
    }

    /// Пустой файл кодируется в пустую строку, а не в мусор из набивки.
    #[tokio::test]
    async fn an_empty_file_encodes_to_an_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        let path = dir.path().join("empty.bin");
        tokio::fs::write(&path, b"").await.unwrap();
        let reporter: SharedReporter =
            Arc::new(std::sync::Mutex::new(UploadReporter::new(st, "u-0", 0)));
        let file = tokio::fs::File::open(&path).await.unwrap();
        assert_eq!(encode_base64(file, 0, &reporter).await.unwrap(), "");
    }

    // ---------------- лестница проверок ----------------

    /// Без подключения — 409, и тело даже не читается: каталог staging'а не
    /// создан, реестр пуст.
    #[tokio::test]
    async fn upload_without_auth_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(resp).await["error"]["code"], "not_authorized");
        assert!(st.uploads.lock().await.is_empty());
        assert!(!uploads_dir(&st.config).exists(), "каталог даже не создан");
    }

    #[tokio::test]
    async fn upload_without_content_length_is_411() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st)
            .oneshot(upload_req(Some("a.txt"), None, b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::LENGTH_REQUIRED);
        assert_eq!(body_json(resp).await["error"]["code"], "length_required");
    }

    #[tokio::test]
    async fn upload_over_the_limit_is_413() {
        let dir = tempfile::tempdir().unwrap();
        let st = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            base_url_override: "https://127.0.0.1:1/rest/1/abc123".into(),
            // Настоящие 25 МиБ в тесте не пришлёшь.
            max_upload_bytes: 4,
            ..BitrixConfig::default()
        });
        st.set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "portal.bitrix24.ru".into(),
        })
        .await;

        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "too_large");
        assert!(
            json["error"]["message"].as_str().unwrap().contains('4'),
            "в сообщении обязан быть лимит: {json}"
        );
        assert!(st.uploads.lock().await.is_empty());
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ ЛЕСТНИЦЫ: при превышении лимита из тела не читается
    /// НИ ОДНОГО байта.
    ///
    /// Проверяется не косвенно (по отсутствию файла), а прямо: тело — поток,
    /// который считает СВОИ опросы. Ноль опросов означает, что до `.next()`
    /// дело не дошло вовсе, то есть гигабайтный файл будет отвергнут, не
    /// доехав до сервера.
    #[tokio::test]
    async fn an_over_limit_body_is_never_polled() {
        let dir = tempfile::tempdir().unwrap();
        let st = B24State::new(BitrixConfig {
            cache_dir: dir.path().join("cache"),
            // Герметичность: без явного пути хранилище заметок легло бы в живой
            // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
            // гарантирует, что тест не запустит настоящий claude.
            prompts_path: dir.path().join("prompts.json"),
            claude_cli_path: dir.path().join("no-such-claude"),
            config_path: dir.path().join("bitrix.toml"),
            base_url_override: "https://127.0.0.1:1/rest/1/abc123".into(),
            max_upload_bytes: 8,
            ..BitrixConfig::default()
        });
        st.set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: "portal.bitrix24.ru".into(),
        })
        .await;

        let polled = Arc::new(AtomicU64::new(0));
        let counter = polled.clone();
        let stream = futures_util::stream::once(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(&[0u8; 4096]))
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/bitrix/upload")
            .header("content-type", "application/octet-stream")
            .header(FILE_NAME_HEADER, "big.bin")
            .header("content-length", 4096)
            .body(Body::from_stream(stream))
            .unwrap();

        let resp = app(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            polled.load(Ordering::SeqCst),
            0,
            "тело обязано остаться непрочитанным: 413 выдаётся ДО чтения"
        );
        assert!(!uploads_dir(&st.config).exists());
    }

    #[tokio::test]
    async fn upload_without_a_name_is_400() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(None, Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(resp).await["error"]["code"], "invalid_input");

        // Заголовок есть, но декодируется в пустоту — тот же отказ.
        let resp = app(st.clone())
            .oneshot(upload_req(Some("%20%20"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(st.uploads.lock().await.is_empty());
    }

    /// Мусор в `Content-Length` — это 400, а не 411: заголовок ЕСТЬ, он просто
    /// нечитаем, и «пришли размер» было бы неверной подсказкой.
    #[tokio::test]
    async fn a_garbage_content_length_is_400_not_411() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/bitrix/upload")
            .header("content-type", "application/octet-stream")
            .header(FILE_NAME_HEADER, "a.txt")
            .header("content-length", "not-a-number")
            .body(Body::from("hello"))
            .unwrap();
        let resp = app(st).oneshot(req).await.unwrap();
        // Мусорный Content-Length hyper отвергает ещё на разборе запроса —
        // важно лишь то, что это НЕ 411 и не приём файла.
        assert!(
            resp.status() == StatusCode::BAD_REQUEST,
            "status={}",
            resp.status()
        );
    }

    /// Порядок ступеней: без подключения имя файла и размер уже не имеют
    /// значения. Иначе роут работал бы оракулом валидности ввода для
    /// неподключённого клиента.
    #[tokio::test]
    async fn auth_is_checked_before_length_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        // Ни имени, ни длины — и всё равно 409, а не 411/400.
        let resp = app(st).oneshot(upload_req(None, None, b"x")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // ---------------- staging ----------------

    #[tokio::test]
    async fn a_successful_upload_creates_a_part_file_and_a_registry_entry() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("%D0%BE%D1%82%D1%87%D1%91%D1%82.pdf"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["file_name"], "отчёт.pdf");
        assert_eq!(json["size"], 5);
        assert_eq!(json["mime"], "application/pdf");

        let upload_id = json["upload_id"].as_str().unwrap().to_string();
        let path = staging_path(&st.config, &upload_id);
        assert!(path.is_file(), "staging-файл не создан: {}", path.display());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello");
        assert_eq!(st.uploads.lock().await.len(), 1);
    }

    /// `take_upload` ПОТРЕБЛЯЕТ запись: второй вызов с тем же `upload_id`
    /// обязан отказать. Это защита от дубля на уровне механики, а не UI.
    #[tokio::test]
    async fn take_upload_consumes_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        let upload_id = body_json(resp).await["upload_id"]
            .as_str()
            .unwrap()
            .to_string();

        let first = take_upload(&st, &upload_id).await.expect("первый берёт");
        assert_eq!(first.size, 5);

        let second = take_upload(&st, &upload_id).await.unwrap_err();
        assert_eq!(second.status, StatusCode::CONFLICT);
        assert_eq!(second.body.code, "invalid_state");
        // И идентификатор в текст не подставлен: ответ не рефлектор ввода.
        assert!(!second.body.message.contains(&upload_id));
    }

    /// Протухшая запись выбрасывается вместе с файлом — БЕЗ сна на полчаса.
    #[tokio::test]
    async fn stale_uploads_are_evicted_with_their_files() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        let upload_id = body_json(resp).await["upload_id"]
            .as_str()
            .unwrap()
            .to_string();
        let path = staging_path(&st.config, &upload_id);
        assert!(path.is_file());

        // «Сейчас» сдвинуто на TTL + минуту вперёд.
        let removed = evict_stale_at(&st, UPLOAD_TTL_SECS, unix_now() + UPLOAD_TTL_SECS + 60).await;
        assert_eq!(removed, 1);
        assert!(!path.exists(), "файл протухшей загрузки обязан исчезнуть");
        assert!(st.uploads.lock().await.is_empty());
    }

    /// Свежая запись переживает прогон GC: TTL не должен резать по живому.
    #[tokio::test]
    async fn a_fresh_upload_survives_the_gc() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        assert_eq!(evict_stale(&st).await, 0);
        assert_eq!(st.uploads.lock().await.len(), 1);
    }

    /// Старт процесса чистит каталог целиком: после рестарта реестр пуст, и
    /// сослаться на `.part` уже некому.
    #[tokio::test]
    async fn a_process_start_removes_orphaned_part_files() {
        let dir = tempfile::tempdir().unwrap();
        let st = state_in(dir.path());
        let d = uploads_dir(&st.config);
        tokio::fs::create_dir_all(&d).await.unwrap();
        tokio::fs::write(d.join("aaa.part"), b"orphan").await.unwrap();
        tokio::fs::write(d.join("bbb.part"), b"orphan").await.unwrap();

        assert_eq!(cleanup_uploads_dir(&st.config).await, 2);
        let mut rd = tokio::fs::read_dir(&d).await.unwrap();
        assert!(rd.next_entry().await.unwrap().is_none(), "каталог пуст");
        // Отсутствующий каталог — не ошибка: первый запуск ещё ничего не
        // принимал.
        let empty = state_in(&dir.path().join("nowhere"));
        assert_eq!(cleanup_uploads_dir(&empty.config).await, 0);
    }

    /// Logout стирает и реестр, и файлы: невыбранный файл — данные аккаунта.
    #[tokio::test]
    async fn logout_clears_the_registry_and_the_staging_files() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let resp = app(st.clone())
            .oneshot(upload_req(Some("a.txt"), Some(5), b"hello"))
            .await
            .unwrap();
        let upload_id = body_json(resp).await["upload_id"]
            .as_str()
            .unwrap()
            .to_string();
        let path = staging_path(&st.config, &upload_id);

        assert_eq!(clear_uploads(&st).await, 1);
        assert!(st.uploads.lock().await.is_empty());
        assert!(!path.exists());
    }

    /// Оборванное тело (пришло меньше обещанного) не оставляет `.part`:
    /// guard снимает файл на любой ветке выхода.
    #[tokio::test]
    async fn a_short_body_leaves_no_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let req = Request::builder()
            .method("POST")
            .uri("/api/bitrix/upload")
            .header("content-type", "application/octet-stream")
            .header(FILE_NAME_HEADER, "a.txt")
            // Обещано 100 байт, придёт 5.
            .header("content-length", 100)
            .body(Body::from("hello"))
            .unwrap();
        let resp = app(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(st.uploads.lock().await.is_empty());

        let d = uploads_dir(&st.config);
        let mut rd = tokio::fs::read_dir(&d).await.unwrap();
        assert!(
            rd.next_entry().await.unwrap().is_none(),
            "после отказа в каталоге staging'а не должно остаться файлов"
        );
    }

    // ---------------- прогресс ----------------

    /// Фаза `client` даёт РАСТУЩИЕ точные проценты, и финальный кадр приходит
    /// обязательно (Р8).
    #[tokio::test]
    async fn the_client_phase_reports_growing_exact_progress() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let mut rx = st.subscribe();

        // Тело из нескольких кусков: один кусок дал бы один кадр и не показал
        // бы роста.
        let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = (0..4)
            .map(|_| Ok(axum::body::Bytes::from_static(&[7u8; 1024])))
            .collect();
        let req = Request::builder()
            .method("POST")
            .uri("/api/bitrix/upload")
            .header("content-type", "application/octet-stream")
            .header(FILE_NAME_HEADER, "a.bin")
            .header("content-length", 4096)
            .body(Body::from_stream(futures_util::stream::iter(chunks)))
            .unwrap();
        let resp = app(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut sent_seq = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            if let crate::ws::protocol::B24ServerMsg::UploadProgress {
                phase,
                sent,
                total,
                indeterminate,
                ..
            } = frame
            {
                assert_eq!(phase, crate::progress::PHASE_CLIENT);
                assert_eq!(total, 4096);
                assert!(!indeterminate, "у приёма от браузера проценты точные");
                sent_seq.push(sent);
            }
        }
        assert!(!sent_seq.is_empty(), "кадров прогресса нет вовсе");
        assert!(
            sent_seq.windows(2).all(|w| w[0] <= w[1]),
            "прогресс обязан расти: {sent_seq:?}"
        );
        assert_eq!(
            *sent_seq.last().unwrap(),
            4096,
            "финальный кадр фазы client обязан прийти: {sent_seq:?}"
        );
    }

    /// Финальный кадр приходит и при ОШИБКЕ приёма — иначе полоса осталась бы
    /// висеть до перезагрузки вкладки.
    #[tokio::test]
    async fn the_final_client_frame_arrives_even_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let st = authorized(dir.path()).await;
        let mut rx = st.subscribe();

        let req = Request::builder()
            .method("POST")
            .uri("/api/bitrix/upload")
            .header("content-type", "application/octet-stream")
            .header(FILE_NAME_HEADER, "a.bin")
            .header("content-length", 100)
            .body(Body::from("hello"))
            .unwrap();
        let resp = app(st.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let mut frames = 0;
        while let Ok(frame) = rx.try_recv() {
            if matches!(
                frame,
                crate::ws::protocol::B24ServerMsg::UploadProgress { .. }
            ) {
                frames += 1;
            }
        }
        assert!(frames > 0, "финальный кадр обязан пройти и при отказе");
    }
}
