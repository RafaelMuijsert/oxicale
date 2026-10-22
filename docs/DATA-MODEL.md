# Oxicale Data Model

Status: Draft 1

This document defines the canonical data model and on-disk layout. It is the
reference for storage, migration, backups and protocol mapping.

## 1. Core entities

```text
Principal 1 ── * Collection 1 ── * Item
                    │
                    ├── 0..1 SubscriptionSource
                    └── * Feed
```

### 1.1 Principal

A principal is a user namespace.

| Field | Type | Notes |
| --- | --- | --- |
| `id` | UUID | Stable internal identifier |
| `username` | string | URL-safe, case-sensitive by default |
| `display_name` | string? | Optional |
| `created_at` | timestamp | UTC |
| `updated_at` | timestamp | UTC |

The principal path is `/{username}/`. The username is validated as a single
path segment. It must not contain `/`, NUL, control characters, or `.`/`..`.

### 1.2 Collection

A collection is a calendar or address book.

| Field | Type | Notes |
| --- | --- | --- |
| `id` | UUID | Stable internal identifier |
| `owner` | principal ID | |
| `slug` | string | URL segment under the principal |
| `kind` | enum | `calendar` or `addressbook` |
| `display_name` | string | User-visible |
| `description` | string? | |
| `color` | string? | `#RRGGBB` or `#RRGGBBAA` |
| `components` | enum set | Calendar: always all supported types |
| `source` | `SubscriptionSource?` | Present only for subscriptions |
| `feeds` | `Feed[]` | Read-only projections |
| `created_at` | timestamp | |
| `updated_at` | timestamp | |
| `revision` | integer | Monotonic metadata revision |
| `collection_uid` | UUID | Used in sync tokens and feed ETags |

A calendar's `components` value is always
`["VEVENT", "VTODO", "VJOURNAL"]`. It is stored explicitly for forward
compatibility and reporting, but the server normalises it on write.

An address book's component set is `["VCARD"]` conceptually and is not exposed
as a calendar component set.

### 1.3 Item

An item is a calendar object resource or a contact.

| Field | Type | Notes |
| --- | --- | --- |
| `collection_id` | UUID | |
| `href` | string | One URL path segment, percent-encoded in URLs |
| `uid` | string | iCalendar UID or vCard UID |
| `component` | enum | `VEVENT`, `VTODO`, `VJOURNAL`, `VCARD` |
| `etag` | string | Strong ETag, quoted at protocol boundary |
| `size` | integer | Bytes of canonical payload |
| `payload_path` | path | Relative to collection |
| `created_at` | timestamp | First seen |
| `updated_at` | timestamp | Last write |
| `sequence` | integer | Change sequence within collection |

Uniqueness constraints:

- One item per `(collection_id, href)`.
- One item per `(collection_id, uid)` for calendars. A recurrence set with
  multiple components sharing one UID is stored in one item.
- One item per `(collection_id, uid)` for address books.

An item may contain:

- A `VCALENDAR` with one master `VEVENT`, `VTODO` or `VJOURNAL`.
- A recurrence set: one master plus exceptions, all sharing a UID and each
  distinguished by `RECURRENCE-ID`.
- Optional `VTIMEZONE` components required by its components.
- Optional `VALARM` children.

An item must not mix unrelated UIDs. A recurrence set is not "unrelated" and is
allowed.

### 1.4 SubscriptionSource

A subscription source belongs to a calendar and makes it read-only.

