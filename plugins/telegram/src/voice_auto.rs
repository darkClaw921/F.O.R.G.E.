//! Автоматический разбор новых голосовых сообщений Telegram.
//!
//! Близнец `plugins/vk/src/voice_auto.rs`; расхождения перечислены ниже и
//! каждое вынужденное.
//!
//! Точка входа — [`on_new_message`], её зовёт `handle_update`
//! (`plugins/telegram/src/updates.rs`) на кадре `new_message`.
//!
//! # ГОЛОСОВОЕ — ЭТО `DocumentAttributeAudio` С ПРИЗНАКОМ `voice`
//!
//! И только оно. Обычное аудио (`kind == "audio"`) — пересланный трек, подкаст,
//! запись концерта — автоматически НЕ разбирается: это может быть час музыки,
//! которую никто не просил переводить в текст (а в облачном движке — ещё и за
//! деньги). Кнопкой «расшифровать» его разобрать по-прежнему можно: там решение
//! принимает человек. Разделение живёт в [`is_auto_kind`] и проверено тестом.
//!
//! # У СООБЩЕНИЯ РОВНО ОДНО ВЛОЖЕНИЕ
//!
//! Поэтому индекс всегда [`crate::voice::ONLY_INDEX`], а не поле DTO: ключ
//! кэша Telegram индекса не содержит вовсе.
//!
//! # ПАПКИ ЧАТА РАЗРЕШАЮТСЯ ЗАПРОСОМ, А НЕ ВЫВОДЯТСЯ ИЗ `dialog_id`
//!
//! Оба белых списка Telegram — и видимость, и скоуп авторазбора — строятся по
//! ПАПКАМ аккаунта. Из одного `dialog_id` папка не следует: правила папки
//! читают вид чата, mute, непрочитанное и явные include/exclude
//! ([`crate::dto::dialog_in_folder`]). Поэтому [`dialog_groups`] спрашивает
//! Telegram: папки берутся из кэша состояния (их правки приезжают
//! `updateDialogFilter*`), сам чат — одним `messages.getPeerDialogs`.
//!
//! Запрос делается ТОЛЬКО когда хоть один из белых списков ДЕЙСТВУЕТ и только
//! для голосовых, то есть единицы раз в минуту в худшем случае. Раньше здесь
//! стоял `peer_kind: None` — «папку не знаем», — и это тихо ломало фичу: при
//! непустом скоупе по папкам НИ ОДНО голосовое не разбиралось, сколько бы
//! папок человек ни отметил.
//!
//! Сторона отказа осталась консервативной: не смогли разрешить папки (нет
//! связи, чат недоступен) — считаем группы неизвестными, и при действующем
//! белом списке разбирается лишь чат, отмеченный ПОИМЁННО. Не разобрать
//! голосовое — мелкая неприятность (кнопка на месте), а разобрать его в чате,
//! который человек исключил, — отправить чужую речь в облако вопреки
//! настройке.

use std::sync::Arc;

use forge_voice::auto::{Candidate, Skip};
use forge_voice::prefs::Prefs;

use crate::dto::{self, MessageDto};
use crate::media_cache::{CacheKey, MediaKind};
use crate::state::TelegramState;
use crate::voice::{voice_key, ONLY_INDEX};

/// Вид вложения, который авторазбор берёт САМ.
///
/// Единственный. См. шапку модуля: обычное аудио остаётся человеку.
pub const AUTO_KIND: &str = "voice";

/// Настройки панели Telegram из `user_settings.json` — читаются на каждое
/// голосовое, чтобы тумблер действовал без перезапуска.
#[must_use]
pub fn prefs(state: &TelegramState) -> Prefs {
    forge_voice::prefs::read(
        &state.config.user_settings_path,
        forge_voice::prefs::TELEGRAM,
    )
}

/// Берёт ли авторазбор такое вложение.
#[must_use]
pub fn is_auto_kind(kind: &str) -> bool {
    kind.trim().eq_ignore_ascii_case(AUTO_KIND)
}

