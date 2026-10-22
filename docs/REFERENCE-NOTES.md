# Reference Review: Radicale

Status: Informational

This document records what was learned from the `radicale/` reference directory
and how it influenced the Oxicale specification. It is not a code porting guide
and must not be used to copy implementation details.

## 1. What Radicale is

Radicale is a small CalDAV/CardDAV server. Its selling points are a simple
filesystem storage model, broad client compatibility, flexible authentication
and a built-in web UI for collection management. It is written in Python and
supports plugins for authentication, rights, storage, web and hooks.

Relevant reference areas:

| Area | Reference location |
| --- | --- |
| Product and setup docs | `radicale/README.md`, `radicale/DOCUMENTATION.md` |
| Sharing docs | `radicale/SHARING.md` |
| Configuration schema | `radicale/radicale/config.py` |
| Request dispatcher | `radicale/radicale/app/*.py` |
| Storage | `radicale/radicale/storage/multifilesystem*.py` |
| Item model and filtering | `radicale/radicale/item/*.py` |
| Authentication | `radicale/radicale/auth/*.py` |
| Rights | `radicale/radicale/rights/*.py` |
| Sharing | `radicale/radicale/sharing/*.py` |
| Web UI assets | `radicale/radicale/web/internal_data/` |
| Integration and compatibility tests | `radicale/integ_tests/`, `radicale/caldav_compat_tests/` |

## 2. Storage model

Radicale's `multifilesystem` backend stores:

- `collection-root/` as the data root.
- One folder per collection.
- `.Radicale.props` with collection metadata as JSON.
- One `.ics` or `.vcf` file per item.
- `.Radicale.cache/` for derived item, history and sync-token caches.
- `.Radicale.lock` for inter-process locking.

Collection type is indicated by the `tag` property:

- `VCALENDAR`
- `VADDRESSBOOK`
- `VSUBSCRIBED`

The reference already includes a `VSUBSCRIBED` tag and maps the CalendarServer
`CS:subscribed` resource type to it. The web UI can create a "web calendar" with
a source URL. This is a useful partial precedent, but it is not a complete
server-side subscription implementation: Oxicale adds a fetch scheduler,
conditional requests, validation, reconciliation, error state and read-only
enforcement.

**Oxicale decision:** Keep payloads as plain files. Add collection metadata and
a rebuildable index. Preserve Radicale collection paths and item hrefs during
import. Do not copy the cache format.

## 3. Collection types and web UI

The Radicale web UI models collection types as combinations:

- `CALENDAR_JOURNAL_TASKS`
- `CALENDAR_JOURNAL`
- `CALENDAR_TASKS`
- `JOURNAL_TASKS`
- `CALENDAR`
- `JOURNAL`
- `TASKS`
- `ADDRESSBOOK`
- `WEBCAL`
- `PRINCIPAL`

The creation form defaults to `CALENDAR_JOURNAL_TASKS` and lets the user narrow
the supported component set. Existing collections can be edited within their
type family.

**Oxicale decision:** Remove this choice entirely. Every calendar is mixed.
The only single-type surface is a read-only component feed. This directly
addresses the product requirement that setup must not ask users to create
separate calendar/TODO collections.

## 4. CalDAV/CardDAV protocol behaviour

The reference implements the expected WebDAV methods and CalDAV/CardDAV reports:

- Discovery and properties in `app/propfind.py`.
- Calendar/address book reports in `app/report.py`.
- Collection creation in `app/mkcol.py` and `app/mkcalendar.py`.
- Item writes in `app/put.py`.
- Collection/item reads in `app/get.py` and `app/head.py`.
- Move in `app/move.py`.
- Free-busy generation and recurrence expansion in `report.py` and
  `item/filter.py`.
- Sync tokens and collection change tags through storage metadata.

The content type for calendar items can include a `component` parameter
(`VEVENT`, `VTODO`, `VJOURNAL`) derived from the primary component.

**Oxicale decision:** Follow the same protocol surface and property semantics,
but implement them from the RFCs and compatibility tests. Advertise only
implemented capabilities. Do not implement WebDAV locking or scheduling in v1.

## 5. Mixed-content handling

Radicale stores collection component sets in
`C:supported-calendar-component-set`, but its item validator accepts
`VEVENT`, `VTODO` and `VJOURNAL` within the same `VCALENDAR`. Reports filter by
component type using `C:comp-filter`.

