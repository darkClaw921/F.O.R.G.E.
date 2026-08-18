//! Реестр платформ видеовстреч и канонизация ссылок.
//!
//! Чистый модуль: ни `axum`, ни `http`, ни ввода-вывода.
//!
//! # Матчинг платформы — ТОЛЬКО по хосту
//!
//! Хост берётся у разобранного [`url::Url`] и сравнивается с образцом либо
//! точным равенством, либо суффиксом ПОСЛЕ ТОЧКИ. Подстрочный поиск
//! (`raw.contains("telemost.yandex.ru")`) даёт три готовых обхода, и все три
//! покрыты отрицательными тестами:
//!
//! | обход | почему проходит подстроку | почему не проходит нас |
//! |---|---|---|
//! | `https://telemost.yandex.ru.evil.com/j/1` | подстрока есть | хост не равен образцу и не кончается на `.telemost.yandex.ru` |
//! | `https://evil.com/?r=https://telemost.yandex.ru/j/1` | подстрока есть | хост `evil.com` |
//! | `https://evil.com/#telemost.yandex.ru/j/1` | подстрока есть | хост `evil.com` |
//!
//! Схема проверяется отдельно: `javascript:`, `data:` и прочее отвергаются до
//! разбора хоста.
//!
//! # `url_key` против `canonical_url` — это РАЗНЫЕ вещи
//!
//! **`url_key` — ИДЕНТИЧНОСТЬ комнаты.** По нему работает частичный уникальный
//! индекс `ux_meetings_active_url`, то есть именно он не даёт боту войти в одну
//! встречу дважды. Поэтому в него входят только платформа и идентификатор
//! комнаты — и НИ ОДНОГО query-параметра.
//!
//! Причина не косметическая. Пароль Zoom (`?pwd=`) и контекст Teams
//! (`?context=`) нужны, чтобы ВОЙТИ, но одну и ту же встречу рассылают и со
//! ссылкой-с-паролем, и без неё. Попади они в ключ — два сообщения об одной
//! встрече дали бы два разных ключа, дедуп не сработал бы, и бот вошёл бы
//! дважды. Ровно та поломка, ради предотвращения которой индекс и заведён.
//!
//! **`canonical_url` — то, КУДА бот идёт.** Здесь параметры входа обязаны
//! сохраниться, поэтому у каждой платформы есть БЕЛЫЙ СПИСОК ключей
//! ([`Provider::keep_params`]). Срезать всю query нельзя: без `pwd` Zoom
//! спросит пароль, без `context` Teams не поймёт, в какой тенант идти. Всё,
//! чего в списке нет (трекинг, `authuser`, `deeplinkId`), выбрасывается — и
//! именно поэтому две формы одной ссылки канонизируются в одну.
//!
//! # Регулярка в реестре — это ДЛЯ ФРОНТА
//!
//! [`Provider::link_regex`] отдаётся наружу через `GET /api/meet/status` и
//! никогда не компилируется бэкендом (крейта `regex` в зависимостях нет
//! намеренно — в дереве он живёт только dev-зависимостью wiremock). Источник
//! истины про «что считать ссылкой на встречу» лежит на сервере потому, что
//! разметку браузер держит в кэше PWA неделями: фронтовая константа протухла
//! бы незаметно, а серверная приезжает с каждым ответом `/status`.

use serde::{Deserialize, Serialize};
use url::Url;

/// Платформа видеовстреч.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Яндекс Телемост. Единственная платформа, измеренная живой пробой (Ф0):
    /// режим `Slots`, спикеры только из DOM.
    Telemost,
    /// `rename` обязателен: `rename_all = "snake_case"` дал бы `google_meet`,
    /// то есть ВТОРОЕ написание рядом с [`Platform::as_str`]. Расхождение
    /// было бы особенно коварным — в базу и в `url_key` уезжает `as_str`, а в
    /// JSON роутов и WS-кадров serde, и лента показывала бы платформу, по
    /// которой ничего не находится. На совпадение обеих форм есть тест.
    #[serde(rename = "meet")]
    GoogleMeet,
    Zoom,
    Teams,
}

impl Platform {
    pub const ALL: [Platform; 4] = [
        Platform::Telemost,
        Platform::GoogleMeet,
        Platform::Zoom,
        Platform::Teams,
    ];

    /// Идентификатор платформы в проводных форматах: `url_key`, JSON роутов,
    /// WS-кадры, `probes.toml`. Один и тот же во всех четырёх.
    ///
    /// `meet`, а не `google_meet`: ровно эту строку использует
    /// `messenger/meet-core.js` в своём встроенном реестре. Реестр бэкенда
    /// ЗАМЕНЯЕТ фронтовый целиком, поэтому разные написания означали бы, что
    /// платформа зовётся по-разному в зависимости от того, ответил ли
    /// `/api/meet/status` — и любое сохранённое значение разъехалось бы само
    /// собой.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Telemost => "telemost",
            Self::GoogleMeet => "meet",
            Self::Zoom => "zoom",
            Self::Teams => "teams",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// Запись реестра.
    pub fn provider(self) -> &'static Provider {
        PROVIDERS
            .iter()
            .find(|p| p.platform == self)
            .expect("реестр обязан содержать все варианты Platform — на это есть тест")
    }
}

