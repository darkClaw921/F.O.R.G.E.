//! Роуты сводки и диаграммы Гантта.
//!
//! Оба роута — тонкая обёртка над чистыми функциями [`crate::report`]: разбор
//! окна и оси группировки, вызов расчёта, ответ. Никакой арифметики здесь нет
//! намеренно, она вся покрыта тестами фазы 1 и должна оставаться в одном месте.
//!
//! # Окно по умолчанию — ТЕКУЩАЯ НЕДЕЛЯ
//!
//! Отсутствующие `from` и `to` не означают «всё время»: сводка за всю историю
//! на экране отчёта бесполезна (одна строка «итого») и с ростом файла всё
//! дороже. Поэтому пустые параметры дают окно `[локальный понедельник 00:00,
//! +7 суток)` — ровно то, что показывает статус-бар панели (`WEEK …`), так что
//! цифры экрана отчёта и статус-бара совпадают без объяснений.
//!
//! Параметры независимы: можно задать только `from` (тогда `to` — конец
//! текущей недели) или только `to`.
//!
//! # Границы окна — локальные, а не UTC
//!
//! `from=2026-09-08` означает локальную полночь 8 сентября, а не 00:00 UTC.
//! Отчёт читает человек, живущий в своём часовом поясе: рабочий день с 9 утра
//! до 6 вечера в UTC+3 при UTC-границах разъехался бы по двум «дням» отчёта.
//! Разбор делает [`crate::report::parse_ts`], который трактует голую дату как
//! локальную полночь.
//!
//! Окно полуоткрытое: `from` включается, `to` — нет. Поэтому
//! `from=2026-09-08&to=2026-09-09` это ровно одни сутки, без двойного учёта
//! отрезка, попавшего на границу.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Datelike, Duration, Local, Utc};
use serde::Deserialize;

use super::error::{ApiError, ApiResult};
use crate::report::{GanttData, Report, ReportGroupBy};
use crate::state::TrackerState;

/// Query `GET /api/tracker/report` и `GET /api/tracker/gantt`.
#[derive(Debug, Default, Deserialize)]
pub struct WindowQuery {
    /// Начало окна: `YYYY-MM-DD` или RFC 3339.
    #[serde(default)]
    pub from: Option<String>,
    /// Конец окна (не включается).
    #[serde(default)]
    pub to: Option<String>,
    /// Ось группировки отчёта: `day` (по умолчанию), `task`, `list`.
    #[serde(default)]
    pub group_by: Option<String>,
}

/// Границы текущей недели: локальный понедельник 00:00 и +7 суток.
fn current_week(now: DateTime<Utc>) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let today = now.with_timezone(&Local).date_naive();
    let back = today.weekday().num_days_from_monday() as i64;
    let monday = today - Duration::days(back);
    (
        crate::report::local_day_start(monday),
        crate::report::local_day_start(monday + Duration::days(7)),
    )
}

/// Разбирает окно, подставляя текущую неделю вместо пропущенных границ.
fn resolve_window(
    query: &WindowQuery,
    now: DateTime<Utc>,
) -> ApiResult<(Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    let (week_from, week_to) = current_week(now);

    let from = match query.from.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(
            crate::report::parse_ts(raw)
                .ok_or_else(|| ApiError::bad_request(format!("bad from: {raw:?}")))?,
        ),
        _ => week_from,
    };
    let to = match query.to.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(
            crate::report::parse_ts(raw)
                .ok_or_else(|| ApiError::bad_request(format!("bad to: {raw:?}")))?,
        ),
        _ => week_to,
    };

    if let (Some(f), Some(t)) = (from, to) {
        if t <= f {
            return Err(ApiError::bad_request("to must be later than from"));
        }
    }
    Ok((from, to))
}

/// `GET /api/tracker/report?from&to&group_by=day|task|list`.
pub async fn report(
    State(state): State<Arc<TrackerState>>,
    Query(query): Query<WindowQuery>,
) -> ApiResult<Json<Report>> {
    let group_by = match query.group_by.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => ReportGroupBy::parse(raw).ok_or_else(|| {
            ApiError::bad_request(format!(
                "unknown group_by {raw:?}, expected one of: day, task, list"
            ))
        })?,
        _ => ReportGroupBy::Day,
    };
    let now = Utc::now();
    let (from, to) = resolve_window(&query, now)?;
    Ok(Json(state.store.report_at(group_by, from, to, now)))
}

/// `GET /api/tracker/gantt?from&to`.
///
/// Ответ несёт полосы задач (план из `start`/`due`/`estimate_min`, факт из
/// суммы отрезков, флаг `over_estimate`) и вехи периода для дорожки
/// `// MILESTONES`.
pub async fn gantt(
    State(state): State<Arc<TrackerState>>,
    Query(query): Query<WindowQuery>,
) -> ApiResult<Json<GanttData>> {
    let now = Utc::now();
    let (from, to) = resolve_window(&query, now)?;
    Ok(Json(state.store.gantt_at(from, to, now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(from: Option<&str>, to: Option<&str>) -> WindowQuery {
        WindowQuery {
            from: from.map(str::to_string),
            to: to.map(str::to_string),
            group_by: None,
        }
    }

    #[test]
    fn empty_window_falls_back_to_the_current_week() {
        let now = Utc::now();
        let (from, to) = resolve_window(&q(None, None), now).expect("defaults resolve");
        let (week_from, week_to) = current_week(now);
        assert_eq!(from, week_from);
        assert_eq!(to, week_to);
        // «Сейчас» обязано лежать внутри окна по умолчанию, иначе экран
        // отчёта на старте показывал бы пустоту при идущем таймере.
        assert!(from.unwrap() <= now && now < to.unwrap());
    }

    #[test]
    fn week_default_starts_on_a_local_monday() {
        let (from, to) = current_week(Utc::now());
        let monday = from.unwrap().with_timezone(&Local);
        assert_eq!(monday.weekday(), chrono::Weekday::Mon);
        assert_eq!(to.unwrap() - from.unwrap(), Duration::days(7));
    }

    #[test]
    fn only_one_border_may_be_given() {
        // Keep the supplied upper bound inside the test's week, regardless
        // of the real date on which the suite runs.
        let now = crate::report::parse_ts("2026-09-08T12:00:00Z").unwrap();
        let (week_from, _) = current_week(now);
        let (from, to) = resolve_window(&q(None, Some("2026-09-09")), now).unwrap();
        assert_eq!(from, week_from);
        assert!(to.is_some());
    }

    #[test]
    fn unparsable_dates_are_rejected() {
        let now = Utc::now();
        assert_eq!(
            resolve_window(&q(Some("вчера"), None), now)
                .unwrap_err()
                .status,
            axum::http::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            resolve_window(&q(None, Some("2026-13-40")), now)
                .unwrap_err()
                .status,
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn inverted_window_is_rejected() {
        let now = Utc::now();
        let err = resolve_window(&q(Some("2026-09-10"), Some("2026-09-01")), now).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn unknown_group_by_is_not_silently_day() {
        assert!(ReportGroupBy::parse("weekday").is_none());
        assert!(ReportGroupBy::parse("day").is_some());
    }
}
