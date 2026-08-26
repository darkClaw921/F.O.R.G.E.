//! Пользовательские настройки панели, которые нужны АВТОРАЗБОРУ.
//!
//! Речь про `~/.forge/user_settings.json` — файл хоста (`tmux-web`), в котором
//! живут тумблер «Автоматически разбирать новые аудиосообщения» и белый список
//! видимости бесед. Плагин читает его САМ, вот этим модулем.
//!
//! # ПОЧЕМУ ПЛАГИН ЧИТАЕТ ЧУЖОЙ ФАЙЛ, А НЕ ПОЛУЧАЕТ НАСТРОЙКИ ОТ ХОСТА
//!
//! Потому что канала от хоста к этим плагинам НЕТ и заводить его дороже, чем
//! читать файл. У VK, Telegram и Bitrix нет своего `*-host-api`: хост знает про
//! них ровно `init` → `register_routes` → `spawn_workers` (см. шапки
//! `plugins/*/src/lib.rs`), и зависимости на `AppState` у них нет вовсе.
//! Завести её ради двух булевых значений значило бы сломать границу, которую
//! три плагина держат с первой фазы.
//!
//! Второй довод — «без перезапуска». Настройки, переданные при старте, надо
//! было бы обновлять по событию; файл же перечитывается на КАЖДОЕ входящее
//! голосовое, то есть переключённый в браузере тумблер действует со
//! следующего сообщения. Файл маленький (единицы килобайт), а голосовые
//! приходят единицами в минуту.
//!
//! # ЧТЕНИЕ НИКОГДА НЕ ПАДАЕТ
//!
//! Нет файла, битый JSON, чужие типы в полях — всё это [`Prefs::default`], то
//! есть «авторазбор выключен, фильтра нет». Это единственная безопасная
//! сторона отказа: неразобранное голосовое человек разберёт кнопкой, а вот
//! разбор при выключенном тумблере — трата GPU и денег за спиной человека.
//! По той же причине здесь `serde_json::Value`, а не строгая структура: хост
//! добавляет в этот файл поля постоянно, и `deny_unknown_fields`-подобная
//! строгость превратила бы каждое новое поле хоста в молчаливое выключение
//! фичи в трёх плагинах.
//!
//! # СЛОВАРЬ ПОЛЕЙ РАЗНЫЙ У ТРЁХ ПАНЕЛЕЙ
//!
//! У VK белый список — это ТИПЫ бесед (`user`/`chat`/`community`) плюс
//! `peer_id`, у Bitrix — ВИДЫ диалогов плюс `DIALOG_ID`, у Telegram — ПАПКИ
//! аккаунта плюс id чатов. Поэтому ключи задаются таблицей [`PrefsKeys`], а
//! решение «видна ли беседа» принимает плагин: только он умеет вывести вид
//! беседы из её идентификатора (`peerKind` во фронте, `kind_of_peer` в
//! плагине).
//!
//! # ДВА РАЗНЫХ БЕЛЫХ СПИСКА: ВИДИМОСТЬ И СКОУП АВТОРАЗБОРА
//!
//! `visible_*` — «какие беседы показывать в панели», `scope_*`
//! (`*_voice_scope_*` в файле хоста) — «в каких беседах разбирать голосовые
//! САМО». Списки отдельные, потому что просьбы разные: человек имеет право
//! читать чат, не оплачивая расшифровку каждого голосового в нём.
//!
//! Отличаются они и прочтением пустоты. У видимости пустой список — это
//! «ничего не выбрано» при отдельном тумблере `*_filter_enabled`, и
//! [`Prefs::filter_active`] обезвреживает сочетание «включён, но пуст». У
//! скоупа тумблера НЕТ вовсе, и **пустой скоуп означает «разбирать везде»**:
//! фича добавлена к уже работающему авторазбору, и любое другое прочтение
//! пустоты выключило бы его у всех, кто ни разу не открывал новую форму.
//!
//! Видимость остаётся ВЫШЕСТОЯЩИМ фильтром: скрытая беседа не разбирается,
//! даже если попала в скоуп ([`crate::auto::decide`], порядок ступеней).