/// Запись реестра платформ. Уезжает наружу целиком в `GET /api/meet/status`.
///
/// # Форма полей — КОНТРАКТ с `messenger/meet-core.js`
///
/// Фронтовый `normalizePlatforms()` отбрасывает запись без хостов ИЛИ без
/// шаблонов пути ПОШТУЧНО и молча. То есть реестр без [`Provider::paths`] не
/// «частично применился бы» — он был бы выброшен целиком, фронт остался бы на
/// встроенном наборе, и весь смысл отдавать реестр с бэкенда (разметку
/// браузер держит в кэше PWA неделями) пропал бы БЕЗ ЕДИНОГО СООБЩЕНИЯ.
/// Поэтому набор полей здесь не «на всякий случай», а ровно тот, который
/// фронт умеет принять.
#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub platform: Platform,
    /// Человекочитаемое имя для интерфейса.
    pub title: &'static str,
    /// Хосты, сравниваемые ТОЧНЫМ равенством.
    pub hosts: &'static [&'static str],
    /// Суффиксы хостов, каждый начинается с точки.
    ///
    /// Отдельным полем, а не «суффиксом любого из hosts»: точка обязана быть
    /// частью образца. Без неё `zoom.us` совпал бы с `notzoom.us`, то есть
    /// вернулся бы ровно тот подстрочный поиск, от которого модуль и уходит.
    pub host_suffixes: &'static [&'static str],
    /// Шаблоны пути (регулярки в синтаксисе, общем для JS и для человека).
    ///
    /// Бэкенд их НЕ КОМПИЛИРУЕТ — путь разбирают функции `*_room` ниже, а
    /// крейта `regex` в зависимостях нет намеренно. Эти строки существуют
    /// ради фронта, и на их согласие с разбором есть контрактный тест ФФ2.
    pub paths: &'static [&'static str],
    /// Белый список query-параметров, переживающих канонизацию. Всё
    /// остальное выбрасывается.
    pub keep_params: &'static [&'static str],
    /// `lower` — идентификатор комнаты регистронезависим (код Meet), иначе
    /// `preserve`. Без приведения одна встреча, присланная двумя людьми в
    /// разном регистре, дала бы два ключа и две записи.
    pub key_case: &'static str,
    /// Регулярка ссылки целиком — для подсветки и диагностики во фронте.
    pub link_regex: &'static str,
    /// Отдаёт ли платформа звук так, что мы умеем его забрать. Zoom — главный
    /// кандидат на `false` (звук идёт через DataChannel и WASM, а не через
    /// WebRTC-медиа), но пока это гипотеза, а не измерение: Ф0 задачу пробы
    /// Zoom отложила. Ни одной платформы с `false` сегодня нет.
    pub audio: bool,
}

impl Provider {
    /// Совпадает ли хост: точное равенство ЛИБО один из суффиксов.
    fn matches_host(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| *h == host)
            || self
                .host_suffixes
                .iter()
                .any(|suffix| host_has_suffix(host, suffix))
    }
}

/// Оканчивается ли хост на суффикс, который САМ начинается с точки.
///
/// Именно точка отделяет `us02web.zoom.us` (законный поддомен Zoom) от
/// `zoom.us.evil.com` и от `notzoom.us`.
fn host_has_suffix(host: &str, suffix: &str) -> bool {
    suffix.starts_with('.') && host.len() > suffix.len() && host.ends_with(suffix)
}

/// Реестр четырёх платформ.
pub const PROVIDERS: [Provider; 4] = [
    Provider {
        platform: Platform::Telemost,
        title: "Яндекс Телемост",
        // Зоны перечислены явно: у Яндекса их несколько, и суффиксное
        // сравнение с `yandex.ru` пустило бы сюда любой сервис компании —
        // поэтому суффиксов у Телемоста нет вовсе.
        hosts: &[
            "telemost.yandex.ru",
            "telemost.yandex.com",
            "telemost.yandex.by",
            "telemost.yandex.kz",
            "telemost.360.yandex.ru",
        ],
        host_suffixes: &[],
        // Форма пути снята с живой встречи Ф0: /j/60497752704486.
        paths: &[r"/j/(\d{4,32})"],
        // Ссылка Телемоста самодостаточна: параметров входа у неё нет, а
        // `ncrnd` Телемост дописывает сам при каждом открытии.
        keep_params: &[],
        key_case: "preserve",
        link_regex: r"https?://telemost\.(?:360\.)?yandex\.(?:ru|com|by|kz)/j/\d+",
        audio: true,
    },
    Provider {
        platform: Platform::GoogleMeet,
        title: "Google Meet",
        hosts: &["meet.google.com"],
        host_suffixes: &[],
        paths: &[
            r"/([A-Za-z]{3}-[A-Za-z]{4}-[A-Za-z]{3})",
            r"/lookup/([A-Za-z0-9_-]{3,64})",
        ],
        // `authuser`, `hs`, `pli` — служебные параметры сессии Google, к
        // идентичности встречи отношения не имеют.
        keep_params: &[],
        // Код встречи Meet регистронезависим.
        key_case: "lower",
        link_regex: r"https?://meet\.google\.com/(?:lookup/[\w-]+|[A-Za-z]{3}-[A-Za-z]{4}-[A-Za-z]{3})",
        audio: true,
    },
    Provider {
        platform: Platform::Zoom,
        title: "Zoom",
        // Личный поддомен компании (`us02web.zoom.us`) — норма, поэтому здесь
        // и нужны суффиксы. Номер встречи при этом глобально уникален, поэтому
        // в `url_key` поддомен не входит.
        hosts: &["zoom.us", "zoomgov.com"],
        host_suffixes: &[".zoom.us", ".zoomgov.com"],
        paths: &[
            r"/j/(\d{8,15})",
            r"/wc/join/(\d{8,15})",
            r"/wc/(\d{8,15})/join",
            r"/s/(\d{8,15})",
            r"/w/(\d{8,15})",
            r"/my/([A-Za-z0-9._-]{1,64})",
        ],
        // `pwd` — ПАРОЛЬ ВСТРЕЧИ. Срез всей query сломал бы вход.
        keep_params: &["pwd"],
        key_case: "preserve",
        link_regex: r"https?://(?:[\w-]+\.)*zoom(?:\.us|gov\.com)/(?:j|s|w|wc|my)/[\w./-]+",
        audio: true,
    },
    Provider {
        platform: Platform::Teams,
        title: "Microsoft Teams",
        hosts: &["teams.microsoft.com", "teams.live.com", "teams.microsoft.us"],
        host_suffixes: &[],
        // Идентификатор треда приезжает процентно-закодированным
        // (`19%3ameeting_…%40thread.v2`) — в шаблоне это обычные символы пути.
        paths: &[r"/l/meetup-join/([^/?#]{4,512})(?:/\d*)?", r"/meet/(\d{6,20})"],
        // `context` — ОБЯЗАТЕЛЬНАЯ часть ссылки корпоративного Teams (в нём
        // тенант и организатор), `p` — код доступа у teams.live.com.
        keep_params: &["context", "p"],
        key_case: "preserve",
        link_regex: r"https?://teams\.(?:microsoft\.com|live\.com|microsoft\.us)/(?:l/meetup-join/|meet/)\S+",
        audio: true,
    },
];

