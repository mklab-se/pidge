# pidge MCP: hands-free inbox review

**Date:** 2026-10-11
**Status:** approved in discussion, awaiting spec review
**Crates:** `crates/pidge-mcp` (main), `crates/pidge-core` (shared classification types), `ailloy` (typed error kinds)
**Builds on:** `2026-09-22-remote-mcp-full-feature-design.md`, `2026-06-15-ai-classification-design.md`

## Goal

Make "let's go through my e-mail" fast and natural from a voice harness (ChatGPT
voice mode in the car is the reference case). Today every message costs three
round trips: the model reads it, the user says "delete it", the model calls
`mail_act`, waits, reports, and the user has to ask for the next one, which is
another `mail_overview`/`mail_read`. The Graph calls are fast; the round trips
and the model narrating each step are what is slow.

The target is **one tool call per message**: the call that acts on the current
message also returns the next one. The action runs in the background on the
server, so to the user it feels asynchronous even though the harness makes an
ordinary synchronous call.

Second goal: sort the review so the user hears what matters first and can clear
bulk mail (promotions, newsletters) in one go. Sorting uses user-defined
**categories**, classified by the user's own AI key (TypeSafe's Jev is the
intended model: calibrated, fast, fractions of an öre per review), with a
no-AI fallback built from signals Graph already returns.

## Decisions taken in discussion

| Topic | Decision |
|---|---|
| Server-side model | The earlier rule "no server-side language model" is relaxed: an optional, per-user **classifier** (typed judgments only, never generated text). Voice, summaries and style stay the harness's job. |
| Shape | A review session: `mail_review_start` + `mail_review_next` (act on current, return next). |
| "Next" with no action | Mark the current message read, leave it in the inbox. |
| Scope | pidge chooses it. Default is "since the last review", widened or narrowed adaptively; the user can override. |
| State | In-process memory only, no database. Durable bits (categories, last review time, AI status) live in the existing per-user `UserRecord`. |
| Categories | First-class, user-editable via a tool; defaults offered on first use. Default names in English. |
| AI config | Per user, own API key, stored in its own Key Vault secret. Entered on a pidge web page via a link, never in the chat. |
| Providers | Fixed-endpoint providers only: TypeSafe, OpenAI, Anthropic. No custom base URLs (SSRF). |
| Status and hints | pidge tells the harness when AI is broken (no credits, bad key). Tips about adding a key are rare, only when it would have made a difference, and can be turned off. |
| Ailloy | Gains a typed error kind so pidge can tell "out of credits" from "bad key" from "overloaded". Released as ailloy 3.1.0. |
| Outlook categories | Not written to messages in this version. Possibly later. |

---

## 1. User experience

### 1.1 First review

> **User:** "Let's go through my e-mail."
>
> Harness calls `mail_review_start` with no arguments.
>
> **Harness:** "We haven't agreed on how to sort your mail yet. I can use the
> standard categories: Important, Personal, Bills and receipts, Newsletters,
> Promotions and Notifications. Shall we go with those? You can change them any
> time." ... "You have 19 unread since Thursday: 3 important, 2 bills, 9
> newsletters and promotions, 5 notifications. Shall I archive the 9 newsletters
> and promotions?"
>
> **User:** "Yes."
>
> Harness calls `mail_review_next` with `category` + `action: archive`.
> Response: "Archived 9." plus the first item.
>
> **Harness:** "First important: Anna, 08:14, about the weekend. She writes ..."

The first review runs with the default categories straight away (marked
*unconfirmed* in the response), so the onboarding question costs no extra
round trip. If the user says yes, the harness calls `mail_categories` with
`action: confirm`; if they want their own, they talk them through.

### 1.2 The loop

> **User:** "Archive. Next."
> Harness: `mail_review_next(session, id, action: archive)` → "Archived. Next: bill from Telia, 449 kr, due 30 October."
>
> **User:** "Flag it." → `action: flag`
>
> **User:** "Undo, that was wrong." → `action: undo`
>
> **User:** "Next." → `action: read` (the default)

### 1.3 Teaching pidge mid-review

> **User:** "Every time I get stuff like this from the climbing club, put it under Leisure."

