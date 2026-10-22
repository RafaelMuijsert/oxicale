# Oxicale Architecture

Status: Draft 1

This document describes the planned Rust architecture. It is intentionally
implementation-oriented but not a substitute for the detailed data and protocol
documents.

## 1. Design constraints

- One process, one data directory, one writer at a time.
- Asynchronous network I/O for HTTP serving and subscription fetching.
- Blocking filesystem work must not stall the async runtime.
- Canonical item data is plain files. The index is subordinate.
- Protocol code must not depend on the web UI implementation.
- Security checks must live next to the operation they protect, not only in the
  UI.

## 2. Proposed workspace

```text
oxicale/
├── Cargo.toml                 workspace
├── crates/
│   ├── oxicale/               binary, config wiring, CLI
│   ├── oxicale-core/          domain types, errors, IDs, time handling
│   ├── oxicale-dav/           WebDAV/CalDAV/CardDAV request handling
│   ├── oxicale-ical/          iCalendar parsing, validation, filtering
│   ├── oxicale-vcard/         vCard parsing and validation
│   ├── oxicale-storage/       filesystem store, metadata, index, sync
│   ├── oxicale-auth/          authentication backends and sessions
│   ├── oxicale-rights/        authorization policies
│   ├── oxicale-subscriptions/ remote fetch, scheduling and reconciliation
│   ├── oxicale-feeds/         component feed generation and tokens
│   ├── oxicale-web/           HTML UI and management API
│   └── oxicale-import/        Radicale migration and storage verification
└── tests/                     cross-crate integration and conformance tests
```

Splitting into crates keeps dependencies directional:

```text
oxicale
  ├── oxicale-web
  ├── oxicale-dav
  ├── oxicale-subscriptions
  └── oxicale-feeds
          │
          ├── oxicale-storage
          ├── oxicale-auth
          ├── oxicale-rights
          ├── oxicale-ical
          └── oxicale-vcard
                  │
                oxicale-core
```

`oxicale-core` must have no dependency on storage, HTTP or network crates.

## 3. Recommended runtime crates

Final selections require a dependency review. The current proposal is:

| Concern | Crate | Notes |
| --- | --- | --- |
| Async runtime | `tokio` | Multi-threaded runtime; blocking pool for filesystem |
| HTTP server | `axum` on `hyper` | Tower middleware, HTTP/1.1 and HTTP/2 |
| TLS | `rustls`, `tokio-rustls` | No OpenSSL dependency |
| HTTP client | `reqwest` with `rustls` | Subscription fetching only |
| iCalendar | evaluated `ical`/`icalendar` crates plus local validation | May need a local hardening layer |
| vCard | evaluated vCard crate plus local validation | Must support 3.0 and 4.0 |
| XML | `quick-xml` or `roxmltree` | Hardened parsing, no external entities |
| Database | `rusqlite` + `r2d2`/`deadpool` or `sqlx` | Embedded SQLite index |
| Serialization | `serde`, `serde_json`, `toml` | Metadata and config |
| Password hashing | `argon2` | Argon2id |
| Secret encryption | `chacha20poly1305` | Subscription credentials at rest |
| Token hashing | `sha2` | Feed token digests |
| Randomness | `getrandom`, `rand` | IDs and tokens |
| Time | `time` or `chrono` | Must handle iCalendar dates and zones |
| Observability | `tracing`, `tracing-subscriber`, `metrics` | Structured logs |

The iCalendar and vCard crates must be evaluated for correctness and
maintenance status before adoption. If no crate is adequate, Oxicale will
implement the required subset with its own validation layer.

## 4. Request lifecycle

### 4.1 DAV request

```text
TCP/TLS
  → Hyper
  → Axum router
  → base-path stripping and path normalisation
  → authentication middleware
  → DAV method dispatcher
  → authorization check
  → request body limit and XML hardening
  → method handler (PROPFIND, REPORT, PUT, ...)
  → storage transaction
  → XML / iCalendar / vCard response
```

Rules:

1. Reject malformed or overly long paths before touching storage.
2. Authenticate before revealing whether a principal or collection exists.
3. Authorize the exact collection path and, for item requests, the parent.
4. Parse and validate the full request body before acquiring a write lock where
   possible.
5. Hold the per-collection lock only for the read-modify-write critical section.
6. Stream large responses when practical, but generate feeds and aggregate
   exports from a consistent snapshot.

### 4.2 Feed request

```text
GET /feeds/{token}
  → constant-time token hash lookup
  → load feed metadata
  → check revocation and expiry
  → rate limit by token and client address
  → consistent read of collection items
  → filter components
  → assemble VCALENDAR
  → ETag / Last-Modified / conditional response
```

Feed lookup must not require authentication. It must not disclose the owner or
collection name in the URL or error message.

