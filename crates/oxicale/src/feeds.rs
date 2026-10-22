use std::sync::Arc;

use chrono::{DateTime, Utc};
use oxicale_core::{hash_token, new_token, token_hint, CollectionPath, ComponentSet, UserName};
use oxicale_storage::{FeedCreation, FeedMeta, FeedMeta as StoredFeed, Store};
use thiserror::Error;

use crate::config::FeedsConfig;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FeedError {
    #[error("feeds are disabled")]
    Disabled,
    #[error("not found")]
    NotFound,
    #[error("feed is revoked or expired")]
    Inactive,
    #[error("invalid feed request: {0}")]
    Invalid(String),
    #[error("feed limit reached")]
    LimitReached,
    #[error("storage error: {0}")]
    Storage(String),
}

impl From<oxicale_core::CoreError> for FeedError {
    fn from(value: oxicale_core::CoreError) -> Self {
        Self::Invalid(value.to_string())
    }
}

impl From<oxicale_storage::StorageError> for FeedError {
    fn from(value: oxicale_storage::StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct CreatedFeed {
    pub feed: FeedMeta,
    pub url: String,
    pub token: String,
}

#[derive(Clone, Debug)]
pub struct FeedResponse {
    pub body: String,
    pub etag: String,
    pub last_modified: DateTime<Utc>,
    pub cache_control: String,
    pub filename: String,
    pub feed_id: String,
    pub path: CollectionPath,
}

pub struct FeedService {
    store: Arc<Store>,
    config: FeedsConfig,
    external_url: String,
    base_path: String,
    version: String,
}

impl FeedService {
    pub fn new(
        store: Arc<Store>,
        config: FeedsConfig,
        external_url: String,
        base_path: String,
        version: String,
    ) -> Self {
        Self {
            store,
            config,
            external_url: external_url.trim_end_matches('/').to_string(),
            base_path,
            version,
        }
    }

    pub fn config(&self) -> &FeedsConfig {
        &self.config
    }

    pub fn collection_by_id(&self, user: &UserName, id: &str) -> Result<CollectionPath, FeedError> {
        let collections = self.store.list_collections(user)?;
        let meta = collections
            .into_iter()
            .find(|collection| collection.id == id)
            .ok_or(FeedError::NotFound)?;
        Ok(CollectionPath::parse(user.as_str(), &meta.slug)?)
    }

    pub async fn create(
        &self,
        user: &UserName,
        collection_id: &str,
        name: &str,
        components: ComponentSet,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<CreatedFeed, FeedError> {
        if !self.config.enabled {
            return Err(FeedError::Disabled);
        }
        if name.trim().is_empty() || name.len() > 255 {
            return Err(FeedError::Invalid("invalid feed name".into()));
        }
        if components.is_empty() {
            return Err(FeedError::Invalid(
                "select at least one component type".into(),
            ));
        }
        let path = self.collection_by_id(user, collection_id)?;
        let meta = self.store.collection(&path)?;
        if meta.kind != oxicale_core::CollectionKind::Calendar {
            return Err(FeedError::Invalid(
                "feeds can only be created for calendars".into(),
            ));
        }
        if meta
            .feeds
            .iter()
            .filter(|feed| feed.revoked_at.is_none())
            .count()
            >= self.config.max_per_calendar
        {
            return Err(FeedError::LimitReached);
        }
        let expires_at = expires_at.or_else(|| {
            if self.config.default_expiry.seconds() == 0 {
                None
            } else {
                Some(
                    Utc::now()
                        + chrono::Duration::seconds(self.config.default_expiry.seconds() as i64),
                )
            }
        });
        let token = new_token();
        let feed = self
            .store
            .create_feed(
                &path,
                FeedCreation {
                    token_sha256: hash_token(&token),
                    token_hint: token_hint(&token),
                    name: name.trim().to_string(),
                    components,
                    expires_at,
                },
            )
            .await?;
        let url = self.feed_url(&token, name);
        Ok(CreatedFeed { feed, url, token })
    }

    pub async fn revoke(
        &self,
        user: &UserName,
        collection_id: &str,
        feed_id: &str,
    ) -> Result<FeedMeta, FeedError> {
        let path = self.collection_by_id(user, collection_id)?;
        Ok(self.store.revoke_feed(&path, feed_id).await?)
    }

    pub async fn rotate(
        &self,
        user: &UserName,
        collection_id: &str,
        feed_id: &str,
    ) -> Result<CreatedFeed, FeedError> {
        let path = self.collection_by_id(user, collection_id)?;
        let token = new_token();
        let feed = self
            .store
            .rotate_feed(&path, feed_id, hash_token(&token), token_hint(&token))
            .await?;
        let url = self.feed_url(&token, &feed.name);
        Ok(CreatedFeed { feed, url, token })
    }

    pub fn list(&self, user: &UserName, collection_id: &str) -> Result<Vec<FeedMeta>, FeedError> {
        let path = self.collection_by_id(user, collection_id)?;
        let meta = self.store.collection(&path)?;
        Ok(meta.feeds)
    }

    pub fn masked_url(&self, feed: &StoredFeed) -> String {
        let filename = feed_filename(&feed.name);
        format!(
            "{}{}feeds/****{}/{filename}",
            self.external_url, self.base_path, feed.token_hint
        )
    }

    pub fn feed_url(&self, token: &str, name: &str) -> String {
        format!(
            "{}{}feeds/{}/{}",
            self.external_url,
            self.base_path,
            token,
            feed_filename(name)
        )
    }

    pub fn lookup(&self, token: &str) -> Result<(CollectionPath, FeedMeta), FeedError> {
        if token.len() < 43
            || token.len() > 128
            || !token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(FeedError::NotFound);
        }
        let hash = hash_token(token);
        let (path, feed) = self
            .store
            .find_feed_by_hash(&hash)?
            .ok_or(FeedError::NotFound)?;
        if !feed.is_active(Utc::now()) {
            return Err(FeedError::Inactive);
        }
        Ok((path, feed))
    }

    pub async fn respond(&self, token: &str) -> Result<FeedResponse, FeedError> {
        if !self.config.enabled {
            return Err(FeedError::NotFound);
        }
        let (path, feed) = self.lookup(token)?;
        let prodid = format!("-//Oxicale//Oxicale {}//EN", self.version);
        let (body, etag, last_modified) = self.store.generate_feed_bytes(&path, &feed, &prodid)?;
        self.store.mark_feed_used(&path, &feed.id).await?;
        Ok(FeedResponse {
            body,
            etag,
            last_modified,
            cache_control: self.config.default_cache_control.clone(),
            filename: feed_filename(&feed.name),
            feed_id: feed.id,
            path,
        })
    }
}

fn feed_filename(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if matches!(c, '-' | '_') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "feed.ics".to_string()
    } else {
        format!("{out}.ics")
    }
}

pub fn masked_token(feed: &FeedMeta) -> String {
    format!("****{}", feed.token_hint)
}

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use std::sync::atomic::Ordering;

use crate::server::{peer_from_request, simple_response, AppState};

pub async fn handle_feed(state: Arc<AppState>, request: Request, segments: &[String]) -> Response {
    let method = request.method().clone();
    if method == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::ALLOW, "OPTIONS, GET, HEAD")
            .body(Body::empty())
            .unwrap();
    }
    if !matches!(method, Method::GET | Method::HEAD) {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(axum::http::header::ALLOW, "OPTIONS, GET, HEAD")
            .body(Body::empty())
            .unwrap();
    }
    let token = match segments {
        [feed, token] if feed == "feeds" => token,
        [feed, token, _] if feed == "feeds" => token,
        _ => return feed_not_found(),
    };
    let peer = peer_from_request(&request);
    let limiter_key = format!("{}|{}", hash_token(token), peer);
    if !state.feed_limiter.check(&limiter_key) {
        state.metrics.feed_failures.fetch_add(1, Ordering::Relaxed);
        return simple_response(
            StatusCode::TOO_MANY_REQUESTS,
            "text/plain; charset=utf-8",
            "too many requests",
        );
    }
    state.feed_limiter.record_failure(&limiter_key);
    let headers = request.headers().clone();
    if state.config.feeds.allow_http_auth && state.authenticate(&headers, peer).is_err() {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(
                axum::http::header::WWW_AUTHENTICATE,
                format!("Basic realm=\"{}\", charset=\"UTF-8\"", state.auth.realm()),
            )
            .header(axum::http::header::REFERRER_POLICY, "no-referrer")
            .body(Body::from("Unauthorized"))
            .unwrap();
    }
    let _ = to_bytes(request.into_body(), 1024).await;
    state.metrics.feed_requests.fetch_add(1, Ordering::Relaxed);
    match state.feeds.respond(token).await {
        Ok(feed) => {
            if conditional(&headers, &feed.etag, feed.last_modified) {
                return Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(axum::http::header::ETAG, format!("\"{}\"", feed.etag))
                    .header(
                        axum::http::header::LAST_MODIFIED,
                        httpdate::fmt_http_date(feed.last_modified.into()),
                    )
                    .header(axum::http::header::REFERRER_POLICY, "no-referrer")
                    .body(Body::empty())
                    .unwrap();
            }
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "text/calendar; charset=utf-8",
                )
                .header(
                    axum::http::header::CONTENT_DISPOSITION,
                    format!("inline; filename=\"{}\"", feed.filename),
                )
                .header(axum::http::header::CACHE_CONTROL, feed.cache_control)
                .header(axum::http::header::ETAG, format!("\"{}\"", feed.etag))
                .header(
                    axum::http::header::LAST_MODIFIED,
                    httpdate::fmt_http_date(feed.last_modified.into()),
                )
                .header(axum::http::header::REFERRER_POLICY, "no-referrer")
                .header(axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff");
            if method == Method::HEAD {
                builder = builder.header(
                    axum::http::header::CONTENT_LENGTH,
                    feed.body.len().to_string(),
                );
            }
            builder
                .body(if method == Method::HEAD {
                    Body::empty()
                } else {
                    Body::from(feed.body)
                })
                .unwrap_or_else(|_| feed_not_found())
        }
        Err(_) => {
            state.metrics.feed_failures.fetch_add(1, Ordering::Relaxed);
            feed_not_found()
        }
    }
}

