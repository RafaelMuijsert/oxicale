# Oxicale Web Interface

Status: Draft 1

The web UI is a management surface, not a calendar editor. It exists to make
onboarding trivial, to manage collections and subscriptions, and to create
filtered feeds. Daily event and contact editing is done in CalDAV/CardDAV
clients.

## 1. Principles

1. **No setup wizard.** The first login lands on a usable dashboard.
2. **No calendar-type picker.** There is no "events vs tasks vs journal"
   choice anywhere.
3. **Explain less, do more.** Defaults are safe and useful. Advanced controls
   are available but collapsed.
4. **Copy-paste friendly.** Connection details and feed URLs are easy to copy.
5. **Progressive enhancement.** Core flows work with server-rendered HTML.
   JavaScript improves convenience without being required.
6. **No accidental exposure.** Feed tokens are shown once, clearly marked as
   secrets, and revocable.

## 2. Information architecture

```text
/                        → redirect to login or dashboard
/login                   → login
/dashboard               → collections overview
/calendar/{slug}         → calendar details and settings
/calendar/{slug}/feeds   → advanced component feeds
/addressbook/{slug}      → address book details
/subscribe               → add external calendar
/settings                → account and connection information
```

URLs may be served under `/.oxicale/` internally, with friendly paths
redirecting or rewritten. The DAV namespace (`/{user}/...`) is separate and must
never be shadowed.

## 3. Authentication screens

### 3.1 Login

Fields:

- Username
- Password
- "Sign in" button

Behaviour:

- On success, set a session cookie and redirect to `/dashboard`.
- On failure, show a generic error: "Invalid username or password."
- Rate-limit repeated failures. Do not reveal whether the username exists.
- If the deployment uses trusted-header authentication, the login form is
  replaced by a message explaining that sign-in is handled by the proxy, plus a
  "Continue" link.
- If authentication is disabled in a loopback-only development configuration,
  show a clear warning banner on every page.

### 3.2 First login

After successful authentication:

1. The server provisions the principal and default collections if missing.
2. The user is redirected to `/dashboard`.
3. No modal, wizard, tour or type selection is shown.
4. A dismissible "Connect a client" panel explains the server URL and username.

## 4. Dashboard

The dashboard is the home screen.

### 4.1 Layout

- Header: product name, username, settings link, logout.
- Primary card: default calendar.
- Section: "Subscribed calendars" with an "Add subscription" button.
- Section: "Address books" if CardDAV is enabled.
- Section: "Other calendars" for additional calendars created by the user.
- Footer: server version and link to documentation.

### 4.2 Calendar card

Shows:

- Colour swatch
- Display name
- Item counts by type: events, tasks, journal entries
- Collection URL, with a copy button
- Badges: "Subscribed" for subscriptions, "Read-only" when applicable
- Last refresh status for subscriptions
- Actions: Edit, Open advanced feeds, Unsubscribe/Delete

The card must not show a "type" dropdown or component checkboxes.

### 4.3 Empty states

- If no calendar exists, the server creates the default calendar and the
  dashboard shows it. There is no "create your first calendar" prompt.
- If no address book exists and CardDAV is enabled, show an "Add address book"
  action, not a blocking wizard.
- If no subscriptions exist, show a short explanation and the "Add
  subscription" button.

### 4.4 Connection panel

Content:

- Server URL, for example `https://calendar.example.com`
- Username
- A short note: "Use these details in your CalDAV/CardDAV client. Your calendar
  is already created."
- Optional collapsible instructions for common clients.

The panel can be dismissed and reopened from Settings.

## 5. Creating a calendar

### 5.1 Entry point

"Add calendar" in the dashboard or calendar section. This is optional; the
default calendar already exists.

### 5.2 Form

Fields:

- Name (required)
- Colour (optional, with a sensible random default)
- Description (optional)

There is no type field, no component selection, and no "Tasks"/"Journal" option.
The form includes a short note: "All Oxicale calendars contain events, tasks
and journal entries. You can create a filtered feed later if a client needs
only one kind."

### 5.3 Behaviour

- The URL slug is derived from the name, made unique, and shown in an
  "Advanced" section for manual editing.
- On success, return to the dashboard with the new calendar visible.
- The new collection is a mixed calendar.

## 6. Editing a calendar

