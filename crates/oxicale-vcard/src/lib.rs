//! vCard 3.0/4.0 parsing, validation, canonicalisation and query filtering.

use std::fmt;

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VcardError {
    #[error("invalid vCard: {0}")]
    Invalid(String),
    #[error("unsupported vCard version: {0}")]
    UnsupportedVersion(String),
    #[error("vCard exceeds {0} limit")]
    Limit(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub struct VcardLimits {
    pub max_bytes: usize,
    pub max_properties: usize,
    pub max_line_bytes: usize,
}

impl Default for VcardLimits {
    fn default() -> Self {
        Self {
            max_bytes: 10 * 1024 * 1024,
            max_properties: 100_000,
            max_line_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub group: Option<String>,
    pub name: String,
    pub params: Vec<(String, Vec<String>)>,
    pub value: String,
}

impl Property {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, values)| values.first())
            .map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VCard {
    pub properties: Vec<Property>,
}

impl VCard {
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
}

#[derive(Clone, Debug)]
pub struct ValidatedVcard {
    pub uid: String,
    pub version: String,
    pub card: VCard,
    pub canonical: String,
}

fn unfold(input: &str, limits: VcardLimits) -> Result<Vec<String>, VcardError> {
    if input.len() > limits.max_bytes {
        return Err(VcardError::Limit("size"));
    }
    if input
        .chars()
        .any(|c| c.is_control() && c != '\r' && c != '\n' && c != '\t')
    {
        return Err(VcardError::Invalid(
            "control characters are not allowed".to_string(),
        ));
    }
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    for line in normalized.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            let previous = lines
                .last_mut()
                .ok_or_else(|| VcardError::Invalid("folded line without predecessor".into()))?;
            previous.push_str(rest);
            if previous.len() > limits.max_line_bytes {
                return Err(VcardError::Limit("line length"));
            }
        } else {
            lines.push(line.to_string());
        }
    }
    Ok(lines)
}

fn split_unquoted(value: &str, separator: char) -> Vec<&str> {
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
    parts
}

fn parse_line(line: &str) -> Result<Property, VcardError> {
    let mut colon = None;
    let mut quoted = false;
    for (idx, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(idx);
                break;
            }
            _ => {}
        }
    }
    let colon = colon.ok_or_else(|| VcardError::Invalid("content line has no value".into()))?;
    let head = &line[..colon];
    let value = &line[colon + 1..];
    let mut pieces = split_unquoted(head, ';').into_iter();
    let mut name_part = pieces.next().unwrap_or_default().trim().to_string();
    let mut group = None;
    if let Some((prefix, name)) = name_part.split_once('.') {
        group = Some(prefix.to_ascii_uppercase());
        name_part = name.to_string();
    }
    let name = name_part.to_ascii_uppercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(VcardError::Invalid("invalid property name".into()));
    }
    let mut params = Vec::new();
    for piece in pieces {
        let (key, raw) = piece
            .split_once('=')
            .ok_or_else(|| VcardError::Invalid("parameter without value".into()))?;
        let values = split_unquoted(raw, ',')
            .into_iter()
            .map(|v| {
                v.trim()
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(v.trim())
                    .to_string()
            })
            .collect();
        params.push((key.trim().to_ascii_uppercase(), values));
    }
    Ok(Property {
        group,
        name,
        params,
        value: value.to_string(),
    })
}

