# Oxicale Protocol Surface

Status: Draft 1

Oxicale exposes three protocol surfaces:

1. **DAV** for CalDAV, CardDAV and WebDAV clients.
2. **Feed HTTP** for read-only filtered ICS feeds.
3. **Management API** for the built-in web UI.

The DAV surface is the public interoperability contract. The management API is
internal to Oxicale's UI and may change between minor versions.

## 1. Standards

Oxicale targets the following documents:

- RFC 4918: WebDAV
- RFC 4791: CalDAV
- RFC 6352: CardDAV
- RFC 5545: iCalendar
- RFC 5546: iTIP subset for free-busy data generation only
- RFC 6350: vCard
- RFC 6638: CalDAV scheduling (not implemented in v1)
- RFC 6764: CalDAV/CardDAV discovery hints
- RFC 7232: HTTP conditional requests
- RFC 7233: HTTP range requests (not required)
- RFC 6578: WebDAV sync (`sync-collection`)
- CalendarServer `subscribed` extension for subscription discovery
- Apple iCalendar colour and calendar-order properties where useful

The server must not claim unsupported capabilities in `DAV:` or
`DAV:supported-report-set`.

## 2. Base path and discovery

### 2.1 Base path

The default base path is `/`. A deployment may mount Oxicale below a prefix,
for example `/dav`. All links in XML responses must include the configured
prefix.

### 2.2 Discovery flow

A well-behaved client performs:

1. `OPTIONS /` to inspect `DAV:` and `Allow`.
2. `PROPFIND /` with `Depth: 0` for `D:current-user-principal`.
3. `PROPFIND /{user}/` with `Depth: 0` for:
   - `C:calendar-home-set`
   - `CR:addressbook-home-set`
   - `D:principal-URL`
   - `C:calendar-user-address-set`
4. `PROPFIND /{user}/` with `Depth: 1` to list collections.
5. `PROPFIND` each collection for its capabilities and metadata.
6. `REPORT` for data.

Oxicale must not require a preliminary `PROPFIND /` when the client knows the
principal URL.

### 2.3 Principal and home paths

| Property | Value |
| --- | --- |
| Principal URL | `/{user}/` |
| Calendar home | `/{user}/` |
| Address book home | `/{user}/` |
| Default calendar | `/{user}/calendar/` |
| Default address book | `/{user}/contacts/` |

Collections are direct children of the principal. Oxicale does not use a
separate `calendars/` segment in URLs.

## 3. HTTP methods

### 3.1 `OPTIONS`

Must return:

```http
Allow: OPTIONS, GET, HEAD, PROPFIND, PROPPATCH, REPORT, PUT, DELETE, MKCOL, MKCALENDAR, MOVE
DAV: 1, 2, 3, calendar-access, addressbook, sync-collection, extended-mkcol
```

`2` may only be advertised if locking is implemented. If locking is not
implemented, advertise `1, 3, calendar-access, addressbook, sync-collection,
extended-mkcol`. This document assumes no WebDAV locking in v1.

Path-specific `Allow` headers must omit methods that are not valid for the
resource. For example, a feed URL allows only `GET`, `HEAD` and `OPTIONS`.

### 3.2 `PROPFIND`

Supported depths:

- `0`: the resource itself.
- `1`: the resource and its direct children.
- `infinity`: rejected with `403 Forbidden` for performance and safety.

Request forms:

- `D:propfind` containing `D:prop`
- `D:propfind` containing `D:allprop`
- `D:propfind` containing `D:propname`

Responses must be `207 Multi-Status`.

### 3.3 `PROPPATCH`

Used to update collection metadata such as display name, description and
colour. Content-changing properties are denied on read-only collections.

Oxicale must reject attempts to change `resourcetype` and the canonical
component set on existing collections.

### 3.4 `MKCOL`

Creates an address book or an empty generic collection. A `MKCOL` with a
calendar resourcetype must be treated like `MKCALENDAR`.

### 3.5 `MKCALENDAR`

Creates a calendar. Regardless of the requested
`C:supported-calendar-component-set`, the resulting calendar is mixed and
advertises all supported components.

If the client supplies unsupported component names, the server may return
`403 Forbidden` with a `C:supported-calendar-component` precondition, or may
ignore them. The default is to reject unknown component names but normalise
known subsets.

