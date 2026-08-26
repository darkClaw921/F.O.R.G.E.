//! Автоматический разбор новых голосовых сообщений Битрикс24.
//!
//! Близнец `plugins/vk/src/voice_auto.rs`, но с ОДНИМ вынужденным отличием, и
//! оно определяет всё устройство модуля.
//!
//! # СОБЫТИЕ ПОРТАЛА НЕ ЗНАЕТ, ЧТО ВЛОЖЕНИЕ — ГОЛОСОВОЕ
//!
//! `im.v2.Event.*` приносит `params.FILE_ID` (то есть КОЛИЧЕСТВО вложений и их
//! номера), но не приносит дескрипторы файлов: ни имени, ни типа, ни признака
//! `isVoiceNote`. В проекции сообщения из события у каждого вложения поэтому
//! `kind == "file"` (`plugins/bitrix/src/events.rs` зовёт `message_from_raw` с
//! пустой картой файлов). Отличить голосовое от таблицы Excel прямо в событии
//! НЕВОЗМОЖНО.
//!
//! Отсюда порядок:
//!
//! ```text
//! есть ли вложения (в памяти)  ─▶ тумблер, своё исходящее, видимость
//!    ─▶ фоновая задача: ОДНА страница истории ради дескрипторов
//!    ─▶ только вид `voice` ─▶ докачка ─▶ движок ─▶ WS-кадры
//! ```
//!
//! Страница истории (`load_page`, `LIMIT=1`) стоит ОДНОГО вызова портала при
//! бюджете 2 rps и делается ТОЛЬКО когда тумблер включён и сообщение прошло
//! остальные проверки. Скачивать вложение вслепую, чтобы потом узнать по MIME,
//! что это ZIP на сто мегабайт, было бы куда дороже.
//!
//! # ЛИМИТ ПОРТАЛА РВЁТ TCP ВМЕСТО ОШИБКИ
//!
//! Известная особенность платформы (см. `plugins/bitrix/src/api`). Поэтому
//! отказ любой ступени здесь — это запись в журнал и выход, а не паника и не
//! остановка воркера: следующее сообщение обрабатывается как ни в чём не
//! бывало. Отказы, которые имеет смысл повторить (оборванная докачка, троттлинг
//! портала), СНИМАЮТ пометку «уже брали», и повторная доставка события пробует
//! снова; окончательные (не голосовое, нет ключа) — оставляют.
//!
//! # ВИД ДИАЛОГА ДЛЯ БЕЛОГО СПИСКА
//!
//! Выводится из `DIALOG_ID`: число — личный диалог (`user`), всё остальное
//! (`chat27`, `sg12`) — `chat`. Чат ЗАДАЧИ по одному идентификатору не
//! отличить: его признак — `entity_type`, который живёт в списке диалогов, а не
//! в id ([`crate::dto::dialog_kind`]). Практическое следствие: белый список,
//! собранный ТОЛЬКО из вида `task`, авторазбор в чатах задач не включит —
//! отметьте такие чаты поимённо. Сторона выбрана в пользу «не разобрать»: она
//! безопасная.

use std::sync::Arc;

use forge_voice::auto::{Candidate, Skip};
use forge_voice::prefs::Prefs;

use crate::attach::media_kind;
use crate::dto::MessageDto;
use crate::media_cache::{CacheKey, DialogKey};
use crate::state::B24State;
use crate::voice::voice_key;

/// Настройки панели Битрикс24 из `user_settings.json` — читаются на каждое
/// сообщение с вложением, чтобы тумблер действовал без перезапуска.
#[must_use]
pub fn prefs(state: &B24State) -> Prefs {
    forge_voice::prefs::read(&state.config.user_settings_path, forge_voice::prefs::BITRIX)
}

/// Вид диалога в лексике настроек видимости (`user` | `chat`).
///
/// См. шапку модуля: `task` из одного `DIALOG_ID` не выводится.
#[must_use]
pub fn visibility_kind(dialog_id: &str) -> &'static str {
    if dialog_id.trim().parse::<i64>().is_ok() {
        "user"
    } else {
        "chat"
    }
}

