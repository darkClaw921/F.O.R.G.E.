//! UserSettings — глобальные настройки уровня пользователя.
//!
//! Хранилище: один файл `~/.forge/user_settings.json`. В отличие от
//! `projects.json` / `themes.json` (per-data_dir) и `todos.json`
//! (per-project), эти настройки **не привязаны** ни к проекту, ни к
//! каталогу — они применяются ко всем сессиям конкретного пользователя
//! на машине.
//!
//! ### Состав настроек
//!
//! Все поля помечены `#[serde(default)]`, чтобы старый/пустой/неполный файл
//! грузился без миграции.
//!
//! ### Инвариант «нулевая конфигурация» и его намеренное сужение
//!
//! Изначально (epic tw-z6l) действовал более сильный инвариант: при полном
//! отсутствии файла поведение системы побитово идентично состоянию «до фичи
//! user-settings». Для всех `todo_*` и `echo_*` полей он по-прежнему в силе.
//!
//! Поля `cmd_hints_enabled` и `next_step_enabled` его **намеренно нарушают**:
//! обе фичи раньше были жёстко всегда включены, а теперь при нулевой
//! конфигурации выключены. Это прямое требование пользователя — фичи заметные
//! (перехват удержания ⌘) и дорогие (`next_step` дёргает Claude CLI), поэтому
//! они opt-in. **Не «чините» эти дефолты обратно на `true`** — это не забытая
//! default-функция, а осознанное решение.
//!
//! Поля:
//!   - `todo_default_plan_mode` (bool, default false) — значение
//!     plan-mode-чекбокса по умолчанию при создании TODO.
//!   - `todo_default_priority` (u8, default 2, clamp 0..=4) — приоритет
//!     новой TODO по умолчанию.
//!   - `todo_default_issue_type` (String, default `"task"`) — тип
//!     issue по умолчанию для новой TODO.
//!   - `todo_plan_mode_suffix` (String, default `""`) — текст, который
//!     присоединяется к notify-сообщению при promote TODO с plan_mode=true.
//!     Пустая строка → используется константа `PLAN_MODE_SUFFIX` из `main.rs`.
//!   - `todo_confirm_delete` (bool, default true) — спрашивать ли
//!     подтверждение при удалении TODO (UI-флаг, backend-side просто хранит).
//!   - `todo_confirm_promote_on_drag` (bool, default false) — спрашивать
//!     ли подтверждение при promote через drag-and-drop.
//!   - `echo_default_model` (Option<String>, default None) — модель Claude
//!     по умолчанию для новых Echo-conversation'ов. `None` → fallback на
//!     `EchoConfig.default_model` в плагине.
//!   - `echo_notifications_enabled` (bool, default true) — показывать ли
//!     toast-нотификации от Echo (autonomous task events / action results).
//!     Пользователь может отключить, если шум мешает.
//!   - `cmd_hints_enabled` (bool, default **false**) — включены ли подсказки
//!     хоткеев при удержании ⌘ (`static/hotkeys.js`: бейджи с буквенными
//!     кодами на кликабельных элементах). Читается фронтендом лениво через
//!     `window.ForgeApp.state.userSettings`; backend флаг только хранит.
//!   - `next_step_enabled` (bool, default **false**) — включена ли фича
//!     «Следующий шаг» целиком: голубое свечение сессий, hover-попап и
//!     **генерация подсказок воркером Echo через Claude CLI**. Backend читает
//!     флаг в `echo_host::EchoHostAdapter::next_step_enabled` (см. trait
//!     `echo_host_api::HostApi`) — при `false` воркер не дёргает Claude CLI.
//!   - `telegram_hotkeys_enabled` (bool, default **true**) — включена ли
//!     клавиатурная навигация во вкладке Telegram (модули
//!     `static/js/telegram/keys.js` и `keynav.js`). Дефолт `true` — в отличие
//!     от двух полей выше это НЕ
//!     нарушение инварианта нулевой конфигурации: сама вкладка Telegram уже
//!     opt-in (флаг `--telegram`), а хоткеи живут только внутри неё и молчат в
//!     полях ввода. Пользователь просил именно «включить и отключить», то есть
//!     тумблер для выключения работающей фичи.
//!   - `telegram_filter_enabled` (bool, default **false**) — включён ли режим
//!     «показывать только выбранное» во вкладке Telegram. Фича opt-in:
//!     нулевая конфигурация обязана показывать все чаты, как и раньше.
//!   - `telegram_visible_folders` (Vec<String>, default пустой) — ключи папок,
//!     чьи чаты остаются видимыми: `inbox`, `archive`, `folder:<id>`.
//!   - `telegram_visible_dialogs` (Vec<String>, default пустой) — id отдельных
//!     чатов, видимых независимо от папок. Оба списка — БЕЛЫЙ список, и
//!     фильтр считается действующим, только когда хотя бы один непуст:
//!     включённый тумблер с пустым выбором показывал бы пустую панель, и
//!     пользователь читал бы это как поломку, а не как свою настройку.
//!   - `telegram_hotkeys` (map String→String, default пустая) —
//!     переопределения комбинаций: `id действия` → `комбинация`. Пустая мапа
//!     означает «все действия на дефолтных клавишах». Backend хранит и
//!     валидирует только ФОРМУ (см. [`sanitize_hotkeys`]) — смысл id действий
//!     и комбинаций знает исключительно фронтенд, иначе список действий
//!     пришлось бы держать синхронным в двух местах.
//!   - `vk_filter_enabled` (bool, default **false**) — включён ли режим
//!     «показывать только выбранные беседы» во вкладке VK. Дефолт `false` по
//!     той же причине, что и у `telegram_filter_enabled`: нулевая конфигурация
//!     обязана показывать все беседы. Фронтенд читает флаг строго как
//!     `=== true` (`static/js/vk/visibility.js`), а не `!== false`.
//!   - `vk_visible_kinds` (Vec<String>, default пустой) — белый список ТИПОВ
//!     бесед: `user` | `chat` | `community`. Словарь намеренно расходится с
//!     телеграмным (там ключи папок): папок у VK нет, а четыре серверных
//!     фильтра `all/unread/important/unanswered` — это параметр запроса, и
//!     «показывать только непрочитанные» как ПОСТОЯННАЯ настройка бессмысленно.
//!     Тип беседы при этом выводится из самого `peer_id`, поэтому предикат
//!     видимости точен даже для беседы вне загруженной страницы списка.
//!   - `vk_hotkeys_enabled` (bool, default **true**) — включена ли
//!     клавиатурная навигация во вкладке VK (модули `static/js/vk/keys.js` и
//!     `keynav.js`). Дефолт `true` по той же причине, что и у
//!     `telegram_hotkeys_enabled`: вкладка VK уже opt-in (флаг `--vk`), хоткеи
//!     живут только внутри неё и молчат в полях ввода, поэтому тумблер нужен
//!     для ВЫКЛЮЧЕНИЯ работающей фичи. Фронтенд читает его строго как
//!     `!== false` (`static/js/vk/keynav.js::hotkeysEnabled`) — путать с
//!     `vk_filter_enabled`, который читается `=== true`, нельзя: ошибка в эту
//!     сторону молча выключает фичу у всех, кто ни разу не открывал настройки.
//!   - `vk_hotkeys` (map String→String, default пустая) — переопределения
//!     комбинаций VK: `id действия` → `комбинация`. Как и у Telegram, backend
//!     хранит и валидирует только ФОРМУ (см. [`sanitize_hotkeys`]): реестр
//!     действий живёт исключительно во фронтенде (`static/js/vk/keys.js`), и
//!     дублировать его здесь значило бы завести вторую точку правды.
//!   - `vk_visible_peers` (Vec<String>, default пустой) — `peer_id` отдельных
//!     бесед, видимых независимо от типа. Как и у Telegram, оба списка —
//!     БЕЛЫЙ список, и фильтр считается ДЕЙСТВУЮЩИМ, только когда хотя бы один
//!     из них непуст: включённый тумблер с пустым выбором показал бы пустую
//!     панель, и пользователь прочитал бы это как поломку, а не как настройку.
//!   - `bitrix_hotkeys_enabled` (bool, default **true**) — включена ли
//!     клавиатурная навигация во вкладке «Битрикс24» (модули
//!     `static/js/bitrix/keys.js` и `keynav.js`). Дефолт `true` по той же
//!     причине, что у двух соседних панелей: вкладка уже opt-in (флаг
//!     `--bitrix`), хоткеи живут только внутри неё и молчат в полях ввода.
//!     Фронтенд читает флаг строго как `!== false`
//!     (`static/js/bitrix/keynav.js::hotkeysEnabled`).
//!   - `bitrix_hotkeys` (map String→String, default пустая) — переопределения
//!     комбинаций Битрикс24. Как и у соседей, backend хранит и валидирует
//!     только ФОРМУ (см. [`sanitize_hotkeys`]): реестр действий живёт
//!     исключительно во фронтенде (`static/js/bitrix/keys.js`).
//!   - `bitrix_filter_enabled` (bool, default **false**) — включён ли режим
//!     «показывать только выбранные диалоги» во вкладке «Битрикс24». Фронтенд
//!     читает флаг строго как `=== true`
//!     (`static/js/bitrix/visibility.js::normalizeFilter`).
//!
//!     АСИММЕТРИЯ ДЕФОЛТОВ НАМЕРЕННА и повторяет обе соседние панели:
//!     `*_hotkeys_enabled` по умолчанию ВКЛЮЧЕНО и читается `!== false`,
//!     `*_filter_enabled` по умолчанию ВЫКЛЮЧЕНО и читается `=== true`.
//!     Перепутать гейты нельзя: ошибка в одну сторону молча выключает хоткеи
//!     у всех, кто ни разу не открывал настройки, а в другую — прячет чаты у
//!     тех, кто белый список никогда не собирал.
//!   - `bitrix_visible_kinds` (Vec<String>, default пустой) — белый список
//!     ВИДОВ диалогов: `user` | `chat` | `task`. Словарь снова свой: папок и
//!     серверных фильтров у портала нет, а семь видов диалога (`sg`,
//!     `openline`, `copilot`, `collab` и прочие) сведены фронтендом к трём —
//!     раскладывать настройку по редким видам значило бы требовать знания
//!     внутренней терминологии портала. Отдельного ключа под чаты задач НЕТ:
//!     `task` — такой же вид в этом же списке, и второй ключ разошёлся бы с
//!     ним при первом же изменении.
//!   - `bitrix_visible_peers` (Vec<String>, default пустой) — `DIALOG_ID`
//!     отдельных диалогов (`"chat7"`, `"19"`, `"sg12"`), видимых независимо от
//!     вида. Как и у соседей, оба списка — БЕЛЫЙ список, и фильтр считается
//!     ДЕЙСТВУЮЩИМ, только когда хотя бы один из них непуст.
//!
//!   - `vk_voice_autotranscribe`, `telegram_voice_autotranscribe`,
//!     `bitrix_voice_autotranscribe` (bool, default **false**) — разбирать ли
//!     НОВЫЕ входящие голосовые сообщения автоматически, каждый в своей
//!     панели. Три отдельных поля, а не одно общее: панели включаются разными
//!     флагами запуска и живут у разных людей по-разному, и «включил в VK —
//!     заработало в рабочем портале» было бы неожиданностью, а не удобством.
//!
//!     Дефолт `false` читается строго как `=== true` (как у `*_filter_enabled`,
//!     а не как у `*_hotkeys_enabled`): фича тратит GPU либо деньги в облаке,
//!     и молчаливо включаться она не имеет права. При выключенном флаге
//!     плагин не делает НИ докачки вложения, НИ запуска движка — это гейт
//!     фазы, а не пожелание.
//!
//!     Настройки не читаются плагинами через `AppState` (его у них нет вовсе):
//!     плагин читает ЭТОТ ЖЕ файл сам, модулем `forge_voice::prefs`, на каждое
//!     входящее голосовое. Отсюда и «применяется без перезапуска».
//!
//!     Выбор ДВИЖКА расшифровки (`local` / `cloud`) в этом файле НЕ живёт: он
//!     общий для всех мессенджеров и лежит в `~/.config/forge/voice.toml`
//!     рядом с ключом polza.ai (роут `…/voice/settings` у каждого плагина).
//!
//! Токена VK в этом файле нет и быть не должно: он живёт в конфиге плагина
//! (`~/.config/forge/vk.toml`, права 0600), а `user_settings.json` читается
//! фронтендом целиком через `GET /api/user-settings` и секретов не хранит.
//! То же и строже — про Битрикс24: там секрет это ВЕСЬ URL входящего вебхука
//! (`https://<портал>/rest/<user_id>/<code>/`), он живёт только в
//! `~/.config/forge/bitrix.toml` с правами 0600 и в этот файл не попадает
//! ни целиком, ни фрагментом.
//!
//! ### Persistence
//!
//! Atomic save: пишем в `<path>.tmp`, затем `fs::rename` поверх. На POSIX
//! rename атомарен в рамках одного mount-point — даже при `kill -9` в
//! момент записи получим либо старый, либо новый файл, но не битый.
//! Стратегия идентична [`crate::todos::save_locked`].
//!
//! ### Lazy file creation
//!
//! `UserSettingsStore::new(path)` НЕ создаёт файл, если его нет — просто
//! возвращает store с `UserSettings::default()`. Файл появится только при
//! первом успешном [`UserSettingsStore::patch`]. Это сохраняет
//! «нулевую конфигурацию» как состояние по умолчанию.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default-функция для `todo_default_priority` — medium (P2).
fn default_priority() -> u8 {
    2
}

