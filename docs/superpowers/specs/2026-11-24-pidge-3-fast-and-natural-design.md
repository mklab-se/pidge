# Pidge 3.0: fast and natural

Status: approved in conversation, 2026-11-24

## Why

The activity log from 22 to 24 November (111 calls, one user) showed where time and trust are lost:

- A 12 minute cleanup held 18 seconds of pidge. Every tool call costs the agent about 4 seconds no
  matter how fast pidge is, so **fewer calls and fewer questions** matter most.
- At least 61 of 412 listed rows were duplicates from the mail service's search, and deleting them
  failed. Handles meant different mail after a restart.
- 8 of 10 plans were for messages the user had already named.
- Tools lay too close: `mail_brief` lost to `mail_find`, `mail_triage` could not categorize, there
  were three ways to categorize and the one used taught pidge nothing, `intent` was forgotten in
  58 % of calls.

## Goals

- A change is answered at once, with an id (`q7`). `undo` takes it back, done or not, and can swap it
  ("archive instead"). How long pidge waits before it acts is an internal detail it never mentions.
- Reads answer from the index, with pending changes already in them. The index keeps itself fresh.
- One answer holds a whole deck: the mail in a handful of groups with a suggestion each. One call
  carries all the decisions and brings the next deck.
- Mail about something that happens at a time says when, and whether that time is free, clashes or
  is already booked.
- "Where are my parcels?" is one call.
- 12 tools instead of 16, each described by when to use it and when not to.

## Non-goals

- A durable queue: work lives for seconds or minutes in the server process. A normal shutdown runs
  it; a crash loses it.
- Booking anything on its own because a mail says so.

## Approaches to the queue

- A. Azure Storage Queue, B. a durable table: survive a crash, at the cost of a service and a
  second source of truth. **C. In process, with ids (picked):** the work is short-lived.

## Design

**Tools:** `mail_inbox` (was `mail_brief` and `mail_triage`), `mail_find`, `mail_read` (several at
once), `mail_act` (several actions in one call), `mail_draft`, `mail_send`, `mail_attachment`,
`calendar_agenda`, `calendar_act` (was `calendar_event`, `calendar_act`, `calendar_respond`),
`calendar_availability`, `undo`, `settings` (was `accounts` and `mail_setup`). `intent` becomes a
field on every tool: what the user asked, in a few words.

**Queue** (`pidge-engine/src/queue.rs`, table `queue`, overlay table `pending`): an instruction has
an id, a due time and a status. Internal delay: read, flag and categories 0 s; archive and move
30 s; delete 60 s; calendar changes that reach other people 30 s. An instruction never runs before
an earlier one that touches the same message. Transient failures are retried three times. A message
that is already gone counts as done for archive, delete and read. A real failure restores the index
and is mentioned once. The CLI runs inline. Servers run the queue empty on shutdown.

**Index truth:** rows carry the Message-ID. A search hit whose Message-ID is already indexed is that
row. Inside the synced window, a search hit in a synced folder that the index does not know is a
stale hit and is dropped. `upsert` re-applies pending changes. Handles start at a base kept in the
preferences and raised for every new database, so an old handle is unknown, never another message.

**Freshness:** a hosted user is synced at server start, every minute while active and every ten
minutes otherwise. A read waits only for an index that has not loaded, or briefly for a stale one.

**Plans** remain only for selectors that match more than 50 messages, and for calendar changes that
e-mail other people.

**Deck:** groups have handles (`g4`) that stand for their messages wherever ids are accepted.
`mail_act next=true` answers with the next deck. `always=sender|kind` turns the action into a rule
for future mail; rules never touch mail that matters, and what they did is told in the next deck.

**Happenings:** a message is checked once, in the background: only mail that is not bulk and whose
text holds a date or time. Rules find the dates. With an AI key the model answers whether it is
something that happens to the recipient, and which of the found dates it is; it cannot invent one.
At most 15 questions per round; 150 a day for everything the AI is asked, counted in the preferences. The calendar status is computed from a local mirror
(90 days ahead) when the mail is shown.

**Deliveries:** kind `delivery` with a status from rules (ordered, on its way, out for delivery, to
collect, delivered). `mail_find kind=delivery` answers per parcel.

**Send check:** replies are checked on the user's own text, not the quoted history.

**Log:** every line carries pidge's version, a line marks each server start, jobs and AI use get
their own lines, the client falls back to the request's own client info or user agent.

## Left to the builder

Wording, thresholds inside the rules, file layout.
