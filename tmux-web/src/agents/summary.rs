//! Historical queries over the authoritative normalized index.
//!
//! Usage belongs to the local day its provider reported it. Work intervals
//! are clipped to the requested dates; agent time sums independent tasks,
//! while wall time unions their overlapping intervals. Unknown/quiet tasks
//! stop accruing historical time at their last journal activity.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, ensure, Context};
use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};

use super::index::{task_at, AgentIndex};
use super::model::*;

#[derive(Clone)]
struct Period {
    from_ms: i64,
    to_ms: i64,
    days: Vec<(NaiveDate, i64, i64)>,
    weekly: bool,
}

fn midnight(date: NaiveDate) -> anyhow::Result<i64> {
    // Some zones advance their clocks at midnight. The first representable
    // minute is the start of that calendar day, not a fixed UTC offset.
    for minute in 0..180 {
        let local = date
            .and_hms_opt(minute / 60, minute % 60, 0)
            .context("invalid date")?;
        if let Some(date) = Local.from_local_datetime(&local).earliest() {
            return Ok(date.timestamp_millis());
        }
    }
    bail!("calendar day cannot be represented in server timezone")
}

fn parse_date(value: &str) -> anyhow::Result<NaiveDate> {
    ensure!(value.len() == 10, "dates must use YYYY-MM-DD");
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| anyhow!("invalid date; expected YYYY-MM-DD"))
}

fn period(query: &AgentQuery, now_ms: i64) -> anyhow::Result<Period> {
    let today = Local
        .timestamp_millis_opt(now_ms)
        .single()
        .context("invalid current time")?
        .date_naive();
    let from = query
        .from
        .as_deref()
        .map(parse_date)
        .transpose()?
        .unwrap_or(today);
    let to = query
        .to
        .as_deref()
        .map(parse_date)
        .transpose()?
        .unwrap_or(today);
    ensure!(from <= to, "from must not be after to");
    ensure!(
        to.signed_duration_since(from).num_days() < 3660,
        "date range exceeds 3660 days"
    );
    let weekly = match query.bucket.as_deref().unwrap_or("day") {
        "day" => false,
        "week" => true,
        _ => bail!("bucket must be day or week"),
    };
    let from_ms = midnight(from)?;
    let to_ms = midnight(to.succ_opt().context("date is out of range")?)?;
    let mut days = Vec::new();
    let mut date = from;
    while date <= to {
        let next = date.succ_opt().context("date is out of range")?;
        days.push((date, midnight(date)?, midnight(next)?));
        date = next;
    }
    Ok(Period {
        from_ms,
        to_ms,
        days,
        weekly,
    })
}

fn matches(provider: AgentProvider, profile: &str, project: &str, query: &AgentQuery) -> bool {
    query.provider.is_none_or(|p| p == provider)
        && query
            .profile
            .as_deref()
            .is_none_or(|p| p.is_empty() || p == profile)
        && query
            .project
            .as_deref()
            .is_none_or(|p| p.is_empty() || p == project)
}

fn day_key(date: NaiveDate, weekly: bool) -> String {
    let date = if weekly {
        date.checked_sub_days(Days::new(date.weekday().num_days_from_monday().into()))
            .unwrap_or(date)
    } else {
        date
    };
    date.format("%Y-%m-%d").to_string()
}

fn model_key(provider: AgentProvider, model: &str) -> String {
    serde_json::to_string(&(provider, model)).expect("string tuple")
}

fn model_name(model: &str) -> String {
    if model.is_empty() {
        "Неизвестная модель".into()
    } else {
        model.into()
    }
}

fn clipped(start: i64, end: i64, from: i64, to: i64) -> Option<(i64, i64)> {
    let start = start.max(from);
    let end = end.min(to);
    (end > start).then_some((start, end))
}

/// The provider duration and the journal's wall interval can differ. Distribute
/// duration proportionally and subtract cumulative fractions so day slices add
/// up exactly, including when the duration is not divisible by the interval.
fn duration_slice(task: &AgentTask, wall_end: i64, slice: (i64, i64)) -> u64 {
    let wall = task
        .ended_at_ms
        .unwrap_or(wall_end)
        .saturating_sub(task.started_at_ms)
        .max(0) as u64;
    let duration = task.duration_ms.unwrap_or(wall);
    if wall == 0 {
        return 0;
    }
    let scale = |at: i64| -> u64 {
        let elapsed = at.saturating_sub(task.started_at_ms).max(0) as u64;
        ((duration as u128 * elapsed.min(wall) as u128) / wall as u128) as u64
    };
    scale(slice.1).saturating_sub(scale(slice.0))
}

