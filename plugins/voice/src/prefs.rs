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
}

/// Ключи панели VK.
pub const VK: PrefsKeys = PrefsKeys {
    autotranscribe: "vk_voice_autotranscribe",
    filter_enabled: "vk_filter_enabled",
    visible_kinds: "vk_visible_kinds",
    visible_peers: "vk_visible_peers",
};

/// Ключи панели Telegram. «Виды» здесь — ПАПКИ аккаунта.
pub const TELEGRAM: PrefsKeys = PrefsKeys {
    autotranscribe: "telegram_voice_autotranscribe",
    filter_enabled: "telegram_filter_enabled",
    visible_kinds: "telegram_visible_folders",
    visible_peers: "telegram_visible_dialogs",
};

/// Ключи панели Битрикс24.
pub const BITRIX: PrefsKeys = PrefsKeys {
    autotranscribe: "bitrix_voice_autotranscribe",
    filter_enabled: "bitrix_filter_enabled",
    visible_kinds: "bitrix_visible_kinds",
    visible_peers: "bitrix_visible_peers",
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

    /// Видна ли беседа, у которой известны идентификатор и вид.
    ///
    /// Порядок проверок тот же, что во фронте: поимённый выбор сильнее вида.
    /// `kind` — уже приведённый плагином вид (`user`/`chat`/`community` у VK,
    /// `user`/`chat`/`task` у Bitrix); `None` — «вид неизвестен», и тогда
    /// решает только поимённый список.
    ///
    /// НЕИЗВЕСТНАЯ беседа при действующем фильтре считается НЕВИДИМОЙ. Это
    /// консервативная сторона: показать лишнее — мелкая неприятность, а
    /// разобрать голосовое из беседы, которую человек попросил не показывать, —
    /// это отправить её содержание в облако вопреки настройке.
    #[must_use]
    pub fn peer_visible(&self, peer: &str, kind: Option<&str>) -> bool {
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
        match kind {
            Some(k) => self.visible_kinds.iter().any(|v| v == k.trim()),
            None => false,
        }
    }
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
        assert!(p.peer_visible("2000000042", Some("chat")));
    }

    /// **ПОИМЁННЫЙ ВЫБОР СИЛЬНЕЕ ВИДА, НЕИЗВЕСТНАЯ БЕСЕДА СКРЫТА.**
    #[test]
    fn the_named_choice_wins_and_the_unknown_peer_is_hidden() {
        let p = Prefs {
            autotranscribe: true,
            filter_enabled: true,
            visible_kinds: vec!["chat".into()],
            visible_peers: vec!["777".into()],
        };
        assert!(p.peer_visible("777", Some("user")), "отмечена поимённо");
        assert!(p.peer_visible("2000000042", Some("chat")), "подходит по виду");
        assert!(!p.peer_visible("42", Some("user")), "ни то, ни другое");
        assert!(!p.peer_visible("42", None), "вид неизвестен — беседа скрыта");
        assert!(!p.peer_visible("", Some("chat")), "беседа без адреса скрыта");
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
