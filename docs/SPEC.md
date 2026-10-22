# Oxicale Product Specification

Status: Draft 1  
Scope: product behaviour and requirements  
Audience: implementers, reviewers, client compatibility testers

This document is the entry point for the Oxicale specification. Detailed
designs live in the linked documents. Requirement identifiers are stable and
should be referenced in issues, tests and pull requests.

## 1. Purpose

Oxicale is a small, self-hosted CalDAV and CardDAV server written in Rust. It is
a clean-room implementation inspired by Radicale. It must be usable with
ordinary calendar and contact clients without custom software.

The product deliberately removes one common source of friction: choosing
between calendars, task lists and journals during setup. Oxicale provisions a
single mixed calendar and lets clients work with all supported component types.
When a client cannot cope with a mixed calendar, the user can create a filtered
feed from the web interface.

The second defining feature is server-side subscription to external ICS
calendars. The server fetches the remote calendar and republishes it to all
clients. Subscriptions are not stored in each client separately.

## 2. Goals and non-goals

### 2.1 Goals

- G1: Be a reliable CalDAV/CardDAV server for individuals and small groups.
- G2: Provide a zero-choice first-run experience with a working mixed calendar.
- G3: Store events, tasks and journal entries together without data loss.
- G4: Let users subscribe to external ICS calendars from the web UI and see the
  result on every client.
- G5: Let users expose a read-only, single-component ICS feed of a calendar
  without duplicating storage.
- G6: Keep calendar and contact payloads as ordinary files that can be backed
  up and inspected.
- G7: Make safe behaviour the default, especially for network fetching and
  authentication.
- G8: Remain compatible with widely used CalDAV/CardDAV clients.
- G9: Allow migration from a Radicale data directory.

### 2.2 Non-goals

- N1: A browser-based event or contact editor.
- N2: CalDAV scheduling, iTIP/iMIP invitation processing, or attendee replies.
- N3: Full WebDAV locking and WebDAV ACL.
- N4: Multi-tenant SaaS administration, billing or quotas beyond simple limits.
- N5: Binary compatibility with Python Radicale plugins.
- N6: Running multiple server processes as concurrent writers against one
  filesystem data directory.
- N7: Perfect equivalence with every Radicale configuration option.

## 3. Users and primary flows

### 3.1 Personas

- **Owner**: Runs Oxicale for themselves or a household. Wants a calendar that
  works on Android, iOS, desktop and Thunderbird without configuration theory.
- **Small-group admin**: Runs Oxicale for a team. Wants accounts, backups and
  predictable access control.
- **Client-limited user**: Has an application that cannot distinguish events
  from tasks. Needs a clean feed of only the relevant component type.

### 3.2 Primary flows

1. **First login**: The user authenticates. A principal and a default mixed
   calendar are created automatically. The dashboard is immediately usable.
2. **Connect a client**: The user copies the server URL and username into a
   CalDAV client. The client discovers the mixed calendar and synchronises it.
3. **Add a subscription**: The user pastes a `webcal://` or `https://` URL into
   the web UI. Oxicale fetches it, validates it, and publishes it as a
   read-only calendar. All connected clients discover it on their next refresh.
4. **Create a task-only feed**: The user opens the advanced settings of a
   calendar, creates a feed named "Tasks only" with `VTODO` selected, and copies
   the generated URL into a client that only understands one component type.
5. **Import from Radicale**: The admin stops Radicale, runs the Oxicale import
   command against its data directory, and starts Oxicale. Existing items and
   collection URLs continue to work.

## 4. Terminology

| Term | Meaning |
| --- | --- |
| Principal | A user namespace at `/{user}/`. It owns collections. |
| Collection | A calendar or address book. It has a URL, metadata and items. |
| Mixed calendar | A calendar that accepts `VEVENT`, `VTODO` and `VJOURNAL`. |
| Item | One calendar object resource (`.ics`) or contact (`.vcf`). |
| Subscription | A read-only calendar whose content is fetched from a remote ICS URL. |
| Component feed | A read-only ICS projection of a collection with a component filter. |
| Feed token | An unguessable bearer secret that grants read access to one feed. |
| Sync token | An opaque CalDAV token used by clients to request incremental changes. |
| href | The URL path segment of an item inside a collection. |
| ctag | A collection change tag exposed to clients. |

