use std::collections::{BTreeMap, BTreeSet};

use chrono_tz::Tz;
use oxicale_core::{CalendarComponent, ItemComponent};

use crate::model::{Component, Property};
use crate::parser::{parse_calendar, serialize_component, IcalError, Limits};
use crate::recurrence::{parse_property_datetime, ParsedDate};

#[derive(Clone, Debug)]
pub struct ValidatedCalendar {
    pub uid: String,
    pub component: ItemComponent,
    pub calendar: Component,
    pub canonical: String,
}

fn supported_name(name: &str) -> Option<CalendarComponent> {
    match name {
        "VEVENT" => Some(CalendarComponent::VEvent),
        "VTODO" => Some(CalendarComponent::VTodo),
        "VJOURNAL" => Some(CalendarComponent::VJournal),
        _ => None,
    }
}

fn validate_supported_component(component: &Component, default_tz: Tz) -> Result<(), IcalError> {
    let name = component.name.as_str();
    if name == "VEVENT" && component.property("DTSTART").is_none() {
        return Err(IcalError::Invalid("VEVENT requires DTSTART".to_string()));
    }

    let start = component
        .property("DTSTART")
        .map(|p| parse_property_datetime(p, default_tz))
        .transpose()?;
    let end = component
        .property("DTEND")
        .or_else(|| component.property("DUE"))
        .map(|p| parse_property_datetime(p, default_tz))
        .transpose()?;
    if let (Some(start), Some(end)) = (start, end) {
        if start.is_date != end.is_date {
            return Err(IcalError::Invalid(format!(
                "{name} DTSTART and DTEND/DUE use inconsistent value types"
            )));
        }
        if end.utc < start.utc {
            return Err(IcalError::Invalid(format!("{name} ends before it starts")));
        }
    }
    if let Some(prop) = component.property("DURATION") {
        if end.is_some() {
            return Err(IcalError::Invalid(format!(
                "{name} has both DURATION and DTEND/DUE"
            )));
        }
        crate::recurrence::parse_duration(&prop.value)?;
    }

    for child in &component.children {
        if child.name == "VALARM" {
            continue;
        }
        return Err(IcalError::Unsupported(format!(
            "{} inside {name}",
            child.name
        )));
    }

    if let (Some(recurrence), Some(start)) = (component.property("RECURRENCE-ID"), start.as_ref()) {
        let recurrence = parse_property_datetime(recurrence, default_tz)?;
        if recurrence.is_date != start.is_date {
            return Err(IcalError::Invalid(
                "RECURRENCE-ID type does not match DTSTART".to_string(),
            ));
        }
    }
    Ok(())
}

fn referenced_tzids(component: &Component, out: &mut BTreeSet<String>) {
    for property in &component.properties {
        if let Some(tzid) = property.param("TZID") {
            out.insert(tzid.to_string());
        }
    }
    for child in &component.children {
        referenced_tzids(child, out);
    }
}

