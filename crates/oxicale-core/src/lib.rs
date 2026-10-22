//! Domain primitives shared by every Oxicale crate.

pub mod component;
pub mod error;
pub mod etag;
pub mod path;
pub mod permission;
pub mod sync_token;
pub mod token;

pub use component::{CalendarComponent, CollectionKind, ComponentSet, ItemComponent};
pub use error::CoreError;
pub use etag::content_etag;
pub use path::{normalize_path, CollectionPath, Href, Slug, UserName};
pub use permission::Permissions;
pub use sync_token::SyncToken;
pub use token::{constant_time_eq, hash_token, new_token, token_hint};
