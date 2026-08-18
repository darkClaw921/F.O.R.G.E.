//! Восстановление после перезапуска devforge.
//!
//! Вызывается **ПЕРВЫМ ДЕЛОМ в `spawn_workers`**, а не в `init`: `init`
//! синхронен и не имеет права трогать файловую систему записей.
//!
//! # Правило, ради которого модуль существует
//!
//! Статусы `recording` и `leaving` при наличии на диске **не менее
//! [`MeetConfig::salvage_min_secs`](crate::config::MeetConfig::salvage_min_secs)
//! секунд звука уходят в `transcoding`, а НЕ в `failed`.**
//!
//! Сорок минут записи не имеют права пропасть из-за Ctrl-C. Именно ради этого
//! чанки пишутся на диск сразу, а не копятся в памяти: к моменту перезапуска
//! спасать уже есть что, и остаётся только не выбросить это по инерции
//! («процесс умер → значит failed»).
//!
//! # Таблица «статус × наличие спасаемого звука»
//!
//! | статус на старте | звук есть | звука нет |
//! |---|---|---|
//! | `queued`, `launching`, `joining`, `waiting_admit` | `cancelled` | `cancelled` |
//! | `recording`, `leaving` | **`transcoding`** | `failed` |
//! | `transcoding`, `transcribing`, `summarizing` | перезапуск этапа | перезапуск этапа |
//! | `ready`, `failed`, `cancelled` | не трогаем | не трогаем |
//!
//! Первая строка даёт `cancelled`, а не `failed`, потому что ошибки не было:
//! бот не успел даже войти, терять нечего, и `failed` в журнале выглядел бы
//! как сбой, которого не случилось.
//!
//! Третья строка безопасна потому, что этапы **идемпотентны по
//! детерминированным путям**: повторный ffmpeg перезаписывает тот же файл,
//! повторная расшифровка заменяет сегменты целиком
//! (`repo::segments::replace_all`), а не дописывает.
//!
//! # Сироты Chrome
//!
//! `kill_on_drop` не спасает от `SIGKILL` по самому devforge. Поэтому обход
//! `<cache_dir>/*/chrome.pid` и убийство группы процессов — тоже здесь; сама
//! механика с ДВОЙНОЙ проверкой (жив И argv содержит наш `--user-data-dir`)
//! реализована в `forge_cdp::orphans`.

use std::path::Path;
use std::sync::Arc;

use crate::db::repo::{meetings, MeetingRow};
use crate::state::MeetState;
use crate::status::MeetStatus;

/// Размер заголовка RIFF/WAVE, который пишет наш писатель.
///
/// У файла от ffmpeg заголовок на 34 байта длиннее (чанк `LIST`/`INFO`) —
/// это около миллисекунды звука и на решение «спасать или нет» повлиять не
/// может, а разбирать чанки ради миллисекунды значило бы завести парсер WAV
/// на пути старта.
const WAV_HEADER_BYTES: u64 = 44;

/// Что сделать с встречей, застрявшей в незавершённом статусе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Записи не было — отменяем, не притворяясь, что произошёл сбой.
    Cancel,
    /// Есть спасаемый звук — отправляем в пост-обработку.
    Salvage,
    /// Звука нет, а запись числилась идущей — это уже настоящая потеря.
    Fail,
    /// Перезапустить этап пайплайна (он идемпотентен).
    Resume(MeetStatus),
    /// Терминальный статус — не трогаем.
    Leave,
}

impl ReconcileAction {
    /// Целевой статус, если действие требует перехода.
    pub fn target(self) -> Option<MeetStatus> {
        match self {
            Self::Cancel => Some(MeetStatus::Cancelled),
            Self::Salvage => Some(MeetStatus::Transcoding),
            Self::Fail => Some(MeetStatus::Failed),
            Self::Resume(_) | Self::Leave => None,
        }
    }

    /// Машиночитаемая причина для журнала переходов.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Cancel => "restart_before_recording",
            Self::Salvage => "salvaged_after_restart",
            Self::Fail => "no_audio_after_restart",
            Self::Resume(_) => "stage_restarted",
            Self::Leave => "untouched",
        }
    }
}