### 3.6 `GET` and `HEAD`

- `GET` on a collection returns an aggregate iCalendar or vCard document.
- `GET` on an item returns the item payload.
- `HEAD` returns headers only.
- Conditional requests via `If-None-Match` and `If-Modified-Since` are
  supported.
- `Accept` is advisory. The server may return the canonical content type with a
  `component` parameter for calendar items.

### 3.7 `PUT`

Creates or replaces an item.

- Request body must be a complete calendar object or vCard.
- A `PUT` to a collection path is rejected with `405 Method Not Allowed`.
- A `PUT` to a subscription item is rejected with `403 Forbidden`.
- `If-Match` and `If-None-Match` preconditions are enforced.
- Content-Type is validated. `text/calendar` and `text/vcard` are expected;
  compatible variants may be accepted.
- On success: `201 Created` for a new item, `204 No Content` for replacement.
- Response includes a strong `ETag`.

### 3.8 `DELETE`

- Deleting an item returns `204 No Content`.
- Deleting a collection returns `204 No Content` and removes its items,
  metadata, feeds and subscription state.
- Deleting a subscription collection unsubscribes it.
- `If-Match` is enforced when supplied.

### 3.9 `MOVE`

Moves an item or collection within the same principal.

- Cross-principal moves are rejected with `403 Forbidden`.
- Moving into or out of a subscription is rejected.
- Moving a collection preserves its UUID where possible and must update sync
  state.
- `Destination` must be a valid path under the same base path.

## 4. DAV properties

### 4.1 Live properties

| Property | Applies to | Notes |
| --- | --- | --- |
| `D:resourcetype` | all | Collection, calendar, addressbook, subscribed |
| `D:displayname` | collections/items | |
| `D:getetag` | items | Strong ETag |
| `D:getcontenttype` | items | Includes `component=` for calendar objects |
| `D:getcontentlength` | items | |
| `D:getlastmodified` | items/collections | HTTP date |
| `D:creationdate` | items/collections | ISO 8601 |
| `D:owner` | collections | Principal URL |
| `D:current-user-principal` | root | |
| `D:principal-URL` | principal | |
| `D:current-user-privilege-set` | collections | Read/write capabilities |
| `D:supported-report-set` | collections | Reports valid on resource |
| `D:sync-token` | collections | Current sync token |
| `D:quota-available-bytes` | optional | If quotas are enabled |
| `D:quota-used-bytes` | optional | If quotas are enabled |

### 4.2 CalDAV properties

| Property | Applies to | Notes |
| --- | --- | --- |
| `C:calendar-home-set` | principal | `/{user}/` |
| `C:calendar-user-address-set` | principal | Includes the principal URL |
| `C:supported-calendar-component-set` | calendar | Always all three |
| `C:calendar-description` | calendar | |
| `C:calendar-timezone` | calendar | Optional default timezone |
| `C:max-resource-size` | calendar | Configured item limit |
| `C:supported-calendar-data` | calendar | `text/calendar` version 2.0 |
| `C:calendar-data` | item in REPORT | Requested content |
| `CS:getctag` | calendar/addressbook | Changes on content change |
| `CS:subscribed` | subscription | Additional resourcetype marker |
| `CS:source` | subscription | Remote URL as a property, read-only |
| `I:calendar-color` | calendar | `#RRGGBB` or `#RRGGBBAA` |
| `I:calendar-order` | calendar | Optional |

### 4.3 CardDAV properties

| Property | Applies to | Notes |
| --- | --- | --- |
| `CR:addressbook-home-set` | principal | `/{user}/` |
| `CR:supported-address-data` | addressbook | vCard 3.0 and 4.0 |
| `CR:addressbook-description` | addressbook | |
| `CR:max-resource-size` | addressbook | Configured item limit |
| `C:addressbook-data` | item in REPORT | Requested content |
| `INF:addressbook-color` | addressbook | InfCloud compatibility |

### 4.4 Subscription properties

A subscription collection returns:

```xml
<D:resourcetype>
  <D:collection/>
  <C:calendar/>
  <CS:subscribed/>
</D:resourcetype>
```

It also returns `CS:source` with the normalised remote URL. It reports
`C:supported-calendar-component-set` with all supported components.

