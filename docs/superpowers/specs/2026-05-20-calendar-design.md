# `pidge calendar` — design

## Goal

Give the user a full calendar surface in pidge: create, read, update,
delete events; send and cancel invitations; reschedule; duplicate;
move between calendars; create recurring events. Like the rest of
pidge, the primary operator is an AI coding agent on the user's
behalf — humans can run it directly, but the surface is shaped for
flag-driven agent invocation.

The driver is conversational scheduling: "schedule a meeting with X
next Tuesday at 3", "what's on my calendar tomorrow", "move my 2pm to
4pm", "cancel the sales sync". With this surface in place, an AI agent
configured with the pidge skill can fulfill all of those without
leaving the terminal.

## Scope decisions (settled during brainstorming)

| Question | Decision |
|---|---|
| How does the AI resolve "John" → e-mail? | **It doesn't go through pidge.** Calendar commands accept e-mail addresses only. The AI uses its own context. A separate contacts surface is a follow-up if it earns its keep. |
| Recurring patterns | **Simple presets.** `daily`, `weekly`, `monthly`, `yearly` with optional `--on` (weekdays for weekly), `--until <date>`, `--count <n>`. Covers ~95% of human-scheduled events; maps cleanly to Graph. |
| Conflict detection | **Not pidge's job.** `calendar new` / `move-time` just write. If the AI wants to check conflicts, it calls `calendar list --json` over the target window and decides. |
| Time zones | **Local everywhere.** Display in the system's local TZ; inputs accept ISO with TZ, local ISO, date-only, time-only ("today, local"), and relative forms (`tomorrow 3pm`, `+2h`). `--tz <iana>` overrides for input and display. |
| `calendar list` default window | **Today + next 7 days.** Explicit flags (`--today`, `--tomorrow`, `--week`, `--month`, `--from`/`--to`) override. |
| Multi-calendar | **Default calendar implicit, `--calendar` overrides.** `calendar calendars` enumerates. `calendar move <hash> --to <name-or-id>` moves between. |
| Namespace | **`pidge calendar`** — no `cal` alias in v1 to avoid two-ways-to-do-one-thing. |

## Surface

```
pidge calendar                                       # alias for `calendar list`
pidge calendar list [--account A]
              [--from DATE] [--to DATE]
              [--today | --tomorrow | --week | --month]
              [--calendar NAME-OR-ID]
              [-n N] [-c|--compact] [-t|--table]
pidge calendar show <hash>
pidge calendar search <query> [--account A] [-n N]
                              [--calendar NAME-OR-ID]
pidge calendar new --title "..." --start "..." [--end "..."]
              [--all-day] [--location "..."]
              [--body "..." | --body-file -]
              [--invite a@x,b@x] [--invite-optional c@x]
              [--repeat daily|weekly|monthly|yearly]
              [--on mon,wed,fri] [--until DATE | --count N] [--interval N]
              [--online]                              # add a Teams meeting
              [--calendar NAME-OR-ID]
              [--from ACCOUNT] [--tz IANA]
              [--confirm] [-y]
pidge calendar edit <hash>                            # TUI prefilled
              [--title|--start|--end|--location|...]  # same flags as `new`
              [--notify | --no-notify]                # for events with attendees
              [--series]                              # apply to whole recurring series
pidge calendar move-time <hash> --start "..." [--end "..."]
              [--notify | --no-notify] [--series]
pidge calendar duplicate <hash> [--start "..."]
              [--title "..."] [--calendar NAME-OR-ID]
pidge calendar delete <hash> [-y] [--series]
pidge calendar cancel <hash> [--comment "..."] [-y] [--series]
pidge calendar move <hash> --to <calendar-name-or-id>
pidge calendar rsvp <hash> --accept|--tentative|--decline
              [--comment "..."] [--no-notify]
pidge calendar calendars [list]
```

Conventions reused verbatim from mail:

- Default subcommand: `pidge calendar` ⇒ `pidge calendar list`.
- Single-fragment commands accept `pidge calendar <fragment>` as shorthand
  for `calendar show <fragment>` via the same arg-preprocessor used for
  mail (extended for `CALENDAR_SUBCOMMAND_NAMES`).
- `--json` for machine output; `--account` to filter; `-y` to skip
  confirmation prompts; `--quiet` to suppress non-essential text.

