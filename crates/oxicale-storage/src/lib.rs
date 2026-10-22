//! Filesystem-first storage with a rebuildable SQLite index.

mod metadata;

pub use metadata::*;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use fs2::FileExt;
use oxicale_core::{
    content_etag, CollectionKind, CollectionPath, Href, ItemComponent, SyncToken, UserName,
};
use oxicale_ical::{FetchedItem, Limits};
use oxicale_vcard::VcardLimits;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("precondition failed")]
    PreconditionFailed,
    #[error("collection is read-only")]
    ReadOnly,
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("data directory is locked by another process")]
    Locked,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("index error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("domain error: {0}")]
    Domain(#[from] oxicale_core::CoreError),
    #[error("iCalendar error: {0}")]
    Ical(String),
    #[error("unsupported component: {0}")]
    Unsupported(String),
    #[error("vCard error: {0}")]
    Vcard(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub fsync: bool,
    pub max_calendar_item_size: usize,
    pub max_contact_item_size: usize,
    pub default_timezone: Tz,
    pub index_path: Option<PathBuf>,
    pub lock_timeout: std::time::Duration,
    pub max_collections_per_user: usize,
    pub max_items_per_collection: usize,
    pub max_principals: usize,
    pub sync_history_days: u64,
    pub sync_history_max_changes: u64,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            fsync: true,
            max_calendar_item_size: 2 * 1024 * 1024,
            max_contact_item_size: 10 * 1024 * 1024,
            default_timezone: Tz::UTC,
            index_path: None,
            lock_timeout: std::time::Duration::from_secs(10),
            max_collections_per_user: 100,
            max_items_per_collection: 100_000,
            max_principals: 10_000,
            sync_history_days: 90,
            sync_history_max_changes: 100_000,
        }
    }
}

