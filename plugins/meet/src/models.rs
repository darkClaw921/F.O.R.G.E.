//! Каталог моделей whisper.cpp и их скачивание.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # СКАЧИВАНИЕ ТОЛЬКО ПО ЯВНОМУ ДЕЙСТВИЮ ЧЕЛОВЕКА
//!
//! Автоподкачки нет и не будет. Докачка 1,6 ГБ на критическом пути «встреча
//! начинается через десять секунд» — худший из возможных сценариев: бот входит
//! на пятой минуте, а пользователь всё это время смотрит на крутилку и не
//! понимает, почему запись не идёт. Отсутствующая модель честно даёт `409
//! model_unavailable` и кнопку «скачать», нажать которую решает человек.
//!
//! Инвариант закреплён тестом [`tests::nothing_but_the_route_starts_a_download`],
//! который обходит исходники: единственные вызывающие [`download_model`] — это
//! роут `POST /api/meet/models/:name/download` и тесты этого модуля.
//!
//! # Раскладка
//!
//! ```text
//! ~/.forge/meet/models/            (FORGE_MEET_MODELS_DIR / models_dir)
//!   ggml-large-v3-turbo-q5_0.bin   модель распознавания
//!   ggml-silero-v5.1.2.bin         модель VAD для `--vad` (см. ниже)
//!   .ggml-....bin.part             недокачанное — НИКОГДА не выглядит готовым
//! ```
//!
//! # Почему у VAD отдельная модель
//!
//! `--vad` без `-vm` в whisper.cpp 1.9.2 **не ругается, а падает**: exit 134,
//! SIGABRT, стек `ggml_abort` (измерено в Ф0). Поэтому модель VAD — такой же
//! элемент каталога, как модель распознавания, а `build_args` не имеет права
//! собрать `--vad` без `-vm`.
//!
//! # Проверка: размер, магия и SHA-256
//!
//! Все три хэша взяты из `x-linked-etag` HuggingFace (у LFS-файлов это ровно
//! SHA-256 содержимого) и сверены с локальной копией: `shasum -a 256` на
//! скачанной в Ф0 модели дал ту же строку. Проверка нужна не от
//! злоумышленника, а от обрыва связи и от HTML-страницы ошибки, прилетевшей
//! вместо гигабайта весов, — файл с расширением `.bin` и текстом «502 Bad
//! Gateway» внутри иначе выглядел бы готовой моделью.

use std::path::{Path, PathBuf};

use crate::error::MeetError;

/// Магия файла GGML: `0x67676d6c` («ggml») в порядке little-endian, то есть
/// на диске это байты `lmgg`.
pub const GGML_MAGIC: [u8; 4] = *b"lmgg";

/// Префикс имени файла модели.
pub const MODEL_FILE_PREFIX: &str = "ggml-";

/// Расширение файла модели.
pub const MODEL_FILE_SUFFIX: &str = ".bin";

/// Расширение недокачанного файла.
///
/// Точка в начале имени и `.part` в конце — два независимых признака. Хватило
/// бы одного, но [`installed`] ищет строго `ggml-*.bin`, и совпасть с этим
/// шаблоном недокачанный файл не может ни при каком стечении обстоятельств.
pub const PART_SUFFIX: &str = ".part";

/// Для чего модель.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// Распознавание речи.
    Transcribe,
    /// Детектор речи для `--vad`.
    Vad,
}

/// Известная модель.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ModelSpec {
    /// Имя в терминах конфига (`whisper_model`) и роутов.
    pub name: &'static str,
    /// Имя файла в каталоге моделей.
    pub file: &'static str,
    /// Откуда скачивать.
    pub url: &'static str,
    /// Точный размер в байтах (`x-linked-size` HuggingFace).
    pub size_bytes: u64,
    /// SHA-256 содержимого (`x-linked-etag` HuggingFace).
    pub sha256: &'static str,
    pub kind: ModelKind,
    /// Человеческое описание для интерфейса.
    pub note: &'static str,
}