/// Default-функция для `todo_default_issue_type` — `"task"`.
fn default_issue_type() -> String {
    "task".to_string()
}

/// Default-функция для `todo_confirm_delete` — true.
fn default_confirm_delete() -> bool {
    true
}

/// Default-функция для `echo_notifications_enabled` — true (нотификации
/// видны по умолчанию; пользователь сам решает приглушать ли их).
fn default_echo_notifications_enabled() -> bool {
    true
}

/// Default-функция для `telegram_hotkeys_enabled` — true.
///
/// Осознанно отличается от `cmd_hints_enabled`/`next_step_enabled`: те две
/// фичи глобальные и заметные, а эта живёт внутри уже opt-in вкладки Telegram.
fn default_telegram_hotkeys_enabled() -> bool {
    true
}

/// Default-функция для `vk_hotkeys_enabled` — true.
///
/// Отдельная функция, а не переиспользование телеграмной: одинаковое значение
/// сегодня не означает связанности завтра, и `#[serde(default = "...")]` — то
/// место, где общая функция сделала бы изменение одного дефолта незаметной
/// сменой второго.
fn default_vk_hotkeys_enabled() -> bool {
    true
}

/// Default-функция для `bitrix_hotkeys_enabled` — true.
///
/// Третья одинаковая функция подряд — и это сознательно, по той же причине,
/// что описана у `default_vk_hotkeys_enabled`: `#[serde(default = "...")]`
/// связывает поле с ИМЕНЕМ функции, и общая на три панели функция превратила
/// бы правку дефолта одной вкладки в молчаливую правку дефолта двух других.
fn default_bitrix_hotkeys_enabled() -> bool {
    true
}

/// Default-функция для `auto_approve_questions` — true.
///
/// Отдельная функция по тем же соображениям, что и у хоткеев: одинаковое
/// значение сегодня не означает связанности завтра. Флаг действует только
/// внутри включённого `auto_approve_enabled` (сам он по умолчанию `false`),
/// поэтому `true` здесь не меняет поведение «из коробки».
fn default_auto_approve_questions() -> bool {
    true
}

/// Максимально допустимое значение приоритета (4 = backlog).
const MAX_PRIORITY: u8 = 4;

/// Максимальное число пользовательских стоп-слов авто-апрува.
const MAX_BLOCKLIST_ENTRIES: usize = 128;

/// Максимальная длина одного стоп-слова авто-апрува.
const MAX_BLOCKLIST_ENTRY_LEN: usize = 200;

/// Приводит список стоп-слов авто-апрува к безопасному виду.
///
/// Отдельный санитайзер, а не [`sanitize_visible_list`]: там записи — короткие
/// ASCII-id без пробелов, а стоп-слово — фрагмент командной строки
/// (`rm -rf`, `git push --force`, `| sh`), в котором пробелы и спецсимволы
/// как раз и несут смысл. Поэтому набор символов не ограничивается вовсе.
///
/// Отбрасываются пустые после trim записи и слишком длинные; всё приводится
/// к нижнему регистру (сравнение в [`crate::auto_approve`] регистронезависимое
/// и ожидает уже нормализованный список); дубликаты схлопываются с сохранением
/// порядка — порядок виден пользователю в textarea настроек.
fn sanitize_blocklist(raw: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter_map(|item| {
            let item = item.trim().to_lowercase();
            if item.is_empty() || item.len() > MAX_BLOCKLIST_ENTRY_LEN {
                return None;
            }
            if !seen.insert(item.clone()) {
                return None;
            }
            Some(item)
        })
        .take(MAX_BLOCKLIST_ENTRIES)
        .collect()
}

/// Максимальное число записей в белом списке видимости Telegram.
///
/// Ограничение того же рода, что и у хоткеев: список приезжает из браузера и
/// сохраняется в файл. Полтысячи чатов — заведомо больше, чем можно осмысленно
/// отобрать руками, но достаточно, чтобы никого не обрезать.
///
/// Константа ОБЩАЯ для обеих панелей — её же использует белый список VK
/// (`vk_visible_kinds` / `vk_visible_peers`). Имя оставлено историческим
/// намеренно: переименование в нейтральное `MAX_VISIBLE_ENTRIES` тронуло бы
/// работающий код Telegram, а правило «не трогать чужое» здесь строже
/// косметики. Потолок один на список, а не на пользователя: списков четыре
/// (две панели × два списка), и раздуть файл ни один из них в одиночку не может.
const MAX_TELEGRAM_VISIBLE: usize = 512;

/// Максимальная длина одной записи белого списка (id чата / ключ папки /
/// `peer_id` беседы VK / имя типа `user`|`chat`|`community`).
///
/// Тоже общая для обеих панелей: форма записей у них совпадает — короткая
/// ASCII-строка без пробелов.
const MAX_VISIBLE_ENTRY_LEN: usize = 64;

/// Приводит белый список видимости к безопасному виду.
///
/// Используется ОБЕИМИ панелями: `telegram_visible_folders`/
/// `telegram_visible_dialogs` и `vk_visible_kinds`/`vk_visible_peers`. Смысл
/// записей у них разный, форма — одинаковая, поэтому санитайзер один.
///
/// Как и [`sanitize_hotkeys`], проверяет только ФОРМУ: смысл ключей (`inbox`,
/// `archive`, `folder:<id>`, id чата) знает фронтенд. Неизвестная запись — не
/// ошибка: она просто ни с чем не совпадёт, а сохранённой останется, что
/// позволяет пережить откат версии клиента.
///
/// Отбрасываются пустые после trim записи, слишком длинные и содержащие
/// что-либо кроме `[0-9a-zA-Z_:.-]` (id чатов бывают отрицательными, ключ
/// папки содержит двоеточие). Дубликаты схлопываются с сохранением порядка —
/// порядок виден пользователю в списке настроек.
fn sanitize_visible_list(raw: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() || item.len() > MAX_VISIBLE_ENTRY_LEN {
                return None;
            }
            if !item
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
            {
                return None;
            }
            if !seen.insert(item.to_string()) {
                return None;
            }
            Some(item.to_string())
        })
        .take(MAX_TELEGRAM_VISIBLE)
        .collect()
}

/// Максимальное число переопределений хоткеев Telegram.
///
/// Действий в реестре фронтенда меньше двадцати; запас взят на рост. Лимит
/// нужен не «для красоты»: мапа приезжает из браузера и сохраняется в файл, а
/// без потолка одна кривая (или враждебная) отправка раздула бы
/// `user_settings.json` неограниченно.
const MAX_TELEGRAM_HOTKEYS: usize = 64;

/// Максимальная длина id действия и записи комбинации.
const MAX_HOTKEY_LEN: usize = 64;

