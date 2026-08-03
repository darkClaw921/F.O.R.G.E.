//! Имена и присутствие собеседников.
//!
//! У Telegram-плагина одноимённый модуль решает другую задачу — добывает
//! `access_hash`, без которого MTProto не умеет адресовать peer'а. У VK
//! адресация тривиальна (`peer_id` — самодостаточное число), поэтому здесь
//! остаётся ровно одно: **превратить идентификатор в отображаемое имя, не
//! потратив на это лимит запросов**.
//!
//! # Главное правило экономии бюджета 3 rps
//!
//! ПЕРВЫЙ источник имён — блоки `profiles` и `groups`, которые VK кладёт в
//! ответ при `extended=1` (`messages.getConversations`, `messages.getHistory`,
//! `messages.getLongPollHistory`). Они приходят БЕСПЛАТНО, вместе с данными,
//! за которыми мы и так пришли, — [`PeerCache::absorb_extended`] просто
//! забирает их из уже полученного ответа и сети не касается вовсе.
//!
//! Отдельные `users.get` / `groups.getById` ([`PeerCache::resolve_uncached`])
//! вызываются ТОЛЬКО для тех id, которых в ответе не оказалось, и ПАЧКАМИ
//! (`user_ids`/`group_ids` через запятую). Резолв по одному id за вызов при
//! бюджете три запроса в секунду означал бы, что беседа из тридцати
//! участников открывается десять секунд.
//!
//! ```text
//!   ответ VK (extended=1)
//!        │ absorb_extended   ← 0 запросов
//!        ▼
//!   PeerCache (id → PeerInfo, TTL + потолок ёмкости)
//!        │ промах
//!        ▼
//!   resolve_uncached: users.get(до 100 id) + groups.getById(до 100 id)
//!        │ не нашли
//!        ▼
//!   имя = числовой id (строка всё равно рисуется)
//! ```
//!
//! # Почему кэш ограничен и по времени, и по размеру
//!
//! **TTL** нужен потому, что имя, `screen_name` и особенно присутствие
//! («онлайн») протухают: пользователь, ушедший офлайн час назад, не должен
//! светиться зелёной точкой до перезапуска devforge.
//!
//! **Потолок ёмкости** нужен потому, что кэш наполняется ИЗ ОТВЕТОВ VK, то
//! есть его размером управляет не наш код: активная переписка с сотнями
//! участников за неделю работы процесса набила бы карту без единого шанса
//! когда-нибудь освободить память. Вытеснение — по возрасту записи.
//!
//! # Отрицательные и большие id
//!
//! Отрицательный — сообщество, положительный — пользователь,
//! `>= `[`crate::dto::CHAT_PEER_BASE`] — групповая беседа. Имя беседы приходит
//! в `chat_settings.title` вместе с самой беседой и отдельного резолва не
//! требует, поэтому такие id [`PeerCache`] игнорирует: попытка спросить о них
//! `users.get` вернула бы код 100 и потратила бы запрос впустую.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::api::schema::{GroupRaw, ProfileRaw};
use crate::api::VkClient;
use crate::api::VkError;
use crate::dto::{peer_kind, PeerNames};
use crate::login::VkUserDto;

/// Сколько секунд запись кэша считается свежей.
///
/// Десять минут — компромисс между «онлайн-точка врёт» и «каждое открытие
/// беседы стоит запроса». Присутствие в реальном времени всё равно приезжает
/// событиями LongPoll (Ф5), а кэш отвечает за имя и аватар, которые меняются
/// куда реже.
pub const PEER_TTL_SECS: i64 = 600;

/// Потолок числа записей. Вытеснение — самые старые (см. шапку модуля).
pub const PEER_CACHE_CAPACITY: usize = 2_000;

/// Сколько идентификаторов уходит в один `users.get` / `groups.getById`.
///
/// VK принимает до 1000 у `users.get`, но общий потолок держим на сотне:
/// длина тела запроса у нас не единственное ограничение, а 100 имён закрывают
/// любую страницу списка бесед или истории целиком.
pub const RESOLVE_BATCH: usize = 100;

