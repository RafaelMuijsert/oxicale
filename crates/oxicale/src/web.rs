use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use base64::Engine;
use chrono::{DateTime, Utc};
use oxicale_core::{
    constant_time_eq, CalendarComponent, CollectionKind, CollectionPath, ComponentSet, UserName,
};
use oxicale_storage::{CollectionMeta, CollectionUpdate};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{AuthError, Backend};
use crate::config::AuthBackend;
use crate::server::{peer_from_request, secure_headers, simple_response, AppState, VERSION};
use crate::subscriptions::{NewSubscription, RefreshOutcome, RemoteAuthInput};
use crate::xml::escape;

pub async fn handle_web(state: Arc<AppState>, request: Request, segments: &[String]) -> Response {
    let method = request.method().clone();
    let headers = request.headers().clone();
    let peer = peer_from_request(&request);
    let (parts, body) = request.into_parts();
    let _ = parts;
    let body = match to_bytes(body, 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => return simple_response(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "too large"),
    };
    let path = segments
        .iter()
        .skip(1)
        .map(String::as_str)
        .collect::<Vec<_>>();
    let response = if path.first() == Some(&"api") {
        handle_api(&state, &method, &path, &headers, &body, peer).await
    } else {
        handle_ui(&state, &method, &path, &headers, &body, peer).await
    };
    secure_headers(response, &state.config)
}

// ---------------------------------------------------------------------------
// HTML UI
// ---------------------------------------------------------------------------

async fn handle_ui(
    state: &Arc<AppState>,
    method: &Method,
    path: &[&str],
    headers: &HeaderMap,
    body: &[u8],
    peer: IpAddr,
) -> Response {
    match path {
        [] => redirect(&ui_url(state, "dashboard")),
        ["login"] if method == Method::GET => render_login(state, headers, None),
        ["login"] if method == Method::POST => login_submit(state, headers, body, peer).await,
        ["logout"] if method == Method::POST => logout(state, headers, body),
        ["static", "style.css"] if method == Method::GET => Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")
            .header(axum::http::header::CACHE_CONTROL, "public, max-age=3600")
            .body(Body::from(STYLE))
            .unwrap(),
        _ => {
            let session = match session_or_auto(state, headers, peer).await {
                Some(session) => session,
                None => return redirect(&ui_url(state, "login")),
            };
            if let Err(response) = check_csrf(state, method, headers, body, &session) {
                return response;
            }
            match path {
                ["dashboard"] if method == Method::GET => render_dashboard(state, &session),
                ["settings"] if method == Method::GET => render_settings(state, &session),
                ["calendar", "new"] if method == Method::POST => {
                    create_calendar_submit(state, &session, body, CollectionKind::Calendar).await
                }
                ["addressbook", "new"] if method == Method::POST => {
                    create_calendar_submit(state, &session, body, CollectionKind::AddressBook).await
                }
                ["calendar", slug] if method == Method::GET => {
                    render_calendar(state, &session, slug)
                }
                ["calendar", slug] if method == Method::POST => {
                    calendar_update_submit(state, &session, slug, body).await
                }
                ["calendar", slug, "delete"] if method == Method::POST => {
                    delete_collection_submit(state, &session, slug).await
                }
                ["calendar", slug, "subscription"] if method == Method::POST => {
                    subscription_interval_submit(state, &session, slug, body).await
                }
                ["calendar", slug, "subscription", "refresh"] if method == Method::POST => {
                    subscription_refresh_submit(state, &session, slug).await
                }
                ["calendar", slug, "feeds"] if method == Method::GET => {
                    render_feeds(state, &session, slug, None, None)
                }
                ["calendar", slug, "feeds"] if method == Method::POST => {
                    feed_create_submit(state, &session, slug, body).await
                }
                ["calendar", slug, "feeds", feed_id, "revoke"] if method == Method::POST => {
                    feed_revoke_submit(state, &session, slug, feed_id).await
                }
                ["calendar", slug, "feeds", feed_id, "rotate"] if method == Method::POST => {
                    feed_rotate_submit(state, &session, slug, feed_id).await
                }
                ["addressbook", slug] if method == Method::GET => {
                    render_addressbook(state, &session, slug)
                }
                ["addressbook", slug] if method == Method::POST => {
                    calendar_update_submit(state, &session, slug, body).await
                }
                ["subscribe"] if method == Method::GET => {
                    render_subscribe(state, &session, None, Vec::new())
                }
                ["subscribe"] if method == Method::POST => {
                    subscribe_submit(state, &session, body).await
                }
                _ => simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
            }
        }
    }
}

async fn session_or_auto(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    peer: IpAddr,
) -> Option<crate::sessions::Session> {
    if let Some(session) = state.session_from_headers(headers) {
        return Some(session);
    }
    match state.auth.config().backend {
        Backend::TrustedHeader | Backend::None => {
            let user = state.authenticate(headers, peer).ok()?;
            state.provision_account(&user).await.ok()?;
            Some(state.sessions.create(user))
        }
        _ => None,
    }
}

fn check_csrf(
    _state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    body: &[u8],
    session: &crate::sessions::Session,
) -> Result<(), Response> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    let header = headers
        .get("X-CSRF-Token")
        .and_then(|value| value.to_str().ok());
    let form = Form::parse(body);
    let token = header.or_else(|| form.get("csrf"));
    match token {
        Some(token) if constant_time_eq(token.as_bytes(), session.csrf.as_bytes()) => Ok(()),
        _ => Err(simple_response(
            StatusCode::FORBIDDEN,
            "text/plain; charset=utf-8",
            "invalid CSRF token",
        )),
    }
}

struct Form {
    values: Vec<(String, String)>,
}

impl Form {
    fn parse(body: &[u8]) -> Self {
        let values = url::form_urlencoded::parse(body)
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        Self { values }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn all(&self, key: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .collect()
    }
}

async fn login_submit(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    body: &[u8],
    peer: IpAddr,
) -> Response {
    let form = Form::parse(body);
    let username = form.get("username").unwrap_or_default();
    let password = form.get("password").unwrap_or_default();
    let key = format!("web|{}|{}", username.to_lowercase(), peer);
    if !state.auth.allowed(&key) {
        return render_login(state, headers, Some("Too many attempts. Try again later."));
    }
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
    );
    match state.auth.authenticate_basic(Some(&basic), peer) {
        Ok(user) => {
            if let Err(error) = state.provision_account(&user).await {
                tracing::error!(%error, "failed to provision user");
                return render_login(state, headers, Some("Sign-in failed."));
            }
            let session = state.sessions.create(user);
            let cookie = state.session_cookie(&session);
            redirect_with_cookie(&ui_url(state, "dashboard"), &cookie)
        }
        Err(AuthError::RateLimited) => {
            render_login(state, headers, Some("Too many attempts. Try again later."))
        }
        Err(_) => {
            state.auth.fail(&key);
            render_login(state, headers, Some("Invalid username or password."))
        }
    }
}

fn logout(state: &Arc<AppState>, headers: &HeaderMap, _body: &[u8]) -> Response {
    if let Some(session) = state.session_from_headers(headers) {
        state.sessions.delete(&session.id);
    }
    redirect_with_cookie(&ui_url(state, "login"), &state.clear_session_cookie())
}

