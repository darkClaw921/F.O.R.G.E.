//! Расчёты поверх отрезков времени: суммы, отчёт и данные Гантта.
//!
//! Модуль СОЗНАТЕЛЬНО не знает про [`crate::store::TrackerStore`] и про
//! блокировки: все функции здесь чистые и работают со срезами моделей. Так
//! их можно проверить точечно, не собирая стор и не трогая диск, а сам стор
//! остаётся тонкой обёрткой, которая берёт `read()` и делегирует сюда.
//!
//! # Правило суток
//!
//! Дни считаются в **локальной зоне машины**, а не в UTC. Отчёт «сколько я
//! сегодня наработал» — вещь бытовая: пользователь в Москве, начавший в 02:00,
//! обязан увидеть время в сегодняшнем дне, а не во вчерашнем. Отрезок,
//! пересекающий локальную полночь, РАЗРЕЗАЕТСЯ по границе и раскладывается в
//! оба дня пропорционально фактическому пересечению (см.
//! [`split_by_local_day`]).
//!
//! # Правило активного отрезка
//!
//! Отрезок с `stopped_at == None` считается идущим и учитывается «до сейчас».
//! Момент «сейчас» во всех функциях передаётся ЯВНЫМ параметром `now`, а не
//! берётся из `Utc::now()` внутри: иначе расчёты были бы непроверяемы тестом.
//!
//! # Правило чек-листа
//!
//! Время пункта чек-листа — это обычный [`TimeEntry`] с `item_id = Some(..)`
//! и тем же `task_id`. Никакой отдельной суммы по пунктам нет, поэтому в
//! общее время задачи оно попадает ровно один раз ПО ПОСТРОЕНИЮ, а не по
//! договорённости.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::models::{Milestone, TaskList, TaskStatus, TimeEntry, TrackerTask};

/// Ось группировки отчёта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportGroupBy {
    /// По календарным дням (локальная зона).
    Day,
    /// По задачам.
    Task,
    /// По спискам; задачи без списка сводятся в строку с пустым ключом.
    List,
}

impl ReportGroupBy {
    /// Разбор значения query-параметра `group_by`; неизвестное значение —
    /// `None`, чтобы роут ответил `400`, а не молча посчитал не то.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "day" => Some(ReportGroupBy::Day),
            "task" => Some(ReportGroupBy::Task),
            "list" => Some(ReportGroupBy::List),
            _ => None,
        }
    }

    /// Стабильное строковое имя (то же, что в JSON).
    pub fn as_str(self) -> &'static str {
        match self {
            ReportGroupBy::Day => "day",
            ReportGroupBy::Task => "task",
            ReportGroupBy::List => "list",
        }
    }
}

/// Строка отчёта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRow {
    /// Ключ группы: дата `YYYY-MM-DD`, `id` задачи или `id` списка
    /// (пустая строка — «без списка»).
    pub key: String,
    /// Человекочитаемая подпись группы.
    pub label: String,
    /// Сумма в секундах.
    pub seconds: i64,
}

/// Результат отчёта за период.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Начало периода (включительно), RFC 3339, или `None`.
    pub from: Option<String>,
    /// Конец периода (исключительно), RFC 3339, или `None`.
    pub to: Option<String>,
    /// Ось группировки.
    pub group_by: ReportGroupBy,
    /// Сумма по всем строкам.
    pub total_seconds: i64,
    /// Строки, отсортированные по ключу (для `day`) либо по убыванию суммы.
    pub rows: Vec<ReportRow>,
    /// Шапка отчёта: числа, которые НЕ зависят от оси группировки.
    pub kpi: ReportKpi,
}

