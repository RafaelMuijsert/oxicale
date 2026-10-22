mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::*;
use oxicale_core::{CalendarComponent, ComponentSet};

#[tokio::test]
async fn oversized_requests_are_rejected() {
    let app = TestApp::with_config(|config| {
        config.server.max_request_body = oxicale::config::ByteSize(1024);
    });
    app.provision().await;
    let huge = EVENT.to_string() + &"X".repeat(5_000);
    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/huge.ics",
            Some("text/calendar"),
            Body::from(huge),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn oversized_items_are_rejected() {
    let app = TestApp::with_config(|config| {
        config.server.max_item_size = oxicale::config::ByteSize(64);
    });
    app.provision().await;
    let (status, _, _) = app
        .dav_body(
            "PUT",
            "/alice/calendar/event.ics",
            Some("text/calendar"),
            Body::from(EVENT),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn session_cookie_has_security_flags() {
    let app = TestApp::new();
    let response = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=correct+horse"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Path=/"));
}

#[tokio::test]
async fn api_accepts_basic_auth_and_provisions() {
    let app = TestApp::new();
    let response = app
        .call(
            Method::GET,
            "/.oxicale/api/v1/collections",
            header("Authorization", &app.basic()),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["collections"].as_array().unwrap().len(), 2);

    let response = app
        .call(
            Method::POST,
            "/.oxicale/api/v1/collections",
            {
                let mut headers = header("Authorization", &app.basic());
                headers.insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("application/json"),
                );
                headers
            },
            Body::from(r#"{"kind":"calendar","slug":"basic","display_name":"Basic"}"#),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn login_rate_limiting_and_enumeration_resistance() {
    let app = TestApp::with_config(|config| {
        config.auth.rate_limit_failures = 2;
        config.auth.rate_limit_window = oxicale::config::DurationSpec(300);
    });
    let wrong = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=wrong"),
        )
        .await;
    let wrong_body = axum::body::to_bytes(wrong.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&wrong_body).contains("Invalid username or password"));

    let unknown = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=nobody&password=wrong"),
        )
        .await;
    let unknown_body = axum::body::to_bytes(unknown.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&unknown_body).contains("Invalid username or password"));

    app.call(
        Method::POST,
        "/.oxicale/login",
        header("Content-Type", "application/x-www-form-urlencoded"),
        Body::from("username=alice&password=wrong"),
    )
    .await;
    let response = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=correct+horse"),
        )
        .await;
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body_text(&body).contains("Too many attempts"));
    assert!(!body_text(&body).contains("correct horse"));
}

#[tokio::test]
async fn feed_tokens_are_not_rendered_in_the_dashboard() {
    let app = TestApp::new();
    app.provision().await;
    let collection = app
        .state
        .store
        .list_collections(&app.username)
        .unwrap()
        .into_iter()
        .find(|collection| collection.kind == oxicale_core::CollectionKind::Calendar)
        .unwrap();
    let created = app
        .state
        .feeds
        .create(
            &app.username,
            &collection.id,
            "Secret tasks",
            ComponentSet::one(CalendarComponent::VTodo),
            None,
        )
        .await
        .unwrap();
    assert!(!created.token.is_empty());
    let response = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=correct+horse"),
        )
        .await;
    let cookie = cookie_from(response.headers()).unwrap();
    let dashboard = app
        .call(
            Method::GET,
            "/.oxicale/dashboard",
            header("Cookie", &cookie),
            Body::empty(),
        )
        .await;
    let body = axum::body::to_bytes(dashboard.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!body_text(&body).contains(&created.token));
    assert!(!body_text(&body).contains("subscribe?token"));
}

#[tokio::test]
async fn disabled_carddav_is_not_advertised() {
    let app = TestApp::with_config(|config| {
        config.addressbook.enabled = false;
        config.addressbook.create_default = false;
    });
    app.provision().await;
    let response = app
        .call(Method::OPTIONS, "/", Default::default(), Body::empty())
        .await;
    let dav = response.headers().get("dav").unwrap().to_str().unwrap();
    assert!(!dav.contains("addressbook"));
    let collections = app.state.store.list_collections(&app.username).unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].kind, oxicale_core::CollectionKind::Calendar);
}

#[tokio::test]
async fn no_auth_backend_provisions_anonymous() {
    let app = TestApp::with_config(|config| {
        config.auth.backend = oxicale::config::AuthBackend::None;
        config.auth.none_user = "anonymous".into();
        config.server.bind = vec!["127.0.0.1:5232".into()];
    });
    let response = app
        .call(
            Method::from_bytes(b"PROPFIND").unwrap(),
            "/anonymous/",
            header("Depth", "0"),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::MULTI_STATUS);
    let user = oxicale_core::UserName::parse("anonymous").unwrap();
    assert_eq!(app.state.store.list_collections(&user).unwrap().len(), 2);
}

#[tokio::test]
async fn api_without_version_path_is_a_clean_404() {
    let app = TestApp::new();
    let response = app
        .call(
            Method::GET,
            "/.oxicale/api",
            header("Authorization", &app.basic()),
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dashboard_logout_form_includes_csrf_token() {
    let app = TestApp::new();
    let response = app
        .call(
            Method::POST,
            "/.oxicale/login",
            header("Content-Type", "application/x-www-form-urlencoded"),
            Body::from("username=alice&password=correct+horse"),
        )
        .await;
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
    let html = String::from_utf8_lossy(&body);
    let logout = html
        .split("action=\"/.oxicale/logout\"")
        .nth(1)
        .unwrap_or("");
    assert!(logout.contains("name=\"csrf\""));
}