Fields:

- Name
- Colour
- Description
- Collection URL (read-only, copyable)

Advanced section:

- "Filtered feeds" link
- "Export calendar" link
- "Delete calendar" action with confirmation

For subscriptions, show the remote source and refresh controls. Local metadata
overrides are allowed.

## 7. Address books

Address books are separate from calendars because contacts are a different
data type, not a component type.

- Default address book "Contacts" is created when CardDAV is enabled.
- "Add address book" asks only for name, colour and description.
- No type selector beyond calendar/address book, which is chosen by the entry
  point rather than a dropdown.

## 8. External subscriptions

### 8.1 Add subscription flow

Entry: "Add subscription" on the dashboard.

Fields:

- Calendar URL (required), accepting `https://`, optionally `http://`, and
  `webcal://`
- Display name (optional; defaults to remote name or host)
- Refresh interval (default: every hour; options: 15 min, 30 min, 1 hour,
  3 hours, 6 hours, 12 hours, 1 day, 1 week)
- Colour (optional)
- Credentials (collapsible, optional):
  - Authentication type: None, Basic, Bearer token
  - Username/password or token as appropriate

Helper text:

- "Oxicale downloads this calendar and shares it with all your clients as a
  read-only calendar."
- "The remote server must be reachable from your Oxicale server, not from your
  browser."

### 8.2 Validation and creation

- Validate URL syntax in the browser.
- On submit, the server performs a fetch and parse.
- Show a loading state: "Fetching and validating calendar..."
- On success, create the subscription and redirect to its details page with a
  success message.
- On failure, keep the form values and show an actionable error:
  - "The URL is not reachable from the server."
  - "The response is not a valid iCalendar file."
  - "The remote server returned 401. Check the credentials."
  - "The URL points to a private network address, which is blocked by default."
  - "The calendar is too large."
- Do not create a placeholder collection on initial failure.

### 8.3 Subscription details

Show:

- Display name and colour
- Remote URL
- Refresh interval
- Last attempt, last success and next refresh
- Status: Healthy, Degraded, Never fetched
- Error message when degraded
- Buttons: Refresh now, Edit, Unsubscribe
- Note: "This calendar is read-only. Changes must be made at the source."

If the subscription is degraded, show a persistent but non-blocking warning:
"Showing the last successfully fetched version. Last error: ..."

### 8.4 Edit subscription

Allowed changes:

- Display name, colour, description
- Refresh interval
- Credentials
- Remote URL (triggers a new fetch and reconciliation)

Changing the URL must be confirmed. On success, existing items are replaced only
after the new fetch succeeds. On failure, the old source and data remain.

### 8.5 Unsubscribe

Confirmation dialog:

- "Remove this subscription? The downloaded calendar will be deleted from
  Oxicale. Your clients will no longer see it."

Action deletes the collection. It does not affect the remote calendar.

## 9. Advanced: component feeds

### 9.1 Entry point

On a calendar details page, an "Advanced" disclosure contains "Filtered feeds".
This is intentionally not part of the main flow.

### 9.2 Feed list

Shows:

- Feed name
- Component badges: Events, Tasks, Journal
- Created date
- Expiry, if any
- Last used, if known
- Masked URL
- Actions: Copy URL, Rotate token, Revoke

Empty state: "No feeds. Create one if a client needs a calendar with only one
kind of item."

### 9.3 Create feed

Fields:

- Feed name (required)
- Components (at least one checkbox): Events (`VEVENT`), Tasks (`VTODO`),
  Journal (`VJOURNAL`)
- Expiry (optional): never, 7 days, 30 days, 90 days, 1 year, custom date

Warning:

> Anyone with this URL can read the selected items. Treat it like a password.
> You can revoke it at any time.

On submit:

1. The server creates the feed and returns a full URL.
2. Show the URL in a one-time panel with a copy button.
3. State clearly: "This URL is shown only once. Store it now."
4. Provide a "Done" action that returns to the feed list showing the masked URL.

### 9.4 Feed details

- Masked URL and last four characters of the token
- Component selection (read-only after creation; rotate or recreate to change)
- Usage and last-used timestamp
- Expiry
- Copy URL is disabled for the full token because it is no longer known; provide
  "Rotate token" to generate a new URL.