/// Разобранная ссылка на встречу.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingLink {
    pub platform: Platform,
    /// Идентичность комнаты. По нему работает дедуп активной записи
    /// (`ux_meetings_active_url`). Query-параметров НЕ содержит — см. шапку
    /// модуля.
    pub url_key: String,
    /// Куда бот реально идёт: параметры входа сохранены, мусор срезан.
    pub canonical_url: String,
    /// Идентификатор комнаты внутри платформы (номер Телемоста, код Meet,
    /// номер Zoom, thread Teams). Диагностическое поле и подпись в интерфейсе.
    pub room_id: String,
}

/// Разбирает ссылку. `None` — это не ссылка на встречу известной платформы.
///
/// Порядок проверок: разбор URL → схема → хост → форма пути. Первая же
/// неудача даёт `None`, и это не «мягкий отказ»: кнопка записи просто не
/// появляется, а ошибок пользователю никто не показывает.
pub fn parse_meeting_url(raw: &str) -> Option<MeetingLink> {
    parse_meeting_url_inner(raw, 0)
}

/// Сколько раз подряд разрешено разворачивать ссылку-обёртку.
///
/// Один. Лаунчер Teams заворачивает НАСТОЯЩИЙ адрес встречи, а не другой
/// лаунчер; без потолка же ссылка, ссылающаяся сама на себя, крутила бы разбор
/// вечно — а разбор зовётся на каждое сообщение ленты.
const MAX_UNWRAP_DEPTH: u8 = 1;