/// Обрабатывает новое сообщение из потока апдейтов.
///
/// Возвращает число запущенных фоновых задач: ноль — значит не произошло
/// ничего, и это ровно то, что проверяют тесты выключенного тумблера.
pub async fn on_new_message(state: &Arc<TelegramState>, msg: &MessageDto) -> usize {
    // Дешёвая отсечка до всякого чтения настроек: у сообщения либо есть
    // голосовое вложение, либо нет, и это видно прямо в DTO.
    let Some(media) = msg.media.as_ref().filter(|m| is_auto_kind(&m.kind)) else {
        return 0;
    };
    let Ok(dialog_id) = msg.dialog_id.trim().parse::<i64>() else {
        return 0;
    };

    let prefs = prefs(state);
    let max_duration_secs = if prefs.autotranscribe {
        state
            .voice_config()
            .map(|c| c.max_duration_secs)
            .unwrap_or(forge_voice::config::DEFAULT_MAX_DURATION_SECS)
    } else {
        0
    };

    // Папки — только если они кому-то нужны: при пустых белых списках это был
    // бы RPC ради значения, которое всё равно не спросят.
    let groups = if prefs.filter_active() || prefs.scope_active() {
        dialog_groups(state, &msg.dialog_id).await
    } else {
        Vec::new()
    };
    let group_refs: Vec<&str> = groups.iter().map(String::as_str).collect();

    let cand = Candidate {
        kind: &media.kind,
        outgoing: msg.out,
        duration_secs: media.duration_secs.map(i64::from),
        peer: &msg.dialog_id,
        peer_groups: &group_refs,
    };
    if let Err(skip) = forge_voice::auto::decide(&prefs, &cand, max_duration_secs) {
        log_skip(&msg.dialog_id, msg.id, &skip);
        return 0;
    }

    let key = CacheKey {
        dialog_id,
        message_id: msg.id,
        kind: MediaKind::Full,
    };
    if !state.voice_seen.claim(&voice_key(&key)) {
        tracing::trace!(
            target: "forge_telegram",
            key = %key.id(),
            "forge-telegram: voice auto-transcribe already claimed"
        );
        return 0;
    }

    let st = state.clone();
    let handle = tokio::spawn(async move { run_one(st, key).await });
    state.register_worker(handle).await;
    1
}

/// Ключи белых списков, которым подходит чат (`inbox`/`archive` плюс папки).
///
/// Пустой список означает «групп не знаем» — при ДЕЙСТВУЮЩЕМ белом списке
/// такой чат проходит только поимённо (см. шапку модуля). Так бывает, когда
/// нет подключения, чат недоступен или его peer'а нет в кэше.
///
/// Папки кэшируются в состоянии до ближайшего `updateDialogFilter*`, сам чат
/// спрашивается каждый раз: его mute, архив и непрочитанное меняются
/// постоянно, а от них зависят `exclude_*`-правила папок.
pub async fn dialog_groups(state: &Arc<TelegramState>, dialog_id: &str) -> Vec<String> {
    let Some(id) = dto::parse_dialog_id(dialog_id) else {
        return Vec::new();
    };
    let Some(client) = state.client().await else {
        return Vec::new();
    };
    let folders = match state.cached_folders().await {
        Some(f) => f,
        None => match crate::routes::dialogs::fetch_folders(&client).await {
            Ok(f) => {
                state.set_folders(f.clone()).await;
                f
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_telegram",
                    dialog_id = %dialog_id,
                    error = %e.body.message,
                    "forge-telegram: could not read folders for voice auto-transcribe"
                );
                return Vec::new();
            }
        },
    };
    let defaults = state.notify_defaults().await;
    let Some(dialog) =
        crate::updates::fetch_peer_dialog(state, &client, id, defaults, "voice_scope").await
    else {
        return Vec::new();
    };
    let keys = dto::dialog_group_keys(&dialog, &folders);
    tracing::debug!(
        target: "forge_telegram",
        dialog_id = %dialog_id,
        groups = %keys.join(","),
        "forge-telegram: voice auto-transcribe resolved the chat folders"
    );
    keys
}

/// Печатает причину пропуска (`debug`: у выключенного тумблера это строка на
/// каждое входящее голосовое).
fn log_skip(dialog_id: &str, message_id: i32, skip: &Skip) {
    tracing::debug!(
        target: "forge_telegram",
        dialog_id = %dialog_id,
        message_id,
        index = ONLY_INDEX,
        reason = %skip.code(),
        "forge-telegram: voice auto-transcribe skipped"
    );
}

