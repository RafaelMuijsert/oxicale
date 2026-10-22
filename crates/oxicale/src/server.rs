use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use oxicale_core::UserName;
use oxicale_storage::{DefaultCollection, Store, StoreOptions};

use crate::auth::{AuthError, AuthService, Backend, PasswordFile};
use crate::config::{AuthBackend, Config};
use crate::crypto::SecretBox;
use crate::dav;
use crate::feeds::FeedService;
use crate::rights::Policy;
use crate::sessions::{Session, SessionStore};
use crate::subscriptions::{HttpFetcher, SubscriptionService};
use crate::web;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
pub struct Metrics {
    pub http_requests: AtomicU64,
    pub http_errors: AtomicU64,
    pub storage_operations: AtomicU64,
    pub subscription_refreshes: AtomicU64,
    pub subscription_failures: AtomicU64,
    pub feed_requests: AtomicU64,
    pub feed_failures: AtomicU64,
}

impl Metrics {
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# TYPE oxicale_http_requests_total counter\n");
        out.push_str(&format!(
            "oxicale_http_requests_total {}\n",
            self.http_requests.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE oxicale_http_errors_total counter\n");
        out.push_str(&format!(
            "oxicale_http_errors_total {}\n",
            self.http_errors.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE oxicale_storage_operations_total counter\n");
        out.push_str(&format!(
            "oxicale_storage_operations_total {}\n",
            self.storage_operations.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE oxicale_subscription_refresh_total counter\n");
        out.push_str(&format!(
            "oxicale_subscription_refresh_total {}\n",
            self.subscription_refreshes.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE oxicale_subscription_failure_total counter\n");
        out.push_str(&format!(
            "oxicale_subscription_failure_total {}\n",
            self.subscription_failures.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE oxicale_feed_requests_total counter\n");
        out.push_str(&format!(
            "oxicale_feed_requests_total {}\n",
            self.feed_requests.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE oxicale_feed_failure_total counter\n");
        out.push_str(&format!(
            "oxicale_feed_failure_total {}\n",
            self.feed_failures.load(Ordering::Relaxed)
        ));
        out
    }
}

pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub auth: Arc<AuthService>,
    pub sessions: Arc<SessionStore>,
    pub feeds: Arc<FeedService>,
    pub subscriptions: Arc<SubscriptionService>,
    pub secrets: Arc<SecretBox>,
    pub rights: Policy,
    pub metrics: Arc<Metrics>,
    pub feed_limiter: Arc<crate::auth::RateLimiter>,
    pub version: String,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("version", &self.version)
            .finish()
    }
}

impl AppState {
    pub fn session_cookie_name(&self) -> &str {
        &self.config.auth.session_cookie_name
    }

    pub fn session_from_headers(&self, headers: &HeaderMap) -> Option<Session> {
        let cookie = headers
            .get(axum::http::header::COOKIE)
            .and_then(|value| value.to_str().ok())?;
        let name = self.session_cookie_name();
        for part in cookie.split(';') {
            let (key, value) = part.trim().split_once('=')?;
            if key == name {
                return self.sessions.get(value);
            }
        }
        None
    }

    pub fn session_cookie(&self, session: &Session) -> String {
        let secure = if self.config.auth.secure_cookies || self.config.server.tls {
            "; Secure"
        } else {
            ""
        };
        format!(
            "{}={}; Path={}; HttpOnly; SameSite=Lax{secure}",
            self.session_cookie_name(),
            session.id,
            self.config.server.normalized_base_path()
        )
    }

    pub fn clear_session_cookie(&self) -> String {
        let secure = if self.config.auth.secure_cookies || self.config.server.tls {
            "; Secure"
        } else {
            ""
        };
        format!(
            "{}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax{secure}",
            self.session_cookie_name(),
            self.config.server.normalized_base_path()
        )
    }

    pub fn authenticate(&self, headers: &HeaderMap, peer: IpAddr) -> Result<UserName, AuthError> {
        let authorization = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        let backend = self.auth.config().backend;
        if backend == Backend::TrustedHeader {
            let header_name = self.auth.config().trusted_header.clone();
            let value = headers
                .get(header_name.as_str())
                .and_then(|value| value.to_str().ok());
            return self.auth.authenticate_trusted_header(value, peer);
        }
        if backend == Backend::None {
            return self.auth.authenticate_basic(None, peer);
        }
        if backend == Backend::Deny {
            return Err(AuthError::Invalid);
        }
        if authorization.is_some() {
            return self.auth.authenticate_basic(authorization, peer);
        }
        if let Some(session) = self.session_from_headers(headers) {
            return Ok(session.user);
        }
        self.auth.authenticate_basic(None, peer)
    }

    pub async fn provision_account(
        &self,
        user: &UserName,
    ) -> Result<(), oxicale_storage::StorageError> {
        let calendar = if self.config.calendar.create_default {
            Some(DefaultCollection {
                slug: self.config.calendar.default_slug.clone(),
                name: self.config.calendar.default_name.clone(),
            })
        } else {
            None
        };
        let addressbook =
            if self.config.addressbook.enabled && self.config.addressbook.create_default {
                Some(DefaultCollection {
                    slug: self.config.addressbook.default_slug.clone(),
                    name: self.config.addressbook.default_name.clone(),
                })
            } else {
                None
            };
        self.store
            .provision_defaults(user, calendar.as_ref(), addressbook.as_ref())
            .await?;
        Ok(())
    }

    pub fn permissions_for(
        &self,
        current: &UserName,
        owner: &UserName,
        read_only: bool,
    ) -> oxicale_core::Permissions {
        let permissions = self.rights.permissions(current, owner);
        if read_only {
            crate::rights::apply_read_only(permissions)
        } else {
            permissions
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let limit = state.config.server.max_connections.max(1);
    Router::new()
        .fallback(dispatch)
        .layer(tower::limit::ConcurrencyLimitLayer::new(limit))
        .with_state(state)
}

async fn dispatch(State(state): State<Arc<AppState>>, request: Request) -> Response {
    state.metrics.http_requests.fetch_add(1, Ordering::Relaxed);
    let timeout = state.config.server.request_timeout.std();
    let response = if timeout.is_zero() {
        dispatch_inner(state.clone(), request).await
    } else {
        match tokio::time::timeout(timeout, dispatch_inner(state.clone(), request)).await {
            Ok(response) => response,
            Err(_) => simple_response(
                StatusCode::REQUEST_TIMEOUT,
                "text/plain; charset=utf-8",
                "request timed out",
            ),
        }
    };
    if response.status().is_client_error() || response.status().is_server_error() {
        state.metrics.http_errors.fetch_add(1, Ordering::Relaxed);
    }
    response
}

async fn dispatch_inner(state: Arc<AppState>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    if state.config.web.require_https && !state.config.server.tls {
        let peer = peer_from_request(&request);
        let forwarded_https = request
            .headers()
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
            && state.config.peer_is_trusted(peer);
        if !forwarded_https {
            let external = state.config.external_url();
            if external.starts_with("https://") {
                let location = format!("{external}{path}");
                return Response::builder()
                    .status(StatusCode::PERMANENT_REDIRECT)
                    .header(axum::http::header::LOCATION, location)
                    .body(Body::empty())
                    .unwrap();
            }
            return simple_response(
                StatusCode::BAD_REQUEST,
                "text/plain",
                "web.require_https is enabled but external_url is not https",
            );
        }
    }
    if path == "/healthz" || path == "/healthz/" {
        return simple_response(StatusCode::OK, "text/plain; charset=utf-8", "ok");
    }
    if path == "/readyz" || path == "/readyz/" {
        let report = state.store.verify_storage();
        return match report {
            Ok(report) if report.errors.is_empty() => {
                simple_response(StatusCode::OK, "text/plain; charset=utf-8", "ready")
            }
            _ => simple_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "text/plain; charset=utf-8",
                "not ready",
            ),
        };
    }
    let base = state.config.server.normalized_base_path();
    let base_trimmed = base.trim_end_matches('/');
    let relative = if base_trimmed.is_empty() || base_trimmed == "/" {
        path.as_str()
    } else if let Some(rest) = path.strip_prefix(base_trimmed) {
        if rest.is_empty() {
            "/"
        } else if rest.starts_with('/') {
            rest
        } else {
            return not_found();
        }
    } else {
        return not_found();
    };

    if (relative.is_empty() || relative == "/") && request.method() == axum::http::Method::GET {
        let location = format!("{}.oxicale/dashboard", base);
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(axum::http::header::LOCATION, location)
            .body(Body::empty())
            .unwrap();
    }

    let segments = match oxicale_core::normalize_path(relative) {
        Ok(segments) => segments,
        Err(_) => return simple_response(StatusCode::BAD_REQUEST, "text/plain", "bad path"),
    };

    if segments.first().map(String::as_str) == Some("feeds") {
        return crate::feeds::handle_feed(state, request, &segments).await;
    }
    if segments.first().map(String::as_str) == Some(".oxicale") {
        return web::handle_web(state, request, &segments).await;
    }
    dav::handle_dav(state, request, &segments).await
}

pub fn simple_response(status: StatusCode, content_type: &str, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub fn not_found() -> Response {
    simple_response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        "not found",
    )
}

pub async fn run(config: Config) -> Result<(), String> {
    let warnings = config.validate()?;
    for warning in &warnings {
        tracing::warn!("{warning}");
    }
    let data_dir = config.storage.data_dir.clone();
    std::fs::create_dir_all(&data_dir).map_err(|error| {
        format!(
            "cannot create data directory {}: {error}",
            data_dir.display()
        )
    })?;
    let store = Arc::new(
        Store::open_with_options(
            &data_dir,
            StoreOptions {
                fsync: config.storage.fsync,
                max_calendar_item_size: config.server.max_item_size.bytes(),
                max_contact_item_size: config.server.max_addressbook_item_size.bytes(),
                default_timezone: config
                    .calendar
                    .default_timezone
                    .parse()
                    .unwrap_or(chrono_tz::UTC),
                index_path: Some(config.storage.resolved_index_path()),
                lock_timeout: config.storage.lock_timeout.std(),
                max_collections_per_user: config.limits.max_collections_per_user,
                max_items_per_collection: config.limits.max_items_per_collection,
                max_principals: config.limits.max_principals,
                sync_history_days: config.storage.sync_history_days,
                sync_history_max_changes: config.storage.sync_history_max_changes,
            },
        )
        .map_err(|error| error.to_string())?,
    );
    let key_file = config.storage.resolved_secret_key_file();
    let (secrets, key_warning) = SecretBox::load(
        std::env::var("OXICALE_SECRET_KEY").ok().as_deref(),
        &key_file,
    )
    .map_err(|error| error.to_string())?;
    if let Some(warning) = key_warning {
        tracing::warn!("{warning}");
    }
    let secrets = Arc::new(secrets);
    let auth = Arc::new(AuthService::new(crate::auth::AuthConfig::from_config(
        &config,
    )));
    let sessions = Arc::new(SessionStore::new(config.auth.session_lifetime.std()));
    let metrics = Arc::new(Metrics::default());
    let feeds = Arc::new(FeedService::new(
        store.clone(),
        config.feeds.clone(),
        config.external_url(),
        config.server.normalized_base_path(),
        VERSION.to_string(),
    ));
    let fetcher: Arc<dyn crate::subscriptions::RemoteFetcher> =
        Arc::new(HttpFetcher::new(&config.subscriptions).map_err(|error| error.to_string())?);
    let subscriptions = Arc::new(SubscriptionService::new(
        store.clone(),
        fetcher,
        secrets.clone(),
        config.subscriptions.clone(),
        metrics.clone(),
    ));
    let feed_limiter = Arc::new(crate::auth::RateLimiter::new(
        config.feeds.rate_limit_per_minute.max(1),
        std::time::Duration::from_secs(60),
    ));
    let state = Arc::new(AppState {
        rights: Policy::from_config(&config),
        config: Arc::new(config.clone()),
        store,
        auth,
        sessions,
        feeds,
        subscriptions: subscriptions.clone(),
        secrets,
        metrics: metrics.clone(),
        feed_limiter,
        version: VERSION.to_string(),
    });
    let app = build_router(state);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let scheduler = crate::subscriptions::spawn_scheduler(subscriptions, shutdown_rx);
    let tls = if config.server.tls {
        Some(
            axum_server::tls_rustls::RustlsConfig::from_pem_file(
                &config.server.tls_certificate,
                &config.server.tls_key,
            )
            .await
            .map_err(|error| format!("cannot load TLS configuration: {error}"))?,
        )
    } else {
        None
    };
    let mut handles = Vec::new();
    for bind in &config.server.bind {
        let address: SocketAddr = bind
            .parse()
            .map_err(|error| format!("invalid bind address {bind}: {error}"))?;
        let app = app.clone();
        let tls = tls.clone();
        let handle = tokio::spawn(async move {
            if let Some(tls) = tls {
                axum_server::bind_rustls(address, tls)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            } else {
                axum_server::bind(address)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
        });
        handles.push(handle);
    }
    let mut metrics_handle = None;
    if config.metrics.enabled {
        let address: SocketAddr = config
            .metrics
            .bind
            .parse()
            .map_err(|error| format!("invalid metrics bind address: {error}"))?;
        let metrics = metrics.clone();
        let metrics_path = config.metrics.path.clone();
        let router = Router::new().route(
            &metrics_path,
            axum::routing::get(move || {
                let metrics = metrics.clone();
                async move {
                    simple_response(
                        StatusCode::OK,
                        "text/plain; version=0.0.4",
                        &metrics.render(),
                    )
                }
            }),
        );
        metrics_handle = Some(tokio::spawn(async move {
            let _ = axum_server::bind(address)
                .serve(router.into_make_service())
                .await;
        }));
    }
    tracing::info!("listening on {}", config.server.bind.join(", "));
    tokio::signal::ctrl_c()
        .await
        .map_err(|error| error.to_string())?;
    let _ = shutdown_tx.send(true);
    for handle in handles {
        handle.abort();
    }
    scheduler.abort();
    if let Some(handle) = metrics_handle {
        handle.abort();
    }
    Ok(())
}

pub fn default_password_file(config: &Config) -> std::path::PathBuf {
    config.auth.password_file.clone()
}

pub fn password_file_from_config(config: &Config) -> Result<PasswordFile, String> {
    PasswordFile::load(default_password_file(config)).map_err(|error| error.to_string())
}

pub fn auth_backend_name(config: &Config) -> &'static str {
    match config.auth.backend {
        AuthBackend::Local => "local",
        AuthBackend::TrustedHeader => "trusted_header",
        AuthBackend::None => "none",
        AuthBackend::Deny => "deny",
    }
}

pub fn peer_from_request(request: &Request) -> IpAddr {
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or_else(|| IpAddr::from([127, 0, 0, 1]))
}

pub fn secure_headers(mut response: Response, config: &Config) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        axum::http::header::X_FRAME_OPTIONS,
        HeaderValue::from_static("DENY"),
    );
    let csp = format!(
        "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'self'; form-action 'self' {}",
        config.web.csp_extra
    );
    if let Ok(value) = HeaderValue::from_str(&csp) {
        headers.insert(axum::http::header::CONTENT_SECURITY_POLICY, value);
    }
    if config.server.tls {
        headers.insert(
            axum::http::header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}
