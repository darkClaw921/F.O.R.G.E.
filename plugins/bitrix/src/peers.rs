//! Имена, аватары и присутствие сотрудников портала.
//!
//! # Задача модуля — превратить id в имя, не потратив лимит
//!
//! Бюджет портала — 2 запроса в секунду на пару портал+IP (§4). Резолв имени
//! по одному идентификатору за вызов означал бы, что чат из тридцати
//! участников открывается пятнадцать секунд. Поэтому здесь два источника, и
//! первый из них бесплатный:
//!
//! ```text
//!   ответ портала (recent / dialog.get / событие)
//!        │ absorb_users        ← 0 запросов: данные УЖЕ приехали
//!        ▼
//!   PeerCache (id → PeerInfo, TTL + потолок ёмкости)
//!        │ промах
//!        ▼
//!   resolve_uncached: ОДИН batch с im.user.list.get по 50 id в команде
//!        │ не нашли
//!        ▼
//!   имя = числовой id (строка всё равно рисуется)
//! ```
//!
//! Первый источник — блоки `user` в строках `im.recent.get`, массивы
//! `users[]` в `im.dialog.messages.get` и `data.user` в событиях
//! `im.v2.Event.get`. Все они приезжают ВМЕСТЕ с данными, за которыми мы и так
//! пришли, поэтому [`PeerCache::absorb_users`] сети не касается вовсе.
//!
//! # Почему `im.user.list.get`, а не `user.get`
//!
//! Метода `user.get` у входящего вебхука НЕТ: он требует scope `user`,
//! которого вебхуку не выдают (живая проверка — `insufficient_scope`).
//! Доступны только `im.*`-методы, и единственный пакетный среди них —
//! `im.user.list.get`. Он же несёт ловушку формата: **возвращает КАРТУ**
//! `{"15":{…},"19":{…}}`, а не массив, и пустой результат приезжает как `[]`
//! (сериализация PHP).
//!
//! # Почему кэш ограничен и по времени, и по размеру
//!
//! **TTL** — потому что присутствие (`online`) протухает: сотрудник, ушедший
//! час назад, не должен светиться зелёной точкой до перезапуска devforge.
//! Realtime-присутствия у платформы нет вовсе (§6 плана), обновление приходит
//! только фолбэк-синхронизацией раз в минуту, поэтому кэш обязан сам признавать
//! свои данные устаревшими.
//!
//! **Потолок ёмкости** — потому что кэш наполняется ИЗ ОТВЕТОВ ПОРТАЛА, то
//! есть его ростом управляет не наш код. На портале в тысячи сотрудников
//! неделя работы процесса набила бы карту без единого шанса освободить память.
//! Вытеснение — по возрасту записи.
//!
//! # Чего здесь нет
//!
//! Аватары кэшируются как АДРЕСА, но наружу не отдаются: в `DialogDto` уезжает
//! путь прокси `/api/bitrix/media/avatar/:user_id` (Р6), а CDN-адрес портала
//! остаётся внутри процесса. Инвариант «внешних URL в DOM нет» держится
//! единообразно с VK и Telegram, несмотря на то что аватары портала публичны.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::api::batch::BatchCall;
use crate::api::methods::{user_list_params, M_USER_LIST_GET};
use crate::api::schema::{UserMapRaw, UserRaw};
use crate::api::{B24Client, B24Error};
use crate::dto::PeerNames;

/// Сколько секунд запись считается свежей.
///
/// Десять минут — компромисс между «зелёная точка врёт» и «каждое открытие
/// диалога стоит запроса». Имя и цвет меняются куда реже присутствия, но
/// разделять TTL по полям означало бы две карты вместо одной ради экономии,
/// которой не видно.
pub const PEER_TTL_SECS: i64 = 600;

/// Потолок числа записей; вытесняются самые старые.
pub const PEER_CACHE_CAPACITY: usize = 2_000;