## 5. Functional requirements

### 5.1 Onboarding and default collections

- **FR-ONB-1**: On the first successful authentication, Oxicale must provision
  the user's principal if it does not exist. Provisioning must be idempotent.
- **FR-ONB-2**: Provisioning must create one default mixed calendar at
  `/{user}/calendar/` with display name `Calendar`, unless a collection at that
  path already exists.
- **FR-ONB-3**: If CardDAV is enabled, provisioning must also create one default
  address book at `/{user}/contacts/` with display name `Contacts`.
- **FR-ONB-4**: The first-run experience must not present a wizard, a collection
  type selector, or a choice between calendars, tasks and journals.
- **FR-ONB-5**: The default calendar must accept `VEVENT`, `VTODO` and
  `VJOURNAL` from the moment it is created.
- **FR-ONB-6**: If a user has no calendar for any reason, the next web login
  must repair the account by creating the default mixed calendar.
- **FR-ONB-7**: The web dashboard must be usable immediately after the first
  login. It must not require the user to create a collection first.

### 5.2 Mixed-calendar model

- **FR-MIX-1**: Every calendar collection in Oxicale is mixed. The canonical
  storage does not distinguish event calendars from task lists.
- **FR-MIX-2**: The CalDAV property `supported-calendar-component-set` for every
  calendar must report `VEVENT`, `VTODO` and `VJOURNAL`.
- **FR-MIX-3**: The web UI must not offer a choice of calendar component types
  when creating or editing a calendar.
- **FR-MIX-4**: A `MKCALENDAR` request that specifies a narrower component set
  must still result in a mixed calendar. Oxicale may accept the request and
  normalise the resulting collection; it must not create a task-only or
  event-only collection.
- **FR-MIX-5**: Existing collections imported from Radicale retain their items
  and URLs. Their advertised component set becomes mixed.
- **FR-MIX-6**: Single-type access is provided only through component feeds
  (section 5.5) or, in the future, through read-only view collections. It must
  never require duplicating items into a second collection.
- **FR-MIX-7**: A calendar may contain a recurrence set with multiple components
  sharing one UID. UID uniqueness is enforced per calendar object resource, not
  per component.
- **FR-MIX-8**: Clients that PUT a component type not currently present in the
  collection must succeed as long as it is one of the supported types and
  passes validation.

### 5.3 Collections and items

- **FR-COL-1**: A collection must have an immutable kind (`calendar` or
  `addressbook`), an owner, a URL slug, a display name, an optional description,
  an optional colour, and a collection UUID.
- **FR-COL-2**: Calendar and address book collections must not contain child
  collections.
- **FR-COL-3**: Collection discovery must expose collections that are direct
  children of the principal path.
- **FR-COL-4**: Collection deletion must be permitted by default for the owner.
- **FR-COL-5**: Item hrefs must be stable across updates. Changing an item's
  content must not change its href unless the client explicitly moves it.
- **FR-COL-6**: Item payloads must be validated before becoming visible. Invalid
  input must be rejected with an appropriate protocol error and must not
  damage existing data.
- **FR-COL-7**: Calendar object resources must be valid iCalendar objects with
  a `VCALENDAR` wrapper and at least one supported component. One object may
  contain a master component and its recurrence exceptions.
- **FR-COL-8**: Contacts must be valid vCards with a `UID`. Both vCard 3.0 and
  4.0 must be accepted.
- **FR-COL-9**: Item writes must be atomic. A crash during a write must not
  leave a partially written item visible to clients.
- **FR-COL-10**: ETags must change when the stored payload or relevant metadata
  changes, and must remain stable otherwise.

### 5.4 External calendar subscriptions

- **FR-SUB-1**: An authenticated user must be able to create a subscription from
  the web UI by providing an external ICS URL.
