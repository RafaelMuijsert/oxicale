mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::Value;

const PROPFIND_PROPS: &str = r#"<?xml version="1.0"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CR="urn:ietf:params:xml:ns:carddav">
<D:prop><D:resourcetype/><D:displayname/><D:getetag/><D:current-user-principal/><D:principal-URL/><C:calendar-home-set/><C:supported-calendar-component-set/><CR:addressbook-home-set/><D:sync-token/><CS:getctag xmlns:CS="http://calendarserver.org/ns/"/><CS:source xmlns:CS="http://calendarserver.org/ns/"/></D:prop></D:propfind>"#;

async fn propfind(app: &TestApp, path: &str, depth: &str) -> (StatusCode, String) {
    let mut headers = header("Depth", depth);
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/xml"),
    );
    let response = app
        .dav("PROPFIND", path, headers, Body::from(PROPFIND_PROPS))
        .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body_text(&body))
}

#[tokio::test]
async fn options_and_authentication() {
    let app = TestApp::new();
    let response = app
        .call(Method::OPTIONS, "/", Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let dav_header = response.headers().get("dav").unwrap().to_str().unwrap();
    assert!(dav_header.contains("calendar-access"));
    assert!(dav_header.contains("addressbook"));
    assert!(!dav_header.contains('2'));

    let response = app
        .call(
            Method::from_bytes(b"PROPFIND").unwrap(),
            "/",
            header("Depth", "0"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response
        .headers()
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("Basic"));
}

#[tokio::test]
async fn discovery_provisions_defaults_and_lists_collections() {
    let app = TestApp::new();
    let (status, body) = propfind(&app, "/", "0").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("/alice/"));

    let (status, body) = propfind(&app, "/alice/", "0").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("calendar-home-set"));
    assert!(body.contains("addressbook-home-set"));

    let (status, body) = propfind(&app, "/alice/", "1").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("/alice/calendar/"));
    assert!(body.contains("/alice/contacts/"));

    let (status, body) = propfind(&app, "/alice/calendar/", "0").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("<C:calendar/>"));
    assert!(body.contains("name=\"VEVENT\""));
    assert!(body.contains("name=\"VTODO\""));
    assert!(body.contains("name=\"VJOURNAL\""));
    assert!(body.contains("getctag"));
    assert!(body.contains("sync-token"));

    let collections = app.state.store.list_collections(&app.username).unwrap();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0].display_name, "Calendar");
}