This confirms that a mixed calendar is technically feasible and that many
clients already send component filters. Oxicale makes the mixed model the only
model rather than an option.

**Oxicale decision:** Always report all three components. Accept all three in a
collection. Normalise `MKCALENDAR` requests that ask for a subset.

## 6. Sync and reports

The reference supports sync tokens, ctags, calendar queries, multiget,
free-busy and address book queries. It uses a cache/history directory for sync
state and treats old sync tokens as invalid, forcing clients to resync.

**Oxicale decision:** Make sync tokens opaque, store a change journal in a
rebuildable index, and explicitly return a valid-sync-token precondition error
when a token is too old. Index loss must cause resynchronisation, not data loss.

## 7. Authentication

Reference backends include:

- `none`, `denyall`
- `htpasswd`
- `remote_user`, `http_remote_user`, `http_x_remote_user`
- LDAP, IMAP, OAuth2, PAM, Dovecot

Authentication is often delegated to a reverse proxy. Password hashing supports
multiple htpasswd schemes.

**Oxicale decision:** Start with local Argon2id, trusted proxy header, no-auth
(development only) and deny. Add more backends through the `Authenticator`
trait later. Do not claim Radicale plugin compatibility.

## 8. Authorization and sharing

The reference supports owner-only, owner-write, authenticated and file-based
rights. Recent versions add sharing by token and by map, property overlays and
birthday conversion. These are powerful but add significant complexity.

**Oxicale decision:** Owner-only by default in v1. Design the rights layer as
capabilities so sharing can be added later. Keep subscription read-only
enforcement independent of the rights policy. Defer sharing, groups and
birthday conversion.

## 9. Configuration

Radicale uses INI files with plugin-specific options and many logging and
compatibility knobs. It supports predefined collections, storage hooks, web
plugins, SMTP/RabbitMQ hooks and detailed logging conditions.

**Oxicale decision:** Use TOML with a smaller validated surface. Provide a
migration mapping rather than trying to read every Radicale option. Defaults
must be safe. Defer hooks and plugin configuration.

## 10. Web UI

The reference web UI supports:

- Login
- Creating and editing collections of many component-type combinations
- Uploading collections from files
- Downloading collections
- Deleting collections
- Sharing controls in recent versions
- Free-busy URL display
- Connection helper and mobile configuration

It does not edit individual events or contacts.

**Oxicale decision:** Keep the web UI as a management surface. Remove collection
type options. Add subscription management and the advanced feed manager. Keep
connection details prominent. Do not add an event editor in v1.

## 11. Client support

The reference documentation lists DAVx5, OneCalendar, GNOME, Evolution, KDE,
Thunderbird, InfCloud/CalDavZAP/CardDavMATE and pimsync, and notes that many
clients cannot create collections and rely on server discovery.

**Oxicale decision:** Treat DAVx5, Thunderbird, GNOME/Evolution, KDE and Apple
clients as primary compatibility targets. The zero-choice default calendar is
especially valuable for clients that cannot create collections. Subscriptions
are exposed as normal read-only calendars so discovery works everywhere.

## 12. Tests

Reference tests cover authentication, storage, sharing, server behaviour, web
behaviour, hooks and external CalDAV compatibility. The integration suite uses
scenario-based tests.

**Oxicale decision:** Build a similar scenario harness, plus targeted tests for
the new default model, subscriptions, feeds and SSRF. Use property-based tests
and fuzzing for parsers.

## 13. Ideas deliberately not carried over

- Component-type selection in the UI.
- Predefined typed collections.
- Python plugin compatibility.
- Storage hook shell commands that run arbitrary code.
- Complex property overlay and birthday conversion in v1.
- Multiple storage cache subfolder modes.
- Full parity with every Radicale logging option.

These are either replaced by simpler defaults or deferred until there is a
concrete user need.

## 14. Summary of Oxicale's deliberate divergence

1. Mixed calendars are the only calendars.
2. Setup has no type choice.
3. Single-type access is a read-only feed, not a separate collection.
4. Subscriptions are fetched and maintained by the server, with safety controls.
5. Configuration is TOML and smaller.
6. Storage is filesystem-first with a rebuildable index.
7. Sharing, scheduling, hooks and plugin compatibility are deferred.