use std::path::{Path, PathBuf};

/// Имя файла настроек пользователя.
pub const FILE_NAME: &str = "user_settings.json";

/// Имена полей одной панели в `user_settings.json`.
///
/// Таблицей, а не форматированием `format!("{prefix}_…")`: словарь у трёх
/// панелей разный (`vk_visible_kinds` против `telegram_visible_folders`), и
/// собранное из префикса имя разошлось бы с хостом молча — тумблер просто
/// перестал бы действовать, без единой ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefsKeys {
    /// Тумблер авторазбора (`*_voice_autotranscribe`).
    pub autotranscribe: &'static str,
    /// Тумблер белого списка (`*_filter_enabled`).
    pub filter_enabled: &'static str,
    /// Белый список видов/папок (`vk_visible_kinds`, …).
    pub visible_kinds: &'static str,
    /// Белый список бесед (`vk_visible_peers`, …).
    pub visible_peers: &'static str,
    /// Скоуп авторазбора по видам/папкам (`vk_voice_scope_kinds`, …).
    pub scope_kinds: &'static str,
    /// Скоуп авторазбора по беседам (`vk_voice_scope_peers`, …).
    pub scope_peers: &'static str,
}

/// Ключи панели VK.
pub const VK: PrefsKeys = PrefsKeys {
    autotranscribe: "vk_voice_autotranscribe",
    filter_enabled: "vk_filter_enabled",
    visible_kinds: "vk_visible_kinds",
    visible_peers: "vk_visible_peers",
    scope_kinds: "vk_voice_scope_kinds",
    scope_peers: "vk_voice_scope_peers",
};

/// Ключи панели Telegram. «Виды» здесь — ПАПКИ аккаунта.
pub const TELEGRAM: PrefsKeys = PrefsKeys {
    autotranscribe: "telegram_voice_autotranscribe",
    filter_enabled: "telegram_filter_enabled",
    visible_kinds: "telegram_visible_folders",
    visible_peers: "telegram_visible_dialogs",
    scope_kinds: "telegram_voice_scope_folders",
    scope_peers: "telegram_voice_scope_dialogs",
};

/// Ключи панели Битрикс24.
pub const BITRIX: PrefsKeys = PrefsKeys {
    autotranscribe: "bitrix_voice_autotranscribe",
    filter_enabled: "bitrix_filter_enabled",
    visible_kinds: "bitrix_visible_kinds",
    visible_peers: "bitrix_visible_peers",
    scope_kinds: "bitrix_voice_scope_kinds",
    scope_peers: "bitrix_voice_scope_peers",
};

/// Настройки одной панели, которые видит авторазбор.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prefs {
    /// Включён ли авторазбор новых голосовых.
    pub autotranscribe: bool,
    /// Включён ли режим «показывать только выбранное».
    pub filter_enabled: bool,
    /// Белый список видов/папок.
    pub visible_kinds: Vec<String>,
    /// Белый список бесед.
    pub visible_peers: Vec<String>,
    /// Скоуп авторазбора по видам/папкам. ПУСТО = разбирать везде.
    pub scope_kinds: Vec<String>,
    /// Скоуп авторазбора по беседам. ПУСТО = разбирать везде.
    pub scope_peers: Vec<String>,
}

impl Prefs {
    /// ДЕЙСТВУЕТ ли фильтр видимости.
    ///
    /// Включённый тумблер с пустым выбором фильтром НЕ считается — ровно как во
    /// фронте (`normalizeFilter().active`). Иначе человек, включивший режим и
    /// ничего не отметивший, видел бы пустую панель и при этом ни одного
    /// разобранного голосового: два разных прочтения одной настройки — худшее,
    /// что может быть в белом списке.
    #[must_use]
    pub fn filter_active(&self) -> bool {
        self.filter_enabled && !(self.visible_kinds.is_empty() && self.visible_peers.is_empty())
    }

