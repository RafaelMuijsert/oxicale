use std::collections::{BTreeMap, BTreeSet};

use chrono_tz::Tz;
use oxicale_core::{ComponentSet, ItemComponent};
use sha2::{Digest, Sha256};

use crate::model::Component;
use crate::parser::{parse_calendar, serialize_component, IcalError, Limits};
use crate::validate::validate_calendar_object;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedItem {
    pub uid: Option<String>,
    pub remote_key: String,
    pub component: ItemComponent,
    pub payload: String,
}

fn hash_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn collect_tzids(component: &Component, out: &mut BTreeSet<String>) {
    for property in &component.properties {
        if let Some(tzid) = property.param("TZID") {
            out.insert(tzid.to_string());
        }
    }
    for child in &component.children {
        collect_tzids(child, out);
    }
}

fn base_calendar() -> Component {
    let mut root = Component::new("VCALENDAR");
    root.properties
        .push(crate::model::Property::new("VERSION", "2.0"));
    root.properties.push(crate::model::Property::new(
        "PRODID",
        "-//Oxicale//Oxicale 0.1//EN",
    ));
    root
}

/// Split an aggregate remote calendar into logical items grouped by UID.
///
/// Components without a UID receive a deterministic generated UID based on
/// their serialised content, as required by the subscription reconciliation
/// model.
pub fn split_calendar(
    input: &str,
    limits: Limits,
    default_tz: Tz,
) -> Result<Vec<FetchedItem>, IcalError> {
    split_calendar_with_options(input, limits, default_tz, false)
}

pub fn split_calendar_with_options(
    input: &str,
    limits: Limits,
    default_tz: Tz,
    skip_broken: bool,
) -> Result<Vec<FetchedItem>, IcalError> {
    let calendar = parse_calendar(input, limits)?;
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
    for child in &calendar.children {
        if child.name == "VTIMEZONE" {
            if let Some(tzid) = child.property("TZID") {
                timezones
                    .entry(tzid.value.trim().to_string())
                    .or_insert_with(|| child.clone());
            }
        } else if !matches!(child.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL") {
            // Unknown components are ignored for storage.
        }
    }

    let mut groups: BTreeMap<String, Vec<Component>> = BTreeMap::new();
    let mut generated_uid_by_key: BTreeMap<String, String> = BTreeMap::new();
    for component in &calendar.children {
        if !matches!(component.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL") {
            continue;
        }
        let uid = component
            .property("UID")
            .map(|p| p.value.trim().to_string())
            .filter(|v| !v.is_empty());
        let key = match uid {
            Some(uid) => uid,
            None => {
                let deterministic = hash_hex(&serialize_component(component));
                generated_uid_by_key.insert(deterministic.clone(), deterministic.clone());
                deterministic
            }
        };
        groups.entry(key).or_default().push(component.clone());
    }

    let mut result = Vec::new();
    for (key, mut components) in groups {
        if let Some(generated) = generated_uid_by_key.get(&key) {
            for component in &mut components {
                if component.property("UID").is_none() {
                    component
                        .properties
                        .insert(0, crate::model::Property::new("UID", generated.clone()));
                }
            }
        }
        components.sort_by_key(|component| {
            (
                component
                    .property("RECURRENCE-ID")
                    .map(|p| p.value.clone())
                    .unwrap_or_default(),
                serialize_component(component),
            )
        });

        let mut root = base_calendar();
        root.properties
            .push(crate::model::Property::new("CALSCALE", "GREGORIAN"));
        let mut referenced = BTreeSet::new();
        for component in &components {
            collect_tzids(component, &mut referenced);
        }
        for tzid in &referenced {
            if let Some(timezone) = timezones.get(tzid) {
                root.children.push(timezone.clone());
            }
        }
        root.children.extend(components);

        let payload = serialize_component(&root);
        if payload.len() > limits.max_bytes {
            return Err(IcalError::Limit("item size"));
        }
        let validated = match validate_calendar_object(&payload, limits, default_tz, false) {
            Ok(validated) => validated,
            Err(_) if skip_broken => continue,
            Err(error) => return Err(error),
        };
        let component = validated.component;
        result.push(FetchedItem {
            uid: validated.uid.clone().into(),
            remote_key: key,
            component,
            payload: validated.canonical,
        });
    }
    result.sort_by(|a, b| a.remote_key.cmp(&b.remote_key));
    Ok(result)
}