fn work_end(task: &AgentTask, now_ms: i64) -> i64 {
    task.ended_at_ms
        .unwrap_or_else(|| match task.state {
            AgentTaskState::Working => now_ms,
            _ => task.last_activity_at_ms,
        })
        .min(now_ms)
        .max(task.started_at_ms)
}

fn task_in_period(task: &AgentTask, period: &Period, now_ms: i64) -> bool {
    clipped(
        task.started_at_ms,
        work_end(task, now_ms),
        period.from_ms,
        period.to_ms,
    )
    .is_some()
        || task.started_at_ms >= period.from_ms
            && task.started_at_ms < period.to_ms
            && task.started_at_ms <= now_ms
        || task
            .ended_at_ms
            .is_some_and(|at| at >= period.from_ms && at < period.to_ms && at <= now_ms)
}

#[derive(Default)]
struct Accumulator {
    totals: AgentTotals,
    intervals: Vec<(i64, i64)>,
}

impl Accumulator {
    fn task(&mut self, task: &AgentTask, from: i64, to: i64, now_ms: i64) {
        let end = work_end(task, now_ms);
        if task.started_at_ms >= from && task.started_at_ms < to && task.started_at_ms <= now_ms {
            self.totals.tasks = self.totals.tasks.saturating_add(1);
        }
        if task
            .ended_at_ms
            .is_some_and(|at| at >= from && at < to && at <= now_ms)
        {
            match task.completed {
                Some(true) => self.totals.completed = self.totals.completed.saturating_add(1),
                Some(false) => self.totals.aborted = self.totals.aborted.saturating_add(1),
                None => {}
            }
        }
        if task.completed.is_none()
            && (task.started_at_ms >= from && task.started_at_ms < to
                || clipped(task.started_at_ms, end, from, to).is_some())
        {
            self.totals.incomplete_tasks = self.totals.incomplete_tasks.saturating_add(1);
        }
        if let Some(interval) = clipped(task.started_at_ms, end, from, to) {
            self.totals.agent_time_ms = self
                .totals
                .agent_time_ms
                .saturating_add(duration_slice(task, end, interval));
            self.intervals.push(interval);
        }
    }

    fn usage(&mut self, record: &UsageRecord) {
        self.totals.requests = self.totals.requests.saturating_add(record.requests);
        if let Some(tokens) = &record.tokens {
            self.totals
                .tokens
                .get_or_insert_with(AgentTokens::default)
                .add(tokens);
        } else {
            self.totals.unknown_token_requests = self
                .totals
                .unknown_token_requests
                .saturating_add(record.requests);
        }
        // These columns describe separate sources of the same spend. Never add
        // a reported charge to an API estimate to manufacture a combined cost.
        let estimated = record
            .estimated_cost_usd
            .filter(|c| c.is_finite() && *c >= 0.0);
        let reported = record
            .reported_cost_usd
            .filter(|c| c.is_finite() && *c >= 0.0);
        if let Some(cost) = estimated {
            *self.totals.estimated_cost_usd.get_or_insert(0.0) += cost;
        }
        if let Some(cost) = reported {
            *self.totals.reported_cost_usd.get_or_insert(0.0) += cost;
        }
        if estimated.is_none() && reported.is_none() {
            self.totals.unpriced_requests = self
                .totals
                .unpriced_requests
                .saturating_add(record.requests.max(u64::from(record.tokens.is_some())));
        }
    }

    fn finish(mut self) -> AgentTotals {
        self.intervals.sort_unstable();
        let mut union = 0u64;
        let mut current: Option<(i64, i64)> = None;
        for interval in self.intervals {
            match &mut current {
                Some((_, end)) if interval.0 <= *end => *end = (*end).max(interval.1),
                Some((start, end)) => {
                    union = union.saturating_add(end.saturating_sub(*start).max(0) as u64);
                    current = Some(interval);
                }
                None => current = Some(interval),
            }
        }
        if let Some((start, end)) = current {
            union = union.saturating_add(end.saturating_sub(start).max(0) as u64);
        }
        self.totals.wall_time_ms = union;
        self.totals
    }
}

struct ShareAccumulator {
    name: String,
    provider: Option<AgentProvider>,
    data: Accumulator,
}

