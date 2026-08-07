//! Фолбэк-синхронизация: медленный воркер списка, счётчиков и присутствия.
//!
//! ```text
//!  раз в recent_sync_secs (или по побудке)
//!            │
//!            ▼
//!   batch { im.recent.get {LAST_SYNC_DATE}, im.counters.get }   ← ОДИН хит лимита
//!            │
//!            ├─ строки списка ──→ dialogs_patch
//!            ├─ тот же message.id, другой текст ──→ message_update  (ЧУЖАЯ ПРАВКА)
//!            ├─ изменилось присутствие ──→ presence
//!            └─ изменились счётчики ──→ counters
//! ```
//!
//! # Зачем он нужен, если есть поток событий
//!
//! Поток событий (`im.v2.Event.*`, [`crate::events`]) не покрывает четыре вещи,
//! и ни одна из них не может подождать до перезагрузки вкладки:
//!
//! | что | почему поток не даёт |
//! |---|---|
//! | **правки чужих сообщений** | `ONIMV2MESSAGEUPDATE` документирован, но живьём НЕ ПРИХОДИТ |
//! | счётчики непрочитанного | событие не несёт счётчиков вовсе |
//! | состав и порядок списка | у `ONIMV2JOINCHAT` нет строки списка, у переименования чата — своего события |
//! | онлайн/оффлайн | realtime-присутствия платформа вебхуку не даёт (§6) |
//!
//! Отсюда прямое следствие для пользователя: **чужая правка появляется в ленте
//! с задержкой до `recent_sync_secs`** (боевое значение — минута). Это
//! поведение по проекту, а не баг; ускорить его нечем, кроме опроса истории
//! каждого открытого диалога, что стоило бы всего бюджета в 2 rps.
//!
//! # Как ловится правка
//!
//! У строки списка есть последнее сообщение — его идентификатор и текст. Если
//! идентификатор ТОТ ЖЕ, а текст ДРУГОЙ, то сообщение отредактировали:
//! новое сообщение получило бы новый идентификатор. Память для сравнения —
//! [`crate::state::DialogSeen`].
//!
//! Ограничение честное и его надо знать: так ловится правка ПОСЛЕДНЕГО
//! сообщения диалога. Правка сообщения в середине истории не двигает строку
//! списка вовсе, и увидеть её можно только перечитав ленту.
//!
//! # `LAST_SYNC_DATE` (Р21)
//!
//! С этим параметром портал отдаёт только изменившиеся строки — на порядок
//! дешевле полного списка. Два правила:
//!
//! 1. **Дата не старше семи дней.** Ограничение платформы: с более старой ответ
//!    бесполезен, поэтому после долгого простоя делается ПОЛНАЯ перезагрузка
//!    списка.
//! 2. **Дата уезжает с перекрытием** в [`SYNC_OVERLAP_SECS`] секунд. Портал
//!    сравнивает её со своими отметками времени, и граница «ровно с этой
//!    секунды» теряла бы изменение, случившееся в ту же секунду, что и запрос.
//!    Лишний повтор безвреден: `dialogs_patch` идемпотентен.
//!
//! Символ `+` в дате обязан уехать как `%2B` — иначе портал прочитает его как
//! пробел и молча ответит данными за другой момент. Кодирование делает
//! сборщик пакета ([`crate::api::batch::encode_params`]), а страж —
//! `the_sync_batch_encodes_the_plus_in_the_date`.
//!
//! # Один клиент, один троттлер
//!
//! Этот воркер и воркер событий ходят через ОДИН [`crate::api::B24Client`]
//! (`B24State::api_client` кэширует его по секрету), поэтому оба лимита портала
//! соблюдаются на двоих. Второй клиент означал бы второе ведро на 2 rps при
//! общем лимите портала — и обрывы соединений вместо честной ошибки.

use std::sync::Arc;
use std::time::Duration;

