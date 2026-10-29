//! Пробы платформ: селекторы как ДАННЫЕ.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Что здесь происходит
//!
//! Встроенный [`BUILTIN_PROBES`] (`bot/probes.toml`, `include_str!`)
//! переопределяется пользовательским `~/.config/forge/meet.probes.toml`
//! ПОЭЛЕМЕНТНО: файл с одной пробой меняет только её. Вёрстка площадок
//! меняется чаще, чем мы выпускаем релизы, и правка TOML обязана чинить бота
//! БЕЗ ПЕРЕСБОРКИ БИНАРЯ — в этом весь смысл механизма.
//!
//! # Инвариант, который держит всю фазу
//!
//! **ЗАПИСЬ НЕ ЗАВИСИТ НИ ОТ ОДНОЙ ПРОБЫ.** Пробы влияют на имена спикеров и
//! на удобство входа; звук пишется тапами `capture.js`, которые про этот файл
//! не знают вовсе. Промахнулись все пробы разом — запись идёт, участники
//! называются «Участник N», конец встречи ловится независимыми сигналами.
//! На это есть два теста: один проверяет деградацию имён, второй —
//! что ни один селектор отсюда не встречается в исходниках захвата.
//!
//! # Почему `BTreeMap`, а не структура с полями
//!
//! Потому что источник — файл, который правит человек, и неизвестное имя
//! пробы обязано давать `warn` с именем, а не отказ разбора всего файла (по
//! тем же соображениям, что [`crate::config::KNOWN_KEYS`]). Типизованная
//! структура дала бы `deny_unknown_fields` или молчаливый пропуск — оба хуже.
//! Имена проб при этом заморожены константами [`probe`], и опечатка в коде
//! невозможна.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Встроенный набор проб. Правится вместе с кодом, но перекрывается
/// пользовательским файлом без пересборки.
pub const BUILTIN_PROBES: &str = include_str!("probes.toml");

/// Имена проб. **Заморожены**: их знает и `probes.toml`, и `observe.js`.
pub mod probe {
    /// Интерстишал «Продолжить в браузере».
    pub const CONTINUE_IN_BROWSER: &str = "continue_in_browser";
    /// Поле гостевого имени. Его ОТСУТСТВИЕ — это ветка залогиненного профиля.
    pub const NAME_INPUT: &str = "name_input";
    /// Кнопка входа во встречу.
    pub const JOIN_BUTTON: &str = "join_button";
    /// Кнопка «выключить микрофон». Присутствует, когда микрофон ВКЛЮЧЁН.
    pub const MIC_MUTE_BUTTON: &str = "mic_mute_button";
    /// Кнопка «включить микрофон». Присутствует, когда микрофон ВЫКЛЮЧЕН.
    pub const MIC_UNMUTE_BUTTON: &str = "mic_unmute_button";
    /// Кнопка «выключить камеру».
    pub const CAM_DISABLE_BUTTON: &str = "cam_disable_button";
    /// Кнопка «включить камеру».
    pub const CAM_ENABLE_BUTTON: &str = "cam_enable_button";
    /// Кнопка выхода из встречи.
    pub const LEAVE_BUTTON: &str = "leave_button";
    /// Плитка либо строка участника.
    pub const PARTICIPANT: &str = "participant";
    /// Имя внутри плитки участника.
    pub const PARTICIPANT_NAME: &str = "participant_name";
    /// Признак СВОЕЙ плитки — единственная проба, которая ВЫЧИТАЕТ элемент из
    /// выборки, а не добавляет.
    ///
    /// Появилась в Ф13 из-за живого замера на vcc.airlabs.pro: LiveKit рисует
    /// бота такой же `.lk-participant-tile`, что и людей, и без вычитания бот
    /// попадал в собственный список участников. Цена ошибки видна не сразу:
    /// «Участник 1» в транскрипте оказывался ботом, а `others()` спасал только
    /// пока имя бота в DOM совпадало с именем из заявки посимвольно.
    ///
    /// Пустая проба — законное состояние: у площадки, не помечающей свою
    /// плитку, вычитать нечего, и список остаётся прежним.
    pub const LOCAL_PARTICIPANT_MARKER: &str = "local_participant_marker";
    /// Индикатор «сейчас говорит» на плитке или внутри неё.
    pub const SPEAKING_MARKER: &str = "speaking_marker";
    /// Атрибуты (`attr:имя`), из которых берётся стабильный ключ участника.
    pub const PARTICIPANT_KEY_ATTR: &str = "participant_key_attr";
    /// Отдельный элемент «активный спикер», если он у площадки есть.
    pub const ACTIVE_SPEAKER: &str = "active_speaker";
    /// DOM-маркер конца встречи.
    pub const MEETING_ENDED_MARKER: &str = "meeting_ended_marker";
    /// Подстроки пост-звонкового адреса (НЕ селекторы).
    pub const POST_CALL_URL: &str = "post_call_url";
    /// Маркер лобби.
    pub const LOBBY_MARKER: &str = "lobby_marker";
    /// Маркер капчи. Капчу мы не решаем — только зовём человека.
    pub const CAPTCHA_MARKER: &str = "captcha_marker";
    /// Маркер ОТКАЗА во входе: организатор отклонил заявку либо анонимный
    /// вход запрещён настройками встречи.
    ///
    /// Появился в Ф8 из-за продуктового ограничения Google Meet: при
    /// ограниченном доступе хоста заявка анонимного участника отклоняется
    /// АВТОМАТИЧЕСКИ, за секунды. Без этого маркера бот молча стоял бы до
    /// таймаута входа и звал человека с причиной `unknown` — то есть
    /// продуктовое ограничение выглядело бы нашей поломкой.
    pub const JOIN_DENIED_MARKER: &str = "join_denied_marker";
    /// Кнопка «подключить звук компьютера» ПОСЛЕ входа.
    ///
    /// Появилась в Ф10: веб-клиент Zoom не подключает аудио сам, и без этого
    /// клика в странице нет ни одного источника звука вообще — записывать
    /// нечего. У остальных площадок проба пустая, и шаг просто пропускается.
    pub const JOIN_AUDIO_BUTTON: &str = "join_audio_button";
    /// Сочетание клавиш «переключить микрофон». **НЕ селектор.**
    ///
    /// Формат — `ctrl+d`, `meta+shift+a`; модификаторы `ctrl`, `alt`, `shift`,
    /// `meta` в любом порядке, последней идёт буква либо цифра.
    ///
    /// Зачем вообще: `aria-label` кнопок Google Meet ЛОКАЛИЗОВАН и
    /// переименовывается, а `Ctrl+D` и `Ctrl+E` не менялись годами. Но
    /// сочетание — ПЕРЕКЛЮЧАТЕЛЬ, а не «выключить»: вслепую его слать нельзя
    /// (см. [`super::chrome::hotkey_allowed`]).
    pub const MIC_TOGGLE_HOTKEY: &str = "mic_toggle_hotkey";
    /// Сочетание клавиш «переключить камеру». **НЕ селектор.**
    pub const CAM_TOGGLE_HOTKEY: &str = "cam_toggle_hotkey";
}