/// Сколько идентификаторов уезжает в ОДНУ команду `im.user.list.get`.
///
/// Полсотни закрывают любую страницу списка диалогов целиком, а вместе с
/// автонарезкой `batch` (тоже 50 команд) это значит, что 2500 неизвестных имён
/// стоят одного HTTP-запроса.
pub const RESOLVE_BATCH: usize = 50;

/// Что известно о сотруднике портала.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerInfo {
    /// Идентификатор пользователя портала.
    pub id: i64,
    /// Отображаемое имя: `name`, либо склейка `first_name`+`last_name`, либо
    /// сам идентификатор — пустой подписи в ленте быть не должно.
    pub title: String,
    /// Имя.
    pub first_name: Option<String>,
    /// Фамилия. У ботов приходит `null`.
    pub last_name: Option<String>,
    /// Должность (`null` либо строка — одно из «шатающихся» полей).
    pub work_position: Option<String>,
    /// Цвет заглушки аватара (`#df532d`).
    pub color: Option<String>,
    /// CDN-адрес фото сотрудника — **внутренняя величина** (Ф8, Р6).
    ///
    /// Наружу не отдаётся: в DTO уезжает путь прокси
    /// `/api/bitrix/media/avatar/<id>`, а этот адрес виден ровно одному
    /// потребителю — [`crate::routes::media`], который по нему скачивает байты.
    ///
    /// Секретом он не является (CDN портала публичен), но инвариант «внешних
    /// URL в DOM нет» держится единообразно с VK и Telegram: одно исключение
    /// означало бы, что проверять придётся каждое поле каждого DTO.
    ///
    /// # Откуда берётся бесплатно
    ///
    /// Из блока `user` события `ONIMV2MESSAGEADD` и из строк `im.recent.get` —
    /// оба приезжают и так, поэтому отдельного запроса ради аватара не нужно
    /// (см. [`PeerCache::absorb_users`]).
    pub avatar: Option<String>,
    /// Это бот.
    pub bot: bool,
    /// Внешний пользователь (экстранет).
    pub extranet: bool,
    /// Сейчас онлайн.
    pub online: bool,
    /// Когда был активен (ATOM). `false` от портала превращается в `None`.
    pub last_activity: Option<String>,
    /// День рождения (`""` и `false` одинаково означают «нет»).
    pub birthday: Option<String>,
    /// Unix-время записи — база для [`PEER_TTL_SECS`].
    pub at: i64,
}

impl PeerInfo {
    /// Проекция сырого пользователя портала в запись кэша.
    ///
    /// Здесь же гасятся все три «шатающихся» поля: `birthday` (`""` либо
    /// `false`), `last_activity_date` (datetime либо `false`) и
    /// `work_position` (`null` либо строка).
    pub fn from_raw(raw: &UserRaw, now: i64) -> Self {
        let id = raw.id.as_i64().unwrap_or_default();
        Self {
            id,
            title: display_name(raw, id),
            first_name: non_empty(raw.first_name.as_deref()),
            last_name: non_empty(raw.last_name.as_deref()),
            work_position: non_empty(raw.work_position.as_deref()),
            color: non_empty(raw.color.as_deref()),
            // `avatar` (resize_cache) идёт первым, `avatar_hr` — запасным:
            // кружок в колонке и в ленте занимает три десятка пикселей, и
            // качать ради него полноразмерное фото значило бы тратить трафик и
            // место в кэше на невидимую разницу.
            avatar: real_avatar_url(&raw.avatar).or_else(|| real_avatar_url(&raw.avatar_hr)),
            bot: raw.bot || raw.kind.as_deref() == Some("bot"),
            extranet: raw.extranet,
            online: raw.status.as_deref() == Some("online"),
            last_activity: raw.last_activity_date.as_opt().map(str::to_string),
            birthday: raw.birthday.as_opt().map(str::to_string),
            at: now,
        }
    }