- **FR-SUB-2**: The server must accept `https://`, `http://` when explicitly
  enabled, and `webcal://` URLs. `webcal://` must be normalised to `https://`.
- **FR-SUB-3**: A subscription must be stored as a server-side collection in the
  user's namespace and exposed to clients as a read-only calendar.
- **FR-SUB-4**: The subscription collection must be discovered by clients using
  ordinary CalDAV discovery. It must appear as a calendar and must carry the
  CalendarServer `subscribed` resource type marker in addition to the calendar
  type.
- **FR-SUB-5**: All clients must see the same subscription content. Oxicale must
  not fetch the remote calendar per client.
- **FR-SUB-6**: The server must fetch the remote calendar on creation and
  periodically thereafter. The default interval is one hour. The minimum
  configurable interval is fifteen minutes.
- **FR-SUB-7**: The server must use conditional requests (`If-None-Match`,
  `If-Modified-Since`) when the remote server supplies validators. A `304 Not
  Modified` response must not change items or ETags.
- **FR-SUB-8**: A successful refresh must be applied atomically. Clients must
  never observe a partially refreshed subscription.
- **FR-SUB-9**: A failed refresh must preserve the last successfully fetched
  content. The error must be recorded and surfaced in the web UI. Existing
  clients must continue to see the last good data.
- **FR-SUB-10**: A subscription must support stable item identity based on UID
  and, where applicable, recurrence ID. Remote updates replace the matching
  item; remote removals delete it.
- **FR-SUB-11**: Subscription collections must reject item creation, update,
  deletion and content-changing `PROPPATCH` requests from clients. Local
  display-name, description and colour overrides are allowed.
- **FR-SUB-12**: The remote calendar's `X-WR-CALNAME`, description and colour may
  be used as defaults for metadata when the user has not set an override.
- **FR-SUB-13**: The user must be able to trigger a manual refresh, change the
  refresh interval, edit local metadata, and delete the subscription.
- **FR-SUB-14**: A subscription must participate in sync. Any applied change must
  advance the collection sync token and change its ctag.
- **FR-SUB-15**: The server must enforce redirect, timeout, size, component-count
  and content-type limits when fetching. See [SECURITY.md](SECURITY.md).
- **FR-SUB-16**: Remote credentials are optional and must be encrypted at rest.
  They must not be logged or returned by the API.
- **FR-SUB-17**: A component feed may be created from a subscription collection
  as well as from a local calendar.
- **FR-SUB-18**: If the first fetch fails, subscription creation fails and no
  collection is created. If a later fetch fails, the collection remains.

### 5.5 Component feeds

- **FR-FEED-1**: An authenticated user must be able to create a component feed
  for any calendar they can read, from the calendar's advanced settings.
- **FR-FEED-2**: A feed has a name, a non-empty set of component types
  (`VEVENT`, `VTODO`, `VJOURNAL`), and an optional expiry date.
- **FR-FEED-3**: A feed must be a read-only projection. It must not create a
  second collection and must not copy items into separate storage.
- **FR-FEED-4**: Feed output must be a valid iCalendar document containing only
  the selected component types and the time zones required by them.
- **FR-FEED-5**: A feed must be reachable at an unguessable URL. The default
  form is `GET /feeds/{token}`. The token must contain at least 256 bits of
  entropy and be stored only as a cryptographic hash.
- **FR-FEED-6**: Creating a feed must return the full URL exactly once. Later
  reads may show only a masked token.
- **FR-FEED-7**: Feed responses must support `GET` and `HEAD`, conditional
  requests with `ETag` and `Last-Modified`, and must return `304` when unchanged.
- **FR-FEED-8**: Feed content must reflect the current collection state. After a
  change to a matching item, the next feed request must return new content and
  a new ETag.
- **FR-FEED-9**: Feed tokens must be revocable immediately. Revocation must
  invalidate the URL for all clients.
- **FR-FEED-10**: A feed with an expiry date must stop working after that date.
- **FR-FEED-11**: The web UI must warn that anyone with the feed URL can read
  the selected component types.
