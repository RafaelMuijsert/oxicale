# Oxicale Compatibility

Status: Draft 1

Oxicale aims to be compatible with standard CalDAV/CardDAV clients and to import
Radicale data. It is not a binary-compatible replacement for Radicale.

## 1. Radicale comparison

| Area | Radicale | Oxicale | Notes |
| --- | --- | --- | --- |
| Language | Python | Rust | Clean-room rewrite |
| Configuration | INI | TOML | Migration mapping provided |
| Default calendar model | Client/UI chooses components | Always mixed | Core product difference |
| Setup | Web UI can create typed collections | Zero-choice default calendar | Core product difference |
| Single-type access | Separate collection or component set | Read-only component feed | No duplicated storage |
| External subscriptions | `VSUBSCRIBED` support exists in recent versions | Server-side subscription with fetch state and UI | Oxicale adds scheduler, validation and safe fetch policy |
| Web UI | Create/edit collections, upload, sharing | Onboarding, subscriptions, feeds, metadata | No event editor |
| Storage | Filesystem `collection-root` | Filesystem payloads + metadata + rebuildable index | Import from Radicale |
| Auth backends | htpasswd, LDAP, IMAP, OAuth2, PAM, Dovecot, remote user | Local, trusted header, none, deny (v1) | More backends deferred |
| Rights | owner_only, owner_write, authenticated, from_file | owner_only, authenticated, from_file (planned) | |
| Sharing | Token/map shares, birthday conversion | Not in v1 | Deferred |
| Plugins | Python modules | Rust traits / built-ins | No binary compatibility |
| Hooks | Email, RabbitMQ | Not in v1 | Deferred |
| Web server | WSGI/built-in/hypercorn | Axum/Hyper, built-in TLS | |
| Scheduling | Free-busy support | Free-busy support | No iTIP scheduling |
| Locking | Storage locks | Process lock + per-collection locks | No WebDAV locks |

## 2. Intentional protocol differences

### 2.1 Mixed calendars only

Oxicale ignores narrowing requested via
`C:supported-calendar-component-set` when creating a calendar. The resulting
collection reports all supported components.

Consequences:

- A client that creates a "Tasks" calendar will see all three component types
  advertised.
- A client that only wants tasks can be given a `VTODO` feed instead.
- Existing Radicale typed calendars are imported as mixed. Their old
  `supported-calendar-component-set` is recorded in the migration report.

This is a deliberate product decision, not an interoperability bug.

### 2.2 Subscription resource type

Oxicale advertises both `C:calendar` and CalendarServer's `CS:subscribed` in
`D:resourcetype`.

Rationale:

- Clients that understand `CS:subscribed` can show a read-only subscription.
- Clients that do not may still discover it as a calendar.

Some Radicale versions advertise only `CS:subscribed` for subscribed
collections. Oxicale intentionally adds the calendar type for broader client
compatibility.

### 2.3 Feeds are not collections

Component feeds are plain HTTP ICS endpoints. They do not appear as CalDAV
collections and cannot be written to. They are intended for clients that
subscribe to URLs and for clients that cannot filter component types.

A future version may add read-only CalDAV view collections, but that is not part
of v1.

### 2.4 WebDAV locking

Oxicale does not implement WebDAV `LOCK`/`UNLOCK`. It must not advertise DAV
class 2. Clients that require locking may fail; this is a known limitation.

### 2.5 Scheduling

Oxicale does not implement CalDAV scheduling (RFC 6638). It does support
free-busy reports. It must not advertise auto-scheduling capabilities.

## 3. Client compatibility

The following clients are target compatibility subjects. Each must be tested
for discovery, initial sync, incremental sync, create/update/delete, and
read-only subscription handling.

| Client | Platform | CalDAV | CardDAV | Subscriptions | Feeds |
| --- | --- | --- | --- | --- | --- |
| DAVx5 | Android | Required | Required | Test | Test |
| Thunderbird | Desktop | Required | Via CardBook | Test | Test |
| GNOME Calendar / Contacts | Linux | Required | Required | Test | N/A |
| Evolution | Linux | Required | Required | Test | Test |
| KDE Kontact / Merkuro | Linux | Required | Required | Test | Test |
| Apple Calendar / Contacts | macOS/iOS | Required | Required | Test | Test |
| pimsync | CLI | Required | Required | Test | Test |
| InfCloud / CalDavZAP | Web | Best effort | Best effort | N/A | N/A |
| OneCalendar | Mobile/desktop | Required | Limited | Test | Test |

### 3.1 Client notes

- Some clients do not support server-created calendars and only display
  discovered collections. Oxicale's default calendar makes them immediately
  useful.
- Some clients ignore the `subscribed` resource type and treat subscriptions as
  normal calendars. Oxicale keeps them read-only at the server, so this is
  safe.
- Some clients cannot filter `VTODO` from `VEVENT` in a mixed collection. Use a
  component feed for those clients.
- Apple clients may require HTTPS. Test with a valid certificate.
- Thunderbird's calendar discovery can be sensitive to trailing slashes and
  principal URLs. Use the exact paths in [PROTOCOLS.md](PROTOCOLS.md).