    /// Карточка владельца/автора для DTO.
    pub fn to_user_dto(&self) -> crate::login::B24UserDto {
        crate::login::B24UserDto {
            id: self.id.to_string(),
            name: self.first_name.clone().or_else(|| Some(self.title.clone())),
            last_name: self.last_name.clone(),
            // «Администратор ли портала» из `im.*`-методов не приходит вовсе:
            // это поле есть только у `profile`, то есть у владельца вебхука.
            admin: false,
        }
    }
}

/// Отображаемое имя из сырого пользователя.
///
/// Порядок источников: готовое `name` портала → склейка `first_name` +
/// `last_name` → идентификатор строкой. Последняя ступень обязательна: строка
/// списка с пустым заголовком выглядит как поломка, а не как «имя неизвестно».
fn display_name(raw: &UserRaw, id: i64) -> String {
    if let Some(name) = non_empty(raw.name.as_deref()) {
        return name;
    }
    let joined = [raw.first_name.as_deref(), raw.last_name.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        id.to_string()
    } else {
        joined
    }
}

/// Непустая строка либо `None`.
fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Адрес НАСТОЯЩЕГО фото сотрудника либо `None`.
///
/// # Три написания одного «нет аватара»
///
/// Портал присылает вместо пустоты три разных значения, и все они означают одно
/// и то же:
///
/// | значение | откуда |
/// |---|---|
/// | `false` | `im.user.list.get` у сотрудника без фото |
/// | `""` | `im.recent.get` в блоке `avatar.url` |
/// | `…/blank.gif` | `im.user.get`, иногда абсолютным адресом, иногда относительным |
///
/// Проверять их по отдельности в каждом месте — верный способ однажды забыть
/// про третье и рисовать пустой квадрат вместо инициалов. Отсюда единственная
/// функция, которой пользуются и проекция DTO
/// ([`crate::dto::PeerNames::has_avatar`]), и прокси байтов (Ф8): «показать
/// картинку» и «есть что скачать» обязаны отвечать одинаково.
pub fn real_avatar_url(raw: &crate::api::schema::StrOrBool) -> Option<String> {
    raw.as_opt()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains("blank.gif"))
        .map(str::to_string)
}

