use chrono::Utc;
use oxicale_core::{
    hash_token, CalendarComponent, CollectionKind, CollectionPath, ComponentSet, Href, UserName,
};
use oxicale_storage::*;
use tempfile::TempDir;

const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:One\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const EVENT_UPDATED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T113000Z\r\nSUMMARY:One updated\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const TODO: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:todo-1\r\nDUE:20261023T100000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
const VCARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:card-1\r\nFN:Alice\r\nEMAIL:alice@example.com\r\nEND:VCARD\r\n";

struct Fixture {
    _dir: TempDir,
    store: Store,
    user: UserName,
    path: CollectionPath,
}

async fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let user = UserName::parse("alice").unwrap();
    store
        .provision_defaults(
            &user,
            Some(&DefaultCollection {
                slug: "calendar".into(),
                name: "Calendar".into(),
            }),
            Some(&DefaultCollection {
                slug: "contacts".into(),
                name: "Contacts".into(),
            }),
        )
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "calendar").unwrap();
    Fixture {
        _dir: dir,
        store,
        user,
        path,
    }
}

#[tokio::test]
async fn provisioning_is_idempotent() {
    let f = fixture().await;
    let collections = f.store.list_collections(&f.user).unwrap();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0].slug, "calendar");
    assert_eq!(collections[1].slug, "contacts");
    assert_eq!(collections[0].components, ComponentSet::all());
    assert!(collections[1].components.is_empty());
    assert_eq!(collections[0].kind, CollectionKind::Calendar);

    f.store
        .provision_defaults(
            &f.user,
            Some(&DefaultCollection {
                slug: "calendar".into(),
                name: "Other name".into(),
            }),
            None,
        )
        .await
        .unwrap();
    let collections = f.store.list_collections(&f.user).unwrap();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0].display_name, "Calendar");
}

#[tokio::test]
async fn put_read_update_item_and_etags() {
    let f = fixture().await;
    let href = Href::parse("event-1.ics").unwrap();
    let outcome = f
        .store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    assert!(outcome.created);
    assert_eq!(outcome.revision, 1);

    let stored = f.store.get_item(&f.path, &href).unwrap();
    assert_eq!(stored.uid, "event-1");
    assert_eq!(stored.component, oxicale_core::ItemComponent::VEvent);
    assert_eq!(stored.etag, outcome.etag);

    // Rewriting identical bytes is still a write, but the ETag stays stable.
    let again = f
        .store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.etag, outcome.etag);

    let updated = f
        .store
        .put_item(
            &f.path,
            &href,
            EVENT_UPDATED,
            false,
            Preconditions::default(),
        )
        .await
        .unwrap();
    assert_ne!(updated.etag, outcome.etag);
    assert_eq!(f.store.list_items(&f.path).unwrap().len(), 1);
    assert!(f
        .store
        .get_item(&f.path, &href)
        .unwrap()
        .payload
        .contains("updated"));
}

#[tokio::test]
async fn uid_conflicts_and_preconditions() {
    let f = fixture().await;
    let href = Href::parse("event-1.ics").unwrap();
    let other = Href::parse("event-2.ics").unwrap();
    let created = f
        .store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();

    let conflict = f
        .store
        .put_item(&f.path, &other, EVENT, false, Preconditions::default())
        .await;
    assert!(matches!(conflict, Err(StorageError::Conflict(_))));

    let failed = f
        .store
        .put_item(
            &f.path,
            &other,
            TODO,
            false,
            Preconditions {
                if_match: Some(&created.etag),
                if_none_match: false,
            },
        )
        .await;
    assert!(matches!(failed, Err(StorageError::PreconditionFailed)));

    let wrong_etag = "\"deadbeef\"";
    let failed = f
        .store
        .put_item(
            &f.path,
            &href,
            EVENT_UPDATED,
            false,
            Preconditions {
                if_match: Some(wrong_etag),
                if_none_match: false,
            },
        )
        .await;
    assert!(matches!(failed, Err(StorageError::PreconditionFailed)));

    let star = format!("\"{}\"", created.etag);
    f.store
        .put_item(
            &f.path,
            &href,
            EVENT_UPDATED,
            false,
            Preconditions {
                if_match: Some(&star),
                if_none_match: false,
            },
        )
        .await
        .unwrap();

    let none_match = f
        .store
        .put_item(
            &f.path,
            &href,
            EVENT,
            false,
            Preconditions {
                if_match: None,
                if_none_match: true,
            },
        )
        .await;
    assert!(matches!(none_match, Err(StorageError::PreconditionFailed)));
}