/// Шапка сводки — четыре числа экрана отчёта.
///
/// # Почему это считает сервер, а не экран
///
/// Каждое из четырёх чисел упирается в правило суток или в правило активного
/// отрезка: среднее делится на календарные дни ЛОКАЛЬНОЙ зоны, «самый долгий
/// фокус» это отрезок, обрезанный окном и досчитанный до `now`, а «закрыто»
/// смотрит на `done_at` внутри окна. Повтори эти правила на фронте — и через
/// одну правку тут они разойдутся молча. Поэтому шапка приезжает готовой в том
/// же ответе, что и строки, и никакой оси группировки не требует: KPI за
/// период одинаковы и для `day`, и для `task`, и для `list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportKpi {
    /// `// TOTAL` — сумма за период (та же, что `Report::total_seconds`).
    pub total_seconds: i64,
    /// Сколько календарных суток в окне; минимум 1, чтобы среднее не делилось
    /// на ноль.
    pub days: i64,
    /// `// AVG/DAY` — `total_seconds / days`, целочисленно.
    pub avg_day_seconds: i64,
    /// Сумма за ПРЕДЫДУЩЕЕ окно той же длины (для подписи «+3:12 к прошлой»).
    /// `None`, если окно открыто хотя бы с одной стороны и сдвинуть его не от
    /// чего.
    pub prev_total_seconds: Option<i64>,
    /// `// CLOSED`, числитель: задач с `done_at` внутри окна.
    pub closed_tasks: i64,
    /// `// CLOSED`, знаменатель: задач, ЗАТРОНУТЫХ в окне — по которым было
    /// время либо которые в окне закрыли. Не «все задачи стора»: доля
    /// закрытого от вечно растущего inbox ничего не означала бы.
    pub touched_tasks: i64,
    /// `// MAX FOCUS` — самый длинный ОДИН отрезок окна, обрезанный его
    /// границами.
    pub max_focus_seconds: i64,
    /// Задача этого отрезка.
    pub max_focus_task_id: Option<String>,
    /// Локальный день его начала (`YYYY-MM-DD`) — подпись «вт · …».
    pub max_focus_day: Option<String>,
}

/// Полоса задачи на диаграмме Гантта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GanttBar {
    /// `id` задачи.
    pub task_id: String,
    /// Заголовок задачи.
    pub title: String,
    /// Список задачи или `None`.
    pub list_id: Option<String>,
    /// Статус (он же колонка канбана).
    pub status: TaskStatus,
    /// Плановое начало как задано в задаче.
    pub start: Option<String>,
    /// Срок как задан в задаче.
    pub due: Option<String>,
    /// Оценка в минутах.
    pub estimate_min: Option<u32>,
    /// Факт: сумма всех отрезков задачи, включая отрезки пунктов чек-листа.
    pub actual_seconds: i64,
    /// Факт превысил оценку. При отсутствии оценки — всегда `false`:
    /// «превышение» без плана бессмысленно.
    pub over_estimate: bool,
}

/// Данные Гантта за период.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GanttData {
    /// Начало окна (RFC 3339) или `None`.
    pub from: Option<String>,
    /// Конец окна (RFC 3339) или `None`.
    pub to: Option<String>,
    /// Полосы задач.
    pub bars: Vec<GanttBar>,
    /// Вехи, попавшие в окно.
    pub milestones: Vec<Milestone>,
}

/// Суммы для статус-бара UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Секунды за текущие локальные сутки.
    pub today_seconds: i64,
    /// Секунды за текущую локальную неделю (с понедельника).
    pub week_seconds: i64,
}

/// Разбор отметки времени.
///
/// Принимает как полный RFC 3339 (`2026-09-08T10:00:00Z`), так и голую дату
/// `YYYY-MM-DD` — последняя нужна полям `start`/`due` задачи и `date` вехи,
/// где время суток смысла не имеет и трактуется как локальная полночь.
pub fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    local_day_start(date)
}

/// Момент локальной полуночи указанной даты в UTC.
///
/// В час перевода стрелок локальной полуночи может не существовать
/// (`LocalResult::None`) — тогда берётся ближайший существующий момент часом
/// позже. Разрыв в один час на границе суток безопаснее, чем паника в отчёте.
pub fn local_day_start(date: NaiveDate) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    if let Some(dt) = Local.from_local_datetime(&naive).earliest() {
        return Some(dt.with_timezone(&Utc));
    }
    let shifted = naive + Duration::hours(1);
    Local
        .from_local_datetime(&shifted)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Границы отрезка: `(начало, конец)`, где у идущего таймера конец — `now`.