fn parse_meeting_url_inner(raw: &str, depth: u8) -> Option<MeetingLink> {
    // Относительная ссылка (`/j/123`) здесь и обязана падать: без хоста
    // сопоставить платформу нечем, а достраивать её origin'ом страницы
    // мессенджера значило бы поверить чужому документу.
    let url = Url::parse(raw.trim()).ok()?;

    // ЛАУНЧЕР TEAMS. Настоящий адрес встречи спрятан в параметре `url`, а сам
    // путь `/dl/launcher/launcher.html` никакой комнаты не содержит вовсе.
    // Разворачиваем ДО матчинга платформы и канонизации: иначе белый список
    // параметров выбросил бы `url` — то есть саму встречу — и осталась бы
    // ссылка на страницу «скачайте приложение».
    if depth < MAX_UNWRAP_DEPTH {
        if let Some(inner) = launcher_target(&url) {
            return parse_meeting_url_inner(&inner, depth + 1);
        }
    }

    // Схема — до всего остального. `javascript:`, `data:`, `file:` не имеют
    // хоста вовсе, но полагаться на это нельзя: проверка явная.
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    let host = url.host_str()?.to_ascii_lowercase();
    let provider = PROVIDERS.iter().find(|p| p.matches_host(&host))?;

    let segments: Vec<String> = url
        .path_segments()
        .map(|it| it.filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
        .unwrap_or_default();

    let (room_id, canonical_path) = match provider.platform {
        Platform::Telemost => telemost_room(&segments)?,
        Platform::GoogleMeet => meet_room(&segments)?,
        Platform::Zoom => zoom_room(&segments)?,
        Platform::Teams => teams_room(&segments)?,
    };

    // Хост канонизируется по-разному: у Zoom вендорный поддомен не входит в
    // идентичность (номер встречи глобально уникален), у остальных хост —
    // часть адреса комнаты.
    let key_host = match provider.platform {
        Platform::Zoom => String::new(),
        _ => host.clone(),
    };
    let url_key = if key_host.is_empty() {
        format!("{}:{}", provider.platform.as_str(), room_id)
    } else {
        format!("{}:{}/{}", provider.platform.as_str(), key_host, room_id)
    };

    // Белый список query. Ключи сортируются, чтобы порядок в исходной ссылке
    // не порождал два разных канонических адреса.
    let mut kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| provider.keep_params.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    kept.sort();

    let mut canonical = Url::parse(&format!("https://{host}")).ok()?;
    canonical.set_path(&canonical_path);
    if kept.is_empty() {
        canonical.set_query(None);
    } else {
        let mut qs = canonical.query_pairs_mut();
        qs.clear();
        for (k, v) in &kept {
            qs.append_pair(k, v);
        }
        drop(qs);
    }

    Some(MeetingLink {
        platform: provider.platform,
        url_key,
        canonical_url: canonical.to_string(),
        room_id,
    })
}

/// Адрес встречи, спрятанный в ссылке-лаунчере.
///
/// Форма: `https://teams.microsoft.com/dl/launcher/launcher.html?url=%2F_%23%2Fl%2Fmeetup-join%2F…`.
/// Внутренний адрес бывает и АБСОЛЮТНЫМ, и относительным (Teams рассылает оба),
/// поэтому относительный достраивается ХОСТОМ САМОГО ЛАУНЧЕРА — и только им:
/// взять хост откуда-то ещё значило бы позволить чужой странице подсунуть свой.
///
/// Фрагмент `#/l/meetup-join/…` тоже разворачивается: Teams кладёт путь после
/// решётки, а `Url` в такой форме считает его якорем, и путь остался бы `/_`.
fn launcher_target(url: &Url) -> Option<String> {
    if !url.path().eq_ignore_ascii_case("/dl/launcher/launcher.html") {
        return None;
    }
    let host = url.host_str()?;
    if !PROVIDERS
        .iter()
        .any(|p| p.platform == Platform::Teams && p.matches_host(&host.to_ascii_lowercase()))
    {
        return None;
    }
    let inner = url
        .query_pairs()
        .find(|(k, _)| k == "url")
        .map(|(_, v)| v.into_owned())?;
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    let absolute = if inner.starts_with("http://") || inner.starts_with("https://") {
        Url::parse(inner).ok()?
    } else {
        Url::parse(&format!("https://{host}")).ok()?.join(inner).ok()?
    };
    // Хост внутренней ссылки обязан остаться хостом Teams: `?url=https://evil`
    // приходит из того же сообщения, что и остальное, и доверия ему не больше.
    let inner_host = absolute.host_str()?.to_ascii_lowercase();
    if !PROVIDERS
        .iter()
        .any(|p| p.platform == Platform::Teams && p.matches_host(&inner_host))
    {
        return None;
    }
    // `/_#/l/meetup-join/…`: настоящий путь лежит во фрагменте.
    if let Some(fragment) = absolute.fragment().filter(|f| f.starts_with('/')) {
        let mut rebuilt = absolute.clone();
        rebuilt.set_fragment(None);
        return rebuilt.join(fragment).ok().map(|u| u.to_string());
    }
    Some(absolute.to_string())
}

/// Адрес, по которому бот РЕАЛЬНО идёт, — он не всегда равен каноническому.
///
/// # Зачем расхождение вообще допущено
///
/// [`MeetingLink::canonical_url`] — адрес встречи как её понимает человек и как
/// её опознаёт дедуп. Но у Zoom по такому адресу открывается не встреча, а
/// страница «скачайте приложение»: кнопка «присоединиться из браузера» спрятана
/// за несколькими шагами и показывается не всегда. Веб-клиент живёт по другому
/// адресу — `app.zoom.us/wc/join/<номер>` — и туда надо идти сразу.
///
/// Подменять на него `canonical_url` НЕЛЬЗЯ: канонический адрес показывается
/// человеку и хранится в записи встречи, а `app.zoom.us` — деталь того, как мы
/// заходим. Поэтому расхождение явное и живёт в одной функции.
///
/// Пароль (`?pwd=`) переносится: без него веб-клиент спросит его руками, и вход
/// упрётся в человека на ровном месте.
pub fn bot_entry_url(link: &MeetingLink) -> String {
    if link.platform != Platform::Zoom {
        return link.canonical_url.clone();
    }
    // Персональная комната (`/my/<имя>`) номера не имеет — веб-клиент собрать
    // не из чего, идём как есть.
    if !link.room_id.chars().all(|c| c.is_ascii_digit()) || link.room_id.is_empty() {
        return link.canonical_url.clone();
    }
    let pwd = Url::parse(&link.canonical_url)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "pwd")
                .map(|(_, v)| v.into_owned())
        })
        .unwrap_or_default();
    let mut out = match Url::parse(&format!("https://{ZOOM_WEB_CLIENT_HOST}")) {
        Ok(u) => u,
        Err(_) => return link.canonical_url.clone(),
    };
    out.set_path(&format!("/wc/join/{}", link.room_id));
    if !pwd.is_empty() {
        out.query_pairs_mut().clear().append_pair("pwd", &pwd);
    }
    out.to_string()
}

/// Хост веб-клиента Zoom.
///
/// Не поддомен компании (`us02web.zoom.us`): веб-клиент общий, а номер встречи
/// у Zoom глобально уникален — ровно поэтому поддомен не входит и в `url_key`.
pub const ZOOM_WEB_CLIENT_HOST: &str = "app.zoom.us";

/// `telemost.yandex.ru/j/<цифры>`.
fn telemost_room(segments: &[String]) -> Option<(String, String)> {
    match segments {
        [j, id] if j == "j" && !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) => {
            Some((id.clone(), format!("/j/{id}")))
        }
        _ => None,
    }
}

/// `meet.google.com/abc-defg-hij` либо `meet.google.com/lookup/<имя>`.
///
/// Форма кода проверяется строго: без этого кнопка записи появлялась бы на
/// ссылке `meet.google.com/` (главная страница сервиса) и на любой её
/// служебной подстранице.
fn meet_room(segments: &[String]) -> Option<(String, String)> {
    match segments {
        [lookup, name] if lookup == "lookup" && !name.is_empty() => {
            Some((format!("lookup/{name}"), format!("/lookup/{name}")))
        }
        [code] if is_meet_code(code) => {
            let lower = code.to_ascii_lowercase();
            Some((lower.clone(), format!("/{lower}")))
        }
        _ => None,
    }
}

/// Код Meet — три буквы, четыре буквы, три буквы через дефис.
fn is_meet_code(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 3
        && [3usize, 4, 3]
            .iter()
            .zip(&parts)
            .all(|(len, part)| part.len() == *len && part.chars().all(|c| c.is_ascii_alphabetic()))
}