fn render_login(state: &AppState, headers: &HeaderMap, error: Option<&str>) -> Response {
    let backend = state.config.auth.backend;
    let content = match backend {
        AuthBackend::TrustedHeader => format!(
            r#"<main class="card narrow"><h1>Oxicale</h1><p>Sign-in is handled by your reverse proxy.</p><p><a class="button" href="{}">Continue</a></p></main>"#,
            ui_url(state, "dashboard")
        ),
        _ => {
            let warning = if backend == AuthBackend::None {
                "<div class=\"warning\">Authentication is disabled. This development instance must not be exposed to the network.</div>"
            } else {
                ""
            };
            let error = error
                .map(|message| {
                    format!(
                        "<div class=\"error\" role=\"alert\">{}</div>",
                        escape(message)
                    )
                })
                .unwrap_or_default();
            format!(
                r#"<main class="card narrow"><h1>Sign in to Oxicale</h1>{warning}{error}
<form method="post" action="{action}">
<label for="username">Username</label><input id="username" name="username" autocomplete="username" required>
<label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" required>
<button type="submit">Sign in</button></form></main>"#,
                action = ui_url(state, "login")
            )
        }
    };
    html_page(state, headers, "Sign in", &content)
}

fn render_dashboard(state: &Arc<AppState>, session: &crate::sessions::Session) -> Response {
    let user = &session.user;
    let collections = match state.store.list_collections(user) {
        Ok(collections) => collections,
        Err(_) => return simple_response(StatusCode::INTERNAL_SERVER_ERROR, "text/plain", "error"),
    };
    let default_calendar = collections
        .iter()
        .find(|collection| {
            collection.kind == CollectionKind::Calendar && collection.subscription.is_none()
        })
        .cloned();
    let subscriptions: Vec<_> = collections
        .iter()
        .filter(|collection| collection.subscription.is_some())
        .cloned()
        .collect();
    let addressbooks: Vec<_> = collections
        .iter()
        .filter(|collection| collection.kind == CollectionKind::AddressBook)
        .cloned()
        .collect();
    let other_calendars: Vec<_> = collections
        .iter()
        .filter(|collection| {
            collection.kind == CollectionKind::Calendar
                && collection.subscription.is_none()
                && default_calendar
                    .as_ref()
                    .map(|c| c.id != collection.id)
                    .unwrap_or(true)
        })
        .cloned()
        .collect();

    let mut content = String::from("<h1>Dashboard</h1>");
    content.push_str(&connection_panel(state, user));
    content.push_str("<h2>Calendar</h2><div class=\"grid\">");
    if let Some(calendar) = &default_calendar {
        content.push_str(&calendar_card(state, user, calendar));
    }
    content.push_str("</div>");
    content.push_str("<h2>Subscribed calendars</h2>");
    if subscriptions.is_empty() {
        content.push_str("<p>No subscriptions yet. Download a remote calendar and share it with every client.</p>");
    } else {
        content.push_str("<div class=\"grid\">");
        for collection in &subscriptions {
            content.push_str(&calendar_card(state, user, collection));
        }
        content.push_str("</div>");
    }
    content.push_str(&format!(
        "<p><a class=\"button\" href=\"{}\">Add subscription</a></p>",
        ui_url(state, "subscribe")
    ));
    if !other_calendars.is_empty() {
        content.push_str("<h2>Other calendars</h2><div class=\"grid\">");
        for collection in &other_calendars {
            content.push_str(&calendar_card(state, user, collection));
        }
        content.push_str("</div>");
    }
    if state.config.addressbook.enabled {
        content.push_str("<h2>Address books</h2>");
        if addressbooks.is_empty() {
            content.push_str("<p>No address books yet.</p>");
        } else {
            content.push_str("<div class=\"grid\">");
            for collection in &addressbooks {
                content.push_str(&calendar_card(state, user, collection));
            }
            content.push_str("</div>");
        }
        content.push_str(&format!(
            r##"<details><summary>Add address book</summary><form method="post" action="{}">
<input type="hidden" name="csrf" value="{csrf}">
<label for="ab-name">Name</label><input id="ab-name" name="name" required>
<label for="ab-color">Colour</label><input id="ab-color" name="color" type="color" value="#10b981">
<label for="ab-description">Description</label><textarea id="ab-description" name="description"></textarea>
<button type="submit">Create address book</button></form></details>"##,
            ui_url(state, "addressbook/new"),
            csrf = escape(&session.csrf)
        ));
    }
    content.push_str(&format!(
        r##"<details><summary>Add calendar</summary><form method="post" action="{}">
<input type="hidden" name="csrf" value="{csrf}">
<label for="cal-name">Name</label><input id="cal-name" name="name" required>
<label for="cal-color">Colour</label><input id="cal-color" name="color" type="color" value="#3b82f6">
<label for="cal-description">Description</label><textarea id="cal-description" name="description"></textarea>
<p class="muted">All Oxicale calendars contain events, tasks and journal entries. You can create a filtered feed later if a client needs only one kind.</p>
<button type="submit">Create calendar</button></form></details>"##,
        ui_url(state, "calendar/new"),
        csrf = escape(&session.csrf)
    ));
    html_page_auth(state, "Dashboard", &content, user, &session.csrf)
}

fn calendar_card(state: &Arc<AppState>, user: &UserName, meta: &CollectionMeta) -> String {
    let path = CollectionPath::parse(user.as_str(), &meta.slug).unwrap();
    let items = state.store.list_items(&path).unwrap_or_default();
    let events = items
        .iter()
        .filter(|i| i.component == oxicale_core::ItemComponent::VEvent)
        .count();
    let tasks = items
        .iter()
        .filter(|i| i.component == oxicale_core::ItemComponent::VTodo)
        .count();
    let journals = items
        .iter()
        .filter(|i| i.component == oxicale_core::ItemComponent::VJournal)
        .count();
    let color = meta.color.as_deref().unwrap_or("#6b7280");
    let url = public_url(state, &format!("{}/{}/", meta.owner, meta.slug));
    let badge = if meta.subscription.is_some() {
        "<span class=\"badge\">Subscribed</span><span class=\"badge\">Read-only</span>"
    } else {
        ""
    };
    let detail = if meta.kind == CollectionKind::Calendar {
        ui_url(state, &format!("calendar/{}", meta.slug))
    } else {
        ui_url(state, &format!("addressbook/{}", meta.slug))
    };
    let counts = if meta.kind == CollectionKind::Calendar {
        format!("{events} events &middot; {tasks} tasks &middot; {journals} journal entries")
    } else {
        format!("{} contacts", items.len())
    };
    let status = meta
        .subscription
        .as_ref()
        .map(|source| {
            if let Some(error) = &source.last_error {
                format!(
                    "<p class=\"error\">Showing last good data. {}</p>",
                    escape(error)
                )
            } else {
                "<p class=\"muted\">Up to date</p>".to_string()
            }
        })
        .unwrap_or_default();
    format!(
        r#"<article class="card"><div class="swatch" style="background:{color}"></div>
<h3><a href="{detail}">{name}</a></h3>{badge}
<p>{counts}</p>{status}
<p class="url"><code>{url}</code></p>
<p><a href="{detail}">Edit</a>{advanced}</p></article>"#,
        name = escape(&meta.display_name),
        url = escape(&url),
        advanced = if meta.kind == CollectionKind::Calendar {
            format!(
                r#" &middot; <a href="{}">Advanced feeds</a>"#,
                ui_url(state, &format!("calendar/{}/feeds", meta.slug))
            )
        } else {
            String::new()
        }
    )
}