#[tokio::test]
async fn invalid_items_are_rejected_without_damage() {
    let f = fixture().await;
    let href = Href::parse("event-1.ics").unwrap();
    f.store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();

    let bad = f
        .store
        .put_item(
            &f.path,
            &href,
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            false,
            Preconditions::default(),
        )
        .await;
    assert!(matches!(bad, Err(StorageError::Ical(_))));
    assert!(f
        .store
        .get_item(&f.path, &href)
        .unwrap()
        .payload
        .contains("SUMMARY:One"));

    // Wrong kind.
    let contacts = CollectionPath::parse("alice", "contacts").unwrap();
    let bad = f
        .store
        .put_item(&contacts, &href, EVENT, false, Preconditions::default())
        .await;
    assert!(matches!(bad, Err(StorageError::Vcard(_))));
    let card_href = Href::parse("card.vcf").unwrap();
    f.store
        .put_item(
            &contacts,
            &card_href,
            VCARD,
            false,
            Preconditions::default(),
        )
        .await
        .unwrap();
    let bad = f
        .store
        .put_item(
            &contacts,
            &card_href,
            EVENT,
            false,
            Preconditions::default(),
        )
        .await;
    assert!(matches!(bad, Err(StorageError::Vcard(_))));
}

#[tokio::test]
async fn delete_and_sync_changes() {
    let f = fixture().await;
    let initial = f.store.sync_token(&f.path).unwrap();
    let href = Href::parse("event-1.ics").unwrap();
    f.store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();

    let changes = f.store.changes_since(&f.path, &initial).unwrap();
    assert_eq!(changes.changes.len(), 1);
    assert_eq!(changes.changes[0].operation, ChangeOperation::Put);
    assert_eq!(changes.changes[0].href, href.as_str());

    let token = changes.token.clone();
    let unchanged = f.store.changes_since(&f.path, &token).unwrap();
    assert!(unchanged.changes.is_empty());
    assert_eq!(unchanged.token, token);

    let outcome = f.store.delete_item(&f.path, &href, None).await.unwrap();
    assert_eq!(outcome.revision, 2);
    let changes = f.store.changes_since(&f.path, &token).unwrap();
    assert_eq!(changes.changes.len(), 1);
    assert_eq!(changes.changes[0].operation, ChangeOperation::Delete);
    assert!(matches!(
        f.store.get_item(&f.path, &href),
        Err(StorageError::NotFound(_))
    ));

    // Tokens from another collection are invalid.
    let contacts = CollectionPath::parse("alice", "contacts").unwrap();
    assert!(f.store.changes_since(&contacts, &changes.token).is_err());
    assert!(f.store.changes_since(&f.path, "not-a-token").is_err());
}