/// **Чистая таблица решений.** Ни ввода-вывода, ни базы — только статус и
/// факт наличия спасаемого звука.
///
/// Вынесена отдельно затем, чтобы табличный тест проверял решение, а не
/// умение открыть файл.
pub fn decide(status: MeetStatus, has_salvageable_audio: bool) -> ReconcileAction {
    use MeetStatus::*;
    match status {
        Queued | Launching | Joining | WaitingAdmit => ReconcileAction::Cancel,
        Recording | Leaving => {
            if has_salvageable_audio {
                ReconcileAction::Salvage
            } else {
                ReconcileAction::Fail
            }
        }
        Transcoding | Transcribing | Summarizing => ReconcileAction::Resume(status),
        // `pruned` — та же «ничего не делать», но по другой причине: каталога
        // на диске уже нет, и трогать его нечем.
        Ready | Failed | Cancelled | Pruned => ReconcileAction::Leave,
    }
}

/// Сколько секунд звука лежит в `<meeting_dir>/raw/*.wav`.
///
/// Берётся МАКСИМУМ по трекам, а не сумма: треки идут параллельно на общей
/// оси, и сумма пятерых говорящих дала бы пятикратную длительность встречи.
///
/// Считается по размеру файла, а не `ffprobe`: спасение не имеет права
/// зависеть от внешнего бинаря — иначе машина без ffmpeg теряла бы записи
/// именно там, где их важнее всего сохранить.
pub fn salvageable_secs(meeting_dir: &Path, sample_rate: u32) -> u64 {
    let raw = meeting_dir.join("raw");
    let Ok(entries) = std::fs::read_dir(&raw) else {
        return 0;
    };
    // PCM16 моно: два байта на сэмпл.
    let bytes_per_sec = u64::from(sample_rate) * 2;
    if bytes_per_sec == 0 {
        return 0;
    }
    let mut best = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let payload = meta.len().saturating_sub(WAV_HEADER_BYTES);
        best = best.max(payload / bytes_per_sec);
    }
    best
}

/// Итог прохода.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Сколько незавершённых встреч нашли.
    pub scanned: usize,
    /// Сколько записей спасено в `transcoding`.
    pub salvaged: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Этапы пайплайна, которые надо перезапустить (Ф5 подхватывает их
    /// отсюда).
    pub resumed: Vec<(String, MeetStatus)>,
    /// Сколько осиротевших Chrome убито.
    pub orphans_killed: usize,
}