/// `zoom.us/j|s|w/<номер>`, `zoom.us/wc/join/<номер>`,
/// `zoom.us/wc/<номер>/join`, `zoom.us/my/<имя>`.
fn zoom_room(segments: &[String]) -> Option<(String, String)> {
    let digits = |s: &String| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match segments {
        [kind, id] if matches!(kind.as_str(), "j" | "s" | "w") && digits(id) => {
            Some((id.clone(), format!("/j/{id}")))
        }
        [wc, join, id] if wc == "wc" && join == "join" && digits(id) => {
            Some((id.clone(), format!("/j/{id}")))
        }
        [wc, id, join] if wc == "wc" && join == "join" && digits(id) => {
            Some((id.clone(), format!("/j/{id}")))
        }
        // Персональная комната: номера нет, идентичность — имя владельца.
        [my, name] if my == "my" && !name.is_empty() => {
            let lower = name.to_ascii_lowercase();
            Some((format!("my/{lower}"), format!("/my/{lower}")))
        }
        _ => None,
    }
}

/// `teams.microsoft.com/l/meetup-join/<thread>/…` либо
/// `teams.live.com/meet/<id>`.
fn teams_room(segments: &[String]) -> Option<(String, String)> {
    match segments {
        [l, meetup, thread, rest @ ..] if l == "l" && meetup == "meetup-join" && !thread.is_empty() => {
            let mut path = format!("/l/meetup-join/{thread}");
            for part in rest {
                path.push('/');
                path.push_str(part);
            }
            // Идентичность — сам thread: хвост пути (`/0`) служебный и в
            // разных рассылках одной встречи отличается.
            Some((thread.clone(), path))
        }
        [meet, id, rest @ ..] if meet == "meet" && !id.is_empty() => {
            let mut path = format!("/meet/{id}");
            for part in rest {
                path.push('/');
                path.push_str(part);
            }
            Some((id.clone(), path))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: &str) -> String {
        parse_meeting_url(raw)
            .unwrap_or_else(|| panic!("ссылка обязана разбираться: {raw}"))
            .url_key
    }

    fn link(raw: &str) -> MeetingLink {
        parse_meeting_url(raw).unwrap_or_else(|| panic!("ссылка обязана разбираться: {raw}"))
    }

    // --- положительные кейсы: ~20 реальных форм четырёх платформ -----------

    #[test]
    fn real_world_links_of_all_four_platforms_parse() {
        let cases: &[(&str, Platform)] = &[
            // Телемост — форма из живой пробы Ф0.
            ("https://telemost.yandex.ru/j/60497752704486", Platform::Telemost),
            ("https://telemost.yandex.com/j/12345678901234", Platform::Telemost),
            ("https://telemost.yandex.by/j/1", Platform::Telemost),
            ("https://telemost.360.yandex.ru/j/999", Platform::Telemost),
            ("http://telemost.yandex.ru/j/60497752704486", Platform::Telemost),
            // Google Meet.
            ("https://meet.google.com/abc-defg-hij", Platform::GoogleMeet),
            ("https://meet.google.com/ABC-DEFG-HIJ", Platform::GoogleMeet),
            ("https://meet.google.com/abc-defg-hij?authuser=1", Platform::GoogleMeet),
            ("https://meet.google.com/lookup/standup-daily", Platform::GoogleMeet),
            // Zoom.
            ("https://zoom.us/j/83123456789", Platform::Zoom),
            ("https://us02web.zoom.us/j/83123456789", Platform::Zoom),
            ("https://us05web.zoom.us/j/83123456789?pwd=Qm9iSXM", Platform::Zoom),
            ("https://zoom.us/wc/join/83123456789", Platform::Zoom),
            ("https://zoom.us/wc/83123456789/join", Platform::Zoom),
            ("https://zoom.us/s/83123456789", Platform::Zoom),
            ("https://zoom.us/my/igor.room", Platform::Zoom),
            ("https://company.zoomgov.com/j/1611223344", Platform::Zoom),
            // Teams.
            (
                "https://teams.microsoft.com/l/meetup-join/19%3ameeting_ZjQ%40thread.v2/0?context=%7B%22Tid%22%3A%22t%22%7D",
                Platform::Teams,
            ),
            (
                "https://teams.microsoft.com/l/meetup-join/19:meeting_abc@thread.v2/0",
                Platform::Teams,
            ),
            ("https://teams.live.com/meet/9312345678901?p=abcDEF", Platform::Teams),
        ];
        assert!(cases.len() >= 20, "положительных кейсов должно быть около двадцати");
        for (raw, platform) in cases {
            let l = link(raw);
            assert_eq!(l.platform, *platform, "{raw}");
            assert!(!l.url_key.is_empty(), "{raw}");
            assert!(l.canonical_url.starts_with("https://"), "{raw}");
        }
    }

    // --- отрицательные кейсы: три обхода домена и опасные схемы ------------

    #[test]
    fn the_subdomain_decoy_is_rejected() {
        // Классика: подстрочный поиск здесь сработал бы, хост — нет.
        assert_eq!(parse_meeting_url("https://telemost.yandex.ru.evil.com/j/1"), None);
        assert_eq!(parse_meeting_url("https://meet.google.com.evil.com/abc-defg-hij"), None);
        assert_eq!(parse_meeting_url("https://zoom.us.evil.com/j/83123456789"), None);
        assert_eq!(parse_meeting_url("https://teams.microsoft.com.evil.com/meet/1"), None);
        // И «похожий» домен без точки-разделителя.
        assert_eq!(parse_meeting_url("https://nottelemost.yandex.ru/j/1"), None);
        assert_eq!(parse_meeting_url("https://notzoom.us/j/83123456789"), None);
    }

    #[test]
    fn a_domain_inside_the_query_is_rejected() {
        assert_eq!(
            parse_meeting_url("https://evil.com/?r=https://telemost.yandex.ru/j/1"),
            None
        );
        assert_eq!(
            parse_meeting_url("https://evil.com/j/1?host=meet.google.com"),
            None
        );
    }

    #[test]
    fn a_domain_inside_the_fragment_is_rejected() {
        assert_eq!(
            parse_meeting_url("https://evil.com/#https://telemost.yandex.ru/j/1"),
            None
        );
        assert_eq!(parse_meeting_url("https://evil.com/#zoom.us/j/83123456789"), None);
    }

    #[test]
    fn dangerous_schemes_are_rejected() {
        for raw in [
            "javascript:alert(1)//telemost.yandex.ru/j/1",
            "javascript://telemost.yandex.ru/j/1",
            "data:text/html,https://telemost.yandex.ru/j/1",
            "file:///telemost.yandex.ru/j/1",
            "ftp://telemost.yandex.ru/j/1",
            "vbscript:msgbox('telemost.yandex.ru')",
        ] {
            assert_eq!(parse_meeting_url(raw), None, "схема обязана отвергаться: {raw}");
        }
    }

    #[test]
    fn relative_links_and_junk_are_rejected() {
        for raw in ["", "   ", "/j/60497752704486", "telemost.yandex.ru/j/1", "не ссылка"] {
            assert_eq!(parse_meeting_url(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn service_pages_do_not_look_like_meetings() {
        // Кнопка «записать» на главной странице сервиса — худший вид ложного
        // срабатывания: пользователь жмёт, бот открывает лендинг и ждёт.
        for raw in [
            "https://telemost.yandex.ru/",
            "https://telemost.yandex.ru/about",
            "https://meet.google.com/",
            "https://meet.google.com/landing",
            "https://meet.google.com/abcdefghij",
            "https://zoom.us/",
            "https://zoom.us/download",
            "https://teams.microsoft.com/",
            "https://teams.microsoft.com/downloads",
        ] {
            assert_eq!(parse_meeting_url(raw), None, "{raw}");
        }
    }

    // --- канонизация -------------------------------------------------------

    #[test]
    fn tracking_parameters_do_not_split_one_meeting_in_two() {
        let plain = link("https://telemost.yandex.ru/j/60497752704486");
        let tracked = link(
            "https://telemost.yandex.ru/j/60497752704486?utm_source=tg&utm_medium=chat&from=share",
        );
        assert_eq!(plain.url_key, tracked.url_key);
        assert_eq!(plain.canonical_url, tracked.canonical_url);

        // Фрагмент, хвостовой слеш и регистр хоста тоже не создают вторую
        // встречу.
        assert_eq!(key("https://TELEMOST.yandex.ru/j/60497752704486#anchor"), plain.url_key);
        assert_eq!(key("https://telemost.yandex.ru/j/60497752704486/"), plain.url_key);
    }

    #[test]
    fn different_meetings_never_collapse_into_one_key() {
        let a = key("https://telemost.yandex.ru/j/1111");
        let b = key("https://telemost.yandex.ru/j/2222");
        assert_ne!(a, b);
        assert_ne!(key("https://zoom.us/j/83123456789"), key("https://zoom.us/j/83123456780"));
        assert_ne!(
            key("https://meet.google.com/abc-defg-hij"),
            key("https://meet.google.com/abc-defg-hik")
        );
        // Разные платформы с совпавшим номером комнаты — тоже разные ключи.
        assert_ne!(key("https://zoom.us/j/1111"), key("https://telemost.yandex.ru/j/1111"));
    }

    /// Критерий задачи: пароль Zoom и контекст Teams обязаны пережить
    /// канонизацию, иначе бот не войдёт.
    #[test]
    fn the_join_parameters_survive_canonicalisation() {
        let zoom = link("https://us02web.zoom.us/j/83123456789?pwd=Qm9iSXM&uname=spy&utm_source=x");
        assert!(zoom.canonical_url.contains("pwd=Qm9iSXM"), "{}", zoom.canonical_url);
        assert!(!zoom.canonical_url.contains("uname"), "{}", zoom.canonical_url);
        assert!(!zoom.canonical_url.contains("utm_source"), "{}", zoom.canonical_url);

        let teams = link(
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_ZjQ%40thread.v2/0\
             ?context=%7B%22Tid%22%3A%22t%22%7D&deeplinkId=abc&directDl=true",
        );
        assert!(teams.canonical_url.contains("context="), "{}", teams.canonical_url);
        assert!(!teams.canonical_url.contains("deeplinkId"), "{}", teams.canonical_url);

        let live = link("https://teams.live.com/meet/9312345678901?p=abcDEF&launchAgent=x");
        assert!(live.canonical_url.contains("p=abcDEF"), "{}", live.canonical_url);
        assert!(!live.canonical_url.contains("launchAgent"), "{}", live.canonical_url);
    }

    /// ЛАУНЧЕР TEAMS разворачивается до настоящего адреса встречи.
    ///
    /// Рассылки Teams часто ведут не на встречу, а на страницу-лаунчер, где
    /// настоящий адрес спрятан в параметре `url`. Не разверни мы его — белый
    /// список параметров выбросил бы `url` вместе со встречей, и бот пошёл бы
    /// на страницу «скачайте приложение». Идентичность при этом обязана
    /// совпадать с идентичностью прямой ссылки: одну встречу рассылают в обеих
    /// формах, и два ключа означали бы двух ботов в одной комнате.
    #[test]
    fn a_teams_launcher_link_unwraps_to_the_meeting_itself() {
        let direct = link(
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_ZjQ%40thread.v2/0\
             ?context=%7B%22Tid%22%3A%22t%22%7D",
        );

        // Абсолютный адрес внутри.
        let wrapped = link(
            "https://teams.microsoft.com/dl/launcher/launcher.html\
             ?url=https%3A%2F%2Fteams.microsoft.com%2Fl%2Fmeetup-join%2F19%253ameeting_ZjQ%2540thread.v2%2F0\
             %3Fcontext%3D%257B%2522Tid%2522%253A%2522t%2522%257D&type=meetup-join&deeplinkId=z",
        );
        assert_eq!(wrapped.url_key, direct.url_key, "лаунчер дал другой ключ комнаты");
        assert!(wrapped.canonical_url.contains("meetup-join"), "{}", wrapped.canonical_url);
        assert!(wrapped.canonical_url.contains("context="), "{}", wrapped.canonical_url);
        assert!(!wrapped.canonical_url.contains("launcher"), "{}", wrapped.canonical_url);

        // ОТНОСИТЕЛЬНЫЙ адрес с путём во фрагменте — форма, которую Teams
        // рассылает чаще всего. Без разбора фрагмента путь остался бы `/_`.
        let hashed = link(
            "https://teams.microsoft.com/dl/launcher/launcher.html\
             ?url=%2F_%23%2Fl%2Fmeetup-join%2F19%253ameeting_ZjQ%2540thread.v2%2F0\
             %3Fcontext%3D%257B%2522Tid%2522%253A%2522t%2522%257D&type=meetup-join",
        );
        assert_eq!(hashed.url_key, direct.url_key, "{}", hashed.canonical_url);
    }

    /// Лаунчер НЕ становится дырой: чужой хост внутри отвергается.
    ///
    /// Параметр `url` приезжает из того же чужого сообщения, что и всё
    /// остальное. Пусти мы туда любой адрес — бот открывал бы произвольную
    /// страницу по ссылке из чата, с отключённым CSP и стоящим инжектом.
    #[test]
    fn a_launcher_pointing_outside_teams_is_not_a_meeting() {
        assert!(parse_meeting_url(
            "https://teams.microsoft.com/dl/launcher/launcher.html?url=https%3A%2F%2Fevil.com%2Fx"
        )
        .is_none());
        // ГЛАВНЫЙ СЛУЧАЙ, РАДИ КОТОРОГО ПРОВЕРКА ХОСТА ВНУТРИ И СУЩЕСТВУЕТ.
        // Здесь внутренний адрес — ЗАКОННАЯ встреча, только ЧУЖОЙ площадки.
        // Без проверки ссылка `teams.microsoft.com/...` опозналась бы как
        // встреча Google Meet, и бот пошёл бы совсем не туда, куда ведёт
        // видимый человеку адрес. Остальные случаи (`evil.com`) отсеиваются и
        // дальше по пути, а этот — нет.
        assert!(
            parse_meeting_url(
                "https://teams.microsoft.com/dl/launcher/launcher.html\
                 ?url=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij"
            )
            .is_none(),
            "лаунчер Teams не имеет права разворачиваться во встречу чужой площадки"
        );
        // Лаунчер без параметра — просто страница, а не встреча.
        assert!(parse_meeting_url("https://teams.microsoft.com/dl/launcher/launcher.html").is_none());
        // Вложенный лаунчер разворачивается ОДИН раз и упирается в потолок.
        assert!(parse_meeting_url(
            "https://teams.microsoft.com/dl/launcher/launcher.html\
             ?url=https%3A%2F%2Fteams.microsoft.com%2Fdl%2Flauncher%2Flauncher.html%3Furl%3Dx"
        )
        .is_none());
        // Тот же путь у чужой платформы встречей не становится.
        assert!(parse_meeting_url(
            "https://meet.google.com/dl/launcher/launcher.html?url=https%3A%2F%2Fmeet.google.com%2Fabc-defg-hij"
        )
        .is_none());
    }

    /// АДРЕС ВХОДА ≠ КАНОНИЧЕСКИЙ АДРЕС, и только у Zoom.
    #[test]
    fn zoom_enters_through_the_web_client_and_the_others_do_not() {
        let zoom = link("https://us02web.zoom.us/j/83123456789?pwd=Qm9iSXM");
        let entry = bot_entry_url(&zoom);
        assert_eq!(entry, "https://app.zoom.us/wc/join/83123456789?pwd=Qm9iSXM");
        assert_ne!(entry, zoom.canonical_url, "канонический адрес обязан остаться прежним");
        assert!(
            zoom.canonical_url.starts_with("https://us02web.zoom.us/j/"),
            "{}",
            zoom.canonical_url
        );

        // Без пароля — без параметра, а не с пустым.
        let plain = link("https://zoom.us/j/83123456789");
        assert_eq!(bot_entry_url(&plain), "https://app.zoom.us/wc/join/83123456789");

        // Персональная комната номера не имеет — веб-клиент собирать не из чего.
        let personal = link("https://zoom.us/my/ivan.petrov");
        assert_eq!(bot_entry_url(&personal), personal.canonical_url);

        for raw in [
            "https://telemost.yandex.ru/j/60497752704486",
            "https://meet.google.com/abc-defg-hij",
            "https://teams.live.com/meet/9312345678901?p=abcDEF",
        ] {
            let l = link(raw);
            assert_eq!(bot_entry_url(&l), l.canonical_url, "{raw}");
        }
    }

    /// Пароль нужен для ВХОДА, но не для ИДЕНТИЧНОСТИ. Одну встречу рассылают
    /// и со ссылкой-с-паролем, и без неё; попади `pwd` в ключ — дедуп не
    /// сработал бы и бот вошёл бы дважды.
    #[test]
    fn join_parameters_stay_out_of_the_identity_key() {
        assert_eq!(
            key("https://us02web.zoom.us/j/83123456789?pwd=Qm9iSXM"),
            key("https://zoom.us/j/83123456789")
        );
        assert_eq!(
            key("https://teams.live.com/meet/9312345678901?p=abcDEF"),
            key("https://teams.live.com/meet/9312345678901")
        );
    }

    /// Вендорный поддомен Zoom — свойство аккаунта, а не встречи: номер
    /// глобально уникален.
    #[test]
    fn zoom_vendor_subdomains_do_not_split_the_key() {
        let a = key("https://us02web.zoom.us/j/83123456789");
        let b = key("https://us05web.zoom.us/j/83123456789");
        let c = key("https://zoom.us/wc/join/83123456789");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn meet_codes_are_case_insensitive() {
        assert_eq!(
            key("https://meet.google.com/ABC-DEFG-HIJ"),
            key("https://meet.google.com/abc-defg-hij")
        );
    }

    // --- сам реестр --------------------------------------------------------

    #[test]
    fn the_registry_covers_every_platform_exactly_once() {
        assert_eq!(PROVIDERS.len(), Platform::ALL.len());
        for p in Platform::ALL {
            // `provider()` паникует, если записи нет — это и есть проверка.
            assert_eq!(p.provider().platform, p);
            assert!(!p.provider().hosts.is_empty(), "{}", p.as_str());
            assert!(!p.provider().link_regex.is_empty(), "{}", p.as_str());
        }
    }

    #[test]
    fn platform_ids_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for p in Platform::ALL {
            assert!(seen.insert(p.as_str()));
            assert_eq!(Platform::parse(p.as_str()), Some(p));
        }
        assert_eq!(Platform::parse("skype"), None);
    }

    #[test]
    fn host_matching_requires_a_dot_boundary() {
        assert!(host_has_suffix("us02web.zoom.us", ".zoom.us"));
        assert!(!host_has_suffix("notzoom.us", ".zoom.us"));
        assert!(!host_has_suffix("zoom.us.evil.com", ".zoom.us"));
        assert!(!host_has_suffix("zoom.us", ".zoom.us"), "суффикс не равен самому хосту");
        // Образец без точки не считается суффиксом вовсе: иначе `zoom.us`
        // совпал бы с `notzoom.us`.
        assert!(!host_has_suffix("us02web.zoom.us", "zoom.us"));
    }

    /// Реестр уезжает наружу целиком, поэтому обязан сериализоваться.
    #[test]
    fn the_registry_serialises_for_the_status_route() {
        let json = serde_json::to_string(&PROVIDERS.iter().collect::<Vec<_>>()).unwrap();
        assert!(json.contains("\"telemost\""));
        assert!(json.contains("link_regex"));
        assert!(json.contains("\"audio\":true"));
    }

    /// КОНТРАКТ С ФРОНТОМ (`messenger/meet-core.js::normalizePlatforms`).
    ///
    /// Фронт отбрасывает запись без хостов ИЛИ без шаблонов пути — поштучно и
    /// МОЛЧА. Отдай мы реестр без `paths`, он был бы выброшен целиком, фронт
    /// остался бы на встроенном наборе, и весь смысл серверного источника
    /// истины пропал бы без единого сообщения. Тест держит именно это.
    #[test]
    fn every_registry_entry_satisfies_what_the_frontend_requires() {
        for p in &PROVIDERS {
            let id = p.platform.as_str();
            assert!(
                !p.hosts.is_empty() || !p.host_suffixes.is_empty(),
                "{id}: ни хостов, ни суффиксов — фронт выбросит запись"
            );
            assert!(!p.paths.is_empty(), "{id}: нет шаблонов пути — фронт выбросит запись");
            for suffix in p.host_suffixes {
                assert!(
                    suffix.starts_with('.') && suffix.len() > 1,
                    "{id}: суффикс {suffix:?} обязан начинаться с точки, иначе фронт его отбросит"
                );
            }
            for host in p.hosts {
                assert!(!host.starts_with('.'), "{id}: {host:?} — это суффикс, а не хост");
            }
            assert!(
                matches!(p.key_case, "lower" | "preserve"),
                "{id}: key_case = {:?}",
                p.key_case
            );
            assert!(!p.title.is_empty(), "{id}: пустое имя");
        }
    }

    /// Идентификаторы платформ обязаны совпадать со встроенным реестром
    /// фронта: серверный реестр ЗАМЕНЯЕТ фронтовый целиком, и разные
    /// написания означали бы, что платформа зовётся по-разному в зависимости
    /// от того, ответил ли `/api/meet/status`.
    #[test]
    fn the_platform_ids_match_the_frontend_registry() {
        let ids: Vec<&str> = Platform::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(ids, vec!["telemost", "meet", "zoom", "teams"]);
    }

    /// У платформы обязана быть ОДНА строковая форма.
    ///
    /// В базу и в `url_key` уезжает [`Platform::as_str`], а в JSON роутов и в
    /// WS-кадры — serde. Разойдись они (а `rename_all = "snake_case"` именно
    /// это и делал: `google_meet` против `meet`), лента показывала бы
    /// платформу, по которой в базе ничего не находится — и заметить это
    /// можно было бы только глазами.
    #[test]
    fn the_serde_form_equals_the_string_form() {
        for p in Platform::ALL {
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", p.as_str()),
                "serde и as_str разошлись у {p:?}"
            );
            // И обратно: то, что уехало в JSON, обязано разобраться нашим же
            // `parse` — иначе строка из WS-кадра не нашлась бы в реестре.
            let back: Platform = serde_json::from_str(&json).unwrap();
            assert_eq!(back, p);
            assert_eq!(Platform::parse(p.as_str()), Some(p));
        }
    }
}