| Field | Type | Notes |
| --- | --- | --- |
| `url` | string | Normalised absolute URL |
| `original_url` | string | What the user entered |
| `refresh_interval_seconds` | integer | Default 3600; min 900 |
| `auth` | `RemoteAuth?` | Encrypted credentials |
| `etag` | string? | Remote `ETag` validator |
| `last_modified` | string? | Remote `Last-Modified` validator |
| `last_attempt_at` | timestamp? | |
| `last_success_at` | timestamp? | |
| `next_attempt_at` | timestamp? | |
| `last_error` | string? | User-safe message |
| `consecutive_failures` | integer | For backoff |
| `remote_display_name` | string? | From `X-WR-CALNAME` |
| `remote_description` | string? | From `X-WR-CALDESC` |
| `remote_color` | string? | From `X-APPLE-CALENDAR-COLOR` |
| `content_hash` | string? | Hash of last accepted normalised data |

`RemoteAuth`:

| Field | Type | Notes |
| --- | --- | --- |
| `kind` | enum | `basic` or `bearer` |
| `username` | string? | Basic only |
| `secret_ciphertext` | string | Encrypted password or bearer token |
| `key_id` | string | Identifies the server encryption key |

The API never returns `secret_ciphertext`. The UI shows "credentials set" and
allows replacement or removal.

### 1.5 Feed

A feed is a read-only projection of a calendar.

| Field | Type | Notes |
| --- | --- | --- |
| `id` | UUID | Stable identifier |
| `name` | string | User-visible |
| `components` | enum set | Non-empty subset of `VEVENT`, `VTODO`, `VJOURNAL` |
| `token_sha256` | string | Hex digest of the bearer token |
| `token_hint` | string | Last 4 characters for UI display |
| `created_at` | timestamp | |
| `expires_at` | timestamp? | |
| `revoked_at` | timestamp? | |
| `last_used_at` | timestamp? | Updated at most once per minute |

The plaintext token exists only in the creation response. It is never stored.

## 2. Path model

### 2.1 URL paths

| Resource | Path |
| --- | --- |
| Root | `/` |
| Principal | `/{user}/` |
| Calendar home | `/{user}/` |
| Calendar | `/{user}/{collection}/` |
| Calendar item | `/{user}/{collection}/{href}` |
| Address book | `/{user}/{collection}/` |
| Address book item | `/{user}/{collection}/{href}` |
| Feed | `/feeds/{token}` |
| Management API | `/.oxicale/api/v1/...` |
| Web UI | `/.oxicale/` and friendly routes |

The base path is configurable (for example `/dav`). Internally, paths are
stored without the base path and without leading or trailing slashes.

### 2.2 Path rules

- Decode percent-encoding exactly once and reject invalid encodings.
- Reject NUL, control characters, backslash and empty segments.
- Reject `.` and `..` segments after decoding.
- Normalise multiple slashes for comparison but preserve the canonical path in
  responses.
- Usernames and collection slugs are single segments.
- Item hrefs are single segments. They may contain percent-encoded characters
  but must not create nested paths.
- Collection and item names are case-sensitive.

## 3. On-disk layout

### 3.1 Native layout

```text
{data_dir}/
├── .oxicale.lock                 process lock
├── index.sqlite3                 derived index (rebuildable)
├── index.sqlite3-wal             SQLite WAL files
├── index.sqlite3-shm
├── tmp/                          atomic write staging
├── users/
│   └── {user}/
│       ├── .principal.json
│       ├── calendar/
│       │   ├── .collection.json
│       │   └── items/
│       │       ├── {href}
│       │       └── ...
│       └── contacts/
│           ├── .collection.json
│           └── items/
│               └── {href}
└── backups/                      optional local snapshots (ops feature)
```

A subscription uses the same layout. Its `.collection.json` contains a
`subscription` object and its `items/` directory is populated by the server.

### 3.2 Item files

- Calendar items are stored as canonical UTF-8 iCalendar with CRLF line
  endings.
- Contact items are stored as canonical UTF-8 vCard with CRLF line endings.
- The filename is the href. It must not start with `.`.
- The file mode is `0600` by default; directories are `0700` by default.
- The payload is the source of truth. The extension is advisory; collection
  kind and content validation determine the type.

Canonicalisation on ingest is limited to what is required for safe storage:

- Ensure UTF-8.
- Normalise line endings to CRLF.
- Ensure the required wrapper component.
- Add a generated UID only when a new item is created without one and the
  protocol allows it.
- Never reorder or reformat content unnecessarily, to keep ETags stable.

The ETag is the SHA-256 of the stored bytes, encoded as a quoted hex string at
the protocol boundary. When a client supplies an ETag-affecting change, the new
payload is stored and the ETag changes.

### 3.3 Collection metadata file

`.collection.json` is a UTF-8 JSON object:

```json
{
  "schema": 1,
  "id": "0d6f2f0e-6a5e-4c5b-b8b9-9b1f5e0f9a11",
  "collection_uid": "8a1d6b42-9c1e-4f0e-9d3c-1a2b3c4d5e6f",
  "kind": "calendar",
  "owner": "alice",
  "slug": "calendar",
  "display_name": "Calendar",
  "description": null,
  "color": "#3b82f6",
  "components": ["VEVENT", "VTODO", "VJOURNAL"],
  "created_at": "2026-10-22T10:00:00Z",
  "updated_at": "2026-10-22T10:00:00Z",
  "revision": 1,
  "subscription": null,
  "feeds": []
}
```

Example with a subscription and a feed:

```json
{
  "schema": 1,
  "id": "7f4f3a2c-1e9b-4c7a-9d0e-2b8c6a5f1d20",
  "collection_uid": "b6b0e2c8-3a4f-4a6e-8a9b-5c1d2e3f4a5b",
  "kind": "calendar",
  "owner": "alice",
  "slug": "team",
  "display_name": "Team",
  "description": null,
  "color": "#10b981",
  "components": ["VEVENT", "VTODO", "VJOURNAL"],
  "created_at": "2026-10-22T10:00:00Z",
  "updated_at": "2026-10-22T11:00:00Z",
  "revision": 17,
  "subscription": {
    "url": "https://example.test/team.ics",
    "original_url": "webcal://example.test/team.ics",
    "refresh_interval_seconds": 3600,
    "auth": null,
    "etag": "\"remote-v3\"",
    "last_modified": "Thu, 22 Oct 2026 09:00:00 GMT",
    "last_attempt_at": "2026-10-22T11:00:00Z",
    "last_success_at": "2026-10-22T11:00:00Z",
    "next_attempt_at": "2026-10-22T12:00:00Z",
    "last_error": null,
    "consecutive_failures": 0,
    "remote_display_name": "Team Calendar",
    "remote_description": null,
    "remote_color": null,
    "content_hash": "sha256:..."
  },
  "feeds": [
    {
      "id": "f1e2d3c4-b5a6-4978-8a9b-0c1d2e3f4a5b",
      "name": "Tasks only",
      "components": ["VTODO"],
      "token_sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
      "token_hint": "x7Qp",
      "created_at": "2026-10-22T10:30:00Z",
      "expires_at": null,
      "revoked_at": null,
      "last_used_at": null
    }
  ]
}
```

Metadata files must be written atomically. Unknown fields must be preserved by
older binaries where possible and ignored with a warning.

### 3.4 Principal metadata

`.principal.json`:

```json
{
  "schema": 1,
  "id": "4a6e2d1c-7b8f-4e9a-9c0d-1e2f3a4b5c6d",
  "username": "alice",
  "display_name": "Alice",
  "created_at": "2026-10-22T10:00:00Z",
  "updated_at": "2026-10-22T10:00:00Z"
}
```

## 4. Index schema

