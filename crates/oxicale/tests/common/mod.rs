#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use axum::Router;
use base64::Engine;
use oxicale::auth::{AuthConfig, AuthService, PasswordFile};
use oxicale::config::Config;
use oxicale::crypto::SecretBox;
use oxicale::feeds::FeedService;
use oxicale::server::{build_router, AppState, Metrics, VERSION};
use oxicale::sessions::SessionStore;
use oxicale::subscriptions::{
    FetchResponse, RemoteCredentials, RemoteFetcher, SubscriptionError, SubscriptionService,
};
use oxicale_core::UserName;
use oxicale_storage::{Store, StoreOptions};
use tempfile::TempDir;
use tower::ServiceExt;

pub struct NoopFetcher;

#[async_trait]
impl RemoteFetcher for NoopFetcher {
    async fn fetch(
        &self,
        _url: &str,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
        _credentials: Option<&RemoteCredentials>,
    ) -> Result<FetchResponse, SubscriptionError> {
        Err(SubscriptionError::Unreachable)
    }
}

pub struct MockFetcher {
    responses: Mutex<VecDeque<Result<FetchResponse, SubscriptionError>>>,
    calls: Mutex<Vec<FetchCall>>,
}

#[derive(Clone, Debug)]
pub struct FetchCall {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub credentials: Option<RemoteCredentials>,
}

impl MockFetcher {
    pub fn new(responses: Vec<Result<FetchResponse, SubscriptionError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<FetchCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl RemoteFetcher for MockFetcher {
    async fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        credentials: Option<&RemoteCredentials>,
    ) -> Result<FetchResponse, SubscriptionError> {
        self.calls.lock().unwrap().push(FetchCall {
            url: url.to_string(),
            etag: etag.map(str::to_string),
            last_modified: last_modified.map(str::to_string),
            credentials: credentials.cloned(),
        });
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(SubscriptionError::Unreachable))
    }
}

pub struct TestApp {
    pub dir: TempDir,
    pub config: Config,
    pub state: Arc<AppState>,
    pub router: Router,
    pub username: UserName,
    pub password: String,
}

impl TestApp {
    pub fn new() -> Self {
        Self::with_config(|config| {
            config.server.external_url = Some("https://example.test".to_string());
        })
    }

    pub fn with_config(configure: impl FnOnce(&mut Config)) -> Self {
        let dir = TempDir::new().unwrap();
        let mut config = Config::default();
        config.storage.data_dir = dir.path().join("data");
        config.auth.password_file = dir.path().join("users");
        config.auth.secure_cookies = false;
        config.auth.rate_limit_failures = 100;
        config.server.bind = vec!["127.0.0.1:0".to_string()];
        config.server.external_url = Some("https://example.test".to_string());
        configure(&mut config);
        std::fs::create_dir_all(&config.storage.data_dir).unwrap();
        let password = "correct horse".to_string();
        oxicale::auth::upsert_password_file(&config.auth.password_file, "alice", &password)
            .unwrap();
        let store = Arc::new(
            Store::open_with_options(
                &config.storage.data_dir,
                StoreOptions {
                    fsync: false,
                    max_calendar_item_size: config.server.max_item_size.bytes(),
                    max_contact_item_size: config.server.max_addressbook_item_size.bytes(),
                    default_timezone: chrono_tz::UTC,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let username = UserName::parse("alice").unwrap();
        let secrets = Arc::new(
            SecretBox::load(
                Some(&base64::engine::general_purpose::STANDARD.encode([9u8; 32])),
                &dir.path().join("key"),
            )
            .unwrap()
            .0,
        );
        let auth = Arc::new(AuthService::new(AuthConfig::from_config(&config)));
        let sessions = Arc::new(SessionStore::new(config.auth.session_lifetime.std()));
        let feeds = Arc::new(FeedService::new(
            store.clone(),
            config.feeds.clone(),
            config.external_url(),
            config.server.normalized_base_path(),
            VERSION.to_string(),
        ));
        let subscriptions = Arc::new(SubscriptionService::new(
            store.clone(),
            Arc::new(NoopFetcher),
            secrets.clone(),
            config.subscriptions.clone(),
            Arc::new(oxicale::server::Metrics::default()),
        ));
        let state = Arc::new(AppState {
            config: Arc::new(config.clone()),
            store,
            auth,
            sessions,
            feeds,
            subscriptions,
            secrets,
            rights: oxicale::rights::Policy::from_config(&config),
            metrics: Arc::new(Metrics::default()),
            feed_limiter: Arc::new(oxicale::auth::RateLimiter::new(
                config.feeds.rate_limit_per_minute.max(1),
                std::time::Duration::from_secs(60),
            )),
            version: VERSION.to_string(),
        });
        let router = build_router(state.clone());
        Self {
            dir,
            config,
            state,
            router,
            username,
            password,
        }
    }

    pub fn password_file(&self) -> PasswordFile {
        PasswordFile::load(&self.config.auth.password_file).unwrap()
    }

    pub fn basic(&self) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                self.username.as_str(),
                self.password
            ))
        )
    }

    pub async fn provision(&self) {
        let state = self.state.clone();
        state.provision_account(&self.username).await.unwrap();
    }

    pub async fn request(&self, request: Request<Body>) -> Response<Body> {
        self.router.clone().oneshot(request).await.unwrap()
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        headers: HeaderMap,
        body: impl Into<Body>,
    ) -> Response<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(axum::http::header::HOST, "example.test");
        for (name, value) in headers.iter() {
            request = request.header(name, value);
        }
        let mut request = request.body(body.into()).unwrap();
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::from([127, 0, 0, 1]),
            12345,
        )));
        self.request(request).await
    }

    pub async fn dav(
        &self,
        method: &str,
        path: &str,
        extra: HeaderMap,
        body: impl Into<Body>,
    ) -> Response<Body> {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&self.basic()).unwrap(),
        );
        headers.extend(extra);
        self.call(
            Method::from_bytes(method.as_bytes()).unwrap(),
            path,
            headers,
            body,
        )
        .await
    }

    pub async fn dav_body(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: impl Into<Body>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut headers = HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_str(content_type).unwrap(),
            );
        }
        let response = self.dav(method, path, headers, body).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }
}

pub fn header(name: &str, value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
        HeaderValue::from_str(value).unwrap(),
    );
    headers
}

pub fn body_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

pub fn cookie_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::to_string)
}

pub const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nSUMMARY:Team Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
pub const EVENT_2: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-2\r\nDTSTART:20261023T100000Z\r\nDTEND:20261023T110000Z\r\nSUMMARY:Lunch\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
pub const TODO: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:todo-1\r\nDUE:20261024T100000Z\r\nSUMMARY:Task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
pub const JOURNAL: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VJOURNAL\r\nUID:journal-1\r\nDTSTART:20261025T100000Z\r\nSUMMARY:Journal\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n";
pub const VCARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:card-1\r\nFN:Alice Example\r\nEMAIL:alice@example.com\r\nEND:VCARD\r\n";
