use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use oxicale_core::{CollectionKind, ComponentSet, Href};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn now() -> DateTime<Utc> {
    Utc::now()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrincipalMeta {
    pub schema: u32,
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl PrincipalMeta {
    pub fn new(username: &str) -> Self {
        let now = now();
        Self {
            schema: 1,
            id: uuid::Uuid::new_v4().to_string(),
            username: username.to_string(),
            display_name: None,
            created_at: now,
            updated_at: now,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CollectionMeta {
    pub schema: u32,
    pub id: String,
    pub collection_uid: String,
    pub kind: CollectionKind,
    pub owner: String,
    pub slug: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub components: ComponentSet,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: u64,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub subscription: Option<SubscriptionSource>,
    #[serde(default)]
    pub feeds: Vec<FeedMeta>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CollectionMeta {
    pub fn new(owner: &str, slug: &str, kind: CollectionKind, display_name: &str) -> Self {
        let now = now();
        Self {
            schema: 1,
            id: uuid::Uuid::new_v4().to_string(),
            collection_uid: uuid::Uuid::new_v4().to_string(),
            kind,
            owner: owner.to_string(),
            slug: slug.to_string(),
            display_name: display_name.to_string(),
            description: None,
            color: default_color(kind),
            components: match kind {
                CollectionKind::Calendar => ComponentSet::all(),
                CollectionKind::AddressBook => ComponentSet::empty(),
            },
            created_at: now,
            updated_at: now,
            revision: 0,
            read_only: false,
            subscription: None,
            feeds: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only || self.subscription.is_some()
    }

    pub fn touch(&mut self) {
        self.updated_at = now();
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.touch();
    }
}

fn default_color(kind: CollectionKind) -> Option<String> {
    Some(
        match kind {
            CollectionKind::Calendar => "#3b82f6",
            CollectionKind::AddressBook => "#10b981",
        }
        .to_string(),
    )
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SubscriptionSource {
    pub url: String,
    pub original_url: String,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_seconds: u64,
    #[serde(default)]
    pub auth: Option<RemoteAuth>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_success_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub next_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub remote_display_name: Option<String>,
    #[serde(default)]
    pub remote_description: Option<String>,
    #[serde(default)]
    pub remote_color: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub remote_keys: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

pub fn default_refresh_interval() -> u64 {
    3600
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteAuth {
    pub kind: String,
    #[serde(default)]
    pub username: Option<String>,
    pub secret_ciphertext: String,
    pub key_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeedMeta {
    pub id: String,
    pub name: String,
    pub components: ComponentSet,
    pub token_sha256: String,
    pub token_hint: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
}

impl FeedMeta {
    pub fn is_active(&self, at: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|expiry| at < expiry)
    }

    pub fn file_name(&self) -> String {
        format!("{}.ics", self.id)
    }
}

#[derive(Clone, Debug)]
pub struct StoredItem {
    pub href: Href,
    pub uid: String,
    pub component: oxicale_core::ItemComponent,
    pub etag: String,
    pub size: usize,
    pub updated_at: DateTime<Utc>,
    pub payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemRow {
    pub href: String,
    pub uid: String,
    pub component: String,
    pub etag: String,
    pub size: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeOperation {
    Put,
    Delete,
}

impl ChangeOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub seq: u64,
    pub href: String,
    pub operation: ChangeOperation,
    pub etag: Option<String>,
    pub changed_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ChangeSet {
    pub token: String,
    pub changes: Vec<Change>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Preconditions<'a> {
    pub if_match: Option<&'a str>,
    pub if_none_match: bool,
}

#[derive(Clone, Debug)]
pub struct PutOutcome {
    pub created: bool,
    pub etag: String,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct DeleteOutcome {
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub principals: usize,
    pub collections: usize,
    pub items: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DefaultCollection {
    pub slug: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct CollectionUpdate {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub clear_description: bool,
    pub color: Option<String>,
    pub clear_color: bool,
}

#[derive(Clone, Debug)]
pub struct FeedCreation {
    pub token_sha256: String,
    pub token_hint: String,
    pub name: String,
    pub components: ComponentSet,
    pub expires_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip_and_unknown_fields() {
        let mut meta =
            CollectionMeta::new("alice", "calendar", CollectionKind::Calendar, "Calendar");
        meta.extra
            .insert("future".into(), Value::String("value".into()));
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let parsed: CollectionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.owner, "alice");
        assert_eq!(parsed.components, ComponentSet::all());
        assert_eq!(parsed.extra.get("future").unwrap(), "value");
    }

    #[test]
    fn addressbook_components_are_not_calendar_components() {
        let meta =
            CollectionMeta::new("alice", "contacts", CollectionKind::AddressBook, "Contacts");
        assert!(meta.components.is_empty());
    }

    #[test]
    fn feed_expiry_and_revocation() {
        let now = Utc::now();
        let mut feed = FeedMeta {
            id: "1".into(),
            name: "Tasks".into(),
            components: ComponentSet::all(),
            token_sha256: "hash".into(),
            token_hint: "abcd".into(),
            created_at: now,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        };
        assert!(feed.is_active(now));
        feed.revoked_at = Some(now);
        assert!(!feed.is_active(now));
        feed.revoked_at = None;
        feed.expires_at = Some(now - chrono::Duration::seconds(1));
        assert!(!feed.is_active(now));
    }
}