use crate::api::B24Client;
use crate::dto::{CountersDto, DialogDto};
use crate::state::{B24State, DialogSeen};
use crate::ws::protocol::B24ServerMsg;

/// Перекрытие окна инкрементальной синхронизации, секунды.
pub const SYNC_OVERLAP_SECS: i64 = 5;

/// Потолок возраста `LAST_SYNC_DATE` (Р21): семь суток.
///
/// Дальше портал инкременту не обещает, и единственный честный путь — полная
/// перезагрузка списка.
pub const LAST_SYNC_MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

/// Сколько символов текста последнего сообщения хранить для сравнения.
///
/// Ровно столько же показывает превью строки списка: сравнивать больше нечего,
/// а держать полные тексты тысячи диалогов в памяти незачем.
pub const SEEN_TEXT_CHARS: usize = crate::dto::PREVIEW_CHARS;

/// Итог одного прохода синхронизации.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Сколько строк списка приехало.
    pub dialogs: usize,
    /// Сколько правок обнаружено (кадров `message_update`).
    pub edits: usize,
    /// Сколько кадров `presence` разослано.
    pub presence: usize,
    /// Счётчики изменились и уехали кадром.
    pub counters_changed: bool,
}

/// Запускает фолбэк-воркер.
///
/// **Как и воркер событий — ВСЕГДА, даже без вебхука.** До подключения он ждёт
/// на [`B24State::authorized_notify`] и не делает ни одного сетевого вызова.
pub async fn spawn(state: &Arc<B24State>) {
    let st = state.clone();
    let handle = tokio::spawn(async move { supervisor(st).await });
    state.register_worker(handle).await;
    tracing::info!(target: "forge_bitrix", "forge-bitrix: recent-sync worker spawned");
}

/// Вечный цикл медленной синхронизации.
pub async fn supervisor(state: Arc<B24State>) {
    let mut step = 0usize;
    let last_step = state.poll.last_step();
    // Момент, с которого портал считает изменения. `None` ⇒ следующий проход
    // будет ПОЛНЫМ (первый запуск, смена вебхука, слишком долгий простой).
    let mut since: Option<String> = None;
    let mut since_at = 0i64;

    loop {
        if !wait_authorized(&state).await {
            tracing::info!(target: "forge_bitrix", "forge-bitrix: recent-sync worker stopped");
            return;
        }

        let client = match state.api_client().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(target: "forge_bitrix", error = %e, "forge-bitrix: no API client for the recent sync");
                if !sleep_or_stop(&state, state.poll.backoff_at(step)).await {
                    return;
                }
                step = (step + 1).min(last_step);
                continue;
            }
        };

        let started = crate::dto::now_unix();
        // Р21: с датой старше семи дней инкремента у портала нет — уходим в
        // ветку полной перезагрузки, а не отправляем заведомо бесполезный
        // параметр.
        let incremental = since
            .as_deref()
            .filter(|_| started.saturating_sub(since_at) <= LAST_SYNC_MAX_AGE_SECS);
        if since.is_some() && incremental.is_none() {
            tracing::info!(
                target: "forge_bitrix",
                idle_secs = started.saturating_sub(since_at),
                "forge-bitrix: the sync window is older than seven days, reloading the whole list"
            );
        }

        match sync_once(&state, &client, incremental).await {
            Ok(outcome) => {
                step = 0;
                since = Some(atom_at(started - SYNC_OVERLAP_SECS));
                since_at = started;
                if outcome != SyncOutcome::default() {
                    tracing::debug!(
                        target: "forge_bitrix",
                        dialogs = outcome.dialogs,
                        edits = outcome.edits,
                        presence = outcome.presence,
                        counters = outcome.counters_changed,
                        "forge-bitrix: recent sync applied"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "forge_bitrix",
                    error_code = e.code().unwrap_or("transport"),
                    step,
                    "forge-bitrix: the recent sync failed"
                );
                if !sleep_or_stop(&state, state.poll.backoff_at(step)).await {
                    return;
                }
                step = (step + 1).min(last_step);
                continue;
            }
        }

        // Ожидание с побудкой: событие «вас добавили в чат» строку списка не
        // несёт, и ждать из-за него полную минуту незачем.
        let nudged = state.sync_nudge.notified();
        tokio::pin!(nudged);
        nudged.as_mut().enable();
        tokio::select! {
            biased;
            () = state.shutdown.cancelled() => {
                tracing::info!(target: "forge_bitrix", "forge-bitrix: recent-sync worker stopped");
                return;
            }
            () = &mut nudged => {}
            () = tokio::time::sleep(state.poll.recent_sync) => {}
        }
    }
}