/// Полный список имён проб — он же проверка опечаток в TOML.
pub const PROBE_NAMES: &[&str] = &[
    probe::CONTINUE_IN_BROWSER,
    probe::NAME_INPUT,
    probe::JOIN_BUTTON,
    probe::MIC_MUTE_BUTTON,
    probe::MIC_UNMUTE_BUTTON,
    probe::CAM_DISABLE_BUTTON,
    probe::CAM_ENABLE_BUTTON,
    probe::LEAVE_BUTTON,
    probe::PARTICIPANT,
    probe::PARTICIPANT_NAME,
    probe::LOCAL_PARTICIPANT_MARKER,
    probe::SPEAKING_MARKER,
    probe::PARTICIPANT_KEY_ATTR,
    probe::ACTIVE_SPEAKER,
    probe::MEETING_ENDED_MARKER,
    probe::POST_CALL_URL,
    probe::LOBBY_MARKER,
    probe::CAPTCHA_MARKER,
    probe::JOIN_DENIED_MARKER,
    probe::JOIN_AUDIO_BUTTON,
    probe::MIC_TOGGLE_HOTKEY,
    probe::CAM_TOGGLE_HOTKEY,
];

/// Пробы, которые НЕ ЯВЛЯЮТСЯ селекторами.
///
/// Список нужен стражу «селектор не протёк в код захвата»: сравнивать
/// `post_call_url` (подстрока адреса) или `ctrl+d` (сочетание клавиш) с
/// исходниками тапов бессмысленно, а молчаливое исключение по виду строки
/// однажды пропустило бы настоящий селектор.
pub const NON_SELECTOR_PROBES: &[&str] = &[
    probe::POST_CALL_URL,
    probe::MIC_TOGGLE_HOTKEY,
    probe::CAM_TOGGLE_HOTKEY,
];

/// Ключ мок-встречи в книге проб. Не [`crate::providers::Platform`]: мок —
/// тестовый стенд, а не платформа, и в реестре платформ ему не место.
pub const MOCK_PLATFORM: &str = "mock";

/// Набор проб одной платформы.
///
/// Пустой набор — ЗАКОННОЕ состояние (например, платформа ещё не размечена):
/// бот войдёт ассистированно, запишет звук и назовёт участников «Участник N».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeSet {
    candidates: BTreeMap<String, Vec<String>>,
}

impl ProbeSet {
    /// Пустой набор — та самая полная деградация.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Кандидаты пробы по порядку. Неизвестное имя — пустой срез.
    pub fn get(&self, name: &str) -> &[String] {
        self.candidates.get(name).map_or(&[], Vec::as_slice)
    }

    /// Есть ли у пробы хоть один кандидат.
    pub fn has(&self, name: &str) -> bool {
        !self.get(name).is_empty()
    }

    /// Первый кандидат пробы. Для проб-одиночек — сочетаний клавиш — это и
    /// есть всё значение; для наборов селекторов это просто высший приоритет.
    pub fn first(&self, name: &str) -> Option<&str> {
        self.get(name).first().map(String::as_str)
    }

    /// Заданы ли пробы вообще.
    pub fn is_empty(&self) -> bool {
        self.candidates.values().all(Vec::is_empty)
    }

    /// Сколько проб непусты — величина для лога и для `GET /status`.
    pub fn filled(&self) -> usize {
        self.candidates.values().filter(|v| !v.is_empty()).count()
    }

    /// Задаёт пробу (используется тестами и слиянием).
    pub fn set(&mut self, name: &str, candidates: Vec<String>) {
        self.candidates.insert(name.to_string(), candidates);
    }

