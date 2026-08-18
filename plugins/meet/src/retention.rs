//! Ретенция каталога встреч: срок хранения, дисковый бюджет и LRU.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # Обкатанный образец, а не четвёртая реализация
//!
//! Устройство повторяет `plugins/bitrix/src/media_cache.rs::sweep_once`,
//! который трижды прошёл боевую эксплуатацию: LRU по времени последнего
//! обращения, подчистка до 90 % потолка с ГИСТЕРЕЗИСОМ (без него подметание
//! запускалось бы каждый тик), защита того, что используется прямо сейчас.
//! Отличий три, и каждое — свойство предметной области:
//!
//! 1. **Единица — КАТАЛОГ ВСТРЕЧИ ЦЕЛИКОМ**, а не файл. Удалить `audio.m4a`,
//!    оставив `transcript.md`, значит получить карточку, у которой плеер
//!    молча не работает; удалить наоборот — расшифровку без текста.
//! 2. **Строка в базе остаётся**, статус становится
//!    [`MeetStatus::Pruned`]. Удали мы строку — бейдж на старом сообщении
//!    превратился бы в «можно записать», то есть предложил бы войти ботом в
//!    давно прошедшую встречу.
//! 3. **Минимальное число последних записей не удаляется никогда**
//!    ([`crate::config::MeetConfig::retention_min_keep`]). Дисковый бюджет,
//!    выставленный слишком низко, иначе стирал бы вчерашнюю планёрку —
//!    ровно то, ради чего фича и работает.
//!
//! # Что подметается
//!
//! | что | когда |
//! |---|---|
//! | `work/` (транзитный `mix.wav`, вывод whisper) | у любой законченной встречи |
//! | `chrome-profile/` | при чистом завершении — сразу; после `failed` — через сутки |
//! | каталог встречи целиком | старше `retention_days`, либо LRU при превышении `max_cache_bytes` |
//!
//! Профиль Chrome после ПАДЕНИЯ живёт сутки намеренно: в нём остаются логи и
//! состояние вкладки, по которым только и можно понять, почему бот не вошёл. А
//! `work/*.wav` — это 115 МБ на час на дорожку, и держать их после успешной
//! расшифровки незачем: сам пайплайн чистит их сразу
//! ([`crate::pipeline::run`]), сюда попадают только остатки после сбоев.
//!
//! # Чужих каталогов подметальщик НЕ ТРОГАЕТ
//!
//! Обход идёт по СТРОКАМ БАЗЫ, а не по содержимому `~/.forge/meet`. Каталог,
//! которому не соответствует ни одна встреча, остаётся на месте: там лежат
//! `models/` с гигабайтами весов, `meet.db` и всё, что пользователь мог
//! положить рядом руками. Подметальщик, обходящий каталог, однажды удалил бы
//! это — и объяснить, почему пропали модели, было бы уже некому.
//!
//! Отсюда же следует: **без базы подметание не работает вовсе** и честно
//! ничего не делает.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::db::repo::meetings;
use crate::state::MeetState;
use crate::status::MeetStatus;

/// Как часто работает фоновый подметальщик.
///
/// Полчаса, а не десять минут как у медиа Битрикса: встречи появляются
/// единицами в день, а обход каталога стоит `stat` по каждому файлу записи.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Числитель целевой доли бюджета, до которой подметаем.
pub const SWEEP_TARGET_NUM: u64 = 9;

/// Знаменатель целевой доли (9/10 ⇒ 90 %). Гистерезис: без него подчистка
/// запускалась бы на каждом тике, едва размер коснулся потолка.
pub const SWEEP_TARGET_DEN: u64 = 10;

/// Сколько живёт профиль Chrome после ПАДЕНИЯ записи.
///
/// Сутки — столько человек реально тратит на «почему бот вчера не вошёл». При
/// чистом завершении профиль удаляется сразу: он весит десятки мегабайт и не
/// содержит ничего, кроме следов одного входа.
pub const CRASH_PROFILE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Имя каталога профиля Chrome внутри каталога встречи.
pub const CHROME_PROFILE_DIR: &str = "chrome-profile";

/// Причина, с которой встреча уходит в [`MeetStatus::Pruned`].
pub const REASON_AGE: &str = "retention_age";
/// Причина «упёрлись в дисковый бюджет».
pub const REASON_BUDGET: &str = "retention_budget";

/// Итог одного прогона.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepStats {
    /// Сколько каталогов встреч осмотрено.
    pub scanned: usize,
    /// Сколько каталогов удалено целиком.
    pub pruned: usize,
    /// Сколько встреч не тронуто из-за защиты (активные, идущая
    /// пост-обработка, `retention_min_keep`).
    pub protected: usize,
    /// Сколько транзитных `work/` подчищено.
    pub work_cleaned: usize,
    /// Сколько профилей Chrome подчищено.
    pub profiles_cleaned: usize,
    /// Освобождено байт.
    pub freed: u64,
    /// Суммарный размер каталогов встреч до прогона.
    pub total_before: u64,
    /// И после.
    pub total_after: u64,
}

