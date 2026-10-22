# Oxicale Configuration

Status: Draft 1

Oxicale uses TOML. Configuration is validated at startup. Invalid keys,
unknown sections and invalid values abort startup with a clear error unless
explicitly marked as forward-compatible.

## 1. Loading order

Priority, highest first:

1. Command-line flags
2. Environment variables
3. The file passed with `--config`
4. `$OXICALE_CONFIG` if set
5. `./oxicale.toml`
6. `/etc/oxicale/oxicale.toml`

All provided files are merged in lower-to-higher priority order. A file that
does not exist is an error unless its path is prefixed with `?` in a compound
path list.

Environment variables use the form:

```text
OXICALE__SECTION__KEY=value
```

Double underscores separate nesting. Arrays use comma-separated values.
Booleans accept `true`/`false` and `1`/`0`. Durations accept `30s`, `15m`,
`1h`, `7d`.

## 2. Full example

```toml
[server]
bind = ["127.0.0.1:5232"]
base_path = "/"
external_url = "https://calendar.example.com"
behind_proxy = false
trusted_proxies = []
max_connections = 100
request_timeout = "30s"
allow_insecure_remote = false
max_request_body = "10MiB"
max_item_size = "2MiB"
max_addressbook_item_size = "10MiB"
tls = false
tls_certificate = ""
tls_key = ""

[storage]
data_dir = "/var/lib/oxicale"
secret_key_file = ""            # empty: {data_dir}/.oxicale.key (auto-generated)
fsync = true
index_path = ""                 # empty: {data_dir}/index.sqlite3
lock_timeout = "10s"
sync_history_days = 90
sync_history_max_changes = 100000

[auth]
backend = "local"               # local | trusted_header | none | deny
none_user = "anonymous"         # principal used by auth.backend = "none"
realm = "Oxicale"
password_file = "/etc/oxicale/users"
session_lifetime = "12h"
session_cookie_name = "oxicale_session"
secure_cookies = true
rate_limit_failures = 10
rate_limit_window = "5m"

[auth.trusted_header]
header = "X-Remote-User"
require_proxy = true

[rights]
backend = "owner_only"          # owner_only | authenticated | from_file
file = "/etc/oxicale/rights"

[calendar]
default_name = "Calendar"
default_slug = "calendar"
create_default = true
allow_client_creation = true
default_timezone = "UTC"

[addressbook]
enabled = true
default_name = "Contacts"
default_slug = "contacts"
create_default = true

[subscriptions]
enabled = true
allow_http = false
allow_private_networks = false
allow_loopback = false
allowed_hosts = []              # empty: all public hosts allowed
denied_hosts = []
redirect_limit = 5
connect_timeout = "10s"
request_timeout = "60s"
max_response_size = "10MiB"
max_components = 100000
max_item_size = "2MiB"
default_interval = "1h"
min_interval = "15m"
max_interval = "7d"
max_per_user = 50
user_agent = "Oxicale/0.1"
skip_broken_components = false
outbound_proxy = ""             # empty: direct connection

[feeds]
enabled = true
default_cache_control = "private, max-age=300, must-revalidate"
rate_limit_per_minute = 120
max_per_calendar = 20
default_expiry = ""             # empty: never
allow_http_auth = false

[web]
enabled = true
theme = "auto"                  # auto | light | dark
allow_basic_api_auth = true
csp_extra = ""
require_https = false

[logging]
level = "info"                  # trace | debug | info | notice | warning | error
format = "text"                 # text | json
target = "stderr"               # stderr | stdout | file
file = ""
redact_headers = ["Authorization", "Cookie", "Set-Cookie"]
log_request_bodies = false

[metrics]
enabled = false
bind = "127.0.0.1:9090"
path = "/metrics"

[limits]
max_collections_per_user = 100
max_items_per_collection = 100000
max_principals = 10000
max_connections_per_user = 20
```

## 3. Sections