pub struct Store {
    root: PathBuf,
    options: StoreOptions,
    index: Mutex<Connection>,
    locks: Mutex<HashMap<String, Arc<RwLock<()>>>>,
    _process_lock: File,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("root", &self.root).finish()
    }
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(root, StoreOptions::default())
    }

    pub fn open_with_options(root: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        set_dir_mode(&root)?;
        fs::create_dir_all(root.join("users"))?;
        fs::create_dir_all(root.join("tmp"))?;
        set_dir_mode(&root.join("users"))?;
        set_dir_mode(&root.join("tmp"))?;

        let lock_path = root.join(".oxicale.lock");
        let process_lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        set_file_mode(&lock_path)?;
        acquire_process_lock(&process_lock, options.lock_timeout)?;

        let index_path = options
            .index_path
            .clone()
            .unwrap_or_else(|| root.join("index.sqlite3"));
        let connection = Connection::open(&index_path)?;
        let _ = set_file_mode(&index_path);
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;
        init_schema(&connection)?;

        let store = Self {
            root,
            options,
            index: Mutex::new(connection),
            locks: Mutex::new(HashMap::new()),
            _process_lock: process_lock,
        };
        store.cleanup_temp_files();
        Ok(store)
    }

    pub fn data_dir(&self) -> &Path {
        &self.root
    }

    pub fn options(&self) -> &StoreOptions {
        &self.options
    }

    fn cleanup_temp_files(&self) {
        let tmp = self.root.join("tmp");
        if let Ok(entries) = fs::read_dir(&tmp) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let _ = fs::remove_file(path);
                }
            }
        }
        let users = self.root.join("users");
        remove_temp_files_recursive(&users);
    }

    fn user_dir(&self, user: &UserName) -> PathBuf {
        self.root.join("users").join(user.as_str())
    }

    fn collection_dir(&self, path: &CollectionPath) -> PathBuf {
        self.user_dir(&path.user).join(path.slug.as_str())
    }

    fn collection_meta_path(&self, path: &CollectionPath) -> PathBuf {
        self.collection_dir(path).join(".collection.json")
    }

    fn items_dir(&self, path: &CollectionPath) -> PathBuf {
        self.collection_dir(path).join("items")
    }

    fn item_path(&self, path: &CollectionPath, href: &Href) -> PathBuf {
        self.items_dir(path).join(href.as_str())
    }

    fn lock_for(&self, key: &str) -> Arc<RwLock<()>> {
        let mut locks = self.locks.lock().expect("lock map poisoned");
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    fn with_read<T>(&self, key: &str, operation: impl FnOnce() -> T) -> T {
        let lock = self.lock_for(key);
        let _guard = lock.read().expect("collection lock poisoned");
        operation()
    }

    fn index(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.index.lock().expect("index mutex poisoned")
    }

    fn read_meta(&self, path: &CollectionPath) -> Result<CollectionMeta> {
        let meta_path = self.collection_meta_path(path);
        let bytes = fs::read(&meta_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("collection {path}"))
            } else {
                StorageError::Io(error)
            }
        })?;
        let meta: CollectionMeta = serde_json::from_slice(&bytes)
            .map_err(|error| StorageError::Invalid(format!("collection metadata: {error}")))?;
        if meta.owner != path.user.as_str() || meta.slug != path.slug.as_str() {
            return Err(StorageError::Invalid(
                "collection metadata does not match its path".to_string(),
            ));
        }
        Ok(meta)
    }

    fn write_meta(&self, path: &CollectionPath, meta: &CollectionMeta) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(meta)
            .map_err(|error| StorageError::Invalid(format!("metadata serialisation: {error}")))?;
        atomic_write(&self.collection_meta_path(path), &bytes, self.options.fsync)
    }

    fn read_principal(&self, user: &UserName) -> Result<PrincipalMeta> {
        let path = self.user_dir(user).join(".principal.json");
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound(format!("principal {user}"))
            } else {
                StorageError::Io(error)
            }
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|error| StorageError::Invalid(format!("principal metadata: {error}")))
    }

    fn write_principal(&self, principal: &PrincipalMeta) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(principal)
            .map_err(|error| StorageError::Invalid(format!("principal serialisation: {error}")))?;
        atomic_write(
            &self
                .user_dir(&UserName::parse(principal.username.clone())?)
                .join(".principal.json"),
            &bytes,
            self.options.fsync,
        )
    }

    pub async fn provision_principal(&self, username: &UserName) -> Result<PrincipalMeta> {
        let key = username.as_str().to_string();
        let lock_0 = self.lock_for(&key);
        let _guard = lock_0.write().expect("collection lock poisoned");
        if let Ok(principal) = self.read_principal(username) {
            return Ok(principal);
        }
        {
            let index = self.index();
            let count: i64 =
                index.query_row("SELECT COUNT(*) FROM principals", [], |row| row.get(0))?;
            if count as usize >= self.options.max_principals {
                return Err(StorageError::Limit("principals".into()));
            }
        }
        let dir = self.user_dir(username);
        fs::create_dir_all(&dir)?;
        set_dir_mode(&dir)?;
        let principal = PrincipalMeta::new(username.as_str());
        self.write_principal(&principal)?;
        let index = self.index();
        index.execute(
            "INSERT OR REPLACE INTO principals (id, username, display_name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                principal.id,
                principal.username,
                principal.display_name,
                principal.created_at.to_rfc3339(),
                principal.updated_at.to_rfc3339()
            ],
        )?;
        Ok(principal)
    }

    pub async fn provision_defaults(
        &self,
        username: &UserName,
        calendar: Option<&DefaultCollection>,
        addressbook: Option<&DefaultCollection>,
    ) -> Result<Vec<CollectionMeta>> {
        self.provision_principal(username).await?;
        let mut created = Vec::new();
        if let Some(default) = calendar {
            if let Ok(slug) = oxicale_core::Slug::parse(default.slug.clone()) {
                let path = CollectionPath::new(username.clone(), slug);
                if self.read_meta(&path).is_err() {
                    created.push(
                        self.create_collection(
                            username,
                            CollectionKind::Calendar,
                            &default.slug,
                            &default.name,
                        )
                        .await?,
                    );
                }
            }
        }
        if let Some(default) = addressbook {
            if let Ok(slug) = oxicale_core::Slug::parse(default.slug.clone()) {
                let path = CollectionPath::new(username.clone(), slug);
                if self.read_meta(&path).is_err() {
                    created.push(
                        self.create_collection(
                            username,
                            CollectionKind::AddressBook,
                            &default.slug,
                            &default.name,
                        )
                        .await?,
                    );
                }
            }
        }
        Ok(created)
    }

    pub async fn repair_defaults(
        &self,
        username: &UserName,
        calendar: Option<&DefaultCollection>,
        addressbook: Option<&DefaultCollection>,
    ) -> Result<()> {
        self.provision_defaults(username, calendar, addressbook)
            .await
            .map(|_| ())
    }

    pub fn principal(&self, username: &UserName) -> Result<PrincipalMeta> {
        self.read_principal(username)
    }

    pub async fn create_collection(
        &self,
        username: &UserName,
        kind: CollectionKind,
        slug: &str,
        display_name: &str,
    ) -> Result<CollectionMeta> {
        let slug = oxicale_core::Slug::parse(slug)?;
        let path = CollectionPath::new(username.clone(), slug);
        let key = path.key();
        let lock_1 = self.lock_for(&key);
        let _guard = lock_1.write().expect("collection lock poisoned");
        if self.collection_dir(&path).exists() {
            return Err(StorageError::Conflict(format!(
                "collection {path} already exists"
            )));
        }
        self.read_principal(username)?;
        if self.list_collections(username)?.len() >= self.options.max_collections_per_user {
            return Err(StorageError::Limit("collections per user".into()));
        }
        fs::create_dir_all(self.items_dir(&path))?;
        set_dir_mode(&self.collection_dir(&path))?;
        set_dir_mode(&self.items_dir(&path))?;
        let meta = CollectionMeta::new(username.as_str(), path.slug.as_str(), kind, display_name);
        self.write_meta(&path, &meta)?;
        let principal = self.read_principal(username)?;
        let index = self.index();
        index.execute(
            "INSERT OR REPLACE INTO collections (id, owner, slug, kind, display_name, color, collection_uid, revision, read_only, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                meta.id,
                principal.id,
                meta.slug,
                meta.kind.as_str(),
                meta.display_name,
                meta.color,
                meta.collection_uid,
                meta.revision as i64,
                0,
                meta.updated_at.to_rfc3339()
            ],
        )?;
        Ok(meta)
    }

    pub fn list_collections(&self, username: &UserName) -> Result<Vec<CollectionMeta>> {
        self.read_principal(username)?;
        let mut collections = Vec::new();
        let dir = self.user_dir(username);
        let entries = fs::read_dir(&dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join(".collection.json");
            if !meta_path.is_file() {
                continue;
            }
            let bytes = fs::read(&meta_path)?;
            if let Ok(meta) = serde_json::from_slice::<CollectionMeta>(&bytes) {
                if meta.owner == username.as_str() {
                    collections.push(meta);
                }
            }
        }
        collections.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(collections)
    }

    pub fn collection(&self, path: &CollectionPath) -> Result<CollectionMeta> {
        self.read_meta(path)
    }

    pub async fn update_collection(
        &self,
        path: &CollectionPath,
        update: CollectionUpdate,
    ) -> Result<CollectionMeta> {
        let lock_2 = self.lock_for(&path.key());
        let _guard = lock_2.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        if let Some(name) = update.display_name {
            if name.trim().is_empty() || name.len() > 512 {
                return Err(StorageError::Invalid("invalid display name".into()));
            }
            meta.display_name = name;
        }
        if update.clear_description {
            meta.description = None;
        } else if let Some(description) = update.description {
            meta.description = Some(description);
        }
        if update.clear_color {
            meta.color = None;
        } else if let Some(color) = update.color {
            validate_color(&color)?;
            meta.color = Some(color);
        }
        meta.bump();
        self.write_meta(path, &meta)?;
        self.sync_collection_row(&meta, path)?;
        Ok(meta)
    }

    fn sync_collection_row(&self, meta: &CollectionMeta, path: &CollectionPath) -> Result<()> {
        let principal = self.read_principal(&path.user)?;
        let index = self.index();
        index.execute(
            "INSERT OR REPLACE INTO collections (id, owner, slug, kind, display_name, color, collection_uid, revision, read_only, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                meta.id,
                principal.id,
                meta.slug,
                meta.kind.as_str(),
                meta.display_name,
                meta.color,
                meta.collection_uid,
                meta.revision as i64,
                if meta.is_read_only() { 1 } else { 0 },
                meta.updated_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub async fn delete_collection(
        &self,
        path: &CollectionPath,
        if_match: Option<&str>,
    ) -> Result<()> {
        let lock_3 = self.lock_for(&path.key());
        let _guard = lock_3.write().expect("collection lock poisoned");
        let meta = self.read_meta(path)?;
        if let Some(expected) = if_match {
            let current = self.ctag(&meta);
            if !etag_header_matches(expected, &current) {
                return Err(StorageError::PreconditionFailed);
            }
        }
        fs::remove_dir_all(self.collection_dir(path))?;
        let index = self.index();
        index.execute("DELETE FROM collections WHERE id = ?1", params![meta.id])?;
        index.execute(
            "DELETE FROM changes WHERE collection_id = ?1",
            params![meta.id],
        )?;
        index.execute(
            "DELETE FROM sync_tokens WHERE collection_id = ?1",
            params![meta.id],
        )?;
        Ok(())
    }

    pub async fn set_collection_read_only(
        &self,
        path: &CollectionPath,
        read_only: bool,
    ) -> Result<CollectionMeta> {
        let lock = self.lock_for(&path.key());
        let _guard = lock.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        meta.read_only = read_only;
        meta.bump();
        self.write_meta(path, &meta)?;
        self.sync_collection_row(&meta, path)?;
        Ok(meta)
    }

    pub async fn attach_imported_subscription(
        &self,
        path: &CollectionPath,
        url: String,
    ) -> Result<CollectionMeta> {
        let lock = self.lock_for(&path.key());
        let _guard = lock.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let now = Utc::now();
        meta.subscription = Some(SubscriptionSource {
            url: url.clone(),
            original_url: url,
            refresh_interval_seconds: 3600,
            next_attempt_at: Some(now),
            ..Default::default()
        });
        meta.bump();
        self.write_meta(path, &meta)?;
        self.sync_collection_row(&meta, path)?;
        Ok(meta)
    }

    pub fn ctag(&self, meta: &CollectionMeta) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:{}", meta.collection_uid, meta.revision));
        hex(&hasher.finalize())
    }

    pub fn index_generation(&self) -> Result<String> {
        let index = self.index();
        let existing: Option<String> = index
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'generation'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(value) = existing {
            return Ok(value);
        }
        let value = uuid::Uuid::new_v4().to_string();
        index.execute(
            "INSERT INTO index_meta (key, value) VALUES ('generation', ?1)",
            params![value],
        )?;
        Ok(value)
    }

    pub fn sync_token(&self, path: &CollectionPath) -> Result<String> {
        let meta = self.read_meta(path)?;
        let generation = self.index_generation()?;
        Ok(SyncToken::new(&meta.collection_uid, &generation, meta.revision).to_string())
    }

    pub fn changes_since(&self, path: &CollectionPath, token: &str) -> Result<ChangeSet> {
        let meta = self.read_meta(path)?;
        let generation = self.index_generation()?;
        let (token_generation, seq) = SyncToken::parse(Some(&meta.collection_uid), token)?;
        if token_generation != generation || seq > meta.revision {
            return Err(StorageError::Invalid("invalid sync token".into()));
        }
        let index = self.index();
        self.prune_changes(&index, &meta.id)?;
        let oldest: Option<i64> = index
            .query_row(
                "SELECT MIN(seq) FROM changes WHERE collection_id = ?1",
                params![meta.id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        match oldest {
            Some(oldest) if (seq as i64) < oldest - 1 => {
                return Err(StorageError::Invalid("invalid sync token".into()));
            }
            None if seq < meta.revision => {
                return Err(StorageError::Invalid("invalid sync token".into()));
            }
            _ => {}
        }
        let mut statement = index.prepare(
            "SELECT seq, href, operation, etag, changed_at FROM changes WHERE collection_id = ?1 AND seq > ?2 ORDER BY seq ASC",
        )?;
        let rows = statement.query_map(params![meta.id, seq as i64], |row| {
            let operation: String = row.get(2)?;
            let changed_at: String = row.get(4)?;
            Ok(Change {
                seq: row.get::<_, i64>(0)? as u64,
                href: row.get(1)?,
                operation: if operation == "delete" {
                    ChangeOperation::Delete
                } else {
                    ChangeOperation::Put
                },
                etag: row.get(3)?,
                changed_at: changed_at.parse().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        let mut changes = Vec::new();
        for row in rows {
            changes.push(row?);
        }
        let new_token =
            SyncToken::new(&meta.collection_uid, &generation, meta.revision).to_string();
        Ok(ChangeSet {
            token: new_token,
            changes,
        })
    }

    fn prune_changes(&self, index: &Connection, collection_id: &str) -> Result<()> {
        if self.options.sync_history_days > 0 {
            let cutoff = Utc::now()
                - chrono::Duration::days(self.options.sync_history_days.min(i64::MAX as u64) as i64);
            index.execute(
                "DELETE FROM changes WHERE collection_id = ?1 AND changed_at < ?2",
                params![collection_id, cutoff.to_rfc3339()],
            )?;
        }
        if self.options.sync_history_max_changes > 0 {
            let count: i64 = index.query_row(
                "SELECT COUNT(*) FROM changes WHERE collection_id = ?1",
                params![collection_id],
                |row| row.get(0),
            )?;
            let overflow = count.saturating_sub(self.options.sync_history_max_changes as i64);
            if overflow > 0 {
                index.execute(
                    "DELETE FROM changes WHERE collection_id = ?1 AND seq IN (SELECT seq FROM changes WHERE collection_id = ?1 ORDER BY seq ASC LIMIT ?2)",
                    params![collection_id, overflow],
                )?;
            }
        }
        Ok(())
    }

    fn ical_limits(&self) -> Limits {
        Limits {
            max_bytes: self.options.max_calendar_item_size,
            ..Limits::default()
        }
    }

    fn vcard_limits(&self) -> VcardLimits {
        VcardLimits {
            max_bytes: self.options.max_contact_item_size,
            ..VcardLimits::default()
        }
    }

    fn validate_item(
        &self,
        kind: CollectionKind,
        payload: &str,
        generate_uid: bool,
    ) -> Result<(String, ItemComponent, String)> {
        match kind {
            CollectionKind::Calendar => {
                if payload.len() > self.options.max_calendar_item_size {
                    return Err(StorageError::Limit("calendar item size".into()));
                }
                let validated = oxicale_ical::validate_calendar_object(
                    payload,
                    self.ical_limits(),
                    self.options.default_timezone,
                    generate_uid,
                )
                .map_err(|error| match error {
                    oxicale_ical::IcalError::Unsupported(_) => {
                        StorageError::Unsupported(error.to_string())
                    }
                    other => StorageError::Ical(other.to_string()),
                })?;
                Ok((validated.uid, validated.component, validated.canonical))
            }
            CollectionKind::AddressBook => {
                if payload.len() > self.options.max_contact_item_size {
                    return Err(StorageError::Limit("contact item size".into()));
                }
                let validated = oxicale_vcard::validate_vcard(payload, self.vcard_limits())
                    .map_err(|error| StorageError::Vcard(error.to_string()))?;
                Ok((validated.uid, ItemComponent::VCard, validated.canonical))
            }
        }
    }

    pub async fn put_item(
        &self,
        path: &CollectionPath,
        href: &Href,
        payload: &str,
        generate_uid: bool,
        preconditions: Preconditions<'_>,
    ) -> Result<PutOutcome> {
        let lock_4 = self.lock_for(&path.key());
        let _guard = lock_4.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        if meta.is_read_only() {
            return Err(StorageError::ReadOnly);
        }
        let (uid, component, canonical) = self.validate_item(meta.kind, payload, generate_uid)?;
        let etag = content_etag(canonical.as_bytes());
        let item_path = self.item_path(path, href);
        let existing = read_payload(&item_path).ok();
        let existing_etag = existing.as_ref().map(|bytes| content_etag(bytes));
        enforce_preconditions(existing_etag.as_deref(), preconditions)?;

        if let Some(bytes) = &existing {
            let existing_payload = std::str::from_utf8(bytes)
                .map_err(|_| StorageError::Invalid("existing item is not UTF-8".into()))?;
            let (existing_uid, _, _) = self.validate_item(meta.kind, existing_payload, false)?;
            if existing_uid != uid {
                return Err(StorageError::Conflict(
                    "the UID of an existing item cannot be changed".into(),
                ));
            }
        } else {
            let index = self.index();
            let count: i64 = index.query_row(
                "SELECT COUNT(*) FROM items WHERE collection_id = ?1",
                params![meta.id],
                |row| row.get(0),
            )?;
            if count as usize >= self.options.max_items_per_collection {
                return Err(StorageError::Limit("items per collection".into()));
            }
        }

        {
            let index = self.index();
            let conflict: Option<String> = index
                .query_row(
                    "SELECT href FROM items WHERE collection_id = ?1 AND uid = ?2 AND href <> ?3",
                    params![meta.id, uid, href.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(conflict_href) = conflict {
                return Err(StorageError::Conflict(format!(
                    "UID already exists at href {conflict_href}"
                )));
            }
        }

        let created = existing.is_none();
        atomic_write(&item_path, canonical.as_bytes(), self.options.fsync)?;
        let seq = meta.revision + 1;
        meta.revision = seq;
        meta.touch();
        self.write_meta(path, &meta)?;
        let updated = Utc::now();
        {
            let index = self.index();
            index.execute(
                "INSERT OR REPLACE INTO items (collection_id, href, uid, component, etag, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    meta.id,
                    href.as_str(),
                    uid,
                    component.as_str(),
                    etag,
                    canonical.len() as i64,
                    updated.to_rfc3339()
                ],
            )?;
            index.execute(
                "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'put', ?4, ?5)",
                params![
                    meta.id,
                    seq as i64,
                    href.as_str(),
                    etag,
                    updated.to_rfc3339()
                ],
            )?;
            index.execute(
                "UPDATE collections SET revision = ?1, updated_at = ?2 WHERE id = ?3",
                params![seq as i64, updated.to_rfc3339(), meta.id],
            )?;
        }
        Ok(PutOutcome {
            created,
            etag,
            revision: seq,
        })
    }

    pub async fn delete_item(
        &self,
        path: &CollectionPath,
        href: &Href,
        if_match: Option<&str>,
    ) -> Result<DeleteOutcome> {
        let lock_5 = self.lock_for(&path.key());
        let _guard = lock_5.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        if meta.is_read_only() {
            return Err(StorageError::ReadOnly);
        }
        let item_path = self.item_path(path, href);
        let bytes = read_payload(&item_path)?;
        let etag = content_etag(&bytes);
        enforce_preconditions(
            Some(&etag),
            Preconditions {
                if_match,
                if_none_match: false,
            },
        )?;
        let seq = meta.revision + 1;
        fs::remove_file(&item_path)?;
        meta.revision = seq;
        meta.touch();
        self.write_meta(path, &meta)?;
        let now = Utc::now();
        {
            let index = self.index();
            index.execute(
                "DELETE FROM items WHERE collection_id = ?1 AND href = ?2",
                params![meta.id, href.as_str()],
            )?;
            index.execute(
                "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'delete', NULL, ?4)",
                params![meta.id, seq as i64, href.as_str(), now.to_rfc3339()],
            )?;
            index.execute(
                "UPDATE collections SET revision = ?1, updated_at = ?2 WHERE id = ?3",
                params![seq as i64, now.to_rfc3339(), meta.id],
            )?;
        }
        Ok(DeleteOutcome { revision: seq })
    }

    pub fn get_item(&self, path: &CollectionPath, href: &Href) -> Result<StoredItem> {
        self.with_read(&path.key(), || self.get_item_unlocked(path, href))
    }

    fn get_item_unlocked(&self, path: &CollectionPath, href: &Href) -> Result<StoredItem> {
        let meta = self.read_meta(path)?;
        let item_path = self.item_path(path, href);
        let bytes = read_payload(&item_path)?;
        let payload = String::from_utf8(bytes.clone())
            .map_err(|_| StorageError::Invalid("item is not UTF-8".into()))?;
        let (uid, component) = match meta.kind {
            CollectionKind::Calendar => {
                let validated = oxicale_ical::validate_calendar_object(
                    &payload,
                    self.ical_limits(),
                    self.options.default_timezone,
                    false,
                )
                .map_err(|error| StorageError::Ical(error.to_string()))?;
                (validated.uid, validated.component)
            }
            CollectionKind::AddressBook => {
                let validated = oxicale_vcard::validate_vcard(&payload, self.vcard_limits())
                    .map_err(|error| StorageError::Vcard(error.to_string()))?;
                (validated.uid, ItemComponent::VCard)
            }
        };
        let updated_at = fs::metadata(&item_path)
            .and_then(|m| m.modified())
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        Ok(StoredItem {
            href: href.clone(),
            uid,
            component,
            etag: content_etag(&bytes),
            size: bytes.len(),
            updated_at,
            payload,
        })
    }

    pub fn list_items(&self, path: &CollectionPath) -> Result<Vec<StoredItem>> {
        self.with_read(&path.key(), || self.list_items_unlocked(path))
    }

    fn list_items_unlocked(&self, path: &CollectionPath) -> Result<Vec<StoredItem>> {
        let meta = self.read_meta(path)?;
        let dir = self.items_dir(path);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_path = entry.path();
            if !file_path.is_file() {
                continue;
            }
            let Some(name) = file_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let href = Href::parse(name)?;
            let bytes = read_payload(&file_path)?;
            let payload = String::from_utf8(bytes.clone())
                .map_err(|_| StorageError::Invalid(format!("item {name} is not UTF-8")))?;
            let (uid, component) = match meta.kind {
                CollectionKind::Calendar => {
                    let validated = oxicale_ical::validate_calendar_object(
                        &payload,
                        self.ical_limits(),
                        self.options.default_timezone,
                        false,
                    )
                    .map_err(|error| StorageError::Invalid(format!("item {name}: {error}")))?;
                    (validated.uid, validated.component)
                }
                CollectionKind::AddressBook => {
                    let validated = oxicale_vcard::validate_vcard(&payload, self.vcard_limits())
                        .map_err(|error| StorageError::Invalid(format!("item {name}: {error}")))?;
                    (validated.uid, ItemComponent::VCard)
                }
            };
            let updated_at = fs::metadata(&file_path)
                .and_then(|m| m.modified())
                .map(DateTime::<Utc>::from)
                .unwrap_or_else(|_| Utc::now());
            items.push(StoredItem {
                href,
                uid,
                component,
                etag: content_etag(&bytes),
                size: bytes.len(),
                updated_at,
                payload,
            });
        }
        items.sort_by(|a, b| a.href.as_str().cmp(b.href.as_str()));
        Ok(items)
    }

    pub async fn move_item(
        &self,
        from: &CollectionPath,
        from_href: &Href,
        to: &CollectionPath,
        to_href: &Href,
        overwrite: bool,
    ) -> Result<()> {
        if from == to {
            let lock_6 = self.lock_for(&from.key());
            let _guard = lock_6.write().expect("collection lock poisoned");
            return self.move_item_locked(from, from_href, to_href, overwrite);
        }
        let first = self.lock_for(&from.key());
        let second = self.lock_for(&to.key());
        let (first_key, second_key) = if from.key() < to.key() {
            (from.key(), to.key())
        } else {
            (to.key(), from.key())
        };
        let _ = (first_key, second_key);
        let (_guard_a, _guard_b) = if from.key() < to.key() {
            (
                first.write().expect("collection lock poisoned"),
                second.write().expect("collection lock poisoned"),
            )
        } else {
            let a = second.write().expect("collection lock poisoned");
            let b = first.write().expect("collection lock poisoned");
            (b, a)
        };
        self.move_item_cross_locked(from, from_href, to, to_href, overwrite)
    }

    fn move_item_cross_locked(
        &self,
        from: &CollectionPath,
        from_href: &Href,
        to: &CollectionPath,
        to_href: &Href,
        overwrite: bool,
    ) -> Result<()> {
        let mut from_meta = self.read_meta(from)?;
        let mut to_meta = self.read_meta(to)?;
        if from_meta.is_read_only() || to_meta.is_read_only() {
            return Err(StorageError::ReadOnly);
        }
        if from_meta.kind != to_meta.kind {
            return Err(StorageError::Conflict(
                "cannot move between collection kinds".into(),
            ));
        }
        let source = self.item_path(from, from_href);
        let target = self.item_path(to, to_href);
        if target.exists() {
            if !overwrite {
                return Err(StorageError::PreconditionFailed);
            }
            fs::remove_file(&target)?;
            let index = self.index();
            index.execute(
                "DELETE FROM items WHERE collection_id = ?1 AND href = ?2",
                params![to_meta.id, to_href.as_str()],
            )?;
        }
        let bytes = read_payload(&source)?;
        let payload = String::from_utf8(bytes)
            .map_err(|_| StorageError::Invalid("item is not UTF-8".into()))?;
        let (uid, component, canonical) = self.validate_item(to_meta.kind, &payload, false)?;
        {
            let index = self.index();
            let conflict: Option<String> = index
                .query_row(
                    "SELECT href FROM items WHERE collection_id = ?1 AND uid = ?2",
                    params![to_meta.id, uid],
                    |row| row.get(0),
                )
                .optional()?;
            if conflict.is_some() {
                return Err(StorageError::Conflict(
                    "UID already exists in destination".into(),
                ));
            }
        }
        fs::rename(&source, &target)?;
        let etag = content_etag(canonical.as_bytes());
        let now = Utc::now();
        from_meta.revision += 1;
        from_meta.touch();
        to_meta.revision += 1;
        to_meta.touch();
        self.write_meta(from, &from_meta)?;
        self.write_meta(to, &to_meta)?;
        let index = self.index();
        index.execute(
            "DELETE FROM items WHERE collection_id = ?1 AND href = ?2",
            params![from_meta.id, from_href.as_str()],
        )?;
        index.execute(
            "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'delete', NULL, ?4)",
            params![from_meta.id, from_meta.revision as i64, from_href.as_str(), now.to_rfc3339()],
        )?;
        index.execute(
            "INSERT OR REPLACE INTO items (collection_id, href, uid, component, etag, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_meta.id,
                to_href.as_str(),
                uid,
                component.as_str(),
                etag,
                canonical.len() as i64,
                now.to_rfc3339()
            ],
        )?;
        index.execute(
            "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'put', ?4, ?5)",
            params![to_meta.id, to_meta.revision as i64, to_href.as_str(), etag, now.to_rfc3339()],
        )?;
        index.execute(
            "UPDATE collections SET revision = ?1, updated_at = ?2 WHERE id = ?3",
            params![from_meta.revision as i64, now.to_rfc3339(), from_meta.id],
        )?;
        index.execute(
            "UPDATE collections SET revision = ?1, updated_at = ?2 WHERE id = ?3",
            params![to_meta.revision as i64, now.to_rfc3339(), to_meta.id],
        )?;
        Ok(())
    }

    fn move_item_locked(
        &self,
        path: &CollectionPath,
        from_href: &Href,
        to_href: &Href,
        overwrite: bool,
    ) -> Result<()> {
        let mut meta = self.read_meta(path)?;
        if meta.is_read_only() {
            return Err(StorageError::ReadOnly);
        }
        let source = self.item_path(path, from_href);
        let target = self.item_path(path, to_href);
        if target.exists() {
            if !overwrite {
                return Err(StorageError::PreconditionFailed);
            }
            fs::remove_file(&target)?;
            let index = self.index();
            index.execute(
                "DELETE FROM items WHERE collection_id = ?1 AND href = ?2",
                params![meta.id, to_href.as_str()],
            )?;
        }
        let bytes = read_payload(&source)?;
        let payload = String::from_utf8(bytes.clone())
            .map_err(|_| StorageError::Invalid("item is not UTF-8".into()))?;
        let (uid, component, canonical) = self.validate_item(meta.kind, &payload, false)?;
        let etag = content_etag(canonical.as_bytes());
        fs::rename(&source, &target)?;
        let seq = meta.revision + 2;
        meta.revision = seq;
        meta.touch();
        self.write_meta(path, &meta)?;
        let now = Utc::now();
        let index = self.index();
        index.execute(
            "DELETE FROM items WHERE collection_id = ?1 AND href = ?2",
            params![meta.id, from_href.as_str()],
        )?;
        index.execute(
            "INSERT OR REPLACE INTO items (collection_id, href, uid, component, etag, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                meta.id,
                to_href.as_str(),
                uid,
                component.as_str(),
                etag,
                canonical.len() as i64,
                now.to_rfc3339()
            ],
        )?;
        index.execute(
            "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'delete', NULL, ?4)",
            params![meta.id, (seq - 1) as i64, from_href.as_str(), now.to_rfc3339()],
        )?;
        index.execute(
            "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'put', ?4, ?5)",
            params![meta.id, seq as i64, to_href.as_str(), etag, now.to_rfc3339()],
        )?;
        index.execute(
            "UPDATE collections SET revision = ?1, updated_at = ?2 WHERE id = ?3",
            params![seq as i64, now.to_rfc3339(), meta.id],
        )?;
        Ok(())
    }

    pub async fn move_collection(
        &self,
        from: &CollectionPath,
        to: &CollectionPath,
        overwrite: bool,
    ) -> Result<CollectionMeta> {
        if from.user != to.user {
            return Err(StorageError::Conflict(
                "cross-principal moves are not allowed".into(),
            ));
        }
        let lock_from = self.lock_for(&from.key());
        let _guard = lock_from.write().expect("collection lock poisoned");
        let lock_to = self.lock_for(&to.key());
        let _guard2 = lock_to.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(from)?;
        if self.collection_dir(to).exists() {
            if !overwrite {
                return Err(StorageError::PreconditionFailed);
            }
            if let Ok(existing) = self.read_meta(to) {
                let index = self.index();
                index.execute(
                    "DELETE FROM collections WHERE id = ?1",
                    params![existing.id],
                )?;
            }
            fs::remove_dir_all(self.collection_dir(to))?;
        }
        fs::rename(self.collection_dir(from), self.collection_dir(to))?;
        meta.slug = to.slug.as_str().to_string();
        meta.touch();
        self.write_meta(to, &meta)?;
        let index = self.index();
        index.execute(
            "UPDATE collections SET slug = ?1, updated_at = ?2 WHERE id = ?3",
            params![meta.slug, meta.updated_at.to_rfc3339(), meta.id],
        )?;
        Ok(meta)
    }

    pub fn find_feed_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<(CollectionPath, FeedMeta)>> {
        let users_dir = self.root.join("users");
        if !users_dir.is_dir() {
            return Ok(None);
        }
        for user_entry in fs::read_dir(&users_dir)? {
            let user_entry = user_entry?;
            if !user_entry.path().is_dir() {
                continue;
            }
            let Some(user_name) = user_entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(user) = UserName::parse(user_name) else {
                continue;
            };
            for entry in fs::read_dir(user_entry.path())? {
                let entry = entry?;
                if !entry.path().is_dir() {
                    continue;
                }
                let Some(slug_name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let Ok(slug) = oxicale_core::Slug::parse(slug_name) else {
                    continue;
                };
                let path = CollectionPath::new(user.clone(), slug);
                let Ok(meta) = self.read_meta(&path) else {
                    continue;
                };
                for feed in &meta.feeds {
                    if constant_time_eq(feed.token_sha256.as_bytes(), token_hash.as_bytes()) {
                        return Ok(Some((path, feed.clone())));
                    }
                }
            }
        }
        Ok(None)
    }

    pub async fn create_feed(
        &self,
        path: &CollectionPath,
        creation: FeedCreation,
    ) -> Result<FeedMeta> {
        let lock_7 = self.lock_for(&path.key());
        let _guard = lock_7.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        if meta.kind != CollectionKind::Calendar {
            return Err(StorageError::Invalid(
                "feeds can only belong to calendars".into(),
            ));
        }
        let feed = FeedMeta {
            id: uuid::Uuid::new_v4().to_string(),
            name: creation.name,
            components: creation.components,
            token_sha256: creation.token_sha256,
            token_hint: creation.token_hint,
            created_at: Utc::now(),
            expires_at: creation.expires_at,
            revoked_at: None,
            last_used_at: None,
        };
        meta.feeds.push(feed.clone());
        meta.touch();
        self.write_meta(path, &meta)?;
        Ok(feed)
    }

    pub async fn revoke_feed(&self, path: &CollectionPath, feed_id: &str) -> Result<FeedMeta> {
        let lock_8 = self.lock_for(&path.key());
        let _guard = lock_8.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let feed = meta
            .feeds
            .iter_mut()
            .find(|feed| feed.id == feed_id)
            .ok_or_else(|| StorageError::NotFound("feed".into()))?;
        feed.revoked_at = Some(Utc::now());
        let result = feed.clone();
        meta.touch();
        self.write_meta(path, &meta)?;
        Ok(result)
    }

    pub async fn rotate_feed(
        &self,
        path: &CollectionPath,
        feed_id: &str,
        token_sha256: String,
        token_hint: String,
    ) -> Result<FeedMeta> {
        let lock_9 = self.lock_for(&path.key());
        let _guard = lock_9.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let feed = meta
            .feeds
            .iter_mut()
            .find(|feed| feed.id == feed_id)
            .ok_or_else(|| StorageError::NotFound("feed".into()))?;
        feed.token_sha256 = token_sha256;
        feed.token_hint = token_hint;
        feed.revoked_at = None;
        let result = feed.clone();
        meta.touch();
        self.write_meta(path, &meta)?;
        Ok(result)
    }

    pub async fn mark_feed_used(&self, path: &CollectionPath, feed_id: &str) -> Result<()> {
        let lock_10 = self.lock_for(&path.key());
        let _guard = lock_10.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let now = Utc::now();
        let Some(feed) = meta.feeds.iter_mut().find(|feed| feed.id == feed_id) else {
            return Ok(());
        };
        if feed
            .last_used_at
            .is_none_or(|last| now - last >= chrono::Duration::seconds(60))
        {
            feed.last_used_at = Some(now);
            meta.touch();
            self.write_meta(path, &meta)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subscription_collection(
        &self,
        username: &UserName,
        slug: &str,
        display_name: &str,
        color: Option<String>,
        mut source: SubscriptionSource,
        items: &[FetchedItem],
        etag: Option<String>,
        last_modified: Option<String>,
        remote_name: Option<String>,
        remote_description: Option<String>,
        remote_color: Option<String>,
        content_hash: Option<String>,
    ) -> Result<CollectionMeta> {
        let slug = oxicale_core::Slug::parse(slug)?;
        let path = CollectionPath::new(username.clone(), slug);
        let key = path.key();
        let lock_11 = self.lock_for(&key);
        let _guard = lock_11.write().expect("collection lock poisoned");
        if self.collection_dir(&path).exists() {
            return Err(StorageError::Conflict(format!(
                "collection {path} already exists"
            )));
        }
        self.read_principal(username)?;
        fs::create_dir_all(self.items_dir(&path))?;
        set_dir_mode(&self.collection_dir(&path))?;
        set_dir_mode(&self.items_dir(&path))?;
        let mut meta = CollectionMeta::new(
            username.as_str(),
            path.slug.as_str(),
            CollectionKind::Calendar,
            display_name,
        );
        if let Some(color) = color {
            validate_color(&color)?;
            meta.color = Some(color);
        }
        if meta.description.is_none() {
            meta.description = remote_description.clone();
        }
        let now = Utc::now();
        source.last_success_at = Some(now);
        source.last_attempt_at = Some(now);
        source.etag = etag;
        source.last_modified = last_modified;
        source.remote_display_name = remote_name;
        source.remote_description = remote_description;
        source.remote_color = remote_color;
        source.content_hash = content_hash;
        source.consecutive_failures = 0;
        source.last_error = None;
        source.next_attempt_at =
            Some(now + chrono::Duration::seconds(source.refresh_interval_seconds as i64));
        source.remote_keys.clear();
        meta.subscription = Some(source);
        let result = (|| -> Result<()> {
            let mut remote_keys = BTreeMap::new();
            let principal = self.read_principal(username)?;
            self.sync_collection_row(&meta, &path)?;
            for item in items.iter() {
                let href = href_for_remote_key(&item.remote_key, &remote_keys);
                let item_path = self.items_dir(&path).join(&href);
                atomic_write(&item_path, item.payload.as_bytes(), self.options.fsync)?;
                remote_keys.insert(item.remote_key.clone(), href.clone());
                let etag = content_etag(item.payload.as_bytes());
                meta.revision += 1;
                let index_db = self.index();
                index_db.execute(
                    "INSERT OR REPLACE INTO collections (id, owner, slug, kind, display_name, color, collection_uid, revision, read_only, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        meta.id,
                        principal.id,
                        meta.slug,
                        meta.kind.as_str(),
                        meta.display_name,
                        meta.color,
                        meta.collection_uid,
                        meta.revision as i64,
                        1,
                        meta.updated_at.to_rfc3339()
                    ],
                )?;
                index_db.execute(
                    "INSERT OR REPLACE INTO items (collection_id, href, uid, component, etag, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        meta.id,
                        href,
                        item.uid.clone().unwrap_or_else(|| item.remote_key.clone()),
                        item.component.as_str(),
                        etag,
                        item.payload.len() as i64,
                        now.to_rfc3339()
                    ],
                )?;
                index_db.execute(
                    "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'put', ?4, ?5)",
                    params![
                        meta.id,
                        meta.revision as i64,
                        href,
                        etag,
                        now.to_rfc3339()
                    ],
                )?;
            }
            if let Some(subscription) = meta.subscription.as_mut() {
                subscription.remote_keys = remote_keys;
            }
            meta.touch();
            self.write_meta(&path, &meta)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(self.collection_dir(&path));
            let _ = self
                .index()
                .execute("DELETE FROM collections WHERE id = ?1", params![meta.id]);
            return Err(error);
        }
        Ok(meta)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn apply_subscription_snapshot(
        &self,
        path: &CollectionPath,
        items: &[FetchedItem],
        etag: Option<String>,
        last_modified: Option<String>,
        next_attempt_at: DateTime<Utc>,
        remote_name: Option<String>,
        remote_description: Option<String>,
        remote_color: Option<String>,
        content_hash: Option<String>,
    ) -> Result<bool> {
        let lock_12 = self.lock_for(&path.key());
        let _guard = lock_12.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        meta.subscription
            .as_ref()
            .ok_or_else(|| StorageError::Invalid("not a subscription".into()))?;
        let current = self.list_items_unlocked(path)?;
        let current_by_href: BTreeMap<String, StoredItem> = current
            .into_iter()
            .map(|item| (item.href.to_string(), item))
            .collect();
        let mut remote_keys = BTreeMap::new();
        let mut desired: Vec<(String, &FetchedItem)> = Vec::new();
        for item in items {
            let href = href_for_remote_key(&item.remote_key, &remote_keys);
            remote_keys.insert(item.remote_key.clone(), href.clone());
            desired.push((href, item));
        }
        let desired_hrefs: BTreeSet<String> =
            desired.iter().map(|(href, _)| href.clone()).collect();
        let mut changed = false;
        let mut writes: Vec<(String, &FetchedItem)> = Vec::new();
        for (href, item) in &desired {
            match current_by_href.get(href) {
                Some(existing) if existing.payload == item.payload => {}
                _ => {
                    changed = true;
                    writes.push((href.clone(), item));
                }
            }
        }
        let deletions: Vec<String> = current_by_href
            .keys()
            .filter(|href| !desired_hrefs.contains(*href))
            .cloned()
            .collect();
        if !deletions.is_empty() {
            changed = true;
        }

        let now = Utc::now();
        let mut seq = meta.revision;
        if changed {
            let index = self.index();
            for (href, item) in &writes {
                let item_path = self.items_dir(path).join(href);
                atomic_write(&item_path, item.payload.as_bytes(), self.options.fsync)?;
                seq += 1;
                let etag = content_etag(item.payload.as_bytes());
                index.execute(
                    "INSERT OR REPLACE INTO items (collection_id, href, uid, component, etag, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        meta.id,
                        href,
                        item.uid.clone().unwrap_or_else(|| item.remote_key.clone()),
                        item.component.as_str(),
                        etag,
                        item.payload.len() as i64,
                        now.to_rfc3339()
                    ],
                )?;
                index.execute(
                    "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'put', ?4, ?5)",
                    params![meta.id, seq as i64, href, etag, now.to_rfc3339()],
                )?;
            }
            for href in &deletions {
                let item_path = self.items_dir(path).join(href);
                if item_path.exists() {
                    fs::remove_file(item_path)?;
                }
                seq += 1;
                index.execute(
                    "DELETE FROM items WHERE collection_id = ?1 AND href = ?2",
                    params![meta.id, href],
                )?;
                index.execute(
                    "INSERT INTO changes (collection_id, seq, href, operation, etag, changed_at) VALUES (?1, ?2, ?3, 'delete', NULL, ?4)",
                    params![meta.id, seq as i64, href, now.to_rfc3339()],
                )?;
            }
            index.execute(
                "UPDATE collections SET revision = ?1, updated_at = ?2, read_only = 1 WHERE id = ?3",
                params![seq as i64, now.to_rfc3339(), meta.id],
            )?;
        }

        {
            let subscription = meta.subscription.as_mut().expect("subscription");
            subscription.etag = etag;
            subscription.last_modified = last_modified;
            subscription.last_attempt_at = Some(now);
            subscription.last_success_at = Some(now);
            subscription.next_attempt_at = Some(next_attempt_at);
            subscription.last_error = None;
            subscription.consecutive_failures = 0;
            subscription.remote_display_name = remote_name;
            subscription.remote_description = remote_description;
            subscription.remote_color = remote_color;
            subscription.content_hash = content_hash;
            subscription.remote_keys = remote_keys;
        }
        if changed {
            meta.revision = seq;
        }
        meta.touch();
        self.write_meta(path, &meta)?;
        Ok(changed)
    }

    pub async fn update_subscription_interval(
        &self,
        path: &CollectionPath,
        interval_seconds: u64,
    ) -> Result<CollectionMeta> {
        let lock = self.lock_for(&path.key());
        let _guard = lock.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let subscription = meta
            .subscription
            .as_mut()
            .ok_or_else(|| StorageError::Invalid("not a subscription".into()))?;
        subscription.refresh_interval_seconds = interval_seconds;
        subscription.next_attempt_at =
            Some(Utc::now() + chrono::Duration::seconds(interval_seconds as i64));
        meta.touch();
        self.write_meta(path, &meta)?;
        Ok(meta)
    }

    pub async fn apply_subscription_fetch_state(
        &self,
        path: &CollectionPath,
        etag: Option<String>,
        last_modified: Option<String>,
        next_attempt_at: DateTime<Utc>,
        not_modified: bool,
    ) -> Result<()> {
        let lock_13 = self.lock_for(&path.key());
        let _guard = lock_13.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let now = Utc::now();
        let subscription = meta
            .subscription
            .as_mut()
            .ok_or_else(|| StorageError::Invalid("not a subscription".into()))?;
        if !not_modified {
            subscription.etag = etag;
            subscription.last_modified = last_modified;
        }
        subscription.last_attempt_at = Some(now);
        subscription.last_success_at = Some(now);
        subscription.next_attempt_at = Some(next_attempt_at);
        subscription.last_error = None;
        subscription.consecutive_failures = 0;
        meta.touch();
        self.write_meta(path, &meta)
    }

    pub async fn record_subscription_failure(
        &self,
        path: &CollectionPath,
        error: String,
        next_attempt_at: DateTime<Utc>,
        consecutive_failures: u32,
    ) -> Result<()> {
        let lock_14 = self.lock_for(&path.key());
        let _guard = lock_14.write().expect("collection lock poisoned");
        let mut meta = self.read_meta(path)?;
        let now = Utc::now();
        let subscription = meta
            .subscription
            .as_mut()
            .ok_or_else(|| StorageError::Invalid("not a subscription".into()))?;
        subscription.last_attempt_at = Some(now);
        subscription.next_attempt_at = Some(next_attempt_at);
        subscription.last_error = Some(error);
        subscription.consecutive_failures = consecutive_failures;
        meta.touch();
        self.write_meta(path, &meta)
    }

    pub fn due_subscriptions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<(CollectionPath, CollectionMeta)>> {
        let mut due = Vec::new();
        let users_dir = self.root.join("users");
        if !users_dir.is_dir() {
            return Ok(due);
        }
        for user_entry in fs::read_dir(users_dir)? {
            let user_entry = user_entry?;
            if !user_entry.path().is_dir() {
                continue;
            }
            let Some(user_name) = user_entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(user) = UserName::parse(user_name) else {
                continue;
            };
            for entry in fs::read_dir(user_entry.path())? {
                let entry = entry?;
                if !entry.path().is_dir() {
                    continue;
                }
                let Some(slug_name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let Ok(slug) = oxicale_core::Slug::parse(slug_name) else {
                    continue;
                };
                let path = CollectionPath::new(user.clone(), slug);
                let Ok(meta) = self.read_meta(&path) else {
                    continue;
                };
                let Some(subscription) = &meta.subscription else {
                    continue;
                };
                if subscription.next_attempt_at.is_none_or(|next| next <= now) {
                    due.push((path, meta));
                }
            }
        }
        Ok(due)
    }

    pub fn rebuild_index(&self) -> Result<VerifyReport> {
        let mut report = VerifyReport {
            principals: 0,
            collections: 0,
            items: 0,
            errors: Vec::new(),
        };
        {
            let index = self.index();
            index.execute_batch(
                "DELETE FROM changes; DELETE FROM sync_tokens; DELETE FROM items; DELETE FROM collections; DELETE FROM principals; DELETE FROM subscriptions; DELETE FROM feed_usage;",
            )?;
            index.execute(
                "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('generation', ?1)",
                params![uuid::Uuid::new_v4().to_string()],
            )?;
        }
        let users_dir = self.root.join("users");
        if !users_dir.is_dir() {
            return Ok(report);
        }
        let generation = self.index_generation()?;
        let _ = generation;
        for user_entry in fs::read_dir(&users_dir)? {
            let user_entry = user_entry?;
            if !user_entry.path().is_dir() {
                continue;
            }
            let Some(user_name) = user_entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(user) = UserName::parse(user_name) else {
                report.errors.push(format!(
                    "invalid user directory name: {}",
                    user_entry.path().display()
                ));
                continue;
            };
            let principal = match self.read_principal(&user) {
                Ok(principal) => principal,
                Err(error) => {
                    report.errors.push(format!("{user}: {error}"));
                    continue;
                }
            };
            report.principals += 1;
            {
                let index = self.index();
                index.execute(
                    "INSERT OR REPLACE INTO principals (id, username, display_name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        principal.id,
                        principal.username,
                        principal.display_name,
                        principal.created_at.to_rfc3339(),
                        principal.updated_at.to_rfc3339()
                    ],
                )?;
            }
            for entry in fs::read_dir(user_entry.path())? {
                let entry = entry?;
                if !entry.path().is_dir() {
                    continue;
                }
                let Some(slug_name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let Ok(slug) = oxicale_core::Slug::parse(slug_name) else {
                    report.errors.push(format!(
                        "invalid collection directory: {}",
                        entry.path().display()
                    ));
                    continue;
                };
                let path = CollectionPath::new(user.clone(), slug);
                let meta = match self.read_meta(&path) {
                    Ok(meta) => meta,
                    Err(error) => {
                        report.errors.push(format!("{path}: {error}"));
                        continue;
                    }
                };
                report.collections += 1;
                self.sync_collection_row(&meta, &path)?;
                match self.list_items(&path) {
                    Ok(items) => {
                        let now = Utc::now();
                        let index = self.index();
                        for item in items {
                            report.items += 1;
                            index.execute(
                                "INSERT OR REPLACE INTO items (collection_id, href, uid, component, etag, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                                params![
                                    meta.id,
                                    item.href.as_str(),
                                    item.uid,
                                    item.component.as_str(),
                                    item.etag,
                                    item.size as i64,
                                    now.to_rfc3339()
                                ],
                            )?;
                        }
                    }
                    Err(error) => report.errors.push(format!("{path}: {error}")),
                }
            }
        }
        Ok(report)
    }

    pub fn verify_storage(&self) -> Result<VerifyReport> {
        let mut report = VerifyReport {
            principals: 0,
            collections: 0,
            items: 0,
            errors: Vec::new(),
        };
        let mut seen_collections = BTreeSet::new();
        let users_dir = self.root.join("users");
        if !users_dir.is_dir() {
            return Ok(report);
        }
        for user_entry in fs::read_dir(&users_dir)? {
            let user_entry = user_entry?;
            if !user_entry.path().is_dir() {
                continue;
            }
            let Some(user_name) = user_entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(user) = UserName::parse(user_name) else {
                report.errors.push(format!(
                    "invalid user directory name: {}",
                    user_entry.path().display()
                ));
                continue;
            };
            match self.read_principal(&user) {
                Ok(_) => report.principals += 1,
                Err(error) => {
                    report.errors.push(format!("{user}: {error}"));
                    continue;
                }
            }
            for entry in fs::read_dir(user_entry.path())? {
                let entry = entry?;
                if !entry.path().is_dir() {
                    continue;
                }
                let Some(slug_name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let Ok(slug) = oxicale_core::Slug::parse(slug_name) else {
                    report.errors.push(format!(
                        "invalid collection directory: {}",
                        entry.path().display()
                    ));
                    continue;
                };
                let path = CollectionPath::new(user.clone(), slug);
                let meta = match self.read_meta(&path) {
                    Ok(meta) => meta,
                    Err(error) => {
                        report.errors.push(format!("{path}: {error}"));
                        continue;
                    }
                };
                report.collections += 1;
                seen_collections.insert(meta.id.clone());
                let indexed: BTreeMap<String, (String, String, String)> = {
                    let index = self.index();
                    let mut statement = index.prepare(
                        "SELECT href, uid, component, etag FROM items WHERE collection_id = ?1",
                    )?;
                    let rows = statement.query_map(params![meta.id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            (
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                            ),
                        ))
                    })?;
                    rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?
                };
                match self.list_items(&path) {
                    Ok(items) => {
                        report.items += items.len();
                        for item in &items {
                            match indexed.get(item.href.as_str()) {
                                Some((uid, component, etag))
                                    if uid == &item.uid
                                        && component == item.component.as_str()
                                        && etag == &item.etag => {}
                                Some(_) => report.errors.push(format!(
                                    "{path}: index row for {} does not match the payload",
                                    item.href
                                )),
                                None => report.errors.push(format!(
                                    "{path}: {} is missing from the index",
                                    item.href
                                )),
                            }
                        }
                        for href in indexed.keys() {
                            if !items.iter().any(|item| item.href.as_str() == href) {
                                report
                                    .errors
                                    .push(format!("{path}: index references missing item {href}"));
                            }
                        }
                    }
                    Err(error) => report.errors.push(format!("{path}: {error}")),
                }
            }
        }
        {
            let index = self.index();
            let mut statement = index.prepare("SELECT id FROM collections")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let id = row?;
                if !seen_collections.contains(&id) {
                    report.errors.push(format!(
                        "index references collection {id} that is missing from disk"
                    ));
                }
            }
        }
        Ok(report)
    }

    pub fn generate_feed_bytes(
        &self,
        path: &CollectionPath,
        feed: &FeedMeta,
        prodid: &str,
    ) -> Result<(String, String, DateTime<Utc>)> {
        self.with_read(&path.key(), || {
            let meta = self.read_meta(path)?;
            let items = self.list_items_unlocked(path)?;
            let last_modified = items
                .iter()
                .map(|item| item.updated_at)
                .max()
                .unwrap_or(meta.updated_at);
            let payloads: Vec<String> = items.into_iter().map(|item| item.payload).collect();
            let body = oxicale_ical::generate_feed(
                &payloads,
                feed.components,
                Some(&feed.name),
                prodid,
                self.ical_limits(),
            )
            .map_err(|error| StorageError::Ical(error.to_string()))?;
            let etag = content_etag(body.as_bytes());
            Ok((body, etag, last_modified))
        })
    }
}

fn acquire_process_lock(file: &File, timeout: std::time::Duration) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) => {
                let would_block = error.kind() == std::io::ErrorKind::WouldBlock;
                if !would_block || start.elapsed() >= timeout {
                    return Err(StorageError::Locked);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn href_for_remote_key(key: &str, existing: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut base = hex(&digest[..20]);
    let mut candidate = format!("{base}.ics");
    let mut counter = 1;
    while existing
        .values()
        .any(|href| href.eq_ignore_ascii_case(&candidate))
    {
        base.truncate(38);
        candidate = format!("{base}{counter}.ics");
        counter += 1;
    }
    candidate
}

fn init_schema(index: &Connection) -> rusqlite::Result<()> {
    index.execute_batch(
        "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS index_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS principals (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS collections (
    id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    slug TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    display_name TEXT NOT NULL,
    color TEXT,
    collection_uid TEXT NOT NULL,
    revision INTEGER NOT NULL,
    read_only INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    UNIQUE (owner, slug)
);
CREATE TABLE IF NOT EXISTS items (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    href TEXT NOT NULL,
    uid TEXT NOT NULL,
    component TEXT NOT NULL,
    etag TEXT NOT NULL,
    size INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (collection_id, href)
);
CREATE UNIQUE INDEX IF NOT EXISTS items_uid ON items (collection_id, uid);
CREATE TABLE IF NOT EXISTS changes (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    href TEXT,
    operation TEXT NOT NULL CHECK (operation IN ('put', 'delete')),
    etag TEXT,
    changed_at TEXT NOT NULL,
    PRIMARY KEY (collection_id, seq)
);
CREATE TABLE IF NOT EXISTS sync_tokens (
    token TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS subscriptions (
    collection_id TEXT PRIMARY KEY REFERENCES collections(id) ON DELETE CASCADE,
    next_attempt_at TEXT,
    last_success_at TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS feed_usage (
    feed_id TEXT PRIMARY KEY,
    last_used_at TEXT
);
",
    )
}

fn enforce_preconditions(
    existing_etag: Option<&str>,
    preconditions: Preconditions<'_>,
) -> Result<()> {
    if let Some(expected) = preconditions.if_match {
        let Some(actual) = existing_etag else {
            return Err(StorageError::PreconditionFailed);
        };
        if !etag_header_matches(expected, actual) {
            return Err(StorageError::PreconditionFailed);
        }
    }
    if preconditions.if_none_match && existing_etag.is_some() {
        return Err(StorageError::PreconditionFailed);
    }
    Ok(())
}

fn etag_header_matches(header: &str, actual: &str) -> bool {
    let header = header.trim();
    if header == "*" {
        return true;
    }
    header.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        let candidate = candidate
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(candidate);
        candidate == actual
    })
}

fn validate_color(value: &str) -> Result<()> {
    let valid_len = value.len() == 7 || value.len() == 9;
    if !valid_len || !value.starts_with('#') || !value[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(StorageError::Invalid(
            "colour must be #RRGGBB or #RRGGBBAA".to_string(),
        ));
    }
    Ok(())
}

fn read_payload(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StorageError::NotFound(format!("item {}", path.display()))
        } else {
            StorageError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(StorageError::Invalid(
            "symlinked item paths are not allowed".to_string(),
        ));
    }
    Ok(fs::read(path)?)
}

fn atomic_write(path: &Path, bytes: &[u8], fsync: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::Invalid("path has no parent".to_string()))?;
    fs::create_dir_all(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(StorageError::Invalid(
                "refusing to write through a symlink".to_string(),
            ));
        }
    }
    let temp = parent.join(format!(".tmp.{}", uuid::Uuid::new_v4()));
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        set_file_mode(&temp)?;
        file.write_all(bytes)?;
        if fsync {
            file.sync_all()?;
        }
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(StorageError::Io(error));
    }
    if fsync {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn remove_temp_files_recursive(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_temp_files_recursive(&path);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tmp."))
        {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn set_file_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path) -> Result<()> {
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