/// Каталог известных моделей.
///
/// Список закрытый и небольшой намеренно: это не зеркало HuggingFace, а
/// перечень того, что имеет смысл на машине с Metal. Свой файл пользователь
/// может положить руками — [`model_path`] найдёт `ggml-<имя>.bin` и без записи
/// в каталоге.
pub const MODELS: [ModelSpec; 6] = [
    ModelSpec {
        name: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        size_bytes: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        kind: ModelKind::Transcribe,
        note: "по умолчанию: 13–15× реального времени на M1 Max, ≈4,5 мин на час аудио",
    },
    ModelSpec {
        name: "large-v3-turbo",
        file: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        size_bytes: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        kind: ModelKind::Transcribe,
        note: "без квантования: чуть точнее, втрое тяжелее на диске",
    },
    ModelSpec {
        name: "medium-q5_0",
        file: "ggml-medium-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin",
        size_bytes: 539_212_467,
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        kind: ModelKind::Transcribe,
        note: "предыдущее поколение, заметно хуже на русском",
    },
    ModelSpec {
        name: "small-q5_1",
        file: "ggml-small-q5_1.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
        size_bytes: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        kind: ModelKind::Transcribe,
        note: "лёгкая: годится проверить пайплайн, не годится для протокола встречи",
    },
    ModelSpec {
        name: "base",
        file: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        size_bytes: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        kind: ModelKind::Transcribe,
        note: "самая лёгкая осмысленная; для отладки",
    },
    ModelSpec {
        name: "silero-v5.1.2",
        file: "ggml-silero-v5.1.2.bin",
        url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
        size_bytes: 885_098,
        sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
        kind: ModelKind::Vad,
        note: "модель VAD: без неё флаг --vad роняет whisper по SIGABRT",
    },
];

/// Модель по умолчанию.
pub const DEFAULT_MODEL: &str = crate::config::DEFAULT_WHISPER_MODEL;

/// Известная модель по имени.
pub fn spec(name: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.name == name)
}

/// Модель VAD из каталога.
pub fn vad_spec() -> &'static ModelSpec {
    MODELS
        .iter()
        .find(|m| m.kind == ModelKind::Vad)
        .expect("в каталоге обязана быть модель VAD")
}

/// Имя файла модели: из каталога либо по соглашению `ggml-<имя>.bin`.
///
/// Соглашение нужно, чтобы пользователь мог положить свои веса руками и
/// прописать их в `whisper_model`, не дожидаясь записи в нашем списке.
pub fn file_name(name: &str) -> String {
    match spec(name) {
        Some(s) => s.file.to_string(),
        None => format!("{MODEL_FILE_PREFIX}{name}{MODEL_FILE_SUFFIX}"),
    }
}

/// Путь к файлу модели в каталоге.
pub fn model_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(file_name(name))
}

/// Путь к недокачанному файлу.
pub fn part_path(models_dir: &Path, name: &str) -> PathBuf {
    models_dir.join(format!(".{}{PART_SUFFIX}", file_name(name)))
}

/// Есть ли модель на диске.
///
/// Пустой файл моделью не считается: так выглядит прерванный `touch` и
/// неудачное копирование.
pub fn is_installed(models_dir: &Path, name: &str) -> bool {
    std::fs::metadata(model_path(models_dir, name))
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Имена моделей, лежащих в каталоге.
///
/// Читается ФАЙЛОВАЯ СИСТЕМА, а не каталог [`MODELS`]: положенные руками веса
/// обязаны быть видны в статусе, иначе человек смотрел бы на «моделей нет»,
/// имея гигабайт на диске.
pub fn installed(models_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let stripped = name
                .strip_prefix(MODEL_FILE_PREFIX)?
                .strip_suffix(MODEL_FILE_SUFFIX)?
                .to_string();
            let len = e.metadata().ok()?.len();
            (len > 0).then_some(stripped)
        })
        .collect();
    out.sort();
    out
}

/// Состояние одной модели для `GET /api/meet/models`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelStatus {
    pub name: String,
    pub file: String,
    pub kind: ModelKind,
    /// Ожидаемый размер, байт. `0` — модель не из каталога.
    pub size_bytes: u64,
    /// Сколько лежит на диске сейчас. `None` — файла нет.
    pub on_disk_bytes: Option<u64>,
    pub downloaded: bool,
    /// Выбрана ли она в конфиге.
    pub selected: bool,
    /// Известна ли она плагину (иначе — положена руками).
    pub known: bool,
    pub note: String,
}