### 3.1 `[server]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `bind` | array of strings | `["127.0.0.1:5232"]` | Listen addresses |
| `base_path` | string | `/` | URL prefix |
| `external_url` | string | derived | Public base URL for generated feed links |
| `behind_proxy` | bool | `false` | Trust forwarded scheme/host from `trusted_proxies` |
| `trusted_proxies` | array | `[]` | CIDR ranges allowed to set forwarded headers |
| `max_connections` | integer | `100` | Concurrent connections |
| `request_timeout` | duration | `30s` | Per-request timeout |
| `allow_insecure_remote` | bool | `false` | Permit no-auth mode beyond loopback |
| `max_request_body` | size | `10MiB` | Global body limit |
| `max_item_size` | size | `2MiB` | Calendar object limit |
| `max_addressbook_item_size` | size | `10MiB` | Contact limit |
| `tls` | bool | `false` | Enable built-in TLS |
| `tls_certificate` | path | | PEM certificate chain |
| `tls_key` | path | | PEM private key |

Warnings:

- Binding to a non-loopback address with `auth.backend = "none"` produces a
  prominent warning and is refused by default unless
  `server.allow_insecure_remote = true` is set.
- `behind_proxy` without `trusted_proxies` is an error.
- `external_url` is required when `behind_proxy = false` and feeds are enabled
  behind a non-trivial setup, so that generated feed URLs are correct.

### 3.2 `[storage]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `data_dir` | path | `/var/lib/oxicale` | Canonical data root |
| `secret_key_file` | path | `{data_dir}/.oxicale.key` | Encryption key for subscription credentials |
| `fsync` | bool | `true` | Sync payloads and directories |
| `index_path` | path | `{data_dir}/index.sqlite3` | Derived index |
| `lock_timeout` | duration | `10s` | Wait for the data-directory lock |
| `sync_history_days` | integer | `90` | Change retention |
| `sync_history_max_changes` | integer | `100000` | Change retention cap |

`data_dir` must not be shared between running Oxicale processes. It must be on a
local filesystem; network filesystems are unsupported.

### 3.3 `[auth]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `backend` | enum | `local` | `local`, `trusted_header`, `none`, `deny` |
| `none_user` | string | `anonymous` | Principal used when backend is `none` |
| `realm` | string | `Oxicale` | HTTP Basic realm |
| `password_file` | path | `/etc/oxicale/users` | Local users file |
| `session_lifetime` | duration | `12h` | Web session lifetime |
| `session_cookie_name` | string | `oxicale_session` | Cookie name |
| `secure_cookies` | bool | `true` | Require HTTPS for cookies |
| `rate_limit_failures` | integer | `10` | Failures per window |
| `rate_limit_window` | duration | `5m` | Failure window |

Password file format:

```text
alice:$argon2id$v=19$m=65536,t=3,p=4$...$...
bob:$argon2id$v=19$m=65536,t=3,p=4$...$...
```

The file must be readable only by the Oxicale service user. A helper command
creates hashes:

```bash
oxicale user add alice --password-file /etc/oxicale/users
oxicale user passwd alice --password-file /etc/oxicale/users
```

`trusted_header` requires `require_proxy = true` and a non-empty
`server.trusted_proxies`. The header value is the username, validated with the
same rules as local usernames.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `header` | string | `X-Remote-User` | Header containing the username |
| `require_proxy` | bool | `true` | Refuse header auth unless the peer is trusted |

`none` is for development only. Every request is mapped to the principal named
by `none_user`, which is provisioned automatically. Binding beyond loopback with
this backend requires `server.allow_insecure_remote = true` and produces a
startup warning.

`deny` rejects all authentication and is a safe default when the server is
misconfigured.

### 3.4 `[rights]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `backend` | enum | `owner_only` | `owner_only`, `owner_write`, `authenticated`, `from_file` |
| `file` | path | `/etc/oxicale/rights` | Rules file for `from_file` |

`owner_only`: a user can read and write `/{user}/...` only.