/// Целевой размер, до которого подметаем (90 % бюджета — гистерезис).
pub fn sweep_target(max_bytes: u64) -> u64 {
    max_bytes / SWEEP_TARGET_DEN * SWEEP_TARGET_NUM
}

/// Суммарный размер каталога, рекурсивно. Недоступное считается нулём:
/// подметальщик не имеет права упасть из-за одного файла без прав.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Время последнего изменения каталога, unix-секунды. `0` — неизвестно.
fn dir_mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Одна встреча глазами подметальщика.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: String,
    dir: PathBuf,
    status: MeetStatus,
    status_reason: Option<String>,
    bytes: u64,
    /// Время последнего обращения, unix-секунды: максимум из `updated_at`
    /// базы и mtime каталога. Второе слагаемое существенно: ручное
    /// переименование спикеров переписывает `transcript.*`, и запись,
    /// которую вчера правили руками, обязана всплыть наверх.
    used_at: i64,
    /// Когда встреча закончилась (для срока хранения).
    ended_at: i64,
    created_at: i64,
}

/// Решение по одной встрече — ЧИСТАЯ функция, чтобы таблица правил
/// проверялась без файловой системы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Нельзя трогать: запись идёт, пост-обработка читает файлы либо это одна
    /// из последних `retention_min_keep`.
    Protected,
    /// Файлов уже нет.
    AlreadyPruned,
    /// Просрочена по возрасту.
    TooOld,
    /// Можно удалить, если не хватит места.
    Candidate,
}

/// Правила «трогать / не трогать», без единого обращения к диску.
///
/// `rank` — порядковый номер записи среди СВЕЖИХ (0 — самая новая).
pub fn verdict(
    status: MeetStatus,
    has_job: bool,
    rank: usize,
    age_secs: i64,
    retention_days: u64,
    min_keep: usize,
) -> Verdict {
    if status == MeetStatus::Pruned {
        return Verdict::AlreadyPruned;
    }
    // Активная запись пишет в этот каталог прямо сейчас; пост-обработка
    // читает из него. Оба случая — не «жалко удалить», а «удаление сломает
    // идущую работу».
    if status.is_active() || status.is_processing() || has_job {
        return Verdict::Protected;
    }
    if rank < min_keep {
        return Verdict::Protected;
    }
    if retention_days > 0 && age_secs > (retention_days as i64) * 86_400 {
        return Verdict::TooOld;
    }
    Verdict::Candidate
}