/// Приводит карту переопределений хоткеев к безопасному виду.
///
/// Проверяется только ФОРМА, не смысл: backend намеренно не знает ни списка
/// действий, ни синтаксиса комбинаций (это реестр `static/js/telegram/keys.js`).
/// Неизвестный id действия — не ошибка: он просто останется лежать и будет
/// проигнорирован фронтендом, что позволяет откатывать версии клиента без
/// потери настроек.
///
/// Отбрасываются: пустые (после trim) ключи и значения, ключи с символами вне
/// `[a-z0-9_.-]` (id действий генерирует наш же код — регистр и пробелы там
/// невозможны), слишком длинные ключи/значения, управляющие символы в
/// комбинации. Лишние записи сверх [`MAX_TELEGRAM_HOTKEYS`] отсекаются — так
/// как `BTreeMap` упорядочен, отсечение детерминировано.
fn sanitize_hotkeys(raw: BTreeMap<String, String>) -> BTreeMap<String, String> {
    raw.into_iter()
        .filter_map(|(action, combo)| {
            let action = action.trim();
            let combo = combo.trim();
            if action.is_empty() || combo.is_empty() {
                return None;
            }
            if action.len() > MAX_HOTKEY_LEN || combo.len() > MAX_HOTKEY_LEN {
                return None;
            }
            if !action
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
            {
                return None;
            }
            if combo.chars().any(|c| c.is_control()) {
                return None;
            }
            Some((action.to_string(), combo.to_string()))
        })
        .take(MAX_TELEGRAM_HOTKEYS)
        .collect()
}

/// Настройки уровня пользователя.
///
/// Сериализуется в `~/.forge/user_settings.json`. Все поля
/// `#[serde(default)]` — чтобы частичный / пустой файл считался
/// валидным состоянием (пропущенные поля берутся из [`Default`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserSettings {
    #[serde(default)]
    pub todo_default_plan_mode: bool,
    #[serde(default = "default_priority")]
    pub todo_default_priority: u8,
    #[serde(default = "default_issue_type")]
    pub todo_default_issue_type: String,
    #[serde(default)]
    pub todo_plan_mode_suffix: String,
    #[serde(default = "default_confirm_delete")]
    pub todo_confirm_delete: bool,
    #[serde(default)]
    pub todo_confirm_promote_on_drag: bool,
    /// Phase 6 (Echo) — модель Claude по умолчанию для новых
    /// Echo-conversation'ов. `None` (или пропущенное поле) → fallback на
    /// `EchoConfig.default_model` в плагине.
    #[serde(default)]
    pub echo_default_model: Option<String>,
    /// Phase 6 (Echo) — показывать ли toast-нотификации от Echo. По
    /// умолчанию включено. Пользователь может приглушить через UI.
    #[serde(default = "default_echo_notifications_enabled")]
    pub echo_notifications_enabled: bool,
    /// Подсказки хоткеев при удержании ⌘ (`static/hotkeys.js`). Default
    /// `false` — фича opt-in, см. раздел про инвариант в шапке модуля.
    #[serde(default)]
    pub cmd_hints_enabled: bool,
    /// Фича «Следующий шаг» целиком (свечение + попап + генерация подсказок
    /// воркером Echo). Default `false` — фича opt-in и дорогая (Claude CLI),
    /// см. раздел про инвариант в шапке модуля.
    #[serde(default)]
    pub next_step_enabled: bool,
    /// Клавиатурная навигация во вкладке Telegram. Default `true` — см. шапку
    /// модуля: фича живёт внутри уже opt-in вкладки, тумблер нужен для
    /// выключения, а не для включения.
    #[serde(default = "default_telegram_hotkeys_enabled")]
    pub telegram_hotkeys_enabled: bool,
    /// Переопределения комбинаций Telegram: `id действия` → `комбинация`.
    /// Пустая мапа = дефолтные клавиши из реестра фронтенда.
    #[serde(default)]
    pub telegram_hotkeys: BTreeMap<String, String>,
    /// Режим «показывать только выбранные чаты и папки». Default `false` —
    /// фича opt-in, нулевая конфигурация показывает всё.
    #[serde(default)]
    pub telegram_filter_enabled: bool,
    /// Белый список папок: `inbox`, `archive`, `folder:<id>`.
    #[serde(default)]
    pub telegram_visible_folders: Vec<String>,
    /// Белый список отдельных чатов (id в формате `DialogDto.id`).
    #[serde(default)]
    pub telegram_visible_dialogs: Vec<String>,
    /// Клавиатурная навигация во вкладке VK. Default `true` — см. шапку
    /// модуля: фича живёт внутри уже opt-in вкладки, тумблер нужен для
    /// выключения, а не для включения.
    #[serde(default = "default_vk_hotkeys_enabled")]
    pub vk_hotkeys_enabled: bool,
    /// Переопределения комбинаций VK: `id действия` → `комбинация`.
    /// Пустая мапа = дефолтные клавиши из реестра фронтенда.
    #[serde(default)]
    pub vk_hotkeys: BTreeMap<String, String>,
    /// Режим «показывать только выбранные беседы» во вкладке VK. Default
    /// `false` — фича opt-in, нулевая конфигурация показывает все беседы.
    #[serde(default)]
    pub vk_filter_enabled: bool,
    /// Белый список ТИПОВ бесед: `user` | `chat` | `community`.
    ///
    /// Не ключи серверных фильтров VK (`all`/`unread`/`important`/
    /// `unanswered`): те — параметр запроса, а не свойство беседы, и
    /// «показывать только непрочитанные» как постоянная настройка означало бы
    /// исчезновение беседы из панели в момент её прочтения.
    #[serde(default)]
    pub vk_visible_kinds: Vec<String>,
    /// Белый список отдельных бесед: `peer_id` строками (в формате
    /// `DialogDto.id` плагина VK).
    #[serde(default)]
    pub vk_visible_peers: Vec<String>,
    /// Клавиатурная навигация во вкладке «Битрикс24». Default `true` — см.
    /// шапку модуля: фича живёт внутри уже opt-in вкладки, тумблер нужен для
    /// выключения, а не для включения. Фронт читает `!== false`.
    #[serde(default = "default_bitrix_hotkeys_enabled")]
    pub bitrix_hotkeys_enabled: bool,
    /// Переопределения комбинаций Битрикс24: `id действия` → `комбинация`.
    /// Пустая мапа = дефолтные клавиши из реестра фронтенда.
    #[serde(default)]
    pub bitrix_hotkeys: BTreeMap<String, String>,
    /// Режим «показывать только выбранные диалоги» во вкладке «Битрикс24».
    /// Default `false` — фича opt-in, нулевая конфигурация показывает все
    /// диалоги. Фронт читает `=== true`.
    #[serde(default)]
    pub bitrix_filter_enabled: bool,
    /// Белый список ВИДОВ диалогов: `user` | `chat` | `task`.
    ///
    /// Не директории колонки (`all`/`chats`/`tasks`/`unread`): те — локальная
    /// нарезка ОДНОГО списка `im.recent.get`, а не свойство диалога, и
    /// «показывать только непрочитанные» как постоянная настройка означало бы
    /// исчезновение чата в момент его прочтения.
    #[serde(default)]
    pub bitrix_visible_kinds: Vec<String>,
    /// Белый список отдельных диалогов: `DIALOG_ID` строками (`"chat7"`,
    /// `"19"`, `"sg12"` — в формате `DialogDto.id` плагина Битрикс24).
    #[serde(default)]
    pub bitrix_visible_peers: Vec<String>,
    /// Разбирать ли новые входящие голосовые VK автоматически. Default
    /// `false` — фича opt-in и тратит GPU либо деньги в облаке (см. шапку
    /// модуля). Читается не только фронтом: плагин VK открывает ЭТОТ ЖЕ файл
    /// модулем `forge_voice::prefs` на каждое входящее голосовое.
    #[serde(default)]
    pub vk_voice_autotranscribe: bool,
    /// То же для Telegram (`telegram_voice_autotranscribe`).
    #[serde(default)]
    pub telegram_voice_autotranscribe: bool,
    /// То же для Битрикс24 (`bitrix_voice_autotranscribe`).
    #[serde(default)]
    pub bitrix_voice_autotranscribe: bool,
    /// Авто-апрув интерактивных prompt'ов Claude Code в tmux-сессиях
    /// (см. [`crate::auto_approve`]). Default `false` — фича opt-in и
    /// нажимает клавиши за пользователя, см. инвариант в шапке модуля.
    /// При выключенном флаге воркер не делает ни одного tmux-вызова.
    #[serde(default)]
    pub auto_approve_enabled: bool,
    /// Расширенный апрув: выбирать «Yes, and don't ask again» / «Allow all
    /// actions for this session», когда такой пункт есть. Default `false` —
    /// разовое подтверждение. Действует только внутри включённого
    /// `auto_approve_enabled` и только там, где второй пункт действительно
    /// расширяющий (в двухопционных Yes/No и Allow/Deny второй пункт — отказ).
    #[serde(default)]
    pub auto_approve_broad: bool,
    /// Отвечать ли автоматически на вопросы `AskUserQuestion` (первый
    /// вариант). Default `true`: тумблер нужен, чтобы выключить, а сам
    /// авто-апрув уже opt-in. Prompt утверждения плана (`ExitPlanMode`)
    /// не подтверждается автоматически НИКОГДА и флагом не управляется.
    #[serde(default = "default_auto_approve_questions")]
    pub auto_approve_questions: bool,
    /// Пользовательские стоп-слова: если подстрока встречается в панели с
    /// prompt'ом, авто-апрув пропускает его (остаётся человеку). Дополняет
    /// встроенный список `auto_approve::DEFAULT_DANGEROUS`, а не заменяет его.
    #[serde(default)]
    pub auto_approve_blocklist: Vec<String>,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            todo_default_plan_mode: false,
            todo_default_priority: default_priority(),
            todo_default_issue_type: default_issue_type(),
            todo_plan_mode_suffix: String::new(),
            todo_confirm_delete: default_confirm_delete(),
            todo_confirm_promote_on_drag: false,
            echo_default_model: None,
            echo_notifications_enabled: default_echo_notifications_enabled(),
            cmd_hints_enabled: false,
            next_step_enabled: false,
            telegram_hotkeys_enabled: default_telegram_hotkeys_enabled(),
            telegram_hotkeys: BTreeMap::new(),
            telegram_filter_enabled: false,
            telegram_visible_folders: Vec::new(),
            telegram_visible_dialogs: Vec::new(),
            vk_hotkeys_enabled: default_vk_hotkeys_enabled(),
            vk_hotkeys: BTreeMap::new(),
            vk_filter_enabled: false,
            vk_visible_kinds: Vec::new(),
            vk_visible_peers: Vec::new(),
            bitrix_hotkeys_enabled: default_bitrix_hotkeys_enabled(),
            bitrix_hotkeys: BTreeMap::new(),
            bitrix_filter_enabled: false,
            bitrix_visible_kinds: Vec::new(),
            bitrix_visible_peers: Vec::new(),
            vk_voice_autotranscribe: false,
            telegram_voice_autotranscribe: false,
            bitrix_voice_autotranscribe: false,
            auto_approve_enabled: false,
            auto_approve_broad: false,
            auto_approve_questions: default_auto_approve_questions(),
            auto_approve_blocklist: Vec::new(),
        }
    }
}