/// Parse a single vCard object.
pub fn parse_vcard(input: &str, limits: VcardLimits) -> Result<VCard, VcardError> {
    let lines = unfold(input, limits)?;
    if lines.len() < 2
        || !lines[0].eq_ignore_ascii_case("BEGIN:VCARD")
        || !lines
            .last()
            .is_some_and(|l| l.eq_ignore_ascii_case("END:VCARD"))
    {
        return Err(VcardError::Invalid(
            "document must be a single VCARD".to_string(),
        ));
    }
    if lines.iter().any(|line| {
        line.eq_ignore_ascii_case("BEGIN:VCARD") || line.eq_ignore_ascii_case("END:VCARD")
    }) && lines
        .iter()
        .filter(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
        .count()
        != 1
    {
        return Err(VcardError::Invalid(
            "document must contain exactly one VCARD".to_string(),
        ));
    }

    let mut card = VCard {
        properties: Vec::new(),
    };
    for line in &lines[1..lines.len() - 1] {
        if line.trim().is_empty() {
            continue;
        }
        if line.to_ascii_uppercase().starts_with("BEGIN:")
            || line.to_ascii_uppercase().starts_with("END:")
        {
            return Err(VcardError::Invalid(
                "nested components are not allowed in vCard".to_string(),
            ));
        }
        card.properties.push(parse_line(line)?);
        if card.properties.len() > limits.max_properties {
            return Err(VcardError::Limit("property count"));
        }
    }
    Ok(card)
}

/// Parse all vCards in an aggregate document.
pub fn parse_vcards(input: &str, limits: VcardLimits) -> Result<Vec<VCard>, VcardError> {
    if input.len() > limits.max_bytes {
        return Err(VcardError::Limit("size"));
    }
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut cards = Vec::new();
    let mut current: Option<String> = None;
    for line in normalized.split('\n') {
        if line.eq_ignore_ascii_case("BEGIN:VCARD") {
            if current.is_some() {
                return Err(VcardError::Invalid("nested BEGIN:VCARD".to_string()));
            }
            current = Some("BEGIN:VCARD\n".to_string());
        } else if let Some(card) = current.as_mut() {
            card.push_str(line);
            card.push('\n');
            if line.eq_ignore_ascii_case("END:VCARD") {
                let card = current.take().unwrap();
                cards.push(parse_vcard(&card, limits)?);
            }
        } else if !line.trim().is_empty() {
            return Err(VcardError::Invalid("content outside of VCARD".to_string()));
        }
    }
    if current.is_some() {
        return Err(VcardError::Invalid("unterminated VCARD".to_string()));
    }
    Ok(cards)
}

/// Validate a vCard and return its canonical payload.
pub fn validate_vcard(input: &str, limits: VcardLimits) -> Result<ValidatedVcard, VcardError> {
    let card = parse_vcard(input, limits)?;
    let version = card
        .property("VERSION")
        .map(|p| p.value.trim().to_string())
        .ok_or_else(|| VcardError::Invalid("VERSION is required".to_string()))?;
    if version != "3.0" && version != "4.0" {
        return Err(VcardError::UnsupportedVersion(version));
    }
    let uid = card
        .property("UID")
        .map(|p| p.value.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| VcardError::Invalid("UID is required".to_string()))?;
    if uid.len() > 255 {
        return Err(VcardError::Invalid("UID is too long".to_string()));
    }
    let has_fn = card
        .property("FN")
        .is_some_and(|p| !p.value.trim().is_empty());
    let has_n = card
        .property("N")
        .is_some_and(|p| !p.value.trim().is_empty());
    if version == "3.0" && !has_fn {
        return Err(VcardError::Invalid("vCard 3.0 requires FN".to_string()));
    }
    if version == "4.0" && !has_fn && !has_n {
        return Err(VcardError::Invalid(
            "vCard 4.0 requires FN or N".to_string(),
        ));
    }
    let canonical = serialize_vcard(&card);
    Ok(ValidatedVcard {
        uid,
        version,
        card,
        canonical,
    })
}

pub fn serialize_vcard(card: &VCard) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCARD\r\n");
    for property in &card.properties {
        let mut line = String::new();
        if let Some(group) = &property.group {
            line.push_str(group);
            line.push('.');
        }
        line.push_str(&property.name);
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
        fold_line(&line, &mut out);
    }
    out.push_str("END:VCARD\r\n");
    out
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchTest {
    AnyOf,
    AllOf,
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
        let result = if self.case_sensitive {
            match self.kind {
                TextMatchKind::Equals => value == self.value,
                TextMatchKind::Contains => value.contains(&self.value),
                TextMatchKind::StartsWith => value.starts_with(&self.value),
                TextMatchKind::EndsWith => value.ends_with(&self.value),
            }
        } else {
            let needle = self.value.to_lowercase();
            let haystack = value.to_lowercase();
            match self.kind {
                TextMatchKind::Equals => haystack == needle,
                TextMatchKind::Contains => haystack.contains(&needle),
                TextMatchKind::StartsWith => haystack.starts_with(&needle),
                TextMatchKind::EndsWith => haystack.ends_with(&needle),
            }
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
pub struct VcardFilter {
    pub test: MatchTest,
    pub prop_filters: Vec<PropFilter>,
}

impl VcardFilter {
    pub fn match_all() -> Self {
        Self {
            test: MatchTest::AllOf,
            prop_filters: Vec::new(),
        }
    }

    pub fn matches(&self, card: &VCard) -> bool {
        if self.prop_filters.is_empty() {
            return true;
        }
        let results = self.prop_filters.iter().map(|filter| {
            let properties: Vec<&Property> = card
                .properties
                .iter()
                .filter(|p| p.name.eq_ignore_ascii_case(&filter.name))
                .collect();
            if filter.is_not_defined {
                return properties.is_empty();
            }
            properties.into_iter().any(|property| {
                filter
                    .text_matches
                    .iter()
                    .all(|m| m.matches(&property.value))
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
                            .any(|value| param.text_matches.iter().all(|m| m.matches(value)))
                    })
            })
        });
        match self.test {
            MatchTest::AllOf => results.into_iter().all(|v| v),
            MatchTest::AnyOf => results.into_iter().any(|v| v),
        }
    }
}

impl fmt::Display for VcardLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VcardLimits({} bytes)", self.max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VCARD3: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:123\r\nFN:Alice Example\r\nN:Example;Alice;;;\r\nEMAIL:alice@example.com\r\nEND:VCARD\r\n";
    const VCARD4: &str =
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:456\r\nFN:Bob\r\nEMAIL:bob@example.com\r\nEND:VCARD\r\n";

    #[test]
    fn validates_versions() {
        assert_eq!(
            validate_vcard(VCARD3, VcardLimits::default())
                .unwrap()
                .version,
            "3.0"
        );
        assert_eq!(
            validate_vcard(VCARD4, VcardLimits::default())
                .unwrap()
                .version,
            "4.0"
        );
    }

    #[test]
    fn requires_uid_and_names() {
        for input in [
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alice\r\nEND:VCARD\r\n",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nEND:VCARD\r\n",
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:1\r\nEND:VCARD\r\n",
        ] {
            assert!(validate_vcard(input, VcardLimits::default()).is_err());
        }
        let v4_n = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:1\r\nN:Smith;John;;;\r\nEND:VCARD\r\n";
        assert!(validate_vcard(v4_n, VcardLimits::default()).is_ok());
    }

    #[test]
    fn rejects_unsupported_version_and_documents() {
        for input in [
            "BEGIN:VCARD\r\nVERSION:2.1\r\nUID:1\r\nFN:A\r\nEND:VCARD\r\n",
            "UID:1\r\nFN:A\r\n",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:A\r\nBEGIN:VCARD\r\nEND:VCARD\r\n",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:A\r\n",
        ] {
            assert!(
                validate_vcard(input, VcardLimits::default()).is_err(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn canonical_stable() {
        let a = validate_vcard(VCARD3, VcardLimits::default()).unwrap();
        let b = validate_vcard(&VCARD3.replace("\r\n", "\n"), VcardLimits::default()).unwrap();
        assert_eq!(a.canonical, b.canonical);
        assert!(a.canonical.ends_with("\r\n"));
    }

    #[test]
    fn parses_groups_and_folding() {
        let input = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:1\r\nFN:Alice\r\nitem1.EMAIL;TYPE=work:alice@example.com\r\nNOTE:hello \r\n world\r\nEND:VCARD\r\n";
        let card = parse_vcard(input, VcardLimits::default()).unwrap();
        assert_eq!(card.property("NOTE").unwrap().value, "hello world");
        assert_eq!(
            card.property("EMAIL").unwrap().group.as_deref(),
            Some("ITEM1")
        );
    }

    #[test]
    fn parses_multiple_cards() {
        let input = format!("{VCARD3}{VCARD4}");
        let cards = parse_vcards(&input, VcardLimits::default()).unwrap();
        assert_eq!(cards.len(), 2);
    }

    #[test]
    fn filters() {
        let card = parse_vcard(VCARD3, VcardLimits::default()).unwrap();
        let filter = VcardFilter {
            test: MatchTest::AllOf,
            prop_filters: vec![
                PropFilter {
                    name: "EMAIL".into(),
                    is_not_defined: false,
                    text_matches: vec![TextMatch {
                        value: "example.com".into(),
                        kind: TextMatchKind::Contains,
                        negate: false,
                        case_sensitive: false,
                    }],
                    param_filters: Vec::new(),
                },
                PropFilter {
                    name: "TEL".into(),
                    is_not_defined: true,
                    text_matches: Vec::new(),
                    param_filters: Vec::new(),
                },
            ],
        };
        assert!(filter.matches(&card));
        let no_match = VcardFilter {
            test: MatchTest::AllOf,
            prop_filters: vec![PropFilter {
                name: "EMAIL".into(),
                is_not_defined: false,
                text_matches: vec![TextMatch {
                    value: "nope".into(),
                    kind: TextMatchKind::Contains,
                    negate: false,
                    case_sensitive: false,
                }],
                param_filters: Vec::new(),
            }],
        };
        assert!(!no_match.matches(&card));
    }
    #[test]
    fn param_filter_matches_property_parameters() {
        let card = parse_vcard(VCARD3, VcardLimits::default()).unwrap();
        let filter = VcardFilter {
            test: MatchTest::AllOf,
            prop_filters: vec![PropFilter {
                name: "EMAIL".into(),
                is_not_defined: false,
                text_matches: Vec::new(),
                param_filters: vec![ParamFilter {
                    name: "TYPE".into(),
                    is_not_defined: true,
                    text_matches: Vec::new(),
                }],
            }],
        };
        assert!(filter.matches(&card));
    }
}
