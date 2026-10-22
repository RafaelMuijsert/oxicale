# Oxicale Roadmap

Status: Draft 1

This roadmap is intentionally modest. The first release focuses on a reliable
mixed-calendar CalDAV/CardDAV core, a zero-choice onboarding experience,
server-side subscriptions and filtered feeds.

## Milestone 0: Specification

- [x] Examine the Radicale reference behaviour.
- [x] Write product specification and architecture.
- [x] Define data model, protocol surface, web UI, configuration and security.
- [x] Define compatibility and testing strategy.
- [ ] Review open questions.
- [ ] Confirm license and project governance.
- [ ] Validate the mixed-calendar decision against target clients.

Exit criteria: documentation is internally consistent and the open questions
have owners.

## Milestone 1: Core skeleton

- Rust workspace and CI.
- Configuration loading and validation.
- Structured logging and basic metrics.
- HTTP server with graceful shutdown.
- Path normalization and security middleware.
- Domain types and error model.

Exit criteria: `oxicale check-config` and a health endpoint work; no DAV
features required.

## Milestone 2: Storage and authentication

- Filesystem payload store with atomic writes.
- Collection and principal metadata files.
- SQLite index and rebuild command.
- Change journal and sync tokens.
- Local password authentication and sessions.
- Owner-only rights.
- Storage verification and crash-injection tests.

Exit criteria: create, read and delete items through an internal API; restart
and verification preserve data.

## Milestone 3: CalDAV/CardDAV core

- WebDAV discovery and properties.
- Calendar and address book CRUD.
- Calendar object and vCard validation.
- `calendar-query`, `calendar-multiget`, `addressbook-query` and
  `addressbook-multiget`.
- `sync-collection`.
- Free-busy query.
- Conditional requests and ETags.
- Read-only collection enforcement.

Exit criteria: DAVx5 and Thunderbird can discover, sync and edit a mixed
calendar; CardDAV works with at least one client.

## Milestone 4: Zero-choice onboarding and web UI

- Automatic principal and default collection provisioning.
- Server-rendered login and dashboard.
- No type selector anywhere.
- Calendar and address book metadata editing.
- Connection details panel.
- Management API.
- CSRF, session hardening and accessibility pass.

Exit criteria: a new user logs in and is immediately productive; no calendar
type is ever requested.

## Milestone 5: External subscriptions

- Subscription data model and metadata.
- URL policy and SSRF-safe HTTP fetcher.
- iCalendar parser and validation.
- Stable reconciliation and atomic application.
- Refresh scheduler with backoff and jitter.
- Read-only subscription collections in CalDAV discovery.
- Subscription UI: create, edit, refresh, delete, status.
- Credential encryption.
- Tests for failures, redirects, limits and restart recovery.

Exit criteria: a user can subscribe to a remote ICS calendar and two clients see
the same updates; failures preserve last-good data.

## Milestone 6: Component feeds

- Feed metadata and token service.
- Deterministic filtered ICS generation.
- Conditional HTTP responses.
- Rate limiting and revocation/rotation.
- Advanced feed UI.
- Feed creation from local calendars and subscriptions.
- Tests for filtering, ETags and token security.

Exit criteria: a `VTODO`-only feed works in a client that cannot separate tasks
from events, and revocation is immediate.

## Milestone 7: Migration and hardening

- Radicale import tool.
- Migration report and dry-run.
- Index rebuild and storage verification commands.
- Backup/restore documentation and drill.
- Fuzzing and security review.
- Performance benchmarks.
- Client compatibility matrix.

Exit criteria: a realistic Radicale data set imports cleanly and all release
gates pass.

## Milestone 8: First release

- Packaging: static binary, container image, systemd example.
- Documentation for installation and reverse proxies.
- Security policy and release notes.
- Versioned data schema and upgrade notes.

Exit criteria: a tagged release that passes the compatibility and security
gates.

## Post-1.0 candidates

In rough priority order:

1. File-based rights rules and group support.
2. Read-only CalDAV filtered views in addition to HTTP feeds.
3. Sharing calendars between users with token or map grants.
4. More authentication backends: LDAP, OIDC, IMAP, PAM, Dovecot.
5. Hooks for email notifications, webhooks and change streams.
6. Birthday calendars from contacts.
7. WebDAV locking if client demand justifies it.
8. Multi-instance support via an external database and shared object storage.
9. An optional minimal event/task viewer in the web UI.
10. Push notifications or WebSocket hints for faster client refresh.

## Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Mixed calendars confuse clients that assume typed collections | Compatibility | Test with target clients; provide feeds; document workarounds |
| SSRF in subscriptions | Critical | Strict policy, DNS pinning, redirect checks, tests, review |
| Feed token leakage | High | Hash tokens, redact logs, one-time display, revocation |
| iCalendar parser gaps | High | Fixture corpus, fuzzing, conservative validation |
| Sync token loss after index rebuild | Medium | Explicit invalid-sync-token response; clients resync |
| Scope creep from Radicale parity | Schedule | Defer sharing, plugins and scheduling to post-1.0 |
| Rust ecosystem crate quality for iCal/vCard | Medium | Evaluate early; keep a local validation layer; be ready to implement |
| Filesystem durability on exotic setups | Medium | Document supported filesystems; fsync by default; crash tests |

## Open decisions

1. License confirmation.
2. Whether the initial implementation is one crate or a workspace.
3. Whether the default address book is created unconditionally.
4. Whether CalDAV clients may create additional calendars in v1.
5. Whether feed tokens may also require Basic auth.
6. How long degraded subscriptions keep serving last-good data.
7. Whether migration automatically proposes feeds for typed collections.
