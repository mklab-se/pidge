# pidge 2: a ground-up rethink

**Date:** 2026-11-05
**Status:** decided (see §10 and §11); built as one step into pidge 2.0
**Starting point:** pidge 1.7.7 (four crates, about 57 000 lines of Rust, 830 tests, 19 MCP tools,
about 60 CLI command modules)

## 0. The question and the short answer

> Given everything we have learned, would we build pidge the same way it is built today?

**No.** The foundations are right and stay: Rust, Microsoft Graph through one retrying client,
multi-mailbox reads merged by default, draft-then-send, untrusted-content wrapping, ids confined to
Graph's alphabet, an OAuth 2.1 front for the hosted server, a fixture-tested HTML renderer. What we
would build differently is the *shape*:

1. **One engine, thin frontends.** Today pidge is two products that grew apart: a human-first CLI
   that was later made agent-friendly, and a hosted MCP server that reimplemented the agent surface
   (better) on its own. The good ideas live in only one of them. pidge 2 has one engine that
   implements every operation once, and three adapters over it: CLI, local MCP (stdio), hosted MCP
   (HTTP).
2. **A state layer.** Most of the sample prompts are about *sets* and *time* ("new since I last
   looked", "all the invoices", "all the commercials", "today's appointments"). Graph alone answers
   those badly: there is no "is a receipt" filter and no "since you last asked" cursor. pidge 2 keeps
   a small, rebuildable local index (per mailbox, synced by Graph delta) plus durable preferences.
3. **Safety as structure, not prose.** Today "propose bulk actions before making them" is a sentence
   in tool descriptions, and the CLI and MCP enforce different rules (the CLI can send mail directly,
   the MCP cannot). pidge 2 has one policy engine and a **plan → apply** step for every bulk or
   outward-facing change, in every frontend.
4. **A surface designed from the prompts backwards.** About 14 intent-shaped operations, named the
   same in the CLI, the skill and MCP, each answering one of the sample prompts in one or two calls.

The rest of this document explains each of these, maps the sample prompts onto the new surface,
and proposes an incremental path from 1.7 to 2.0 that never leaves us without a working tool.

---

## 1. What we learned (the evidence)

These are observations from the current code, not opinions about it.

| # | Observation | Where |
|---|---|---|
| L1 | The CLI and the MCP server implement the same features twice, with different behaviour. `pidge mail list` and `mail_overview`, `pidge mail archive --from` and `mail_act`, `pidge calendar new` and `calendar_event` share Graph calls but not logic, output or rules. | `crates/pidge/src/commands/*` vs `crates/pidge-mcp/src/tools/*` |
| L2 | The best agent ideas exist only in the MCP: one tool per intent, compact text output, triage flags, inbox review (one call per message), categories with rules and examples, the send cap, untrusted wrapping on every read. A skill user gets none of them. | `tools/mod.rs`, `review/` |
| L3 | Two classification systems: the CLI's `ai classify` (free labels, local cache) and the MCP's review categories (rules → user's AI key → signals). Neither can see the other's results. | `commands/ai_classify.rs`, `review/classify.rs` |
| L4 | Two id schemes: the CLI's 8-char short-hash fragments (resolved through a local cache), the MCP's full Graph ids (about 150 characters each, repeated in every list item and every follow-up call). | `mail_fragment.rs`, `render.rs` |
| L5 | Two safety models: the CLI has user-configurable guardrails and `--dry-run`, and `mail new --to --subject --body` sends immediately; the MCP has hard-coded draft-only sending and a 30/hour cap but no dry-run and no user policy. | `pidge-core/config.rs`, `tools/mail_write.rs` |
| L6 | There is no local MCP. Claude Desktop, Cowork, Codex or any stdio MCP client can only use pidge through the hosted server or by shelling out through a skill. Self-hosting is the only path for anyone who prefers MCP. | (absent) |
| L7 | Bulk actions take explicit ids (1-100). "Archive all invoices" means the harness pages through search results, judges each one, and sends ids back, often several hundred items of tokens per round. The CLI has `--from`/`--older-than` bulk filters, the MCP does not. | `mail_act` schema |
| L8 | Calendar writes are single-event. "Cancel all appointments today" is N round trips, each needing the harness to know whether to cancel (organizer, notifies attendees), decline (invitee) or delete (no attendees). | `calendar_event` |
| L9 | `calendar_event update` with `attendees` *replaces* the attendee list and drops room bookings. "Invite this person to this event" therefore needs the harness to read and resend the full list, and gets it wrong by omission. | `calendar_event` description |
| L10 | "New" has no memory. `mail_overview` defaults to "today"; only a review remembers "since the last review". "Do I have any new e-mail?" at 16:00 re-reports what was discussed at 09:00. | `mail_overview`, `review/scope.rs` |
| L11 | Message ids are the default Graph ids, which change when a message moves folder. Anything that holds an id across an archive or move (undo, a review session, a later "move it back") has to work around that. Graph offers immutable ids (`Prefer: IdType="ImmutableId"`). | no `ImmutableId` anywhere |
| L12 | The hosted server's durable state is Key Vault secrets (user records, mailbox tokens, AI keys); everything else is in memory and lost on scale-to-zero (`minReplicas: 0`, `maxReplicas: 1`). Key Vault is a fine secret store and a poor database. | `secrets/`, `main.bicep` |
| L13 | A meaningful share of the CLI is human-terminal craft that agents never touch: full-screen compose forms (about 1 000 lines), inline image rendering and its trusted-senders list, OSC 8 link styling, page-number paging. | `compose_form.rs`, `calendar_compose_form.rs`, `trust.rs` |
| L14 | The emitted skill is deliberately small and points at `--help`. That kept it stable, but it means every agent rediscovers the same recipes, and JSON output (the skill's advice) costs several times the tokens of the MCP's compact text for the same list. | `commands/skill.rs` |

None of these is a bug in isolation. Together they say: the product we actually want is the MCP's
design, available everywhere, with memory and structural safety.

---

## 2. Principles for pidge 2

1. **The harness is the user interface; pidge is the hands and the memory.** pidge does not write
   prose, summaries or replies (voice and style are the harness's job). It does everything that is
   cheap and deterministic for a program and expensive or unreliable for a model: merging mailboxes,
   remembering what was seen, resolving names, finding sets, executing in bulk, undoing.
2. **One operation per intent, identical everywhere.** The same names, inputs and output shapes in
   the CLI, the skill, local MCP and hosted MCP. Learn it once.
3. **Answer in one call, act in two.** Any read question in the sample list is one call. Any change
   that touches more than one item, or reaches another person, is *plan* then *apply*.
4. **Graph is the source of truth.** The local index is a cache that can always be thrown away and
   rebuilt. Only preferences (categories, rules, watermarks, policy) are durable, and they are small.
5. **Tokens are a budget.** Short handles instead of Graph ids, compact text by default, structured
   JSON when asked, never a field the harness did not need.
6. **Local first, hosted is a deployment.** The hosted server is the same engine with a multi-user
   identity and secret-store adapter, for a small invited group and for self-hosters. pidge is not a
   service for the public, and nothing in the engine assumes it is.
7. **Nothing irreversible.** Delete means Deleted Items; cancel is Outlook's cancel; every mailbox
   change pidge makes is in an undo log for a while.

---

## 3. Target architecture

```
                 ┌────────────── frontends (thin, no business logic) ───────────────┐
                 │  pidge (CLI)         pidge mcp serve (stdio)    pidge-mcp (HTTP)  │
                 │  clap → Op input     rmcp stdio → Op input       rmcp HTTP + OAuth │
                 └───────────────┬───────────────────┬───────────────────┬──────────┘
                                 │      Op::run(ctx, input) -> Output    │
                 ┌───────────────▼───────────────────────────────────────▼──────────┐
                 │ pidge-engine                                                      │
                 │   ops/        one module per operation (mail_brief, mail_find …)  │
                 │   policy/     guardrails, caps, plan/apply, undo log              │
                 │   triage/     categories, rules, signals, importance, classifier  │
                 │   people/     contact resolution, VIPs, "people I write to"       │
                 │   render/     compact text + JSON for every Output                │
                 │   playbooks/  recipes, emitted as SKILL.md, MCP instructions,     │
                 │               MCP prompts                                         │
                 └───────┬──────────────────────┬──────────────────────┬────────────┘
                         │ Provider traits      │ Store trait          │ Secrets trait
                 ┌───────▼────────┐   ┌─────────▼──────────┐   ┌───────▼────────────┐
                 │ pidge-graph    │   │ pidge-store        │   │ keychain / file /  │
                 │ (today's       │   │ SQLite index +     │   │ Key Vault          │
                 │ pidge-client)  │   │ prefs, delta sync  │   │                    │
                 └────────────────┘   └────────────────────┘   └────────────────────┘
```

### 3.1 Crates

| Crate | Role | From today |
|---|---|---|
| `pidge-core` | Plain types: message, event, handle, selector, time ranges, categories. No IO. | Mostly kept; gains `Selector`, `Handle`, `Plan` types. |
| `pidge-graph` | Microsoft Graph: auth, retry, `$batch`, delta, immutable ids. Implements the provider traits. | Today's `pidge-client`, renamed or re-exported; the `mcp/` client-side module moves to the CLI. |
| `pidge-store` | The index and preferences (§5). SQLite via `rusqlite` with the `bundled` feature (no system library, works on every release target). | New. Replaces the CLI's ad-hoc JSON caches (messages, events, contacts, classify). |
| `pidge-engine` | Every operation, policy, triage, people, rendering, playbooks. Depends on traits, not on Graph or SQLite directly, so it is testable with in-memory fakes. | New. Built mostly by moving logic *out of* `pidge-mcp/src/tools` and `review/`, which is already closest to the target. |
| `pidge` | CLI binary. Parses arguments into op inputs, prints op outputs, owns the TTY confirmation prompt, `account add`, `mcp serve` (stdio), `mcp connect`. | Shrinks substantially. |
| `pidge-mcp` | Hosted HTTP server: OAuth AS, consent pages, bearer middleware, multi-user context, Key Vault, `/dl`, `/ai`. Tools are generated from the engine's op registry. | Keeps its security-critical parts (OAuth, ownership, markitdown hardening) as they are; tool files collapse into a registry. |

### 3.2 The operation seam

```rust
pub trait Op {
    const NAME: &'static str;              // "mail_find": the MCP tool name, the CLI verb path, the skill name
    const CLASS: ActionClass;              // Read | Change | Outbound | Bulk ...: drives policy
    type Input: DeserializeOwned + JsonSchema;
    type Output: Serialize + RenderText;
    async fn run(ctx: &Ctx, input: Self::Input) -> Result<Self::Output, OpError>;
}
```

- `Ctx` carries the caller (local user, or the hosted bearer identity), their mailboxes, timezone,
  the store, the provider, the policy and a clock. Hosted ownership checks (today's `ToolContext`)
  become the hosted implementation of "which mailboxes may this caller touch"; the engine only ever
  sees the mailboxes it was handed. This keeps today's guarantee (no tool takes a user id) by
  construction.
- MCP tool schemas come from `JsonSchema` (`schemars`, as `rmcp` already uses). Tool descriptions
  live next to the op, once.
- The CLI keeps hand-written `clap` for ergonomics (short flags, positional args), but each
  subcommand is a parse into `Op::Input` and nothing else. A generic `pidge op <name> --input -`
  accepts the exact MCP JSON, so the skill can use either form and tests can drive the CLI with the
  same fixtures as the MCP.
- `OpError` carries one of today's CLI exit-code kinds (`auth_expired`, `not_found`, `ambiguous`,
  `throttled`, `guardrail_denied`, `needs_confirmation`, `usage`), so the CLI exit codes and the MCP
  error messages come from one mapping.

### 3.3 Three frontends, one behaviour

| | CLI + skill | Local MCP (`pidge mcp serve`) | Hosted MCP (`pidge-mcp`) |
|---|---|---|---|
| Who | Claude Code, Codex, Copilot CLI; humans | Claude Desktop, Cowork, any stdio MCP client | claude.ai, ChatGPT, mobile/voice harnesses |
| Identity | the OS user | the OS user | OAuth bearer (pidge's own AS, Microsoft sign-in) |
| Tokens | keychain or file | keychain or file | Key Vault, one secret per mailbox |
| Store | `~/.cache/pidge/index.db`, `~/.config/pidge/prefs` | same files as the CLI | per-user files on the container (index), prefs in Key Vault or Blob (§5.4) |
| Confirmation | TTY prompt, or plan id | plan id, MCP elicitation where the client supports it | plan id, MCP elicitation where supported |
| AI classifier | optional (Ailloy config) | optional (Ailloy config) | optional (per-user key via `/ai`) |

Local MCP is the single biggest product gap today (L6): it gives every desktop MCP client the full
hosted experience without anyone hosting anything, and it is mostly free once the engine exists.

---

## 4. The operation catalog

Fourteen operations. Names are shared; the CLI form is `pidge <area> <verb>`
(`mail_find` is `pidge mail find`).

### 4.1 Mail

| Op | Purpose | Notes |
|---|---|---|
| `mail_brief` | "Any new mail?", "anything important?" | New. Everything since the user's *seen watermark* (§6.1), grouped by importance and category, with counts, the top items with a one-word reason each (`vip`, `reply-to-you`, `to-me`, `invite`, `deadline`, `question`), pending invites, and what is waiting for a reply. `mark_seen` (default true) moves the watermark. |
| `mail_find` | "Find the mail from X about Y", any list | Replaces `mail_overview` + `mail_search`. One `Selector` (§4.3). Ranked by relevance when there is free text, newest first otherwise. `count_only` for "how many…". |
| `mail_read` | Read one message or a thread | As today, plus `handles` in and out. Converted attachments stay in `mail_attachment`. |
| `mail_triage` | "Help me go through my inbox", "clear my inbox" | Today's review session, generalized: works in every frontend (sessions live in the store, so a CLI agent can resume), and each item carries a *suggested action* (§6.3). `start`, `next` (act on current, return next), `undo`, `summary`. |
| `mail_act` | Any change to mail | Takes `ids` **or** a `Selector`. Actions: `read`, `unread`, `flag` (optional due date), `unflag`, `archive`, `move`, `categorize`, `delete`, `unsubscribe`, `undo`. Selector-based or more than a threshold of ids returns a **plan** (§7.2) instead of acting. |
| `mail_draft` | New, reply, reply-all, forward | As today. |
| `mail_send` | Send a draft by id | As today, everywhere, including the CLI. The CLI's `mail new --to --subject --body` becomes draft + send (one command, two ops, same policy). |
| `mail_attachment` | Read or link an attachment | As today. Local mode writes to Downloads instead of a link. |
| `mail_setup` | Categories, rules, examples, VIPs, policy | Today's `mail_categories` plus importance preferences (VIPs, muted senders) and a read-only view of the policy. |

### 4.2 Calendar

| Op | Purpose | Notes |
|---|---|---|
| `calendar_agenda` | "What's on my calendar today / this week?" | As today, plus `conflicts` (overlapping events marked), and a short day summary line per day for week views. |
| `calendar_event` | Create, update, cancel one event | Adds `add_attendees` / `remove_attendees` (L9; `attendees` stays as an explicit replace), `from_message` (create from an e-mail or its `.ics` attachment: "add this event to my calendar"), and `scope: occurrence \| series` for recurring events. |
| `calendar_act` | "Cancel all appointments today", any bulk change | New. `Selector` over events (range, organizer/attendee role, title text, has attendees). Actions `clear` (the right thing per event: cancel what the user organizes with attendees, decline what they were invited to, delete what is theirs alone), `cancel`, `decline`, `delete`, `move` (shift by a duration). Always a plan first; the plan lists who will be notified. |
| `calendar_respond` | Accept, tentative, decline, propose a time | As today. |
| `calendar_availability` | "When am I free?", "when can Anna and I meet?" | As today for the user's own calendars; with `with` (people), uses Graph `getSchedule` where the account supports it (work/school accounts; personal accounts fall back to the user's own free time and say so). |

### 4.3 The selector

One type for every set-based question, used by `mail_find`, `mail_act`, `mail_triage`,
`calendar_act`:

```jsonc
{
  "text": "contract renewal",         // free text, Graph $search where possible
  "from": "Anna",                      // name or address, resolved through people/
  "to_me": true,
  "folder": "inbox",
  "since": "last_seen",                // or today, this_week, 7d, ISO
  "before": "2026-10-01",
  "unread": true,
  "has_attachments": true,
  "category": "Bills and receipts",    // the user's categories
  "kind": "receipt",                   // pidge's built-in kinds: receipt, newsletter, promotion,
                                       // notification, invite, personal, automated
  "importance": "high",
  "account": "kristofer@mklab.se",
  "limit": 50
}
```

`kind` is the important addition: it is what "invoices or receipts" and "commercials" mean, and
pidge resolves it from the index (§6.2) rather than asking the harness to judge every message.

### 4.4 Handles

Every message, thread, event and plan gets a short, stable, per-user handle such as `m7Kq2`,
`e3Fz9`, `p9Ab1`, backed by the store and mapped to Graph *immutable* ids (L11). A list of 20
messages drops from about 3 000 characters of ids to about 120. Handles survive moves (immutable
ids), work across frontends (the same store) and cannot name another user's item (the map is per
user; hosted lookups stay scoped to the caller exactly as today). Graph ids are still accepted as
input for compatibility.

### 4.5 Output

- **Compact text by default**, in every frontend: the MCP's current item and event lines, which are
  already tuned for models and readable by humans. The CLI's colour and alignment are a rendering
  of the same lines on a TTY, not a separate formatter.
- **JSON** with `--json` (CLI) and as MCP `structuredContent` next to the text, for harnesses that
  want to script.
- Every output that has an obvious follow-up ends with one `next:` line, as the MCP does today.
- Untrusted content is wrapped by the renderer for every frontend, so a CLI agent gets the same
  `<untrusted-email-content>` protection as an MCP client (today it does not).

---

## 5. The state layer

### 5.1 What is stored

| Table | Contents | Durable? |
|---|---|---|
| `messages` | Per mailbox, recent window (default 90 days of inbox, archive and sent; configurable): immutable id, handle, conversation, folder, from/to, subject, received, preview (200 chars), flags, Graph categories, `inferenceClassification`, list-unsubscribe presence, pidge kind + category + importance + the source of each (rule, signal, AI, harness) | Cache |
| `events` | Rolling window (7 days back to 60 days ahead): immutable id, handle, times, title, organizer, my role, response, attendee count, series id | Cache |
| `sync` | Graph delta tokens per mailbox and folder, per calendar | Cache |
| `people` | Today's contact cache: who the user writes to, how often, last contact | Cache |
| `plans`, `undo`, `triage_sessions` | §7 | Short-lived (hours) |
| `prefs` | Categories, rules, examples, VIPs, muted senders, seen watermarks, last triage, policy, default sender, timezone | **Durable** |

Bodies are not stored. Attachments are not stored. Anything outside the window falls through to
Graph (`$search`, folder listing), so the index makes the common case fast without ever being
required for correctness.

### 5.2 Sync

- On each operation, a mailbox whose sync is older than a few seconds is brought up to date with
  Graph delta first (one or two cheap calls when nothing changed). The CLI's `watch` and the
  hosted server can keep this warm in the background; nothing depends on it.
- A delta token that Graph has expired (410) means a quiet re-bootstrap of that folder.
- Classification runs on sync for new messages only, so `mail_brief` and `kind` selectors are
  index reads.

### 5.3 Why SQLite

The CLI already keeps four separate JSON caches with their own invalidation rules. One embedded
database with transactions replaces them, gives us indexed queries for selectors, and is the same
code locally and hosted. `rusqlite` with `bundled` needs no system library on any release target
(Linux, macOS, Windows). An in-memory database serves tests and the hosted fallback.

### 5.4 Hosted state

The hosted server runs one replica that scales to zero (L12). Proposal:

- **Index:** per-user SQLite files on the container's local disk, treated as cache. On a cold start
  the first call for a user bootstraps the recent window by delta (seconds for a typical inbox).
  We deliberately do not put SQLite on an Azure Files (SMB) mount; its locking is not safe for
  SQLite.
- **Preferences:** stay small JSON documents. Either keep them in the `UserRecord` in Key Vault
  (today's approach; it works for a handful of users) or move non-secret preferences to one Blob
  container, leaving Key Vault for tokens and AI keys only. Recommendation: move them, because
  categories, rules and examples grow and change on every review, and Key Vault is throttled and
  versioned per write. Decision in §11.
- **Plans, undo, sessions:** in the per-user index, so a restart loses at most the last few
  minutes of undo, which is acceptable and the same as today.

---

## 6. Triage: importance, kinds and the harness as classifier

### 6.1 "New"

Each user has a seen watermark per mailbox. `mail_brief` reports everything received after it and
moves it when the brief is delivered (`mark_seen=false` to peek). `mail_triage` moves it too.
"Do I have any new e-mail?" at 16:00 therefore reports only what arrived since 09:00, and says
"nothing new since 09:12" when there is nothing. `since` overrides it.

### 6.2 Kinds, categories and importance

Three related but separate things, which today are tangled into one category list:

- **Kind** is what a message *is*: receipt, newsletter, promotion, notification, invite, personal,
  automated. Fixed set, built in, language-neutral, used by selectors and by the suggested-action
  table. It is what makes "archive all receipts" and "delete all commercials" one call.
- **Category** is where the *user* files it: their own list (Uppdrag, Leisure, Bills and receipts …)
  with rules and examples, as today, optionally synced to Outlook categories, as today.
- **Importance** is how much it *matters now*: high, normal, low, from signals that are cheap and
  explainable: VIP sender, sender the user writes to, reply in a thread the user wrote in,
  to-me (not cc), Focused vs Other, invite awaiting a reply, deadline or question phrases,
  muted senders. Every high item carries its reason.

Classification order, extending today's review pipeline:

1. The user's rules (sender, domain) and examples.
2. **Deterministic signals** that are close to certain: `List-Unsubscribe` + Other ⇒ newsletter or
   promotion; a calendar `meetingMessageType` ⇒ invite; receipt patterns (sender domains of known
   payment providers, subject and preview patterns in Swedish and English, PDF attachment named
   like an invoice) ⇒ receipt; no-reply senders ⇒ notification.
3. The user's own AI key, if configured (Ailloy, as today, one cheap request per new message,
   cached by immutable id).
4. **Otherwise: unsure**, and that is an acceptable answer.

### 6.3 The harness is an LLM; use it

Today, without an AI key, pidge falls back to signals and the harness never gets to help. In pidge
2, every set-based operation returns the confident matches and, separately, the unsure ones in the
cheapest possible form (handle, sender, subject, 80-character preview). The harness, which is a
capable model already in the conversation, judges those few and either acts on them or teaches
pidge (`mail_setup add_example`, `add_rule`), and pidge records the harness's verdict in the index
(`source: harness`) so the same message is never asked about twice.

Example, "archive all e-mails that contain invoices or receipts":

```
mail_act action=archive selector={kind: receipt, folder: inbox}
→ plan p9Ab1: archive 37 receipts (31 by rule or signal, 6 by your AI key).
  4 unsure, not included:
    m7Kq2  Telia        "Din faktura för oktober"           …
    m2Lp8  Anna Berg    "Kvitto från middagen"              …
  next: apply plan p9Ab1, or include unsure handles with mail_act ids=[…]
```

This makes the AI key an accelerator, not a requirement, and keeps pidge free of generated text.
Where a client supports MCP sampling, pidge may later ask the client's model directly; no client
behaviour is assumed until we have verified it.

### 6.4 Suggested actions

Each triage item carries one suggested action, from a small table the user can override in
`mail_setup`:

| Kind / signal | Suggested |
|---|---|
| invite awaiting reply | respond (accept / decline / propose) |
| question or reply-to-you, to-me | draft a reply |
| receipt | file under the user's receipts category or folder, mark read |
| newsletter | archive; unsubscribe offered if the user has archived this sender before |
| promotion | delete; unsubscribe offered |
| notification | mark read |
| deadline phrase | flag with the due date |

"Clear my inbox (act on it, do not just delete it)" is then: `mail_triage start`, the harness
proposes the bulk part ("37 newsletters and promotions: archive them?"), applies it as one plan,
and walks the rest one call per message, each with a concrete proposal the user can accept with
"yes".

---

## 7. Safety: one policy, plan and apply, undo

### 7.1 One policy

Action classes, each `allow | confirm | deny`, configured once and enforced by the engine for every
frontend:

| Class | Covers | Default |
|---|---|---|
| `send` | `mail_send` | hold (§12) |
| `notify` | invites, attendee changes, cancellations, RSVPs with a message, unsubscribe e-mails | confirm |
| `bulk` | any change to more than 5 items, or any selector-based change | confirm |
| `delete` | delete (always to Deleted Items) | allow for single, confirm in bulk |
| `change` | read/unread, flag, move, archive, categorize, single-event edits with no attendees | allow |

Caps stay (30 outgoing messages per hour per user, counting notifications), now in the engine, so
the CLI gets them too. `confirm` means "the user must have seen a plan": in the CLI that is a TTY
prompt or `--apply <plan>`; in MCP it is the plan id, or MCP elicitation where the client supports
it. An agent cannot satisfy `confirm` by passing `-y` (today's guardrail semantics, kept).

### 7.2 Plans

A plan is the resolved, frozen set of changes: the exact handles, what will happen to each, who
will be notified, and the policy classes involved. It is stored for 10 minutes, bound to the
caller, and applying it re-checks that each item still matches (an item that moved or changed is
skipped and reported). The plan's text is short and designed to be read out to the user as-is
("Cancel 3 events today: 2 you organize (Anna, Bob and 4 others will be told), 1 decline").

### 7.3 Undo

Every change pidge makes to mail is appended to an undo log with what is needed to reverse it
(previous folder, read state, flags, categories). `mail_act action=undo` reverses the last plan or
action; triage keeps its per-step undo. Calendar cancellations and sent mail cannot be undone and
the plan says so before applying.

### 7.4 Untrusted content

Unchanged in substance (wrapping, one-line header fields, injection rules in the instructions), but
now applied by the shared renderer, so the skill path gets it too.

---

## 8. The sample prompts, end to end

| Prompt | Calls | How |
|---|---|---|
| Do I have any new e-mails? | 1 | `mail_brief` |
| Do I have any new important e-mails? | 1 | `mail_brief importance=high` |
| Help me go through my inbox | 1 + 1 per message | `mail_triage start`, then `next` per decision |
| Clear my inbox (act on it) | 2 + 1 per remaining | `mail_triage start` → apply the suggested bulk plan → `next` with suggested actions |
| Find the e-mail from … about … | 1 (+1 to read) | `mail_find from=… text=…`, then `mail_read` |
| Archive all e-mails with invoices or receipts | 2 | `mail_act archive selector={kind: receipt}` → plan → apply (harness may add unsure handles) |
| Delete all commercials | 2 | `mail_act delete selector={kind: promotion}` → plan → apply |
| What's on my calendar today? | 1 | `calendar_agenda` |
| What's on my calendar this week? | 1 | `calendar_agenda range=this_week` |
| Add this event to my calendar | 1-2 | `calendar_event create from_message=m…` or explicit fields; plan only if it invites anyone |
| Invite this person to this event | 2 | `calendar_event update add_attendees=[…]` → plan (notifies) → apply |
| Cancel all appointments today | 2 | `calendar_act clear selector={range: today}` → plan (cancel / decline / delete per event, lists who is told) → apply |
| When can Anna and I meet next week? | 1 | `calendar_availability with=[Anna] range=next_week` |
| Reply to Bob and propose Tuesday | 2-3 | `calendar_availability` → `mail_draft reply` → `mail_send` after approval |

Today the same list needs, for the bulk and calendar rows, between N and 3N calls, and the
"new" rows cannot be answered correctly at all.

---

## 9. What we keep, change and drop

### Keep (as is or nearly)

- Rust workspace, tokio, `rmcp`, axum, Ailloy for the optional classifier.
- The Graph client: `send_with_retry`, `$batch`, delta, cursor pagination.
- The hosted server's security core: OAuth 2.1 AS with consent pages, one-browser-per-flow cookie,
  `tid`/`oid` pinning, mailbox ownership records, bearer-only identity, markitdown hardening,
  signed download links, CORS. These were hard won (security review of 2026-09-23) and do not
  change shape.
- The HTML renderer and its fixture snapshot workflow.
- Draft-then-send, the send cap, untrusted wrapping, nothing deleted permanently.
- Exit codes and the JSON error envelope (now generated from `OpError`).
- Token storage (keychain or file) and `account migrate-storage`.
- `pidge mcp connect` for moving a local setup onto a hosted server.

### Change

- Business logic moves from `crates/pidge/src/commands` and `crates/pidge-mcp/src/tools` into
  `pidge-engine` ops.
- Graph ids → immutable ids + short handles.
- `ai classify` labels and review categories → one triage model (kinds, categories, importance).
- CLI guardrails and MCP hard-coded rules → one policy with plans.
- `mail_overview` + `mail_search` → `mail_find`; `mail_review_*` → `mail_triage`;
  `mail_categories` → `mail_setup`.
- `calendar_event attendees` replace semantics → explicit `add_attendees` / `remove_attendees`.
- The skill: generated from the op registry and the playbooks (§3, `playbooks/`), so it carries the
  same recipes as the MCP instructions and prompts, and recommends compact text over JSON.
- Hosted non-secret preferences: Key Vault → Blob (pending §11).

### Drop or demote

- Full-screen compose forms for mail and calendar. Humans can still compose with flags or
  `$EDITOR` (`--body-file -`); the forms are about 1 000 lines with no agent value.
- Inline image rendering in the terminal and the trusted-senders list that exists to drive it.
  "Trusted sender" survives as a VIP importance signal.
- Page-number paging (`--page`); cursors and selectors cover it.
- `--fields` / `--max-chars` (compact text and `limit` make them unnecessary; JSON keeps a
  documented field set).
- Separate per-feature JSON caches in the CLI (replaced by the store).

Rough effect: the CLI crate should shrink from about 16 700 lines to well under half, the MCP
tool layer from about 11 500 to a few thousand (registry, hosted context, pages), with the engine
holding the moved logic once. Total size should fall, not grow.

---

## 10. Decisions (2026-11-05)

| Question | Decision |
|---|---|
| Local front ends | Both: the skill (CLI) and local MCP (`pidge mcp serve`, stdio), on the same engine. |
| Hosted preferences | Move non-secret preferences to Blob storage; Key Vault keeps tokens and AI keys only. |
| Index window | 90 days by default. |
| Sending | No "confirm before send" by default. Sending is **held**: it goes out by itself after a short delay that grows with risk, can be cancelled during the hold, and the riskiest mail waits for a second approval (§12). |
| Human CLI features | Remove the compose forms, inline images and the trusted-senders list behind them. |
| Providers | Add **Gmail** (with Google Calendar) as a second provider, next to Microsoft (§13). |
| Tool names | As proposed in §4. |
| Migration | **One step.** There is one user and no compatibility to keep: pidge 2.0 replaces 1.7 outright, old tool names, CLI commands and file formats included. Section 10 of the first draft (phased releases) is withdrawn; §14 is the build order inside the single step. |

## 11. What the car session taught us (2026-11-05)

A voice session (ChatGPT voice mode, hosted pidge) gave five findings. Each one changes the design.

**F1. Waking up is slow, but acceptable.** The hosted app scales to zero, so the first call pays for
a container start, Key Vault reads and Graph token refreshes. pidge 2 also adds an index bootstrap
to that first call, which would make it worse. Responses:
- The index bootstraps in two steps: the inbox since the seen watermark (or the last 14 days) is
  synced before the first answer; the rest of the 90-day window fills in the background.
- `minReplicas` becomes a Bicep parameter, **default 1** for the reference deployment: with one
  always-on replica of this size the cost is small (idle pricing on Container Apps; check the
  current price list) and the index, plans and undo log survive between conversations.
  Self-hosters can set it to 0.

**F2. "Archive it" must not wait.** Every mailbox change is now **fire-and-forget**: the operation
records the change in the index at once (so the next read already reflects it), queues the provider
call on a per-user worker, and answers immediately. The answer is one word ("Archived.") followed by
whatever comes next. A failure is reported once, at the top of the next answer ("Earlier: archiving
the Telia mail failed, it is back in the inbox."). The CLI runs the queue before it exits; the MCP
frontends run it in the background. Sending is the only exception, and it has its own hold (§12).

**F3. The agent lacks the full picture.** Every triage start and every brief opens with an
**inbox map**: totals per mailbox, unread, new since the last look, per category and kind, waiting
for a reply, pending invites, flagged, oldest unread. The agent can answer "how bad is it?" and
plan without asking for more.

**F4. The agent forgets to continue.** Two causes, both fixed structurally:
- The agent used a different tool to act (the general `mail_act` instead of `mail_review_next`),
  and that tool's answer ended the turn. In pidge 2, **any** change to a message that belongs to an
  active triage session advances the session, and its answer carries the next item, whichever
  operation the agent called.
- The answer did not say what to do next. Every triage answer now ends with an explicit
  instruction line for the agent: `next: read this to the user and ask what to do; call
  mail_triage with their answer`. Triage answers put the next item *first*, and the confirmation
  of the last action is a single word, so a voice harness reads on without pausing.

**F5. An efficient order, part AI and part plain efficiency.** Triage is planned in **passes**
instead of walking the inbox newest first:
1. **Sweep:** everything pidge is confident is bulk (promotions, newsletters, notifications, by
   rule, signal or AI) is offered as one decision per kind: "31 newsletters and promotions and 12
   notifications: archive them all?" One yes clears most of an inbox.
2. **Batches:** routine mail that needs a glance, not a thought, grouped by kind and sender in
   groups of up to five: "4 receipts: Telia 449 kr, SL 970 kr, Apple 29 kr, Spotify 119 kr. File
   them?" One decision per group, with the option to pull one out.
3. **One by one:** what matters, most important first (VIP, waiting for your reply, invites,
   deadlines), each with a suggested action, and conversations collapsed into one item.

The sweep and batches are where plain efficiency wins; the one-by-one pass is where the AI helps
(the harness drafts the reply, proposes the RSVP). pidge decides the order, the user can always
say "skip", "later" or "just the important ones".

## 12. Sending: a hold instead of a question

The user wants to send without a ceremony, and still be able to catch a mistake. `mail_send` puts
the draft in pidge's **outbox** with a hold, and returns at once:

```
Sending to Anna Berg in 30 s ("Re: Avtal"). Say "cancel" to stop it.
next: tell the user it will go out in 30 s; mail_send action=cancel draft=d4 stops it
```

**Hold length** comes from a risk assessment of the draft (heuristics always, the user's AI key
when configured):

| Level | Hold | Typical triggers |
|---|---|---|
| low | 10 s | reply to one known person in an existing thread, short, no attachments |
| normal | 30 s | new mail to a known person, small recipient list |
| high | 2 min | first mail to an address, external recipients on a work account, many recipients, reply-all to a large list, attachments, forwarding |
| approval | held until approved | sensitive content (personal identity numbers, card or account numbers, passwords, health or legal words), more than 20 recipients, a forward of a thread that contains such content |

`approval` means the draft waits in the outbox until `mail_send action=approve` (the "second
approval"). The answer says why ("it contains what looks like a personal identity number"), so the
agent can read it to the user. `action=now` skips the hold (for "send it now"), `action=cancel`
stops it and leaves it a draft. The policy (`send: hold | confirm | allow | deny`) still applies;
`hold` is the new default.

**Where the hold runs:** the hosted server and local MCP run the outbox on their per-user worker.
The CLI, which exits, records the outbox entry in the store and starts a small detached
`pidge outbox run` process that sends due items and exits when the outbox is empty; every pidge
start also sends anything overdue, so a reboot during a hold only delays the send. Held sends count
against the send cap when they are queued, not when they go out.

## 13. Gmail

Gmail (with Google Calendar) is the second implementation of the provider traits. The engine does
not know which provider an account uses; the mapping is the provider's job:

| pidge concept | Microsoft Graph | Gmail / Google Calendar |
|---|---|---|
| inbox, archive, deleted | folders (`inbox`, `archive`, `deleteditems`) | labels: `INBOX`; archive = remove `INBOX`; delete = trash |
| move to folder | `move` | add the user label, remove `INBOX` |
| user category | Outlook categories | user labels (`pidge/<name>` namespace when synced) |
| bulk signal | `inferenceClassification` (Focused/Other) | `CATEGORY_PROMOTIONS`, `CATEGORY_SOCIAL`, `CATEGORY_UPDATES`, `CATEGORY_FORUMS` |
| delta | `/delta` with immutable ids | `history.list` from a `historyId` |
| flag | follow-up flag | `STARRED` |
| draft, send | Graph drafts | `drafts.create` (raw RFC 5322), `drafts.send` |
| calendar | `calendarView`, Graph events | Calendar API v3 `events` (`singleEvents=true`), `sendUpdates=all` on changes |
| free/busy | `getSchedule` (work/school) | `freeBusy.query` |
| online meeting | Teams (`isOnlineMeeting`) | Google Meet (`conferenceData.createRequest`) |

**Sign-in.** pidge cannot ship a Google OAuth client for everyone: the Gmail scopes pidge needs
(`gmail.modify`) are restricted, and a public app with them needs Google's verification and a
security assessment. Instead, whoever runs pidge brings their own Google Cloud OAuth client:
- locally, a "Desktop app" client (`PIDGE_GOOGLE_CLIENT_ID` / `PIDGE_GOOGLE_CLIENT_SECRET`, or
  `google.client_id` / `google.client_secret` in `config.yaml`), loopback redirect with PKCE;
- hosted, a "Web application" client, redirect `https://<host>/callback/google`, secret in Key
  Vault.
A consent screen left in "Testing" issues refresh tokens that expire after 7 days; for lasting use
the project should be published (unverified, it shows a warning screen and is capped at 100 users,
which suits personal use). `docs/gmail.md` walks through the setup.

## 14. Build order inside the single step

Everything lands on one branch and ships as 2.0. The order only exists so each part can be tested
on the parts below it:

1. `pidge-core` types (handles, selectors, kinds, provider-neutral message and event shapes) and
   the provider traits.
2. `pidge-graph` (today's `pidge-client`, renamed): immutable ids, the trait implementation.
3. `pidge-gmail`: Gmail and Google Calendar behind the same traits.
4. `pidge-store`: SQLite index, handles, prefs, plans, undo, outbox, triage sessions.
5. `pidge-engine`: op registry, policy, queue worker, outbox and risk, triage, people, render,
   playbooks, every operation in §4.
6. Frontends: the CLI (op commands built from the registry), `pidge mcp serve` (stdio), the hosted
   server (tools from the same registry; Blob prefs; Gmail connect).
7. Deploy (`minReplicas`, Blob storage, Google secrets), docs, skill, CHANGELOG, release 2.0.0.

## 15. As built (2026-11-05)

The single step landed on `claude/pidge-architecture-rethink-dshoko`. Where the build differs from
the text above:

- **Crate names.** The Microsoft crate keeps the name `pidge-client` (it holds the Graph provider
  and the shared token plumbing that `pidge-gmail` reuses); there is no `pidge-graph`.
- **Bootstrap.** Instead of two sync steps, a mailbox's first sync runs in the background for the
  whole window and a read waits for it up to a budget (8 s for MCP, 120 s for the CLI), answering
  with what is there and a note that older mail is still loading. With `minReplicas` 1 this happens
  once per user, not once per conversation.
- **Hosted MCP surface.** The 1.x MCP prompts and the mailbox category sync (`prune_mailboxes`,
  per-mailbox restore) are not carried over; `mail_setup action=label_mail` writes pidge's
  categories onto recent mail on request. MCP sampling is not used.
- **Gmail specifics.** Labels serve as both folders and categories, so a label used as a folder is
  also listed as a category; cancellations cannot carry a note (Google sends its own); forwards are
  text only; `has_attachments` in lists is a heuristic (the full message is exact).
- **Policy.** Single-message changes act at once; `bulk` (more than 5, or any selector) and
  `notify` (calendar changes that e-mail people) default to `confirm`, which returns a plan. RSVPs
  are not planned (answering an invitation is the user's explicit request) but a message with an
  RSVP counts against the hourly cap.
