use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use oxicale_core::{CollectionPath, UserName};
use oxicale_ical::{parse_calendar, split_calendar_with_options, Limits};
use oxicale_storage::{CollectionMeta, RemoteAuth, StorageError, Store, SubscriptionSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::config::SubscriptionsConfig;
use crate::crypto::SecretBox;
use crate::server::Metrics;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SubscriptionError {
    #[error("invalid subscription URL: {0}")]
    InvalidUrl(String),
    #[error("the URL points to a blocked network address")]
    BlockedAddress,
    #[error("the URL host is not allowed")]
    HostDenied,
    #[error("the URL scheme is not allowed")]
    SchemeDenied,
    #[error("the remote calendar is unreachable")]
    Unreachable,
    #[error("the remote server returned {0}")]
    Http(u16),
    #[error("the response is not a valid iCalendar file")]
    InvalidCalendar,
    #[error("the calendar is too large")]
    TooLarge,
    #[error("subscription limit reached")]
    LimitReached,
    #[error("subscription storage error: {0}")]
    Storage(String),
    #[error("subscription authentication is no longer decryptable")]
    CredentialsUnavailable,
    #[error("subscriptions are disabled")]
    Disabled,
}

impl From<StorageError> for SubscriptionError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedUrl {
    pub url: Url,
    pub normalized: String,
    pub host: String,
    pub addresses: Vec<SocketAddr>,
}

#[derive(Clone, Debug)]
pub struct UrlPolicy {
    allow_http: bool,
    allow_private_networks: bool,
    allow_loopback: bool,
    allowed_hosts: Vec<String>,
    denied_hosts: Vec<String>,
}

impl UrlPolicy {
    pub fn new(config: &SubscriptionsConfig) -> Self {
        Self {
            allow_http: config.allow_http,
            allow_private_networks: config.allow_private_networks,
            allow_loopback: config.allow_loopback,
            allowed_hosts: config.allowed_hosts.clone(),
            denied_hosts: config.denied_hosts.clone(),
        }
    }

    pub async fn validate(&self, input: &str) -> Result<ValidatedUrl, SubscriptionError> {
        if input.len() > 4096 {
            return Err(SubscriptionError::InvalidUrl("URL is too long".into()));
        }
        let trimmed = input.trim();
        let normalized_input = if let Some(rest) = trimmed.strip_prefix("webcal://") {
            format!("https://{rest}")
        } else {
            trimmed.to_string()
        };
        let url = Url::parse(&normalized_input)
            .map_err(|_| SubscriptionError::InvalidUrl("malformed URL".into()))?;
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            "http" => return Err(SubscriptionError::SchemeDenied),
            _ => return Err(SubscriptionError::SchemeDenied),
        }
        if url.username() != "" || url.password().is_some() {
            return Err(SubscriptionError::InvalidUrl(
                "credentials must be supplied separately".into(),
            ));
        }
        if !url.has_host() {
            return Err(SubscriptionError::InvalidUrl("URL has no host".into()));
        }
        if let Some(port) = url.port() {
            let default = if url.scheme() == "http" { 80 } else { 443 };
            if port != default {
                return Err(SubscriptionError::InvalidUrl(
                    "non-default ports are not allowed".into(),
                ));
            }
        }
        let host = url
            .host_str()
            .ok_or_else(|| SubscriptionError::InvalidUrl("URL has no host".into()))?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(SubscriptionError::InvalidUrl("invalid host".into()));
        }
        if !self.host_allowed(&host) {
            return Err(SubscriptionError::HostDenied);
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| SubscriptionError::InvalidUrl("unknown port".into()))?;
        let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            let resolved = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| SubscriptionError::Unreachable)?
                .collect::<Vec<_>>();
            if resolved.is_empty() {
                return Err(SubscriptionError::Unreachable);
            }
            resolved
        };
        for address in &addresses {
            if !self.ip_allowed(address.ip()) {
                return Err(SubscriptionError::BlockedAddress);
            }
        }
        Ok(ValidatedUrl {
            url,
            normalized: normalized_input,
            host,
            addresses,
        })
    }

    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
        if self
            .denied_hosts
            .iter()
            .any(|pattern| host_matches(pattern, &host))
        {
            return false;
        }
        if self.allowed_hosts.is_empty() {
            return true;
        }
        self.allowed_hosts
            .iter()
            .any(|pattern| host_matches(pattern, &host))
    }

    pub fn ip_allowed(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    IpAddr::V4(v4)
                } else {
                    IpAddr::V6(v6)
                }
            }
            other => other,
        };
        if is_metadata(ip) || is_link_local(ip) {
            return false;
        }
        if ip.is_loopback() {
            return self.allow_loopback;
        }
        if is_private(ip) {
            return self.allow_private_networks;
        }
        if is_forbidden(ip) {
            return false;
        }
        true
    }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        pattern == host
    }
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || in_v4(v4, [100, 64, 0, 0], 10),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => in_v6(v6, [0xfe80, 0, 0, 0], 10),
    }
}

