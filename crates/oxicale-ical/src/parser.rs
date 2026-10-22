use std::fmt;

use thiserror::Error;

use crate::model::{Component, Property};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IcalError {
    #[error("invalid iCalendar: {0}")]
    Invalid(String),
    #[error("unsupported iCalendar feature: {0}")]
    Unsupported(String),
    #[error("iCalendar exceeds {0} limit")]
    Limit(&'static str),
}

type ContentLine = (String, Vec<(String, Vec<String>)>, String);

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_properties: usize,
    pub max_components: usize,
    pub max_depth: usize,
    pub max_line_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1024 * 1024,
            max_lines: 200_000,
            max_properties: 500_000,
            max_components: 100_000,
            max_depth: 32,
            max_line_bytes: 64 * 1024,
        }
    }
}

/// Parse an iCalendar document into a component tree.
///
/// This parser deliberately implements only the small, safe subset needed by
/// CalDAV. It rejects a missing VCALENDAR wrapper, malformed content lines,
/// excessive nesting and component mismatches.
pub fn parse_calendar(input: &str, limits: Limits) -> Result<Component, IcalError> {
    if input.len() > limits.max_bytes {
        return Err(IcalError::Limit("size"));
    }
    if input
        .chars()
        .any(|c| c.is_control() && c != '\r' && c != '\n' && c != '\t')
    {
        return Err(IcalError::Invalid(
            "control characters are not allowed".to_string(),
        ));
    }

    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut unfolded: Vec<String> = Vec::new();
    let mut line_count = 0usize;
    for line in normalized.split('\n') {
        line_count += 1;
        if line_count > limits.max_lines {
            return Err(IcalError::Limit("line count"));
        }
        if line.ends_with('\n') {
            continue;
        }
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            let previous = unfolded
                .last_mut()
                .ok_or_else(|| IcalError::Invalid("folded line without predecessor".into()))?;
            previous.push_str(rest);
            if previous.len() > limits.max_line_bytes {
                return Err(IcalError::Limit("line length"));
            }
        } else {
            unfolded.push(line.to_string());
        }
    }
    while unfolded.last().is_some_and(|line| line.is_empty()) {
        unfolded.pop();
    }
    if unfolded.is_empty() {
        return Err(IcalError::Invalid("empty document".to_string()));
    }

    let mut stack: Vec<Component> = Vec::new();
    let mut roots: Vec<Component> = Vec::new();
    let mut total_properties = 0usize;
    let mut total_components = 0usize;

    for line in unfolded {
        if line.is_empty() {
            continue;
        }
        let (name, params, value) = parse_content_line(&line)?;
        match name.as_str() {
            "BEGIN" => {
                let component_name = value.trim().to_ascii_uppercase();
                if component_name.is_empty()
                    || !component_name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    return Err(IcalError::Invalid("invalid component name".to_string()));
                }
                total_components += 1;
                if total_components > limits.max_components {
                    return Err(IcalError::Limit("component count"));
                }
                if stack.len() >= limits.max_depth {
                    return Err(IcalError::Limit("component depth"));
                }
                stack.push(Component::new(component_name));
            }
            "END" => {
                let component_name = value.trim().to_ascii_uppercase();
                let component = stack
                    .pop()
                    .ok_or_else(|| IcalError::Invalid("END without BEGIN".to_string()))?;
                if component.name != component_name {
                    return Err(IcalError::Invalid(format!(
                        "END:{component_name} does not match BEGIN:{}",
                        component.name
                    )));
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(component);
                } else {
                    roots.push(component);
                }
            }
            _ => {
                total_properties += 1;
                if total_properties > limits.max_properties {
                    return Err(IcalError::Limit("property count"));
                }
                let current = stack.last_mut().ok_or_else(|| {
                    IcalError::Invalid("property outside of a component".to_string())
                })?;
                current.properties.push(Property {
                    name,
                    params,
                    value,
                });
            }
        }
    }

    if !stack.is_empty() {
        return Err(IcalError::Invalid("unterminated component".to_string()));
    }
    if roots.len() != 1 {
        return Err(IcalError::Invalid(
            "document must contain exactly one top-level component".to_string(),
        ));
    }
    let root = roots.pop().unwrap();
    if root.name != "VCALENDAR" {
        return Err(IcalError::Invalid(
            "top-level component must be VCALENDAR".to_string(),
        ));
    }
    Ok(root)
}

fn parse_content_line(line: &str) -> Result<ContentLine, IcalError> {
    let bytes = line.as_bytes();
    let mut colon = None;
    let mut quoted = false;
    for (idx, byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b':' if !quoted => {
                colon = Some(idx);
                break;
            }
            _ => {}
        }
    }
    let colon = colon.ok_or_else(|| IcalError::Invalid("content line has no value".to_string()))?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut pieces = split_unquoted(head, ';');
    let name = pieces
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(IcalError::Invalid("invalid property name".to_string()));
    }
    let mut params = Vec::new();
    for piece in pieces {
        let (key, raw_value) = piece
            .split_once('=')
            .ok_or_else(|| IcalError::Invalid("parameter without value".to_string()))?;
        let key = key.trim().to_ascii_uppercase();
        if key.is_empty() {
            return Err(IcalError::Invalid("empty parameter name".to_string()));
        }
        let mut values = Vec::new();
        for item in split_unquoted(raw_value, ',') {
            let item = item.trim();
            let item = item
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(item);
            values.push(item.to_string());
        }
        params.push((key, values));
    }
    Ok((name, params, value.to_string()))
}

