mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::*;
use oxicale_core::{CalendarComponent, ComponentSet};

async fn put(app: &TestApp, path: &str, content: &str) {
    let (status, _, _) = app
        .dav_body(
            "PUT",
            path,
            Some("text/calendar"),
            Body::from(content.to_string()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{path}");
}

fn calendar_id(app: &TestApp) -> String {
    app.state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .find(|collection| collection.kind == oxicale_core::CollectionKind::Calendar)
        .unwrap()
        .id
}

async fn create_feed(app: &TestApp, components: ComponentSet, name: &str) -> (String, String) {
    let created = app
        .state
        .feeds
        .create(&app.username, &calendar_id(app), name, components, None)
        .await
        .unwrap();
    (
        created
            .url
            .strip_prefix("https://example.test")
            .unwrap()
            .to_string(),
        created.token,
    )
}

#[tokio::test]
async fn todo_feed_filters_events_and_journals() {
    let app = TestApp::new();
    app.provision().await;
    put(&app, "/alice/calendar/event.ics", EVENT).await;
    put(&app, "/alice/calendar/todo.ics", TODO).await;
    put(&app, "/alice/calendar/journal.ics", JOURNAL).await;
    let (path, _) = create_feed(
        &app,
        ComponentSet::one(CalendarComponent::VTodo),
        "Tasks only",
    )
    .await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = body_text(&body);
    assert!(text.contains("BEGIN:VTODO"));
    assert!(text.contains("UID:todo-1"));
    assert!(!text.contains("BEGIN:VEVENT"));
    assert!(!text.contains("BEGIN:VJOURNAL"));
    assert!(text.contains("X-WR-CALNAME:Tasks only"));
}

#[tokio::test]
async fn event_and_mixed_feeds() {
    let app = TestApp::new();
    app.provision().await;
    put(&app, "/alice/calendar/event.ics", EVENT).await;
    put(&app, "/alice/calendar/todo.ics", TODO).await;
    put(&app, "/alice/calendar/journal.ics", JOURNAL).await;

    let (path, _) = create_feed(&app, ComponentSet::one(CalendarComponent::VEvent), "Events").await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = body_text(&body);
    assert!(text.contains("BEGIN:VEVENT"));
    assert!(!text.contains("BEGIN:VTODO"));
    assert!(!text.contains("BEGIN:VJOURNAL"));

    let mut mixed = ComponentSet::empty();
    mixed.insert(CalendarComponent::VEvent);
    mixed.insert(CalendarComponent::VJournal);
    let (path, _) = create_feed(&app, mixed, "Events and journal").await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = body_text(&body);
    assert!(text.contains("BEGIN:VEVENT"));
    assert!(text.contains("BEGIN:VJOURNAL"));
    assert!(!text.contains("BEGIN:VTODO"));
}

#[tokio::test]
async fn feed_updates_only_for_included_components() {
    let app = TestApp::new();
    app.provision().await;
    put(&app, "/alice/calendar/todo.ics", TODO).await;
    let (path, _) = create_feed(&app, ComponentSet::one(CalendarComponent::VTodo), "Tasks").await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let etag = response
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let original = body_text(&body);

    put(&app, "/alice/calendar/event.ics", EVENT).await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    assert_eq!(
        response.headers().get("etag").unwrap().to_str().unwrap(),
        etag
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body_text(&body), original);

    let new_todo = TODO
        .replace("UID:todo-1", "UID:todo-2")
        .replace("SUMMARY:Task", "SUMMARY:Second");
    put(&app, "/alice/calendar/todo2.ics", &new_todo).await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let new_etag = response
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(new_etag, etag);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&body).contains("Second"));

    let response = app
        .call(
            Method::GET,
            &path,
            header("If-None-Match", &new_etag),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn feed_preserves_timezones_and_is_deterministic() {
    let app = TestApp::new();
    app.provision().await;
    let event = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:tz-event\r\nDTSTART;TZID=Europe/Paris:20261022T120000\r\nDTEND;TZID=Europe/Paris:20261022T130000\r\nSUMMARY:Paris\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    put(&app, "/alice/calendar/tz.ics", event).await;
    let (path, _) = create_feed(&app, ComponentSet::all(), "All").await;
    let first = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let first_etag = first
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&first_body).contains("BEGIN:VTIMEZONE"));
    assert!(body_text(&first_body).contains("TZID:Europe/Paris"));
    let second = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let second_etag = second
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let second_body = axum::body::to_bytes(second.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(first_body, second_body);
    assert_eq!(first_etag, second_etag);
}

#[tokio::test]
async fn feed_head_has_headers_without_body() {
    let app = TestApp::new();
    app.provision().await;
    put(&app, "/alice/calendar/todo.ics", TODO).await;
    let (path, _) = create_feed(&app, ComponentSet::one(CalendarComponent::VTodo), "Tasks").await;
    let response = app
        .call(Method::HEAD, &path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("content-length"));
    assert!(response.headers().contains_key("etag"));
    assert_eq!(
        response.headers().get("referrer-policy").unwrap(),
        "no-referrer"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn feed_from_subscription_collection() {
    let app = TestApp::new();
    app.provision().await;
    let aggregate = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:re\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Remote\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:rt\r\nDUE:20261024T100000Z\r\nSUMMARY:Remote task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
    let items =
        oxicale_ical::split_calendar(aggregate, oxicale_ical::Limits::default(), chrono_tz::UTC)
            .unwrap();
    app.state
        .store
        .create_subscription_collection(
            &app.username,
            "remote",
            "Remote",
            None,
            oxicale_storage::SubscriptionSource {
                url: "https://93.184.216.34/remote.ics".into(),
                original_url: "https://93.184.216.34/remote.ics".into(),
                ..Default::default()
            },
            &items,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let collection = app
        .state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .find(|collection| collection.slug == "remote")
        .unwrap();
    let created = app
        .state
        .feeds
        .create(
            &app.username,
            &collection.id,
            "Remote tasks",
            ComponentSet::one(CalendarComponent::VTodo),
            None,
        )
        .await
        .unwrap();
    let path = created.url.strip_prefix("https://example.test").unwrap();
    let response = app
        .call(Method::GET, path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = body_text(&body);
    assert!(text.contains("BEGIN:VTODO"));
    assert!(!text.contains("BEGIN:VEVENT"));
}

#[tokio::test]
async fn feed_rate_limit_applies() {
    let app = TestApp::with_config(|config| {
        config.feeds.rate_limit_per_minute = 1;
    });
    app.provision().await;
    put(&app, "/alice/calendar/todo.ics", TODO).await;
    let (path, _) = create_feed(&app, ComponentSet::one(CalendarComponent::VTodo), "Tasks").await;
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn unknown_and_revoked_tokens_are_indistinguishable() {
    let app = TestApp::new();
    app.provision().await;
    let (path, _) = create_feed(&app, ComponentSet::all(), "All").await;
    let created = app
        .state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .find(|c| c.kind == oxicale_core::CollectionKind::Calendar)
        .unwrap();
    let feed_id = created.feeds[0].id.clone();
    let response = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    app.state
        .feeds
        .revoke(&app.username, &created.id, &feed_id)
        .await
        .unwrap();
    let revoked = app
        .call(Method::GET, &path, Default::default(), Body::empty())
        .await;
    let unknown = app
        .call(
            Method::GET,
            "/feeds/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            Default::default(),
            Body::empty(),
        )
        .await;
    assert_eq!(revoked.status(), StatusCode::NOT_FOUND);
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let revoked_body = axum::body::to_bytes(revoked.into_body(), usize::MAX)
        .await
        .unwrap();
    let unknown_body = axum::body::to_bytes(unknown.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(revoked_body, unknown_body);
}