fn conditional(
    headers: &HeaderMap,
    etag: &str,
    last_modified: chrono::DateTime<chrono::Utc>,
) -> bool {
    if let Some(value) = headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        return value.trim() == "*"
            || value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
                candidate.trim_matches('"') == etag
            });
    }
    if let Some(value) = headers
        .get(axum::http::header::IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(since) = httpdate::parse_http_date(value) {
            let since: chrono::DateTime<chrono::Utc> = since.into();
            return last_modified <= since + chrono::Duration::seconds(1);
        }
    }
    false
}

fn feed_not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )
        .header(axum::http::header::REFERRER_POLICY, "no-referrer")
        .header(axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from("Not Found"))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames() {
        assert_eq!(feed_filename("Tasks only"), "tasks-only.ics");
        assert_eq!(feed_filename("!!!"), "feed.ics");
        assert_eq!(feed_filename("A_B-C"), "a_b-c.ics");
    }

    #[test]
    fn masks_tokens() {
        let feed = FeedMeta {
            id: "1".into(),
            name: "x".into(),
            components: ComponentSet::all(),
            token_sha256: hash_token("secret"),
            token_hint: "abcd".into(),
            created_at: Utc::now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        };
        assert_eq!(masked_token(&feed), "****abcd");
    }
}