/// Каталог моделей с их состоянием на диске.
pub fn catalog(models_dir: &Path, selected: &str) -> Vec<ModelStatus> {
    let mut out: Vec<ModelStatus> = MODELS
        .iter()
        .map(|m| {
            let path = models_dir.join(m.file);
            let on_disk = std::fs::metadata(&path).ok().map(|md| md.len());
            ModelStatus {
                name: m.name.to_string(),
                file: m.file.to_string(),
                kind: m.kind,
                size_bytes: m.size_bytes,
                on_disk_bytes: on_disk,
                downloaded: on_disk.is_some_and(|n| n > 0),
                selected: m.name == selected,
                known: true,
                note: m.note.to_string(),
            }
        })
        .collect();

    // Чужие веса, положенные руками, тоже обязаны быть видны.
    for name in installed(models_dir) {
        if out.iter().any(|m| m.name == name) {
            continue;
        }
        let path = model_path(models_dir, &name);
        let on_disk = std::fs::metadata(&path).ok().map(|md| md.len());
        out.push(ModelStatus {
            name: name.clone(),
            file: file_name(&name),
            kind: ModelKind::Transcribe,
            size_bytes: 0,
            on_disk_bytes: on_disk,
            downloaded: true,
            selected: name == selected,
            known: false,
            note: "положена вручную".to_string(),
        });
    }
    out
}

/// Удаляет модель с диска.
pub fn delete(models_dir: &Path, name: &str) -> Result<(), MeetError> {
    let path = model_path(models_dir, name);
    if !path.is_file() {
        return Err(MeetError::NotFound);
    }
    std::fs::remove_file(&path)
        .map_err(|e| MeetError::Internal(format!("модель не удалилась: {e}")))
}

// --- проверка файла ------------------------------------------------------------

/// SHA-256 файла, посчитанный потоково.
///
/// Потоково, а не `read_to_end`: модель весит 1,6 ГБ, и разово поднимать её в
/// память ради хэша значит платить полутора гигабайтами RSS на пустом месте.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Проверяет скачанный файл: магия, размер, контрольная сумма.
///
/// Порядок проверок от дешёвой к дорогой: магия читает четыре байта, размер —
/// метаданные, и только SHA-256 проходит по всему файлу. HTML-страница ошибки
/// вместо весов отсеивается на первом же шаге.
pub fn verify_file(path: &Path, spec: Option<&ModelSpec>) -> Result<(), String> {
    let mut head = [0u8; 4];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path).map_err(|e| format!("файл не читается: {e}"))?;
        f.read_exact(&mut head)
            .map_err(|_| "файл короче четырёх байт — это не модель".to_string())?;
    }
    if head != GGML_MAGIC {
        return Err(format!(
            "не файл GGML: первые байты {head:?}, ожидались {GGML_MAGIC:?} (скорее всего пришла страница ошибки, а не веса)"
        ));
    }

    let Some(spec) = spec else {
        // Неизвестная модель: сверять не с чем, магии достаточно.
        return Ok(());
    };

    let len = std::fs::metadata(path)
        .map_err(|e| format!("размер не читается: {e}"))?
        .len();
    if len != spec.size_bytes {
        return Err(format!(
            "размер {len} вместо {} — загрузка оборвалась",
            spec.size_bytes
        ));
    }

    let got = sha256_file(path).map_err(|e| format!("контрольная сумма не считается: {e}"))?;
    if got != spec.sha256 {
        return Err(format!(
            "контрольная сумма {got} вместо {} — файл повреждён",
            spec.sha256
        ));
    }
    Ok(())
}

// --- скачивание ----------------------------------------------------------------