/// DTO для `PATCH /api/user-settings`. Все поля `Option<T>` — применяются
/// только `Some(..)`-варианты. Это позволяет клиенту менять одно поле, не
/// высылая полный объект.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PatchUserSettingsReq {
    #[serde(default)]
    pub todo_default_plan_mode: Option<bool>,
    #[serde(default)]
    pub todo_default_priority: Option<u8>,
    #[serde(default)]
    pub todo_default_issue_type: Option<String>,
    #[serde(default)]
    pub todo_plan_mode_suffix: Option<String>,
    #[serde(default)]
    pub todo_confirm_delete: Option<bool>,
    #[serde(default)]
    pub todo_confirm_promote_on_drag: Option<bool>,
    /// Phase 6 (Echo) — модель Claude по умолчанию для новых
    /// Echo-conversation'ов. Sentinel: пустая строка / `null` → сбросить
    /// в `None` (fallback на дефолт плагина).
    #[serde(default)]
    pub echo_default_model: Option<String>,
    /// Phase 6 (Echo) — включить/выключить toast-нотификации Echo.
    #[serde(default)]
    pub echo_notifications_enabled: Option<bool>,
    /// Включить/выключить подсказки хоткеев при удержании ⌘.
    #[serde(default)]
    pub cmd_hints_enabled: Option<bool>,
    /// Включить/выключить фичу «Следующий шаг» целиком (свечение + попап +
    /// генерация подсказок воркером Echo).
    #[serde(default)]
    pub next_step_enabled: Option<bool>,
    /// Включить/выключить клавиатурную навигацию во вкладке Telegram.
    #[serde(default)]
    pub telegram_hotkeys_enabled: Option<bool>,
    /// Переопределения комбинаций Telegram. Семантика — ПОЛНАЯ ЗАМЕНА карты,
    /// а не слияние: только так UI может снять одно переопределение (прислать
    /// карту без него). Пустая карта = сброс всех действий к дефолтам.
    #[serde(default)]
    pub telegram_hotkeys: Option<BTreeMap<String, String>>,
    /// Включить/выключить режим «показывать только выбранное».
    #[serde(default)]
    pub telegram_filter_enabled: Option<bool>,
    /// Белый список папок. Семантика — ПОЛНАЯ ЗАМЕНА списка (как у
    /// `telegram_hotkeys`): снять галочку иначе нечем.
    #[serde(default)]
    pub telegram_visible_folders: Option<Vec<String>>,
    /// Белый список чатов. Семантика — ПОЛНАЯ ЗАМЕНА списка.
    #[serde(default)]
    pub telegram_visible_dialogs: Option<Vec<String>>,
    /// Включить/выключить клавиатурную навигацию во вкладке VK.
    #[serde(default)]
    pub vk_hotkeys_enabled: Option<bool>,
    /// Переопределения комбинаций VK. Семантика — ПОЛНАЯ ЗАМЕНА карты, а не
    /// слияние: только так UI может снять одно переопределение (прислать карту
    /// без него). Пустая карта = сброс всех действий к дефолтам.
    #[serde(default)]
    pub vk_hotkeys: Option<BTreeMap<String, String>>,
    /// Включить/выключить режим «показывать только выбранные беседы» во
    /// вкладке VK.
    #[serde(default)]
    pub vk_filter_enabled: Option<bool>,
    /// Белый список типов бесед VK. Семантика — ПОЛНАЯ ЗАМЕНА списка, как и у
    /// телеграмных: слияние сделало бы снятие галочки невозможным — прислать
    /// «минус одну запись» в merge-семантике нечем. Пустой список = «ничего не
    /// выбрано», и тогда включённый тумблер показывает ВСЁ (см. шапку модуля).
    #[serde(default)]
    pub vk_visible_kinds: Option<Vec<String>>,
    /// Белый список отдельных бесед VK. Семантика — ПОЛНАЯ ЗАМЕНА списка.
    #[serde(default)]
    pub vk_visible_peers: Option<Vec<String>>,
    /// Включить/выключить клавиатурную навигацию во вкладке «Битрикс24».
    #[serde(default)]
    pub bitrix_hotkeys_enabled: Option<bool>,
    /// Переопределения комбинаций Битрикс24. Семантика — ПОЛНАЯ ЗАМЕНА карты,
    /// а не слияние: только так UI может снять одно переопределение (прислать
    /// карту без него). Пустая карта = сброс всех действий к дефолтам.
    #[serde(default)]
    pub bitrix_hotkeys: Option<BTreeMap<String, String>>,
    /// Включить/выключить режим «показывать только выбранные диалоги» во
    /// вкладке «Битрикс24».
    #[serde(default)]
    pub bitrix_filter_enabled: Option<bool>,
    /// Белый список видов диалогов Битрикс24. Семантика — ПОЛНАЯ ЗАМЕНА
    /// списка, как и у соседних панелей. Пустой список = «ничего не выбрано»,
    /// и тогда включённый тумблер показывает ВСЁ (см. шапку модуля).
    #[serde(default)]
    pub bitrix_visible_kinds: Option<Vec<String>>,
    /// Белый список отдельных диалогов Битрикс24. Семантика — ПОЛНАЯ ЗАМЕНА
    /// списка.
    #[serde(default)]
    pub bitrix_visible_peers: Option<Vec<String>>,
    /// Включить/выключить авторазбор новых голосовых VK.
    #[serde(default)]
    pub vk_voice_autotranscribe: Option<bool>,
    /// То же для Telegram.
    #[serde(default)]
    pub telegram_voice_autotranscribe: Option<bool>,
    /// То же для Битрикс24.
    #[serde(default)]
    pub bitrix_voice_autotranscribe: Option<bool>,
    /// Включить/выключить авто-апрув prompt'ов Claude Code целиком.
    #[serde(default)]
    pub auto_approve_enabled: Option<bool>,
    /// Включить/выключить расширенный апрув («не спрашивать снова»).
    #[serde(default)]
    pub auto_approve_broad: Option<bool>,
    /// Включить/выключить автоответ на вопросы `AskUserQuestion`.
    #[serde(default)]
    pub auto_approve_questions: Option<bool>,
    /// Стоп-слова авто-апрува. Семантика — ПОЛНАЯ ЗАМЕНА списка (как у
    /// `telegram_visible_folders`): снять одно стоп-слово в merge-семантике
    /// было бы нечем. Пустой список = только встроенный блоклист.
    #[serde(default)]
    pub auto_approve_blocklist: Option<Vec<String>>,
}

#[derive(Debug)]
struct Inner {
    settings: UserSettings,
    path: PathBuf,
}

/// In-memory + on-disk хранилище [`UserSettings`].
///
/// Cheap-clonable: внутри `Arc<RwLock<Inner>>`. Один экземпляр на
/// процесс — кладётся в `AppState`. Все мутации проходят через `patch`,
/// который делает atomic save на диск.
#[derive(Debug, Clone)]
pub struct UserSettingsStore {
    inner: Arc<RwLock<Inner>>,
}

