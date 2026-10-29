//! Автоматический разбор новых голосовых сообщений VK.
//!
//! Точка входа одна — [`on_new_message`], её зовёт лонгпол на каждом кадре
//! `new_message` (`plugins/vk/src/longpoll.rs`). Дальше:
//!
//! ```text
//! есть ли голосовое вложение (в памяти)
//!    ─▶ тумблер и белый список (forge_voice::auto::decide)
//!    ─▶ ключ ещё не брали (SeenKeys)
//!    ─▶ фоновая задача: докачка ─▶ реестр расшифровок ─▶ WS-кадры
//! ```
//!
//! # ПРИ ВЫКЛЮЧЕННОМ ТУМБЛЕРЕ НЕ ПРОИСХОДИТ НИЧЕГО
//!
//! Это обещание фазы, и держится оно порядком: докачка ([`obtain_media`] внутри
//! [`crate::routes::voice::begin`]) и запуск движка стоят ПОСЛЕ решения
//! [`forge_voice::auto::decide`], а само решение начинается с тумблера. У
//! сообщения без голосового вложения дело не доходит даже до чтения настроек:
//! вложения уже в памяти, и проверка их вида бесплатна.
//!
//! # ПОЧЕМУ ФОНОВАЯ ЗАДАЧА, А НЕ ПРЯМО В ЛОНГПОЛЕ
//!
//! Докачка голосового — это сеть и диск, а лонгпол в это время держит очередь
//! событий всего аккаунта. Ждать в нём означало бы задерживать ленту у всех
//! бесед ради одного вложения. Поэтому решение принимается синхронно (оно
//! дешёвое), а работа уезжает в [`tokio::spawn`]; потолок параллелизма при этом
//! общий с кнопкой «расшифровать» — он живёт в [`forge_voice::VoiceHub`].
//!
//! # ЧТО СЧИТАЕТСЯ «ВИДИМОЙ БЕСЕДОЙ»
//!
//! Ровно то же, что во фронте (`static/js/vk/visibility.js`): поимённый выбор
//! сильнее вида, вид выводится из самого `peer_id`. Единственная тонкость —
//! СЛОВАРЬ: бэкенд называет сообщество `group` ([`crate::dto::peer_kind`]), а
//! настройки видимости — `community`. Перевод живёт в [`visibility_kind`] и
//! проверен тестом; без него белый список «только сообщества» молча перестал бы
//! действовать на авторазборе.

use std::sync::Arc;

use forge_voice::auto::{Candidate, Skip};
use forge_voice::prefs::Prefs;

use crate::dto::{MediaDto, MessageDto};
use crate::media_cache::{CacheKey, MediaKind};
use crate::state::VkState;
use crate::voice::voice_key;

/// Настройки панели VK из `user_settings.json`.
///
/// Файл читается на КАЖДОЕ голосовое — см. [`forge_voice::prefs`]: тумблер,
/// переключённый в браузере, обязан действовать со следующего сообщения, а не
/// после перезапуска devforge.
#[must_use]
pub fn prefs(state: &VkState) -> Prefs {
    forge_voice::prefs::read(&state.config.user_settings_path, forge_voice::prefs::VK)
}

/// Вид беседы В ЛЕКСИКЕ НАСТРОЕК ВИДИМОСТИ (`user` | `chat` | `community`).
///
/// Перевод из лексики плагина ([`crate::dto::peer_kind`], где сообщество —
/// `group`) в лексику формы настроек. Совпадает с фронтовым `peerKind`
/// (`static/js/vk/format.js`), и это проверено тестом: разъехавшиеся словари
/// означали бы, что беседа видна в панели, но её голосовые не разбираются.
#[must_use]
pub fn visibility_kind(peer_id: i64) -> &'static str {
    match crate::dto::peer_kind(peer_id) {
        "group" => "community",
        other => other,
    }
}

/// Голосовые вложения сообщения (обычно ноль или одно).
fn voice_attachments(msg: &MessageDto) -> Vec<&MediaDto> {
    msg.attachments
        .iter()
        .filter(|a| forge_voice::gate::is_voice_kind(&a.kind))
        .collect()
}