fn connection_panel(state: &AppState, user: &UserName) -> String {
    format!(
        r#"<section class="card"><h2>Connect a client</h2>
<p>Use these details in your CalDAV/CardDAV client. Your calendar is already created.</p>
<dl><dt>Server URL</dt><dd><code>{server}</code></dd><dt>Username</dt><dd><code>{user}</code></dd></dl>
<p class="muted">Principal URL: <code>{server}/{user}/</code></p></section>"#,
        server = escape(&public_base_url(state)),
        user = escape(user.as_str())
    )
}

fn render_settings(state: &Arc<AppState>, session: &crate::sessions::Session) -> Response {
    let content = format!(
        r#"<h1>Settings</h1>
<section class="card"><h2>Account</h2><p>Username: <code>{user}</code></p>
<p>Principal URL: <code>{server}/{user}/</code></p></section>
<section class="card"><h2>Session</h2>
<form method="post" action="{logout}"><input type="hidden" name="csrf" value="{csrf}"><button type="submit">Sign out</button></form></section>
<section class="card"><h2>About</h2><p>Oxicale {version}. Licensed GPL-3.0-or-later.</p></section>"#,
        user = escape(session.user.as_str()),
        server = escape(&public_base_url(state)),
        logout = ui_url(state, "logout"),
        csrf = escape(&session.csrf),
        version = VERSION
    );
    html_page_auth(state, "Settings", &content, &session.user, &session.csrf)
}