fn share<'a>(
    map: &'a mut BTreeMap<String, ShareAccumulator>,
    key: String,
    name: String,
    provider: Option<AgentProvider>,
) -> &'a mut Accumulator {
    &mut map
        .entry(key)
        .or_insert_with(|| ShareAccumulator {
            name,
            provider,
            data: Accumulator::default(),
        })
        .data
}

fn finish_shares(map: BTreeMap<String, ShareAccumulator>) -> Vec<AgentShare> {
    let mut shares: Vec<_> = map
        .into_iter()
        .map(|(id, data)| AgentShare {
            id,
            name: data.name,
            provider: data.provider,
            totals: data.data.finish(),
        })
        .collect();
    shares.sort_by(|a, b| {
        b.totals
            .tokens
            .as_ref()
            .map(AgentTokens::total)
            .unwrap_or(0)
            .cmp(
                &a.totals
                    .tokens
                    .as_ref()
                    .map(AgentTokens::total)
                    .unwrap_or(0),
            )
            .then_with(|| b.totals.requests.cmp(&a.totals.requests))
            .then_with(|| a.id.cmp(&b.id))
    });
    shares
}

/// Summarize usage and task intervals under one validated local-date filter.
/// Queries are read-only and never trigger scanning or mutate the index.
pub fn summary(
    index: &AgentIndex,
    query: &AgentQuery,
    now_ms: i64,
    updated_at_ms: i64,
    pricing: PriceStatus,
) -> anyhow::Result<AgentSummary> {
    let period = period(query, now_ms)?;
    let mut total = Accumulator::default();
    let mut providers: BTreeMap<AgentProvider, Accumulator> = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut projects = BTreeMap::new();
    let mut series: BTreeMap<String, Accumulator> = BTreeMap::new();
    let mut bucket_ranges: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for (date, from, to) in &period.days {
        let key = day_key(*date, period.weekly);
        series.entry(key.clone()).or_default();
        bucket_ranges
            .entry(key)
            .and_modify(|range| range.1 = *to)
            .or_insert((*from, *to));
    }

    for raw in &index.tasks {
        let task = task_at(raw, now_ms);
        if !matches(task.provider, &task.profile, &task.project, query)
            || !task_in_period(&task, &period, now_ms)
        {
            continue;
        }
        total.task(&task, period.from_ms, period.to_ms, now_ms);
        providers.entry(task.provider).or_default().task(
            &task,
            period.from_ms,
            period.to_ms,
            now_ms,
        );
        share(
            &mut models,
            model_key(task.provider, &task.model),
            model_name(&task.model),
            Some(task.provider),
        )
        .task(&task, period.from_ms, period.to_ms, now_ms);
        if !task.project.is_empty() {
            share(
                &mut projects,
                task.project.clone(),
                task.project.clone(),
                None,
            )
            .task(&task, period.from_ms, period.to_ms, now_ms);
        }
        for (key, (from, to)) in &bucket_ranges {
            series
                .entry(key.clone())
                .or_default()
                .task(&task, *from, *to, now_ms);
        }
    }
    for record in &index.usage {
        if record.at_ms < period.from_ms
            || record.at_ms >= period.to_ms
            || record.at_ms > now_ms
            || !matches(record.provider, &record.profile, &record.project, query)
        {
            continue;
        }
        total.usage(record);
        providers.entry(record.provider).or_default().usage(record);
        share(
            &mut models,
            model_key(record.provider, &record.model),
            model_name(&record.model),
            Some(record.provider),
        )
        .usage(record);
        if !record.project.is_empty() {
            share(
                &mut projects,
                record.project.clone(),
                record.project.clone(),
                None,
            )
            .usage(record);
        }
        if let Some(local) = Local.timestamp_millis_opt(record.at_ms).single() {
            series
                .entry(day_key(local.date_naive(), period.weekly))
                .or_default()
                .usage(record);
        }
    }
    Ok(AgentSummary {
        updated_at_ms,
        totals: total.finish(),
        by_provider: providers
            .into_iter()
            .map(|(p, a)| (p, a.finish()))
            .collect(),
        models: finish_shares(models),
        projects: finish_shares(projects),
        series: series
            .into_iter()
            .map(|(start, a)| AgentBucket {
                start,
                totals: a.finish(),
            })
            .collect(),
        pricing,
    })
}