/// Кэш сотрудников портала.
///
/// Синхронный (`std::sync::RwLock`, не `tokio`) намеренно: ни одна операция
/// внутри не делает `.await`, а проекция DTO ([`crate::dto::PeerNames`])
/// вызывается на КАЖДОЙ строке списка — асинхронный доступ добавил бы точку
/// отмены на горячем пути. Сетевой [`PeerCache::resolve_uncached`] асинхронен,
/// но lock в нём живёт только вокруг чтения и записи, а не через запрос.
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

    /// БЕСПЛАТНОЕ поглощение: забирает пользователей из уже полученного ответа.
    ///
    /// Вызывается сразу после КАЖДОГО ответа, который несёт блок `user` или
    /// `users[]`, — иначе смысла в этих блоках нет и каждое имя пришлось бы
    /// докупать отдельным запросом.
    pub fn absorb_users(&self, users: &[UserRaw]) {
        self.absorb_users_at(users, crate::dto::now_unix());
    }

    /// То же с явным «сейчас» (тест TTL не должен спать десять минут).
    pub fn absorb_users_at(&self, users: &[UserRaw], now: i64) {
        if users.is_empty() {
            return;
        }
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            // Отравленный lock — это паника в другом потоке ПОД этим же
            // lock'ом. Терять из-за неё имена незачем: данные производные, и
            // худшее, что грозит, — лишний резолв.
            Err(poisoned) => poisoned.into_inner(),
        };
        for raw in users {
            let info = PeerInfo::from_raw(raw, now);
            // `{"id":0}` — так портал заполняет блок `user` у ГРУППОВОГО чата.
            // Это не пользователь, а заглушка формы.
            if info.id <= 0 {
                continue;
            }
            guard.insert(info.id, info);
        }
        evict_over_capacity(&mut guard, self.capacity);
    }

    /// Поглощение карты `{"15":{…}}` — формы ответа `im.user.list.get`.
    ///
    /// Ключ карты игнорируется: идентификатор берётся из самой записи. Ключ и
    /// поле `id` у портала всегда совпадают, но доверять ключу означало бы
    /// иметь два источника одного значения.
    pub fn absorb_map_at(&self, users: &HashMap<String, UserRaw>, now: i64) {
        if users.is_empty() {
            return;
        }
        let list: Vec<UserRaw> = users.values().cloned().collect();
        self.absorb_users_at(&list, now);
    }

    /// Свежая запись по id.
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

    /// CDN-адрес фото сотрудника — ТОЛЬКО для прокси аватаров (Ф8, Р6).
    ///
    /// Единственный вызывающий — `routes::media::get_avatar`. В DTO это
    /// значение не попадает никогда: там живёт путь прокси
    /// [`crate::dto::AVATAR_PATH_PREFIX`]`<id>`.
    ///
    /// Читает запись БЕЗ оглядки на TTL: адрес фото — не присутствие, он не
    /// «протухает» через десять минут, а перевыпуск ссылки портал делает вместе
    /// со сменой самого фото. Свежий ответ портала перепишет запись сам, и
    /// несовпадение [`crate::media_cache::MediaMeta::source_tag`] выбросит
    /// старые байты из кэша.
    pub fn avatar(&self, id: i64) -> Option<String> {
        let guard = self.inner.read().ok()?;
        guard.get(&id).and_then(|i| i.avatar.clone())
    }

    /// Идентификаторы, о которых нет свежих сведений.
    ///
    /// Ноль и отрицательные выброшены: `author_id == 0` означает СИСТЕМНОЕ
    /// сообщение, а не пользователя, и спрашивать о нём портал — гарантированно
    /// потраченный запрос. Результат дедуплицирован, порядок первого появления
    /// сохранён — так пачки детерминированы, а диффы тестов читаемы.
    pub fn missing(&self, ids: &[i64]) -> Vec<i64> {
        self.missing_at(ids, crate::dto::now_unix())
    }

    /// То же с явным «сейчас».
    pub fn missing_at(&self, ids: &[i64], now: i64) -> Vec<i64> {
        let mut out: Vec<i64> = Vec::new();
        for id in ids.iter().copied() {
            if id <= 0 || out.contains(&id) || self.get_at(id, now).is_some() {
                continue;
            }
            out.push(id);
        }
        out
    }

    /// Досылает недостающие имена — ПАЧКАМИ и одним `batch`.
    ///
    /// Разбивает промахи по [`RESOLVE_BATCH`] и складывает КАЖДУЮ пачку
    /// отдельной командой в ОДИН `batch`: тридцать неизвестных сотрудников —
    /// это один HTTP-запрос и один хит лимита интенсивности, а не тридцать.
    /// Автонарезка `batch` (50 команд) при этом продолжает действовать, так что
    /// патологический случай в тысячи имён деградирует до нескольких запросов,
    /// а не до тысячи.
    ///
    /// Пустой вход — ноль запросов.
    ///
    /// Отказ ОТДЕЛЬНОЙ команды ошибкой не считается: имена — украшение, и
    /// терять из-за одного отказа всю страницу списка нельзя. Такие промахи
    /// просто остаются нерезолвленными, и подписью станет идентификатор.
    ///
    /// # Errors
    ///
    /// Отказ САМОГО `batch` — транспорт, оба лимита портала, отказ по
    /// существу.
    pub async fn resolve_uncached(&self, client: &B24Client, ids: &[i64]) -> Result<(), B24Error> {
        let missing = self.missing(ids);
        if missing.is_empty() {
            return Ok(());
        }

        let calls: Vec<BatchCall> = missing
            .chunks(RESOLVE_BATCH)
            .enumerate()
            .map(|(i, chunk)| {
                BatchCall::new(format!("users{i}"), M_USER_LIST_GET, user_list_params(chunk))
            })
            .collect();

        let outcome = client.batch(calls).await?;
        let now = crate::dto::now_unix();
        let mut resolved = 0usize;
        for value in outcome.ok.values() {
            // Форма ответа — КАРТА либо `[]`; ошибка разбора одной команды не
            // должна отменять остальные.
            if let Ok(map) = serde_json::from_value::<UserMapRaw>(value.clone()) {
                let map = map.into_map();
                resolved += map.len();
                self.absorb_map_at(&map, now);
            }
        }
        if resolved < missing.len() {
            tracing::debug!(
                target: "forge_bitrix",
                asked = missing.len(),
                resolved,
                errors = outcome.errors.len(),
                "forge-bitrix: some peer names are left unresolved"
            );
        }
        Ok(())
    }

    /// Сколько записей сейчас в кэше (логи и тесты).
    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }

    /// Пуст ли кэш.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Забывает всё — вызывается при отвязке портала вместе с остальными
    /// данными аккаунта: имена сотрудников прошлого вебхука не должны
    /// переживать logout.
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.write() {
            g.clear();
        }
    }
}