fn render_calendar(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
) -> Response {
    let path = match CollectionPath::parse(session.user.as_str(), slug) {
        Ok(path) => path,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let meta = match state.store.collection(&path) {
        Ok(meta) => meta,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    if meta.kind == CollectionKind::AddressBook {
        return render_addressbook(state, session, slug);
    }
    let mut content = format!(
        r#"<h1>{name}</h1><p><a href="{dashboard}">Back to dashboard</a></p>
<form method="post" action="{action}"><input type="hidden" name="csrf" value="{csrf}">
<label for="name">Name</label><input id="name" name="name" value="{name}" required>
<label for="color">Colour</label><input id="color" name="color" type="color" value="{color}">
<label for="description">Description</label><textarea id="description" name="description">{description}</textarea>
<p class="muted">Collection URL: <code>{url}</code></p>
<button type="submit">Save changes</button></form>"#,
        name = escape(&meta.display_name),
        color = escape(meta.color.as_deref().unwrap_or("#3b82f6")),
        description = escape(meta.description.as_deref().unwrap_or("")),
        dashboard = ui_url(state, "dashboard"),
        action = ui_url(state, &format!("calendar/{slug}")),
        csrf = escape(&session.csrf),
        url = escape(&public_url(
            state,
            &format!("{}/{}/", meta.owner, meta.slug)
        ))
    );
    if let Some(source) = &meta.subscription {
        content.push_str(&format!(
            r#"<section class="card"><h2>Subscription</h2>
<p>Source: <code>{url}</code></p>
<p>Refresh interval: {interval} seconds</p>
<p>Status: {status}</p>{error}
<form method="post" action="{refresh}"><input type="hidden" name="csrf" value="{csrf}"><button type="submit">Refresh now</button></form>
<form method="post" action="{settings}"><input type="hidden" name="csrf" value="{csrf}">
<label for="interval">Refresh interval (seconds)</label><input id="interval" name="refresh_interval_seconds" type="number" min="900" value="{interval}">
<button type="submit">Save interval</button></form>
<p class="muted">This calendar is read-only. Changes must be made at the source.</p></section>"#,
            url = escape(&source.url),
            interval = source.refresh_interval_seconds,
            status = if source.last_error.is_some() { "Degraded" } else { "Healthy" },
            error = source
                .last_error
                .as_ref()
                .map(|error| format!("<p class=\"error\">{}</p>", escape(error)))
                .unwrap_or_default(),
            refresh = ui_url(state, &format!("calendar/{slug}/subscription/refresh")),
            settings = ui_url(state, &format!("calendar/{slug}/subscription")),
            csrf = escape(&session.csrf),
        ));
    }
    let export_url = public_url(state, &format!("{}/{}/", meta.owner, meta.slug));
    content.push_str(&format!(
        r#"<section class="card"><h2>Advanced</h2>
<p><a href="{feeds}">Filtered feeds</a></p>
<p><a href="{export}">Export calendar</a></p>
<form method="post" action="{delete}"><input type="hidden" name="csrf" value="{csrf}"><button class="danger" type="submit">Delete calendar</button></form></section>"#,
        feeds = ui_url(state, &format!("calendar/{slug}/feeds")),
        export = export_url,
        delete = ui_url(state, &format!("calendar/{slug}/delete")),
        csrf = escape(&session.csrf)
    ));
    html_page_auth(
        state,
        &meta.display_name,
        &content,
        &session.user,
        &session.csrf,
    )
}

fn render_addressbook(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
) -> Response {
    let path = match CollectionPath::parse(session.user.as_str(), slug) {
        Ok(path) => path,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let meta = match state.store.collection(&path) {
        Ok(meta) => meta,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let content = format!(
        r#"<h1>{name}</h1><p><a href="{dashboard}">Back to dashboard</a></p>
<form method="post" action="{action}"><input type="hidden" name="csrf" value="{csrf}">
<label for="name">Name</label><input id="name" name="name" value="{name}" required>
<label for="color">Colour</label><input id="color" name="color" type="color" value="{color}">
<label for="description">Description</label><textarea id="description" name="description">{description}</textarea>
<p class="muted">Collection URL: <code>{url}</code></p>
<button type="submit">Save changes</button></form>"#,
        name = escape(&meta.display_name),
        color = escape(meta.color.as_deref().unwrap_or("#10b981")),
        description = escape(meta.description.as_deref().unwrap_or("")),
        dashboard = ui_url(state, "dashboard"),
        action = ui_url(state, &format!("addressbook/{slug}")),
        csrf = escape(&session.csrf),
        url = escape(&public_url(
            state,
            &format!("{}/{}/", meta.owner, meta.slug)
        ))
    );
    html_page_auth(
        state,
        &meta.display_name,
        &content,
        &session.user,
        &session.csrf,
    )
}

fn render_subscribe(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    error: Option<&str>,
    values: Vec<(String, String)>,
) -> Response {
    let value = |name: &str| {
        values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| escape(value))
            .unwrap_or_default()
    };
    let error = error
        .map(|message| {
            format!(
                "<div class=\"error\" role=\"alert\">{}</div>",
                escape(message)
            )
        })
        .unwrap_or_default();
    let content = format!(
        r#"<h1>Add subscription</h1><p><a href="{dashboard}">Back to dashboard</a></p>{error}
<p>Oxicale downloads this calendar and shares it with all your clients as a read-only calendar. The remote server must be reachable from your Oxicale server, not from your browser.</p>
<form method="post" action="{action}"><input type="hidden" name="csrf" value="{csrf}">
<label for="url">Calendar URL</label><input id="url" name="url" type="url" value="{url}" placeholder="https://example.com/calendar.ics" required>
<label for="display_name">Display name (optional)</label><input id="display_name" name="display_name" value="{display_name}">
<label for="refresh">Refresh interval</label><select id="refresh" name="refresh_interval_seconds">
<option value="900">15 minutes</option><option value="1800">30 minutes</option><option value="3600" selected>1 hour</option>
<option value="10800">3 hours</option><option value="21600">6 hours</option><option value="43200">12 hours</option>
<option value="86400">1 day</option><option value="604800">1 week</option></select>
<label for="color">Colour</label><input id="color" name="color" type="color" value="{color}">
<details><summary>Credentials (optional)</summary>
<label for="auth_kind">Authentication</label><select id="auth_kind" name="auth_kind"><option value="">None</option><option value="basic">Basic</option><option value="bearer">Bearer token</option></select>
<label for="auth_username">Username</label><input id="auth_username" name="auth_username" value="{auth_username}">
<label for="auth_secret">Password or token</label><input id="auth_secret" name="auth_secret" type="password">
</details>
<button type="submit">Add subscription</button></form>"#,
        dashboard = ui_url(state, "dashboard"),
        action = ui_url(state, "subscribe"),
        csrf = escape(&session.csrf),
        url = value("url"),
        display_name = value("display_name"),
        color = values
            .iter()
            .find(|(key, _)| key == "color")
            .map(|(_, value)| escape(value))
            .unwrap_or_else(|| "#10b981".to_string()),
        auth_username = value("auth_username")
    );
    html_page_auth(
        state,
        "Add subscription",
        &content,
        &session.user,
        &session.csrf,
    )
}

fn render_feeds(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
    one_time_url: Option<&str>,
    error: Option<&str>,
) -> Response {
    let path = match CollectionPath::parse(session.user.as_str(), slug) {
        Ok(path) => path,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let meta = match state.store.collection(&path) {
        Ok(meta) => meta,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let mut content = format!(
        "<h1>{} &mdash; Filtered feeds</h1><p><a href=\"{}\">Back to calendar</a></p>",
        escape(&meta.display_name),
        ui_url(state, &format!("calendar/{slug}"))
    );
    if let Some(error) = error {
        content.push_str(&format!("<div class=\"error\">{}</div>", escape(error)));
    }
    if let Some(url) = one_time_url {
        content.push_str(&format!(
            r#"<section class="card secret"><h2>This URL is shown only once</h2>
<p>Store it now. Anyone with this URL can read the selected items.</p>
<p><code>{}</code></p><p class="warning">Treat it like a password. You can revoke it at any time.</p></section>"#,
            escape(url)
        ));
    }
    content.push_str("<section class=\"card\"><h2>Existing feeds</h2>");
    if meta.feeds.is_empty() {
        content.push_str(
            "<p>No feeds. Create one if a client needs a calendar with only one kind of item.</p>",
        );
    }
    for feed in &meta.feeds {
        let active = feed.is_active(Utc::now());
        content.push_str(&format!(
            r#"<article class="feed"><h3>{name}</h3>
<p>{components}</p><p class="muted">Masked URL: <code>{masked}</code></p>
<p class="muted">Created {created}{expiry}{revoked}</p>
<form method="post" action="{rotate}" class="inline"><input type="hidden" name="csrf" value="{csrf}"><button type="submit">Rotate token</button></form>
<form method="post" action="{revoke}" class="inline"><input type="hidden" name="csrf" value="{csrf}"><button class="danger" type="submit">Revoke</button></form>
</article>"#,
            name = escape(&feed.name),
            components = feed
                .components
                .iter()
                .map(|component| format!("<span class=\"badge\">{}</span>", escape(component.as_str())))
                .collect::<String>(),
            masked = escape(&state.feeds.masked_url(feed)),
            created = escape(&feed.created_at.format("%Y-%m-%d").to_string()),
            expiry = feed
                .expires_at
                .map(|expiry| format!(" &middot; expires {}", escape(&expiry.format("%Y-%m-%d").to_string())))
                .unwrap_or_default(),
            revoked = if active {
                String::new()
            } else {
                " &middot; <strong>revoked or expired</strong>".to_string()
            },
            rotate = ui_url(state, &format!("calendar/{slug}/feeds/{}/rotate", feed.id)),
            revoke = ui_url(state, &format!("calendar/{slug}/feeds/{}/revoke", feed.id)),
            csrf = escape(&session.csrf)
        ));
    }
    content.push_str("</section>");
    content.push_str(&format!(
        r#"<section class="card"><h2>Create feed</h2>
<p class="warning">Anyone with this URL can read the selected items. Treat it like a password. You can revoke it at any time.</p>
<form method="post" action="{action}"><input type="hidden" name="csrf" value="{csrf}">
<label for="feed-name">Feed name</label><input id="feed-name" name="name" required>
<fieldset><legend>Components</legend>
<label><input type="checkbox" name="components" value="VEVENT"> Events</label>
<label><input type="checkbox" name="components" value="VTODO"> Tasks</label>
<label><input type="checkbox" name="components" value="VJOURNAL"> Journal</label></fieldset>
<label for="expiry">Expiry</label><select id="expiry" name="expiry_days"><option value="">Never</option><option value="7">7 days</option><option value="30">30 days</option><option value="90">90 days</option><option value="365">1 year</option></select>
<button type="submit">Create feed</button></form></section>"#,
        action = ui_url(state, &format!("calendar/{slug}/feeds")),
        csrf = escape(&session.csrf)
    ));
    html_page_auth(state, "Feeds", &content, &session.user, &session.csrf)
}

async fn subscription_refresh_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
) -> Response {
    if let Ok(path) = CollectionPath::parse(session.user.as_str(), slug) {
        if let Ok(meta) = state.store.collection(&path) {
            if meta.subscription.is_some() {
                let _ = state.subscriptions.refresh(&path).await;
            }
        }
    }
    redirect(&ui_url(state, &format!("calendar/{slug}")))
}

async fn subscription_interval_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
    body: &[u8],
) -> Response {
    let form = Form::parse(body);
    let path = match CollectionPath::parse(session.user.as_str(), slug) {
        Ok(path) => path,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let Some(interval) = form
        .get("refresh_interval_seconds")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return simple_response(StatusCode::BAD_REQUEST, "text/plain", "invalid interval");
    };
    if interval < state.config.subscriptions.min_interval.seconds()
        || interval > state.config.subscriptions.max_interval.seconds()
    {
        return simple_response(
            StatusCode::BAD_REQUEST,
            "text/plain",
            "interval out of range",
        );
    }
    if let Err(error) = state
        .store
        .update_subscription_interval(&path, interval)
        .await
    {
        return simple_response(StatusCode::BAD_REQUEST, "text/plain", &error.to_string());
    }
    redirect(&ui_url(state, &format!("calendar/{slug}")))
}

async fn subscribe_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    body: &[u8],
) -> Response {
    let form = Form::parse(body);
    let url_value = form.get("url").unwrap_or_default().to_string();
    let display_name = form.get("display_name").unwrap_or_default().to_string();
    let interval = form
        .get("refresh_interval_seconds")
        .and_then(|value| value.parse().ok());
    let color = form
        .get("color")
        .map(str::to_string)
        .filter(|c| !c.is_empty());
    let auth_kind = form.get("auth_kind").unwrap_or_default();
    let auth = if auth_kind.is_empty() {
        None
    } else {
        Some(RemoteAuthInput {
            kind: auth_kind.to_string(),
            username: form.get("auth_username").map(str::to_string),
            password: form.get("auth_secret").map(str::to_string),
            token: form.get("auth_secret").map(str::to_string),
        })
    };
    let slug = unique_slug(
        state,
        &session.user,
        if display_name.trim().is_empty() {
            "subscribed"
        } else {
            &display_name
        },
    );
    let request = NewSubscription {
        url: url_value.clone(),
        display_name: Some(display_name.clone()).filter(|v| !v.trim().is_empty()),
        color,
        refresh_interval_seconds: interval,
        auth,
    };
    match state
        .subscriptions
        .create(&session.user, &slug, request)
        .await
    {
        Ok(_) => redirect(&ui_url(state, "dashboard")),
        Err(error) => {
            let values = vec![
                ("url".to_string(), url_value),
                ("display_name".to_string(), display_name),
                (
                    "refresh_interval_seconds".to_string(),
                    interval.unwrap_or(3600).to_string(),
                ),
            ];
            render_subscribe(state, session, Some(&subscription_error(&error)), values)
        }
    }
}

fn subscription_error(error: &crate::subscriptions::SubscriptionError) -> String {
    match error {
        crate::subscriptions::SubscriptionError::BlockedAddress => {
            "The URL points to a private network address, which is blocked by default.".into()
        }
        crate::subscriptions::SubscriptionError::Unreachable => {
            "The URL is not reachable from the server.".into()
        }
        crate::subscriptions::SubscriptionError::InvalidCalendar => {
            "The response is not a valid iCalendar file.".into()
        }
        crate::subscriptions::SubscriptionError::Http(status) => {
            format!("The remote server returned HTTP {status}. Check the credentials.")
        }
        crate::subscriptions::SubscriptionError::TooLarge => "The calendar is too large.".into(),
        other => other.to_string(),
    }
}

async fn create_calendar_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    body: &[u8],
    kind: CollectionKind,
) -> Response {
    if kind == CollectionKind::AddressBook && !state.config.addressbook.enabled {
        return simple_response(StatusCode::FORBIDDEN, "text/plain", "CardDAV is disabled");
    }
    let form = Form::parse(body);
    let name = form.get("name").unwrap_or("Untitled").trim().to_string();
    if name.is_empty() {
        return simple_response(StatusCode::BAD_REQUEST, "text/plain", "name is required");
    }
    let slug = unique_slug(state, &session.user, &name);
    match state
        .store
        .create_collection(&session.user, kind, &slug, &name)
        .await
    {
        Ok(meta) => {
            let update = CollectionUpdate {
                description: form.get("description").map(str::to_string),
                color: form
                    .get("color")
                    .map(str::to_string)
                    .filter(|c| !c.is_empty()),
                ..Default::default()
            };
            let _ = state
                .store
                .update_collection(
                    &CollectionPath::parse(session.user.as_str(), &meta.slug).unwrap(),
                    update,
                )
                .await;
            redirect(&ui_url(state, "dashboard"))
        }
        Err(error) => simple_response(
            StatusCode::BAD_REQUEST,
            "text/plain; charset=utf-8",
            &error.to_string(),
        ),
    }
}

async fn calendar_update_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
    body: &[u8],
) -> Response {
    let form = Form::parse(body);
    let path = match CollectionPath::parse(session.user.as_str(), slug) {
        Ok(path) => path,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let update = CollectionUpdate {
        display_name: form.get("name").map(str::to_string),
        description: form.get("description").map(str::to_string),
        color: form
            .get("color")
            .map(str::to_string)
            .filter(|c| !c.is_empty()),
        ..Default::default()
    };
    match state.store.update_collection(&path, update).await {
        Ok(meta) => {
            let target = if meta.kind == CollectionKind::Calendar {
                format!("calendar/{slug}")
            } else {
                format!("addressbook/{slug}")
            };
            redirect(&ui_url(state, &target))
        }
        Err(error) => simple_response(StatusCode::BAD_REQUEST, "text/plain", &error.to_string()),
    }
}

async fn delete_collection_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
) -> Response {
    if let Ok(path) = CollectionPath::parse(session.user.as_str(), slug) {
        let _ = state.store.delete_collection(&path, None).await;
    }
    redirect(&ui_url(state, "dashboard"))
}