fn split_unquoted(value: &str, separator: char) -> impl Iterator<Item = &str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    for (idx, c) in value.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c == separator && !quoted => {
                parts.push(&value[start..idx]);
                start = idx + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts.into_iter()
}

/// Serialise a component tree with CRLF line endings and safe folding.
pub fn serialize_component(component: &Component) -> String {
    let mut out = String::new();
    write_component(component, &mut out);
    out
}

fn write_component(component: &Component, out: &mut String) {
    fold_line(&format!("BEGIN:{}", component.name), out);
    for property in &component.properties {
        let mut line = property.name.clone();
        for (key, values) in &property.params {
            line.push(';');
            line.push_str(key);
            line.push('=');
            let encoded: Vec<String> = values
                .iter()
                .map(|v| {
                    if v.contains([';', ',', ':']) {
                        format!("\"{v}\"")
                    } else {
                        v.clone()
                    }
                })
                .collect();
            line.push_str(&encoded.join(","));
        }
        line.push(':');
        line.push_str(&property.value);
        fold_line(&line, out);
    }
    for child in &component.children {
        write_component(child, out);
    }
    fold_line(&format!("END:{}", component.name), out);
}

fn fold_line(line: &str, out: &mut String) {
    let bytes = line.as_bytes();
    if bytes.len() <= 75 {
        out.push_str(line);
        out.push_str("\r\n");
        return;
    }
    let mut offset = 0usize;
    let mut first = true;
    while offset < bytes.len() {
        let budget = if first { 75 } else { 74 };
        let mut end = (offset + budget).min(bytes.len());
        while end > offset && !line.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset {
            end = (offset + budget).min(bytes.len());
            while end < bytes.len() && !line.is_char_boundary(end) {
                end += 1;
            }
        }
        if !first {
            out.push(' ');
        }
        out.push_str(&line[offset..end]);
        out.push_str("\r\n");
        offset = end;
        first = false;
    }
}

impl fmt::Display for Limits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IcalLimits({} bytes)", self.max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_calendar() {
        let input =
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Hello\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let cal = parse_calendar(input, Limits::default()).unwrap();
        assert_eq!(cal.name, "VCALENDAR");
        assert_eq!(cal.children.len(), 1);
        assert_eq!(cal.children[0].property("SUMMARY").unwrap().value, "Hello");
    }

    #[test]
    fn unfolds_lines() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Hello \r\n World\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let cal = parse_calendar(input, Limits::default()).unwrap();
        assert_eq!(
            cal.children[0].property("SUMMARY").unwrap().value,
            "Hello World"
        );
    }

    #[test]
    fn parses_parameters_and_quoted_values() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nDTSTART;TZID=\"Europe/Paris\";X-A=\"a,b\":20260101T100000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let cal = parse_calendar(input, Limits::default()).unwrap();
        let prop = cal.children[0].property("DTSTART").unwrap();
        assert_eq!(prop.param("TZID"), Some("Europe/Paris"));
        assert_eq!(prop.param("X-A"), Some("a,b"));
    }

    #[test]
    fn rejects_bad_documents() {
        for input in [
            "",
            "VERSION:2.0",
            "BEGIN:VCALENDAR\r\nEND:VEVENT\r\n",
            "BEGIN:VCALENDAR\r\nUID:1\r\n",
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nbad line\r\nEND:VCALENDAR\r\n",
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX:\u{1}\r\nEND:VCALENDAR\r\n",
        ] {
            assert!(
                parse_calendar(input, Limits::default()).is_err(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn rejects_limits() {
        let limits = Limits {
            max_bytes: 5,
            ..Limits::default()
        };
        assert!(parse_calendar("BEGIN:VCALENDAR\r\n", limits).is_err());
        let limits = Limits {
            max_depth: 2,
            ..Limits::default()
        };
        assert!(parse_calendar(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nBEGIN:VALARM\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            limits
        )
        .is_err());
    }

    #[test]
    fn serialization_folds_and_roundtrips() {
        let input = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            "x".repeat(200)
        );
        let cal = parse_calendar(&input, Limits::default()).unwrap();
        let serialized = serialize_component(&cal);
        assert!(serialized.contains("\r\n "));
        let reparsed = parse_calendar(&serialized, Limits::default()).unwrap();
        assert_eq!(
            reparsed.children[0].property("SUMMARY").unwrap().value,
            "x".repeat(200)
        );
    }
}