/// Источник байтов модели.
///
/// Трейт, а не прямой вызов reqwest, ровно ради тестов: гонять в них живой
/// HuggingFace значило бы качать полтора гигабайта на каждый прогон и падать
/// без сети.
#[async_trait::async_trait]
pub trait ModelSource: Send + Sync {
    /// Скачивает `url` в `dest`, отдавая прогресс «скачано / всего».
    async fn fetch(
        &self,
        url: &str,
        dest: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<(), String>;
}

/// Настоящий источник: HTTP через reqwest со стримингом на диск.
pub struct HttpModelSource;

#[async_trait::async_trait]
impl ModelSource for HttpModelSource {
    async fn fetch(
        &self,
        url: &str,
        dest: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
    ) -> Result<(), String> {
        use futures_util::StreamExt;
        use std::io::Write;

        let resp = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .map_err(|e| format!("запрос не ушёл: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("сервер ответил {}", resp.status()));
        }
        let total = resp.content_length();
        let mut file =
            std::fs::File::create(dest).map_err(|e| format!("файл не создался: {e}"))?;
        let mut done = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("обрыв загрузки: {e}"))?;
            file.write_all(&chunk)
                .map_err(|e| format!("запись не удалась: {e}"))?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        file.flush().map_err(|e| format!("сброс на диск не удался: {e}"))?;
        Ok(())
    }
}

/// Скачивает модель В КАТАЛОГ, проверяет и укладывает под правильным именем.
///
/// **Зовётся ТОЛЬКО из роута скачивания.** Ни один автоматический путь —
/// старт записи, пост-обработка, повтор этапа — не имеет права её вызвать; на
/// это есть тест, обходящий исходники.
///
/// Укладка атомарна по построению: сначала `.<имя>.part` в том же каталоге,
/// потом проверка, и только потом `rename`. Обрыв на любом шаге оставляет
/// файл, который [`installed`] не считает моделью, — а не «модель», на
/// которой whisper упадёт через полчаса ожидания.
pub async fn download_model(
    models_dir: &Path,
    name: &str,
    source: &dyn ModelSource,
    progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<PathBuf, MeetError> {
    let spec = spec(name).ok_or_else(|| {
        MeetError::BadRequest(format!(
            "модель {name:?} не из каталога; известные: {}",
            MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
        ))
    })?;
    download_with_spec(models_dir, spec, source, progress).await
}

/// То же, но с явной спецификацией.
///
/// Существует ради тестов: проверить укладку и контрольную сумму на настоящей
/// модели значило бы качать полтора гигабайта, а подменить проверку — снять
/// её. Синтетическая спецификация с честно посчитанным SHA-256 маленького
/// файла проверяет ровно тот же код.
pub async fn download_with_spec(
    models_dir: &Path,
    spec: &ModelSpec,
    source: &dyn ModelSource,
    progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<PathBuf, MeetError> {
    let name = spec.name;
    std::fs::create_dir_all(models_dir)
        .map_err(|e| MeetError::Internal(format!("каталог моделей не создался: {e}")))?;
    let final_path = models_dir.join(spec.file);
    if final_path.is_file() {
        return Ok(final_path);
    }
    let tmp = models_dir.join(format!(".{}{PART_SUFFIX}", spec.file));
    let _ = std::fs::remove_file(&tmp);

    let fetched = source.fetch(spec.url, &tmp, progress).await;
    let verified = fetched.and_then(|()| verify_file(&tmp, Some(spec)));
    if let Err(e) = verified {
        // Мусор не остаётся: недокачанный файл под правильным именем — это
        // полчаса ожидания и падение whisper вместо честной ошибки сейчас.
        let _ = std::fs::remove_file(&tmp);
        return Err(MeetError::Internal(format!(
            "модель {name} не скачалась: {e}"
        )));
    }

    std::fs::rename(&tmp, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        MeetError::Internal(format!("модель не уложилась на место: {e}"))
    })?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Имена, размеры и хэши каталога — ЗАМОРОЖЕННЫЕ данные.
    ///
    /// Все три взяты из заголовков HuggingFace (`x-linked-size`,
    /// `x-linked-etag`), а хэш модели по умолчанию сверен с локальной копией
    /// командой `shasum -a 256`. Опечатка здесь означала бы, что честно
    /// скачанная модель отвергается проверкой.
    #[test]
    fn the_catalog_is_frozen_and_self_consistent() {
        assert_eq!(MODELS.len(), 6);
        assert_eq!(
            spec(DEFAULT_MODEL).map(|s| s.size_bytes),
            Some(574_041_195),
            "размер модели по умолчанию измерен на живом файле"
        );
        assert_eq!(
            spec(DEFAULT_MODEL).map(|s| s.sha256),
            Some("394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2")
        );
        for m in MODELS {
            assert!(m.file.starts_with(MODEL_FILE_PREFIX) && m.file.ends_with(MODEL_FILE_SUFFIX));
            assert!(m.url.ends_with(m.file), "url и имя файла разошлись: {}", m.name);
            assert_eq!(m.sha256.len(), 64, "{}: не SHA-256", m.name);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(m.size_bytes > 0);
        }
        assert_eq!(vad_spec().name, "silero-v5.1.2");
        assert_eq!(file_name("large-v3-turbo-q5_0"), "ggml-large-v3-turbo-q5_0.bin");
        // Чужая модель — по соглашению.
        assert_eq!(file_name("my-own"), "ggml-my-own.bin");
    }

    fn write_model(dir: &Path, file: &str, body: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), body).unwrap();
    }

