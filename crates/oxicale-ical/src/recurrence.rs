use std::collections::BTreeSet;

use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;

use crate::model::{Component, IcalTimeRange, Property};
use crate::parser::IcalError;

pub const DEFAULT_OCCURRENCE_LIMIT: usize = 10_000;

type ComponentSpan = (DateTime<Utc>, DateTime<Utc>, bool);
type Period = (DateTime<Utc>, DateTime<Utc>);

#[derive(Clone, Copy, Debug)]
pub struct ParsedDate {
    pub utc: DateTime<Utc>,
    pub is_date: bool,
}

pub fn parse_property_datetime(
    property: &Property,
    default_tz: Tz,
) -> Result<ParsedDate, IcalError> {
    let value = property.value.trim();
    let is_date = property
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        || (value.len() == 8 && !value.contains('T'));
    if is_date {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d")
            .map_err(|_| IcalError::Invalid(format!("invalid DATE value {value}")))?;
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| IcalError::Invalid("invalid date".into()))?;
        let utc = default_tz
            .from_local_datetime(&naive)
            .single()
            .unwrap_or_else(|| default_tz.from_utc_datetime(&naive))
            .with_timezone(&Utc);
        return Ok(ParsedDate { utc, is_date });
    }

    if value.ends_with('Z') {
        let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
            .map_err(|_| IcalError::Invalid(format!("invalid UTC datetime {value}")))?;
        return Ok(ParsedDate {
            utc: Utc.from_utc_datetime(&naive),
            is_date: false,
        });
    }

    let naive = parse_naive_datetime(value)?;
    if let Some(tzid) = property.param("TZID") {
        let tz: Tz = tzid
            .parse()
            .map_err(|_| IcalError::Invalid(format!("unknown TZID {tzid}")))?;
        let utc = tz
            .from_local_datetime(&naive)
            .single()
            .or_else(|| tz.from_local_datetime(&naive).earliest())
            .ok_or_else(|| IcalError::Invalid(format!("ambiguous local time in {tzid}")))?;
        Ok(ParsedDate {
            utc: utc.with_timezone(&Utc),
            is_date: false,
        })
    } else {
        let utc = default_tz
            .from_local_datetime(&naive)
            .single()
            .unwrap_or_else(|| default_tz.from_utc_datetime(&naive))
            .with_timezone(&Utc);
        Ok(ParsedDate {
            utc,
            is_date: false,
        })
    }
}

fn parse_naive_datetime(value: &str) -> Result<NaiveDateTime, IcalError> {
    NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M"))
        .map_err(|_| IcalError::Invalid(format!("invalid datetime {value}")))
}

pub fn parse_duration(value: &str) -> Result<Duration, IcalError> {
    let value = value.trim();
    let (sign, value) = if let Some(rest) = value.strip_prefix('-') {
        (-1i64, rest)
    } else if let Some(rest) = value.strip_prefix('+') {
        (1, rest)
    } else {
        (1, value)
    };
    let value = value
        .strip_prefix('P')
        .ok_or_else(|| IcalError::Invalid(format!("invalid DURATION {value}")))?;
    let mut seconds = 0i64;
    let mut number = String::new();
    let mut in_time = false;
    for c in value.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        if c == 'T' {
            in_time = true;
            number.clear();
            continue;
        }
        let n: i64 = number
            .parse()
            .map_err(|_| IcalError::Invalid(format!("invalid DURATION {value}")))?;
        number.clear();
        match c {
            'W' => seconds += n * 7 * 86_400,
            'D' => seconds += n * 86_400,
            'H' if in_time => seconds += n * 3_600,
            'M' if in_time => seconds += n * 60,
            'S' if in_time => seconds += n,
            _ => return Err(IcalError::Invalid(format!("invalid DURATION {value}"))),
        }
    }
    if !number.is_empty() {
        return Err(IcalError::Invalid(format!("invalid DURATION {value}")));
    }
    Ok(Duration::seconds(sign * seconds))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frequency {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug)]