/// Один проход синхронизации.
///
/// `since = None` означает ПОЛНУЮ перезагрузку списка: и первый запуск, и
/// ветку Р18 (протухший курсор событий), и ветку Р21 (слишком долгий простой).
/// Полный ответ — единственный, по которому можно судить об ИСЧЕЗНУВШИХ
/// строках, поэтому память о диалогах он перетирает целиком, а инкрементальный
/// только дополняет.
///
/// # Errors
///
/// Всё, чем может ответить `batch` портала. Отказ ОДНОЙ команды внутри пакета
/// ошибкой не является (`halt: 0`): список без счётчиков лучше, чем пустая
/// панель.
pub async fn sync_once(
    state: &Arc<B24State>,
    client: &Arc<B24Client>,
    since: Option<&str>,
) -> Result<SyncOutcome, crate::api::B24Error> {
    let full = since.is_none();
    let self_id = state.self_id().await;
    // Счётчики задач едут ТРЕТЬЕЙ командой того же пакета: внешний `batch`
    // стоит один хит лимита независимо от числа команд внутри, поэтому бейджи
    // задач достаются бесплатно. Отдельный запрос стоил бы половины бюджета в
    // 2 rps каждую минуту.
    let (items, counters_raw, task_counters_raw) = client
        .sync_bundle(since, Some(self_id).filter(|v| *v > 0))
        .await?;

    // Имена и присутствие приезжают в тех же строках — берём бесплатно.
    let users: Vec<crate::api::schema::UserRaw> = items.iter().map(|i| i.user.clone()).collect();
    state.peers.absorb_users(&users);

    let dialogs: Vec<DialogDto> = items
        .iter()
        .filter_map(|i| crate::dto::dialog_from_raw(i, self_id, &state.peers))
        .collect();

    let mut outcome = SyncOutcome {
        dialogs: dialogs.len(),
        ..SyncOutcome::default()
    };
    let mut remember: Vec<(String, DialogSeen)> = Vec::with_capacity(dialogs.len());

    for (dialog, raw) in dialogs.iter().zip(items.iter()) {
        let seen = seen_of(dialog);
        let previous = state.seen_dialog(&dialog.id).await;

        if let Some(frame) = edit_frame(dialog, raw, previous.as_ref(), self_id, &state.peers) {
            // Правка — ЕДИНСТВЕННЫЙ случай, когда содержимое ключа кэша медиа
            // меняется без удаления сообщения, а событий о ней платформа не
            // присылает вовсе (`ONIMV2MESSAGEUPDATE` живьём не приходит).
            // Поэтому именно здесь, у медленного воркера, лежит вторая половина
            // права роута медиа помечать ответ `Cache-Control: immutable`:
            // уберёшь эту строку — и отредактированное вложение залипнет у
            // клиента на год.
            if let B24ServerMsg::MessageUpdate { message, .. } = &frame {
                crate::media_cache::invalidate(&state.config, &dialog.id, message.id).await;
            }
            state.broadcast_event(frame);
            outcome.edits += 1;
        }
        if let Some(frame) = presence_frame(dialog, &seen, previous.as_ref()) {
            state.broadcast_event(frame);
            outcome.presence += 1;
        }
        remember.push((dialog.id.clone(), seen));
    }

    state.remember_dialogs(remember, full).await;

    if !dialogs.is_empty() {
        state.broadcast_event(B24ServerMsg::DialogsPatch {
            dialogs,
            // Об ИСЧЕЗНОВЕНИИ строк портал не сообщает вовсе: `im.recent.get`
            // просто перестаёт их отдавать. Полный ответ позволил бы вычислить
            // разницу, но полным он бывает редко, и класть сюда «всё, чего нет
            // в этом ответе» из инкрементального прохода означало бы стирать
            // весь список при каждой синхронизации.
            removed: Vec::new(),
        });
    }

    let counters = CountersDto::from_raw(&counters_raw);
    let now = crate::dto::now_unix();
    // ДВА независимых семейства счётчиков (Ф10): непрочитанные сообщения чатов
    // задач (`im.counters.get`) и состояние самих задач
    // (`tasks.task.counters.get`). Кадр один, но числа не складываются — см.
    // [`crate::tasks::TaskCountersDto`].
    let tasks_counters = task_counters_raw
        .as_ref()
        .map(crate::tasks::TaskCountersDto::from_raw);
    let tasks_changed = match tasks_counters {
        Some(c) => {
            let changed = state.task_counters_fresh(now, i64::MAX).await != Some(c);
            state.set_task_counters(c, now).await;
            changed
        }
        // Команда не отправлялась (владелец вебхука ещё не известен) либо
        // отказала внутри пакета: старый снимок в этом случае честнее нуля.
        None => false,
    };
    let shown = tasks_counters
        .or(state.task_counters_fresh(now, i64::MAX).await)
        .unwrap_or_default();

    let changed = state.counters_fresh(now, i64::MAX).await != Some(counters);
    state.set_counters(counters, now).await;
    if changed || tasks_changed {
        outcome.counters_changed = true;
        state.broadcast_event(B24ServerMsg::Counters {
            total: counters.total,
            chat: counters.chat,
            dialog: counters.dialog,
            tasks: counters.tasks,
            notify: counters.notify,
            expired: shown.expired,
            new_comments: shown.new_comments,
            mentioned: shown.mentioned,
        });
    }

    Ok(outcome)
}