### Time inputs

A shared `parse_when` helper accepts:

- ISO with TZ: `2026-05-22T15:00+02:00`, `2026-05-22T13:00Z`
- Local ISO: `2026-05-22T15:00` (interpreted in `--tz` or system local)
- Date only: `2026-05-22` (with `--all-day`, or `T00:00` local without)
- Time only: `15:00` (today, local; if already past, tomorrow)
- Weekday: `mon 09:00`, `next tue 14:00`, `tomorrow 15:00`
- Relative offsets (only on `--end`, relative to `--start`):
  `+2h`, `+30m`, `+1d`

DST boundary days: when `+2h` would cross a DST transition, we add wall
hours, not absolute hours; this matches user intuition ("the same time
on the next day"). For ISO + offset inputs, we use the input's UTC
exactly.

## Data model

New module `crates/pidge-core/src/event.rs`:

```rust
pub struct Event {
    pub account: String,
    pub calendar_id: String,
    pub id: String,                              // Graph event ID
    pub subject: String,
    pub start: EventTime,
    pub end: EventTime,
    pub all_day: bool,
    pub location: Option<String>,
    pub organizer: Attendee,
    pub attendees: Vec<Attendee>,
    pub body_preview: String,
    pub body_content: String,
    pub body_content_type: BodyContentType,      // reused from message.rs
    pub recurrence: Option<RecurrencePattern>,
    pub is_organizer: bool,                      // drives `cancel`/`edit`
    pub response_status: ResponseStatus,         // viewer's own RSVP
    pub online_meeting_url: Option<String>,
    pub series_master_id: Option<String>,        // set on recurrence instances
}

pub struct EventTime {
    pub at: chrono::DateTime<chrono::Utc>,       // canonical UTC
    pub tz: String,                              // IANA, e.g. "Europe/Stockholm"
}

pub struct Attendee {
    pub name: String,
    pub address: String,
    pub kind: AttendeeKind,                      // Required, Optional, Resource
    pub response: ResponseStatus,
}

pub enum ResponseStatus {
    None, Organizer, Accepted, Tentative, Declined, NotResponded,
}

pub struct RecurrencePattern {
    pub freq: RecurrenceFreq,                    // Daily, Weekly, Monthly, Yearly
    pub interval: u32,                           // 1 = every freq, 2 = every other, …
    pub by_weekday: Vec<Weekday>,                // for Weekly only
    pub range: RecurrenceRange,
}

pub enum RecurrenceRange { EndDate(NaiveDate), Count(u32), NoEnd }

pub struct Calendar {
    pub account: String,
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub color: Option<String>,
    pub can_edit: bool,
}
```

`EventTime` keeps both UTC and the originating IANA TZ so display can
show "10:00 Stockholm" even when the laptop is in Sydney. The short
hash (8-char) is computed over `account + event_id` so it's stable
per-event-per-account. Recurring instances get distinct hashes because
Graph returns distinct IDs for each occurrence (via `calendarView`).

`pidge-core/src/cache.rs` gets an `events:` namespace keyed by hash →
`{account, calendar_id, event_id}` with a TTL. Like mail, the cache is
populated on every `list` / `search` and consulted by every command
that takes a `<hash>`.

## Graph mapping

| pidge command | Graph endpoint |
|---|---|
| `calendar list` | `GET /me/calendarView?startDateTime=..&endDateTime=..` — expands recurrence instances within the window. |
| `calendar list --calendar X` | Same under `/me/calendars/{id}/calendarView`. |
| `calendar show <hash>` | `GET /me/events/{id}?$expand=attendees`. |
| `calendar search <q>` | `GET /me/events?$search="<q>"`. Returns relevance-ranked results, not date-ordered. |
| `calendar new` | `POST /me/calendar/events` (or `/me/calendars/{id}/events`). Attendees + recurrence in same payload — Graph auto-sends invitations when attendees are present. |
| `calendar edit` | `PATCH /me/events/{id}`. Use `sendUpdates` semantics: when `--no-notify`, set `responseRequested: false` and omit attendee changes that would trigger notification. |
| `calendar move-time` | `PATCH /me/events/{id}` with `start` + `end` only. |
| `calendar duplicate` | `GET /me/events/{id}` → strip ID + occurrence-specific fields → `POST /me/calendar/events`. Drops attendee response statuses (new event = fresh invites). |
| `calendar delete` | `DELETE /me/events/{id}`. No attendee notification. |
| `calendar cancel` | `POST /me/events/{id}/cancel` with `Comment`. Only valid for organizers. |
| `calendar move <hash> --to <cal>` | `PATCH /me/events/{id}` with `calendar@odata.bind` set to the destination calendar's URL. |
| `calendar rsvp` | `POST /me/events/{id}/accept` | `/tentativelyAccept` | `/decline` with `comment` and `sendResponse: bool`. |
| `calendar calendars` | `GET /me/calendars`. |

Auth: add `Calendars.ReadWrite` to the scope list. No new auth flow —
the existing device-code path picks up the new scope on next sign-in;
existing tokens get a fresh refresh on first calendar command.

Headers: every events request sends `Prefer: outlook.timezone="UTC"`
so Graph returns timestamps in UTC regardless of the mailbox's
preferred TZ — we own the display formatting, so a stable internal
zone is simpler than juggling Graph's per-account preference.

## Code layout

```
crates/pidge-core/src/
  event.rs                       (new) — types listed above
  cache.rs                       (extend) — events table
  lib.rs                         (re-export)

crates/pidge-client/src/
  graph/
    events.rs                    (new) — list/get/create/update/delete/cancel/move/rsvp/search
    calendars.rs                 (new) — list_calendars, calendar lookup by name
    mod.rs                       (extend)
  auth/                          (extend scopes list)

crates/pidge/src/
  cli.rs                         (extend) — Calendar { command: CalendarCommands }
                                            CalendarCommands enum
                                            CALENDAR_SUBCOMMAND_NAMES
  commands/
    mod.rs                       (register new modules)
    calendar.rs                  (new) — dispatch
    calendar_list.rs
    calendar_show.rs
    calendar_search.rs
    calendar_new.rs
    calendar_edit.rs
    calendar_move_time.rs
    calendar_duplicate.rs
    calendar_delete.rs
    calendar_cancel.rs
    calendar_move.rs
    calendar_rsvp.rs
    calendar_calendars.rs
    calendar_fragment.rs         (new) — short-hash resolver
    calendar_compose_form.rs     (new) — TUI wizard for new/edit
    time.rs                      (new) — parse_when, format_when, DST tests
  main.rs                        (extend arg-preprocessor for calendar)
```

Module-level rules carried over from mail:

- `pidge-core` stays HTTP-free; new types are plain data.
- `pidge-client` knows nothing about clap or terminal output.
- `time.rs` is the only place that parses human time strings; every
  calendar command consumes it.

## Recurrence: occurrence vs. series

When a command targets a `<hash>` that resolves to a recurrence
instance (`series_master_id` is `Some(_)`), commands that mutate (
`edit`, `move-time`, `delete`, `cancel`) behave like this:

- Default: act on the single occurrence only. Editing creates an
  exception via Graph's PATCH-on-occurrence-ID semantics.
- `--series` flag: act on the master event. Affects all future
  occurrences too.
- Interactive default (when no `-y` and no `--series`): prompt:
  > This is one occurrence of a series. Edit just this occurrence, or
  > the whole series? [occurrence/series]
- `-y` without `--series` defaults to **occurrence** (least destructive
  default).

`calendar duplicate` of a recurrence instance always produces a fresh
single event (the simplest mental model). `calendar move <hash> --to`
of an occurrence is rejected with a clear error pointing the user at
`--series`.

## Notifications

Microsoft Graph's default behaviour:

- `POST /me/calendar/events` with attendees → invitations sent automatically.
- `PATCH /me/events/{id}` with attendee or time changes → updates sent.
- `POST /me/events/{id}/cancel` → cancellation notices sent.
- `POST /me/events/{id}/accept|/decline|/tentativelyAccept` → RSVP sent
  unless `sendResponse: false`.

pidge surfaces this with `--notify` / `--no-notify` on `edit`,
`move-time`, and `rsvp`. Defaults: `--notify` when the event has
attendees other than the organizer; otherwise no notification is sent
either way. `cancel` always sends a notice (that's its whole point) —
the equivalent silent action is `delete`.

## Error handling

Concrete, actionable errors:

- **Hash not found / ambiguous** — same UX as mail (`No event found for
  fragment 'X'` / `Multiple events match 'X' — try a longer fragment`).
- **Not the organizer** on `cancel` / `edit` — refuse with:
  `You're not the organizer of "<title>". Use 'rsvp --decline' to remove
  yourself, or 'delete' to drop the event from your calendar without
  notifying anyone.`
- **Recurrence on an occurrence-targeted mutating call** — prompt
  (interactive) or default to *occurrence* with `-y`, `--series` to opt
  in.
- **No write permission on calendar** — `Calendar "<name>" is read-only
  on this account. Try --calendar to pick a different one.`
- **Time parse failure** — show the parser's hint plus accepted forms.
- **Graph errors** — bubble status + truncated body, like every other
  command.

## AI skill (`pidge ai skill --emit`)

The emitted `SKILL.md` gets a new `## Calendar` section teaching the agent:

- **Default to local TZ** when the user says "3pm Tuesday"; pass `--tz`
  only if the user explicitly named a different zone.
- **Two-step pattern** for "what time is my X meeting": run
  `calendar search` or `calendar list --json --week`, then
  `calendar show <hash>` for details. Same pattern as mail.
- **Disambiguate attendees** before invoking pidge — if the user said
  "John", confirm John's e-mail with the user; do not invent one.
  pidge does not look up names.
- **Cancel vs delete**: `cancel` for events with attendees (sends a
  notice); `delete` for solo events; `rsvp --decline` to remove yourself
  from someone else's invite.
- **Recurring instance vs series**: pass `--series` only when the user
  clearly meant "all of them".
- **Conflict checks**: when scheduling, optionally call
  `calendar list --json --from … --to …` first; pidge will not warn.
- **Date math**: convert relative dates ("next Tuesday") to absolute
  before calling pidge, so the command is reproducible in logs.

## Tests

**Unit / parser tests** (in `pidge-client` and `pidge-core`):

- `parse_when` — every time-string form (ISO, date-only, time-only,
  relative, weekday names) including DST-boundary days.
- `RecurrencePattern` ↔ Graph `recurrence` JSON serialization
  round-trips for daily/weekly/monthly/yearly with each `range` variant.
- Short-hash collision regression: same event ID under two accounts →
  distinct hashes.

**Wiremock tests** (in `pidge-client/src/graph/events.rs::tests`):

- `list_calendar_view` constructs the right query (start/end times,
  `$top`, `$orderby`).
- `create_event` serializes attendees, recurrence, and the online-meeting
  flag correctly.
- `cancel_event` POSTs to `/cancel` with the comment field.
- `move_event` PATCHes `calendar@odata.bind` to the right URL.
- `rsvp_event` posts to `/accept` vs `/tentativelyAccept` vs `/decline`
  with `sendResponse` as set.
- `search_events` passes the quoted KQL query.

**Snapshot tests** (in `crates/pidge/src/commands/calendar_show.rs::tests`):

- `render_event_full` against anonymized fixtures: recurring weekly
  meeting with 5 attendees and mixed RSVP, all-day birthday, online
  meeting with Teams URL.

**JSON-output tests:** `calendar list --json`, `calendar show --json`,
`calendar calendars --json` produce stable shapes for AI agents —
assert structure, not content.

**Manual tests:** New `MANUAL_TESTS.md` section covering: schedule a
meeting against a live mailbox, RSVP from a second account, move time,
cancel, create + delete a recurring weekly event, move event between
calendars.

## Out of scope for v1

- **Reminders** (`reminderMinutesBeforeStart`) — easy follow-up.
- **Free/busy queries across users** (Graph `findMeetingTimes`) —
  separate spec; would unlock AI-driven slot-finding.
- **Recurrence exceptions beyond occurrence-vs-series** — adding skip
  days or modifying multiple instances at once.
- **Categories / colour assignment.**
- **Importance / sensitivity / privacy flags.**
- **Bulk operations** (`calendar delete --older-than 6m`) — add-on later.
- **Tasks / To Do** — different Graph surface, separate feature.
- **Outlook calendar sharing / publishing.**
- **Address-book / contacts surface** — explicitly deferred; AI passes
  e-mails.

## Open questions

None at design time. Implementation will surface the usual edge cases
(Graph's `Prefer: outlook.timezone` behavior, recurrence-exception
semantics, `calendar@odata.bind` PATCH quirks) which we'll resolve in
code.
