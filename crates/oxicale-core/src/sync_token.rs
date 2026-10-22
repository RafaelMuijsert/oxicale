use std::fmt;
use std::str::FromStr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Opaque sync token. The wire form encodes a collection UUID, an index
/// generation and the collection revision. Clients must treat it as opaque.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SyncToken(String);

impl SyncToken {
    pub fn new(collection_uid: &str, generation: &str, seq: u64) -> Self {
        Self(URL_SAFE_NO_PAD.encode(format!("{collection_uid}:{generation}:{seq}")))
    }

    pub fn parse(collection_uid: Option<&str>, value: &str) -> Result<(String, u64), CoreError> {
        if value.is_empty() {
            return Err(CoreError::InvalidSyncToken);
        }
        let raw = URL_SAFE_NO_PAD
            .decode(value.as_bytes())
            .map_err(|_| CoreError::InvalidSyncToken)?;
        let raw = String::from_utf8(raw).map_err(|_| CoreError::InvalidSyncToken)?;
        let mut parts = raw.split(':');
        let uid = parts.next().ok_or(CoreError::InvalidSyncToken)?;
        let generation = parts.next().ok_or(CoreError::InvalidSyncToken)?;
        let seq = parts
            .next()
            .ok_or(CoreError::InvalidSyncToken)?
            .parse::<u64>()
            .map_err(|_| CoreError::InvalidSyncToken)?;
        if parts.next().is_some() || generation.is_empty() {
            return Err(CoreError::InvalidSyncToken);
        }
        if let Some(expected) = collection_uid {
            if uid != expected {
                return Err(CoreError::InvalidSyncToken);
            }
        }
        Ok((generation.to_string(), seq))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SyncToken {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(None, &value)?;
        Ok(Self(value))
    }
}

impl From<SyncToken> for String {
    fn from(value: SyncToken) -> Self {
        value.0
    }
}

impl FromStr for SyncToken {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_string())
    }
}

impl fmt::Display for SyncToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let token = SyncToken::new("uuid-a", "gen-1", 42);
        let (generation, seq) = SyncToken::parse(Some("uuid-a"), token.as_str()).unwrap();
        assert_eq!(generation, "gen-1");
        assert_eq!(seq, 42);
    }

    #[test]
    fn wrong_collection_rejected() {
        let token = SyncToken::new("uuid-a", "gen-1", 42);
        assert!(SyncToken::parse(Some("uuid-b"), token.as_str()).is_err());
    }

    #[test]
    fn malformed_rejected() {
        for value in ["", "!!!", "dXVpZC1h", "dXVpZC1hOmdlbi0xOm5vdGE"] {
            assert!(SyncToken::parse(None, value).is_err(), "accepted {value}");
        }
    }
}