    /// ПОЭЛЕМЕНТНОЕ наложение: заданные в `other` пробы заменяют свои, всё
    /// остальное остаётся. Именно поэтому пользовательский файл с одной
    /// строкой чинит одну пробу, а не обнуляет платформу.
    pub fn overlay(&mut self, other: &ProbeSet) {
        for (name, candidates) in &other.candidates {
            self.candidates.insert(name.clone(), candidates.clone());
        }
    }

    /// Все кандидаты всех проб — вход для стража «селекторы не протекли в
    /// код захвата».
    pub fn all_candidates(&self) -> Vec<&str> {
        self.candidates
            .values()
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// Кандидаты тех проб, которые ОБЯЗАНЫ быть селекторами.
    ///
    /// Отсеиваются пробы из [`NON_SELECTOR_PROBES`] — подстроки адреса и
    /// сочетания клавиш. Именно по имени пробы, а не по виду строки: «похоже
    /// на селектор» — не проверка, а угадывание.
    pub fn selector_candidates(&self) -> Vec<(&str, &str)> {
        self.candidates
            .iter()
            .filter(|(name, _)| !NON_SELECTOR_PROBES.contains(&name.as_str()))
            .flat_map(|(name, list)| list.iter().map(move |c| (name.as_str(), c.as_str())))
            .collect()
    }

    /// JSON для `observe.js`. Пробы уезжают в страницу ДАННЫМИ — так же, как
    /// конфиг захвата, и по той же причине: селектор с кавычкой, склеенный в
    /// текст скрипта, сломал бы всё выражение.
    pub fn to_json(&self) -> String {
        let mut map = serde_json::Map::new();
        for name in PROBE_NAMES {
            let list: Vec<serde_json::Value> = self
                .get(name)
                .iter()
                .map(|c| serde_json::Value::String(c.clone()))
                .collect();
            map.insert((*name).to_string(), serde_json::Value::Array(list));
        }
        serde_json::Value::Object(map).to_string()
    }
}

/// Книга проб: платформа → набор.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeBook {
    platforms: BTreeMap<String, ProbeSet>,
    /// Откуда взято переопределение. `None` — только встроенные значения.
    source: Option<PathBuf>,
}

impl ProbeBook {
    /// Только встроенный файл.
    ///
    /// Битый встроенный TOML невозможен (он в репозитории и покрыт тестом),
    /// но даже в этом случае возвращается пустая книга, а не паника: плагин
    /// обязан стартовать при любом состоянии окружения.
    pub fn builtin() -> Self {
        match Self::parse(BUILTIN_PROBES) {
            Ok(book) => book,
            Err(e) => {
                tracing::error!(
                    target: "forge_meet",
                    error = %e,
                    "forge-meet: встроенный probes.toml не разбирается — работаю совсем без проб"
                );
                Self::default()
            }
        }
    }

    /// Разбирает TOML книги проб.
    ///
    /// Неизвестные имена проб дают `warn` и СОХРАНЯЮТСЯ: файл может приехать
    /// от более новой версии devforge, и стирать из него незнакомое — значит
    /// ломать откат версии.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let table: toml::Table = raw.parse::<toml::Table>().map_err(|e| e.to_string())?;
        let mut platforms = BTreeMap::new();
        for (key, value) in table {
            // `version` и прочие скаляры верхнего уровня — не платформы.
            let Some(section) = value.as_table() else {
                continue;
            };
            let mut set = ProbeSet::empty();
            for (name, list) in section {
                if !PROBE_NAMES.contains(&name.as_str()) {
                    tracing::warn!(
                        target: "forge_meet",
                        platform = %key,
                        probe = %name,
                        "forge-meet: неизвестное имя пробы, оставляю как есть"
                    );
                }
                let candidates: Vec<String> = match list {
                    toml::Value::Array(items) => items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    // Одиночная строка вместо массива — частая описка руками.
                    // Принимаем как массив из одного элемента: отказ здесь
                    // стоил бы пробы, а помочь ничем бы не помог.
                    toml::Value::String(s) => vec![s.clone()],
                    _ => {
                        tracing::warn!(
                            target: "forge_meet",
                            platform = %key,
                            probe = %name,
                            "forge-meet: проба должна быть массивом строк, игнорирую"
                        );
                        continue;
                    }
                };
                set.set(name, candidates);
            }
            platforms.insert(key, set);
        }
        Ok(Self {
            platforms,
            source: None,
        })
    }

    /// Накладывает другую книгу поэлементно.
    pub fn overlay(&mut self, other: &ProbeBook) {
        for (platform, set) in &other.platforms {
            self.platforms
                .entry(platform.clone())
                .or_default()
                .overlay(set);
        }
    }

    /// Набор платформы. Незнакомая платформа — пустой набор, а не ошибка:
    /// именно так выглядит обещанная деградация.
    pub fn for_platform(&self, key: &str) -> ProbeSet {
        self.platforms.get(key).cloned().unwrap_or_default()
    }

    /// Известные платформы книги.
    pub fn platforms(&self) -> Vec<&str> {
        self.platforms.keys().map(String::as_str).collect()
    }

    /// Откуда пришло переопределение.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Встроенные пробы, перекрытые пользовательским файлом.
    ///
    /// **Никогда не возвращает ошибку и никогда не паникует.** Битый
    /// пользовательский файл — это `warn` и встроенные значения: его правят
    /// руками ровно тогда, когда бот сломался, и опечатка в такой момент не
    /// должна отнимать ещё и запись.
    pub fn load() -> Self {
        Self::load_from(&probes_path())
    }

    /// То же с явным путём — герметичная форма для тестов.
    pub fn load_from(path: &Path) -> Self {
        let mut book = Self::builtin();
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return book,
            Err(e) => {
                tracing::warn!(
                    target: "forge_meet",
                    path = %path.display(),
                    error = %e,
                    "forge-meet: файл проб не читается, беру встроенные"
                );
                return book;
            }
        };
        match Self::parse(&raw) {
            Ok(user) => {
                book.overlay(&user);
                book.source = Some(path.to_path_buf());
                tracing::info!(
                    target: "forge_meet",
                    path = %path.display(),
                    platforms = ?user.platforms(),
                    "forge-meet: пробы переопределены пользовательским файлом"
                );
            }
            Err(e) => tracing::warn!(
                target: "forge_meet",
                path = %path.display(),
                error = %e,
                "forge-meet: файл проб не разбирается, беру встроенные"
            ),
        }
        book
    }
}