fn is_metadata(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4 == Ipv4Addr::new(169, 254, 169, 254),
        IpAddr::V6(v6) => {
            v6 == "fd00:ec2::254".parse::<Ipv6Addr>().unwrap()
                || v6 == "fe80::a9fe:a9fe".parse::<Ipv6Addr>().unwrap()
        }
    }
}

fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_link_local()
                || in_v4(v4, [192, 0, 2, 0], 24)
                || in_v4(v4, [198, 51, 100, 0], 24)
                || in_v4(v4, [203, 0, 113, 0], 24)
                || in_v4(v4, [198, 18, 0, 0], 15)
        }
        IpAddr::V6(v6) => {
            v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_loopback()
                || in_v6(v6, [0x2001, 0x0db8, 0, 0], 32)
                || in_v6(v6, [0xfe80, 0, 0, 0], 10)
                || v6 == Ipv6Addr::LOCALHOST
        }
    }
}

fn in_v4(address: Ipv4Addr, network: [u8; 4], prefix: u32) -> bool {
    let value = u32::from(address);
    let network = u32::from(Ipv4Addr::from(network));
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    value & mask == network & mask
}

fn in_v6(address: Ipv6Addr, network: [u16; 4], prefix: u32) -> bool {
    let value = u128::from(address);
    let mut network_value = 0u128;
    for (index, segment) in network.iter().enumerate() {
        network_value |= (*segment as u128) << (112 - index * 16);
    }
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    value & mask == network_value & mask
}

#[derive(Clone, Debug)]
pub struct RemoteCredentials {
    pub kind: String,
    pub username: Option<String>,
    pub secret: String,
}

#[derive(Clone, Debug)]
pub struct FetchResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait RemoteFetcher: Send + Sync {
    async fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        credentials: Option<&RemoteCredentials>,
    ) -> Result<FetchResponse, SubscriptionError>;
}

pub struct HttpFetcher {
    policy: UrlPolicy,
    redirect_limit: usize,
    max_response_size: usize,
    user_agent: String,
    connect_timeout: Duration,
    request_timeout: Duration,
    outbound_proxy: String,
}

impl HttpFetcher {
    pub fn new(config: &SubscriptionsConfig) -> Result<Self, SubscriptionError> {
        Ok(Self {
            policy: UrlPolicy::new(config),
            redirect_limit: config.redirect_limit,
            max_response_size: config.max_response_size.bytes(),
            user_agent: config.user_agent.clone(),
            connect_timeout: config.connect_timeout.std(),
            request_timeout: config.request_timeout.std(),
            outbound_proxy: config.outbound_proxy.clone(),
        })
    }
}