/// Разбирает незавершённые встречи и подметает осиротевшие браузеры.
///
/// **Идемпотентна.** Повторный вызов не находит незавершённых встреч (все уже
/// переведены), а подметальщик не находит `chrome.pid` (файлы удалены). Это не
/// теория: `spawn_workers` зовётся один раз за процесс, но тест обязан
/// доказать, что второй вызов ничего не ломает — иначе повтор при будущем
/// «горячем перезапуске плагина» тихо убил бы идущие записи.
pub async fn reconcile_on_boot(state: &Arc<MeetState>) -> ReconcileReport {
    let mut report = ReconcileReport::default();

    let unfinished: Vec<MeetingRow> = match meetings::list_by_status(
        state,
        &[
            MeetStatus::Queued,
            MeetStatus::Launching,
            MeetStatus::Joining,
            MeetStatus::WaitingAdmit,
            MeetStatus::Recording,
            MeetStatus::Leaving,
            MeetStatus::Transcoding,
            MeetStatus::Transcribing,
            MeetStatus::Summarizing,
        ],
    )
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "forge_meet",
                error = %e,
                "forge-meet: не удалось прочитать незавершённые встречи, восстановление пропущено"
            );
            return report;
        }
    };

    report.scanned = unfinished.len();
    for row in unfinished {
        let secs = salvageable_secs(Path::new(&row.meeting_dir), state.config.audio_sample_rate);
        let has_audio = secs >= state.config.salvage_min_secs;
        let action = decide(row.status, has_audio);

        tracing::info!(
            target: "forge_meet",
            meeting_id = %row.id,
            status = row.status.as_str(),
            audio_secs = secs,
            salvageable = has_audio,
            action = ?action,
            "forge-meet: восстановление после перезапуска"
        );

        match action {
            ReconcileAction::Resume(stage) => {
                report.resumed.push((row.id.clone(), stage));
                // Прерванная пост-обработка продолжается сама: этапы
                // идемпотентны по детерминированным путям, и повторный
                // прогон безопасен.
                if stage == MeetStatus::Transcoding {
                    crate::pipeline::run::spawn(state, &row.id);
                }
            }
            ReconcileAction::Leave => {}
            _ => {
                let Some(target) = action.target() else { continue };
                match meetings::set_status(state, &row.id, target, Some(action.reason())).await {
                    Ok(_) => match action {
                        ReconcileAction::Salvage => {
                            // Сорок минут звука на диске обязаны дойти до
                            // расшифровки, а не остаться лежать.
                            crate::pipeline::run::spawn(state, &row.id);
                            report.salvaged += 1;
                        }
                        ReconcileAction::Fail => report.failed += 1,
                        ReconcileAction::Cancel => report.cancelled += 1,
                        _ => {}
                    },
                    Err(e) => tracing::warn!(
                        target: "forge_meet",
                        meeting_id = %row.id,
                        error = %e,
                        "forge-meet: восстановление статуса не удалось"
                    ),
                }
            }
        }
    }

    report.orphans_killed = sweep_orphans(state).await;
    tracing::info!(
        target: "forge_meet",
        scanned = report.scanned,
        salvaged = report.salvaged,
        failed = report.failed,
        cancelled = report.cancelled,
        resumed = report.resumed.len(),
        orphans_killed = report.orphans_killed,
        "forge-meet: восстановление завершено"
    );
    report
}