/// Что мы знаем о собеседнике.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// `peer_id` (отрицательный у сообществ).
    pub id: i64,
    /// Отображаемое имя: «Имя Фамилия» либо название сообщества.
    pub title: String,
    /// `screen_name` без «@».
    pub username: Option<String>,
    /// Аватар 50×50 — адрес на серверах VK, наружу НЕ отдаётся (кадры и DTO
    /// его не несут; картинка, если понадобится, пойдёт через прокси).
    pub photo: Option<String>,
    /// В друзьях (`users.get(fields=is_friend)`).
    pub is_friend: bool,
    /// Сейчас онлайн.
    pub online: bool,
    /// Когда был в сети, unix-время.
    pub last_seen: Option<i64>,
    /// Аккаунт/сообщество удалено или заблокировано.
    pub deactivated: bool,
    /// Unix-время записи — база для [`PEER_TTL_SECS`].
    pub at: i64,
}

impl PeerInfo {
    /// Карточка для [`crate::dto::MessageDto::sender`].
    ///
    /// У сообщества фамилии нет, поэтому имя целиком уезжает в `first_name`:
    /// [`VkUserDto::display_name`] склеивает поля через пробел, и разложить
    /// «Тестовое сообщество» на два поля было бы порчей названия.
    pub fn to_user_dto(&self) -> VkUserDto {
        let mut first = self.title.clone();
        let mut last = None;
        if self.id > 0 {
            if let Some((f, l)) = self.title.split_once(' ') {
                first = f.to_string();
                last = Some(l.to_string());
            }
        }
        VkUserDto {
            id: self.id.to_string(),
            first_name: (!first.is_empty()).then_some(first),
            last_name: last,
            screen_name: self.username.clone(),
        }
    }
}

/// Кэш имён и присутствия.
///
/// Синхронный (`std::sync::RwLock`, не `tokio`) намеренно: ни одна операция
/// внутри не делает `.await`, а проекция DTO ([`crate::dto::PeerNames`])
/// вызывается на КАЖДОЙ строке ленты — асинхронный доступ означал бы точку
/// отмены и лишний `await` на горячем пути. Сетевой метод
/// [`PeerCache::resolve_uncached`] асинхронный, но lock в нём живёт только
/// вокруг чтения и записи, а не через запрос.
#[derive(Debug)]
pub struct PeerCache {
    inner: RwLock<HashMap<i64, PeerInfo>>,
    ttl: i64,
    capacity: usize,
}

impl Default for PeerCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerCache {
    /// Кэш с боевыми ограничениями.
    pub fn new() -> Self {
        Self::with_limits(PEER_TTL_SECS, PEER_CACHE_CAPACITY)
    }

    /// Кэш с явными ограничениями — для тестов TTL и вытеснения.
    pub fn with_limits(ttl: i64, capacity: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// Забирает имена из блоков `profiles`/`groups` расширенного ответа.
    ///
    /// ЧИСТАЯ и бесплатная: данные уже приехали вместе с беседами или
    /// историей. Вызывается сразу после КАЖДОГО `extended=1`-ответа — иначе
    /// смысла в самом `extended=1` нет.
    pub fn absorb_extended(&self, profiles: &[ProfileRaw], groups: &[GroupRaw]) {
        self.absorb_extended_at(profiles, groups, crate::dto::now_unix());
    }

    /// То же с явным «сейчас» (тесты TTL не должны спать десять минут).
    pub fn absorb_extended_at(&self, profiles: &[ProfileRaw], groups: &[GroupRaw], now: i64) {
        if profiles.is_empty() && groups.is_empty() {
            return;
        }
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            // Отравленный lock означает панику в другом потоке ПОД этим же
            // lock'ом. Терять из-за неё имена собеседников незачем: данные тут
            // производные, и худшее, что грозит, — лишний резолв.
            Err(poisoned) => poisoned.into_inner(),
        };
        for p in profiles {
            guard.insert(p.id, profile_info(p, now));
        }
        for g in groups {
            // В `peer_id` сообщество приходит СО ЗНАКОМ МИНУС, а в блоке
            // `groups` — без него. Разъезд этих двух представлений — самая
            // вероятная ошибка во всём модуле, поэтому знак ставится ровно
            // здесь, в одном месте.
            guard.insert(-g.id, group_info(g, now));
        }
        evict_over_capacity(&mut guard, self.capacity);
    }

