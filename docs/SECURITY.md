# Oxicale Security Design

Status: Draft 1

This document describes the threat model and the security requirements for
Oxicale. Security-sensitive features must be reviewed against this document.

## 1. Threat model

### 1.1 Assets

- Calendar and contact content.
- User credentials and session tokens.
- Feed bearer tokens.
- Remote subscription credentials.
- The server's ability to make outbound network requests.
- Availability of the service and storage.
- Server configuration and encryption keys.

### 1.2 Adversaries

- **Unauthenticated network attacker**: can reach the HTTP port.
- **Authenticated user**: may attempt to read other users' data, escalate
  privileges, exhaust resources or use the server as a network proxy.
- **Malicious remote calendar**: controls content fetched by a subscription,
  including iCalendar structure and HTTP responses.
- **Token thief**: possesses a feed URL or session cookie.
- **Local unprivileged user**: can read files or process memory if permissions
  are wrong.
- **Malicious client**: sends malformed XML, oversized requests, path traversal
  attempts or conflicting writes.

### 1.3 Trust boundaries

- The HTTP listener is untrusted.
- Reverse proxies are untrusted unless explicitly listed in
  `server.trusted_proxies`.
- Remote subscription URLs are untrusted, including DNS, redirects and content.
- The data directory is trusted but must be protected by filesystem
  permissions.
- Configuration files and encryption keys are trusted.

## 2. Authentication

### 2.1 Local passwords

- Hash with Argon2id.
- Parameters must be documented and meet current OWASP guidance at release
  time. Proposed baseline: memory 64 MiB, iterations 3, parallelism 4.
- Use a unique random salt per password.
- Compare in constant time.
- Never log or return passwords.
- Password file mode must be `0600`; refuse to load a world-readable file.

### 2.2 HTTP Basic

- Only accepted over TLS in production.
- The `Authorization` header is never logged.
- A failed attempt returns `401` with a generic body.
- Rate limit failures by username and source address.
- Add a small constant delay or use a dummy hash verification to reduce
  username enumeration timing differences.

### 2.3 Sessions

- 256-bit random session identifiers.
- Stored server-side with expiry and optional user-agent binding.
- Cookie flags: `HttpOnly`, `Secure` when TLS is enabled, `SameSite=Lax`.
- Session ID rotation on login and privilege change.
- Logout deletes the server-side session.
- Idle and absolute expiry are enforced.
- Sessions are invalidated when the password changes.

### 2.4 Trusted header authentication

- Only active when the direct peer is in `server.trusted_proxies`.
- The configured header must be stripped from untrusted incoming requests.
- If `behind_proxy` is false or the peer is not trusted, the header is ignored.
- Usernames from headers undergo the same validation and length limits as local
  usernames.
- Document that the proxy must overwrite, not append, the header.

### 2.5 No-auth mode

- Explicitly configured only.
- Refuse to bind beyond loopback unless an additional override is set.
- Show a warning at startup and in the web UI.
- Never the default.

## 3. Authorization

- Default policy is owner-only.
- Authorize the exact normalized path after percent-decoding and segment
  validation.
- Do not rely on URL prefixes alone; check segment boundaries.
- Subscription content is read-only regardless of the user's role.
- Feed tokens authorize exactly one feed.
- Management API endpoints check that the collection belongs to the current
  principal.
- Administrators are not a special bypass in v1; there is no global admin
  role.

### 3.1 Path traversal and normalization

- Decode percent-encoding once.
- Reject invalid percent sequences.
- Reject NUL, control characters and backslash.
- Reject `.` and `..` segments.
- Reject duplicate slashes in canonical paths, or normalize them before
  authorization and consistently thereafter.
- Store item hrefs as single segments.
- Never concatenate untrusted paths into filesystem paths without validating
  each segment.
- Use a directory-relative open strategy that does not follow symlinks
  (`openat`/`O_NOFOLLOW` where available).

## 4. XML security

- Disable DTDs and external entities.
- Reject documents with a `DOCTYPE`.
- Limit document size, depth and element count.
- Use a parser that does not expand entities.
- Validate namespace usage; reject unexpected namespaces where safe.
- Return `400` for malformed XML without echoing the body.
- Do not include stack traces in responses.

