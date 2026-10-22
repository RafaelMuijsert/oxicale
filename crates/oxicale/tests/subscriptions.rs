mod common;

use std::sync::Arc;

use base64::Engine;
use chrono::Utc;
use common::MockFetcher;
use oxicale::crypto::SecretBox;
use oxicale::subscriptions::{
    FetchResponse, NewSubscription, RefreshOutcome, RemoteAuthInput, SubscriptionError,
    SubscriptionService,
};
use oxicale_core::{CollectionPath, UserName};
use oxicale_storage::{Store, StoreOptions};
use tempfile::TempDir;

const REMOTE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team Calendar\r\nX-WR-CALDESC:Shared calendar\r\nX-APPLE-CALENDAR-COLOR:#10b981\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Remote event\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:todo-1\r\nDUE:20261024T100000Z\r\nSUMMARY:Remote task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
const REMOTE_UPDATED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team Calendar\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T120000Z\r\nSUMMARY:Changed event\r\nEND:VEVENT\r\nBEGIN:VJOURNAL\r\nUID:journal-1\r\nDTSTART:20261025T100000Z\r\nSUMMARY:New journal\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n";

fn response(status: u16, body: &str, etag: Option<&str>) -> FetchResponse {
    FetchResponse {
        status,
        etag: etag.map(str::to_string),
        last_modified: Some("Thu, 22 Oct 2026 09:00:00 GMT".to_string()),
        content_type: Some("text/calendar; charset=utf-8".to_string()),
        body: body.as_bytes().to_vec(),
    }
}

struct Fixture {
    _dir: TempDir,
    store: Arc<Store>,
    fetcher: Arc<MockFetcher>,
    service: SubscriptionService,
    user: UserName,
}