`authenticated`: any authenticated user can read/write everything. Not
recommended except for trusted single-team deployments.

`from_file`: reserved for a future release. The current release refuses to
start when this backend is selected rather than silently applying a different
policy.

### 3.5 `[calendar]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `default_name` | string | `Calendar` | Display name of the default calendar |
| `default_slug` | string | `calendar` | URL slug |
| `create_default` | bool | `true` | Provision automatically |
| `allow_client_creation` | bool | `true` | Allow CalDAV `MKCALENDAR` |
| `default_timezone` | string | `UTC` | IANA timezone for floating and all-day times |

There is no option to create task-only or event-only calendars. This is
intentional.

### 3.6 `[addressbook]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | Enable CardDAV |
| `default_name` | string | `Contacts` | Display name |
| `default_slug` | string | `contacts` | URL slug |
| `create_default` | bool | `true` | Provision automatically |

When `enabled = false`, CardDAV reports and properties are not advertised.

### 3.7 `[subscriptions]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | Enable external subscriptions |
| `allow_http` | bool | `false` | Allow plain HTTP URLs |
| `allow_private_networks` | bool | `false` | Allow RFC 1918, CGNAT, etc. |
| `allow_loopback` | bool | `false` | Allow 127.0.0.0/8 and ::1 |
| `allowed_hosts` | array | `[]` | If non-empty, only these hosts |
| `denied_hosts` | array | `[]` | Always denied hosts |
| `redirect_limit` | integer | `5` | Maximum redirects |
| `connect_timeout` | duration | `10s` | TCP/TLS connect timeout |
| `request_timeout` | duration | `60s` | Total fetch timeout |
| `max_response_size` | size | `10MiB` | Response body limit |
| `max_components` | integer | `100000` | Component count limit |
| `max_item_size` | size | `2MiB` | Parsed item limit |
| `default_interval` | duration | `1h` | Default refresh interval |
| `min_interval` | duration | `15m` | Minimum refresh interval |
| `max_interval` | duration | `7d` | Maximum refresh interval |
| `max_per_user` | integer | `50` | Subscription count limit |
| `user_agent` | string | `Oxicale/0.1` | Outbound User-Agent |
| `skip_broken_components` | bool | `false` | Skip invalid components instead of rejecting the refresh |
| `outbound_proxy` | string | empty | Optional HTTP(S) proxy for outbound fetches |

Host matching rules:

- Exact hostnames, `*.example.com` wildcards and IP literals are supported.
- Denied hosts always win over allowed hosts.
- `allow_private_networks` and `allow_loopback` require an explicit
  acknowledgement in the configuration file; they are never enabled by an
  environment variable alone when the value would expose a network.

SSRF checks are defined in [SECURITY.md](SECURITY.md).

### 3.8 `[feeds]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | Enable component feeds |
| `default_cache_control` | string | `private, max-age=300, must-revalidate` | Response cache policy |
| `rate_limit_per_minute` | integer | `120` | Per-token/IP request limit |
| `max_per_calendar` | integer | `20` | Feed count limit |
| `default_expiry` | duration | disabled | Default expiry for new feeds |
| `allow_http_auth` | bool | `false` | Require additional Basic auth |

### 3.9 `[web]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `enabled` | bool | `true` | Serve the web UI |
| `theme` | enum | `auto` | Default theme |
| `allow_basic_api_auth` | bool | `true` | Allow Basic auth on the management API |
| `csp_extra` | string | empty | Additional CSP directives |
| `require_https` | bool | `false` | Redirect HTTP to HTTPS |

### 3.10 `[logging]`

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `level` | enum | `info` | Log level |
| `format` | enum | `text` | `text` or `json` |
| `target` | enum | `stderr` | `stderr`, `stdout` or `file` |
| `file` | path | empty | Required when target is `file` |
| `redact_headers` | array | Authorization, Cookie, Set-Cookie | Header names to redact |
| `log_request_bodies` | bool | `false` | Never enable in production |