- Revoke action with confirmation: "Revoke this feed? Clients using it will stop
  receiving updates immediately."

### 9.5 Rotation

Rotating creates a new token, invalidates the old URL immediately, and shows the
new URL once. Feed ID, name and filter remain.

## 10. Settings

- Account: username, display name (if supported)
- Change password (local auth only)
- Connection details (server URL, principal URL)
- Session: logout, active sessions (future)
- Appearance: follow system / light / dark (optional)
- About: version, license, documentation link

## 11. Visual and interaction design

- Clean, low-chrome layout with a single accent colour.
- Colour swatches use accessible contrast.
- Destructive actions require confirmation and are visually distinct.
- Forms validate on the server; client-side validation is an enhancement.
- Loading states are explicit for network operations (subscription fetch).
- Errors appear next to the relevant field and in a summary at the top.
- Success messages are announced to screen readers.
- All interactive elements are keyboard accessible.
- Focus states are visible.
- No auto-playing animation beyond a simple spinner.
- Dark mode respects `prefers-color-scheme`.

## 12. Security requirements for the UI

- Session cookies: `HttpOnly`, `Secure` when TLS is used, `SameSite=Lax`.
- CSRF token required for every state-changing request.
- Login and subscription creation are rate limited.
- Feed tokens are never logged or placed in referrer-leaking links. Use a
  `Referrer-Policy: no-referrer` header on feed and UI pages.
- Copy buttons must not send data to third parties.
- No third-party scripts by default. If analytics are added, they must be
  opt-in and documented.
- Content Security Policy disallows inline scripts except with a nonce.
- HTML templates auto-escape user-controlled values.
- Remote calendar names and descriptions are untrusted and must be escaped.
- Password fields are never re-rendered with their value.
- The UI must not display remote credentials after saving.

## 13. Accessibility

- Target WCAG 2.1 AA.
- Semantic landmarks and headings.
- Labels for all form controls.
- Error messages linked with `aria-describedby`.
- Status changes use live regions where appropriate.
- Colour is never the only indicator of state.
- Touch targets at least 44x44 CSS pixels.
- Works at 200% zoom and narrow mobile widths.

## 14. Management API mapping

| UI action | API |
| --- | --- |
| Login | `POST /.oxicale/api/v1/session` |
| Logout | `DELETE /.oxicale/api/v1/session` |
| Dashboard | `GET /.oxicale/api/v1/collections` |
| Create calendar | `POST /.oxicale/api/v1/collections` with `kind: calendar` |
| Create address book | `POST /.oxicale/api/v1/collections` with `kind: addressbook` |
| Edit calendar | `PATCH /.oxicale/api/v1/collections/{id}` |
| Add subscription | `POST /.oxicale/api/v1/collections` with `kind: calendar` and subscription fields, or `POST /collections/{id}/subscription` |
| Refresh subscription | `POST /.oxicale/api/v1/collections/{id}/subscription/refresh` |
| Unsubscribe | `DELETE /.oxicale/api/v1/collections/{id}` |
| List feeds | `GET /.oxicale/api/v1/collections/{id}/feeds` |
| Create feed | `POST /.oxicale/api/v1/collections/{id}/feeds` |
| Rotate feed | `POST /.oxicale/api/v1/collections/{id}/feeds/{feed_id}/rotate` |
| Revoke feed | `DELETE /.oxicale/api/v1/collections/{id}/feeds/{feed_id}` |

The exact payloads are defined in [PROTOCOLS.md](PROTOCOLS.md#10-management-api).

## 15. Explicitly absent UI

The following must not appear in the first release:

- A calendar type selector (`Calendar`, `Tasks`, `Journal`, combinations)
- A "create a task list" button
- A per-calendar component-set editor
- A subscription wizard that asks the client to fetch the URL
- A feed builder that lets the user edit raw iCalendar
- A full event or contact editor
- A sharing or invitation screen

## 16. Copy guidelines

- Prefer "calendar" for the whole mixed collection, not "events calendar".
- Use "tasks" and "journal entries" only when describing component types.
- Use "subscription" for a remote calendar, not "webcal" or "feed".
- Use "filtered feed" or "feed" for the advanced single-type projection.
- Say "read-only" explicitly for subscriptions and feeds.
- Never imply that a feed is a separate copy of the data.