/// Обрабатывает новое сообщение из потока событий.
///
/// Возвращает число запущенных фоновых задач (ноль или одна на сообщение).
/// Ноль — значит не произошло ничего: ни вызова портала, ни докачки, ни
/// движка.
pub async fn on_new_message(state: &Arc<B24State>, msg: &MessageDto) -> usize {
    // Дешёвая отсечка: вложений нет — дальше идти незачем. Номера вложений в
    // событии ЕСТЬ (они из `params.FILE_ID`), а видов нет.
    if msg.media.is_empty() {
        return 0;
    }
    let Some(dialog) = DialogKey::parse(&msg.dialog_id) else {
        return 0;
    };

    let prefs = prefs(state);
    // Вид вложения на этом шаге неизвестен, поэтому лестнице подаётся заведомо
    // голосовой: нас интересуют ровно те ступени, которые НЕ зависят от вида, —
    // тумблер, своё исходящее, видимость. Настоящий вид проверяется в задаче,
    // после единственного вызова портала.
    // Группа у Битрикса ровно одна — вид диалога.
    let groups = [visibility_kind(&msg.dialog_id)];
    let cand = Candidate {
        kind: media_kind::VOICE,
        outgoing: msg.out,
        // Длительности портал не отдаёт НИКОГДА — потолок ловится уже по
        // декодированному звуку в `forge_voice::audio::prepare`.
        duration_secs: None,
        peer: &msg.dialog_id,
        peer_groups: &groups,
    };
    if let Err(skip) = forge_voice::auto::decide(&prefs, &cand, 0) {
        log_skip(&msg.dialog_id, msg.id, &skip);
        return 0;
    }

    // Ключи занимаются ЗДЕСЬ, до похода в портал: иначе повторная доставка
    // того же события, пришедшая, пока первая задача ждёт ответа, сходила бы за
    // страницей истории второй раз — и это при бюджете 2 rps.
    let mut claimed: Vec<CacheKey> = Vec::new();
    for media in &msg.media {
        let key = CacheKey::File {
            dialog,
            message_id: msg.id,
            index: media.index,
        };
        if voice_key(&key).is_some_and(|vk| state.voice_seen.claim(&vk)) {
            claimed.push(key);
        }
    }
    if claimed.is_empty() {
        tracing::trace!(
            target: "forge_bitrix",
            dialog_id = %msg.dialog_id,
            message_id = msg.id,
            "forge-bitrix: voice auto-transcribe already claimed"
        );
        return 0;
    }

    let st = state.clone();
    let dialog_id = msg.dialog_id.clone();
    let message_id = msg.id;
    let handle = tokio::spawn(async move { run_message(st, dialog_id, message_id, claimed).await });
    state.register_worker(handle).await;
    1
}

/// Печатает причину пропуска (`debug`: у выключенного тумблера это строка на
/// каждое сообщение с вложением).
fn log_skip(dialog_id: &str, message_id: i64, skip: &Skip) {
    tracing::debug!(
        target: "forge_bitrix",
        dialog_id = %dialog_id,
        message_id,
        reason = %skip.code(),
        "forge-bitrix: voice auto-transcribe skipped"
    );
}

/// Фоновая задача одного сообщения: дескрипторы → голосовые → расшифровка.
async fn run_message(
    state: Arc<B24State>,
    dialog_id: String,
    message_id: i64,
    claimed: Vec<CacheKey>,
) {
    let voices = match voice_indexes(&state, &dialog_id, message_id).await {
        Ok(v) => v,
        Err(code) => {
            // Портал не ответил — снимаем пометки: следующая доставка события
            // имеет право попробовать снова. Воркер при этом жив.
            for key in &claimed {
                if let Some(vk) = voice_key(key) {
                    state.voice_seen.forget(&vk);
                }
            }
            tracing::warn!(
                target: "forge_bitrix",
                dialog_id = %dialog_id,
                message_id,
                code = %code,
                "forge-bitrix: voice auto-transcribe could not read the message"
            );
            return;
        }
    };

    for key in claimed {
        let CacheKey::File { index, .. } = key else {
            continue;
        };
        if !voices.contains(&index) {
            // Не голосовое. Пометка ОСТАЁТСЯ: вид вложения сам не изменится, и
            // второй раз за страницей истории ходить незачем.
            continue;
        }
        match crate::routes::voice::begin(&state, &key, false).await {
            Ok(resp) => tracing::info!(
                target: "forge_bitrix",
                dialog_id = %dialog_id,
                message_id,
                index,
                engine = %resp.engine,
                ready = resp.ready,
                "forge-bitrix: voice auto-transcribe started"
            ),
            Err(e) => {
                let code = e.body.code.clone();
                if retryable(&code) {
                    if let Some(vk) = voice_key(&key) {
                        state.voice_seen.forget(&vk);
                    }
                }
                tracing::warn!(
                    target: "forge_bitrix",
                    dialog_id = %dialog_id,
                    message_id,
                    index,
                    code = %code,
                    retryable = retryable(&code),
                    "forge-bitrix: voice auto-transcribe refused"
                );
            }
        }
    }
}