/// Обрабатывает новое сообщение из лонгпола.
///
/// Синхронной работы — только разбор уже полученного DTO; всё дорогое уезжает
/// в фон. Возвращает число ЗАПУЩЕННЫХ задач (нужно тестам: «ноль» — это и есть
/// «ничего не произошло»).
pub async fn on_new_message(state: &Arc<VkState>, msg: &MessageDto) -> usize {
    // Дешёвая отсечка: у подавляющего большинства сообщений вложений нет
    // вовсе, и ради них незачем открывать файл настроек.
    let candidates = voice_attachments(msg);
    if candidates.is_empty() {
        return 0;
    }
    let Some(peer_id) = crate::dto::parse_dialog_id(&msg.dialog_id) else {
        return 0;
    };

    let prefs = prefs(state);
    // Потолок длительности живёт в настройках голосовых, а не в настройках
    // панели; читаем их ТОЛЬКО когда тумблер включён — иначе выключенная фича
    // трогала бы ещё и `voice.toml`.
    let max_duration_secs = if prefs.autotranscribe {
        state
            .voice_config()
            .map(|c| c.max_duration_secs)
            .unwrap_or(forge_voice::config::DEFAULT_MAX_DURATION_SECS)
    } else {
        0
    };

    // Группа у VK ровно одна — вид беседы; срез собирается один раз на
    // сообщение, а не на вложение.
    let groups = [visibility_kind(peer_id)];

    let mut started = 0usize;
    for att in candidates {
        let cand = Candidate {
            kind: &att.kind,
            outgoing: msg.out,
            duration_secs: att.duration_secs.map(i64::from),
            peer: &msg.dialog_id,
            peer_groups: &groups,
        };
        match forge_voice::auto::decide(&prefs, &cand, max_duration_secs) {
            Ok(()) => {}
            Err(skip) => {
                log_skip(&msg.dialog_id, msg.id, att.index, &skip);
                continue;
            }
        }

        let key = CacheKey {
            peer_id,
            message_id: msg.id,
            index: att.index,
            kind: MediaKind::Full,
        };
        // Дедуп по ключу вложения: повторная доставка события (переподключение
        // лонгпола, догон истории) не имеет права запустить второй прогон.
        if !state.voice_seen.claim(&voice_key(&key)) {
            tracing::trace!(
                target: "forge_vk",
                key = %key.id(),
                "forge-vk: voice auto-transcribe already claimed"
            );
            continue;
        }

        let st = state.clone();
        let handle = tokio::spawn(async move { run_one(st, key).await });
        state.register_worker(handle).await;
        started += 1;
    }
    started
}

/// Печатает причину пропуска.
///
/// `debug`, а не `info`: у выключенного тумблера это была бы строка на КАЖДОЕ
/// входящее голосовое. Причина при этом называется словом — «почему ничего не
/// расшифровалось» первый вопрос о фоновой фиче.
fn log_skip(dialog_id: &str, message_id: i32, index: u8, skip: &Skip) {
    tracing::debug!(
        target: "forge_vk",
        dialog_id = %dialog_id,
        message_id,
        index,
        reason = %skip.code(),
        "forge-vk: voice auto-transcribe skipped"
    );
}

/// Фоновая задача одного вложения: докачка → расшифровка → кадры.
///
/// Ошибки НЕ роняют ни задачу-соседа, ни лонгпол: их место — журнал. Отказ по
/// внешней причине (сеть, портал) СНИМАЕТ пометку «уже брали», и следующая
/// доставка того же события имеет право попробовать снова; отказ по причине,
/// которая сама не пройдёт (не голосовое, слишком длинное), пометку оставляет.
async fn run_one(state: Arc<VkState>, key: CacheKey) {
    match crate::routes::voice::begin(&state, &key, false).await {
        Ok(resp) => {
            tracing::info!(
                target: "forge_vk",
                key = %key.id(),
                engine = %resp.engine,
                ready = resp.ready,
                run_id = %resp.run_id.clone().unwrap_or_default(),
                "forge-vk: voice auto-transcribe started"
            );
        }
        Err(e) => {
            let code = e.body.code.clone();
            if retryable(&code) {
                state.voice_seen.forget(&voice_key(&key));
            }
            tracing::warn!(
                target: "forge_vk",
                key = %key.id(),
                code = %code,
                retryable = retryable(&code),
                "forge-vk: voice auto-transcribe refused"
            );
        }
    }
}

