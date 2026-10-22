use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxicale_core::{CollectionKind, Href, UserName};
use oxicale_storage::{CollectionUpdate, Store, StoreOptions};
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct MigrationReport {
    pub principals: usize,
    pub collections: usize,
    pub items: usize,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    pub component_set_changes: Vec<String>,
    pub subscriptions_needing_source: Vec<String>,
    pub feed_suggestions: Vec<String>,
    pub dry_run: bool,
}

impl MigrationReport {
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.dry_run {
            out.push_str("Radicale migration (dry run)\n");
        } else {
            out.push_str("Radicale migration complete\n");
        }
        out.push_str(&format!("Principals: {}\n", self.principals));
        out.push_str(&format!("Collections: {}\n", self.collections));
        out.push_str(&format!("Items: {}\n", self.items));
        for warning in &self.warnings {
            out.push_str(&format!("Warning: {warning}\n"));
        }
        for change in &self.component_set_changes {
            out.push_str(&format!("Component set normalised: {change}\n"));
        }
        for subscription in &self.subscriptions_needing_source {
            out.push_str(&format!("Subscription needs a source: {subscription}\n"));
        }
        for suggestion in &self.feed_suggestions {
            out.push_str(&format!("Suggested feed: {suggestion}\n"));
        }
        for skipped in &self.skipped {
            out.push_str(&format!("Skipped: {skipped}\n"));
        }
        out
    }
}

#[derive(Clone, Debug, Default)]
struct RadicaleCollection {
    user: String,
    relative_path: String,
    directory: PathBuf,
    tag: String,
    display_name: Option<String>,
    description: Option<String>,
    color: Option<String>,
    components: Option<String>,
    source: Option<String>,
}