The harness has the current message in hand and calls `mail_categories`:
- a sender or domain → a **rule** (`climbingclub.se` → Leisure);
- a kind of mail → an **example** (this message's sender + subject) and a sharper description;
- a missing category → it asks "Shall I create the category Leisure?" and adds it.

---

## 2. Tools

Three new tools, two changed. All follow the existing patterns: identity from
the bearer, `account` can only narrow to the caller's own mailboxes, ids pass
`check_id`, third-party text goes out through `render::untrusted` and header
fields through `render::one_line`, every mutation calls `cache.invalidate_user`.

### 2.1 `mail_review_start`

Input (all optional):
- `scope`: `auto` (default), `today`, `since_last_review`, `days` (with `days: 1..=30`), `all_unread`.
- `account`: narrow to one mailbox.

Behaviour:
1. Resolve the scope (§3).
2. List unread inbox messages in the window (one Graph list per mailbox, `$select` with `from`, `toRecipients`, `subject`, `bodyPreview`, `receivedDateTime`, `hasAttachments`, `inferenceClassification`, `internetMessageHeaders` only where needed for `List-Unsubscribe`).
3. Classify (§4) within a latency budget.
4. Order (§4.5), create a session (§5), prefetch the first item's body.

Output (text, voice-oriented):
- one header line: count, chosen scope in words ("19 unread since Thursday"), and why when pidge widened or narrowed it;
- per-category counts, in category order, with a suggested bulk action where the category asks for one;
- notes for the harness, each at most one line: categories unconfirmed (first time), AI status problem (§7), at most one hint (§7.3), older unread outside the window ("plus 31 older unread");
- the session id;
- the first item (§6).

### 2.2 `mail_review_next`

Input:
- `session` (required).
- `id`: the message the action applies to (the current item's id, echoed from the previous response). Required when `action` targets a single message. Carrying the id makes the call correct even if the session was lost (§5.3).
- `action`: `read` (default), `archive`, `delete`, `flag`, `keep_unread`, `undo`, `skip_category`.
- `category`: with `archive`, `delete` or `read`, applies the action to every remaining item of that category in the session (bulk). With `skip_category`, moves past them untouched.

Behaviour:
1. Validate synchronously (ownership, ids, action/category combination). Refuse bad input before any Graph call.
2. Enqueue the action on the session's action queue and return immediately; a per-session worker applies queued actions in order (Graph `$batch` for bulk).
3. Return: a short receipt ("Archived.", "Archived 9."), any failure of an earlier background action ("Earlier: archiving the Telia bill failed: not found."), then the next item, or "Done: nothing left in this review." with a one-line tally.

Semantics:
- `archive`, `delete`, `flag` also mark the message read. `delete` moves to Deleted Items, as everywhere in pidge.
- `keep_unread` leaves the message untouched and moves on.
- `undo` reverses the last action of the session (one level): move back to Inbox using the id Graph returned from the move, unflag, or mark unread. A bulk action is undone as a whole.
- Bulk actions are proposed before they are made (the server instructions already require that); the response of `mail_review_start` contains the proposal text so the harness only has to ask.

### 2.3 `mail_categories`

Input: `action` one of `list`, `confirm`, `use_defaults`, `add`, `update`, `remove`, `add_rule`, `remove_rule`, `add_example`, `remove_example`, `reorder`; plus the fields each action needs (`name`, `description`, `new_name`, `rule` as `sender:<address>` or `domain:<domain>`, `id` of a message for `add_example`, `order` as a list of names, `bulk_suggestion`).

- `add_example` takes a message id, not text: the server reads that message's sender and subject from the caller's own mailbox (ownership check) and stores them. The harness cannot inject arbitrary example text this way, and examples always mirror real mail.
- Returns the updated category list (names, descriptions, rule and example counts, order).
- Mutations invalidate the user's classification cache entries (§4.4).

### 2.4 `accounts_connect_ai`

Returns a link, valid 10 minutes, to a pidge page where the user picks a provider and pastes their key (§8.2). Same one-browser-per-flow cookie as `/connect`. After they finish, `accounts_list` shows the AI status.

### 2.5 Changes to existing tools

- `accounts_list`: one extra line, `AI classification: off | ok (TypeSafe, jev-latest) | no credits | key rejected | unavailable since <time>`. The key itself is never shown.
- `accounts_update`: new fields `ai: off` (removes the key), `ai_hints: on|off`.
- Server instructions (`tools/mod.rs`): a short review-mode paragraph: "When the user wants to go through their mail, call `mail_review_start`. After each decision, call `mail_review_next` with the action and the current id at once; confirm in at most a word or two and read the next item. Mention AI status and hint notes once, when they appear."

---

## 3. Scope

Unread messages in the inbox are the candidates. The window decides which of
them a review covers.

`auto` (the default):
1. Start from `last_review_at` (§5.4); on the first review, start of today in the user's timezone.
2. If more than 30 candidates fall in that window, narrow to today (if today is non-empty), so a long absence starts with what is fresh.
3. If fewer than 15, widen in steps (yesterday, 3 days, 7 days) until at least 15 or 7 days back.
4. Never further than 7 days back in `auto`; older unread are counted and mentioned, not included.

Thresholds are constants in one place (`review::scope`), unit-tested with fixed clocks. The output always says in words what was chosen and why.

---

## 4. Classification

### 4.1 Signals, cheapest first

1. **User rules** (`sender:` / `domain:`): exact, in code, always win.
2. **Jev** (or another configured provider via Ailloy's `eval`), when AI status is `ok`.
3. **Graph signals**, when AI is off or failed for a message: `List-Unsubscribe` present → the category flagged `signal: bulk`; `noreply`/`no-reply`/`donotreply` style senders → `signal: automated`; `inferenceClassification = focused` → sorted ahead of other uncategorised mail. Everything else → Other.

### 4.2 Jev requests

One request per message, all questions in that request (TypeSafe reads the
state once and answers every question in parallel; this is its batching model).

State, as JSON:
```json
{
  "from": { "name": "...", "address": "..." },
  "to_me_directly": true,
  "subject": "...",
  "preview": "... bodyPreview, up to 255 chars ...",
  "has_attachments": false,
  "has_list_unsubscribe": true,
  "received": "2026-10-11T08:14:00+02:00"
}
```

Questions:
- `category`: Choice over the user's categories plus `Other`. Each option's criteria is a structured object: the description and up to 5 examples (`{from, subject}`).
- `needs_reply`: YesNo, "Does this e-mail ask the recipient personally for a reply or a decision?"
- `importance`: Score with 4 concrete levels (from "automated or bulk, safe to ignore" to "personal and time-sensitive").

Requests run concurrently (at most 16 in flight per review, well under the
documented 80 requests/s). Not the alternative of packing many messages into
one state: TypeSafe documents accuracy dropping as the state grows, messages
would influence each other, and per-message caching would break.

The message text is third-party content: it goes to the classifier only as
`state`, never into `instructions` or `criteria`. The answer is a label from
the user's own list, so a hostile e-mail can at worst mislabel itself.

### 4.3 Latency budget

`mail_review_start` waits at most 4 s for classification. Messages not
classified by then use the Graph signals for this response and are classified
in the background; their final category applies to items not yet read out.
Each request has a 5 s timeout; one retry for 429/529 is Ailloy's.

### 4.4 Cache

Per-user, in-memory LRU, keyed by `(message id, categories hash, provider/model)`,
24 h TTL. A new review the same day reclassifies nothing it already knows.
Changing categories changes the hash, so affected messages are classified again,
and only then.

### 4.5 Order

Policy lives in code, separate from the raw judgments, so it can change without
reclassifying:
1. category order (the user's list order; defaults put Important and Personal first, Notifications last, bulk categories offered as a group up front);
2. within a category: `needs_reply` (≥ 0.5) first, then `importance`, then newest first.

A Choice answer with confidence below a threshold (start at 0.4, tuned on real
mail) falls back to Other rather than guessing.

---

## 5. Review session

### 5.1 State

In `AppState`, a per-user map of sessions (at most 3 per user, oldest evicted):
```text
ReviewSession {
  id, user, created_at, last_used,
  scope (resolved window, for display and rebuild),
  items: Vec<Item { id, account, category, needs_reply, importance, received }>,
  cursor,
  last_action: Option<Undo>,
  failures: Vec<String>,   // drained into the next response
  queue: mpsc::Sender<Action>,
}
```
Idle TTL 60 min. Session ids are random, 128 bits, and bound to the user: a
session id from another user reads as unknown.

### 5.2 Background actions

One worker task per session drains the action queue in order, so "archive,
then undo" can never race. Bulk actions use `$batch`. Each action calls
`cache.invalidate_user`. Failures are recorded with pidge's own fixed wording
(never a Graph body) and reported on the next response.

While the user listens to item N, the server prefetches item N+1's body so the
next call answers from memory.

### 5.3 Losing the session

The container runs one replica and scales to zero, so sessions disappear after
idle periods or deploys. That is acceptable:
- `mail_review_next` carries the message `id`, so the action is still applied correctly.
- An unknown session is rebuilt from the inbox with `scope: auto` (processed mail is no longer unread, so it does not come back) and the response says "(review restarted)".

### 5.4 Last review time

`UserRecord.review.last_review_at` is set to a session's start time once at
least one action has been applied in it. One Key Vault write per review.

---

## 6. Item rendering

Short, because both the model and the speech synthesis pay for every word:
```text
[Important · needs reply] Anna Holmberg · 08:14 · id AAMk...
Subject: Weekend
<untrusted-email-content>
...body as text, quoted replies and signature stripped, at most ~1,200 chars...
</untrusted-email-content>
2 attachments
```
Times are relative and in the user's timezone ("08:14", "yesterday 17:02",
"Thursday"). Sender name, not address, unless there is no name. HTML goes
through the same text conversion as `mail_read`.

---

## 7. Categories

### 7.1 Model

```text
Category {
  name,                    // as the user says it, any language
  description,             // in the user's words
  rules: Vec<Rule>,        // sender:/domain:, at most 20
  examples: Vec<Example>,  // {from, subject}, at most 5
  signal: Option<bulk|automated>,  // how the no-AI fallback maps onto it
  bulk_suggestion: bool,   // offer "archive all" at review start
}
```
At most 12 categories plus the implicit `Other`, which cannot be removed.
Stored in `UserRecord.review.categories` together with `confirmed: bool`. The
whole `UserRecord` must stay under Key Vault's 25 KB secret limit; saving
refuses with a clear message before that.

### 7.2 Defaults

| Name | Description | Signal | Bulk |
|---|---|---|---|
| Important | Needs the user's attention or action soon | | |
| Personal | From friends and family, not work or commerce | | |
| Bills and receipts | Invoices, payment reminders, receipts, order confirmations | | |
| Newsletters | Subscribed editorial content and digests | bulk | yes |
| Promotions | Marketing, offers, sales | bulk | yes |
| Notifications | Automated messages from services and systems | automated | |

English names; the harness speaks them in the user's language. User categories
can have any name.

---

## 8. Per-user AI configuration

### 8.1 Storage

A separate secret per user, `ai-<sanitized signin>-<hash8>`, holding
`{ provider, model, api_key, updated_at }`. Kept out of `UserRecord` because
`UserRecord` is rendered by `accounts_list`. Removing AI writes an empty value
(the store has no delete; empty already reads as absent).

Non-secret state goes in `UserRecord.ai`: `status`, `status_since`,
`last_hint_at`, `hints_off`.

The server builds an Ailloy client from an in-memory node (`Client::from_node`)
per user, cached for 10 minutes and dropped on any change. No `config.yaml`, no
keychain on the server.

### 8.2 Entering the key

`accounts_connect_ai` → `/ai/connect/{flow}` → Continue sets the flow cookie →
form (provider: TypeSafe, OpenAI or Anthropic; model, defaulted per provider;
key) → POST checked against the cookie and the flow token → the server makes
one tiny validation `eval` call (a single YesNo on a one-line state, a few
hundred tokens) → saved only if it succeeds, with a plain result page.

The key never passes through the harness or its model.

### 8.3 Fixed endpoints only

Ailloy can target arbitrary base URLs (Azure OpenAI, Ollama, custom). On the
hosted server that would let a user make the server call internal addresses,
including the managed-identity endpoint that unlocks every user's secrets. The
form therefore offers only providers with fixed public endpoints, and the
server constructs the node itself (provider + model + key), never taking a URL
from input. Custom endpoints are out of scope.

---

## 9. AI status and hints

### 9.1 Status

| Status | Cause (Ailloy error kind) | Behaviour |
|---|---|---|
| `off` | no key | Graph signals; maybe a hint (§9.3) |
| `ok` | | nothing to say |
| `no_credits` | `QuotaExhausted` | note once per review, with the fix (top up, or `accounts_connect_ai` for a new key) |
| `key_rejected` | `Auth` | same |
| `unavailable` | `RateLimited`, `Unavailable`, `Network`, timeouts | silent fallback; a note only after 24 h in this state |

Transitions are persisted (one Key Vault write each, rare). In `no_credits` and
`key_rejected` the server does not call the provider on every review: it
retries at most once an hour, and immediately after the key is changed.
Unknown 4xx statuses are treated as needing user action, not as temporary: a
spurious note is better than a silent failure.

### 9.2 Wording

Provider error bodies never reach the harness or the logs; they can echo the
request. pidge maps the kind to its own fixed sentences. Logs carry the user
hash, the kind, and the HTTP status only.

### 9.3 Hints

A hint ("pidge can sort better with your own AI key, for example Jev; ask me
how") appears in a `mail_review_start` response only when:
- AI is `off`, hints are not turned off, and
- the review has at least 15 items and more than half of them ended up in Other, or the user has custom categories without rules or signals (which need AI to work), and
- no hint was shown in the last 90 days (the first time counts).

It is addressed to the harness, which decides whether the moment suits it.
"Stop suggesting that" → `accounts_update ai_hints: off`.

---

## 10. Ailloy 3.1.0: typed error kinds

Add, without breaking the public API:
```rust
#[non_exhaustive]
pub enum ErrorKind { Auth, QuotaExhausted, RateLimited, Unavailable, InvalidRequest, Network, Other }
impl ClientError { pub fn kind(&self) -> ErrorKind }
```
Mapping per provider (from each provider's documented responses):
- TypeSafe: 401 → Auth; 422 → InvalidRequest; 429 → RateLimited; 529 → Unavailable; 402 → QuotaExhausted; 403 → Auth. TypeSafe does not document its out-of-credit response; until we know it, a 402 or a 429 whose body mentions credits, balance, quota or billing maps to QuotaExhausted. We find out by asking TypeSafe or reading their SDK, **never by draining the account**.
- OpenAI: 429 with `insufficient_quota` → QuotaExhausted; other 429 → RateLimited; 401 → Auth.
- Anthropic: 400/402 mentioning credit balance → QuotaExhausted; 401/403 → Auth; 429 → RateLimited; 529 → Unavailable.
- `Http` timeouts and connection errors → Network.

Also verified in this round: building a TypeSafe node in code with an
in-memory key (no config file, no keychain), and that `pidge-mcp` can depend
on ailloy without the TUI feature.

---

## 11. Security summary

- Cross-user isolation unchanged: sessions, caches and AI secrets are keyed by the bearer's user; session ids are user-bound.
- Keys enter only through the pidge web form, are stored only in Key Vault, are never rendered, logged or sent to the harness.
- No user-supplied URLs reach the AI client (SSRF).
- E-mail content reaches the classifier only as `state`; labels are constrained to the user's list.
- Examples are copied from the user's own messages by id, not taken as free text.
- Provider error bodies are never surfaced.
- Bulk and destructive actions keep the existing propose-first rule; delete still only moves to Deleted Items; undo covers one step.

## 12. Cost

No new Azure resources. Memory: sessions and caches are small (ids and a few
floats per message). Jev at $0.042 per million input tokens: about 1,000 tokens
per message with the questions, so a 50-message review is roughly $0.002, paid
by the user's own key.

## 13. Testing

- Scope resolution: pure function, fixed clocks and counts.
- Ordering and fallback: pure functions over fake judgments.
- Tools: the existing `ToolHarness` with wiremock for Graph and for TypeSafe (status codes 401/402/422/429/529 and malformed bodies), including session loss, undo after move, bulk via `$batch`, background failure reported on the next call, cross-user session ids.
- Ailloy: unit tests of `kind()` per provider from recorded response shapes.
- A handful of real, tiny Jev calls during development to confirm the request and answer format, never anything that meaningfully spends credits.
- Not testable here: ChatGPT voice behaviour. Verified by the user after deploy.

## 14. Delivery

Four steps, each its own plan and green CI:
1. **Ailloy 3.1.0**: `ErrorKind`, released.
2. **Review without AI**: sessions, `mail_review_start`/`next`, scope, Graph-signal fallback, `mail_categories`, defaults, rendering, server instructions.
3. **Per-user AI**: secret, `accounts_connect_ai` and web form, Jev classification, cache, budget.
4. **Status and hints**, `accounts_list`/`accounts_update` changes, docs (`deploy/azure/README.md` tool list, `docs/mcp.md`), CHANGELOG; release pidge 1.7.0, which also deploys `pidge-mcp`.

After deploy, the user enters their TypeSafe key via `accounts_connect_ai`; until then reviews use the Graph signals.

## Out of scope

- Writing pidge categories to Outlook categories on messages (possibly a later version).
- Uploading the local Ailloy node from `pidge mcp connect`.
- Custom AI endpoints on the hosted server.
- Unsubscribe as a review action (it stays in `mail_act`, with its send cap).