struct RecurrenceRule {
    frequency: Frequency,
    interval: u32,
    count: Option<u32>,
    until: Option<DateTime<Utc>>,
    by_day: Vec<Weekday>,
    by_month_day: Vec<u32>,
}

fn parse_rrule(value: &str) -> Result<RecurrenceRule, IcalError> {
    let mut frequency = None;
    let mut interval = 1u32;
    let mut count = None;
    let mut until = None;
    let mut by_day = Vec::new();
    let mut by_month_day = Vec::new();
    for part in value.split(';') {
        let (key, val) = part
            .split_once('=')
            .ok_or_else(|| IcalError::Invalid("invalid RRULE".into()))?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                frequency = Some(match val.to_ascii_uppercase().as_str() {
                    "SECONDLY" => Frequency::Secondly,
                    "MINUTELY" => Frequency::Minutely,
                    "HOURLY" => Frequency::Hourly,
                    "DAILY" => Frequency::Daily,
                    "WEEKLY" => Frequency::Weekly,
                    "MONTHLY" => Frequency::Monthly,
                    "YEARLY" => Frequency::Yearly,
                    other => {
                        return Err(IcalError::Unsupported(format!("RRULE FREQ={other}")));
                    }
                })
            }
            "INTERVAL" => {
                interval = val
                    .parse::<u32>()
                    .map_err(|_| IcalError::Invalid("invalid RRULE INTERVAL".into()))?
                    .max(1)
            }
            "COUNT" => {
                count = Some(
                    val.parse::<u32>()
                        .map_err(|_| IcalError::Invalid("invalid RRULE COUNT".into()))?,
                )
            }
            "UNTIL" => {
                let prop = Property::new("UNTIL", val);
                until = Some(
                    parse_property_datetime(&prop, chrono_tz::UTC)
                        .map_err(|_| IcalError::Invalid("invalid RRULE UNTIL".into()))?
                        .utc,
                );
            }
            "BYDAY" => {
                for item in val.split(',') {
                    let item = item.trim();
                    let mut chars = item.chars();
                    let first = chars
                        .next()
                        .ok_or_else(|| IcalError::Invalid("invalid RRULE BYDAY".into()))?;
                    let mut ordinal = false;
                    if first == '+' || first == '-' {
                        ordinal = chars.next().is_some_and(|c| c.is_ascii_digit());
                    } else if first.is_ascii_digit() {
                        ordinal = true;
                    }
                    if ordinal {
                        return Err(IcalError::Unsupported("ordinal RRULE BYDAY values".into()));
                    }
                    let weekday = match item.to_ascii_uppercase().as_str() {
                        "MO" => Weekday::Mon,
                        "TU" => Weekday::Tue,
                        "WE" => Weekday::Wed,
                        "TH" => Weekday::Thu,
                        "FR" => Weekday::Fri,
                        "SA" => Weekday::Sat,
                        "SU" => Weekday::Sun,
                        _ => return Err(IcalError::Invalid("invalid RRULE BYDAY".into())),
                    };
                    by_day.push(weekday);
                }
            }
            "BYMONTHDAY" => {
                for item in val.split(',') {
                    let item = item.trim();
                    if item.starts_with('-') || item.starts_with('+') {
                        return Err(IcalError::Unsupported(
                            "signed RRULE BYMONTHDAY values".into(),
                        ));
                    }
                    by_month_day.push(
                        item.parse::<u32>()
                            .map_err(|_| IcalError::Invalid("invalid RRULE BYMONTHDAY".into()))?,
                    );
                }
            }
            "WKST" => {
                if !val.eq_ignore_ascii_case("MO") {
                    return Err(IcalError::Unsupported("RRULE WKST other than MO".into()));
                }
            }
            "BYMONTH" | "BYSETPOS" => {
                return Err(IcalError::Unsupported(format!("RRULE part {key}")));
            }
            other => {
                return Err(IcalError::Unsupported(format!("RRULE part {other}")));
            }
        }
    }
    Ok(RecurrenceRule {
        frequency: frequency.ok_or_else(|| IcalError::Invalid("RRULE without FREQ".into()))?,
        interval,
        count,
        until,
        by_day,
        by_month_day,
    })
}