    /// Свежая запись по id, если она есть.
    pub fn get(&self, id: i64) -> Option<PeerInfo> {
        self.get_at(id, crate::dto::now_unix())
    }

    /// То же с явным «сейчас».
    pub fn get_at(&self, id: i64, now: i64) -> Option<PeerInfo> {
        let guard = self.inner.read().ok()?;
        guard
            .get(&id)
            .filter(|i| now.saturating_sub(i.at) < self.ttl)
            .cloned()
    }

    /// Идентификаторы, о которых мы ничего (свежего) не знаем.
    ///
    /// Отсюда выброшены беседы (`>= CHAT_PEER_BASE`) и ноль: спрашивать о них
    /// `users.get` — гарантированный код 100 и потраченный впустую запрос.
    /// Результат дедуплицирован, порядок первого появления сохранён — так
    /// диффы тестов читаемы, а пачки детерминированы.
    pub fn missing(&self, ids: &[i64]) -> Vec<i64> {
        self.missing_at(ids, crate::dto::now_unix())
    }

    /// То же с явным «сейчас».
    pub fn missing_at(&self, ids: &[i64], now: i64) -> Vec<i64> {
        let mut out: Vec<i64> = Vec::new();
        for id in ids {
            let id = *id;
            if id == 0 || peer_kind(id) == "chat" {
                continue;
            }
            if out.contains(&id) || self.get_at(id, now).is_some() {
                continue;
            }
            out.push(id);
        }
        out
    }

    /// Досылает недостающие имена — ПАЧКАМИ и только те, которых нет в кэше.
    ///
    /// Делит вход на пользователей (положительные id) и сообщества
    /// (отрицательные), режет каждую половину по [`RESOLVE_BATCH`] и делает по
    /// одному вызову на пачку. Пустой вход — ноль запросов.
    ///
    /// Ошибка ОДНОЙ пачки не отменяет остальные: имена — украшение, а не
    /// данные, и терять из-за одного отказа всю страницу истории нельзя.
    /// Возвращается последняя ошибка (для лога), уже полученные имена при этом
    /// остаются в кэше.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK, случившаяся хотя бы в одной пачке.
    pub async fn resolve_uncached(&self, client: &VkClient, ids: &[i64]) -> Result<(), VkError> {
        let missing = self.missing(ids);
        if missing.is_empty() {
            return Ok(());
        }
        let users: Vec<i64> = missing.iter().copied().filter(|i| *i > 0).collect();
        let groups: Vec<i64> = missing.iter().copied().filter(|i| *i < 0).map(i64::abs).collect();

        let mut last_err: Option<VkError> = None;
        let now = crate::dto::now_unix();

        for chunk in users.chunks(RESOLVE_BATCH) {
            match client.users_get(chunk).await {
                Ok(profiles) => self.absorb_extended_at(&profiles, &[], now),
                Err(e) => {
                    tracing::debug!(
                        target: "forge_vk",
                        count = chunk.len(),
                        error = %e,
                        "forge-vk: user names are left unresolved"
                    );
                    last_err = Some(e);
                }
            }
        }
        for chunk in groups.chunks(RESOLVE_BATCH) {
            match client.groups_get_by_id(chunk).await {
                Ok(list) => self.absorb_extended_at(&[], &list, now),
                Err(e) => {
                    tracing::debug!(
                        target: "forge_vk",
                        count = chunk.len(),
                        error = %e,
                        "forge-vk: community names are left unresolved"
                    );
                    last_err = Some(e);
                }
            }
        }

        match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Сколько записей сейчас в кэше (для логов и тестов).
    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    /// Пуст ли кэш.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Забывает всё — вызывается при logout вместе с остальными данными
    /// аккаунта: имена собеседников прошлого токена не должны переживать
    /// отвязку.
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.write() {
            g.clear();
        }
    }
}