fn header_string(
    response: &reqwest::Response,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[async_trait]
impl RemoteFetcher for HttpFetcher {
    async fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        credentials: Option<&RemoteCredentials>,
    ) -> Result<FetchResponse, SubscriptionError> {
        let mut validated = self.policy.validate(url).await?;
        let original_origin = origin(&validated.url);
        for _ in 0..=self.redirect_limit {
            let address = validated
                .addresses
                .first()
                .copied()
                .ok_or(SubscriptionError::Unreachable)?;
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(self.connect_timeout)
                .timeout(self.request_timeout)
                .user_agent(&self.user_agent)
                .resolve(&validated.host, address);
            if !self.outbound_proxy.trim().is_empty() {
                builder = builder.proxy(
                    reqwest::Proxy::all(self.outbound_proxy.trim())
                        .map_err(|_| SubscriptionError::Unreachable)?,
                );
            }
            let client = builder
                .build()
                .map_err(|_| SubscriptionError::Unreachable)?;
            let mut request = client.get(validated.url.clone());
            if let Some(etag) = etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = last_modified {
                request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
            }
            if let Some(credentials) = credentials {
                if origin(&validated.url) == original_origin {
                    request = match credentials.kind.as_str() {
                        "basic" => request.basic_auth(
                            credentials.username.clone().unwrap_or_default(),
                            Some(&credentials.secret),
                        ),
                        "bearer" => request.bearer_auth(&credentials.secret),
                        _ => request,
                    };
                }
            }
            let response = request
                .send()
                .await
                .map_err(|_| SubscriptionError::Unreachable)?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(SubscriptionError::Unreachable)?;
                let next = validated
                    .url
                    .join(location)
                    .map_err(|_| SubscriptionError::Unreachable)?;
                validated = self.policy.validate(next.as_str()).await?;
                continue;
            }
            let etag = header_string(&response, reqwest::header::ETAG);
            let last_modified = header_string(&response, reqwest::header::LAST_MODIFIED);
            let content_type = header_string(&response, reqwest::header::CONTENT_TYPE);
            if status == reqwest::StatusCode::NOT_MODIFIED {
                return Ok(FetchResponse {
                    status: 304,
                    etag,
                    last_modified,
                    content_type,
                    body: Vec::new(),
                });
            }
            if !status.is_success() {
                return Err(SubscriptionError::Http(status.as_u16()));
            }
            let declared_length = response.content_length().unwrap_or(0) as usize;
            if declared_length > self.max_response_size {
                return Err(SubscriptionError::TooLarge);
            }
            let mut response = response;
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| SubscriptionError::Unreachable)?
            {
                if body.len() + chunk.len() > self.max_response_size {
                    return Err(SubscriptionError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(FetchResponse {
                status: status.as_u16(),
                etag,
                last_modified,
                content_type,
                body,
            });
        }
        Err(SubscriptionError::Unreachable)
    }
}

fn origin(url: &Url) -> (String, Option<u16>, Option<String>) {
    (
        url.scheme().to_string(),
        url.port_or_known_default(),
        url.host_str().map(|host| host.to_ascii_lowercase()),
    )
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NewSubscription {
    pub url: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub refresh_interval_seconds: Option<u64>,
    pub auth: Option<RemoteAuthInput>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteAuthInput {
    pub kind: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
}

#[derive(Clone, Debug)]
pub enum RefreshOutcome {
    Changed,
    NotModified,
    Degraded(String),
}

pub struct SubscriptionService {
    store: Arc<Store>,
    fetcher: Arc<dyn RemoteFetcher>,
    secrets: Arc<SecretBox>,
    config: SubscriptionsConfig,
    metrics: Arc<Metrics>,
}

impl SubscriptionService {
    pub fn new(
        store: Arc<Store>,
        fetcher: Arc<dyn RemoteFetcher>,
        secrets: Arc<SecretBox>,
        config: SubscriptionsConfig,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            store,
            fetcher,
            secrets,
            config,
            metrics,
        }
    }

    pub fn url_policy(&self) -> UrlPolicy {
        UrlPolicy::new(&self.config)
    }

    pub fn fetcher(&self) -> Arc<dyn RemoteFetcher> {
        self.fetcher.clone()
    }

    pub fn interval(&self, requested: Option<u64>) -> Result<u64, SubscriptionError> {
        let interval = requested.unwrap_or(self.config.default_interval.seconds());
        if interval < self.config.min_interval.seconds() {
            return Err(SubscriptionError::InvalidUrl(format!(
                "refresh interval must be at least {} seconds",
                self.config.min_interval.seconds()
            )));
        }
        if interval > self.config.max_interval.seconds() {
            return Err(SubscriptionError::InvalidUrl(format!(
                "refresh interval must be at most {} seconds",
                self.config.max_interval.seconds()
            )));
        }
        Ok(interval)
    }

    fn encrypt_auth(&self, input: &RemoteAuthInput) -> Result<RemoteAuth, SubscriptionError> {
        let secret = match input.kind.as_str() {
            "basic" => input.password.clone().ok_or_else(|| {
                SubscriptionError::InvalidUrl("basic auth requires a password".into())
            })?,
            "bearer" => input.token.clone().ok_or_else(|| {
                SubscriptionError::InvalidUrl("bearer auth requires a token".into())
            })?,
            other => {
                return Err(SubscriptionError::InvalidUrl(format!(
                    "unknown auth kind {other}"
                )))
            }
        };
        Ok(RemoteAuth {
            kind: input.kind.clone(),
            username: input.username.clone(),
            secret_ciphertext: self.secrets.encrypt(&secret),
            key_id: self.secrets.key_id().to_string(),
        })
    }

    fn decrypt_auth(&self, auth: &RemoteAuth) -> Result<RemoteCredentials, SubscriptionError> {
        let secret = self
            .secrets
            .decrypt(&auth.secret_ciphertext)
            .map_err(|_| SubscriptionError::CredentialsUnavailable)?;
        Ok(RemoteCredentials {
            kind: auth.kind.clone(),
            username: auth.username.clone(),
            secret,
        })
    }

    pub async fn create(
        &self,
        user: &UserName,
        slug: &str,
        request: NewSubscription,
    ) -> Result<CollectionMeta, SubscriptionError> {
        if !self.config.enabled {
            return Err(SubscriptionError::Disabled);
        }
        // Validate the URL policy before making any request.
        let policy = self.url_policy();
        let validated = policy.validate(&request.url).await?;
        let interval = self.interval(request.refresh_interval_seconds)?;
        let existing = self.store.list_collections(user)?;
        let subscription_count = existing
            .iter()
            .filter(|collection| collection.subscription.is_some())
            .count();
        if subscription_count >= self.config.max_per_user {
            return Err(SubscriptionError::LimitReached);
        }
        if existing.iter().any(|collection| collection.slug == slug) {
            return Err(SubscriptionError::Storage(
                "collection already exists".into(),
            ));
        }

        let credentials = request
            .auth
            .as_ref()
            .map(|input| self.encrypt_auth(input))
            .transpose()?;
        let plain_credentials = request
            .auth
            .as_ref()
            .map(|_| self.decrypt_auth(credentials.as_ref().unwrap()))
            .transpose()?;

        let response = self
            .fetcher
            .fetch(
                &validated.normalized,
                None,
                None,
                plain_credentials.as_ref(),
            )
            .await?;
        if response.status == 304 {
            return Err(SubscriptionError::InvalidCalendar);
        }
        if !(200..300).contains(&response.status) {
            return Err(SubscriptionError::Http(response.status));
        }
        let (items, remote) = self.parse_response(&response)?;
        let content_hash = sha256_hex(&response.body);
        let display_name = request
            .display_name
            .filter(|name| !name.trim().is_empty())
            .or(remote.display_name.clone())
            .unwrap_or_else(|| validated.host.clone());
        let source = SubscriptionSource {
            url: validated.normalized.clone(),
            original_url: request.url.clone(),
            refresh_interval_seconds: interval,
            auth: credentials,
            ..Default::default()
        };
        let meta = self
            .store
            .create_subscription_collection(
                user,
                slug,
                &display_name,
                request.color.or(remote.color.clone()),
                source,
                &items,
                response.etag,
                response.last_modified,
                remote.display_name,
                remote.description,
                remote.color,
                Some(content_hash),
            )
            .await?;
        Ok(meta)
    }

    pub async fn refresh(
        &self,
        path: &CollectionPath,
    ) -> Result<RefreshOutcome, SubscriptionError> {
        self.metrics
            .subscription_refreshes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let meta = self.store.collection(path)?;
        let source = meta
            .subscription
            .clone()
            .ok_or_else(|| SubscriptionError::Storage("not a subscription".into()))?;
        let credentials = source
            .auth
            .as_ref()
            .map(|auth| self.decrypt_auth(auth))
            .transpose()?;
        let next_default =
            Utc::now() + chrono::Duration::seconds(source.refresh_interval_seconds as i64);
        let result = self
            .fetcher
            .fetch(
                &source.url,
                source.etag.as_deref(),
                source.last_modified.as_deref(),
                credentials.as_ref(),
            )
            .await;
        match result {
            Ok(response) if response.status == 304 => {
                self.store
                    .apply_subscription_fetch_state(path, None, None, next_default, true)
                    .await?;
                Ok(RefreshOutcome::NotModified)
            }
            Ok(response) if (200..300).contains(&response.status) => {
                let (items, remote) = match self.parse_response(&response) {
                    Ok(value) => value,
                    Err(error) => return self.fail(path, &source, error).await,
                };
                let content_hash = sha256_hex(&response.body);
                let changed = self
                    .store
                    .apply_subscription_snapshot(
                        path,
                        &items,
                        response.etag,
                        response.last_modified,
                        next_default,
                        remote.display_name,
                        remote.description,
                        remote.color,
                        Some(content_hash),
                    )
                    .await?;
                if changed {
                    Ok(RefreshOutcome::Changed)
                } else {
                    Ok(RefreshOutcome::NotModified)
                }
            }
            Ok(response) => {
                self.fail(path, &source, SubscriptionError::Http(response.status))
                    .await
            }
            Err(error) => self.fail(path, &source, error).await,
        }
    }

    async fn fail(
        &self,
        path: &CollectionPath,
        source: &SubscriptionSource,
        error: SubscriptionError,
    ) -> Result<RefreshOutcome, SubscriptionError> {
        self.metrics
            .subscription_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let failures = source.consecutive_failures.saturating_add(1);
        let delay = backoff(failures, source.refresh_interval_seconds);
        let next = Utc::now() + chrono::Duration::seconds(delay as i64);
        let message = user_error(&error);
        self.store
            .record_subscription_failure(path, message.clone(), next, failures)
            .await?;
        Ok(RefreshOutcome::Degraded(message))
    }

    fn parse_response(
        &self,
        response: &FetchResponse,
    ) -> Result<(Vec<oxicale_ical::FetchedItem>, RemoteMetadata), SubscriptionError> {
        if response.body.len() > self.config.max_response_size.bytes() {
            return Err(SubscriptionError::TooLarge);
        }
        let text =
            std::str::from_utf8(&response.body).map_err(|_| SubscriptionError::InvalidCalendar)?;
        if let Some(content_type) = &response.content_type {
            let lower = content_type.to_ascii_lowercase();
            let acceptable = lower.starts_with("text/calendar")
                || lower.starts_with("text/plain")
                || lower.starts_with("application/octet-stream")
                || lower.starts_with("application/ics")
                || text.trim_start().starts_with("BEGIN:VCALENDAR");
            if !acceptable {
                return Err(SubscriptionError::InvalidCalendar);
            }
        }
        if !text.trim_start().starts_with("BEGIN:VCALENDAR") {
            return Err(SubscriptionError::InvalidCalendar);
        }
        let limits = Limits {
            max_bytes: self.config.max_response_size.bytes(),
            max_components: self.config.max_components,
            ..Limits::default()
        };
        let calendar =
            parse_calendar(text, limits).map_err(|_| SubscriptionError::InvalidCalendar)?;
        let remote = RemoteMetadata {
            display_name: calendar
                .property("X-WR-CALNAME")
                .map(|p| p.text().trim().to_string())
                .filter(|v| !v.is_empty()),
            description: calendar
                .property("X-WR-CALDESC")
                .map(|p| p.text().trim().to_string())
                .filter(|v| !v.is_empty()),
            color: calendar
                .property("X-APPLE-CALENDAR-COLOR")
                .map(|p| p.value.trim().to_string())
                .filter(|v| is_color(v)),
        };
        let items = split_calendar_with_options(
            text,
            limits,
            chrono_tz::UTC,
            self.config.skip_broken_components,
        )
        .map_err(|_| SubscriptionError::InvalidCalendar)?;
        if items.len() > self.config.max_components {
            return Err(SubscriptionError::TooLarge);
        }
        for item in &items {
            if item.payload.len() > self.config.max_item_size.bytes() {
                return Err(SubscriptionError::TooLarge);
            }
        }
        Ok((items, remote))
    }

    pub async fn refresh_all_due(
        &self,
    ) -> Vec<(CollectionPath, Result<RefreshOutcome, SubscriptionError>)> {
        let due = match self.store.due_subscriptions(Utc::now()) {
            Ok(due) => due,
            Err(_) => return Vec::new(),
        };
        let mut results = Vec::new();
        for (path, _) in due {
            let result = self.refresh(&path).await;
            results.push((path, result));
        }
        results
    }
}

#[derive(Clone, Debug, Default)]
pub struct RemoteMetadata {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
}

fn is_color(value: &str) -> bool {
    (value.len() == 7 || value.len() == 9)
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn user_error(error: &SubscriptionError) -> String {
    match error {
        SubscriptionError::BlockedAddress => {
            "The URL points to a private network address, which is blocked by default.".into()
        }
        SubscriptionError::InvalidUrl(message) => format!("The URL is invalid: {message}"),
        SubscriptionError::HostDenied | SubscriptionError::SchemeDenied => {
            "The URL is not allowed by the subscription policy.".into()
        }
        SubscriptionError::Unreachable => "The URL is not reachable from the server.".into(),
        SubscriptionError::Http(status) => {
            format!("The remote server returned HTTP {status}.")
        }
        SubscriptionError::InvalidCalendar => "The response is not a valid iCalendar file.".into(),
        SubscriptionError::TooLarge => "The calendar is too large.".into(),
        SubscriptionError::LimitReached => "The subscription limit has been reached.".into(),
        SubscriptionError::CredentialsUnavailable => {
            "The stored credentials can no longer be decrypted.".into()
        }
        SubscriptionError::Storage(_) => "The subscription could not be stored.".into(),
        SubscriptionError::Disabled => "Subscriptions are disabled.".into(),
    }
}

pub fn backoff(failures: u32, interval_seconds: u64) -> u64 {
    match failures {
        0 | 1 => 300,
        2 => 900,
        3 => 3_600,
        4 => 10_800,
        _ => interval_seconds.max(6 * 3_600),
    }
}

pub fn spawn_scheduler(
    service: Arc<SubscriptionService>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown = shutdown;
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let _ = service.refresh_all_due().await;
                }
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SubscriptionsConfig;

    fn config() -> SubscriptionsConfig {
        SubscriptionsConfig::default()
    }

    #[test]
    fn backoff_schedule() {
        assert_eq!(backoff(1, 3600), 300);
        assert_eq!(backoff(2, 3600), 900);
        assert_eq!(backoff(3, 3600), 3600);
        assert_eq!(backoff(4, 3600), 10_800);
        assert_eq!(backoff(5, 3600), 21_600);
        assert_eq!(backoff(5, 86_400), 86_400);
    }

    #[test]
    fn host_matching() {
        let mut config = config();
        config.allowed_hosts = vec!["example.com".into(), "*.example.org".into()];
        config.denied_hosts = vec!["bad.example.org".into()];
        let policy = UrlPolicy::new(&config);
        assert!(policy.host_allowed("example.com"));
        assert!(policy.host_allowed("sub.example.org"));
        assert!(!policy.host_allowed("bad.example.org"));
        assert!(!policy.host_allowed("other.test"));
    }

    #[test]
    fn ip_policy() {
        let policy = UrlPolicy::new(&config());
        for ip in [
            "127.0.0.1",
            "::1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "100.64.0.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(
                !policy.ip_allowed(ip.parse().unwrap()),
                "{ip} should be blocked"
            );
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(
                policy.ip_allowed(ip.parse().unwrap()),
                "{ip} should be allowed"
            );
        }
        let mut relaxed = config();
        relaxed.allow_loopback = true;
        relaxed.allow_private_networks = true;
        let policy = UrlPolicy::new(&relaxed);
        assert!(policy.ip_allowed("127.0.0.1".parse().unwrap()));
        assert!(policy.ip_allowed("192.168.1.1".parse().unwrap()));
        assert!(policy.ip_allowed("100.64.0.1".parse().unwrap()));
        // Link-local metadata endpoints stay blocked even in relaxed mode.
        assert!(!policy.ip_allowed("169.254.169.254".parse().unwrap()));
        assert!(!policy.ip_allowed("fe80::a9fe:a9fe".parse().unwrap()));
    }

    #[test]
    fn origins_include_the_host() {
        let a = Url::parse("https://a.example/x").unwrap();
        let b = Url::parse("https://b.example/x").unwrap();
        assert_ne!(origin(&a), origin(&b));
        assert_eq!(
            origin(&a),
            origin(&Url::parse("https://a.example/y").unwrap())
        );
    }

    #[tokio::test]
    async fn url_scheme_rules() {
        let policy = UrlPolicy::new(&config());
        assert_eq!(
            policy
                .validate("ftp://example.com/a.ics")
                .await
                .unwrap_err(),
            SubscriptionError::SchemeDenied
        );
        assert_eq!(
            policy
                .validate("http://93.184.216.34/a.ics")
                .await
                .unwrap_err(),
            SubscriptionError::SchemeDenied
        );
        assert_eq!(
            policy
                .validate("https://user:pass@example.com/a.ics")
                .await
                .unwrap_err(),
            SubscriptionError::InvalidUrl("credentials must be supplied separately".into())
        );
        let mut config = config();
        config.allow_http = true;
        let policy = UrlPolicy::new(&config);
        let validated = policy
            .validate("http://93.184.216.34:80/a.ics")
            .await
            .unwrap();
        assert_eq!(validated.url.scheme(), "http");
    }

    #[tokio::test]
    async fn rejects_loopback_without_request() {
        let policy = UrlPolicy::new(&config());
        assert_eq!(
            policy.validate("http://127.0.0.1/a.ics").await.unwrap_err(),
            SubscriptionError::SchemeDenied
        );
        let mut config = config();
        config.allow_http = true;
        let policy = UrlPolicy::new(&config);
        assert_eq!(
            policy.validate("http://127.0.0.1/a.ics").await.unwrap_err(),
            SubscriptionError::BlockedAddress
        );
    }

    #[tokio::test]
    async fn webcal_is_normalized() {
        let mut config = config();
        config.allowed_hosts = vec!["example.com".into()];
        let policy = UrlPolicy::new(&config);
        let validated = policy
            .validate("webcal://example.com/team.ics")
            .await
            .unwrap();
        assert_eq!(validated.url.scheme(), "https");
        assert_eq!(validated.normalized, "https://example.com/team.ics");
    }
}