/// Merge selected items into a deterministic VCALENDAR for a component feed.
pub fn generate_feed(
    items: &[String],
    components: ComponentSet,
    name: Option<&str>,
    prodid: &str,
    limits: Limits,
) -> Result<String, IcalError> {
    let mut root = Component::new("VCALENDAR");
    root.properties
        .push(crate::model::Property::new("VERSION", "2.0"));
    root.properties
        .push(crate::model::Property::new("PRODID", prodid));
    root.properties
        .push(crate::model::Property::new("CALSCALE", "GREGORIAN"));
    if let Some(name) = name {
        root.properties.push(crate::model::Property::new(
            "X-WR-CALNAME",
            crate::model::escape_text(name),
        ));
    }

    let mut selected_components: Vec<Component> = Vec::new();
    let mut timezones: BTreeMap<String, Component> = BTreeMap::new();
    let mut referenced: BTreeSet<String> = BTreeSet::new();

    for payload in items {
        let calendar = parse_calendar(payload, limits)?;
        for child in &calendar.children {
            if child.name == "VTIMEZONE" {
                if let Some(tzid) = child.property("TZID") {
                    timezones
                        .entry(tzid.value.trim().to_string())
                        .or_insert_with(|| child.clone());
                }
            }
        }
        for child in &calendar.children {
            let Some(component_type) = (match child.name.as_str() {
                "VEVENT" => Some(oxicale_core::CalendarComponent::VEvent),
                "VTODO" => Some(oxicale_core::CalendarComponent::VTodo),
                "VJOURNAL" => Some(oxicale_core::CalendarComponent::VJournal),
                _ => None,
            }) else {
                continue;
            };
            if !components.contains(component_type) {
                continue;
            }
            collect_tzids(child, &mut referenced);
            selected_components.push(child.clone());
        }
    }

    selected_components.sort_by_key(|component| {
        (
            component
                .property("UID")
                .map(|p| p.value.clone())
                .unwrap_or_default(),
            component
                .property("RECURRENCE-ID")
                .map(|p| p.value.clone())
                .unwrap_or_default(),
            serialize_component(component),
        )
    });

    for tzid in &referenced {
        if let Some(timezone) = timezones.get(tzid) {
            root.children.push(timezone.clone());
        }
    }
    root.children.extend(selected_components);
    Ok(serialize_component(&root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxicale_core::CalendarComponent;

    fn aggregate() -> String {
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n\
BEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Event\r\nEND:VEVENT\r\n\
BEGIN:VTODO\r\nUID:t1\r\nDUE:20261023T100000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\n\
BEGIN:VJOURNAL\r\nUID:j1\r\nDTSTART;VALUE=DATE:20261024\r\nSUMMARY:Journal\r\nEND:VJOURNAL\r\n\
END:VCALENDAR\r\n"
            .to_string()
    }

    #[test]
    fn splits_into_items() {
        let items = split_calendar(&aggregate(), Limits::default(), chrono_tz::UTC).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].component, ItemComponent::VEvent);
        assert!(items.iter().all(|i| i.payload.ends_with("\r\n")));
        assert!(items.iter().all(|i| i.uid.is_some()));
    }

    #[test]
    fn groups_recurrence_set() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:e1\r\nRECURRENCE-ID:20261023T100000Z\r\nDTSTART:20261023T120000Z\r\nDTEND:20261023T130000Z\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
        let items = split_calendar(input, Limits::default(), chrono_tz::UTC).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].payload.contains("RECURRENCE-ID"));
    }

    #[test]
    fn generated_uid_is_deterministic() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nSUMMARY:No uid\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let a = split_calendar(input, Limits::default(), chrono_tz::UTC).unwrap();
        let b = split_calendar(input, Limits::default(), chrono_tz::UTC).unwrap();
        assert_eq!(a[0].remote_key, b[0].remote_key);
        assert_eq!(a[0].payload, b[0].payload);
    }

    #[test]
    fn feed_filters_components() {
        let items = split_calendar(&aggregate(), Limits::default(), chrono_tz::UTC)
            .unwrap()
            .into_iter()
            .map(|i| i.payload)
            .collect::<Vec<_>>();
        let body = generate_feed(
            &items,
            ComponentSet::one(CalendarComponent::VTodo),
            Some("Tasks only"),
            "-//Oxicale//Test//EN",
            Limits::default(),
        )
        .unwrap();
        assert!(body.contains("BEGIN:VTODO"));
        assert!(!body.contains("BEGIN:VEVENT"));
        assert!(!body.contains("BEGIN:VJOURNAL"));
        assert!(body.contains("X-WR-CALNAME:Tasks only"));
    }

    #[test]
    fn feed_is_deterministic() {
        let items = split_calendar(&aggregate(), Limits::default(), chrono_tz::UTC)
            .unwrap()
            .into_iter()
            .map(|i| i.payload)
            .collect::<Vec<_>>();
        let a = generate_feed(
            &items,
            ComponentSet::all(),
            None,
            "-//Oxicale//Test//EN",
            Limits::default(),
        )
        .unwrap();
        let mut reversed = items.clone();
        reversed.reverse();
        let b = generate_feed(
            &reversed,
            ComponentSet::all(),
            None,
            "-//Oxicale//Test//EN",
            Limits::default(),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn feed_includes_timezones() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:e1\r\nDTSTART;TZID=Europe/Paris:20261022T120000\r\nDTEND;TZID=Europe/Paris:20261022T130000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let items = split_calendar(input, Limits::default(), chrono_tz::UTC)
            .unwrap()
            .into_iter()
            .map(|i| i.payload)
            .collect::<Vec<_>>();
        let body = generate_feed(
            &items,
            ComponentSet::all(),
            None,
            "-//Oxicale//Test//EN",
            Limits::default(),
        )
        .unwrap();
        assert!(body.contains("BEGIN:VTIMEZONE"));
        assert!(body.contains("TZID:Europe/Paris"));
    }
}