impl PeerNames for PeerCache {
    fn sender(&self, id: i64) -> Option<VkUserDto> {
        self.get(id).map(|i| i.to_user_dto())
    }

    fn is_friend(&self, id: i64) -> bool {
        self.get(id).is_some_and(|i| i.is_friend)
    }
}

// ---------------------------------------------------------------------------
// Вспомогательное.
// ---------------------------------------------------------------------------

/// `profiles[i]` → запись кэша.
fn profile_info(p: &ProfileRaw, now: i64) -> PeerInfo {
    let title = [p.first_name.trim(), p.last_name.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    PeerInfo {
        id: p.id,
        title: if title.is_empty() {
            p.id.to_string()
        } else {
            title
        },
        username: non_empty(&p.screen_name),
        photo: non_empty(&p.photo_50),
        is_friend: p.is_friend == 1,
        online: p.online == 1,
        last_seen: p.last_seen.as_ref().map(|l| l.time).filter(|t| *t > 0),
        deactivated: !p.deactivated.is_empty(),
        at: now,
    }
}

/// `groups[i]` → запись кэша (id уже со знаком минус, см. `absorb_extended_at`).
fn group_info(g: &GroupRaw, now: i64) -> PeerInfo {
    let name = g.name.trim();
    PeerInfo {
        id: -g.id,
        title: if name.is_empty() {
            format!("Сообщество {}", g.id)
        } else {
            name.to_string()
        },
        username: non_empty(&g.screen_name),
        photo: non_empty(&g.photo_50),
        // У сообществ «дружбы» и присутствия нет вовсе — не выдумываем.
        is_friend: false,
        online: false,
        last_seen: None,
        deactivated: !g.deactivated.is_empty(),
        at: now,
    }
}

/// Выбрасывает самые старые записи, пока карта не влезет в потолок.
fn evict_over_capacity(map: &mut HashMap<i64, PeerInfo>, capacity: usize) {
    if map.len() <= capacity {
        return;
    }
    let mut by_age: Vec<(i64, i64)> = map.iter().map(|(id, i)| (*id, i.at)).collect();
    // Сортировка по (возраст, id): без второго ключа порядок вытеснения
    // записей с одинаковым временем зависел бы от порядка обхода HashMap, то
    // есть от случайного seed'а процесса.
    by_age.sort_by_key(|(id, at)| (*at, *id));
    let excess = map.len() - capacity;
    for (id, _) in by_age.into_iter().take(excess) {
        map.remove(&id);
    }
}

/// Непустая строка или `None`.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::LastSeenRaw;

    fn profile(id: i64, first: &str, last: &str) -> ProfileRaw {
        ProfileRaw {
            id,
            first_name: first.into(),
            last_name: last.into(),
            screen_name: format!("user{id}"),
            photo_50: format!("https://sun.example/{id}.jpg"),
            photo_100: format!("https://sun.example/{id}_100.jpg"),
            online: 1,
            last_seen: Some(LastSeenRaw {
                time: 1_700_000_000,
                platform: 7,
            }),
            is_friend: 1,
            deactivated: String::new(),
        }
    }

    fn group(id: i64, name: &str) -> GroupRaw {
        GroupRaw {
            id,
            name: name.into(),
            screen_name: format!("club{id}"),
            photo_50: format!("https://sun.example/g{id}.jpg"),
            photo_100: format!("https://sun.example/g{id}_100.jpg"),
            kind: "group".into(),
            deactivated: String::new(),
        }
    }

    /// Бесплатный источник имён: всё, что нужно, уже приехало в ответе.
    #[test]
    fn absorb_extended_fills_the_cache_without_any_network() {
        let cache = PeerCache::new();
        cache.absorb_extended(&[profile(1234, "Павел", "Дуров")], &[group(55, "Клуб")]);

        let user = cache.get(1234).unwrap();
        assert_eq!(user.title, "Павел Дуров");
        assert_eq!(user.username.as_deref(), Some("user1234"));
        assert!(user.is_friend && user.online);
        assert_eq!(user.last_seen, Some(1_700_000_000));

        // ЗНАК: в блоке `groups` id положительный, а в `peer_id` — со знаком
        // минус. Разъезд этих представлений — самая вероятная ошибка модуля.
        assert!(cache.get(55).is_none(), "положительный id — это пользователь");
        let club = cache.get(-55).unwrap();
        assert_eq!(club.title, "Клуб");
        assert!(!club.is_friend && !club.online);

        assert_eq!(cache.len(), 2);
    }

    /// Проекция карточки автора: у пользователя имя разбивается на два поля,
    /// у сообщества — остаётся целым названием.
    #[test]
    fn user_dto_projection_keeps_community_names_whole() {
        let cache = PeerCache::new();
        cache.absorb_extended(&[profile(1234, "Павел", "Дуров")], &[group(55, "Тестовое сообщество")]);

        let u = cache.sender(1234).unwrap();
        assert_eq!(u.first_name.as_deref(), Some("Павел"));
        assert_eq!(u.last_name.as_deref(), Some("Дуров"));
        assert_eq!(u.display_name(), "Павел Дуров");

        let g = cache.sender(-55).unwrap();
        assert_eq!(g.first_name.as_deref(), Some("Тестовое сообщество"));
        assert!(g.last_name.is_none(), "у сообщества фамилии нет");
        assert_eq!(g.display_name(), "Тестовое сообщество");

        assert!(cache.is_friend(1234));
        assert!(!cache.is_friend(-55));
        assert!(!cache.is_friend(999), "о незнакомце ничего не утверждаем");
    }

    /// Пустой профиль не должен давать пустую строку в ленте: подписью
    /// становится сам id.
    #[test]
    fn nameless_profiles_fall_back_to_their_id() {
        let cache = PeerCache::new();
        cache.absorb_extended(
            &[ProfileRaw {
                id: 7,
                ..ProfileRaw::default()
            }],
            &[GroupRaw {
                id: 9,
                ..GroupRaw::default()
            }],
        );
        assert_eq!(cache.get(7).unwrap().title, "7");
        assert_eq!(cache.get(-9).unwrap().title, "Сообщество 9");
        assert!(cache.get(7).unwrap().username.is_none());
    }

    /// `missing` — единственное место, решающее, за что вообще платить
    /// запросом: беседы и ноль исключены, дубликаты схлопнуты, известные
    /// пропущены.
    #[test]
    fn missing_skips_chats_zero_duplicates_and_known_ids() {
        let cache = PeerCache::with_limits(600, 100);
        cache.absorb_extended_at(&[profile(1234, "П", "Д")], &[], 1_000);

        let asked = [
            1234,              // уже знаем
            999,               // не знаем
            999,               // дубль
            -55,               // сообщество
            0,                 // не бывает
            2_000_000_007,     // беседа: имя придёт в chat_settings
            i64::MAX,          // тоже беседа по диапазону
        ];
        assert_eq!(cache.missing_at(&asked, 1_000), vec![999, -55]);
        assert!(cache.missing_at(&[], 1_000).is_empty());
    }

    /// TTL: по истечении запись перестаёт отвечать и снова попадает в
    /// `missing` — иначе «онлайн» держался бы до перезапуска процесса.
    #[test]
    fn entries_expire_by_ttl() {
        let cache = PeerCache::with_limits(600, 100);
        cache.absorb_extended_at(&[profile(1234, "П", "Д")], &[], 1_000);

        assert!(cache.get_at(1234, 1_000).is_some());
        assert!(cache.get_at(1234, 1_599).is_some(), "внутри TTL");
        assert!(cache.get_at(1234, 1_600).is_none(), "ровно на границе — уже нет");
        assert_eq!(cache.missing_at(&[1234], 1_600), vec![1234]);

        // Повторное поглощение освежает запись.
        cache.absorb_extended_at(&[profile(1234, "П", "Д")], &[], 1_600);
        assert!(cache.get_at(1234, 1_600).is_some());
    }

    /// Кэш наполняется ИЗ ОТВЕТОВ VK, то есть его ростом управляет не наш
    /// код — потолок обязателен, иначе неделя работы процесса набивает карту
    /// без шанса освободить память.
    #[test]
    fn cache_never_grows_past_its_capacity() {
        let cache = PeerCache::with_limits(10_000, 50);
        for batch in 0..10 {
            let profiles: Vec<ProfileRaw> = (0..20)
                .map(|i| profile(batch * 100 + i + 1, "И", "П"))
                .collect();
            cache.absorb_extended_at(&profiles, &[], 1_000 + batch);
        }
        assert_eq!(cache.len(), 50, "потолок соблюдён");

        // Вытесняются САМЫЕ СТАРЫЕ: последняя пачка обязана уцелеть целиком.
        for i in 0..20 {
            assert!(
                cache.get_at(900 + i + 1, 1_009).is_some(),
                "запись {} из последней пачки вытеснена",
                900 + i + 1
            );
        }
        assert!(cache.get_at(1, 1_009).is_none(), "первая пачка вытеснена");
    }

    #[test]
    fn clear_forgets_everything() {
        let cache = PeerCache::new();
        cache.absorb_extended(&[profile(1, "A", "B")], &[group(2, "C")]);
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
        assert!(cache.sender(1).is_none());
    }

    /// Деактивированный аккаунт помечается, а не выбрасывается: его сообщения
    /// в истории остаются, и подписывать их нечем, кроме имени.
    #[test]
    fn deactivated_accounts_are_flagged_not_dropped() {
        let cache = PeerCache::new();
        cache.absorb_extended(
            &[ProfileRaw {
                id: 5,
                first_name: "Удалённый".into(),
                deactivated: "deleted".into(),
                ..ProfileRaw::default()
            }],
            &[],
        );
        let info = cache.get(5).unwrap();
        assert!(info.deactivated);
        assert_eq!(info.title, "Удалённый");
    }

    /// Пустой вход не должен ни трогать lock, ни менять содержимое.
    #[test]
    fn absorbing_nothing_changes_nothing() {
        let cache = PeerCache::new();
        cache.absorb_extended(&[], &[]);
        assert!(cache.is_empty());
    }

    /// Резолв без сети: пустой список недостающих ⇒ ни одного запроса. Клиент
    /// намеренно указывает на закрытый порт — если бы вызов случился, тест
    /// упал бы транспортной ошибкой.
    #[tokio::test]
    async fn resolve_uncached_makes_no_calls_when_everything_is_known() {
        use crate::config::{SecretToken, VkConfig};
        use std::sync::Arc;
        use tokio::sync::RwLock as AsyncRwLock;

        let client = VkClient::new(
            &VkConfig {
                access_token: SecretToken::new("vk1.a.x"),
                base_url: "http://127.0.0.1:1/method".into(),
                ..VkConfig::default()
            },
            Arc::new(AsyncRwLock::new(crate::state::ConnHealth::default())),
        )
        .unwrap();

        let cache = PeerCache::new();
        cache.absorb_extended(&[profile(1234, "П", "Д")], &[group(55, "Клуб")]);

        // Всё известно либо не требует резолва (беседа, ноль).
        cache
            .resolve_uncached(&client, &[1234, -55, 2_000_000_007, 0])
            .await
            .expect("ни одного запроса — ни одной ошибки");
    }
}
