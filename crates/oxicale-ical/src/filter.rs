use chrono_tz::Tz;

use crate::model::{Component, IcalTimeRange, Property};
use crate::recurrence::item_matches_range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterResult {
    Match,
    NoMatch,
    Unsupported(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMatchKind {
    Equals,
    Contains,
    StartsWith,
    EndsWith,
}

#[derive(Clone, Debug)]
pub struct TextMatch {
    pub value: String,
    pub kind: TextMatchKind,
    pub negate: bool,
    pub case_sensitive: bool,
}

impl TextMatch {
    pub fn matches(&self, value: &str) -> bool {
        let needle = if self.case_sensitive {
            self.value.clone()
        } else {
            self.value.to_lowercase()
        };
        let haystack = if self.case_sensitive {
            value.to_string()
        } else {
            value.to_lowercase()
        };
        let result = match self.kind {
            TextMatchKind::Equals => haystack == needle,
            TextMatchKind::Contains => haystack.contains(&needle),
            TextMatchKind::StartsWith => haystack.starts_with(&needle),
            TextMatchKind::EndsWith => haystack.ends_with(&needle),
        };
        if self.negate {
            !result
        } else {
            result
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParamFilter {
    pub name: String,
    pub is_not_defined: bool,
    pub text_matches: Vec<TextMatch>,
}

#[derive(Clone, Debug)]
pub struct PropFilter {
    pub name: String,
    pub is_not_defined: bool,
    pub text_matches: Vec<TextMatch>,
    pub param_filters: Vec<ParamFilter>,
}

#[derive(Clone, Debug)]
pub struct ComponentFilter {
    pub name: Option<String>,
    pub is_not_defined: bool,
    pub time_range: Option<IcalTimeRange>,
    pub prop_filters: Vec<PropFilter>,
    pub comp_filters: Vec<ComponentFilter>,
}

impl ComponentFilter {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            is_not_defined: false,
            time_range: None,
            prop_filters: Vec::new(),
            comp_filters: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CalendarFilter {
    pub root: ComponentFilter,
}

impl CalendarFilter {
    pub fn match_all() -> Self {
        let mut root = ComponentFilter::named("VCALENDAR");
        root.comp_filters.push(ComponentFilter::named("VEVENT"));
        root.comp_filters.push(ComponentFilter::named("VTODO"));
        root.comp_filters.push(ComponentFilter::named("VJOURNAL"));
        // A calendar-query with no useful filter should match any supported
        // component. We represent that by no sub-filters.
        root.comp_filters.clear();
        Self { root }
    }

    pub fn matches(&self, calendar: &Component, default_tz: Tz) -> FilterResult {
        if let Some(name) = &self.root.name {
            if !name.eq_ignore_ascii_case(&calendar.name) {
                return FilterResult::NoMatch;
            }
        }
        if self.root.is_not_defined {
            return FilterResult::NoMatch;
        }
        if let Some(range) = &self.root.time_range {
            match item_matches_range(calendar, range, default_tz) {
                Ok(true) => {}
                Ok(false) => return FilterResult::NoMatch,
                Err(error) => return FilterResult::Unsupported(error.to_string()),
            }
        }
        if !prop_filters_match(&calendar.properties, &self.root.prop_filters) {
            return FilterResult::NoMatch;
        }
        for filter in &self.root.comp_filters {
            if !component_filter_satisfied(calendar, filter, default_tz) {
                return FilterResult::NoMatch;
            }
        }
        FilterResult::Match
    }
}

fn component_filter_satisfied(parent: &Component, filter: &ComponentFilter, tz: Tz) -> bool {
    let matching: Vec<&Component> = parent
        .children
        .iter()
        .filter(|child| match &filter.name {
            Some(name) => child.name.eq_ignore_ascii_case(name),
            None => true,
        })
        .collect();
    if filter.is_not_defined {
        return matching.is_empty();
    }
    matching
        .into_iter()
        .any(|component| component_matches(component, filter, tz))
}

fn component_matches(component: &Component, filter: &ComponentFilter, tz: Tz) -> bool {
    if let Some(name) = &filter.name {
        if !component.name.eq_ignore_ascii_case(name) {
            return false;
        }
    }
    if !prop_filters_match(&component.properties, &filter.prop_filters) {
        return false;
    }
    if let Some(range) = &filter.time_range {
        let mut wrapper = Component::new("VCALENDAR");
        wrapper.children.push(component.clone());
        match item_matches_range(&wrapper, range, tz) {
            Ok(true) => {}
            _ => return false,
        }
    }
    for child_filter in &filter.comp_filters {
        if !component_filter_satisfied(component, child_filter, tz) {
            return false;
        }
    }
    true
}

fn prop_filters_match(properties: &[Property], filters: &[PropFilter]) -> bool {
    filters.iter().all(|filter| {
        let matching: Vec<&Property> = properties
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(&filter.name))
            .collect();
        if filter.is_not_defined {
            return matching.is_empty();
        }
        matching.into_iter().any(|property| {
            filter
                .text_matches
                .iter()
                .all(|text| text.matches(&property.text()))
                && filter.param_filters.iter().all(|param| {
                    let values: Vec<&String> = property
                        .params
                        .iter()
                        .filter(|(name, _)| name.eq_ignore_ascii_case(&param.name))
                        .flat_map(|(_, values)| values.iter())
                        .collect();
                    if param.is_not_defined {
                        return values.is_empty();
                    }
                    values
                        .into_iter()
                        .any(|value| param.text_matches.iter().all(|text| text.matches(value)))
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_calendar, Limits};

    fn cal(body: &str) -> Component {
        parse_calendar(
            &format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n"),
            Limits::default(),
        )
        .unwrap()
    }

    fn text(value: &str) -> TextMatch {
        TextMatch {
            value: value.to_string(),
            kind: TextMatchKind::Contains,
            negate: false,
            case_sensitive: false,
        }
    }

    #[test]
    fn component_filter() {
        let calendar = cal("BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20261022T100000Z\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:t\r\nDUE:20261023T100000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\n");
        let mut root = ComponentFilter::named("VCALENDAR");
        root.comp_filters.push(ComponentFilter::named("VTODO"));
        let filter = CalendarFilter { root };
        assert_eq!(
            filter.matches(&calendar, chrono_tz::UTC),
            FilterResult::Match
        );

        let mut root = ComponentFilter::named("VCALENDAR");
        root.comp_filters.push(ComponentFilter::named("VJOURNAL"));
        let filter = CalendarFilter { root };
        assert_eq!(
            filter.matches(&calendar, chrono_tz::UTC),
            FilterResult::NoMatch
        );
    }

    #[test]
    fn time_range_and_property_text() {
        let calendar = cal("BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Team Meeting\r\nEND:VEVENT\r\n");
        let mut event = ComponentFilter::named("VEVENT");
        event.time_range = Some(IcalTimeRange {
            start: Some("2026-10-22T00:00:00Z".parse().unwrap()),
            end: Some("2026-10-23T00:00:00Z".parse().unwrap()),
        });
        event.prop_filters.push(PropFilter {
            name: "SUMMARY".to_string(),
            is_not_defined: false,
            text_matches: vec![text("meeting")],
            param_filters: Vec::new(),
        });
        let mut root = ComponentFilter::named("VCALENDAR");
        root.comp_filters.push(event);
        let filter = CalendarFilter { root };
        assert_eq!(
            filter.matches(&calendar, chrono_tz::UTC),
            FilterResult::Match
        );
    }

    #[test]
    fn is_not_defined_property() {
        let calendar = cal("BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20261022T100000Z\r\nEND:VEVENT\r\n");
        let mut event = ComponentFilter::named("VEVENT");
        event.prop_filters.push(PropFilter {
            name: "SUMMARY".to_string(),
            is_not_defined: true,
            text_matches: Vec::new(),
            param_filters: Vec::new(),
        });
        let mut root = ComponentFilter::named("VCALENDAR");
        root.comp_filters.push(event);
        let filter = CalendarFilter { root };
        assert_eq!(
            filter.matches(&calendar, chrono_tz::UTC),
            FilterResult::Match
        );
    }

    #[test]
    fn text_match_kinds() {
        let base = TextMatch {
            value: "Hello".into(),
            kind: TextMatchKind::Equals,
            negate: false,
            case_sensitive: false,
        };
        assert!(base.matches("hello"));
        assert!(!base.matches("hello!"));
        assert!(TextMatch {
            kind: TextMatchKind::StartsWith,
            ..base.clone()
        }
        .matches("Hello world"));
        assert!(TextMatch {
            kind: TextMatchKind::EndsWith,
            ..base.clone()
        }
        .matches("say hello"));
        assert!(TextMatch {
            negate: true,
            ..base.clone()
        }
        .matches("goodbye"));
    }
}