/// Дефолтный путь пользовательского файла проб.
pub fn default_probes_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".config/forge/meet.probes.toml"),
        None => PathBuf::from("./meet.probes.toml"),
    }
}

/// Фактический путь: непустой `FORGE_MEET_PROBES_PATH` побеждает.
///
/// Переменная существует ради герметичности тестов ровно так же, как
/// `FORGE_MEET_CONFIG_PATH`: без неё `cargo test` читал бы живой файл
/// разработчика, и проба, поправленная им вчера вечером, роняла бы сборку.
pub fn probes_path() -> PathBuf {
    match std::env::var_os("FORGE_MEET_PROBES_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_probes_path(),
    }
}

// ---------------------------------------------------------------------------
// Выражения для страницы
// ---------------------------------------------------------------------------

/// Исходник `observe.js` без `export` — в таком виде он вставляется в
/// `Runtime.evaluate`.
///
/// Тот же снимающий маркер, что у `pcm.js` и `vad.js`: модуль обязан
/// оставаться модулем для `node --test` и переставать им быть в браузере.
pub fn observe_source() -> String {
    super::inject::strip_exports(super::inject::OBSERVE_JS)
}

/// Выражение, отдающее ЭЛЕМЕНТ первой сработавшей пробы (или `null`).
///
/// Скармливается `CdpPage::click_expression`, который сам вычислит
/// `getBoundingClientRect` и кликнет по центру. Клик по вычисленному боксу, а
/// не по координатам со скриншота, — прямое следствие живой пробы Ф0: клик по
/// снятым с картинки координатам промахнулся, скриншот отмасштабирован.
pub fn probe_element_expression(set: &ProbeSet, name: &str) -> String {
    let candidates = serde_json::to_string(set.get(name)).unwrap_or_else(|_| "[]".to_string());
    format!(
        "(function () {{\n{src}\nvar hit = forgeProbeHit(document, {candidates});\n\
         return hit ? hit.el : null;\n}})()",
        src = observe_source()
    )
}