/// Return task history newest first. Pagination applies after every filter.
pub fn sessions(
    index: &AgentIndex,
    query: &AgentQuery,
    now_ms: i64,
) -> anyhow::Result<Vec<AgentTask>> {
    let period = period(query, now_ms)?;
    let limit = query.limit.unwrap_or(200).min(500);
    let offset = query.offset.unwrap_or(0);
    ensure!(
        offset.checked_add(limit).is_some(),
        "pagination offset is out of range"
    );
    let mut tasks: Vec<_> = index
        .tasks
        .iter()
        .map(|task| {
            let mut task = task_at(task, now_ms);
            if task.ended_at_ms.is_none() && task.state != AgentTaskState::Working {
                task.elapsed_ms = task
                    .last_activity_at_ms
                    .saturating_sub(task.started_at_ms)
                    .max(0) as u64;
            }
            task
        })
        .filter(|task| {
            matches(task.provider, &task.profile, &task.project, query)
                && task_in_period(task, &period, now_ms)
        })
        .collect();
    tasks.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(tasks.into_iter().skip(offset).take(limit).collect())
}

/// Project IDs and names are full canonical paths, keeping equal basenames apart.
pub fn projects(
    index: &AgentIndex,
    query: &AgentQuery,
    now_ms: i64,
) -> anyhow::Result<Vec<AgentShare>> {
    Ok(summary(index, query, now_ms, 0, PriceStatus::default())?.projects)
}

#[cfg(test)]
mod tests {
    use super::super::index::fold;
    use super::*;

    fn at(day: &str, h: u32, m: u32) -> i64 {
        Local
            .from_local_datetime(&parse_date(day).unwrap().and_hms_opt(h, m, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp_millis()
    }
    fn query(from: &str, to: &str) -> AgentQuery {
        AgentQuery {
            from: Some(from.into()),
            to: Some(to.into()),
            ..Default::default()
        }
    }
    fn task(id: &str, provider: AgentProvider, start: i64, end: i64) -> AgentTask {
        let event = |at, kind| AgentEvent {
            id: format!("{id}:{at}"),
            at_ms: at,
            sequence: 0,
            session: AgentSessionId {
                provider,
                profile: "/profile".into(),
                native_id: id.into(),
            },
            parent: None,
            project: "/one/project".into(),
            model: "model".into(),
            kind,
        };
        fold([
            event(
                start,
                AgentEventKind::Started {
                    task_id: "task".into(),
                },
            ),
            event(
                end,
                AgentEventKind::Ended {
                    completed: true,
                    duration_ms: None,
                },
            ),
        ])
        .tasks
        .remove(0)
    }
    fn record(id: &str, provider: AgentProvider, time: i64) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            session_id: id.into(),
            root_session_id: id.into(),
            provider,
            profile: "/profile".into(),
            project: "/one/project".into(),
            model: "model".into(),
            at_ms: time,
            tokens: Some(AgentTokens {
                input: 10,
                output: 20,
                reasoning: Some(15),
                ..Default::default()
            }),
            requests: 1,
            estimated_cost_usd: Some(0.5),
            reported_cost_usd: Some(0.7),
            fast: false,
            cache_write_1h: None,
        }
    }