async fn feed_create_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
    body: &[u8],
) -> Response {
    let form = Form::parse(body);
    let path = match CollectionPath::parse(session.user.as_str(), slug) {
        Ok(path) => path,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let meta = match state.store.collection(&path) {
        Ok(meta) => meta,
        Err(_) => return simple_response(StatusCode::NOT_FOUND, "text/plain", "not found"),
    };
    let mut components = ComponentSet::empty();
    for value in form.all("components") {
        if let Ok(component) = value.parse::<CalendarComponent>() {
            components.insert(component);
        }
    }
    let expiry = form
        .get("expiry_days")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|days| *days > 0)
        .map(|days| Utc::now() + chrono::Duration::days(days));
    let name = form.get("name").unwrap_or("Feed").trim().to_string();
    match state
        .feeds
        .create(&session.user, &meta.id, &name, components, expiry)
        .await
    {
        Ok(created) => render_feeds(state, session, slug, Some(&created.url), None),
        Err(error) => render_feeds(state, session, slug, None, Some(&error.to_string())),
    }
}

async fn feed_revoke_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
    feed_id: &str,
) -> Response {
    if let Ok(path) = CollectionPath::parse(session.user.as_str(), slug) {
        if let Ok(meta) = state.store.collection(&path) {
            let _ = state.feeds.revoke(&session.user, &meta.id, feed_id).await;
        }
    }
    redirect(&ui_url(state, &format!("calendar/{slug}/feeds")))
}

async fn feed_rotate_submit(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    slug: &str,
    feed_id: &str,
) -> Response {
    if let Ok(path) = CollectionPath::parse(session.user.as_str(), slug) {
        if let Ok(meta) = state.store.collection(&path) {
            match state.feeds.rotate(&session.user, &meta.id, feed_id).await {
                Ok(created) => return render_feeds(state, session, slug, Some(&created.url), None),
                Err(error) => {
                    return render_feeds(state, session, slug, None, Some(&error.to_string()))
                }
            }
        }
    }
    simple_response(StatusCode::NOT_FOUND, "text/plain", "not found")
}

fn html_page(state: &AppState, headers: &HeaderMap, title: &str, content: &str) -> Response {
    let user = state
        .session_from_headers(headers)
        .map(|session| session.user);
    let body = layout(state, title, content, user.as_ref(), None);
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap()
}

fn html_page_auth(
    state: &AppState,
    title: &str,
    content: &str,
    user: &UserName,
    csrf: &str,
) -> Response {
    let body = layout(state, title, content, Some(user), Some(csrf));
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap()
}

fn layout(
    state: &AppState,
    title: &str,
    content: &str,
    user: Option<&UserName>,
    csrf: Option<&str>,
) -> String {
    let nav = user
        .map(|user| {
            let csrf = csrf
                .map(|token| {
                    format!(
                        r#"<input type="hidden" name="csrf" value="{}">"#,
                        escape(token)
                    )
                })
                .unwrap_or_default();
            format!(
                r#"<header><nav><a class="brand" href="{dashboard}">Oxicale</a><span class="spacer"></span>
<span>{user}</span><a href="{settings}">Settings</a>
<form method="post" action="{logout}" class="inline">{csrf}<button type="submit">Sign out</button></form></nav></header>"#,
                dashboard = ui_url(state, "dashboard"),
                settings = ui_url(state, "settings"),
                logout = ui_url(state, "logout"),
                user = escape(user.as_str())
            )
        })
        .unwrap_or_default();
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{} &mdash; Oxicale</title><link rel="stylesheet" href="{}"></head>
<body>{}<div class="shell">{}</div><footer>Oxicale {VERSION}</footer></body></html>"#,
        escape(title),
        ui_url(state, "static/style.css"),
        nav,
        content
    )
}