- **FR-FEED-12**: A feed must be usable as an ICS subscription URL in third
  party clients. It must also work through the Oxicale subscription feature.
- **FR-FEED-13**: Feed access must be rate limited and must not reveal whether a
  guessed token belongs to an existing collection.
- **FR-FEED-14**: The default feed does not require an Oxicale login. A future
  configuration may require HTTP auth in addition to the token.

### 5.6 CalDAV, CardDAV and WebDAV

- **FR-PROTO-1**: Oxicale must implement the WebDAV methods required for
  discovery and object management: `OPTIONS`, `PROPFIND`, `PROPPATCH`,
  `MKCOL`, `MKCALENDAR`, `GET`, `HEAD`, `PUT`, `DELETE` and `MOVE`.
- **FR-PROTO-2**: Oxicale must implement CalDAV reports `calendar-query`,
  `calendar-multiget`, `free-busy-query` and `sync-collection`.
- **FR-PROTO-3**: Oxicale must implement CardDAV reports `addressbook-query`,
  `addressbook-multiget` and `sync-collection`.
- **FR-PROTO-4**: Time-range filtering must handle `VEVENT`, `VTODO` and
  `VJOURNAL`, recurrence, floating times, all-day dates and time zones.
- **FR-PROTO-5**: Sync tokens must be opaque to clients. An invalid or expired
  token must produce a protocol error that causes the client to perform a full
  resynchronisation, not silent data loss.
- **FR-PROTO-6**: Read-only collections must reject content writes with a clear
  WebDAV error, while still allowing allowed local metadata changes.
- **FR-PROTO-7**: XML parsing must be hardened against external entities,
  excessive depth and oversized payloads.
- **FR-PROTO-8**: Protocol details and property support are defined in
  [PROTOCOLS.md](PROTOCOLS.md).

### 5.7 Web interface

- **FR-WEB-1**: The web UI must provide login, logout, dashboard, collection
  metadata editing, subscription management and feed management.
- **FR-WEB-2**: The first-run experience must not include a setup wizard or a
  collection type selector.
- **FR-WEB-3**: The dashboard must show the default calendar, subscriptions,
  address books, item counts and connection details.
- **FR-WEB-4**: Subscription creation must validate the URL and report fetch
  errors clearly before or during creation.
- **FR-WEB-5**: Feed management must live behind an `Advanced` disclosure for
  each calendar.
- **FR-WEB-6**: The UI must not be required for normal CalDAV/CardDAV usage.
- **FR-WEB-7**: The UI must be usable without JavaScript for the core flows, or
  must degrade to a documented minimum. Progressive enhancement is preferred.
- **FR-WEB-8**: Detailed flows are defined in [WEB-UI.md](WEB-UI.md).

### 5.8 Authentication and authorization

- **FR-AUTH-1**: The default authentication backend must reject unauthenticated
  access. A no-auth backend must only be available when explicitly configured
  and must warn when bound beyond loopback.
- **FR-AUTH-2**: Local passwords must be stored using Argon2id with per-password
  salts and documented parameters.
- **FR-AUTH-3**: Oxicale must support trusted reverse-proxy header
  authentication. Trusted proxy addresses must be configured explicitly.
- **FR-AUTH-4**: The default authorization model is owner-only: an authenticated
  user may read and write their own principal and its descendants.
- **FR-AUTH-5**: Feed tokens grant read-only access to exactly one feed and
  nothing else.
- **FR-AUTH-6**: Subscription collections are read-only to clients regardless of
  the user's normal write permission.
- **FR-AUTH-7**: Authentication failures must be rate limited and must not leak
  whether a username exists.
- **FR-AUTH-8**: Future backends (LDAP, OIDC, IMAP, PAM) must be addable without
  changing the protocol layer.

### 5.9 Storage and synchronisation

- **FR-STO-1**: Calendar and contact payloads must be stored as ordinary files.
- **FR-STO-2**: Collection metadata must be stored in a documented,
  human-readable format.