    #[test]
    fn installed_reads_the_disk_including_hand_placed_weights() {
        let dir = tempfile::tempdir().unwrap();
        assert!(installed(dir.path()).is_empty());

        write_model(dir.path(), "ggml-base.bin", b"lmgg....");
        write_model(dir.path(), "ggml-my-own.bin", b"lmgg....");
        // Пустой файл моделью не считается.
        write_model(dir.path(), "ggml-empty.bin", b"");
        // Посторонние файлы — тоже.
        write_model(dir.path(), "readme.txt", b"hi");

        assert_eq!(installed(dir.path()), vec!["base", "my-own"]);
        assert!(is_installed(dir.path(), "base"));
        assert!(is_installed(dir.path(), "my-own"));
        assert!(!is_installed(dir.path(), "empty"));
        assert!(!is_installed(dir.path(), "large-v3-turbo-q5_0"));

        let cat = catalog(dir.path(), "base");
        assert_eq!(cat.len(), MODELS.len() + 1, "чужая модель обязана быть видна");
        let base = cat.iter().find(|m| m.name == "base").unwrap();
        assert!(base.downloaded && base.selected && base.known);
        let own = cat.iter().find(|m| m.name == "my-own").unwrap();
        assert!(own.downloaded && !own.known);
        let turbo = cat.iter().find(|m| m.name == DEFAULT_MODEL).unwrap();
        assert!(!turbo.downloaded && turbo.on_disk_bytes.is_none());
    }

    #[test]
    fn verification_rejects_an_error_page_a_short_file_and_a_bad_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.bin");

        std::fs::write(&path, b"<html>502 Bad Gateway</html>").unwrap();
        let err = verify_file(&path, spec("base")).unwrap_err();
        assert!(err.contains("не файл GGML"), "{err}");

        // Магия правильная, но размер не тот.
        std::fs::write(&path, b"lmgg\x00\x00\x00\x00").unwrap();
        let err = verify_file(&path, spec("base")).unwrap_err();
        assert!(err.contains("размер"), "{err}");

        // Совсем короткий файл.
        std::fs::write(&path, b"lm").unwrap();
        let err = verify_file(&path, spec("base")).unwrap_err();
        assert!(err.contains("короче"), "{err}");

