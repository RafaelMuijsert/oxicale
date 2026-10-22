# Oxicale

Oxicale is a clean-room, Rust implementation of a self-hosted CalDAV and
CardDAV server, inspired by the behaviour of [Radicale](https://radicale.org/).
It stores calendars, tasks, journal entries and contacts on your own
infrastructure and exposes them through standard CalDAV, CardDAV and WebDAV.

The project now contains a runnable Rust implementation of the v1 feature set:
mixed CalDAV calendars, CardDAV address books, zero-choice onboarding,
server-side subscriptions, filtered feeds, the web management UI, Radicale
migration and the documented CLI. The `radicale/` directory remains a reference
implementation used to understand protocol behaviour. It is not part of the
build and no code from it is copied into Oxicale.

## Why Oxicale?

Radicale is excellent because it is small, file-based and predictable. Oxicale
keeps that spirit while making the default experience simpler and adding two
features that are usually missing from small CalDAV servers:

1. **One mixed calendar by default.** New users get a working calendar
   immediately. They are never asked whether they want a calendar, a task list
   or a journal. Events, to-dos and journal entries live together in one
   collection.
2. **Server-side subscriptions to external ICS calendars.** A user can add a
   remote `webcal://` or `https://` calendar from the web interface. Oxicale
   fetches, validates and stores it, then publishes it to every CalDAV client
   as a normal (read-only) calendar collection.
3. **Filtered component feeds.** An advanced per-calendar setting can expose a
   read-only ICS feed containing only one kind of component, for example only
   `VTODO`. This is for clients that cannot separate events from tasks in a
   mixed calendar.

## Documentation

| Document | Contents |
| --- | --- |
| [docs/SPEC.md](docs/SPEC.md) | Product specification, requirements and acceptance criteria |
| [docs/REFERENCE-NOTES.md](docs/REFERENCE-NOTES.md) | What was learned from the Radicale reference and what diverges |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Rust architecture, components and request lifecycle |
| [docs/DATA-MODEL.md](docs/DATA-MODEL.md) | Principals, collections, items, subscriptions, feeds and storage |
| [docs/PROTOCOLS.md](docs/PROTOCOLS.md) | CalDAV, CardDAV, WebDAV, sync and feed endpoints |
| [docs/WEB-UI.md](docs/WEB-UI.md) | Onboarding, dashboard, subscription and feed workflows |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | TOML configuration reference and Radicale migration |
| [docs/SECURITY.md](docs/SECURITY.md) | Threat model, SSRF protection, tokens and hardening |
| [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) | Radicale differences, client support and migration |
| [docs/TESTING.md](docs/TESTING.md) | Test strategy, conformance and acceptance tests |
| [docs/CLIENT-CHECKLIST.md](docs/CLIENT-CHECKLIST.md) | Manual core-client release checklist |
| [docs/NIXOS.md](docs/NIXOS.md) | NixOS module, flake usage and sops/reverse-proxy setup |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Delivery milestones and open decisions |

## Product principles

- **Zero-choice onboarding.** The first login creates a usable account. There is
  no calendar-type wizard and no "events or tasks?" question.
- **Mixed storage, focused views.** A calendar always stores all supported
  component types. Narrow, single-type access is a read-only projection (a
  feed), never a second copy of the data.
- **Standards first.** Clients talk CalDAV, CardDAV and WebDAV. Oxicale does not
  require a custom client.
- **Server-side subscriptions.** A subscription is one collection on the
  server. Every client sees the same data and the same updates.
- **Plain data.** Calendar and contact payloads remain ordinary `.ics` and
  `.vcf` files on disk. Metadata is transparent and documented.
- **Safe by default.** No unauthenticated network access, no SSRF by default,
  no secret tokens in logs, and conservative limits.
- **Small surface.** Prefer a small, auditable core over a large feature matrix.
  Plugins can extend it.

## Planned feature set

- CalDAV calendars with `VEVENT`, `VTODO` and `VJOURNAL`
- CardDAV address books with vCard 3.0 and 4.0
- WebDAV discovery, `PROPFIND`, `PROPPATCH`, `MKCOL`, `MKCALENDAR`, `GET`,
  `HEAD`, `PUT`, `DELETE`, `MOVE` and `REPORT`
- CalDAV `calendar-query`, `calendar-multiget`, `free-busy-query` and
  `sync-collection`
- CardDAV `addressbook-query`, `addressbook-multiget` and `sync-collection`
- Local users with Argon2id password hashes, trusted reverse-proxy headers,
  and explicit no-auth mode for loopback-only development
- Owner-only access by default, with file-based rules planned
- Filesystem item storage with a rebuildable SQLite index
- Web UI for onboarding, collection metadata, subscriptions and feeds
- External ICS subscriptions with conditional fetches and last-good snapshots
- Filtered, revocable, token-protected ICS feeds
- Import from a Radicale data directory
- Prometheus metrics and structured logs

## Non-goals for the first release

- A full calendar or contact editor in the browser
- Server-side CalDAV scheduling (RFC 6638 / iTIP)
- Email invitations and attendee notifications
- A multi-tenant administration console
- Drop-in compatibility with Python Radicale plugins
- WebDAV locking and full WebDAV ACL
- Running multiple writers against one filesystem data directory

See [docs/ROADMAP.md](docs/ROADMAP.md) for deferred work.

## Repository layout

```text
.
├── README.md          This file
├── docs/              Specification and design documents
└── radicale/          Reference implementation (read-only, not built)
```

The Rust workspace lives at the repository root, alongside `docs/` and
`radicale/`.

## Quick start

The project uses devenv for the Rust toolchain and development shell:

```bash
devenv shell

# Create a local configuration and password file.
mkdir -p /tmp/oxicale
cat > oxicale.toml <<'TOML'
[server]
bind = ["127.0.0.1:5232"]
external_url = "http://127.0.0.1:5232"

[storage]
data_dir = "/tmp/oxicale/data"

[auth]
password_file = "/tmp/oxicale/users"
secure_cookies = false
TOML

cargo run -- user add alice --password-file /tmp/oxicale/users --password secret
cargo run -- run --config oxicale.toml
```

Then open <http://127.0.0.1:5232/.oxicale/login>. The first login creates the
principal, a mixed `Calendar` and a `Contacts` address book automatically. Point
a CalDAV/CardDAV client at the server URL and the same username.

Useful commands:

```bash
cargo run -- check-config --config oxicale.toml
cargo run -- verify-storage --data-dir /tmp/oxicale/data
cargo run -- rebuild-index --data-dir /tmp/oxicale/data
cargo run -- migrate radicale --from /var/lib/radicale/collections --to /tmp/oxicale/data --dry-run
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Workspace layout

| Path | Contents |
| --- | --- |
| `crates/oxicale-core` | Domain newtypes, paths, components, tokens and sync tokens |
| `crates/oxicale-ical` | Hardened iCalendar parser, validation, recurrence, filters and feed merging |
| `crates/oxicale-vcard` | vCard 3.0/4.0 validation, canonicalisation and CardDAV filters |
| `crates/oxicale-storage` | Atomic filesystem payloads, metadata, SQLite index, sync journal and feeds |
| `crates/oxicale` | Configuration, auth, sessions, DAV, subscriptions, feeds, web UI, CLI and import |

## Status

The implementation covers the first-release milestones in `docs/ROADMAP.md` and
is exercised by unit, storage, protocol, subscription, feed, web, CLI and
security tests. The documents in `docs/` remain the source of truth; report any
observed divergence as a compatibility bug.

## Clean-room statement

Oxicale is developed from public protocol specifications (RFC 4791, RFC 6352,
RFC 4918, RFC 5545, RFC 6350 and related documents), observable behaviour of
CalDAV/CardDAV clients, and the public documentation of Radicale. The reference
directory is consulted to understand expected behaviour and interoperability
requirements. Code, comments and internal data structures are not copied from
the reference implementation.

## License

The intended license is **GPL-3.0-or-later**, matching the wider ecosystem and
the reference implementation. The final license must be confirmed before the
first code commit. See [docs/SPEC.md](docs/SPEC.md#license-and-provenance).