Rationale: clients that understand the subscribed marker can display a
read-only subscription; clients that do not may still treat it as a calendar.
Oxicale does not hide the collection from discovery.

## 5. CalDAV reports

### 5.1 `calendar-query`

Supported `C:filter` elements:

- `C:comp-filter` with `name="VCALENDAR"`
- Nested `C:comp-filter` for `VEVENT`, `VTODO`, `VJOURNAL`, `VALARM`
- `C:time-range`
- `C:prop-filter`
- `C:text-match`
- `C:is-not-defined`
- `C:param-filter`

Time-range semantics:

- `VEVENT`: use `DTSTART`/`DTEND`/`DURATION`, recurrence expansion.
- `VTODO`: use `DTSTART`, `DUE`, `COMPLETED`, `CREATED` as applicable.
- `VJOURNAL`: use `DTSTART` when present.
- Floating times are interpreted in the configured default timezone.
- All-day dates are treated as dates spanning the full day.
- Recurrence expansion is capped by configuration.

Requested `C:calendar-data` may include:

- `content-type`
- `version`
- `comp` selection with `name` and `allprop`/`prop`
- `expand` with `start` and `end`
- `limit-recurrence-set`

Oxicale must support plain data return and at least basic `expand`. Unsupported
`C:calendar-data` sub-elements must produce a clear error, not incorrect data.

### 5.2 `calendar-multiget`

- Request contains `D:href` elements.
- Only hrefs within the target collection are returned.
- Missing hrefs are reported with `404` in the multistatus response.
- Duplicate hrefs are ignored.

### 5.3 `free-busy-query`

- Accepts a `C:time-range`.
- Expands `VEVENT` recurrence.
- Honors `TRANSP` and `STATUS`.
- Returns `VFREEBUSY`.
- Requires read or free-busy permission.
- Enforces occurrence limits.
- Must not reveal event details in the response.

A separate URL form may be exposed for clients that expect a free-busy endpoint,
for example `/{user}/{collection}/?freebusy=1`. The canonical form is a
`REPORT` with `C:free-busy-query`.

### 5.4 `sync-collection`

Request:

```xml
<D:sync-collection xmlns:D="DAV:">
  <D:sync-token>...</D:sync-token>
  <D:sync-level>1</D:sync-level>
  <D:prop>
    <D:getetag/>
  </D:prop>
</D:sync-collection>
```

Rules:

- `sync-level` must be `1`.
- An empty token requests a full listing.
- The response includes a new `D:sync-token` and one `D:response` per changed
  href.
- Deleted hrefs are reported with `404` status.
- An invalid or too-old token returns `403 Forbidden` with
  `<D:error><D:valid-sync-token/></D:error>`, prompting a full resync.
- Changes are ordered by collection sequence.

## 6. CardDAV reports

### 6.1 `addressbook-query`

Supported filters:

- `CR:filter` with `test="anyof"` or `test="allof"`
- `CR:prop-filter` with `name`
- `CR:text-match` with `collation`, `match-type`, `negate-condition`
- `CR:param-filter`

Oxicale must support matching on common properties such as `FN`, `N`, `EMAIL`,
`TEL`, `ORG` and `UID`. Unsupported collations must fall back to a documented
default or return an error; they must not silently return wrong matches.

### 6.2 `addressbook-multiget`

Same rules as `calendar-multiget`, with vCard data.

## 7. Content validation

### 7.1 Calendar object rules

- Must contain one `VCALENDAR`.
- `VERSION` must be `2.0`.
- Must contain at least one `VEVENT`, `VTODO` or `VJOURNAL`.
- Every supported component must have a UID, except when the server is allowed
  to generate one.
- All components with the same UID in one resource must be a recurrence set.
- `RECURRENCE-ID` must be valid for the master component.
- DTSTART/DTEND/DUE types must be consistent.
- `VTIMEZONE` definitions must be valid and referenced TZIDs must exist or be
  resolvable.
- Line length, folding and encoding must be valid.
- Unsupported components (for example `VFREEBUSY` in storage) are rejected with
  `415 Unsupported Media Type` unless explicitly allowed.
- Recurrence rules must be bounded and expandable within the configured limit.

### 7.2 vCard rules