    #[test]
    fn parallel_tasks_have_distinct_sum_and_union() {
        let start = at("2026-10-25", 12, 0);
        let end = start + 600_000;
        let index = AgentIndex {
            tasks: vec![
                task("a", AgentProvider::Codex, start, end),
                task("b", AgentProvider::Claude, start, end),
            ],
            usage: Vec::new(),
        };
        let result = summary(
            &index,
            &query("2026-10-25", "2026-10-25"),
            end,
            1,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(result.totals.agent_time_ms, 1_200_000);
        assert_eq!(result.totals.wall_time_ms, 600_000);
        assert_eq!(result.totals.tasks, 2);
        assert_eq!(result.totals.completed, 2);
        assert_eq!(result.series[0].totals.wall_time_ms, 600_000);
    }

    #[test]
    fn dates_clip_time_and_provider_duration_is_prorated() {
        let start = at("2026-10-24", 23, 50);
        let end = at("2026-10-25", 0, 10);
        let mut t = task("a", AgentProvider::Codex, start, end);
        t.duration_ms = Some(600_000);
        let index = AgentIndex {
            tasks: vec![t],
            usage: Vec::new(),
        };
        let now = at("2026-10-25", 12, 0);
        let result = summary(
            &index,
            &query("2026-10-25", "2026-10-25"),
            now,
            1,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(result.totals.agent_time_ms, 300_000);
        assert_eq!(result.totals.wall_time_ms, 600_000);
        assert_eq!(result.totals.tasks, 0);
        assert_eq!(result.totals.completed, 1);
        let full = summary(
            &index,
            &query("2026-10-24", "2026-10-25"),
            now,
            1,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(full.totals.agent_time_ms, 600_000);
        assert_eq!(
            full.series
                .iter()
                .map(|b| b.totals.agent_time_ms)
                .sum::<u64>(),
            600_000
        );
    }

    #[test]
    fn every_aggregate_respects_provider_profile_and_project_filters() {
        let start = at("2026-10-25", 10, 0);
        let end = start + 600_000;
        let a = task("a", AgentProvider::Codex, start, end);
        let mut b = task("b", AgentProvider::Claude, start, end);
        b.profile = "/other".into();
        b.project = "/two/project".into();
        let mut ra = record("a", AgentProvider::Codex, start);
        let mut rb = record("b", AgentProvider::Claude, start);
        rb.profile = b.profile.clone();
        rb.project = b.project.clone();
        ra.model = "codex-model".into();
        let index = AgentIndex {
            tasks: vec![a, b],
            usage: vec![ra, rb],
        };
        for q in [
            AgentQuery {
                provider: Some(AgentProvider::Codex),
                ..query("2026-10-25", "2026-10-25")
            },
            AgentQuery {
                profile: Some("/profile".into()),
                ..query("2026-10-25", "2026-10-25")
            },
            AgentQuery {
                project: Some("/one/project".into()),
                ..query("2026-10-25", "2026-10-25")
            },
        ] {
            let result = summary(&index, &q, end, 0, PriceStatus::default()).unwrap();
            assert_eq!(result.totals.tasks, 1);
            assert_eq!(result.totals.requests, 1);
            assert_eq!(result.totals.tokens.unwrap().total(), 30);
            assert_eq!(result.projects.len(), 1);
            assert_eq!(result.by_provider.len(), 1);
            assert_eq!(result.series[0].totals.requests, 1);
            assert!(result
                .models
                .iter()
                .all(|m| m.provider == Some(AgentProvider::Codex)));
            assert_eq!(sessions(&index, &q, end).unwrap().len(), 1);
        }
    }

    #[test]
    fn unknown_metrics_are_nullable_and_cost_sources_are_separate() {
        let time = at("2026-10-25", 12, 0);
        let mut known = record("known", AgentProvider::Codex, time);
        known.estimated_cost_usd = Some(0.5);
        known.reported_cost_usd = Some(0.7);
        let mut unknown = record("unknown", AgentProvider::Copilot, time);
        unknown.tokens = None;
        unknown.estimated_cost_usd = None;
        unknown.reported_cost_usd = None;
        let q = query("2026-10-25", "2026-10-25");
        let result = summary(
            &AgentIndex {
                tasks: Vec::new(),
                usage: vec![unknown.clone()],
            },
            &q,
            time,
            0,
            PriceStatus::default(),
        )
        .unwrap();
        assert!(result.totals.tokens.is_none());
        assert!(result.totals.estimated_cost_usd.is_none());
        assert_eq!(result.totals.unknown_token_requests, 1);
        let result = summary(
            &AgentIndex {
                tasks: Vec::new(),
                usage: vec![known, unknown],
            },
            &q,
            time,
            0,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(result.totals.estimated_cost_usd, Some(0.5));
        assert_eq!(result.totals.reported_cost_usd, Some(0.7));
        assert_eq!(result.totals.tokens.unwrap().total(), 30);
        assert_eq!(result.totals.unpriced_requests, 1);
    }

    #[test]
    fn waiting_unknown_tasks_do_not_accrue_endless_time() {
        let start = at("2026-10-25", 10, 0);
        let mut t = task("a", AgentProvider::Claude, start, start + 60_000);
        t.ended_at_ms = None;
        t.duration_ms = None;
        t.completed = None;
        t.state = AgentTaskState::Working;
        t.last_activity_at_ms = start + 60_000;
        let index = AgentIndex {
            tasks: vec![t],
            usage: Vec::new(),
        };
        let now = at("2026-10-25", 12, 0);
        let result = summary(
            &index,
            &query("2026-10-25", "2026-10-25"),
            now,
            0,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(result.totals.agent_time_ms, 60_000);
        assert_eq!(result.totals.incomplete_tasks, 1);
        assert_eq!(
            sessions(&index, &query("2026-10-25", "2026-10-25"), now).unwrap()[0].state,
            AgentTaskState::Unknown
        );
    }

    #[test]
    fn projects_keep_identical_basenames_distinct_and_history_paginates() {
        let start = at("2026-10-25", 10, 0);
        let a = task("a", AgentProvider::Codex, start, start + 10);
        let mut b = task("b", AgentProvider::Codex, start + 100, start + 110);
        b.project = "/two/project".into();
        let index = AgentIndex {
            tasks: vec![a, b],
            usage: Vec::new(),
        };
        let q = query("2026-10-25", "2026-10-25");
        let now = start + 1000;
        let p = projects(&index, &q, now).unwrap();
        assert_eq!(p.len(), 2);
        assert_ne!(p[0].id, p[1].id);
        let page = sessions(
            &index,
            &AgentQuery {
                limit: Some(1),
                offset: Some(1),
                ..q
            },
            now,
        )
        .unwrap();
        assert_eq!(page[0].native_id, "a");
    }

    #[test]
    fn weekly_series_and_validation() {
        let now = at("2026-10-25", 12, 0);
        let q = AgentQuery {
            bucket: Some("week".into()),
            ..query("2026-10-19", "2026-10-25")
        };
        let r = summary(&AgentIndex::default(), &q, now, 0, PriceStatus::default()).unwrap();
        assert_eq!(r.series.len(), 1);
        assert_eq!(r.series[0].start, "2026-10-19");
        for q in [
            query("invalid", "2026-10-25"),
            query("2026-10-26", "2026-10-25"),
            query("2000-01-01", "2026-10-25"),
            AgentQuery {
                bucket: Some("hour".into()),
                ..query("2026-10-25", "2026-10-25")
            },
        ] {
            assert!(summary(&AgentIndex::default(), &q, now, 0, PriceStatus::default()).is_err());
        }
    }

    #[test]
    fn a_completion_at_midnight_belongs_to_the_next_day() {
        let end = at("2026-10-25", 0, 0);
        let index = AgentIndex {
            tasks: vec![task("a", AgentProvider::Codex, end - 600_000, end)],
            usage: Vec::new(),
        };
        let result = summary(
            &index,
            &query("2026-10-25", "2026-10-25"),
            end + 1000,
            0,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(result.totals.completed, 1);
        assert_eq!(result.totals.wall_time_ms, 0);
        assert_eq!(result.totals.agent_time_ms, 0);
        assert_eq!(
            sessions(&index, &query("2026-10-25", "2026-10-25"), end + 1000)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn fractional_duration_slices_add_to_the_provider_duration() {
        let midnight = at("2026-10-25", 0, 0);
        let mut t = task("a", AgentProvider::Codex, midnight - 1, midnight + 2);
        t.duration_ms = Some(2);
        let index = AgentIndex {
            tasks: vec![t],
            usage: Vec::new(),
        };
        let result = summary(
            &index,
            &query("2026-10-24", "2026-10-25"),
            midnight + 10,
            0,
            PriceStatus::default(),
        )
        .unwrap();
        assert_eq!(result.totals.agent_time_ms, 2);
        assert_eq!(
            result
                .series
                .iter()
                .map(|b| b.totals.agent_time_ms)
                .sum::<u64>(),
            2
        );
    }

    #[test]
    fn replayed_source_contributions_do_not_change_summary_totals() {
        let start = at("2026-10-25", 10, 0);
        let end = start + 600_000;
        let event = |time, kind| AgentEvent {
            id: format!("{time}:{kind:?}"),
            at_ms: time,
            sequence: 0,
            session: AgentSessionId {
                provider: AgentProvider::Codex,
                profile: "/p".into(),
                native_id: "s".into(),
            },
            parent: None,
            project: "/project".into(),
            model: "m".into(),
            kind,
        };
        let events = vec![
            event(
                start,
                AgentEventKind::Started {
                    task_id: "a".into(),
                },
            ),
            event(
                start + 1,
                AgentEventKind::Usage {
                    response_id: "native:s:r".into(),
                    tokens: Some(AgentTokens {
                        input: 10,
                        output: 20,
                        reasoning: Some(15),
                        ..Default::default()
                    }),
                    requests: 1,
                    reported_cost_usd: None,
                    fast: false,
                    cumulative: false,
                    cache_write_1h: None,
                },
            ),
            event(
                end,
                AgentEventKind::Ended {
                    completed: true,
                    duration_ms: None,
                },
            ),
        ];
        let one = fold(events.clone());
        let two = fold(events.iter().cloned().chain(events.iter().cloned()));
        let q = query("2026-10-25", "2026-10-25");
        let a = summary(&one, &q, end, 0, PriceStatus::default()).unwrap();
        let b = summary(&two, &q, end, 0, PriceStatus::default()).unwrap();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }
}