- CardBook and other CardDAV clients may require vCard 3.0. Oxicale accepts and
  stores both 3.0 and 4.0 without rewriting by default.

### 3.2 Core client compatibility contract

This is the release contract for the core-client track. It deliberately does
not attempt full Radicale feature parity.

**In scope**

- Discovery from `/` and directly from the principal URL.
- Calendar and address book creation.
- Item create, read, update, delete and move inside one principal.
- Initial and incremental sync, including restart and index rebuild.
- Calendar queries with component filters, property/text filters and time
  ranges.
- Recurrence handling for ordinary daily, weekly, monthly and yearly rules.
- CardDAV address book query and multiget.
- Read-only subscription discovery and sync.
- Filtered feed consumption with conditional requests.
- Local password auth, trusted-header auth, no-auth development mode and deny.
- Migration of ordinary Radicale data directories, with path normalisation
  reported explicitly.

**Out of scope for this track**

- Radicale sharing, groups, property overlays and birthday conversion.
- LDAP, IMAP, OAuth2, PAM, Dovecot and htpasswd compatibility.
- Python plugin compatibility and Radicale hooks.
- Nested collection trees.
- Whole-collection `PUT` uploads.
- WebDAV locking and CalDAV scheduling.
- Radicale cache/history modes, Git storage and profiling configuration.
- Exact Radicale configuration-file parity.

**Automated gate**

`crates/oxicale/tests/compat.rs` must cover every in-scope operation against a
freshly provisioned instance. A release candidate must also pass the manual
client checklist for DAVx5, Thunderbird, Apple, GNOME/Evolution and KDE.

## 4. Radicale import

### 4.1 Supported input

- Radicale `collection-root` directory.
- `.Radicale.props` metadata.
- `.ics` and `.vcf` item files.
- Nested user collections as produced by Radicale.

### 4.2 Import mapping

See [DATA-MODEL.md](DATA-MODEL.md#11-radicale-import-mapping).

### 4.3 Behaviour

- User directories become principals.
- Collection paths are preserved.
- Item hrefs are preserved.
- Collection tags map to calendar or address book.
- A `VSUBSCRIBED` collection is imported as a subscription only if a source URL
  can be determined from its properties; otherwise it is imported as a
  read-only calendar and the user is asked to provide the source.
- Typed calendars become mixed calendars.
- Invalid items are reported. The default is to skip them and continue; a
  strict mode aborts.
- Existing `.Radicale.cache` and sync data is ignored.
- Existing Radicale lock files are ignored.
- The import is offline. Stop Radicale first or use a consistent snapshot.

### 4.4 Post-import verification

```bash
oxicale migrate radicale --from /var/lib/radicale/collections \
  --to /var/lib/oxicale --dry-run
oxicale migrate radicale --from /var/lib/radicale/collections \
  --to /var/lib/oxicale
oxicale verify-storage --data-dir /var/lib/oxicale
```

The migration report includes:

- Number of principals, collections and items imported.
- Items skipped and why.
- Collections whose component set changed.
- Subscriptions that need credentials or a source URL.
- Feed suggestions for formerly typed collections.

### 4.5 Rollback

An import does not modify the Radicale source. To roll back, stop Oxicale and
start Radicale again against the original data directory. Keep the Radicale
data until the migration is validated.

## 5. Configuration compatibility

Oxicale does not read Radicale's INI files. Use the mapping in
[CONFIGURATION.md](CONFIGURATION.md#5-radicale-migration). A configuration
conversion tool may be added later.

Key behavioural differences:

- There is no `predefined_collections` option. Defaults are automatic.
- There is no typed collection creation option.
- Authentication backends are fewer in v1.
- Hook commands are not executed.
- The web UI has a different feature set.
- The storage layout is different, but an importer is provided.

## 6. Known limitations

- No WebDAV locking.
- No CalDAV scheduling or invitations.
- No sharing between users in v1.
- No LDAP/OIDC/IMAP/PAM/Dovecot authentication in v1.
- No Python plugin compatibility.
- No multi-writer clustering.
- No server-side event rendering or editing.
- No automatic birthday calendar conversion.
- Subscription refresh is periodic, not push. Clients see changes on their next
  sync or poll.
- Feed URLs are bearer secrets and must be protected by the user.

## 7. Compatibility test gates

A release candidate must not ship unless:

- The automated core-client contract in `tests/compat.rs` passes.
- DAVx5 completes discovery, sync and CRUD against a mixed calendar.
- Thunderbird completes discovery, sync and CRUD.
- Apple or Evolution completes discovery, sync and CRUD.
- A subscription created from a mock remote calendar is visible and read-only in
  at least two clients.
- A `VTODO` feed works in a client that cannot separate tasks from events.
- A Radicale import with events, tasks, journals, contacts and a typed calendar
  passes verification, and every normalised collection path is reported.
- Sync tokens survive a server restart and invalidate gracefully after an index
  rebuild.
- Unsupported features return explicit protocol errors, never silently wrong
  data.