The index is a rebuildable SQLite database. Indicative schema:

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE principals (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE collections (
    id TEXT PRIMARY KEY,
    owner TEXT NOT NULL REFERENCES principals(id),
    slug TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    display_name TEXT NOT NULL,
    color TEXT,
    collection_uid TEXT NOT NULL,
    revision INTEGER NOT NULL,
    read_only INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    UNIQUE (owner, slug)
);

CREATE TABLE items (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    href TEXT NOT NULL,
    uid TEXT NOT NULL,
    component TEXT NOT NULL,
    etag TEXT NOT NULL,
    size INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (collection_id, href)
);

CREATE UNIQUE INDEX items_uid
    ON items (collection_id, uid);

CREATE TABLE changes (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    href TEXT,
    operation TEXT NOT NULL CHECK (operation IN ('put', 'delete')),
    etag TEXT,
    changed_at TEXT NOT NULL,
    PRIMARY KEY (collection_id, seq)
);

CREATE TABLE sync_tokens (
    token TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE subscriptions (
    collection_id TEXT PRIMARY KEY REFERENCES collections(id) ON DELETE CASCADE,
    next_attempt_at TEXT,
    last_success_at TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE feed_usage (
    feed_id TEXT PRIMARY KEY,
    last_used_at TEXT
);
```

The SQLite schema is an implementation detail and may evolve. The canonical
files and documented metadata fields are the compatibility contract.

Index rebuild:

1. Scan `users/`.
2. Parse each `.principal.json`.
3. Parse each `.collection.json`.
4. Scan `items/`, validate headers enough to extract UID and component, and
   compute ETags.
5. Insert rows.
6. Generate a new collection sync generation. Old sync tokens become invalid.

## 5. Sync model

Each collection has a monotonically increasing `revision` stored in metadata and
mirrored in the index. Each item mutation appends a change row.

A sync token encodes:

- collection UUID
- revision/sequence
- a server-generated opaque MAC or random identifier

The token is opaque to clients. Oxicale stores a mapping from token to
`(collection_id, seq)`. If the mapping is missing or the token is older than the
retention window, the server returns an invalid-sync-token error and the client
performs a full sync.

Change retention defaults to 90 days or 100,000 changes per collection,
whichever is larger. Compaction removes old change rows but must not invalidate
tokens that are still within the retention window.

Collection `ctag`/`getctag` is derived from the collection UUID and current
revision or from a hash of item ETags. It must change on every content change
and must not change on unrelated collections.

## 6. Item identity and reconciliation

### 6.1 Client writes

- The href is the item's identity in the collection.
- The UID is the logical identity inside calendar data.
- A `PUT` to an existing href replaces the item only if the UID is unchanged or
  the request is for a new item.
- A `PUT` to a new href with a UID already present in the collection is rejected
  with `409 Conflict` and a `no-uid-conflict` precondition.
- A `MOVE` preserves the item payload and changes the href.
- A `DELETE` removes the item and appends a delete change.

### 6.2 Subscription reconciliation

Remote items are identified by:

1. `UID` of the primary component.
2. `RECURRENCE-ID` value for exception components within a recurrence set.
3. For components without UID, a deterministic hash of the normalised
   component text.

The reconciliation algorithm:

1. Parse the remote calendar into logical items (grouping recurrence sets by
   UID).
2. Compute a stable remote key for each item.
3. Match remote keys to existing subscription items using a persisted
   `remote_key -> href` map.
4. For matched items, compare canonical payload to decide whether to update.
5. For new items, allocate a stable href derived from a hash of the remote key;
   resolve collisions deterministically.
6. For missing items, delete them.
7. Apply all changes in one collection transaction.
8. Update the persisted remote-key map and fetch metadata.

The `remote_key -> href` map is stored in the subscription metadata to avoid
href churn when the remote server reorders components. If the map is lost, the
server can reconstruct stable hrefs from the remote key hash.

A remote item whose UID changes is treated as a deletion and a creation.

## 7. Subscription content rules

Accepted remote content:

- Must be `text/calendar` or a compatible calendar content type. If the content
  type is missing or generic, parse as iCalendar if the body starts with
  `BEGIN:VCALENDAR`.
- Must contain a `VCALENDAR` with `VERSION:2.0`.
- May contain `VEVENT`, `VTODO`, `VJOURNAL` and `VTIMEZONE`.
- Unknown components are ignored for item storage but may be preserved in an
  optional debug snapshot.
- `VALARM` children are preserved.
- Invalid components are skipped if `skip_broken_components` is enabled;
  otherwise the whole refresh is rejected. The default is to reject the refresh
  to avoid silently losing data.

Limits (configurable):

- Maximum response size: 10 MiB.
- Maximum components: 100,000.
- Maximum redirects: 5.
- Connect timeout: 10 seconds.
- Total request timeout: 60 seconds.
- Maximum UID length: 255 bytes.
- Maximum item size after parsing: 2 MiB.

## 8. Feed model

A feed is generated on demand. It is not stored as a collection.

Generation algorithm:

1. Load the calendar's metadata and current item set from a consistent
   snapshot.
2. Select items whose primary component type is in the feed's `components`.
3. Collect the `VTIMEZONE` definitions referenced by selected items.
4. Serialise a single `VCALENDAR` with:
   - `VERSION:2.0`
   - `PRODID:-//Oxicale//Oxicale {version}//EN`
   - `CALSCALE:GREGORIAN`
   - optional `X-WR-CALNAME` from the feed name or collection display name
   - deduplicated `VTIMEZONE` blocks
   - selected components
5. Sort components deterministically by UID, then `RECURRENCE-ID`, then
   serialised bytes.
6. Compute a strong ETag over the filter, collection UUID and selected item
   ETags.
7. Compute `Last-Modified` as the maximum item update time, or collection
   metadata time if there are no items.

Deterministic ordering is required for stable ETags and reproducible tests.

## 9. Invariants

- A principal path contains no collections with nested collections.
- A calendar always advertises all three supported component types.
- A subscription calendar has `source != null` and is read-only by default.
- Feed tokens are never stored in plaintext.
- Feed tokens grant access only to their own filtered data.
- Every visible item has a payload file and an index row after repair.
- Every item has a unique href and a unique UID within its collection.
- No item is partially visible during a write.
- Deleting a collection deletes its items, feeds and subscription state.
- Deleting a principal deletes all owned collections and content.
- A collection's sync revision only increases.
- Feed generation never mutates storage.

## 10. Backup and restore

Backup requirements:

- Back up `users/` and `index.sqlite3` together when the server is stopped, or
  use a filesystem snapshot while it is running.
- If restored without `index.sqlite3`, run `oxicale --rebuild-index`.
- If restored from an old index, run `oxicale --verify-storage`.
- Subscription credentials require the configured encryption key. Back it up
  separately and securely. Without it, subscriptions must be re-authenticated.
- Feed tokens are hashed. Existing feed URLs continue to work after restore if
  metadata is restored. They cannot be recovered from the database.

Restore procedure:

1. Stop Oxicale.
2. Restore `users/`.
3. Optionally restore `index.sqlite3`.
4. Ensure the encryption key is available.
5. Run `oxicale --verify-storage`.
6. Start Oxicale.
7. Trigger a full client resync if sync tokens were invalidated.

## 11. Radicale import mapping

| Radicale | Oxicale |
| --- | --- |
| `collection-root/{user}/` | `users/{user}/` principal |
| `{collection}/.Radicale.props` | `.collection.json` |
| `tag: VCALENDAR` | `kind: calendar` |
| `tag: VADDRESSBOOK` | `kind: addressbook` |
| `tag: VSUBSCRIBED` | calendar with an imported subscription marker |
| Item `.ics` / `.vcf` files | Items with the same href |
| `D:displayname` | `display_name` |
| `C:calendar-description` | `description` |
| `ICAL:calendar-color` | `color` |
| `C:supported-calendar-component-set` | Recorded in the migration report; runtime always mixed |

The import must not rewrite item payloads unless required for validity. Broken
items are reported and either skipped or imported into a quarantine directory,
depending on the selected mode.