- Must contain one `VCARD`.
- Must have `VERSION` 3.0 or 4.0.
- Must have a `UID`.
- `FN` is required for vCard 3.0; vCard 4.0 requires `FN` or `N`.
- Binary values and photos are subject to the resource size limit.
- Duplicate UIDs in one address book are rejected with a UID conflict.

### 7.3 Size and complexity limits

Defaults, all configurable:

| Limit | Default |
| --- | --- |
| Request body | 10 MiB |
| Calendar item | 2 MiB |
| Contact item | 10 MiB |
| XML request body | 1 MiB |
| XML depth | 32 |
| Components per item | 1,000 |
| Recurrence occurrences per query | 10,000 |
| `PROPFIND Depth: 1` children | 10,000 |

## 8. Read-only collections

A subscription calendar is read-only for content. The following are denied with
`403 Forbidden`:

- `PUT` of a new or existing item
- `DELETE` of an item
- `MOVE` of an item into or out of the collection
- `PROPPATCH` of content-affecting properties

Allowed:

- `PROPPATCH` of `D:displayname`, `I:calendar-color`, `C:calendar-description`
  when the user has write-properties permission
- `DELETE` of the collection (unsubscribe)
- `REPORT` and `GET`
- `sync-collection`

The server exposes `D:current-user-privilege-set` accordingly. It must not
advertise `D:write-content`.

## 9. Feed HTTP surface

### 9.1 URL

```text
GET /feeds/{token}
HEAD /feeds/{token}
OPTIONS /feeds/{token}
```

`{token}` is a base64url string of at least 43 characters (256 bits). The URL
must not contain the username, collection slug or component filter.

Optional pretty URL for user convenience:

```text
GET /feeds/{token}/{name}.ics
```

The `{name}` segment is ignored for lookup and exists only to give the URL a
recognisable filename. The token remains the only secret.

### 9.2 Response

```http
HTTP/1.1 200 OK
Content-Type: text/calendar; charset=utf-8
Content-Disposition: inline; filename="tasks.ics"
Cache-Control: private, max-age=300, must-revalidate
ETag: "feed-<hash>"
Last-Modified: Thu, 22 Oct 2026 10:00:00 GMT
```

The body is a complete `VCALENDAR` with only the feed's selected components.

### 9.3 Conditional requests

- `If-None-Match` matching the current ETag returns `304 Not Modified` with no
  body.
- `If-Modified-Since` is supported when no ETag is supplied.
- ETags must change when the filtered content changes. Changes to excluded
  components may or may not change the ETag; the implementation should compute
  the ETag from included items only so that excluded changes do not cause
  needless client refreshes.

### 9.4 Access control

- The token is a bearer credential.
- Missing, invalid, revoked and expired tokens all return the same `404` body.
- Rate limiting is applied by token and source address.
- Feed responses must not include `Set-Cookie` or authentication challenges.
- CORS is disabled by default.
- `Referrer-Policy: no-referrer` is sent.

### 9.5 Errors

| Condition | Status |
| --- | --- |
| Unknown or revoked token | `404 Not Found` |
| Expired feed | `404 Not Found` |
| Rate limited | `429 Too Many Requests` |
| Storage unavailable | `503 Service Unavailable` |
| Method not allowed | `405 Method Not Allowed` |

## 10. Management API

Base path: `/.oxicale/api/v1/`.

Authentication: session cookie established by `POST /session`, or HTTP Basic
for scripted use when enabled. State-changing requests require a CSRF token.

### 10.1 Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/session` | Log in |
| `DELETE` | `/session` | Log out |
| `GET` | `/me` | Current principal and capabilities |
| `GET` | `/collections` | List collections |
| `POST` | `/collections` | Create a calendar or address book |
| `GET` | `/collections/{id}` | Collection details |
| `PATCH` | `/collections/{id}` | Update local metadata |
| `DELETE` | `/collections/{id}` | Delete collection |
| `POST` | `/collections/{id}/subscription` | Create or replace subscription settings |
| `POST` | `/collections/{id}/subscription/refresh` | Manual refresh |
| `DELETE` | `/collections/{id}/subscription` | Remove subscription, keep or delete items |
| `GET` | `/collections/{id}/feeds` | List feeds |
| `POST` | `/collections/{id}/feeds` | Create feed |
| `DELETE` | `/collections/{id}/feeds/{feed_id}` | Revoke feed |
| `POST` | `/collections/{id}/feeds/{feed_id}/rotate` | Rotate feed token |
| `GET` | `/collections/{id}/export` | Download merged collection |
| `POST` | `/collections/{id}/refresh` | Refresh local index or subscription |