impl UserSettingsStore {
    /// Создаёт store. Если файл существует — пытается прочитать его и
    /// разобрать как [`UserSettings`]. При успехе использует разобранное
    /// значение; при ошибке парсинга — печатает warning в tracing и
    /// продолжает с [`UserSettings::default`] (политика «битый файл не
    /// блокирует работу»; пользователь поймёт по логам).
    ///
    /// Файл **не создаётся**, если его нет — это нужно для критического
    /// инварианта «нулевая конфигурация = поведение как до фичи».
    pub fn new(path: PathBuf) -> Self {
        let settings = match std::fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<UserSettings>(&body) {
                Ok(s) => {
                    tracing::info!(
                        path = %path.display(),
                        "loaded user_settings.json"
                    );
                    s
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "failed to parse user_settings.json; falling back to defaults"
                    );
                    UserSettings::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %path.display(),
                    "user_settings.json not found; using defaults"
                );
                UserSettings::default()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = ?e,
                    "failed to read user_settings.json; falling back to defaults"
                );
                UserSettings::default()
            }
        };

        Self {
            inner: Arc::new(RwLock::new(Inner { settings, path })),
        }
    }

    /// Возвращает клон текущих настроек под read-lock'ом.
    pub fn get(&self) -> UserSettings {
        let inner = self.inner.read().expect("UserSettingsStore lock poisoned");
        inner.settings.clone()
    }

    /// Применяет частичный patch: только поля `Some(..)` обновляются.
    /// После мутации — atomic save на диск. Возвращает обновлённую копию.
    ///
    /// Валидация: `todo_default_priority` клампится в диапазон `0..=4`
    /// (значения > 4 приводятся к 4). Suffix принимается as-is (без trim) —
    /// клиент сам решает, какие пробелы хранить.
    pub fn patch(&self, payload: PatchUserSettingsReq) -> Result<UserSettings> {
        let mut inner = self.inner.write().expect("UserSettingsStore lock poisoned");
        if let Some(v) = payload.todo_default_plan_mode {
            inner.settings.todo_default_plan_mode = v;
        }
        if let Some(v) = payload.todo_default_priority {
            inner.settings.todo_default_priority = v.min(MAX_PRIORITY);
        }
        if let Some(v) = payload.todo_default_issue_type {
            inner.settings.todo_default_issue_type = v;
        }
        if let Some(v) = payload.todo_plan_mode_suffix {
            inner.settings.todo_plan_mode_suffix = v;
        }
        if let Some(v) = payload.todo_confirm_delete {
            inner.settings.todo_confirm_delete = v;
        }
        if let Some(v) = payload.todo_confirm_promote_on_drag {
            inner.settings.todo_confirm_promote_on_drag = v;
        }
        if let Some(v) = payload.echo_default_model {
            // Пустая строка = сброс в None (используется фронтендом для
            // выбора «use plugin default» — отдельная sentinel-кнопка лишняя).
            inner.settings.echo_default_model = if v.trim().is_empty() {
                None
            } else {
                Some(v)
            };
        }
        if let Some(v) = payload.echo_notifications_enabled {
            inner.settings.echo_notifications_enabled = v;
        }
        if let Some(v) = payload.cmd_hints_enabled {
            inner.settings.cmd_hints_enabled = v;
        }
        if let Some(v) = payload.next_step_enabled {
            inner.settings.next_step_enabled = v;
        }
        if let Some(v) = payload.telegram_hotkeys_enabled {
            inner.settings.telegram_hotkeys_enabled = v;
        }
        if let Some(v) = payload.telegram_hotkeys {
            // Замена целиком, а не merge: см. комментарий у поля в
            // PatchUserSettingsReq. Пустая карта — законный «сброс к дефолтам».
            inner.settings.telegram_hotkeys = sanitize_hotkeys(v);
        }
        if let Some(v) = payload.telegram_filter_enabled {
            inner.settings.telegram_filter_enabled = v;
        }
        if let Some(v) = payload.telegram_visible_folders {
            inner.settings.telegram_visible_folders = sanitize_visible_list(v);
        }
        if let Some(v) = payload.telegram_visible_dialogs {
            inner.settings.telegram_visible_dialogs = sanitize_visible_list(v);
        }
        if let Some(v) = payload.vk_hotkeys_enabled {
            inner.settings.vk_hotkeys_enabled = v;
        }
        if let Some(v) = payload.vk_hotkeys {
            // Тот же санитайзер, что у Telegram: он проверяет только форму
            // (id из [a-z0-9_.-], непустые значения, потолок записей), а смысл
            // id знает исключительно фронтенд. Замена целиком, а не merge —
            // см. комментарий у поля в PatchUserSettingsReq.
            inner.settings.vk_hotkeys = sanitize_hotkeys(v);
        }
        if let Some(v) = payload.vk_filter_enabled {
            inner.settings.vk_filter_enabled = v;
        }
        if let Some(v) = payload.vk_visible_kinds {
            // Замена целиком, а не merge — см. комментарий у поля в
            // PatchUserSettingsReq. Пустой список законен: это «ничего не
            // выбрано», при котором включённый фильтр показывает всё.
            inner.settings.vk_visible_kinds = sanitize_visible_list(v);
        }
        if let Some(v) = payload.vk_visible_peers {
            inner.settings.vk_visible_peers = sanitize_visible_list(v);
        }
        if let Some(v) = payload.bitrix_hotkeys_enabled {
            inner.settings.bitrix_hotkeys_enabled = v;
        }
        if let Some(v) = payload.bitrix_hotkeys {
            // Тот же санитайзер на все три панели: он проверяет только ФОРМУ
            // (id из [a-z0-9_.-], непустые значения, потолок записей), а смысл
            // id знает исключительно фронтенд. Замена целиком, а не merge —
            // см. комментарий у поля в PatchUserSettingsReq.
            inner.settings.bitrix_hotkeys = sanitize_hotkeys(v);
        }
        if let Some(v) = payload.bitrix_filter_enabled {
            inner.settings.bitrix_filter_enabled = v;
        }
        if let Some(v) = payload.bitrix_visible_kinds {
            // Записи вида — короткие ASCII-строки (`user`/`chat`/`task`), а
            // записи диалогов — `DIALOG_ID` портала (`chat7`, `19`, `sg12`).
            // Обе формы проходят общий санитайзер: он допускает
            // `[0-9a-zA-Z_:.-]`, чего хватает и на то, и на другое.
            inner.settings.bitrix_visible_kinds = sanitize_visible_list(v);
        }
        if let Some(v) = payload.bitrix_visible_peers {
            inner.settings.bitrix_visible_peers = sanitize_visible_list(v);
        }
        // Три ОТДЕЛЬНЫХ флага авторазбора голосовых — по одному на панель.
        // Санитайзер им не нужен: это булевы, а не списки.
        if let Some(v) = payload.vk_voice_autotranscribe {
            inner.settings.vk_voice_autotranscribe = v;
        }
        if let Some(v) = payload.telegram_voice_autotranscribe {
            inner.settings.telegram_voice_autotranscribe = v;
        }
        if let Some(v) = payload.bitrix_voice_autotranscribe {
            inner.settings.bitrix_voice_autotranscribe = v;
        }
        if let Some(v) = payload.auto_approve_enabled {
            inner.settings.auto_approve_enabled = v;
        }
        if let Some(v) = payload.auto_approve_broad {
            inner.settings.auto_approve_broad = v;
        }
        if let Some(v) = payload.auto_approve_questions {
            inner.settings.auto_approve_questions = v;
        }
        if let Some(v) = payload.auto_approve_blocklist {
            // Замена целиком, а не merge — см. комментарий у поля в
            // PatchUserSettingsReq. Пустой список законен: это «только
            // встроенный блоклист».
            inner.settings.auto_approve_blocklist = sanitize_blocklist(v);
        }

        save_locked(&inner)?;
        Ok(inner.settings.clone())
    }
}