#[tokio::test]
async fn propfind_depth_infinity_is_rejected() {
    let app = TestApp::new();
    let (status, _) = propfind(&app, "/alice/", "infinity").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn mixed_calendar_accepts_all_component_types() {
    let app = TestApp::new();
    app.provision().await;
    for (href, payload) in [
        ("event.ics", EVENT),
        ("todo.ics", TODO),
        ("journal.ics", JOURNAL),
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
    let (status, body) = propfind(&app, "/alice/calendar/", "0").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("name=\"VEVENT\""));
}

#[tokio::test]
async fn mkcalendar_with_narrow_component_set_is_normalised() {
    let app = TestApp::new();
    app.provision().await;
    let body = r#"<?xml version="1.0"?>
<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<D:set><D:prop><D:displayname>Tasks</D:displayname>
<C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set>
</D:prop></D:set></C:mkcalendar>"#;
    let (status, _, _) = app
        .dav_body(
            "MKCALENDAR",
            "/alice/tasks/",
            Some("application/xml"),
            Body::from(body),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let meta = app
        .state
        .store
        .collection(&oxicale_core::CollectionPath::parse("alice", "tasks").unwrap())
        .unwrap();
    assert_eq!(meta.components, oxicale_core::ComponentSet::all());
    let (status, body) = propfind(&app, "/alice/tasks/", "0").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("name=\"VEVENT\""));
    assert!(body.contains("name=\"VTODO\""));
    assert!(body.contains("name=\"VJOURNAL\""));

    for (href, payload) in [
        ("event.ics", EVENT),
        ("todo.ics", TODO),
        ("journal.ics", JOURNAL),
    ] {
        let (status, _, _) = app
            .dav_body(
                "PUT",
                &format!("/alice/tasks/{href}"),
                Some("text/calendar"),
                Body::from(payload),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let bad = r#"<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:set><D:prop><C:supported-calendar-component-set><C:comp name="VFREEBUSY"/></C:supported-calendar-component-set></D:prop></D:set></C:mkcalendar>"#;
    let (status, _, _) = app
        .dav_body(
            "MKCALENDAR",
            "/alice/bad/",
            Some("application/xml"),
            Body::from(bad),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn put_get_head_update_delete_and_conditionals() {
    let app = TestApp::new();
    app.provision().await;
    let (status, headers, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/event.ics",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_string();

    let (status, headers, body) = app
        .dav_body("GET", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_text(&body), EVENT);
    assert_eq!(headers.get("etag").unwrap().to_str().unwrap(), etag);
    assert!(headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("component=VEVENT"));

    let (status, headers, body) = app
        .dav_body("HEAD", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert!(headers.contains_key("content-length"));

    let (status, _, body) = app
        .dav_body(
            "GET",
            "/alice/calendar/event.ics",
            Some("text/plain"),
            Body::empty(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty());

    let response = app
        .dav(
            "GET",
            "/alice/calendar/event.ics",
            header("If-None-Match", &etag),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let updated = EVENT.replace("Team Meeting", "Updated Meeting");
    let (status, headers, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/event.ics",
            Some("text/calendar"),
            Body::from(updated.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let new_etag = headers.get("etag").unwrap().to_str().unwrap();
    assert_ne!(new_etag, etag);

    let (status, _, body) = app
        .dav_body("GET", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_text(&body).contains("Updated Meeting"));

    let (status, _, _) = app
        .dav_body("DELETE", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = app
        .dav_body("GET", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uid_conflicts_and_preconditions() {
    let app = TestApp::new();
    app.provision().await;
    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/a.ics",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, body) = app
        .dav_body(
            "PUT",
            "/alice/calendar/b.ics",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body_text(&body).contains("no-uid-conflict"));

    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/a.ics",
            Some("text/calendar"),
            Body::from(EVENT.replace("Team Meeting", "Nope")),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let fresh_etag = app
        .state
        .store
        .get_item(
            &oxicale_core::CollectionPath::parse("alice", "calendar").unwrap(),
            &oxicale_core::Href::parse("a.ics").unwrap(),
        )
        .unwrap()
        .etag;

    let wrong = header("If-Match", "\"deadbeef\"");
    let response = app
        .dav(
            "PUT",
            "/alice/calendar/a.ics",
            {
                let mut headers = wrong;
                headers.insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/calendar"),
                );
                headers
            },
            Body::from(EVENT),
        )
        .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    let response = app
        .dav(
            "PUT",
            "/alice/calendar/a.ics",
            {
                let mut headers = header("If-Match", &format!("\"{fresh_etag}\""));
                headers.insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/calendar"),
                );
                headers
            },
            Body::from(EVENT.replace("Team Meeting", "Matched")),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn invalid_data_and_content_types_are_rejected() {
    let app = TestApp::new();
    app.provision().await;
    let (status, _, body) = app
        .dav_body(
            "PUT",
            "/alice/calendar/bad.ics",
            Some("text/calendar"),
            Body::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body_text(&body).contains("valid-calendar-data"));

    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/bad.ics",
            Some("application/json"),
            Body::from("{}"),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn proppatch_updates_metadata_and_rejects_resourcetype() {
    let app = TestApp::new();
    app.provision().await;
    let body = r#"<?xml version="1.0"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:I="http://apple.com/ns/ical/">
<D:set><D:prop><D:displayname>Family</D:displayname><I:calendar-color>#ff0000</I:calendar-color><C:calendar-description>Shared</C:calendar-description></D:prop></D:set>
</D:propertyupdate>"#;
    let (status, _, body) = app
        .dav_body(
            "PROPPATCH",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(body),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains("200 OK"));
    let meta = app
        .state
        .store
        .collection(&oxicale_core::CollectionPath::parse("alice", "calendar").unwrap())
        .unwrap();
    assert_eq!(meta.display_name, "Family");
    assert_eq!(meta.color.as_deref(), Some("#ff0000"));
    assert_eq!(meta.description.as_deref(), Some("Shared"));

    let denied = r#"<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:set></D:propertyupdate>"#;
    let (status, _, body) = app
        .dav_body(
            "PROPPATCH",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(denied),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains("403 Forbidden"));
}

#[tokio::test]
async fn move_item_and_collection() {
    let app = TestApp::new();
    app.provision().await;
    app.dav_body(
        "PUT",
        "/alice/calendar/event.ics",
        Some("text/calendar"),
        Body::from(EVENT),
    )
    .await;
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
    let (status, _, _) = app
        .dav_body("GET", "/alice/calendar/event.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = app
        .dav_body("GET", "/alice/calendar/moved.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);

    let response = app
        .dav(
            "MOVE",
            "/alice/calendar/",
            header("Destination", "https://example.test/alice/renamed/"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let (status, _, _) = app
        .dav_body("GET", "/alice/renamed/moved.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn calendar_query_multiget_and_freebusy() {
    let app = TestApp::new();
    app.provision().await;
    for (href, payload) in [
        ("event.ics", EVENT),
        ("event2.ics", EVENT_2),
        ("todo.ics", TODO),
    ] {
        app.dav_body(
            "PUT",
            &format!("/alice/calendar/{href}"),
            Some("text/calendar"),
            Body::from(payload),
        )
        .await;
    }
    let query = r#"<?xml version="1.0"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<D:prop><D:getetag/><C:calendar-data/></D:prop>
<C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT">
<C:time-range start="20261022T000000Z" end="20261023T000000Z"/>
<C:prop-filter name="SUMMARY"><C:text-match>meeting</C:text-match></C:prop-filter>
</C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#;
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
    assert!(text.contains("event.ics"));
    assert!(!text.contains("todo.ics"));
    assert!(!text.contains("event2.ics"));

    let multiget = r#"<?xml version="1.0"?>
<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<D:prop><D:getetag/><C:calendar-data/></D:prop>
<D:href>/alice/calendar/event.ics</D:href>
<D:href>/alice/calendar/event.ics</D:href>
<D:href>/alice/calendar/missing.ics</D:href>
</C:calendar-multiget>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(multiget),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let text = body_text(&body);
    assert_eq!(text.matches("event.ics</D:href>").count(), 1);
    assert!(text.contains("404 Not Found"));

    let freebusy = r#"<?xml version="1.0"?>
<C:free-busy-query xmlns:C="urn:ietf:params:xml:ns:caldav"><C:time-range start="20261022T000000Z" end="20261023T000000Z"/></C:free-busy-query>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(freebusy),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let text = body_text(&body);
    assert!(text.contains("BEGIN:VFREEBUSY"));
    assert!(text.contains("FREEBUSY;FBTYPE=BUSY:20261022T100000Z/20261022T110000Z"));
    assert!(!text.contains("SUMMARY"));
}

#[tokio::test]
async fn sync_collection_full_and_incremental() {
    let app = TestApp::new();
    app.provision().await;
    app.dav_body(
        "PUT",
        "/alice/calendar/event.ics",
        Some("text/calendar"),
        Body::from(EVENT),
    )
    .await;
    let sync = r#"<?xml version="1.0"?>
<D:sync-collection xmlns:D="DAV:"><D:sync-token/><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#;
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
    assert!(text.contains("event.ics"));
    let token = text
        .split("<D:sync-token>")
        .nth(1)
        .unwrap()
        .split("</D:sync-token>")
        .next()
        .unwrap()
        .to_string();

    let incremental = format!(
        r#"<D:sync-collection xmlns:D="DAV:"><D:sync-token>{token}</D:sync-token><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#
    );
    app.dav_body(
        "PUT",
        "/alice/calendar/event2.ics",
        Some("text/calendar"),
        Body::from(EVENT_2),
    )
    .await;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(incremental.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let text = body_text(&body);
    assert!(text.contains("event2.ics"));
    assert!(!text.contains("event.ics</D:href>"));
    assert_ne!(text.split("<D:sync-token>").nth(1).unwrap(), token);

    let invalid = incremental.replace(&token, "not-a-token");
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(invalid),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body_text(&body).contains("valid-sync-token"));

    let (status, _, _) = app
        .dav_body("DELETE", "/alice/calendar/event2.ics", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let new_token = text
        .split("<D:sync-token>")
        .nth(1)
        .unwrap()
        .split("</D:sync-token>")
        .next()
        .unwrap()
        .to_string();
    let deletion = format!(
        r#"<D:sync-collection xmlns:D="DAV:"><D:sync-token>{new_token}</D:sync-token><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#
    );
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/calendar/",
            Some("application/xml"),
            Body::from(deletion),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains("404 Not Found"));
}

#[tokio::test]
async fn carddav_crud_query_and_multiget() {
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
    let (status, _, body) = app
        .dav_body("GET", "/alice/contacts/card.vcf", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_text(&body), VCARD);

    let query = r#"<?xml version="1.0"?>
<CR:addressbook-query xmlns:D="DAV:" xmlns:CR="urn:ietf:params:xml:ns:carddav">
<D:prop><D:getetag/><CR:address-data/></D:prop>
<CR:filter test="anyof"><CR:prop-filter name="EMAIL"><CR:text-match>example.com</CR:text-match></CR:prop-filter></CR:filter>
</CR:addressbook-query>"#;
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
    assert!(body_text(&body).contains("Alice Example"));

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

    let (status, _, _) = app
        .dav_body("DELETE", "/alice/contacts/card.vcf", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn subscription_is_discovered_read_only_and_syncs() {
    let app = TestApp::new();
    app.provision().await;
    let aggregate = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Remote\r\nBEGIN:VEVENT\r\nUID:remote-event\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Remote\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:remote-task\r\nDUE:20261024T100000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
    let items =
        oxicale_ical::split_calendar(aggregate, oxicale_ical::Limits::default(), chrono_tz::UTC)
            .unwrap();
    let source = oxicale_storage::SubscriptionSource {
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

    let (status, body) = propfind(&app, "/alice/team/", "0").await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body.contains("<C:calendar/>"));
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
            "DELETE",
            "/alice/team/remote-event.ics",
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let remote_href = app
        .state
        .store
        .list_items(&oxicale_core::CollectionPath::parse("alice", "team").unwrap())
        .unwrap()
        .into_iter()
        .find(|item| item.uid == "remote-event")
        .unwrap()
        .href
        .to_string();
    let (status, _, body) = app
        .dav_body(
            "GET",
            &format!("/alice/team/{remote_href}"),
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_text(&body).contains("SUMMARY:Remote"));

    let sync = r#"<D:sync-collection xmlns:D="DAV:"><D:sync-token/><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#;
    let (status, _, body) = app
        .dav_body(
            "REPORT",
            "/alice/team/",
            Some("application/xml"),
            Body::from(sync),
        )
        .await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    assert!(body_text(&body).contains(&remote_href));
}

#[tokio::test]
async fn path_traversal_and_xml_hardening() {
    let app = TestApp::new();
    app.provision().await;
    for path in [
        "/alice/%2e%2e/secret",
        "/alice/calendar/%2fetc/passwd",
        "/alice/calendar/%00x",
    ] {
        let (status, _, _) = app.dav_body("GET", path, None, Body::empty()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    let dtd = r#"<!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><D:propfind xmlns:D="DAV:"><D:prop><D:displayname/></D:prop></D:propfind>"#;
    let mut dtd_headers = header("Depth", "0");
    dtd_headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/xml"),
    );
    let response = app
        .dav("PROPFIND", "/alice/calendar/", dtd_headers, Body::from(dtd))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let malformed = "<D:propfind><D:prop></D:propfind>";
    let mut malformed_headers = header("Depth", "0");
    malformed_headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/xml"),
    );
    let response = app
        .dav(
            "PROPFIND",
            "/alice/calendar/",
            malformed_headers,
            Body::from(malformed),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ownership_is_enforced() {
    let app = TestApp::new();
    app.provision().await;
    let bob = oxicale_core::UserName::parse("bob").unwrap();
    app.state.store.provision_principal(&bob).await.unwrap();
    let (status, _, _) = app.dav_body("GET", "/bob/", None, Body::empty()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/bob/calendar/x.ics",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn collection_get_export_is_aggregate() {
    let app = TestApp::new();
    app.provision().await;
    for (href, payload) in [("e.ics", EVENT), ("t.ics", TODO), ("j.ics", JOURNAL)] {
        app.dav_body(
            "PUT",
            &format!("/alice/calendar/{href}"),
            Some("text/calendar"),
            Body::from(payload),
        )
        .await;
    }
    let (status, headers, body) = app
        .dav_body("GET", "/alice/calendar/", None, Body::empty())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/calendar"));
    let text = body_text(&body);
    assert!(text.contains("BEGIN:VEVENT"));
    assert!(text.contains("BEGIN:VTODO"));
    assert!(text.contains("BEGIN:VJOURNAL"));
}

#[tokio::test]
async fn json_api_crud_sessions_and_feeds() {
    let app = TestApp::new();
    let response = app
        .call(
            Method::POST,
            "/.oxicale/api/v1/session",
            header("Content-Type", "application/json"),
            Body::from(r#"{"username":"alice","password":"correct horse"}"#),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    let cookie = cookie_from(&headers).unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let csrf = json["csrf"].as_str().unwrap().to_string();

    let response = app
        .call(
            Method::GET,
            "/.oxicale/api/v1/collections",
            header("Cookie", &cookie),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let calendar_id = json["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["kind"] == "calendar")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut create_headers = header("Content-Type", "application/json");
    create_headers.insert("X-CSRF-Token", csrf.parse().unwrap());
    create_headers.insert(axum::http::header::COOKIE, cookie.parse().unwrap());
    let response = app
        .call(
            Method::POST,
            "/.oxicale/api/v1/collections",
            create_headers.clone(),
            Body::from(r#"{"kind":"calendar","slug":"family","display_name":"Family"}"#),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .call(
            Method::POST,
            "/.oxicale/api/v1/collections",
            create_headers.clone(),
            Body::from(r#"{"kind":"calendar","display_name":"Typed","components":["VTODO"]}"#),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = app
        .call(
            Method::POST,
            &format!("/.oxicale/api/v1/collections/{calendar_id}/feeds"),
            create_headers.clone(),
            Body::from(r#"{"name":"Tasks only","components":["VTODO"]}"#),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let feed: Value = serde_json::from_slice(&body).unwrap();
    let url = feed["url"].as_str().unwrap().to_string();
    let feed_id = feed["id"].as_str().unwrap().to_string();
    let path = url.strip_prefix("https://example.test").unwrap();

    let response = app
        .call(Method::GET, path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
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
    assert!(body_text(&body).contains("BEGIN:VCALENDAR"));
    assert!(!body_text(&body).contains("BEGIN:VEVENT"));

    let response = app
        .call(
            Method::GET,
            path,
            header("If-None-Match", &etag),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let response = app
        .call(
            Method::POST,
            &format!("/.oxicale/api/v1/collections/{calendar_id}/feeds/{feed_id}/rotate"),
            create_headers.clone(),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let rotated: Value = serde_json::from_slice(&body).unwrap();
    let new_url = rotated["url"].as_str().unwrap().to_string();
    let old_path = path.to_string();
    let new_path = new_url.strip_prefix("https://example.test").unwrap();
    let response = app
        .call(Method::GET, &old_path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .call(Method::GET, new_path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .call(
            Method::DELETE,
            &format!("/.oxicale/api/v1/collections/{calendar_id}/feeds/{feed_id}"),
            create_headers,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .call(Method::GET, new_path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn web_login_dashboard_and_csrf() {
    let app = TestApp::new();
    let response = app
        .call(
            Method::GET,
            "/.oxicale/login",
            Default::default(),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&body).contains("Sign in"));

    let response = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=correct+horse"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = cookie_from(response.headers()).unwrap();

    let response = app
        .call(
            Method::GET,
            "/.oxicale/dashboard",
            header("Cookie", &cookie),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = body_text(&body);
    assert!(text.contains("Calendar"));
    assert!(!text.contains("calendar type"));
    assert!(text.contains("Connect a client"));

    let response = app
        .call(
            Method::POST,
            "/.oxicale/calendar/new",
            {
                let mut headers = header("Content-Type", "application/x-www-form-urlencoded");
                headers.insert(axum::http::header::COOKIE, cookie.parse().unwrap());
                headers
            },
            Body::from("name=NoCSRF"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let session_id = cookie.split_once('=').unwrap().1.to_string();
    let csrf = app.state.sessions.get(&session_id).unwrap().csrf;
    let response = app
        .call(
            Method::POST,
            "/.oxicale/calendar/new",
            {
                let mut headers = header("Content-Type", "application/x-www-form-urlencoded");
                headers.insert(axum::http::header::COOKIE, cookie.parse().unwrap());
                headers
            },
            Body::from(format!("name=Created+from+web&csrf={csrf}")),
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let names: Vec<String> = app
        .state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .map(|collection| collection.display_name)
        .collect();
    assert!(names.contains(&"Created from web".to_string()));

    let response = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=wrong"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&body).contains("Invalid username or password"));
}

#[tokio::test]
async fn feed_unknown_expired_and_head_are_safe() {
    let app = TestApp::new();
    app.provision().await;
    let collection = app
        .state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .find(|c| c.kind == oxicale_core::CollectionKind::Calendar)
        .unwrap();
    let created = app
        .state
        .feeds
        .create(
            &app.username,
            &collection.id,
            "Expired",
            oxicale_core::ComponentSet::all(),
            Some(chrono::Utc::now() - chrono::Duration::days(1)),
        )
        .await
        .unwrap();
    let path = created.url.strip_prefix("https://example.test").unwrap();
    let response = app
        .call(Method::GET, path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .call(
            Method::GET,
            "/feeds/not-a-real-token",
            Default::default(),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .call(Method::PUT, path, Default::default(), Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