#[tokio::test]
async fn move_item_and_collection() {
    let f = fixture().await;
    let from = Href::parse("a.ics").unwrap();
    let to = Href::parse("b.ics").unwrap();
    f.store
        .put_item(&f.path, &from, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    f.store
        .move_item(&f.path, &from, &f.path, &to, false)
        .await
        .unwrap();
    assert!(f.store.get_item(&f.path, &from).is_err());
    assert!(f.store.get_item(&f.path, &to).is_ok());

    let new_path = CollectionPath::parse("alice", "renamed").unwrap();
    f.store
        .move_collection(&f.path, &new_path, false)
        .await
        .unwrap();
    assert!(f.store.collection(&new_path).is_ok());
    assert!(f.store.collection(&f.path).is_err());
}

#[tokio::test]
async fn subscription_snapshot_reconciles() {
    let f = fixture().await;
    let slug = "remote";
    let source = SubscriptionSource {
        url: "https://example.test/team.ics".into(),
        original_url: "webcal://example.test/team.ics".into(),
        refresh_interval_seconds: 3600,
        ..Default::default()
    };
    let aggregate = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:One\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:todo-1\r\nDUE:20261023T100000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
    let items =
        oxicale_ical::split_calendar(aggregate, oxicale_ical::Limits::default(), chrono_tz::UTC)
            .unwrap();
    let meta = f
        .store
        .create_subscription_collection(
            &f.user,
            slug,
            "Remote",
            None,
            source,
            &items,
            Some("\"v1\"".into()),
            None,
            Some("Remote name".into()),
            None,
            None,
            Some("hash".into()),
        )
        .await
        .unwrap();
    assert!(meta.subscription.is_some());
    let remote_path = CollectionPath::parse("alice", slug).unwrap();
    assert_eq!(f.store.list_items(&remote_path).unwrap().len(), 2);

    // Client writes are rejected.
    let href = Href::parse("x.ics").unwrap();
    assert!(matches!(
        f.store
            .put_item(&remote_path, &href, EVENT, false, Preconditions::default())
            .await,
        Err(StorageError::ReadOnly)
    ));

    let initial_token = f.store.sync_token(&remote_path).unwrap();

    // Update the event, remove the task, add a journal.
    let updated = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T120000Z\r\nSUMMARY:Changed\r\nEND:VEVENT\r\nBEGIN:VJOURNAL\r\nUID:journal-1\r\nDTSTART:20261025T100000Z\r\nSUMMARY:J\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n";
    let new_items =
        oxicale_ical::split_calendar(updated, oxicale_ical::Limits::default(), chrono_tz::UTC)
            .unwrap();
    let changed = f
        .store
        .apply_subscription_snapshot(
            &remote_path,
            &new_items,
            Some("\"v2\"".into()),
            None,
            Utc::now() + chrono::Duration::hours(1),
            None,
            None,
            None,
            Some("hash2".into()),
        )
        .await
        .unwrap();
    assert!(changed);
    let changes = f.store.changes_since(&remote_path, &initial_token).unwrap();
    assert!(changes
        .changes
        .iter()
        .any(|c| c.operation == ChangeOperation::Delete));
    assert!(changes
        .changes
        .iter()
        .any(|c| c.operation == ChangeOperation::Put));
    let items = f.store.list_items(&remote_path).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i.uid == "journal-1"));
    assert!(!items.iter().any(|i| i.uid == "todo-1"));

    // Identical snapshot does not advance the token.
    let token = f.store.sync_token(&remote_path).unwrap();
    let changed = f
        .store
        .apply_subscription_snapshot(
            &remote_path,
            &new_items,
            Some("\"v2\"".into()),
            None,
            Utc::now() + chrono::Duration::hours(1),
            None,
            None,
            None,
            Some("hash2".into()),
        )
        .await
        .unwrap();
    assert!(!changed);
    assert_eq!(f.store.sync_token(&remote_path).unwrap(), token);

    // Failure preserves items and records an error.
    f.store
        .record_subscription_failure(&remote_path, "HTTP 500".into(), Utc::now(), 1)
        .await
        .unwrap();
    let meta = f.store.collection(&remote_path).unwrap();
    let source = meta.subscription.unwrap();
    assert_eq!(source.last_error.as_deref(), Some("HTTP 500"));
    assert_eq!(f.store.list_items(&remote_path).unwrap().len(), 2);
}