fn public_base_url(state: &AppState) -> String {
    let external = state.config.external_url();
    let base = state.config.server.normalized_base_path();
    let base = base.trim_end_matches('/');
    format!("{external}{base}")
}

fn public_url(state: &AppState, path: &str) -> String {
    format!(
        "{}/{}",
        public_base_url(state),
        path.trim_start_matches('/')
    )
}

fn ui_url(state: &AppState, path: &str) -> String {
    format!(
        "{}.oxicale/{}",
        state.config.server.normalized_base_path(),
        path.trim_start_matches('/')
    )
}

fn redirect(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(axum::http::header::LOCATION, location)
        .body(Body::empty())
        .unwrap()
}

fn redirect_with_cookie(location: &str, cookie: &str) -> Response {
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(axum::http::header::LOCATION, location)
        .header(axum::http::header::SET_COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn unique_slug(state: &AppState, user: &UserName, value: &str) -> String {
    let base = slugify(value);
    let existing: BTreeSet<String> = state
        .store
        .list_collections(user)
        .unwrap_or_default()
        .into_iter()
        .map(|meta| meta.slug)
        .collect();
    if !existing.contains(&base) {
        return base;
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{base}-{counter}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "calendar".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

// ---------------------------------------------------------------------------
// Management API
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct CreateCollectionRequest {
    kind: String,
    slug: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    color: Option<String>,
    components: Option<Value>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    subscription: Option<Value>,
    #[serde(default)]
    refresh_interval_seconds: Option<u64>,
    #[serde(default)]
    auth: Option<RemoteAuthInput>,
}

#[derive(Debug, Deserialize)]
struct PatchCollectionRequest {
    display_name: Option<String>,
    description: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateFeedRequest {
    name: String,
    components: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
}

async fn handle_api(
    state: &Arc<AppState>,
    method: &Method,
    path: &[&str],
    headers: &HeaderMap,
    body: &[u8],
    peer: IpAddr,
) -> Response {
    if path.len() < 2 || path[0] != "api" || path[1] != "v1" {
        return api_not_found();
    }
    let path = &path[2..];
    match (method, path) {
        (&Method::POST, ["session"]) => api_login(state, headers, body, peer).await,
        (&Method::DELETE, ["session"]) => api_logout(state, headers),
        _ => {
            let session = match authenticate_api(state, headers, peer).await {
                Ok(session) => session,
                Err(response) => return response,
            };
            if let Err(response) = check_api_csrf(state, method, headers, &session) {
                return response;
            }
            match (method, path) {
                (&Method::GET, ["me"]) => api_me(state, &session),
                (&Method::GET, ["collections"]) => api_list_collections(state, &session),
                (&Method::POST, ["collections"]) => {
                    api_create_collection(state, &session, body).await
                }
                (&Method::GET, ["collections", id]) => api_get_collection(state, &session, id),
                (&Method::PATCH, ["collections", id]) => {
                    api_patch_collection(state, &session, id, body).await
                }
                (&Method::DELETE, ["collections", id]) => {
                    api_delete_collection(state, &session, id).await
                }
                (&Method::POST, ["collections", id, "subscription", "refresh"]) => {
                    api_refresh_subscription(state, &session, id).await
                }
                (&Method::POST, ["collections", id, "refresh"]) => {
                    api_refresh_subscription(state, &session, id).await
                }
                (&Method::GET, ["collections", id, "feeds"]) => api_list_feeds(state, &session, id),
                (&Method::POST, ["collections", id, "feeds"]) => {
                    api_create_feed(state, &session, id, body).await
                }
                (&Method::DELETE, ["collections", id, "feeds", feed_id]) => {
                    api_revoke_feed(state, &session, id, feed_id).await
                }
                (&Method::POST, ["collections", id, "feeds", feed_id, "rotate"]) => {
                    api_rotate_feed(state, &session, id, feed_id).await
                }
                (&Method::GET, ["collections", id, "export"]) => {
                    api_export_collection(state, &session, id)
                }
                _ => api_not_found(),
            }
        }
    }
}

async fn api_login(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    body: &[u8],
    peer: IpAddr,
) -> Response {
    let request: LoginRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "invalid JSON"),
    };
    let key = format!("api|{}|{}", request.username.to_lowercase(), peer);
    if !state.auth.allowed(&key) {
        return api_error(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", request.username, request.password))
    );
    match state.auth.authenticate_basic(Some(&basic), peer) {
        Ok(user) => {
            if let Err(error) = state.provision_account(&user).await {
                tracing::error!(%error, "failed to provision account");
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "account provisioning failed",
                );
            }
            let session = state.sessions.create(user);
            let cookie = state.session_cookie(&session);
            let mut response = api_json(
                StatusCode::OK,
                json!({"user": session.user.as_str(), "csrf": session.csrf}),
            );
            response.headers_mut().insert(
                axum::http::header::SET_COOKIE,
                HeaderValue::from_str(&cookie).unwrap(),
            );
            let _ = headers;
            response
        }
        Err(AuthError::RateLimited) => {
            api_error(StatusCode::TOO_MANY_REQUESTS, "too many attempts")
        }
        Err(_) => {
            state.auth.fail(&key);
            api_error(StatusCode::UNAUTHORIZED, "invalid username or password")
        }
    }
}

fn api_logout(state: &Arc<AppState>, headers: &HeaderMap) -> Response {
    if let Some(session) = state.session_from_headers(headers) {
        state.sessions.delete(&session.id);
    }
    let mut response = api_json(StatusCode::OK, json!({"ok": true}));
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&state.clear_session_cookie()).unwrap(),
    );
    response
}

async fn authenticate_api(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    peer: IpAddr,
) -> Result<crate::sessions::Session, Response> {
    if let Some(session) = state.session_from_headers(headers) {
        return Ok(session);
    }
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        if !state.config.web.allow_basic_api_auth {
            return Err(api_error(
                StatusCode::UNAUTHORIZED,
                "Basic authentication is disabled for the management API",
            ));
        }
        let user = state
            .authenticate(headers, peer)
            .map_err(|_| api_error(StatusCode::UNAUTHORIZED, "invalid credentials"))?;
        state.provision_account(&user).await.map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "account provisioning failed",
            )
        })?;
        return Ok(state.sessions.create(user));
    }
    Err(api_error(
        StatusCode::UNAUTHORIZED,
        "authentication required",
    ))
}

fn check_api_csrf(
    _state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    session: &crate::sessions::Session,
) -> Result<(), Response> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        return Ok(());
    }
    let token = headers
        .get("X-CSRF-Token")
        .and_then(|value| value.to_str().ok());
    match token {
        Some(token) if constant_time_eq(token.as_bytes(), session.csrf.as_bytes()) => Ok(()),
        _ => Err(api_error(StatusCode::FORBIDDEN, "invalid CSRF token")),
    }
}

fn api_me(state: &Arc<AppState>, session: &crate::sessions::Session) -> Response {
    api_json(
        StatusCode::OK,
        json!({
            "username": session.user.as_str(),
            "csrf": session.csrf,
            "server_url": public_base_url(state),
            "principal_url": public_url(state, &format!("{}/", session.user)),
            "version": VERSION,
        }),
    )
}