async fn fixture(responses: Vec<Result<FetchResponse, SubscriptionError>>) -> Fixture {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(
        Store::open_with_options(
            dir.path(),
            StoreOptions {
                fsync: false,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let user = UserName::parse("alice").unwrap();
    store.provision_principal(&user).await.unwrap();
    let fetcher = Arc::new(MockFetcher::new(responses));
    let secrets = Arc::new(
        SecretBox::load(
            Some(&base64::engine::general_purpose::STANDARD.encode([4u8; 32])),
            &dir.path().join("key"),
        )
        .unwrap()
        .0,
    );
    let service = SubscriptionService::new(
        store.clone(),
        fetcher.clone(),
        secrets,
        oxicale::config::SubscriptionsConfig {
            allow_loopback: false,
            allow_private_networks: false,
            ..Default::default()
        },
        Arc::new(oxicale::server::Metrics::default()),
    );
    Fixture {
        _dir: dir,
        store,
        fetcher,
        service,
        user,
    }
}

fn request(url: &str) -> NewSubscription {
    NewSubscription {
        url: url.to_string(),
        display_name: None,
        color: None,
        refresh_interval_seconds: None,
        auth: None,
    }
}

#[tokio::test]
async fn initial_fetch_creates_read_only_collection() {
    let f = fixture(vec![Ok(response(200, REMOTE, Some("\"v1\"")))]).await;
    let meta = f
        .service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    assert_eq!(meta.display_name, "Team Calendar");
    assert_eq!(meta.description.as_deref(), Some("Shared calendar"));
    assert_eq!(meta.color.as_deref(), Some("#10b981"));
    assert!(meta.is_read_only());
    let path = CollectionPath::parse("alice", "team").unwrap();
    let items = f.store.list_items(&path).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|item| item.uid == "event-1"));
    assert!(items.iter().any(|item| item.uid == "todo-1"));
    let source = meta.subscription.unwrap();
    assert_eq!(source.url, "https://93.184.216.34/team.ics");
    assert_eq!(source.etag.as_deref(), Some("\"v1\""));
    assert!(source.content_hash.is_some());
    assert!(f.fetcher.calls()[0].etag.is_none());
}

#[tokio::test]
async fn initial_failure_does_not_create_collection() {
    let f = fixture(vec![Err(SubscriptionError::Http(500))]).await;
    let result = f
        .service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await;
    assert!(result.is_err());
    assert!(f
        .store
        .list_collections(&f.user)
        .unwrap()
        .iter()
        .all(|collection| collection.slug != "team"));
}

#[tokio::test]
async fn webcal_is_stored_normalized() {
    let f = fixture(vec![Ok(response(200, REMOTE, None))]).await;
    f.service
        .create(&f.user, "team", request("webcal://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let meta = f.store.collection(&path).unwrap();
    let source = meta.subscription.unwrap();
    assert_eq!(source.url, "https://93.184.216.34/team.ics");
    assert_eq!(source.original_url, "webcal://93.184.216.34/team.ics");
}

#[tokio::test]
async fn not_modified_does_not_advance_sync_token() {
    let f = fixture(vec![
        Ok(response(200, REMOTE, Some("\"v1\""))),
        Ok(FetchResponse {
            status: 304,
            etag: Some("\"v1\"".into()),
            last_modified: None,
            content_type: None,
            body: Vec::new(),
        }),
    ])
    .await;
    f.service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let token = f.store.sync_token(&path).unwrap();
    let outcome = f.service.refresh(&path).await.unwrap();
    assert!(matches!(outcome, RefreshOutcome::NotModified));
    assert_eq!(f.store.sync_token(&path).unwrap(), token);
    let call = &f.fetcher.calls()[1];
    assert_eq!(call.etag.as_deref(), Some("\"v1\""));
}

#[tokio::test]
async fn refresh_applies_remote_changes_once() {
    let f = fixture(vec![
        Ok(response(200, REMOTE, Some("\"v1\""))),
        Ok(response(200, REMOTE_UPDATED, Some("\"v2\""))),
    ])
    .await;
    f.service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let token = f.store.sync_token(&path).unwrap();
    let outcome = f.service.refresh(&path).await.unwrap();
    assert!(matches!(outcome, RefreshOutcome::Changed));
    let items = f.store.list_items(&path).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .any(|item| item.uid == "event-1" && item.payload.contains("Changed event")));
    assert!(items.iter().any(|item| item.uid == "journal-1"));
    assert!(!items.iter().any(|item| item.uid == "todo-1"));
    let changes = f.store.changes_since(&path, &token).unwrap();
    assert!(!changes.changes.is_empty());
    let token_after = f.store.sync_token(&path).unwrap();
    let outcome = f.service.refresh(&path).await.unwrap();
    assert!(matches!(outcome, RefreshOutcome::Degraded(_)));
    // A failed refresh preserves content.
    assert_eq!(f.store.list_items(&path).unwrap().len(), 2);
    assert_eq!(f.store.sync_token(&path).unwrap(), token_after);
}

#[tokio::test]
async fn refresh_failure_preserves_last_good_data() {
    let f = fixture(vec![
        Ok(response(200, REMOTE, Some("\"v1\""))),
        Ok(response(500, "", None)),
    ])
    .await;
    f.service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let token = f.store.sync_token(&path).unwrap();
    let outcome = f.service.refresh(&path).await.unwrap();
    match outcome {
        RefreshOutcome::Degraded(message) => assert!(message.contains("500")),
        other => panic!("expected degraded, got {other:?}"),
    }
    assert_eq!(f.store.list_items(&path).unwrap().len(), 2);
    assert_eq!(f.store.sync_token(&path).unwrap(), token);
    let meta = f.store.collection(&path).unwrap();
    let source = meta.subscription.unwrap();
    assert!(source.last_error.is_some());
    assert_eq!(source.consecutive_failures, 1);
    assert!(source.next_attempt_at.is_some());
}

#[tokio::test]
async fn invalid_calendar_preserves_data() {
    let f = fixture(vec![
        Ok(response(200, REMOTE, Some("\"v1\""))),
        Ok(response(200, "not a calendar", Some("\"v2\""))),
    ])
    .await;
    f.service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let outcome = f.service.refresh(&path).await.unwrap();
    assert!(matches!(outcome, RefreshOutcome::Degraded(_)));
    assert_eq!(f.store.list_items(&path).unwrap().len(), 2);
    let source = f.store.collection(&path).unwrap().subscription.unwrap();
    assert!(source.last_error.unwrap().contains("valid iCalendar"));
}

#[tokio::test]
async fn credentials_are_encrypted_and_not_returned() {
    let f = fixture(vec![Ok(response(200, REMOTE, Some("\"v1\"")))]).await;
    let mut req = request("https://93.184.216.34/team.ics");
    req.auth = Some(RemoteAuthInput {
        kind: "basic".into(),
        username: Some("alice".into()),
        password: Some("super-secret-password".into()),
        token: None,
    });
    f.service.create(&f.user, "team", req).await.unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let meta = f.store.collection(&path).unwrap();
    let auth = meta.subscription.unwrap().auth.unwrap();
    assert_ne!(auth.secret_ciphertext, "super-secret-password");
    let metadata_file =
        std::fs::read_to_string(f.store.data_dir().join("users/alice/team/.collection.json"))
            .unwrap();
    assert!(!metadata_file.contains("super-secret-password"));
    let call = &f.fetcher.calls()[0];
    assert_eq!(
        call.credentials.as_ref().unwrap().secret,
        "super-secret-password"
    );
}

#[tokio::test]
async fn invalid_urls_and_intervals_are_rejected_before_fetch() {
    let f = fixture(vec![]).await;
    assert!(matches!(
        f.service
            .create(&f.user, "x", request("ftp://93.184.216.34/x.ics"))
            .await,
        Err(SubscriptionError::SchemeDenied)
    ));
    assert!(matches!(
        f.service
            .create(&f.user, "x", request("https://127.0.0.1/x.ics"))
            .await,
        Err(SubscriptionError::BlockedAddress)
    ));
    let mut req = request("https://93.184.216.34/x.ics");
    req.refresh_interval_seconds = Some(60);
    assert!(f.service.create(&f.user, "x", req).await.is_err());
    assert!(f.fetcher.calls().is_empty());
}

#[tokio::test]
async fn backoff_and_next_attempt_are_persisted() {
    let f = fixture(vec![
        Ok(response(200, REMOTE, Some("\"v1\""))),
        Err(SubscriptionError::Unreachable),
    ])
    .await;
    f.service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    let before = Utc::now();
    f.service.refresh(&path).await.unwrap();
    let source = f.store.collection(&path).unwrap().subscription.unwrap();
    let next = source.next_attempt_at.unwrap();
    assert!(next >= before + chrono::Duration::seconds(300));
    assert_eq!(source.consecutive_failures, 1);
}

#[tokio::test]
async fn an_empty_subscription_can_later_receive_items() {
    let empty = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";
    let f = fixture(vec![
        Ok(response(200, empty, Some("\"empty\""))),
        Ok(response(200, REMOTE, Some("\"v2\""))),
    ])
    .await;
    f.service
        .create(&f.user, "team", request("https://93.184.216.34/team.ics"))
        .await
        .unwrap();
    let path = CollectionPath::parse("alice", "team").unwrap();
    assert!(f.store.list_items(&path).unwrap().is_empty());
    let outcome = f.service.refresh(&path).await.unwrap();
    assert!(matches!(outcome, RefreshOutcome::Changed));
    assert_eq!(f.store.list_items(&path).unwrap().len(), 2);
}