### 3.11 `[metrics]`

Disabled by default. When enabled, expose Prometheus metrics on a separate bind
address. Do not expose the metrics endpoint publicly without protection.

### 3.12 `[limits]`

Coarse safety limits. Per-collection and per-user limits are enforced after
authentication, so they do not leak information.

## 4. CLI

```text
oxicale run [--config PATH]
oxicale check-config [--config PATH]
oxicale verify-storage [--data-dir PATH]
oxicale rebuild-index [--data-dir PATH]
oxicale user add USER
oxicale user passwd USER
oxicale migrate radicale --from PATH --to PATH [--dry-run]
oxicale export --data-dir PATH --output PATH
```

Flags override configuration:

```text
--server-bind 0.0.0.0:5232
--storage-data-dir /srv/oxicale
--auth-backend trusted_header
--log-level debug
```

`check-config` prints the effective configuration with secrets redacted and
exits non-zero on errors.

## 5. Radicale migration

There is no direct INI compatibility mode. The following table maps common
Radicale options to Oxicale.

| Radicale | Oxicale |
| --- | --- |
| `[server] hosts` | `server.bind` |
| `[server] ssl` / `certificate` / `key` | `server.tls` / `tls_certificate` / `tls_key` |
| `[server] max_connections` | `server.max_connections` |
| `[server] max_content_length` | `server.max_request_body` |
| `[server] max_resource_size` | `server.max_item_size` |
| `[server] timeout` | `server.request_timeout` |
| `[auth] type = htpasswd` | `auth.backend = local` (convert hashes or reset passwords) |
| `[auth] type = remote_user` | `auth.backend = trusted_header` |
| `[auth] type = none` / `denyall` | `auth.backend = none` / `deny` |
| `[rights] type = owner_only` | `rights.backend = owner_only` |
| `[rights] type = authenticated` | `rights.backend = authenticated` |
| `[rights] type = from_file` | `rights.backend = from_file` (rules must be rewritten) |
| `[storage] filesystem_folder` | `storage.data_dir` |
| `[storage] max_sync_token_age` | `storage.sync_history_days` |
| `[storage] hook` | Not supported; use an external watcher or future hook plugin |
| `[storage] predefined_collections` | Not needed; defaults are automatic |
| `[web] type = internal` | `web.enabled = true` |
| `[web] type = none` | `web.enabled = false` |
| `[logging] level` | `logging.level` |

Radicale authentication hashes other than Argon2id may need conversion. The
import tool can read bcrypt, SHA-256, SHA-512, MD5 and plaintext htpasswd
entries and rewrite them as Argon2id, or the admin can reset passwords.

## 6. Validation and safety

Startup validation must check:

- Data directory exists or can be created, and is writable.
- The data directory is not shared with another running instance (lock).
- TLS files are readable and form a valid key pair.
- `base_path` is a clean absolute path with no `..`.
- `external_url` uses `https` unless `web.require_https = false` and the
  deployment is explicitly local.
- Private-network subscription access is not enabled without a warning.
- `auth.backend = none` is not exposed beyond loopback without an explicit
  override.
- Feed token generation has access to secure randomness.
- The subscription credential encryption key is present if encrypted
  credentials exist.

The server must refuse to start on invalid security-critical configuration. It
may start with warnings for risky but intentional settings.

## 7. Encryption key

Subscription credentials are encrypted with a server key. Key sources, in order:

1. `OXICALE_SECRET_KEY` environment variable (base64, 32 bytes).
2. `storage.secret_key_file` containing a base64 key with mode `0600`.
3. An auto-generated key file in the data directory on first use, with a
   warning that it must be backed up.

The key file is separate from the database. Losing it makes encrypted
subscription credentials unrecoverable. The server must detect a key mismatch
and mark affected subscriptions as needing credentials, without deleting
content.

Key rotation is future work. The metadata includes a `key_id` to support it.