/// Подметает `<cache_dir>/*/chrome.pid`.
async fn sweep_orphans(state: &Arc<MeetState>) -> usize {
    let cfg = forge_cdp::orphans::SweepConfig::default();
    let report = forge_cdp::orphans::sweep_orphans(&state.config.cache_dir, &cfg).await;
    if report.foreign() > 0 {
        // Сработала защита от переиспользованного pid: процесс жив, но argv не
        // наш. Это ровно тот случай, ради которого проверка двойная.
        tracing::warn!(
            target: "forge_meet",
            foreign = report.foreign(),
            "forge-meet: pid переиспользован чужим процессом, не трогаю"
        );
    }
    report.killed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeetConfig;

    /// ТАБЛИЧНЫЙ ТЕСТ: по строке на каждое сочетание «статус × наличие
    /// спасаемого звука». Ожидаемый исход зафиксирован явно.
    #[test]
    fn the_decision_table_is_complete() {
        use MeetStatus::*;
        use ReconcileAction::*;
        let table: [(MeetStatus, bool, ReconcileAction); MeetStatus::ALL.len() * 2] = [
            (Queued, false, Cancel),
            (Queued, true, Cancel),
            (Launching, false, Cancel),
            (Launching, true, Cancel),
            (Joining, false, Cancel),
            (Joining, true, Cancel),
            (WaitingAdmit, false, Cancel),
            (WaitingAdmit, true, Cancel),
            // ГЛАВНАЯ строка таблицы: сорок минут записи не имеют права
            // пропасть из-за Ctrl-C.
            (Recording, true, Salvage),
            (Recording, false, Fail),
            (Leaving, true, Salvage),
            (Leaving, false, Fail),
            (Transcoding, true, Resume(Transcoding)),
            (Transcoding, false, Resume(Transcoding)),
            (Transcribing, true, Resume(Transcribing)),
            (Transcribing, false, Resume(Transcribing)),
            (Summarizing, true, Resume(Summarizing)),
            (Summarizing, false, Resume(Summarizing)),
            (Ready, true, Leave),
            (Ready, false, Leave),
            (Failed, true, Leave),
            (Failed, false, Leave),
            (Cancelled, true, Leave),
            (Cancelled, false, Leave),
            // Каталога уже нет — трогать нечего, и «спасаемый звук» у такой
            // встречи может быть только чужим мусором с совпавшим путём.
            (Pruned, true, Leave),
            (Pruned, false, Leave),
        ];
        // Каждый статус обязан быть в таблице в обоих вариантах: иначе новый
        // статус проскочил бы мимо восстановления.
        assert_eq!(table.len(), MeetStatus::ALL.len() * 2);
        for st in MeetStatus::ALL {
            assert_eq!(
                table.iter().filter(|(s, _, _)| *s == st).count(),
                2,
                "статус {} не покрыт обоими вариантами",
                st.as_str()
            );
        }
        for (status, audio, expected) in table {
            assert_eq!(
                decide(status, audio),
                expected,
                "{} со звуком={audio}",
                status.as_str()
            );
        }
    }

    /// Всякий целевой статус восстановления обязан быть РАЗРЕШЁННЫМ переходом:
    /// иначе `set_status` откажет, и запись зависнет в `recording` навсегда.
    #[test]
    fn every_reconcile_target_is_an_allowed_transition() {
        for status in MeetStatus::ALL {
            for audio in [false, true] {
                if let Some(target) = decide(status, audio).target() {
                    assert!(
                        crate::status::can_transition(status, target),
                        "{} → {} запрещён таблицей переходов",
                        status.as_str(),
                        target.as_str()
                    );
                }
            }
        }
    }

    fn write_wav(dir: &Path, name: &str, secs: u64, sample_rate: u32) {
        let raw = dir.join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        let bytes = WAV_HEADER_BYTES + secs * u64::from(sample_rate) * 2;
        std::fs::write(raw.join(name), vec![0u8; bytes as usize]).unwrap();
    }

    /// Десять секунд — ГРАНИЦА, и она проверяется с обеих сторон.
    #[test]
    fn ten_seconds_is_the_salvage_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = MeetConfig::default();
        let min = cfg.salvage_min_secs;
        assert_eq!(min, 10);

        let nine = dir.path().join("nine");
        write_wav(&nine, "track-0.wav", 9, cfg.audio_sample_rate);
        let secs = salvageable_secs(&nine, cfg.audio_sample_rate);
        assert_eq!(secs, 9);
        assert_eq!(decide(MeetStatus::Recording, secs >= min), ReconcileAction::Fail);

        let eleven = dir.path().join("eleven");
        write_wav(&eleven, "track-0.wav", 11, cfg.audio_sample_rate);
        let secs = salvageable_secs(&eleven, cfg.audio_sample_rate);
        assert_eq!(secs, 11);
        assert_eq!(
            decide(MeetStatus::Recording, secs >= min),
            ReconcileAction::Salvage
        );

        // Ровно десять — спасаем: граница включительная.
        let ten = dir.path().join("ten");
        write_wav(&ten, "track-0.wav", 10, cfg.audio_sample_rate);
        assert_eq!(salvageable_secs(&ten, cfg.audio_sample_rate), 10);
    }

    /// Длительность — МАКСИМУМ по трекам, а не сумма: пятеро говорящих иначе
    /// дали бы пятикратную встречу.
    #[test]
    fn the_longest_track_defines_the_duration() {
        let dir = tempfile::tempdir().unwrap();
        let sr = 16_000;
        write_wav(dir.path(), "track-0.wav", 5, sr);
        write_wav(dir.path(), "track-1.wav", 40, sr);
        write_wav(dir.path(), "track-2.wav", 7, sr);
        assert_eq!(salvageable_secs(dir.path(), sr), 40);
    }

    #[test]
    fn a_missing_directory_simply_has_no_audio() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(salvageable_secs(&dir.path().join("nope"), 16_000), 0);
        // И посторонние файлы не считаются звуком.
        let raw = dir.path().join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        std::fs::write(raw.join("manifest.json"), vec![0u8; 10_000_000]).unwrap();
        assert_eq!(salvageable_secs(dir.path(), 16_000), 0);
    }
}