/// Фоновая задача: докачка по MTProto → расшифровка → кадры.
///
/// Именно поэтому она фоновая: файл тянется кусками по 512 КиБ, и держать на
/// этом поток апдейтов означало бы остановить ленту всего аккаунта.
async fn run_one(state: Arc<TelegramState>, key: CacheKey) {
    match crate::routes::voice::begin(&state, &key, false).await {
        Ok(resp) => {
            tracing::info!(
                target: "forge_telegram",
                key = %key.id(),
                engine = %resp.engine,
                ready = resp.ready,
                run_id = %resp.run_id.clone().unwrap_or_default(),
                "forge-telegram: voice auto-transcribe started"
            );
        }
        Err(e) => {
            let code = e.body.code.clone();
            if retryable(&code) {
                state.voice_seen.forget(&voice_key(&key));
            }
            tracing::warn!(
                target: "forge_telegram",
                key = %key.id(),
                code = %code,
                retryable = retryable(&code),
                "forge-telegram: voice auto-transcribe refused"
            );
        }
    }
}

/// Имеет ли смысл пробовать ещё раз. Список ЯВНЫЙ: незнакомый код считается
/// окончательным, иначе получился бы бесконечный цикл повторов.
fn retryable(code: &str) -> bool {
    matches!(
        code,
        "not_connected" | "no_media" | "voice_busy" | "telegram_error" | "internal"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TelegramConfig;
    use crate::dto::MediaDto;

    fn state(dir: &std::path::Path) -> Arc<TelegramState> {
        TelegramState::new(TelegramConfig {
            cache_dir: dir.join("cache"),
            session_path: dir.join("telegram.session"),
            prompts_path: dir.join("prompts.json"),
            user_settings_path: dir.join("user_settings.json"),
            ..TelegramConfig::default()
        })
    }

    fn write_settings(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("user_settings.json"), body).unwrap();
    }

    fn media(kind: &str, duration: Option<i32>) -> MediaDto {
        MediaDto {
            kind: kind.to_string(),
            duration_secs: duration,
            ..MediaDto::default()
        }
    }

    fn message(dialog_id: &str, id: i32, out: bool, media: Option<MediaDto>) -> MessageDto {
        MessageDto {
            id,
            dialog_id: dialog_id.to_string(),
            out,
            media,
            ..MessageDto::default()
        }
    }

    /// Рабочий `voice.toml` рядом с сессией — там же, где его ищет состояние.
    fn write_voice_config(dir: &std::path::Path) {
        std::fs::write(
            dir.join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("meet.toml"), b"").unwrap();
    }

    /// **ОБЫЧНОЕ АУДИО АВТОМАТИЧЕСКИ НЕ РАЗБИРАЕТСЯ, ГОЛОСОВОЕ — ДА.**
    ///
    /// Решение зафиксировано явно: час музыки в облаке стоит денег, а просил бы
    /// его расшифровать человек кнопкой.
    #[test]
    fn only_a_real_voice_note_is_taken_automatically() {
        assert!(is_auto_kind("voice"));
        assert!(is_auto_kind(" VOICE "));
        for kind in ["audio", "video", "document", "photo", "round", ""] {
            assert!(!is_auto_kind(kind), "{kind:?}");
        }
    }

    /// **ТУМБЛЕР ВЫКЛЮЧЕН — НИ ОДНОЙ ЗАДАЧИ И НИ ОДНОГО ЗАНЯТОГО КЛЮЧА.**
    #[tokio::test]
    async fn a_disabled_switch_starts_nothing_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let msg = message("777000", 42, false, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert_eq!(st.voice.active(), 0);
        assert!(st.voice_seen.is_empty());

        write_settings(dir.path(), r#"{"telegram_voice_autotranscribe":false}"#);
        assert_eq!(on_new_message(&st, &msg).await, 0);
    }

    /// **ФЛАГ VK НЕ ВКЛЮЧАЕТ TELEGRAM.**
    ///
    /// Три панели — три решения человека; общий файл настроек их не смешивает.
    #[tokio::test]
    async fn the_vk_flag_does_not_switch_telegram_on() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        write_voice_config(dir.path());
        let st = state(dir.path());
        let msg = message("777000", 42, false, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &msg).await, 0);
    }

    /// **СВОЁ ИСХОДЯЩЕЕ И ОБЫЧНОЕ АУДИО ПРОПУСКАЮТСЯ.**
    #[tokio::test]
    async fn my_own_message_and_plain_audio_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"telegram_voice_autotranscribe":true}"#);
        write_voice_config(dir.path());
        let st = state(dir.path());

        let mine = message("777000", 42, true, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &mine).await, 0);

        let track = message("777000", 43, false, Some(media("audio", Some(3600))));
        assert_eq!(on_new_message(&st, &track).await, 0);

        let text = message("777000", 44, false, None);
        assert_eq!(on_new_message(&st, &text).await, 0);
        assert!(st.voice_seen.is_empty());
    }

    /// **ЧАТ ВНЕ БЕЛОГО СПИСКА НЕ РАЗБИРАЕТСЯ, ОТМЕЧЕННЫЙ ПОИМЁННО —
    /// РАЗБИРАЕТСЯ.**
    #[tokio::test]
    async fn the_folder_whitelist_is_respected_by_name() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(
            dir.path(),
            r#"{"telegram_voice_autotranscribe":true,"telegram_filter_enabled":true,
                "telegram_visible_folders":["inbox"],"telegram_visible_dialogs":["777000"]}"#,
        );
        write_voice_config(dir.path());
        let st = state(dir.path());

        let hidden = message("-1001234", 42, false, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &hidden).await, 0);

        let named = message("777000", 43, false, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &named).await, 1);
    }

    /// **ВКЛЮЧЁННЫЙ ТУМБЛЕР: ЗАДАЧА ОДНА, ПОВТОРНАЯ ДОСТАВКА — НОЛЬ.**
    #[tokio::test]
    async fn an_enabled_switch_starts_one_task_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"telegram_voice_autotranscribe":true}"#);
        write_voice_config(dir.path());
        let st = state(dir.path());

        let msg = message("777000", 42, false, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &msg).await, 1);
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert_eq!(st.voice_seen.len(), 1);
    }

    /// **ГЕЙТ ФАЗЫ: ПРИ ВЫКЛЮЧЕННОМ ТУМБЛЕРЕ ДИСК НЕ ТРОГАЕТСЯ ВООБЩЕ.**
    ///
    /// Не «не запустился движок», а именно НИЧЕГО: ни файла в медиа-кэше (то
    /// есть докачки не было), ни файла в сторе расшифровок (то есть и записи
    /// не было). Проверяется по каталогам, потому что обещание фазы дано
    /// человеку про его трафик и его деньги, а не про внутренние счётчики.
    #[tokio::test]
    async fn the_gate_a_disabled_switch_writes_nothing_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"telegram_voice_autotranscribe":false}"#);
        let st = state(dir.path());
        let msg = message("777000", 42, false, Some(media("voice", Some(12))));
        assert_eq!(on_new_message(&st, &msg).await, 0);

        // Дать фоновой задаче шанс появиться, если бы она была.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(st.voice.active(), 0, "движок не запускался");
        assert!(
            st.voice_store().get(&voice_key(&CacheKey {
                dialog_id: 777_000,
                message_id: 42,
                kind: MediaKind::Full,
            }))
            .is_none(),
            "в сторе ничего не появилось"
        );
        assert_eq!(files_under(&dir.path().join("cache")), 0, "медиа-кэш пуст");
    }

    /// Сколько файлов лежит под каталогом (рекурсивно). Отсутствующий каталог —
    /// это ноль: его отсутствие и есть самое сильное доказательство.
    fn files_under(root: &std::path::Path) -> usize {
        let Ok(entries) = std::fs::read_dir(root) else {
            return 0;
        };
        let mut n = 0;
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                n += files_under(&path);
            } else {
                n += 1;
            }
        }
        n
    }

    /// **ЗАПИСЬ ДЛИННЕЕ ПОТОЛКА ПРОПУСКАЕТСЯ.**
    #[tokio::test]
    async fn a_recording_longer_than_the_limit_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"telegram_voice_autotranscribe":true}"#);
        std::fs::write(
            dir.path().join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n\
             max_duration_secs = 60\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("meet.toml"), b"").unwrap();
        let st = state(dir.path());

        let long = message("777000", 42, false, Some(media("voice", Some(600))));
        assert_eq!(on_new_message(&st, &long).await, 0);
        assert!(st.voice_seen.is_empty());
    }
}