/// Снимок строки для последующего диффа.
pub fn seen_of(dialog: &DialogDto) -> DialogSeen {
    let last = dialog.last_message.as_ref();
    DialogSeen {
        muted: dialog.muted,
        last_id: last.map(|m| m.id).unwrap_or_default(),
        last_text: crate::dto::truncate_chars(
            last.map(|m| m.text.as_str()).unwrap_or_default(),
            SEEN_TEXT_CHARS,
        ),
        online: dialog.online,
        peer_id: peer_of(dialog),
    }
}

/// Идентификатор собеседника личного диалога (`0` у групповых).
///
/// У личного диалога `DIALOG_ID` и есть идентификатор собеседника — это
/// свойство платформы, а не совпадение.
fn peer_of(dialog: &DialogDto) -> i64 {
    if dialog.kind != "user" {
        return 0;
    }
    dialog.id.parse::<i64>().unwrap_or_default()
}

/// Кадр `message_update`, если последнее сообщение ОТРЕДАКТИРОВАЛИ.
///
/// Условие ровно одно и оно точное: идентификатор тот же, текст другой. Новое
/// сообщение получило бы новый идентификатор, а неизменное — тот же текст.
/// Первый проход (`previous == None`) кадров не даёт вовсе: сравнивать не с
/// чем, и объявлять правкой всё содержимое списка при старте было бы враньём.
fn edit_frame(
    dialog: &DialogDto,
    raw: &crate::api::schema::RecentItemRaw,
    previous: Option<&DialogSeen>,
    self_id: i64,
    peers: &dyn crate::dto::PeerNames,
) -> Option<B24ServerMsg> {
    let previous = previous?;
    let last = dialog.last_message.as_ref()?;
    if last.id <= 0 || previous.last_id != last.id {
        return None;
    }
    let fresh = crate::dto::truncate_chars(&last.text, SEEN_TEXT_CHARS);
    if fresh == previous.last_text {
        return None;
    }

    // Строка списка несёт УРЕЗАННОЕ сообщение (без `params`, вложений и
    // клавиатур), поэтому кадр собирается из того, что есть: идентификатор,
    // автор, дата и новый текст. Метка «(изм.)» ставится здесь, а не берётся
    // из `params.IS_EDITED`: в строке списка этого поля нет вовсе, а факт
    // правки мы только что установили сравнением.
    let raw_message = crate::api::schema::MessageRaw {
        id: last.id,
        chat_id: dialog.chat_id.unwrap_or_default(),
        author_id: raw.message.author_id,
        date: last.date.clone(),
        text: raw.message.text.clone(),
        unread: false,
        params: serde_json::Value::Null,
    };
    let mut message = crate::dto::message_from_raw(
        &raw_message,
        &dialog.id,
        self_id,
        &std::collections::HashMap::new(),
        peers,
    );
    message.edited = true;
    Some(B24ServerMsg::MessageUpdate {
        dialog_id: dialog.id.clone(),
        message,
    })
}