    /// Видна ли беседа, у которой известны идентификатор и ГРУППЫ.
    ///
    /// Порядок проверок тот же, что во фронте: поимённый выбор сильнее группы.
    /// `groups` — уже приведённые плагином группы беседы: у VK и Bitrix это
    /// ровно один вид (`user`/`chat`/`community`, `user`/`chat`/`task`), у
    /// Telegram — ключи ПАПОК аккаунта, а их у чата может быть несколько
    /// (`inbox`, `folder:10`, …). Пустой срез — «группы неизвестны», и тогда
    /// решает только поимённый список.
    ///
    /// НЕИЗВЕСТНАЯ беседа при действующем фильтре считается НЕВИДИМОЙ. Это
    /// консервативная сторона: показать лишнее — мелкая неприятность, а
    /// разобрать голосовое из беседы, которую человек попросил не показывать, —
    /// это отправить её содержание в облако вопреки настройке.
    #[must_use]
    pub fn peer_visible(&self, peer: &str, groups: &[&str]) -> bool {
        if !self.filter_active() {
            return true;
        }
        let id = peer.trim();
        if id.is_empty() {
            return false;
        }
        if self.visible_peers.iter().any(|p| p == id) {
            return true;
        }
        any_group(&self.visible_kinds, groups)
    }

    /// ДЕЙСТВУЕТ ли скоуп авторазбора.
    ///
    /// Тумблера у скоупа НЕТ — в отличие от видимости, где есть
    /// `*_filter_enabled`. Действие определяется одной только непустотой
    /// списков, и это ровно то, что делает добавление фичи безопасным для уже
    /// работающих установок: пустой скоуп неотличим от его отсутствия.
    #[must_use]
    pub fn scope_active(&self) -> bool {
        !(self.scope_kinds.is_empty() && self.scope_peers.is_empty())
    }

    /// Попадает ли беседа в скоуп авторазбора.
    ///
    /// **ПУСТОЙ СКОУП — ЭТО `true` ВСЕГДА**, для любых `peer` и `kind`. Иначе
    /// появление полей молча выключило бы авторазбор у всех, кто его уже
    /// включил: белый список, которого человек ни разу не заполнял, обязан
    /// значить «во всех беседах».
    ///
    /// Дальше — та же семантика, что у [`Prefs::peer_visible`]: поимённый
    /// выбор сильнее группы, `kind` уже приведён плагином к словарю панели
    /// (`user`/`chat`/`community` у VK, `user`/`chat`/`task` у Bitrix, ключ
    /// папки у Telegram), `None` — «группа неизвестна», и тогда решает только
    /// поимённый список.
    ///
    /// Сторона отказа тоже общая с видимостью: при ДЕЙСТВУЮЩЕМ скоупе
    /// неизвестная беседа считается ВНЕ скоупа. Разобрать лишнее — это отдать
    /// содержание чужой беседы движку (и деньги облаку) вопреки настройке, а
    /// неразобранное человек разберёт кнопкой.
    ///
    /// Скоуп НЕ отменяет видимость: скрытая беседа не разбирается, даже если
    /// попала в скоуп, — ступень «беседа скрыта» стоит в
    /// [`crate::auto::decide`] РАНЬШЕ.
    #[must_use]
    pub fn peer_in_scope(&self, peer: &str, groups: &[&str]) -> bool {
        if !self.scope_active() {
            return true;
        }
        let id = peer.trim();
        if id.is_empty() {
            return false;
        }
        if self.scope_peers.iter().any(|p| p == id) {
            return true;
        }
        any_group(&self.scope_kinds, groups)
    }
}