## 5. iCalendar and vCard security

- Enforce maximum body and item sizes before full parsing.
- Limit component count, property count, nesting depth and line length.
- Limit recurrence expansion in queries.
- Reject invalid UTF-8 and control characters.
- Do not evaluate anything from the content as code or templates.
- Treat all text as untrusted when rendering in the web UI.
- Protect against algorithmic complexity in recurrence rules and timezone
  handling.
- Reject or skip components that reference missing timezones rather than
  guessing.
- vCard photos and attachments are subject to size limits.

## 6. Subscription fetching and SSRF

External subscription fetching is the highest-risk feature because a user can
ask the server to make an outbound request.

### 6.1 URL policy

Allowed schemes:

- `https`
- `http` only when `subscriptions.allow_http = true`
- `webcal` is normalized to `https`

Rejected:

- `file`, `ftp`, `gopher`, `data`, `javascript` and all other schemes
- URLs with userinfo (except explicit credentials fields)
- Hosts containing spaces or control characters
- Extremely long URLs
- URLs with non-default ports unless explicitly allowed

### 6.2 SSRF checks

For every connection, including every redirect:

1. Resolve the hostname to all A and AAAA records.
2. Reject if any resolved address is:
   - loopback
   - private (RFC 1918)
   - link-local
   - unique-local IPv6
   - multicast
   - unspecified
   - broadcast
   - documentation or reserved ranges that should not be reachable
   - cloud metadata addresses, including `169.254.169.254` and IPv6
     equivalents
3. If private access is enabled by configuration, apply the configured allow
   rules explicitly.
4. Connect to the validated IP address, not by re-resolving the hostname, to
   prevent DNS rebinding.
5. Verify the TLS certificate for the original hostname, not the IP.
6. Apply the same checks after every redirect.
7. Do not forward credentials across origins. The default policy is to drop
   credentials on any redirect; same-origin redirects may keep them.

DNS resolution must use a resolver controlled by the server. If the platform
resolver is used, pin the resolved address for the connection and re-validate
on each new request.

### 6.3 HTTP client hardening

- No automatic decompression bombs: cap decompressed size as well as wire size.
- Maximum redirects: 5.
- Connect timeout: 10 seconds; total timeout: 60 seconds.
- Limit response header size.
- Accept only calendar content types or a body that begins with
  `BEGIN:VCALENDAR`.
- Do not follow redirects to disallowed schemes.
- Do not allow the remote server to set cookies that affect Oxicale.
- Use a dedicated User-Agent.
- Optionally use an outbound proxy; if used, document that the proxy can see
  credentials and content.
- Do not send Oxicale session cookies, Authorization headers or internal
  headers to remote hosts.
- Limit concurrency per host and globally.
- Apply exponential backoff on failures.

### 6.4 Content limits

- Maximum response size: 10 MiB by default.
- Maximum decompressed size: 50 MiB by default.
- Maximum components: 100,000.
- Maximum item size after parsing: 2 MiB.
- Maximum UID and property lengths.
- Reject calendars with excessive timezone definitions.

### 6.5 Failure behaviour

- Initial fetch failure: do not create the subscription.
- Refresh failure: keep last-good content, record an error, expose it to the
  owner, continue serving clients.
- Never partially apply a failed refresh.
- Do not automatically delete a subscription on remote 404/410. Mark it
  degraded and let the user decide.
- Do not leak remote response bodies in error messages shown to users; summarize
  the failure.

## 7. Feed token security

- 256 bits of entropy from a CSPRNG.
- Base64url encoding without padding.
- Store only a SHA-256 digest and a short hint.
- Constant-time comparison.
- Treat unknown, revoked and expired tokens identically.
- Token in the URL path, never a query string that might be logged more
  broadly.
- Do not log the token. Redact `/feeds/...` path segments in access logs,
  replacing the token with a hash prefix.
- Set `Referrer-Policy: no-referrer`.
- Rate limit by token hash and source address.
- Allow immediate revocation and rotation.
- Expiry is enforced server-side.
- Optional additional HTTP Basic auth can be required by configuration, but is
  off by default for compatibility.