/// Кадр `presence`, если собеседник появился или пропал.
///
/// Только для личных диалогов и только на ИЗМЕНЕНИИ: слать «всё ещё офлайн»
/// каждую минуту по каждому диалогу значило бы заливать сокет ничем.
fn presence_frame(
    dialog: &DialogDto,
    seen: &DialogSeen,
    previous: Option<&DialogSeen>,
) -> Option<B24ServerMsg> {
    if seen.peer_id <= 0 {
        return None;
    }
    let previous = previous?;
    if previous.online == seen.online {
        return None;
    }
    Some(B24ServerMsg::Presence {
        user_id: seen.peer_id.to_string(),
        online: seen.online,
        was_online: dialog
            .was_online
            .as_deref()
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            .map(|d| d.timestamp()),
    })
}

/// Unix-время в формате ATOM, который ждёт `LAST_SYNC_DATE`.
///
/// UTC со смещением `+00:00`, а не локальное время: это тот же самый момент,
/// портал разбирает смещение сам, и расхождение часовых поясов машины и портала
/// перестаёт что-либо значить.
pub fn atom_at(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts.max(0), 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// Ждёт подключения. `false` ⇒ дождались остановки.
///
/// Копия правила из [`crate::events`]: `notified().enable()` ДО проверки
/// состояния, иначе вебхук, вставленный в окне между ними, разбудил бы никого.
async fn wait_authorized(state: &Arc<B24State>) -> bool {
    loop {
        let notified = state.authorized_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        if state.shutdown.is_cancelled() {
            return false;
        }
        if state.is_authorized().await {
            return true;
        }

        tokio::select! {
            biased;
            () = state.shutdown.cancelled() => return false,
            () = &mut notified => {}
            () = tokio::time::sleep(state.poll.auth_poll) => {}
        }
    }
}

/// Спит, если раньше не пришла остановка. `false` ⇒ пора выходить.
async fn sleep_or_stop(state: &Arc<B24State>, delay: Duration) -> bool {
    tokio::select! {
        biased;
        () = state.shutdown.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::LastMessageDto;

    fn dialog(id: &str, last_id: i64, text: &str) -> DialogDto {
        DialogDto {
            id: id.to_string(),
            kind: if id.starts_with("chat") { "chat" } else { "user" }.to_string(),
            last_message: Some(LastMessageDto {
                id: last_id,
                text: text.to_string(),
                author_id: "15".into(),
                date: Some("2026-08-05T20:20:36+03:00".into()),
                ..LastMessageDto::default()
            }),
            ..DialogDto::default()
        }
    }

    fn raw_item(text: &str) -> crate::api::schema::RecentItemRaw {
        crate::api::schema::RecentItemRaw {
            message: crate::api::schema::RecentMessageRaw {
                author_id: 15,
                text: Some(text.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// ГЛАВНОЕ ПРАВИЛО ВОРКЕРА: тот же идентификатор + другой текст = правка.
    #[test]
    fn the_same_id_with_a_different_text_is_an_edit() {
        let before = seen_of(&dialog("chat27", 95, "было"));
        let after = dialog("chat27", 95, "стало");

        let frame = edit_frame(
            &after,
            &raw_item("стало"),
            Some(&before),
            1,
            &crate::dto::NoNames,
        )
        .expect("правка обязана дать кадр");
        match frame {
            B24ServerMsg::MessageUpdate { dialog_id, message } => {
                assert_eq!(dialog_id, "chat27");
                assert_eq!(message.id, 95);
                assert_eq!(message.text, "стало");
                assert!(message.edited, "метку ставим мы: в строке списка её нет");
            }
            other => panic!("{other:?}"),
        }
    }

    /// НОВОЕ сообщение — не правка: у него другой идентификатор.
    #[test]
    fn a_new_message_is_not_an_edit() {
        let before = seen_of(&dialog("chat27", 95, "было"));
        assert!(edit_frame(
            &dialog("chat27", 97, "совсем другое"),
            &raw_item("совсем другое"),
            Some(&before),
            1,
            &crate::dto::NoNames
        )
        .is_none());

        // Тот же текст — тоже не правка (строка списка просто поднялась).
        let same = seen_of(&dialog("chat27", 95, "было"));
        assert!(edit_frame(
            &dialog("chat27", 95, "было"),
            &raw_item("было"),
            Some(&same),
            1,
            &crate::dto::NoNames
        )
        .is_none());

        // Первый проход не знает прошлого — и молчит, а не объявляет правкой
        // весь список.
        assert!(edit_frame(
            &dialog("chat27", 95, "было"),
            &raw_item("было"),
            None,
            1,
            &crate::dto::NoNames
        )
        .is_none());
    }

    /// Присутствие: кадр только на ИЗМЕНЕНИИ и только у личных диалогов.
    #[test]
    fn presence_frames_are_sent_on_change_only() {
        let mut offline = dialog("19", 5, "привет");
        offline.online = false;
        let seen_offline = seen_of(&offline);
        assert_eq!(seen_offline.peer_id, 19);

        let mut online = dialog("19", 5, "привет");
        online.online = true;
        let seen_online = seen_of(&online);

        assert!(presence_frame(&online, &seen_online, Some(&seen_offline)).is_some());
        assert!(
            presence_frame(&online, &seen_online, Some(&seen_online)).is_none(),
            "«всё ещё онлайн» — не событие"
        );
        assert!(
            presence_frame(&online, &seen_online, None).is_none(),
            "первый проход не знает прошлого"
        );

        // У группового чата собеседника нет вовсе.
        let group = dialog("chat27", 5, "привет");
        let seen_group = seen_of(&group);
        assert_eq!(seen_group.peer_id, 0);
        assert!(presence_frame(&group, &seen_group, Some(&seen_group)).is_none());
    }

    /// Дата уезжает в ATOM с явным смещением — портал разбирает его сам.
    #[test]
    fn the_sync_date_is_an_atom_timestamp() {
        let s = atom_at(1_754_420_083);
        assert!(s.starts_with("2025-") || s.starts_with("2026-"), "{s}");
        assert!(s.contains('T') && s.contains("+00:00"), "{s}");
        // Отрицательное время — это баг вызывающего, но падать из-за него
        // воркеру незачем.
        assert!(!atom_at(-5).is_empty());
    }

    /// Р21 своими числами: неделя — потолок инкременты.
    #[test]
    fn the_incremental_window_is_capped_at_seven_days() {
        assert_eq!(LAST_SYNC_MAX_AGE_SECS, 604_800);
        assert_eq!(SYNC_OVERLAP_SECS, 5);
    }
}