/// Пересекается ли белый список групп с группами беседы.
///
/// Отдельной функцией, потому что правило одно на оба списка: ХОТЯ БЫ ОДНА
/// группа беседы отмечена. Множественность важна для Telegram — чат лежит
/// сразу в нескольких папках, и требовать совпадения всех значило бы, что
/// отмеченная папка не работает ни для одного чата, который есть ещё где-то.
fn any_group(whitelist: &[String], groups: &[&str]) -> bool {
    groups
        .iter()
        .any(|g| whitelist.iter().any(|w| w == g.trim()))
}

/// Путь `~/.forge/user_settings.json`.
///
/// Ровно тот же, что строит хост (`tmux-web/src/main.rs`): второе написание
/// означало бы, что плагин читает файл, которого никто не пишет.
#[must_use]
pub fn default_path() -> PathBuf {
    match std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()) {
        Some(home) => PathBuf::from(home).join(".forge").join(FILE_NAME),
        None => PathBuf::from(FILE_NAME),
    }
}

/// Читает настройки панели. Любой отказ — это [`Prefs::default`].
#[must_use]
pub fn read(path: &Path, keys: PrefsKeys) -> Prefs {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Prefs::default();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        tracing::warn!(
            target: "forge_voice",
            path = %path.display(),
            "forge-voice: user_settings.json не разбирается — авторазбор считается выключенным"
        );
        return Prefs::default();
    };
    Prefs {
        autotranscribe: flag(&json, keys.autotranscribe),
        filter_enabled: flag(&json, keys.filter_enabled),
        visible_kinds: list(&json, keys.visible_kinds),
        visible_peers: list(&json, keys.visible_peers),
        scope_kinds: list(&json, keys.scope_kinds),
        scope_peers: list(&json, keys.scope_peers),
    }
}

/// Булево поле; всё, что не `true`, — это `false` (как `=== true` во фронте).
fn flag(json: &serde_json::Value, key: &str) -> bool {
    json.get(key).and_then(serde_json::Value::as_bool) == Some(true)
}