- Warn users that a feed URL is a bearer secret.

## 8. Secrets at rest

- Subscription credentials encrypted with XChaCha20-Poly1305.
- 32-byte key from environment or a `0600` key file.
- Include a key identifier in metadata for future rotation.
- Never return secrets through the API.
- Logs redact authorization headers, cookies, feed tokens and remote
  credentials.
- Password hashes are one-way; feed tokens are hashes only.
- Configuration files containing secrets must be mode `0600`.

## 9. Web application security

- CSRF tokens on all state-changing requests.
- `Content-Security-Policy` with no inline scripts unless nonce-based.
- `X-Content-Type-Options: nosniff`.
- `X-Frame-Options: DENY` or CSP `frame-ancestors 'none'`.
- `Referrer-Policy: no-referrer` on sensitive pages.
- HSTS when TLS is terminated by Oxicale.
- Auto-escape all template variables.
- Validate `Origin`/`Referer` for state-changing requests as defence in depth.
- Do not expose stack traces.
- Rate limit login, subscription creation, feed creation and feed access.
- Use secure, random anti-CSRF tokens tied to the session.
- Session fixation prevention by rotating session IDs.

## 10. Availability and resource limits

- Maximum request body and item sizes.
- Maximum number of collections, items, subscriptions and feeds per user.
- Maximum concurrent connections per user and globally.
- Timeouts for all network and storage operations.
- Query complexity limits for reports and recurrence expansion.
- Background job concurrency limits.
- Disk space checks and clear `507 Insufficient Storage` errors.
- Graceful degradation when the index is corrupt or being rebuilt.
- Avoid unbounded in-memory buffering of large collections.

Denial-of-service risks from expensive calendar queries must be bounded. If a
query would exceed limits, return an error rather than consuming unlimited CPU
or memory.

## 11. Storage security

- Data directory mode `0700`, payload files `0600` by default.
- Refuse to run as root unless explicitly allowed.
- Do not follow symlinks when reading or writing item paths.
- Atomic writes and fsync for durability.
- Validate collection and item names before filesystem access.
- Backups must include the encryption key separately.
- Temporary files are created in a private directory and cleaned up.
- SQLite database files are protected by the same filesystem permissions.

## 12. Logging and monitoring

Log:

- Authentication success and failure (without secrets).
- Authorization denials with user and path.
- Subscription refresh failures with a category, not secrets.
- Feed token failures with a hashed token prefix.
- Storage errors and startup validation failures.
- Security-relevant configuration warnings.

Never log:

- Passwords or password hashes.
- Session identifiers or cookies.
- Authorization headers.
- Feed tokens or full feed URLs.
- Subscription credentials.
- Full request bodies by default.
- Remote response bodies that may contain private data.

## 13. Deployment hardening

- Use TLS. Built-in TLS or a reverse proxy with correct forwarded-header
  configuration.
- Run as a dedicated unprivileged user.
- Keep the data directory outside the web root.
- Restrict access to configuration and key files.
- Do not expose the metrics endpoint publicly.
- Keep the subscription private-network policy at its default deny.
- Use a firewall to limit outbound traffic if subscriptions are not needed.
- Back up regularly and test restore.
- Monitor disk space and failed authentication rates.
- Pin the container image or binary version and verify checksums.

## 14. Vulnerability handling

- Security issues should be reported privately to the maintainers.
- Do not disclose an issue publicly before a fix is available.
- The project should publish a security policy and supported versions before
  the first release.
- Dependencies must be audited (`cargo audit` or equivalent) in CI.
- Fuzz targets must cover XML, iCalendar, vCard, path parsing and URL parsing.

## 15. Security acceptance criteria

- Default configuration denies unauthenticated access.
- A subscription to a loopback, private or metadata address is rejected before
  any connection.
- A redirect to a private address is rejected.
- Feed tokens cannot be recovered from logs or the database.
- Revoked feed tokens fail immediately.
- Path traversal attempts do not read or write outside the data directory.
- Malformed XML with a DTD/external entity is rejected.
- Oversized requests are rejected with `413`.
- Rate limits apply to login and feed access.
- Secrets are absent from structured logs in tests.
