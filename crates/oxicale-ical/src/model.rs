use chrono::{DateTime, Utc};

/// A single unfolded iCalendar content line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, Vec<String>)>,
    pub value: String,
}

impl Property {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into().to_ascii_uppercase(),
            params: Vec::new(),
            value: value.into(),
        }
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, values)| values.first())
            .map(String::as_str)
    }

    pub fn text(&self) -> String {
        unescape_text(&self.value)
    }
}

/// A component with its properties and nested components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub properties: Vec<Property>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into().to_ascii_uppercase(),
            properties: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn properties<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.properties
            .iter()
            .filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn set_property(&mut self, property: Property) {
        if let Some(existing) = self
            .properties
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&property.name))
        {
            *existing = property;
        } else {
            self.properties.push(property);
        }
    }

    pub fn remove_property(&mut self, name: &str) {
        self.properties
            .retain(|p| !p.name.eq_ignore_ascii_case(name));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcalTimeRange {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

impl IcalTimeRange {
    pub fn unbounded() -> Self {
        Self {
            start: None,
            end: None,
        }
    }

    pub fn contains(&self, instant: DateTime<Utc>) -> bool {
        self.start.is_none_or(|start| instant >= start) && self.end.is_none_or(|end| instant < end)
    }

    pub fn overlaps(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
        if start == end {
            return self.contains(start);
        }
        let after_start = self.start.is_none_or(|s| end > s);
        let before_end = self.end.is_none_or(|e| start < e);
        after_start && before_end
    }
}

/// Unescape RFC 5545 TEXT values.
pub fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(';') => out.push(';'),
                Some(',') => out.push(','),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escaping_roundtrip() {
        let original = "a,b;c\\d\ne";
        assert_eq!(unescape_text(&escape_text(original)), original);
    }

    #[test]
    fn range_semantics() {
        let start = "2026-01-01T00:00:00Z".parse().unwrap();
        let end = "2026-01-02T00:00:00Z".parse().unwrap();
        let range = IcalTimeRange {
            start: Some(start),
            end: Some(end),
        };
        assert!(range.overlaps(start, start + chrono::Duration::hours(1)));
        assert!(!range.overlaps(end, end + chrono::Duration::hours(1)));
        assert!(range.contains(start));
        assert!(!range.contains(end));
    }
}