/// Имеет ли смысл пробовать это вложение ещё раз.
///
/// Список ЯВНЫЙ, а не «всё, кроме...»: новый код отказа должен по умолчанию
/// считаться окончательным. Иначе неизвестная ошибка превратилась бы в
/// бесконечный цикл повторов на каждом переподключении лонгпола — с оплатой
/// каждого повтора, если движок облачный.
fn retryable(code: &str) -> bool {
    matches!(
        code,
        "not_connected" | "no_media" | "voice_busy" | "vk_error" | "internal"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SecretToken, VkConfig};
    use crate::dto::MediaDto;

    fn state(dir: &std::path::Path) -> Arc<VkState> {
        VkState::new(VkConfig {
            access_token: SecretToken::new("vk1.a.TEST"),
            base_url: "http://127.0.0.1:1/method".into(),
            cache_dir: dir.join("cache"),
            config_path: dir.join("vk.toml"),
            prompts_path: dir.join("prompts.json"),
            user_settings_path: dir.join("user_settings.json"),
            ..VkConfig::default()
        })
    }

    /// Настройки панели на диске (их пишет хост при `PATCH /api/user-settings`).
    fn write_settings(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("user_settings.json"), body).unwrap();
    }

    fn voice_media(index: u8, duration: Option<i32>) -> MediaDto {
        MediaDto {
            index,
            kind: "voice".into(),
            mime: Some("audio/ogg".into()),
            duration_secs: duration,
            ..MediaDto::default()
        }
    }

    fn message(dialog_id: &str, id: i32, out: bool, media: Vec<MediaDto>) -> MessageDto {
        MessageDto {
            id,
            dialog_id: dialog_id.to_string(),
            out,
            attachments: media,
            ..MessageDto::default()
        }
    }

    /// **СЛОВАРЬ ВИДОВ СОВПАДАЕТ С ФОРМОЙ НАСТРОЕК.**
    ///
    /// `community`, а не `group`: в `vk_visible_kinds` лежит слово фронта.
    #[test]
    fn the_kind_dictionary_matches_the_settings_form() {
        assert_eq!(visibility_kind(2_000_000_042), "chat");
        assert_eq!(visibility_kind(-42), "community");
        assert_eq!(visibility_kind(777), "user");
    }

    /// **ТУМБЛЕР ВЫКЛЮЧЕН — НИ ОДНОЙ ЗАДАЧИ.**
    ///
    /// Сеть в этом состоянии заведомо мертва, а конфига голосовых нет вовсе:
    /// любая попытка докачки или запуска движка проявилась бы либо задачей,
    /// либо занятым слотом реестра.
    #[tokio::test]
    async fn a_disabled_switch_starts_nothing_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        // Файла настроек нет вовсе — самая обычная нулевая конфигурация.
        let msg = message("2000000042", 7001, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert_eq!(st.voice.active(), 0);
        assert!(st.voice_seen.is_empty(), "ключ даже не занимался");

        // И с файлом, где флаг выключен явно.
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":false}"#);
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert_eq!(st.voice.active(), 0);
    }

    /// **СВОЁ ИСХОДЯЩЕЕ ГОЛОСОВОЕ НЕ РАЗБИРАЕТСЯ.**
    #[tokio::test]
    async fn my_own_voice_message_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        let st = state(dir.path());
        let msg = message("2000000042", 7001, true, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert!(st.voice_seen.is_empty());
    }

    /// **НЕ ГОЛОСОВОЕ ВЛОЖЕНИЕ И ТЕКСТ БЕЗ ВЛОЖЕНИЙ ПРОХОДЯТ МИМО.**
    #[tokio::test]
    async fn a_plain_message_never_reaches_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        let st = state(dir.path());

        let text = message("2000000042", 7001, false, Vec::new());
        assert_eq!(on_new_message(&st, &text).await, 0);

        let photo = MediaDto {
            index: 0,
            kind: "photo".into(),
            ..MediaDto::default()
        };
        let with_photo = message("2000000042", 7002, false, vec![photo]);
        assert_eq!(on_new_message(&st, &with_photo).await, 0);
    }

    /// **ЗАПИСЬ ДЛИННЕЕ ПОТОЛКА ПРОПУСКАЕТСЯ.**
    ///
    /// Потолок берётся из `voice.toml` рядом с конфигом плагина — в тесте это
    /// временный каталог, а не настройки живого пользователя.
    #[tokio::test]
    async fn a_recording_longer_than_the_limit_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        std::fs::write(
            dir.path().join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n\
             max_duration_secs = 60\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("meet.toml"), b"").unwrap();
        let st = state(dir.path());

        let long = message("2000000042", 7001, false, vec![voice_media(0, Some(600))]);
        assert_eq!(on_new_message(&st, &long).await, 0);
        assert!(st.voice_seen.is_empty());
    }

    /// **БЕСЕДА ВНЕ БЕЛОГО СПИСКА НЕ РАЗБИРАЕТСЯ ДАЖЕ ПРИ ВКЛЮЧЁННОМ
    /// ТУМБЛЕРЕ.**
    #[tokio::test]
    async fn a_hidden_peer_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(
            dir.path(),
            r#"{"vk_voice_autotranscribe":true,"vk_filter_enabled":true,
                "vk_visible_kinds":["user"],"vk_visible_peers":["777"]}"#,
        );
        let st = state(dir.path());
        let hidden = message("2000000042", 7001, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &hidden).await, 0);
        assert!(st.voice_seen.is_empty());
    }

    /// **ВКЛЮЧЁННЫЙ ТУМБЛЕР: ЗАДАЧА ЗАПУСКАЕТСЯ, А ПОВТОРНАЯ ДОСТАВКА ТОГО ЖЕ
    /// СОБЫТИЯ — НЕТ.**
    ///
    /// Сеть мертва, поэтому докачка отказом и закончится; проверяется здесь
    /// ровно решение и дедуп, а не сам прогон (он проверен на роуте).
    #[tokio::test]
    async fn an_enabled_switch_starts_one_task_per_attachment_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        std::fs::write(
            dir.path().join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("meet.toml"), b"").unwrap();
        let st = state(dir.path());

        let msg = message("2000000042", 7001, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &msg).await, 1);
        assert_eq!(on_new_message(&st, &msg).await, 0, "повторная доставка события");
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
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":false}"#);
        let st = state(dir.path());
        let msg = message("2000000042", 7001, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &msg).await, 0);

        // Дать фоновой задаче шанс появиться, если бы она была.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(st.voice.active(), 0, "движок не запускался");
        assert!(
            st.voice_store().get(&voice_key(&CacheKey {
                peer_id: 2_000_000_042,
                message_id: 7001,
                index: 0,
                kind: MediaKind::Full,
            }))
            .is_none(),
            "в сторе ничего не появилось"
        );
        assert_eq!(files_under(&dir.path().join("cache")), 0, "медиа-кэш пуст");
    }

    /// **ГЕЙТ ФАЗЫ: БЕСЕДА ВНЕ СКОУПА НЕ ВЫЗЫВАЕТ НИ ДОКАЧКИ, НИ ДВИЖКА.**
    ///
    /// Проверяется тем же способом, что и выключенный тумблер, и по той же
    /// причине: обещание дано человеку про его трафик и его деньги. Отказ
    /// принимается ДО `routes::voice::begin`, поэтому ни задачи, ни файла в
    /// медиа-кэше, ни записи в сторе не появляется. Соседняя половина теста
    /// показывает, что выбранная беседа при тех же настройках разбирается, —
    /// иначе «ноль задач» доказывал бы лишь то, что фича сломана целиком.
    #[tokio::test]
    async fn the_gate_a_peer_outside_the_scope_downloads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(
            dir.path(),
            r#"{"vk_voice_autotranscribe":true,"vk_voice_scope_peers":["777"]}"#,
        );
        std::fs::write(
            dir.path().join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("meet.toml"), b"").unwrap();
        let st = state(dir.path());

        let foreign = message("2000000042", 7001, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &foreign).await, 0);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(st.voice.active(), 0, "движок не запускался");
        assert!(st.voice_seen.is_empty(), "ключ даже не занимался");
        assert!(
            st.voice_store()
                .get(&voice_key(&CacheKey {
                    peer_id: 2_000_000_042,
                    message_id: 7001,
                    index: 0,
                    kind: MediaKind::Full,
                }))
                .is_none(),
            "в сторе ничего не появилось"
        );
        assert_eq!(files_under(&dir.path().join("cache")), 0, "медиа-кэш пуст");

        // А выбранная беседа при тех же настройках — разбирается.
        let chosen = message("777", 7002, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &chosen).await, 1);
    }

    /// **ГЕЙТ ФАЗЫ: ПУСТОЙ СКОУП НЕ МЕНЯЕТ ПОВЕДЕНИЕ СУЩЕСТВУЮЩИХ УСТАНОВОК.**
    ///
    /// Ровно та конфигурация, что лежит на диске у человека, обновившего
    /// devforge: тумблер включён, полей скоупа в файле нет вовсе. Разбираться
    /// обязано всё, что разбиралось до фазы, — в беседе, в личных и в
    /// сообществе.
    #[tokio::test]
    async fn the_gate_an_empty_scope_keeps_the_old_behaviour() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"vk_voice_autotranscribe":true}"#);
        std::fs::write(
            dir.path().join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("meet.toml"), b"").unwrap();
        let st = state(dir.path());

        for (n, dialog) in ["2000000042", "777", "-42"].iter().enumerate() {
            let msg = message(dialog, 8000 + n as i32, false, vec![voice_media(0, Some(12))]);
            assert_eq!(on_new_message(&st, &msg).await, 1, "{dialog}");
        }

        // И пустые списки в файле читаются так же, как их отсутствие.
        write_settings(
            dir.path(),
            r#"{"vk_voice_autotranscribe":true,"vk_voice_scope_kinds":[],
                "vk_voice_scope_peers":[]}"#,
        );
        let msg = message("2000000099", 8100, false, vec![voice_media(0, Some(12))]);
        assert_eq!(on_new_message(&st, &msg).await, 1);
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

    /// Список повторяемых отказов — ЯВНЫЙ: незнакомый код считается
    /// окончательным, иначе получился бы бесконечный цикл повторов.
    #[test]
    fn only_a_listed_failure_is_worth_a_second_try() {
        assert!(retryable("not_connected"));
        assert!(retryable("no_media"));
        assert!(!retryable("voice_no_key"));
        assert!(!retryable("voice_too_long"));
        assert!(!retryable("voice_not_audio"));
        assert!(!retryable("что-то новое"));
    }
}