/// Validate a complete calendar object resource.
///
/// The returned canonical payload has CRLF line endings and, when allowed, a
/// generated UID if the client omitted one.
pub fn validate_calendar_object(
    input: &str,
    limits: Limits,
    default_tz: Tz,
    generate_uid: bool,
) -> Result<ValidatedCalendar, IcalError> {
    let mut calendar = parse_calendar(input, limits)?;

    let version = calendar
        .property("VERSION")
        .ok_or_else(|| IcalError::Invalid("VCALENDAR requires VERSION".to_string()))?;
    if version.value.trim() != "2.0" {
        return Err(IcalError::Unsupported(format!(
            "iCalendar version {}",
            version.value
        )));
    }

    let mut timezones: BTreeMap<String, Component> = BTreeMap::new();
    for component in &calendar.children {
        match component.name.as_str() {
            "VTIMEZONE" => {
                let tzid = component
                    .property("TZID")
                    .map(|p| p.value.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| IcalError::Invalid("VTIMEZONE requires a TZID".to_string()))?;
                if tzid.len() > 255 {
                    return Err(IcalError::Invalid("TZID is too long".to_string()));
                }
                let has_rules = component.children.iter().any(|child| {
                    matches!(child.name.as_str(), "STANDARD" | "DAYLIGHT")
                        && child.property("DTSTART").is_some()
                        && child.property("TZOFFSETFROM").is_some()
                        && child.property("TZOFFSETTO").is_some()
                });
                if !has_rules {
                    return Err(IcalError::Invalid(format!(
                        "VTIMEZONE {tzid} has no valid STANDARD or DAYLIGHT rule"
                    )));
                }
                timezones.entry(tzid).or_insert_with(|| component.clone());
            }
            "VEVENT" | "VTODO" | "VJOURNAL" => {}
            other => {
                return Err(IcalError::Unsupported(format!(
                    "unsupported component {other}"
                )))
            }
        }
    }

    let mut supported_indices = Vec::new();
    for (index, component) in calendar.children.iter().enumerate() {
        if supported_name(&component.name).is_some() {
            supported_indices.push(index);
        }
    }
    if supported_indices.is_empty() {
        return Err(IcalError::Invalid(
            "calendar object requires at least one supported component".to_string(),
        ));
    }

    let primary_name = calendar
        .children
        .get(supported_indices[0])
        .expect("supported index")
        .name
        .clone();
    let mut masters = 0usize;
    let mut recurrence_ids = BTreeSet::new();
    for index in &supported_indices {
        let component = &calendar.children[*index];
        if component.name != primary_name {
            return Err(IcalError::Invalid(
                "one calendar object resource must contain a single component type".to_string(),
            ));
        }
        match component.property("RECURRENCE-ID") {
            Some(property) => {
                let recurrence = parse_property_datetime(property, default_tz)?;
                if !recurrence_ids.insert(recurrence.utc) {
                    return Err(IcalError::Invalid(
                        "duplicate RECURRENCE-ID in one resource".to_string(),
                    ));
                }
            }
            None => masters += 1,
        }
    }
    if masters != 1 {
        return Err(IcalError::Invalid(
            "a calendar object resource must contain exactly one master component".to_string(),
        ));
    }

    let mut uid: Option<String> = None;
    let mut generated = false;
    for index in &supported_indices {
        let component = &calendar.children[*index];
        let existing = component
            .property("UID")
            .map(|p| p.value.trim().to_string())
            .filter(|v| !v.is_empty());
        let component_uid = match existing {
            Some(value) => value,
            None if generate_uid => {
                generated = true;
                format!("{}@oxicale", uuid::Uuid::new_v4())
            }
            None => {
                return Err(IcalError::Invalid(format!(
                    "{} requires a UID",
                    component.name
                )))
            }
        };
        if component_uid.len() > 255 {
            return Err(IcalError::Invalid(
                "UID is longer than 255 bytes".to_string(),
            ));
        }
        if let Some(uid) = &uid {
            if uid != &component_uid {
                return Err(IcalError::Invalid(
                    "all components in one resource must share a UID".to_string(),
                ));
            }
        } else {
            uid = Some(component_uid);
        }
    }

    if generated {
        for index in &supported_indices {
            let component = &mut calendar.children[*index];
            if component.property("UID").is_none() {
                let mut props = vec![Property::new("UID", uid.clone().unwrap())];
                props.extend(std::mem::take(&mut component.properties));
                component.properties = props;
            }
        }
    }

    for index in &supported_indices {
        let component = &calendar.children[*index];
        validate_supported_component(component, default_tz)?;
    }

    let mut referenced = BTreeSet::new();
    for index in &supported_indices {
        referenced_tzids(&calendar.children[*index], &mut referenced);
    }
    for tzid in &referenced {
        if timezones.contains_key(tzid) {
            continue;
        }
        if tzid.parse::<Tz>().is_err() {
            return Err(IcalError::Invalid(format!(
                "component references unknown TZID {tzid}"
            )));
        }
    }

    let primary = calendar
        .children
        .get(supported_indices[0])
        .expect("supported index");
    let component = ItemComponent::from(supported_name(&primary.name).expect("supported"));
    let canonical = serialize_component(&calendar);
    Ok(ValidatedCalendar {
        uid: uid.expect("uid set"),
        component,
        calendar,
        canonical,
    })
}