- **FR-STO-3**: A derived index may be used for queries and sync. It must be
  rebuildable from canonical files.
- **FR-STO-4**: Writers must serialise access per collection. The server must
  not expose partially written state.
- **FR-STO-5**: The storage must survive unclean shutdown without losing
  acknowledged writes.
- **FR-STO-6**: A verification command must check item parseability, orphaned
  metadata and index consistency.
- **FR-STO-7**: See [DATA-MODEL.md](DATA-MODEL.md) for layout and invariants.

### 5.10 Operations and observability

- **FR-OPS-1**: Configuration must be validated at startup with actionable error
  messages.
- **FR-OPS-2**: The server must expose structured logs with secret redaction.
- **FR-OPS-3**: The server must expose optional Prometheus metrics for requests,
  storage operations, subscription refreshes and feed requests.
- **FR-OPS-4**: A health endpoint must distinguish process liveness from storage
  readiness.
- **FR-OPS-5**: The binary must support `--check-config`, `--verify-storage`,
  `--rebuild-index` and `--migrate-radicale`.
- **FR-OPS-6**: TLS must be configurable directly or delegated to a reverse
  proxy with documented trusted-header settings.

## 6. Non-functional requirements

- **NFR-1 (Compatibility)**: Works with the clients listed in
  [COMPATIBILITY.md](COMPATIBILITY.md) for the tested operations.
- **NFR-2 (Performance)**: A calendar with 10,000 items must answer a
  time-range query in under 500 ms at p95 on a modest VPS, with a warm index.
- **NFR-3 (Scalability)**: A single instance should support at least 100 users
  and 1,000 collections on commodity hardware. Exact limits are deployment
  dependent.
- **NFR-4 (Reliability)**: Acknowledged writes survive process crashes. Index
  loss must not lose item payloads.
- **NFR-5 (Security)**: No known critical vulnerabilities in default network
  fetching, authentication or token handling. See [SECURITY.md](SECURITY.md).
- **NFR-6 (Maintainability)**: Protocol, storage and web layers must be
  separable and independently testable.
- **NFR-7 (Portability)**: Linux is the primary target. macOS and BSD should
  work. Windows support is best effort.
- **NFR-8 (Footprint)**: The release binary should be a single executable
  without a Python runtime or external service dependency. SQLite is embedded.

## 7. Acceptance criteria for the new features

### 7.1 Zero-choice onboarding

```gherkin
Scenario: A new user logs in for the first time
  Given a new username "alice" with a valid password
  When alice logs in to the web interface for the first time
  Then no setup wizard or collection-type selector is shown
  And a calendar at "/alice/calendar/" exists
  And that calendar supports VEVENT, VTODO and VJOURNAL
  And the dashboard is immediately usable
```

```gherkin
Scenario: A client creates a task-only calendar
  Given alice is authenticated
  When the client sends MKCALENDAR with only VTODO in the component set
  Then the request succeeds
  And the resulting collection still reports VEVENT, VTODO and VJOURNAL
  And the client can upload all three component types
```

### 7.2 External subscription

```gherkin
Scenario: A user subscribes to an external ICS calendar
  Given a reachable remote calendar at "https://example.test/team.ics"
  And the remote calendar contains one VEVENT and one VTODO
  When alice adds the URL as a subscription in the web interface
  Then a read-only calendar collection is created under alice's principal
  And a CalDAV client discovers the collection
  And the client can read both components
  And the client cannot modify or delete items in the collection
```

```gherkin
Scenario: A remote update is reflected to every client
  Given alice has an existing subscription and two connected clients
  When the remote calendar changes an event and adds a task
  And Oxicale refreshes the subscription
  Then both clients observe the new sync token on their next sync
  And both clients receive the changed event and the new task
  And no client receives a duplicate item
```

```gherkin
Scenario: A refresh failure preserves data
  Given a subscription has successfully fetched content
  When the next refresh returns HTTP 500 or invalid iCalendar data
  Then the existing items remain unchanged
  And the subscription is marked as degraded with the error visible in the UI
  And clients continue to read the last good content
```

### 7.3 Component feed