pub fn migrate(source: &Path, target: &Path, dry_run: bool) -> Result<MigrationReport, String> {
    if !source.is_dir() {
        return Err(format!(
            "source directory {} does not exist",
            source.display()
        ));
    }
    let mut report = MigrationReport {
        dry_run,
        ..Default::default()
    };
    let collections = scan(source, &mut report)?;
    if dry_run {
        validate_only(source, &collections, &mut report)?;
        return Ok(report);
    }
    let store = Store::open_with_options(
        target,
        StoreOptions {
            fsync: true,
            ..Default::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let mut created_users = BTreeMap::new();
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    for collection in &collections {
        let user = match UserName::parse(collection.user.clone()) {
            Ok(user) => user,
            Err(error) => {
                report.skipped.push(format!(
                    "{}: invalid username: {error}",
                    collection.directory.display()
                ));
                continue;
            }
        };
        if !created_users.contains_key(user.as_str()) {
            runtime
                .block_on(store.provision_principal(&user))
                .map_err(|error| error.to_string())?;
            created_users.insert(user.as_str().to_string(), ());
            report.principals += 1;
        }
        let kind = match collection.tag.as_str() {
            "VADDRESSBOOK" => CollectionKind::AddressBook,
            _ => CollectionKind::Calendar,
        };
        let relative_slug = collection
            .relative_path
            .strip_prefix(&format!("{}/", collection.user))
            .unwrap_or("collection");
        let slug = slug_for_path(relative_slug);
        let path = oxicale_core::CollectionPath::parse(user.as_str(), &slug)
            .map_err(|error| error.to_string())?;
        if store.collection(&path).is_ok() {
            report
                .warnings
                .push(format!("collection {path} already exists; skipping"));
            continue;
        }
        let display_name = collection
            .display_name
            .clone()
            .unwrap_or_else(|| slug.clone());
        runtime
            .block_on(store.create_collection(&user, kind, &slug, &display_name))
            .map_err(|error| error.to_string())?;
        let update = CollectionUpdate {
            description: collection.description.clone(),
            color: collection.color.clone().filter(|color| is_color(color)),
            ..Default::default()
        };
        if update.description.is_some() || update.color.is_some() {
            let _ = runtime.block_on(store.update_collection(&path, update));
        }
        report.collections += 1;
        if collection.tag == "VCALENDAR" {
            if let Some(components) = &collection.components {
                if !components.contains("VEVENT")
                    || !components.contains("VTODO")
                    || !components.contains("VJOURNAL")
                {
                    report.component_set_changes.push(format!(
                        "{}: {components} -> VEVENT,VTODO,VJOURNAL",
                        collection.relative_path
                    ));
                    report.feed_suggestions.push(format!(
                        "{} ({} selected originally)",
                        collection.relative_path, components
                    ));
                }
            }
        }
        let items_dir = collection.directory.clone();
        let entries = match std::fs::read_dir(&items_dir) {
            Ok(entries) => entries,
            Err(error) => {
                report
                    .skipped
                    .push(format!("{}: {error}", items_dir.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let file = entry.path();
            if !file.is_file() {
                continue;
            }
            let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if !(name.ends_with(".ics") || name.ends_with(".vcf")) {
                continue;
            }
            let href = match Href::parse(name.to_string()) {
                Ok(href) => href,
                Err(error) => {
                    report
                        .skipped
                        .push(format!("{}: invalid href: {error}", file.display()));
                    continue;
                }
            };
            let payload = match std::fs::read_to_string(&file) {
                Ok(payload) => payload,
                Err(error) => {
                    report.skipped.push(format!("{}: {error}", file.display()));
                    continue;
                }
            };
            match runtime.block_on(store.put_item(
                &path,
                &href,
                &payload,
                false,
                oxicale_storage::Preconditions::default(),
            )) {
                Ok(_) => report.items += 1,
                Err(error) => report.skipped.push(format!("{}: {error}", file.display())),
            }
        }
        if collection.tag == "VSUBSCRIBED" {
            match &collection.source {
                Some(source) if !source.is_empty() => {
                    runtime
                        .block_on(store.attach_imported_subscription(&path, source.clone()))
                        .map_err(|error| error.to_string())?;
                    report.warnings.push(format!(
                        "{}: imported as a read-only subscription from {source}",
                        collection.relative_path
                    ));
                }
                _ => {
                    runtime
                        .block_on(store.set_collection_read_only(&path, true))
                        .map_err(|error| error.to_string())?;
                    report
                        .subscriptions_needing_source
                        .push(collection.relative_path.clone());
                }
            }
        }
    }
    Ok(report)
}

fn validate_only(
    source: &Path,
    collections: &[RadicaleCollection],
    report: &mut MigrationReport,
) -> Result<(), String> {
    let users: BTreeMap<String, ()> = collections
        .iter()
        .map(|collection| (collection.user.clone(), ()))
        .collect();
    report.principals = users.len();
    for collection in collections {
        report.collections += 1;
        if collection.tag == "VCALENDAR" {
            if let Some(components) = &collection.components {
                if !components.contains("VEVENT")
                    || !components.contains("VTODO")
                    || !components.contains("VJOURNAL")
                {
                    report.component_set_changes.push(format!(
                        "{}: {components} -> VEVENT,VTODO,VJOURNAL",
                        collection.relative_path
                    ));
                }
            }
        }
        for entry in std::fs::read_dir(&collection.directory)
            .map_err(|error| error.to_string())?
            .flatten()
        {
            let file = entry.path();
            if !file.is_file() {
                continue;
            }
            let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') || !(name.ends_with(".ics") || name.ends_with(".vcf")) {
                continue;
            }
            let payload = std::fs::read_to_string(&file).map_err(|error| error.to_string())?;
            let valid = if name.ends_with(".vcf") {
                oxicale_vcard::validate_vcard(&payload, oxicale_vcard::VcardLimits::default())
                    .is_ok()
            } else {
                oxicale_ical::validate_calendar_object(
                    &payload,
                    oxicale_ical::Limits::default(),
                    chrono_tz::UTC,
                    false,
                )
                .is_ok()
            };
            if valid {
                report.items += 1;
            } else {
                report
                    .skipped
                    .push(format!("{}: invalid item", file.display()));
            }
        }
    }
    let _ = source;
    Ok(())
}

fn scan(source: &Path, report: &mut MigrationReport) -> Result<Vec<RadicaleCollection>, String> {
    let mut collections = Vec::new();
    let entries = std::fs::read_dir(source).map_err(|error| error.to_string())?;
    for user_entry in entries.flatten() {
        let user_path = user_entry.path();
        if !user_path.is_dir() {
            continue;
        }
        let Some(user) = user_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if user.starts_with('.') {
            continue;
        }
        walk_collections(source, user, &user_path, report, &mut collections)?;
    }
    Ok(collections)
}

fn walk_collections(
    root: &Path,
    user: &str,
    directory: &Path,
    _report: &mut MigrationReport,
    out: &mut Vec<RadicaleCollection>,
) -> Result<(), String> {
    let props_path = directory.join(".Radicale.props");
    if props_path.is_file() {
        let text = std::fs::read_to_string(&props_path).map_err(|error| error.to_string())?;
        let props: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let tag = props
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or("VCALENDAR")
            .to_string();
        let relative = directory
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .strip_prefix(user)
            .unwrap_or_else(|_| Path::new(""))
            .to_string_lossy()
            .trim_matches('/')
            .to_string();
        if tag == "PRINCIPAL" {
            return Ok(());
        }
        let display_name = string_prop(&props, "D:displayname");
        let description = string_prop(&props, "C:calendar-description");
        let color = string_prop(&props, "ICAL:calendar-color");
        let components = props
            .get("C:supported-calendar-component-set")
            .map(|value| value.to_string());
        let source = string_prop(&props, "CS:source").or_else(|| string_prop(&props, "C:source"));
        out.push(RadicaleCollection {
            user: user.to_string(),
            relative_path: if relative.is_empty() {
                user.to_string()
            } else {
                format!("{user}/{relative}")
            },
            directory: directory.to_path_buf(),
            tag,
            display_name,
            description,
            color,
            components,
            source,
        });
    }
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() && !name.starts_with('.') {
            walk_collections(root, user, &path, _report, out)?;
        }
    }
    Ok(())
}

fn string_prop(props: &Value, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            props
                .get(key)
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn slug_for_path(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "collection".to_string()
    } else {
        out.to_string()
    }
}

fn is_color(value: &str) -> bool {
    (value.len() == 7 || value.len() == 9)
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) {
        let root = dir.join("collection-root");
        let alice = root.join("alice");
        let calendar = alice.join("calendar");
        std::fs::create_dir_all(&calendar).unwrap();
        std::fs::write(
            calendar.join(".Radicale.props"),
            r#"{"tag":"VCALENDAR","D:displayname":"Work","C:supported-calendar-component-set":"VTODO"}"#,
        )
        .unwrap();
        std::fs::write(
            calendar.join("event.ics"),
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        let contacts = alice.join("contacts");
        std::fs::create_dir_all(&contacts).unwrap();
        std::fs::write(
            contacts.join(".Radicale.props"),
            r#"{"tag":"VADDRESSBOOK","D:displayname":"Contacts"}"#,
        )
        .unwrap();
        std::fs::write(
            contacts.join("card.vcf"),
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Alice\r\nEND:VCARD\r\n",
        )
        .unwrap();
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let dir = tempfile::TempDir::new().unwrap();
        fixture(dir.path());
        let target = dir.path().join("target");
        let report = migrate(&dir.path().join("collection-root"), &target, true).unwrap();
        assert_eq!(report.principals, 1);
        assert_eq!(report.collections, 2);
        assert_eq!(report.items, 2);
        assert!(!report.component_set_changes.is_empty());
        assert!(!target.exists());
    }

    #[test]
    fn imports_items_and_preserves_hrefs() {
        let dir = tempfile::TempDir::new().unwrap();
        fixture(dir.path());
        let target = dir.path().join("target");
        let report = migrate(&dir.path().join("collection-root"), &target, false).unwrap();
        assert_eq!(report.principals, 1);
        assert_eq!(report.collections, 2);
        assert_eq!(report.items, 2);
        let store = Store::open(&target).unwrap();
        let user = UserName::parse("alice").unwrap();
        let calendar = oxicale_core::CollectionPath::parse("alice", "calendar").unwrap();
        let item = store
            .get_item(&calendar, &Href::parse("event.ics").unwrap())
            .unwrap();
        assert_eq!(item.uid, "e1");
        assert_eq!(store.collection(&calendar).unwrap().display_name, "Work");
        let contacts = oxicale_core::CollectionPath::parse("alice", "contacts").unwrap();
        assert_eq!(
            store.collection(&contacts).unwrap().kind,
            CollectionKind::AddressBook
        );
        assert_eq!(store.list_collections(&user).unwrap().len(), 2);
    }
}