/// Атомарно сохраняет состояние под write-lock'ом.
///
/// Стратегия (как в `todos::save_locked` и `projects::ProjectStore::save`):
/// пишем в `<file>.tmp`, затем `rename` поверх. На POSIX rename атомарен
/// в рамках одного mount-point.
fn save_locked(inner: &Inner) -> Result<()> {
    let body = serde_json::to_vec_pretty(&inner.settings)
        .context("failed to serialize UserSettings")?;

    if let Some(parent) = inner.path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent dir {}", parent.display())
            })?;
        }
    }

    let mut tmp = inner.path.clone();
    let mut tmp_name = tmp.file_name().map(|s| s.to_owned()).unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, &body)
        .with_context(|| format!("failed to write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &inner.path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp.display(),
            inner.path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Уникальный путь во временной директории — UUID v4 как имя файла.
    fn tmp_path(label: &str) -> PathBuf {
        let id = uuid::Uuid::new_v4();
        std::env::temp_dir().join(format!("devforge_user_settings_{label}_{id}.json"))
    }

    #[test]
    fn test_default_when_no_file() {
        let path = tmp_path("default_no_file");
        assert!(!path.exists(), "precondition: path must not exist");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert_eq!(s, UserSettings::default());
        // Файл НЕ должен появиться от чтения с дефолтами.
        assert!(
            !path.exists(),
            "file must not be created on read-only access"
        );
    }

    #[test]
    fn test_create_patch_reload() {
        let path = tmp_path("create_patch_reload");
        let store = UserSettingsStore::new(path.clone());
        assert_eq!(store.get(), UserSettings::default());

        let patched = store
            .patch(PatchUserSettingsReq {
                todo_default_plan_mode: Some(true),
                todo_default_priority: Some(3),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(patched.todo_default_plan_mode);
        assert_eq!(patched.todo_default_priority, 3);

        // Re-open: новый store на тот же путь должен видеть применённые изменения.
        let store2 = UserSettingsStore::new(path.clone());
        let s2 = store2.get();
        assert!(s2.todo_default_plan_mode);
        assert_eq!(s2.todo_default_priority, 3);
        // Остальные поля — дефолтные.
        assert_eq!(s2.todo_default_issue_type, "task");
        assert_eq!(s2.todo_plan_mode_suffix, "");
        assert!(s2.todo_confirm_delete);
        assert!(!s2.todo_confirm_promote_on_drag);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_priority_clamp() {
        let path = tmp_path("priority_clamp");
        let store = UserSettingsStore::new(path.clone());
        let res = store
            .patch(PatchUserSettingsReq {
                todo_default_priority: Some(10),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.todo_default_priority, 4);

        // И на диске тоже 4 после reload.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().todo_default_priority, 4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_echo_defaults_present() {
        let path = tmp_path("echo_defaults");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        // По дефолту модель не задана (использовать дефолт плагина) и
        // notifications включены.
        assert_eq!(s.echo_default_model, None);
        assert!(s.echo_notifications_enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_echo_patch_set_and_clear_model() {
        let path = tmp_path("echo_patch_model");
        let store = UserSettingsStore::new(path.clone());

        let res = store
            .patch(PatchUserSettingsReq {
                echo_default_model: Some("claude-opus-x".into()),
                echo_notifications_enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(res.echo_default_model.as_deref(), Some("claude-opus-x"));
        assert!(!res.echo_notifications_enabled);

        // Empty string → сброс в None.
        let res2 = store
            .patch(PatchUserSettingsReq {
                echo_default_model: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(res2.echo_default_model, None);

        // Persistence.
        let store2 = UserSettingsStore::new(path.clone());
        let s2 = store2.get();
        assert_eq!(s2.echo_default_model, None);
        assert!(!s2.echo_notifications_enabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_legacy_settings_file_loads_with_echo_defaults() {
        // Эмулируем «старый» settings.json без echo_* полей.
        let path = tmp_path("legacy_load");
        let body = r#"{
            "todo_default_plan_mode": true,
            "todo_default_priority": 1,
            "todo_default_issue_type": "bug",
            "todo_plan_mode_suffix": "",
            "todo_confirm_delete": false,
            "todo_confirm_promote_on_drag": true
        }"#;
        std::fs::write(&path, body).unwrap();

        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        // Старые поля прочитаны.
        assert!(s.todo_default_plan_mode);
        assert_eq!(s.todo_default_priority, 1);
        assert_eq!(s.todo_default_issue_type, "bug");
        assert!(!s.todo_confirm_delete);
        assert!(s.todo_confirm_promote_on_drag);
        // Новые echo-поля — дефолтные.
        assert_eq!(s.echo_default_model, None);
        assert!(s.echo_notifications_enabled);
        // Interface-флаги на legacy-файле — выключены (обе фичи opt-in).
        assert!(!s.cmd_hints_enabled);
        assert!(!s.next_step_enabled);
        // А хоткеи Telegram — включены: старый файл не должен молча лишать
        // пользователя клавиатурной навигации.
        assert!(s.telegram_hotkeys_enabled);
        assert!(s.telegram_hotkeys.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_interface_flags_default_off() {
        // Обе фичи (Cmd-подсказки и «Следующий шаг») — opt-in: при нулевой
        // конфигурации выключены. Это намеренное сужение инварианта tw-z6l,
        // см. шапку модуля — тест фиксирует его, чтобы дефолты не «починили».
        let path = tmp_path("interface_flags_default");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(!s.cmd_hints_enabled);
        assert!(!s.next_step_enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_interface_flags_patch_and_persist() {
        let path = tmp_path("interface_flags_patch");
        let store = UserSettingsStore::new(path.clone());

        let res = store
            .patch(PatchUserSettingsReq {
                cmd_hints_enabled: Some(true),
                next_step_enabled: Some(true),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res.cmd_hints_enabled);
        assert!(res.next_step_enabled);

        // Persistence: новый store на тот же путь видит включённые флаги.
        let store2 = UserSettingsStore::new(path.clone());
        let s2 = store2.get();
        assert!(s2.cmd_hints_enabled);
        assert!(s2.next_step_enabled);

        // И обратно в выключенное состояние.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                next_step_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res3.next_step_enabled);
        // Соседний флаг не задет — patch применяет только Some(..).
        assert!(res3.cmd_hints_enabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_telegram_hotkeys_default_enabled_with_empty_map() {
        // Дефолт ОТЛИЧАЕТСЯ от cmd_hints/next_step — включено. Тест фиксирует
        // это, чтобы правку «привести к общему виду» ловил CI, а не пользователь.
        let path = tmp_path("tg_hotkeys_default");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(s.telegram_hotkeys_enabled);
        assert!(s.telegram_hotkeys.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_telegram_hotkeys_patch_replaces_map_and_persists() {
        let path = tmp_path("tg_hotkeys_patch");
        let store = UserSettingsStore::new(path.clone());

        let mut first = BTreeMap::new();
        first.insert("msg.reply".to_string(), "r".to_string());
        first.insert("focus.input".to_string(), "i".to_string());
        let res = store
            .patch(PatchUserSettingsReq {
                telegram_hotkeys: Some(first),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.telegram_hotkeys.len(), 2);
        assert_eq!(res.telegram_hotkeys.get("msg.reply").unwrap(), "r");

        // Persistence: новый store на тот же путь видит карту.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().telegram_hotkeys.len(), 2);

        // ЗАМЕНА, а не merge: прислали одну запись — прошлые исчезли. Это
        // единственный способ для UI снять переопределение.
        let mut second = BTreeMap::new();
        second.insert("msg.reply".to_string(), "Shift+R".to_string());
        let res2 = store2
            .patch(PatchUserSettingsReq {
                telegram_hotkeys: Some(second),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.telegram_hotkeys.len(), 1);
        assert_eq!(res2.telegram_hotkeys.get("msg.reply").unwrap(), "Shift+R");
        assert!(!res2.telegram_hotkeys.contains_key("focus.input"));

        // Пустая карта — законный сброс к дефолтам.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                telegram_hotkeys: Some(BTreeMap::new()),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res3.telegram_hotkeys.is_empty());
        // Соседний флаг не задет.
        assert!(res3.telegram_hotkeys_enabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_telegram_hotkeys_toggle_does_not_touch_map() {
        let path = tmp_path("tg_hotkeys_toggle");
        let store = UserSettingsStore::new(path.clone());
        let mut map = BTreeMap::new();
        map.insert("msg.reply".to_string(), "r".to_string());
        store
            .patch(PatchUserSettingsReq {
                telegram_hotkeys: Some(map),
                ..Default::default()
            })
            .expect("patch must succeed");

        let res = store
            .patch(PatchUserSettingsReq {
                telegram_hotkeys_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res.telegram_hotkeys_enabled);
        // Выключение хоткеев НЕ стирает переназначения: включив обратно,
        // пользователь должен получить свои клавиши, а не дефолтные.
        assert_eq!(res.telegram_hotkeys.get("msg.reply").unwrap(), "r");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sanitize_hotkeys_drops_malformed_entries() {
        let mut raw = BTreeMap::new();
        // Годные записи.
        raw.insert("msg.reply".to_string(), "r".to_string());
        raw.insert("dialog_next".to_string(), "Shift+J".to_string());
        raw.insert("log-bottom".to_string(), "  G  ".to_string()); // trim
        // Мусор.
        raw.insert("".to_string(), "x".to_string()); // пустой ключ
        raw.insert("msg.copy".to_string(), "   ".to_string()); // пустое значение
        raw.insert("Msg.Upper".to_string(), "u".to_string()); // верхний регистр
        raw.insert("msg reply".to_string(), "q".to_string()); // пробел в id
        raw.insert("msg/slash".to_string(), "s".to_string()); // недопустимый символ
        raw.insert("a".repeat(65), "z".to_string()); // длинный ключ
        raw.insert("msg.long".to_string(), "b".repeat(65)); // длинное значение
        raw.insert("msg.ctrl".to_string(), "a\nb".to_string()); // управляющий символ

        let clean = sanitize_hotkeys(raw);
        assert_eq!(clean.len(), 3, "остаться должны только три годные записи");
        assert_eq!(clean.get("msg.reply").unwrap(), "r");
        assert_eq!(clean.get("dialog_next").unwrap(), "Shift+J");
        // Значение обрезано по краям, но не изменено внутри.
        assert_eq!(clean.get("log-bottom").unwrap(), "G");
    }

    #[test]
    fn test_sanitize_hotkeys_caps_map_size() {
        let mut raw = BTreeMap::new();
        for i in 0..(MAX_TELEGRAM_HOTKEYS + 20) {
            raw.insert(format!("action.{i:03}"), "k".to_string());
        }
        let clean = sanitize_hotkeys(raw);
        assert_eq!(clean.len(), MAX_TELEGRAM_HOTKEYS);
        // BTreeMap упорядочен — отсечение детерминировано: остаются первые по
        // алфавиту, а не случайные.
        assert!(clean.contains_key("action.000"));
        assert!(!clean.contains_key("action.083"));
    }

    #[test]
    fn test_telegram_hotkeys_patch_sanitizes() {
        let path = tmp_path("tg_hotkeys_sanitize");
        let store = UserSettingsStore::new(path.clone());
        let mut raw = BTreeMap::new();
        raw.insert("msg.reply".to_string(), "r".to_string());
        raw.insert("ПЛОХОЙ".to_string(), "x".to_string());
        let res = store
            .patch(PatchUserSettingsReq {
                telegram_hotkeys: Some(raw),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.telegram_hotkeys.len(), 1);
        assert!(res.telegram_hotkeys.contains_key("msg.reply"));
        let _ = std::fs::remove_file(&path);
    }

    /// **АВТОРАЗБОР ГОЛОСОВЫХ ВЫКЛЮЧЕН ВО ВСЕХ ТРЁХ ПАНЕЛЯХ, И СТАРЫЙ ФАЙЛ
    /// БЕЗ ЭТИХ ПОЛЕЙ ЧИТАЕТСЯ, А НЕ ЛОМАЕТСЯ.**
    ///
    /// Файл пишется РУКАМИ, без новых ключей, — ровно то, что лежит на диске у
    /// человека, обновившего devforge. Ошибка миграции здесь означала бы, что
    /// обновление сбрасывает ВСЕ настройки пользователя разом.
    #[test]
    fn test_voice_autotranscribe_defaults_off_on_an_old_file() {
        let path = tmp_path("voice_auto_old_file");
        std::fs::write(
            &path,
            r#"{"todo_default_priority":1,"vk_filter_enabled":true,
                "vk_visible_peers":["2000000042"]}"#,
        )
        .unwrap();
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(!s.vk_voice_autotranscribe);
        assert!(!s.telegram_voice_autotranscribe);
        assert!(!s.bitrix_voice_autotranscribe);
        // Старые поля при этом уцелели — файл прочитан, а не заменён дефолтами.
        assert!(s.vk_filter_enabled);
        assert_eq!(s.vk_visible_peers, vec!["2000000042"]);
        let _ = std::fs::remove_file(&path);
    }

    /// **PATCH ОДНОГО ФЛАГА НЕ ТРОГАЕТ ДВА СОСЕДНИХ.**
    ///
    /// Три панели — три решения человека, и включение разбора в VK не имеет
    /// права включить его в рабочем портале.
    #[test]
    fn test_voice_autotranscribe_patch_is_per_panel() {
        let path = tmp_path("voice_auto_per_panel");
        let store = UserSettingsStore::new(path.clone());
        let res = store
            .patch(PatchUserSettingsReq {
                vk_voice_autotranscribe: Some(true),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res.vk_voice_autotranscribe);
        assert!(!res.telegram_voice_autotranscribe);
        assert!(!res.bitrix_voice_autotranscribe);

        let res = store
            .patch(PatchUserSettingsReq {
                bitrix_voice_autotranscribe: Some(true),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res.vk_voice_autotranscribe, "первый флаг не сброшен вторым патчем");
        assert!(res.bitrix_voice_autotranscribe);
        assert!(!res.telegram_voice_autotranscribe);

        // Плагин читает ЭТОТ файл сам (forge_voice::prefs), поэтому патч обязан
        // оказаться на диске сразу, а не при завершении процесса.
        let raw = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(json["vk_voice_autotranscribe"], true);
        assert_eq!(json["telegram_voice_autotranscribe"], false);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_telegram_visibility_defaults_off_and_empty() {
        // Нулевая конфигурация обязана показывать ВСЁ: фильтр — opt-in, иначе
        // обновление devforge молча спрятало бы часть чатов.
        let path = tmp_path("tg_visibility_default");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(!s.telegram_filter_enabled);
        assert!(s.telegram_visible_folders.is_empty());
        assert!(s.telegram_visible_dialogs.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_telegram_visibility_patch_replaces_lists() {
        let path = tmp_path("tg_visibility_patch");
        let store = UserSettingsStore::new(path.clone());

        let res = store
            .patch(PatchUserSettingsReq {
                telegram_filter_enabled: Some(true),
                telegram_visible_folders: Some(vec![
                    "folder:12".to_string(),
                    "archive".to_string(),
                ]),
                telegram_visible_dialogs: Some(vec!["-1001234".to_string(), "777".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res.telegram_filter_enabled);
        assert_eq!(res.telegram_visible_folders, vec!["folder:12", "archive"]);
        assert_eq!(res.telegram_visible_dialogs, vec!["-1001234", "777"]);

        // Persistence.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().telegram_visible_dialogs.len(), 2);

        // ЗАМЕНА, а не добавление: снять галочку иначе нечем.
        let res2 = store2
            .patch(PatchUserSettingsReq {
                telegram_visible_dialogs: Some(vec!["777".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.telegram_visible_dialogs, vec!["777"]);
        // Соседний список не задет — patch применяет только Some(..).
        assert_eq!(res2.telegram_visible_folders.len(), 2);

        // Выключение тумблера НЕ стирает выбор: включив обратно, пользователь
        // должен получить свой список, а не пустой.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                telegram_filter_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res3.telegram_filter_enabled);
        assert_eq!(res3.telegram_visible_dialogs, vec!["777"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sanitize_visible_list() {
        let clean = sanitize_visible_list(vec![
            "  folder:12  ".to_string(), // trim
            "-1001234567890".to_string(),
            "inbox".to_string(),
            "inbox".to_string(),  // дубль
            "".to_string(),       // пусто
            "   ".to_string(),    // одни пробелы
            "чат".to_string(),    // не ASCII
            "a b".to_string(),    // пробел внутри
            "id/slash".to_string(),
            "x".repeat(65), // длинная
        ]);
        assert_eq!(clean, vec!["folder:12", "-1001234567890", "inbox"]);
    }

    #[test]
    fn test_sanitize_visible_list_caps_size() {
        let raw: Vec<String> = (0..(MAX_TELEGRAM_VISIBLE + 10))
            .map(|i| format!("id{i}"))
            .collect();
        let clean = sanitize_visible_list(raw);
        assert_eq!(clean.len(), MAX_TELEGRAM_VISIBLE);
        // Порядок сохранён — он виден пользователю в списке настроек.
        assert_eq!(clean[0], "id0");
    }

    #[test]
    fn test_vk_visibility_defaults_off_and_empty() {
        // Нулевая конфигурация обязана показывать ВСЕ беседы: фильтр — opt-in,
        // иначе обновление devforge молча спрятало бы часть переписки.
        let path = tmp_path("vk_visibility_default");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(!s.vk_filter_enabled);
        assert!(s.vk_visible_kinds.is_empty());
        assert!(s.vk_visible_peers.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_visibility_patch_replaces_lists() {
        let path = tmp_path("vk_visibility_patch");
        let store = UserSettingsStore::new(path.clone());

        let res = store
            .patch(PatchUserSettingsReq {
                vk_filter_enabled: Some(true),
                vk_visible_kinds: Some(vec!["chat".to_string(), "community".to_string()]),
                vk_visible_peers: Some(vec![
                    "2000000042".to_string(),
                    "-70".to_string(),
                    "777".to_string(),
                ]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res.vk_filter_enabled);
        assert_eq!(res.vk_visible_kinds, vec!["chat", "community"]);
        assert_eq!(res.vk_visible_peers, vec!["2000000042", "-70", "777"]);

        // Persistence.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().vk_visible_peers.len(), 3);

        // ЗАМЕНА, а не слияние: снять галочку иначе нечем.
        let res2 = store2
            .patch(PatchUserSettingsReq {
                vk_visible_peers: Some(vec!["777".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.vk_visible_peers, vec!["777"]);
        // Соседний список не задет — patch применяет только Some(..).
        assert_eq!(res2.vk_visible_kinds, vec!["chat", "community"]);

        // Пустой список — законное «ничего не выбрано», а не игнорируемый
        // patch: фронт снимает последнюю галочку именно так.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                vk_visible_peers: Some(Vec::new()),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res3.vk_visible_peers.is_empty());

        // Выключение тумблера НЕ стирает выбор: включив обратно, пользователь
        // должен получить свой список типов, а не пустой.
        let res4 = store2
            .patch(PatchUserSettingsReq {
                vk_filter_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res4.vk_filter_enabled);
        assert_eq!(res4.vk_visible_kinds, vec!["chat", "community"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_visibility_patch_sanitizes_and_keeps_telegram_intact() {
        let path = tmp_path("vk_visibility_sanitize");
        let store = UserSettingsStore::new(path.clone());

        // Сначала кладём телеграмный белый список: соседняя панель не должна
        // пострадать от сохранения формы VK.
        store
            .patch(PatchUserSettingsReq {
                telegram_filter_enabled: Some(true),
                telegram_visible_dialogs: Some(vec!["13".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");

        let res = store
            .patch(PatchUserSettingsReq {
                vk_visible_kinds: Some(vec![
                    "  chat  ".to_string(),   // trim
                    "user".to_string(),
                    "user".to_string(),       // дубль
                    "".to_string(),           // пусто
                    "беседа".to_string(),     // не ASCII
                    "open app".to_string(),   // пробел внутри
                    "x".repeat(65),           // длинная
                ]),
                vk_visible_peers: Some(vec!["2000000001".to_string(), "-1/2".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.vk_visible_kinds, vec!["chat", "user"]);
        assert_eq!(res.vk_visible_peers, vec!["2000000001"]);
        // Телеграмные поля на месте — панели делят файл, но не настройки.
        assert!(res.telegram_filter_enabled);
        assert_eq!(res.telegram_visible_dialogs, vec!["13"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_visible_peers_capped() {
        let path = tmp_path("vk_visibility_cap");
        let store = UserSettingsStore::new(path.clone());
        let raw: Vec<String> = (0..(MAX_TELEGRAM_VISIBLE + 10))
            .map(|i| format!("200000{i:04}"))
            .collect();
        let res = store
            .patch(PatchUserSettingsReq {
                vk_visible_peers: Some(raw),
                ..Default::default()
            })
            .expect("patch must succeed");
        // Потолок применяется и на пути VK: список приезжает из браузера и
        // ложится в файл ровно так же, как телеграмный.
        assert_eq!(res.vk_visible_peers.len(), MAX_TELEGRAM_VISIBLE);
        assert_eq!(res.vk_visible_peers[0], "2000000000");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_hotkeys_default_enabled_with_empty_map() {
        // Дефолт ВКЛЮЧЁН — как у telegram_hotkeys_enabled и в отличие от
        // vk_filter_enabled рядом. Фронт читает его `!== false`, и правка
        // «привести к общему виду с фильтром» молча выключила бы фичу всем, у
        // кого настройка ещё ни разу не сохранялась. Ловит это CI, не пользователь.
        let path = tmp_path("vk_hotkeys_default");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(s.vk_hotkeys_enabled);
        assert!(s.vk_hotkeys.is_empty());
        // Соседний тумблер VK дефолтится НАОБОРОТ — эти два поля путают чаще
        // всего, поэтому разница закреплена одним ассертом.
        assert!(!s.vk_filter_enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_hotkeys_default_survives_partial_file() {
        // Файл, записанный СТАРОЙ версией devforge (до появления поля), обязан
        // читаться как «хоткеи включены». Без `#[serde(default = ...)]` тут
        // приехал бы false, и фича исчезла бы у всех, кто обновился.
        let path = tmp_path("vk_hotkeys_partial");
        std::fs::write(&path, r#"{"cmd_hints_enabled":true}"#).expect("write");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(s.vk_hotkeys_enabled);
        assert!(s.vk_hotkeys.is_empty());
        assert!(s.telegram_hotkeys_enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_hotkeys_patch_replaces_map_and_persists() {
        let path = tmp_path("vk_hotkeys_patch");
        let store = UserSettingsStore::new(path.clone());

        let mut first = BTreeMap::new();
        first.insert("msg.reply".to_string(), "r".to_string());
        first.insert("msg.attachments".to_string(), "v".to_string());
        let res = store
            .patch(PatchUserSettingsReq {
                vk_hotkeys: Some(first),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.vk_hotkeys.len(), 2);
        assert_eq!(res.vk_hotkeys.get("msg.attachments").unwrap(), "v");

        // Persistence: новый store на тот же путь видит карту.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().vk_hotkeys.len(), 2);

        // ЗАМЕНА, а не merge: прислали одну запись — прошлые исчезли. Это
        // единственный способ для UI снять переопределение.
        let mut second = BTreeMap::new();
        second.insert("msg.reply".to_string(), "Shift+R".to_string());
        let res2 = store2
            .patch(PatchUserSettingsReq {
                vk_hotkeys: Some(second),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.vk_hotkeys.len(), 1);
        assert_eq!(res2.vk_hotkeys.get("msg.reply").unwrap(), "Shift+R");
        assert!(!res2.vk_hotkeys.contains_key("msg.attachments"));

        // Пустая карта — законный сброс к дефолтам.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                vk_hotkeys: Some(BTreeMap::new()),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res3.vk_hotkeys.is_empty());
        assert!(res3.vk_hotkeys_enabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_hotkeys_toggle_does_not_touch_map_or_telegram() {
        let path = tmp_path("vk_hotkeys_toggle");
        let store = UserSettingsStore::new(path.clone());

        // Кладём переназначения ОБЕИХ панелей: они делят один файл, и сохранение
        // формы VK не смеет трогать телеграмные клавиши.
        let mut vk = BTreeMap::new();
        vk.insert("msg.copy".to_string(), "Ctrl+c".to_string());
        let mut tg = BTreeMap::new();
        tg.insert("msg.reply".to_string(), "Shift+R".to_string());
        store
            .patch(PatchUserSettingsReq {
                vk_hotkeys: Some(vk),
                telegram_hotkeys: Some(tg),
                ..Default::default()
            })
            .expect("patch must succeed");

        let res = store
            .patch(PatchUserSettingsReq {
                vk_hotkeys_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res.vk_hotkeys_enabled);
        // Выключение хоткеев НЕ стирает переназначения: включив обратно,
        // пользователь должен получить свои клавиши, а не дефолтные.
        assert_eq!(res.vk_hotkeys.get("msg.copy").unwrap(), "Ctrl+c");
        // Соседняя панель не задета ни тумблером, ни картой.
        assert!(res.telegram_hotkeys_enabled);
        assert_eq!(res.telegram_hotkeys.get("msg.reply").unwrap(), "Shift+R");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_hotkeys_patch_sanitizes() {
        let path = tmp_path("vk_hotkeys_sanitize");
        let store = UserSettingsStore::new(path.clone());
        let mut raw = BTreeMap::new();
        raw.insert("msg.reply".to_string(), "Shift+R".to_string());
        raw.insert("Msg.Reply".to_string(), "r".to_string()); // регистр в id
        raw.insert("msg copy".to_string(), "y".to_string()); // пробел в id
        raw.insert("msg.quote".to_string(), "  ".to_string()); // пустое значение
        raw.insert("x".repeat(65), "q".to_string()); // длинный id
        let res = store
            .patch(PatchUserSettingsReq {
                vk_hotkeys: Some(raw),
                ..Default::default()
            })
            .expect("patch must succeed");
        // Тот же санитайзер, что у Telegram: проверяется только ФОРМА.
        assert_eq!(res.vk_hotkeys.len(), 1);
        assert!(res.vk_hotkeys.contains_key("msg.reply"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_vk_hotkeys_capped() {
        let path = tmp_path("vk_hotkeys_cap");
        let store = UserSettingsStore::new(path.clone());
        let mut raw = BTreeMap::new();
        for i in 0..(MAX_TELEGRAM_HOTKEYS + 20) {
            raw.insert(format!("act.{i:03}"), "j".to_string());
        }
        let res = store
            .patch(PatchUserSettingsReq {
                vk_hotkeys: Some(raw),
                ..Default::default()
            })
            .expect("patch must succeed");
        // Потолок общий на обе панели: карта приезжает из браузера и ложится в
        // файл ровно так же, как телеграмная.
        assert_eq!(res.vk_hotkeys.len(), MAX_TELEGRAM_HOTKEYS);

        let _ = std::fs::remove_file(&path);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Битрикс24 (Ф11)
    //
    // Проверки сгруппированы вокруг ОДНОЙ ошибки, которую здесь легче всего
    // сделать: перепутать гейты дефолтов двух флагов. `bitrix_hotkeys_enabled`
    // по умолчанию ВКЛЮЧЁН (фронт читает `!== false`), `bitrix_filter_enabled`
    // по умолчанию ВЫКЛЮЧЕН (фронт читает `=== true`). Перепутанные местами,
    // они дают два молчаливых регресса: неработающие хоткеи и спрятанные чаты.
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn test_bitrix_defaults_hotkeys_on_filter_off() {
        let path = tmp_path("bitrix_defaults");
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        // Хоткеи — ВКЛ по умолчанию.
        assert!(s.bitrix_hotkeys_enabled);
        assert!(s.bitrix_hotkeys.is_empty());
        // Фильтр — ВЫКЛ по умолчанию, списки пусты.
        assert!(!s.bitrix_filter_enabled);
        assert!(s.bitrix_visible_kinds.is_empty());
        assert!(s.bitrix_visible_peers.is_empty());
        // И то же самое у чистого `Default` — файла может не быть вовсе.
        let d = UserSettings::default();
        assert!(d.bitrix_hotkeys_enabled);
        assert!(!d.bitrix_filter_enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_defaults_survive_partial_file() {
        // Файл, записанный СТАРОЙ версией devforge (полей Битрикс24 в нём нет
        // вовсе). Отсутствующее `bitrix_hotkeys_enabled` обязано читаться как
        // `true`, иначе обновление молча выключит навигацию всем, кто ни разу
        // не открывал настройки. Это ровно тот случай, который ловит
        // `#[serde(default = "default_bitrix_hotkeys_enabled")]`.
        let path = tmp_path("bitrix_partial");
        std::fs::write(&path, br#"{"todo_default_priority":3}"#).unwrap();
        let store = UserSettingsStore::new(path.clone());
        let s = store.get();
        assert!(s.bitrix_hotkeys_enabled);
        assert!(s.bitrix_hotkeys.is_empty());
        // А вот фильтр в том же файле обязан остаться выключенным.
        assert!(!s.bitrix_filter_enabled);
        // Соседние панели не задеты.
        assert!(s.vk_hotkeys_enabled);
        assert!(s.telegram_hotkeys_enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_visibility_patch_replaces_lists() {
        let path = tmp_path("bitrix_visibility_patch");
        let store = UserSettingsStore::new(path.clone());

        let res = store
            .patch(PatchUserSettingsReq {
                bitrix_filter_enabled: Some(true),
                bitrix_visible_kinds: Some(vec!["chat".to_string(), "task".to_string()]),
                // DIALOG_ID портала полиморфен: число для личного диалога,
                // строка `chat7`/`sg12` для группового. Санитайзер обязан
                // пропускать обе формы.
                bitrix_visible_peers: Some(vec![
                    "chat7".to_string(),
                    "19".to_string(),
                    "sg12".to_string(),
                ]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res.bitrix_filter_enabled);
        assert_eq!(res.bitrix_visible_kinds, vec!["chat", "task"]);
        assert_eq!(res.bitrix_visible_peers, vec!["chat7", "19", "sg12"]);

        // Persistence.
        let store2 = UserSettingsStore::new(path.clone());
        assert_eq!(store2.get().bitrix_visible_peers.len(), 3);

        // ЗАМЕНА, а не слияние: снять галочку иначе нечем.
        let res2 = store2
            .patch(PatchUserSettingsReq {
                bitrix_visible_peers: Some(vec!["chat7".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.bitrix_visible_peers, vec!["chat7"]);
        // Соседний список не задет — patch применяет только Some(..).
        assert_eq!(res2.bitrix_visible_kinds, vec!["chat", "task"]);

        // Выключение тумблера НЕ стирает выбор.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                bitrix_filter_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res3.bitrix_filter_enabled);
        assert_eq!(res3.bitrix_visible_peers, vec!["chat7"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_visibility_patch_sanitizes_and_caps() {
        let path = tmp_path("bitrix_visibility_sanitize");
        let store = UserSettingsStore::new(path.clone());
        let res = store
            .patch(PatchUserSettingsReq {
                bitrix_visible_kinds: Some(vec![
                    "  task  ".to_string(),   // trim
                    "chat".to_string(),
                    "chat".to_string(),       // дубль
                    String::new(),            // пустая
                    "с пробелом".to_string(), // недопустимые символы
                ]),
                bitrix_visible_peers: Some(vec!["chat7".to_string(), "../etc".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.bitrix_visible_kinds, vec!["task", "chat"]);
        assert_eq!(res.bitrix_visible_peers, vec!["chat7"]);

        // Потолок общий на все три панели.
        let raw: Vec<String> = (0..(MAX_TELEGRAM_VISIBLE + 10))
            .map(|i| format!("chat{i}"))
            .collect();
        let res2 = store
            .patch(PatchUserSettingsReq {
                bitrix_visible_peers: Some(raw),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.bitrix_visible_peers.len(), MAX_TELEGRAM_VISIBLE);
        assert_eq!(res2.bitrix_visible_peers[0], "chat0");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_hotkeys_patch_replaces_map_and_sanitizes() {
        let path = tmp_path("bitrix_hotkeys_patch");
        let store = UserSettingsStore::new(path.clone());

        let mut first = BTreeMap::new();
        first.insert("msg.reply".to_string(), "r".to_string());
        first.insert("task.card".to_string(), "c".to_string());
        // Мусор отбрасывается по ФОРМЕ: смысл id знает только фронтенд.
        first.insert("MSG.SHOUT".to_string(), "x".to_string());
        first.insert("msg.empty".to_string(), "   ".to_string());
        let res = store
            .patch(PatchUserSettingsReq {
                bitrix_hotkeys: Some(first),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.bitrix_hotkeys.len(), 2);
        assert_eq!(res.bitrix_hotkeys.get("task.card").unwrap(), "c");

        // Persistence + ЗАМЕНА карты целиком.
        let store2 = UserSettingsStore::new(path.clone());
        let mut second = BTreeMap::new();
        second.insert("msg.reply".to_string(), "Shift+R".to_string());
        let res2 = store2
            .patch(PatchUserSettingsReq {
                bitrix_hotkeys: Some(second),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res2.bitrix_hotkeys.len(), 1);
        assert!(!res2.bitrix_hotkeys.contains_key("task.card"));
        // Пустая карта — законный сброс к дефолтам, тумблер при этом не гаснет.
        let res3 = store2
            .patch(PatchUserSettingsReq {
                bitrix_hotkeys: Some(BTreeMap::new()),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(res3.bitrix_hotkeys.is_empty());
        assert!(res3.bitrix_hotkeys_enabled);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_hotkeys_toggle_does_not_touch_neighbours() {
        let path = tmp_path("bitrix_hotkeys_toggle");
        let store = UserSettingsStore::new(path.clone());

        let mut b24 = BTreeMap::new();
        b24.insert("msg.copy".to_string(), "Ctrl+c".to_string());
        let mut vk = BTreeMap::new();
        vk.insert("msg.reply".to_string(), "q".to_string());
        store
            .patch(PatchUserSettingsReq {
                bitrix_hotkeys: Some(b24),
                vk_hotkeys: Some(vk),
                ..Default::default()
            })
            .expect("patch must succeed");

        let res = store
            .patch(PatchUserSettingsReq {
                bitrix_hotkeys_enabled: Some(false),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert!(!res.bitrix_hotkeys_enabled);
        // Тумблер не трогает карту — иначе выключение стирало бы настройку.
        assert_eq!(res.bitrix_hotkeys.get("msg.copy").unwrap(), "Ctrl+c");
        // И не трогает соседние панели.
        assert!(res.vk_hotkeys_enabled);
        assert!(res.telegram_hotkeys_enabled);
        assert_eq!(res.vk_hotkeys.get("msg.reply").unwrap(), "q");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_hotkeys_capped() {
        let path = tmp_path("bitrix_hotkeys_cap");
        let store = UserSettingsStore::new(path.clone());
        let mut raw = BTreeMap::new();
        for i in 0..(MAX_TELEGRAM_HOTKEYS + 20) {
            raw.insert(format!("act.{i:03}"), "j".to_string());
        }
        let res = store
            .patch(PatchUserSettingsReq {
                bitrix_hotkeys: Some(raw),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.bitrix_hotkeys.len(), MAX_TELEGRAM_HOTKEYS);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_bitrix_settings_never_store_webhook_secret() {
        // Секрет Битрикс24 — ВЕСЬ URL вебхука целиком. В user_settings.json он
        // не попадает ни целиком, ни фрагментом: структура настроек его просто
        // не умеет принять. Проверяем сериализованный файл: у `PatchUserSettingsReq`
        // нет ни одного поля, куда URL мог бы приехать, а сам JSON не содержит
        // ни `rest/`, ни `bitrix24.ru`.
        let path = tmp_path("bitrix_no_secret");
        let store = UserSettingsStore::new(path.clone());
        store
            .patch(PatchUserSettingsReq {
                bitrix_filter_enabled: Some(true),
                bitrix_visible_peers: Some(vec!["chat7".to_string()]),
                ..Default::default()
            })
            .expect("patch must succeed");
        let body = std::fs::read_to_string(&path).expect("file must exist");
        assert!(!body.contains("/rest/"));
        assert!(!body.contains("webhook"));
        assert!(!body.contains("bitrix24."));
        // Зато сами настройки видимости на месте.
        assert!(body.contains("bitrix_visible_peers"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_suffix_not_trimmed() {
        // Хранение suffix должно быть «как есть» — даже с ведущими пробелами.
        let path = tmp_path("suffix_no_trim");
        let store = UserSettingsStore::new(path.clone());
        let res = store
            .patch(PatchUserSettingsReq {
                todo_plan_mode_suffix: Some("  spaced  ".to_string()),
                ..Default::default()
            })
            .expect("patch must succeed");
        assert_eq!(res.todo_plan_mode_suffix, "  spaced  ");

        let _ = std::fs::remove_file(&path);
    }
}