pub fn parse_utc_datetime(value: &str) -> Result<chrono::DateTime<chrono::Utc>, IcalError> {
    let prop = Property::new("VALUE", value);
    let parsed: ParsedDate = parse_property_datetime(&prop, chrono_tz::UTC)?;
    Ok(parsed.utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")
    }

    const EVENT: &str = "BEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Test\r\nEND:VEVENT\r\n";

    #[test]
    fn validates_simple_event() {
        let result =
            validate_calendar_object(&wrap(EVENT), Limits::default(), chrono_tz::UTC, false)
                .unwrap();
        assert_eq!(result.uid, "event-1");
        assert_eq!(result.component, ItemComponent::VEvent);
        assert!(result.canonical.ends_with("\r\n"));
    }

    #[test]
    fn accepts_todo_and_journal() {
        for (body, expected) in [
            (
                "BEGIN:VTODO\r\nUID:t\r\nDUE:20261022T100000Z\r\nEND:VTODO\r\n",
                ItemComponent::VTodo,
            ),
            (
                "BEGIN:VJOURNAL\r\nUID:j\r\nDTSTART:20261022T100000Z\r\nEND:VJOURNAL\r\n",
                ItemComponent::VJournal,
            ),
        ] {
            let result =
                validate_calendar_object(&wrap(body), Limits::default(), chrono_tz::UTC, false)
                    .unwrap();
            assert_eq!(result.component, expected);
        }
    }

    #[test]
    fn rejects_invalid() {
        for input in [
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261022T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:1.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261022T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VFREEBUSY\r\nUID:1\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261022T100000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:2\r\nDTSTART:20261022T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261022\r\nDTEND:20261022T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ] {
            assert!(
                validate_calendar_object(input, Limits::default(), chrono_tz::UTC, false).is_err(),
                "accepted {input:?}"
            );
        }
    }

    #[test]
    fn generates_uid_when_allowed() {
        let input = wrap("BEGIN:VTODO\r\nSUMMARY:no uid\r\nEND:VTODO\r\n");
        assert!(
            validate_calendar_object(&input, Limits::default(), chrono_tz::UTC, false).is_err()
        );
        let result =
            validate_calendar_object(&input, Limits::default(), chrono_tz::UTC, true).unwrap();
        assert!(result.canonical.contains("UID:"));
        assert!(result.uid.ends_with("@oxicale"));
    }

    #[test]
    fn recurrence_set_allowed() {
        let body = format!(
            "{EVENT}BEGIN:VEVENT\r\nUID:event-1\r\nRECURRENCE-ID:20261023T100000Z\r\nDTSTART:20261023T120000Z\r\nDTEND:20261023T130000Z\r\nEND:VEVENT\r\n"
        );
        let result =
            validate_calendar_object(&wrap(&body), Limits::default(), chrono_tz::UTC, false)
                .unwrap();
        assert_eq!(result.uid, "event-1");
    }

    #[test]
    fn timezone_reference_must_resolve() {
        let body =
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=Nowhere/Nope:20261022T100000\r\nEND:VEVENT\r\n";
        assert!(
            validate_calendar_object(&wrap(body), Limits::default(), chrono_tz::UTC, false)
                .is_err()
        );
    }

    #[test]
    fn canonical_payload_is_stable() {
        let a = validate_calendar_object(&wrap(EVENT), Limits::default(), chrono_tz::UTC, false)
            .unwrap();
        let b = validate_calendar_object(
            &wrap(EVENT).replace("\r\n", "\n"),
            Limits::default(),
            chrono_tz::UTC,
            false,
        )
        .unwrap();
        assert_eq!(a.canonical, b.canonical);
    }
}