impl PeerNames for PeerCache {
    fn display_name(&self, id: i64) -> Option<String> {
        self.get(id).map(|i| i.title)
    }

    fn color(&self, id: i64) -> Option<String> {
        self.get(id).and_then(|i| i.color)
    }

    fn presence(&self, id: i64) -> Option<(bool, Option<String>)> {
        self.get(id).map(|i| (i.online, i.last_activity))
    }

    fn is_bot(&self, id: i64) -> bool {
        self.get(id).is_some_and(|i| i.bot)
    }

    /// Есть ли что показать в кружке аватара.
    ///
    /// Отвечает по СВЕЖЕЙ записи ([`PeerCache::get`] уважает TTL): протухшая
    /// запись означает «мы не знаем», и в DTO уедет `avatar_url: null`, то есть
    /// инициалы. Это честнее прокси-пути, который тут же ответил бы 404.
    fn has_avatar(&self, id: i64) -> bool {
        self.get(id).is_some_and(|i| i.avatar.is_some())
    }
}

/// Выбрасывает самые старые записи, пока карта не влезет в потолок.
fn evict_over_capacity(map: &mut HashMap<i64, PeerInfo>, capacity: usize) {
    if map.len() <= capacity {
        return;
    }
    let mut by_age: Vec<(i64, i64)> = map.iter().map(|(id, i)| (*id, i.at)).collect();
    // Сортировка по паре (возраст, id): без второго ключа порядок вытеснения
    // записей одного возраста зависел бы от случайного seed'а HashMap.
    by_age.sort_by_key(|(id, at)| (*at, *id));
    let excess = map.len() - capacity;
    for (id, _) in by_age.into_iter().take(excess) {
        map.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Живой пользователь портала (фикстура `recent_get.json`).
    fn live_user_json() -> &'static str {
        r##"{"id":15,"active":true,"name":"Максим Крылов","first_name":"Максим",
            "last_name":"Крылов","work_position":null,"color":"#3e99ce",
            "avatar":"https://cdn-ru.bitrix24.ru/b38365992/resize_cache/137/x.png",
            "avatar_hr":"https://cdn-ru.bitrix24.ru/b38365992/resize_cache/137/x.png",
            "gender":"M","birthday":"","extranet":false,"network":false,"bot":false,
            "connector":false,"external_auth_id":"socservices","status":"online",
            "idle":false,"last_activity_date":"2026-07-21T21:47:01+03:00",
            "mobile_last_date":false,"desktop_last_date":false,"absent":false,
            "departments":[1],"phones":false,"bot_data":null,"type":"user",
            "website":"","email":"open.biz@yandex.ru"}"##
    }

    fn user(id: i64, first: &str, last: &str) -> UserRaw {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": format!("{first} {last}"), "first_name": first,
            "last_name": last, "color": "#29619b", "status": "online",
            "birthday": "", "last_activity_date": false, "work_position": null
        }))
        .unwrap()
    }

    /// ФОРМА ОТВЕТА `im.user.list.get` — КАРТА, а не массив, и `[]` когда
    /// пусто.
    #[test]
    fn the_user_list_response_is_a_map_and_an_empty_php_array() {
        let map: UserMapRaw = serde_json::from_str(&format!(
            r#"{{"15":{},"19":{{"id":19,"name":"Алексей Пруцков","status":"offline"}}}}"#,
            live_user_json()
        ))
        .unwrap();
        let map = map.into_map();
        assert_eq!(map.len(), 2);
        assert_eq!(map["15"].id.as_i64(), Some(15));

        // Пусто — это `[]`, и строгая карта здесь падала бы в самом частом
        // случае.
        let empty: UserMapRaw = serde_json::from_str("[]").unwrap();
        assert!(empty.into_map().is_empty());
    }

    /// Три «шатающихся» поля обязаны пережить ОБЕ свои формы.
    #[test]
    fn the_wobbly_user_fields_survive_both_forms() {
        // Форма 1: `im.user.get` — пустая строка, datetime, null.
        let raw: UserRaw = serde_json::from_str(live_user_json()).unwrap();
        let info = PeerInfo::from_raw(&raw, 1_000);
        assert_eq!(info.birthday, None, "\"\" — это «дня рождения нет»");
        assert_eq!(
            info.last_activity.as_deref(),
            Some("2026-07-21T21:47:01+03:00")
        );
        assert_eq!(info.work_position, None);

        // Форма 2: `im.search.user.list` / бот — `false`, `false`, строка.
        let raw: UserRaw = serde_json::from_value(serde_json::json!({
            "id": 17, "name": "Команда заботы Битрикс24", "first_name": "Команда заботы",
            "last_name": null, "work_position": "Помогаем бизнесу работать",
            "birthday": false, "last_activity_date": false, "status": "online",
            "bot": true, "type": "bot"
        }))
        .unwrap();
        let info = PeerInfo::from_raw(&raw, 1_000);
        assert_eq!(info.birthday, None);
        assert_eq!(info.last_activity, None, "false — это «неизвестно»");
        assert_eq!(
            info.work_position.as_deref(),
            Some("Помогаем бизнесу работать")
        );
        assert_eq!(info.last_name, None, "у бота фамилии нет вовсе");
        assert!(info.bot);
    }

    /// Пустой ответ портала не должен ронять разбор ни одним полем.
    #[test]
    fn a_bare_user_object_parses_and_falls_back_to_its_id() {
        let raw: UserRaw = serde_json::from_str(r#"{"id":7}"#).unwrap();
        let info = PeerInfo::from_raw(&raw, 0);
        assert_eq!(info.title, "7", "пустая подпись выглядела бы как поломка");
        assert!(!info.online);
        assert!(info.color.is_none());
    }

    #[test]
    fn absorbing_fills_the_cache_without_any_network() {
        let cache = PeerCache::new();
        cache.absorb_users(&[user(15, "Максим", "Крылов"), user(19, "Алексей", "Пруцков")]);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.display_name(15).as_deref(), Some("Максим Крылов"));
        assert_eq!(cache.color(19).as_deref(), Some("#29619b"));
        assert_eq!(cache.presence(15), Some((true, None)));
        assert!(!cache.is_bot(15));
        assert!(cache.display_name(999).is_none());
    }

    /// `{"id":0}` — это заглушка блока `user` у ГРУППОВОГО чата, а не
    /// пользователь: попав в кэш, она подписала бы системные сообщения
    /// «пользователем 0».
    #[test]
    fn the_zero_user_placeholder_is_ignored() {
        let cache = PeerCache::new();
        let zero: UserRaw = serde_json::from_str(r#"{"id":0}"#).unwrap();
        cache.absorb_users(&[zero]);
        assert!(cache.is_empty());
        assert!(cache.get(0).is_none());
    }

    #[test]
    fn missing_skips_known_zero_and_duplicates() {
        let cache = PeerCache::with_limits(600, 100);
        cache.absorb_users_at(&[user(15, "М", "К")], 1_000);
        assert_eq!(
            cache.missing_at(&[15, 19, 19, 0, -1, 17], 1_000),
            vec![19, 17]
        );
        assert!(cache.missing_at(&[], 1_000).is_empty());
    }

    /// TTL: по истечении запись перестаёт отвечать и снова попадает в
    /// `missing` — иначе «онлайн» держался бы до перезапуска процесса.
    #[test]
    fn entries_expire_by_ttl() {
        let cache = PeerCache::with_limits(600, 100);
        cache.absorb_users_at(&[user(15, "М", "К")], 1_000);
        assert!(cache.get_at(15, 1_599).is_some(), "внутри TTL");
        assert!(cache.get_at(15, 1_600).is_none(), "ровно на границе — уже нет");
        assert_eq!(cache.missing_at(&[15], 1_600), vec![15]);

        cache.absorb_users_at(&[user(15, "М", "К")], 1_600);
        assert!(cache.get_at(15, 1_600).is_some(), "поглощение освежает запись");
    }

    /// Кэш растёт от ответов портала, а не от нашего кода, — потолок
    /// обязателен.
    #[test]
    fn the_cache_never_grows_past_its_capacity() {
        let cache = PeerCache::with_limits(10_000, 50);
        for batch in 0..10 {
            let users: Vec<UserRaw> = (0..20)
                .map(|i| user(batch * 100 + i + 1, "И", "П"))
                .collect();
            cache.absorb_users_at(&users, 1_000 + batch);
        }
        assert_eq!(cache.len(), 50);
        // Вытесняются САМЫЕ СТАРЫЕ: последняя пачка обязана уцелеть целиком.
        for i in 0..20 {
            assert!(cache.get_at(900 + i + 1, 1_009).is_some(), "id {}", 900 + i + 1);
        }
        assert!(cache.get_at(1, 1_009).is_none(), "первая пачка вытеснена");
    }

    #[test]
    fn clear_forgets_everything() {
        let cache = PeerCache::new();
        cache.absorb_users(&[user(15, "М", "К")]);
        cache.clear();
        assert!(cache.is_empty());
        assert!(cache.display_name(15).is_none());
    }

    /// Резолв без промахов не делает НИ ОДНОГО запроса: клиент указывает на
    /// заведомо закрытый порт, и любой вызов упал бы транспортной ошибкой.
    #[tokio::test]
    async fn resolve_uncached_makes_no_calls_when_everything_is_known() {
        use crate::config::{BitrixConfig, WebhookUrl};
        use std::sync::Arc;
        use tokio::sync::RwLock as AsyncRwLock;

        let client = B24Client::new(
            &BitrixConfig {
                base_url_override: "http://127.0.0.1:1/rest/1/stub".into(),
                connect_timeout_secs: 1,
                request_timeout_secs: 1,
                ..BitrixConfig::default()
            },
            &WebhookUrl::default(),
            Arc::new(AsyncRwLock::new(crate::state::ConnHealth::default())),
        )
        .unwrap();

        let cache = PeerCache::new();
        cache.absorb_users(&[user(15, "М", "К")]);
        cache
            .resolve_uncached(&client, &[15, 0, -3])
            .await
            .expect("ни одного промаха — ни одного запроса");
    }
}