### 10.2 Create calendar

```json
{
  "kind": "calendar",
  "slug": "family",
  "display_name": "Family",
  "description": null,
  "color": "#3b82f6"
}
```

The `kind` is `calendar` or `addressbook`. For calendars, component types are
not accepted. Any supplied `components` field is ignored or rejected with
`422 Unprocessable Entity`. The collection is always mixed.

### 10.3 Create subscription

```json
{
  "url": "webcal://example.test/team.ics",
  "display_name": "Team",
  "refresh_interval_seconds": 3600,
  "color": "#10b981",
  "auth": {
    "kind": "basic",
    "username": "alice",
    "password": "..."
  }
}
```

Response includes the collection and its subscription status. On validation or
fetch failure, the server returns a structured error and does not create the
collection.

### 10.4 Create feed

```json
{
  "name": "Tasks only",
  "components": ["VTODO"],
  "expires_at": null
}
```

Response includes the full URL once:

```json
{
  "id": "...",
  "name": "Tasks only",
  "components": ["VTODO"],
  "url": "https://oxicale.example/feeds/3Xy.../tasks.ics",
  "token_hint": "x7Qp",
  "created_at": "...",
  "expires_at": null
}
```

Later list responses return `token_hint` and a masked URL, never the full token.

## 11. Error model

### 11.1 HTTP statuses

| Status | Typical cause |
| --- | --- |
| `400 Bad Request` | Malformed XML, bad path, invalid conditional header |
| `401 Unauthorized` | Missing or invalid credentials |
| `403 Forbidden` | Authenticated but not allowed; read-only collection |
| `404 Not Found` | Unknown collection, item, feed or token |
| `405 Method Not Allowed` | Method not valid for resource |
| `409 Conflict` | UID conflict, collection exists, sync conflict |
| `412 Precondition Failed` | `If-Match` failure |
| `413 Payload Too Large` | Body or item limit exceeded |
| `415 Unsupported Media Type` | Wrong content type or unsupported component |
| `422 Unprocessable Entity` | Semantically invalid management API request |
| `423 Locked` | Reserved; not used without WebDAV locking |
| `429 Too Many Requests` | Rate limit |
| `500 Internal Server Error` | Unexpected server failure |
| `503 Service Unavailable` | Storage or dependency unavailable |

### 11.2 WebDAV precondition bodies

On failure, return a `D:error` body with the relevant precondition, for example:

```xml
<D:error xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <C:valid-calendar-data/>
</D:error>
```

Common preconditions:

- `D:valid-resourcetype`
- `D:resource-must-be-null`
- `D:valid-sync-token`
- `C:valid-calendar-data`
- `C:valid-calendar-object-resource`
- `C:supported-calendar-data`
- `C:no-uid-conflict`
- `C:calendar-collection-location-ok`
- `CR:valid-address-data`
- `CR:no-uid-conflict`

### 11.3 Authentication

`401` responses include:

```http
WWW-Authenticate: Basic realm="Oxicale", charset="UTF-8"
```

The realm is configurable. Failed authentication must not distinguish between
unknown users and wrong passwords.

## 12. Caching and headers

- DAV responses must include `Cache-Control: no-cache` or an appropriate
  private cache policy.
- Feed responses may be cached for a short configured time and must revalidate.
- `ETag` and `Last-Modified` must be correct or omitted; never fabricate.
- Security headers for the web UI: `Content-Security-Policy`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy`, and HSTS when TLS is
  terminated by Oxicale.
- CORS is disabled unless explicitly configured.

## 13. Compatibility notes

- Mixed calendars advertise all components even if a client requested fewer.
  See [COMPATIBILITY.md](COMPATIBILITY.md).
- Subscription collections advertise both `C:calendar` and `CS:subscribed`.
- Component feeds are plain ICS endpoints, not CalDAV collections. Clients that
  only support URL subscriptions can use them directly.
- Oxicale does not implement WebDAV locks and must not advertise class 2.
- Oxicale does not implement CalDAV scheduling and must not advertise
  `calendar-auto-schedule`.