/// Номера ГОЛОСОВЫХ вложений сообщения — один вызов портала.
///
/// Лестница та же, что у `GET /api/bitrix/messages/:mid`: метода «дай сообщение
/// по id» у портала нет, поэтому берётся страница `LIMIT = 1`,
/// `LAST_ID = mid + 1`.
///
/// # Errors
///
/// Код отказа строкой — его достаточно журналу и решению «повторять ли».
async fn voice_indexes(
    state: &Arc<B24State>,
    dialog_id: &str,
    message_id: i64,
) -> Result<Vec<u32>, String> {
    let client = crate::routes::dialogs::require_client(state)
        .await
        .map_err(|e| e.body.code.clone())?;
    let page = crate::routes::dialogs::load_page(
        state,
        &client,
        dialog_id,
        1,
        Some(message_id.saturating_add(1)),
        None,
    )
    .await
    .map_err(|e| e.body.code.clone())?;

    let message = page
        .into_iter()
        .find(|m| m.id == message_id)
        .ok_or_else(|| "unknown_message".to_string())?;
    Ok(message
        .media
        .iter()
        .filter(|m| m.kind == media_kind::VOICE)
        .map(|m| m.index)
        .collect())
}

/// Имеет ли смысл пробовать ещё раз.
///
/// Список ЯВНЫЙ, и в нём есть `flood_wait`: портал под лимитом рвёт соединение,
/// и такой отказ ничего не говорит о самом вложении. Незнакомый код считается
/// окончательным — иначе получился бы бесконечный цикл повторов с оплатой
/// каждого, если движок облачный.
fn retryable(code: &str) -> bool {
    matches!(
        code,
        "not_connected"
            | "no_media"
            | "voice_busy"
            | "flood_wait"
            | "bitrix_error"
            | "unknown_message"
            | "internal"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::MediaDto;
    use crate::config::BitrixConfig;

    fn state(dir: &std::path::Path) -> Arc<B24State> {
        B24State::new(BitrixConfig {
            cache_dir: dir.join("cache"),
            config_path: dir.join("bitrix.toml"),
            prompts_path: dir.join("prompts.json"),
            user_settings_path: dir.join("user_settings.json"),
            ..BitrixConfig::default()
        })
    }

    fn write_settings(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("user_settings.json"), body).unwrap();
    }

    fn write_voice_config(dir: &std::path::Path) {
        std::fs::write(
            dir.join("voice.toml"),
            "engine = \"cloud\"\npolza_api_key = \"pza_TESTnotArealKeyJustAFixture\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("meet.toml"), b"").unwrap();
    }

    /// Вложение ТАК, КАК ОНО ПРИХОДИТ В СОБЫТИИ: номер есть, вид — `file`.
    fn attachment(index: u32) -> MediaDto {
        MediaDto {
            index,
            kind: media_kind::FILE.to_string(),
            ..MediaDto::default()
        }
    }

    fn message(dialog_id: &str, id: i64, out: bool, media: Vec<MediaDto>) -> MessageDto {
        MessageDto {
            id,
            dialog_id: dialog_id.to_string(),
            out,
            media,
            ..MessageDto::default()
        }
    }

    /// Вид диалога для белого списка выводится из `DIALOG_ID`.
    #[test]
    fn the_dialog_kind_comes_from_the_id() {
        assert_eq!(visibility_kind("19"), "user");
        assert_eq!(visibility_kind("chat27"), "chat");
        assert_eq!(visibility_kind("sg12"), "chat");
    }

    /// **ТУМБЛЕР ВЫКЛЮЧЕН — НИ ОДНОГО ВЫЗОВА ПОРТАЛА И НИ ОДНОГО ЗАНЯТОГО
    /// КЛЮЧА.**
    ///
    /// Адрес портала в конфиге теста заведомо мёртвый: сходи модуль за
    /// страницей истории — тест бы этого не заметил, зато заметил бы занятый
    /// ключ и запущенную задачу. Их нет.
    #[tokio::test]
    async fn a_disabled_switch_starts_nothing_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let msg = message("chat27", 1000, false, vec![attachment(0)]);
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert_eq!(st.voice.active(), 0);
        assert!(st.voice_seen.is_empty());

        write_settings(dir.path(), r#"{"bitrix_voice_autotranscribe":false}"#);
        assert_eq!(on_new_message(&st, &msg).await, 0);
        assert!(st.voice_seen.is_empty());
    }

    /// **СООБЩЕНИЕ БЕЗ ВЛОЖЕНИЙ НЕ ДОХОДИТ ДАЖЕ ДО ФАЙЛА НАСТРОЕК.**
    #[tokio::test]
    async fn a_message_without_attachments_is_free() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"bitrix_voice_autotranscribe":true}"#);
        write_voice_config(dir.path());
        let st = state(dir.path());
        let text = message("chat27", 1000, false, Vec::new());
        assert_eq!(on_new_message(&st, &text).await, 0);
    }

    /// **СВОЁ ИСХОДЯЩЕЕ НЕ РАЗБИРАЕТСЯ.**
    #[tokio::test]
    async fn my_own_message_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"bitrix_voice_autotranscribe":true}"#);
        write_voice_config(dir.path());
        let st = state(dir.path());
        let mine = message("chat27", 1000, true, vec![attachment(0)]);
        assert_eq!(on_new_message(&st, &mine).await, 0);
        assert!(st.voice_seen.is_empty());
    }

    /// **ДИАЛОГ ВНЕ БЕЛОГО СПИСКА НЕ РАЗБИРАЕТСЯ, ОТМЕЧЕННЫЙ ПОИМЁННО —
    /// РАЗБИРАЕТСЯ.**
    #[tokio::test]
    async fn the_whitelist_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(
            dir.path(),
            r#"{"bitrix_voice_autotranscribe":true,"bitrix_filter_enabled":true,
                "bitrix_visible_kinds":["user"],"bitrix_visible_peers":["chat9"]}"#,
        );
        write_voice_config(dir.path());
        let st = state(dir.path());

        let hidden = message("chat27", 1000, false, vec![attachment(0)]);
        assert_eq!(on_new_message(&st, &hidden).await, 0);
        assert!(st.voice_seen.is_empty());

        let named = message("chat9", 1002, false, vec![attachment(0)]);
        assert_eq!(on_new_message(&st, &named).await, 1);
    }

    /// **ВКЛЮЧЁННЫЙ ТУМБЛЕР: ЗАДАЧА ОДНА НА СООБЩЕНИЕ, ПОВТОРНАЯ ДОСТАВКА —
    /// НОЛЬ.**
    ///
    /// Ключи занимаются на ВСЕ вложения сообщения сразу: одна страница истории
    /// отвечает про все из них, и второй заход за ней при бюджете 2 rps —
    /// непозволительная роскошь.
    #[tokio::test]
    async fn an_enabled_switch_claims_every_attachment_once() {
        let dir = tempfile::tempdir().unwrap();
        write_settings(dir.path(), r#"{"bitrix_voice_autotranscribe":true}"#);
        write_voice_config(dir.path());
        let st = state(dir.path());

        let msg = message("chat27", 1000, false, vec![attachment(0), attachment(1)]);
        assert_eq!(on_new_message(&st, &msg).await, 1);
        assert_eq!(st.voice_seen.len(), 2, "по ключу на вложение");
        assert_eq!(on_new_message(&st, &msg).await, 0, "повторная доставка события");
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
        write_settings(dir.path(), r#"{"bitrix_voice_autotranscribe":false}"#);
        let st = state(dir.path());
        let msg = message("chat27", 1000, false, vec![attachment(0)]);
        assert_eq!(on_new_message(&st, &msg).await, 0);

        // Дать фоновой задаче шанс появиться, если бы она была.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(st.voice.active(), 0, "движок не запускался");
        assert!(
            st.voice_store()
                .get(&voice_key(&CacheKey::File {
                    dialog: DialogKey::parse("chat27").unwrap(),
                    message_id: 1000,
                    index: 0,
                })
                .unwrap())
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

    /// **ОБРЫВ СВЯЗИ С ПОРТАЛОМ ПОВТОРЯЕМ, «НЕ ГОЛОСОВОЕ» — НЕТ.**
    ///
    /// Список повторяемых кодов явный: незнакомый отказ считается
    /// окончательным, иначе на каждом переподключении получался бы новый круг
    /// оплаченных прогонов.
    #[test]
    fn only_a_listed_failure_is_worth_a_second_try() {
        assert!(retryable("flood_wait"));
        assert!(retryable("not_connected"));
        assert!(retryable("unknown_message"));
        assert!(!retryable("voice_not_audio"));
        assert!(!retryable("voice_no_key"));
        assert!(!retryable("что-то новое"));
    }
}