fn api_list_collections(state: &Arc<AppState>, session: &crate::sessions::Session) -> Response {
    let collections = match state.store.list_collections(&session.user) {
        Ok(collections) => collections,
        Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let values: Vec<Value> = collections
        .iter()
        .map(|meta| collection_json(state, meta, false))
        .collect();
    api_json(StatusCode::OK, json!({"collections": values}))
}

async fn api_create_collection(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    body: &[u8],
) -> Response {
    let request: CreateCollectionRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid request"),
    };
    let kind = match request.kind.as_str() {
        "calendar" => CollectionKind::Calendar,
        "addressbook" => CollectionKind::AddressBook,
        _ => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid kind"),
    };
    if kind == CollectionKind::Calendar && request.components.is_some() {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "calendar component sets cannot be chosen; calendars are always mixed",
        );
    }
    if kind == CollectionKind::AddressBook && !state.config.addressbook.enabled {
        return api_error(StatusCode::FORBIDDEN, "CardDAV is disabled");
    }
    let subscription_url = request.url.clone().or_else(|| {
        request
            .subscription
            .as_ref()
            .and_then(|value| value.get("url"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
    });
    if let Some(url) = subscription_url {
        if kind != CollectionKind::Calendar {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "subscriptions are calendars",
            );
        }
        let name = request
            .display_name
            .clone()
            .unwrap_or_else(|| "Subscribed".into());
        let slug = request
            .slug
            .clone()
            .unwrap_or_else(|| unique_slug(state, &session.user, &name));
        let interval = request.refresh_interval_seconds.or_else(|| {
            request
                .subscription
                .as_ref()
                .and_then(|value| value.get("refresh_interval_seconds"))
                .and_then(|value| value.as_u64())
        });
        let auth = request.auth.clone().or_else(|| {
            request
                .subscription
                .as_ref()
                .and_then(|value| serde_json::from_value(value.get("auth")?.clone()).ok())
        });
        let new_subscription = NewSubscription {
            url,
            display_name: request.display_name,
            color: request.color,
            refresh_interval_seconds: interval,
            auth,
        };
        return match state
            .subscriptions
            .create(&session.user, &slug, new_subscription)
            .await
        {
            Ok(meta) => api_json(StatusCode::CREATED, collection_json(state, &meta, false)),
            Err(error) => api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                &subscription_error(&error),
            ),
        };
    }
    let name = request
        .display_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Untitled".to_string());
    let slug = request
        .slug
        .clone()
        .unwrap_or_else(|| unique_slug(state, &session.user, &name));
    let meta = match state
        .store
        .create_collection(&session.user, kind, &slug, &name)
        .await
    {
        Ok(meta) => meta,
        Err(error) => return api_error(StatusCode::CONFLICT, &error.to_string()),
    };
    if request.description.is_some() || request.color.is_some() {
        let path = CollectionPath::parse(session.user.as_str(), &meta.slug).unwrap();
        let _ = state
            .store
            .update_collection(
                &path,
                CollectionUpdate {
                    description: request.description,
                    color: request.color,
                    ..Default::default()
                },
            )
            .await;
    }
    let meta = state
        .store
        .collection(&CollectionPath::parse(session.user.as_str(), &meta.slug).unwrap())
        .unwrap_or(meta);
    api_json(StatusCode::CREATED, collection_json(state, &meta, false))
}

fn find_collection(
    state: &AppState,
    session: &crate::sessions::Session,
    id: &str,
) -> Result<CollectionMeta, Response> {
    state
        .store
        .list_collections(&session.user)
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "storage error"))?
        .into_iter()
        .find(|meta| meta.id == id)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "collection not found"))
}

fn api_get_collection(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
) -> Response {
    match find_collection(state, session, id) {
        Ok(meta) => api_json(StatusCode::OK, collection_json(state, &meta, true)),
        Err(response) => response,
    }
}

async fn api_patch_collection(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
    body: &[u8],
) -> Response {
    let request: PatchCollectionRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid request"),
    };
    let meta = match find_collection(state, session, id) {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let path = match CollectionPath::parse(session.user.as_str(), &meta.slug) {
        Ok(path) => path,
        Err(_) => return api_error(StatusCode::NOT_FOUND, "collection not found"),
    };
    match state
        .store
        .update_collection(
            &path,
            CollectionUpdate {
                display_name: request.display_name,
                description: request.description,
                color: request.color,
                ..Default::default()
            },
        )
        .await
    {
        Ok(meta) => api_json(StatusCode::OK, collection_json(state, &meta, true)),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, &error.to_string()),
    }
}