fn parse_exdates(
    component: &Component,
    default_tz: Tz,
) -> Result<BTreeSet<DateTime<Utc>>, IcalError> {
    let mut out = BTreeSet::new();
    for prop in component.properties("EXDATE") {
        for value in prop.value.split(',') {
            let mut single = prop.clone();
            single.value = value.trim().to_string();
            out.insert(parse_property_datetime(&single, default_tz)?.utc);
        }
    }
    Ok(out)
}

fn parse_rdates(component: &Component, default_tz: Tz) -> Result<Vec<DateTime<Utc>>, IcalError> {
    let mut out = Vec::new();
    for prop in component.properties("RDATE") {
        for value in prop.value.split(',') {
            let mut single = prop.clone();
            single.value = value.trim().to_string();
            out.push(parse_property_datetime(&single, default_tz)?.utc);
        }
    }
    Ok(out)
}

fn match_weekday(day: Weekday, weekdays: &[Weekday]) -> bool {
    weekdays.is_empty() || weekdays.contains(&day)
}

/// Expand a component's start instants within an optional ceiling.
pub fn expand_starts(
    component: &Component,
    default_tz: Tz,
    range_end: Option<DateTime<Utc>>,
    limit: usize,
) -> Result<Vec<DateTime<Utc>>, IcalError> {
    let Some(dtstart) = component.property("DTSTART") else {
        return Ok(Vec::new());
    };
    let start = parse_property_datetime(dtstart, default_tz)?;
    let mut out = Vec::new();
    let rrule = match component.property("RRULE") {
        Some(prop) => Some(parse_rrule(&prop.value)?),
        None => None,
    };
    let exdates = parse_exdates(component, default_tz)?;
    let rdates = parse_rdates(component, default_tz)?;

    if let Some(rule) = &rrule {
        let until_limit = range_end.or(rule.until);
        let mut seen = 0u32;
        let max_count = rule.count.unwrap_or(u32::MAX);
        let base = start.utc;
        let base_naive = tz_naive(base, default_tz);
        let mut generated = 0usize;
        let mut emit = |instant: DateTime<Utc>| -> bool {
            seen += 1;
            if seen > max_count {
                return false;
            }
            if rule.until.is_some_and(|until| instant > until) {
                return false;
            }
            if until_limit.is_some_and(|end| instant >= end) {
                return false;
            }
            if !exdates.contains(&instant) {
                out.push(instant);
                generated += 1;
            }
            generated < limit
        };

        match rule.frequency {
            Frequency::Secondly | Frequency::Minutely | Frequency::Hourly | Frequency::Daily => {
                let step = match rule.frequency {
                    Frequency::Secondly => Duration::seconds(1),
                    Frequency::Minutely => Duration::seconds(60),
                    Frequency::Hourly => Duration::seconds(3600),
                    Frequency::Daily => Duration::seconds(86_400),
                    _ => unreachable!(),
                } * rule.interval as i32;
                let mut current_naive = base_naive;
                loop {
                    let current = default_tz
                        .from_local_datetime(&current_naive)
                        .earliest()
                        .map(|value| value.with_timezone(&Utc))
                        .unwrap_or_else(|| {
                            default_tz
                                .from_utc_datetime(&current_naive)
                                .with_timezone(&Utc)
                        });
                    if !emit(current) {
                        break;
                    }
                    let Some(next) = current_naive.checked_add_signed(step) else {
                        break;
                    };
                    if next <= current_naive {
                        break;
                    }
                    current_naive = next;
                    if current_naive > base_naive + Duration::days(365 * 100) {
                        break;
                    }
                }
            }
            Frequency::Weekly => {
                let weekdays = if rule.by_day.is_empty() {
                    vec![base_naive.weekday()]
                } else {
                    rule.by_day.clone()
                };
                let week_start = base_naive
                    .date()
                    .checked_sub_signed(Duration::days(
                        base_naive.weekday().num_days_from_monday() as i64
                    ))
                    .ok_or_else(|| IcalError::Invalid("invalid recurrence date".into()))?;
                let mut week = 0i64;
                'outer: loop {
                    if week > 52 * 100 {
                        break;
                    }
                    for weekday in &weekdays {
                        let offset = weekday.num_days_from_monday() as i64;
                        let date =
                            week_start + Duration::days(week * 7 * rule.interval as i64 + offset);
                        let Some(candidate) = date.and_hms_opt(
                            base_naive.hour(),
                            base_naive.minute(),
                            base_naive.second(),
                        ) else {
                            continue;
                        };
                        let instant = default_tz
                            .from_local_datetime(&candidate)
                            .earliest()
                            .map(|v| v.with_timezone(&Utc));
                        let Some(instant) = instant else {
                            continue;
                        };
                        if instant < base {
                            continue;
                        }
                        if !emit(instant) {
                            break 'outer;
                        }
                    }
                    week += 1;
                }
            }
            Frequency::Monthly => {
                let mut step = 0u32;
                loop {
                    let months = step as i32 * rule.interval as i32;
                    let total = base_naive.year() * 12 + base_naive.month0() as i32 + months;
                    let year = total.div_euclid(12);
                    let month = total.rem_euclid(12) as u32 + 1;
                    let Some(date) = NaiveDate::from_ymd_opt(year, month, base_naive.day()) else {
                        step += 1;
                        if step > 1200 {
                            break;
                        }
                        continue;
                    };
                    let Some(candidate) = date.and_hms_opt(
                        base_naive.hour(),
                        base_naive.minute(),
                        base_naive.second(),
                    ) else {
                        step += 1;
                        if step > 1200 {
                            break;
                        }
                        continue;
                    };
                    if !rule.by_month_day.is_empty()
                        && !rule.by_month_day.contains(&candidate.day())
                    {
                        step += 1;
                        if step > 1200 {
                            break;
                        }
                        continue;
                    }
                    if !match_weekday(candidate.weekday(), &rule.by_day) {
                        step += 1;
                        if step > 1200 {
                            break;
                        }
                        continue;
                    }
                    let Some(instant) = default_tz
                        .from_local_datetime(&candidate)
                        .earliest()
                        .map(|v| v.with_timezone(&Utc))
                    else {
                        step += 1;
                        if step > 1200 {
                            break;
                        }
                        continue;
                    };
                    if instant < base || !emit(instant) {
                        break;
                    }
                    step += 1;
                    if step > 1200 {
                        break;
                    }
                }
            }
            Frequency::Yearly => {
                let mut year = base_naive.year();
                let mut ticks = 0;
                loop {
                    ticks += 1;
                    if ticks > 500 {
                        break;
                    }
                    let Some(candidate) =
                        NaiveDate::from_ymd_opt(year, base_naive.month(), base_naive.day())
                            .and_then(|d| {
                                d.and_hms_opt(
                                    base_naive.hour(),
                                    base_naive.minute(),
                                    base_naive.second(),
                                )
                            })
                    else {
                        year += rule.interval as i32;
                        continue;
                    };
                    let Some(instant) = default_tz
                        .from_local_datetime(&candidate)
                        .earliest()
                        .map(|v| v.with_timezone(&Utc))
                    else {
                        year += rule.interval as i32;
                        continue;
                    };
                    if instant < base || !emit(instant) {
                        break;
                    }
                    year += rule.interval as i32;
                }
            }
        }
        for rdate in rdates {
            if !exdates.contains(&rdate) {
                out.push(rdate);
            }
        }
    } else {
        if !exdates.contains(&start.utc) {
            out.push(start.utc);
        }
        for rdate in rdates {
            if !exdates.contains(&rdate) {
                out.push(rdate);
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn tz_naive(instant: DateTime<Utc>, tz: Tz) -> NaiveDateTime {
    instant.with_timezone(&tz).naive_local()
}

fn component_span(
    component: &Component,
    default_tz: Tz,
) -> Result<Option<ComponentSpan>, IcalError> {
    let name = component.name.as_str();
    if name == "VEVENT" {
        let Some(start_prop) = component.property("DTSTART") else {
            return Ok(None);
        };
        let start = parse_property_datetime(start_prop, default_tz)?;
        let end = if let Some(prop) = component.property("DTEND") {
            parse_property_datetime(prop, default_tz)?.utc
        } else if let Some(prop) = component.property("DURATION") {
            start.utc + parse_duration(&prop.value)?
        } else if start.is_date {
            start.utc + Duration::days(1)
        } else {
            start.utc
        };
        return Ok(Some((start.utc, end.max(start.utc), start.is_date)));
    }
    if name == "VTODO" {
        let start = component
            .property("DTSTART")
            .map(|p| parse_property_datetime(p, default_tz))
            .transpose()?;
        let due = component
            .property("DUE")
            .map(|p| parse_property_datetime(p, default_tz))
            .transpose()?;
        let completed = component
            .property("COMPLETED")
            .map(|p| parse_property_datetime(p, default_tz))
            .transpose()?;
        let created = component
            .property("CREATED")
            .map(|p| parse_property_datetime(p, default_tz))
            .transpose()?;
        if let (Some(start), Some(due)) = (start, due) {
            let end = if due.is_date {
                due.utc + Duration::days(1)
            } else {
                due.utc
            };
            return Ok(Some((start.utc, end.max(start.utc), start.is_date)));
        }
        let instant = start.or(due).or(completed).or(created);
        if let Some(instant) = instant {
            return Ok(Some((instant.utc, instant.utc, instant.is_date)));
        }
        return Ok(None);
    }
    if name == "VJOURNAL" {
        if let Some(prop) = component.property("DTSTART") {
            let start = parse_property_datetime(prop, default_tz)?;
            let end = if start.is_date {
                start.utc + Duration::days(1)
            } else {
                start.utc
            };
            return Ok(Some((start.utc, end, start.is_date)));
        }
        return Ok(None);
    }
    Ok(None)
}

fn recurrence_id(
    component: &Component,
    default_tz: Tz,
) -> Result<Option<DateTime<Utc>>, IcalError> {
    component
        .property("RECURRENCE-ID")
        .map(|p| parse_property_datetime(p, default_tz).map(|v| v.utc))
        .transpose()
}

/// Does any calendar object component intersect the requested range?
pub fn item_matches_range(
    calendar: &Component,
    range: &IcalTimeRange,
    default_tz: Tz,
) -> Result<bool, IcalError> {
    if range.start.is_none() && range.end.is_none() {
        return Ok(calendar
            .children
            .iter()
            .any(|c| matches!(c.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL")));
    }

    let mut exception_ids = BTreeSet::new();
    for component in &calendar.children {
        if matches!(component.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL") {
            if let Some(id) = recurrence_id(component, default_tz)? {
                exception_ids.insert(id);
            }
        }
    }

    for component in &calendar.children {
        if !matches!(component.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL") {
            continue;
        }
        let Some((master_start, master_end, _)) = component_span(component, default_tz)? else {
            continue;
        };
        if recurrence_id(component, default_tz)?.is_some() {
            if range.overlaps(master_start, master_end) {
                return Ok(true);
            }
            continue;
        }
        if component.property("DTSTART").is_none() {
            if range.overlaps(master_start, master_end) {
                return Ok(true);
            }
            continue;
        }
        let duration = master_end - master_start;
        let starts = expand_starts(component, default_tz, range.end, DEFAULT_OCCURRENCE_LIMIT)?;
        for start in starts {
            if exception_ids.contains(&start) {
                continue;
            }
            let end = start + duration;
            if range.overlaps(start, end) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn occurrence_count(
    calendar: &Component,
    default_tz: Tz,
    limit: usize,
) -> Result<usize, IcalError> {
    let mut count = 0usize;
    for component in &calendar.children {
        if !matches!(component.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL") {
            continue;
        }
        if component.property("RRULE").is_some() || component.property("RDATE").is_some() {
            count += expand_starts(component, default_tz, None, limit)?.len();
        } else if component.property("DTSTART").is_some() {
            count += 1;
        }
    }
    Ok(count)
}

/// Collect busy periods for free-busy generation.
pub fn event_periods(
    calendar: &Component,
    range: &IcalTimeRange,
    default_tz: Tz,
) -> Result<Vec<Period>, IcalError> {
    let mut exceptions = BTreeSet::new();
    for component in &calendar.children {
        if component.name == "VEVENT" {
            if let Some(id) = recurrence_id(component, default_tz)? {
                exceptions.insert(id);
            }
        }
    }
    let mut periods = Vec::new();
    for component in &calendar.children {
        if component.name != "VEVENT" {
            continue;
        }
        if component
            .property("TRANSP")
            .is_some_and(|p| p.value.eq_ignore_ascii_case("TRANSPARENT"))
        {
            continue;
        }
        if component
            .property("STATUS")
            .is_some_and(|p| p.value.eq_ignore_ascii_case("CANCELLED"))
        {
            continue;
        }
        let Some((start, end, _)) = component_span(component, default_tz)? else {
            continue;
        };
        if recurrence_id(component, default_tz)?.is_some() {
            if range.overlaps(start, end) {
                periods.push((start, end));
            }
            continue;
        }
        if component.property("DTSTART").is_none() {
            if range.overlaps(start, end) {
                periods.push((
                    start.max(range.start.unwrap_or(start)),
                    end.min(range.end.unwrap_or(end)),
                ));
            }
            continue;
        }
        let duration = end - start;
        for occurrence in expand_starts(component, default_tz, range.end, DEFAULT_OCCURRENCE_LIMIT)?
        {
            if exceptions.contains(&occurrence) {
                continue;
            }
            let occurrence_end = occurrence + duration;
            if range.overlaps(occurrence, occurrence_end) {
                periods.push((
                    occurrence.max(range.start.unwrap_or(occurrence)),
                    occurrence_end.min(range.end.unwrap_or(occurrence_end)),
                ));
            }
        }
    }
    periods.sort();
    periods.dedup();
    Ok(periods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_calendar, Limits};

    fn cal(body: &str) -> Component {
        let input = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n");
        parse_calendar(&input, Limits::default()).unwrap()
    }

    fn range(start: &str, end: &str) -> IcalTimeRange {
        IcalTimeRange {
            start: Some(start.parse().unwrap()),
            end: Some(end.parse().unwrap()),
        }
    }

    #[test]
    fn parses_dates_and_durations() {
        let p = Property::new("DTSTART", "20261022");
        let parsed = parse_property_datetime(&p, chrono_tz::UTC).unwrap();
        assert!(parsed.is_date);
        assert_eq!(parsed.utc.to_string(), "2026-10-22 00:00:00 UTC");

        let p = Property::new("DTSTART", "20261022T101500Z");
        assert!(!parse_property_datetime(&p, chrono_tz::UTC).unwrap().is_date);

        assert_eq!(parse_duration("PT1H30M").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("P1D").unwrap(), Duration::days(1));
        assert_eq!(parse_duration("-P1W").unwrap(), Duration::days(-7));
        assert!(parse_duration("garbage").is_err());
    }

    #[test]
    fn simple_event_range() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nEND:VEVENT\r\n");
        assert!(item_matches_range(
            &c,
            &range("2026-10-22T10:30:00Z", "2026-10-22T10:45:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
        assert!(!item_matches_range(
            &c,
            &range("2026-10-22T11:00:00Z", "2026-10-22T12:00:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
    }

    #[test]
    fn all_day_event_spans_day() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART;VALUE=DATE:20261022\r\nEND:VEVENT\r\n");
        assert!(item_matches_range(
            &c,
            &range("2026-10-22T12:00:00Z", "2026-10-22T13:00:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
        assert!(!item_matches_range(
            &c,
            &range("2026-10-23T00:00:00Z", "2026-10-23T01:00:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
    }

    #[test]
    fn todo_and_journal_ranges() {
        let c = cal("BEGIN:VTODO\r\nUID:1\r\nDUE:20261025T120000Z\r\nEND:VTODO\r\n");
        assert!(item_matches_range(
            &c,
            &range("2026-10-25T00:00:00Z", "2026-10-26T00:00:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
        let c = cal("BEGIN:VJOURNAL\r\nUID:2\r\nDTSTART:20261020T000000Z\r\nEND:VJOURNAL\r\n");
        assert!(item_matches_range(
            &c,
            &range("2026-10-19T00:00:00Z", "2026-10-21T00:00:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
    }

    #[test]
    fn recurring_daily() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEND:VEVENT\r\n");
        let starts = expand_starts(&c.children[0], chrono_tz::UTC, None, 100).unwrap();
        assert_eq!(starts.len(), 5);
        assert!(item_matches_range(
            &c,
            &range("2026-10-05T10:30:00Z", "2026-10-05T10:45:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
        assert!(!item_matches_range(
            &c,
            &range("2026-10-06T10:30:00Z", "2026-10-06T10:45:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
    }

    #[test]
    fn recurrence_exception_replaces_occurrence() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:1\r\nRECURRENCE-ID:20261002T100000Z\r\nDTSTART:20261002T150000Z\r\nDTEND:20261002T160000Z\r\nEND:VEVENT\r\n");
        assert!(!item_matches_range(
            &c,
            &range("2026-10-02T10:30:00Z", "2026-10-02T10:45:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
        assert!(item_matches_range(
            &c,
            &range("2026-10-02T15:30:00Z", "2026-10-02T15:45:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
    }

    #[test]
    fn weekly_byday() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261005T090000Z\r\nDTEND:20261005T100000Z\r\nRRULE:FREQ=WEEKLY;COUNT=4;BYDAY=MO,WE\r\nEND:VEVENT\r\n");
        let starts = expand_starts(&c.children[0], chrono_tz::UTC, None, 100).unwrap();
        assert_eq!(starts.len(), 4);
    }

    #[test]
    fn timezone_parsing() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=Europe/Paris:20261022T120000\r\nDTEND;TZID=Europe/Paris:20261022T130000\r\nEND:VEVENT\r\n");
        let p = c.children[0].property("DTSTART").unwrap();
        let parsed = parse_property_datetime(p, chrono_tz::UTC).unwrap();
        assert_eq!(parsed.utc.to_string(), "2026-10-22 10:00:00 UTC");
    }

    #[test]
    fn exdate_removes_occurrence() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20261002T100000Z\r\nEND:VEVENT\r\n");
        let starts = expand_starts(&c.children[0], chrono_tz::UTC, None, 100).unwrap();
        assert_eq!(starts.len(), 2);
    }
    #[test]
    fn rdate_only_occurrences_match_ranges() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261001T100000Z\r\nDTEND:20261001T110000Z\r\nRDATE:20261005T100000Z\r\nEND:VEVENT\r\n");
        assert!(item_matches_range(
            &c,
            &range("2026-10-05T10:30:00Z", "2026-10-05T10:45:00Z"),
            chrono_tz::UTC
        )
        .unwrap());
    }

    #[test]
    fn unsupported_rrule_parts_are_rejected() {
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261001T100000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=12\r\nEND:VEVENT\r\n");
        assert!(expand_starts(&c.children[0], chrono_tz::UTC, None, 100).is_err());
        let c = cal("BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261001T100000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=1MO\r\nEND:VEVENT\r\n");
        assert!(expand_starts(&c.children[0], chrono_tz::UTC, None, 100).is_err());
    }
}
