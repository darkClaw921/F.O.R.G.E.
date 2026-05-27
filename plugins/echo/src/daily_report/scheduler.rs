//! Scheduler авто-генерации «Сводки дня» в локальные 23:00.
//!
//! Фоновый loop, который раз в сутки в ~23:00 локального времени
//! ([`chrono::Local`]) вызывает [`super::generate_report`] с `source="auto"`
//! за текущий локальный день. После генерации пересчитывает интервал до
//! следующих 23:00 и снова засыпает.
//!
//! В отличие от [`crate::memory::scheduler`] (UTC-rollover раз в час с
//! маркером в БД) этот scheduler привязан к локальному времени пользователя:
//! сводку логично делать в конце его рабочего дня, а не в UTC-полночь.
//!
//! ## Graceful shutdown
//!
//! Сон до 23:00 обёрнут в `tokio::select!` вместе с
//! `state.shutdown.cancelled()` ([`tokio_util::sync::CancellationToken`]).
//! При отмене токена loop немедленно завершается, не дожидаясь срабатывания
//! таймера. `spawn` возвращает `JoinHandle`, который сохраняется в
//! `state.workers` и abort'ится в [`crate::shutdown`].
//!
//! ## Тестирование
//!
//! Расчёт интервала вынесен в чистую [`duration_until_next`] (принимает
//! `now: DateTime<Local>`), что даёт детерминированный hook для unit-тестов
//! без зависимости от системных часов — по аналогии с `tick_once` в
//! [`crate::memory::scheduler`].

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, TimeZone};
use tokio::task::JoinHandle;

use echo_host_api::HostApi;

use crate::state::EchoState;

/// Час локального времени, в который запускается авто-сводка.
const REPORT_HOUR: u32 = 23;

/// Спавнит scheduler-loop авто-сводки. Возвращает `JoinHandle`, который
/// вызывающий ([`crate::spawn_workers`]) сохраняет в `state.workers` для
/// graceful shutdown.
pub fn spawn(state: Arc<EchoState>, host: Arc<dyn HostApi>) -> JoinHandle<()> {
    tracing::info!(
        report_hour = REPORT_HOUR,
        "Echo daily_report scheduler started"
    );
    tokio::spawn(async move {
        loop {
            let wait = duration_until_next(Local::now());
            tracing::debug!(
                wait_secs = wait.as_secs(),
                "daily_report::scheduler: sleeping until next 23:00 local"
            );

            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = state.shutdown.cancelled() => {
                    tracing::info!("daily_report::scheduler: shutdown requested, stopping");
                    return;
                }
            }

            // День сводки — текущая локальная дата на момент срабатывания.
            let day = Local::now().date_naive();
            match super::generate_report(state.clone(), host.clone(), day, "auto").await {
                Ok(report) => tracing::info!(
                    %day,
                    chars = report.content.chars().count(),
                    "daily_report::scheduler: auto report generated"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    %day,
                    "daily_report::scheduler: auto report failed"
                ),
            }
        }
    })
}

/// Чистый расчёт [`Duration`] до ближайших локальных [`REPORT_HOUR`]:00.
///
/// Если `now` строго раньше сегодняшних 23:00 — возвращает интервал до них;
/// иначе (в 23:00 или позже, включая переход через полночь) — до 23:00
/// следующего дня. Вынесено в `pub(crate)` для детерминированных unit-тестов.
pub(crate) fn duration_until_next(now: DateTime<Local>) -> Duration {
    let report_time = NaiveTime::from_hms_opt(REPORT_HOUR, 0, 0)
        .expect("REPORT_HOUR is a valid hour");

    // Кандидат — сегодняшние 23:00 в локальной зоне.
    let today_target = local_at(now, now.date_naive(), report_time);

    let target = if now < today_target {
        today_target
    } else {
        let tomorrow = now.date_naive() + chrono::Duration::days(1);
        local_at(now, tomorrow, report_time)
    };

    // target всегда строго > now, поэтому signed-длительность неотрицательна.
    let delta = target - now;
    delta.to_std().unwrap_or(Duration::ZERO)
}

/// Конструирует локальный [`DateTime`] для `date` + `time`, корректно
/// разрешая неоднозначности DST (берём раннюю границу) и пропуски (fallback
/// на `now` + сутки как безопасный потолок).
fn local_at(now: DateTime<Local>, date: chrono::NaiveDate, time: NaiveTime) -> DateTime<Local> {
    let naive = date.and_time(time);
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(dt, _) => dt,
        // Пропуск из-за DST: целевое время не существует. Безопасный потолок —
        // примерно сутки вперёд от `now`, чтобы loop точно дождался валидного
        // момента следующего дня.
        chrono::LocalResult::None => now + chrono::Duration::hours(24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid local datetime")
    }

    #[test]
    fn before_2300_same_day() {
        // 10:00 → до 23:00 того же дня = 13 часов.
        let now = local(2026, 5, 27, 10, 0);
        let d = duration_until_next(now);
        assert_eq!(d, Duration::from_secs(13 * 3600));
    }

    #[test]
    fn just_before_2300() {
        // 22:59 → 60 секунд до 23:00.
        let now = local(2026, 5, 27, 22, 59);
        let d = duration_until_next(now);
        assert_eq!(d, Duration::from_secs(60));
    }

    #[test]
    fn at_2300_rolls_to_next_day() {
        // Ровно 23:00 → следующие 23:00 (через сутки).
        let now = local(2026, 5, 27, 23, 0);
        let d = duration_until_next(now);
        assert_eq!(d, Duration::from_secs(24 * 3600));
    }

    #[test]
    fn after_2300_rolls_to_next_day() {
        // 23:30 → до завтрашних 23:00 = 23.5 часа.
        let now = local(2026, 5, 27, 23, 30);
        let d = duration_until_next(now);
        assert_eq!(d, Duration::from_secs(23 * 3600 + 30 * 60));
    }

    #[test]
    fn after_midnight_targets_today_2300() {
        // 00:30 → до сегодняшних 23:00 = 22.5 часа.
        let now = local(2026, 5, 28, 0, 30);
        let d = duration_until_next(now);
        assert_eq!(d, Duration::from_secs(22 * 3600 + 30 * 60));
    }

    #[test]
    fn never_zero_and_within_a_day() {
        // Для любого «круглого» времени дельта в (0, 24h].
        for h in 0..24 {
            let now = local(2026, 5, 27, h, 0);
            let d = duration_until_next(now);
            assert!(d > Duration::ZERO, "h={h} produced zero");
            assert!(d <= Duration::from_secs(24 * 3600), "h={h} exceeded a day");
        }
    }
}