async fn api_delete_collection(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
) -> Response {
    let meta = match find_collection(state, session, id) {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let path = match CollectionPath::parse(session.user.as_str(), &meta.slug) {
        Ok(path) => path,
        Err(_) => return api_error(StatusCode::NOT_FOUND, "collection not found"),
    };
    match state.store.delete_collection(&path, None).await {
        Ok(()) => api_json(StatusCode::OK, json!({"deleted": true})),
        Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn api_refresh_subscription(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
) -> Response {
    let meta = match find_collection(state, session, id) {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let path = match CollectionPath::parse(session.user.as_str(), &meta.slug) {
        Ok(path) => path,
        Err(_) => return api_error(StatusCode::NOT_FOUND, "collection not found"),
    };
    if meta.subscription.is_none() {
        return api_error(StatusCode::UNPROCESSABLE_ENTITY, "not a subscription");
    }
    match state.subscriptions.refresh(&path).await {
        Ok(RefreshOutcome::Changed) => api_json(
            StatusCode::OK,
            json!({"status": "changed", "collection": collection_json(state, &state.store.collection(&path).unwrap_or(meta), true)}),
        ),
        Ok(RefreshOutcome::NotModified) => {
            api_json(StatusCode::OK, json!({"status": "not-modified"}))
        }
        Ok(RefreshOutcome::Degraded(error)) => api_json(
            StatusCode::OK,
            json!({"status": "degraded", "error": error}),
        ),
        Err(error) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &subscription_error(&error),
        ),
    }
}

fn api_list_feeds(state: &Arc<AppState>, session: &crate::sessions::Session, id: &str) -> Response {
    let meta = match find_collection(state, session, id) {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let feeds: Vec<Value> = meta
        .feeds
        .iter()
        .map(|feed| {
            json!({
                "id": feed.id,
                "name": feed.name,
                "components": feed.components.as_strings(),
                "token_hint": feed.token_hint,
                "masked_url": state.feeds.masked_url(feed),
                "created_at": feed.created_at,
                "expires_at": feed.expires_at,
                "revoked_at": feed.revoked_at,
                "last_used_at": feed.last_used_at,
                "active": feed.is_active(Utc::now()),
            })
        })
        .collect();
    api_json(StatusCode::OK, json!({"feeds": feeds}))
}

async fn api_create_feed(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
    body: &[u8],
) -> Response {
    let request: CreateFeedRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid request"),
    };
    let mut components = ComponentSet::empty();
    for value in &request.components {
        match value.parse::<CalendarComponent>() {
            Ok(component) => components.insert(component),
            Err(_) => return api_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid component"),
        }
    }
    match state
        .feeds
        .create(
            &session.user,
            id,
            &request.name,
            components,
            request.expires_at,
        )
        .await
    {
        Ok(created) => api_json(
            StatusCode::CREATED,
            json!({
                "id": created.feed.id,
                "name": created.feed.name,
                "components": created.feed.components.as_strings(),
                "url": created.url,
                "token_hint": created.feed.token_hint,
                "created_at": created.feed.created_at,
                "expires_at": created.feed.expires_at,
            }),
        ),
        Err(error) => api_error(StatusCode::UNPROCESSABLE_ENTITY, &error.to_string()),
    }
}

async fn api_revoke_feed(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
    feed_id: &str,
) -> Response {
    match state.feeds.revoke(&session.user, id, feed_id).await {
        Ok(_) => api_json(StatusCode::OK, json!({"revoked": true})),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

async fn api_rotate_feed(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
    feed_id: &str,
) -> Response {
    match state.feeds.rotate(&session.user, id, feed_id).await {
        Ok(created) => api_json(
            StatusCode::OK,
            json!({"id": created.feed.id, "url": created.url, "token_hint": created.feed.token_hint}),
        ),
        Err(error) => api_error(StatusCode::NOT_FOUND, &error.to_string()),
    }
}

fn api_export_collection(
    state: &Arc<AppState>,
    session: &crate::sessions::Session,
    id: &str,
) -> Response {
    let meta = match find_collection(state, session, id) {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let path = match CollectionPath::parse(session.user.as_str(), &meta.slug) {
        Ok(path) => path,
        Err(_) => return api_error(StatusCode::NOT_FOUND, "collection not found"),
    };
    let items = match state.store.list_items(&path) {
        Ok(items) => items,
        Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    match meta.kind {
        CollectionKind::Calendar => {
            let payloads: Vec<String> = items.into_iter().map(|item| item.payload).collect();
            match oxicale_ical::generate_feed(
                &payloads,
                ComponentSet::all(),
                Some(&meta.display_name),
                &format!("-//Oxicale//Oxicale {VERSION}//EN"),
                oxicale_ical::Limits::default(),
            ) {
                Ok(body) => Response::builder()
                    .status(StatusCode::OK)
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        "text/calendar; charset=utf-8",
                    )
                    .header(
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}.ics\"", meta.slug),
                    )
                    .body(Body::from(body))
                    .unwrap(),
                Err(error) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
            }
        }
        CollectionKind::AddressBook => {
            let body: String = items.into_iter().map(|item| item.payload).collect();
            Response::builder()
                .status(StatusCode::OK)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "text/vcard; charset=utf-8",
                )
                .header(
                    axum::http::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}.vcf\"", meta.slug),
                )
                .body(Body::from(body))
                .unwrap()
        }
    }
}

fn collection_json(state: &AppState, meta: &CollectionMeta, details: bool) -> Value {
    let path = CollectionPath::parse(&meta.owner, &meta.slug).ok();
    let items = path
        .as_ref()
        .and_then(|path| state.store.list_items(path).ok())
        .unwrap_or_default();
    let mut value = json!({
        "id": meta.id,
        "kind": meta.kind.as_str(),
        "slug": meta.slug,
        "display_name": meta.display_name,
        "description": meta.description,
        "color": meta.color,
        "components": meta.components.as_strings(),
        "url": public_url(state, &format!("{}/{}/", meta.owner, meta.slug)),
        "item_counts": {
            "events": items.iter().filter(|i| i.component == oxicale_core::ItemComponent::VEvent).count(),
            "tasks": items.iter().filter(|i| i.component == oxicale_core::ItemComponent::VTodo).count(),
            "journals": items.iter().filter(|i| i.component == oxicale_core::ItemComponent::VJournal).count(),
            "contacts": items.iter().filter(|i| i.component == oxicale_core::ItemComponent::VCard).count(),
        },
        "read_only": meta.is_read_only(),
    });
    if details {
        if let Some(object) = value.as_object_mut() {
            object.insert("created_at".into(), json!(meta.created_at));
            object.insert("updated_at".into(), json!(meta.updated_at));
            object.insert("revision".into(), json!(meta.revision));
            if let Some(source) = &meta.subscription {
                object.insert(
                    "subscription".into(),
                    json!({
                        "url": source.url,
                        "original_url": source.original_url,
                        "refresh_interval_seconds": source.refresh_interval_seconds,
                        "last_attempt_at": source.last_attempt_at,
                        "last_success_at": source.last_success_at,
                        "next_attempt_at": source.next_attempt_at,
                        "last_error": source.last_error,
                        "consecutive_failures": source.consecutive_failures,
                        "credentials_set": source.auth.is_some(),
                    }),
                );
            }
        }
    }
    value
}

fn api_json(status: StatusCode, value: Value) -> Response {
    Response::builder()
        .status(status)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        .body(Body::from(value.to_string()))
        .unwrap()
}

fn api_error(status: StatusCode, message: &str) -> Response {
    api_json(status, json!({"error": {"message": message}}))
}

fn api_not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, "not found")
}

const STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#f5f7fb;--fg:#15202b;--card:#fff;--accent:#2563eb;--muted:#5b6875;--error:#b91c1c;--border:#d7dee8}
@media (prefers-color-scheme:dark){:root{--bg:#111821;--fg:#e7edf5;--card:#182230;--accent:#60a5fa;--muted:#9fb0c2;--error:#f87171;--border:#2c3b4d}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 system-ui,sans-serif}
a{color:var(--accent)}header{background:var(--card);border-bottom:1px solid var(--border)}nav{max-width:1100px;margin:auto;display:flex;gap:1rem;align-items:center;padding:.8rem 1rem;flex-wrap:wrap}
.brand{font-weight:700;font-size:1.2rem;text-decoration:none}.spacer{flex:1}.shell{max-width:1100px;margin:auto;padding:1.5rem 1rem}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:1rem}.card{background:var(--card);border:1px solid var(--border);border-radius:12px;padding:1rem;margin:1rem 0}
.narrow{max-width:430px;margin:3rem auto}label{display:block;margin:.7rem 0 .25rem;font-weight:600}input,select,textarea,button{font:inherit;padding:.55rem;border-radius:8px;border:1px solid var(--border);background:var(--bg);color:var(--fg);max-width:100%}
input[type=checkbox]{width:auto}button,.button{background:var(--accent);color:#fff;border:none;padding:.55rem .9rem;border-radius:8px;cursor:pointer;text-decoration:none;display:inline-block;margin-top:.6rem}
button.danger{background:var(--error)}form.inline{display:inline}fieldset{border:1px solid var(--border);border-radius:8px;margin:.6rem 0}
.swatch{width:2.2rem;height:2.2rem;border-radius:50%;display:inline-block;margin-bottom:.4rem}.badge{display:inline-block;border:1px solid var(--border);border-radius:999px;padding:.1rem .5rem;font-size:.8rem;margin-right:.3rem}
.muted{color:var(--muted)}.error{color:var(--error);background:color-mix(in srgb,var(--error) 10%,transparent);padding:.7rem;border-radius:8px;margin:.7rem 0}
.warning{background:#fef3c7;color:#78350f;padding:.7rem;border-radius:8px}.secret{outline:2px solid var(--accent)}
code{overflow-wrap:anywhere}footer{text-align:center;color:var(--muted);padding:2rem 1rem}dt{font-weight:700}dd{margin:0 0 .6rem}
details{margin:1rem 0}summary{cursor:pointer;font-weight:700}.feed{border-top:1px solid var(--border);padding:1rem 0}
@media(max-width:600px){.shell{padding:1rem}nav{gap:.6rem}}
"#;