///
/// `None`, если отметки не разбираются или конец не позже начала (такой
/// отрезок в суммы не попадает вовсе).
pub fn entry_span(entry: &TimeEntry, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = parse_ts(&entry.started_at)?;
    let end = match entry.stopped_at.as_deref() {
        Some(s) => parse_ts(s)?,
        None => now,
    };
    if end <= start {
        return None;
    }
    Some((start, end))
}

/// Длительность отрезка в секундах с учётом окна `[from, to)`.
///
/// Границы окна необязательны: `None` означает «без ограничения с этой
/// стороны».
pub fn entry_seconds_in_window(
    entry: &TimeEntry,
    now: DateTime<Utc>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> i64 {
    let Some((mut start, mut end)) = entry_span(entry, now) else {
        return 0;
    };
    if let Some(f) = from {
        start = start.max(f);
    }
    if let Some(t) = to {
        end = end.min(t);
    }
    if end <= start {
        return 0;
    }
    (end - start).num_seconds()
}

/// Раскладывает отрезок по локальным календарным дням.
///
/// Отрезок, пересекающий локальную полночь, режется по границе: вернётся
/// несколько пар `(дата, секунды)`, сумма которых равна полной длительности
/// пересечения с окном.
pub fn split_by_local_day(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<(NaiveDate, i64)> {
    let mut out: Vec<(NaiveDate, i64)> = Vec::new();
    let mut cursor = start;
    // Защита от патологических данных (отрезок длиной в столетия): цикл
    // ограничен сверху, потому что каждая итерация продвигает курсор минимум
    // на сутки, а без потолка ошибка во входных данных повесила бы отчёт.
    let mut guard = 0;
    while cursor < end && guard < 10_000 {
        guard += 1;
        let date = cursor.with_timezone(&Local).date_naive();
        let next_day = date
            .succ_opt()
            .and_then(local_day_start)
            .unwrap_or(end);
        let chunk_end = next_day.min(end);
        if chunk_end <= cursor {
            // Локальная полночь не продвинулась (аномалия зоны) — досчитываем
            // остаток в текущий день и выходим.
            out.push((date, (end - cursor).num_seconds()));
            break;
        }
        out.push((date, (chunk_end - cursor).num_seconds()));
        cursor = chunk_end;
    }
    out
}

/// Суммарное время задачи в секундах — по ВСЕМ её отрезкам, включая отрезки
/// пунктов чек-листа.
pub fn task_seconds(entries: &[TimeEntry], task_id: &str, now: DateTime<Utc>) -> i64 {
    entries
        .iter()
        .filter(|e| e.task_id == task_id)
        .map(|e| entry_seconds_in_window(e, now, None, None))
        .sum()
}

/// Суммарное время конкретного пункта чек-листа в секундах.
pub fn item_seconds(entries: &[TimeEntry], item_id: &str, now: DateTime<Utc>) -> i64 {
    entries
        .iter()
        .filter(|e| e.item_id.as_deref() == Some(item_id))
        .map(|e| entry_seconds_in_window(e, now, None, None))
        .sum()
}

/// Строит отчёт за период по выбранной оси.
///
/// * `from` включительно, `to` исключительно; обе границы необязательны.
/// * Для оси `day` строки отсортированы по дате, для остальных — по убыванию
///   суммы (в UI это сразу «на что ушло больше всего»).
pub fn build_report(
    tasks: &[TrackerTask],
    lists: &[TaskList],
    entries: &[TimeEntry],
    group_by: ReportGroupBy,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Report {
    let mut buckets: HashMap<String, i64> = HashMap::new();

    for entry in entries {
        match group_by {
            ReportGroupBy::Day => {
                let Some((mut s, mut e)) = entry_span(entry, now) else {
                    continue;
                };
                if let Some(f) = from {
                    s = s.max(f);
                }
                if let Some(t) = to {
                    e = e.min(t);
                }
                if e <= s {
                    continue;
                }
                for (date, secs) in split_by_local_day(s, e) {
                    *buckets.entry(date.format("%Y-%m-%d").to_string()).or_insert(0) += secs;
                }
            }
            ReportGroupBy::Task => {
                let secs = entry_seconds_in_window(entry, now, from, to);
                if secs > 0 {
                    *buckets.entry(entry.task_id.clone()).or_insert(0) += secs;
                }
            }
            ReportGroupBy::List => {
                let secs = entry_seconds_in_window(entry, now, from, to);
                if secs == 0 {
                    continue;
                }
                let list_id = tasks
                    .iter()
                    .find(|t| t.id == entry.task_id)
                    .and_then(|t| t.list_id.clone())
                    .unwrap_or_default();
                *buckets.entry(list_id).or_insert(0) += secs;
            }
        }
    }

    let mut rows: Vec<ReportRow> = buckets
        .into_iter()
        .map(|(key, seconds)| {
            let label = match group_by {
                ReportGroupBy::Day => key.clone(),
                ReportGroupBy::Task => tasks
                    .iter()
                    .find(|t| t.id == key)
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| key.clone()),
                ReportGroupBy::List => {
                    if key.is_empty() {
                        "без списка".to_string()
                    } else {
                        lists
                            .iter()
                            .find(|l| l.id == key)
                            .map(|l| l.name.clone())
                            .unwrap_or_else(|| key.clone())
                    }
                }
            };
            ReportRow {
                key,
                label,
                seconds,
            }
        })
        .collect();

    match group_by {
        ReportGroupBy::Day => rows.sort_by(|a, b| a.key.cmp(&b.key)),
        _ => rows.sort_by(|a, b| b.seconds.cmp(&a.seconds).then(a.label.cmp(&b.label))),
    }

    Report {
        from: from.map(|d| d.to_rfc3339()),
        to: to.map(|d| d.to_rfc3339()),
        group_by,
        total_seconds: rows.iter().map(|r| r.seconds).sum(),
        rows,
        kpi: build_kpi(tasks, entries, from, to, now),
    }
}

/// Сколько календарных ЛОКАЛЬНЫХ суток покрывает окно.
///
/// Окно полуоткрытое, поэтому последним считается день момента `to - 1 сек`:
/// `[08.09 00:00, 09.09 00:00)` — это одни сутки, а не двое. Открытое окно
/// суток не задаёт вовсе (`None`) — считать среднее «в день» там не от чего.
fn window_days(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> Option<i64> {
    let (f, t) = (from?, to?);
    if t <= f {
        return Some(1);
    }
    let first = f.with_timezone(&Local).date_naive();
    let last = (t - Duration::seconds(1)).with_timezone(&Local).date_naive();
    Some(((last - first).num_days() + 1).max(1))
}

/// Считает шапку сводки за окно.
///
/// Все правила — те же, что у строк отчёта: активный отрезок идёт «до `now`»,
/// границы окна режут отрезок, дни локальные. Функция чистая: `now` приходит
/// параметром.
pub fn build_kpi(
    tasks: &[TrackerTask],
    entries: &[TimeEntry],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> ReportKpi {
    let mut total = 0i64;
    let mut per_task: HashMap<&str, i64> = HashMap::new();
    let mut max_focus = 0i64;
    let mut max_task: Option<String> = None;
    let mut max_day: Option<String> = None;
    // Дни с активностью нужны только открытому окну: там календарного
    // знаменателя нет, и «в день» считается по дням, когда работа шла.
    let mut active_days: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in entries {
        let secs = entry_seconds_in_window(entry, now, from, to);
        if secs <= 0 {
            continue;
        }
        total += secs;
        *per_task.entry(entry.task_id.as_str()).or_insert(0) += secs;
        if secs > max_focus {
            max_focus = secs;
            max_task = Some(entry.task_id.clone());
            let start = entry_span(entry, now)
                .map(|(s, _)| from.map_or(s, |f| s.max(f)))
                .unwrap_or(now);
            max_day = Some(
                start
                    .with_timezone(&Local)
                    .date_naive()
                    .format("%Y-%m-%d")
                    .to_string(),
            );
        }
        if let Some((mut s, mut e)) = entry_span(entry, now) {
            if let Some(f) = from {
                s = s.max(f);
            }
            if let Some(t) = to {
                e = e.min(t);
            }
            for (date, _) in split_by_local_day(s, e) {
                active_days.insert(date.format("%Y-%m-%d").to_string());
            }
        }
    }

    let in_window = |dt: DateTime<Utc>| {
        from.map(|f| dt >= f).unwrap_or(true) && to.map(|t| dt < t).unwrap_or(true)
    };
    let mut closed = 0i64;
    let mut touched = 0i64;
    for task in tasks {
        let done_here = task
            .done_at
            .as_deref()
            .and_then(parse_ts)
            .is_some_and(in_window);
        if done_here {
            closed += 1;
        }
        if done_here || per_task.get(task.id.as_str()).copied().unwrap_or(0) > 0 {
            touched += 1;
        }
    }

    let days = window_days(from, to)
        .unwrap_or_else(|| active_days.len() as i64)
        .max(1);

    // Прошлое окно — ровно такой же длины и встык к текущему. Считать его
    // осмысленно только у закрытого окна: у «всего времени» прошлого нет.
    let prev_total = match (from, to) {
        (Some(f), Some(t)) if t > f => {
            let len = t - f;
            Some(
                entries
                    .iter()
                    .map(|e| entry_seconds_in_window(e, now, Some(f - len), Some(f)))
                    .sum(),
            )
        }
        _ => None,
    };

    ReportKpi {
        total_seconds: total,
        days,
        avg_day_seconds: total / days,
        prev_total_seconds: prev_total,
        closed_tasks: closed,
        touched_tasks: touched,
        max_focus_seconds: max_focus,
        max_focus_task_id: max_task,
        max_focus_day: max_day,
    }
}

/// Строит данные Гантта за окно.
///
/// В окно попадает задача, у которой хотя бы одна из планируемых дат
/// (`start`, `due`) лежит внутри, ЛИБО по которой в окне есть отрезки
/// времени. Задачи без `start`/`due` расчёт не роняют: они просто не имеют
/// плановой полосы и рисуются по факту.
pub fn build_gantt(
    tasks: &[TrackerTask],
    entries: &[TimeEntry],
    milestones: &[Milestone],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> GanttData {
    let in_window = |dt: DateTime<Utc>| {
        from.map(|f| dt >= f).unwrap_or(true) && to.map(|t| dt < t).unwrap_or(true)
    };

    let mut bars = Vec::new();
    for task in tasks {
        let actual_seconds = task_seconds(entries, &task.id, now);
        let windowed: i64 = entries
            .iter()
            .filter(|e| e.task_id == task.id)
            .map(|e| entry_seconds_in_window(e, now, from, to))
            .sum();

        let planned_hit = [task.start.as_deref(), task.due.as_deref()]
            .into_iter()
            .flatten()
            .filter_map(parse_ts)
            .any(in_window);

        if !(planned_hit || windowed > 0) && (from.is_some() || to.is_some()) {
            continue;
        }

        let over_estimate = match task.estimate_min {
            Some(est) if est > 0 => actual_seconds > i64::from(est) * 60,
            _ => false,
        };

        bars.push(GanttBar {
            task_id: task.id.clone(),
            title: task.title.clone(),
            list_id: task.list_id.clone(),
            status: task.status,
            start: task.start.clone(),
            due: task.due.clone(),
            estimate_min: task.estimate_min,
            actual_seconds,
            over_estimate,
        });
    }

    let milestones = milestones
        .iter()
        .filter(|m| match parse_ts(&m.date) {
            Some(dt) => in_window(dt),
            // Веха с неразбираемой датой не теряется молча: показываем её,
            // иначе пользователь не поймёт, куда делся ромб.
            None => true,
        })
        .cloned()
        .collect();

    GanttData {
        from: from.map(|d| d.to_rfc3339()),
        to: to.map(|d| d.to_rfc3339()),
        bars,
        milestones,
    }
}

/// Суммы за сегодня и за текущую неделю (обе — в локальной зоне).
///
/// Неделя начинается с понедельника — так же, как в календарях, на которые
/// смотрит пользователь.
pub fn build_totals(entries: &[TimeEntry], now: DateTime<Utc>) -> Totals {
    let today = now.with_timezone(&Local).date_naive();
    let day_start = local_day_start(today);
    let week_start = {
        let weekday = today.weekday().num_days_from_monday() as i64;
        today
            .checked_sub_signed(Duration::days(weekday))
            .and_then(local_day_start)
    };

    let sum = |from: Option<DateTime<Utc>>| -> i64 {
        entries
            .iter()
            .map(|e| entry_seconds_in_window(e, now, from, None))
            .sum()
    };

    Totals {
        today_seconds: sum(day_start),
        week_seconds: sum(week_start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SOURCE_TIMER;

    fn entry(id: &str, task: &str, item: Option<&str>, from: &str, to: Option<&str>) -> TimeEntry {
        TimeEntry {
            id: id.into(),
            task_id: task.into(),
            item_id: item.map(|s| s.to_string()),
            started_at: from.into(),
            stopped_at: to.map(|s| s.to_string()),
            note: None,
            source: SOURCE_TIMER.into(),
        }
    }

    fn task(id: &str, title: &str, list: Option<&str>) -> TrackerTask {
        TrackerTask {
            id: id.into(),
            list_id: list.map(|s| s.to_string()),
            title: title.into(),
            notes: None,
            status: TaskStatus::Inbox,
            priority: 2,
            tags: vec![],
            estimate_min: None,
            start: None,
            due: None,
            order: 0,
            created_at: "2026-09-01T00:00:00Z".into(),
            updated_at: "2026-09-01T00:00:00Z".into(),
            done_at: None,
        }
    }

    #[test]
    fn parses_both_rfc3339_and_bare_date() {
        assert!(parse_ts("2026-09-08T10:00:00Z").is_some());
        assert!(parse_ts("2026-09-08").is_some());
        assert!(parse_ts("не дата").is_none());
    }

    #[test]
    fn running_entry_counts_until_now() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let e = entry("e", "t", None, "2026-09-08T11:30:00Z", None);
        assert_eq!(entry_seconds_in_window(&e, now, None, None), 1800);
    }

    #[test]
    fn entry_crossing_local_midnight_is_split_between_days() {
        // Строим отрезок вокруг ЛОКАЛЬНОЙ полуночи, поэтому тест не зависит
        // от зоны машины.
        let today = Local::now().date_naive();
        let midnight = local_day_start(today).unwrap();
        let start = midnight - Duration::minutes(30);
        let end = midnight + Duration::minutes(45);

        let parts = split_by_local_day(start, end);
        assert_eq!(parts.len(), 2, "{parts:?}");
        let yesterday = today.pred_opt().unwrap();
        assert_eq!(parts[0], (yesterday, 30 * 60));
        assert_eq!(parts[1], (today, 45 * 60));
        assert_eq!(
            parts.iter().map(|(_, s)| *s).sum::<i64>(),
            (end - start).num_seconds()
        );
    }

    #[test]
    fn checklist_time_counts_once_in_task_total() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let entries = vec![
            entry(
                "e1",
                "t1",
                None,
                "2026-09-08T10:00:00Z",
                Some("2026-09-08T10:30:00Z"),
            ),
            entry(
                "e2",
                "t1",
                Some("i1"),
                "2026-09-08T11:00:00Z",
                Some("2026-09-08T11:20:00Z"),
            ),
        ];
        assert_eq!(task_seconds(&entries, "t1", now), 50 * 60);
        assert_eq!(item_seconds(&entries, "i1", now), 20 * 60);
    }

    #[test]
    fn report_groups_by_task_and_list() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let tasks = vec![task("t1", "первая", Some("l1")), task("t2", "вторая", None)];
        let lists = vec![TaskList {
            id: "l1".into(),
            name: "работа".into(),
            color: None,
            order: 0,
        }];
        let entries = vec![
            entry(
                "e1",
                "t1",
                None,
                "2026-09-08T10:00:00Z",
                Some("2026-09-08T11:00:00Z"),
            ),
            entry(
                "e2",
                "t2",
                None,
                "2026-09-08T11:00:00Z",
                Some("2026-09-08T11:30:00Z"),
            ),
        ];

        let by_task = build_report(
            &tasks,
            &lists,
            &entries,
            ReportGroupBy::Task,
            None,
            None,
            now,
        );
        assert_eq!(by_task.total_seconds, 5400);
        assert_eq!(by_task.rows[0].label, "первая");
        assert_eq!(by_task.rows[0].seconds, 3600);

        let by_list = build_report(
            &tasks,
            &lists,
            &entries,
            ReportGroupBy::List,
            None,
            None,
            now,
        );
        assert_eq!(by_list.rows.len(), 2);
        assert_eq!(by_list.rows[0].label, "работа");
        assert_eq!(by_list.rows[1].label, "без списка");
    }

    #[test]
    fn report_window_clips_entries() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let tasks = vec![task("t1", "первая", None)];
        let entries = vec![entry(
            "e1",
            "t1",
            None,
            "2026-09-08T10:00:00Z",
            Some("2026-09-08T12:00:00Z"),
        )];
        let report = build_report(
            &tasks,
            &[],
            &entries,
            ReportGroupBy::Task,
            parse_ts("2026-09-08T11:00:00Z"),
            parse_ts("2026-09-08T11:30:00Z"),
            now,
        );
        assert_eq!(report.total_seconds, 1800);
    }

    #[test]
    fn kpi_counts_days_average_focus_and_closed() {
        // Окно — ровно двое локальных суток; ставим их вокруг локальной
        // полуночи, чтобы тест не зависел от зоны машины.
        let today = Local::now().date_naive();
        let day0 = local_day_start(today.pred_opt().unwrap()).unwrap();
        let day1 = local_day_start(today).unwrap();
        let day2 = local_day_start(today.succ_opt().unwrap()).unwrap();
        let now = day1 + Duration::hours(12);

        let mut done = task("t1", "закрытая", None);
        done.done_at = Some((day1 + Duration::hours(1)).to_rfc3339());
        let open = task("t2", "открытая", None);
        let elsewhere = task("t3", "вне окна", None);
        let tasks = vec![done, open, elsewhere];

        let entries = vec![
            // Вчера: два часа подряд — это и есть максимальный фокус.
            entry(
                "e1",
                "t1",
                None,
                &(day0 + Duration::hours(9)).to_rfc3339(),
                Some(&(day0 + Duration::hours(11)).to_rfc3339()),
            ),
            // Сегодня: полчаса.
            entry(
                "e2",
                "t2",
                None,
                &(day1 + Duration::hours(10)).to_rfc3339(),
                Some(&(day1 + Duration::hours(10) + Duration::minutes(30)).to_rfc3339()),
            ),
            // Позапрошлые сутки — в окно не попадают, но идут в «прошлое окно».
            entry(
                "e3",
                "t3",
                None,
                &(day0 - Duration::hours(23)).to_rfc3339(),
                Some(&(day0 - Duration::hours(22)).to_rfc3339()),
            ),
        ];

        let kpi = build_kpi(&tasks, &entries, Some(day0), Some(day2), now);
        assert_eq!(kpi.days, 2, "окно [день, +2 суток) это двое суток");
        assert_eq!(kpi.total_seconds, 2 * 3600 + 1800);
        assert_eq!(kpi.avg_day_seconds, kpi.total_seconds / 2);
        assert_eq!(kpi.max_focus_seconds, 2 * 3600);
        assert_eq!(kpi.max_focus_task_id.as_deref(), Some("t1"));
        assert_eq!(
            kpi.max_focus_day.as_deref(),
            Some(today.pred_opt().unwrap().format("%Y-%m-%d").to_string().as_str())
        );
        assert_eq!(kpi.closed_tasks, 1);
        assert_eq!(kpi.touched_tasks, 2, "t3 в окне не тронута");
        assert_eq!(kpi.prev_total_seconds, Some(3600), "предыдущие двое суток");
    }

    #[test]
    fn kpi_clips_focus_by_window_and_counts_running_entry() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let tasks = vec![task("t1", "идёт", None)];
        // Отрезок идёт с 10:00 и не остановлен: до `now` это два часа.
        let entries = vec![entry("e1", "t1", None, "2026-09-08T10:00:00Z", None)];
        let kpi = build_kpi(
            &tasks,
            &entries,
            parse_ts("2026-09-08T11:00:00Z"),
            parse_ts("2026-09-08T11:30:00Z"),
            now,
        );
        assert_eq!(kpi.total_seconds, 1800);
        assert_eq!(kpi.max_focus_seconds, 1800, "фокус обрезан окном");
        assert_eq!(kpi.touched_tasks, 1);
        assert_eq!(kpi.closed_tasks, 0);
    }

    #[test]
    fn kpi_of_empty_window_is_zero_but_not_divided_by_zero() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let kpi = build_kpi(
            &[task("t1", "без времени", None)],
            &[],
            parse_ts("2026-09-01"),
            parse_ts("2026-09-08"),
            now,
        );
        assert_eq!(kpi.total_seconds, 0);
        assert_eq!(kpi.avg_day_seconds, 0);
        assert_eq!(kpi.days, 7);
        assert_eq!(kpi.max_focus_seconds, 0);
        assert!(kpi.max_focus_task_id.is_none());
        assert_eq!(kpi.touched_tasks, 0);
    }

    #[test]
    fn gantt_marks_over_estimate_and_survives_missing_dates() {
        let now = parse_ts("2026-09-08T12:00:00Z").unwrap();
        let mut over = task("t1", "оценка час", None);
        over.estimate_min = Some(60);
        let plain = task("t2", "без дат", None);
        let tasks = vec![over, plain];
        // 278 минут факта против 60 минут оценки.
        let entries = vec![entry(
            "e1",
            "t1",
            None,
            "2026-09-08T00:00:00Z",
            Some("2026-09-08T04:38:00Z"),
        )];

        let data = build_gantt(&tasks, &entries, &[], None, None, now);
        assert_eq!(data.bars.len(), 2);
        let bar = data.bars.iter().find(|b| b.task_id == "t1").unwrap();
        assert_eq!(bar.actual_seconds, 278 * 60);
        assert!(bar.over_estimate);
        let plain = data.bars.iter().find(|b| b.task_id == "t2").unwrap();
        assert!(!plain.over_estimate);
        assert_eq!(plain.actual_seconds, 0);
    }

    #[test]
    fn totals_cover_today_and_week() {
        let now = Local::now().with_timezone(&Utc);
        let day_start = local_day_start(now.with_timezone(&Local).date_naive()).unwrap();
        // Отрезок целиком внутри сегодняшних суток.
        let start = (day_start + Duration::minutes(1)).min(now);
        let e = entry(
            "e1",
            "t1",
            None,
            &start.to_rfc3339(),
            Some(&(start + Duration::minutes(10)).to_rfc3339()),
        );
        let totals = build_totals(&[e], start + Duration::minutes(20));
        assert_eq!(totals.today_seconds, 600);
        assert!(totals.week_seconds >= totals.today_seconds);
        assert!(now.with_timezone(&Local).date_naive().weekday().number_from_monday() >= 1);
    }
}
