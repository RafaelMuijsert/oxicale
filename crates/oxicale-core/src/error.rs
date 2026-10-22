use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    #[error("invalid username: {0}")]
    InvalidUsername(String),
    #[error("invalid collection slug: {0}")]
    InvalidSlug(String),
    #[error("invalid item href: {0}")]
    InvalidHref(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("invalid sync token")]
    InvalidSyncToken,
    #[error("invalid component: {0}")]
    InvalidComponent(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