### 4.3 Subscription refresh

```text
scheduler tick
  → select due subscriptions
  → enqueue with per-host concurrency limit
  → SSRF-safe DNS resolution and connection
  → HTTP GET with validators
  → 304: record success, schedule next run
  → 2xx: enforce limits, parse, validate, normalise
  → compute diff against current items
  → acquire collection write lock
  → apply diff and advance revision/sync token
  → record fetch metadata
```

A refresh must never hold the collection lock while performing network I/O.
The diff and apply steps must be one storage transaction.

## 5. Storage architecture

### 5.1 Layers

- **Payload store**: one file per item under the collection directory. Owns
  atomic writes, fsync policy, payload hashing and ETags.
- **Metadata store**: a `.collection.json` file per collection. Owns kind,
  display metadata, subscription configuration and feed records.
- **Index**: an embedded SQLite database. Owns item lookup by UID, time-range
  candidate selection, change journal and sync token mapping.
- **Lock manager**: per-collection async read/write locks plus a process-level
  data-directory lock.

The payload store and metadata store are canonical. The index is rebuildable.
Losing the index may invalidate sync tokens; clients then perform a full
resynchronisation. Losing the index must not lose items.

### 5.2 Transactions

Logical write transaction:

1. Validate input and compute new payload and ETag.
2. Write payload to a temporary file in the same filesystem.
3. `fsync` the file if enabled.
4. Rename into place.
5. Update the SQLite index and append the change record.
6. Commit the index transaction.
7. `fsync` the directory if enabled.

Crash recovery scans for temporary files, removes them, and verifies that every
visible item has an index row. A background or startup repair can rebuild the
index from payloads and metadata.

### 5.3 Locking

- A single process-level lock on the data directory prevents two Oxicale
  instances from writing the same data.
- Each collection has an async `RwLock`. Reads take a shared lock; writes take
  an exclusive lock.
- External tooling that modifies files must take the documented OS file lock.
- The index uses SQLite WAL mode with a busy timeout. Index transactions are
  short and never include network I/O.

## 6. Domain model

The detailed model is in [DATA-MODEL.md](DATA-MODEL.md). At the architecture
level:

- `Principal` owns `Collection`s.
- `Collection` is either `Calendar` or `AddressBook`.
- A `Calendar` is always mixed and has zero or one `SubscriptionSource`.
- A `Feed` belongs to a calendar, has a component filter and a token digest.
- An `Item` has an href, UID, component type, ETag and stored payload.
- A `Change` records an item mutation for sync.

Domain types must use newtypes for user names, collection IDs, hrefs, ETags,
sync tokens and tokens. This prevents mixing untrusted strings with trusted
identifiers.

## 7. Protocol layer

`oxicale-dav` maps HTTP requests to domain operations. It is responsible for:

- WebDAV path semantics and percent-decoding.
- Depth handling.
- XML request parsing and response serialisation.
- Conditional headers (`If-Match`, `If-None-Match`, `If-Modified-Since`).
- CalDAV/CardDAV report evaluation.
- WebDAV precondition error bodies.
- `DAV:` capability advertisement.

It must not know how payloads are stored. It works through a repository
interface implemented by `oxicale-storage`.

### 7.1 Repository boundary

```rust
trait CollectionRepository {
    async fn principal(&self, user: &UserName) -> Result<Principal>;
    async fn list_collections(&self, user: &UserName) -> Result<Vec<Collection>>;
    async fn get_collection(&self, path: &CollectionPath) -> Result<Collection>;
    async fn read_items(&self, path: &CollectionPath, query: ItemQuery)
        -> Result<Vec<Item>>;
    async fn put_item(&self, path: &CollectionPath, href: &Href,
                      payload: ValidatedCalendarObject, precondition: Precondition)
        -> Result<PutOutcome>;
    async fn delete_item(&self, path: &CollectionPath, href: &Href,
                         precondition: Precondition) -> Result<()>;
    async fn changes_since(&self, path: &CollectionPath, token: &SyncToken)
        -> Result<ChangeSet>;
}
```

Implementations may be backed by the filesystem store, an in-memory store for
tests, or a future alternative backend. The trait must be object-safe or use
static dispatch consistently.

## 8. Authentication and authorization

### 8.1 Authentication

`oxicale-auth` exposes:

```rust
trait Authenticator {
    async fn authenticate(&self, request: &AuthRequest) -> Result<Option<PrincipalId>>;
}
```

Built-in backends:

- `local`: password file with Argon2id hashes.
- `trusted_header`: accept a username from a configured header only when the
  peer address is in a trusted proxy list.
- `none`: explicit development mode; binds only to loopback by default.
- `deny`: default deny-all backend.

Web sessions are separate from DAV Basic authentication but resolve to the same
principal. Session cookies are `HttpOnly`, `Secure` when TLS is enabled,
`SameSite=Lax`, and tied to a server-side session record.

