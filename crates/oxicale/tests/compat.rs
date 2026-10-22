mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::*;
use oxicale_core::{CalendarComponent, ComponentSet};
use oxicale_storage::SubscriptionSource;

const RECURRING: &str = include_str!("fixtures/client/recurring.ics");
const AGGREGATE: &str = include_str!("fixtures/client/aggregate.ics");

async fn dav_propfind(app: &TestApp, path: &str, depth: &str, body: &str) -> (StatusCode, String) {
    let headers = header("Depth", depth);
    let response = app
        .dav("PROPFIND", path, headers, Body::from(body.to_string()))
        .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body_text(&body))
}

#[tokio::test]
async fn discovery_and_mixed_calendar_contract() {
    let app = TestApp::new();
    app.provision().await;

    let (status, body) = dav_propfind(
        &app,
        "/",
        "0",
        r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:prop><D:current-user-principal/><D:principal-collection-set/></D:prop></D:propfind>"#,
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("/alice/"));
    assert!(body.contains("principal-collection-set"));

    let (status, body) = dav_propfind(
        &app,
        "/alice/",
        "0",
        r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CR="urn:ietf:params:xml:ns:carddav"><D:prop><D:principal-URL/><C:calendar-home-set/><CR:addressbook-home-set/></D:prop></D:propfind>"#,
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("calendar-home-set"));
    assert!(body.contains("addressbook-home-set"));

    let (status, body) = dav_propfind(
        &app,
        "/alice/",
        "1",
        r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:prop><D:displayname/><D:resourcetype/></D:prop></D:propfind>"#,
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("/alice/calendar/"));
    assert!(body.contains("/alice/contacts/"));

    let mkcalendar = r#"<?xml version="1.0"?><C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:set><D:prop><D:displayname>Tasks</D:displayname><C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set></D:prop></D:set></C:mkcalendar>"#;
    let (status, _, _) = app
        .dav_body(
            "MKCALENDAR",
            "/alice/tasks/",
            Some("application/xml"),
            Body::from(mkcalendar),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = dav_propfind(
        &app,
        "/alice/tasks/",
        "0",
        r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><C:supported-calendar-component-set/></D:prop></D:propfind>"#,
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    for component in ["VEVENT", "VTODO", "VJOURNAL"] {
        assert!(body.contains(&format!("name=\"{component}\"")), "{body}");
    }
}

#[tokio::test]
async fn calendar_crud_query_and_sync_contract() {
    let app = TestApp::new();
    app.provision().await;

    for (href, payload) in [
        ("event.ics", EVENT),
        ("todo.ics", TODO),
        ("journal.ics", JOURNAL),
        ("recurring.ics", RECURRING),
    ] {
        let (status, _, _) = app
            .dav_body(
                "PUT",
                &format!("/alice/calendar/{href}"),
                Some("text/calendar"),
                Body::from(payload),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{href}");
    }

    let query = r#"<?xml version="1.0"?><C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/><C:calendar-data/></D:prop><C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"><C:time-range start="20261011T000000Z" end="20261012T000000Z"/></C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(query),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let text = body_text(&body);
    assert!(text.contains("recurring.ics"));
    assert!(!text.contains("todo.ics</D:href>"));

    let (status, headers, _) = app
        .dav_body("GET", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    let etag = headers.get(axum::http::header::ETAG).unwrap().clone();
    let response = app
        .dav(
            "PUT",
            "/alice/calendar/event.ics",
            {
                let mut headers = header("If-Match", etag.to_str().unwrap());
                headers.insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/calendar"),
                );
                headers
            },
            Body::from(EVENT.replace("Team Meeting", "Updated")),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .dav(
            "MOVE",
            "/alice/calendar/event.ics",
            header(
                "Destination",
                "https://example.test/alice/calendar/moved.ics",
            ),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let sync = r#"<D:sync-collection xmlns:D="DAV:"><D:sync-token/><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(sync),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let text = body_text(&body);
    assert!(text.contains("moved.ics"));
    let token = text
        .split("<D:sync-token>")
        .nth(1)
        .unwrap()
        .split("</D:sync-token>")
        .next()
        .unwrap()
        .to_string();

    let (status, _, _) = app
        .dav_body("DELETE", "/alice/calendar/moved.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let incremental = format!(
        r#"<D:sync-collection xmlns:D="DAV:"><D:sync-token>{token}</D:sync-token><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#
    );
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(incremental),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains("404 Not Found"));
}

#[tokio::test]
async fn carddav_contract() {
    let app = TestApp::new();
    app.provision().await;

    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/contacts/card.vcf",
            Some("text/vcard"),
            Body::from(VCARD),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let query = r#"<?xml version="1.0"?><CR:addressbook-query xmlns:D="DAV:" xmlns:CR="urn:ietf:params:xml:ns:carddav"><D:prop><D:getetag/><CR:address-data/></D:prop><CR:filter test="allof"><CR:prop-filter name="EMAIL"><CR:text-match>example.com</CR:text-match></CR:prop-filter></CR:filter></CR:addressbook-query>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/contacts/",
            Some("application/xml"),
            Body::from(query),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains("card.vcf"));

    let multiget = r#"<CR:addressbook-multiget xmlns:D="DAV:" xmlns:CR="urn:ietf:params:xml:ns:carddav"><D:prop><D:getetag/><CR:address-data/></D:prop><D:href>/alice/contacts/card.vcf</D:href></CR:addressbook-multiget>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/contacts/",
            Some("application/xml"),
            Body::from(multiget),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains("UID:card-1"));
}

#[tokio::test]
async fn subscription_and_feed_contract() {
    let app = TestApp::new();
    app.provision().await;

    let items =
        oxicale_ical::split_calendar(AGGREGATE, oxicale_ical::Limits::default(), chrono_tz::UTC)
            .unwrap();
    let source = SubscriptionSource {
        url: "https://93.184.216.34/team.ics".into(),
        original_url: "webcal://93.184.216.34/team.ics".into(),
        ..Default::default()
    };
    app.state
        .store
        .create_subscription_collection(
            &app.username,
            "team",
            "Team",
            None,
            source,
            &items,
            Some("\"v1\"".into()),
            None,
            Some("Remote".into()),
            None,
            None,
            Some("hash".into()),
        )
        .await
        .unwrap();

    let (status, body) = dav_propfind(
        &app,
        "/alice/team/",
        "0",
        r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/"><D:prop><D:resourcetype/><CS:source/></D:prop></D:propfind>"#,
    )
    .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("<CS:subscribed/>"));
    assert!(body.contains("<CS:source>"));
    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/team/new.ics",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/todo.ics",
            Some("text/calendar"),
            Body::from(TODO),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let calendar_id = app
        .state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .find(|collection| collection.slug == "calendar")
        .unwrap()
        .id;
    let created = app
        .state
        .feeds
        .create(
            &app.username,
            &calendar_id,
            "Tasks",
            ComponentSet::one(CalendarComponent::VTodo),
            None,
        )
        .await
        .unwrap();
    let feed_path = created
        .url
        .strip_prefix("https://example.test")
        .unwrap()
        .to_string();
    let response = app
        .call(Method::GET, &feed_path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response
        .headers()
        .get(axum::http::header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&body).contains("UID:todo-1"));

    let response = app
        .call(
            Method::GET,
            &feed_path,
            header("If-None-Match", &etag),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/todo2.ics",
            Some("text/calendar"),
            Body::from(TODO.replace("todo-1", "todo-2")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let response = app
        .call(
            Method::GET,
            &feed_path,
            header("If-None-Match", &etag),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unsupported_features_fail_explicitly() {
    let app = TestApp::new();
    app.provision().await;

    let unsupported = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VFREEBUSY\r\nUID:freebusy-1\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n";
    let (status, _, body) = app
        .dav_body(
            "PUT",
            "/alice/calendar/freebusy.ics",
            Some("text/calendar"),
            Body::from(unsupported),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(body_text(&body).contains("supported-calendar-data"));

    let query = r#"<?xml version="1.0"?><C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/><C:calendar-data><C:expand start="20261001T000000Z" end="20261031T000000Z"/></C:calendar-data></D:prop><C:filter><C:comp-filter name="VCALENDAR"/></C:filter></C:calendar-query>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(query),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body_text(&body).contains("supported-calendar-data"));
}