```gherkin
Scenario: A task-only feed
  Given alice's calendar contains events and tasks
  When alice creates a feed named "Tasks only" with VTODO selected
  Then a URL with a new unguessable token is returned once
  And GET on that URL returns a VCALENDAR containing only VTODO components
  And events and journal entries are not present
```

```gherkin
Scenario: Feed updates follow the collection
  Given a task-only feed exists
  When a new task is added to the calendar
  Then the next conditional GET to the feed returns new content and a new ETag
  When an event is added to the calendar
  Then the feed content and ETag may remain unchanged
```

```gherkin
Scenario: Feed revocation
  Given a feed URL is in use by a client
  When alice revokes the feed in the web interface
  Then subsequent requests to the old URL return 404
  And no other collection data is exposed
```

### 7.4 Safety

```gherkin
Scenario: Subscription SSRF protection
  Given the default configuration
  When a user tries to subscribe to a URL that resolves to a loopback or
    private address
  Then the subscription is rejected with a clear error
  And no outbound request is made to that address
```

## 8. Compatibility and migration

- Oxicale must import a Radicale `collection-root` directory. Item hrefs and
  relative collection paths must be preserved.
- Typed Radicale calendars are imported as mixed calendars. Their items are not
  rewritten unless required for validity.
- A migration report must list imported users, collections, items, skipped
  broken items and any changes to advertised component sets.
- Oxicale configuration is TOML; Radicale INI options do not map one to one.
  See [CONFIGURATION.md](CONFIGURATION.md#radicale-migration).
- Behavioural differences are documented in
  [COMPATIBILITY.md](COMPATIBILITY.md).

## 9. Out of scope and future work

These are explicitly deferred and must not block the first release:

- Read-only CalDAV filtered views (the feed feature is the first version).
- Sharing calendars between users, token-based shares and group shares.
- Birthday/contact-to-calendar conversion.
- Email and RabbitMQ hooks.
- LDAP, IMAP, OAuth2, PAM and Dovecot authentication backends.
- A mobile configuration profile generator.
- WebDAV locking and full ACL.
- Server-side scheduling and invitations.
- Multiple-node clustering and external databases.

## 10. Open questions

1. Should the default address book be created unconditionally, or only when
   CardDAV is enabled? Current proposal: only when CardDAV is enabled.
2. Should clients be able to create additional calendars from a CalDAV client,
   or only from the web UI? Current proposal: allow `MKCALENDAR`, always mixed.
3. Should feed tokens support HTTP Basic auth in addition to the URL token?
   Current proposal: token only for v1; optional auth later.
4. Should a subscription's items be exposed with the same hrefs after a remote
   UID change? Current proposal: UID is the identity; a changed UID is treated
   as deletion plus creation.
5. How long should a degraded subscription keep serving last-good data?
   Current proposal: indefinitely, with a warning, until the user deletes it.
6. Should an imported Radicale typed calendar automatically get a matching
   feed? Current proposal: yes, offer it during migration as an option.

## 11. Decision log

| ID | Decision | Rationale |
| --- | --- | --- |
| D1 | One mixed calendar is the only native calendar model | Removes setup friction; avoids split data |
| D2 | Single-type access is a read-only feed | No duplication, clear semantics |
| D3 | Subscriptions are server-side collections | One fetch, all clients see the same data |
| D4 | Subscriptions are read-only | Remote source is authoritative |
| D5 | Feed tokens are bearer secrets stored hashed | Simple client compatibility, revocable |
| D6 | Filesystem item payloads plus rebuildable index | Plain data, good query performance |
| D7 | TOML configuration | Native Rust ecosystem, clear structure |
| D8 | GPL-3.0-or-later proposed | Ecosystem alignment; confirm before code |

## 12. License and provenance

Oxicale is a clean-room implementation. The reference directory may be read for
behaviour, but source code, comments and internal implementation details must
not be copied. Contributors must confirm that contributions are original or
compatible with the project license.

The proposed license is GPL-3.0-or-later. A final decision is required before
the first source commit.