/// Список строк без пустых и дублей, порядок сохраняется.
fn list(json: &serde_json::Value, key: &str) -> Vec<String> {
    let Some(arr) = json.get(key).and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::with_capacity(arr.len());
    for item in arr {
        let s = match item {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => continue,
        };
        if !s.is_empty() && !out.iter().any(|x| *x == s) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// **ФАЙЛА НЕТ — АВТОРАЗБОР ВЫКЛЮЧЕН.**
    ///
    /// Нулевая конфигурация обязана вести себя как до фичи.
    #[test]
    fn a_missing_file_means_the_feature_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let p = read(&dir.path().join(FILE_NAME), VK);
        assert_eq!(p, Prefs::default());
        assert!(!p.autotranscribe);
        assert!(!p.filter_active());
    }

    /// **БИТЫЙ JSON — ТОЖЕ ВЫКЛЮЧЕНО, А НЕ ПАНИКА.**
    #[test]
    fn a_broken_file_means_the_feature_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "{ это не json");
        assert!(!read(&path, VK).autotranscribe);
    }

    /// **СТАРЫЙ ФАЙЛ БЕЗ НОВЫХ ПОЛЕЙ ЧИТАЕТСЯ, И ОСТАЛЬНОЕ ИЗ НЕГО ВИДНО.**
    ///
    /// Незнакомые поля хоста (их там десятки) разбор не ломают — иначе каждое
    /// новое поле в `user_settings.json` молча выключало бы фичу.
    #[test]
    fn an_old_file_without_the_new_keys_is_read_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"todo_default_priority":2,"vk_filter_enabled":true,
                "vk_visible_kinds":["chat"],"vk_hotkeys":{"a":"b"}}"#,
        );
        let p = read(&path, VK);
        assert!(!p.autotranscribe);
        assert!(p.filter_enabled);
        assert_eq!(p.visible_kinds, vec!["chat"]);
    }

    /// **ТРИ ПАНЕЛИ ЧИТАЮТ ТРИ РАЗНЫХ ПОЛЯ ИЗ ОДНОГО ФАЙЛА.**
    #[test]
    fn each_panel_reads_only_its_own_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"vk_voice_autotranscribe":true,
                "telegram_voice_autotranscribe":false,
                "bitrix_voice_autotranscribe":true}"#,
        );
        assert!(read(&path, VK).autotranscribe);
        assert!(!read(&path, TELEGRAM).autotranscribe);
        assert!(read(&path, BITRIX).autotranscribe);
    }

    /// **ФЛАГ ЧИТАЕТСЯ СТРОГО КАК `true`.**
    ///
    /// Строка `"true"`, единица и `null` — это НЕ включено: фронт пишет булево,
    /// а всё прочее в этом поле означает правку файла руками с опечаткой.
    #[test]
    fn only_a_real_true_turns_the_feature_on() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            r#"{"vk_voice_autotranscribe":"true"}"#,
            r#"{"vk_voice_autotranscribe":1}"#,
            r#"{"vk_voice_autotranscribe":null}"#,
            "{}",
        ] {
            let path = write(dir.path(), body);
            assert!(!read(&path, VK).autotranscribe, "{body}");
        }
        let path = write(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        assert!(read(&path, VK).autotranscribe);
    }

    /// **НОВОЕ ЗНАЧЕНИЕ ВИДНО БЕЗ ПЕРЕЗАПУСКА.**
    ///
    /// Ровно то, что происходит при `PATCH /api/user-settings`: хост переписал
    /// файл, а плагин читает его на следующее голосовое. Кэша между этими двумя
    /// событиями нет и быть не должно — он и означал бы «перезапустите
    /// devforge, чтобы тумблер подействовал».
    #[test]
    fn a_rewritten_file_is_seen_by_the_next_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), r#"{"vk_voice_autotranscribe":false}"#);
        assert!(!read(&path, VK).autotranscribe);

        std::fs::write(&path, r#"{"vk_voice_autotranscribe":true}"#).unwrap();
        assert!(read(&path, VK).autotranscribe);

        std::fs::write(&path, r#"{"vk_voice_autotranscribe":false}"#).unwrap();
        assert!(!read(&path, VK).autotranscribe, "выключение тоже мгновенно");
    }

    /// **ВКЛЮЧЁННЫЙ ФИЛЬТР С ПУСТЫМ ВЫБОРОМ НЕ ДЕЙСТВУЕТ — КАК ВО ФРОНТЕ.**
    #[test]
    fn an_empty_whitelist_is_not_a_filter() {
        let p = Prefs {
            filter_enabled: true,
            ..Prefs::default()
        };
        assert!(!p.filter_active());
        assert!(p.peer_visible("2000000042", &["chat"]));
    }

    /// **ПОИМЁННЫЙ ВЫБОР СИЛЬНЕЕ ВИДА, НЕИЗВЕСТНАЯ БЕСЕДА СКРЫТА.**
    #[test]
    fn the_named_choice_wins_and_the_unknown_peer_is_hidden() {
        let p = Prefs {
            autotranscribe: true,
            filter_enabled: true,
            visible_kinds: vec!["chat".into()],
            visible_peers: vec!["777".into()],
            ..Prefs::default()
        };
        assert!(p.peer_visible("777", &["user"]), "отмечена поимённо");
        assert!(p.peer_visible("2000000042", &["chat"]), "подходит по виду");
        assert!(!p.peer_visible("42", &["user"]), "ни то, ни другое");
        assert!(!p.peer_visible("42", &[]), "вид неизвестен — беседа скрыта");
        assert!(!p.peer_visible("", &["chat"]), "беседа без адреса скрыта");
    }

    /// **ПУСТОЙ СКОУП РАЗБИРАЕТ ВЕЗДЕ — ГЛАВНОЕ ОБЕЩАНИЕ ФИЧИ.**
    ///
    /// Ноль настроек = поведение, которое было до появления полей. Провал
    /// этого теста означал бы, что обновление молча выключило авторазбор.
    #[test]
    fn an_empty_scope_transcribes_everywhere() {
        let p = Prefs {
            autotranscribe: true,
            ..Prefs::default()
        };
        assert!(!p.scope_active());
        for (peer, groups) in [
            ("2000000042", &["chat"][..]),
            ("777", &["user"][..]),
            ("chat7", &["task"][..]),
            ("-99", &[][..]),
            ("", &[][..]),
        ] {
            assert!(p.peer_in_scope(peer, groups), "{peer:?}/{groups:?}");
        }
    }

    /// **ПОИМЁННЫЙ ВЫБОР И ВЫБОР ГРУППЫ, НЕИЗВЕСТНАЯ БЕСЕДА — ВНЕ СКОУПА.**
    #[test]
    fn a_non_empty_scope_is_a_whitelist() {
        let p = Prefs {
            autotranscribe: true,
            scope_kinds: vec!["chat".into()],
            scope_peers: vec!["777".into()],
            ..Prefs::default()
        };
        assert!(p.scope_active());
        assert!(p.peer_in_scope("777", &["user"]), "отмечена поимённо");
        assert!(p.peer_in_scope("2000000042", &["chat"]), "подходит по группе");
        assert!(p.peer_in_scope("2000000099", &["chat"]), "вся группа целиком");
        assert!(!p.peer_in_scope("42", &["user"]), "ни то, ни другое");
        assert!(!p.peer_in_scope("42", &[]), "группа неизвестна");
        assert!(!p.peer_in_scope("", &["chat"]), "беседа без адреса");
    }

    /// **ЧАТ В НЕСКОЛЬКИХ ГРУППАХ: ХВАТАЕТ ОДНОЙ ОТМЕЧЕННОЙ.**
    ///
    /// Случай Telegram: чат лежит и в `inbox`, и в пользовательской папке.
    /// Требовать совпадения ВСЕХ групп значило бы, что отмеченная папка не
    /// работает ни для одного чата, который есть ещё где-то, — то есть ни для
    /// одного вообще.
    #[test]
    fn one_matching_group_out_of_several_is_enough() {
        let p = Prefs {
            autotranscribe: true,
            scope_kinds: vec!["folder:10".into()],
            ..Prefs::default()
        };
        assert!(p.peer_in_scope("777000", &["inbox", "folder:10"]));
        assert!(!p.peer_in_scope("777000", &["inbox", "folder:11"]));
        assert!(!p.peer_in_scope("777000", &[]), "групп нет — вне скоупа");

        let v = Prefs {
            filter_enabled: true,
            visible_kinds: vec!["archive".into()],
            ..Prefs::default()
        };
        assert!(v.peer_visible("777000", &["archive", "folder:3"]));
        assert!(!v.peer_visible("777000", &["inbox"]));
    }

    /// **ОДНОГО НЕПУСТОГО СПИСКА ХВАТАЕТ, ЧТОБЫ СКОУП ЗАДЕЙСТВОВАЛСЯ.**
    ///
    /// Выбрать только папку (не перечисляя чаты) — законный и, вероятно,
    /// самый частый способ пользоваться формой.
    #[test]
    fn either_list_alone_makes_the_scope_act() {
        let only_kinds = Prefs {
            scope_kinds: vec!["inbox".into()],
            ..Prefs::default()
        };
        assert!(only_kinds.peer_in_scope("1", &["inbox"]));
        assert!(!only_kinds.peer_in_scope("1", &["archive"]));

        let only_peers = Prefs {
            scope_peers: vec!["1".into()],
            ..Prefs::default()
        };
        assert!(only_peers.peer_in_scope("1", &["archive"]));
        assert!(!only_peers.peer_in_scope("2", &["archive"]));
    }

    /// **СКОУП ЧИТАЕТСЯ ИЗ СВОИХ ПОЛЕЙ И НЕ ПУТАЕТСЯ С ВИДИМОСТЬЮ.**
    #[test]
    fn the_scope_is_read_from_its_own_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"vk_visible_peers":["1"],"vk_voice_scope_peers":["2"],
                "vk_voice_scope_kinds":["chat"],
                "telegram_voice_scope_dialogs":["3"],
                "telegram_voice_scope_folders":["inbox"],
                "bitrix_voice_scope_peers":["chat7"],
                "bitrix_voice_scope_kinds":["task"]}"#,
        );
        let vk = read(&path, VK);
        assert_eq!(vk.visible_peers, vec!["1"]);
        assert_eq!(vk.scope_peers, vec!["2"]);
        assert_eq!(vk.scope_kinds, vec!["chat"]);
        assert!(vk.visible_kinds.is_empty(), "чужие списки не подмешаны");

        let tg = read(&path, TELEGRAM);
        assert_eq!(tg.scope_peers, vec!["3"], "у Telegram это *_dialogs");
        assert_eq!(tg.scope_kinds, vec!["inbox"], "у Telegram это ПАПКИ");
        assert!(tg.visible_peers.is_empty());

        let b24 = read(&path, BITRIX);
        assert_eq!(b24.scope_peers, vec!["chat7"]);
        assert_eq!(b24.scope_kinds, vec!["task"]);
    }

    /// **БИТЫЙ ФАЙЛ И ЧУЖИЕ ТИПЫ В ПОЛЯХ СКОУПА — ЭТО НЕ ПАНИКА.**
    ///
    /// Скоуп при отказе ПУСТ, но и тумблер при отказе выключен, поэтому
    /// «пусто = везде» не превращается в разбор всего подряд.
    #[test]
    fn a_broken_or_wrongly_typed_scope_is_empty_and_safe() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            "{ это не json",
            r#"{"vk_voice_scope_peers":"777"}"#,
            r#"{"vk_voice_scope_peers":{"a":1},"vk_voice_scope_kinds":42}"#,
            "{}",
        ] {
            let path = write(dir.path(), body);
            let p = read(&path, VK);
            assert!(p.scope_peers.is_empty(), "{body}");
            assert!(p.scope_kinds.is_empty(), "{body}");
            assert!(!p.autotranscribe, "{body}");
        }
    }

    /// **ИМЕНА КЛЮЧЕЙ СОВПАДАЮТ С ПОЛЯМИ ХОСТА БУКВАЛЬНО.**
    ///
    /// Таблица [`PrefsKeys`] — единственное место, где эти строки написаны на
    /// стороне плагина, и разойтись с хостом они могут только МОЛЧА: тумблер и
    /// скоуп просто перестанут действовать. Поэтому здесь читается сам исходник
    /// хоста. Если его рядом нет (крейт собирают отдельно от репозитория),
    /// проверка пропускается — ронять чужую сборку она не должна.
    #[test]
    fn every_key_exists_in_the_host_source() {
        let host = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tmux-web/src/user_settings.rs");
        let Ok(src) = std::fs::read_to_string(&host) else {
            return;
        };
        for keys in [VK, TELEGRAM, BITRIX] {
            for key in [
                keys.autotranscribe,
                keys.filter_enabled,
                keys.visible_kinds,
                keys.visible_peers,
                keys.scope_kinds,
                keys.scope_peers,
            ] {
                assert!(
                    src.contains(&format!("pub {key}:")),
                    "поля {key} нет в user_settings.rs хоста"
                );
            }
        }
    }

    /// Список чистится так же, как во фронте: без пустых, без дублей, числа
    /// принимаются (id беседы вполне может быть записан числом).
    #[test]
    fn the_lists_are_cleaned_like_the_frontend_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"vk_visible_peers":[" 777 ","777","",42,null,{"a":1},"chat7"]}"#,
        );
        assert_eq!(read(&path, VK).visible_peers, vec!["777", "42", "chat7"]);
    }
}