        // Неизвестной модели хватает магии.
        std::fs::write(&path, b"lmgg\x00\x00\x00\x00").unwrap();
        verify_file(&path, None).unwrap();
    }

    #[test]
    fn the_hash_is_computed_over_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "SHA-256 пустого файла — известная константа"
        );
        // Больше буфера чтения: проверяем именно потоковую сборку.
        std::fs::write(&path, vec![7u8; (1 << 20) + 13]).unwrap();
        assert_eq!(sha256_file(&path).unwrap().len(), 64);
    }

    /// Источник, который отдаёт заданные байты и считает вызовы.
    struct StubSource {
        body: Vec<u8>,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ModelSource for StubSource {
        async fn fetch(
            &self,
            _url: &str,
            dest: &Path,
            progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
        ) -> Result<(), String> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::write(dest, &self.body).map_err(|e| e.to_string())?;
            progress(self.body.len() as u64, Some(self.body.len() as u64));
            Ok(())
        }
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ: оборванная загрузка не оставляет файл, который
    /// выглядит готовой моделью.
    #[tokio::test]
    async fn an_interrupted_download_leaves_nothing_that_looks_like_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // Половина модели: магия на месте, размер не тот.
        let source = StubSource {
            body: b"lmgg-half-a-model".to_vec(),
            calls: calls.clone(),
        };

        let err = download_model(dir.path(), "base", &source, &|_, _| {})
            .await
            .unwrap_err();
        assert!(matches!(err, MeetError::Internal(_)), "{err}");
        assert!(err.message().contains("размер"), "{}", err.message());

        assert!(!is_installed(dir.path(), "base"), "битый файл выглядит моделью");
        assert!(installed(dir.path()).is_empty(), "в каталоге остался мусор");
        assert!(!part_path(dir.path(), "base").exists(), "остался .part");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Успешная загрузка укладывается под правильным именем И ПРОХОДИТ
    /// ПОЛНУЮ ПРОВЕРКУ, включая SHA-256.
    ///
    /// Спецификация синтетическая, а хэш настоящий: качать 141 МиБ ради теста
    /// нельзя, а подменять проверку — значит снять её. Здесь работает ровно
    /// тот же код укладки, что и на живой модели.
    #[tokio::test]
    async fn a_finished_download_lands_under_the_final_name_after_a_full_check() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"lmgg\x01\x02\x03\x04-tiny-test-model".to_vec();
        let hash = {
            let f = dir.path().join("probe");
            std::fs::write(&f, &body).unwrap();
            let h = sha256_file(&f).unwrap();
            std::fs::remove_file(&f).unwrap();
            h
        };
        let spec = ModelSpec {
            name: "тестовая",
            file: "ggml-тестовая.bin",
            url: "https://example.invalid/ggml-тестовая.bin",
            size_bytes: body.len() as u64,
            sha256: Box::leak(hash.into_boxed_str()),
            kind: ModelKind::Transcribe,
            note: "синтетическая",
        };

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = StubSource {
            body: body.clone(),
            calls: calls.clone(),
        };
        let seen = std::sync::Mutex::new(Vec::new());
        let path = download_with_spec(dir.path(), &spec, &source, &|done, total| {
            seen.lock().unwrap().push((done, total));
        })
        .await
        .unwrap();

        assert_eq!(path, dir.path().join(spec.file));
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(is_installed(dir.path(), "тестовая"));
        assert!(!dir.path().join(format!(".{}{PART_SUFFIX}", spec.file)).exists());
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[(body.len() as u64, Some(body.len() as u64))],
            "прогресс обязан доезжать до вызывающего"
        );

        // Повторный вызов не качает заново.
        download_with_spec(dir.path(), &spec, &source, &|_, _| {})
            .await
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Битая контрольная сумма отвергается, даже когда размер сошёлся.
    #[tokio::test]
    async fn a_corrupted_body_of_the_right_length_is_rejected_by_the_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            name: "тестовая",
            file: "ggml-тестовая.bin",
            url: "https://example.invalid/x.bin",
            size_bytes: 8,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            kind: ModelKind::Transcribe,
            note: "синтетическая",
        };
        let source = StubSource {
            body: b"lmgg1234".to_vec(),
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let err = download_with_spec(dir.path(), &spec, &source, &|_, _| {})
            .await
            .unwrap_err();
        assert!(err.message().contains("контрольная сумма"), "{}", err.message());
        assert!(installed(dir.path()).is_empty(), "битый файл остался в каталоге");
    }

    /// Неизвестное имя отвергается ДО начала загрузки.
    #[tokio::test]
    async fn an_unknown_model_is_refused_before_any_bytes_move() {
        let dir = tempfile::tempdir().unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = StubSource {
            body: b"lmggXXXX".to_vec(),
            calls: calls.clone(),
        };
        let err = download_model(dir.path(), "no-such-model", &source, &|_, _| {})
            .await
            .unwrap_err();
        assert!(matches!(err, MeetError::BadRequest(_)), "{err}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "качать не начали");
    }

    #[test]
    fn deleting_a_missing_model_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(delete(dir.path(), "base").unwrap_err(), MeetError::NotFound);
        write_model(dir.path(), "ggml-base.bin", b"lmgg");
        delete(dir.path(), "base").unwrap();
        assert!(!is_installed(dir.path(), "base"));
    }

    /// **АВТОПОДКАЧКИ НЕТ.** Тест обходит исходники крейта и проверяет, что
    /// [`download_model`] зовут только роут скачивания и тесты этого модуля.
    ///
    /// Проверка структурная, а не на слово: добавить «ну а если модели нет,
    /// давайте скачаем» в пайплайн — работа на одну строку, после которой
    /// пользователь ждёт полтора гигабайта в момент, когда встреча уже идёт.
    #[test]
    fn nothing_but_the_route_starts_a_download() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut callers = Vec::new();
        let mut checked = 0usize;
        visit(&root, &mut |path, src| {
            checked += 1;
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "models.rs" {
                return; // определение и его тесты
            }
            let production = src.split("#[cfg(test)]").next().unwrap_or(src);
            let calls = production
                .lines()
                .map(str::trim)
                .filter(|l| !l.starts_with("//"))
                .any(|l| l.contains("download_model("));
            if calls {
                callers.push(rel);
            }
        });
        assert!(checked > 10, "обход исходников ничего не нашёл");
        assert_eq!(
            callers,
            vec!["routes/models.rs".to_string()],
            "скачивание начинается не только по явному действию пользователя"
        );
    }

    fn visit(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, f);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let src = std::fs::read_to_string(&path).unwrap();
                f(&path, &src);
            }
        }
    }
}