#[tokio::test]
async fn feeds_rotate_and_revoke() {
    let f = fixture().await;
    let token = oxicale_core::new_token();
    let feed = f
        .store
        .create_feed(
            &f.path,
            FeedCreation {
                token_sha256: hash_token(&token),
                token_hint: oxicale_core::token_hint(&token),
                name: "Tasks".into(),
                components: ComponentSet::one(CalendarComponent::VTodo),
                expires_at: None,
            },
        )
        .await
        .unwrap();
    let (path, found) = f
        .store
        .find_feed_by_hash(&hash_token(&token))
        .unwrap()
        .unwrap();
    assert_eq!(path, f.path);
    assert_eq!(found.id, feed.id);
    assert_eq!(
        found.components,
        ComponentSet::one(CalendarComponent::VTodo)
    );

    f.store.mark_feed_used(&f.path, &feed.id).await.unwrap();
    assert!(f.store.collection(&f.path).unwrap().feeds[0]
        .last_used_at
        .is_some());

    let rotated_token = oxicale_core::new_token();
    f.store
        .rotate_feed(
            &f.path,
            &feed.id,
            hash_token(&rotated_token),
            oxicale_core::token_hint(&rotated_token),
        )
        .await
        .unwrap();
    assert!(f
        .store
        .find_feed_by_hash(&hash_token(&token))
        .unwrap()
        .is_none());
    assert!(f
        .store
        .find_feed_by_hash(&hash_token(&rotated_token))
        .unwrap()
        .is_some());

    f.store.revoke_feed(&f.path, &feed.id).await.unwrap();
    let (_, found) = f
        .store
        .find_feed_by_hash(&hash_token(&rotated_token))
        .unwrap()
        .unwrap();
    assert!(!found.is_active(Utc::now()));
}

#[tokio::test]
async fn rebuild_index_preserves_items_and_invalidates_tokens() {
    let f = fixture().await;
    let href = Href::parse("event-1.ics").unwrap();
    f.store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    let token = f.store.sync_token(&f.path).unwrap();
    let report = f.store.rebuild_index().unwrap();
    assert_eq!(report.principals, 1);
    assert_eq!(report.collections, 2);
    assert_eq!(report.items, 1);
    assert!(report.errors.is_empty());
    assert_eq!(f.store.list_items(&f.path).unwrap().len(), 1);
    assert!(f.store.changes_since(&f.path, &token).is_err());
}

#[tokio::test]
async fn verify_detects_corrupt_item() {
    let f = fixture().await;
    let href = Href::parse("event-1.ics").unwrap();
    f.store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    assert!(f.store.verify_storage().unwrap().errors.is_empty());
    std::fs::write(
        f.store
            .data_dir()
            .join("users/alice/calendar/items/event-1.ics"),
        "not a calendar",
    )
    .unwrap();
    let report = f.store.verify_storage().unwrap();
    assert!(!report.errors.is_empty());
}

#[tokio::test]
async fn metadata_update_and_ctag() {
    let f = fixture().await;
    let before = f.store.collection(&f.path).unwrap();
    let ctag = f.store.ctag(&before);
    let updated = f
        .store
        .update_collection(
            &f.path,
            CollectionUpdate {
                display_name: Some("Family".into()),
                description: Some("Shared".into()),
                color: Some("#ff0000".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.display_name, "Family");
    assert_eq!(updated.description.as_deref(), Some("Shared"));
    assert_ne!(f.store.ctag(&updated), ctag);
    assert!(updated.updated_at >= before.updated_at);
    let bad = f
        .store
        .update_collection(
            &f.path,
            CollectionUpdate {
                color: Some("red".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(bad.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn storage_permissions_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture().await;
    let href = Href::parse("event-1.ics").unwrap();
    f.store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    let mode = std::fs::metadata(
        f.store
            .data_dir()
            .join("users/alice/calendar/items/event-1.ics"),
    )
    .unwrap()
    .permissions()
    .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(f.store.data_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_items_are_rejected() {
    let f = fixture().await;
    let href = Href::parse("link.ics").unwrap();
    let path = f
        .store
        .data_dir()
        .join("users/alice/calendar/items/link.ics");
    std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
    assert!(matches!(
        f.store.get_item(&f.path, &href),
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        f.store
            .put_item(&f.path, &href, EVENT, false, Preconditions::default())
            .await,
        Err(StorageError::Invalid(_))
    ));
}

#[tokio::test]
async fn replacing_an_item_cannot_change_its_uid() {
    let f = fixture().await;
    let href = Href::parse("event.ics").unwrap();
    f.store
        .put_item(&f.path, &href, EVENT, false, Preconditions::default())
        .await
        .unwrap();
    let different = EVENT.replace("UID:event-1", "UID:other-uid");
    let result = f
        .store
        .put_item(&f.path, &href, &different, false, Preconditions::default())
        .await;
    assert!(matches!(result, Err(StorageError::Conflict(_))));
    assert_eq!(f.store.get_item(&f.path, &href).unwrap().uid, "event-1");
}
