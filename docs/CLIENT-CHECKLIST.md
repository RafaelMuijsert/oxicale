# Core Client Compatibility Checklist

Status: Release gate for the core-client track.

This checklist is the manual companion to the automated contract in
`crates/oxicale/tests/compat.rs`. Run it against a release candidate with a
fresh data directory for each client.

## Scope

In scope:

- Discovery from the server URL and from the principal URL.
- Initial and incremental sync.
- Calendar and contact create, update and delete.
- Recurrence and time-range display.
- Read-only subscription discovery.
- Filtered feed consumption where the client supports subscriptions by URL.

Out of scope for this checklist:

- Sharing, groups, public shares and birthday calendars.
- LDAP/IMAP/OAuth2/PAM/Dovecot authentication.
- Nested collections and whole-collection uploads.
- WebDAV locking and CalDAV scheduling.
- Radicale plugin or configuration compatibility.

## Required clients

| Client | Platform | Calendar | Contacts | Subscriptions | Feeds |
| --- | --- | --- | --- | --- | --- |
| DAVx5 | Android | Required | Required | Required | Optional |
| Thunderbird | Desktop | Required | Via CardBook | Required | Optional |
| Apple Calendar / Contacts | macOS/iOS | Required | Required | Required | Optional |
| GNOME Calendar / Contacts | Linux | Required | Required | Required | N/A |
| Evolution | Linux | Required | Required | Required | Optional |
| KDE Kontact / Merkuro | Linux | Required | Required | Required | Optional |
| pimsync / vdirsyncer | CLI | Required | Required | Required | Required |

## Per-client procedure

For each client, record pass/fail and the observed HTTP status for each step.

1. Add the server using the external URL and username. Do not pre-create a
   calendar or address book.
2. Confirm discovery finds the default `Calendar` and `Contacts`.
3. Create an all-day event in the calendar. Confirm it appears on the server.
4. Create a recurring event. Change one occurrence and confirm the exception
   survives sync.
5. Create a task and a journal entry. Confirm both remain in the mixed
   calendar.
6. Create, edit and delete a contact.
7. Delete an event, wait for sync, and confirm it disappears from the server.
8. Restart the server. Confirm the client performs an incremental sync without
   data loss.
9. Create a read-only subscription. Confirm it appears, syncs, and cannot be
   edited or deleted from the client.
10. If supported, add a `VTODO` feed URL. Confirm it contains only tasks.

## Sign-off

| Client | Version | Discovery | Sync | Calendar CRUD | Contacts CRUD | Subscription | Result |
| --- | --- | --- | --- | --- | --- | --- | --- |
| DAVx5 | | | | | | | |
| Thunderbird | | | | | | | |
| Apple | | | | | | | |
| GNOME | | | | | | | |
| Evolution | | | | | | | |
| KDE | | | | | | | |
| pimsync/vdirsyncer | | | | | | | |

A release candidate is blocked if any required cell fails or has no recorded
result.
