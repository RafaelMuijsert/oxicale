use std::fmt;

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

fn has_invalid_percent(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

fn decode_segment(raw: &str) -> CoreResult<String> {
    if has_invalid_percent(raw) {
        return Err(CoreError::InvalidPath(
            "invalid percent-encoding".to_string(),
        ));
    }
    let decoded = percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| CoreError::InvalidPath("path is not valid UTF-8".to_string()))?;
    Ok(decoded.into_owned())
}

fn basic_segment_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && !value.starts_with('.')
        && !value
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\' || c == '\0' || c.is_whitespace())
}

/// A principal username. It is always a single URL path segment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UserName(String);

impl UserName {
    pub fn parse(value: impl Into<String>) -> CoreResult<Self> {
        let value = value.into();
        if value.len() > 255 {
            return Err(CoreError::InvalidUsername(
                "username is longer than 255 bytes".to_string(),
            ));
        }
        if !basic_segment_ok(&value) {
            return Err(CoreError::InvalidUsername(value));
        }
        if value.contains(':') {
            return Err(CoreError::InvalidUsername(
                "username must not contain ':'".to_string(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl TryFrom<String> for UserName {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<UserName> for String {
    fn from(value: UserName) -> Self {
        value.0
    }
}

impl fmt::Display for UserName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for UserName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A collection URL slug. It is always a single path segment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Slug(String);

impl Slug {
    pub fn parse(value: impl Into<String>) -> CoreResult<Self> {
        let value = value.into();
        if value.len() > 255 {
            return Err(CoreError::InvalidSlug(
                "slug is longer than 255 bytes".to_string(),
            ));
        }
        if !basic_segment_ok(&value) {
            return Err(CoreError::InvalidSlug(value));
        }
        for bad in ['?', '#', '%', ':', '&'] {
            if value.contains(bad) {
                return Err(CoreError::InvalidSlug(format!(
                    "slug contains reserved character {bad:?}"
                )));
            }
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl TryFrom<String> for Slug {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<Slug> for String {
    fn from(value: Slug) -> Self {
        value.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Slug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// An item href. It is always one path segment and never starts with a dot.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Href(String);

impl Href {
    pub fn parse(value: impl Into<String>) -> CoreResult<Self> {
        let value = value.into();
        if value.len() > 512 {
            return Err(CoreError::InvalidHref(
                "href is longer than 512 bytes".to_string(),
            ));
        }
        if !basic_segment_ok(&value) || value.contains(['?', '#']) {
            return Err(CoreError::InvalidHref(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl TryFrom<String> for Href {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<Href> for String {
    fn from(value: Href) -> Self {
        value.0
    }
}

impl fmt::Display for Href {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Href {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A decoded collection path: `/{user}/{slug}` without surrounding slashes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CollectionPath {
    pub user: UserName,
    pub slug: Slug,
}

impl CollectionPath {
    pub fn new(user: UserName, slug: Slug) -> Self {
        Self { user, slug }
    }

    pub fn parse(user: &str, slug: &str) -> CoreResult<Self> {
        Ok(Self {
            user: UserName::parse(user)?,
            slug: Slug::parse(slug)?,
        })
    }

    pub fn key(&self) -> String {
        format!("{}/{}", self.user.as_str(), self.slug.as_str())
    }

    pub fn to_path(&self) -> String {
        format!("/{}/{}/", self.user.as_str(), self.slug.as_str())
    }
}

impl fmt::Display for CollectionPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}/{}/", self.user, self.slug)
    }
}

/// Decode a request path once, reject unsafe segments and return its segments.
///
/// Leading and trailing slashes are ignored and duplicate slashes are collapsed.
/// Invalid percent escapes, NUL/control characters, backslashes and `.`/`..`
/// segments are rejected.
pub fn normalize_path(raw: &str) -> CoreResult<Vec<String>> {
    if raw.is_empty() {
        return Err(CoreError::InvalidPath("empty path".to_string()));
    }
    let raw = raw.split(['?', '#']).next().unwrap_or(raw);
    let mut out = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() {
            continue;
        }
        let decoded = decode_segment(segment)?;
        if decoded.is_empty() {
            continue;
        }
        if decoded == "." || decoded == ".." {
            return Err(CoreError::InvalidPath(
                "dot segments are not allowed".to_string(),
            ));
        }
        if decoded
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == '\0' || c == '/')
        {
            return Err(CoreError::InvalidPath(
                "path contains an unsafe character".to_string(),
            ));
        }
        out.push(decoded);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_rules() {
        assert!(UserName::parse("alice").is_ok());
        assert!(UserName::parse("alice@example.com").is_ok());
        assert!(UserName::parse("").is_err());
        assert!(UserName::parse(".").is_err());
        assert!(UserName::parse("..").is_err());
        assert!(UserName::parse("a/b").is_err());
        assert!(UserName::parse("a\\b").is_err());
        assert!(UserName::parse("a:b").is_err());
        assert!(UserName::parse("a b").is_err());
        assert!(UserName::parse("a\0b").is_err());
        assert!(UserName::parse("é").is_ok());
        assert!(UserName::parse("a".repeat(256)).is_err());
    }

    #[test]
    fn slug_rules() {
        assert!(Slug::parse("calendar").is_ok());
        assert!(Slug::parse("my-calendar_2").is_ok());
        assert!(Slug::parse(".hidden").is_err());
        assert!(Slug::parse("a?b").is_err());
        assert!(Slug::parse("a#b").is_err());
        assert!(Slug::parse("a%b").is_err());
        assert!(Slug::parse("a:b").is_err());
    }

    #[test]
    fn href_rules() {
        assert!(Href::parse("event.ics").is_ok());
        assert!(Href::parse(".secret").is_err());
        assert!(Href::parse("a/b.ics").is_err());
        assert!(Href::parse("a?x.ics").is_err());
    }

    #[test]
    fn normalize_valid_paths() {
        assert_eq!(normalize_path("/").unwrap(), Vec::<String>::new());
        assert_eq!(normalize_path("/alice/").unwrap(), vec!["alice"]);
        assert_eq!(
            normalize_path("/alice/calendar/event.ics").unwrap(),
            vec!["alice", "calendar", "event.ics"]
        );
        assert_eq!(
            normalize_path("//alice///calendar//").unwrap(),
            vec!["alice", "calendar"]
        );
        assert_eq!(
            normalize_path("/alice/my%20calendar/a%2Bb.ics").unwrap(),
            vec!["alice", "my calendar", "a+b.ics"]
        );
    }

    #[test]
    fn normalize_rejects_traversal() {
        for path in [
            "/alice/../secret",
            "/alice/%2e%2e/secret",
            "/alice/%2E%2E/secret",
            "/alice/%2fetc/passwd",
            "/alice/%5cetc",
            "/alice/%00x",
            "/alice/%zz",
            "/alice/%2",
        ] {
            assert!(normalize_path(path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn collection_path_roundtrip() {
        let p = CollectionPath::parse("alice", "calendar").unwrap();
        assert_eq!(p.key(), "alice/calendar");
        assert_eq!(p.to_path(), "/alice/calendar/");
    }

    #[test]
    fn serde_validates_newtypes() {
        assert!(serde_json::from_str::<UserName>("\"alice\"").is_ok());
        assert!(serde_json::from_str::<UserName>("\"a/b\"").is_err());
        assert_eq!(
            serde_json::to_string(&UserName::parse("alice").unwrap()).unwrap(),
            "\"alice\""
        );
    }
}