/// Один прогон подчистки.
///
/// Порядок: транзитные каталоги → просрочка по возрасту → LRU по бюджету.
/// Именно такой: transient-мусор дешевле удалить, чем считать против бюджета,
/// а возраст — правило пользователя, тогда как бюджет — вынужденная мера.
pub async fn sweep_once(state: &Arc<MeetState>) -> SweepStats {
    let mut stats = SweepStats::default();
    // Без базы подметание невозможно: обход идёт по строкам, а не по
    // каталогу, — см. шапку модуля.
    if state.db().is_none() {
        return stats;
    }
    let rows = match meetings::all(state).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                error = %e,
                "forge-meet: ретенция не смогла прочитать список встреч"
            );
            return stats;
        }
    };

    let cfg = &state.config;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Размеры каталогов считаются в блокирующем пуле: обход тысячи записей —
    // это тысячи `stat`, и делать их в реакторе значит держать исполнителя.
    let dirs: Vec<(String, PathBuf)> = rows
        .iter()
        .map(|r| (r.id.clone(), PathBuf::from(&r.meeting_dir)))
        .collect();
    let measured = tokio::task::spawn_blocking(move || {
        dirs.into_iter()
            .map(|(id, dir)| {
                let exists = dir.is_dir();
                let bytes = if exists { dir_size(&dir) } else { 0 };
                let mtime = dir_mtime_secs(&dir);
                (id, dir, exists, bytes, mtime)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    let mut entries: Vec<Entry> = Vec::new();
    for (row, (_, dir, exists, bytes, mtime)) in rows.iter().zip(measured.into_iter()) {
        if !exists {
            continue;
        }
        stats.scanned += 1;
        stats.total_before += bytes;
        entries.push(Entry {
            id: row.id.clone(),
            dir,
            status: row.status,
            status_reason: row.status_reason.clone(),
            bytes,
            used_at: row.updated_at.max(mtime),
            ended_at: row.ended_at.unwrap_or(row.updated_at),
            created_at: row.created_at,
        });
    }
    stats.total_after = stats.total_before;

    // Ранг свежести: 0 — самая новая запись. Считается по `created_at`, а не
    // по последнему обращению: «последние N» — это последние ПРОВЕДЁННЫЕ
    // встречи, и открытие старой записи не должно двигать этот список.
    let mut by_created: Vec<(&String, i64)> = entries.iter().map(|e| (&e.id, e.created_at)).collect();
    by_created.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(a.0)));
    let rank_of: std::collections::HashMap<String, usize> = by_created
        .into_iter()
        .enumerate()
        .map(|(i, (id, _))| (id.clone(), i))
        .collect();

    // --- транзитные каталоги ------------------------------------------------
    for e in &entries {
        if e.status.is_active() || e.status.is_processing() || state.has_job(&e.id).await {
            continue;
        }
        let work = e.dir.join(crate::pipeline::WORK_DIR);
        if work.is_dir() {
            let size = dir_size(&work);
            if tokio::fs::remove_dir_all(&work).await.is_ok() {
                stats.work_cleaned += 1;
                stats.freed += size;
                stats.total_after = stats.total_after.saturating_sub(size);
            }
        }
        let profile = e.dir.join(CHROME_PROFILE_DIR);
        if profile.is_dir() && profile_is_disposable(e.status, now - e.ended_at) {
            let size = dir_size(&profile);
            if tokio::fs::remove_dir_all(&profile).await.is_ok() {
                stats.profiles_cleaned += 1;
                stats.freed += size;
                stats.total_after = stats.total_after.saturating_sub(size);
            }
        }
    }

    // --- вердикты -----------------------------------------------------------
    let mut candidates: Vec<&Entry> = Vec::new();
    let mut too_old: Vec<&Entry> = Vec::new();
    for e in &entries {
        let rank = rank_of.get(&e.id).copied().unwrap_or(usize::MAX);
        let has_job = state.has_job(&e.id).await;
        match verdict(
            e.status,
            has_job,
            rank,
            now - e.ended_at,
            cfg.retention_days,
            cfg.retention_min_keep,
        ) {
            Verdict::Protected => stats.protected += 1,
            Verdict::AlreadyPruned => {}
            Verdict::TooOld => too_old.push(e),
            Verdict::Candidate => candidates.push(e),
        }
    }

    for e in too_old {
        if prune(state, e, REASON_AGE).await {
            stats.pruned += 1;
            stats.freed += e.bytes;
            stats.total_after = stats.total_after.saturating_sub(e.bytes);
        }
    }

    // --- LRU по бюджету ------------------------------------------------------
    if cfg.max_cache_bytes == 0 || stats.total_after <= cfg.max_cache_bytes {
        return stats;
    }
    let target = sweep_target(cfg.max_cache_bytes);
    candidates.sort_by_key(|e| e.used_at);
    for e in candidates {
        if stats.total_after <= target {
            break;
        }
        if prune(state, e, REASON_BUDGET).await {
            stats.pruned += 1;
            stats.freed += e.bytes;
            stats.total_after = stats.total_after.saturating_sub(e.bytes);
        }
    }
    stats
}

/// Можно ли уже удалить профиль Chrome.
///
/// После `failed` — только через [`CRASH_PROFILE_TTL`]: в профиле остаются
/// логи и состояние вкладки, по которым только и можно понять, почему бот не
/// вошёл.
pub fn profile_is_disposable(status: MeetStatus, age_secs: i64) -> bool {
    if !status.is_terminal() {
        return false;
    }
    if status == MeetStatus::Failed {
        return age_secs >= CRASH_PROFILE_TTL.as_secs() as i64;
    }
    true
}

/// Удаляет каталог и отмечает встречу. `false` — что-то не получилось.
///
/// Порядок «сначала диск, потом статус» существен: обратный оставил бы при
/// сбое удаления строку `pruned` рядом с целыми файлами, то есть карточку,
/// которая врёт про отсутствие записи.
async fn prune(state: &Arc<MeetState>, e: &Entry, reason: &str) -> bool {
    if let Err(err) = tokio::fs::remove_dir_all(&e.dir).await {
        tracing::warn!(
            target: "forge_meet",
            meeting_id = %e.id,
            dir = %e.dir.display(),
            error = %err,
            "forge-meet: каталог встречи не удалился"
        );
        return false;
    }
    // Статус — через `set_status`: единственная точка мутации, она же
    // обновляет тёплый индекс бейджей и вещает кадр. Иначе лента показывала бы
    // «ГОТОВО» у записи, которой уже нет.
    if let Err(err) = meetings::set_status(state, &e.id, MeetStatus::Pruned, Some(reason)).await {
        tracing::warn!(
            target: "forge_meet",
            meeting_id = %e.id,
            error = %err,
            "forge-meet: статус после вычистки не записался"
        );
        return false;
    }
    let _ = meetings::clear_paths(state, &e.id).await;
    let _ = crate::db::repo::events::note(
        state,
        &e.id,
        MeetStatus::Pruned.as_str(),
        reason,
        Some(&format!("освобождено {} байт", e.bytes)),
    )
    .await;
    true
}

/// Фоновый подметальщик: прогон на старте и далее раз в [`SWEEP_INTERVAL`].
pub async fn sweeper_loop(state: Arc<MeetState>) {
    loop {
        let stats = sweep_once(&state).await;
        if stats.pruned > 0 || stats.work_cleaned > 0 || stats.profiles_cleaned > 0 {
            tracing::info!(
                target: "forge_meet",
                scanned = stats.scanned,
                pruned = stats.pruned,
                protected = stats.protected,
                work_cleaned = stats.work_cleaned,
                profiles_cleaned = stats.profiles_cleaned,
                freed = stats.freed,
                total_after = stats.total_after,
                "forge-meet: ретенция подмела каталог записей"
            );
        }
        tokio::select! {
            _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
            _ = state.shutdown.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_leaves_a_hysteresis_gap() {
        assert_eq!(sweep_target(1000), 900);
        // Без бюджета цель бессмысленна, но и не должна быть отрицательной.
        assert_eq!(sweep_target(0), 0);
    }

    /// ТАБЛИЦА ПРАВИЛ целиком, без единого обращения к диску.
    #[test]
    fn the_rules_protect_exactly_what_must_be_protected() {
        let day = 86_400i64;

        // Идущая запись — никогда, ни при каком возрасте и ранге.
        for st in MeetStatus::ACTIVE {
            assert_eq!(
                verdict(st, false, 100, 1000 * day, 1, 0),
                Verdict::Protected,
                "{}",
                st.as_str()
            );
        }
        // Пост-обработка читает файлы прямо сейчас.
        for st in [MeetStatus::Transcoding, MeetStatus::Transcribing, MeetStatus::Summarizing] {
            assert_eq!(verdict(st, false, 100, 1000 * day, 1, 0), Verdict::Protected);
        }
        // Идущее задание пайплайна — тоже защита, даже у терминального статуса
        // (повтор этапа из `ready`).
        assert_eq!(
            verdict(MeetStatus::Ready, true, 100, 1000 * day, 1, 0),
            Verdict::Protected
        );

        // Последние N — независимо от возраста.
        assert_eq!(
            verdict(MeetStatus::Ready, false, 2, 1000 * day, 1, 5),
            Verdict::Protected
        );
        assert_eq!(
            verdict(MeetStatus::Ready, false, 5, 1000 * day, 1, 5),
            Verdict::TooOld
        );

        // Срок хранения.
        assert_eq!(
            verdict(MeetStatus::Ready, false, 9, 91 * day, 90, 0),
            Verdict::TooOld
        );
        assert_eq!(
            verdict(MeetStatus::Ready, false, 9, 89 * day, 90, 0),
            Verdict::Candidate
        );
        // `0` — «хранить вечно», и никакой возраст этого не отменяет.
        assert_eq!(
            verdict(MeetStatus::Ready, false, 9, 100_000 * day, 0, 0),
            Verdict::Candidate
        );

        // Уже вычищенную не вычищают повторно.
        assert_eq!(
            verdict(MeetStatus::Pruned, false, 0, 0, 90, 5),
            Verdict::AlreadyPruned
        );
    }

    /// Профиль Chrome после ПАДЕНИЯ живёт сутки: в нём единственные следы того,
    /// почему бот не вошёл.
    #[test]
    fn a_crashed_run_keeps_its_chrome_profile_for_a_day() {
        let hour = 3600;
        assert!(!profile_is_disposable(MeetStatus::Failed, hour));
        assert!(!profile_is_disposable(MeetStatus::Failed, 23 * hour));
        assert!(profile_is_disposable(MeetStatus::Failed, 25 * hour));

        // Чистое завершение — сразу.
        assert!(profile_is_disposable(MeetStatus::Ready, 0));
        assert!(profile_is_disposable(MeetStatus::Cancelled, 0));

        // Пока встреча идёт — профиль нужен браузеру.
        for st in MeetStatus::ALL.into_iter().filter(|s| !s.is_terminal()) {
            assert!(
                !profile_is_disposable(st, 1000 * hour),
                "{}",
                st.as_str()
            );
        }
    }

    #[test]
    fn dir_size_walks_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("raw")).unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("raw/track-0.wav"), vec![0u8; 250]).unwrap();
        assert_eq!(dir_size(dir.path()), 350);
        assert_eq!(dir_size(&dir.path().join("нет-такого")), 0);
    }
}