/// Выражение, отдающее JSON-снимок встречи.
///
/// Возвращается СТРОКА, а не объект: `Runtime.evaluate` с
/// `returnByValue: true` сериализует объекты сам, но на глубоких структурах
/// молча теряет часть полей, а на `null`-ах меняет форму. Строка приезжает
/// как есть.
pub fn observe_expression(set: &ProbeSet) -> String {
    format!(
        "(function () {{\n{src}\nreturn JSON.stringify(forgeObserveMeeting(document, {probes}, \
         location, {{perf: performance.now(), unix: Date.now()}}));\n}})()",
        src = observe_source(),
        probes = set.to_json()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Встроенный файл обязан разбираться и содержать Телемост с моком.
    #[test]
    fn the_builtin_book_parses_and_knows_telemost() {
        let book = ProbeBook::builtin();
        let telemost = book.for_platform(crate::providers::Platform::Telemost.as_str());
        assert!(!telemost.is_empty(), "у Телемоста обязаны быть пробы");
        assert_eq!(
            telemost.get(probe::JOIN_BUTTON)[0],
            "[data-testid=\"enter-conference-button\"]",
            "селектор входа взят живой пробой Ф0 и не имеет права молча измениться"
        );
        assert_eq!(
            telemost.get(probe::SPEAKING_MARKER),
            ["[class*=\"rootStroke\"]"],
            "индикатор говорящего — единственный источник спикеров Телемоста"
        );
        // Подстрочность обязательна: классы плиток — CSS-модули с хешами.
        assert!(telemost.get(probe::PARTICIPANT)[0].contains("[class*="));

        let mock = book.for_platform(MOCK_PLATFORM);
        assert!(!mock.is_empty(), "мок-встреча документирует формат и обязана быть в книге");
        assert!(book.platforms().contains(&"zoom"));
    }

    /// Пользовательский файл переопределяет ПОЭЛЕМЕНТНО, а не целиком.
    #[test]
    fn a_user_file_overrides_one_probe_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.probes.toml");
        std::fs::write(
            &path,
            "[telemost]\njoin_button = [\"#new-join\"]\n",
        )
        .unwrap();

        let book = ProbeBook::load_from(&path);
        let set = book.for_platform("telemost");
        assert_eq!(set.get(probe::JOIN_BUTTON), ["#new-join"], "проба не перекрылась");
        assert_eq!(
            set.get(probe::SPEAKING_MARKER),
            ["[class*=\"rootStroke\"]"],
            "перекрытие одной пробы обнулило остальные — это и есть «правка ломает бота»"
        );
        assert_eq!(book.source(), Some(path.as_path()));
    }

    /// Битый TOML НЕ роняет плагин: встроенные значения остаются.
    #[test]
    fn a_broken_user_file_falls_back_to_the_builtin_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meet.probes.toml");
        std::fs::write(&path, "[telemost\njoin_button = [").unwrap();

        let book = ProbeBook::load_from(&path);
        assert_eq!(
            book.for_platform("telemost").get(probe::JOIN_BUTTON),
            ProbeBook::builtin().for_platform("telemost").get(probe::JOIN_BUTTON),
        );
        assert_eq!(book.source(), None, "битый файл не имеет права считаться источником");
    }

    /// Отсутствующий файл — это НОРМА, а не ошибка.
    #[test]
    fn a_missing_user_file_is_normal() {
        let dir = tempfile::tempdir().unwrap();
        let book = ProbeBook::load_from(&dir.path().join("нет-такого.toml"));
        assert!(!book.for_platform("telemost").is_empty());
    }

    /// Новая платформа целиком из пользовательского файла — тоже поддержана:
    /// иначе поддержка пятой площадки требовала бы релиза.
    #[test]
    fn a_user_file_may_add_a_whole_platform() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.toml");
        std::fs::write(&path, "[whereby]\njoin_button = [\"#j\"]\n").unwrap();
        let book = ProbeBook::load_from(&path);
        assert_eq!(book.for_platform("whereby").get(probe::JOIN_BUTTON), ["#j"]);
    }

    /// Незнакомая платформа отдаёт пустой набор — деградация, а не отказ.
    #[test]
    fn an_unknown_platform_degrades_to_an_empty_set() {
        let book = ProbeBook::builtin();
        let set = book.for_platform("нет-такой-площадки");
        assert!(set.is_empty());
        assert_eq!(set.get(probe::JOIN_BUTTON), Vec::<String>::new().as_slice());
        assert_eq!(set.filled(), 0);
    }

    /// JSON для страницы содержит ВСЕ имена проб, включая пустые: `observe.js`
    /// иначе ходил бы по `undefined` и требовал бы защиты в каждой строке.
    #[test]
    fn the_page_json_lists_every_probe_name() {
        let set = ProbeBook::builtin().for_platform("telemost");
        let v: serde_json::Value = serde_json::from_str(&set.to_json()).unwrap();
        for name in PROBE_NAMES {
            assert!(v.get(name).is_some_and(|x| x.is_array()), "нет пробы {name}");
        }
        assert_eq!(v[probe::ACTIVE_SPEAKER], serde_json::json!([]));
    }

    /// Выражения для страницы обязаны быть синтаксически цельными: имя
    /// функции, пробы данными, никаких `export`.
    #[test]
    fn the_page_expressions_carry_the_stripped_module_and_the_probes_as_data() {
        let set = ProbeBook::builtin().for_platform("mock");
        let obs = observe_expression(&set);
        assert!(obs.contains("function forgeObserveMeeting"), "модуль не доехал");
        assert!(!obs.contains("export "), "export в выражении — синтаксическая ошибка");
        assert!(obs.contains("data-testid=\\\"participant\\\""), "пробы не уехали данными");

        let click = probe_element_expression(&set, probe::JOIN_BUTTON);
        assert!(click.contains("forgeProbeHit(document, [\"[data-testid=\\\"join\\\"]\""));
        assert!(click.trim_end().ends_with("})()"), "выражение обязано быть вызовом");
    }

    /// ГЛАВНЫЙ СТРАЖ ФАЗЫ: НИ ОДИН селектор проб не встречается в исходниках
    /// захвата.
    ///
    /// Проверка структурная, а не по вере: протащи кто-нибудь `data-testid`
    /// платформы в `capture.js` — и запись начала бы зависеть от вёрстки,
    /// после чего протухший селектор оставлял бы встречу без звука вместо
    /// «Участник N». Ровно этого инварианта фаза и держится.
    #[test]
    fn no_probe_selector_leaks_into_the_capture_sources() {
        let book = ProbeBook::builtin();
        let capture = [
            super::super::inject::CAPTURE_JS,
            super::super::inject::TRANSPORT_JS,
            super::super::inject::WORKLET_JS,
            super::super::inject::PCM_JS,
            super::super::inject::VAD_JS,
        ];
        for platform in book.platforms() {
            let set = book.for_platform(platform);
            for (name, candidate) in set.selector_candidates() {
                // Кандидаты по тексту и атрибуты сравнивать бессмысленно —
                // это не селекторы.
                if candidate.starts_with("text:") || candidate.starts_with("attr:") {
                    continue;
                }
                for src in capture {
                    assert!(
                        !src.contains(candidate),
                        "селектор {candidate} ({platform}.{name}) протёк в код захвата: \
                         запись стала зависеть от вёрстки"
                    );
                }
            }
        }
    }

    /// Все платформы реестра размечены, и ни одна не пуста.
    ///
    /// Пустой набор — законное состояние механизма, но НЕ законный результат
    /// фаз 8–10: пустая разметка означала бы «Участник N» на каждой встрече
    /// Meet, Teams и Zoom при полностью исправном боте.
    #[test]
    fn every_platform_of_the_registry_is_mapped() {
        let book = ProbeBook::builtin();
        for platform in crate::providers::Platform::ALL {
            let set = book.for_platform(platform.as_str());
            assert!(
                !set.is_empty(),
                "у платформы {} нет ни одной пробы",
                platform.as_str()
            );
            for name in [probe::JOIN_BUTTON, probe::LEAVE_BUTTON, probe::PARTICIPANT] {
                assert!(
                    set.has(name),
                    "у платформы {} не размечена проба {name}",
                    platform.as_str()
                );
            }
        }
    }

    /// ФАКТЫ ЖИВОЙ ПРОБЫ vcc.airlabs.pro, замороженные проверкой.
    ///
    /// Замер Ф13 нашёл то, чего разведка Ф0 не заметила: подписи «Выключить
    /// микрофон» и «Выключить камеру» живут ТОЛЬКО на пре-джойне, а на панели
    /// внутри встречи те же кнопки подписаны «Микрофон» и «Камера», и
    /// состояние висит атрибутом `data-lk-enabled`. Пока в пробах были одни
    /// пре-джойновые тексты, снимок ВНУТРИ встречи давал `mic = unknown`, и
    /// повторное гашение микрофона после входа не находило кнопку вовсе — то
    /// есть бот оставался в чужой встрече с включённым микрофоном.
    ///
    /// Поэтому у обеих пар кандидатов проверяется НАЛИЧИЕ ОБОИХ ЭКРАНОВ.
    #[test]
    fn the_vcc_device_probes_cover_both_screens() {
        let set = ProbeBook::builtin().for_platform(crate::providers::Platform::Vcc.as_str());
        for (name, source, enabled) in [
            (probe::MIC_MUTE_BUTTON, "microphone", "true"),
            (probe::MIC_UNMUTE_BUTTON, "microphone", "false"),
            (probe::CAM_DISABLE_BUTTON, "camera", "true"),
            (probe::CAM_ENABLE_BUTTON, "camera", "false"),
        ] {
            let candidates = set.get(name);
            let panel = format!(".lk-button[data-lk-source=\"{source}\"][data-lk-enabled=\"{enabled}\"]");
            assert!(
                candidates.iter().any(|c| *c == panel),
                "у пробы {name} нет кандидата панели встречи: {candidates:?}"
            );
            assert!(
                candidates.iter().any(|c| c.starts_with("text:")),
                "у пробы {name} нет кандидата пре-джойна: {candidates:?}"
            );
        }
        // `.lk-button` в селекторе не украшение: атрибут `data-lk-source`
        // есть и у ПЛИТКИ участника (`data-lk-source="camera"`, измерено), и
        // без сужения по классу «выключить камеру» кликала бы по чужому видео.
        for name in [probe::CAM_DISABLE_BUTTON, probe::CAM_ENABLE_BUTTON] {
            for c in set.get(name) {
                assert!(
                    !c.starts_with("[data-lk-source"),
                    "кандидат {c:?} пробы {name} поймает плитку участника"
                );
            }
        }
    }

    /// СВОЯ ПЛИТКА ВЫЧИТАЕТСЯ, и это размечено только там, где измерено.
    #[test]
    fn only_vcc_subtracts_its_own_tile() {
        let book = ProbeBook::builtin();
        assert_eq!(
            book.for_platform(crate::providers::Platform::Vcc.as_str())
                .get(probe::LOCAL_PARTICIPANT_MARKER),
            ["[data-lk-local-participant=\"true\"]"],
            "атрибут снят живой пробой и не имеет права молча измениться"
        );
        for platform in crate::providers::Platform::ALL {
            if platform == crate::providers::Platform::Vcc {
                continue;
            }
            assert!(
                book.for_platform(platform.as_str())
                    .get(probe::LOCAL_PARTICIPANT_MARKER)
                    .is_empty(),
                "у {} проба своей плитки заполнена догадкой",
                platform.as_str()
            );
        }
    }

    /// ДЕМОНСТРАЦИЯ ЭКРАНА — НЕ УЧАСТНИК.
    ///
    /// LiveKit рисует трансляцию такой же `.lk-participant-tile` с именем
    /// «<имя>'s screen». На живой встрече пользователя это стоило 15 реплик из
    /// 78, подписанных экраном, и двух фантомов в списке участников.
    /// Единственное, что их отличает, — `data-lk-source`, поэтому обе пробы
    /// обязаны его требовать: и та, что СОБИРАЕТ участников, и та, что
    /// выбирает активного спикера (живьём индикатор речи встаёт на обеих
    /// плитках человека сразу, а плитка экрана идёт в DOM раньше).
    #[test]
    fn the_vcc_probes_ignore_screen_shares() {
        let set = ProbeBook::builtin().for_platform(crate::providers::Platform::Vcc.as_str());
        for name in [probe::PARTICIPANT, probe::ACTIVE_SPEAKER] {
            let candidates = set.get(name);
            assert!(!candidates.is_empty(), "проба {name} опустела");
            for c in candidates {
                assert!(
                    c.contains("[data-lk-source=\"camera\"]"),
                    "кандидат {c:?} пробы {name} поймает плитку демонстрации экрана"
                );
            }
        }
    }

    /// ТЕКСТ ПЕРВЫМ у площадок, снятых НЕ живой пробой.
    ///
    /// Урок Ф4 в виде проверки: снятый живой пробой класс Телемоста протух за
    /// три дня, и спас текстовый кандидат. У Meet, Teams и Zoom живой пробы не
    /// было вовсе, то есть их классы и `aria-label` — заведомо расходный
    /// материал, и первым кандидатом кнопки входа обязан стоять текст.
    #[test]
    fn unverified_platforms_lead_with_a_text_candidate() {
        let book = ProbeBook::builtin();
        for platform in ["meet", "teams", "zoom"] {
            let set = book.for_platform(platform);
            let first = set.first(probe::JOIN_BUTTON).unwrap_or_default();
            assert!(
                first.starts_with("text:"),
                "у {platform} первым кандидатом кнопки входа стоит {first:?}, а не текст"
            );
        }
    }

    /// Маркеры ищутся `contains:`, а не `text:`.
    ///
    /// **СТРАЖ МОЛЧАЛИВОГО ПРОМАХА.** `text:` перебирает ТОЛЬКО кликабельные
    /// элементы, а лобби, конец встречи и отказ во входе площадки пишут
    /// обычным `div`. Кандидат `text:Вы вышли` не находил бы такой маркер
    /// НИКОГДА, и выглядело бы это как «маркера на странице нет» — то есть
    /// проба была бы написана, задокументирована и мертва.
    #[test]
    fn marker_probes_use_the_contains_candidate_not_the_clickable_one() {
        let book = ProbeBook::builtin();
        let markers = [
            probe::LOBBY_MARKER,
            probe::MEETING_ENDED_MARKER,
            probe::JOIN_DENIED_MARKER,
            probe::CAPTCHA_MARKER,
        ];
        for platform in book.platforms() {
            let set = book.for_platform(platform);
            for name in markers {
                for candidate in set.get(name) {
                    assert!(
                        !candidate.starts_with("text:"),
                        "{platform}.{name}: кандидат {candidate:?} ищется среди кликабельных, \
                         а маркер площадки — обычный div; нужен contains:"
                    );
                }
            }
        }
    }

    /// Пробы «включить» и «выключить» НЕ МОГУТ находить одно и то же.
    ///
    /// Ловушка настоящая и найдена в этом же файле: `[aria-label*="mute" i]`
    /// подходит и к «Mute», и к «Unmute», потому что второе содержит первое.
    /// Совпади пробы — состояние микрофона читалось бы как «включён» всегда, и
    /// бот жал бы кнопку до исчерпания попыток, оставляя микрофон как есть.
    #[test]
    fn the_on_and_off_probes_of_a_device_never_share_a_candidate() {
        let book = ProbeBook::builtin();
        for platform in book.platforms() {
            let set = book.for_platform(platform);
            for (on, off, what) in [
                (probe::MIC_MUTE_BUTTON, probe::MIC_UNMUTE_BUTTON, "микрофон"),
                (probe::CAM_DISABLE_BUTTON, probe::CAM_ENABLE_BUTTON, "камера"),
            ] {
                for a in set.get(on) {
                    assert!(
                        !set.get(off).contains(a),
                        "{platform}: {what} — кандидат {a:?} стоит и во «включить», и в «выключить»"
                    );
                    // Подстрочное совпадение по `*=` — та же ловушка, только
                    // незаметнее: «Unmute» содержит «mute» целиком.
                    let Some((attr, "*=", needle)) = lone_attr_clause(a) else {
                        continue;
                    };
                    for b in set.get(off) {
                        let Some((other_attr, _, other)) = lone_attr_clause(b) else {
                            continue;
                        };
                        if attr != other_attr {
                            continue;
                        }
                        assert!(
                            !(needle.contains(&other) || other.contains(&needle)),
                            "{platform}: {what} — {a:?} и {b:?} ищут вкладывающиеся подстроки \
                             ({needle:?} и {other:?}) через `*=`; нужен `^=`"
                        );
                    }
                }
            }
        }
    }

    /// Разбирает селектор ИЗ ОДНОЙ атрибутной скобки: `[aria-label*="Mute" i]`.
    ///
    /// `None` для всего остального, включая селекторы из нескольких скобок:
    /// у них вторая скобка и есть то, что разводит «включено» и «выключено»
    /// (`[data-testid="mic-toggle"][data-enabled="true"]`), и сравнивать их
    /// подстроки бессмысленно.
    fn lone_attr_clause(selector: &str) -> Option<(String, &'static str, String)> {
        let s = selector.trim();
        if !s.starts_with('[') || !s.ends_with(']') || s[1..s.len() - 1].contains('[') {
            return None;
        }
        let body = &s[1..s.len() - 1];
        let (op, at) = ["*=", "^=", "$=", "|=", "~=", "="]
            .into_iter()
            .filter_map(|op| body.find(op).map(|at| (op, at)))
            .min_by_key(|(_, at)| *at)?;
        let attr = body[..at].trim().to_lowercase();
        let value = body[at + op.len()..].trim();
        let value = value.strip_suffix(" i").unwrap_or(value).trim();
        let needle = value.trim_matches('"').trim_matches('\'').to_lowercase();
        Some((attr, op, needle))
    }

    /// Сочетания клавиш разбираются, а мусор — нет.
    #[test]
    fn the_hotkey_probes_of_the_book_are_all_parseable() {
        let book = ProbeBook::builtin();
        for platform in book.platforms() {
            let set = book.for_platform(platform);
            for name in [probe::MIC_TOGGLE_HOTKEY, probe::CAM_TOGGLE_HOTKEY] {
                for raw in set.get(name) {
                    assert!(
                        super::super::chrome::Hotkey::parse(raw).is_some(),
                        "{platform}.{name}: сочетание {raw:?} не разбирается — проба мертва"
                    );
                }
            }
        }
    }

    /// Не-селекторы не участвуют в стражe «селектор не протёк в захват».
    ///
    /// Проверка самого механизма отсечения: `post_call_url` и сочетания клавиш
    /// обязаны выпадать ПО ИМЕНИ пробы, а не потому, что «не похожи на
    /// селектор».
    #[test]
    fn the_selector_view_drops_the_non_selector_probes() {
        let mut set = ProbeSet::empty();
        set.set(probe::JOIN_BUTTON, vec!["#join".to_string()]);
        set.set(probe::POST_CALL_URL, vec!["/after-call".to_string()]);
        set.set(probe::MIC_TOGGLE_HOTKEY, vec!["ctrl+d".to_string()]);

        let names: Vec<&str> = set.selector_candidates().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec![probe::JOIN_BUTTON]);
        assert_eq!(set.all_candidates().len(), 3, "полный список обязан остаться полным");
    }

    /// ГЛАВНЫЙ ИНВАРИАНТ ПРОЕКТА, ПРОВЕРЕННЫЙ НА КАЖДОЙ ПЛАТФОРМЕ ОТДЕЛЬНО:
    /// ПРОТУХШАЯ ВЁРСТКА ДЕГРАДИРУЕТ ДО «УЧАСТНИК N», А НЕ ОТМЕНЯЕТ ЗАПИСЬ.
    ///
    /// Проверка идёт по каждой площадке реестра, потому что цена ошибки у
    /// разных площадок разная только на словах: селекторы Meet, Teams и Zoom —
    /// черновики, снятые НЕ живой пробой, и день, когда они все промахнутся,
    /// наступит раньше, чем у Телемоста. В этот день обязано случиться ровно
    /// три вещи и ни одной больше:
    ///
    /// 1. участники называются «Участник N» — с устойчивым номером;
    /// 2. цикл входа НИКУДА не уходит сам (ни «мы внутри», ни «нам отказали»);
    /// 3. автовыход НЕ срабатывает, пока дорожки живы, — иначе промах пробы
    ///    выкидывал бы бота с живой встречи через минуту после входа.
    #[test]
    fn a_total_probe_miss_degrades_to_participant_n_on_every_platform() {
        use super::super::chrome::{next_join_step, JoinProgress, JoinStep};
        use super::super::endgame::{EndConfig, EndSignals, EndWatch};
        use super::super::observe::{ParticipantNamer, RawParticipant};

        for platform in crate::providers::Platform::ALL {
            let key = platform.as_str();
            // Пробы у платформы ЕСТЬ, но на странице не нашлось ничего: ровно
            // так выглядит смена вёрстки. Снимок при этом приходит пустым.
            assert!(!ProbeBook::builtin().for_platform(key).is_empty(), "{key}");
            let blank = super::super::observe::PageSnapshot::default();

            // 1. Имена.
            let mut namer = ParticipantNamer::new();
            let unnamed = vec![RawParticipant::default(), RawParticipant::default()];
            let labelled = namer.label(&unnamed);
            assert_eq!(labelled[0].name, "Участник 1", "{key}");
            assert_eq!(labelled[1].name, "Участник 2", "{key}");

            // 2. Вход не выдумывает себе состояний.
            assert_eq!(
                next_join_step(&blank, &JoinProgress::default(), 3),
                JoinStep::Wait,
                "{key}: пустой снимок увёл цикл входа с места"
            );

            // 3. Автовыход молчит, пока дорожки живы. `participants_known`
            //    ложно именно потому, что пробы не сработали НИ РАЗУ, — и
            //    отличить «никого нет» от «селектор протух» иначе нельзя.
            let mut watch = EndWatch::new(EndConfig {
                alone_ms: 1,
                silence_ms: 1,
                max_ms: 0,
                ..EndConfig::default()
            });
            let reason = watch.observe(
                10_000,
                EndSignals {
                    target_gone: false,
                    tracks_seen: 3,
                    tracks_live: 3,
                    dom_ended: blank.ended,
                    post_call: blank.post_call,
                    participants_known: false,
                    others: 0,
                    speaking: false,
                },
            );
            assert_eq!(reason, None, "{key}: промах проб выкинул бота с живой встречи");
        }
    }

    /// Переменная окружения побеждает дефолт — на ней держится герметичность.
    #[test]
    fn the_env_variable_wins_over_the_default_path() {
        let _guard = crate::testing::env_guard();
        std::env::set_var("FORGE_MEET_PROBES_PATH", "/tmp/forge-meet-probes-test.toml");
        assert_eq!(probes_path(), PathBuf::from("/tmp/forge-meet-probes-test.toml"));
        std::env::remove_var("FORGE_MEET_PROBES_PATH");
        assert_eq!(probes_path(), default_probes_path());
    }
}