### 8.2 Authorization

`oxicale-rights` answers whether a principal may perform an action on a path.
The default policy is owner-only. Subscription collections add a read-only
overlay. Feed tokens bypass user sessions but are checked by `oxicale-feeds`
before any storage read.

Rights are evaluated as capabilities, not booleans:

```rust
bitflags! {
    struct Permissions: u16 {
        const READ            = 1 << 0;
        const WRITE           = 1 << 1;
        const DELETE          = 1 << 2;
        const READ_FREEBUSY   = 1 << 3;
        const WRITE_PROPERTIES = 1 << 4;
    }
}
```

## 9. Subscription subsystem

Components:

- **URL policy**: scheme validation, host allow/deny rules, and SSRF checks.
- **Fetcher**: HTTP client with timeouts, redirect policy, size limits and
  conditional headers.
- **Parser**: iCalendar hardening, component extraction and identity rules.
- **Reconciler**: stable diff between the fetched set and stored items.
- **Scheduler**: due-time calculation, jitter, backoff and per-host limits.
- **State store**: fetch metadata inside the collection metadata file.

The scheduler is a supervised Tokio task. It wakes on a fixed short interval,
loads due subscriptions from the index, and submits fetch jobs to a bounded
worker pool. It must recover after restart by recalculating due times from
persisted metadata.

Backoff on failure: 5 minutes, 15 minutes, 1 hour, 3 hours, capped at the
configured refresh interval or 6 hours, whichever is larger. A manual refresh
resets the backoff after success.

## 10. Feed subsystem

Components:

- **Token service**: generate 256-bit base64url tokens, hash with SHA-256,
  compare in constant time.
- **Feed registry**: feed metadata stored in the collection metadata file.
- **Generator**: load matching items, merge required `VTIMEZONE` components,
  emit a deterministic VCALENDAR.
- **Conditional response**: compute a strong ETag from the filtered item ETags
  plus collection identity and filter. Return `304` when appropriate.

Feed generation must not write to storage. It uses a consistent read snapshot.
The generator must be deterministic for identical inputs so that ETags and
caching work correctly.

## 11. Web subsystem

`oxicale-web` contains:

- Server-rendered templates for login, dashboard, calendar details,
  subscription form and feed manager.
- A small management JSON API under `/.oxicale/api/v1/`.
- CSRF protection for state-changing requests.
- Session middleware.
- Static assets embedded in the binary.

The management API is an internal interface for the UI. It must not be the only
way to manage collections; CalDAV remains the public interface for content.
Breaking changes to the internal API require a version bump.

## 12. Error handling

- Domain errors use typed enums, not stringly typed errors.
- Protocol errors map to HTTP status codes and WebDAV precondition elements.
- Internal errors are logged with a correlation ID and returned to clients
  without leaking paths, URLs or secrets.
- Subscription fetch errors are stored in user-facing form and logged in
  structured form.
- Feed token misses return the same response as revoked tokens.

## 13. Configuration and startup

Startup sequence:

1. Parse CLI arguments.
2. Load TOML configuration and environment overrides.
3. Validate configuration and resolve paths.
4. Initialise logging and metrics.
5. Acquire the data-directory lock.
6. Open storage and run the lightweight consistency check.
7. Start the subscription scheduler.
8. Start the HTTP server.

Failures in steps 1 to 6 abort startup. A failed subscription scheduler must
degrade subscriptions without taking down the DAV server.

## 14. Observability

Structured log fields include request ID, method, path template, status,
duration, user ID (not password), collection ID and item href. Never log:

- `Authorization` headers
- session cookies
- feed tokens
- subscription credentials
- full request bodies by default

Metrics:

- `oxicale_http_requests_total{method,status}`
- `oxicale_http_request_duration_seconds`
- `oxicale_storage_operations_total{operation}`
- `oxicale_subscription_refresh_total{result}`
- `oxicale_subscription_last_success_timestamp`
- `oxicale_feed_requests_total{result}`

## 15. Testing hooks

The architecture must support:

- Injecting a fake clock for subscription scheduling.
- Injecting a fake HTTP transport for remote calendar tests.
- Injecting a fake DNS resolver and IP policy for SSRF tests.
- In-memory storage for protocol tests.
- A deterministic token generator in tests only.

## 16. Open architecture questions

1. Whether to use a single crate with modules instead of a workspace for the
   first release, to reduce build complexity.
2. Whether the index should store full payloads for query performance or only
   metadata. Current proposal: metadata only.
3. Whether feed ETags should include a server build identifier. Current
   proposal: no, only data and filter identity.
4. Whether subscription reconciliation should store a raw last-good snapshot for
   debugging. Current proposal: optional and disabled by default.
